//! nirifetch —— 面向 Niri Wayland 混成器的轻量级 fetch 工具。
//!
//! 本文件只做两件事：**环境检测** 与 **流程编排**。具体职责被拆到各模块：
//!
//! - [`ipc`]      ：向运行中的 niri 索取动态状态（`niri msg --json ...`）
//! - [`config`]   ：读取 `~/.config/niri/config.kdl` 的静态元信息
//! - [`bar`]      ：状态栏 / 桌面外壳（先查配置里的 spawn 指令，再查进程）
//! - [`hardware`] ：CPU / GPU（几乎全部来自 procfs 与 sysfs）
//! - [`font`]     ：终端与它使用的字体（进程链 + 终端自身配置）
//! - [`sys`]      ：上面几个模块共用的底层原语（带超时的子进程、限长读文件）
//! - [`ui`]       ：Logo、左右分栏排版、调色板与提示文案
//!
//! 设计原则是 **永不 panic**：任何一步失败都退化成 `Unknown` / `None`
//! 并继续渲染。全文件不出现 `unwrap()` / `expect()` / 越界下标。

mod bar;
mod config;
mod font;
mod hardware;
mod ipc;
mod sys;
mod ui;

use std::process::ExitCode;

use ipc::NiriIpc;
use sys::env_non_empty;

/// 成功退出。
const EXIT_OK: ExitCode = ExitCode::SUCCESS;
/// 环境不满足（不在 niri 会话中）时的退出码。
const EXIT_NOT_NIRI: ExitCode = ExitCode::FAILURE;
/// 命令行用法错误时的退出码，沿用 `sysexits.h` 的 `EX_USAGE` 惯例。
/// `ExitCode::from` 不是 const 函数，所以这里存数值、用的时候再转。
const EXIT_USAGE_CODE: u8 = 2;

fn main() -> ExitCode {
    // ── 0. 参数：--help / --version 不该被会话检测拦住 ───────────────
    let options = match parse_args(std::env::args().skip(1)) {
        Invocation::Help => {
            ui::print_help();
            return EXIT_OK;
        }
        Invocation::Version => {
            ui::print_version();
            return EXIT_OK;
        }
        Invocation::Unknown(arg) => {
            ui::print_unknown_argument(&arg);
            return ExitCode::from(EXIT_USAGE_CODE);
        }
        Invocation::Fetch(options) => options,
    };

    // ── 1. 环境检测：不在 niri 会话里就友好报错并干净退出 ────────────
    let session = match ipc::detect_session() {
        Ok(session) => session,
        Err(err) => {
            ui::print_session_error(&err);
            return EXIT_NOT_NIRI;
        }
    };

    // ── 2. 采集 + 渲染 ─────────────────────────────────────────────
    let client = NiriIpc::new(session.socket);
    let info = collect(&client);

    if options.json {
        ui::print_json(&info);
    } else {
        ui::render(&info);
    }

    EXIT_OK
}

// ════════════════════════════════════════════════════════════════════
//  命令行参数
// ════════════════════════════════════════════════════════════════════

/// 抓取时的开关。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Options {
    /// `--json`：跳过排版，输出结构化数据。
    json: bool,
}

/// 一次调用的走向。
#[derive(Debug, PartialEq, Eq)]
enum Invocation {
    Help,
    Version,
    Fetch(Options),
    /// 认不出的参数（原样带出来，用于报错文案）。
    Unknown(String),
}

