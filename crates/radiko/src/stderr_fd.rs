//! Linux で fd 2 を一時的に `/dev/null` へ差し替え、アプリの表示は元の標準エラーへ残す。
//!
//! `dup` / `dup2` / `open` は rustix の安全な関数だけを使う。

use std::fmt::Arguments;
use std::os::fd::OwnedFd;
use std::sync::{Mutex, MutexGuard};

use rustix::fd::AsFd;
use rustix::fs::{self, Mode, OFlags};
use rustix::io::{self, Errno};

struct State {
    /// プロセス開始時の標準エラー。アプリの表示はここへ書く。
    preserved: Option<OwnedFd>,
    /// 差し替え中だけ持つ、戻すための fd 2 の複製。
    saved: Option<OwnedFd>,
    /// 入れ子の差し替え。0 のとき fd 2 は元のまま。
    depth: usize,
}

static STATE: Mutex<State> = Mutex::new(State {
    preserved: None,
    saved: None,
    depth: 0,
});

static WRITE_ORDER: Mutex<()> = Mutex::new(());

fn lock() -> MutexGuard<'static, State> {
    STATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn ensure_preserved(state: &mut State) {
    if state.preserved.is_some() {
        return;
    }
    // fd 2 より大きい番号を使う。失敗したときは差し替えないので、表示は fd 2 に残る。
    if let Ok(fd) = io::fcntl_dupfd_cloexec(rustix::stdio::stderr(), 3) {
        state.preserved = Some(fd);
    }
}

pub(super) fn begin_redirect() -> bool {
    install_panic_hook();
    let mut state = lock();
    ensure_preserved(&mut state);
    if state.preserved.is_none() {
        return false;
    }
    if state.depth > 0 {
        state.depth += 1;
        return true;
    }
    let saved = match io::fcntl_dupfd_cloexec(rustix::stdio::stderr(), 3) {
        Ok(fd) => fd,
        Err(_) => return false,
    };
    let null = match fs::open("/dev/null", OFlags::WRONLY | OFlags::CLOEXEC, Mode::empty()) {
        Ok(fd) => fd,
        Err(_) => return false,
    };
    if rustix::stdio::dup2_stderr(&null).is_err() {
        return false;
    }
    state.saved = Some(saved);
    state.depth = 1;
    true
}

pub(super) fn end_redirect() {
    let mut state = lock();
    if state.depth == 0 {
        return;
    }
    state.depth -= 1;
    if state.depth == 0 {
        if let Some(saved) = state.saved.take() {
            let _ = rustix::stdio::dup2_stderr(&saved);
        }
    }
}

fn clone_preserved() -> Option<OwnedFd> {
    let mut state = lock();
    ensure_preserved(&mut state);
    state.preserved.as_ref().and_then(|fd| fd.try_clone().ok())
}

fn redirected() -> bool {
    lock().depth > 0
}

fn write_all(fd: impl AsFd, mut bytes: &[u8]) {
    while !bytes.is_empty() {
        match io::write(&fd, bytes) {
            Ok(0) => return,
            Ok(n) => bytes = &bytes[n..],
            Err(Errno::INTR) => continue,
            Err(_) => return,
        }
    }
}

pub(super) fn write_fmt(args: Arguments<'_>, newline: bool) {
    let mut text = String::new();
    if std::fmt::Write::write_fmt(&mut text, args).is_err() {
        return;
    }
    if newline {
        text.push('\n');
    }
    write_bytes(text.as_bytes());
}

/// パニックはガードのドロップより先に標準のフックへ書かれる。
/// fd 2 が `/dev/null` のままだとそのメッセージは消える。
/// フックは一度だけ、それまで入っていたフックを呼ぶ形で足す。
/// 窓の外ではそのフックをそのまま呼ぶ。窓の中では、呼ぶ直前だけ fd 2 を
/// 保存してある標準エラーへ戻し、戻ったら深さが残っていれば再び `/dev/null` にする。
/// メッセージの文面は前のフック（既定または libtest）に任せる。
/// ロックを持ったまま前のフックは呼ばない。同じスレッドが既にロックを持って
/// パニックしたときは取り直さず、デッドロックもしない。
fn install_panic_hook() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let conceal = ConcealAfterPanic(reveal_stderr_for_panic());
            previous(info);
            drop(conceal);
        }));
    });
}

struct ConcealAfterPanic(bool);

impl Drop for ConcealAfterPanic {
    fn drop(&mut self) {
        if self.0 {
            conceal_stderr_after_panic();
        }
    }
}

fn reveal_stderr_for_panic() -> bool {
    let state = match STATE.try_lock() {
        Ok(guard) => guard,
        Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => return false,
    };
    if state.depth == 0 {
        return false;
    }
    let Some(saved) = state.saved.as_ref() else {
        return false;
    };
    rustix::stdio::dup2_stderr(saved).is_ok()
}

fn conceal_stderr_after_panic() {
    let state = match STATE.try_lock() {
        Ok(guard) => guard,
        Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => return,
    };
    if state.depth == 0 {
        return;
    }
    let Ok(null) = fs::open("/dev/null", OFlags::WRONLY | OFlags::CLOEXEC, Mode::empty()) else {
        return;
    };
    let _ = rustix::stdio::dup2_stderr(&null);
}

fn write_bytes(bytes: &[u8]) {
    let preserved = clone_preserved();
    let _order = WRITE_ORDER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(fd) = preserved {
        write_all(fd, bytes);
        return;
    }
    if !redirected() {
        let _ = std::io::Write::write_all(&mut std::io::stderr(), bytes);
    }
}

