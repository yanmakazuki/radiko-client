use super::super::device::{self, DeviceFailure, DeviceProbe, DeviceReport, DeviceStatus};
use super::*;
use radiko::player::output::{OutputError, OutputSampleFormat, OutputSummary};
use ratatui::{
    style::{Color, Modifier},
    text::Span,
};
use std::{
    sync::{Arc, Condvar, Mutex},
    thread::{self, ThreadId},
    time::{Duration, Instant},
};

fn search() -> Service {
    Service::new(|| -> Result<CatalogClient, _> { panic!("the device panel must not fetch") })
}

fn press(screen: &mut Screen, search: &mut Service, code: KeyCode) -> bool {
    screen
        .key(KeyEvent::new(code, KeyModifiers::NONE), search)
        .unwrap()
}

fn rows(screen: &mut Screen, width: u16, height: u16) -> Vec<String> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| draw(frame, screen, Utc::now().fixed_offset()))
        .unwrap();
    let columns = usize::from(terminal.backend().buffer().area.width);
    terminal
        .backend()
        .buffer()
        .content
        .chunks(columns)
        .map(|row| {
            let mut line = String::new();
            let mut x = 0;
            while x < row.len() {
                let symbol = row[x].symbol();
                line.push_str(symbol);
                x += Span::raw(symbol).width().max(1);
            }
            let line = line.strip_prefix('│').unwrap_or(&line);
            let line = line.strip_suffix('│').unwrap_or(line);
            line.trim_end().to_owned()
        })
        .collect()
}

fn row_style(screen: &mut Screen, width: u16, height: u16, starts: &str) -> (String, Color, bool) {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| draw(frame, screen, Utc::now().fixed_offset()))
        .unwrap();
    let columns = usize::from(terminal.backend().buffer().area.width);
    for row in terminal.backend().buffer().content.chunks(columns) {
        let mut line = String::new();
        let mut x = 0;
        while x < row.len() {
            let symbol = row[x].symbol();
            line.push_str(symbol);
            x += Span::raw(symbol).width().max(1);
        }
        let text = line.strip_prefix('│').unwrap_or(&line);
        let text = text.strip_suffix('│').unwrap_or(text).trim_end().to_owned();
        if text.contains(starts) {
            let cell = row.iter().find(|cell| cell.symbol() != "│").unwrap();
            return (text, cell.fg, cell.modifier.contains(Modifier::BOLD));
        }
    }
    panic!("row containing {starts} was not drawn");
}

fn program() -> radiko::domain::Program {
    radiko::api::catalog::parse_programs(
        include_bytes!("../../../../../tests/fixtures/schedule.xml"),
        "TBS",
        radiko::api::catalog::parse_day("20261015").unwrap(),
    )
    .unwrap()
    .programs
    .remove(0)
}

fn report(rate: u32) -> DeviceReport {
    DeviceReport {
        host: "ALSA".into(),
        name: "MockSpeaker".into(),
        sample_rate: rate,
        f32_mono: true,
        f32_stereo: false,
    }
}

struct FixedProbe {
    answer: Mutex<Result<DeviceReport, OutputError>>,
    calls: Mutex<Vec<(ThreadId, u32)>>,
}

impl DeviceProbe for FixedProbe {
    fn inquire(&self, sample_rate: u32) -> Result<DeviceReport, OutputError> {
        self.calls
            .lock()
            .unwrap()
            .push((thread::current().id(), sample_rate));
        let mut answer = self.answer.lock().unwrap().clone();
        if let Ok(found) = &mut answer {
            found.sample_rate = sample_rate;
        }
        answer
    }
}

struct GatedProbe {
    ui: ThreadId,
    calls: Mutex<Vec<(ThreadId, u32)>>,
    gate: Arc<(Mutex<bool>, Condvar)>,
    answer: Mutex<Result<DeviceReport, OutputError>>,
}

