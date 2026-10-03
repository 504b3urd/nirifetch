//! Niri IPC 交互层。
//!
//! 职责边界：**只负责向运行中的 niri 索取动态状态**，并以子进程方式调用
//! `niri msg --json <子命令>`，把返回的 JSON 反序列化成 Rust 结构体。
//! 本模块不做任何排版 / 着色 —— 那是 [`crate::ui`] 的事。
//!
//! 两条贯穿全模块的约定：
//!
//! 1. **绝不 panic**：不使用 `unwrap()` / `expect()` / 直接下标。任何失败
//!    （niri 未安装、socket 断开、JSON 结构随版本变化、调用超时）统一退化为
//!    `Option::None` 或空集合，由调用方决定如何兜底展示。
//! 2. **绝不挂死**：所有子进程调用都带超时（超时逻辑见 [`crate::sys::run`]）。
//!    混成器无响应时 `niri msg` 会一直阻塞，若不加超时会把 nirifetch 一起拖住。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::Deserialize;

use crate::sys;

/// 单次 IPC 调用的超时上限。正常情况这些命令都在毫秒级返回。
const IPC_TIMEOUT: Duration = Duration::from_millis(1500);

/// `niri validate` 的超时。它会完整解析配置（含全部 include），留宽一点。
const VALIDATE_TIMEOUT: Duration = Duration::from_millis(3000);

/// 用于识别 niri IPC socket 的文件名特征：`niri.<wayland_display>.<pid>.sock`
const SOCKET_PREFIX: &str = "niri.";
const SOCKET_SUFFIX: &str = ".sock";

// ════════════════════════════════════════════════════════════════════
//  会话检测
// ════════════════════════════════════════════════════════════════════

/// 会话检测失败的原因。带上上下文，方便 [`crate::ui`] 给出有针对性的提示。
#[derive(Debug, Clone)]
pub enum SessionError {
    /// 连 Wayland 会话都不在（大概率是 SSH / TTY / X11）。
    NoWayland,
    /// 在 Wayland，但当前桌面不是 niri。
    NotNiri { desktop: String },
}

/// 检测通过的会话信息。
#[derive(Debug, Clone)]
pub struct Session {
    /// 探测到的 IPC socket 路径。
    ///
    /// `None` 表示环境变量与运行时目录都没有直接证据，但桌面标识指向 niri；
    /// 此时交给 `niri msg` 自己去发现 socket。
    pub socket: Option<PathBuf>,
}

/// 判断当前进程是否运行在 niri 会话中，并尽力定位 IPC socket。
///
/// 检测按可靠性从高到低逐级回退：
///
/// 1. `NIRI_SOCKET` 已设置且路径真实存在 —— 最可靠的证据，niri 会给它启动的
///    每个进程注入这个变量。
/// 2. 扫描 `$XDG_RUNTIME_DIR` 下的 `niri.*.sock`。从别的 TTY 或 systemd
///    服务里启动 nirifetch 时，`NIRI_SOCKET` 往往缺失，但 socket 就在那儿。
/// 3. `WAYLAND_DISPLAY` 存在且 `XDG_CURRENT_DESKTOP` 含 `niri` —— 证据最弱，
///    此时仍放行，让后续 IPC 自己失败并兜底成 "Unknown"。
pub fn detect_session() -> Result<Session, SessionError> {
    // ── 第 1 级：环境变量直接命中 ──────────────────────────────────
    if let Some(sock) = sys::env_non_empty("NIRI_SOCKET").map(PathBuf::from) {
        if sock.exists() {
            return Ok(Session { socket: Some(sock) });
        }
        // 变量存在但文件已消失（多半是 niri 重启过、留下了陈旧变量）。
        // 不直接判失败，继续往下找活着的 socket。
    }

    // ── 第 2 级：扫描运行时目录 ────────────────────────────────────
    if let Some(sock) = find_socket_in_runtime_dir() {
        return Ok(Session { socket: Some(sock) });
    }

    // ── 第 3 级：靠桌面标识判断 ────────────────────────────────────
    let wayland = sys::env_non_empty("WAYLAND_DISPLAY");
    if wayland.is_none() {
        return Err(SessionError::NoWayland);
    }

    let desktop = sys::env_non_empty("XDG_CURRENT_DESKTOP").unwrap_or_default();
    if !desktop_is_niri(&desktop) {
        return Err(SessionError::NotNiri { desktop });
    }

    Ok(Session { socket: None })
}

