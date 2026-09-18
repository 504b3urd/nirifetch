//! 硬件信息探测层（CPU / GPU）。
//!
//! 设计目标是**极速轻量**，因此几乎全部数据都来自 Linux 的虚拟文件系统：
//!
//! - CPU：直接读 `/proc/cpuinfo`，一次 `read` 拿到型号与核心数；
//! - GPU：遍历 `/sys/class/drm/card*`，从 `device/uevent` 一次取齐厂商 / 设备 /
//!   子系统 ID 与驱动名，再拿这些 ID 去 `pci.ids` 换商用名。
//!
//! 唯一的外部数据文件是 `pci.ids`（1.6MB）。这里**刻意不调用 `lspci`**：
//! 实测 `lspci` 单次要 63–112ms，而 nirifetch 目前端到端只要 65ms —— 为了一个
//! 显卡名字让程序慢一倍多并不划算。`pci.ids` 正是 `lspci` 自己的数据源，流式读取
//! 且命中即停（实测只读到全文件的 4%），开销可以忽略。
//!
//! 容错原则：任何一步读不到都退化成 `None`，绝不上报猜测值，也绝不 panic。

use std::io::{BufRead, Read};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::sys;

/// `/proc/cpuinfo` 的读取上限。真实文件只有几 KB，设上限纯属防御。
const MAX_CPUINFO_BYTES: u64 = 256 * 1024;

/// 单个 sysfs 属性的读取上限（`uevent` 只有几百字节）。
const MAX_SYSFS_BYTES: u64 = 64 * 1024;

/// `pci.ids` 的读取上限。文件实测 1.6MB，这里给足余量。
const MAX_PCI_IDS_BYTES: u64 = 8 * 1024 * 1024;

/// DRM 设备类目录，显卡与连接器都在这里。
const DRM_CLASS_DIR: &str = "/sys/class/drm";

/// `pci.ids` 的候选位置。顺序按发行版常见程度排列。
const PCI_IDS_PATHS: [&str; 3] = [
    "/usr/share/hwdata/pci.ids",
    "/usr/share/misc/pci.ids",
    "/usr/share/pci.ids",
];

// ════════════════════════════════════════════════════════════════════
//  CPU
// ════════════════════════════════════════════════════════════════════

/// CPU 的基本信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpuInfo {
    /// 清洗过的型号，例如 `AMD Ryzen 5 7640U`。
    pub model: String,
    /// 逻辑核心数，即 `/proc/cpuinfo` 里 `processor` 条目的数量。
    pub logical: usize,
    /// 物理核心数。x86 上由 `cpu cores` × 插槽数算得；ARM 等平台没有该字段时为 `None`。
    pub physical: Option<usize>,
}

/// 探测 CPU。读不到 `/proc/cpuinfo` 或其中没有型号信息时返回 `None`。
pub fn cpu() -> Option<CpuInfo> {
    let text = sys::read_capped(Path::new("/proc/cpuinfo"), MAX_CPUINFO_BYTES)?;
    parse_cpuinfo(&text)
}

/// 从 `/proc/cpuinfo` 的文本中提取 CPU 信息。
///
/// 抽成纯函数是为了能用真实样例做穷举测试，不必依赖跑测试的机器。
fn parse_cpuinfo(text: &str) -> Option<CpuInfo> {
    let mut raw_model: Option<&str> = None;
    let mut logical = 0usize;
    let mut cores_per_socket: Option<usize> = None;
    // `physical id` 每个逻辑核都会出现一次，去重后才是插槽数。
    let mut sockets: Vec<&str> = Vec::new();

    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "processor" => logical += 1,
            // 多插槽机器上每条都相同，取第一条即可。
            "model name" if raw_model.is_none() => raw_model = Some(value),
            "cpu cores" if cores_per_socket.is_none() => cores_per_socket = value.parse().ok(),
            "physical id" if !sockets.contains(&value) => sockets.push(value),
            _ => {}
        }
    }

    let model = clean_model(raw_model?);
    if model.is_empty() || logical == 0 {
        return None;
    }

    // 物理核心 = 每插槽核心数 × 插槽数。缺任一因子就老实报 None。
    let physical = match (cores_per_socket, sockets.len()) {
        (Some(cores), sockets) if sockets > 0 => Some(cores * sockets),
        _ => None,
    };

    Some(CpuInfo {
        model,
        logical,
        physical,
    })
}

/// 清洗 `/proc/cpuinfo` 里的商业型号串，只保留核心型号。
///
/// 内核原样转述厂商写进 CPU 的字符串，里面塞满了对用户没有信息量的营销与法律后缀：
///
/// | 原始 | 清洗后 |
/// |---|---|
/// | `AMD Ryzen 5 7640U w/ Radeon 760M Graphics` | `AMD Ryzen 5 7640U` |
/// | `12th Gen Intel(R) Core(TM) i5-12400F` | `Intel Core i5-12400F` |
/// | `Intel(R) Xeon(R) CPU E5-2670 v3 @ 2.30GHz` | `Intel Xeon E5-2670 v3` |
/// | `AMD Ryzen 5 5600X 6-Core Processor` | `AMD Ryzen 5 5600X` |
fn clean_model(raw: &str) -> String {
    let mut s = raw.trim().to_owned();

    // 1) 商标标记。替换成空格而不是删掉，避免 `Intel(R)Core` 这类没有空格的写法
    //    把相邻单词粘在一起。
    for mark in ["(R)", "(r)", "(TM)", "(tm)", "(C)", "(c)", "®", "™"] {
        if s.contains(mark) {
            s = s.replace(mark, " ");
        }
    }

    // 2) `@ 2.50GHz` 这类频率后缀 —— 频率不是型号的一部分。
    if let Some(idx) = s.find('@') {
        s.truncate(idx);
    }

    // 3) `w/ Radeon 760M Graphics` / `with Radeon Graphics`：AMD APU 的核显后缀。
    //    核显信息由 GPU 那一行负责，这里重复一遍只会挤占宽度。
    for separator in [" w/ ", " with "] {
        if let Some(idx) = s.find(separator) {
            s.truncate(idx);
        }
    }

    // 4) 开头的 `12th Gen ` / `8th Gen `。
    s = strip_generation_prefix(&s).to_owned();

    // 5) 词级过滤：`CPU` / `Processor` / `6-Core` 都不是型号。
    let cleaned = s
        .split_whitespace()
        .filter(|token| !is_noise_token(token))
        .collect::<Vec<_>>()
        .join(" ");

    // 万一洗没了（例如型号字面量就叫 "CPU"），宁可原样返回也不要显示空白。
    if cleaned.is_empty() {
        raw.trim().to_owned()
    } else {
        cleaned
    }
}