impl DeviceProbe for GatedProbe {
    fn inquire(&self, sample_rate: u32) -> Result<DeviceReport, OutputError> {
        let id = thread::current().id();
        self.calls.lock().unwrap().push((id, sample_rate));
        if id != self.ui {
            let (lock, cv) = &*self.gate;
            let mut open = lock.lock().unwrap();
            let started = Instant::now();
            while !*open && started.elapsed() < Duration::from_secs(2) {
                let (guard, wait) = cv.wait_timeout(open, Duration::from_millis(50)).unwrap();
                open = guard;
                if wait.timed_out() && started.elapsed() >= Duration::from_secs(2) {
                    break;
                }
            }
        }
        self.answer.lock().unwrap().clone()
    }
}

fn settle(screen: &mut Screen) {
    let started = Instant::now();
    while matches!(screen.device_status, DeviceStatus::Loading) {
        screen.poll_device();
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the device result did not arrive"
        );
        if matches!(screen.device_status, DeviceStatus::Loading) {
            thread::sleep(Duration::from_millis(5));
        }
    }
}

#[test]
fn d_on_the_normal_screen_opens_a_panel_and_q_esc_or_d_only_closes_it() {
    let probe = Arc::new(FixedProbe {
        answer: Mutex::new(Ok(report(48_000))),
        calls: Mutex::new(vec![]),
    });
    let mut search = search();
    for live in [false, true] {
        let mut screen = screen();
        screen.live_mode = live;
        screen.set_device_probe(Arc::clone(&probe) as Arc<dyn DeviceProbe>);
        assert!(
            !press(&mut screen, &mut search, KeyCode::Char('d')),
            "d must not quit"
        );
        assert!(screen.device_open, "d opens the panel, live={live}");
        assert!(matches!(screen.device_status, DeviceStatus::Loading));
        for (code, label) in [
            (KeyCode::Char('q'), "q"),
            (KeyCode::Esc, "esc"),
            (KeyCode::Char('d'), "d"),
        ] {
            if !screen.device_open {
                assert!(!press(&mut screen, &mut search, KeyCode::Char('d')));
            }
            assert!(screen.device_open, "{label}");
            let quit = press(&mut screen, &mut search, code);
            assert!(!quit, "{label} must only close the panel");
            assert!(!screen.device_open, "{label} closes the panel");
        }
        assert!(
            press(&mut screen, &mut search, KeyCode::Char('q')),
            "q on the normal screen still quits"
        );
    }

    let mut typing = screen();
    typing.set_device_probe(Arc::clone(&probe) as Arc<dyn DeviceProbe>);
    assert!(!press(&mut typing, &mut search, KeyCode::Char('/')));
    assert!(!press(&mut typing, &mut search, KeyCode::Char('d')));
    assert_eq!(typing.input.as_deref(), Some("d"));
    assert!(!typing.device_open, "d in the keyword field is a character");

    let mut filters = screen();
    filters.set_device_probe(probe);
    assert!(!press(&mut filters, &mut search, KeyCode::Char('f')));
    for _ in 0..5 {
        assert!(!press(&mut filters, &mut search, KeyCode::Tab));
    }
    assert!(!press(&mut filters, &mut search, KeyCode::Char('d')));
    assert_eq!(
        filters.app.draft.as_ref().unwrap().filter().keyword(),
        Some("d")
    );
    assert!(!filters.device_open, "d in the search form is a character");
}

#[test]
fn panel_text_is_literal_at_80x24_and_160x40() {
    let mut screen = screen();
    screen.app.playback = Some(playback_at(44_100));
    screen.apply_playback(radiko::app::playback::Update {
        output_failure: Some(radiko::app::playback::OutputFailure::NoDevice),
        stopped: true,
        ended: true,
        notice: Some("再生停止: 既定の音声出力機器がありません".into()),
        ..radiko::app::playback::Update::default()
    });
    // The failure above ends playback. Put the rate back for the query.
    screen.app.playback = Some(playback_at(44_100));
    screen.set_device_probe(Arc::new(FixedProbe {
        answer: Mutex::new(Ok(report(44_100))),
        calls: Mutex::new(vec![]),
    }));
    let mut search = search();
    assert!(!press(&mut screen, &mut search, KeyCode::Char('d')));
    settle(&mut screen);
    let expected = [
        "ホスト: ALSA",
        "機器: MockSpeaker",
        "f32 mono 44100Hz: 対応",
        "f32 stereo 44100Hz: 非対応",
        "最後の出力エラー: 既定の音声出力機器がありません",
        "出力機器は OS の既定に従います。アプリ内では選べません",
    ];
    for (width, height) in [(80, 24), (160, 40)] {
        let lines = rows(&mut screen, width, height);
        for line in expected {
            assert!(
                lines.iter().any(|row| row.contains(line)),
                "{width}x{height} missing {line:?}\n{}",
                lines.join("\n")
            );
        }
    }
}

