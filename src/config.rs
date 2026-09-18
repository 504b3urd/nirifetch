//! 静态配置读取层。
//!
//! 职责边界：**定位并读取 niri 的 `config.kdl`，产出描述这个文件的元信息**，
//! 其中最重要的是「配置架构检测」——判断用户用的是单体配置还是模块化拆分。
//! 本模块不解析完整的 KDL 语法，那需要引入 `kdl` 这类较重的依赖，而
//! nirifetch 只需要知道「配置在哪、有多大、被拆成了几块」，轻量扫描即可。
//!
//! 容错原则：配置缺失、无读权限、编码非法、文件超大，都不会让程序失败，
//! 只是对应的字段变成 `None` / [`ConfigStructure::Unknown`]，由 UI 兜底展示。

use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::sys;

/// 读取配置文件时的字节上限。
///
/// 正常情况下 config.kdl 只有几 KB；设上限是为了防御「路径被软链到
/// 巨型文件或设备节点」这类意外，避免把整个文件读进内存。
const MAX_CONFIG_BYTES: u64 = 256 * 1024;

/// include 展开时允许的最大嵌套深度。
///
/// niri 允许被 include 的文件再 include 别人，但真实配置里嵌套很少超过两层。
/// 设上限是为了防住「A include B、B include A」这种成环写法 —— 虽然下面的
/// [`HashSet`] 去重已经能兜住环，深度上限是第二道保险。
const MAX_INCLUDE_DEPTH: usize = 4;

/// include 展开时读取的文件总数上限（含主文件）。
const MAX_INCLUDE_FILES: usize = 32;

/// include 展开后所有文件正文的**合计**字节上限。
///
/// 单文件上限 256 KiB 乘以上限 32 个文件，最坏情况会留下 8 MiB 常驻内存。
/// 真实配置总共只有几十 KB，这里再压一道，让病态配置也只是被截断。
const MAX_TOTAL_SOURCE_BYTES: usize = 1024 * 1024;

/// 配置文件是从哪个位置找到的。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigSource {
    /// 由 `NIRI_CONFIG` 环境变量显式指定，优先级最高。
    EnvOverride,
    /// 用户级配置，即 `$XDG_CONFIG_HOME/niri/config.kdl`。
    User,
    /// 系统级兜底配置 `/etc/niri/config.kdl`。
    System,
}

impl ConfigSource {
    /// 供 UI 展示的说明。用户级配置是默认情况，无需在界面上标注，故返回 `None`。
    pub fn label(self) -> Option<&'static str> {
        match self {
            ConfigSource::EnvOverride => Some("via $NIRI_CONFIG"),
            ConfigSource::System => Some("system"),
            ConfigSource::User => None,
        }
    }
}

/// 配置的**组织形态**。
///
/// niri 允许用 `include` 把配置拆成多个文件（见 niri wiki: Configuration →
/// Multiple files）。社区里通常称拆分写法为 "modular config"，单文件为
/// "monolithic config"，这里沿用这套叫法。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigStructure {
    /// 单体配置：只有一个主文件，内部没有 `include`。
    Monolithic,
    /// 模块化配置：主文件引入了 `includes` 个子配置。
    Modular { includes: usize },
    /// 无法判定 —— 配置文件不存在或读不出来。
    Unknown,
}

impl fmt::Display for ConfigStructure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigStructure::Monolithic => f.write_str("Monolithic (Single)"),
            ConfigStructure::Modular { includes } => {
                write!(f, "Modular ({includes} included files)")
            }
            ConfigStructure::Unknown => f.write_str("Unknown"),
        }
    }
}

/// 配置文件的基本信息快照。
#[derive(Debug, Clone)]
pub struct ConfigInfo {
    /// 解析出的配置路径（无论文件是否存在都会给出「期望路径」）。
    pub path: PathBuf,
    /// 该路径的来历。
    pub source: ConfigSource,
    /// 文件当前是否存在。
    pub exists: bool,
    /// 文件大小（字节）。
    pub size: Option<u64>,
    /// 总行数。
    pub lines: Option<usize>,
    /// 配置架构检测结果。
    pub structure: ConfigStructure,
    /// 主文件正文，**加上全部 `include` 展开后的文件正文**（按引入顺序）。
    ///
    /// 供需要「跨文件找某条指令」的调用方使用 —— 目前是 [`crate::bar`]：
    /// 状态栏往往 `spawn-at-startup` 在主文件里，也可能写在任意一个被 include
    /// 的文件里，只扫主文件会漏。主文件读不出来时为空。
    pub sources: Vec<String>,
}

