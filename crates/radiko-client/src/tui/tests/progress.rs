use super::*;
use radiko::api::stream::ProbeMode;
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

fn slot() -> (DateTime<FixedOffset>, DateTime<FixedOffset>) {
    let start = DateTime::parse_from_rfc3339("2026-10-15T03:00:00+09:00").unwrap();
    (start, start + chrono::Duration::seconds(3300))
}

fn snapshot(
    mode: ProbeMode,
    state: PlaybackState,
    position: Option<Duration>,
    total_seconds: i64,
) -> radiko::player::state::PlaybackSnapshot {
    let start = DateTime::parse_from_rfc3339("2026-10-15T03:00:00+09:00").unwrap();
    let end = start + chrono::Duration::seconds(total_seconds);
    radiko::player::state::PlaybackSnapshot {
        info: radiko::api::stream::playback::StreamInfo {
            title: Some("朝の番組".into()),
            station: "TBS".into(),
            mode,
            starts_at: start,
            ends_at: end,
            position: Duration::ZERO,
        },
        state,
        pending: None,
        volume: 25,
        position,
        output: None,
        segments: 1,
        pipeline: None,
        receiving_for: None,
        reconnecting_for: None,
    }
}

fn attach(screen: &mut Screen, playback: radiko::player::state::PlaybackSnapshot) {
    screen.playback_notice = "NOTICE_KEEP".into();
    screen.app.playback = Some(playback);
}

fn buffer(screen: &mut Screen, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| draw(frame, screen, slot().0))
        .unwrap();
    terminal.backend().buffer().clone()
}

fn text(buffer: &Buffer) -> String {
    let mut text = String::new();
    for y in 0..buffer.area.height {
        let mut x = 0;
        while x < buffer.area.width {
            let symbol = buffer[(x, y)].symbol();
            text.push_str(symbol);
            x += unicode_width::UnicodeWidthStr::width(symbol).max(1) as u16;
        }
        text.push('\n');
    }
    text
}

fn counts(buffer: &Buffer) -> (usize, usize) {
    let mut filled = 0;
    let mut empty = 0;
    for cell in &buffer.content {
        match cell.symbol() {
            super::super::playback_view::FILLED => filled += 1,
            super::super::playback_view::UNFILLED => empty += 1,
            _ => {}
        }
    }
    (filled, empty)
}

fn assert_inside(buffer: &Buffer) {
    let width = buffer.area.width;
    let height = buffer.area.height;
    if width < 2 || height < 2 {
        return;
    }
    let filled = super::super::playback_view::FILLED;
    let empty = super::super::playback_view::UNFILLED;
    if width >= 20 && height >= 16 {
        assert_eq!(buffer[(0, height - 1)].symbol(), "└", "bottom-left");
        assert_eq!(
            buffer[(width - 1, height - 1)].symbol(),
            "┘",
            "bottom-right"
        );
    }
    for y in 0..height {
        for x in [0, width - 1] {
            let symbol = buffer[(x, y)].symbol();
            assert_ne!(symbol, filled, "left/right {x},{y}");
            assert_ne!(symbol, empty, "left/right {x},{y}");
        }
    }
    for x in 0..width {
        let symbol = buffer[(x, height - 1)].symbol();
        assert_ne!(symbol, filled, "bottom {x}");
        assert_ne!(symbol, empty, "bottom {x}");
    }
}

fn assert_no_debug(body: &str) {
    for word in ["TimeFree", "Live", "Playing", "Paused", "Buffering"] {
        assert!(!body.contains(word), "{word} leaked into\n{body}");
    }
}

