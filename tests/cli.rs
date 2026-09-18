//! 命令行接口的端到端测试。
//!
//! 这里跑的是**真正的二进制**，断言的是单元测试够不到的东西：退出码、stdout
//! 与 stderr 的分工、以及与真实管道交互时的行为。
//!
//! 覆盖的路径全都在会话检测**之前**处理，所以不需要 niri 会话，任何环境下
//! 都能跑。

use std::process::{Command, Stdio};

/// 待测二进制。`CARGO_BIN_EXE_*` 由 cargo 在集成测试里注入。
fn nirifetch() -> Command {
    Command::new(env!("CARGO_BIN_EXE_nirifetch"))
}

/// 一次调用的完整结果。
struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

fn run(cmd: &mut Command) -> Run {
    let out = cmd.output().expect("二进制应当能启动");
    Run {
        code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

#[test]
fn every_version_spelling_prints_the_same_line() {
    for flag in ["-v", "-V", "--version"] {
        let r = run(nirifetch().arg(flag));
        assert_eq!(r.code, Some(0), "{flag} 应当正常退出");
        assert_eq!(
            r.stdout.trim(),
            format!("nirifetch {}", env!("CARGO_PKG_VERSION")),
            "{flag} 的输出不对"
        );
        assert!(r.stderr.is_empty(), "{flag} 不该往 stderr 写东西");
    }
}

#[test]
fn help_documents_the_options_and_the_author() {
    let r = run(nirifetch().arg("--help"));
    assert_eq!(r.code, Some(0));
    for needle in ["USAGE", "OPTIONS", "--json", "--version", "AUTHOR"] {
        assert!(r.stdout.contains(needle), "--help 里缺少 {needle:?}");
    }
}

#[test]
fn unknown_options_are_an_error_not_a_silent_fetch() {
    // 静默忽略会让人以为 `--jsno` 生效了，所以这里必须是用法错误。
    let r = run(nirifetch().arg("--jsno"));
    assert_eq!(r.code, Some(2), "用法错误应当以 EX_USAGE(2) 退出");
    assert!(r.stdout.is_empty(), "报错时不该往 stdout 写东西");
    assert!(r.stderr.contains("--jsno"), "报错里应当点名那个参数");
}

#[test]
fn help_wins_over_an_unknown_option() {
    // `-h` 是「告诉我怎么用」的请求，不该被拼写错误挡住。
    let r = run(nirifetch().args(["--typo", "-h"]));
    assert_eq!(r.code, Some(0));
    assert!(r.stdout.contains("USAGE"));
}

#[test]
fn writing_into_a_closed_pipe_does_not_panic() {
    // 回归测试：`std::println!` 在 EPIPE 上会 panic，而
    // `nirifetch | head -1`、`| grep -q …`、`| jq`（没装 jq）都会提前关掉读端。
    // 一个宣称「绝不 panic」的工具不该在这里破功。
    //
    // 直接丢掉读端来制造 EPIPE；子进程可能在我们丢掉之前就写完了，
    // 那样这条就退化成「没 panic 也没关系」—— 所以断言写成单向的。
    let mut child = nirifetch()
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("二进制应当能启动");

    // 立刻关闭读端，让后续写入拿到 EPIPE。
    drop(child.stdout.take());

    let out = child.wait_with_output().expect("应当能等到子进程退出");
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        !stderr.contains("panicked"),
        "写入关闭的管道时 panic 了：{stderr}"
    );
    assert_ne!(out.status.code(), Some(101), "101 是 Rust 的 panic 退出码");
}