/// 定位配置路径并采集其元信息。这是本模块对外的唯一入口。
pub fn inspect() -> ConfigInfo {
    let (path, source) = resolve_path();
    inspect_path(path, source)
}

/// 采集**指定路径**配置文件的元信息。
///
/// 与 [`inspect`] 拆开是为了让测试能直接指向临时目录：路径解析依赖真实的
/// 环境变量与用户目录，把「解析路径」与「读取这个路径」分成两步，后者就能
/// 脱离环境单独测。
fn inspect_path(path: PathBuf, source: ConfigSource) -> ConfigInfo {
    // `metadata()` 会跟随符号链接 —— 这正是我们要的：
    // 很多用户把 config.kdl 软链到 dotfiles 仓库里。
    let meta = std::fs::metadata(&path).ok();
    let exists = meta.is_some();
    let size = meta.as_ref().map(|m| m.len());

    let text = read_capped(&path);
    let lines = text.as_ref().map(|t| t.lines().count());
    let structure = detect_structure(&path, text.as_deref());
    // include 展开要读被引入的文件，必须赶在 `path` 被移动进结构体之前做。
    let sources = collect_sources(&path, text);

    ConfigInfo {
        path,
        source,
        exists,
        size,
        lines,
        structure,
        sources,
    }
}

// ════════════════════════════════════════════════════════════════════
//  include 展开
// ════════════════════════════════════════════════════════════════════

/// 把主文件与它 include 的全部文件正文收集成一个列表（主文件在前）。
///
/// 这一步**只服务于「跨文件搜索指令」**，不参与架构判定 —— 架构判定用的是
/// [`count_includes`]，数的是主文件里写了几条 include，与能否读出被引入的
/// 文件无关。配置读不出来、include 指向不存在的文件，都只是少几段正文，
/// 绝不报错。
fn collect_sources(path: &Path, text: Option<String>) -> Vec<String> {
    let Some(text) = text else {
        return Vec::new();
    };

    // visited 用规范化后的绝对路径做键：软链与 `./` `../` 都能归一到同一个文件，
    // 否则 `include "./b.kdl"` 与 `include "b.kdl"` 会被当成两个文件重复读。
    let mut visited = HashSet::new();
    visited.insert(visit_key(path));
    let mut budget = MAX_TOTAL_SOURCE_BYTES.saturating_sub(text.len());

    let mut sources = Vec::new();
    expand(path, text, 0, &mut visited, &mut sources, &mut budget);
    sources
}

/// 递归展开 `text` 里的 `include`，把每个文件正文按「父先于子」的顺序压入 `sources`。
///
/// `depth` 是当前文件所处的嵌套层数（主文件为 0）。`text` 按值传入，
/// 这样它能被直接移动进 `sources`，不必为了递归再克隆一份。
fn expand(
    owner: &Path,
    text: String,
    depth: usize,
    visited: &mut HashSet<PathBuf>,
    sources: &mut Vec<String>,
    budget: &mut usize,
) {
    if depth >= MAX_INCLUDE_DEPTH {
        return;
    }

    // 先把自己放进去，再处理子文件 —— 与人在编辑器里顺着 include 往下读的顺序一致。
    let targets = include_targets(&text);
    sources.push(text);

    for raw in targets {
        if sources.len() >= MAX_INCLUDE_FILES || *budget == 0 {
            return;
        }

        let target = resolve_include(owner, &raw);
        // 同一个文件被 include 两次（或成环）时只读一次。
        if !visited.insert(visit_key(&target)) {
            continue;
        }

        let Some(body) = read_capped(&target) else {
            continue;
        };
        *budget = budget.saturating_sub(body.len());
        expand(&target, body, depth + 1, visited, sources, budget);
    }
}