/// 剥掉 `12th Gen ` / `3rd Gen ` 这类代际前缀。
fn strip_generation_prefix(s: &str) -> &str {
    let digits_end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    if digits_end == 0 {
        return s;
    }

    let rest = &s[digits_end..];
    // 用 lower 比较实现大小写不敏感，再按同样的字节长度切原串
    // （后缀全是 ASCII，字节偏移一一对应）。
    let lower = rest.to_ascii_lowercase();
    for suffix in ["st gen ", "nd gen ", "rd gen ", "th gen "] {
        if lower.starts_with(suffix) {
            return &rest[suffix.len()..];
        }
    }
    s
}

/// 判断一个词是不是型号里的噪音。
fn is_noise_token(token: &str) -> bool {
    let lower = token.to_ascii_lowercase();
    if lower == "cpu" || lower == "processor" {
        return true;
    }
    // `6-Core` / `16-core`
    lower
        .strip_suffix("-core")
        .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

// ════════════════════════════════════════════════════════════════════
//  GPU
// ════════════════════════════════════════════════════════════════════

/// 一张显卡的信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuInfo {
    /// 展示用的型号名，例如 `AMD Radeon 760M`。
    pub name: String,
    /// 内核驱动名，例如 `amdgpu`。
    pub driver: Option<String>,
    /// 是否正在驱动一块已连接的显示器 —— 即「当前活跃的渲染卡」。
    pub active: bool,
}

/// 探测全部显卡，活跃的排在前面。
///
/// 双显卡机器（核显 + 独显）会返回多项，由 UI 决定怎么合并展示。
pub fn gpus() -> Vec<GpuInfo> {
    let mut found: Vec<(u32, GpuInfo)> = drm_cards()
        .into_iter()
        .filter_map(|(index, dir)| probe_card(index, &dir).map(|gpu| (index, gpu)))
        .collect();

    // 先按 card 序号定序，保证同一台机器每次输出顺序一致
    // （read_dir 的顺序是文件系统给的，不保证稳定）。
    found.sort_by_key(|(index, _)| *index);

    let mut list: Vec<GpuInfo> = found.into_iter().map(|(_, gpu)| gpu).collect();
    // 稳定排序：活跃的浮到最前，同组内仍保持 card 序号顺序。
    list.sort_by_key(|gpu| std::cmp::Reverse(gpu.active));
    list
}

/// 枚举 `/sys/class/drm` 下的 `cardN`。
///
/// 必须把连接器（`card0-DP-1`）和渲染节点（`renderD128`）排除掉：
/// 前者是同一张卡的接口，后者不是显示设备，都会让显卡被重复计数。
fn drm_cards() -> Vec<(u32, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(DRM_CLASS_DIR) else {
        return Vec::new();
    };

    let mut cards = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        // 先绑定成有所有权的 String 再切前缀：`entry.file_name()` 是临时值，
        // 直接在其上借用会被立刻释放。
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let Some(digits) = name.strip_prefix("card") else {
            continue;
        };
        // 这一步同时排除了 `card0-DP-1`（剩 `0-DP-1`，含非数字）和 `renderD128`。
        if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let Ok(index) = digits.parse::<u32>() else {
            continue;
        };
        cards.push((index, entry.path()));
    }
    cards
}

/// 读取单张卡的 sysfs 信息，拼出展示名。
fn probe_card(index: u32, card_dir: &Path) -> Option<GpuInfo> {
    let uevent = sys::read_capped(&card_dir.join("device/uevent"), MAX_SYSFS_BYTES)?;
    let driver = uevent_prop(&uevent, "DRIVER");

    // PCI_ID 形如 `1002:1900`。虚拟显卡（如 virtio）可能没有这一项，
    // 此时仍可用驱动名给出一个有意义的兜底。
    let name = match uevent_prop(&uevent, "PCI_ID")
        .as_deref()
        .and_then(parse_pci_id)
    {
        // 子系统 ID 就在同一个 uevent 里（`PCI_SUBSYS_ID`），
        // 不必再去 `/sys/bus/pci/devices/*/subsystem_device` 多开一次文件。
        Some((vendor, device)) => {
            let subsystem = uevent_prop(&uevent, "PCI_SUBSYS_ID")
                .as_deref()
                .and_then(parse_pci_id);
            pci_display_name(vendor, device, subsystem, driver.as_deref())
        }
        None => driver
            .as_deref()
            .and_then(driver_family)
            .map(|family| format!("{family} GPU"))
            .unwrap_or_else(|| "Unknown GPU".to_owned()),
    };

    Some(GpuInfo {
        name,
        driver,
        active: drives_connected_display(index),
    })
}

