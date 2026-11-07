use super::*;
use radiko::domain::{Chapter, ChapterSet};
use ratatui::{
    Terminal,
    backend::TestBackend,
    buffer::Buffer,
    style::{Color, Modifier},
};
use std::time::Duration;
use unicode_width::UnicodeWidthStr;

fn now() -> DateTime<FixedOffset> {
    radiko::api::stream::parse_time("20261015060000").unwrap()
}

fn buffer(screen: &mut Screen, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| draw(frame, screen, now())).unwrap();
    terminal.backend().buffer().clone()
}

fn rows(buffer: &Buffer) -> Vec<String> {
    let mut rows = Vec::new();
    for y in 0..buffer.area.height {
        let mut line = String::new();
        let mut x = 0;
        while x < buffer.area.width {
            let symbol = buffer[(x, y)].symbol();
            line.push_str(symbol);
            x += UnicodeWidthStr::width(symbol).max(1) as u16;
        }
        let line = line.strip_prefix('│').unwrap_or(&line);
        let line = line.strip_suffix('│').unwrap_or(line);
        rows.push(line.trim_end().to_owned());
    }
    rows
}

fn plain(buffer: &Buffer) -> String {
    rows(buffer).join("\n")
}

fn list_title_y(buffer: &Buffer) -> u16 {
    rows(buffer)
        .iter()
        .position(|row| row.contains("番組一覧"))
        .unwrap_or_else(|| panic!("番組一覧 was not drawn\n{}", plain(buffer))) as u16
}

fn program_entries(buffer: &Buffer) -> Vec<String> {
    let rows = rows(buffer);
    let top = rows
        .iter()
        .position(|row| row.contains("番組一覧"))
        .expect("list title");
    let bottom = rows
        .iter()
        .enumerate()
        .skip(top + 1)
        .find(|(_, row)| row.contains('└'))
        .map(|(index, _)| index)
        .expect("list bottom border");
    rows[top + 1..bottom]
        .iter()
        .filter(|row| row.contains("SLOT"))
        .cloned()
        .collect()
}

fn slot_ids(row: &str) -> Vec<String> {
    let mut ids = Vec::new();
    let bytes = row.as_bytes();
    let mut index = 0;
    while index + 6 <= bytes.len() {
        if &bytes[index..index + 4] == b"SLOT"
            && bytes[index + 4].is_ascii_digit()
            && bytes[index + 5].is_ascii_digit()
        {
            ids.push(row[index..index + 6].to_owned());
            index += 6;
        } else {
            index += 1;
        }
    }
    ids
}

fn catalog(count: usize) -> Screen {
    let mut screen = screen();
    let template = radiko::api::catalog::parse_programs(
        include_bytes!("../../../../../tests/fixtures/schedule.xml"),
        "TBS",
        radiko::api::catalog::parse_day("20261015").unwrap(),
    )
    .unwrap()
    .programs
    .remove(0);
    let mut programs = Vec::new();
    for index in 0..count {
        let mut program = template.clone();
        program.title = format!("SLOT{index:02}");
        program.program_id = format!("slot-{index}");
        program.key.starts_at += chrono::Duration::minutes(index as i64);
        program.ends_at += chrono::Duration::minutes(index as i64);
        programs.push(program);
    }
    let len = programs.len();
    screen.app.programs = programs;
    screen.app.visible = (0..len).collect();
    screen.list.select(Some(0));
    screen
}

fn playing() -> radiko::player::state::PlaybackSnapshot {
    let start = DateTime::parse_from_rfc3339("2026-10-15T03:00:00+09:00").unwrap();
    radiko::player::state::PlaybackSnapshot {
        info: radiko::api::stream::playback::StreamInfo {
            title: Some("朝の番組".into()),
            station: "TBS".into(),
            mode: radiko::api::stream::ProbeMode::TimeFree,
            starts_at: start,
            ends_at: start + chrono::Duration::seconds(3300),
            position: Duration::ZERO,
        },
        state: PlaybackState::Playing,
        pending: None,
        volume: 25,
        position: Some(Duration::from_secs(754)),
        output: None,
        segments: 1,
        pipeline: None,
        receiving_for: None,
        reconnecting_for: None,
    }
}