#[test]
fn long_device_names_stay_on_one_line_and_tiny_terminals_do_not_panic() {
    let mut screen = screen();
    let name = "N".repeat(200);
    screen.set_device_probe(Arc::new(FixedProbe {
        answer: Mutex::new(Ok(DeviceReport {
            host: "CoreAudio".into(),
            name: name.clone(),
            sample_rate: 48_000,
            f32_mono: false,
            f32_stereo: true,
        })),
        calls: Mutex::new(vec![]),
    }));
    let mut search = search();
    assert!(!press(&mut screen, &mut search, KeyCode::Char('d')));
    settle(&mut screen);
    let lines = rows(&mut screen, 80, 24);
    let device_lines: Vec<_> = lines.iter().filter(|line| line.contains("機器:")).collect();
    assert_eq!(device_lines.len(), 1, "{lines:?}");
    assert!(device_lines[0].contains('…'), "{}", device_lines[0]);
    assert!(!lines.join("").contains(&name), "the long name must be cut");
    assert!(Span::raw(device_lines[0].as_str()).width() <= 78);
    assert!(lines.iter().any(|line| line.contains("ホスト: CoreAudio")));
    for (width, height) in [(0, 0), (1, 1), (2, 2), (10, 3)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| draw(frame, &mut screen, Utc::now().fixed_offset()))
            .unwrap();
    }
    screen.device_status = DeviceStatus::Loading;
    for (width, height) in [(0, 0), (1, 1)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| draw(frame, &mut screen, Utc::now().fixed_offset()))
            .unwrap();
    }
    screen.device_status = DeviceStatus::Failed(DeviceFailure::NoDevice);
    let mut terminal = Terminal::new(TestBackend::new(1, 1)).unwrap();
    terminal
        .draw(|frame| draw(frame, &mut screen, Utc::now().fixed_offset()))
        .unwrap();
}

#[test]
fn nodevice_and_mock_device_info_are_literal_in_the_panel_and_the_help_line() {
    let mut search = search();
    let mut info = screen();
    info.last_output_error = Some("既定の音声出力機器がありません".into());
    info.set_device_probe(Arc::new(FixedProbe {
        answer: Mutex::new(Ok(report(48_000))),
        calls: Mutex::new(vec![]),
    }));
    assert!(!press(&mut info, &mut search, KeyCode::Char('d')));
    settle(&mut info);
    let panel = rows(&mut info, 80, 24).join("\n");
    assert!(panel.contains("ホスト: ALSA"), "{panel}");
    assert!(panel.contains("機器: MockSpeaker"), "{panel}");
    assert!(panel.contains("f32 mono 48000Hz: 対応"), "{panel}");
    assert!(panel.contains("f32 stereo 48000Hz: 非対応"), "{panel}");
    assert!(panel.contains("既定の音声出力機器がありません"), "{panel}");
    assert!(!press(&mut info, &mut search, KeyCode::Char('q')));
    assert!(!press(&mut info, &mut search, KeyCode::Char('?')));
    let help = rows(&mut info, 160, 40);
    let summary = help
        .iter()
        .find(|line| line.contains("MockSpeaker"))
        .expect("help summary");
    assert_eq!(
        summary, "ALSA / MockSpeaker / 既定の音声出力機器がありません",
        "{help:?}"
    );
    assert!(summary.contains("既定の音声出力機器がありません"));

    let mut missing = screen();
    missing.set_device_probe(Arc::new(FixedProbe {
        answer: Mutex::new(Err(OutputError::NoDevice)),
        calls: Mutex::new(vec![]),
    }));
    assert!(!press(&mut missing, &mut search, KeyCode::Char('d')));
    settle(&mut missing);
    let panel = rows(&mut missing, 80, 24);
    assert!(
        panel
            .iter()
            .any(|line| line.contains("✕ 既定の音声出力機器がありません")),
        "{panel:?}"
    );
    assert!(!press(&mut missing, &mut search, KeyCode::Esc));
    assert!(!press(&mut missing, &mut search, KeyCode::Char('?')));
    let help = rows(&mut missing, 80, 24);
    assert!(
        help.iter()
            .any(|line| line.contains("✕ 既定の音声出力機器がありません")),
        "{help:?}"
    );
}