#[test]
fn player_and_help_cells_use_the_same_japanese_labels_for_every_mode_and_state() {
    let modes = [
        (ProbeMode::Live, "ライブ"),
        (ProbeMode::TimeFree, "タイムフリー"),
    ];
    let states = [
        (PlaybackState::Playing, "再生中"),
        (PlaybackState::Paused, "一時停止"),
        (PlaybackState::Buffering, "バッファ中"),
    ];
    for (mode, mode_label) in modes {
        for (state, state_label) in states {
            let playback = snapshot(mode, state, Some(Duration::from_secs(754)), 3300);
            let mut screen = screen();
            attach(&mut screen, playback);
            for (width, height) in [(80u16, 24u16), (160, 40)] {
                let shown = buffer(&mut screen, width, height);
                let body = text(&shown);
                assert_inside(&shown);
                assert!(
                    body.contains(&format!("[TBS / {mode_label}]")),
                    "{width}x{height} {mode_label}\n{body}"
                );
                assert!(
                    body.contains(&format!(" / {state_label} / 音量25% / ")),
                    "{width}x{height} {state_label}\n{body}"
                );
                assert_no_debug(&body);
                assert!(body.contains("NOTICE_KEEP"), "{width}x{height}\n{body}");
                assert!(body.contains("? 操作一覧"), "{width}x{height}\n{body}");
            }
            screen.help_open = true;
            for (width, height) in [(80u16, 24u16), (160, 40)] {
                let shown = buffer(&mut screen, width, height);
                let body = text(&shown);
                assert_inside(&shown);
                let compact = body.replace(' ', "");
                assert!(
                    compact.contains(&format!("再生中:[TBS/{mode_label}]{state_label}/音量25%/")),
                    "{width}x{height} help {mode:?} {state:?}\n{body}"
                );
                assert_no_debug(&body);
                assert!(
                    body.contains("? 操作一覧") || body.contains("番組を探す"),
                    "{body}"
                );
            }
            screen.help_open = false;
            screen.failures_open = true;
            let failures = text(&buffer(&mut screen, 80, 24));
            assert!(
                failures.contains(&format!("再生中: TBS / {state_label} / 音量25%")),
                "{failures}"
            );
            assert_no_debug(&failures);
            screen.failures_open = false;
        }
    }
}

#[test]
fn timefree_cells_show_754_over_3300_as_12_34_of_55_00_at_start_middle_and_end() {
    let cases = [
        (Duration::ZERO, "00:00 / 55:00", 0.0),
        (
            Duration::new(754, 900_000_000),
            "12:34 / 55:00",
            754.9 / 3300.0,
        ),
        (Duration::from_secs(3300), "55:00 / 55:00", 1.0),
    ];
    for (position, clock, ratio) in cases {
        let stored = position;
        let mut screen = screen();
        attach(
            &mut screen,
            snapshot(
                ProbeMode::TimeFree,
                PlaybackState::Playing,
                Some(position),
                3300,
            ),
        );
        for (width, height) in [(80u16, 24u16), (160, 40)] {
            let shown = buffer(&mut screen, width, height);
            let body = text(&shown);
            assert!(body.contains(clock), "{width}x{height} {clock}\n{body}");
            assert!(!body.contains("位置754"), "{body}");
            assert!(!body.contains(".0秒"), "{body}");
            assert!(!body.replace("音量25%", "").contains('%'), "{body}");
            let (filled, empty) = counts(&shown);
            let bar = filled + empty;
            assert!(bar > 0, "{width}x{height} missing bar for {clock}\n{body}");
            assert_eq!(
                filled,
                (bar as f64 * ratio).floor() as usize,
                "{width}x{height} {clock} filled {filled} empty {empty}"
            );
            assert_inside(&shown);
            assert!(body.contains("NOTICE_KEEP"), "{body}");
            assert!(body.contains("? 操作一覧"), "{body}");
        }
        screen.help_open = true;
        let help = buffer(&mut screen, 80, 24);
        let help_text = text(&help);
        assert!(help_text.contains(clock), "{help_text}");
        let (filled, empty) = counts(&help);
        let bar = filled + empty;
        assert!(bar > 0, "help missing bar\n{help_text}");
        assert_eq!(filled, (bar as f64 * ratio).floor() as usize);
        assert_eq!(screen.app.playback.as_ref().unwrap().position, Some(stored));
        screen.help_open = false;
    }
}