/// 该卡是否驱动着至少一块已连接的显示器。
///
/// niri 没有「主显卡」的概念，但「哪张卡接着亮着的屏」是最贴近
/// 「当前活跃渲染卡」的可观测信号。
fn drives_connected_display(card_index: u32) -> bool {
    let prefix = format!("card{card_index}-");
    let Ok(entries) = std::fs::read_dir(DRM_CLASS_DIR) else {
        return false;
    };

    entries
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with(&prefix))
        })
        .any(|entry| {
            std::fs::read_to_string(entry.path().join("status"))
                .is_ok_and(|status| status.trim() == "connected")
        })
}

/// 从 `key=value` 形式的 sysfs 属性文件里取一个值。
fn uevent_prop(text: &str, key: &str) -> Option<String> {
    text.lines()
        .filter_map(|line| line.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, value)| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// 解析 `1002:1900` 形式的 PCI ID。
fn parse_pci_id(raw: &str) -> Option<(u16, u16)> {
    let (vendor, device) = raw.split_once(':')?;
    Some((
        u16::from_str_radix(vendor.trim(), 16).ok()?,
        u16::from_str_radix(device.trim(), 16).ok()?,
    ))
}

/// 拼出显卡的展示名，逐级回退。
fn pci_display_name(
    vendor: u16,
    device: u16,
    subsystem: Option<PciKey>,
    driver: Option<&str>,
) -> String {
    if let Some(entry) = lookup_pci_entry(PciQuery {
        vendor,
        device,
        subsystem,
    }) {
        let label = clean_vendor_name(&entry.vendor);
        return format!("{label} {}", resolve_device_name(&entry));
    }

    // `pci.ids` 缺失或查不到：用厂商 ID 表兜底；再不行就靠驱动名认家族。
    let family = vendor_fallback_name(vendor)
        .map(str::to_owned)
        .or_else(|| driver.and_then(driver_family).map(str::to_owned))
        .unwrap_or_else(|| format!("PCI {:04X}", vendor));
    format!("{family} GPU")
}

/// 从 `pci.ids` 的设备条目里挑出最像「商用名」的那个名字。
///
/// 三级取舍，越靠前越可信：
///
/// 1. **方括号**。`pci.ids` 约定把商用名写在 `[]` 里，AMD 的 1146 条设备条目中
///    有 818 条如此（如 `Rembrandt [Radeon 680M]`）—— 这是上游人工校对过的，
///    直接采信。
/// 2. **子系统名**。少数条目会把具体型号写在子系统里（`Radeon RX Vega 11`），
///    但笔记本上子系统名十有八九是**整机型号**（形如 `17aa:3814` 对应 `Z50-75`）。
///    拿它当显卡名是错的，所以要过 [`looks_like_gpu_name`]。
/// 3. **代号别名表**。较新的 APU 在 `pci.ids` 里只有代号（`HawkPoint1`、
///    `Phoenix1`、`Raphael`），查 [`AMD_APU_CODENAMES`] 换成商用名。
///
/// 三级都不命中就原样返回代号 —— 显示 `AMD HawkPoint1` 也比编一个名字强。
fn resolve_device_name(entry: &PciEntry) -> String {
    if let Some(marketing) = bracketed_name(&entry.device) {
        return marketing;
    }
    if let Some(name) = entry
        .subsystem
        .as_deref()
        .filter(|name| looks_like_gpu_name(name))
    {
        return name.to_owned();
    }
    if let Some(alias) = codename_alias(&entry.device) {
        return alias.to_owned();
    }
    entry.device.clone()
}

/// 取出 `RV380/M24 [Mobility Radeon X600]` 里方括号中的部分。
///
/// 只取第一个括号；`[Radeon X600] (Secondary)` 这类后缀标记留在外面，
/// 反正整个代号部分都会被丢弃。
fn bracketed_name(device: &str) -> Option<String> {
    let start = device.find('[')? + 1;
    let end = start + device.get(start..)?.find(']')?;
    let inner = device.get(start..end)?.trim();
    (!inner.is_empty()).then(|| inner.to_owned())
}

/// 判断一个名字是否**像显卡名**，用来挡掉子系统里的整机型号。
fn looks_like_gpu_name(name: &str) -> bool {
    const KEYWORDS: [&str; 9] = [
        "radeon", "geforce", "graphics", "quadro", "mobility", "iris", "uhd", "vega", "gpu",
    ];

    let lower = name.to_ascii_lowercase();
    if KEYWORDS.iter().any(|keyword| lower.contains(keyword)) {
        return true;
    }
    // `Arc` 只有三个字母，子串匹配会误伤 `Search` 之类的词，按整词判定。
    lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|word| word == "arc")
}

/// AMD APU 代号 → 商用名。键必须是小写。
///
/// **这是推断，不是查证。** `pci.ids` 没有收录这些较新 APU 的商用名，只能由代号
/// 反推。同一个代号往往横跨多个 SKU —— 例如 `HawkPoint1` 既出现在 8845HS
/// （Radeon 780M）上，也出现在 8645HS（Radeon 760M）上，而 PCI ID 区分不了它们。
/// 表里取的是该代号下**出货量最大**的那个名字，因此低配 SKU 上可能偏高。
/// 宁可如此，也好过显示一个用户认不出来的内部代号。
///
/// 用「前缀匹配」（见 [`codename_alias`]）是为了让 `HawkPoint2`、`Phoenix1`
/// 这类带数字后缀的写法一并命中，不必逐个罗列。
const AMD_APU_CODENAMES: [(&str, &str); 14] = [
    ("hawkpoint", "Radeon 780M"),
    ("phoenix", "Radeon 780M"),
    ("strix", "Radeon 890M"),
    ("krackan", "Radeon 860M"),
    ("rembrandt", "Radeon 680M"),
    ("mendocino", "Radeon 610M"),
    ("cezanne", "Radeon Vega 8"),
    ("lucienne", "Radeon Vega 8"),
    ("barcelo", "Radeon Vega 8"),
    ("renoir", "Radeon Vega 8"),
    ("picasso", "Radeon Vega 8"),
    ("raven", "Radeon Vega 8"),
    ("raphael", "Radeon Graphics"),
    ("granite ridge", "Radeon Graphics"),
];