#[test]
fn nodevice_and_a_failed_query_use_different_error_sentences() {
    let mut search = search();
    let mut missing = screen();
    missing.set_device_probe(Arc::new(FixedProbe {
        answer: Mutex::new(Err(OutputError::NoDevice)),
        calls: Mutex::new(vec![]),
    }));
    assert!(!press(&mut missing, &mut search, KeyCode::Char('d')));
    settle(&mut missing);
    let lines = rows(&mut missing, 80, 24);
    assert!(
        lines
            .iter()
            .any(|line| line == "✕ 既定の音声出力機器がありません"),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .all(|line| !line.contains("出力機器を確認できません。")),
        "{lines:?}"
    );
    assert!(
        lines.iter().any(|line| {
            line.contains("OSのサウンド設定を確認し、d で閉じてから Enter で再生し直してください")
        }),
        "{lines:?}"
    );
    let (text, fg, bold) = row_style(&mut missing, 80, 24, "✕");
    assert!(text.contains("既定の音声出力機器がありません"), "{text}");
    assert_eq!(fg, Color::Red);
    assert!(bold, "NoDevice is an error");

    let detail = OutputError::DeviceInfo.to_string();
    let mut failed = screen();
    failed.set_device_probe(Arc::new(FixedProbe {
        answer: Mutex::new(Err(OutputError::DeviceInfo)),
        calls: Mutex::new(vec![]),
    }));
    assert!(!press(&mut failed, &mut search, KeyCode::Char('d')));
    settle(&mut failed);
    let sentence = format!("出力機器を確認できません。{detail}");
    let lines = rows(&mut failed, 160, 40);
    assert!(
        lines.iter().any(|line| line == &format!("✕ {sentence}")),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .all(|line| line != "✕ 既定の音声出力機器がありません"),
        "{lines:?}"
    );
    assert!(lines.iter().any(|line| line.contains(&detail)), "{lines:?}");
    assert!(
        lines.iter().any(|line| {
            line.contains("OSのサウンド設定を確認し、d で閉じてから Enter で再生し直してください")
        }),
        "{lines:?}"
    );
    let (text, fg, bold) = row_style(&mut failed, 160, 40, "✕");
    assert!(text.contains(&sentence), "{text}");
    assert_eq!(fg, Color::Red);
    assert!(bold, "a failed query is an error");
    assert_ne!(sentence, "既定の音声出力機器がありません");
}

#[test]
fn the_probe_runs_off_the_ui_thread_and_loading_shows_before_the_result() {
    let ui = thread::current().id();
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let probe = Arc::new(GatedProbe {
        ui,
        calls: Mutex::new(vec![]),
        gate: Arc::clone(&gate),
        answer: Mutex::new(Ok(report(48_000))),
    });
    let mut screen = screen();
    screen.set_device_probe(Arc::clone(&probe) as Arc<dyn DeviceProbe>);
    let mut search = search();
    let started = Instant::now();
    assert!(!press(&mut screen, &mut search, KeyCode::Char('d')));
    assert!(
        started.elapsed() < Duration::from_millis(1_000),
        "opening the panel waited for the output query"
    );
    assert!(matches!(screen.device_status, DeviceStatus::Loading));
    let loading = rows(&mut screen, 80, 24);
    assert!(
        loading.iter().any(|line| line.contains("取得中…")),
        "{loading:?}"
    );
    let calls_ready = Instant::now();
    while probe.calls.lock().unwrap().is_empty() {
        assert!(
            calls_ready.elapsed() < Duration::from_secs(2),
            "the probe was never called"
        );
        thread::sleep(Duration::from_millis(10));
    }
    for code in [
        KeyCode::Enter,
        KeyCode::Down,
        KeyCode::Up,
        KeyCode::Left,
        KeyCode::Right,
    ] {
        assert!(!press(&mut screen, &mut search, code));
    }
    let again = rows(&mut screen, 80, 24);
    assert!(
        again.iter().any(|line| line.contains("取得中…")),
        "{again:?}"
    );
    let calls = probe.calls.lock().unwrap().clone();
    assert!(!calls.is_empty());
    assert!(
        calls.iter().all(|(id, _)| *id != ui),
        "the output query ran on the UI thread: {calls:?}"
    );
    {
        let (lock, cv) = &*gate;
        *lock.lock().unwrap() = true;
        cv.notify_all();
    }
    settle(&mut screen);
    assert!(matches!(screen.device_status, DeviceStatus::Ready(_)));
    let calls = probe.calls.lock().unwrap().clone();
    assert!(calls.iter().all(|(id, _)| *id != ui));
}