#[cfg(test)]
mod tests {
    use std::process::{Command, Stdio};
    use std::sync::{Arc, Barrier, mpsc};
    use std::thread;

    const HIDDEN_APP: &str = "\
並行表示
AFTER
エラー: 既定の音声出力機器がありません
";
    const SHOWN: &str = "\
HIDDEN-JACK
STILL
並行表示
DURING
AFTER
エラー: 既定の音声出力機器がありません
";

    #[test]
    fn linux_native_stderr_redirect_hides_fd2_and_keeps_app_lines() {
        if std::env::var_os("RADIKO_FD2_CHILD").is_some() {
            child();
            return;
        }
        let name = std::thread::current()
            .name()
            .expect("libtest names the test thread")
            .to_owned();
        let hidden = run_child(&name, false);
        assert!(
            hidden.status.success(),
            "hide child status {:?}\nstdout:\n{}\nstderr:\n{}",
            hidden.status.code(),
            String::from_utf8_lossy(&hidden.stdout),
            String::from_utf8_lossy(&hidden.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&hidden.stderr),
            HIDDEN_APP,
            "native fd 2 writes leaked or the restored error line was lost"
        );

        let shown = run_child(&name, true);
        assert!(
            shown.status.success(),
            "show child status {:?}\nstderr:\n{}",
            shown.status.code(),
            String::from_utf8_lossy(&shown.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&shown.stderr),
            SHOWN,
            "RADIKO_ALSA_LOG=1 should pass native fd 2 writes through"
        );
    }

    fn run_child(name: &str, show_logs: bool) -> std::process::Output {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", name, "--test-threads=1"])
            .env("RADIKO_FD2_CHILD", "1")
            .stdin(Stdio::null());
        if show_logs {
            command.env("RADIKO_ALSA_LOG", "1");
        } else {
            command.env_remove("RADIKO_ALSA_LOG");
        }
        command.output().unwrap()
    }

    fn child() {
        let (done_tx, done_rx) = mpsc::channel();
        let start = Arc::new(Barrier::new(2));
        let start_writer = Arc::clone(&start);
        let writer = thread::spawn(move || {
            start_writer.wait();
            crate::stderr::write_line(format_args!("並行表示"));
            let _ = done_tx.send(());
        });
        let failed = failing_open(start, done_rx);
        writer.join().unwrap();
        let _ = rustix::io::write(rustix::stdio::stderr(), b"AFTER\n");
        if let Err(message) = failed {
            crate::stderr::write_line(format_args!("エラー: {message}"));
        }
    }

    #[test]
    fn panic_inside_the_silence_window_is_printed_and_exits_101() {
        if let Some(kind) = std::env::var_os("RADIKO_PANIC_CHILD") {
            match kind.to_string_lossy().as_ref() {
                "main" => {
                    let _guard = crate::stderr::silence_native_stderr();
                    panic!("silence-window-main-thread");
                }
                "other" => {
                    let _guard = crate::stderr::silence_native_stderr();
                    let error = thread::spawn(|| panic!("silence-window-other-thread"))
                        .join()
                        .expect_err("other thread should panic");
                    std::panic::resume_unwind(error);
                }
                "closed" => {
                    let _guard = crate::stderr::silence_native_stderr();
                    panic!("silence-window-closed-stderr");
                }
                other => panic!("unknown panic child {other}"),
            }
        }
        let name = std::thread::current()
            .name()
            .expect("libtest names the test thread")
            .to_owned();
        for (kind, marker) in [
            ("main", "silence-window-main-thread"),
            ("other", "silence-window-other-thread"),
        ] {
            let output = run_panic_child(&name, kind);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert_eq!(
                output.status.code(),
                Some(101),
                "{kind} status {:?} stderr:\n{stderr}",
                output.status
            );
            assert!(
                stderr.contains(marker) && stderr.contains("panicked at"),
                "{kind} panic message was swallowed:\n{stderr}"
            );
        }
        let closed = Command::new("bash")
            .arg("-c")
            .arg(r#"exec "$@" 2>&-"#)
            .arg("closed-stderr")
            .arg(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg(&name)
            .arg("--test-threads=1")
            .arg("--nocapture")
            .env("RADIKO_PANIC_CHILD", "closed")
            .env_remove("RADIKO_ALSA_LOG")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&closed.stdout);
        assert_eq!(
            closed.status.code(),
            Some(101),
            "closed stderr must exit 101 without aborting: {:?} stdout:\n{stdout}",
            closed.status
        );
        assert!(
            !stdout.contains("fatal runtime error"),
            "double panic:\n{stdout}"
        );
    }

    fn run_panic_child(name: &str, kind: &str) -> std::process::Output {
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", name, "--test-threads=1", "--nocapture"])
            .env("RADIKO_PANIC_CHILD", kind)
            .env_remove("RADIKO_ALSA_LOG")
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }

    fn failing_open(start: Arc<Barrier>, done: mpsc::Receiver<()>) -> Result<(), &'static str> {
        let _outer = crate::stderr::silence_native_stderr();
        let nested = crate::stderr::silence_native_stderr();
        let _ = rustix::io::write(rustix::stdio::stderr(), b"HIDDEN-JACK\n");
        drop(nested);
        let _ = rustix::io::write(rustix::stdio::stderr(), b"STILL\n");
        start.wait();
        done.recv().map_err(|_| "writer")?;
        let _ = rustix::io::write(rustix::stdio::stderr(), b"DURING\n");
        Err("既定の音声出力機器がありません")
    }
}