/// 查代号别名表。前缀匹配，大小写不敏感。
fn codename_alias(device: &str) -> Option<&'static str> {
    let lower = device.to_ascii_lowercase();
    AMD_APU_CODENAMES
        .iter()
        .find(|(codename, _)| lower.starts_with(codename))
        .map(|(_, marketing)| *marketing)
}

/// 把厂商全称压成常用简称。
///
/// `pci.ids` 里写的是 `Advanced Micro Devices, Inc. [AMD/ATI]` 这种法律全称，
/// 直接展示会占掉半行。
fn clean_vendor_name(raw: &str) -> String {
    const ALIASES: [(&str, &str); 10] = [
        ("Advanced Micro Devices", "AMD"),
        ("Intel", "Intel"),
        ("NVIDIA", "NVIDIA"),
        ("Apple", "Apple"),
        ("Qualcomm", "Qualcomm"),
        ("Broadcom", "Broadcom"),
        ("Red Hat", "virtio"),
        ("VMware", "VMware"),
        ("Raspberry", "Raspberry Pi"),
        ("ARM", "ARM"),
    ];

    for (needle, alias) in ALIASES {
        if raw.contains(needle) {
            return alias.to_owned();
        }
    }

    // 没收录的厂商：砍掉逗号之后的公司后缀与方括号里的别名，只留主干。
    let head = raw.split([',', '[']).next().unwrap_or(raw).trim();
    if head.is_empty() {
        raw.trim().to_owned()
    } else {
        head.to_owned()
    }
}

/// 未收录进 `pci.ids` 时按厂商 ID 兜底。
fn vendor_fallback_name(vendor: u16) -> Option<&'static str> {
    match vendor {
        0x1002 | 0x1022 => Some("AMD"),
        0x1010 => Some("ImgTec"),
        0x106B => Some("Apple"),
        0x10DE => Some("NVIDIA"),
        0x1234 => Some("QEMU"),
        0x13B5 => Some("ARM"),
        0x15AD => Some("VMware"),
        0x1AE0 => Some("Google"),
        0x1AF4 => Some("virtio"),
        0x5143 => Some("Qualcomm"),
        0x8086 => Some("Intel"),
        _ => None,
    }
}

/// 按内核驱动名推断显卡家族。
///
/// 用精确匹配而不是子串匹配：`xe`、`msm` 这类短名字做子串匹配会误伤一大片。
fn driver_family(driver: &str) -> Option<&'static str> {
    match driver.to_ascii_lowercase().as_str() {
        "amdgpu" | "radeon" => Some("AMD"),
        "nvidia" | "nvidia-drm" | "nouveau" => Some("NVIDIA"),
        "i915" | "xe" | "i965" => Some("Intel"),
        "virtio-pci" | "virtio_gpu" | "qxl" | "bochs" | "cirrus" => Some("virtio"),
        "vmwgfx" => Some("VMware"),
        "v3d" | "vc4" => Some("Broadcom"),
        "msm" | "adreno" => Some("Qualcomm"),
        _ => None,
    }
}

// ════════════════════════════════════════════════════════════════════
//  pci.ids 查询
// ════════════════════════════════════════════════════════════════════

/// PCI 标识：`(厂商 ID, 设备 ID)`。
type PciKey = (u16, u16);

/// 一次查询要问 `pci.ids` 的东西。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PciQuery {
    vendor: u16,
    device: u16,
    /// 来自 uevent 的 `PCI_SUBSYS_ID`。子系统条目嵌在设备条目**内部**，
    /// 所以查它必须连同所属设备一起问。
    subsystem: Option<PciKey>,
}

impl PciQuery {
    fn key(self) -> PciKey {
        (self.vendor, self.device)
    }
}

/// `pci.ids` 里一条设备记录的查询结果。
#[derive(Debug, Clone, PartialEq, Eq)]
struct PciEntry {
    vendor: String,
    /// 设备名，形如 `HawkPoint1` 或 `RV380/M24 [Mobility Radeon X600]`。
    device: String,
    /// 该设备条目下、与本机子系统 ID 匹配的那条子系统名。
    subsystem: Option<String>,
}

/// 在 `pci.ids` 中查一条设备记录（含子系统）。
///
/// 文件只读一次并缓存 —— 双显卡机器会对每张卡各查一次，而文件有 1.6MB。
/// 缓存的是**查询结果**而不是文件内容，因此内存占用与显卡数量同阶。
fn lookup_pci_entry(query: PciQuery) -> Option<PciEntry> {
    static CACHE: OnceLock<Vec<(PciKey, PciEntry)>> = OnceLock::new();

    let cache = CACHE.get_or_init(load_pci_ids);
    cache
        .iter()
        .find(|(key, _)| *key == query.key())
        .map(|(_, entry)| entry.clone())
}

/// 读取 `pci.ids` 并按需查询。
///
/// 这里只查「本机实际存在的显卡」，而不是把 1.6MB 全解析成表 ——
/// 后者要分配几万条记录，前者只需读到自己那张卡为止（实测约 4% 的文件）。
fn load_pci_ids() -> Vec<(PciKey, PciEntry)> {
    let queries: Vec<PciQuery> = drm_cards()
        .iter()
        .filter_map(|(_, dir)| {
            let uevent = sys::read_capped(&dir.join("device/uevent"), MAX_SYSFS_BYTES)?;
            let (vendor, device) = uevent_prop(&uevent, "PCI_ID")
                .as_deref()
                .and_then(parse_pci_id)?;
            Some(PciQuery {
                vendor,
                device,
                subsystem: uevent_prop(&uevent, "PCI_SUBSYS_ID")
                    .as_deref()
                    .and_then(parse_pci_id),
            })
        })
        .collect();

    if queries.is_empty() {
        return Vec::new();
    }

    let Some(path) = PCI_IDS_PATHS
        .iter()
        .map(Path::new)
        .find(|path| path.is_file())
    else {
        return Vec::new();
    };
    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };

    lookup_many(
        std::io::BufReader::new(file).take(MAX_PCI_IDS_BYTES),
        &queries,
    )
}