#[test]
fn playback_and_search_continue_while_the_panel_is_open() {
    let mut warnings = super::screen();
    warnings.playback_notice = "テスト警告".into();
    warnings.notice_level = super::super::notices::NoticeLevel::Warning;
    warnings.notice_hold = super::NoticeHold::Warning;
    warnings.playback_seq = 4;
    warnings.set_device_probe(Arc::new(FixedProbe {
        answer: Mutex::new(Ok(report(48_000))),
        calls: Mutex::new(vec![]),
    }));
    let mut search = search();
    assert!(!press(&mut warnings, &mut search, KeyCode::Char('d')));
    assert!(warnings.device_open);
    assert_eq!(warnings.playback_notice, "テスト警告");
    assert_eq!(
        warnings.notice_level,
        super::super::notices::NoticeLevel::Warning
    );
    warnings.apply(radiko::app::search::Event::Progress {
        id: radiko::app::search::SearchId(0),
        progress: radiko::app::search::Progress {
            phase: "取得",
            target: "TBS".into(),
            day: None,
            completed: 1,
            total: 1,
            failed: 0,
            programs: vec![program()],
        },
    });
    assert_eq!(warnings.app.programs.len(), 1);
    assert!(warnings.device_open);
    let mut snapshot = playback_at(48_000);
    snapshot.info.title = Some("診断中も再生".into());
    warnings.apply_playback(radiko::app::playback::Update {
        snapshot: Some(snapshot),
        ..radiko::app::playback::Update::default()
    });
    assert_eq!(
        warnings
            .app
            .playback
            .as_ref()
            .and_then(|playback| playback.info.title.as_deref()),
        Some("診断中も再生")
    );
    assert_eq!(warnings.playback_notice, "テスト警告");
    assert!(!press(&mut warnings, &mut search, KeyCode::Char('q')));
    assert!(!warnings.device_open);
    let (text, fg, bold) = row_style(&mut warnings, 80, 24, "テスト警告");
    assert!(text.contains("! テスト警告"), "{text}");
    assert_eq!(fg, Color::Yellow);
    assert!(!bold);

    let mut errors = screen();
    errors.playback_notice = "テストエラー".into();
    errors.notice_level = super::super::notices::NoticeLevel::Error;
    errors.notice_hold = super::NoticeHold::Error;
    errors.playback_seq = 9;
    errors.set_device_probe(Arc::new(FixedProbe {
        answer: Mutex::new(Ok(report(48_000))),
        calls: Mutex::new(vec![]),
    }));
    assert!(!press(&mut errors, &mut search, KeyCode::Char('d')));
    assert_eq!(errors.playback_notice, "テストエラー");
    assert!(!press(&mut errors, &mut search, KeyCode::Esc));
    let (text, fg, bold) = row_style(&mut errors, 80, 24, "テストエラー");
    assert!(text.contains("✕ テストエラー"), "{text}");
    assert_eq!(fg, Color::Red);
    assert!(bold);
}