fn attach(screen: &mut Screen) {
    screen.app.playback = Some(playing());
}

fn footer(buffer: &Buffer) -> (String, String, String) {
    let rows = rows(buffer);
    let height = rows.len();
    assert!(height >= 3, "{rows:?}");
    (
        rows[height - 3].clone(),
        rows[height - 2].clone(),
        rows[height - 1].clone(),
    )
}

const HINT: &str = "? 操作一覧 / Enter 再生 / Space 一時停止 / q 終了";

#[test]
fn height_29_is_compact_and_30_is_normal_including_one_row_either_way() {
    let mut screen = catalog(24);
    attach(&mut screen);
    screen.show_notice("境界の通知");

    let compact = buffer(&mut screen, 80, 29);
    assert_eq!(
        list_title_y(&compact),
        2,
        "header is 2 rows\n{}",
        plain(&compact)
    );
    let (play, notice, hint) = footer(&compact);
    assert!(play.contains("[TBS / タイムフリー]"), "{play}");
    assert!(play.contains("再生中"), "{play}");
    assert!(play.contains("12:34 / 55:00"), "{play}");
    assert!(!play.contains("音量"), "{play}");
    assert_eq!(notice, "境界の通知");
    assert_eq!(hint, HINT);
    assert!(program_entries(&compact).len() >= 12);

    let normal = buffer(&mut screen, 80, 30);
    assert_eq!(
        list_title_y(&normal),
        7,
        "height 30 keeps the 7-row header\n{}",
        plain(&normal)
    );
    let body = plain(&normal);
    assert!(body.contains("音量25%"), "{body}");
    assert!(body.contains("e 取得状況"), "{body}");
    assert!(body.contains("12:34 / 55:00"), "{body}");
    assert!(
        !body.contains(HINT),
        "normal hint is the long control line\n{body}"
    );
}

#[test]
fn width_alone_does_not_select_the_compact_layout() {
    let mut screen = catalog(4);
    attach(&mut screen);
    let short_wide = buffer(&mut screen, 160, 24);
    assert_eq!(list_title_y(&short_wide), 2, "{}", plain(&short_wide));
    assert!(
        !plain(&short_wide).contains("音量"),
        "{}",
        plain(&short_wide)
    );
    assert!(plain(&short_wide).contains(HINT), "{}", plain(&short_wide));

    let tall_narrow = buffer(&mut screen, 40, 40);
    assert_eq!(list_title_y(&tall_narrow), 7, "{}", plain(&tall_narrow));
    assert!(
        plain(&tall_narrow).contains("e 取得状況"),
        "{}",
        plain(&tall_narrow)
    );
    assert!(
        !plain(&tall_narrow).contains(HINT),
        "{}",
        plain(&tall_narrow)
    );
}

#[test]
fn eighty_by_twenty_four_shows_twelve_program_rows_and_the_scrolled_selection() {
    let mut screen = catalog(24);
    attach(&mut screen);
    screen.show_notice("一覧の通知");
    screen.list.select(Some(0));
    let top = buffer(&mut screen, 80, 24);
    assert_eq!(list_title_y(&top), 2);
    let entries = program_entries(&top);
    assert!(
        entries.len() >= 12,
        "expected at least 12 program rows, got {}\n{}",
        entries.len(),
        entries.join("\n")
    );
    let mut seen = std::collections::BTreeSet::new();
    for row in &entries {
        for id in slot_ids(row) {
            seen.insert(id);
        }
    }
    assert!(
        seen.len() >= 12,
        "program rows were not distinct: {seen:?}\n{}",
        entries.join("\n")
    );
    let selected = entries
        .iter()
        .find(|row| row.contains('▶'))
        .expect("highlight");
    assert!(selected.contains("SLOT00"), "{selected}");
    let (play, notice, hint) = footer(&top);
    assert!(play.contains("[TBS / タイムフリー]"), "{play}");
    assert_eq!(notice, "一覧の通知");
    assert_eq!(hint, HINT);
    assert_ne!(play, notice);
    assert!(!play.contains("一覧の通知"));
    assert!(!notice.contains("? 操作一覧"));
    assert!(!hint.contains("一覧の通知"));
    let rows = rows(&top);
    assert!(
        rows[rows.len() - 4].contains('└'),
        "footer is 3 rows above the list border\n{}",
        rows.join("\n")
    );

    screen.list.select(Some(23));
    let bottom = buffer(&mut screen, 80, 24);
    let entries = program_entries(&bottom);
    assert!(entries.len() >= 12, "{}", entries.len());
    let selected = entries
        .iter()
        .find(|row| row.contains('▶'))
        .expect("highlight");
    assert!(selected.contains("SLOT23"), "{selected}");
    assert!(
        entries.iter().all(|row| !row.contains("SLOT00")),
        "the first row stayed on screen after scrolling\n{}",
        entries.join("\n")
    );
    assert_eq!(screen.list.selected(), Some(23));
}

