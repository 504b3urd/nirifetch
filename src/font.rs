//! 终端与字体探测层。
//!
//! 两件事各自独立，但都属于「用户正看着的这块屏幕长什么样」，放在一起：
//!
//! - **终端**：沿 `/proc/<pid>/stat` 的父进程链往上走，找到最近的一个终端模拟器。
//!   这比读 `$TERM` 可靠得多 —— `$TERM` 只描述能力（`xterm-256color` 可能来自
//!   kitty、也可能来自 gnome-terminal），而进程链是事实。
//! - **字体**：终端自己的配置文件最准确（kitty 的 `font_family`），
//!   读不到再退回桌面设置（`gsettings`）与 fontconfig（`fc-match`）。
//!
//! 代价控制：进程链与 kitty.conf 都是纯 procfs / 普通文件读取，合计不到 1ms；
//! 只有前两者都失败时才会去 fork `gsettings` / `fc-match`，且都带超时。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::sys;

/// 配置文件读取上限。kitty.conf 通常只有几百字节。
const MAX_CONFIG_BYTES: u64 = 256 * 1024;

/// 沿父进程链向上最多回溯的层数。
///
/// `nirifetch → cargo → fish → kitty` 这类链路已有 4 层，
/// 留 12 层余量足以应对各种 wrapper，又不会在异常环境里绕圈。
const MAX_ANCESTOR_DEPTH: usize = 12;

/// 跟随 `include` 读取的子配置文件数量上限。
const MAX_INCLUDES: usize = 8;

/// 调用 `gsettings` / `fc-match` 的超时。
///
/// 比 IPC 的 1500ms 短得多：这两个只是锦上添花的回退手段，
/// 在 D-Bus 不可达的环境里（SSH、容器）不值得让用户等。
const PROBE_TIMEOUT: Duration = Duration::from_millis(400);

/// 识别得出的终端模拟器。
///
/// 列表顺序无关紧要，匹配时按「全等」或「名字 + `-`」前缀判定 ——
/// 后者是为了兼容 `/proc/<pid>/comm` 的 15 字符截断
/// （`gnome-terminal-server` 会变成 `gnome-terminal-`）。
const TERMINALS: [&str; 22] = [
    "kitty",
    "alacritty",
    "wezterm",
    "ghostty",
    "foot",
    "footclient",
    "konsole",
    "gnome-terminal",
    "xfce4-terminal",
    "mate-terminal",
    "lxterminal",
    "tilix",
    "terminator",
    "terminology",
    "cool-retro-term",
    "yakuake",
    "guake",
    "deepin-terminal",
    "roxterm",
    "sakura",
    "contour",
    "xterm",
];

/// 只有一个字母级的短名字必须单独放：它们做前缀匹配会误伤一大片
/// （`st` 会命中 `steam`、`ssh`、`systemd`）。
const SHORT_TERMINALS: [&str; 3] = ["st", "urxvt", "rxvt"];

/// 终端复用器。它们不是终端本身，命中了要继续往上找真正的终端。
const MULTIPLEXERS: [&str; 5] = ["tmux", "screen", "zellij", "abduco", "dvtm"];

/// 终端与字体的探测结果。任一项都可能为 `None`（UI 显示 `Unknown`）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TerminalFont {
    pub terminal: Option<String>,
    pub font: Option<String>,
}

/// 探测当前终端与它使用的字体。这是本模块对外的唯一入口。
pub fn probe() -> TerminalFont {
    let terminal = detect_terminal();
    let font = detect_font(terminal.as_deref());
    TerminalFont { terminal, font }
}

// ════════════════════════════════════════════════════════════════════
//  终端识别
// ════════════════════════════════════════════════════════════════════

/// 识别当前终端：先走进程链，再退回环境变量。
fn detect_terminal() -> Option<String> {
    detect_terminal_from_process_tree().or_else(detect_terminal_from_env)
}

/// 沿父进程链向上找终端模拟器。
///
/// 命中复用器（tmux 等）时不会立即返回 —— 复用器跑在某个终端*里面*，
/// 真正的答案还在上面。把它记作兜底，继续往上找。
fn detect_terminal_from_process_tree() -> Option<String> {
    let mut pid = std::process::id();
    let mut multiplexer: Option<String> = None;

    for _ in 0..MAX_ANCESTOR_DEPTH {
        let Some(name) = sys::process_name(pid) else {
            break;
        };
        let lower = name.to_ascii_lowercase();

        if let Some(terminal) = match_terminal(&lower) {
            return Some(terminal.to_owned());
        }
        if multiplexer.is_none()
            && let Some(mux) = MULTIPLEXERS.iter().find(|m| lower.starts_with(**m))
        {
            multiplexer = Some((*mux).to_owned());
        }

        match parent_of(pid) {
            // pid 1 是 init，走到它就到底了。
            Some(parent) if parent > 1 => pid = parent,
            _ => break,
        }
    }

    multiplexer
}

