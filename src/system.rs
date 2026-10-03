//! 通用系统信息探测层（内核 / 运行时长 / 负载 / Shell / 内存 / 磁盘 / 包 / 电池）。
//!
//! 与 [`crate::hardware`]（CPU / GPU）分开：那边是「这台机器的硬件拓扑」，
//! 这边是「操作系统与运行状态」。绝大多数数据同样来自 procfs / sysfs，
//! 只有磁盘用量必须借 `statvfs` —— 而 std 没有该封装，故调用 `df` 一次
//! （带超时），与项目「不为一两个字段引入 libc」的取舍一致。
//!
//! 容错原则与其它探测模块相同：任何一步失败都折叠成 `None`，绝不 panic，
//! 也绝不上报猜测值。

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use crate::sys;

/// 通用小文件的读取上限（`/proc` 里的这些文件都只有几百字节）。
const MAX_SMALL_BYTES: u64 = 64 * 1024;

/// `/var/lib/dpkg/status` 可能有好几 MB，给它一个宽松但仍有限的上限。
const MAX_DPKG_BYTES: u64 = 64 * 1024 * 1024;

/// `df` 的超时。它读的是本地文件系统，正常在毫秒级返回。
const DF_TIMEOUT: Duration = Duration::from_millis(1500);

// ════════════════════════════════════════════════════════════════════
//  内核 / 运行时长 / 负载
// ════════════════════════════════════════════════════════════════════

/// 内核标识，形如 `Linux 7.2.8-arch1-2`。
pub fn kernel() -> Option<String> {
    let release = read_trimmed("/proc/sys/kernel/osrelease")?;
    let os = read_trimmed("/proc/sys/kernel/ostype").unwrap_or_default();
    if os.is_empty() {
        Some(release)
    } else {
        Some(format!("{os} {release}"))
    }
}

/// 系统运行时长（秒），取自 `/proc/uptime` 的第一个字段。
pub fn uptime_seconds() -> Option<u64> {
    let text = sys::read_capped(Path::new("/proc/uptime"), MAX_SMALL_BYTES)?;
    let seconds: f64 = text.split_whitespace().next()?.parse().ok()?;
    (seconds.is_finite() && seconds >= 0.0).then_some(seconds as u64)
}

/// 1 / 5 / 15 分钟平均负载，取自 `/proc/loadavg`。
pub fn load() -> Option<[f32; 3]> {
    let text = sys::read_capped(Path::new("/proc/loadavg"), MAX_SMALL_BYTES)?;
    let mut fields = text.split_whitespace();
    let one: f32 = fields.next()?.parse().ok()?;
    let five: f32 = fields.next()?.parse().ok()?;
    let fifteen: f32 = fields.next()?.parse().ok()?;
    Some([one, five, fifteen])
}

// ════════════════════════════════════════════════════════════════════
//  Shell
// ════════════════════════════════════════════════════════════════════

/// 当前 Shell 的名字，取自 `$SHELL` 的文件名部分（`/bin/fish` → `fish`）。
///
/// 用 `$SHELL`（登录 Shell）而不是沿父进程链找：调用 nirifetch 的往往是
/// 脚本或 fetch 的打印时机，父进程链未必停在用户交互用的那个 Shell 上。
pub fn shell() -> Option<String> {
    let path = sys::env_non_empty("SHELL")?;
    let name = Path::new(&path)
        .file_name()
        .and_then(|n| n.to_str())?
        .trim();
    (!name.is_empty()).then(|| name.to_owned())
}

// ════════════════════════════════════════════════════════════════════
//  内存
// ════════════════════════════════════════════════════════════════════

/// 物理内存总量与可用量（单位 KiB）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Memory {
    pub total_kib: u64,
    pub available_kib: u64,
}