/// 逐行挑出顶层 `include "路径"` 的目标路径。
///
/// 与 [`count_includes`] 共用同一套「剥注释 + 行首锚定 + 前缀后必须跟空白或引号」
/// 的识别规则，两者对「什么算一条 include」的判断因此永远一致。
fn include_targets(text: &str) -> Vec<String> {
    text.lines()
        .map(strip_kdl_comment)
        .map(str::trim)
        .filter(|line| !line.starts_with("/-"))
        .filter_map(|line| line.strip_prefix("include"))
        .filter(|rest| rest.starts_with(char::is_whitespace) || rest.starts_with('"'))
        .filter_map(first_string_literal)
        .collect()
}

/// 取出文本里第一个 KDL 字符串字面量的内容。
///
/// 不处理 `\"` 转义与 `r#"..."#` 原始字符串：出现在**文件路径**里的概率极低，
/// 而多写一层转义解析只会让这段更难读。取不到就返回 `None`，不影响其它行。
fn first_string_literal(text: &str) -> Option<String> {
    let rest = text.split_once('"')?.1;
    let (value, _) = rest.split_once('"')?;
    Some(value.to_owned())
}

/// 把 include 里写的路径变成实际可读的路径。
///
/// niri 的相对路径基准是**被 include 文件自己所在的目录**，不是主配置的目录，
/// 所以这里必须用 `owner.parent()` 而不是固定的主配置目录。
fn resolve_include(owner: &Path, raw: &str) -> PathBuf {
    // `~/...` 是 shell 习惯写法，PathBuf::join 不认识它，得先手工展开。
    if let Some(rest) = raw.strip_prefix("~/")
        && let Some(home) = sys::env_non_empty("HOME")
    {
        return PathBuf::from(home).join(rest);
    }

    let candidate = Path::new(raw);
    if candidate.is_absolute() {
        return candidate.to_path_buf();
    }

    match owner.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.join(candidate),
        _ => candidate.to_path_buf(),
    }
}

/// 用于去重的键：优先用规范化路径（解析软链与 `..`），失败时退回原路径。
fn visit_key(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

// ════════════════════════════════════════════════════════════════════
//  配置架构检测
// ════════════════════════════════════════════════════════════════════

/// 综合两个信号判断配置架构。
///
/// - **主信号**：主文件里的 `include` 语句数量。这是最直接的证据 ——
///   niri 只认主文件里的 include，数出来几个就是拆成了几块。
/// - **辅信号**：同目录下的兄弟 `.kdl` 文件数量。用于兜住主信号失效的场景：
///   例如嵌套拆分（include 写在被 include 的文件里），或用户只是把文件
///   摆在那儿、靠目录约定组织。
///
/// 两个信号都为 0 才判定为单体配置。
fn detect_structure(path: &Path, text: Option<&str>) -> ConfigStructure {
    let Some(text) = text else {
        // 配置不存在或读不出来，无从判断。
        return ConfigStructure::Unknown;
    };
    decide_structure(count_includes(text), count_sibling_kdl_files(path))
}

/// 纯函数形式的判定规则，便于穷举测试各种信号组合。
fn decide_structure(declared_includes: usize, sibling_files: usize) -> ConfigStructure {
    if declared_includes > 0 {
        ConfigStructure::Modular {
            includes: declared_includes,
        }
    } else if sibling_files > 0 {
        ConfigStructure::Modular {
            includes: sibling_files,
        }
    } else {
        ConfigStructure::Monolithic
    }
}

/// 统计主配置**同目录**下的兄弟 `.kdl` 文件数量（不含主配置自身）。
///
/// 会排除备份与编辑器临时文件，否则一次 `cp config.kdl config.kdl.bak`
/// 就会把单体配置误判成模块化。
fn count_sibling_kdl_files(main: &Path) -> usize {
    let Some(main_name) = main.file_name().and_then(|n| n.to_str()) else {
        return 0;
    };

    // 相对路径（如 `config.kdl`）的 parent() 是空串，read_dir 会失败，
    // 这里按当前目录处理。
    let dir = match main.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };

    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };

    entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name != main_name)
        .filter(|name| !is_backup_like(name))
        // 用 Path::extension 而不是切字节：后者遇到非 ASCII 文件名
        // 可能在字符边界上切错导致 panic。
        .filter(|name| {
            Path::new(name)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("kdl"))
        })
        .count()
}

