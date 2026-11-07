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

const IDLE_SENTENCE: &str = "再生切替失敗（現在の状態を維持）: 配信取得: シーク位置は番組冒頭以上・放送枠末尾未満で指定してください";
const IDLE_MARKED: &str = "✕ 再生切替失敗（現在の状態を維持）: 配信取得: シーク位置は番組冒頭以上・放送枠末尾未満で指定してください";
const GUIDE_FIRST: &str =
    "出力先がありません。OSのサウンド設定を確認し、Enterで再生し直してください";
const GUIDE_SECOND: &str = "自動再開はしません";

fn show_idle_error(screen: &mut Screen) {
    screen.apply_playback(radiko::app::playback::Update {
        notice: Some(IDLE_SENTENCE.to_owned()),
        notice_kind: Some(radiko::app::playback::NoticeKind::InvalidPositionWhilePlaying),
        notice_prefix: Some("再生切替失敗（現在の状態を維持）"),
        ..radiko::app::playback::Update::default()
    });
    assert_eq!(screen.playback_notice, IDLE_SENTENCE);
    assert_eq!(
        screen.notice_level,
        super::super::notices::NoticeLevel::Error
    );
}

fn footer_after_list(buffer: &Buffer) -> Vec<String> {
    let rows = rows(buffer);
    let bottom = rows
        .iter()
        .rposition(|row| row.contains('└'))
        .unwrap_or_else(|| panic!("list border missing\n{}", rows.join("\n")));
    rows[bottom + 1..].to_vec()
}

fn joined(rows: &[String]) -> String {
    rows.iter().map(String::as_str).collect()
}

fn notice_rows(footer: &[String]) -> &[String] {
    assert!(footer.len() >= 3, "{footer:?}");
    &footer[1..footer.len() - 1]
}

#[test]
fn long_idle_error_is_wrapped_in_full_at_80x24_including_the_suffix() {
    let mut screen = catalog(16);
    show_idle_error(&mut screen);
    assert_eq!(UnicodeWidthStr::width(IDLE_MARKED), 104);
    let shown = buffer(&mut screen, 80, 24);
    let footer = footer_after_list(&shown);
    assert_eq!(footer[0], "再生中: なし");
    assert_eq!(footer.last().map(String::as_str), Some(HINT));
    let notice = joined(notice_rows(&footer));
    assert_eq!(notice, IDLE_MARKED);
    assert!(notice.contains("末尾未満で指定してください"), "{notice}");
    assert!(notice_rows(&footer).len() > 1, "{footer:?}");
    let all = rows(&shown);
    for notice_row in notice_rows(&footer) {
        let y = all
            .iter()
            .position(|row| row == notice_row)
            .unwrap_or_else(|| panic!("missing {notice_row}"));
        let mut x = 0u16;
        let mut checked = false;
        while x < shown.area.width {
            let cell = &shown[(x, y as u16)];
            let advance = UnicodeWidthStr::width(cell.symbol()).max(1) as u16;
            if cell.symbol() != "│" && cell.symbol() != " " {
                assert_eq!(cell.fg, Color::Red, "{notice_row}");
                assert!(cell.modifier.contains(Modifier::BOLD), "{notice_row}");
                checked = true;
                break;
            }
            x = x.saturating_add(advance);
        }
        assert!(checked, "{notice_row}");
    }
}