#[test]
fn playback_notice_and_hint_rows_stay_apart_for_stopped_and_chapter_views() {
    let mut playing_screen = catalog(2);
    attach(&mut playing_screen);
    let shown = buffer(&mut playing_screen, 80, 24);
    let (play, notice, hint) = footer(&shown);
    assert_eq!(play, "再生中: [TBS / タイムフリー] 再生中 / 12:34 / 55:00");
    assert_eq!(notice, "");
    assert_eq!(hint, HINT);
    assert!(!plain(&shown).contains('█'));
    assert!(!plain(&shown).contains('░'));

    playing_screen.show_notice("再生中の通知");
    let shown = buffer(&mut playing_screen, 80, 24);
    let (play, notice, hint) = footer(&shown);
    assert_eq!(play, "再生中: [TBS / タイムフリー] 再生中 / 12:34 / 55:00");
    assert_eq!(notice, "再生中の通知");
    assert_eq!(hint, HINT);

    let mut stopped = catalog(2);
    stopped.stop_cause = Some(super::super::StopCause::AuthExpired);
    stopped.show_notice(super::super::messages::AUTH_EXPIRED_STOP);
    let shown = buffer(&mut stopped, 80, 24);
    let (play, notice, hint) = footer(&shown);
    assert_eq!(play, "停止中：認証期限切れ（a で再確認）");
    assert_eq!(notice, super::super::messages::AUTH_EXPIRED_STOP);
    assert_eq!(hint, HINT);
    assert!(!play.contains('✕'));
    assert!(!notice.contains("停止中："));

    let mut chapters = catalog(1);
    attach(&mut chapters);
    chapters.show_notice("章の通知");
    let target = chapters.app.programs[0].clone();
    chapters.app.chapters.target = Some(target);
    chapters.app.chapters.candidates = Some(ChapterSet {
        chapters: vec![Chapter {
            title: "章タイトル甲".into(),
            summary: String::new(),
            start: Duration::from_secs(1),
            end: Duration::from_secs(2),
            source_index: 0,
        }],
        invalid_count: 0,
        music_count: 0,
    });
    chapters.chapter_list.select(Some(0));
    let shown = buffer(&mut chapters, 80, 24);
    let body = plain(&shown);
    assert!(body.contains("章タイトル甲"), "{body}");
    assert!(body.contains("公式AIチャプター"), "{body}");
    let (play, notice, hint) = footer(&shown);
    assert_eq!(play, "再生中: [TBS / タイムフリー] 再生中 / 12:34 / 55:00");
    assert_eq!(notice, "章の通知");
    assert_eq!(hint, HINT);
    assert!(!notice.contains("章タイトル甲"));

    chapters.app.playback = None;
    chapters.stop_cause = Some(super::super::StopCause::ReconnectLimit);
    let shown = buffer(&mut chapters, 80, 24);
    let (play, notice, _) = footer(&shown);
    assert_eq!(
        play,
        "停止中：再接続できませんでした（番組を選び直してください）"
    );
    assert_eq!(notice, "章の通知");
    assert!(plain(&shown).contains("章タイトル甲"));
}