/// 从父进程链上摘下终端名；不是已知终端则返回 `None`。
fn match_terminal(process: &str) -> Option<&'static str> {
    if let Some(name) = TERMINALS
        .iter()
        .find(|name| process == **name || process.starts_with(&format!("{name}-")))
    {
        return Some(name);
    }
    // 短名字只认全等，避免 `st` 命中 `steam`。
    SHORT_TERMINALS
        .iter()
        .find(|name| process == **name)
        .copied()
}

/// 读取 `/proc/<pid>/stat` 里的父进程 PID。
///
/// `stat` 的第 2 个字段是 `(comm)`，而 comm 里**可以包含空格甚至右括号**
/// （`tmux: server`、`(sd-pam)`），所以不能按空白切分 —— 必须从最后一个
/// `)` 之后开始解析。字段顺序：comm 之后依次是 state、ppid。
fn parent_of(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    parse_ppid(&stat)
}

/// [`parent_of`] 的纯解析部分，便于用畸形输入做测试。
fn parse_ppid(stat: &str) -> Option<u32> {
    let (_, after_comm) = stat.rsplit_once(')')?;
    let mut fields = after_comm.split_whitespace();
    fields.next()?; // state
    fields.next()?.parse().ok() // ppid
}

/// 环境变量兜底。进程链走不通时（例如从 systemd 服务启动）用它。
fn detect_terminal_from_env() -> Option<String> {
    // 各终端自己注入的专属变量，比 $TERM 精确。
    const MARKERS: [(&str, &str); 8] = [
        ("KITTY_WINDOW_ID", "kitty"),
        ("KITTY_PID", "kitty"),
        ("ALACRITTY_SOCKET", "alacritty"),
        ("ALACRITTY_LOG", "alacritty"),
        ("WEZTERM_EXECUTABLE", "wezterm"),
        ("WEZTERM_PANE", "wezterm"),
        ("GHOSTTY_RESOURCES_DIR", "ghostty"),
        ("KONSOLE_VERSION", "konsole"),
    ];
    for (var, name) in MARKERS {
        if sys::env_non_empty(var).is_some() {
            return Some(name.to_owned());
        }
    }

    // $TERM_PROGRAM 由终端自己设置（macOS 系与部分 Linux 终端）。
    if let Some(program) = sys::env_non_empty("TERM_PROGRAM") {
        return Some(program);
    }

    terminal_from_term(&sys::env_non_empty("TERM")?)
}

/// 从 `$TERM` 猜终端名。
///
/// 这是最弱的一档证据：`xterm-256color` 可能来自 kitty 的兼容模式、
/// 也可能真的来自 xterm。仅在其它手段都失效时使用。
fn terminal_from_term(term: &str) -> Option<String> {
    let lower = term.to_ascii_lowercase();
    let name = if lower.contains("kitty") {
        "kitty"
    } else if lower.contains("alacritty") {
        "alacritty"
    } else if lower.contains("wezterm") {
        "wezterm"
    } else if lower.contains("ghostty") {
        "ghostty"
    } else if lower.contains("foot") {
        "foot"
    } else if lower.starts_with("tmux") {
        "tmux"
    } else if lower.starts_with("screen") {
        "screen"
    } else if lower.contains("xterm") {
        "xterm"
    } else {
        return None;
    };
    Some(name.to_owned())
}

// ════════════════════════════════════════════════════════════════════
//  字体识别
// ════════════════════════════════════════════════════════════════════

/// 识别字体：终端自己的配置 → 桌面设置 → fontconfig。
fn detect_font(terminal: Option<&str>) -> Option<String> {
    if terminal.is_some_and(|name| name.eq_ignore_ascii_case("kitty"))
        && let Some(font) = kitty_font()
    {
        return Some(font);
    }
    gsettings_font().or_else(fontconfig_font)
}