/// 解析命令行参数。
///
/// 手写而不是引入 `clap`：nirifetch 一共只有三个开关，而 clap 会给一个端到端
/// 65ms 的工具加上几百 KB 依赖和可感知的参数解析开销 —— 与「轻量」这个立项
/// 理由直接冲突。
///
/// 判定顺序：`-h/--help` 一旦出现立即生效（它是「告诉我怎么用」的请求，
/// 不该被别的参数或拼写错误挡住）；其次是不认识的参数；再次是 `--version`；
/// 都轮不上才是正常抓取。
///
/// 写成接收迭代器的纯函数，是为了能脱离真实进程参数直接测试。
fn parse_args<I: IntoIterator<Item = String>>(args: I) -> Invocation {
    let mut options = Options::default();
    let mut wants_version = false;
    let mut unknown: Option<String> = None;

    for arg in args {
        match arg.as_str() {
            "-h" | "--help" => return Invocation::Help,
            // `-v` 是用户最顺手的写法，`-V` 是不少工具的历史习惯，都认。
            "-v" | "-V" | "--version" => wants_version = true,
            "--json" => options.json = true,
            // 只留第一个：一次报一个错比刷一屏更好读。
            other => {
                if unknown.is_none() {
                    unknown = Some(other.to_owned());
                }
            }
        }
    }

    if let Some(arg) = unknown {
        return Invocation::Unknown(arg);
    }
    if wants_version {
        return Invocation::Version;
    }
    Invocation::Fetch(options)
}

/// 采集全部展示数据。
///
/// 这里刻意做成「尽力而为」：每个字段独立失败、独立兜底，互不影响 ——
/// 比如 IPC 断开时窗口信息变成 `None`，但配置文件路径照常显示。
fn collect(client: &NiriIpc) -> ui::Info {
    // 配置只探测一次，架构判定与路径 / 大小 / 行数都来自同一份快照，
    // 避免两次 `inspect()` 之间文件被改动导致前后不一致。
    let config = config::inspect();
    let terminal = font::probe();

    ui::Info {
        user: username(),
        host: hostname(),
        home: env_non_empty("HOME"),
        wm: compositor_version(client),
        structure: config.structure.to_string(),
        window: focused_window(client),
        output: primary_output(client),
        config: config_view(&config),
        // 复用上面那份配置快照：状态栏常常是配置里 spawn 起来的，
        // 顺手把已经读进内存的正文扫一遍，不额外产生文件 IO。
        bar: bar::probe(&config),
        terminal: terminal.terminal,
        font: terminal.font,
        cpu: cpu_view(),
        gpus: gpu_views(),
    }
}

/// CPU 的展示数据。
fn cpu_view() -> Option<ui::CpuView> {
    hardware::cpu().map(|cpu| ui::CpuView {
        model: cpu.model,
        logical: cpu.logical,
        physical: cpu.physical,
    })
}

/// 全部显卡的展示数据（活跃的已在 `hardware` 里排到前面）。
fn gpu_views() -> Vec<ui::GpuView> {
    hardware::gpus()
        .into_iter()
        .map(|gpu| ui::GpuView {
            name: gpu.name,
            driver: gpu.driver,
            active: gpu.active,
        })
        .collect()
}

/// WM 版本号，三级回退：
///
/// 1. `niri msg --json version` 的 `compositor` 字段 —— **运行中**的混成器版本；
/// 2. `niri --version` —— 磁盘上已安装的二进制版本（niri 升级后未重启时会与
///    第 1 项不一致，所以只作兜底）；
/// 3. `None`，由 UI 渲染成 `Unknown`。
///
/// 是否「未知」的判定交给 UI 而不是在这里硬编码成 `"Unknown"` 字符串，
/// 免得展示层还要反过去比较这个魔法值。
fn compositor_version(client: &NiriIpc) -> Option<String> {
    client.compositor_version().or_else(ipc::installed_version)
}

/// 当前聚焦窗口。无窗口获得焦点时 niri 会返回错误，此处自然退化为 `None`。
fn focused_window(client: &NiriIpc) -> ui::WindowView {
    client
        .focused_window()
        .map_or_else(ui::WindowView::default, |w| ui::WindowView {
            title: w.title,
            app_id: w.app_id,
            floating: w.is_floating,
        })
}