#[test]
fn enter_and_arrows_do_not_select_or_switch_devices() {
    let probe = Arc::new(FixedProbe {
        answer: Mutex::new(Ok(report(48_000))),
        calls: Mutex::new(vec![]),
    });
    let mut screen = screen();
    screen.volume = 40;
    screen.set_device_probe(Arc::clone(&probe) as Arc<dyn DeviceProbe>);
    let mut search = search();
    assert!(!press(&mut screen, &mut search, KeyCode::Char('d')));
    settle(&mut screen);
    let calls = probe.calls.lock().unwrap().len();
    for code in [
        KeyCode::Enter,
        KeyCode::Up,
        KeyCode::Down,
        KeyCode::Left,
        KeyCode::Right,
        KeyCode::Char('j'),
        KeyCode::Char('k'),
    ] {
        assert!(!press(&mut screen, &mut search, code));
        assert!(screen.device_open);
        assert!(screen.playback_action.is_none());
    }
    assert_eq!(probe.calls.lock().unwrap().len(), calls);
    assert_eq!(screen.volume, 40);
    let lines = rows(&mut screen, 80, 24);
    assert!(lines.iter().any(|line| line.contains("機器: MockSpeaker")));
    assert!(lines.iter().all(|line| !line.contains("選択中")));
    let design = include_str!("../../../../../docs/design.md");
    assert!(
        design.contains("読み取り専用の診断を全画面で出します"),
        "docs/design.md の出力機器に診断パネルを書く"
    );
    assert!(design.contains("機器の選択と切り替えはありません"));
}

fn playback_at(rate: u32) -> radiko::player::state::PlaybackSnapshot {
    let start = radiko::api::stream::parse_time("20261015120000").unwrap();
    radiko::player::state::PlaybackSnapshot {
        pipeline: None,
        info: radiko::api::stream::playback::StreamInfo {
            title: Some("合成テスト番組".into()),
            station: "TBS".into(),
            mode: radiko::api::stream::ProbeMode::TimeFree,
            starts_at: start,
            ends_at: start + chrono::Duration::hours(1),
            position: Duration::ZERO,
        },
        state: PlaybackState::Playing,
        pending: None,
        volume: 25,
        position: Some(Duration::from_secs(12)),
        output: Some(OutputSummary {
            frames: u64::from(rate),
            underrun_frames: 0,
            sample_rate: rate,
            channels: 2,
            sample_format: OutputSampleFormat::F32,
        }),
        segments: 1,
        receiving_for: None,
        reconnecting_for: None,
    }
}

#[cfg(target_os = "linux")]
#[test]
fn linux_device_worker_does_not_leak_alsa_text_and_shows_nodevice_without_panic() {
    if std::env::var_os("RADIKO_DEVICE_CHILD").is_some() {
        let probe = device::CpalProbe;
        let result = thread::spawn(move || probe.inquire(48_000))
            .join()
            .expect("device worker");
        match result {
            Err(OutputError::NoDevice) => {
                radiko::stderr::write_line(format_args!("DEVICE-RESULT NoDevice"))
            }
            Ok(found) => {
                radiko::stderr::write_line(format_args!("DEVICE-RESULT Ok {}", found.host))
            }
            Err(error) => radiko::stderr::write_line(format_args!("DEVICE-RESULT Err {error}")),
        }
        return;
    }
    let name = thread::current()
        .name()
        .expect("libtest names the test thread")
        .to_owned();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", &name, "--test-threads=1"])
        .env("RADIKO_DEVICE_CHILD", "1")
        .env_remove("RADIKO_ALSA_LOG")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "status {:?} stdout:\n{stdout}\nstderr:\n{stderr}",
        output.status.code()
    );
    assert!(
        stderr.contains("DEVICE-RESULT"),
        "worker produced no result:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(!stderr.contains("ALSA lib"), "{stderr}");
    assert!(!stderr.contains("jackd"), "{stderr}");
    assert!(!stderr.contains("Cannot connect to server"), "{stderr}");
    if stderr.contains("DEVICE-RESULT NoDevice") {
        let mut screen = screen();
        screen.device_open = true;
        screen.device_status = DeviceStatus::Failed(DeviceFailure::NoDevice);
        let lines = rows(&mut screen, 80, 24);
        assert!(
            lines
                .iter()
                .any(|line| line.contains("✕ 既定の音声出力機器がありません")),
            "{lines:?}"
        );
        assert!(!lines.join("\n").contains("ALSA lib"));
    }
}