/// 在 `pci.ids` 里批量查询多条设备记录（含各自的子系统）。
///
/// 单趟扫描解决全部查询：GPU 数量是个位数，逐个 ID 各扫一遍文件纯属浪费。
/// `pci.ids` 按厂商 ID 升序排列，扫过最后一个待查厂商即可收工。
///
/// 层级结构决定了扫描方式：厂商行 → 设备行（一个 tab）→ 子系统行（两个 tab）。
/// 子系统行只对它**上面最近的那条设备行**有意义，所以要记住当前停在哪个
/// 已命中的设备里（`open`），遇到下一条设备行就作废。
fn lookup_many<R: BufRead>(reader: R, wanted: &[PciQuery]) -> Vec<(PciKey, PciEntry)> {
    let mut pending: Vec<PciQuery> = wanted.to_vec();
    pending.sort_unstable_by_key(|query| query.key());
    pending.dedup_by_key(|query| query.key());

    let mut results: Vec<(PciKey, PciEntry)> = Vec::new();
    let mut current_vendor: Option<(u16, String)> = None;
    // 正在其内部的、已命中的设备 —— 后续的子系统行归它。
    let mut open: Option<PciKey> = None;

    for line in reader.lines().map_while(Result::ok) {
        // 待查的都有着落、且没有正在等子系统的设备，就可以收工了。
        if pending.is_empty() && open.is_none() {
            break;
        }
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let Some(rest) = line.strip_prefix('\t') else {
            // 厂商行。已过目标厂商就不必再往下扫。
            open = None;
            if let Some((id, name)) = split_id_name(&line) {
                if pending.iter().any(|query| query.vendor == id) {
                    current_vendor = Some((id, name));
                } else if current_vendor.is_some() {
                    break;
                }
            }
            continue;
        };

        let Some((vendor_id, vendor_name)) = &current_vendor else {
            continue;
        };

        if let Some(rest) = rest.strip_prefix('\t') {
            // 子系统行：`ssss dddd  名称`。
            let Some(key) = open else { continue };
            let Some((sub_vendor, sub_device, name)) = split_subsystem(rest) else {
                continue;
            };
            let matches = wanted.iter().any(|query| {
                query.key() == key && query.subsystem == Some((sub_vendor, sub_device))
            });
            if matches && let Some((_, entry)) = results.iter_mut().find(|(k, _)| *k == key) {
                entry.subsystem = Some(name);
                open = None; // 已拿到，不必再为这条设备停留
            }
            continue;
        }

        // 设备行。
        open = None;
        let Some((id, name)) = split_id_name(rest) else {
            continue;
        };
        let Some(slot) = pending
            .iter()
            .position(|query| query.vendor == *vendor_id && query.device == id)
        else {
            continue;
        };

        let query = pending.remove(slot);
        // 只有还需要子系统的设备才值得继续停留。
        if query.subsystem.is_some() {
            open = Some(query.key());
        }
        results.push((
            query.key(),
            PciEntry {
                vendor: vendor_name.clone(),
                device: name,
                subsystem: None,
            },
        ));
    }

    results
}

/// 拆出子系统行的 `(子厂商 ID, 子设备 ID, 名称)`。
///
/// 形如 `17aa 3814  Z50-75`：就是两组「4 位十六进制 + 空白」，
/// 正好可以复用两次 [`split_id_name`]。
fn split_subsystem(s: &str) -> Option<(u16, u16, String)> {
    let (sub_vendor, rest) = split_id_name(s)?;
    let (sub_device, name) = split_id_name(&rest)?;
    Some((sub_vendor, sub_device, name))
}

/// 拆出 `pci.ids` 条目开头的 4 位十六进制 ID 与其余名称。
///
/// 要求 ID 之后必须紧跟空白，否则 `C 03  Display controller` 这类分类行
/// 会被误认成条目（`split_at_checked(4)` 恰好切出 `C 03`）。
fn split_id_name(s: &str) -> Option<(u16, String)> {
    let (id_part, rest) = s.split_at_checked(4)?;
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let id = u16::from_str_radix(id_part, 16).ok()?;
    let name = rest.trim();
    if name.is_empty() {
        return None;
    }
    Some((id, name.to_owned()))
}