#[test]
fn hint_cells_drop_whole_items_at_mixed_fullwidth_boundaries() {
    let mut screen = catalog(1);
    let full = buffer(&mut screen, 80, 24);
    assert_eq!(footer(&full).2, HINT);
    let hint_width = UnicodeWidthStr::width(HINT);
    assert_eq!(hint_width, 49);
    assert!(hint_width <= 78);

    let two = buffer(&mut screen, 32, 24);
    assert_eq!(footer(&two).2, "? 操作一覧 / Enter 再生");
    assert!(!footer(&two).2.contains("Space"));

    let inside_second = buffer(&mut screen, 21, 24);
    assert_eq!(footer(&inside_second).2, "? 操作一覧");
    assert!(!footer(&inside_second).2.contains("Enter"));
    assert!(!footer(&inside_second).2.contains("一覧 /"));

    let first = buffer(&mut screen, 12, 24);
    assert_eq!(footer(&first).2, "? 操作一覧");

    let none = buffer(&mut screen, 11, 24);
    assert_eq!(footer(&none).2, "");
    assert!(!footer(&none).2.contains('?'));
}

#[test]
fn resize_between_29_and_30_keeps_input_selection_playback_and_notices() {
    let mut screen = catalog(12);
    attach(&mut screen);
    let position = screen.app.playback.as_ref().unwrap().position;
    screen.list.select(Some(4));
    screen.input = Some("保持入力あ".into());
    screen.show_notice("保持する通知");

    let compact = buffer(&mut screen, 80, 29);
    let text = plain(&compact);
    assert!(text.contains("保持入力あ"), "{text}");
    assert!(text.contains("キーワード: 保持入力あ"), "{text}");
    assert!(text.contains("保持する通知"), "{text}");
    assert!(
        program_entries(&compact)
            .iter()
            .any(|row| row.contains("SLOT04") && row.contains('▶'))
    );
    assert_eq!(list_title_y(&compact), 2);

    let normal = buffer(&mut screen, 160, 30);
    assert_eq!(screen.input.as_deref(), Some("保持入力あ"));
    assert_eq!(screen.playback_notice, "保持する通知");
    assert_eq!(screen.list.selected(), Some(4));
    assert_eq!(screen.app.playback.as_ref().unwrap().position, position);
    let text = plain(&normal);
    assert!(text.contains("保持入力あ"), "{text}");
    assert!(text.contains("条件編集中"), "{text}");
    assert_eq!(list_title_y(&normal), 7, "{text}");

    let again = buffer(&mut screen, 80, 29);
    assert_eq!(screen.input.as_deref(), Some("保持入力あ"));
    assert_eq!(screen.playback_notice, "保持する通知");
    assert_eq!(screen.list.selected(), Some(4));
    assert_eq!(screen.app.playback.as_ref().unwrap().position, position);
    let text = plain(&again);
    assert!(text.contains("保持入力あ"), "{text}");
    assert!(text.contains("保持する通知"), "{text}");
    assert_eq!(list_title_y(&again), 2);
    assert_eq!(footer(&again).1, "保持する通知");
}

#[test]
fn compact_hides_diagnostics_and_help_and_device_still_show_them() {
    let mut screen = catalog(2);
    attach(&mut screen);
    screen.pipeline_diagnostics = "処理診断 PIPE_TOKEN_4000\n通信診断 PIPE_HTTP_12".into();
    let shown = plain(&buffer(&mut screen, 80, 24));
    assert!(!shown.contains("PIPE_TOKEN_4000"), "{shown}");
    assert!(!shown.contains("PIPE_HTTP_12"), "{shown}");
    assert!(!shown.contains("処理診断"), "{shown}");
    assert!(shown.contains("? 操作一覧"), "{shown}");

    screen.help_open = true;
    let help = plain(&buffer(&mut screen, 80, 24));
    assert!(help.contains("操作一覧"), "{help}");
    assert!(help.contains("PIPE_TOKEN_4000"), "{help}");
    assert!(help.contains("PIPE_HTTP_12"), "{help}");
    screen.help_open = false;

    screen.device_open = true;
    screen.device_status =
        super::super::device::DeviceStatus::Failed(super::super::device::DeviceFailure::NoDevice);
    let device = plain(&buffer(&mut screen, 80, 24));
    assert!(device.contains("出力機器"), "{device}");
    assert!(
        device.contains("既定の音声出力機器がありません"),
        "{device}"
    );
    assert!(device.contains(super::super::device::NOTE), "{device}");
}

