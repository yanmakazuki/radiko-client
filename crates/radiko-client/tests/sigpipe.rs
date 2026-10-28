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
use std::sync::Mutex;

/// `pipe` から `FD_CLOEXEC` を付けるまでのあいだに、他のテストが `spawn` しないようにする。
fn fork_guard() -> std::sync::MutexGuard<'static, ()> {
    static GUARD: Mutex<()> = Mutex::new(());
    GUARD.lock().unwrap_or_else(|poison| poison.into_inner())
}

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_radiko-client")
}

fn chapters_fixture() -> String {
    format!(
        "{}/../../tests/fixtures/chapters.json",
        env!("CARGO_MANIFEST_DIR")
    )
}

/// 読み口を、子プロセスが書くより前に閉じたパイプの書き口。
fn closed_pipe_writer() -> Stdio {
    let (_reader, writer) = cloexec_pipe();
    Stdio::from(writer)
}

/// 読み口を、子プロセスが書くより前に閉じた標準出力。
fn stdout_closed_before_start() -> Stdio {
    closed_pipe_writer()
}

/// 両方の端に `FD_CLOEXEC` を付けたパイプ。
///
/// `pipe` の直後は端が子プロセスへ引き継がれる。並列テストがそこへ `spawn` すると、
/// 閉じたはずの読み口が残り、短い `--help` が壊れたパイプにならず終了 0 になる。
fn cloexec_pipe() -> (OwnedFd, OwnedFd) {
    let _guard = fork_guard();
    let mut fds = [0; 2];
    // SAFETY: 成功した pipe(2) の二つの fd を、CLOEXEC を付けてからそれぞれ一度だけ OwnedFd に渡す。
    let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
    assert_eq!(rc, 0, "pipe");
    for fd in fds {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        assert!(flags >= 0, "F_GETFD");
        let rc = unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) };
        assert_eq!(rc, 0, "FD_CLOEXEC");
    }
    let reader = unsafe { OwnedFd::from_raw_fd(fds[0]) };
    let writer = unsafe { OwnedFd::from_raw_fd(fds[1]) };
    (reader, writer)
}