/// 在 `$XDG_RUNTIME_DIR` 中寻找 niri 的 IPC socket。
///
/// 同一台机器可能同时跑着多个 niri 实例（多 seat / 嵌套合成），此时取
/// **修改时间最新** 的那个 —— 最有可能就是当前活跃的实例。
fn find_socket_in_runtime_dir() -> Option<PathBuf> {
    let dir = sys::env_non_empty("XDG_RUNTIME_DIR")?;
    let entries = std::fs::read_dir(dir).ok()?;

    entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(SOCKET_PREFIX) && n.ends_with(SOCKET_SUFFIX))
        })
        .filter_map(|p| {
            let mtime = std::fs::metadata(&p).ok()?.modified().ok()?;
            Some((mtime, p))
        })
        .max_by_key(|(mtime, _)| *mtime)
        .map(|(_, p)| p)
}

/// `XDG_CURRENT_DESKTOP` 是冒号分隔的列表（例如 `niri:wlroots`），逐项比对。
/// 大小写不敏感，避免不同发行版写法不一致导致误判。
fn desktop_is_niri(desktop: &str) -> bool {
    desktop
        .split(':')
        .any(|part| part.trim().eq_ignore_ascii_case("niri"))
}

// ════════════════════════════════════════════════════════════════════
//  数据模型
// ════════════════════════════════════════════════════════════════════

/// `niri msg --json focused-window` 的返回结构。
///
/// 注意：当没有任何窗口获得焦点时，niri 会返回错误而非空对象，
/// 因此调用方拿到的是 `Option`，失败即代表「无聚焦窗口」。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct FocusedWindow {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub app_id: Option<String>,
    /// 进程 PID。当前 UI 不展示，但它是窗口规则调试时最常用的字段，
    /// 保留在模型里以免后续扩展时又要回头补解析。
    #[serde(default)]
    #[allow(dead_code)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub is_floating: bool,
}

/// 单个输出（显示器）的信息。
///
/// 只解析界面真正会用到、以及挑选「主要输出」所需的字段。niri 的 JSON 里
/// 还有 `make` / `model` / `serial` 等显示器厂牌信息，界面不展示就不解析
/// —— serde 默认忽略未知字段，少解析一项就少一处随版本失效的风险。
#[derive(Debug, Clone, Deserialize)]
pub struct OutputInfo {
    pub name: String,
    /// 该输出支持的全部显示模式。
    #[serde(default)]
    pub modes: Vec<Mode>,
    /// **注意：这是 `modes` 数组的下标，不是分辨率本身。**
    #[serde(default)]
    pub current_mode: Option<usize>,
    /// 逻辑尺寸（已应用缩放），与物理分辨率不同。
    #[serde(default)]
    pub logical: Option<Logical>,
    #[serde(default)]
    pub vrr_supported: bool,
    #[serde(default)]
    pub vrr_enabled: bool,
}

/// 一种显示模式。`refresh_rate` 单位是 **毫赫兹**（120000 表示 120Hz）。
#[derive(Debug, Clone, Deserialize)]
pub struct Mode {
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub refresh_rate: u32,
    #[serde(default)]
    pub is_preferred: bool,
}

/// 输出的逻辑几何信息（已应用缩放与旋转）。
///
/// 只取 `scale` 与 `transform`：逻辑分辨率就是物理分辨率除以缩放，
/// 界面已经同时给出了这两项，再单独列一行逻辑分辨率纯属冗余。
#[derive(Debug, Clone, Deserialize)]
pub struct Logical {
    #[serde(default = "default_scale")]
    pub scale: f64,
    #[serde(default)]
    pub transform: Option<String>,
}

fn default_scale() -> f64 {
    1.0
}

impl OutputInfo {
    /// 解析出当前生效的显示模式。
    ///
    /// 三级回退，保证即使 `current_mode` 越界或缺失也能给出合理结果：
    /// 1. `modes[current_mode]` —— 正常路径；
    /// 2. 标记为 `is_preferred` 的模式；
    /// 3. 列表中的第一个模式。
    pub fn active_mode(&self) -> Option<&Mode> {
        self.current_mode
            .and_then(|idx| self.modes.get(idx))
            .or_else(|| self.modes.iter().find(|m| m.is_preferred))
            .or_else(|| self.modes.first())
    }

