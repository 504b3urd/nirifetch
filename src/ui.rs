//! 终端排版与着色层。
//!
//! 职责边界：**只负责把数据变成字符** —— Niri ASCII Logo、左右分栏对齐、
//! Nerd Font 图标列、字段着色、调色板色块，以及各类帮助 / 报错文案。
//!
//! 依赖方向：`main → ui → ipc`。对 `ipc` 的依赖仅限[`SessionError`] 这一个
//! 错误类型，好让报错文案留在这里而不是散落到入口。渲染用的 [`Info`] 是本
//! 模块自己定义的「展示模型」，字段全是朴素类型，因此排版逻辑与数据来源
//! 彻底解耦，单独测试或替换数据源都不需要改动本文件。

use colored::{Color, Colorize};
use serde::Serialize;

use crate::ipc::SessionError;
use crate::sys::env_non_empty;

// ── 输出：写入失败不 panic ──────────────────────────────────────────
//
// `std::println!` 在写入失败时会 **panic** —— 管道另一头提前关闭就会触发：
//
//     nirifetch | jq          # 没装 jq，读端在首次写入前就没了
//     nirifetch | grep -q x   # grep 命中即退出
//     nirifetch | head -1
//
// 这些是命令行工具的日常用法，而 nirifetch 明确承诺「绝不 panic」，
// 不该在这里破功。下面两个宏在本模块内**遮蔽同名的标准库宏**，把输出换成
// 「写不进去就安静收工」。
//
// 用遮蔽而不是逐个改写调用点：模块里有四十多处输出、写法各异，遮蔽能让
// 「本模块所有输出都不会 panic」成为一条**结构性**保证 —— 以后新增的输出
// 语句自动继承，不会漏掉某一个。测试也验证了这一点。
//
// 有意**不**引入 `libc` 把 SIGPIPE 复位成默认行为：那会为了三行代码给一个
// 以「依赖精简」为卖点的工具添上真实依赖，代价与收益不成比例。

/// 见上方说明：写入失败不 panic。
macro_rules! println {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stdout(), $($arg)*);
    }};
}

/// 见上方说明：写入失败不 panic。
macro_rules! eprintln {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stderr(), $($arg)*);
    }};
}

/// 左侧 Logo 主体（点阵化的 "NIRI" 字样）。
///
/// 五行都已右补空格到等宽 26 格。这些字符在终端里都是单格宽，
/// 所以 `chars().count()` 就等于显示宽度，对齐是安全的。
const LOGO_ART: [&str; 5] = [
    "      _   _ ___ ____  ___ ",
    "     | \\ | |_ _|  _ \\|_ _|",
    "     |  \\| || || |_) || | ",
    "     | |\\  || ||  _ < | | ",
    "     |_| \\_|___|_| \\_\\___|",
];

/// Logo 下方的标语，手动居中在 26 格宽的主体之下。
const LOGO_TAGLINE: &str = "    ~ endless scroll ~";

/// Logo 渐变的停靠点：青 → 紫蓝。
const GRADIENT_STOPS: [(u8, u8, u8); 5] = [
    (0x22, 0xD3, 0xEE), // 青
    (0x38, 0xBD, 0xF8),
    (0x63, 0x66, 0xF1),
    (0x8B, 0x5C, 0xF6),
    (0xA8, 0x55, 0xF7), // 紫
];

/// 标语色：柔和的青，与主体的渐变收尾区分开。
const TAGLINE_TRUECOLOR: (u8, u8, u8) = (103, 232, 249);

/// 图标列颜色：柔紫，呼应渐变的收尾。
const ICON_COLOR: Color = Color::Magenta;
/// 标签列颜色。
const LABEL_COLOR: Color = Color::Cyan;
/// 分隔线的颜色。
const RULE_COLOR: Color = Color::BrightBlack;

// ── Nerd Font 图标 ──────────────────────────────────────────────────
//
// 绝大多数取自经典 Font Awesome 码位（U+F000–U+F2FF），兼容所有 Nerd Font 版本；
// CPU / GPU 两个取自 Material Design Icons（U+F0001 以上，需 Nerd Font v3+）。
//
// 所有码位都实测过 advance width：在 CaskaydiaCove / JetBrainsMono Nerd Font 的
// Mono 与非 Mono 变体中均恰好等于一个字符宽，因此图标列不会撑歪对齐。
// `icons_are_exactly_one_cell_wide` 断言守着这条不变量。

/// fa-linux
const ICON_OS: &str = "\u{f17c}";
/// fa-window-maximize
const ICON_WM: &str = "\u{f2d0}";
/// fa-sitemap
const ICON_STRUCTURE: &str = "\u{f0e8}";
/// fa-file-text-o
const ICON_CONFIG: &str = "\u{f0f6}";
/// fa-terminal
const ICON_TERMINAL: &str = "\u{f120}";
/// fa-desktop
const ICON_OUTPUT: &str = "\u{f108}";
/// md-dock-bottom（Material Design Icons，需 Nerd Font v3+）
const ICON_BAR: &str = "\u{f10ac}";
/// fa-font
const ICON_FONT: &str = "\u{f031}";
/// md-memory（Material Design Icons，需 Nerd Font v3+）
const ICON_CPU: &str = "\u{f035b}";
/// fa-expansion-card
const ICON_GPU: &str = "\u{f08ae}";
/// fa-tint
const ICON_PALETTE: &str = "\u{f043}";
/// fa-th-large —— 工作区
const ICON_WORKSPACE: &str = "\u{f009}";
/// fa-keyboard-o —— 键盘布局
const ICON_KEYBOARD: &str = "\u{f11c}";
/// fa-microchip —— 内存
const ICON_MEMORY: &str = "\u{f2db}";
/// fa-hdd-o —— 磁盘
const ICON_DISK: &str = "\u{f0a0}";
/// fa-cogs —— 内核
const ICON_KERNEL: &str = "\u{f085}";
/// fa-code —— Shell
const ICON_SHELL: &str = "\u{f121}";
/// fa-clock-o —— 运行时长
const ICON_UPTIME: &str = "\u{f017}";
/// fa-cube —— 软件包
const ICON_PACKAGES: &str = "\u{f1b2}";
/// fa-battery-full —— 电池
const ICON_BATTERY: &str = "\u{f240}";
/// fa-tachometer —— 负载
const ICON_LOAD: &str = "\u{f0e4}";

/// 图标 + 一个空格占用的列数。
const ICON_SLOT: usize = 2;

/// 调色板：8 个色块，取高亮档。
///
/// 单行展示时高亮档可读性最好 —— 普通档的 0 号黑在深色终端上几乎不可见。
const PALETTE: [Color; 8] = [
    Color::BrightBlack,
    Color::BrightRed,
    Color::BrightGreen,
    Color::BrightYellow,
    Color::BrightBlue,
    Color::BrightMagenta,
    Color::BrightCyan,
    Color::BrightWhite,
];

/// 终端宽度未知时的假设值。
const DEFAULT_TERM_WIDTH: usize = 100;

/// 值列宽度的下限 / 上限（单位：显示格）。
///
/// 下限压得很低是有意为之：它是一个**兜底**，不是舒适值。Logo(26) + 间距(2) +
/// 图标(2) + 标签(9) + 间距(2) 已经占掉 41 格，窄于 46 列的终端本就摆不下这套
/// 版式。此时若还强撑一个较大的下限，值列就会溢出边界、把对齐彻底冲垮；
/// 宁可显示成 `AMD Ry…`，也要守住「所有行的值都从同一列开始」。
const VALUE_MIN: usize = 4;
const VALUE_MAX: usize = 72;

/// 分隔线右段的长度上限 —— 太长会喧宾夺主。
const RULE_MAX: usize = 44;

/// 左列与右列之间的间距。
const GAP: usize = 2;

// ════════════════════════════════════════════════════════════════════
//  展示模型
// ════════════════════════════════════════════════════════════════════

/// 一次 fetch 的完整展示数据。由 `main` 从各数据源装配而来。
///
/// 派生 `Serialize` 是给 `--json` 用的：JSON 与终端排版**共用同一份模型**，
/// 因此两边字段不可能对不上，加字段时也不会漏掉某一个输出格式。
/// 着色全部发生在 [`field_value`] 里，模型自身始终是纯文本，
/// 序列化出来天然干净。
#[derive(Debug, Clone, Serialize)]
pub struct Info {
    pub user: String,
    pub host: String,
    /// 家目录路径，用于把配置路径缩写成 `~/...`。
    pub home: Option<String>,
    /// WM 版本，例如 `25.05 (1a2b3c4)`；`None` 表示取不到。
    pub wm: Option<String>,
    /// 配置架构，已格式化的英文文案（如 `Modular (6 included files)`）。
    pub structure: String,
    pub window: WindowView,
    /// 取不到任何输出时为 `None`。
    pub output: Option<OutputView>,
    pub config: ConfigView,
    /// 状态栏 / 桌面外壳的展示名（如 `Waybar`）。没检测到为 `None`。
    pub bar: Option<&'static str>,
    /// 终端名。聚焦窗口的信息会附在这一行后面，不再单独占一行 ——
    /// 两者在绝大多数情况下是同一个东西（在 kitty 里跑就是 kitty）。
    pub terminal: Option<String>,
    pub font: Option<String>,
    pub cpu: Option<CpuView>,
    /// 全部显卡，活跃的排在前面。空表示一张都没探测到。
    pub gpus: Vec<GpuView>,
    /// 当前工作区与窗口统计。
    pub workspace: Option<WorkspaceView>,
    /// 当前键盘布局名。
    pub keyboard: Option<String>,
    pub memory: Option<MemoryView>,
    pub disk: Option<DiskView>,
    pub kernel: Option<String>,
    pub shell: Option<String>,
    /// 系统运行时长（秒）。
    pub uptime: Option<u64>,
    pub packages: Option<PackageView>,
    pub battery: Option<BatteryView>,
    /// 1 / 5 / 15 分钟平均负载。
    pub load: Option<[f32; 3]>,
    /// 终端真实调色板（RGB）。`None` 时用内置的高亮色板兜底。
    pub palette: Option<Vec<(u8, u8, u8)>>,
}

/// 聚焦窗口的展示数据。
#[derive(Debug, Clone, Default, Serialize)]
pub struct WindowView {
    pub title: Option<String>,
    pub app_id: Option<String>,
    pub floating: bool,
}

/// 输出的展示数据。
#[derive(Debug, Clone, Serialize)]
pub struct OutputView {
    pub name: String,
    /// 物理分辨率。
    pub physical: Option<(u32, u32)>,
    /// 刷新率（Hz）。
    pub refresh_hz: Option<f64>,
    /// 缩放系数。
    pub scale: Option<f64>,
    /// 「主要输出」的选取依据。
    pub source_label: &'static str,
    /// VRR（可变刷新率）是否已启用。`None` 表示该输出不支持 VRR。
    pub vrr_enabled: Option<bool>,
    /// 显示变换（旋转 / 翻转）。`Normal` 已被过滤掉，只保留值得提示的值。
    pub transform: Option<String>,
}