/// 从 kitty 配置里读 `font_family`。
///
/// 主文件读不到就跟着 `include` 再找一层 —— 很多用户把字体设置放在主题文件里。
fn kitty_font() -> Option<String> {
    let dir = kitty_config_dir()?;
    let main = dir.join("kitty.conf");
    let text = sys::read_capped(&main, MAX_CONFIG_BYTES)?;

    if let Some(font) = find_directive(&text, "font_family") {
        return Some(font);
    }

    for include in included_files(&text).into_iter().take(MAX_INCLUDES) {
        let path = resolve_include(&dir, &include);
        if let Some(font) = sys::read_capped(&path, MAX_CONFIG_BYTES)
            .as_deref()
            .and_then(|t| find_directive(t, "font_family"))
        {
            return Some(font);
        }
    }
    None
}

/// kitty 的配置目录。`$KITTY_CONFIG_DIRECTORY` 是 kitty 自己支持的覆盖手段。
fn kitty_config_dir() -> Option<PathBuf> {
    if let Some(dir) = sys::env_non_empty("KITTY_CONFIG_DIRECTORY") {
        return Some(PathBuf::from(dir));
    }
    Some(xdg_config_home()?.join("kitty"))
}

/// `$XDG_CONFIG_HOME`，未设置时按 XDG 规范回退到 `~/.config`。
fn xdg_config_home() -> Option<PathBuf> {
    if let Some(dir) = sys::env_non_empty("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(dir));
    }
    Some(PathBuf::from(sys::env_non_empty("HOME")?).join(".config"))
}

/// 从配置文件文本里取一条 `key value` 指令的值。
///
/// 只做行级扫描：跳过空行与 `#` 注释，取第一个词等于 `key` 的行，
/// 其余部分即值（去掉可能的引号）。kitty.conf 里带空格的值是合法的
/// （`font_family Adwaita Mono`），所以值要取到行尾而不是下一个空格。
fn find_directive(text: &str, key: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((name, value)) = line.split_once(|c: char| c.is_whitespace() || c == '=') else {
            continue;
        };
        if !name.eq_ignore_ascii_case(key) {
            continue;
        }
        let value = value.trim().trim_matches(['"', '\'']).trim();
        if !value.is_empty() {
            return Some(value.to_owned());
        }
    }
    None
}

/// 收集配置文件里所有 `include` 指令指向的路径。
fn included_files(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let (name, value) = line.split_once(|c: char| c.is_whitespace() || c == '=')?;
            if !name.eq_ignore_ascii_case("include") {
                return None;
            }
            let value = value.trim().trim_matches(['"', '\'']).trim();
            if value.is_empty() {
                None
            } else {
                Some(value.to_owned())
            }
        })
        .collect()
}

/// 把 `include` 里的相对路径解析成绝对路径。
fn resolve_include(base_dir: &Path, include: &str) -> PathBuf {
    if let Some(rest) = include.strip_prefix("~/")
        && let Some(home) = sys::env_non_empty("HOME")
    {
        return PathBuf::from(home).join(rest);
    }
    let path = Path::new(include);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base_dir.join(path)
    }
}

/// 从 kitty 配置读取终端调色板（`color0`–`color7`）。
///
/// 只有终端是 kitty 且 8 个基础色都能读到才返回 `Some`；否则返回 `None`，
/// 由 UI 退回内置的高亮色板。主文件缺失的颜色会尝试到 `include` 的文件里补齐。
pub fn terminal_palette(terminal: Option<&str>) -> Option<Vec<(u8, u8, u8)>> {
    if !terminal.is_some_and(|name| name.eq_ignore_ascii_case("kitty")) {
        return None;
    }
    let dir = kitty_config_dir()?;
    let text = sys::read_capped(&dir.join("kitty.conf"), MAX_CONFIG_BYTES)?;

    let mut palette = [None; 8];
    fill_palette(&mut palette, &text);
    if palette.iter().any(Option::is_none) {
        for include in included_files(&text).into_iter().take(MAX_INCLUDES) {
            if let Some(body) = sys::read_capped(&resolve_include(&dir, &include), MAX_CONFIG_BYTES)
            {
                fill_palette(&mut palette, &body);
            }
            if palette.iter().all(Option::is_some) {
                break;
            }
        }
    }
    palette.into_iter().collect()
}

/// 把一份配置正文里的 `colorN #rrggbb` 填进色板，已在的槽位不覆盖。
fn fill_palette(palette: &mut [Option<(u8, u8, u8)>; 8], text: &str) {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((name, value)) = split_directive_pair(line) else {
            continue;
        };
        let Some(idx) = name
            .strip_prefix("color")
            .and_then(|n| n.parse::<usize>().ok())
        else {
            continue;
        };
        if idx >= 8 || palette[idx].is_some() {
            continue;
        }
        if let Some(rgb) = parse_hex_color(value) {
            palette[idx] = Some(rgb);
        }
    }
}