    /// 当前刷新率（Hz）。毫赫兹换算而来，非整数时保留一位小数。
    pub fn refresh_hz(&self) -> Option<f64> {
        let mhz = self.active_mode()?.refresh_rate;
        if mhz == 0 {
            return None;
        }
        Some(f64::from(mhz) / 1000.0)
    }

    /// 物理分辨率 `(宽, 高)`。
    pub fn physical_size(&self) -> Option<(u32, u32)> {
        let m = self.active_mode()?;
        Some((m.width, m.height))
    }

    /// 缩放系数。
    pub fn scale(&self) -> Option<f64> {
        self.logical.as_ref().map(|l| l.scale)
    }

    /// 显示变换（旋转 / 翻转），`Normal` 已被滤掉，只保留值得提示的值。
    pub fn transform(&self) -> Option<String> {
        self.logical
            .as_ref()
            .and_then(|l| l.transform.as_deref())
            .map(str::trim)
            .filter(|t| !t.is_empty() && !t.eq_ignore_ascii_case("normal"))
            .map(str::to_owned)
    }
}

/// 「主要输出」是怎么选出来的。niri 没有 X11 那种 primary output 概念，
/// 所以需要一套明确的回退策略，并把依据告诉用户。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputSource {
    /// 当前聚焦的输出 —— 最贴近「用户正在看的这块屏」。
    Focused,
    /// 系统只接了一块屏，无需选择。
    Only,
    /// 多屏且无法确定焦点，按名称排序取第一个，保证结果稳定可复现。
    First,
}

impl OutputSource {
    /// 供 UI 展示的英文说明。界面文案统一用英文，与 WM / Config 等字段保持一致。
    pub fn label(self) -> &'static str {
        match self {
            OutputSource::Focused => "Focused",
            OutputSource::Only => "Single output",
            OutputSource::First => "Auto-selected",
        }
    }
}

// ════════════════════════════════════════════════════════════════════
//  工作区 / 键盘布局
// ════════════════════════════════════════════════════════════════════

/// `niri msg --json workspaces` 里单个工作区的精简模型。
///
/// 只解析挑选「当前工作区」与计数所需的最小字段。niri 的 JSON 里还有
/// `output` / `name` / `active_window_id` 等，界面用不到就不解析 ——
/// serde 默认忽略未知字段，少解析一项就少一处随版本失效的风险。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WorkspaceInfo {
    /// 工作区序号（从 1 开始）。界面展示 `#N`。
    #[serde(default)]
    pub idx: usize,
    /// 是否是当前聚焦的工作区。
    #[serde(default)]
    pub is_focused: bool,
}

/// `niri msg --json keyboard-layouts` 的返回结构。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct KeyboardLayouts {
    #[serde(default)]
    pub names: Vec<String>,
    #[serde(default)]
    pub current_idx: usize,
}

