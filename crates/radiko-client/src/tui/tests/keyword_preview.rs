use super::*;
use radiko::domain::search::Filter;
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer, layout::Position};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use unicode_width::UnicodeWidthStr;

fn now() -> DateTime<FixedOffset> {
    radiko::api::stream::parse_time("20261015060000").unwrap()
}

fn press(screen: &mut Screen, service: &mut Service, code: KeyCode) {
    screen
        .key(KeyEvent::new(code, KeyModifiers::NONE), service)
        .unwrap();
}

fn press_char(screen: &mut Screen, service: &mut Service, c: char) {
    press(screen, service, KeyCode::Char(c));
}

fn type_str(screen: &mut Screen, service: &mut Service, text: &str) {
    for c in text.chars() {
        press_char(screen, service, c);
    }
}

fn ctrl_u(screen: &mut Screen, service: &mut Service) {
    screen
        .key(
            KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
            service,
        )
        .unwrap();
}

fn render(screen: &mut Screen, width: u16, height: u16) -> (Buffer, Position) {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| draw(frame, screen, now())).unwrap();
    let cursor = terminal.get_cursor_position().unwrap();
    let buffer = terminal.backend().buffer().clone();
    (buffer, cursor)
}

fn row_text(buffer: &Buffer, y: u16) -> String {
    let mut x = 0u16;
    let mut text = String::new();
    while x < buffer.area.width {
        let symbol = buffer[(x, y)].symbol();
        let advance = u16::try_from(UnicodeWidthStr::width(symbol).max(1)).unwrap_or(1);
        text.push_str(symbol);
        x = x.saturating_add(advance);
    }
    text
}

fn keyword_row(buffer: &Buffer) -> (u16, String) {
    // The committed header also says 「キーワード:」. The input field is the
    // line above 「Enter 検索 / …（編集中は取得しません）」.
    for y in 1..buffer.area.height {
        let text = row_text(buffer, y);
        if text.contains("編集中は取得しません") || text.contains("Enter 検索") {
            return (y - 1, row_text(buffer, y - 1));
        }
    }
    for y in 0..buffer.area.height {
        let text = row_text(buffer, y);
        if text.contains("キーワード: ") {
            return (y, text);
        }
    }
    panic!("keyword row missing");
}

fn plain(buffer: &Buffer) -> String {
    (0..buffer.area.height)
        .map(|y| row_text(buffer, y))
        .collect::<Vec<_>>()
        .join("\n")
}

fn label_start(buffer: &Buffer, y: u16, needle: &str) -> u16 {
    let mut x = 0u16;
    let mut text = String::new();
    let mut marks = vec![(0usize, 0u16)];
    while x < buffer.area.width {
        let symbol = buffer[(x, y)].symbol();
        let advance = u16::try_from(UnicodeWidthStr::width(symbol).max(1)).unwrap_or(1);
        text.push_str(symbol);
        x = x.saturating_add(advance);
        marks.push((text.len(), x));
    }
    let pos = text
        .find(needle)
        .unwrap_or_else(|| panic!("missing {needle:?} in {text:?}"));
    marks
        .into_iter()
        .find(|(len, _)| *len == pos)
        .map(|(_, at)| at)
        .unwrap_or_else(|| panic!("no cell for {needle:?} in {text:?}"))
}

struct Bench {
    runtime: tokio::runtime::Runtime,
    fetches: Arc<AtomicUsize>,
    service: Service,
}

impl Bench {
    fn new() -> Self {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let fetches = Arc::new(AtomicUsize::new(0));
        let counter = fetches.clone();
        let service = Service::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok::<_, radiko::api::catalog::CatalogError>(super::super::offline::Fixture)
        });
        Self {
            runtime,
            fetches,
            service,
        }
    }

    fn fetches(&self) -> usize {
        self.fetches.load(Ordering::SeqCst)
    }

    fn drain(&mut self, screen: &mut Screen) {
        let runtime = &self.runtime;
        let service = &mut self.service;
        runtime.block_on(async {
            let start = tokio::time::Instant::now();
            while screen.app.status == SearchStatus::Loading
                && start.elapsed() < Duration::from_secs(2)
            {
                for event in service.poll().await {
                    screen.apply(event);
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        });
    }
}

fn template() -> radiko::domain::Program {
    radiko::api::catalog::parse_programs(
        include_bytes!("../../../../../tests/fixtures/schedule.xml"),
        "TBS",
        radiko::api::catalog::parse_day("20261015").unwrap(),
    )
    .unwrap()
    .programs
    .remove(0)
}

fn program(title: &str) -> radiko::domain::Program {
    let mut program = template();
    program.title = title.into();
    program.performer.clear();
    program.description.clear();
    program.info.clear();
    program.tags.clear();
    program.program_genres.clear();
    program.personality_genres.clear();
    program.key.station_id = "TBS".into();
    program
}

fn genre(id: &str) -> radiko::domain::Classification {
    radiko::domain::Classification {
        id: id.into(),
        name: id.into(),
    }
}

fn commit_genres(screen: &mut Screen, ids: &[&str]) {
    let current = screen.app.committed.clone();
    let filter = Filter::new(
        ids.iter().map(|id| (*id).to_owned()).collect(),
        vec![],
        false,
        false,
        None,
    )
    .unwrap();
    screen.app.committed =
        SearchCriteria::new(current.scope().clone(), current.range().clone(), filter).unwrap();
}

fn install(screen: &mut Screen, programs: Vec<radiko::domain::Program>, visible: Vec<usize>) {
    screen.app.programs = programs;
    screen.app.visible = visible;
    screen.app.status = SearchStatus::Ready;
    screen.list.select(Some(0));
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
            position: std::time::Duration::ZERO,
        },
        state: PlaybackState::Playing,
        pending: None,
        volume: 25,
        position: Some(std::time::Duration::from_secs(754)),
        output: None,
        segments: 1,
        pipeline: None,
        receiving_for: None,
        reconnecting_for: None,
    }
}