/// 配置文件的展示数据。
#[derive(Debug, Clone, Serialize)]
pub struct ConfigView {
    pub path: String,
    pub exists: bool,
    /// 配置来源说明。`None` 表示用户级配置（默认情况，无需标注）。
    pub source_label: Option<&'static str>,
    pub size: Option<u64>,
    pub lines: Option<usize>,
    /// `niri validate` 的结果。`None` 表示没法校验（niri 缺失 / 超时）。
    pub validation: Option<ValidationView>,
}

/// 配置校验结果。
#[derive(Debug, Clone, Serialize)]
pub struct ValidationView {
    pub ok: bool,
    pub message: Option<String>,
}

/// CPU 的展示数据。
#[derive(Debug, Clone, Serialize)]
pub struct CpuView {
    /// 清洗过的型号，如 `AMD Ryzen 7 5800X3D`。
    pub model: String,
    /// 逻辑核心数。
    pub logical: usize,
    /// 物理核心数。非 x86 平台可能取不到。
    pub physical: Option<usize>,
}

/// 单张显卡的展示数据。
#[derive(Debug, Clone, Serialize)]
pub struct GpuView {
    pub name: String,
    pub driver: Option<String>,
    /// 是否正在驱动一块已连接的显示器。
    pub active: bool,
}

/// 工作区与窗口统计。
#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceView {
    /// 当前聚焦工作区的序号（从 1 开始）；取不到为 `None`。
    pub focused_idx: Option<usize>,
    /// 工作区总数。
    pub total: usize,
    /// 打开的窗口总数。
    pub windows: usize,
}

/// 内存用量（单位 KiB）。
#[derive(Debug, Clone, Copy, Serialize)]
pub struct MemoryView {
    pub total_kib: u64,
    pub available_kib: u64,
}

/// 磁盘用量（单位 KiB）。
#[derive(Debug, Clone, Serialize)]
pub struct DiskView {
    pub mount: String,
    pub used_kib: u64,
    pub total_kib: u64,
}

/// 软件包统计。
#[derive(Debug, Clone, Serialize)]
pub struct PackageView {
    pub count: usize,
    pub manager: &'static str,
}

/// 电池状态。
#[derive(Debug, Clone, Serialize)]
pub struct BatteryView {
    pub percent: u8,
    pub status: Option<String>,
}

// ════════════════════════════════════════════════════════════════════
//  字段表
// ════════════════════════════════════════════════════════════════════

/// 可选的信息字段。输出的行序即 [`Field::ALL`] 的顺序。
///
/// 用枚举而不是「标签字符串」是因为字段现在可以由 `--fields` / `--short`
/// 任意筛选、排序；枚举让「有哪些字段、各自怎么画」在类型层面收敛到一处，
/// 加字段时不会漏掉渲染分支（match 会强制补齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Os,
    Wm,
    Structure,
    Config,
    Output,
    Bar,
    Terminal,
    Font,
    Workspace,
    Keyboard,
    Cpu,
    Gpu,
    Memory,
    Disk,
    Kernel,
    Shell,
    Uptime,
    Packages,
    Battery,
    Load,
    Palette,
}

impl Field {
    /// 全部字段，顺序即默认输出顺序。
    ///
    /// 分组逻辑：先是 niri 会话本身（OS→WM→配置→屏幕→状态栏→终端），
    /// 再是运行时状态（工作区 / 键盘），然后是硬件（CPU→GPU→内存→磁盘），
    /// 最后是系统与杂项（内核→Shell→运行时长→包→电池→负载→调色板）。
    pub const ALL: [Field; 21] = [
        Field::Os,
        Field::Wm,
        Field::Structure,
        Field::Config,
        Field::Output,
        // Bar 紧跟 Output：先讲屏幕，再讲屏幕上的状态栏，桌面层级是自上而下的。
        Field::Bar,
        Field::Terminal,
        Field::Font,
        Field::Workspace,
        Field::Keyboard,
        Field::Cpu,
        Field::Gpu,
        Field::Memory,
        Field::Disk,
        Field::Kernel,
        Field::Shell,
        Field::Uptime,
        Field::Packages,
        Field::Battery,
        Field::Load,
        Field::Palette,
    ];

    /// `--short` 用的精简集合：一屏能看完的关键信息。
    pub const SHORT: [Field; 9] = [
        Field::Os,
        Field::Wm,
        Field::Output,
        Field::Workspace,
        Field::Cpu,
        Field::Gpu,
        Field::Memory,
        Field::Battery,
        Field::Palette,
    ];

    /// 展示标签。
    pub fn label(self) -> &'static str {
        match self {
            Field::Os => "OS",
            Field::Wm => "WM",
            Field::Structure => "Structure",
            Field::Config => "Config",
            Field::Output => "Output",
            Field::Bar => "Bar",
            Field::Terminal => "Terminal",
            Field::Font => "Font",
            Field::Workspace => "Workspace",
            Field::Keyboard => "Keyboard",
            Field::Cpu => "CPU",
            Field::Gpu => "GPU",
            Field::Memory => "Memory",
            Field::Disk => "Disk",
            Field::Kernel => "Kernel",
            Field::Shell => "Shell",
            Field::Uptime => "Uptime",
            Field::Packages => "Packages",
            Field::Battery => "Battery",
            Field::Load => "Load",
            Field::Palette => "Palette",
        }
    }

    /// Nerd Font 图标。
    pub fn icon(self) -> &'static str {
        match self {
            Field::Os => ICON_OS,
            Field::Wm => ICON_WM,
            Field::Structure => ICON_STRUCTURE,
            Field::Config => ICON_CONFIG,
            Field::Output => ICON_OUTPUT,
            Field::Bar => ICON_BAR,
            Field::Terminal => ICON_TERMINAL,
            Field::Font => ICON_FONT,
            Field::Workspace => ICON_WORKSPACE,
            Field::Keyboard => ICON_KEYBOARD,
            Field::Cpu => ICON_CPU,
            Field::Gpu => ICON_GPU,
            Field::Memory => ICON_MEMORY,
            Field::Disk => ICON_DISK,
            Field::Kernel => ICON_KERNEL,
            Field::Shell => ICON_SHELL,
            Field::Uptime => ICON_UPTIME,
            Field::Packages => ICON_PACKAGES,
            Field::Battery => ICON_BATTERY,
            Field::Load => ICON_LOAD,
            Field::Palette => ICON_PALETTE,
        }
    }

    /// `--fields` 里使用的名字（小写、无空格）。
    pub fn name(self) -> &'static str {
        match self {
            Field::Os => "os",
            Field::Wm => "wm",
            Field::Structure => "structure",
            Field::Config => "config",
            Field::Output => "output",
            Field::Bar => "bar",
            Field::Terminal => "terminal",
            Field::Font => "font",
            Field::Workspace => "workspace",
            Field::Keyboard => "keyboard",
            Field::Cpu => "cpu",
            Field::Gpu => "gpu",
            Field::Memory => "memory",
            Field::Disk => "disk",
            Field::Kernel => "kernel",
            Field::Shell => "shell",
            Field::Uptime => "uptime",
            Field::Packages => "packages",
            Field::Battery => "battery",
            Field::Load => "load",
            Field::Palette => "palette",
        }
    }

    /// 供 `--help` 使用的一句话说明。
    pub fn description(self) -> &'static str {
        match self {
            Field::Os => "user@host",
            Field::Wm => "running niri compositor version",
            Field::Structure => "modular (includes) or monolithic config layout",
            Field::Config => "niri config path, size and validate status",
            Field::Output => "primary output resolution, refresh rate and scale",
            Field::Bar => "status bar or desktop shell (waybar, noctalia, eww, …)",
            Field::Terminal => "terminal emulator, plus the focused window",
            Field::Font => "font used by that terminal",
            Field::Workspace => "focused workspace, total workspaces and window count",
            Field::Keyboard => "active keyboard layout",
            Field::Cpu => "processor model, physical cores and threads",
            Field::Gpu => "graphics cards; the one driving a display is marked",
            Field::Memory => "used / total physical memory",
            Field::Disk => "used / total disk space of /",
            Field::Kernel => "Linux kernel release",
            Field::Shell => "the shell you are running",
            Field::Uptime => "system uptime",
            Field::Packages => "installed package count and manager",
            Field::Battery => "battery charge and status",
            Field::Load => "1 / 5 / 15 minute load average",
            Field::Palette => "8 terminal palette swatches",
        }
    }

    /// 按名字（大小写不敏感）解析字段。
    pub fn from_name(name: &str) -> Option<Field> {
        let name = name.trim();
        Field::ALL
            .into_iter()
            .find(|field| field.name().eq_ignore_ascii_case(name))
    }
}

/// 选中的字段、Logo 形态与着色开关。由 `main` 从命令行参数构造。
#[derive(Debug, Clone)]
pub struct Layout {
    pub fields: Vec<Field>,
    pub logo: Logo,
    /// 强制关闭真彩色渐变（`--ascii`）。
    pub ascii: bool,
}

/// 左侧 Logo 的形态。
#[derive(Debug, Clone)]
pub enum Logo {
    /// 内置的 Niri 点阵字样。
    Niri,
    /// 不显示 Logo。
    None,
    /// 用户用 `--logo-file` 提供的自定义 Logo（逐行）。
    Custom(Vec<String>),
}

/// 标签列宽度 —— 由最长的标签（`Structure`）决定。
fn label_column_width() -> usize {
    Field::ALL
        .iter()
        .map(|field| field.label().chars().count())
        .max()
        .unwrap_or(0)
}

/// 单个字段的值。
///
/// 各格式化函数会先做**有取舍的**降级（丢掉次要的括号信息、保住主体），
/// 之后这里再统一按预算兜底截断一次 —— 这样「任何一行都不撑破右边界」
/// 是结构性保证，新加字段时不会因为忘了传预算而破功。
fn field_value(field: Field, info: &Info, value_budget: usize) -> String {
    let value = match field {
        Field::Os => format!(
            "{}{}{}",
            info.user.bold().cyan(),
            "@".bright_black(),
            info.host.bold().cyan()
        ),
        Field::Wm => format_wm(info.wm.as_deref()),
        Field::Structure => info.structure.clone(),
        Field::Config => format_config(&info.config, info.home.as_deref(), value_budget),
        Field::Output => format_output(info.output.as_ref(), value_budget),
        Field::Bar => format_bar(info.bar),
        Field::Terminal => format_terminal(info.terminal.as_deref(), &info.window, value_budget),
        Field::Font => info
            .font
            .clone()
            .map_or_else(unknown, |f| f.bold().to_string()),
        Field::Workspace => format_workspace(info.workspace.as_ref()),
        Field::Keyboard => format_keyboard(info.keyboard.as_deref()),
        Field::Cpu => format_cpu(info.cpu.as_ref(), value_budget),
        Field::Gpu => format_gpu(&info.gpus, value_budget),
        Field::Memory => format_memory(info.memory.as_ref()),
        Field::Disk => format_disk(info.disk.as_ref()),
        Field::Kernel => format_kernel(info.kernel.as_deref()),
        Field::Shell => format_shell(info.shell.as_deref()),
        Field::Uptime => format_uptime(info.uptime),
        Field::Packages => format_packages(info.packages.as_ref()),
        Field::Battery => format_battery(info.battery.as_ref()),
        Field::Load => format_load(info.load),
        Field::Palette => palette_line(info.palette.as_deref()),
    };
    truncate(&value, value_budget)
}