/// 从 `/proc/meminfo` 读取内存信息。
///
/// `MemAvailable` 是较新内核对「不触发交换就能给新进程用」的估算，比 `MemFree`
/// 更贴近用户直觉；老内核没有它时回退到 `MemFree`（会是 0 则说明都读不到）。
pub fn memory() -> Option<Memory> {
    let text = sys::read_capped(Path::new("/proc/meminfo"), MAX_SMALL_BYTES)?;

    let mut total = None;
    let mut available = None;
    let mut free = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            total = parse_leading_u64(rest);
        } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
            available = parse_leading_u64(rest);
        } else if let Some(rest) = line.strip_prefix("MemFree:") {
            free = parse_leading_u64(rest);
        }
        if total.is_some() && (available.is_some() || free.is_some()) {
            break;
        }
    }

    Some(Memory {
        total_kib: total?,
        available_kib: available.or(free).unwrap_or(0),
    })
}

/// 取一行里第一个十进制数（`   20267444 kB` → `20267444`）。
fn parse_leading_u64(rest: &str) -> Option<u64> {
    rest.split_whitespace().next()?.parse().ok()
}

// ════════════════════════════════════════════════════════════════════
//  磁盘
// ════════════════════════════════════════════════════════════════════

/// 一个挂载点的磁盘用量（单位 KiB）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Disk {
    pub mount: String,
    pub used_kib: u64,
    pub total_kib: u64,
}

/// 查询挂载点的磁盘用量。
///
/// std 没有 `statvfs` 封装，而为一个字段引入 `libc` 与项目「零重依赖」的
/// 定位冲突，因此调用一次 `df -Pk`（POSIX 输出格式，单位 1024 字节块）。
pub fn disk(mount: &str) -> Option<Disk> {
    let mut cmd = Command::new("df");
    cmd.arg("-Pk").arg("--").arg(mount);
    let out = sys::run(&mut cmd, DF_TIMEOUT)?;
    if !out.status.success() {
        return None;
    }
    parse_df(&String::from_utf8_lossy(&out.stdout), mount)
}

/// 解析 `df -Pk` 的输出。
///
/// 表头一行，数据一行；多挂载同名设备时可能有额外行，只取第一行数据。
/// 列序固定：`Filesystem 1024-blocks Used Available Capacity Mounted`。
fn parse_df(text: &str, mount: &str) -> Option<Disk> {
    let line = text.lines().skip(1).find(|l| !l.trim().is_empty())?;
    let cols: Vec<&str> = line.split_whitespace().collect();
    if cols.len() < 6 {
        return None;
    }
    let total_kib: u64 = cols[1].parse().ok()?;
    let used_kib: u64 = cols[2].parse().ok()?;
    Some(Disk {
        mount: mount.to_owned(),
        used_kib,
        total_kib,
    })
}

// ════════════════════════════════════════════════════════════════════
//  包管理器
// ════════════════════════════════════════════════════════════════════

/// 已安装包数量与来源包管理器。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packages {
    pub count: usize,
    pub manager: &'static str,
}

/// 统计已安装的软件包。按常见发行版依次探测，命中即返回。
pub fn packages() -> Option<Packages> {
    if let Some(count) = pacman_count() {
        return Some(Packages {
            count,
            manager: "pacman",
        });
    }
    if let Some(count) = dpkg_count() {
        return Some(Packages {
            count,
            manager: "dpkg",
        });
    }
    None
}

/// pacman：`/var/lib/pacman/local` 下每个包一个目录。
fn pacman_count() -> Option<usize> {
    let entries = std::fs::read_dir("/var/lib/pacman/local").ok()?;
    let count = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_dir()))
        .count();
    (count > 0).then_some(count)
}

/// dpkg：`/var/lib/dpkg/status` 里每个包一个 `Package:` 条目。
fn dpkg_count() -> Option<usize> {
    let text = sys::read_capped(Path::new("/var/lib/dpkg/status"), MAX_DPKG_BYTES)?;
    let count = text
        .lines()
        .filter(|line| line.starts_with("Package:"))
        .count();
    (count > 0).then_some(count)
}

// ════════════════════════════════════════════════════════════════════
//  电池
// ════════════════════════════════════════════════════════════════════

