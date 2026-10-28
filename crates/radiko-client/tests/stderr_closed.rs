//! `tui --offline` は、標準エラーが閉じていても `q` で終了 0 になり、端末を戻す。
#![cfg(unix)]

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_radiko-client")
}

/// macOS `openpty` takes `*mut winsize`. Linux takes `*const`.
fn open_pty(
    master: *mut libc::c_int,
    slave: *mut libc::c_int,
    winsize: libc::winsize,
) -> libc::c_int {
    #[cfg(target_os = "macos")]
    let mut winsize = winsize;
    unsafe {
        #[cfg(target_os = "macos")]
        {
            libc::openpty(
                master,
                slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut winsize,
            )
        }
        #[cfg(not(target_os = "macos"))]
        {
            libc::openpty(
                master,
                slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &winsize,
            )
        }
    }
}

fn termios(fd: i32) -> libc::termios {
    let mut value = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::tcgetattr(fd, &mut value) };
    assert_eq!(rc, 0, "tcgetattr");
    value
}

fn same_terminal(before: &libc::termios, after: &libc::termios) -> bool {
    before.c_iflag == after.c_iflag
        && before.c_oflag == after.c_oflag
        && before.c_cflag == after.c_cflag
        && before.c_lflag == after.c_lflag
        && before.c_cc == after.c_cc
        && before.c_ispeed == after.c_ispeed
        && before.c_ospeed == after.c_ospeed
}

fn plain(bytes: &[u8]) -> String {
    let mut text = String::new();
    let owned = String::from_utf8_lossy(bytes);
    let mut chars = owned.chars();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            for next in chars.by_ref() {
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
        } else if !ch.is_control() {
            text.push(ch);
        }
    }
    text
}

fn read_available(master: &mut File, stored: &mut Vec<u8>) {
    let mut buf = [0u8; 8192];
    let mut pollfd = libc::pollfd {
        fd: master.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let ready = unsafe { libc::poll(&mut pollfd, 1, 50) };
    if ready <= 0 {
        return;
    }
    match master.read(&mut buf) {
        Ok(0) | Err(_) => {}
        Ok(n) => stored.extend_from_slice(&buf[..n]),
    }
}

fn quit_offline(close_fd: bool, stderr: Stdio) {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let mut master_fd = -1;
    let mut slave_fd = -1;
    let opened = open_pty(
        &mut master_fd,
        &mut slave_fd,
        libc::winsize {
            ws_row: 40,
            ws_col: 160,
            ws_xpixel: 0,
            ws_ypixel: 0,
        },
    );
    assert_eq!(opened, 0, "openpty");
    let mut master = unsafe { File::from_raw_fd(master_fd) };
    let slave = unsafe { File::from_raw_fd(slave_fd) };
    let flags = unsafe { libc::fcntl(master.as_raw_fd(), libc::F_GETFL) };
    assert!(flags >= 0);
    assert_eq!(
        unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) },
        0
    );
    let kept = slave.try_clone().unwrap();
    let before = termios(kept.as_raw_fd());
    let mut command = Command::new(binary());
    command
        .args([
            "--data-dir",
            data.to_str().unwrap(),
            "tui",
            "--offline",
            "TBS",
        ])
        .env("TERM", "xterm-256color")
        .env("LANG", "C.UTF-8")
        .env("LC_ALL", "C.UTF-8")
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(stderr);
    unsafe {
        command.pre_exec(move || {
            if close_fd {
                libc::close(libc::STDERR_FILENO);
            }
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().unwrap();
    let mut stored = Vec::new();
    let ready_deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < ready_deadline {
        read_available(&mut master, &mut stored);
        if plain(&stored).contains("オフライン確認") {
            break;
        }
        assert!(
            child.try_wait().unwrap().is_none(),
            "TUI exited before it was ready"
        );
    }
    assert!(
        plain(&stored).contains("オフライン確認"),
        "offline TUI did not start ({} bytes)",
        stored.len()
    );
    master.write_all(b"q").unwrap();
    let exit_deadline = Instant::now() + Duration::from_secs(8);
    let status = loop {
        read_available(&mut master, &mut stored);
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < exit_deadline, "offline TUI did not exit");
    };
    read_available(&mut master, &mut stored);
    let code = status
        .code()
        .unwrap_or_else(|| panic!("signal exit: {status}"));
    assert_eq!(code, 0, "closed stderr changed the TUI exit code to {code}");
    assert!(
        stored.windows(8).any(|window| window == b"\x1b[?1049l"),
        "alternate screen was not left"
    );
    let after = termios(kept.as_raw_fd());
    assert!(
        same_terminal(&before, &after),
        "terminal attributes were not restored"
    );
}

#[test]
fn offline_tui_q_exits_0_and_restores_the_terminal_when_stderr_is_closed() {
    quit_offline(true, Stdio::null());
    let (reader, writer) = std::os::unix::net::UnixStream::pair().unwrap();
    drop(reader);
    quit_offline(false, Stdio::from(OwnedFd::from(writer)));
}