fn spawn(args: &[String], stdout: Stdio) -> std::process::Child {
    let _guard = fork_guard();
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
    let _guard = fork_guard();
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
    let _guard = fork_guard();
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
    let (reader, writer) = cloexec_pipe();
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

#[derive(Clone, Copy, Debug)]
enum Stderr {
    /// 読み手が最後まで読む。
    Open,
    /// `2>&-`。ファイル記述子 2 を閉じる。
    ClosedFd,
    /// パイプの読み口を、起動より前に閉じる。
    ClosedReader,
}

fn code_with_stderr(args: &[String], mode: Stderr) -> i32 {
    match mode {
        Stderr::Open => {
            let _guard = fork_guard();
            let output = Command::new(binary())
                .args(args)
                .stdin(Stdio::null())
                .output()
                .unwrap();
            code_of(output.status)
        }
        Stderr::ClosedFd => {
            let _guard = fork_guard();
            let output = Command::new("bash")
                .arg("-c")
                .arg(r#"exec "$@" 2>&-"#)
                .arg("closed-stderr")
                .arg(binary())
                .args(args)
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .output()
                .unwrap();
            assert!(
                output.status.code().is_some(),
                "bash/exec died on a signal: {}",
                output.status
            );
            code_of(output.status)
        }
        Stderr::ClosedReader => {
            let stderr = closed_pipe_writer();
            let _guard = fork_guard();
            let child = Command::new(binary())
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(stderr)
                .spawn()
                .unwrap();
            let output = child.wait_with_output().unwrap();
            code_of(output.status)
        }
    }
}

fn sequence(mode: Stderr) -> Vec<i32> {
    let root = tempfile::tempdir().unwrap();
    let dir = data_dir(root.path());
    cases(&dir)
        .into_iter()
        .map(|(args, _)| {
            let code = code_with_stderr(&args, mode);
            assert_ne!(code, 101, "args={args:?} mode={mode:?} code={code}");
            code
        })
        .collect()
}

#[test]
fn closed_stderr_does_not_change_exit_codes_for_the_stdout_command_set() {
    let open = sequence(Stderr::Open);
    assert_eq!(sequence(Stderr::ClosedFd), open);
    assert_eq!(sequence(Stderr::ClosedReader), open);
}

fn code_with_closed_stdout(args: &[String], mode: Stderr) -> i32 {
    match mode {
        Stderr::Open => {
            let child = spawn(args, stdout_closed_before_start());
            let output = child.wait_with_output().unwrap();
            code_of(output.status)
        }
        Stderr::ClosedFd => {
            let stdout = stdout_closed_before_start();
            let _guard = fork_guard();
            let status = Command::new("bash")
                .arg("-c")
                .arg(r#"exec "$@" 2>&-"#)
                .arg("closed-stderr")
                .arg(binary())
                .args(args)
                .stdin(Stdio::null())
                .stdout(stdout)
                .stderr(Stdio::null())
                .status()
                .unwrap();
            code_of(status)
        }
        Stderr::ClosedReader => {
            let stdout = closed_pipe_writer();
            let stderr = closed_pipe_writer();
            let _guard = fork_guard();
            let mut child = Command::new(binary())
                .args(args)
                .stdin(Stdio::null())
                .stdout(stdout)
                .stderr(stderr)
                .spawn()
                .unwrap();
            code_of(child.wait().unwrap())
        }
    }
}

#[test]
fn closed_stdout_stays_141_when_stderr_is_also_closed() {
    for mode in [Stderr::ClosedFd, Stderr::ClosedReader] {
        let root = tempfile::tempdir().unwrap();
        let dir = data_dir(root.path());
        let open_root = tempfile::tempdir().unwrap();
        let open_dir = data_dir(open_root.path());
        let open_cases = cases(&open_dir);
        for (index, (args, kind)) in cases(&dir).into_iter().enumerate() {
            let code = code_with_closed_stdout(&args, mode);
            let open = code_with_closed_stdout(&open_cases[index].0, Stderr::Open);
            assert_ne!(code, 101, "args={args:?} mode={mode:?}");
            assert_eq!(code, open, "args={args:?} mode={mode:?}");
            match kind {
                Kind::Writes => assert_eq!(code, 141, "args={args:?} mode={mode:?}"),
                Kind::Empty => assert_eq!(code, 0, "args={args:?} mode={mode:?}"),
                Kind::Fails(_) => assert_eq!(code, 1, "args={args:?} mode={mode:?}"),
                Kind::Device => {
                    assert!(
                        code == 1 || code == 141,
                        "args={args:?} mode={mode:?} code={code}"
                    )
                }
            }
        }
    }
}

fn dead_proxy(command: &mut Command) {
    for key in [
        "http_proxy",
        "HTTP_PROXY",
        "https_proxy",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "all_proxy",
    ] {
        command.env(key, "http://127.0.0.1:1");
    }
    command.env_remove("NO_PROXY");
    command.env_remove("no_proxy");
}

fn code_progress(args: &[String], mode: Stderr) -> i32 {
    match mode {
        Stderr::Open => {
            let _guard = fork_guard();
            let mut command = Command::new(binary());
            dead_proxy(&mut command);
            let output = command.args(args).stdin(Stdio::null()).output().unwrap();
            code_of(output.status)
        }
        Stderr::ClosedFd => {
            let _guard = fork_guard();
            let mut command = Command::new("bash");
            dead_proxy(&mut command);
            let output = command
                .arg("-c")
                .arg(r#"exec "$@" 2>&-"#)
                .arg("closed-stderr")
                .arg(binary())
                .args(args)
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .output()
                .unwrap();
            code_of(output.status)
        }
        Stderr::ClosedReader => {
            let stderr = closed_pipe_writer();
            let _guard = fork_guard();
            let mut command = Command::new(binary());
            dead_proxy(&mut command);
            let child = command
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(stderr)
                .spawn()
                .unwrap();
            code_of(child.wait_with_output().unwrap().status)
        }
    }
}

#[test]
fn program_progress_on_closed_stderr_exits_1_without_a_panic() {
    // `programs --areas` reaches the progress line in programs.rs. A dead proxy
    // fails the fetch before any radiko connection, and still runs that line.
    let args_for = |dir: &str, tail: &[&str]| {
        let mut args = vec!["--data-dir".to_string(), dir.to_string(), "programs".into()];
        args.extend(tail.iter().copied().map(str::to_string));
        args
    };
    for tail in [vec!["--areas", "JP13"], vec!["TBS", "20261001", "20261002"]] {
        for mode in [Stderr::Open, Stderr::ClosedFd, Stderr::ClosedReader] {
            let root = tempfile::tempdir().unwrap();
            let args = args_for(data_dir(root.path()).as_str(), &tail);
            let code = code_progress(&args, mode);
            assert_eq!(code, 1, "args={args:?} mode={mode:?} code={code}");
        }
    }
}

fn prepared_dir(bytes: &[u8], name: &str, mode: u32) -> (tempfile::TempDir, String) {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("data");
    fs::create_dir_all(&dir).unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
    let path = dir.join(name);
    fs::write(&path, bytes).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
    (root, dir.to_str().unwrap().to_string())
}

#[test]
fn stderr_messages_that_do_not_fail_the_command_keep_exit_0() {
    for mode in [Stderr::Open, Stderr::ClosedFd, Stderr::ClosedReader] {
        let (_root, dir) = prepared_dir(b"corrupt-synthetic-session", "session.json", 0o600);
        let session = vec!["--data-dir".into(), dir.clone(), "session".into()];
        assert_eq!(code_with_stderr(&session, mode), 0, "session mode={mode:?}");

        let (_root, dir) = prepared_dir(b"not-json", "searches.json", 0o600);
        let restore = vec![
            "--data-dir".into(),
            dir,
            "searches".into(),
            "restore".into(),
        ];
        assert_eq!(code_with_stderr(&restore, mode), 0, "restore mode={mode:?}");
    }
}

#[test]
fn search_save_and_restore_errors_on_closed_stderr_still_exit_1() {
    for mode in [Stderr::Open, Stderr::ClosedFd, Stderr::ClosedReader] {
        let (_root, dir) = prepared_dir(b"not-json", "searches.json", 0o644);
        let save = vec!["--data-dir".into(), dir, "programs".into(), "TBS".into()];
        let code = code_progress(&save, mode);
        assert_eq!(code, 1, "save mode={mode:?} code={code}");

        let (_root, dir) = prepared_dir(b"not-json", "searches.json", 0o600);
        let restore = vec![
            "--data-dir".into(),
            dir,
            "programs".into(),
            "--restore".into(),
        ];
        let code = code_progress(&restore, mode);
        assert_eq!(code, 1, "programs restore mode={mode:?} code={code}");
    }
}
