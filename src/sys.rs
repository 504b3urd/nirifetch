//! 与本机系统交互的底层原语。
//!
//! [`crate::ipc`]、[`crate::hardware`]、[`crate::font`] 三个模块都要做同样几件事：
//! 执行外部命令、读取虚拟文件系统里的文本、读环境变量。把这层单独拎出来，既避免
//! 同一段逻辑抄三遍，也让那三个模块各自专注于自己的领域（niri 协议 / 硬件拓扑 /
//! 字体配置）。
//!
//! 两条贯穿全模块的约定：
//!
//! 1. **绝不挂死**：所有子进程调用都带超时。混成器无响应、D-Bus 不可达时，
//!    被调用的命令会一直阻塞，不加超时会把 nirifetch 一起拖住。
//! 2. **绝不 panic**：失败一律折叠成 `Option::None`，由调用方决定怎么兜底展示。

use std::io::Read;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// 等待子进程退出时的轮询间隔。
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// 执行命令并捕获输出，超过 `timeout` 就杀掉子进程并返回 `None`。
///
/// 这里刻意用「轮询 `try_wait` + 截止时间」而不是依赖 `wait_timeout` 之类的 crate：
/// nirifetch 要调用的命令输出都只有几 KB，远小于 64KiB 的管道缓冲区，不会出现
/// 「子进程写满管道阻塞、父进程却在等它退出」的死锁，因此不需要额外的依赖。
pub fn run(cmd: &mut Command, timeout: Duration) -> Option<Output> {
    let mut child = cmd
        // stdin 必须置空：否则被调用的命令会继承终端，存在抢读用户输入的风险。
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;

    let deadline = Instant::now() + timeout;

    loop {
        match child.try_wait() {
            // 已退出：进程状态被缓存，此时再读管道不会丢失数据。
            Ok(Some(_)) => return child.wait_with_output().ok(),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait(); // 回收僵尸进程
                    return None;
                }
                std::thread::sleep(POLL_INTERVAL);
            }
            Err(_) => return None,
        }
    }
}

/// 读取环境变量，把「未设置」和「空字符串」统一折叠成 `None`。
///
/// 空字符串在实际环境里几乎总是「等于没设置」（`FOO= nirifetch`），
/// 若不平滑掉，调用方就得在每处 `var()` 后面各写一遍 `.filter(|v| !v.is_empty())`。
pub fn env_non_empty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

/// 遍历 `/proc` 下的全部进程，产出每个进程的名字。
///
/// 名字优先取 `/proc/<pid>/exe` 指向的可执行文件名（readlink 很便宜，且不受
/// 长度限制），内核线程没有 `exe`，再退回 `/proc/<pid>/comm`。两者的取舍见
/// [`process_name`]。
///
/// 迭代是**惰性**的：调用方一旦找到目标就可以停下，不必付完整的扫描开销。
/// 目录里读不出数字目录（例如 `/proc` 没挂载）时返回空迭代器，绝不报错。
pub fn process_names() -> impl Iterator<Item = String> {
    let pids = std::fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            entry
                .file_name()
                .to_str()
                .and_then(|n| n.parse::<u32>().ok())
        })
        // 防御性上限：正常机器上进程数是几百，这里留出足够余量，
        // 同时挡住「/proc 被换成含海量条目的目录」这类病态情况。
        .take(MAX_SCANNED_PIDS);

    pids.filter_map(process_name)
}

/// 扫描 `/proc` 时最多查看的进程数。
const MAX_SCANNED_PIDS: usize = 4096;

/// 取单个进程的名字。
///
/// 两条路径各有取舍：
/// - `/proc/<pid>/exe` 是符号链接，`readlink` 不需要打开并读取实际文件，
///   实测比读 `comm` 快数倍，且拿到的是**完整**文件名；内核线程没有它。
/// - `/proc/<pid>/comm` 一定存在，但内核会把内容**截断到 15 字节**
///   （`dank-material-shell` 会变成 `dank-material-sh`），所以只能兜底。
///
/// [`crate::font`] 用它沿父进程链找终端，[`crate::bar`] 用它遍历全部进程，
/// 两处对「进程名」的定义必须一致，故放在这里共用。
pub fn process_name(pid: u32) -> Option<String> {
    if let Ok(exe) = std::fs::read_link(format!("/proc/{pid}/exe"))
        && let Some(name) = exe.file_name().and_then(|n| n.to_str())
        && !name.is_empty()
    {
        return Some(name.to_owned());
    }

    let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    let comm = comm.trim();
    if comm.is_empty() {
        None
    } else {
        Some(comm.to_owned())
    }
}

