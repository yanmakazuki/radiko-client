//! Isolated signal handling regression; no account, network, terminal, or audio.
#![cfg(unix)]
use std::{
    io::{BufRead, BufReader, Read, Write},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

struct Reap(Child);
impl Drop for Reap {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn termination_signals_reach_the_registered_shutdown_handler() {
    const ROLE: &str = "RADIKO_TEST_SIGNAL_CHILD";
    if std::env::var_os(ROLE).is_some() {
        let shutdown = Arc::new(AtomicBool::new(false));
        let signal = shutdown.clone();
        // This is the ctrlc registration used by the TUI and playback/probe CLIs.
        ctrlc::set_handler(move || signal.store(true, Ordering::Release)).unwrap();
        println!("SIGNAL_READY");
        std::io::stdout().flush().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !shutdown.load(Ordering::Acquire) {
            assert!(
                Instant::now() < deadline,
                "signal was not delivered to the handler"
            );
            thread::sleep(Duration::from_millis(1));
        }
        println!("SIGNAL_HANDLED");
        return;
    }
    for signal in ["TERM", "HUP", "INT"] {
        let mut child = Reap(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "termination_signals_reach_the_registered_shutdown_handler",
                    "--nocapture",
                ])
                .env(ROLE, "1")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let mut reader = BufReader::new(child.0.stdout.take().unwrap());
        let mut ready = false;
        let mut line = String::new();
        while reader.read_line(&mut line).unwrap() > 0 {
            if line.trim() == "SIGNAL_READY" {
                ready = true;
                break;
            }
            line.clear();
        }
        assert!(ready, "isolated handler did not become ready");
        assert!(
            Command::new("/bin/kill")
                .args([&format!("-{signal}"), &child.0.id().to_string()])
                .status()
                .unwrap()
                .success()
        );
        let deadline = Instant::now() + Duration::from_secs(6);
        let status = loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                break status;
            }
            assert!(Instant::now() < deadline, "handler child did not exit");
            thread::sleep(Duration::from_millis(1));
        };
        let mut output = String::new();
        reader.read_to_string(&mut output).unwrap();
        assert!(
            status.success(),
            "{signal} bypassed the shutdown handler: {status}"
        );
        assert!(
            output.contains("SIGNAL_HANDLED"),
            "{signal} was not observed by the handler"
        );
    }
}