// ════════════════════════════════════════════════════════════════════
//  测试
// ════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    // ── CPU 型号清洗 ──────────────────────────────────────────────

    #[test]
    fn cleans_the_documented_examples() {
        // 文档表格里的四行，逐条钉死。
        assert_eq!(
            clean_model("AMD Ryzen 5 7640U w/ Radeon 760M Graphics"),
            "AMD Ryzen 5 7640U"
        );
        assert_eq!(
            clean_model("12th Gen Intel(R) Core(TM) i5-12400F"),
            "Intel Core i5-12400F"
        );
        assert_eq!(
            clean_model("Intel(R) Xeon(R) CPU E5-2670 v3 @ 2.30GHz"),
            "Intel Xeon E5-2670 v3"
        );
        assert_eq!(
            clean_model("AMD Ryzen 5 5600X 6-Core Processor"),
            "AMD Ryzen 5 5600X"
        );
    }

    #[test]
    fn cleans_frequency_and_trademark_together() {
        assert_eq!(
            clean_model("Intel(R) Core(TM) i5-12400F CPU @ 2.50GHz"),
            "Intel Core i5-12400F"
        );
    }

    #[test]
    fn keeps_models_that_need_no_cleaning() {
        // 已经干净的型号必须原样保留，不能被"清洗"坏了。
        for model in ["Apple M1", "AMD Ryzen 7 5800X3D", "Intel Core i9-13900K"] {
            assert_eq!(clean_model(model), model);
        }
    }

    #[test]
    fn removes_extra_cores_suffix_but_keeps_model_digits() {
        // `8-Core` 要去掉，但型号里的 `5800X3D` / `i7-12700K` 必须留下。
        assert_eq!(
            clean_model("AMD Ryzen 7 5800X3D 8-Core Processor"),
            "AMD Ryzen 7 5800X3D"
        );
        assert_eq!(
            clean_model("12th Gen Intel(R) Core(TM) i7-12700K"),
            "Intel Core i7-12700K"
        );
    }

    #[test]
    fn strips_with_style_graphics_suffix() {
        assert_eq!(
            clean_model("AMD Ryzen 5 7530U with Radeon Graphics"),
            "AMD Ryzen 5 7530U"
        );
    }

    #[test]
    fn never_returns_empty_for_a_degenerate_model() {
        // 型号字面量就叫 "CPU" 时，宁可原样返回也不能显示空白。
        assert_eq!(clean_model("CPU"), "CPU");
        assert_eq!(clean_model("  "), "");
    }

    #[test]
    fn generation_prefix_requires_a_full_match() {
        // "12th Generation" 不是 "12th Gen "，不该被当成代际前缀切掉。
        assert_eq!(clean_model("12th Generation Foo"), "12th Generation Foo");
        // 但真正的代际前缀要认出来。
        assert_eq!(
            clean_model("8th Gen Intel Core i7-8700"),
            "Intel Core i7-8700"
        );
    }

    #[test]
    fn noise_token_matching_is_precise() {
        assert!(is_noise_token("CPU"));
        assert!(is_noise_token("cpu"));
        assert!(is_noise_token("Processor"));
        assert!(is_noise_token("6-Core"));
        assert!(is_noise_token("16-core"));
        // 型号词不能被误杀。
        assert!(!is_noise_token("i5-12400F"));
        assert!(!is_noise_token("5800X3D"));
        assert!(!is_noise_token("-Core"));
        assert!(!is_noise_token("Core"));
        assert!(!is_noise_token("X-Core"));
    }

    // ── cpuinfo 解析 ──────────────────────────────────────────────

    /// 与真实机器同构的样例：8 物理核 / 16 逻辑核、单插槽。
    const CPUINFO_SAMPLE: &str = "\
processor\t: 0
vendor_id\t: AuthenticAMD
cpu family\t: 25
model\t\t: 117
model name\t: AMD Ryzen 5 7640U w/ Radeon 760M Graphics
physical id\t: 0
siblings\t: 16
cpu cores\t: 8

