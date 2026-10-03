//! nirifetch —— 面向 Niri Wayland 混成器的轻量级 fetch 工具。
//!
//! 本文件只做三件事：**环境检测**、**命令行解析** 与 **流程编排**。具体职责被拆到各模块：
//!
//! - [`ipc`]      ：向运行中的 niri 索取动态状态（`niri msg --json ...`）与配置校验
//! - [`config`]   ：读取 `~/.config/niri/config.kdl` 的静态元信息
//! - [`bar`]      ：状态栏 / 桌面外壳（先查配置里的 spawn 指令，再查进程）
//! - [`hardware`] ：CPU / GPU（几乎全部来自 procfs 与 sysfs）
//! - [`system`]   ：内核 / 运行时长 / 负载 / Shell / 内存 / 磁盘 / 包 / 电池
//! - [`font`]     ：终端、它使用的字体，以及终端调色板（进程链 + 终端自身配置）
//! - [`sys`]      ：上面几个模块共用的底层原语（带超时的子进程、限长读文件）
//! - [`ui`]       ：Logo、左右分栏排版、字段选择、调色板与提示文案
//!
//! 设计原则是 **永不 panic**：任何一步失败都退化成 `Unknown` / `None`
//! 并继续渲染。全文件不出现 `unwrap()` / `expect()` / 越界下标。
//!
//! 性能：所有探测（IPC / 硬件 / 字体 / 配置校验）互相独立，在 [`collect`] 里
//! 用 `std::thread::scope` 并行执行，最坏延迟由「最慢的一项」而不是「各项之和」决定。

mod bar;
mod config;
mod font;
mod hardware;
mod ipc;
mod sys;
mod system;
mod ui;

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::thread::{self, Scope, ScopedJoinHandle};

use ipc::NiriIpc;
use sys::env_non_empty;
use ui::Field;

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
            return usage_error();
        }
        Invocation::BadValue { option, value } => {
            ui::print_bad_value(option, &value);
            return usage_error();
        }
        Invocation::MissingValue(option) => {
            ui::print_missing_value(option);
            return usage_error();
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
        ui::render(&info, &build_layout(&options));
    }

    EXIT_OK
}

/// 用法错误的退出码。
fn usage_error() -> ExitCode {
    ExitCode::from(EXIT_USAGE_CODE)
}

// ════════════════════════════════════════════════════════════════════
//  命令行参数
// ════════════════════════════════════════════════════════════════════

/// 抓取时的开关。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Options {
    /// `--json`：跳过排版，输出结构化数据。
    json: bool,
    /// `--fields <list>`：只显示这些字段（保持给定顺序）。
    fields: Option<Vec<Field>>,
    /// `--short`：只显示精简字段集。
    short: bool,
    /// `--no-logo`：不画 Logo。
    no_logo: bool,
    /// `--ascii`：关闭 Logo 的真彩色渐变。
    ascii: bool,
    /// `--logo-file <path>`：用文件内容作为自定义 Logo。
    logo_file: Option<PathBuf>,
}

