//! 標準出力の読み手が `| head` のように閉じても、CLI がパニックしないこと。
//!
//! 閉じたパイプは子プロセスを起動する前に読み口を閉じる。`| head -1` は bash の
//! パイプラインで、書き手の終了コードを `PIPESTATUS` から取る。どちらも Unix と
//! macOS で同じである。`stations` というサブコマンドはなく、局の表示は `programs` である。
#![cfg(unix)]

use std::fs::{self, File};
use std::io::Read;
use std::os::fd::{FromRawFd, OwnedFd};
use std::process::{Command, ExitStatus, Stdio};

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_radiko-client")
}

fn chapters_fixture() -> String {
    format!(
        "{}/../../tests/fixtures/chapters.json",
        env!("CARGO_MANIFEST_DIR")
    )
}

/// 読み口を、子プロセスが書くより前に閉じた標準出力。
fn stdout_closed_before_start() -> Stdio {
    let mut fds = [0; 2];
    // SAFETY: 成功した pipe(2) の二つの fd を、それぞれ一度だけ OwnedFd に渡す。
    let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
    assert_eq!(rc, 0, "pipe");
    let reader = unsafe { OwnedFd::from_raw_fd(fds[0]) };
    let writer = unsafe { OwnedFd::from_raw_fd(fds[1]) };
    drop(reader);
    Stdio::from(writer)
}