/// 主要输出的展示数据。取不到任何输出时为 `None`（UI 显示 `Unknown`）。
fn primary_output(client: &NiriIpc) -> Option<ui::OutputView> {
    client.primary_output().map(|(output, source)| {
        // 先把派生值算完，最后再移动 name 字段，避免部分移动。
        let physical = output.physical_size();
        // 滤掉 NaN / ±inf：JSON 没有这两个值，`serde_json` 遇到它们会让整个
        // `--json` 输出失败。这种值本身也没有展示意义，在源头挡掉最省事。
        let refresh_hz = output.refresh_hz().filter(|hz| hz.is_finite());
        let scale = output.scale().filter(|s| s.is_finite());
        let transform = output.transform();
        // 只有硬件支持 VRR 时，「开没开」才是有效信息；不支持则一律不展示。
        let vrr_enabled = output.vrr_supported.then_some(output.vrr_enabled);

        ui::OutputView {
            name: output.name,
            physical,
            refresh_hz,
            scale,
            source_label: source.label(),
            vrr_enabled,
            transform,
        }
    })
}

/// niri 配置文件的展示数据。
fn config_view(info: &config::ConfigInfo) -> ui::ConfigView {
    ui::ConfigView {
        // `display()` 对非 UTF-8 路径做有损转换，不会 panic。
        path: info.path.display().to_string(),
        exists: info.exists,
        // 「绝大多数用户都用默认的用户级配置，无需标注来源」这条规则由
        // `ConfigSource::label()` 自己表达（对 `User` 返回 `None`），
        // 调用方不必再判一次。
        source_label: info.source.label(),
        size: info.size,
        lines: info.lines,
    }
}

/// 当前用户名，取自 `$USER` / `$LOGNAME`。
fn username() -> String {
    env_non_empty("USER")
        .or_else(|| env_non_empty("LOGNAME"))
        .unwrap_or_else(|| "unknown".to_owned())
}

/// 主机名。优先读 `/etc/hostname`（最可靠），退回 `$HOSTNAME`。
fn hostname() -> String {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .or_else(|| env_non_empty("HOSTNAME"))
        .unwrap_or_else(|| "unknown".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 用字符串字面量拼一个参数迭代器，省得每处都写 `.to_owned()`。
    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn no_arguments_means_a_plain_fetch() {
        assert_eq!(
            parse_args(args(&[])),
            Invocation::Fetch(Options { json: false })
        );
    }

    #[test]
    fn accepts_every_spelling_of_version() {
        for flag in ["-v", "-V", "--version"] {
            assert_eq!(parse_args(args(&[flag])), Invocation::Version, "{flag}");
        }
    }

    #[test]
    fn accepts_every_spelling_of_help() {
        for flag in ["-h", "--help"] {
            assert_eq!(parse_args(args(&[flag])), Invocation::Help, "{flag}");
        }
    }

    #[test]
    fn json_flag_selects_the_structured_output() {
        assert_eq!(
            parse_args(args(&["--json"])),
            Invocation::Fetch(Options { json: true })
        );
    }

    #[test]
    fn help_wins_over_everything_else() {
        // 帮助是「告诉我怎么用」的请求，不该被别的参数或拼写错误挡住。
        assert_eq!(parse_args(args(&["--json", "--help"])), Invocation::Help);
        assert_eq!(parse_args(args(&["--typo", "-h"])), Invocation::Help);
        assert_eq!(parse_args(args(&["--version", "--help"])), Invocation::Help);
    }

    #[test]
    fn unknown_arguments_are_reported_not_ignored() {
        // 静默忽略会让人以为 `--jsno` 生效了。
        assert_eq!(
            parse_args(args(&["--jsno"])),
            Invocation::Unknown("--jsno".to_owned())
        );
        // 认不出的参数优先于 --version 报出来。
        assert_eq!(
            parse_args(args(&["--typo", "--version"])),
            Invocation::Unknown("--typo".to_owned())
        );
        // 一次只报第一个，不刷一屏。
        assert_eq!(
            parse_args(args(&["--a", "--b"])),
            Invocation::Unknown("--a".to_owned())
        );
    }

    #[test]
    fn flags_can_be_combined_in_any_order() {
        assert_eq!(
            parse_args(args(&["--json", "--json"])),
            Invocation::Fetch(Options { json: true })
        );
    }

    #[test]
    fn empty_string_argument_is_rejected() {
        // 空参数不是有效开关，也不该被当成「没有参数」而悄悄放过。
        assert_eq!(parse_args(args(&[""])), Invocation::Unknown(String::new()));
    }
}