#[test]
fn logout_confirmation_replaces_the_compact_hint_and_tiny_screens_do_not_panic() {
    let mut screen = catalog(4);
    attach(&mut screen);
    screen.input = Some("残す".into());
    screen.show_notice("残す通知");
    screen.logout_confirm = true;
    let shown = buffer(&mut screen, 80, 24);
    let hint = footer(&shown).2;
    assert!(
        hint.starts_with("ログアウトしますか？"),
        "{hint}\n{}",
        plain(&shown)
    );
    assert_ne!(hint, HINT);

    screen.logout_confirm = false;
    let input = screen.input.clone();
    let notice = screen.playback_notice.clone();
    let selected = screen.list.selected();
    for (width, height) in [(0u16, 0u16), (1, 1), (2, 2), (8, 4), (3, 5), (80, 24)] {
        let _ = buffer(&mut screen, width, height);
    }
    assert_eq!(screen.input, input);
    assert_eq!(screen.playback_notice, notice);
    assert_eq!(screen.list.selected(), selected);
}

#[test]
fn notice_mark_and_color_stay_on_the_dedicated_row() {
    let mut screen = catalog(1);
    attach(&mut screen);
    let raw = "再生切替失敗（現在の状態を維持）: 合成";
    screen.apply_playback(radiko::app::playback::Update {
        snapshot: Some(playing()),
        notice: Some(raw.into()),
        notice_kind: Some(radiko::app::playback::NoticeKind::HttpWhilePlaying),
        notice_prefix: Some("再生切替失敗（現在の状態を維持）"),
        ..radiko::app::playback::Update::default()
    });
    let shown = buffer(&mut screen, 80, 24);
    let (play, notice, _) = footer(&shown);
    assert_eq!(
        notice,
        "! 切り替え先の配信を取得できませんでした。再生は続いています"
    );
    assert!(!play.contains('!'), "{play}");
    let columns = usize::from(shown.area.width);
    let mut found = false;
    for row in shown.content.chunks(columns) {
        let mut line = String::new();
        let mut x = 0;
        while x < row.len() {
            line.push_str(row[x].symbol());
            x += UnicodeWidthStr::width(row[x].symbol()).max(1);
        }
        if line.contains('!') {
            let cell = row.iter().find(|cell| cell.symbol() != "│").unwrap();
            assert_eq!(cell.fg, Color::Yellow);
            assert!(!cell.modifier.contains(Modifier::BOLD));
            found = true;
            break;
        }
    }
    assert!(found, "{}", plain(&shown));

    let marked = "! 認証期限切れです。再生は続いています。a で再確認 / I で再ログイン";
    let marked_width = UnicodeWidthStr::width(marked);
    assert_eq!(marked_width, 67);
    assert!(marked_width <= 78);
    let longer = "✕ 再生切替失敗（現在の状態を維持）: 配信取得: シーク位置は番組冒頭以上・放送枠末尾未満で指定してください";
    assert_eq!(UnicodeWidthStr::width(longer), 104);
    assert!(UnicodeWidthStr::width(longer) > 80);
}

#[test]
fn existing_small_terminal_draws_still_return() {
    let mut screen = screen();
    let id = screen.app.commit().unwrap();
    let list = radiko::api::catalog::parse_programs(
        include_bytes!("../../../../../tests/fixtures/schedule.xml"),
        "TBS",
        radiko::api::catalog::parse_day("20261015").unwrap(),
    )
    .unwrap();
    screen.apply(radiko::app::search::Event::Progress {
        id,
        progress: radiko::app::search::Progress {
            phase: "番組表",
            target: "TBS".into(),
            day: Some(list.broadcast_day),
            completed: 1,
            total: 1,
            failed: 0,
            programs: list.programs,
        },
    });
    let mut tiny = Terminal::new(TestBackend::new(8, 4)).unwrap();
    tiny.draw(|frame| draw(frame, &mut screen, now())).unwrap();
    let mut normal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    normal
        .draw(|frame| draw(frame, &mut screen, now()))
        .unwrap();
    let text: String = normal
        .backend()
        .buffer()
        .content
        .iter()
        .filter(|cell| cell.symbol() != " ")
        .map(|cell| cell.symbol())
        .collect();
    assert!(text.contains("タイムフリー"), "{text}");
    assert!(text.contains("番組一覧"), "{text}");
}