/// 一次调用的走向。
#[derive(Debug, PartialEq, Eq)]
enum Invocation {
    Help,
    Version,
    Fetch(Options),
    /// 认不出的参数（原样带出来，用于报错文案）。
    Unknown(String),
    /// 选项取值非法（例如 `--fields` 里的字段名不存在）。
    BadValue {
        option: &'static str,
        value: String,
    },
    /// 选项缺少必需的取值。
    MissingValue(&'static str),
}

/// 解析命令行参数。
///
/// 手写而不是引入 `clap`：nirifetch 的开关很少，而 clap 会给一个端到端
/// 65ms 的工具加上几百 KB 依赖和可感知的参数解析开销 —— 与「轻量」这个立项
/// 理由直接冲突。
///
/// 判定顺序：`-h/--help` 一旦出现立即生效（它是「告诉我怎么用」的请求，
/// 不该被别的参数或拼写错误挡住）；其次是取值错误与不认识的参数；再次是
/// `--version`；都轮不上才是正常抓取。
///
/// 写成接收迭代器的纯函数，是为了能脱离真实进程参数直接测试。
fn parse_args<I: IntoIterator<Item = String>>(args: I) -> Invocation {
    let mut options = Options::default();
    let mut wants_version = false;
    let mut unknown: Option<String> = None;
    let mut bad: Option<Invocation> = None;

    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-h" | "--help" => return Invocation::Help,
            // `-v` 是用户最顺手的写法，`-V` 是不少工具的历史习惯，都认。
            "-v" | "-V" | "--version" => wants_version = true,
            "--json" => options.json = true,
            "--no-logo" => options.no_logo = true,
            "--ascii" => options.ascii = true,
            "--short" => options.short = true,
            "--fields" => match iter.next() {
                Some(value) => match parse_fields(&value) {
                    Ok(fields) => options.fields = Some(fields),
                    Err(value) => set_bad(
                        &mut bad,
                        Invocation::BadValue {
                            option: "--fields",
                            value,
                        },
                    ),
                },
                None => set_bad(&mut bad, Invocation::MissingValue("--fields")),
            },
            "--logo-file" => match iter.next() {
                Some(value) => options.logo_file = Some(PathBuf::from(value)),
                None => set_bad(&mut bad, Invocation::MissingValue("--logo-file")),
            },
            other => {
                if let Some(value) = other.strip_prefix("--fields=") {
                    match parse_fields(value) {
                        Ok(fields) => options.fields = Some(fields),
                        Err(value) => set_bad(
                            &mut bad,
                            Invocation::BadValue {
                                option: "--fields",
                                value,
                            },
                        ),
                    }
                } else if let Some(value) = other.strip_prefix("--logo-file=") {
                    options.logo_file = Some(PathBuf::from(value));
                } else if unknown.is_none() {
                    // 只留第一个：一次报一个错比刷一屏更好读。
                    unknown = Some(other.to_owned());
                }
            }
        }
    }

    if let Some(bad) = bad {
        return bad;
    }
    if let Some(arg) = unknown {
        return Invocation::Unknown(arg);
    }
    if wants_version {
        return Invocation::Version;
    }
    Invocation::Fetch(options)
}

/// 记下第一个用法错误（后面的不再覆盖）。
fn set_bad(slot: &mut Option<Invocation>, value: Invocation) {
    if slot.is_none() {
        *slot = Some(value);
    }
}

/// 解析 `--fields` 的取值：逗号分隔的字段名，保持给定顺序并去重。
fn parse_fields(value: &str) -> Result<Vec<Field>, String> {
    let mut fields = Vec::new();
    for part in value.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let Some(field) = Field::from_name(part) else {
            return Err(part.to_owned());
        };
        if !fields.contains(&field) {
            fields.push(field);
        }
    }
    if fields.is_empty() {
        Err(value.trim().to_owned())
    } else {
        Ok(fields)
    }
}

/// 根据命令行开关构造渲染布局。
fn build_layout(options: &Options) -> ui::Layout {
    let fields = if let Some(fields) = &options.fields {
        fields.clone()
    } else if options.short {
        Field::SHORT.to_vec()
    } else {
        Field::ALL.to_vec()
    };

    let logo = if options.no_logo {
        ui::Logo::None
    } else if let Some(path) = &options.logo_file {
        match read_logo_file(path) {
            Some(lines) => ui::Logo::Custom(lines),
            None => {
                warn(&format!(
                    "could not read logo file {}; using the built-in logo",
                    path.display()
                ));
                ui::Logo::Niri
            }
        }
    } else {
        ui::Logo::Niri
    };

    ui::Layout {
        fields,
        logo,
        ascii: options.ascii,
    }
}

/// 自定义 Logo 文件的读取上限。
///
/// Logo 再大也就几十行，这里给足余量；设上限是为了防住
/// `--logo-file /dev/zero`（无限文件）或误指向巨型文件时把内存吃光。
const MAX_LOGO_BYTES: u64 = 64 * 1024;

