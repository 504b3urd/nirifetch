//! 状态栏 / 桌面外壳检测。
//!
//! Niri 本身只画窗口，状态栏、面板、启动器全靠用户自己 spawn 一个外部程序 ——
//! 于是「你在用哪套 bar」就成了 niri 配置里最有辨识度的一条信息。
//!
//! 检测分两级，**先便宜后昂贵**：
//!
//! 1. **静态配置**：扫 [`crate::config`] 已经读进内存的配置正文（主文件加全部
//!    include 展开），找 `spawn-at-startup` / `spawn-sh-at-startup` 指令。
//!    这一步不产生任何文件 IO，配置里写明了就直接命中并返回。
//! 2. **动态补充**：配置里没写（或配置读不出来）时，才去遍历 `/proc` 看有没有
//!    同名进程常驻。
//!
//! 状态栏几乎总是由 niri 自己在启动时拉起来的，所以第 1 级覆盖了绝大多数情况，
//! 第 2 级只是兜住「配置在别处生成」或「bar 由 systemd user service 托管」的人。
//!
//! 两级都落空时返回 `None`，由 UI 渲染成 `Unknown`。

use crate::config::{self, ConfigInfo};
use crate::sys;

/// 已知的状态栏 / 外壳程序：`(进程名或命令名, 展示名)`。
///
/// 展示名以社区里通用的叫法为准，不追求与上游项目全名一致 ——
/// `Dank Material Shell (DMS)` 写成全名会挤占值列预算，把缩写放进括号里
/// 反而更好认。
const BARS: [(&str, &str); 9] = [
    ("waybar", "Waybar"),
    ("noctalia", "Noctalia Shell"),
    ("noctalia-shell", "Noctalia Shell"),
    ("dms", "Dank Material Shell (DMS)"),
    ("dank-material-shell", "Dank Material Shell (DMS)"),
    ("eww", "Eww"),
    ("ironbar", "Ironbar"),
    ("ags", "AGS"),
    // Astal 是 AGS 底下的那层库，单独跑 `astal` 的情况少见但确实存在。
    ("astal", "AGS"),
];

/// 检测当前使用的状态栏 / 外壳。
///
/// 传入 [`ConfigInfo`] 而不是在这里重新读一遍配置：调用方（`main`）已经为了
/// 架构检测读过一次，重复读会多一遍文件 IO，还可能读到两次之间被改动过的内容。
pub fn probe(config: &ConfigInfo) -> Option<&'static str> {
    from_config(&config.sources).or_else(from_processes)
}

/// 第一级：从配置正文里的 spawn 指令找。
fn from_config(sources: &[String]) -> Option<&'static str> {
    sources
        .iter()
        .flat_map(|text| startup_programs(text))
        .find_map(|program| match_bar(&program))
}

/// 第二级：从正在运行的进程里找。
///
/// 只有第一级落空才会走到这里 —— [`sys::process_names`] 是惰性迭代器，
/// 一旦命中就立刻停下，不会白扫完剩下的进程。
fn from_processes() -> Option<&'static str> {
    sys::process_names().find_map(|name| match_bar(&name))
}

/// 把进程名 / 命令名规范化成展示名。
///
/// 用**精确匹配**而不是前缀匹配：`st`（simple terminal）是终端、`steam` 是游戏，
/// 前缀匹配会把后者也吞进来。同理 `dms` 不该匹配上 `dmsomething`。
/// 大小写不敏感是因为进程名在 Linux 上大小写混用很常见（`Eww` / `eww`）。
fn match_bar(name: &str) -> Option<&'static str> {
    let name = name.trim();
    BARS.iter()
        .find(|(needle, _)| name.eq_ignore_ascii_case(needle))
        .map(|(_, display)| *display)
}

// ════════════════════════════════════════════════════════════════════
//  spawn 指令扫描
// ════════════════════════════════════════════════════════════════════

/// 启动类指令的名字，长的在前 —— 前缀判断按顺序做，先匹配更具体的那个更安全。
const STARTUP_DIRECTIVES: [&str; 2] = ["spawn-sh-at-startup", "spawn-at-startup"];