/// 读取文本文件，最多读 `max_bytes` 字节。
///
/// 设上限是为了防御「路径被软链到巨型文件或设备节点」这类意外 ——
/// `pci.ids` 有 1.6MB，而 `/dev/zero` 是无限的。
///
/// 用 `from_utf8_lossy` 而非 `read_to_string`：文件里混入非法 UTF-8 字节时
/// 不该整个读取失败，替换成 U+FFFD 继续解析即可。
pub fn read_capped(path: &Path, max_bytes: u64) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(max_bytes).read_to_end(&mut bytes).ok()?;
    // 空文件返回 `Some("")` 而不是 `None`：文件**存在**但为空，与读不到是两回事，
    // 调用方（例如配置检测）需要区分这两种情况。
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_non_empty_folds_missing_and_blank() {
        // 这两个变量在测试环境里几乎不可能被设置成有意义的值。
        assert_eq!(env_non_empty("NIRIFETCH_TEST_UNSET_VARIABLE"), None);
        // PATH 一定存在且非空。
        assert!(env_non_empty("PATH").is_some());
    }

    #[test]
    fn read_capped_returns_none_for_missing_file() {
        assert!(read_capped(Path::new("/nonexistent/nope"), 1024).is_none());
    }

    #[test]
    fn read_capped_returns_none_for_directory() {
        // 目录不是普通文件，open 会成功但 read 会失败 —— 不能 panic。
        assert!(read_capped(Path::new("/"), 1024).is_none());
    }

    #[test]
    fn read_capped_truncates_at_the_limit() {
        let path = std::env::temp_dir().join(format!("nirifetch-sys-{}", std::process::id()));
        std::fs::write(&path, "0123456789").expect("写入临时文件");

        assert_eq!(read_capped(&path, 4).as_deref(), Some("0123"));
        // 上限大于文件长度时读出全部内容。
        assert_eq!(read_capped(&path, 999).as_deref(), Some("0123456789"));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn read_capped_survives_invalid_utf8() {
        let path = std::env::temp_dir().join(format!("nirifetch-sys-bin-{}", std::process::id()));
        std::fs::write(&path, [0x41, 0xFF, 0xFE, 0x42]).expect("写入临时文件");

        let text = read_capped(&path, 64).expect("非法 UTF-8 不应导致读取失败");
        assert!(
            text.starts_with('A') && text.ends_with('B'),
            "实际得到 {text:?}"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn process_name_reads_this_very_process() {
        // 当前进程一定有名字；测试二进制跑在 target/debug/deps 下，
        // 文件名带 `nirifetch`，用它顺便验证走的是 exe 而不是被截断的 comm。
        let name = process_name(std::process::id()).expect("当前进程应当有名字");
        assert!(
            name.contains("nirifetch"),
            "期望拿到测试二进制的文件名，实际得到 {name:?}"
        );
    }

    #[test]
    fn process_name_folds_missing_pids() {
        // 不存在的 pid 必须安静地返回 None —— 进程随时可能退出，
        // 遍历 /proc 时读到刚消失的 pid 是常态。
        assert_eq!(process_name(u32::MAX), None);
    }

    #[test]
    fn process_names_includes_the_current_process() {
        // 全量扫描至少要能看到自己，否则说明 /proc 读取路径坏了。
        let me = process_name(std::process::id()).expect("当前进程应当有名字");
        assert!(
            process_names().any(|name| name == me),
            "全量扫描里没找到当前进程 {me:?}"
        );
    }

    #[test]
    fn run_times_out_on_a_hanging_command() {
        // 关键防挂死路径：命令永不退出时必须返回 None 而不是无限等待。
        let mut cmd = Command::new("sleep");
        cmd.arg("30");
        let start = Instant::now();
        assert!(run(&mut cmd, Duration::from_millis(150)).is_none());
        assert!(start.elapsed() < Duration::from_secs(5), "超时没有生效");
    }

    #[test]
    fn run_returns_none_for_a_missing_binary() {
        let mut cmd = Command::new("nirifetch-definitely-not-a-real-binary");
        assert!(run(&mut cmd, Duration::from_millis(200)).is_none());
    }

    #[test]
    fn run_captures_stdout() {
        let mut cmd = Command::new("echo");
        cmd.arg("hi");
        let out = run(&mut cmd, Duration::from_secs(5)).expect("echo 应当成功");
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "hi");
    }
}