/// 读取自定义 Logo：按行切分，丢掉空行，保留行内空白。
fn read_logo_file(path: &Path) -> Option<Vec<String>> {
    // 用带字节上限的读取，而不是 `std::fs::read` —— 后者会一直读到 EOF，
    // 对 `/dev/zero` 这类无限文件会持续膨胀直到内存耗尽。
    let text = sys::read_capped(path, MAX_LOGO_BYTES)?;
    let lines: Vec<String> = text
        .lines()
        .map(|line| line.trim_end().to_owned())
        .filter(|line| !line.is_empty())
        .collect();
    (!lines.is_empty()).then_some(lines)
}

/// 往 stderr 写一句警告，写不进去也安静收场（不 panic）。
fn warn(message: &str) {
    let _ = writeln!(std::io::stderr(), "nirifetch: {message}");
}

// ════════════════════════════════════════════════════════════════════
//  采集
// ════════════════════════════════════════════════════════════════════

/// 在并行作用域里生成一个线程；创建失败时返回 `None` 而不是 panic。
///
/// `std::thread::Scope::spawn` 在线程创建失败（资源耗尽）时会 panic，与
/// 「绝不 panic」的承诺冲突。改走 `Builder::spawn_scoped`，失败就把该字段
/// 退化成默认值。
fn spawn<'scope, 'env, T, F>(
    scope: &'scope Scope<'scope, 'env>,
    f: F,
) -> Option<ScopedJoinHandle<'scope, T>>
where
    F: FnOnce() -> T + Send + 'scope,
    T: Send + 'scope,
{
    thread::Builder::new().spawn_scoped(scope, f).ok()
}

/// 等待并行线程结束并取其结果；线程创建失败或线程 panic 时返回默认值。
fn joined<'scope, T: Default>(handle: Option<ScopedJoinHandle<'scope, T>>) -> T {
    handle
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default()
}