fn full_label(count: usize) -> String {
    format!("表示中の一覧で一致 {count} 件（未確定）")
}

fn short_label(count: usize) -> String {
    format!("一致 {count} 件")
}

/// Inner columns required for the prefix, one cursor cell, one gap, and the label.
fn columns_needed(label: &str) -> usize {
    UnicodeWidthStr::width("キーワード: ") + 1 + 1 + UnicodeWidthStr::width(label)
}

fn assert_label(buffer: &Buffer, label: &str) {
    let (y, text) = keyword_row(buffer);
    let pos = text
        .find(label)
        .unwrap_or_else(|| panic!("keyword row clipped or missing {label:?}\n{text}"));
    let rest = &text[pos + label.len()..];
    assert!(
        rest.chars().all(|c| c == '│'),
        "count must end at the right border, not a fragment: {text:?}"
    );
    assert_eq!(
        &text[pos..pos + label.len()],
        label,
        "count text must be the whole label"
    );
    let _ = (y, label_start(buffer, y, label));
}

fn assert_no_count(buffer: &Buffer) {
    let (_, text) = keyword_row(buffer);
    assert!(
        !text.contains("未確定") && !text.contains("表示中の一覧") && !text.contains("一致 "),
        "count must be absent, not clipped: {text}"
    );
}