impl KeyboardLayouts {
    /// 当前生效的键盘布局名。索引越界时返回 `None`。
    pub fn current(&self) -> Option<&str> {
        self.names
            .get(self.current_idx)
            .map(String::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
    }
}

/// `niri validate` 的结果。
#[derive(Debug, Clone)]
pub struct ConfigValidation {
    /// 配置是否合法。
    pub ok: bool,
    /// 非法时的首条错误摘要（已剥掉 ANSI 与日志前缀）。
    pub message: Option<String>,
}

/// 调用 `niri validate -c <path>` 校验配置。
///
/// 返回 `None` 表示**没法校验**（niri 未安装、调用超时），与「校验失败」是
/// 两回事：前者 UI 不显示任何标记，后者显示一个红色 `✗`。
pub fn validate_config(config: &Path) -> Option<ConfigValidation> {
    let mut cmd = Command::new("niri");
    cmd.arg("validate").arg("-c").arg(config);
    let out = sys::run(&mut cmd, VALIDATE_TIMEOUT)?;

    if out.status.success() {
        return Some(ConfigValidation {
            ok: true,
            message: None,
        });
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    Some(ConfigValidation {
        ok: false,
        message: extract_error(&stderr),
    })
}

/// 从 `niri validate` 的 stderr 里挑出首条错误摘要。
///
/// 输出里混着带颜色的日志行（`2026-… DEBUG niri_config: loaded config`）与
/// 多行错误框；先剥 ANSI，再取第一条含 `error` 的行，去掉 `Error:` 前缀。
fn extract_error(stderr: &str) -> Option<String> {
    stderr
        .lines()
        .map(sys::strip_ansi)
        .map(|line| line.trim().to_owned())
        .find(|line| line.to_ascii_lowercase().contains("error"))
        .map(|line| {
            let rest = line
                .strip_prefix("Error:")
                .or_else(|| line.strip_prefix("error:"))
                .unwrap_or(&line);
            rest.trim().to_owned()
        })
        .filter(|line| !line.is_empty())
}

// ════════════════════════════════════════════════════════════════════
//  IPC 客户端
// ════════════════════════════════════════════════════════════════════

/// `niri msg` 的调用封装。
///
/// 持有 socket 路径是为了给每个子进程显式注入 `NIRI_SOCKET` 环境变量 ——
/// 既避免改动本进程的全局环境（`std::env::set_var` 在 Rust 2024 已是
/// `unsafe`），也让「探测到的 socket」与「实际使用的 socket」严格一致。
#[derive(Debug, Clone)]
pub struct NiriIpc {
    socket: Option<PathBuf>,
}

impl NiriIpc {
    pub fn new(socket: Option<PathBuf>) -> Self {
        Self { socket }
    }

    /// 组装一条 `niri msg --json ...` 命令。
    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new("niri");
        cmd.arg("msg").arg("--json").args(args);
        if let Some(sock) = &self.socket {
            cmd.env("NIRI_SOCKET", sock);
        }
        cmd
    }

    /// 执行一次调用并把 stdout 解析成 JSON。任何一步出错都返回 `None`。
    fn json(&self, args: &[&str]) -> Option<serde_json::Value> {
        let out = sys::run(&mut self.command(args), IPC_TIMEOUT)?;
        // 非零退出码是正常业务错误，例如没有聚焦窗口时 niri 会报错退出。
        if !out.status.success() {
            return None;
        }
        serde_json::from_slice(&out.stdout).ok()
    }

    /// **运行中**的 niri 版本，取自 `niri msg --json version` 的 `compositor` 字段。
    ///
    /// 这比 `niri --version` 更准确：后者报告的是磁盘上已安装的二进制版本，
    /// niri 升级后未重启时会与实际运行的混成器不一致。
    pub fn compositor_version(&self) -> Option<String> {
        let v = self.json(&["version"])?;
        v.get("compositor")?
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    }

    /// 当前聚焦的窗口。无聚焦窗口（或 IPC 失败）时返回 `None`。
    pub fn focused_window(&self) -> Option<FocusedWindow> {
        let v = self.json(&["focused-window"])?;
        serde_json::from_value(v).ok()
    }

    /// 当前聚焦的输出。
    pub fn focused_output(&self) -> Option<OutputInfo> {
        let v = self.json(&["focused-output"])?;
        serde_json::from_value(v).ok()
    }

    /// 全部已连接输出，按名称排序以保证顺序稳定。
    pub fn outputs(&self) -> Vec<OutputInfo> {
        let Some(v) = self.json(&["outputs"]) else {
            return Vec::new();
        };
        // niri 返回的是以输出名为 key 的对象；`OutputInfo` 自带 name 字段，
        // 所以这里的 key 用不上。
        let map: HashMap<String, OutputInfo> = serde_json::from_value(v).unwrap_or_default();
        let mut list: Vec<OutputInfo> = map.into_values().collect();
        list.sort_by(|a, b| a.name.cmp(&b.name));
        list
    }

    /// 挑选「主要输出」，并说明挑选依据。
    ///
    /// 策略：聚焦输出 → 唯一输出 → 按名称排序的第一个。
    pub fn primary_output(&self) -> Option<(OutputInfo, OutputSource)> {
        if let Some(o) = self.focused_output() {
            // 聚焦输出拿得到，但某些情况下（如全部输出都关闭）它可能不含模式信息，
            // 此时再回退到 outputs 列表里同名的那一项。
            if o.active_mode().is_some() {
                return Some((o, OutputSource::Focused));
            }
            let name = o.name.clone();
            if let Some(full) = self.outputs().into_iter().find(|x| x.name == name) {
                return Some((full, OutputSource::Focused));
            }
            return Some((o, OutputSource::Focused));
        }

        let mut all = self.outputs();
        if all.is_empty() {
            return None;
        }
        let source = if all.len() == 1 {
            OutputSource::Only
        } else {
            OutputSource::First
        };
        Some((all.remove(0), source))
    }

    /// 全部工作区。失败时返回空列表。
    pub fn workspaces(&self) -> Vec<WorkspaceInfo> {
        let Some(v) = self.json(&["workspaces"]) else {
            return Vec::new();
        };
        serde_json::from_value(v).unwrap_or_default()
    }

    /// 打开窗口的数量。IPC 失败时返回 `None`（与「0 个窗口」区分开）。
    pub fn window_count(&self) -> Option<usize> {
        let v = self.json(&["windows"])?;
        v.as_array().map(Vec::len)
    }

    /// 配置的键盘布局。
    pub fn keyboard_layouts(&self) -> Option<KeyboardLayouts> {
        let v = self.json(&["keyboard-layouts"])?;
        serde_json::from_value(v).ok()
    }
}

/// 已安装的 niri 二进制版本（`niri --version` → `niri 25.05 (1a2b3c4)`）。
///
/// 仅在 IPC 不可用时作为兜底，因此失败时静默返回 `None`。
pub fn installed_version() -> Option<String> {
    let mut cmd = Command::new("niri");
    cmd.arg("--version");
    let out = sys::run(&mut cmd, IPC_TIMEOUT)?;
    if !out.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&out.stdout);
    // 去掉 "niri " 前缀，只留版本号本身。
    let v = raw.trim().strip_prefix("niri").unwrap_or(raw.trim()).trim();
    if v.is_empty() {
        None
    } else {
        Some(v.to_owned())
    }
}