// ════════════════════════════════════════════════════════════════════
//  主渲染
// ════════════════════════════════════════════════════════════════════

/// 渲染整屏输出：左侧 Logo（可选），右侧「图标 + 标签 + 值」信息栏。
pub fn render(info: &Info, layout: &Layout) {
    let truecolor = supports_truecolor() && !layout.ascii;
    let icons = icons_enabled();

    // ── 左列 ──────────────────────────────────────────────────────
    let logo = logo_lines(&layout.logo, truecolor);
    let logo_width = logo.iter().map(|(w, _)| *w).max().unwrap_or(0);

    // ── 布局预算：先算宽度，再构造字段 ────────────────────────────
    let icon_slot = if icons { ICON_SLOT } else { 0 };
    let label_width = label_column_width();
    // 没有 Logo 时不占 Logo 列，也不必补那一格间距。
    let left_slot = if logo_width > 0 { logo_width + GAP } else { 0 };
    let value_budget = terminal_width()
        .saturating_sub(left_slot + icon_slot + label_width + GAP)
        .clamp(VALUE_MIN, VALUE_MAX);
    let rule_width = value_budget.min(RULE_MAX);

    // ── 右列：首个字段之后插一条分隔线，把标题行与明细行隔开 ──────
    let mut right: Vec<String> = Vec::with_capacity(layout.fields.len() + 1);
    for (idx, field) in layout.fields.iter().enumerate() {
        let value = field_value(*field, info, value_budget);
        right.push(field_row(
            field.label(),
            field.icon(),
            &value,
            label_width,
            icons,
        ));
        if idx == 0 {
            right.push(separator_row(label_width, rule_width, icon_slot));
        }
    }

    // ── 无 Logo：右列独占整行 ─────────────────────────────────────
    if logo.is_empty() {
        for content in &right {
            println!("{}", content.trim_end());
        }
        return;
    }

    // ── 左右合并 ──────────────────────────────────────────────────
    //
    // 信息栏通常比 Logo 高，顶部对齐会让左下角空出一大块。把 Logo 垂直居中，
    // 两列的视觉重心才对得上。
    let logo_offset = right.len().saturating_sub(logo.len()) / 2;
    let total = right.len().max(logo.len() + logo_offset);

    for i in 0..total {
        let entry = i.checked_sub(logo_offset).and_then(|idx| logo.get(idx));
        let (left_width, left) = entry.map_or((0, ""), |(w, s)| (*w, s.as_str()));
        let content = right.get(i).map(String::as_str).unwrap_or("");

        // 右列没有内容时不要拖尾空格。
        let line = if content.is_empty() {
            left.to_owned()
        } else {
            let pad = " ".repeat(logo_width.saturating_sub(left_width) + GAP);
            format!("{left}{pad}{content}")
        };
        println!("{}", line.trim_end());
    }
}

/// `--json`：把同一份展示模型序列化成 JSON 并打印。
///
/// 与 [`render`] 是同一模型的两个渲染器 —— 终端版给人看，JSON 版给脚本看。
/// 两者读的是同一个 [`Info`]，所以不存在「界面加了字段、JSON 忘了加」的可能。
pub fn print_json(info: &Info) {
    println!("{}", json_text(info));
}

/// 序列化后的 JSON 文本。单独抽出来是为了让测试能直接断言内容。
///
/// `to_string_pretty` 会拒绝 `NaN` / `±inf`（JSON 没有这两个值），失败时
/// 退回一个空对象而不是 panic。**正常情况下走不到这条分支**：`main` 在装配
/// 模型时已经把非有限的刷新率与缩放系数滤掉了，这里只是最后一道保险。
fn json_text(info: &Info) -> String {
    serde_json::to_string_pretty(info).unwrap_or_else(|_| "{}".to_owned())
}

/// 左列的全部行：Logo 主体（渐变）+ 标语，返回 `(显示宽度, 已着色文本)`。
///
/// 同时返回宽度是因为着色后的字符串无法再量宽度（ANSI 序列会被算进去），
/// 必须在这里把纯文本宽度一并带出来。
fn logo_lines(logo: &Logo, truecolor: bool) -> Vec<(usize, String)> {
    match logo {
        Logo::None => Vec::new(),
        Logo::Custom(art) => {
            let last = art.len().saturating_sub(1);
            art.iter()
                .enumerate()
                .map(|(i, line)| {
                    (
                        // 用 `plain_width` 而非 `display_width`：用户可能往 Logo
                        // 里塞 ANSI 颜色，转义码不该算进列宽。
                        plain_width(line),
                        line.as_str()
                            .color(logo_color(i, last, truecolor))
                            .to_string(),
                    )
                })
                .collect()
        }
        Logo::Niri => {
            let last = LOGO_ART.len().saturating_sub(1);
            let mut lines: Vec<(usize, String)> = LOGO_ART
                .iter()
                .enumerate()
                .map(|(i, line)| {
                    (
                        display_width(line),
                        line.color(logo_color(i, last, truecolor)).to_string(),
                    )
                })
                .collect();

            let tagline_color = if truecolor {
                let (r, g, b) = TAGLINE_TRUECOLOR;
                Color::TrueColor { r, g, b }
            } else {
                Color::Cyan
            };
            lines.push((
                display_width(LOGO_TAGLINE),
                LOGO_TAGLINE.italic().color(tagline_color).to_string(),
            ));

            lines
        }
    }
}

/// 一行字段：`<图标> <标签><补白><值>`。
fn field_row(label: &str, icon: &str, value: &str, label_width: usize, icons: bool) -> String {
    let icon_part = if icons {
        format!("{} ", icon.color(ICON_COLOR))
    } else {
        String::new()
    };
    let pad = " ".repeat(label_width.saturating_sub(label.chars().count()) + GAP);
    format!("{icon_part}{}{pad}{value}", label.bold().color(LABEL_COLOR))
}

/// 标题行下方的两段式分隔线：左段对齐标签列，右段对齐值列。
fn separator_row(label_width: usize, rule_width: usize, icon_slot: usize) -> String {
    format!(
        "{}{}{}{}",
        // 有图标时用等宽空格占住图标列，让左段与标签对齐。
        " ".repeat(icon_slot),
        "─".repeat(label_width).color(RULE_COLOR),
        " ".repeat(GAP),
        "─".repeat(rule_width).color(RULE_COLOR),
    )
}

// ════════════════════════════════════════════════════════════════════
//  字段格式化
// ════════════════════════════════════════════════════════════════════

/// WM：`Niri 25.05 (1a2b3c4)`。
fn format_wm(version: Option<&str>) -> String {
    match version.map(str::trim).filter(|s| !s.is_empty()) {
        Some(v) => format!("{} {}", "Niri".bold(), v),
        None => unknown(),
    }
}

/// 终端：`kitty (Window: cargo run ~/W/nirifetch)`。
///
/// 聚焦窗口不再单独占一行 —— 在终端里跑 nirifetch 时，窗口的 `app_id` 就是
/// 终端自己，拆成两行会得到 `Terminal kitty` 和 `Window kitty · ...` 这种重复。
/// 合并后既省一行，也让「窗口信息属于谁」一目了然。
fn format_terminal(terminal: Option<&str>, window: &WindowView, title_max: usize) -> String {
    let Some(terminal) = terminal.map(str::trim).filter(|s| !s.is_empty()) else {
        // 认不出终端时，至少别把聚焦窗口也一起丢掉。
        return format_window(window, title_max);
    };

    let mut out = terminal.bold().cyan().to_string();
    if let Some(summary) = window_summary(window, terminal, title_max) {
        out.push_str(&format!(
            "{}{}{}",
            " (".bright_black(),
            summary,
            ")".bright_black()
        ));
    }
    out
}

/// 聚焦窗口：`app_id · 标题`，浮动窗口附标记。
fn format_window(w: &WindowView, title_max: usize) -> String {
    let title = w.title.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let app_id = w.app_id.as_deref().map(str::trim).filter(|s| !s.is_empty());

    let mut out = match (app_id, title) {
        (Some(a), Some(t)) => format!(
            "{} {} {}",
            a.bold().cyan(),
            "·".bright_black(),
            truncate(t, title_max).bold()
        ),
        (Some(a), None) => a.bold().cyan().to_string(),
        (None, Some(t)) => truncate(t, title_max).bold().to_string(),
        // 两种标识都拿不到：多半是没有窗口获得焦点。
        (None, None) => return unknown(),
    };

    if w.floating {
        out.push_str(&format!(" {}", "[floating]".yellow()));
    }
    out
}

/// 终端行括号里的窗口摘要。没有任何可说的内容时返回 `None`。
fn window_summary(window: &WindowView, terminal: &str, title_max: usize) -> Option<String> {
    let title = window
        .title
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let app_id = window
        .app_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    // app_id 与终端同名是绝大多数情况，再写一遍纯属噪音。
    let same_as_terminal = |a: &str| a.eq_ignore_ascii_case(terminal);

    let body = match (app_id, title) {
        (Some(a), Some(t)) if same_as_terminal(a) => truncate(t, title_max).bold().to_string(),
        (Some(a), Some(t)) => format!(
            "{} {} {}",
            a.bold().cyan(),
            "·".bright_black(),
            truncate(t, title_max).bold()
        ),
        // 只有 app_id 且就是终端本身 —— 没有额外信息可给。
        (Some(a), None) if same_as_terminal(a) => return None,
        (Some(a), None) => a.bold().cyan().to_string(),
        (None, Some(t)) => truncate(t, title_max).bold().to_string(),
        (None, None) => return None,
    };

    let mut inner = format!("{} {}", "Window:".bright_black(), body);
    if window.floating {
        inner.push_str(&format!(" {}", "[floating]".yellow()));
    }
    Some(inner)
}

/// CPU：`AMD Ryzen 7 5800X3D (8 cores / 16 threads)`。
///
/// 预算不够时先丢括号里的核心数，再考虑截断型号 —— 型号是这一行的身份，
/// `AMD Ryzen 7 87…` 这种残缺反而认不出是哪颗 U。
fn format_cpu(cpu: Option<&CpuView>, budget: usize) -> String {
    let Some(cpu) = cpu else {
        return unknown();
    };

    let cores = match cpu.physical {
        // 有物理核心数时两个都给 —— 只说「16 核」在 8 核 16 线程的机器上是错的。
        Some(physical) => format!("{physical} cores / {} threads", cpu.logical),
        None => format!("{} cores", cpu.logical),
    };
    let suffix = format!(" ({cores})");

    if display_width(&cpu.model) + display_width(&suffix) <= budget {
        return format!("{}{}", cpu.model.bold().cyan(), suffix.bright_black());
    }
    truncate(&cpu.model, budget).bold().cyan().to_string()
}