#[test]
fn long_notice_and_guide_change_row_count_one_column_below_an_exact_fit() {
    let mut notice = catalog(2);
    show_idle_error(&mut notice);
    let fit = buffer(&mut notice, 106, 12);
    let fit_footer = footer_after_list(&fit);
    let fit_notice = notice_rows(&fit_footer);
    assert_eq!(fit_notice, &[IDLE_MARKED.to_owned()]);
    assert_eq!(UnicodeWidthStr::width(fit_notice[0].as_str()), 104);

    let below = buffer(&mut notice, 105, 12);
    let below_footer = footer_after_list(&below);
    let below_notice = notice_rows(&below_footer);
    assert_eq!(below_notice.len(), fit_notice.len() + 1);
    assert_eq!(joined(below_notice), IDLE_MARKED);
    assert!(UnicodeWidthStr::width(below_notice[0].as_str()) <= 103);
    assert!(UnicodeWidthStr::width(below_notice[1].as_str()) <= 103);

    let mut guide = catalog(2);
    guide.output_guide = Some(super::super::OutputGuide::NoDevice);
    let fit_guide = footer_after_list(&buffer(&mut guide, 75, 12));
    let fit_rows = &fit_guide[..fit_guide.len() - 2];
    assert_eq!(fit_rows, &[GUIDE_FIRST.to_owned(), GUIDE_SECOND.to_owned()]);
    let below_guide = footer_after_list(&buffer(&mut guide, 74, 12));
    let below_rows = &below_guide[..below_guide.len() - 2];
    assert_eq!(below_rows.len(), fit_rows.len() + 1);
    assert_eq!(joined(below_rows), format!("{GUIDE_FIRST}{GUIDE_SECOND}"));
    assert_eq!(below_rows.last().map(String::as_str), Some(GUIDE_SECOND));
}

#[test]
fn every_output_guide_keeps_both_sentences_at_80x24() {
    for guide in [
        super::super::OutputGuide::NoDevice,
        super::super::OutputGuide::UnsupportedFormat,
        super::super::OutputGuide::Device,
        super::super::OutputGuide::Stalled,
    ] {
        let mut screen = catalog(2);
        screen.output_guide = Some(guide);
        let lines = guide.lines();
        let shown = plain(&buffer(&mut screen, 80, 24));
        assert!(shown.contains(lines[0]), "{shown}");
        assert!(shown.contains(lines[1]), "{shown}");
        assert!(shown.contains(GUIDE_SECOND), "{shown}");
        let footer = footer_after_list(&buffer(&mut screen, 80, 24));
        assert_eq!(footer[0], lines[0]);
        assert_eq!(footer[1], lines[1]);
        assert_eq!(footer.last().map(String::as_str), Some(HINT));
    }
}

#[test]
fn long_notice_and_guide_share_the_footer_without_overlap_or_a_lost_hint() {
    let mut screen = catalog(24);
    show_idle_error(&mut screen);
    screen.output_guide = Some(super::super::OutputGuide::NoDevice);
    screen.list.select(Some(23));
    let shown = buffer(&mut screen, 80, 24);
    let footer = footer_after_list(&shown);
    assert!(footer.len() > 3, "{footer:?}");
    assert_eq!(footer.last().map(String::as_str), Some(HINT));
    let guide = &footer[..2];
    assert_eq!(guide[0], GUIDE_FIRST);
    assert_eq!(guide[1], GUIDE_SECOND);
    let notice = joined(&footer[2..footer.len() - 1]);
    assert_eq!(notice, IDLE_MARKED);
    assert!(!guide[0].contains("末尾未満で指定してください"));
    assert!(!notice.contains(GUIDE_SECOND));
    let entries = program_entries(&shown);
    let selected = entries
        .iter()
        .find(|row| row.contains('▶'))
        .expect("highlight");
    assert!(selected.contains("SLOT23"), "{selected}");
}