#[test]
fn unknown_position_zero_total_and_live_draw_no_program_bar() {
    let mut unknown = screen();
    attach(
        &mut unknown,
        snapshot(ProbeMode::TimeFree, PlaybackState::Playing, None, 3300),
    );
    let shown = buffer(&mut unknown, 80, 24);
    let body = text(&shown);
    assert!(body.contains("番組内位置未確認"), "{body}");
    assert_eq!(counts(&shown), (0, 0), "{body}");
    assert!(!body.replace("音量25%", "").contains('%'), "{body}");
    assert!(!body.contains("00:00"), "{body}");

    let mut zero = screen();
    attach(
        &mut zero,
        snapshot(
            ProbeMode::TimeFree,
            PlaybackState::Paused,
            Some(Duration::from_secs(754)),
            0,
        ),
    );
    let shown = buffer(&mut zero, 160, 40);
    let body = text(&shown);
    assert!(body.contains("12:34 / --:--"), "{body}");
    assert!(body.contains("一時停止"), "{body}");
    assert_eq!(counts(&shown), (0, 0), "{body}");

    let mut backwards = screen();
    attach(
        &mut backwards,
        snapshot(
            ProbeMode::TimeFree,
            PlaybackState::Playing,
            Some(Duration::from_secs(754)),
            -60,
        ),
    );
    let shown = buffer(&mut backwards, 80, 24);
    assert!(text(&shown).contains("12:34 / --:--"));
    assert_eq!(counts(&shown), (0, 0));

    let mut live = screen();
    attach(
        &mut live,
        snapshot(
            ProbeMode::Live,
            PlaybackState::Playing,
            Some(Duration::from_secs(754)),
            3300,
        ),
    );
    let shown = buffer(&mut live, 80, 24);
    let body = text(&shown);
    assert!(body.contains("[TBS / ライブ]"), "{body}");
    assert!(body.contains("12:34 / --:--"), "{body}");
    assert!(!body.contains("55:00"), "{body}");
    assert_eq!(counts(&shown), (0, 0), "{body}");
    assert_no_debug(&body);

    live.help_open = true;
    let help = text(&buffer(&mut live, 80, 24));
    assert!(help.contains("12:34 / --:--"), "{help}");
    assert!(help.contains("[TBS / ライブ]"), "{help}");
    assert!(!help.contains("55:00"), "{help}");

    for (mut screen, clock) in [
        (unknown, "番組内位置未確認"),
        (zero, "12:34 / --:--"),
        (backwards, "12:34 / --:--"),
        (live, "12:34 / --:--"),
    ] {
        screen.help_open = false;
        let shown = buffer(&mut screen, 60, 24);
        let row = detail_row(&shown);
        if row.contains(clock) {
            assert!(row.contains("再生中") || row.contains("一時停止"), "{row}");
        } else {
            assert_no_clock_fragment(&row, clock);
        }
        assert_eq!(counts(&shown), (0, 0), "{row}");
        screen.help_open = true;
        let help_line = help_row(&buffer(&mut screen, 60, 24));
        if help_line.contains(clock) {
            assert!(help_line.contains("[TBS /"), "{help_line}");
        } else {
            assert_no_clock_fragment(&help_line, clock);
        }
        assert!(
            !help_line.contains("55:00") || clock.contains("55:00"),
            "{help_line}"
        );
    }
}

#[test]
fn out_of_range_position_clamps_the_bar_and_keeps_the_clock() {
    let mut screen = screen();
    let position = Duration::from_secs(4000);
    attach(
        &mut screen,
        snapshot(
            ProbeMode::TimeFree,
            PlaybackState::Playing,
            Some(position),
            3300,
        ),
    );
    let shown = buffer(&mut screen, 80, 24);
    let body = text(&shown);
    assert!(body.contains("66:40 / 55:00"), "{body}");
    let (filled, empty) = counts(&shown);
    assert!(filled > 0, "{body}");
    assert_eq!(empty, 0, "{body}");
    assert_eq!(
        screen.app.playback.as_ref().unwrap().position,
        Some(position)
    );
}