/// 电池状态。台式机没有电池时探测器返回 `None`，UI 直接隐藏该行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Battery {
    /// 电量百分比（0–100）。
    pub percent: u8,
    /// 内核报告的状态，例如 `Charging` / `Discharging` / `Full`。
    pub status: Option<String>,
}

/// 读取第一块电池（`/sys/class/power_supply/BAT*`）。
pub fn battery() -> Option<Battery> {
    let entries = std::fs::read_dir("/sys/class/power_supply").ok()?;
    for entry in entries.filter_map(Result::ok) {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if !name.starts_with("BAT") {
            continue;
        }
        let base = entry.path();
        let Some(percent) = read_trimmed_path(&base.join("capacity")).and_then(|v| v.parse().ok())
        else {
            continue;
        };
        let status = read_trimmed_path(&base.join("status")).filter(|s| !s.is_empty());
        return Some(Battery { percent, status });
    }
    None
}

// ════════════════════════════════════════════════════════════════════
//  内部工具
// ════════════════════════════════════════════════════════════════════

/// 读取一个文件并去掉首尾空白。
fn read_trimmed(path: &str) -> Option<String> {
    read_trimmed_path(Path::new(path))
}

/// [`read_trimmed`] 的 `Path` 版本。
fn read_trimmed_path(path: &Path) -> Option<String> {
    let text = sys::read_capped(path, MAX_SMALL_BYTES)?;
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

// ════════════════════════════════════════════════════════════════════
//  测试
// ════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_leading_numbers_from_meminfo_lines() {
        assert_eq!(parse_leading_u64("   20267444 kB"), Some(20267444));
        assert_eq!(parse_leading_u64(" 0"), Some(0));
        assert_eq!(parse_leading_u64("kB"), None);
        assert_eq!(parse_leading_u64(""), None);
    }

    #[test]
    fn parses_df_output() {
        let sample = "\
Filesystem     1024-blocks     Used Available Capacity Mounted on
/dev/sda2        243148800 39717116 201948180      17% /
";
        let disk = parse_df(sample, "/").expect("样例应当能解析");
        assert_eq!(disk.total_kib, 243148800);
        assert_eq!(disk.used_kib, 39717116);
        assert_eq!(disk.mount, "/");
    }

    #[test]
    fn rejects_short_df_output() {
        // 表头之外没有数据行、或列数不足时不能 panic，也不能编造数据。
        assert!(
            parse_df(
                "Filesystem 1024-blocks Used Available Capacity Mounted\n",
                "/"
            )
            .is_none()
        );
        assert!(parse_df("", "/").is_none());
        assert!(parse_df("head\n/dev/sda2 1 2\n", "/").is_none());
    }

    #[test]
    fn real_probes_do_not_panic() {
        // 这些字段在测试环境里未必都有值（容器、无电池的机器……），
        // 所以只断言「能跑通、不 panic」，不锁定具体数值。
        let _ = kernel();
        let _ = uptime_seconds();
        let _ = load();
        let _ = shell();
        let _ = memory();
        let _ = packages();
        let _ = battery();
    }

    #[test]
    fn memory_sample_is_parsed() {
        // 直接验证解析逻辑，避免依赖跑测试的机器的真实内存。
        let text = "MemTotal:       20267444 kB\nMemFree:        15644408 kB\nMemAvailable:   17768624 kB\n";
        let mut total = None;
        let mut available = None;
        let mut free = None;
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("MemTotal:") {
                total = parse_leading_u64(rest);
            } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
                available = parse_leading_u64(rest);
            } else if let Some(rest) = line.strip_prefix("MemFree:") {
                free = parse_leading_u64(rest);
            }
        }
        let mem = Memory {
            total_kib: total.expect("有 MemTotal"),
            available_kib: available.or(free).unwrap_or(0),
        };
        assert_eq!(mem.total_kib, 20267444);
        assert_eq!(mem.available_kib, 17768624);
    }
}