#[test]
fn clearing_a_long_notice_and_guide_restores_three_footer_rows_and_twelve_entries() {
    let mut screen = catalog(24);
    screen.input = Some("残す入力".into());
    screen.list.select(Some(23));
    show_idle_error(&mut screen);
    screen.output_guide = Some(super::super::OutputGuide::NoDevice);
    let during = buffer(&mut screen, 80, 24);
    assert_eq!(screen.input.as_deref(), Some("残す入力"));
    let during_entries = program_entries(&during);
    assert!(!during_entries.is_empty(), "{}", during_entries.len());
    assert!(footer_after_list(&during).len() > 3);
    let during_count = during_entries.len();
    // Esc while the keyword field is open cancels the field. Dismiss is the
    // normal screen, so close the field first and keep the typed value checked.
    screen.input = None;

    let mut search =
        Service::new(|| -> Result<CatalogClient, _> { panic!("dismiss must not fetch") });
    assert!(
        !screen
            .key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut search)
            .unwrap()
    );
    assert!(screen.output_guide.is_none());
    assert!(screen.playback_notice.is_empty());
    assert_eq!(screen.list.selected(), Some(23));
    let restored = buffer(&mut screen, 80, 24);
    let restored_rows = rows(&restored);
    assert!(
        restored_rows[restored_rows.len() - 4].contains('└'),
        "footer did not return to 3 rows\n{}",
        restored_rows.join("\n")
    );
    let entries = program_entries(&restored);
    assert!(entries.len() >= 12, "{}", entries.len());
    assert!(
        entries.len() > during_count,
        "{} vs {during_count}",
        entries.len()
    );
    let selected = entries
        .iter()
        .find(|row| row.contains('▶'))
        .expect("highlight");
    assert!(selected.contains("SLOT23"), "{selected}");
    assert_eq!(
        footer_after_list(&restored).last().map(String::as_str),
        Some(HINT)
    );
    assert!(!plain(&restored).contains("末尾未満で指定してください"));
    assert!(!plain(&restored).contains(GUIDE_SECOND));

    screen.output_guide = Some(super::super::OutputGuide::Stalled);
    show_idle_error(&mut screen);
    let _ = buffer(&mut screen, 80, 24);
    screen.apply_playback(radiko::app::playback::Update {
        new_session: true,
        snapshot: Some(playing()),
        ..radiko::app::playback::Update::default()
    });
    assert!(screen.output_guide.is_none());
    assert!(screen.playback_notice.is_empty());
    let playing_view = buffer(&mut screen, 80, 24);
    let playing_rows = rows(&playing_view);
    assert!(
        playing_rows[playing_rows.len() - 4].contains('└'),
        "{}",
        playing_rows.join("\n")
    );
    assert!(program_entries(&playing_view).len() >= 12);
    assert!(plain(&playing_view).contains("[TBS / タイムフリー]"));
    assert!(!plain(&playing_view).contains(GUIDE_SECOND));
    assert_eq!(screen.list.selected(), Some(23));
}

#[test]
fn resize_keeps_a_wrapped_notice_and_tiny_terminals_do_not_panic() {
    let mut screen = catalog(4);
    show_idle_error(&mut screen);
    screen.output_guide = Some(super::super::OutputGuide::NoDevice);
    screen.input = Some("幅を変えても残る".into());
    screen.list.select(Some(2));
    let compact = plain(&buffer(&mut screen, 80, 29));
    assert!(compact.contains("末尾未満で指定してください"), "{compact}");
    assert!(compact.contains(GUIDE_SECOND), "{compact}");
    assert!(compact.contains("幅を変えても残る"), "{compact}");
    assert_eq!(list_title_y(&buffer(&mut screen, 80, 29)), 2);

    let normal = buffer(&mut screen, 160, 30);
    assert_eq!(screen.playback_notice, IDLE_SENTENCE);
    assert_eq!(screen.input.as_deref(), Some("幅を変えても残る"));
    assert_eq!(screen.list.selected(), Some(2));
    assert!(screen.output_guide.is_some());
    assert_eq!(list_title_y(&normal), 7, "{}", plain(&normal));

    let again = plain(&buffer(&mut screen, 80, 29));
    assert_eq!(
        joined(&{
            let shown = buffer(&mut screen, 80, 24);
            let footer = footer_after_list(&shown);
            assert_eq!(footer.last().map(String::as_str), Some(HINT));
            notice_rows_after_guide(&footer)
        }),
        IDLE_MARKED
    );
    assert!(again.contains(GUIDE_FIRST), "{again}");

    for (width, height) in [(0u16, 0u16), (1, 1), (2, 2), (8, 4), (3, 5)] {
        let _ = buffer(&mut screen, width, height);
    }
    assert_eq!(screen.playback_notice, IDLE_SENTENCE);
    assert_eq!(screen.input.as_deref(), Some("幅を変えても残る"));
    assert_eq!(screen.list.selected(), Some(2));
    assert!(screen.output_guide.is_some());
}

fn notice_rows_after_guide(footer: &[String]) -> Vec<String> {
    assert!(footer.len() >= 4, "{footer:?}");
    assert_eq!(footer[0], GUIDE_FIRST);
    assert_eq!(footer[1], GUIDE_SECOND);
    footer[2..footer.len() - 1].to_vec()
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
