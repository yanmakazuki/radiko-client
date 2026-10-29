//! `/dev/full` は Linux のデバイスである。macOS には無いので、このファイルは Linux だけでコンパイルする。
//! 番組表の試験用オリジンはデバッグビルドだけが読むので、リリースの `cargo test` には入れない。
#![cfg(all(target_os = "linux", debug_assertions))]

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::process::{Command, Stdio};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

fn full_device() -> File {
    // `File::open` is read-only. Rust then replaces stdout, and the write does not see ENOSPC.
    OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/full")
        .expect("/dev/full")
}

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_radiko-client")
}

struct Catalog {
    origin: String,
    paths: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    server: Option<JoinHandle<()>>,
}

impl Catalog {
    fn spawn() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.set_nonblocking(true).expect("nonblocking");
        let origin = format!("http://{}", listener.local_addr().expect("addr"));
        let paths = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let recorded = Arc::clone(&paths);
        let stop_flag = Arc::clone(&stop);
        let server = thread::spawn(move || {
            while !stop_flag.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut socket, _)) => {
                        socket.set_nonblocking(false).expect("blocking");
                        let _ = socket.set_read_timeout(Some(Duration::from_secs(2)));
                        let _ = socket.set_write_timeout(Some(Duration::from_secs(2)));
                        let path = request_path(&mut socket);
                        recorded.lock().expect("paths").push(path.clone());
                        let body = schedule_xml(&path);
                        let header = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = socket.write_all(header.as_bytes());
                        let _ = socket.write_all(&body);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("accept: {error}"),
                }
            }
        });
        Self {
            origin,
            paths,
            stop,
            server: Some(server),
        }
    }

    fn paths(&self) -> Vec<String> {
        self.paths.lock().expect("paths").clone()
    }
}

impl Drop for Catalog {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(server) = self.server.take() {
            let _ = server.join();
        }
    }
}

fn request_path(socket: &mut TcpStream) -> String {
    let mut bytes = Vec::new();
    let mut chunk = [0; 1024];
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while !bytes.windows(4).any(|window| window == b"\r\n\r\n") {
        if std::time::Instant::now() >= deadline || bytes.len() > 8192 {
            break;
        }
        match socket.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => bytes.extend_from_slice(&chunk[..n]),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
                ) => {}
            Err(_) => break,
        }
    }
    let text = String::from_utf8_lossy(&bytes);
    text.split_whitespace().nth(1).unwrap_or("/").to_string()
}

fn schedule_xml(path: &str) -> Vec<u8> {
    let date = path
        .split('/')
        .find(|part| part.len() == 8 && part.chars().all(|ch| ch.is_ascii_digit()))
        .unwrap_or("20261001");
    format!(
        "<radiko><stations><station id=\"TBS\"><name>試験</name><progs>\
<prog id=\"p\" ft=\"{date}060000\" to=\"{date}070000\"><title>試験番組</title></prog>\
</progs></station></stations></radiko>"
    )
    .into_bytes()
}

struct Ran {
    code: i32,
    stderr: String,
}

fn run(origin: &str, stdout: Stdio, stderr: Stdio) -> Ran {
    let root = tempfile::tempdir().expect("tempdir");
    let dir = root.path().join("data");
    std::fs::create_dir_all(&dir).expect("data dir");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("0700");
    }
    let output = Command::new(binary())
        .args([
            "--data-dir",
            dir.to_str().expect("utf-8"),
            "programs",
            "TBS",
            "20261001",
            "20261005",
        ])
        .env("RADIKO_CATALOG_ORIGIN", origin)
        .env_remove("http_proxy")
        .env_remove("HTTP_PROXY")
        .env_remove("https_proxy")
        .env_remove("HTTPS_PROXY")
        .env_remove("ALL_PROXY")
        .env_remove("all_proxy")
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        .output()
        .expect("spawn");
    Ran {
        code: output.status.code().expect("signal"),
        stderr: String::from_utf8(output.stderr).expect("stderr utf-8"),
    }
}

fn program_paths(paths: &[String]) -> Vec<&str> {
    paths
        .iter()
        .filter(|path| path.contains("/program/"))
        .map(String::as_str)
        .collect()
}

#[test]
fn stdout_full_stops_later_days_and_prints_one_out_of_space_line() {
    let catalog = Catalog::spawn();
    let ran = run(&catalog.origin, Stdio::from(full_device()), Stdio::piped());
    let paths = catalog.paths();
    let programs = program_paths(&paths);
    assert_eq!(ran.code, 1, "stderr={}", ran.stderr);
    let errors: Vec<_> = ran
        .stderr
        .lines()
        .filter(|line| line.starts_with("エラー:"))
        .collect();
    assert_eq!(
        errors,
        ["エラー: 書き込みに失敗しました（空き容量不足）"],
        "stderr={}",
        ran.stderr
    );
    assert!(
        programs.iter().any(|path| path.contains("/20261001/")),
        "{programs:?}"
    );
    assert!(
        programs.iter().all(|path| !path.contains("/20261005/")),
        "later day was fetched: {programs:?}"
    );
    assert!(!programs.is_empty() && programs.len() < 5, "{programs:?}");
}

#[test]
fn stderr_full_does_not_stop_the_fetch_or_change_the_exit_code() {
    let open = Catalog::spawn();
    let open_run = run(&open.origin, Stdio::piped(), Stdio::piped());
    let full = Catalog::spawn();
    let sink = full_device();
    let full_run = run(&full.origin, Stdio::piped(), Stdio::from(sink));
    assert_eq!(open_run.code, 0, "stderr={}", open_run.stderr);
    assert_eq!(full_run.code, open_run.code);
    assert_eq!(program_paths(&full.paths()).len(), 5, "{:?}", full.paths());
    assert!(
        full.paths().iter().any(|path| path.contains("/20261005/")),
        "{:?}",
        full.paths()
    );
}

#[test]
fn closed_stdout_stays_141_without_an_error_line_and_skips_later_days() {
    let catalog = Catalog::spawn();
    let (reader, writer) = UnixStream::pair().expect("socketpair");
    drop(reader);
    let ran = run(
        &catalog.origin,
        Stdio::from(OwnedFd::from(writer)),
        Stdio::piped(),
    );
    assert_eq!(ran.code, 141, "stderr={}", ran.stderr);
    assert!(
        ran.stderr.lines().all(|line| !line.starts_with("エラー:")),
        "stderr={}",
        ran.stderr
    );
    assert!(
        program_paths(&catalog.paths())
            .iter()
            .all(|path| !path.contains("/20261005/")),
        "{:?}",
        catalog.paths()
    );
}