fn spawn(args: &[String], stdout: Stdio) -> std::process::Child {
    Command::new(binary())
        .args(args)
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

fn code_of(status: ExitStatus) -> i32 {
    status
        .code()
        .unwrap_or_else(|| panic!("signal exit: {status}"))
}

fn reject_panic(args: &[String], stderr: &str) {
    assert!(
        !stderr.contains("panicked")
            && !stderr.contains("failed printing to stdout")
            && !stderr.contains("os error 32"),
        "args={args:?}\nstderr={stderr}"
    );
}

#[derive(Clone, Copy)]
enum Kind {
    /// 標準出力へ書く。閉じたパイプでは 141。
    Writes,
    /// 成功しても標準出力へは書かない。
    Empty,
    /// 標準出力へ書く前に失敗する。終了コード 1 と stderr の断片を維持する。
    Fails(&'static str),
    /// 音声機器が無い環境では終了 1、書ける環境では標準出力へ進む。
    Device,
}

fn data_dir(root: &std::path::Path) -> String {
    root.join("data").to_str().unwrap().to_string()
}

fn cases(dir: &str) -> Vec<(Vec<String>, Kind)> {
    let with_dir = |args: &[&str]| {
        let mut full = vec!["--data-dir".to_string(), dir.to_string()];
        full.extend(args.iter().copied().map(str::to_string));
        full
    };
    vec![
        (vec!["--help".into()], Kind::Writes),
        (with_dir(&["login"]), Kind::Fails("対話端末")),
        (with_dir(&["session"]), Kind::Writes),
        (with_dir(&["logout"]), Kind::Writes),
        (vec!["probe-live".into()], Kind::Fails("引数が不正")),
        (vec!["probe-timefree".into()], Kind::Fails("引数が不正")),
        (vec!["play-live".into(), "--help".into()], Kind::Writes),
        (vec!["play-timefree".into(), "--help".into()], Kind::Writes),
        (vec!["play-live".into()], Kind::Fails("play-live")),
        (vec!["play-timefree".into()], Kind::Fails("play-timefree")),
        (vec!["output-info".into(), "--help".into()], Kind::Writes),
        (vec!["output-info".into()], Kind::Device),
        (vec!["probe-output".into(), "--help".into()], Kind::Writes),
        (vec!["probe-output".into()], Kind::Device),
        (vec!["play-sample".into()], Kind::Fails("引数が不正")),
        (vec!["programs".into(), "--help".into()], Kind::Writes),
        (with_dir(&["programs"]), Kind::Fails("programs")),
        (vec!["tui".into(), "--help".into()], Kind::Writes),
        (vec!["searches".into(), "--help".into()], Kind::Writes),
        (with_dir(&["searches"]), Kind::Fails("searches")),
        (with_dir(&["searches", "list"]), Kind::Empty),
        (with_dir(&["searches", "restore"]), Kind::Writes),
        (
            with_dir(&["searches", "save", "demo", "--all"]),
            Kind::Writes,
        ),
        (with_dir(&["searches", "show", "demo"]), Kind::Writes),
        (with_dir(&["searches", "list"]), Kind::Writes),
        (with_dir(&["searches", "delete", "demo"]), Kind::Writes),
        (vec!["chapters".into(), "--help".into()], Kind::Writes),
        (vec!["chapters".into()], Kind::Fails("chapters")),
        (
            vec![
                "inspect-chapters".into(),
                chapters_fixture(),
                "90".into(),
                "0".into(),
            ],
            Kind::Writes,
        ),
    ]
}

#[test]
fn closed_stdout_exits_141_without_a_panic_for_every_stdout_command() {
    let root = tempfile::tempdir().unwrap();
    let dir = data_dir(root.path());
    for (args, kind) in cases(&dir) {
        let child = spawn(&args, stdout_closed_before_start());
        let output = child.wait_with_output().unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        let code = code_of(output.status);
        reject_panic(&args, &stderr);
        assert_ne!(code, 101, "args={args:?}\nstderr={stderr}");
        match kind {
            Kind::Writes => {
                assert_eq!(code, 141, "args={args:?}\nstderr={stderr}");
                assert!(
                    stderr.is_empty(),
                    "閉じたパイプでエラー表示が増えた args={args:?}\nstderr={stderr}"
                );
            }
            Kind::Empty => {
                assert_eq!(code, 0, "args={args:?}\nstderr={stderr}");
                assert!(stderr.is_empty(), "args={args:?}\nstderr={stderr}");
            }
            Kind::Fails(snippet) => {
                assert_eq!(code, 1, "args={args:?}\nstderr={stderr}");
                assert!(stderr.contains(snippet), "args={args:?}\nstderr={stderr}");
            }
            Kind::Device => {
                assert!(
                    code == 1 || code == 141,
                    "args={args:?} code={code}\nstderr={stderr}"
                );
                if code == 1 {
                    assert!(stderr.contains("エラー:"), "args={args:?}\nstderr={stderr}");
                } else {
                    assert!(
                        !stderr.contains("エラー:"),
                        "args={args:?}\nstderr={stderr}"
                    );
                }
            }
        }
    }
}

fn head_pipeline(args: &[String]) -> (i32, String, String) {
    let scratch = tempfile::tempdir().unwrap();
    let status = scratch.path().join("status");
    let first = scratch.path().join("first");
    let output = Command::new("bash")
        .arg("-c")
        .arg(
            r#"set +o pipefail
"$RADIKO_BIN" "$@" | head -1 > "$RADIKO_FIRST"
code=${PIPESTATUS[0]}
printf '%s' "$code" > "$RADIKO_STATUS"
"#,
        )
        .arg("head-pipeline")
        .args(args)
        .env("RADIKO_BIN", binary())
        .env("RADIKO_STATUS", &status)
        .env("RADIKO_FIRST", &first)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "bash failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let code = fs::read_to_string(&status).unwrap().trim().parse().unwrap();
    let line = fs::read_to_string(&first).unwrap_or_default();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    (code, line, stderr)
}

#[test]
fn head_dash_one_does_not_panic_for_every_command() {
    let root = tempfile::tempdir().unwrap();
    let dir = data_dir(root.path());
    for (args, kind) in cases(&dir) {
        let (code, line, stderr) = head_pipeline(&args);
        reject_panic(&args, &stderr);
        assert!(
            !stderr.contains("Broken pipe"),
            "args={args:?}\nstderr={stderr}"
        );
        assert_ne!(code, 101, "args={args:?}\nstderr={stderr}");
        match kind {
            Kind::Writes => {
                assert!(
                    code == 0 || code == 141,
                    "args={args:?} code={code} line={line:?}\nstderr={stderr}"
                );
                assert!(
                    !line.is_empty(),
                    "head が1行を受け取れなかった args={args:?} code={code}"
                );
                assert!(stderr.is_empty(), "args={args:?}\nstderr={stderr}");
            }
            Kind::Empty => {
                assert_eq!(code, 0, "args={args:?}\nstderr={stderr}");
                assert!(line.is_empty(), "args={args:?} line={line:?}");
            }
            Kind::Fails(snippet) => {
                assert_eq!(code, 1, "args={args:?}\nstderr={stderr}");
                assert!(stderr.contains(snippet), "args={args:?}\nstderr={stderr}");
                assert!(line.is_empty(), "args={args:?} line={line:?}");
            }
            Kind::Device => {
                assert!(
                    code == 0 || code == 1 || code == 141,
                    "args={args:?} code={code}\nstderr={stderr}"
                );
                if code == 1 {
                    assert!(stderr.contains("エラー:"), "args={args:?}\nstderr={stderr}");
                    assert!(line.is_empty(), "args={args:?} line={line:?}");
                }
            }
        }
    }
}

fn captured(args: &[String]) -> (i32, String, String) {
    let output = Command::new(binary())
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    (
        code_of(output.status),
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

#[test]
fn fully_read_stdout_keeps_exit_status_and_complete_text() {
    let root = tempfile::tempdir().unwrap();
    let dir = data_dir(root.path());
    for (args, kind) in cases(&dir) {
        let (code, stdout, stderr) = captured(&args);
        reject_panic(&args, &stderr);
        match kind {
            Kind::Writes => {
                assert_eq!(code, 0, "args={args:?}\nstderr={stderr}\nstdout={stdout}");
                assert!(!stdout.is_empty(), "args={args:?}");
                assert!(stderr.is_empty(), "args={args:?}\nstderr={stderr}");
                if args.iter().any(|arg| arg == "show" || arg == "restore") {
                    let text = stdout.trim();
                    assert!(text.starts_with('{') && text.ends_with('}'), "{stdout}");
                    assert!(stdout.contains("\"scope\""), "{stdout}");
                }
                if args.first().is_some_and(|arg| arg == "inspect-chapters") {
                    assert!(stdout.contains("不正項目: 1 / 除外した楽曲: 1"), "{stdout}");
                }
                if args == ["programs", "--help"] {
                    assert!(stdout.contains("--areas") && stdout.contains("--all"));
                }
            }
            Kind::Empty => {
                assert_eq!(code, 0, "args={args:?}\nstderr={stderr}");
                assert!(stdout.is_empty());
            }
            Kind::Fails(snippet) => {
                assert_eq!(code, 1, "args={args:?}\nstderr={stderr}");
                assert!(stderr.contains(snippet), "args={args:?}\nstderr={stderr}");
                assert!(stdout.is_empty());
            }
            Kind::Device => {
                assert!(
                    code == 0 || code == 1,
                    "args={args:?} code={code}\n{stderr}"
                );
                if code == 1 {
                    assert!(stderr.contains("エラー:"), "{stderr}");
                } else {
                    assert!(!stdout.is_empty(), "{stdout}");
                }
            }
        }
    }
}

#[test]
fn read_one_line_then_close_matches_head_without_a_panic() {
    let root = tempfile::tempdir().unwrap();
    let dir = data_dir(root.path());
    let args = vec![
        "--data-dir".to_string(),
        dir,
        "searches".into(),
        "restore".into(),
    ];
    let mut fds = [0; 2];
    let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
    assert_eq!(rc, 0, "pipe");
    let reader = unsafe { OwnedFd::from_raw_fd(fds[0]) };
    let writer = unsafe { OwnedFd::from_raw_fd(fds[1]) };
    let child = spawn(&args, Stdio::from(writer));
    let mut reader = File::from(reader);
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match reader.read(&mut byte) {
            Ok(0) => break,
            Ok(_) => {
                line.push(byte[0]);
                if byte[0] == b'\n' {
                    break;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => panic!("{error}"),
        }
    }
    drop(reader);
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    let code = code_of(output.status);
    reject_panic(&args, &stderr);
    assert!(code == 0 || code == 141, "code={code} stderr={stderr}");
    assert!(!line.is_empty());
    assert!(!stderr.contains("Broken pipe"), "{stderr}");
}
