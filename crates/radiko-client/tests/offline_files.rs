//! Offline TUI must not read, rewrite, or delete session and search files.
#![cfg(unix)]
use std::{
    fs,
    io::{Read, Write},
    os::unix::{
        fs::PermissionsExt,
        io::{AsRawFd, FromRawFd},
        process::CommandExt,
    },
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

const SESSION: &[u8] = b"SENTINEL-SESSION-DO-NOT-TOUCH\n";
const SEARCHES: &[u8] = b"SENTINEL-SEARCHES-DO-NOT-TOUCH\n";

fn place(dir: &Path) {
    fs::create_dir_all(dir).unwrap();
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).unwrap();
    for (name, bytes) in [("session.json", SESSION), ("searches.json", SEARCHES)] {
        let path = dir.join(name);
        fs::write(&path, bytes).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    }
}

fn unchanged(dir: &Path) {
    assert_eq!(fs::read(dir.join("session.json")).unwrap(), SESSION);
    assert_eq!(fs::read(dir.join("searches.json")).unwrap(), SEARCHES);
}

fn pump(master: &mut std::fs::File, timeout: Duration) {
    let _ = read_until(master, "\u{0}", timeout);
}

fn visible(bytes: &[u8]) -> String {
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
        } else {
            text.push(ch);
        }
    }
    text
}

fn read_until(master: &mut std::fs::File, needle: &str, timeout: Duration) -> String {
    let mut stored = Vec::new();
    let mut buf = [0u8; 8192];
    let start = Instant::now();
    while start.elapsed() < timeout {
        let mut pollfd = libc::pollfd {
            fd: master.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&mut pollfd, 1, 100) };
        if ready <= 0 {
            continue;
        }
        match master.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => stored.extend_from_slice(&buf[..n]),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(error) => panic!("pty read failed: {error}"),
        }
        if !needle.is_empty() && visible(&stored).contains(needle) {
            break;
        }
    }
    visible(&stored)
}

#[test]
fn offline_tui_leaves_session_and_search_files_untouched() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let xdg = root.path().join("xdg");
    let config = root.path().join("config");
    let cache = root.path().join("cache");
    let data = root.path().join("data");
    let stores = [
        data.clone(),
        xdg.join("radiko-client"),
        home.join(".local/share/radiko-client"),
        home.join("Library/Application Support/jp.radiko-client.radiko-client"),
    ];
    for dir in &stores {
        place(dir);
    }
    fs::create_dir_all(&config).unwrap();
    fs::create_dir_all(&cache).unwrap();

    let mut master_fd = -1;
    let mut slave_fd = -1;
    let winsize = libc::winsize {
        ws_row: 40,
        ws_col: 160,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let opened = unsafe {
        libc::openpty(
            &mut master_fd,
            &mut slave_fd,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &winsize,
        )
    };
    assert_eq!(opened, 0, "openpty failed");
    let mut master = unsafe { std::fs::File::from_raw_fd(master_fd) };
    let slave = unsafe { std::fs::File::from_raw_fd(slave_fd) };
    let flags = unsafe { libc::fcntl(master.as_raw_fd(), libc::F_GETFL) };
    assert!(flags >= 0);
    assert_eq!(
        unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) },
        0
    );

    let mut command = Command::new(env!("CARGO_BIN_EXE_radiko-client"));
    command
        .args([
            "--data-dir",
            data.to_str().unwrap(),
            "tui",
            "--offline",
            "TBS",
        ])
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", &home)
        .env("XDG_DATA_HOME", &xdg)
        .env("XDG_CONFIG_HOME", &config)
        .env("XDG_CACHE_HOME", &cache)
        .env("XDG_STATE_HOME", &cache)
        .env("TERM", "xterm-256color")
        .env("LANG", "C.UTF-8")
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave));
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    let mut child = command.spawn().unwrap();
    let text = read_until(&mut master, "オフライン確認", Duration::from_secs(8));
    assert!(
        text.contains("オフライン確認"),
        "offline TUI did not start: {}",
        text.chars().take(500).collect::<String>()
    );
    for forbidden in ["破損", "読み込み失敗", "条件復元失敗", "条件保存失敗"] {
        assert!(
            !text.contains(forbidden),
            "offline TUI touched stored state ({forbidden})"
        );
    }
    master.write_all(b"Oybq").unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(deadline > Instant::now(), "offline TUI did not exit");
        pump(&mut master, Duration::from_millis(50));
    };
    let _ = child.wait();
    assert!(status.success(), "offline TUI exited {status}");
    for dir in &stores {
        unchanged(dir);
    }
}