// ════════════════════════════════════════════════════════════════════
//  测试
// ════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(width: u32, height: u32, mhz: u32, preferred: bool) -> Mode {
        Mode {
            width,
            height,
            refresh_rate: mhz,
            is_preferred: preferred,
        }
    }

    fn output(modes: Vec<Mode>, current_mode: Option<usize>) -> OutputInfo {
        OutputInfo {
            name: "DP-1".to_owned(),
            modes,
            current_mode,
            logical: None,
            vrr_supported: false,
            vrr_enabled: false,
        }
    }

    #[test]
    fn active_mode_uses_current_index() {
        let o = output(
            vec![
                mode(1920, 1080, 60_000, true),
                mode(2560, 1440, 144_000, false),
            ],
            Some(1),
        );
        // 索引指向的不是首选模式，也应当以索引为准。
        assert_eq!(o.physical_size(), Some((2560, 1440)));
        assert_eq!(o.refresh_hz(), Some(144.0));
    }

    #[test]
    fn active_mode_survives_out_of_range_index() {
        // niri 换了显示模式后索引可能越界，这里是防 panic 的关键路径。
        let o = output(
            vec![
                mode(1920, 1080, 60_000, false),
                mode(2560, 1440, 90_000, true),
            ],
            Some(99),
        );
        assert_eq!(o.physical_size(), Some((2560, 1440)));
    }

    #[test]
    fn active_mode_falls_back_to_first_when_none_preferred() {
        let o = output(vec![mode(1280, 720, 60_000, false)], None);
        assert_eq!(o.physical_size(), Some((1280, 720)));
    }

    #[test]
    fn active_mode_is_none_without_modes() {
        let o = output(Vec::new(), Some(0));
        assert_eq!(o.physical_size(), None);
        assert_eq!(o.refresh_hz(), None);
    }

    #[test]
    fn zero_refresh_rate_means_unknown() {
        // 0 mHz 是无意义的值，应该报 None 而不是显示 0Hz。
        let o = output(vec![mode(1920, 1080, 0, true)], None);
        assert_eq!(o.refresh_hz(), None);
    }

    #[test]
    fn fractional_refresh_rate_keeps_precision() {
        // 59.94Hz 这类非整数刷新率不能被抹成整数。
        let o = output(vec![mode(1920, 1080, 59_940, true)], None);
        let hz = o.refresh_hz().expect("应当解析出刷新率");
        assert!((hz - 59.94).abs() < 0.001, "实际得到 {hz}");
    }

    #[test]
    fn transform_filters_out_the_boring_case() {
        let mut o = output(Vec::new(), None);
        assert_eq!(o.transform(), None, "没有 logical 时应当无变换");

        o.logical = Some(Logical {
            scale: 1.0,
            transform: Some("Normal".to_owned()),
        });
        assert_eq!(o.transform(), None, "Normal 是常态，不该占用界面空间");

        o.logical = Some(Logical {
            scale: 2.0,
            transform: Some("normal".to_owned()),
        });
        assert_eq!(o.transform(), None, "大小写不敏感");

        o.logical = Some(Logical {
            scale: 2.0,
            transform: Some("270".to_owned()),
        });
        assert_eq!(o.transform().as_deref(), Some("270"));

        o.logical = Some(Logical {
            scale: 2.0,
            transform: Some("   ".to_owned()),
        });
        assert_eq!(o.transform(), None, "空白值不算变换");
    }

    #[test]
    fn scale_reads_from_logical() {
        let mut o = output(Vec::new(), None);
        assert_eq!(o.scale(), None);
        o.logical = Some(Logical {
            scale: 2.0,
            transform: None,
        });
        assert_eq!(o.scale(), Some(2.0));
    }

    #[test]
    fn desktop_matching_handles_lists_and_case() {
        assert!(desktop_is_niri("niri"));
        assert!(desktop_is_niri("NIRI"));
        // XDG_CURRENT_DESKTOP 是冒号分隔的列表。
        assert!(desktop_is_niri("niri:wlroots"));
        assert!(desktop_is_niri("gnome:niri"));
        assert!(!desktop_is_niri("GNOME"));
        assert!(!desktop_is_niri(""));
        // 不能被同前缀的名字骗过去。
        assert!(!desktop_is_niri("nirim"));
    }

    #[test]
    fn keyboard_layout_reports_the_current_one() {
        let layouts = KeyboardLayouts {
            names: vec!["English (US)".to_owned(), "Chinese".to_owned()],
            current_idx: 1,
        };
        assert_eq!(layouts.current(), Some("Chinese"));

        // 索引越界不能 panic。
        assert_eq!(
            KeyboardLayouts {
                names: vec!["English (US)".to_owned()],
                current_idx: 9,
            }
            .current(),
            None
        );
        // 空名字等同于没有。
        assert_eq!(
            KeyboardLayouts {
                names: vec!["  ".to_owned()],
                current_idx: 0,
            }
            .current(),
            None
        );
    }

    #[test]
    fn extracts_the_first_error_from_validate_output() {
        // 真实输出里混着带颜色的日志行，取第一条含 error 的行并剥掉前缀。
        let stderr = "\
\x1b[2m2026-10-03T05:30:54Z\x1b[0m \x1b[34mDEBUG\x1b[0m \x1b[2mniri_config\x1b[0m loaded config
\x1b[1m\x1b[31mError:\x1b[0m   × found `{`, expected `\"`
  │ or whitespace
";
        assert_eq!(
            extract_error(stderr).as_deref(),
            Some("× found `{`, expected `\"`")
        );
        // 没有 error 行时返回 None，不编造摘要。
        assert_eq!(extract_error("config is valid\n"), None);
    }

    #[test]
    fn workspace_info_ignores_unknown_fields() {
        // 只解析关心的字段，多出来的键不应导致反序列化失败。
        let json = r#"[
            {"id":2,"idx":2,"name":null,"output":"eDP-1","is_focused":true,"active_window_id":5},
            {"id":1,"idx":1,"is_focused":false}
        ]"#;
        let spaces: Vec<WorkspaceInfo> = serde_json::from_str(json).expect("应当能解析");
        assert_eq!(spaces.len(), 2);
        assert_eq!(spaces[0].idx, 2);
        assert!(spaces[0].is_focused);
        assert!(!spaces[1].is_focused);
    }
}