/// 从一份配置正文里挑出所有「启动时会跑的程序名」。
///
/// 只认**行首**是启动指令的行。这一条同时解决两个问题：
///
/// - `binds { Mod+D { spawn-sh "noctalia msg …" } }` 里的 `spawn-sh` 是按键触发的，
///   不是常驻程序，行首是键位名所以天然被排除；
/// - `spawn` / `spawn-sh`（不带 `-at-startup`）也不再需要单独排除。
fn startup_programs(text: &str) -> Vec<String> {
    text.lines()
        .map(config::strip_kdl_comment)
        .map(str::trim)
        .filter(|line| !line.starts_with("/-"))
        .filter_map(split_directive)
        .flat_map(programs_of)
        .collect()
}

/// 拆出 `(指令名, 指令后面的原文)`；行首不是启动指令则返回 `None`。
fn split_directive(line: &str) -> Option<(&'static str, &str)> {
    STARTUP_DIRECTIVES.iter().find_map(|directive| {
        let rest = line.strip_prefix(directive)?;
        // 前缀后面必须跟空白或引号，否则 `spawn-at-startup-foo` 之类的
        // 同前缀标识符会被误判。
        (rest.starts_with(char::is_whitespace) || rest.starts_with('"'))
            .then_some((*directive, rest))
    })
}