/// 把一行拆成 `(键, 值)`：`=` 优先，其次按空白分隔。
///
/// 先认 `=` 是因为 `color1 = #f38ba8` 这种写法按空白切会把 `=` 留在值里。
fn split_directive_pair(line: &str) -> Option<(&str, &str)> {
    if let Some((name, value)) = line.split_once('=') {
        return Some((name.trim(), value.trim()));
    }
    let (name, value) = line.split_once(char::is_whitespace)?;
    Some((name.trim(), value.trim()))
}

/// 解析 `#rrggbb` / `rrggbb`（可带引号）为 RGB 三元组。
fn parse_hex_color(raw: &str) -> Option<(u8, u8, u8)> {
    let hex = raw.trim().trim_matches(['"', '\'']).trim_start_matches('#');
    if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some((r, g, b))
}

/// 读 GNOME 的界面字体设置。
///
/// 注意这是**桌面**的字体偏好，不一定等于终端实际使用的字体 ——
/// 所以它排在终端自身配置之后。
fn gsettings_font() -> Option<String> {
    let mut cmd = Command::new("gsettings");
    cmd.args(["get", "org.gnome.desktop.interface", "font-name"]);
    let out = sys::run(&mut cmd, PROBE_TIMEOUT)?;
    if !out.status.success() {
        return None;
    }
    parse_gsettings_font(&String::from_utf8_lossy(&out.stdout))
}

/// 解析 `gsettings` 的输出。
///
/// 形如 `'Adwaita Sans 11'` —— 带引号、且末尾附着一个字号。
fn parse_gsettings_font(raw: &str) -> Option<String> {
    let trimmed = raw.trim().trim_matches(['\'', '"']).trim();
    if trimmed.is_empty() {
        return None;
    }

    let mut parts: Vec<&str> = trimmed.split_whitespace().collect();
    // 末尾的纯数字是字号，不属于字体名。
    if parts.len() > 1
        && parts
            .last()
            .is_some_and(|p| p.chars().all(|c| c.is_ascii_digit()))
    {
        parts.pop();
    }

    let name = parts.join(" ");
    if name.is_empty() { None } else { Some(name) }
}

/// fontconfig 兜底：问系统「monospace 实际会落到哪个字体」。
fn fontconfig_font() -> Option<String> {
    let mut cmd = Command::new("fc-match");
    cmd.args(["--format", "%{family}", "monospace"]);
    let out = sys::run(&mut cmd, PROBE_TIMEOUT)?;
    if !out.status.success() {
        return None;
    }
    parse_fc_match(&String::from_utf8_lossy(&out.stdout))
}

/// 解析 `fc-match` 的输出。
///
/// `%{family}` 可能给出逗号分隔的候选列表（`Noto Sans Mono,Noto Sans Mono CJK`），
/// 只取第一个 —— 那是实际首选。
fn parse_fc_match(raw: &str) -> Option<String> {
    let name = raw.split(',').next()?.trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_owned())
    }
}