processor\t: 1
model name\t: AMD Ryzen 5 7640U w/ Radeon 760M Graphics
physical id\t: 0
cpu cores\t: 8
";

    #[test]
    fn parses_model_and_core_counts() {
        let info = parse_cpuinfo(CPUINFO_SAMPLE).expect("样例应当能解析");
        assert_eq!(info.model, "AMD Ryzen 5 7640U");
        // processor 条目数 = 逻辑核数
        assert_eq!(info.logical, 2);
        // cpu cores(8) × 去重后的 physical id 数(1) = 8
        assert_eq!(info.physical, Some(8));
    }

    #[test]
    fn physical_cores_scale_with_socket_count() {
        // 双路机器：cpu cores 每条都是 8，physical id 有 2 个不同值。
        let text = "\
processor\t: 0
model name\t: Intel Xeon Gold 6248
physical id\t: 0
cpu cores\t: 8

processor\t: 1
model name\t: Intel Xeon Gold 6248
physical id\t: 1
cpu cores\t: 8
";
        let info = parse_cpuinfo(text).expect("样例应当能解析");
        assert_eq!(info.physical, Some(16));
        assert_eq!(info.logical, 2);
    }

    #[test]
    fn missing_cpu_cores_leaves_physical_unknown() {
        // ARM 平台的 cpuinfo 没有 cpu cores 字段，此时物理核心数应当老实报 None。
        let text = "processor\t: 0\nmodel name\t: ARMv8 Processor rev 1\n";
        let info = parse_cpuinfo(text).expect("样例应当能解析");
        assert_eq!(info.physical, None);
        assert_eq!(info.logical, 1);
    }

    #[test]
    fn rejects_input_without_a_model() {
        assert!(parse_cpuinfo("processor\t: 0\n").is_none());
        assert!(parse_cpuinfo("").is_none());
    }

    // ── sysfs / PCI ───────────────────────────────────────────────

    #[test]
    fn reads_sysfs_properties() {
        let uevent = "DRIVER=amdgpu\nPCI_CLASS=30000\nPCI_ID=1002:1900\n";
        assert_eq!(uevent_prop(uevent, "DRIVER").as_deref(), Some("amdgpu"));
        assert_eq!(uevent_prop(uevent, "PCI_ID").as_deref(), Some("1002:1900"));
        assert_eq!(uevent_prop(uevent, "MISSING"), None);
    }

    #[test]
    fn parses_pci_ids() {
        assert_eq!(parse_pci_id("1002:1900"), Some((0x1002, 0x1900)));
        assert_eq!(parse_pci_id("10DE:2504"), Some((0x10DE, 0x2504)));
        // 畸形输入不能 panic。
        assert_eq!(parse_pci_id("1002"), None);
        assert_eq!(parse_pci_id("1002:ZZZZ"), None);
        assert_eq!(parse_pci_id(""), None);
    }

    /// 与真实 `pci.ids` 同构的片段，含注释、分类行与子系统条目等干扰项。
    ///
    /// 三条 AMD 设备分别对应三种取名路径，顺序刻意打乱以验证它们互不串味：
    /// `1114` 自带方括号、`1900` 只有代号且子系统是整机型号、`1901` 只有代号
    /// 但子系统是真显卡名。
    const PCI_IDS_SAMPLE: &str = "\
#\tList of PCI ID's
#
#\tMaintained by Martin Mares
#
C 00  Unclassified device
\t00  Non-VGA unclassified device
1000  Broadcom / LSI
\t0010  MegaRAID
1002  Advanced Micro Devices, Inc. [AMD/ATI]
\t1114  Krackan [Radeon 840M / 860M Graphics]
\t1304  Kaveri
\t1900  HawkPoint1
\t\t17aa 3814  Z50-75
\t1901  Raphael
\t\t1458 d000  Radeon RX Vega 11
\t1902  Totally Unknown Codename
10DE  NVIDIA Corporation
\t2504  GA106 [GeForce RTX 3050]
";

    /// 不关心子系统时的简写。
    fn probe(vendor: u16, device: u16) -> Option<PciEntry> {
        lookup_sample(PciQuery {
            vendor,
            device,
            subsystem: None,
        })
    }

    fn lookup_sample(query: PciQuery) -> Option<PciEntry> {
        lookup_many(PCI_IDS_SAMPLE.as_bytes(), &[query])
            .into_iter()
            .next()
            .map(|(_, entry)| entry)
    }

    #[test]
    fn finds_a_device_in_the_right_vendor_section() {
        let entry = probe(0x1002, 0x1900).expect("应当查到 1002:1900");
        assert_eq!(entry.vendor, "Advanced Micro Devices, Inc. [AMD/ATI]");
        assert_eq!(entry.device, "HawkPoint1");
    }

    #[test]
    fn device_ids_are_scoped_to_their_vendor() {
        // 1002 段里有 1900，10DE 段里没有 —— 不能跨厂商串味。
        assert!(probe(0x10DE, 0x1900).is_none());
        assert_eq!(
            probe(0x10DE, 0x2504).map(|e| e.device).as_deref(),
            Some("GA106 [GeForce RTX 3050]")
        );
    }

    #[test]
    fn ignores_class_lines_and_subsystem_entries() {
        // `C 00  Unclassified device` 的分类行不能被当成厂商条目。
        // 子系统条目（两个 tab）也不能冒充设备。
        let entry = probe(0x1002, 0x1114).expect("应当查到 1002:1114");
        assert_eq!(entry.device, "Krackan [Radeon 840M / 860M Graphics]");
        // 子系统 ID `17aa` 不该被当作可查询的设备。
        assert!(probe(0x1002, 0x17AA).is_none());
        // 子系统里那个设备号 `3814` 同理。
        assert!(probe(0x1002, 0x3814).is_none());
    }

    #[test]
    fn lookup_handles_an_empty_wishlist() {
        assert!(lookup_many(PCI_IDS_SAMPLE.as_bytes(), &[]).is_empty());
    }

    // ── 子系统条目 ────────────────────────────────────────────────

    #[test]
    fn subsystem_is_attached_to_the_device_it_belongs_to() {
        let query = PciQuery {
            vendor: 0x1002,
            device: 0x1900,
            subsystem: Some((0x17AA, 0x3814)),
        };
        let entry = lookup_sample(query).expect("应当查到 1002:1900");
        assert_eq!(entry.subsystem.as_deref(), Some("Z50-75"));
    }

    #[test]
    fn subsystem_does_not_leak_to_a_sibling_device() {
        // `1901` 的子系统是 `1458:d000`，不能捡到 `1900` 名下的 `17aa:3814`。
        let query = PciQuery {
            vendor: 0x1002,
            device: 0x1901,
            subsystem: Some((0x17AA, 0x3814)),
        };
        let entry = lookup_sample(query).expect("应当查到 1002:1901");
        assert_eq!(entry.subsystem, None);
    }

    #[test]
    fn subsystem_lookup_needs_the_matching_id() {
        // 设备对但子系统对不上时不该带回任何子系统名。
        let query = PciQuery {
            vendor: 0x1002,
            device: 0x1901,
            subsystem: Some((0xFFFF, 0xFFFF)),
        };
        assert_eq!(lookup_sample(query).expect("设备本身仍在").subsystem, None);
    }

    #[test]
    fn devices_without_a_subsystem_query_still_resolve() {
        // 虚拟显卡没有 PCI_SUBSYS_ID，不该因此查不到设备名。
        let entry = probe(0x1002, 0x1901).expect("应当查到 1002:1901");
        assert_eq!(entry.device, "Raphael");
        assert_eq!(entry.subsystem, None);
    }

    #[test]
    fn split_subsystem_parses_the_two_id_form() {
        assert_eq!(
            split_subsystem("17aa 3814  Z50-75"),
            Some((0x17AA, 0x3814, "Z50-75".to_owned()))
        );
        // 名称里可以有空格。
        assert_eq!(
            split_subsystem("1043 876b  PRIME B450M-A Motherboard"),
            Some((0x1043, 0x876B, "PRIME B450M-A Motherboard".to_owned()))
        );
        // 只有一个 ID、或 ID 后没有名称，都不算数。
        assert_eq!(split_subsystem("17aa"), None);
        assert_eq!(split_subsystem("17aa 3814"), None);
        assert_eq!(split_subsystem(""), None);
    }

    // ── 商用名解析 ────────────────────────────────────────────────

    #[test]
    fn bracket_wins_over_everything_else() {
        // `pci.ids` 人工校对过的方括号名最可信。
        let entry = probe(0x1002, 0x1114).expect("应当查到 1002:1114");
        assert_eq!(resolve_device_name(&entry), "Radeon 840M / 860M Graphics");
    }

    #[test]
    fn subsystem_wins_over_the_codename_alias() {
        // `Raphael` 在别名表里，但子系统给出了更具体的真实型号。
        let query = PciQuery {
            vendor: 0x1002,
            device: 0x1901,
            subsystem: Some((0x1458, 0xD000)),
        };
        let entry = lookup_sample(query).expect("应当查到 1002:1901");
        assert_eq!(resolve_device_name(&entry), "Radeon RX Vega 11");
    }

    #[test]
    fn a_board_name_in_the_subsystem_is_rejected() {
        // 关键防线：`17aa:3814` 这个子系统指向的是**整机型号**而非显卡型号。
        // 若不做这层过滤就会显示成 `AMD Z50-75 (amdgpu)`。
        let query = PciQuery {
            vendor: 0x1002,
            device: 0x1900,
            subsystem: Some((0x17AA, 0x3814)),
        };
        let entry = lookup_sample(query).expect("应当查到 1002:1900");
        assert_eq!(entry.subsystem.as_deref(), Some("Z50-75"));
        assert_eq!(resolve_device_name(&entry), "Radeon 780M");
    }

    #[test]
    fn unknown_codenames_are_passed_through_unchanged() {
        // 三级都不命中时宁可显示代号，也不要编一个名字。
        let entry = probe(0x1002, 0x1902).expect("应当查到 1002:1902");
        assert_eq!(resolve_device_name(&entry), "Totally Unknown Codename");
    }

    #[test]
    fn codename_alias_matches_with_a_numeric_suffix() {
        // `pci.ids` 里的代号带数字后缀（HawkPoint1 / Phoenix2），
        // 前缀匹配让它们一并命中，不必逐个罗列。
        assert_eq!(codename_alias("HawkPoint1"), Some("Radeon 780M"));
        assert_eq!(codename_alias("HawkPoint2"), Some("Radeon 780M"));
        assert_eq!(codename_alias("Phoenix1"), Some("Radeon 780M"));
        assert_eq!(codename_alias("Rembrandt"), Some("Radeon 680M"));
        assert_eq!(codename_alias("Granite Ridge"), Some("Radeon Graphics"));
        // 大小写不敏感。
        assert_eq!(codename_alias("hawkpoint1"), Some("Radeon 780M"));
        assert_eq!(codename_alias("GA106"), None);
    }

    #[test]
    fn gpu_name_detection_rejects_board_names() {
        for name in [
            "Radeon RX Vega 11",
            "Radeon Vega 8 Mobile",
            "GeForce GTX 1050 Ti",
            "Intel Iris Xe Graphics",
            "UHD Graphics 630",
            "Arc A770",
        ] {
            assert!(looks_like_gpu_name(name), "{name} 应被认作显卡名");
        }
        // 整机型号是最常见的干扰项。
        for name in [
            "Z50-75",
            "IdeaPad 1 15AMN7",
            "ThinkPad E595",
            "MS-7C28 Motherboard",
            "Search", // `arc` 的子串误伤，靠整词匹配挡掉
        ] {
            assert!(!looks_like_gpu_name(name), "{name} 不该被认作显卡名");
        }
    }

    #[test]
    fn bracketed_name_extracts_only_the_first_bracket() {
        assert_eq!(
            bracketed_name("RV380/M24 [Mobility Radeon X600]").as_deref(),
            Some("Mobility Radeon X600")
        );
        assert_eq!(
            bracketed_name("HawkPoint1").as_deref(),
            None,
            "没有方括号时应返回 None，交由别名表处理"
        );
        // 畸形输入不能 panic。
        assert_eq!(bracketed_name("[unclosed"), None);
        assert_eq!(bracketed_name("[]"), None);
    }

    #[test]
    fn split_id_name_rejects_short_and_class_lines() {
        assert_eq!(
            split_id_name("1900  HawkPoint1"),
            Some((0x1900, "HawkPoint1".to_owned()))
        );
        assert_eq!(split_id_name("abc"), None);
        assert_eq!(split_id_name("C 00  Unclassified device"), None);
        // ID 后面必须有空白分隔。
        assert_eq!(split_id_name("1900HawkPoint1"), None);
    }

    // ── 厂商名与驱动 ──────────────────────────────────────────────

    #[test]
    fn compresses_legal_vendor_names() {
        assert_eq!(
            clean_vendor_name("Advanced Micro Devices, Inc. [AMD/ATI]"),
            "AMD"
        );
        assert_eq!(clean_vendor_name("NVIDIA Corporation"), "NVIDIA");
        assert_eq!(clean_vendor_name("Intel Corporation"), "Intel");
        // 未收录的厂商退化为「逗号/方括号前的主干」。
        assert_eq!(clean_vendor_name("Some Corp, Inc. [Alias]"), "Some Corp");
        assert_eq!(clean_vendor_name(""), "");
    }

    #[test]
    fn driver_family_uses_exact_matching() {
        assert_eq!(driver_family("amdgpu"), Some("AMD"));
        assert_eq!(driver_family("i915"), Some("Intel"));
        assert_eq!(driver_family("xe"), Some("Intel"));
        // 精确匹配的意义：`xe` 不能把 `xen` 之类的名字匹配进去。
        assert_eq!(driver_family("xen"), None);
        assert_eq!(driver_family("vortex"), None);
        assert_eq!(driver_family(""), None);
    }

    #[test]
    fn builds_a_display_name_with_fallbacks() {
        // 有 pci.ids 时用厂商简称 + 设备名。
        assert_eq!(
            clean_vendor_name("Advanced Micro Devices, Inc. [AMD/ATI]"),
            "AMD"
        );
        // 没有 pci.ids 时退回厂商 ID 表。
        assert_eq!(vendor_fallback_name(0x1002), Some("AMD"));
        assert_eq!(vendor_fallback_name(0x8086), Some("Intel"));
        assert_eq!(vendor_fallback_name(0xFFFF), None);
    }
}