#[test]
fn narrow_resize_and_tiny_terminals_keep_borders_notices_and_the_clock() {
    let mut screen = screen();
    attach(
        &mut screen,
        snapshot(
            ProbeMode::TimeFree,
            PlaybackState::Buffering,
            Some(Duration::from_secs(754)),
            3300,
        ),
    );
    let wide = buffer(&mut screen, 160, 40);
    assert!(text(&wide).contains("12:34 / 55:00"));
    assert!(text(&wide).contains("バッファ中"));
    assert!(counts(&wide).0 + counts(&wide).1 > 0);
    assert!(text(&wide).contains("NOTICE_KEEP"));
    assert_inside(&wide);

    let normal = buffer(&mut screen, 80, 24);
    assert!(text(&normal).contains("12:34 / 55:00"));
    assert!(text(&normal).contains("NOTICE_KEEP"));
    assert!(text(&normal).contains("? 操作一覧"));
    assert!(counts(&normal).0 + counts(&normal).1 > 0);
    assert_inside(&normal);

    let narrow = buffer(&mut screen, 36, 24);
    let narrow_text = text(&narrow);
    assert_inside(&narrow);
    assert!(narrow_text.contains("NOTICE_KEEP"), "{narrow_text}");
    assert!(
        counts(&narrow).0 + counts(&narrow).1 == 0,
        "a 36-column row has no room beside the clock\n{narrow_text}"
    );

    for (width, height) in [(0u16, 0u16), (1, 1), (2, 2), (8, 4), (12, 6)] {
        let tiny = buffer(&mut screen, width, height);
        assert_inside(&tiny);
    }

    screen.help_open = true;
    let help = buffer(&mut screen, 80, 24);
    assert!(text(&help).contains("12:34 / 55:00"));
    assert!(text(&help).contains("バッファ中"));
    assert_inside(&help);
    for (width, height) in [(0u16, 0u16), (1, 1), (2, 2), (10, 5)] {
        let tiny = buffer(&mut screen, width, height);
        assert_inside(&tiny);
    }
    screen.help_open = false;

    let again = buffer(&mut screen, 160, 40);
    assert!(text(&again).contains("12:34 / 55:00"));
    assert!(text(&again).contains("NOTICE_KEEP"));
    assert!(text(&again).contains("? 操作一覧"));
    assert_eq!(
        screen.app.playback.as_ref().unwrap().position,
        Some(Duration::from_secs(754))
    );
    assert_eq!(
        screen.app.playback.as_ref().unwrap().info.position,
        Duration::ZERO
    );
}

const CLOCK: &str = "12:34 / 55:00";

fn rows(buffer: &Buffer) -> Vec<String> {
    let mut rows = Vec::new();
    for y in 0..buffer.area.height {
        let mut line = String::new();
        let mut x = 0;
        while x < buffer.area.width {
            let symbol = buffer[(x, y)].symbol();
            line.push_str(symbol);
            x += unicode_width::UnicodeWidthStr::width(symbol).max(1) as u16;
        }
        rows.push(line.trim_end().to_owned());
    }
    rows
}

fn detail_row(buffer: &Buffer) -> String {
    rows(buffer)
        .into_iter()
        .find(|row| row.contains("JST /"))
        .unwrap_or_default()
}

fn help_row(buffer: &Buffer) -> String {
    rows(buffer)
        .into_iter()
        .find(|row| row.contains("再生中: ["))
        .unwrap_or_default()
}

fn assert_no_clock_fragment(row: &str, clock: &str) {
    assert!(!row.contains(clock), "full clock still present:\n{row}");
    let mut prefix = String::new();
    for ch in clock.chars() {
        prefix.push(ch);
        if prefix.chars().count() >= 4 {
            assert!(!row.contains(&prefix), "partial clock {prefix:?} in\n{row}");
        }
    }
}

fn whole_clock(row: &str, clock: &str, state: &str) {
    assert!(row.contains(state), "{state} missing:\n{row}");
    assert!(row.contains(clock), "clock missing:\n{row}");
}