/// 一条启动指令里所有「可能被拉起来的程序名」。
///
/// 两种形态分开处理：
/// - `spawn-at-startup "waybar" "-c" "…"` —— KDL 参数列表，第一个字符串就是程序；
/// - `spawn-sh-at-startup "pkill waybar; waybar &"` —— 整串交给 `sh -c`，
///   得先按 shell 控制符切成命令段，再逐段取首个词。
///
/// 两者都只取**首个词**：`pkill waybar` 里的 `waybar` 是参数而非被启动的程序，
/// 若整串按词扫描就会把「正在杀掉 waybar」误读成「正在运行 waybar」。
fn programs_of((directive, rest): (&'static str, &str)) -> Vec<String> {
    let Some(command) = first_string_literal(rest) else {
        return Vec::new();
    };

    if directive == "spawn-at-startup" {
        // 非 shell 形态：第一个字面量本身就是程序路径。
        return basename(&command).into_iter().collect();
    }

    command
        .split([';', '|', '&', '\n'])
        .filter_map(first_word)
        .filter_map(basename)
        .collect()
}

/// 取一个 shell 命令段里真正被执行的那个词，跳过 `exec` / `env` 这类包装。
fn first_word(segment: &str) -> Option<&str> {
    segment.split_whitespace().find(|word| !is_wrapper(word))
}

/// 判断一个词是不是「包装词」—— 它后面才是真正的程序。
fn is_wrapper(word: &str) -> bool {
    const WRAPPERS: [&str; 8] = [
        "exec", "nohup", "setsid", "env", "command", "nice", "stdbuf", "time",
    ];
    if WRAPPERS.contains(&word) {
        return true;
    }
    // `env FOO=bar waybar` 里的环境变量赋值也要跳过。只认「全大写 / 下划线 /
    // 数字」组成的键名，免得把 `--config=x` 这种选项当成赋值。
    word.split_once('=').is_some_and(|(key, _)| {
        !key.is_empty()
            && key
                .chars()
                .all(|c| c.is_ascii_uppercase() || c == '_' || c.is_ascii_digit())
    })
}

/// 取出文本里第一个 KDL 字符串字面量的内容。
fn first_string_literal(text: &str) -> Option<String> {
    let rest = text.split_once('"')?.1;
    let (value, _) = rest.split_once('"')?;
    Some(value.to_owned())
}

/// 从命令里取可执行文件名：`/usr/bin/waybar` 与 `waybar` 都归到 `waybar`。
///
/// 命令里可能带引号（`spawn-at-startup "\"waybar\""` 这种写法罕见但合法），
/// 一并剥掉；结果为空则返回 `None`。
fn basename(command: &str) -> Option<String> {
    let command = command.trim().trim_matches('"').trim();
    if command.is_empty() {
        return None;
    }
    let name = command.rsplit('/').next().unwrap_or(command).trim();
    (!name.is_empty()).then(|| name.to_owned())
}

// ════════════════════════════════════════════════════════════════════
//  测试
// ════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    /// 把一段配置正文跑完整条「扫描 → 匹配」链路，返回展示名。
    fn detect(text: &str) -> Option<&'static str> {
        from_config(&[text.to_owned()])
    }

    // ── 名称规范化 ────────────────────────────────────────────────

    #[test]
    fn maps_every_supported_bar_to_its_display_name() {
        for (needle, expected) in [
            ("waybar", "Waybar"),
            ("noctalia", "Noctalia Shell"),
            ("noctalia-shell", "Noctalia Shell"),
            ("dms", "Dank Material Shell (DMS)"),
            ("eww", "Eww"),
            ("ironbar", "Ironbar"),
            ("ags", "AGS"),
        ] {
            assert_eq!(match_bar(needle), Some(expected), "{needle} 的展示名不对");
        }
    }

    #[test]
    fn matching_ignores_case_and_surrounding_space() {
        assert_eq!(match_bar("Waybar"), Some("Waybar"));
        assert_eq!(match_bar("  EWW "), Some("Eww"));
        assert_eq!(match_bar("DMS"), Some("Dank Material Shell (DMS)"));
    }

    #[test]
    fn matching_is_exact_not_prefix() {
        // `st` 是终端、`steam` 是游戏；`dms` 不该吞掉 `dmsomething`。
        // 前缀匹配在这里会制造假阳性，所以宁可什么都不匹配。
        for name in ["dmshelper", "waybar-extra", "ewwish", "astal-io", ""] {
            assert_eq!(match_bar(name), None, "{name} 不该被匹配");
        }
    }

    // ── 配置扫描：能认出来的形态 ──────────────────────────────────

    #[test]
    fn finds_a_bare_spawn_at_startup() {
        // 本机 config.kdl 的真实写法。
        assert_eq!(
            detect("spawn-at-startup \"noctalia\"\n"),
            Some("Noctalia Shell")
        );
    }

    #[test]
    fn finds_a_spawn_at_startup_with_arguments() {
        let text = "spawn-at-startup \"waybar\" \"-c\" \"~/.config/waybar/config\"\n";
        assert_eq!(detect(text), Some("Waybar"));
    }

    #[test]
    fn finds_a_program_hidden_behind_a_shell_string() {
        // spawn-sh 形态：整串交给 shell，程序在 `&` 后面。
        assert_eq!(
            detect("spawn-sh-at-startup \"sleep 2 && waybar &\"\n"),
            Some("Waybar")
        );
        // `exec` 包装词要跳过。
        assert_eq!(
            detect("spawn-sh-at-startup \"exec ironbar\"\n"),
            Some("Ironbar")
        );
        // `env KEY=VALUE prog` 形态。
        assert_eq!(
            detect("spawn-sh-at-startup \"env GDK_BACKEND=wayland eww daemon\"\n"),
            Some("Eww")
        );
    }

    #[test]
    fn accepts_absolute_paths_and_leading_whitespace() {
        assert_eq!(
            detect("    spawn-at-startup \"/usr/bin/waybar\"\n"),
            Some("Waybar")
        );
    }

    #[test]
    fn finds_a_bar_declared_in_an_included_file() {
        // 状态栏写在被 include 的文件里同样要能找到 —— 这正是
        // `ConfigInfo::sources` 把全部正文都带上的理由。
        let sources = vec![
            "// 主配置\ninclude \"bar.kdl\"\n".to_owned(),
            "spawn-at-startup \"waybar\"\n".to_owned(),
        ];
        assert_eq!(from_config(&sources), Some("Waybar"));
    }

    // ── 配置扫描：必须排除的形态 ──────────────────────────────────

    #[test]
    fn ignores_keybind_spawns() {
        // binds 里的 spawn / spawn-sh 是按键触发的，不是常驻程序。
        // 本机 binds.kdl 里就有一堆 `spawn-sh "noctalia msg …"`。
        let text = concat!(
            "    Mod+D hotkey-overlay-title=\"启动器\" { spawn-sh \"noctalia msg panel-toggle launcher\"; }\n",
            "    Mod+T { spawn \"kitty\" \"-e\" \"fish\"; }\n",
        );
        assert_eq!(detect(text), None);
    }

    #[test]
    fn ignores_commented_out_spawns() {
        let text = concat!(
            "// spawn-at-startup \"waybar\"\n",
            "/- spawn-at-startup \"waybar\"\n",
            "spawn-at-startup \"eww\" // spawn-at-startup \"ironbar\"\n",
        );
        assert_eq!(detect(text), Some("Eww"));
    }

    #[test]
    fn ignores_identifiers_sharing_the_prefix() {
        assert_eq!(detect("spawn-at-startup-helper \"waybar\"\n"), None);
        assert_eq!(detect("spawn-at-startupish \"waybar\"\n"), None);
    }

    #[test]
    fn does_not_mistake_an_argument_for_the_program() {
        // `pkill waybar` 是在**杀掉** waybar，把它读成「正在运行 waybar」是最
        // 容易犯的错。只取每个命令段的首个词就不会踩到。
        assert_eq!(detect("spawn-sh-at-startup \"pkill waybar\"\n"), None);
        assert_eq!(detect("spawn-at-startup \"pkill\" \"waybar\"\n"), None);
    }

    #[test]
    fn skips_directives_with_no_string_argument() {
        // 语法不完整（或写成 KDL 原始字符串）的行不该 panic，也不该产出垃圾。
        for text in [
            "spawn-at-startup\n",
            "spawn-sh-at-startup \n",
            "spawn-at-startup \"\"\n",
        ] {
            assert_eq!(detect(text), None, "{text:?} 不该产出结果");
        }
    }

    #[test]
    fn reports_none_when_nothing_matches() {
        // 配置里只有别的启动项，且没有 bar 在跑。
        let text = "spawn-at-startup \"fcitx5\" \"-d\"\n";
        assert_eq!(detect(text), None);
        assert_eq!(from_config(&[]), None);
    }

    #[test]
    fn picks_the_first_bar_in_configuration_order() {
        // 同时写了两套时按配置里的先后顺序取第一个 —— 后写的通常是被
        // 临时试用的那套，先写的才是日常在用的。
        let text = "spawn-at-startup \"waybar\"\nspawn-at-startup \"eww\"\n";
        assert_eq!(detect(text), Some("Waybar"));
    }

    // ── 进程补充路径 ──────────────────────────────────────────────

    #[test]
    fn process_scan_runs_without_panicking() {
        // 这台机器上跑没跑 bar 是未知的，所以不断言具体结果，
        // 只断言「扫描本身能跑通、不 panic」。
        let found = from_processes();
        assert!(
            found.is_none() || found.is_some_and(|name| !name.is_empty()),
            "扫描结果异常：{found:?}"
        );
    }

    #[test]
    fn wrapper_detection_covers_the_usual_launchers() {
        for word in ["exec", "nohup", "setsid", "env", "nice", "FOO=bar", "A_B=1"] {
            assert!(is_wrapper(word), "{word} 应被当作包装词");
        }
        for word in ["waybar", "--config=x", "lower=case", "="] {
            assert!(!is_wrapper(word), "{word} 不该被当作包装词");
        }
    }

    #[test]
    fn basename_strips_directories_and_quotes() {
        assert_eq!(basename("/usr/bin/waybar").as_deref(), Some("waybar"));
        assert_eq!(basename("waybar").as_deref(), Some("waybar"));
        assert_eq!(basename("  \"eww\"  ").as_deref(), Some("eww"));
        assert_eq!(basename("   "), None);
        assert_eq!(basename("/"), None);
    }
}