/// 采集全部展示数据。
///
/// 所有探测互相独立，用 `std::thread::scope` 并行执行；每个字段独立失败、
/// 独立兜底，互不影响 —— 比如 IPC 断开时窗口信息变成 `None`，但配置文件
/// 路径照常显示。配置要先探测（其余多项依赖它），所以它留在 scope 之外。
fn collect(client: &NiriIpc) -> ui::Info {
    // 配置只探测一次，架构判定、路径 / 大小 / 行数、以及给 bar 扫描的正文
    // 都来自同一份快照，避免两次读取之间文件被改动导致前后不一致。
    let config = config::inspect();

    let (
        terminal,
        wm,
        window,
        output,
        cpu,
        gpus,
        bar,
        workspace,
        keyboard,
        validation,
        memory,
        disk,
        kernel,
        shell,
        uptime,
        packages,
        battery,
        load,
    ) = std::thread::scope(|scope| {
        let terminal = spawn(scope, font::probe);
        let wm = spawn(scope, || compositor_version(client));
        let window = spawn(scope, || focused_window(client));
        let output = spawn(scope, || primary_output(client));
        let cpu = spawn(scope, cpu_view);
        let gpus = spawn(scope, gpu_views);
        let bar = spawn(scope, || bar::probe(&config));
        let workspace = spawn(scope, || workspace_view(client));
        let keyboard = spawn(scope, || keyboard_view(client));
        let validation = spawn(scope, || ipc::validate_config(&config.path));
        let memory = spawn(scope, system::memory);
        let disk = spawn(scope, || system::disk("/"));
        let kernel = spawn(scope, system::kernel);
        let shell = spawn(scope, system::shell);
        let uptime = spawn(scope, system::uptime_seconds);
        let packages = spawn(scope, system::packages);
        let battery = spawn(scope, system::battery);
        let load = spawn(scope, system::load);

        // 各闭包都不会 panic；即便真的 panic 了，`join()` 失败也只会把该字段
        // 退化成默认值，不会把整个进程带崩。
        (
            joined(terminal),
            joined(wm),
            joined(window),
            joined(output),
            joined(cpu),
            joined(gpus),
            joined(bar),
            joined(workspace),
            joined(keyboard),
            joined(validation),
            joined(memory),
            joined(disk),
            joined(kernel),
            joined(shell),
            joined(uptime),
            joined(packages),
            joined(battery),
            joined(load),
        )
    });

    // 调色板依赖已识别出的终端，放在并行段之后（只是读一个小配置文件）。
    let palette = font::terminal_palette(terminal.terminal.as_deref());

    ui::Info {
        user: username(),
        host: hostname(),
        home: env_non_empty("HOME"),
        wm,
        structure: config.structure.to_string(),
        window,
        output,
        config: config_view(&config, validation),
        // 复用上面那份配置快照：状态栏常常是配置里 spawn 起来的，
        // 顺手把已经读进内存的正文扫一遍，不额外产生文件 IO。
        bar,
        terminal: terminal.terminal,
        font: terminal.font,
        cpu,
        gpus,
        workspace,
        keyboard,
        memory: memory.map(|m| ui::MemoryView {
            total_kib: m.total_kib,
            available_kib: m.available_kib,
        }),
        disk: disk.map(|d| ui::DiskView {
            mount: d.mount,
            used_kib: d.used_kib,
            total_kib: d.total_kib,
        }),
        kernel,
        shell,
        uptime,
        packages: packages.map(|p| ui::PackageView {
            count: p.count,
            manager: p.manager,
        }),
        battery: battery.map(|b| ui::BatteryView {
            percent: b.percent,
            status: b.status,
        }),
        load,
        palette,
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

/// 当前工作区与窗口统计。
///
/// 工作区列表与窗口数都取不到时返回 `None`（UI 显示 `Unknown`）。
fn workspace_view(client: &NiriIpc) -> Option<ui::WorkspaceView> {
    let workspaces = client.workspaces();
    let windows = client.window_count();
    if workspaces.is_empty() && windows.is_none() {
        return None;
    }
    let focused_idx = workspaces
        .iter()
        .find(|workspace| workspace.is_focused)
        .map(|workspace| workspace.idx);
    Some(ui::WorkspaceView {
        focused_idx,
        total: workspaces.len(),
        windows: windows.unwrap_or(0),
    })
}

/// 当前键盘布局名。
fn keyboard_view(client: &NiriIpc) -> Option<String> {
    client
        .keyboard_layouts()
        .and_then(|layouts| layouts.current().map(str::to_owned))
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
fn config_view(
    info: &config::ConfigInfo,
    validation: Option<ipc::ConfigValidation>,
) -> ui::ConfigView {
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
        validation: validation.map(|v| ui::ValidationView {
            ok: v.ok,
            message: v.message,
        }),
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

    /// 只关心「字段选择」时的简写。
    fn fetch(options: Options) -> Invocation {
        Invocation::Fetch(options)
    }

    #[test]
    fn no_arguments_means_a_plain_fetch() {
        assert_eq!(parse_args(args(&[])), fetch(Options::default()));
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
            fetch(Options {
                json: true,
                ..Options::default()
            })
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
            fetch(Options {
                json: true,
                ..Options::default()
            })
        );
    }

    #[test]
    fn empty_string_argument_is_rejected() {
        // 空参数不是有效开关，也不该被当成「没有参数」而悄悄放过。
        assert_eq!(parse_args(args(&[""])), Invocation::Unknown(String::new()));
    }

    #[test]
    fn layout_flags_are_collected() {
        let options = match parse_args(args(&["--short", "--no-logo", "--ascii"])) {
            Invocation::Fetch(options) => options,
            other => panic!("应当正常抓取，实际 {other:?}"),
        };
        assert!(options.short);
        assert!(options.no_logo);
        assert!(options.ascii);
    }

    #[test]
    fn fields_accept_both_separated_and_equals_forms() {
        let expected = vec![Field::Os, Field::Cpu, Field::Palette];
        for argv in [
            args(&["--fields", "os,cpu,palette"]),
            args(&["--fields=os,cpu,palette"]),
        ] {
            let options = match parse_args(argv) {
                Invocation::Fetch(options) => options,
                other => panic!("应当正常抓取，实际 {other:?}"),
            };
            assert_eq!(options.fields.as_deref(), Some(expected.as_slice()));
        }
    }

    #[test]
    fn fields_preserve_order_and_drop_duplicates() {
        let options = match parse_args(args(&["--fields", "gpu,os,os,cpu"])) {
            Invocation::Fetch(options) => options,
            other => panic!("应当正常抓取，实际 {other:?}"),
        };
        assert_eq!(
            options.fields.as_deref(),
            Some([Field::Gpu, Field::Os, Field::Cpu].as_slice())
        );
    }

    #[test]
    fn unknown_field_is_a_usage_error() {
        assert_eq!(
            parse_args(args(&["--fields", "os,nope"])),
            Invocation::BadValue {
                option: "--fields",
                value: "nope".to_owned(),
            }
        );
        // 空列表同样视为用法错误。
        assert!(matches!(
            parse_args(args(&["--fields", ""])),
            Invocation::BadValue { .. }
        ));
    }

    #[test]
    fn missing_option_values_are_reported() {
        assert_eq!(
            parse_args(args(&["--fields"])),
            Invocation::MissingValue("--fields")
        );
        assert_eq!(
            parse_args(args(&["--logo-file"])),
            Invocation::MissingValue("--logo-file")
        );
    }

    #[test]
    fn logo_file_path_is_kept() {
        let options = match parse_args(args(&["--logo-file=/tmp/x.txt"])) {
            Invocation::Fetch(options) => options,
            other => panic!("应当正常抓取，实际 {other:?}"),
        };
        assert_eq!(options.logo_file.as_deref(), Some(Path::new("/tmp/x.txt")));
    }

    #[test]
    fn bad_values_win_over_unknown_options() {
        // `--fields` 的取值错误比后面拼错的参数更具体，优先报它。
        assert_eq!(
            parse_args(args(&["--fields", "nope", "--typo"])),
            Invocation::BadValue {
                option: "--fields",
                value: "nope".to_owned(),
            }
        );
    }

    #[test]
    fn logo_file_is_split_into_lines() {
        let path = std::env::temp_dir().join(format!("nirifetch-logo-{}", std::process::id()));
        std::fs::write(&path, "  _\n | |\n\n A \n").expect("写入临时 Logo");

        let lines = read_logo_file(&path).expect("应当读到行");
        // 行尾空白被去掉，空行被丢弃，行首空白保留（点阵对齐需要）。
        assert_eq!(lines, vec!["  _", " | |", " A"]);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn logo_file_read_is_bounded_and_never_missing_panics() {
        // 目录、不存在的路径都读不出内容 —— 调用方会退回内置 Logo。
        assert!(read_logo_file(Path::new("/")).is_none());
        assert!(read_logo_file(Path::new("/nonexistent/logo")).is_none());

        // 远超上限的文件只读前 MAX_LOGO_BYTES 字节，不会无节制增长。
        let path = std::env::temp_dir().join(format!("nirifetch-logo-big-{}", std::process::id()));
        let line = "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\n";
        let mut content = String::new();
        while content.len() < (MAX_LOGO_BYTES as usize) * 2 {
            content.push_str(line);
        }
        std::fs::write(&path, &content).expect("写入临时大 Logo");

        let total: usize = read_logo_file(&path)
            .expect("大文件也应读到内容")
            .iter()
            .map(|line| line.len() + 1)
            .sum();
        assert!(
            total <= MAX_LOGO_BYTES as usize + 64,
            "读取没有被上限截住：{total} 字节"
        );

        let _ = std::fs::remove_file(&path);
    }
}
