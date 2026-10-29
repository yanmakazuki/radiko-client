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