/// GPU：`AMD HawkPoint1 (amdgpu)`；多卡时全部列出并标注正在驱动显示器的那张。
///
/// 与 [`format_cpu`] 同样的取舍：先丢驱动名（次要信息），再考虑截断型号。
fn format_gpu(gpus: &[GpuView], budget: usize) -> String {
    if gpus.is_empty() {
        return unknown();
    }

    // 只有一张卡时「哪张在用」不是有效信息，标出来只是噪音。
    let show_active = gpus.len() > 1;

    let render = |with_driver: bool| -> String {
        let separator = format!(" {} ", "·".bright_black());
        gpus.iter()
            .map(|gpu| {
                let mut text = gpu.name.bold().cyan().to_string();
                if let Some(driver) = gpu.driver.as_deref().filter(|_| with_driver) {
                    text.push_str(&format!(" {}", format!("({driver})").bright_black()));
                }
                if show_active && gpu.active {
                    text.push_str(&format!(" {}", "[active]".green()));
                }
                text
            })
            .collect::<Vec<_>>()
            .join(&separator)
    };

    // 带驱动名的版本放不下就退回不带驱动名的版本。
    let full = render(true);
    if plain_width(&full) <= budget {
        return full;
    }
    let lean = render(false);
    if plain_width(&lean) <= budget {
        return lean;
    }

    // 连型号都放不下才动刀。多卡时截断位置会落在某一型号中间，
    // 但这是极窄终端下的最后手段，总好过撑破右边界。
    truncate(&lean, budget)
}

/// 输出：`DP-1 (2560x1440@144Hz · Scale 1x · Focused)`。
///
/// `budget` 是值的可用列数。放不下时**从尾部开始丢元信息**（旋转 / VRR 最先走，
/// 分辨率最后走），而不是硬截断 —— 把 `2560x1440@144Hz` 切成 `2560x14…` 比
/// 干脆不显示更糟。
fn format_output(o: Option<&OutputView>, budget: usize) -> String {
    let Some(o) = o else {
        return unknown();
    };

    // 每一项是 `(无色文本, 已上色文本)`。分开存是因为 `display_width` 会把
    // ANSI 转义字节也当成列数，直接量上色后的串会严重高估宽度。
    // 顺序即优先级：越靠后越先被丢弃。
    let mut meta: Vec<(String, String)> = Vec::new();

    if let Some((w, h)) = o.physical {
        let hz = o
            .refresh_hz
            .map(|hz| format!("@{hz}Hz"))
            .unwrap_or_default();
        let plain = format!("{w}x{h}{hz}");
        meta.push((plain.clone(), plain.cyan().to_string()));
    }
    if let Some(scale) = o.scale {
        let plain = format!("Scale {scale}x");
        meta.push((plain.clone(), plain.bright_black().to_string()));
    }
    {
        let plain = o.source_label.to_owned();
        meta.push((plain.clone(), plain.bright_black().to_string()));
    }

    match o.vrr_enabled {
        Some(true) => meta.push(("VRR".to_owned(), "VRR".green().to_string())),
        Some(false) => meta.push(("VRR off".to_owned(), "VRR off".bright_black().to_string())),
        None => {}
    }
    if let Some(transform) = o.transform.as_deref() {
        meta.push((transform.to_owned(), transform.yellow().to_string()));
    }

    let render = |meta: &[(String, String)]| -> String {
        if meta.is_empty() {
            return o.name.bold().cyan().to_string();
        }
        let separator = format!(" {} ", "·".bright_black());
        let colored: Vec<&str> = meta.iter().map(|(_, colored)| colored.as_str()).collect();
        format!("{} ({})", o.name.bold().cyan(), colored.join(&separator))
    };

    // 一直丢到放得下为止；全丢光就只剩输出名，由下面的 truncate 收尾。
    // 宁可少说一句，也不让这一行撑破右边界。
    while !meta.is_empty() && plain_width(&render(&meta)) > budget {
        meta.pop();
    }

    let out = render(&meta);
    if plain_width(&out) <= budget {
        return out;
    }
    truncate(&o.name, budget).bold().cyan().to_string()
}

/// 状态栏 / 桌面外壳，如 `Waybar`。
///
/// 值本身就是 [`crate::bar`] 规范化好的展示名（`&'static str`），这里只负责
/// 着色与兜底 —— 与 Font 一行同样的处理，所以不单独做降级：名字最长的
/// `Dank Material Shell (DMS)` 也只有 25 格，窄终端交给 `field_value` 的
/// 统一截断即可。
fn format_bar(bar: Option<&str>) -> String {
    match bar.map(str::trim).filter(|s| !s.is_empty()) {
        Some(name) => name.bold().to_string(),
        None => unknown(),
    }
}

/// 配置：`~/.config/niri/config.kdl (6.6 KiB · 135 lines) ✓`。
///
/// 预算不够时按优先级逐级舍弃：**先丢错误摘要**（最占地方），再丢括号里的
/// 元信息，但要尽量保住校验标记；最后才考虑截断路径 —— 路径是这一行存在的
/// 理由，`~/.config/nir…` 这种残缺路径反而让人认不出是哪个文件。
fn format_config(c: &ConfigView, home: Option<&str>, budget: usize) -> String {
    let path = shorten_home(&c.path, home);

    if !c.exists {
        let full = format!("{}{}", path.red(), "  not found".red().bold());
        if plain_width(&full) <= budget {
            return full;
        }
        return truncate(&path, budget).red().to_string();
    }

    let mut meta: Vec<String> = Vec::new();
    if let Some(size) = c.size {
        meta.push(humanize_bytes(size));
    }
    if let Some(lines) = c.lines {
        meta.push(format!("{lines} lines"));
    }
    if let Some(label) = c.source_label {
        meta.push(label.to_owned());
    }
    let meta_suffix = if meta.is_empty() {
        String::new()
    } else {
        format!(" {}", format!("({})", meta.join(" · ")).bright_black())
    };

    let (marker, message) = match &c.validation {
        Some(v) if v.ok => (Some("✓".green().to_string()), None),
        Some(v) => (Some("✗".red().bold().to_string()), v.message.clone()),
        None => (None, None),
    };

    let base = format!("{}{meta_suffix}", path.bold());
    let path_only = path.bold().to_string();

    // 候选由「最完整」到「最精简」，取第一个放得下的。
    // 校验标记比元信息更值得保住，所以在丢掉元信息之后仍留一档 `路径 + 标记`。
    let mut candidates: Vec<String> = Vec::new();
    if let (Some(marker), Some(message)) = (&marker, &message) {
        candidates.push(format!("{base} {marker} {}", message.bright_black()));
    }
    if let Some(marker) = &marker {
        candidates.push(format!("{base} {marker}"));
        candidates.push(format!("{path_only} {marker}"));
    }
    if !meta_suffix.is_empty() {
        candidates.push(base);
    }
    candidates.push(path_only);

    for candidate in candidates {
        if plain_width(&candidate) <= budget {
            return candidate;
        }
    }

    // 连路径都放不下，才轮到截断它。
    truncate(&path, budget).bold().to_string()
}

/// 工作区：聚焦序号 + 总数 + 窗口数。
fn format_workspace(w: Option<&WorkspaceView>) -> String {
    let Some(w) = w else {
        return unknown();
    };
    let head = match w.focused_idx {
        Some(idx) => format!(
            "{} {}",
            idx.to_string().bold().cyan(),
            format!("of {}", w.total).bright_black()
        ),
        None => format!(
            "{} {}",
            w.total.to_string().bold().cyan(),
            "workspaces".bright_black()
        ),
    };
    format!(
        "{head} {} {}",
        "·".bright_black(),
        format!("{} windows", w.windows).bright_black()
    )
}

/// 键盘布局：当前布局名。
fn format_keyboard(name: Option<&str>) -> String {
    name.map(str::trim)
        .filter(|s| !s.is_empty())
        .map_or_else(unknown, |name| name.bold().cyan().to_string())
}

/// 内存：`3.5 GiB / 19.3 GiB`（已用 / 总量）。
fn format_memory(m: Option<&MemoryView>) -> String {
    let Some(m) = m else {
        return unknown();
    };
    let used = m.total_kib.saturating_sub(m.available_kib);
    format!(
        "{} {} {}",
        humanize_kib(used).bold().cyan(),
        "/".bright_black(),
        humanize_kib(m.total_kib).bright_black()
    )
}

/// 磁盘：`37.9 GiB / 231.9 GiB · 17%`。
fn format_disk(d: Option<&DiskView>) -> String {
    let Some(d) = d else {
        return unknown();
    };
    let percent = d.used_kib.saturating_mul(100).checked_div(d.total_kib);
    let mut text = format!(
        "{} {} {}",
        humanize_kib(d.used_kib).bold().cyan(),
        "/".bright_black(),
        humanize_kib(d.total_kib).bright_black()
    );
    if let Some(percent) = percent {
        text.push_str(&format!(
            " {} {}",
            "·".bright_black(),
            format!("{percent}%").bright_black()
        ));
    }
    text
}

/// 内核版本。
fn format_kernel(kernel: Option<&str>) -> String {
    plain_or_unknown(kernel, |text| text.bold().cyan().to_string())
}

/// 当前 Shell。
fn format_shell(shell: Option<&str>) -> String {
    plain_or_unknown(shell, |text| text.bold().cyan().to_string())
}

/// 运行时长：`3d 4h` / `4h 5m` / `5m`。
fn format_uptime(seconds: Option<u64>) -> String {
    let Some(seconds) = seconds else {
        return unknown();
    };
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    let minutes = (seconds % 3_600) / 60;
    let text = if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    };
    text.bold().cyan().to_string()
}

/// 软件包数量：`1061 (pacman)`。
fn format_packages(p: Option<&PackageView>) -> String {
    let Some(p) = p else {
        return unknown();
    };
    format!(
        "{} {}",
        p.count.to_string().bold().cyan(),
        format!("({})", p.manager).bright_black()
    )
}

/// 电池：`85% · Charging`。充电 / 满电标绿，低电量放电标红。
fn format_battery(b: Option<&BatteryView>) -> String {
    let Some(b) = b else {
        return unknown();
    };
    let percent = format!("{}%", b.percent);
    let charging = |s: &str| s.eq_ignore_ascii_case("charging") || s.eq_ignore_ascii_case("full");
    let colored = match b.status.as_deref() {
        Some(status) if charging(status) => percent.green().to_string(),
        Some(status) if status.eq_ignore_ascii_case("discharging") && b.percent <= 20 => {
            percent.red().to_string()
        }
        _ => percent.bold().cyan().to_string(),
    };

    match b.status.as_deref().filter(|s| !s.is_empty()) {
        Some(status) => format!("{colored} {} {}", "·".bright_black(), status.bright_black()),
        None => colored,
    }
}

/// 平均负载：`0.42 0.55 0.48`。
fn format_load(load: Option<[f32; 3]>) -> String {
    let Some([one, five, fifteen]) = load else {
        return unknown();
    };
    format!("{one:.2} {five:.2} {fifteen:.2}")
        .bold()
        .cyan()
        .to_string()
}

/// 把 `Option<&str>` 渲染成「加粗值」或 `Unknown` 的通用小工具。
fn plain_or_unknown(text: Option<&str>, render: impl Fn(&str) -> String) -> String {
    text.map(str::trim)
        .filter(|s| !s.is_empty())
        .map_or_else(unknown, render)
}

/// KiB → 人类可读字节数。
fn humanize_kib(kib: u64) -> String {
    humanize_bytes(kib.saturating_mul(1024))
}