/// 判断文件名是否像备份 / 编辑器临时文件。
fn is_backup_like(name: &str) -> bool {
    name.starts_with('.')          // 隐藏文件，含 `.#foo`（Emacs）与 `.foo.kdl.swp`
        || name.starts_with('#')   // `#foo#`（Emacs 自动保存）
        || name.ends_with('~')     // `foo~`（各类编辑器的备份）
        || name.contains(".bak")   // `config.kdl.bak` / `config.kdl.bak-2026-09-13`
        || name.ends_with(".orig")
        || name.ends_with(".swp")
}

/// 剥掉 `//` 之后的行内注释（KDL 沿用 C 风格的单行注释）。
///
/// 供本模块的 include 识别与 [`crate::bar`] 的 spawn 指令扫描共用，
/// 保证「什么算注释」在两个模块里是同一套判断。
pub(crate) fn strip_kdl_comment(line: &str) -> &str {
    line.split("//").next().unwrap_or("")
}

/// 统计顶层 `include` 指令数量。
///
/// 只做行级扫描，不构建语法树：
/// - 先剥掉 `//` 之后的行内注释（见 [`strip_kdl_comment`]）；
/// - 跳过 KDL 的 `/-` 注释行；
/// - 行首为 `include` 且其后紧跟空白或引号才算数（排除 `includes` 这类同前缀标识符）。
///
/// 已知局限：不处理 `/* */` 块注释和字符串字面量里的 `//`。对一个只用来
/// 展示「配置被拆成了几块」的辅助信息而言，这个精度足够。
fn count_includes(text: &str) -> usize {
    text.lines()
        .map(strip_kdl_comment)
        .map(str::trim)
        .filter(|line| !line.starts_with("/-"))
        .filter(|line| {
            line.strip_prefix("include")
                .is_some_and(|rest| rest.starts_with(char::is_whitespace) || rest.starts_with('"'))
        })
        .count()
}

// ════════════════════════════════════════════════════════════════════
//  路径解析
// ════════════════════════════════════════════════════════════════════

/// 按优先级确定配置文件路径。
///
/// 顺序与 niri 自身的查找逻辑保持一致：
/// 1. `$NIRI_CONFIG`（命令行 `-c` 的等价物）；
/// 2. `$XDG_CONFIG_HOME/niri/config.kdl`，`XDG_CONFIG_HOME` 未设置时
///    按 XDG 规范回退到 `~/.config`；
/// 3. `/etc/niri/config.kdl`。
///
/// 三者都不存在时，返回第 2 条的路径并标记为「不存在」—— 这是 niri 在
/// 用户从未写过配置时会去创建的位置，对用户最有参考价值。
fn resolve_path() -> (PathBuf, ConfigSource) {
    if let Some(custom) = sys::env_non_empty("NIRI_CONFIG") {
        return (PathBuf::from(custom), ConfigSource::EnvOverride);
    }

    if let Some(user) = user_config_path() {
        if user.exists() {
            return (user, ConfigSource::User);
        }
        // 用户配置不存在时先看系统配置，都没有再回落到这里。
        let system = PathBuf::from("/etc/niri/config.kdl");
        if system.exists() {
            return (system, ConfigSource::System);
        }
        return (user, ConfigSource::User);
    }

    (PathBuf::from("/etc/niri/config.kdl"), ConfigSource::System)
}

/// `$XDG_CONFIG_HOME/niri/config.kdl`，必要时用 `$HOME/.config` 兜底。
fn user_config_path() -> Option<PathBuf> {
    if let Some(xdg) = sys::env_non_empty("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(xdg).join("niri").join("config.kdl"));
    }
    let home = sys::env_non_empty("HOME")?;
    Some(
        PathBuf::from(home)
            .join(".config")
            .join("niri")
            .join("config.kdl"),
    )
}

/// 带字节上限地读取配置文件文本。上限与容错细节见 [`sys::read_capped`]。
fn read_capped(path: &Path) -> Option<String> {
    sys::read_capped(path, MAX_CONFIG_BYTES)
}