#[test]
fn narrow_player_and_help_keep_state_and_a_whole_clock() {
    for state in [
        PlaybackState::Playing,
        PlaybackState::Paused,
        PlaybackState::Buffering,
    ] {
        let mut screen = screen();
        attach(
            &mut screen,
            snapshot(
                ProbeMode::TimeFree,
                state,
                Some(Duration::from_secs(754)),
                3300,
            ),
        );
        let state_label = match state {
            PlaybackState::Playing => "再生中",
            PlaybackState::Paused => "一時停止",
            PlaybackState::Buffering => "バッファ中",
        };
        for width in [60u16, 64] {
            let shown = buffer(&mut screen, width, 24);
            let row = detail_row(&shown);
            whole_clock(&row, CLOCK, state_label);
            assert!(
                row.contains("[TBS / タイムフリー]")
                    || text(&shown).contains("[TBS / タイムフリー]")
            );
            assert_inside(&shown);
            screen.help_open = true;
            let help = buffer(&mut screen, width, 24);
            let help_line = help_row(&help);
            whole_clock(&help_line, CLOCK, state_label);
            assert!(help_line.contains("[TBS / タイムフリー]"), "{help_line}");
            assert_inside(&help);
            screen.help_open = false;
        }
        for (width, height) in [(80u16, 24u16), (160, 40)] {
            let shown = buffer(&mut screen, width, height);
            let row = detail_row(&shown);
            whole_clock(&row, CLOCK, state_label);
            assert!(row.contains("音量25%"), "wide player keeps volume:\n{row}");
            screen.help_open = true;
            let help_line = help_row(&buffer(&mut screen, width, height));
            whole_clock(&help_line, CLOCK, state_label);
            assert!(
                help_line.contains("音量25%"),
                "wide help keeps volume:\n{help_line}"
            );
            screen.help_open = false;
        }
        let wide = buffer(&mut screen, 160, 40);
        assert!(detail_row(&wide).contains(CLOCK));
        let _ = buffer(&mut screen, 80, 24);
        let _ = buffer(&mut screen, 64, 24);
        let again = buffer(&mut screen, 60, 24);
        whole_clock(&detail_row(&again), CLOCK, state_label);
        let restored = buffer(&mut screen, 160, 40);
        whole_clock(&detail_row(&restored), CLOCK, state_label);
        assert!(detail_row(&restored).contains("音量25%"));
        assert!(text(&restored).contains("NOTICE_KEEP"));
        assert!(text(&restored).contains("? 操作一覧"));
    }
}

#[test]
fn clock_fits_at_one_width_and_is_absent_one_column_below() {
    let mut player = screen();
    attach(
        &mut player,
        snapshot(
            ProbeMode::TimeFree,
            PlaybackState::Buffering,
            Some(Duration::from_secs(754)),
            3300,
        ),
    );
    let player_fit = (20u16..90)
        .find(|width| detail_row(&buffer(&mut player, *width, 24)).contains(CLOCK))
        .expect("player clock never fit");
    let at = detail_row(&buffer(&mut player, player_fit, 24));
    whole_clock(&at, CLOCK, "バッファ中");
    let below = detail_row(&buffer(&mut player, player_fit - 1, 24));
    assert!(
        below.contains("バッファ中") || below.contains("JST"),
        "{below}"
    );
    assert_no_clock_fragment(&below, CLOCK);
    assert_inside(&buffer(&mut player, player_fit, 24));
    assert_inside(&buffer(&mut player, player_fit - 1, 24));

    player.help_open = true;
    let help_fit = (20u16..90)
        .find(|width| help_row(&buffer(&mut player, *width, 24)).contains(CLOCK))
        .expect("help clock never fit");
    let help_at = help_row(&buffer(&mut player, help_fit, 24));
    whole_clock(&help_at, CLOCK, "バッファ中");
    assert!(help_at.contains("[TBS / タイムフリー]"), "{help_at}");
    let help_below = help_row(&buffer(&mut player, help_fit - 1, 24));
    assert!(help_below.contains("バッファ中"), "{help_below}");
    assert!(help_below.contains("[TBS / タイムフリー]"), "{help_below}");
    assert_no_clock_fragment(&help_below, CLOCK);
    assert_inside(&buffer(&mut player, help_fit, 24));
    assert_inside(&buffer(&mut player, help_fit - 1, 24));
}