/// 一行 8 个色块，展示终端调色板。
///
/// `palette` 是终端真实配色（来自 kitty 配置）；为 `None` 或不是 8 色时，
/// 退回内置的高亮档色板 —— 普通档的 0 号黑在深色终端上几乎不可见。
pub fn palette_line(palette: Option<&[(u8, u8, u8)]>) -> String {
    let swatches: Vec<String> = match palette {
        Some(colors) if colors.len() == 8 => colors
            .iter()
            .map(|&(r, g, b)| "●".color(Color::TrueColor { r, g, b }).to_string())
            .collect(),
        _ => PALETTE
            .iter()
            .map(|color| "●".color(*color).to_string())
            .collect(),
    };
    swatches.join(" ")
}

// ════════════════════════════════════════════════════════════════════
//  提示文案
// ════════════════════════════════════════════════════════════════════

/// 不在 niri 会话中时的友好报错（走 stderr，不 panic，退出码由调用方决定）。
pub fn print_session_error(err: &SessionError) {
    eprintln!(
        "{} {}",
        "✗ nirifetch".red().bold(),
        "no running Niri session detected".red()
    );
    eprintln!();

    match err {
        SessionError::NoWayland => {
            eprintln!(
                "  This environment has no {} variable — it does not look like a Wayland session.",
                "WAYLAND_DISPLAY".cyan()
            );
            eprintln!(
                "  {}",
                "nirifetch talks to niri over its IPC socket, so it must run inside niri."
                    .bright_black()
            );
        }
        SessionError::NotNiri { desktop } => {
            if desktop.trim().is_empty() {
                eprintln!(
                    "  This is a Wayland session, but the desktop could not be confirmed as {}.",
                    "niri".cyan()
                );
            } else {
                eprintln!("  Current desktop is {}, not niri.", desktop.cyan());
            }
        }
    }

    eprintln!();
    eprintln!("{}", "Things to check:".bold());
    eprintln!(
        "  {} make sure niri is running and nirifetch is launched from a terminal inside niri",
        "·".bright_black()
    );
    eprintln!(
        "  {} check that {} points at a live socket",
        "·".bright_black(),
        "NIRI_SOCKET".cyan()
    );
    eprintln!(
        "  {} from another TTY / SSH, set it explicitly: {}",
        "·".bright_black(),
        "NIRI_SOCKET=/run/user/$UID/niri.wayland-1.<pid>.sock nirifetch".cyan()
    );
}

/// `--help` 文案。
pub fn print_help() {
    println!(
        "{} {} — a lightweight fetch tool for the Niri Wayland compositor",
        "nirifetch".bold().cyan(),
        env!("CARGO_PKG_VERSION")
    );
    println!();
    println!("{}", "USAGE:".bold());
    println!("  nirifetch [OPTIONS]");
    println!();
    println!("{}", "OPTIONS:".bold());
    for (flag, desc) in [
        ("-h, --help", "print this help"),
        ("-v, -V, --version", "print nirifetch version"),
        (
            "--json",
            "print every field as JSON instead of the fetch layout",
        ),
        (
            "--fields <list>",
            "only show these comma-separated fields (see FIELDS)",
        ),
        ("--short", "show a compact subset of the fields"),
        ("--no-logo", "hide the logo"),
        ("--ascii", "disable the truecolor logo gradient"),
        (
            "--logo-file <path>",
            "use a custom logo read from a text file",
        ),
    ] {
        // 先按纯文本补齐再着色 —— 宽度说明见 `pad` 相关注释。
        println!("  {}{}", format!("{flag:<22}").cyan(), desc);
    }
    println!();
    println!("{}", "FIELDS:".bold());
    for field in Field::ALL {
        println!(
            "  {}{}",
            format!("{:<22}", field.name()).cyan(),
            field.description()
        );
    }
    println!();
    println!(
        "  {}",
        "pass names to --fields, e.g. --fields os,wm,cpu,gpu".bright_black()
    );
    println!();
    println!("{}", "ENVIRONMENT:".bold());
    for (var, desc) in [
        ("NIRIFETCH_ICONS=0", "disable Nerd Font icons"),
        (
            "NO_COLOR=1",
            "disable colors (CLICOLOR_FORCE=1 forces them)",
        ),
        ("NIRI_SOCKET", "path to the niri IPC socket"),
    ] {
        println!("  {}{}", format!("{var:<22}").cyan(), desc);
    }
    println!();
    println!("{}", "AUTHOR:".bold());
    println!("  {}", env!("CARGO_PKG_AUTHORS"));
}

/// `--version` 文案。
pub fn print_version() {
    println!("nirifetch {}", env!("CARGO_PKG_VERSION"));
}

/// 认不出的命令行参数。
///
/// 不静默忽略：`nirifetch --jsno` 若无其事地打印整套 fetch 输出，会让人以为
/// 参数生效了。宁可在这里明确报错。
pub fn print_unknown_argument(arg: &str) {
    eprintln!(
        "{} unknown option {}",
        "✗ nirifetch".red().bold(),
        format!("`{arg}`").red()
    );
    eprintln!();
    eprintln!(
        "  Run {} to see the available options.",
        "nirifetch --help".cyan()
    );
}

/// 选项的取值非法（例如 `--fields` 里出现了不认识的字段名）。
pub fn print_bad_value(option: &str, value: &str) {
    eprintln!(
        "{} invalid value {} for {}",
        "✗ nirifetch".red().bold(),
        format!("`{value}`").red(),
        option.cyan()
    );
    eprintln!();
    eprintln!(
        "  Field names are listed under FIELDS in {}.",
        "nirifetch --help".cyan()
    );
}

/// 选项缺少必需的取值（例如以 `--fields` 结尾）。
pub fn print_missing_value(option: &str) {
    eprintln!(
        "{} option {} needs a value",
        "✗ nirifetch".red().bold(),
        option.cyan()
    );
    eprintln!();
    eprintln!(
        "  Run {} to see the available options.",
        "nirifetch --help".cyan()
    );
}

// ════════════════════════════════════════════════════════════════════
//  内部工具
// ════════════════════════════════════════════════════════════════════

/// 统一的「取不到」占位符，保证各字段兜底文案一致。
fn unknown() -> String {
    "Unknown".bright_black().to_string()
}

/// 按行号在渐变停靠点之间插值取色；终端不支持真彩色时退化为单一青色。
fn logo_color(index: usize, last_index: usize, truecolor: bool) -> Color {
    if !truecolor {
        return Color::Cyan;
    }

    let last_stop = GRADIENT_STOPS.len() - 1;
    let t = if last_index == 0 {
        0.0
    } else {
        index.min(last_index) as f64 / last_index as f64
    };

    // 把 [0,1] 的进度映射到停靠点区间上，在相邻两点之间线性插值。
    let position = t * last_stop as f64;
    let lower = (position.floor() as usize).min(last_stop);
    let upper = (lower + 1).min(last_stop);
    let frac = position - lower as f64;

    let mix = |a: u8, b: u8| (f64::from(a) + (f64::from(b) - f64::from(a)) * frac).round() as u8;
    Color::TrueColor {
        r: mix(GRADIENT_STOPS[lower].0, GRADIENT_STOPS[upper].0),
        g: mix(GRADIENT_STOPS[lower].1, GRADIENT_STOPS[upper].1),
        b: mix(GRADIENT_STOPS[lower].2, GRADIENT_STOPS[upper].2),
    }
}

/// 是否输出 Nerd Font 图标。
///
/// 默认开启；`NIRIFETCH_ICONS=0` 可关闭，方便在不支持 Nerd Font 的终端里
/// 使用，或把输出直接贴进 issue / 聊天记录。
fn icons_enabled() -> bool {
    match env_non_empty("NIRIFETCH_ICONS") {
        Some(value) => !matches!(
            value.to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        ),
        None => true,
    }
}

/// 终端是否支持 24 位真彩色。
fn supports_truecolor() -> bool {
    if let Ok(colorterm) = std::env::var("COLORTERM") {
        let value = colorterm.to_ascii_lowercase();
        if value.contains("truecolor") || value.contains("24bit") {
            return true;
        }
    }
    std::env::var("TERM").is_ok_and(|t| {
        let t = t.to_ascii_lowercase();
        t.contains("direct") || t.contains("truecolor")
    })
}

/// 读取终端宽度，用于推算值列的预算。
///
/// 不引入 `terminal_size` 依赖：优先用 `$COLUMNS`（shell 通常会导出），
/// 拿不到就退回一个保守的默认值。
fn terminal_width() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|c| c.trim().parse::<usize>().ok())
        .filter(|w| *w >= 40)
        .unwrap_or(DEFAULT_TERM_WIDTH)
}

/// 单个字符在终端里占用的列数。
///
/// 不引入 `unicode-width` 依赖，只覆盖实际会遇到的宽字符区段：
/// CJK 汉字、假名、谚文、全角标点与常见 emoji 记 2 格，组合记号记 0 格，
/// 其余（含拉丁字母与 `◑` 这类「宽度有歧义」的符号）按 1 格处理。
fn char_width(ch: char) -> usize {
    match ch as u32 {
        // 组合记号、零宽字符、变体选择符：不占位
        0x0300..=0x036F | 0x200B..=0x200F | 0xFE00..=0xFE0F => 0,
        // 东亚宽字符与全角形式
        0x1100..=0x115F       // 谚文字母
        | 0x2E80..=0x303E     // CJK 部首、假名标点、CJK 符号
        | 0x3041..=0x33FF     // 假名、注音、CJK 兼容
        | 0x3400..=0x4DBF     // CJK 扩展 A
        | 0x4E00..=0x9FFF     // CJK 基本区
        | 0xA000..=0xA4CF     // 彝文
        | 0xAC00..=0xD7A3     // 谚文音节
        | 0xF900..=0xFAFF     // CJK 兼容表意
        | 0xFE30..=0xFE6F     // CJK 兼容形式
        | 0xFF00..=0xFF60     // 全角 ASCII
        | 0xFFE0..=0xFFE6     // 全角符号
        | 0x1F300..=0x1F64F   // emoji
        | 0x1F900..=0x1F9FF
        | 0x20000..=0x3FFFD => 2,
        _ => 1,
    }
}

/// 字符串在终端里占用的总列数。**入参必须是纯文本** —— 它不认识 ANSI 转义，
/// 传入已上色的串会把转义字节也算成列。上色后的串请用 [`plain_width`]。
fn display_width(s: &str) -> usize {
    s.chars().map(char_width).sum()
}

/// 已上色字符串的**可见**列数：跳过 ANSI 转义序列后再量。
///
/// 排版预算必须在「构造字段」的同时校验，而字段是边拼边上色的，
/// 所以需要一个能直接看穿转义码的度量。
fn plain_width(s: &str) -> usize {
    let mut width = 0;
    let mut chars = s.chars();
    while let Some(ch) = chars.next() {
        if ch != '\x1b' {
            width += char_width(ch);
            continue;
        }
        // CSI 序列形如 `ESC [ 参数… m`，一路跳到终结字母为止。
        // 只认这一种形式，够用且不会误吞普通文本。
        for c in chars.by_ref() {
            if c.is_ascii_alphabetic() {
                break;
            }
        }
    }
    width
}