// ════════════════════════════════════════════════════════════════════
//  测试
// ════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    // ── /proc/<pid>/stat 解析 ─────────────────────────────────────

    #[test]
    fn parses_ppid_from_a_normal_stat_line() {
        // pid (comm) state ppid ...
        assert_eq!(parse_ppid("1234 (kitty) S 1000 1234 1234 0 -1"), Some(1000));
    }

    #[test]
    fn parses_ppid_when_comm_contains_spaces() {
        // tmux 的 comm 带空格，按空白切分会错位。
        assert_eq!(
            parse_ppid("900 (tmux: server) S 1797 900 900 0 -1"),
            Some(1797)
        );
    }

    #[test]
    fn parses_ppid_when_comm_contains_parentheses() {
        // comm 里连右括号都可能出现 —— 必须从**最后**一个 ')' 之后开始解析。
        assert_eq!(parse_ppid("900 (weird)name) S 4242 900 900 0"), Some(4242));
    }

    #[test]
    fn rejects_malformed_stat_lines() {
        assert_eq!(parse_ppid(""), None);
        assert_eq!(parse_ppid("no parens here"), None);
        // 括号后缺少 ppid 字段。
        assert_eq!(parse_ppid("1234 (kitty) S"), None);
        assert_eq!(parse_ppid("1234 (kitty) S notanumber 0"), None);
    }

    // ── 终端匹配 ──────────────────────────────────────────────────

    #[test]
    fn matches_terminals_by_exact_name() {
        assert_eq!(match_terminal("kitty"), Some("kitty"));
        assert_eq!(match_terminal("alacritty"), Some("alacritty"));
        assert_eq!(match_terminal("xterm"), Some("xterm"));
    }

    #[test]
    fn matches_truncated_comm_names() {
        // /proc/<pid>/comm 只保留 15 字符，`gnome-terminal-server` 会被截断。
        assert_eq!(match_terminal("gnome-terminal-"), Some("gnome-terminal"));
        assert_eq!(match_terminal("xfce4-terminal"), Some("xfce4-terminal"));
    }

    #[test]
    fn short_names_require_an_exact_match() {
        // 这是本文件里最容易写错的一处：`st` 做前缀匹配会命中一堆无关进程。
        assert_eq!(match_terminal("st"), Some("st"));
        assert_eq!(match_terminal("steam"), None);
        assert_eq!(match_terminal("ssh"), None);
        assert_eq!(match_terminal("systemd"), None);
        assert_eq!(match_terminal("urxvt"), Some("urxvt"));
        assert_eq!(match_terminal("urxvt-unicode"), None);
    }

    #[test]
    fn ignores_unrelated_processes() {
        for name in ["bash", "fish", "systemd", "cargo", "nirifetch", "claude"] {
            assert_eq!(match_terminal(name), None, "{name} 不该被认成终端");
        }
    }

    #[test]
    fn terminals_do_not_shadow_each_other() {
        // `footclient` 与 `foot` 同在列表里，必须各自命中自己。
        assert_eq!(match_terminal("foot"), Some("foot"));
        assert_eq!(match_terminal("footclient"), Some("footclient"));
        assert_eq!(match_terminal("wezterm-gui"), Some("wezterm"));
    }

    // ── $TERM 推断 ────────────────────────────────────────────────

    #[test]
    fn infers_terminal_from_term_variable() {
        assert_eq!(terminal_from_term("xterm-kitty").as_deref(), Some("kitty"));
        assert_eq!(
            terminal_from_term("alacritty").as_deref(),
            Some("alacritty")
        );
        assert_eq!(terminal_from_term("foot-extra").as_deref(), Some("foot"));
        assert_eq!(terminal_from_term("tmux-256color").as_deref(), Some("tmux"));
        assert_eq!(
            terminal_from_term("screen.xterm-256color").as_deref(),
            Some("screen")
        );
        assert_eq!(
            terminal_from_term("xterm-256color").as_deref(),
            Some("xterm")
        );
    }

    #[test]
    fn returns_none_for_unknown_term() {
        assert_eq!(terminal_from_term("linux"), None);
        assert_eq!(terminal_from_term("dumb"), None);
        assert_eq!(terminal_from_term(""), None);
    }

    // ── 配置解析 ──────────────────────────────────────────────────

    /// 与本机 kitty.conf 同构的样例。
    const KITTY_CONF: &str = "\
# 主题
include themes/frappe.conf
# 字体
font_family Adwaita Mono
font_size 15
";

    #[test]
    fn finds_a_directive_with_a_spaced_value() {
        // 值必须取到行尾：`Adwaita Mono` 中间的空格是值的一部分。
        assert_eq!(
            find_directive(KITTY_CONF, "font_family").as_deref(),
            Some("Adwaita Mono")
        );
        assert_eq!(
            find_directive(KITTY_CONF, "font_size").as_deref(),
            Some("15")
        );
    }

    #[test]
    fn directive_lookup_ignores_comments_and_unknown_keys() {
        assert_eq!(find_directive("font_family X", "font_size"), None);
        // 被注释掉的设置不生效。
        assert_eq!(
            find_directive("# font_family Commented", "font_family"),
            None
        );
        assert_eq!(find_directive("", "font_family"), None);
        // 键名比较不区分大小写。
        assert_eq!(
            find_directive("Font_Family Foo", "font_family").as_deref(),
            Some("Foo")
        );
    }

    #[test]
    fn directive_lookup_strips_quotes() {
        assert_eq!(
            find_directive("font_family \"JetBrains Mono\"", "font_family").as_deref(),
            Some("JetBrains Mono")
        );
        assert_eq!(
            find_directive("font_family='Foo Bar'", "font_family").as_deref(),
            Some("Foo Bar")
        );
        // 空值不算命中，应当继续找下一行。
        assert_eq!(
            find_directive("font_family   \nfont_family Real", "font_family").as_deref(),
            Some("Real")
        );
    }

    #[test]
    fn collects_include_directives() {
        assert_eq!(included_files(KITTY_CONF), vec!["themes/frappe.conf"]);
        assert!(included_files("font_family X").is_empty());
        assert!(included_files("# include nope.conf").is_empty());
    }

    #[test]
    fn parses_hex_colours() {
        assert_eq!(parse_hex_color("#1e1e2e"), Some((0x1e, 0x1e, 0x2e)));
        assert_eq!(parse_hex_color("1e1e2e"), Some((0x1e, 0x1e, 0x2e)));
        assert_eq!(parse_hex_color("  \"#ABCDEF\"  "), Some((0xAB, 0xCD, 0xEF)));
        // 长度不对、含非法字符都要安静失败。
        assert_eq!(parse_hex_color("#fff"), None);
        assert_eq!(parse_hex_color("#gggggg"), None);
        assert_eq!(parse_hex_color(""), None);
    }

    #[test]
    fn builds_a_palette_from_colour_directives() {
        let text = "\
color0 #1e1e2e
color1 = #f38ba8
# color2 #000000 这一行是注释，不生效
color3 #a6e3a1
";
        let mut palette = [None; 8];
        fill_palette(&mut palette, text);
        assert_eq!(palette[0], Some((0x1e, 0x1e, 0x2e)));
        assert_eq!(palette[1], Some((0xf3, 0x8b, 0xa8)));
        assert_eq!(palette[2], None, "被注释的颜色不应生效");
        assert_eq!(palette[3], Some((0xa6, 0xe3, 0xa1)));
        // 缺色时整体不可用，让 UI 退回内置色板。
        assert!(
            palette.into_iter().collect::<Option<Vec<_>>>().is_none(),
            "缺色的色板不应被采用"
        );
    }

    #[test]
    fn first_palette_definition_wins() {
        // 主文件已给定时，include 里的同号颜色不该覆盖它。
        let mut palette = [None; 8];
        fill_palette(&mut palette, "color0 #111111\n");
        fill_palette(&mut palette, "color0 #222222\n");
        assert_eq!(palette[0], Some((0x11, 0x11, 0x11)));
    }

    #[test]
    fn resolves_include_paths_against_the_config_dir() {
        let base = Path::new("/home/u/.config/kitty");
        // 相对路径按配置目录解析。
        assert_eq!(
            resolve_include(base, "themes/frappe.conf"),
            PathBuf::from("/home/u/.config/kitty/themes/frappe.conf")
        );
        // 绝对路径原样使用。
        assert_eq!(
            resolve_include(base, "/etc/kitty/x.conf"),
            PathBuf::from("/etc/kitty/x.conf")
        );
        // `~/` 展开到家目录（取当前环境里一定存在的 HOME）。
        if let Some(home) = sys::env_non_empty("HOME") {
            assert_eq!(
                resolve_include(base, "~/a.conf"),
                PathBuf::from(home).join("a.conf")
            );
        }
    }

    // ── 回退链解析 ────────────────────────────────────────────────

    #[test]
    fn parses_gsettings_output() {
        // gsettings 输出带引号，且末尾附着字号。
        assert_eq!(
            parse_gsettings_font("'Adwaita Sans 11'\n").as_deref(),
            Some("Adwaita Sans")
        );
        assert_eq!(
            parse_gsettings_font("'Adwaita Mono 10'").as_deref(),
            Some("Adwaita Mono")
        );
        // 没有字号时不该误删字体名里的词。
        assert_eq!(
            parse_gsettings_font("'Source Code Pro'").as_deref(),
            Some("Source Code Pro")
        );
        assert_eq!(parse_gsettings_font(""), None);
        assert_eq!(parse_gsettings_font("''"), None);
    }

    #[test]
    fn parses_fc_match_output() {
        assert_eq!(
            parse_fc_match("Noto Sans Mono\n").as_deref(),
            Some("Noto Sans Mono")
        );
        // 多候选时只取首选。
        assert_eq!(
            parse_fc_match("Noto Sans Mono,Noto Sans Mono CJK").as_deref(),
            Some("Noto Sans Mono")
        );
        assert_eq!(parse_fc_match(""), None);
        assert_eq!(parse_fc_match("   "), None);
    }
}