fn assert_idle(screen: &mut Screen, bench: &mut Bench) {
    // Drive a submitted search long enough for its factory to run, and stop
    // before a missing task would look like a timeout.
    let deadline = std::time::Instant::now() + Duration::from_millis(200);
    while std::time::Instant::now() < deadline && bench.fetches() == 0 {
        let runtime = &bench.runtime;
        let service = &mut bench.service;
        runtime.block_on(async {
            let events = service.poll().await;
            for event in events {
                screen.apply(event);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        });
    }
    assert_eq!(bench.fetches(), 0, "keyword editing fetched");
    assert_eq!(screen.app.status, SearchStatus::Ready);
    assert!(
        screen.playback_action.is_none(),
        "keyword editing started playback"
    );
    assert!(
        screen.named_action.is_none(),
        "keyword editing saved a named search"
    );
    assert!(!screen.auth_refresh, "keyword editing refreshed auth");
    assert!(screen.chapter_action.is_none());
    assert!(!screen.logout_requested);
}

#[test]
fn displayed_rows_use_the_committed_filter_and_ignore_hidden_programs() {
    assert_eq!(UnicodeWidthStr::width("キーワード: "), 12);
    assert_eq!("ＥＸＡＭＰＬＥ".to_lowercase(), "ｅｘａｍｐｌｅ");
    assert_eq!("Ｅｘａｍｐｌｅ".to_lowercase(), "ｅｘａｍｐｌｅ");
    assert_ne!("ＥＸＡＭＰＬＥ".to_lowercase(), "example");
    assert_ne!("ﾗｼﾞｵ".to_lowercase(), "ラジオ");
    assert_eq!("\u{3000}red\u{3000}".trim(), "red");
    assert!("  \u{3000}".trim().is_empty());
    assert_eq!("red  fox".trim(), "red  fox");

    let mut bench = Bench::new();
    let _entered = bench.runtime.enter();
    let mut screen = screen();
    commit_genres(&mut screen, &["G1"]);
    let committed = screen.app.committed.clone();
    screen.app.playback = Some(playing());
    let position = screen.app.playback.as_ref().unwrap().position;

    let mut title = program("An Example");
    title.program_genres = vec![genre("G1")];
    let mut performer = program("ROW-PERFORMER");
    performer.performer = "An Example".into();
    performer.program_genres = vec![genre("G1")];
    let mut description = program("ROW-DESCRIPTION");
    description.description = "An Example".into();
    description.program_genres = vec![genre("G1")];
    let mut info = program("ROW-INFO");
    info.info = "An Example".into();
    info.program_genres = vec![genre("G1")];
    let mut tags = program("ROW-TAGS");
    tags.tags = vec!["An Example".into()];
    tags.program_genres = vec![genre("G1")];
    let mut fullwidth = program("Ｅｘａｍｐｌｅ");
    fullwidth.program_genres = vec![genre("G1")];
    let mut genre_miss = program("ROW-GENRE Example");
    genre_miss.program_genres = vec![genre("G2")];
    let plain_row = {
        let mut row = program("ROW-PLAIN");
        row.program_genres = vec![genre("G1")];
        row
    };
    let mut kana = program("ラジオ");
    kana.program_genres = vec![genre("G1")];
    let mut tagged = program("ROW-TAG");
    tagged.tags = vec!["タグ".into()];
    tagged.program_genres = vec![genre("G1")];
    let mut hidden = program("ROW-HIDDEN Example");
    hidden.program_genres = vec![genre("G1")];
    hidden.key.station_id = "OBS".into();
    hidden.key.starts_at -= chrono::Duration::days(3);
    let programs = vec![
        title,
        performer,
        description,
        info,
        tags,
        fullwidth,
        genre_miss,
        plain_row,
        kana,
        tagged,
        hidden,
    ];
    let visible: Vec<_> = (0..programs.len() - 1).collect();
    let order: Vec<_> = programs.iter().map(|p| p.title.clone()).collect();
    install(&mut screen, programs, visible);
    screen.list.select(Some(3));

    let oracle = |screen: &Screen, keyword: &str| {
        let filter = Filter::new(
            vec!["G1".into()],
            vec![],
            false,
            false,
            Some(keyword.trim().to_owned()),
        )
        .unwrap();
        screen
            .rows()
            .iter()
            .filter(|index| filter.matches(&screen.app.programs[**index]))
            .count()
    };
    assert_eq!(oracle(&screen, "EXAMPLE"), 5);
    assert_eq!(oracle(&screen, "ＥＸＡＭＰＬＥ"), 1);
    assert_eq!(oracle(&screen, "Ex.*"), 0);
    assert_eq!(oracle(&screen, "タグ"), 1);
    assert_eq!(oracle(&screen, "ラジオ"), 1);
    assert_eq!(oracle(&screen, "ﾗｼﾞｵ"), 0);
    // The hidden OBS row matches the keyword and G1, and is still not counted.
    assert!(
        Filter::new(
            vec!["G1".into()],
            vec![],
            false,
            false,
            Some("EXAMPLE".into())
        )
        .unwrap()
        .matches(screen.app.programs.last().unwrap())
    );

    press(&mut screen, &mut bench.service, KeyCode::Char('/'));
    // `/` is still a list command and may dismiss a notice. Characters after
    // the field is open must not.
    screen.playback_notice = "消えないエラー".into();
    screen.notice_level = super::super::notices::NoticeLevel::Error;
    screen.preferences_notice = "保存案内".into();
    let expect = |screen: &mut Screen, keyword: &str, count: usize| {
        assert_eq!(screen.keyword_preview_count(), Some(count), "{keyword}");
        assert_eq!(oracle(screen, keyword), count, "{keyword}");
        let (buffer, _) = render(screen, 160, 40);
        assert_label(&buffer, &full_label(count));
        let text = plain(&buffer);
        for title in [
            "An Example",
            "ROW-PERFORMER",
            "ROW-DESCRIPTION",
            "ROW-INFO",
            "ROW-TAGS",
            "Ｅｘａｍｐｌｅ",
            "ROW-GENRE",
            "ROW-PLAIN",
            "ラジオ",
            "ROW-TAG",
        ] {
            assert!(
                text.contains(title),
                "{title} left the displayed list\n{text}"
            );
        }
        assert!(!text.contains("ROW-HIDDEN"), "{text}");
        assert_eq!(screen.list.selected(), Some(3));
        assert_eq!(
            screen
                .app
                .programs
                .iter()
                .map(|p| p.title.as_str())
                .collect::<Vec<_>>(),
            order.iter().map(String::as_str).collect::<Vec<_>>()
        );
        assert_eq!(screen.app.committed, committed);
        assert_eq!(screen.app.playback.as_ref().unwrap().position, position);
        assert_eq!(screen.playback_notice, "消えないエラー");
        assert_eq!(screen.preferences_notice, "保存案内");
        assert!(screen.playback_action.is_none());
    };

    type_str(&mut screen, &mut bench.service, "EXAMPLE");
    expect(&mut screen, "EXAMPLE", 5);
    assert_eq!(bench.fetches(), 0);
    ctrl_u(&mut screen, &mut bench.service);
    type_str(&mut screen, &mut bench.service, "  EXAMPLE  ");
    expect(&mut screen, "EXAMPLE", 5);
    ctrl_u(&mut screen, &mut bench.service);
    type_str(&mut screen, &mut bench.service, "ＥＸＡＭＰＬＥ");
    expect(&mut screen, "ＥＸＡＭＰＬＥ", 1);
    ctrl_u(&mut screen, &mut bench.service);
    type_str(&mut screen, &mut bench.service, "Ex.*");
    expect(&mut screen, "Ex.*", 0);
    let (zero, _) = render(&mut screen, 80, 24);
    assert_label(&zero, "表示中の一覧で一致 0 件（未確定）");
    assert!(plain(&zero).contains('0'));
    ctrl_u(&mut screen, &mut bench.service);
    type_str(&mut screen, &mut bench.service, "  タグ  ");
    expect(&mut screen, "タグ", 1);
    ctrl_u(&mut screen, &mut bench.service);
    type_str(&mut screen, &mut bench.service, "ラジオ");
    expect(&mut screen, "ラジオ", 1);
    ctrl_u(&mut screen, &mut bench.service);
    type_str(&mut screen, &mut bench.service, "ﾗｼﾞｵ");
    expect(&mut screen, "ﾗｼﾞｵ", 0);
    ctrl_u(&mut screen, &mut bench.service);
    assert!(screen.keyword_preview_count().is_none());
    let (empty, _) = render(&mut screen, 160, 40);
    assert_no_count(&empty);
    type_str(&mut screen, &mut bench.service, "   ");
    assert!(
        screen.keyword_preview_count().is_none(),
        "spaces are an empty keyword"
    );
    let (spaces, _) = render(&mut screen, 80, 30);
    assert_no_count(&spaces);
    type_str(&mut screen, &mut bench.service, "\u{3000}");
    assert!(screen.keyword_preview_count().is_none());
    assert_idle(&mut screen, &mut bench);
}

#[test]
fn live_preview_counts_only_on_air_rows() {
    let mut bench = Bench::new();
    let _entered = bench.runtime.enter();
    let mut screen = screen();
    let mut on_hit = program("ON-HIT");
    on_hit.key.starts_at = radiko::api::stream::parse_time("20261015050000").unwrap();
    on_hit.ends_at = radiko::api::stream::parse_time("20261015070000").unwrap();
    let mut on_miss = program("ON-MISS");
    on_miss.key.starts_at = radiko::api::stream::parse_time("20261015053000").unwrap();
    on_miss.ends_at = radiko::api::stream::parse_time("20261015063000").unwrap();
    let mut off_hit = program("OFF-HIT");
    off_hit.key.starts_at = radiko::api::stream::parse_time("20261015080000").unwrap();
    off_hit.ends_at = radiko::api::stream::parse_time("20261015090000").unwrap();
    let programs = vec![on_hit, on_miss, off_hit];
    install(&mut screen, programs, vec![0, 1, 2]);
    press(&mut screen, &mut bench.service, KeyCode::Char('/'));
    type_str(&mut screen, &mut bench.service, "HIT");
    assert_eq!(screen.keyword_preview_count(), Some(2));
    let (timefree, _) = render(&mut screen, 80, 30);
    assert_label(&timefree, &full_label(2));
    let shown = plain(&timefree);
    assert!(shown.contains("ON-HIT") && shown.contains("OFF-HIT") && shown.contains("ON-MISS"));

    screen.live_mode = true;
    screen.rebuild_live(now());
    assert_eq!(screen.rows().len(), 2, "live rows are the on-air subset");
    assert_eq!(screen.keyword_preview_count(), Some(1));
    let (live, _) = render(&mut screen, 80, 24);
    assert_label(&live, "表示中の一覧で一致 1 件（未確定）");
    let shown = plain(&live);
    assert!(shown.contains("ON-HIT"), "{shown}");
    assert!(shown.contains("ON-MISS"), "{shown}");
    assert!(!shown.contains("OFF-HIT"), "{shown}");
    assert_eq!(bench.fetches(), 0);
    assert!(screen.playback_action.is_none());
    assert_eq!(screen.app.committed.filter().keyword(), None);
}

#[test]
fn edits_update_the_count_without_fetch_save_or_playback() {
    let mut bench = Bench::new();
    let _entered = bench.runtime.enter();
    let mut screen = screen();
    screen.app.playback = Some(playing());
    let position = screen.app.playback.as_ref().unwrap().position;
    install(
        &mut screen,
        vec![
            program("red apple"),
            program("red fox"),
            program("blue bird"),
        ],
        vec![0, 1, 2],
    );
    screen.list.select(Some(1));
    let committed = screen.app.committed.clone();
    let order: Vec<_> = screen
        .app
        .programs
        .iter()
        .map(|p| p.title.clone())
        .collect();
    press(&mut screen, &mut bench.service, KeyCode::Char('/'));
    screen.playback_notice = "消えないエラー".into();
    screen.notice_level = super::super::notices::NoticeLevel::Error;
    screen.preferences_notice = "保存案内".into();

    let see = |screen: &mut Screen, count: Option<usize>| {
        assert_eq!(
            screen.keyword_preview_count(),
            count,
            "input {:?}",
            screen.input.as_deref()
        );
        let (buffer, _) = render(screen, 80, 30);
        match count {
            Some(count) => assert_label(&buffer, &full_label(count)),
            None => assert_no_count(&buffer),
        }
        assert_eq!(screen.app.committed, committed);
        assert_eq!(screen.list.selected(), Some(1));
        assert_eq!(
            screen
                .app
                .programs
                .iter()
                .map(|p| p.title.clone())
                .collect::<Vec<_>>(),
            order
        );
        assert_eq!(screen.app.playback.as_ref().unwrap().position, position);
        assert_eq!(screen.playback_notice, "消えないエラー");
        assert_eq!(screen.preferences_notice, "保存案内");
        assert!(screen.playback_action.is_none());
        assert!(screen.named_action.is_none());
        assert!(!screen.auth_refresh);
        let text = plain(&buffer);
        assert!(
            text.contains("番組一覧 一致3 / 取得3枠"),
            "the preview must not replace the list count\n{text}"
        );
        assert!(text.contains("red fox"), "{text}");
        assert!(
            text.contains("red ap") && text.contains("blue b"),
            "input {:?} hid a row\n{text}",
            screen.input.as_deref()
        );
    };

    type_str(&mut screen, &mut bench.service, "red");
    see(&mut screen, Some(2));
    let (before, _) = render(&mut screen, 80, 30);
    let before_row = keyword_row(&before).1;
    for code in [KeyCode::Left, KeyCode::Right, KeyCode::Home, KeyCode::End] {
        press(&mut screen, &mut bench.service, code);
        see(&mut screen, Some(2));
        let (after, _) = render(&mut screen, 80, 30);
        assert_eq!(
            keyword_row(&after).1,
            before_row,
            "{code:?} changed the count"
        );
    }
    press(&mut screen, &mut bench.service, KeyCode::End);
    type_str(&mut screen, &mut bench.service, " fox");
    see(&mut screen, Some(1));
    type_str(&mut screen, &mut bench.service, "!");
    see(&mut screen, Some(0));
    let (zero, _) = render(&mut screen, 80, 24);
    assert!(
        keyword_row(&zero)
            .1
            .contains("表示中の一覧で一致 0 件（未確定）")
    );
    assert!(!keyword_row(&zero).1.contains("一致 2 件"));
    press(&mut screen, &mut bench.service, KeyCode::Backspace);
    see(&mut screen, Some(1));
    assert_eq!(screen.input.as_deref(), Some("red fox"));
    // Delete the 'f' from the middle. "red ox" matches none of the titles.
    press(&mut screen, &mut bench.service, KeyCode::Home);
    for _ in 0..4 {
        press(&mut screen, &mut bench.service, KeyCode::Right);
    }
    press(&mut screen, &mut bench.service, KeyCode::Delete);
    assert_eq!(screen.input.as_deref(), Some("red ox"));
    see(&mut screen, Some(0));
    ctrl_u(&mut screen, &mut bench.service);
    assert_eq!(screen.input.as_deref(), Some(""));
    see(&mut screen, None);
    let (cleared, _) = render(&mut screen, 80, 30);
    assert_no_count(&cleared);
    type_str(&mut screen, &mut bench.service, "blue");
    see(&mut screen, Some(1));
    press(&mut screen, &mut bench.service, KeyCode::Home);
    press(&mut screen, &mut bench.service, KeyCode::Delete);
    assert_eq!(screen.input.as_deref(), Some("lue"));
    // "lue" is still a substring of "blue bird". The count follows that match.
    see(&mut screen, Some(1));
    ctrl_u(&mut screen, &mut bench.service);
    type_str(&mut screen, &mut bench.service, "n");
    assert_eq!(screen.input.as_deref(), Some("n"));
    assert_eq!(
        screen.list.selected(),
        Some(1),
        "n in the keyword field is a character"
    );
    see(&mut screen, Some(0));
    ctrl_u(&mut screen, &mut bench.service);
    type_str(&mut screen, &mut bench.service, "o");
    assert_eq!(screen.input.as_deref(), Some("o"));
    assert_eq!(screen.list.selected(), Some(1));
    // "o" is inside "red fox". It is a character here, not the station draft.
    see(&mut screen, Some(1));
    assert_idle(&mut screen, &mut bench);
}

#[test]
fn escape_restores_the_committed_keyword_and_enter_refetches() {
    let mut bench = Bench::new();
    let _entered = bench.runtime.enter();
    let mut screen = screen();
    let current = screen.app.committed.clone();
    screen.app.committed = SearchCriteria::new(
        current.scope().clone(),
        current.range().clone(),
        current.filter().with_keyword(Some("元".into())).unwrap(),
    )
    .unwrap();
    let committed = screen.app.committed.clone();
    install(
        &mut screen,
        vec![program("blue bird"), program("red apple")],
        vec![0, 1],
    );
    screen.list.select(Some(1));
    press(&mut screen, &mut bench.service, KeyCode::Char('/'));
    screen.preferences_notice = "保存案内".into();
    ctrl_u(&mut screen, &mut bench.service);
    type_str(&mut screen, &mut bench.service, "  blue  ");
    assert_eq!(screen.keyword_preview_count(), Some(1));
    assert_eq!(screen.app.committed.filter().keyword(), Some("元"));
    assert_eq!(
        screen.app.draft.as_ref().unwrap().filter().keyword(),
        Some("blue")
    );
    let (preview, _) = render(&mut screen, 80, 30);
    assert_label(&preview, &full_label(1));
    assert_eq!(bench.fetches(), 0);
    assert_eq!(screen.preferences_notice, "保存案内");

    press(&mut screen, &mut bench.service, KeyCode::Esc);
    assert!(screen.input.is_none());
    assert!(
        screen.app.draft.is_none(),
        "Esc must drop the draft keyword"
    );
    assert_eq!(screen.app.committed, committed);
    assert_eq!(screen.list.selected(), Some(1));
    assert_eq!(screen.app.programs.len(), 2);
    assert_eq!(bench.fetches(), 0);
    assert!(screen.playback_action.is_none());
    let (cancelled, cursor) = render(&mut screen, 80, 30);
    assert!(
        !plain(&cancelled).contains("未確定"),
        "Esc must remove the preview\n{}",
        plain(&cancelled)
    );
    assert!(cursor.y < 30);

    press(&mut screen, &mut bench.service, KeyCode::Char('/'));
    ctrl_u(&mut screen, &mut bench.service);
    type_str(&mut screen, &mut bench.service, "  blue  ");
    press(&mut screen, &mut bench.service, KeyCode::Enter);
    assert!(screen.input.is_none(), "Enter closes the preview");
    assert!(screen.app.draft.is_none());
    assert_eq!(screen.app.status, SearchStatus::Loading);
    assert_eq!(screen.app.committed.filter().keyword(), Some("blue"));
    assert!(
        screen.app.programs.is_empty(),
        "Enter must drop the displayed list instead of keeping the preview"
    );
    assert!(screen.playback_action.is_none());
    let (fetching, _) = render(&mut screen, 80, 30);
    assert!(
        !plain(&fetching).contains("未確定"),
        "the preview is not the fetched list"
    );
    bench.runtime.block_on(async {
        let start = tokio::time::Instant::now();
        while bench.fetches() == 0 && start.elapsed() < Duration::from_secs(2) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    });
    assert_eq!(bench.fetches(), 1, "Enter fetches once");
    bench.drain(&mut screen);
    assert_eq!(screen.app.status, SearchStatus::Ready);
    assert_eq!(screen.app.programs.len(), 1);
    assert_eq!(screen.app.programs[0].title, "オフライン番組");
    assert!(
        screen.app.visible.is_empty(),
        "the refetched catalog is filtered by the committed keyword, not the old preview"
    );
    assert_ne!(screen.app.programs[0].title, "blue bird");
    assert!(screen.playback_action.is_none());
}

#[test]
fn unfinished_lists_omit_the_count_and_a_finished_empty_list_shows_zero() {
    let mut bench = Bench::new();
    let _entered = bench.runtime.enter();
    for status in [
        SearchStatus::Idle,
        SearchStatus::Loading,
        SearchStatus::Failed,
    ] {
        let mut screen = screen();
        install(&mut screen, vec![program("red apple")], vec![0]);
        screen.app.status = status;
        press(&mut screen, &mut bench.service, KeyCode::Char('/'));
        type_str(&mut screen, &mut bench.service, "red");
        assert_eq!(
            screen.keyword_preview_count(),
            None,
            "{:?}",
            screen.app.status
        );
        let (buffer, _) = render(&mut screen, 80, 30);
        assert_no_count(&buffer);
        assert_eq!(bench.fetches(), 0);
    }
    let mut empty = screen();
    install(&mut empty, vec![], vec![]);
    press(&mut empty, &mut bench.service, KeyCode::Char('/'));
    type_str(&mut empty, &mut bench.service, "red");
    assert_eq!(empty.keyword_preview_count(), Some(0));
    let (buffer, _) = render(&mut empty, 80, 24);
    assert_label(&buffer, "表示中の一覧で一致 0 件（未確定）");
    let mut partial = screen();
    install(&mut partial, vec![], vec![]);
    partial.app.status = SearchStatus::Partial;
    press(&mut partial, &mut bench.service, KeyCode::Char('/'));
    type_str(&mut partial, &mut bench.service, "red");
    assert_eq!(partial.keyword_preview_count(), Some(0));
    let (buffer, _) = render(&mut partial, 160, 40);
    assert_label(&buffer, &full_label(0));
}

#[test]
fn count_labels_are_whole_at_the_width_boundary_and_the_cursor_stays_visible() {
    assert_eq!(
        UnicodeWidthStr::width("表示中の一覧で一致 0 件（未確定）"),
        33
    );
    assert_eq!(UnicodeWidthStr::width("一致 0 件"), 9);
    assert_eq!(
        UnicodeWidthStr::width("表示中の一覧で一致 10 件（未確定）"),
        34
    );
    assert_eq!(UnicodeWidthStr::width("一致 10 件"), 10);
    assert_eq!(columns_needed(&full_label(0)), 47);
    assert_eq!(columns_needed(&short_label(0)), 23);
    assert_eq!(columns_needed(&full_label(10)), 48);
    assert_eq!(columns_needed(&short_label(10)), 24);

    let mut bench = Bench::new();
    let _entered = bench.runtime.enter();
    let mut zero = screen();
    install(&mut zero, vec![program("other")], vec![0]);
    press(&mut zero, &mut bench.service, KeyCode::Char('/'));
    type_str(&mut zero, &mut bench.service, "qx");
    assert_eq!(zero.keyword_preview_count(), Some(0));
    let full_width = u16::try_from(columns_needed(&full_label(0)) + 2).unwrap();
    let (exact_full, _) = render(&mut zero, full_width, 30);
    assert_label(&exact_full, &full_label(0));
    let (exact_full_compact, _) = render(&mut zero, full_width, 24);
    assert_label(&exact_full_compact, &full_label(0));

    let (short_only, _) = render(&mut zero, full_width - 1, 30);
    let short_row = keyword_row(&short_only).1;
    assert!(short_row.contains(&short_label(0)), "{short_row}");
    assert!(!short_row.contains("未確定"), "{short_row}");
    assert!(!short_row.contains("表示中"), "{short_row}");
    assert_label(&short_only, &short_label(0));
    let (short_compact, _) = render(&mut zero, full_width - 1, 29);
    assert_label(&short_compact, &short_label(0));

    let short_width = u16::try_from(columns_needed(&short_label(0)) + 2).unwrap();
    let (exact_short, _) = render(&mut zero, short_width, 30);
    assert_label(&exact_short, &short_label(0));
    let (omit, _) = render(&mut zero, short_width - 1, 30);
    assert_no_count(&omit);
    let (omit_compact, _) = render(&mut zero, short_width - 1, 24);
    assert_no_count(&omit_compact);

    let mut ten = screen();
    install(
        &mut ten,
        (0..10).map(|_| program("qx")).collect(),
        (0..10).collect(),
    );
    press(&mut ten, &mut bench.service, KeyCode::Char('/'));
    type_str(&mut ten, &mut bench.service, "qx");
    assert_eq!(ten.keyword_preview_count(), Some(10));
    let ten_full = u16::try_from(columns_needed(&full_label(10)) + 2).unwrap();
    let (ten_exact, _) = render(&mut ten, ten_full, 30);
    assert_label(&ten_exact, &full_label(10));
    // One column narrower still fits the one-digit full label, and not this one.
    let (ten_short, _) = render(&mut ten, full_width, 30);
    assert_label(&ten_short, &short_label(10));
    assert!(!keyword_row(&ten_short).1.contains("未確定"));
    assert_eq!(bench.fetches(), 0);

    for (width, height) in [(80u16, 24u16), (80, 29), (80, 30), (160, 40)] {
        let (buffer, _) = render(&mut zero, width, height);
        assert_label(&buffer, &full_label(0));
        assert!(
            keyword_row(&buffer).1.contains("未確定"),
            "{width}x{height} must show the unconfirmed label\n{}",
            keyword_row(&buffer).1
        );
    }

    let value = format!("{}中{}", "あ".repeat(30), "い".repeat(30));
    let mut long = screen();
    install(&mut long, vec![program("other")], vec![0]);
    press(&mut long, &mut bench.service, KeyCode::Char('/'));
    ctrl_u(&mut long, &mut bench.service);
    long.input = Some(value.as_str().into());
    for _ in 0..31 {
        press(&mut long, &mut bench.service, KeyCode::Left);
    }
    let at_naka = long.input.as_ref().unwrap().cursor();
    assert_eq!(at_naka, "あ".repeat(30).len());
    assert!(super::super::field::is_grapheme_boundary(
        long.input.as_deref().unwrap(),
        at_naka
    ));
    assert_eq!(long.keyword_preview_count(), Some(0));

    let (compact, compact_cursor) = render(&mut long, 80, 29);
    let (normal, normal_cursor) = render(&mut long, 80, 30);
    assert_eq!(long.input.as_ref().unwrap().cursor(), at_naka);
    assert_eq!(compact_cursor.y, 0);
    assert_eq!(normal_cursor.y, 27);
    assert_eq!(compact_cursor.x, normal_cursor.x);
    assert_eq!(compact[(compact_cursor.x, compact_cursor.y)].symbol(), "中");
    assert_eq!(normal[(normal_cursor.x, normal_cursor.y)].symbol(), "中");
    for (buffer, cursor) in [(&compact, compact_cursor), (&normal, normal_cursor)] {
        assert_label(buffer, &full_label(0));
        let start = label_start(buffer, cursor.y, &full_label(0));
        assert!(cursor.x < start, "cursor overlaps the count");
        assert!(cursor.x < buffer.area.width - 1);
        let field = &keyword_row(buffer).1;
        assert!(
            !field.contains(&"あ".repeat(30)),
            "the long keyword must scroll inside the field: {field}"
        );
    }
    let (wide, wide_cursor) = render(&mut long, 160, 40);
    assert_label(&wide, &full_label(0));
    assert_eq!(wide[(wide_cursor.x, wide_cursor.y)].symbol(), "中");
    assert!(wide_cursor.x < label_start(&wide, wide_cursor.y, &full_label(0)));
    let (compact_80, compact_80_cursor) = render(&mut long, 80, 24);
    assert_label(&compact_80, &full_label(0));
    assert_eq!(
        compact_80[(compact_80_cursor.x, compact_80_cursor.y)].symbol(),
        "中"
    );

    let (narrow, narrow_cursor) = render(&mut long, short_width - 1, 30);
    assert_no_count(&narrow);
    assert_eq!(narrow[(narrow_cursor.x, narrow_cursor.y)].symbol(), "中");
    assert!(narrow_cursor.x < narrow.area.width - 1);
    let (short_cursor_buf, short_cursor) = render(&mut long, full_width - 1, 24);
    assert_label(&short_cursor_buf, &short_label(0));
    assert_eq!(
        short_cursor_buf[(short_cursor.x, short_cursor.y)].symbol(),
        "中"
    );
    assert!(short_cursor.x < label_start(&short_cursor_buf, short_cursor.y, &short_label(0)));

    let cluster = "か\u{3099}";
    ctrl_u(&mut long, &mut bench.service);
    long.input = Some(cluster.into());
    let (cells, cursor) = render(&mut long, 80, 30);
    assert_label(&cells, &full_label(0));
    let field_start = {
        let (y, _) = keyword_row(&cells);
        // The cluster is the first field cell after the prefix.
        let mut x = 0u16;
        let mut text = String::new();
        let prefix = "キーワード: ";
        while x < cells.area.width {
            let symbol = cells[(x, y)].symbol();
            let advance = u16::try_from(UnicodeWidthStr::width(symbol).max(1)).unwrap_or(1);
            text.push_str(symbol);
            x = x.saturating_add(advance);
            if text.ends_with(prefix) {
                break;
            }
        }
        x
    };
    assert_eq!(cells[(field_start, cursor.y)].symbol(), cluster);
    assert_eq!(
        cursor.x,
        field_start + UnicodeWidthStr::width(cluster) as u16
    );
    assert!(cursor.x < label_start(&cells, cursor.y, &full_label(0)));

    for (width, height) in [(0u16, 0u16), (1, 1), (8, 4), (14, 29), (20, 30)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| draw(frame, &mut long, now()))
            .unwrap();
        let cursor = terminal.get_cursor_position().unwrap();
        if width == 0 || height == 0 {
            assert_eq!(cursor, Position::new(0, 0));
        } else {
            assert!(
                cursor.x < width && cursor.y < height,
                "{width}x{height} {cursor:?}"
            );
        }
    }
    assert_eq!(long.input.as_ref().unwrap().cursor(), cluster.len());
    assert_eq!(bench.fetches(), 0);
    assert!(long.playback_action.is_none());
}