/// 按**显示宽度**截断，超出部分以 `…` 收尾。
///
/// 用显示宽度而非字符数：中日韩字符占两格，按字符数截断会让实际输出比
/// 预算宽一倍，长标题会撑破排版。按 `char` 迭代也顺带保证了不会切出
/// 半个 UTF-8 序列。
///
/// 认得 ANSI 转义序列：转义码原样保留且不计入列宽，否则已上色的串会被
/// 提前截断（一条 `ESC[36m` 就要吃掉 5 格预算）。若截断点落在着色区间内，
/// 会补一个 `ESC[0m` 收尾，免得省略号连同后面的内容一起被染色。
fn truncate(s: &str, max_cells: usize) -> String {
    if plain_width(s) <= max_cells {
        return s.to_owned();
    }

    // 给省略号本身留出一格。
    let budget = max_cells.saturating_sub(1);
    let mut out = String::new();
    let mut used = 0;
    let mut saw_escape = false;
    let mut chars = s.chars();

    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            // 转义序列整段搬运，且不占列宽。
            saw_escape = true;
            out.push(ch);
            for c in chars.by_ref() {
                out.push(c);
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        let width = char_width(ch);
        if used + width > budget {
            break;
        }
        out.push(ch);
        used += width;
    }

    // 截断点可能落在某个着色区间内，补一个重置再收尾。
    if saw_escape {
        out.push_str("\x1b[0m");
    }
    out.push('…');
    out
}

/// 把家目录前缀缩写为 `~`，让路径更易读。
fn shorten_home(path: &str, home: Option<&str>) -> String {
    match home {
        Some(h) if !h.is_empty() => match path.strip_prefix(h) {
            Some(rest) => format!("~{rest}"),
            None => path.to_owned(),
        },
        _ => path.to_owned(),
    }
}

/// 字节数的人类可读形式，例如 `6.6 KiB`。
fn humanize_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];

    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }

    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

