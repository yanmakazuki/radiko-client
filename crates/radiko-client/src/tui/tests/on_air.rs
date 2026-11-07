use super::*;
use ratatui::{buffer::Buffer, style::Color};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use unicode_width::UnicodeWidthStr;

const NEAREST: &str = "現在時刻に近い番組を選択しました";
const NONE_MOVE: &str = "移動できる番組がありません";
const NEED_ROW: &str = "局で絞るには番組を選択してください";

fn clock() -> DateTime<FixedOffset> {
    radiko::api::stream::parse_time("20261015120000").unwrap()
}

struct Counts {
    fetches: Arc<AtomicUsize>,
    playbacks: Arc<AtomicUsize>,
}

impl Counts {
    fn new() -> Self {
        Self {
            fetches: Arc::new(AtomicUsize::new(0)),
            playbacks: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn service(&self) -> Service {
        let fetches = self.fetches.clone();
        Service::new(move || -> Result<CatalogClient, _> {
            fetches.fetch_add(1, Ordering::SeqCst);
            Err(radiko::api::catalog::CatalogError::InvalidDate)
        })
    }

    fn note_playback(&self, screen: &Screen) {
        if screen.playback_action.is_some() {
            self.playbacks.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn assert_quiet(&self, screen: &Screen) {
        self.note_playback(screen);
        assert_eq!(screen.app.status, SearchStatus::Idle, "fetch status");
        assert_eq!(self.fetches.load(Ordering::SeqCst), 0, "fetch counter");
        assert_eq!(self.playbacks.load(Ordering::SeqCst), 0, "playback counter");
        assert!(screen.named_action.is_none(), "save counter");
        assert!(screen.playback_action.is_none());
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
}

async fn quiesce(screen: &mut Screen, search: &mut Service) {
    tokio::time::sleep(std::time::Duration::from_millis(40)).await;
    for event in search.poll().await {
        screen.apply(event);
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

fn at(
    base: &radiko::domain::Program,
    station: &str,
    title: &str,
    start: &str,
    end: &str,
) -> radiko::domain::Program {
    let mut program = base.clone();
    program.key.station_id = station.to_owned();
    program.title = title.to_owned();
    program.program_id = title.to_owned();
    program.key.starts_at = radiko::api::stream::parse_time(start).unwrap();
    program.ends_at = radiko::api::stream::parse_time(end).unwrap();
    program
}

fn show(screen: &mut Screen, programs: Vec<radiko::domain::Program>, selected: Option<usize>) {
    let len = programs.len();
    screen.app.programs = programs;
    screen.app.visible = (0..len).collect();
    screen.live_mode = false;
    screen.live_rows.clear();
    screen.list.select(selected);
    screen.set_list_now(clock());
}

fn tap(screen: &mut Screen, search: &mut Service, code: KeyCode) {
    assert!(
        !screen
            .key(KeyEvent::new(code, KeyModifiers::NONE), search)
            .unwrap()
    );
}

fn title_at(screen: &Screen, index: usize) -> &str {
    let program = screen.rows()[index];
    screen.app.programs[program].title.as_str()
}

fn order(screen: &Screen) -> Vec<String> {
    screen
        .rows()
        .iter()
        .map(|index| screen.app.programs[*index].title.clone())
        .collect()
}

fn rich_criteria() -> SearchCriteria {
    SearchCriteria::new(
        Scope::Areas(vec!["JP13".into(), "JP14".into()]),
        radiko::domain::BroadcastRange::new(
            chrono::NaiveDate::from_ymd_opt(2026, 10, 1).unwrap(),
            chrono::NaiveDate::from_ymd_opt(2026, 10, 3).unwrap(),
        )
        .unwrap(),
        radiko::domain::search::Filter::new(
            vec!["P1".into()],
            vec!["N2".into()],
            true,
            false,
            Some("朝刊".into()),
        )
        .unwrap(),
    )
    .unwrap()
}

fn draw_at(screen: &mut Screen, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| draw(frame, screen, clock())).unwrap();
    terminal.backend().buffer().clone()
}

fn row_text(buffer: &Buffer, y: u16) -> String {
    let mut line = String::new();
    let mut x = 0;
    while x < buffer.area.width {
        let symbol = buffer[(x, y)].symbol();
        line.push_str(symbol);
        x += UnicodeWidthStr::width(symbol).max(1) as u16;
    }
    line
}

fn cell_text(buffer: &Buffer) -> String {
    (0..buffer.area.height)
        .map(|y| row_text(buffer, y))
        .collect::<Vec<_>>()
        .concat()
}

/// The literal is spelled by successive cell symbols, not by a painted rectangle.
fn assert_cells_spell(buffer: &Buffer, literal: &str) {
    assert!(
        cell_text(buffer).contains(literal),
        "cells do not contain {literal:?}\n{}",
        (0..buffer.area.height)
            .map(|y| row_text(buffer, y))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let mut found = false;
    for y in 0..buffer.area.height {
        let line = row_text(buffer, y);
        let Some(byte) = line.find(literal) else {
            continue;
        };
        let mut x = 0u16;
        let mut seen = String::new();
        while x < buffer.area.width && seen.len() < byte {
            let symbol = buffer[(x, y)].symbol();
            seen.push_str(symbol);
            x += UnicodeWidthStr::width(symbol).max(1) as u16;
        }
        assert_eq!(seen.len(), byte, "cell walk missed the literal start");
        let mut rest = literal;
        let origin = (x, y);
        while !rest.is_empty() {
            assert!(x < buffer.area.width, "literal ran off the row: {literal}");
            let symbol = buffer[(x, y)].symbol();
            assert!(
                rest.starts_with(symbol),
                "cell ({x},{y}) is {symbol:?}, remaining {rest:?}"
            );
            rest = &rest[symbol.len()..];
            x += UnicodeWidthStr::width(symbol).max(1) as u16;
        }
        let cell = &buffer[origin];
        assert_ne!(cell.fg, Color::Red, "info text was drawn as an error");
        assert_ne!(cell.fg, Color::Yellow, "info text was drawn as a warning");
        assert!(!cell.modifier.contains(ratatui::style::Modifier::BOLD));
        found = true;
        break;
    }
    assert!(found, "no row spelled {literal:?} in its cells");
}

fn content_rows(buffer: &Buffer) -> Vec<String> {
    (0..buffer.area.height)
        .map(|y| {
            let line = row_text(buffer, y);
            let line = line.strip_prefix('│').unwrap_or(&line);
            let line = line.strip_suffix('│').unwrap_or(line);
            line.trim_end().to_owned()
        })
        .collect()
}

fn assert_notice(screen: &mut Screen, notice: &str) {
    for (width, height) in [(80, 24), (160, 40), (80, 29), (80, 30)] {
        let buffer = draw_at(screen, width, height);
        assert_cells_spell(&buffer, notice);
        assert!(
            !content_rows(&buffer)
                .iter()
                .any(|line| line.contains('✕') && line.contains(notice)),
            "{width}x{height} marked an information notice as an error"
        );
    }
    let narrow = draw_at(screen, 24, 8);
    let joined = content_rows(&narrow).concat();
    assert!(
        joined.contains(notice),
        "wrapped cells dropped {notice:?}\n{joined}"
    );
    let first = notice.chars().next().unwrap().to_string();
    let mut marked = false;
    for y in 0..narrow.area.height {
        let mut x = 0u16;
        while x < narrow.area.width {
            if narrow[(x, y)].symbol() == first {
                assert_ne!(narrow[(x, y)].fg, Color::Red);
                assert_ne!(narrow[(x, y)].fg, Color::Yellow);
                marked = true;
                break;
            }
            x += UnicodeWidthStr::width(narrow[(x, y)].symbol()).max(1) as u16;
        }
        if marked {
            break;
        }
    }
    assert!(marked, "the first notice cell was not drawn");
    let tiny = draw_at(screen, 10, 4);
    assert!(!tiny.content.is_empty(), "tiny terminal did not draw cells");
}

fn highlighted_row(buffer: &Buffer) -> (u16, String) {
    (0..buffer.area.height)
        .map(|y| (y, row_text(buffer, y)))
        .find(|(_, line)| line.contains('▶'))
        .unwrap_or_else(|| {
            panic!(
                "no highlighted program row\n{}",
                (0..buffer.area.height)
                    .map(|y| row_text(buffer, y))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        })
}

#[test]
fn n_uses_an_injected_clock_and_keeps_fetch_and_playback_at_zero() {
    runtime().block_on(async {
        let counts = Counts::new();
        let mut search = counts.service();
        let base = template();
        let mut screen = screen();
        screen.app.playback = Some(playing());
        let before_state = screen.app.playback.as_ref().map(|play| play.state);
        let before_position = screen.app.playback.as_ref().and_then(|play| play.position);
        show(
            &mut screen,
            vec![
                at(&base, "TBS", "ENDED", "20261015110000", "20261015120000"),
                at(&base, "QRR", "START", "20261015120000", "20261015130000"),
                at(&base, "RN1", "LATER", "20261015130000", "20261015140000"),
            ],
            Some(0),
        );
        let order_before = order(&screen);
        tap(&mut screen, &mut search, KeyCode::Char('n'));
        assert_eq!(title_at(&screen, screen.list.selected().unwrap()), "START");
        assert!(
            screen.playback_notice.is_empty(),
            "an exact start is on air, so the nearest-start notice stays off"
        );
        tap(&mut screen, &mut search, KeyCode::Char('n'));
        assert_eq!(screen.list.selected(), Some(1), "repeated n does not cycle");
        assert_eq!(order(&screen), order_before);
        assert_eq!(
            screen.app.playback.as_ref().map(|play| play.state),
            before_state
        );
        assert_eq!(
            screen.app.playback.as_ref().and_then(|play| play.position),
            before_position
        );
        counts.assert_quiet(&screen);
        quiesce(&mut screen, &mut search).await;
        counts.assert_quiet(&screen);

        show(
            &mut screen,
            vec![at(
                &base,
                "TBS",
                "ENDED",
                "20261015110000",
                "20261015120000",
            )],
            Some(0),
        );
        tap(&mut screen, &mut search, KeyCode::Char('n'));
        assert_eq!(screen.list.selected(), Some(0));
        assert_eq!(screen.playback_notice, NEAREST);
        assert_notice(&mut screen, NEAREST);
        counts.assert_quiet(&screen);
    });
}

#[test]
fn n_stays_on_the_current_on_air_row_and_prefers_display_order() {
    runtime().block_on(async {
        let counts = Counts::new();
        let mut search = counts.service();
        let base = template();
        let mut screen = screen();
        show(
            &mut screen,
            vec![
                at(&base, "TBS", "NOW", "20261015115900", "20261015125900"),
                at(&base, "QRR", "ALSO", "20261015110000", "20261015130000"),
            ],
            Some(0),
        );
        tap(&mut screen, &mut search, KeyCode::Char('n'));
        tap(&mut screen, &mut search, KeyCode::Char('n'));
        assert_eq!(title_at(&screen, 0), "NOW");
        assert_eq!(screen.list.selected(), Some(0));
        assert!(screen.playback_notice.is_empty());
        counts.assert_quiet(&screen);

        show(
            &mut screen,
            vec![
                at(&base, "TBS", "UPCOMING", "20261015140000", "20261015150000"),
                at(&base, "QRR", "SECOND", "20261015115500", "20261015125500"),
                at(
                    &base,
                    "RN1",
                    "FIRSTSTART",
                    "20261015100000",
                    "20261015130000",
                ),
            ],
            Some(0),
        );
        tap(&mut screen, &mut search, KeyCode::Char('n'));
        assert_eq!(
            title_at(&screen, screen.list.selected().unwrap()),
            "SECOND",
            "the first on-air row in displayed order wins, not the earlier start"
        );
        assert!(screen.playback_notice.is_empty());
        assert_eq!(
            order(&screen),
            vec!["UPCOMING".to_owned(), "SECOND".into(), "FIRSTSTART".into()]
        );
        counts.assert_quiet(&screen);
        quiesce(&mut screen, &mut search).await;
        counts.assert_quiet(&screen);
    });
}

#[test]
fn n_fallback_is_start_distance_and_keeps_the_earlier_tie() {
    runtime().block_on(async {
        let counts = Counts::new();
        let mut search = counts.service();
        let base = template();
        let mut screen = screen();
        show(
            &mut screen,
            vec![
                at(&base, "TBS", "EARLYAIR", "20261015090000", "20261015150000"),
                at(&base, "QRR", "CLOSE", "20261015115000", "20261015115900"),
                at(&base, "RN1", "LATER", "20261015140000", "20261015150000"),
            ],
            Some(1),
        );
        tap(&mut screen, &mut search, KeyCode::Char('n'));
        assert_eq!(
            title_at(&screen, screen.list.selected().unwrap()),
            "CLOSE",
            "an earlier on-air row does not beat a closer start"
        );
        assert_eq!(screen.playback_notice, NEAREST);
        counts.assert_quiet(&screen);

        show(
            &mut screen,
            vec![
                at(&base, "TBS", "PAST", "20261015110000", "20261015113000"),
                at(&base, "QRR", "NEXT", "20261015130000", "20261015140000"),
            ],
            Some(1),
        );
        tap(&mut screen, &mut search, KeyCode::Char('n'));
        assert_eq!(screen.list.selected(), Some(0));
        assert_eq!(title_at(&screen, 0), "PAST");
        assert_eq!(screen.playback_notice, NEAREST);
        counts.assert_quiet(&screen);
        quiesce(&mut screen, &mut search).await;
        counts.assert_quiet(&screen);
    });
}

#[test]
fn n_on_an_empty_displayed_list_does_not_search_hidden_rows() {
    runtime().block_on(async {
        let counts = Counts::new();
        let mut search = counts.service();
        let base = template();
        let mut screen = screen();
        show(&mut screen, vec![], None);
        tap(&mut screen, &mut search, KeyCode::Char('n'));
        assert_eq!(screen.playback_notice, NONE_MOVE);
        assert!(screen.list.selected().is_none());
        assert_notice(&mut screen, NONE_MOVE);
        counts.assert_quiet(&screen);

        show(
            &mut screen,
            vec![
                at(&base, "TBS", "HIDDEN", "20261015120000", "20261015130000"),
                at(
                    &base,
                    "QRR",
                    "ALSOHIDDEN",
                    "20261015110000",
                    "20261015140000",
                ),
            ],
            Some(0),
        );
        screen.live_mode = true;
        screen.live_rows.clear();
        tap(&mut screen, &mut search, KeyCode::Char('n'));
        assert_eq!(screen.playback_notice, NONE_MOVE);
        assert_eq!(screen.app.visible, vec![0, 1]);
        counts.assert_quiet(&screen);

        screen.live_rows = vec![1];
        screen.list.select(Some(0));
        screen.playback_notice.clear();
        tap(&mut screen, &mut search, KeyCode::Char('n'));
        assert_eq!(
            title_at(&screen, screen.list.selected().unwrap()),
            "ALSOHIDDEN"
        );
        assert!(screen.playback_notice.is_empty());
        tap(&mut screen, &mut search, KeyCode::Char('n'));
        assert_eq!(screen.list.selected(), Some(0));
        quiesce(&mut screen, &mut search).await;
        counts.assert_quiet(&screen);
    });
}

#[test]
fn n_scrolls_the_selected_on_air_row_into_the_list_cells() {
    runtime().block_on(async {
        let counts = Counts::new();
        let mut search = counts.service();
        let base = template();
        let mut programs = Vec::new();
        for index in 0..40 {
            let start = if index == 30 {
                "20261015120000"
            } else {
                "20261015060000"
            };
            let end = if index == 30 {
                "20261015130000"
            } else {
                "20261015070000"
            };
            programs.push(at(&base, "TBS", &format!("AIR{index:02}"), start, end));
        }
        let mut screen = screen();
        show(&mut screen, programs, Some(0));
        tap(&mut screen, &mut search, KeyCode::Char('n'));
        assert_eq!(screen.list.selected(), Some(30));
        assert!(screen.playback_notice.is_empty());
        counts.assert_quiet(&screen);
        for (width, height) in [(80, 24), (160, 40), (80, 29), (80, 30), (40, 12)] {
            let buffer = draw_at(&mut screen, width, height);
            let (y, row) = highlighted_row(&buffer);
            assert!(
                row.contains("12:00"),
                "{width}x{height} highlight is not the on-air slot: {row}"
            );
            assert!(
                !row.contains("06:00"),
                "{width}x{height} highlighted an earlier row: {row}"
            );
            if width >= 80 {
                assert!(
                    row.contains("AIR30"),
                    "{width}x{height} list row hid the title: {row}"
                );
            }
            assert!(
                cell_text(&buffer).contains("AIR30"),
                "{width}x{height} did not draw the selected title"
            );
            assert!(
                !cell_text(&buffer).contains("AIR00"),
                "{width}x{height} left the first row on screen"
            );
            let mut x = 0u16;
            while x < buffer.area.width && buffer[(x, y)].symbol() != "▶" {
                x += UnicodeWidthStr::width(buffer[(x, y)].symbol()).max(1) as u16;
            }
            assert_eq!(buffer[(x, y)].symbol(), "▶");
            let body: String = (0..buffer.area.height)
                .map(|y| row_text(&buffer, y))
                .filter(|line| line.contains('▶') || line.contains("AIR"))
                .collect::<Vec<_>>()
                .join("\n");
            assert!(body.contains("AIR30"), "{width}x{height}\n{body}");
            assert!(
                !body
                    .lines()
                    .any(|line| line.contains('▶') && line.contains("AIR00")),
                "the old row stayed highlighted\n{body}"
            );
            let selected = screen.list.selected().unwrap();
            let offset = screen.list.offset();
            let height_rows = screen.list_height.max(1);
            assert!(
                (offset..offset + height_rows).contains(&selected),
                "{width}x{height} selected {selected} offset {offset} height {height_rows}"
            );
        }
        let tiny = draw_at(&mut screen, 10, 4);
        assert!(!tiny.content.is_empty());
        quiesce(&mut screen, &mut search).await;
        counts.assert_quiet(&screen);
    });
}

#[test]
fn o_copies_committed_filters_and_only_the_selected_station() {
    runtime().block_on(async {
        let counts = Counts::new();
        let mut search = counts.service();
        let base = template();
        let mut screen = screen();
        let committed = rich_criteria();
        screen.app.committed = committed.clone();
        let stale = SearchCriteria::new(
            Scope::Station("FMJ".into()),
            committed.range().clone(),
            radiko::domain::search::Filter::new(
                vec![],
                vec![],
                false,
                false,
                Some("古い下書き".into()),
            )
            .unwrap(),
        )
        .unwrap();
        screen.app.begin_edit();
        screen.app.edit_criteria(stale).unwrap();
        show(
            &mut screen,
            vec![
                at(&base, "TBS", "ROW0", "20261015100000", "20261015110000"),
                at(&base, "QRR", "ROW1", "20261015110000", "20261015120000"),
                at(&base, "RN1", "ROW2", "20261015120000", "20261015130000"),
            ],
            Some(2),
        );
        tap(&mut screen, &mut search, KeyCode::Char('o'));
        assert!(screen.filters.is_some());
        let draft = screen.app.draft.as_ref().unwrap();
        assert_eq!(draft.scope(), &Scope::Station("RN1".into()));
        assert_eq!(draft.range(), committed.range());
        assert_eq!(draft.filter().keyword(), Some("朝刊"));
        assert_eq!(draft.filter().program_genres(), &["P1".to_owned()]);
        assert_eq!(draft.filter().personality_genres(), &["N2".to_owned()]);
        assert!(draft.filter().unclassified_program());
        assert!(!draft.filter().unclassified_personality());
        assert_eq!(screen.app.committed, committed);
        counts.assert_quiet(&screen);
        for (width, height) in [(80, 24), (160, 40), (80, 29), (80, 30)] {
            let buffer = draw_at(&mut screen, width, height);
            let text = cell_text(&buffer);
            assert!(
                text.contains("局:RN1"),
                "{width}x{height} hid the selected station\n{text}"
            );
            assert!(
                text.contains(": 局:RN1") || text.contains(":局:RN1"),
                "{width}x{height} did not show RN1 as the field value\n{text}"
            );
            assert!(text.contains("朝刊"), "{width}x{height}\n{text}");
            assert!(text.contains("20261001"), "{width}x{height}\n{text}");
            assert!(text.contains("20261003"), "{width}x{height}\n{text}");
            assert!(text.contains("P1"), "{width}x{height}\n{text}");
            assert!(text.contains("N2"), "{width}x{height}\n{text}");
            assert!(!text.contains("古い下書き"), "{text}");
            assert!(!text.contains("局:FMJ"), "{text}");
            assert!(!text.contains("局:QRR"), "{text}");
            let station = (0..buffer.area.height)
                .map(|y| row_text(&buffer, y))
                .find(|line| line.contains("局:RN1"))
                .expect("station row");
            let byte = station.find("局:RN1").unwrap();
            let mut x = 0u16;
            let mut seen = String::new();
            let y = (0..buffer.area.height)
                .find(|y| row_text(&buffer, *y).contains("局:RN1"))
                .unwrap();
            while x < buffer.area.width && seen.len() < byte {
                let symbol = buffer[(x, y)].symbol();
                seen.push_str(symbol);
                x += UnicodeWidthStr::width(symbol).max(1) as u16;
            }
            assert_eq!(buffer[(x, y)].symbol(), "局");
        }
        let tiny = draw_at(&mut screen, 10, 4);
        assert!(!tiny.content.is_empty());
        quiesce(&mut screen, &mut search).await;
        counts.assert_quiet(&screen);
    });
}

#[test]
fn o_without_a_row_does_not_guess_a_station() {
    runtime().block_on(async {
        let counts = Counts::new();
        let mut search = counts.service();
        let base = template();
        let mut screen = screen();
        let committed = rich_criteria();
        screen.app.committed = committed.clone();
        show(
            &mut screen,
            vec![at(&base, "TBS", "ROW0", "20261015120000", "20261015130000")],
            None,
        );
        tap(&mut screen, &mut search, KeyCode::Char('o'));
        assert!(screen.filters.is_none());
        assert!(screen.app.draft.is_none());
        assert_eq!(screen.app.committed, committed);
        assert_eq!(screen.playback_notice, NEED_ROW);
        assert_notice(&mut screen, NEED_ROW);
        counts.assert_quiet(&screen);

        screen.playback_notice.clear();
        screen.list.select(Some(0));
        screen.app.programs[0].key.station_id = "bad id".into();
        tap(&mut screen, &mut search, KeyCode::Char('o'));
        assert!(screen.filters.is_none());
        assert!(screen.app.draft.is_none());
        assert_eq!(screen.app.committed, committed);
        assert_eq!(screen.playback_notice, "保存する局IDが不正です");
        quiesce(&mut screen, &mut search).await;
        counts.assert_quiet(&screen);
    });
}

#[test]
fn o_enter_fetches_once_and_escape_leaves_committed_unchanged() {
    runtime().block_on(async {
        let counts = Counts::new();
        let mut search = counts.service();
        let base = template();
        let mut screen = screen();
        let committed = rich_criteria();
        screen.app.committed = committed.clone();
        show(
            &mut screen,
            vec![at(&base, "QRR", "ROW", "20261015120000", "20261015130000")],
            Some(0),
        );
        tap(&mut screen, &mut search, KeyCode::Char('o'));
        assert_eq!(screen.app.status, SearchStatus::Idle);
        assert!(screen.filters.is_some());
        for _ in 0..5 {
            tap(&mut screen, &mut search, KeyCode::Tab);
        }
        tap(&mut screen, &mut search, KeyCode::Char('X'));
        assert_eq!(
            screen.app.draft.as_ref().unwrap().filter().keyword(),
            Some("朝刊X")
        );
        tap(&mut screen, &mut search, KeyCode::Esc);
        assert!(screen.filters.is_none());
        assert!(screen.app.draft.is_none());
        assert_eq!(screen.app.committed, committed);
        quiesce(&mut screen, &mut search).await;
        counts.assert_quiet(&screen);

        tap(&mut screen, &mut search, KeyCode::Char('o'));
        assert_eq!(
            screen.app.draft.as_ref().unwrap().scope(),
            &Scope::Station("QRR".into())
        );
        assert_eq!(screen.app.status, SearchStatus::Idle);
        quiesce(&mut screen, &mut search).await;
        counts.assert_quiet(&screen);
        tap(&mut screen, &mut search, KeyCode::Enter);
        assert!(screen.filters.is_none());
        assert_eq!(screen.app.status, SearchStatus::Loading);
        assert_eq!(screen.app.committed.scope(), &Scope::Station("QRR".into()));
        assert_eq!(screen.app.committed.filter().keyword(), Some("朝刊"));
        assert_eq!(screen.app.committed.range(), committed.range());
        let began = std::time::Instant::now();
        while screen.app.status == SearchStatus::Loading {
            assert!(began.elapsed() < std::time::Duration::from_secs(3));
            for event in search.poll().await {
                screen.apply(event);
            }
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
        assert_eq!(counts.fetches.load(Ordering::SeqCst), 1);
        counts.note_playback(&screen);
        assert_eq!(counts.playbacks.load(Ordering::SeqCst), 0);
        assert!(screen.named_action.is_none());
        assert_ne!(screen.app.status, SearchStatus::Loading);
    });
}

#[test]
fn n_and_o_are_typed_in_fields_and_do_not_run_on_other_surfaces() {
    runtime().block_on(async {
        let counts = Counts::new();
        let mut search = counts.service();
        let base = template();

        let mut keyword = screen();
        show(
            &mut keyword,
            vec![at(&base, "TBS", "ROW", "20261015100000", "20261015110000")],
            Some(0),
        );
        tap(&mut keyword, &mut search, KeyCode::Char('/'));
        tap(&mut keyword, &mut search, KeyCode::Char('n'));
        tap(&mut keyword, &mut search, KeyCode::Char('o'));
        assert_eq!(keyword.input.as_deref(), Some("no"));
        assert!(keyword.filters.is_none());
        assert_eq!(keyword.list.selected(), Some(0));
        let keyword_cells = draw_at(&mut keyword, 80, 24);
        assert_cells_spell(&keyword_cells, "キーワード: no");
        counts.assert_quiet(&keyword);

        let mut form = screen();
        let committed = form.app.committed.clone();
        tap(&mut form, &mut search, KeyCode::Char('f'));
        tap(&mut form, &mut search, KeyCode::Tab);
        tap(&mut form, &mut search, KeyCode::Char('n'));
        tap(&mut form, &mut search, KeyCode::Char('o'));
        let form_cells = draw_at(&mut form, 80, 30);
        assert!(
            cell_text(&form_cells).contains("終了日（空欄は単日）: no"),
            "{}",
            cell_text(&form_cells)
        );
        assert_eq!(form.app.committed, committed);
        assert_eq!(form.app.status, SearchStatus::Idle);
        tap(&mut form, &mut search, KeyCode::Esc);
        assert!(form.filters.is_none());
        assert!(form.app.draft.is_none());

        let mut scope = screen();
        tap(&mut scope, &mut search, KeyCode::Char('f'));
        tap(&mut scope, &mut search, KeyCode::Tab);
        tap(&mut scope, &mut search, KeyCode::Tab);
        tap(&mut scope, &mut search, KeyCode::Char('n'));
        tap(&mut scope, &mut search, KeyCode::Char('o'));
        let scope_cells = draw_at(&mut scope, 80, 24);
        assert!(
            cell_text(&scope_cells).contains("局:TBSno"),
            "{}",
            cell_text(&scope_cells)
        );
        assert_eq!(scope.app.status, SearchStatus::Idle);
        tap(&mut scope, &mut search, KeyCode::Esc);

        let mut named_screen = screen();
        tap(&mut named_screen, &mut search, KeyCode::Char('b'));
        assert!(matches!(
            named_screen.named_action.take(),
            Some(NamedOperation::List)
        ));
        let view = named_screen.named.as_ref().unwrap().view;
        named_screen
            .named_event(radiko::app::named::Event {
                view,
                mutating: false,
                result: Ok(radiko::app::named::Outcome::Listed(Default::default())),
            })
            .unwrap();
        tap(&mut named_screen, &mut search, KeyCode::Char('o'));
        assert!(named_screen.named.is_some());
        assert!(named_screen.filters.is_none());
        assert!(named_screen.named_action.is_none());
        tap(&mut named_screen, &mut search, KeyCode::Char('n'));
        tap(&mut named_screen, &mut search, KeyCode::Char('n'));
        tap(&mut named_screen, &mut search, KeyCode::Char('o'));
        let name_cells = draw_at(&mut named_screen, 80, 24);
        assert!(
            cell_text(&name_cells).contains("保存名: no"),
            "{}",
            cell_text(&name_cells)
        );
        assert!(
            named_screen.named_action.is_none(),
            "typing the name must not save"
        );
        tap(&mut named_screen, &mut search, KeyCode::Esc);
        assert!(named_screen.named.is_some());
        assert!(named_screen.filters.is_none());

        let mut login = screen();
        tap(&mut login, &mut search, KeyCode::Char('I'));
        tap(&mut login, &mut search, KeyCode::Char('n'));
        tap(&mut login, &mut search, KeyCode::Char('o'));
        let login_cells = draw_at(&mut login, 80, 24);
        assert!(
            cell_text(&login_cells)
                .replace(' ', "")
                .contains("メールアドレス:no"),
            "{}",
            cell_text(&login_cells)
        );
        assert!(login.login_requested.is_none());
        assert!(login.login.is_some());

        let mut seconds = screen();
        seconds.app.playback = Some(playing());
        tap(&mut seconds, &mut search, KeyCode::Char('g'));
        tap(&mut seconds, &mut search, KeyCode::Char('n'));
        tap(&mut seconds, &mut search, KeyCode::Char('o'));
        assert_eq!(seconds.seek.as_ref().unwrap().input, "");
        assert!(seconds.seek.is_some());

        let mut help = screen();
        show(
            &mut help,
            vec![at(&base, "QRR", "ROW", "20261015120000", "20261015130000")],
            Some(0),
        );
        tap(&mut help, &mut search, KeyCode::Char('?'));
        tap(&mut help, &mut search, KeyCode::Char('n'));
        tap(&mut help, &mut search, KeyCode::Char('o'));
        assert!(help.help_open);
        assert_eq!(help.list.selected(), Some(0));
        assert!(help.filters.is_none());
        assert_ne!(help.playback_notice, NEAREST);

        let mut device = screen();
        show(
            &mut device,
            vec![at(&base, "QRR", "ROW", "20261015120000", "20261015130000")],
            Some(0),
        );
        device.device_open = true;
        tap(&mut device, &mut search, KeyCode::Char('n'));
        tap(&mut device, &mut search, KeyCode::Char('o'));
        assert!(device.device_open);
        assert_eq!(device.list.selected(), Some(0));
        assert!(device.filters.is_none());

        let mut failures = screen();
        failures.failures_open = true;
        tap(&mut failures, &mut search, KeyCode::Char('n'));
        tap(&mut failures, &mut search, KeyCode::Char('o'));
        assert!(failures.failures_open);
        assert!(failures.filters.is_none());

        let mut chapters = screen();
        show(
            &mut chapters,
            vec![at(&base, "QRR", "ROW", "20261015120000", "20261015130000")],
            Some(0),
        );
        chapters
            .open_chapters(chapters.selected().unwrap().clone())
            .unwrap();
        tap(&mut chapters, &mut search, KeyCode::Char('n'));
        tap(&mut chapters, &mut search, KeyCode::Char('o'));
        assert!(chapters.app.chapters.target.is_some());
        assert_eq!(chapters.list.selected(), Some(0));
        assert!(chapters.filters.is_none());
        assert!(chapters.playback_action.is_none());

        let mut logout = screen();
        logout.logout_confirm = true;
        tap(&mut logout, &mut search, KeyCode::Char('o'));
        assert!(logout.logout_confirm);
        assert!(logout.filters.is_none());
        tap(&mut logout, &mut search, KeyCode::Char('n'));
        assert!(!logout.logout_confirm);

        let mut modified = screen();
        show(
            &mut modified,
            vec![
                at(&base, "TBS", "EARLY", "20261015100000", "20261015110000"),
                at(&base, "QRR", "NOW", "20261015120000", "20261015130000"),
            ],
            Some(0),
        );
        assert!(
            !modified
                .key(
                    KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL),
                    &mut search,
                )
                .unwrap()
        );
        assert_eq!(modified.list.selected(), Some(0));
        assert!(
            !modified
                .key(
                    KeyEvent::new(KeyCode::Char('o'), KeyModifiers::ALT),
                    &mut search,
                )
                .unwrap()
        );
        assert!(modified.filters.is_none());
        quiesce(&mut modified, &mut search).await;
        counts.assert_quiet(&modified);
    });
}

fn playing() -> radiko::player::state::PlaybackSnapshot {
    let start = chrono::DateTime::parse_from_rfc3339("2026-10-15T03:00:00+09:00").unwrap();
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