// ════════════════════════════════════════════════════════════════════
//  测试
// ════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_top_level_includes() {
        let text = "include \"a.kdl\"\ninclude \"b.kdl\"\n\nlayout {\n}\n";
        assert_eq!(count_includes(text), 2);
    }

    #[test]
    fn counts_indented_includes() {
        let text = "  include \"a.kdl\"\n\tinclude \"b.kdl\"\n";
        assert_eq!(count_includes(text), 2);
    }

    #[test]
    fn ignores_commented_out_includes() {
        // 整行注释与行内注释都不该计入。
        let text = "// include \"a.kdl\"\ninclude \"b.kdl\" // include \"c.kdl\"\n";
        assert_eq!(count_includes(text), 1);
    }

    #[test]
    fn ignores_slashdash_commented_includes() {
        // `/-` 是 KDL 的注释语法，被注释掉的节点不生效。
        let text = "/- include \"a.kdl\"\ninclude \"b.kdl\"\n";
        assert_eq!(count_includes(text), 1);
    }

    #[test]
    fn does_not_match_identifiers_sharing_the_prefix() {
        // `includes` / `include-path` 都是别的标识符，不能被误判成 include。
        let text = "includes 3\ninclude-path \"x\"\ninclude \"real.kdl\"\n";
        assert_eq!(count_includes(text), 1);
    }

    #[test]
    fn handles_empty_and_include_free_configs() {
        assert_eq!(count_includes(""), 0);
        assert_eq!(count_includes("layout {\n    gaps 8\n}\n"), 0);
    }

    // ── 架构判定 ──────────────────────────────────────────────────

    #[test]
    fn declared_includes_win_over_sibling_count() {
        // 主信号优先：主文件明写 3 个 include 时，就以 3 为准，
        // 哪怕目录里还散落着别的 kdl。
        assert_eq!(
            decide_structure(3, 7),
            ConfigStructure::Modular { includes: 3 }
        );
    }

    #[test]
    fn siblings_are_the_fallback_signal() {
        // 主文件没有 include，但目录里有拆出来的子配置 —— 仍算模块化。
        assert_eq!(
            decide_structure(0, 4),
            ConfigStructure::Modular { includes: 4 }
        );
    }

    #[test]
    fn no_signal_means_monolithic() {
        assert_eq!(decide_structure(0, 0), ConfigStructure::Monolithic);
    }

    #[test]
    fn missing_config_is_unknown() {
        // 读不到主文件时不猜，直接 Unknown。
        assert_eq!(
            detect_structure(Path::new("/nonexistent/config.kdl"), None),
            ConfigStructure::Unknown
        );
    }

    #[test]
    fn structure_renders_the_documented_wording() {
        assert_eq!(
            ConfigStructure::Monolithic.to_string(),
            "Monolithic (Single)"
        );
        assert_eq!(
            ConfigStructure::Modular { includes: 6 }.to_string(),
            "Modular (6 included files)"
        );
        assert_eq!(ConfigStructure::Unknown.to_string(), "Unknown");
    }

    // ── 目录扫描 ──────────────────────────────────────────────────

    #[test]
    fn counts_siblings_and_skips_backups() {
        // 用独立目录，避免污染真实环境；文件名带上 pid 防止并发测试互踩。
        let dir = std::env::temp_dir().join(format!("nirifetch-cfg-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("创建临时目录");

        let main = dir.join("config.kdl");
        std::fs::write(&main, "// 主配置\n").expect("写入主配置");

        for name in ["binds.kdl", "layout.kdl", "input.kdl"] {
            std::fs::write(dir.join(name), "// 子配置\n").expect("写入子配置");
        }
        // 下面这些都不该被计入：备份、编辑器临时文件、非 kdl 文件
        for name in [
            "config.kdl.bak",
            "config.kdl.bak-2026-09-13-split",
            "binds.kdl.orig",
            ".#layout.kdl",
            "input.kdl~",
            "notes.txt",
        ] {
            std::fs::write(dir.join(name), "x").expect("写入干扰文件");
        }

        assert_eq!(count_sibling_kdl_files(&main), 3);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sibling_count_survives_bad_directory() {
        // 目录不存在时返回 0，不能 panic。
        assert_eq!(
            count_sibling_kdl_files(Path::new("/nonexistent-dir/config.kdl")),
            0
        );
    }

    // ── include 展开 ──────────────────────────────────────────────

    /// 直接对临时目录里的文件跑一遍完整的采集流程。
    fn inspect_at(path: &Path) -> ConfigInfo {
        inspect_path(path.to_path_buf(), ConfigSource::User)
    }

    /// 建一个只属于本测试的临时目录（文件名带 pid，避免并发测试互踩）。
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nirifetch-inc-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("创建临时目录");
        dir
    }

    #[test]
    fn include_targets_ignores_comments_and_prefixes() {
        let text = concat!(
            "// include \"commented.kdl\"\n",
            "include \"real.kdl\" // 行内注释不影响\n",
            "/- include \"disabled.kdl\"\n",
            "includes \"typo.kdl\"\n",
            "layout {\n    include \"nested-is-not-top-level? anyway matched\"\n}\n",
        );
        // 本函数只管「行首是 include」，是否在块内由 niri 自己校验；
        // 这里要守住的是注释与被注释掉的行不能混进来。
        assert_eq!(
            include_targets(text),
            vec![
                "real.kdl".to_owned(),
                "nested-is-not-top-level? anyway matched".to_owned()
            ]
        );
    }

    #[test]
    fn expands_includes_relative_to_the_including_file() {
        // 相对路径的基准是**被 include 文件自己所在的目录**，不是主配置目录。
        // sub/a.kdl 里写 include "b.kdl"，指向的是 sub/b.kdl。
        let dir = scratch("relative");
        std::fs::write(
            dir.join("config.kdl"),
            "include \"sub/a.kdl\"\nspawn-at-startup \"top\"\n",
        )
        .expect("写主配置");
        std::fs::create_dir_all(dir.join("sub")).expect("建子目录");
        std::fs::write(dir.join("sub/a.kdl"), "include \"b.kdl\"\n").expect("写 a.kdl");
        std::fs::write(dir.join("sub/b.kdl"), "spawn-at-startup \"deep\"\n").expect("写 b.kdl");

        let info = inspect_at(&dir.join("config.kdl"));

        assert_eq!(info.sources.len(), 3, "主文件 + a.kdl + b.kdl");
        // 主文件在前；父先于子。
        assert!(info.sources[0].contains("top"));
        assert!(info.sources[1].contains("include"));
        assert!(info.sources[2].contains("deep"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn include_cycles_terminate_and_are_read_once() {
        // a include b、b include a：既要能停下来，也不能把同一个文件读两遍。
        let dir = scratch("cycle");
        std::fs::write(dir.join("config.kdl"), "include \"a.kdl\"\n").expect("写主配置");
        std::fs::write(dir.join("a.kdl"), "include \"b.kdl\"\n").expect("写 a.kdl");
        std::fs::write(dir.join("b.kdl"), "include \"a.kdl\"\n").expect("写 b.kdl");

        let info = inspect_at(&dir.join("config.kdl"));

        assert_eq!(info.sources.len(), 3, "成环时每个文件只读一次");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_includes_are_skipped_without_failing() {
        let dir = scratch("missing");
        let main = dir.join("config.kdl");
        std::fs::write(&main, "include \"nope.kdl\"\ninclude \"also-gone.kdl\"\n")
            .expect("写主配置");

        let info = inspect_at(&main);

        // 展开失败不影响主文件本身，也不影响架构判定。
        assert_eq!(info.sources.len(), 1);
        assert_eq!(info.structure, ConfigStructure::Modular { includes: 2 });

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_include_handles_absolute_and_tilde_free_paths() {
        let owner = Path::new("/etc/niri/config.kdl");
        assert_eq!(
            resolve_include(owner, "binds.kdl"),
            PathBuf::from("/etc/niri/binds.kdl")
        );
        assert_eq!(
            resolve_include(owner, "/opt/shared/x.kdl"),
            PathBuf::from("/opt/shared/x.kdl")
        );
        // 没有父目录时按当前目录处理，不能拼出 `/x.kdl` 这样的绝对路径。
        assert_eq!(
            resolve_include(Path::new("config.kdl"), "binds.kdl"),
            PathBuf::from("binds.kdl")
        );
    }

    #[test]
    fn unreadable_config_yields_no_sources() {
        assert!(collect_sources(Path::new("/nonexistent/config.kdl"), None).is_empty());
    }

    #[test]
    fn backup_detection_covers_editor_artifacts() {
        for name in [
            ".#x.kdl",
            "#x.kdl#",
            "x.kdl~",
            "x.kdl.bak",
            "x.kdl.orig",
            "x.kdl.swp",
        ] {
            assert!(is_backup_like(name), "{name} 应被识别为备份");
        }
        for name in ["config.kdl", "binds.kdl", "layout.kdl"] {
            assert!(!is_backup_like(name), "{name} 不应被识别为备份");
        }
    }
}