// ════════════════════════════════════════════════════════════════════
//  测试
// ════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_leaves_short_strings_alone() {
        assert_eq!(truncate("kitty", 10), "kitty");
        // 正好等于预算时也不该加省略号。
        assert_eq!(truncate("kitty", 5), "kitty");
    }

    #[test]
    fn truncate_respects_display_width() {
        // 逐字节截断会切出半个 UTF-8 序列（进而 panic），
        // 按字符数截断则会让 CJK 标题撑破排版 —— 必须按显示宽度来。
        let title = "开发 nirifetch 命令行工具";
        let out = truncate(title, 9);

        assert!(out.ends_with('…'));
        assert!(
            display_width(&out) <= 9,
            "截断后宽度 {} 超出预算",
            display_width(&out)
        );
        // 截断结果必须是原串的前缀（省略号除外），不能切出乱码。
        let body = out.trim_end_matches('…');
        assert!(title.starts_with(body));
    }

    #[test]
    fn truncate_handles_degenerate_budget() {
        // 预算为 0 / 1 时不应 panic，也不能凭空多出字符。
        assert_eq!(truncate("abc", 0), "…");
        assert_eq!(truncate("abc", 1), "…");
    }

    /// 手工拼一个上色串。不用 `Colorize`：`colored` 在非 TTY（测试环境）下
    /// 会自动关闭着色，那样断言就退化成在测纯文本，白白放过真问题。
    fn painted(text: &str) -> String {
        format!("\x1b[36m{text}\x1b[0m")
    }

    #[test]
    fn truncate_ignores_ansi_escapes() {
        // `ESC[36m` 有 5 个字符，若被算进列宽，上色后的串会被提前截断。
        let colored = painted("abcdefgh");
        assert_eq!(plain_width(&colored), 8);
        // `ESC[36m` 5 字符 + 8 + `ESC[0m` 4 字符。
        assert_eq!(display_width(&colored), 17, "前提：转义码确实占字符数");

        // 放得下时原样返回，一个字节都不动。
        assert_eq!(truncate(&colored, 8), colored);

        let cut = truncate(&colored, 4);
        assert_eq!(strip_ansi(&cut), "abc…");
        assert!(plain_width(&cut) <= 4, "实际宽度 {}", plain_width(&cut));
    }

    #[test]
    fn truncate_closes_an_open_colour_run() {
        // 截断点落在着色区间内时必须补重置，否则省略号连同后续内容会被一起染色。
        let cut = truncate(&painted("abcdefgh"), 4);
        assert!(cut.ends_with("\x1b[0m…"), "缺少重置序列：{cut:?}");
        // 纯文本不该被塞进多余的转义码。
        assert!(!truncate("abcdefgh", 4).contains('\x1b'));
    }

    #[test]
    fn plain_width_matches_display_width_on_plain_text() {
        for text in ["", "abc", "中文字", "a·b", "AMD Ryzen 7 5800X3D"] {
            assert_eq!(plain_width(text), display_width(text), "文本 {text:?}");
        }
    }

    #[test]
    fn every_field_is_capped_by_the_budget() {
        // 「任何一行都不撑破右边界」是结构性保证：即便某个格式化函数忘了
        // 做降级（Structure / Font / Palette 目前就没做），兜底截断也要生效。
        let info = full_info();

        // 从「刚好摆得下」到极窄，逐个预算检查每个字段。
        for budget in [VALUE_MAX, 40, 24, VALUE_MIN] {
            for field in Field::ALL {
                let value = field_value(field, &info, budget);
                let width = plain_width(&value);
                assert!(
                    width <= budget,
                    "预算 {budget} 下 {} 字段宽 {width}：{value:?}",
                    field.name()
                );
            }
        }
    }

    #[test]
    fn cjk_takes_two_cells() {
        assert_eq!(char_width('中'), 2);
        assert_eq!(char_width('a'), 1);
        assert_eq!(display_width("中"), 2);
        assert_eq!(display_width("ab"), 2);
        assert_eq!(display_width("中a"), 3);
        // 零宽字符不占位。
        assert_eq!(display_width("\u{200b}"), 0);
    }

    #[test]
    fn humanize_bytes_scales_units() {
        assert_eq!(humanize_bytes(0), "0 B");
        assert_eq!(humanize_bytes(512), "512 B");
        assert_eq!(humanize_bytes(1024), "1.0 KiB");
        assert_eq!(humanize_bytes(6714), "6.6 KiB");
        assert_eq!(humanize_bytes(1024 * 1024), "1.0 MiB");
    }

    #[test]
    fn shorten_home_replaces_only_a_real_prefix() {
        assert_eq!(
            shorten_home("/home/user/.config/niri/config.kdl", Some("/home/user")),
            "~/.config/niri/config.kdl"
        );
        // 不是家目录下的路径不能被改写。
        assert_eq!(
            shorten_home("/etc/niri/config.kdl", Some("/home/user")),
            "/etc/niri/config.kdl"
        );
        // 拿不到 HOME 时原样返回。
        assert_eq!(
            shorten_home("/etc/niri/config.kdl", None),
            "/etc/niri/config.kdl"
        );
        assert_eq!(shorten_home("/etc/x.kdl", Some("")), "/etc/x.kdl");
    }

    #[test]
    fn logo_art_rows_are_uniform_width() {
        // 行宽不一致会导致左右分栏错位，这里把它固化成断言。
        let widths: Vec<usize> = LOGO_ART.iter().map(|l| display_width(l)).collect();
        assert!(
            widths.windows(2).all(|w| w[0] == w[1]),
            "Logo 各行宽度不一致: {widths:?}"
        );
    }

    #[test]
    fn logo_tagline_fits_under_the_art() {
        let art_width = display_width(LOGO_ART[0]);
        assert!(
            display_width(LOGO_TAGLINE) <= art_width,
            "标语比 Logo 主体还宽，会撑破左列"
        );
    }

    #[test]
    fn icons_are_exactly_one_cell_wide() {
        // 图标必须是单格宽，否则整个标签列会被挤歪。
        // 这些码位在实测的 Nerd Font 里 advance width 均等于一个字符宽。
        for field in Field::ALL {
            assert_eq!(
                display_width(field.icon()),
                1,
                "{} 的图标不是单格宽，会破坏对齐",
                field.name()
            );
        }
        // 图标 + 空格正好占满图标列。
        assert_eq!(ICON_SLOT, 2);
    }

    #[test]
    fn palette_line_draws_eight_swatches() {
        // 无论着色是否开启，色块数量都必须是 8；内置与自定义色板都一样。
        assert_eq!(palette_line(None).matches('●').count(), 8);
        let custom = [(1u8, 2u8, 3u8); 8];
        assert_eq!(palette_line(Some(&custom)).matches('●').count(), 8);
        // 长度不对的色板退回内置色板，而不是画出乱七八糟的块数。
        assert_eq!(palette_line(Some(&custom[..3])).matches('●').count(), 8);
    }

    #[test]
    fn every_field_has_a_unique_name() {
        // `--fields` 的解析依赖名字唯一，重名会让其中一个永远选不中。
        for (i, field) in Field::ALL.iter().enumerate() {
            for other in &Field::ALL[i + 1..] {
                assert_ne!(field.name(), other.name(), "字段名重复");
                assert_ne!(field.label(), other.label(), "字段标签重复");
            }
            assert!(Field::from_name(field.name()).is_some());
        }
        // 大小写不敏感，且认不出时返回 None。
        assert_eq!(Field::from_name("GPU"), Some(Field::Gpu));
        assert_eq!(Field::from_name("nope"), None);
    }

    #[test]
    fn output_line_reports_unknown_without_data() {
        assert_eq!(strip_ansi(&format_output(None, VALUE_MAX)), "Unknown");
    }

    #[test]
    fn window_line_reports_unknown_without_data() {
        assert_eq!(
            strip_ansi(&format_window(&WindowView::default(), 40)),
            "Unknown"
        );
    }

    #[test]
    fn window_line_puts_app_id_first() {
        // 布局按 mockup：app_id 在前，标题在后。
        let view = WindowView {
            title: Some("cargo run".to_owned()),
            app_id: Some("kitty".to_owned()),
            floating: false,
        };
        let text = strip_ansi(&format_window(&view, 40));
        assert_eq!(text, "kitty · cargo run");
    }

    #[test]
    fn window_line_truncates_long_titles() {
        let view = WindowView {
            title: Some("x".repeat(200)),
            app_id: Some("kitty".to_owned()),
            floating: false,
        };
        let text = strip_ansi(&format_window(&view, 20));
        assert!(text.contains('…'), "超长标题应被截断：{text}");
        assert!(text.contains("kitty"), "app_id 不应被一起截掉：{text}");
    }

    #[test]
    fn floating_windows_are_marked() {
        let view = WindowView {
            title: Some("terminal".to_owned()),
            app_id: None,
            floating: true,
        };
        assert!(strip_ansi(&format_window(&view, 40)).contains("[floating]"));
    }

    // ── Terminal / Font ───────────────────────────────────────────

    /// 在 `kitty` 里跑 `cargo run` 的典型场景。
    fn kitty_window() -> WindowView {
        WindowView {
            title: Some("cargo run ~/W/nirifetch".to_owned()),
            app_id: Some("kitty".to_owned()),
            floating: false,
        }
    }

    #[test]
    fn terminal_line_folds_the_window_in() {
        // 布局按 mockup：`kitty (Window: cargo run ~/W/nirifetch)`。
        assert_eq!(
            strip_ansi(&format_terminal(Some("kitty"), &kitty_window(), 40)),
            "kitty (Window: cargo run ~/W/nirifetch)"
        );
    }

    #[test]
    fn terminal_line_omits_a_redundant_app_id() {
        // app_id 就是终端本身时只留标题 —— 否则会得到 `kitty (Window: kitty · ...)`。
        let text = strip_ansi(&format_terminal(Some("kitty"), &kitty_window(), 40));
        assert_eq!(text.matches("kitty").count(), 1, "终端名不该重复：{text}");
        // 大小写不同也算同一个。
        let upper = WindowView {
            app_id: Some("Kitty".to_owned()),
            ..kitty_window()
        };
        assert_eq!(
            strip_ansi(&format_terminal(Some("kitty"), &upper, 40))
                .matches("itty")
                .count(),
            1
        );
    }

    #[test]
    fn terminal_line_keeps_a_foreign_app_id() {
        // 聚焦的不是终端时，app_id 必须保留。
        let view = WindowView {
            title: Some("GitHub".to_owned()),
            app_id: Some("firefox".to_owned()),
            floating: false,
        };
        assert_eq!(
            strip_ansi(&format_terminal(Some("kitty"), &view, 40)),
            "kitty (Window: firefox · GitHub)"
        );
    }

    #[test]
    fn terminal_line_without_a_window_shows_just_the_name() {
        let text = strip_ansi(&format_terminal(Some("kitty"), &WindowView::default(), 40));
        assert_eq!(text, "kitty");
        // app_id 与终端同名且没有标题 —— 括号里没有任何信息，不该出现。
        let bare = WindowView {
            title: None,
            app_id: Some("kitty".to_owned()),
            floating: false,
        };
        assert_eq!(
            strip_ansi(&format_terminal(Some("kitty"), &bare, 40)),
            "kitty"
        );
    }

    #[test]
    fn terminal_line_falls_back_to_the_window() {
        // 认不出终端时不能把窗口信息一起丢掉。
        assert_eq!(
            strip_ansi(&format_terminal(None, &kitty_window(), 40)),
            "kitty · cargo run ~/W/nirifetch"
        );
        assert_eq!(
            strip_ansi(&format_terminal(None, &WindowView::default(), 40)),
            "Unknown"
        );
        // 空字符串等同于没有。
        assert_eq!(
            strip_ansi(&format_terminal(Some("  "), &WindowView::default(), 40)),
            "Unknown"
        );
    }

    #[test]
    fn terminal_line_marks_floating_windows() {
        let view = WindowView {
            floating: true,
            ..kitty_window()
        };
        assert!(strip_ansi(&format_terminal(Some("kitty"), &view, 40)).contains("[floating]"));
    }

    #[test]
    fn terminal_line_truncates_long_titles() {
        let view = WindowView {
            title: Some("x".repeat(200)),
            app_id: Some("kitty".to_owned()),
            floating: false,
        };
        let text = strip_ansi(&format_terminal(Some("kitty"), &view, 20));
        assert!(text.contains('…'), "超长标题应被截断：{text}");
        assert!(text.starts_with("kitty (Window: "), "终端名应保留：{text}");
    }

    // ── CPU / GPU ─────────────────────────────────────────────────

    #[test]
    fn cpu_line_reports_both_core_counts() {
        let cpu = CpuView {
            model: "AMD Ryzen 7 5800X3D".to_owned(),
            logical: 16,
            physical: Some(8),
        };
        assert_eq!(
            strip_ansi(&format_cpu(Some(&cpu), VALUE_MAX)),
            "AMD Ryzen 7 5800X3D (8 cores / 16 threads)"
        );
    }

    #[test]
    fn cpu_line_falls_back_to_logical_only() {
        // 非 x86 平台拿不到物理核心数，此时只说逻辑核心数。
        let cpu = CpuView {
            model: "ARMv8 Processor".to_owned(),
            logical: 4,
            physical: None,
        };
        assert_eq!(
            strip_ansi(&format_cpu(Some(&cpu), VALUE_MAX)),
            "ARMv8 Processor (4 cores)"
        );
        assert_eq!(strip_ansi(&format_cpu(None, VALUE_MAX)), "Unknown");
    }

    fn gpu(name: &str, driver: &str, active: bool) -> GpuView {
        GpuView {
            name: name.to_owned(),
            driver: Some(driver.to_owned()),
            active,
        }
    }

    #[test]
    fn gpu_line_omits_the_active_marker_for_a_single_card() {
        // 只有一张卡时「哪张在用」没有信息量。
        let single = vec![gpu("AMD HawkPoint1", "amdgpu", true)];
        assert_eq!(
            strip_ansi(&format_gpu(&single, VALUE_MAX)),
            "AMD HawkPoint1 (amdgpu)"
        );
    }

    #[test]
    fn gpu_line_marks_the_active_card_when_there_are_several() {
        let dual = vec![
            gpu("AMD HawkPoint1", "amdgpu", true),
            gpu("NVIDIA GA107M", "nvidia", false),
        ];
        assert_eq!(
            strip_ansi(&format_gpu(&dual, VALUE_MAX)),
            "AMD HawkPoint1 (amdgpu) [active] · NVIDIA GA107M (nvidia)"
        );
    }

    #[test]
    fn gpu_line_handles_missing_driver_and_no_cards() {
        let no_driver = vec![GpuView {
            name: "Unknown GPU".to_owned(),
            driver: None,
            active: false,
        }];
        assert_eq!(
            strip_ansi(&format_gpu(&no_driver, VALUE_MAX)),
            "Unknown GPU"
        );
        assert_eq!(strip_ansi(&format_gpu(&[], VALUE_MAX)), "Unknown");
    }

    #[test]
    fn logo_is_vertically_centred_against_the_info_column() {
        // 信息栏比 Logo 高，顶部对齐会让左下角空一大块。
        let logo_height = LOGO_ART.len() + 1; // 主体 + 标语
        let info_height = Field::ALL.len() + 1; // 字段 + 分隔线
        assert!(
            info_height > logo_height,
            "本测试假设信息栏更高，若不成立则居中没有意义"
        );

        let offset = info_height.saturating_sub(logo_height) / 2;
        assert!(offset > 0, "应当产生一个非零的居中偏移");
        // 上下留白相差不超过一行，视觉上才是居中的。
        let below = info_height - logo_height - offset;
        assert!(below.abs_diff(offset) <= 1);
    }

    #[test]
    fn output_line_follows_the_documented_shape() {
        let view = OutputView {
            name: "DP-1".to_owned(),
            physical: Some((2560, 1440)),
            refresh_hz: Some(144.0),
            scale: Some(1.0),
            source_label: "Focused",
            vrr_enabled: None,
            transform: None,
        };
        // 预算给足时就是 mockup 里那一行。
        let roomy = VALUE_MAX;
        assert_eq!(
            strip_ansi(&format_output(Some(&view), roomy)),
            "DP-1 (2560x1440@144Hz · Scale 1x · Focused)"
        );
    }

    #[test]
    fn output_line_sheds_meta_before_truncating() {
        let view = OutputView {
            name: "DP-1".to_owned(),
            physical: Some((2560, 1440)),
            refresh_hz: Some(144.0),
            scale: Some(1.0),
            source_label: "Focused",
            vrr_enabled: Some(true),
            transform: Some("90°".to_owned()),
        };
        // 整行 56 格。预算 52 时优先级最低的旋转先走。
        assert_eq!(
            strip_ansi(&format_output(Some(&view), 52)),
            "DP-1 (2560x1440@144Hz · Scale 1x · Focused · VRR)"
        );
        // 一路丢到只剩分辨率，而不是把分辨率切成 `2560x14…`。
        let narrow = strip_ansi(&format_output(Some(&view), 26));
        assert_eq!(narrow, "DP-1 (2560x1440@144Hz)");
        assert!(!narrow.contains('…'), "还有元信息可丢时不该动刀：{narrow}");
    }

    #[test]
    fn output_line_truncates_the_name_as_a_last_resort() {
        let view = OutputView {
            name: "DP-1".to_owned(),
            physical: None,
            refresh_hz: None,
            scale: None,
            source_label: "Focused",
            vrr_enabled: None,
            transform: None,
        };
        // 只剩 `name (Focused)` 一项元信息可丢，丢掉后仍放不下才截断名字。
        assert_eq!(strip_ansi(&format_output(Some(&view), 8)), "DP-1");
        // 连唯一一项元信息都放不下时，丢信息而不是让它溢出。
        assert_eq!(strip_ansi(&format_output(Some(&view), 6)), "DP-1");
        let view = OutputView {
            name: "a-very-long-connector-name".to_owned(),
            ..view
        };
        let text = strip_ansi(&format_output(Some(&view), 8));
        assert!(text.ends_with('…'), "超长输出名应被截断：{text}");
        assert!(display_width(&text) <= 8);
    }

    #[test]
    fn config_line_follows_the_documented_shape() {
        let view = ConfigView {
            path: "/home/user/.config/niri/config.kdl".to_owned(),
            exists: true,
            source_label: None,
            size: Some(6714),
            lines: Some(135),
            validation: None,
        };
        assert_eq!(
            strip_ansi(&format_config(&view, Some("/home/user"), VALUE_MAX)),
            "~/.config/niri/config.kdl (6.6 KiB · 135 lines)"
        );
    }

    #[test]
    fn config_line_drops_the_metadata_before_the_path() {
        let view = ConfigView {
            path: "/home/user/.config/niri/config.kdl".to_owned(),
            exists: true,
            source_label: None,
            size: Some(6714),
            lines: Some(135),
            validation: None,
        };
        // 括号整体让位，换来一个完整可辨认的路径。
        let text = strip_ansi(&format_config(&view, Some("/home/user"), 28));
        assert_eq!(text, "~/.config/niri/config.kdl");
        assert!(!text.contains('…'), "还有元信息可丢时不该动刀：{text}");

        // 连路径都放不下，才轮到截断。
        let tight = strip_ansi(&format_config(&view, Some("/home/user"), 12));
        assert!(tight.ends_with('…'), "实际得到 {tight:?}");
        assert!(display_width(&tight) <= 12);
    }

    #[test]
    fn config_line_handles_a_missing_file_in_narrow_terminals() {
        let view = ConfigView {
            path: "/etc/niri/config.kdl".to_owned(),
            exists: false,
            source_label: Some("system"),
            size: None,
            lines: None,
            validation: None,
        };
        assert_eq!(
            strip_ansi(&format_config(&view, None, VALUE_MAX)),
            "/etc/niri/config.kdl  not found"
        );
        // 放不下时先丢提示语，保住路径。
        assert_eq!(
            strip_ansi(&format_config(&view, None, 20)),
            "/etc/niri/config.kdl"
        );
    }

    #[test]
    fn config_line_renders_the_validation_marker() {
        let base = ConfigView {
            path: "/home/user/.config/niri/config.kdl".to_owned(),
            exists: true,
            source_label: None,
            size: Some(6714),
            lines: Some(135),
            validation: Some(ValidationView {
                ok: true,
                message: None,
            }),
        };
        let ok = strip_ansi(&format_config(&base, Some("/home/user"), VALUE_MAX));
        assert!(ok.ends_with('✓'), "合法配置应以绿勾收尾：{ok}");

        let bad = ConfigView {
            validation: Some(ValidationView {
                ok: false,
                message: Some("unexpected token".to_owned()),
            }),
            ..base
        };
        let text = strip_ansi(&format_config(&bad, Some("/home/user"), VALUE_MAX));
        assert!(text.contains('✗'), "非法配置应有叉号：{text}");
        assert!(text.contains("unexpected token"), "应显示错误摘要：{text}");
    }

    #[test]
    fn config_line_keeps_the_marker_when_the_message_does_not_fit() {
        let view = ConfigView {
            path: "/home/user/.config/niri/config.kdl".to_owned(),
            exists: true,
            source_label: None,
            size: Some(6714),
            lines: Some(135),
            validation: Some(ValidationView {
                ok: false,
                message: Some("identifiers cannot be used as arguments".to_owned()),
            }),
        };
        // 预算放不下整行时，先丢错误摘要，但路径与 ✗ 必须保住。
        let text = strip_ansi(&format_config(&view, Some("/home/user"), 30));
        assert!(
            text.starts_with("~/.config/niri/config.kdl"),
            "路径应保留：{text}"
        );
        assert!(text.contains('✗'), "校验标记应保留：{text}");
        assert!(!text.contains("identifiers"), "应先丢错误摘要：{text}");
        assert!(
            display_width(&text) <= 30,
            "实际宽度 {}",
            display_width(&text)
        );
    }

    #[test]
    fn wm_line_prefixes_the_compositor_name() {
        assert_eq!(
            strip_ansi(&format_wm(Some("25.05 (1a2b3c4)"))),
            "Niri 25.05 (1a2b3c4)"
        );
        assert_eq!(strip_ansi(&format_wm(None)), "Unknown");
        assert_eq!(strip_ansi(&format_wm(Some("   "))), "Unknown");
    }

    #[test]
    fn field_row_aligns_under_icons_and_without_them() {
        let width = label_column_width();
        // 有图标：图标列 + 标签 + 补白，两行的值必须从同一列开始。
        let with = strip_ansi(&field_row("WM", ICON_WM, "v", width, true));
        let with_long = strip_ansi(&field_row("Structure", ICON_STRUCTURE, "v", width, true));
        assert_eq!(display_width(&with), display_width(&with_long));

        // 无图标：同样要对齐。
        let without = strip_ansi(&field_row("WM", ICON_WM, "v", width, false));
        let without_long = strip_ansi(&field_row("Structure", ICON_STRUCTURE, "v", width, false));
        assert_eq!(display_width(&without), display_width(&without_long));

        // 关闭图标后应当少掉整个图标列。
        assert_eq!(display_width(&with) - display_width(&without), ICON_SLOT);
    }

    // ── JSON 输出 ─────────────────────────────────────────────────

    /// 一份字段填满的展示模型，供 JSON 测试使用。
    fn full_info() -> Info {
        Info {
            user: "user".to_owned(),
            host: "host".to_owned(),
            home: Some("/home/user".to_owned()),
            wm: Some("25.05 (1a2b3c4)".to_owned()),
            structure: "Modular (6 included files)".to_owned(),
            window: kitty_window(),
            output: Some(OutputView {
                name: "DP-1".to_owned(),
                physical: Some((2560, 1440)),
                refresh_hz: Some(144.0),
                scale: Some(1.0),
                source_label: "Focused",
                vrr_enabled: Some(false),
                transform: None,
            }),
            config: ConfigView {
                path: "/home/user/.config/niri/config.kdl".to_owned(),
                exists: true,
                source_label: None,
                size: Some(6714),
                lines: Some(135),
                validation: Some(ValidationView {
                    ok: true,
                    message: None,
                }),
            },
            bar: Some("Waybar"),
            terminal: Some("kitty".to_owned()),
            font: Some("Adwaita Mono".to_owned()),
            cpu: Some(CpuView {
                model: "AMD Ryzen 7 5800X3D".to_owned(),
                logical: 16,
                physical: Some(8),
            }),
            gpus: vec![gpu("AMD Radeon RX 6700 XT", "amdgpu", true)],
            workspace: Some(WorkspaceView {
                focused_idx: Some(2),
                total: 3,
                windows: 2,
            }),
            keyboard: Some("English (US)".to_owned()),
            memory: Some(MemoryView {
                total_kib: 20_267_444,
                available_kib: 17_768_624,
            }),
            disk: Some(DiskView {
                mount: "/".to_owned(),
                used_kib: 39_717_116,
                total_kib: 243_148_800,
            }),
            kernel: Some("Linux 7.2.8-arch1-2".to_owned()),
            shell: Some("fish".to_owned()),
            uptime: Some(1047),
            packages: Some(PackageView {
                count: 1061,
                manager: "pacman",
            }),
            battery: Some(BatteryView {
                percent: 85,
                status: Some("Charging".to_owned()),
            }),
            load: Some([0.20, 0.49, 0.38]),
            palette: Some(vec![(0x1e, 0x1e, 0x2e); 8]),
        }
    }

    #[test]
    fn json_output_is_valid_and_carries_every_field() {
        let text = json_text(&full_info());
        let value: serde_json::Value = serde_json::from_str(&text).expect("应当是合法 JSON");

        // 精确锁定 JSON 的 schema：往 `Info` 里加字段却忘了想清楚它该不该
        // 出现在 JSON 里，这条就会失败。
        //
        // 键比字段行多几个是**有意**的：`user` / `host` 在终端里合成一行 `OS`，
        // JSON 里拆成两个键更好用；`home` 根本不是字段行，只是把配置路径缩写
        // 成 `~/...` 的助手；`window` 在终端里并进 Terminal 行，JSON 里独立。
        let obj = value.as_object().expect("顶层应当是对象");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "bar",
                "battery",
                "config",
                "cpu",
                "disk",
                "font",
                "gpus",
                "home",
                "host",
                "kernel",
                "keyboard",
                "load",
                "memory",
                "output",
                "packages",
                "palette",
                "shell",
                "structure",
                "terminal",
                "uptime",
                "user",
                "window",
                "wm",
                "workspace",
            ]
        );

        assert_eq!(value["user"], "user");
        assert_eq!(value["host"], "host");
        assert_eq!(value["bar"], "Waybar");
        assert_eq!(value["cpu"]["logical"], 16);
        assert_eq!(value["cpu"]["physical"], 8);
        assert_eq!(value["gpus"][0]["name"], "AMD Radeon RX 6700 XT");
        assert_eq!(value["gpus"][0]["active"], true);
        assert_eq!(value["window"]["app_id"], "kitty");
        assert_eq!(value["config"]["lines"], 135);
        assert_eq!(value["config"]["validation"]["ok"], true);
        assert_eq!(value["output"]["physical"], serde_json::json!([2560, 1440]));
        assert_eq!(value["output"]["refresh_hz"], 144.0);
        assert_eq!(value["workspace"]["focused_idx"], 2);
        assert_eq!(value["packages"]["manager"], "pacman");
        assert_eq!(value["load"][0], 0.20);
    }

    #[test]
    fn json_output_uses_null_for_missing_data() {
        // 缺字段用 null 而不是省略键：脚本里 `jq .bar` 与 `jq .gpu` 的行为一致，
        // 不用先判断键在不在。
        let info = Info {
            wm: None,
            output: None,
            bar: None,
            font: None,
            cpu: None,
            // 一张卡都没探测到时是**空数组**而不是 null：脚本里 `.[]` 直接遍历，
            // 不用先判空。
            gpus: Vec::new(),
            ..full_info()
        };
        let value: serde_json::Value =
            serde_json::from_str(&json_text(&info)).expect("应当是合法 JSON");

        assert!(value["wm"].is_null());
        assert!(value["output"].is_null());
        assert!(value["bar"].is_null());
        assert!(value["font"].is_null());
        assert!(value["cpu"].is_null());
        assert_eq!(value["gpus"], serde_json::json!([]));
    }

    #[test]
    fn json_output_survives_a_non_finite_float() {
        // JSON 没有 NaN / inf，serde_json 会直接报错。`main` 在装配模型时
        // 已经把它们滤掉了，这里验证「万一漏进来」也不会 panic 或吐出半截 JSON。
        let info = Info {
            output: Some(OutputView {
                name: "DP-1".to_owned(),
                physical: None,
                refresh_hz: Some(f64::NAN),
                scale: None,
                source_label: "Focused",
                vrr_enabled: None,
                transform: None,
            }),
            ..full_info()
        };
        // 兜底结果是空对象，不是 panic，也不是非法 JSON。
        let text = json_text(&info);
        serde_json::from_str::<serde_json::Value>(&text).expect("兜底结果仍须是合法 JSON");
    }

    #[test]
    fn json_output_contains_no_ansi_escapes() {
        // 模型全程是纯文本，着色只发生在 field_value 里 —— 若哪天有人把
        // 着色提前到装配阶段，这条会立刻发现。
        let text = json_text(&full_info());
        assert!(!text.contains('\u{1b}'), "JSON 里混进了 ANSI 转义序列");
    }

    // ── Bar ───────────────────────────────────────────────────────

    #[test]
    fn bar_sits_between_output_and_terminal() {
        // 桌面层级是自上而下的：先讲屏幕（Output），再讲屏幕上的状态栏（Bar），
        // 最后才轮到跑在里面的终端。
        let labels: Vec<&str> = Field::ALL.iter().map(|field| field.label()).collect();
        let output = labels
            .iter()
            .position(|l| *l == "Output")
            .expect("有 Output");
        let bar = labels.iter().position(|l| *l == "Bar").expect("有 Bar");
        let terminal = labels
            .iter()
            .position(|l| *l == "Terminal")
            .expect("有 Terminal");
        assert!(output < bar && bar < terminal, "实际顺序 {labels:?}");
    }

    #[test]
    fn bar_line_shows_the_detected_shell() {
        assert_eq!(strip_ansi(&format_bar(Some("Waybar"))), "Waybar");
        assert_eq!(
            strip_ansi(&format_bar(Some("Dank Material Shell (DMS)"))),
            "Dank Material Shell (DMS)"
        );
    }

    #[test]
    fn bar_line_reports_unknown_without_data() {
        assert_eq!(strip_ansi(&format_bar(None)), "Unknown");
        // 空字符串与纯空白都当作「没检测到」，不能渲染出一行空白。
        assert_eq!(strip_ansi(&format_bar(Some(""))), "Unknown");
        assert_eq!(strip_ansi(&format_bar(Some("   "))), "Unknown");
    }

    /// 去掉 ANSI 转义序列，让断言不受「测试进程是否着色」影响
    /// （结果取决于环境变量，直接断言原文会很脆）。
    fn strip_ansi(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut in_escape = false;
        for ch in s.chars() {
            if in_escape {
                // ANSI 序列以字母收尾，例如 `\x1b[1m` 的 `m`。
                if ch.is_ascii_alphabetic() {
                    in_escape = false;
                }
            } else if ch == '\u{1b}' {
                in_escape = true;
            } else {
                out.push(ch);
            }
        }
        out
    }
}