#[test]
fn condition_fields_and_saved_names_do_not_show_the_keyword_count() {
    let mut bench = Bench::new();
    let _entered = bench.runtime.enter();
    let mut form = screen();
    install(&mut form, vec![program("red apple")], vec![0]);
    press(&mut form, &mut bench.service, KeyCode::Char('f'));
    for _ in 0..5 {
        press(&mut form, &mut bench.service, KeyCode::Tab);
    }
    type_str(&mut form, &mut bench.service, "red");
    let (buffer, _) = render(&mut form, 80, 30);
    let text = plain(&buffer);
    assert!(!text.contains("未確定"), "{text}");
    assert!(!text.contains("表示中の一覧で一致"), "{text}");
    assert_eq!(bench.fetches(), 0);
    assert!(form.playback_action.is_none());
    assert!(form.input.is_none());

    let mut named = screen();
    install(&mut named, vec![program("red apple")], vec![0]);
    press(&mut named, &mut bench.service, KeyCode::Char('b'));
    named.named_action = None;
    named.named.as_mut().unwrap().listed(Default::default());
    press_char(&mut named, &mut bench.service, 'n');
    type_str(&mut named, &mut bench.service, "保存名");
    let (buffer, _) = render(&mut named, 80, 30);
    let text = plain(&buffer);
    assert!(!text.contains("未確定"), "{text}");
    assert!(!text.contains("表示中の一覧で一致"), "{text}");
    assert_eq!(bench.fetches(), 0);
    assert!(named.named_action.is_none());
}
