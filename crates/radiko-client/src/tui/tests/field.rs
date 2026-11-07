use super::*;
use radiko::app::named::Operation as NamedOperation;
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer, layout::Position};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use unicode_segmentation::UnicodeSegmentation;
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

fn with_keyword(screen: &mut Screen, keyword: &str) {
    let committed = screen.app.committed.clone();
    screen.app.committed = SearchCriteria::new(
        committed.scope().clone(),
        committed.range().clone(),
        committed
            .filter()
            .with_keyword(Some(keyword.into()))
            .unwrap(),
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

fn x_after_marker(buffer: &Buffer, y: u16, marker: &str) -> u16 {
    let mut x = 0u16;
    let mut text = String::new();
    while x < buffer.area.width {
        let symbol = buffer[(x, y)].symbol();
        let advance = u16::try_from(UnicodeWidthStr::width(symbol).max(1)).unwrap_or(1);
        text.push_str(symbol);
        if let Some(start) = text.find(marker) {
            if text.len() == start + marker.len() {
                return x.saturating_add(advance);
            }
        }
        x = x.saturating_add(advance);
    }
    panic!("row {y} has no {marker:?}\n{}", row(buffer, y));
}

fn row(buffer: &Buffer, y: u16) -> String {
    let mut x = 0u16;
    let mut text = String::new();
    while x < buffer.area.width {
        let symbol = buffer[(x, y)].symbol();
        text.push_str(symbol);
        x = x.saturating_add(u16::try_from(UnicodeWidthStr::width(symbol).max(1)).unwrap_or(1));
    }
    text
}

struct Fetching {
    runtime: tokio::runtime::Runtime,
    fetches: Arc<AtomicUsize>,
    service: Service,
}

impl Fetching {
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
}

fn drain(runtime: &tokio::runtime::Runtime, service: &mut Service, screen: &mut Screen) {
    runtime.block_on(async {
        let start = tokio::time::Instant::now();
        while screen.app.status == SearchStatus::Loading && start.elapsed() < Duration::from_secs(2)
        {
            for event in service.poll().await {
                screen.apply(event);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    });
}

fn assert_idle(screen: &Screen, fetches: &AtomicUsize) {
    assert_eq!(
        fetches.load(Ordering::SeqCst),
        0,
        "movement or editing started a search"
    );
    assert_eq!(screen.app.status, SearchStatus::Idle);
    assert!(screen.playback_action.is_none());
}

fn keyword_cursor(screen: &Screen) -> usize {
    screen.input.as_ref().unwrap().cursor()
}

#[test]
fn keyword_condition_and_name_delete_i_from_aiu_and_backspace_ai() {
    let mut bench = Fetching::new();
    let _entered = bench.runtime.enter();
    let fetches = &bench.fetches;

    let mut keyword = screen();
    with_keyword(&mut keyword, "あいう");
    press(&mut keyword, &mut bench.service, KeyCode::Char('/'));
    assert_eq!(
        keyword_cursor(&keyword),
        "あいう".len(),
        "edit starts at the end"
    );
    press(&mut keyword, &mut bench.service, KeyCode::Home);
    assert_eq!(keyword_cursor(&keyword), 0);
    press(&mut keyword, &mut bench.service, KeyCode::Right);
    assert_eq!(
        keyword_cursor(&keyword),
        "あ".len(),
        "cursor is the byte before い"
    );
    let (buffer, cursor) = render(&mut keyword, 80, 30);
    let start = x_after_marker(&buffer, cursor.y, "キーワード: ");
    assert_eq!(buffer[(start, cursor.y)].symbol(), "あ");
    assert_eq!(
        buffer[(start + 2, cursor.y)].symbol(),
        "い",
        "い is the cell under the cursor"
    );
    assert_eq!(cursor.x, start + 2);
    assert_ne!(cursor.x, start + "あ".len() as u16);
    press(&mut keyword, &mut bench.service, KeyCode::Delete);
    assert_eq!(keyword.input.as_deref(), Some("あう"));
    assert_eq!(keyword_cursor(&keyword), "あ".len());
    assert_idle(&keyword, fetches);

    let mut end = screen();
    with_keyword(&mut end, "あい");
    press(&mut end, &mut bench.service, KeyCode::Char('/'));
    assert_eq!(keyword_cursor(&end), "あい".len());
    press(&mut end, &mut bench.service, KeyCode::Backspace);
    assert_eq!(end.input.as_deref(), Some("あ"));
    assert_eq!(keyword_cursor(&end), "あ".len());
    assert_idle(&end, fetches);

    let mut form = screen();
    press(&mut form, &mut bench.service, KeyCode::Char('f'));
    for _ in 0..5 {
        press(&mut form, &mut bench.service, KeyCode::Tab);
    }
    ctrl_u(&mut form, &mut bench.service);
    type_str(&mut form, &mut bench.service, "あいう");
    press(&mut form, &mut bench.service, KeyCode::Home);
    press(&mut form, &mut bench.service, KeyCode::Right);
    let (buffer, cursor) = render(&mut form, 80, 30);
    let start = x_after_marker(&buffer, cursor.y, "キーワード: ");
    assert_eq!(buffer[(cursor.x, cursor.y)].symbol(), "い");
    assert_eq!(cursor.x, start + 2);
    press(&mut form, &mut bench.service, KeyCode::Delete);
    assert_eq!(
        form.app.draft.as_ref().unwrap().filter().keyword(),
        Some("あう")
    );
    assert_eq!(form.app.committed.filter().keyword(), None);
    assert_idle(&form, fetches);

    let mut region = screen();
    press(&mut region, &mut bench.service, KeyCode::Char('f'));
    press(&mut region, &mut bench.service, KeyCode::Tab);
    press(&mut region, &mut bench.service, KeyCode::Tab);
    ctrl_u(&mut region, &mut bench.service);
    type_str(&mut region, &mut bench.service, "あいう");
    press(&mut region, &mut bench.service, KeyCode::Home);
    press(&mut region, &mut bench.service, KeyCode::Right);
    press(&mut region, &mut bench.service, KeyCode::Delete);
    let (buffer, cursor) = render(&mut region, 120, 30);
    let start = x_after_marker(&buffer, cursor.y, "地域（全国 / JP13,JP14 / 局:TBS）: ");
    assert_eq!(buffer[(start, cursor.y)].symbol(), "あ");
    assert_eq!(buffer[(cursor.x, cursor.y)].symbol(), "う");
    assert_eq!(cursor.x, start + 2);
    assert_eq!(region.app.committed.scope(), screen().app.committed.scope());
    assert_idle(&region, fetches);

    let mut named = screen();
    let committed = named.app.committed.clone();
    press(&mut named, &mut bench.service, KeyCode::Char('b'));
    named.named_action = None;
    named.named.as_mut().unwrap().listed(Default::default());
    press_char(&mut named, &mut bench.service, 'n');
    type_str(&mut named, &mut bench.service, "あいう");
    press(&mut named, &mut bench.service, KeyCode::Home);
    press(&mut named, &mut bench.service, KeyCode::Right);
    let (buffer, cursor) = render(&mut named, 80, 30);
    let start = x_after_marker(&buffer, cursor.y, "保存名: ");
    assert_eq!(buffer[(cursor.x, cursor.y)].symbol(), "い");
    assert_eq!(cursor.x, start + 2);
    press(&mut named, &mut bench.service, KeyCode::Delete);
    assert!(named.named_action.is_none());
    press(&mut named, &mut bench.service, KeyCode::End);
    press(&mut named, &mut bench.service, KeyCode::Backspace);
    press(&mut named, &mut bench.service, KeyCode::Backspace);
    type_str(&mut named, &mut bench.service, "あい");
    press(&mut named, &mut bench.service, KeyCode::Backspace);
    let (buffer, cursor) = render(&mut named, 80, 30);
    let start = x_after_marker(&buffer, cursor.y, "保存名: ");
    assert!(
        row(&buffer, cursor.y).contains("保存名: あ"),
        "{}",
        row(&buffer, cursor.y)
    );
    assert_eq!(cursor.x, start + UnicodeWidthStr::width("あ") as u16);
    assert_eq!(buffer[(cursor.x, cursor.y)].symbol(), " ");
    assert!(named.named_action.is_none());
    assert_eq!(named.app.committed, committed);
    assert_idle(&named, fetches);
}

#[test]
fn empty_edges_and_fullwidth_cursor_use_display_columns_in_the_cells() {
    let mut bench = Fetching::new();
    let _entered = bench.runtime.enter();
    let fetches = &bench.fetches;
    assert_eq!(UnicodeWidthStr::width("あ"), 2);
    assert_eq!("あ".len(), 3);
    assert_eq!(UnicodeWidthStr::width("aあ"), 3);
    assert_eq!("aあ".len(), 4);
    assert_eq!(UnicodeWidthStr::width("キーワード: "), 12);

    let mut empty = screen();
    press(&mut empty, &mut bench.service, KeyCode::Char('/'));
    for code in [
        KeyCode::Left,
        KeyCode::Right,
        KeyCode::Home,
        KeyCode::End,
        KeyCode::Backspace,
        KeyCode::Delete,
    ] {
        press(&mut empty, &mut bench.service, code);
    }
    assert_eq!(empty.input.as_deref(), Some(""));
    assert_eq!(keyword_cursor(&empty), 0);
    assert_idle(&empty, fetches);

    let mut edge = screen();
    with_keyword(&mut edge, "あいう");
    press(&mut edge, &mut bench.service, KeyCode::Char('/'));
    press(&mut edge, &mut bench.service, KeyCode::Home);
    press(&mut edge, &mut bench.service, KeyCode::Backspace);
    assert_eq!(edge.input.as_deref(), Some("あいう"));
    press(&mut edge, &mut bench.service, KeyCode::Delete);
    assert_eq!(edge.input.as_deref(), Some("いう"));
    assert_eq!(keyword_cursor(&edge), 0);
    press(&mut edge, &mut bench.service, KeyCode::End);
    press(&mut edge, &mut bench.service, KeyCode::Delete);
    assert_eq!(edge.input.as_deref(), Some("いう"));
    assert_idle(&edge, fetches);

    let mut mixed = screen();
    with_keyword(&mut mixed, "aあ");
    press(&mut mixed, &mut bench.service, KeyCode::Char('/'));
    let (buffer, cursor) = render(&mut mixed, 80, 30);
    let start = x_after_marker(&buffer, cursor.y, "キーワード: ");
    assert_eq!(buffer[(start, cursor.y)].symbol(), "a");
    assert_eq!(buffer[(start + 1, cursor.y)].symbol(), "あ");
    assert_eq!(cursor.x, start + 3, "cursor follows display width 3");
    assert_ne!(
        cursor.x,
        start + 4,
        "byte length 4 must not place the cursor"
    );
    assert_eq!(buffer[(cursor.x, cursor.y)].symbol(), " ");
    assert!(cursor.x < 80 && cursor.y < 30);
}

#[test]
fn narrow_scroll_and_resize_29_30_keep_the_cursor_in_the_viewport() {
    let mut bench = Fetching::new();
    let _entered = bench.runtime.enter();
    let mut screen = screen();
    let value = format!("{}中{}", "あ".repeat(30), "い".repeat(30));
    with_keyword(&mut screen, &value);
    press(&mut screen, &mut bench.service, KeyCode::Char('/'));
    for _ in 0..31 {
        press(&mut screen, &mut bench.service, KeyCode::Left);
    }
    let at_naka = keyword_cursor(&screen);
    assert_eq!(at_naka, "あ".repeat(30).len());
    assert!(super::super::field::is_grapheme_boundary(
        screen.input.as_deref().unwrap(),
        at_naka
    ));

    let (compact, compact_cursor) = render(&mut screen, 80, 29);
    let (normal, normal_cursor) = render(&mut screen, 80, 30);
    assert_eq!(keyword_cursor(&screen), at_naka);
    assert_eq!(
        compact_cursor.y, 0,
        "compact keyword cursor stays on the header row"
    );
    assert_eq!(
        normal_cursor.y, 27,
        "normal keyword cursor stays on the footer row"
    );
    assert_eq!(compact_cursor.x, normal_cursor.x);
    assert_eq!(compact[(compact_cursor.x, compact_cursor.y)].symbol(), "中");
    assert_eq!(normal[(normal_cursor.x, normal_cursor.y)].symbol(), "中");
    for (buffer, cursor) in [(&compact, compact_cursor), (&normal, normal_cursor)] {
        assert!(cursor.x < buffer.area.width && cursor.y < buffer.area.height);
        let start = x_after_marker(buffer, cursor.y, "キーワード: ");
        assert!(cursor.x >= start);
        assert!(
            cursor.x < buffer.area.width - 1,
            "cursor stays inside the frame"
        );
    }

    let (narrow_compact, narrow_compact_cursor) = render(&mut screen, 40, 29);
    let (narrow_normal, narrow_normal_cursor) = render(&mut screen, 40, 30);
    assert_eq!(keyword_cursor(&screen), at_naka);
    assert_eq!(
        narrow_compact[(narrow_compact_cursor.x, narrow_compact_cursor.y)].symbol(),
        "中"
    );
    assert_eq!(
        narrow_normal[(narrow_normal_cursor.x, narrow_normal_cursor.y)].symbol(),
        "中"
    );
    assert_ne!(narrow_compact_cursor.y, narrow_normal_cursor.y);

    let (scrolled, scrolled_cursor) = render(&mut screen, 20, 30);
    assert_eq!(keyword_cursor(&screen), at_naka);
    assert_eq!(
        scrolled[(scrolled_cursor.x, scrolled_cursor.y)].symbol(),
        "中"
    );
    assert!(
        scrolled_cursor.x < 19,
        "cursor stays left of the right border"
    );
    let field_start = x_after_marker(&scrolled, scrolled_cursor.y, "キーワード: ");
    assert!(scrolled_cursor.x >= field_start);
    assert!(
        !row(&scrolled, scrolled_cursor.y).contains('い'),
        "the window follows the cursor"
    );

    let cluster = format!("あ{}", "か\u{3099}");
    assert_eq!(cluster.graphemes(true).count(), 2);
    let mut grapheme = super::screen();
    with_keyword(&mut grapheme, &cluster);
    press(&mut grapheme, &mut bench.service, KeyCode::Char('/'));
    let view = grapheme.input.as_ref().unwrap().view(3);
    assert!(view.text.starts_with('か'), "{:?}", view.text);
    assert!(view.text.contains('\u{3099}'));
    assert!(!view.text.starts_with('\u{3099}'));
    assert!(super::super::field::is_grapheme_boundary(
        grapheme.input.as_deref().unwrap(),
        view.origin
    ));
    assert_eq!(
        &grapheme.input.as_deref().unwrap()[view.origin..view.origin + view.text.len()],
        view.text.as_str()
    );
    let (cells, cursor) = render(&mut grapheme, 17, 29);
    let start = x_after_marker(&cells, cursor.y, "キーワード: ");
    assert_eq!(
        cells[(start, cursor.y)].symbol(),
        "か\u{3099}",
        "the cell keeps the whole decomposed cluster"
    );
    assert_eq!(
        cursor.x,
        start + UnicodeWidthStr::width("か\u{3099}") as u16
    );
    assert!(cursor.x < 17 && cursor.y < 29);

    for (width, height) in [
        (0u16, 0u16),
        (1, 1),
        (2, 2),
        (3, 5),
        (8, 4),
        (14, 29),
        (14, 30),
    ] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| draw(frame, &mut screen, now()))
            .unwrap();
        let cursor = terminal.get_cursor_position().unwrap();
        if width == 0 || height == 0 {
            assert_eq!(cursor, Position::new(0, 0));
        } else {
            assert!(
                cursor.x < width && cursor.y < height,
                "{width}x{height} cursor {cursor:?}"
            );
        }
    }
    assert_eq!(keyword_cursor(&screen), at_naka);
    press(&mut screen, &mut bench.service, KeyCode::Delete);
    assert_eq!(
        screen.input.as_deref().unwrap(),
        format!("{}{}", "あ".repeat(30), "い".repeat(30))
    );
}

#[test]
fn movement_edits_and_escape_do_not_search_and_enter_commits() {
    let mut bench = Fetching::new();
    let _entered = bench.runtime.enter();
    let fetches = &bench.fetches;
    let mut screen = screen();
    screen.app.playback = Some(playing());
    with_keyword(&mut screen, "元");
    press(&mut screen, &mut bench.service, KeyCode::Char('/'));
    for code in [
        KeyCode::Left,
        KeyCode::Right,
        KeyCode::Home,
        KeyCode::End,
        KeyCode::Backspace,
        KeyCode::Delete,
        KeyCode::Char('あ'),
    ] {
        let result = screen.key(KeyEvent::new(code, KeyModifiers::NONE), &mut bench.service);
        assert_eq!(
            screen.app.status,
            SearchStatus::Idle,
            "{code:?} started a search: {result:?}"
        );
        assert_eq!(
            fetches.load(Ordering::SeqCst),
            0,
            "{code:?} issued a search request"
        );
        assert!(result.is_ok(), "{code:?} {result:?}");
        assert_eq!(screen.app.committed.filter().keyword(), Some("元"));
        assert!(screen.playback_action.is_none(), "editing must not seek");
    }
    ctrl_u(&mut screen, &mut bench.service);
    assert_idle(&screen, fetches);
    type_str(&mut screen, &mut bench.service, "あう");
    press(&mut screen, &mut bench.service, KeyCode::Esc);
    assert!(screen.input.is_none());
    assert!(screen.app.draft.is_none());
    assert_eq!(screen.app.committed.filter().keyword(), Some("元"));
    assert_idle(&screen, fetches);

    press(&mut screen, &mut bench.service, KeyCode::Char('/'));
    ctrl_u(&mut screen, &mut bench.service);
    type_str(&mut screen, &mut bench.service, "あう");
    press(&mut screen, &mut bench.service, KeyCode::Enter);
    assert_eq!(screen.app.status, SearchStatus::Loading);
    assert_eq!(screen.app.committed.filter().keyword(), Some("あう"));
    assert!(screen.input.is_none());
    bench.runtime.block_on(async {
        let start = tokio::time::Instant::now();
        while fetches.load(Ordering::SeqCst) == 0 && start.elapsed() < Duration::from_secs(2) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    });
    assert_eq!(fetches.load(Ordering::SeqCst), 1);
    drain(&bench.runtime, &mut bench.service, &mut screen);
    assert_ne!(screen.app.status, SearchStatus::Loading);

    fetches.store(0, Ordering::SeqCst);
    let mut form = super::screen();
    with_keyword(&mut form, "元");
    let committed = form.app.committed.clone();
    press(&mut form, &mut bench.service, KeyCode::Char('f'));
    for _ in 0..5 {
        press(&mut form, &mut bench.service, KeyCode::Tab);
    }
    type_str(&mut form, &mut bench.service, "追加");
    press(&mut form, &mut bench.service, KeyCode::Left);
    press(&mut form, &mut bench.service, KeyCode::Delete);
    assert_eq!(
        form.app.draft.as_ref().unwrap().filter().keyword(),
        Some("元追")
    );
    assert_eq!(form.app.committed, committed);
    assert_idle(&form, fetches);
    press(&mut form, &mut bench.service, KeyCode::Esc);
    assert!(form.filters.is_none());
    assert!(form.app.draft.is_none());
    assert_eq!(form.app.committed, committed);
    assert_idle(&form, fetches);

    press(&mut form, &mut bench.service, KeyCode::Char('f'));
    for _ in 0..5 {
        press(&mut form, &mut bench.service, KeyCode::Tab);
    }
    ctrl_u(&mut form, &mut bench.service);
    type_str(&mut form, &mut bench.service, "確定");
    press(&mut form, &mut bench.service, KeyCode::Enter);
    assert_eq!(form.app.status, SearchStatus::Loading);
    assert_eq!(form.app.committed.filter().keyword(), Some("確定"));
    bench.runtime.block_on(async {
        let start = tokio::time::Instant::now();
        while fetches.load(Ordering::SeqCst) == 0 && start.elapsed() < Duration::from_secs(2) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    });
    assert_eq!(fetches.load(Ordering::SeqCst), 1);

    fetches.store(0, Ordering::SeqCst);
    let mut named = super::screen();
    let committed = named.app.committed.clone();
    press(&mut named, &mut bench.service, KeyCode::Char('b'));
    named.named_action = None;
    named.named.as_mut().unwrap().listed(Default::default());
    press_char(&mut named, &mut bench.service, 'n');
    type_str(&mut named, &mut bench.service, "下書き");
    for code in [
        KeyCode::Left,
        KeyCode::Right,
        KeyCode::Home,
        KeyCode::End,
        KeyCode::Delete,
    ] {
        press(&mut named, &mut bench.service, code);
        assert!(named.named_action.is_none());
        assert_idle(&named, fetches);
    }
    press(&mut named, &mut bench.service, KeyCode::Esc);
    assert!(
        named
            .named
            .as_ref()
            .unwrap()
            .controls()
            .contains("Enter 呼び出し")
    );
    assert_eq!(named.app.committed, committed);
    assert_idle(&named, fetches);
    press_char(&mut named, &mut bench.service, 'n');
    type_str(&mut named, &mut bench.service, "保存名");
    press(&mut named, &mut bench.service, KeyCode::Enter);
    assert!(
        matches!(named.named_action, Some(NamedOperation::Save(ref name, _)) if name == "保存名")
    );
    assert_eq!(fetches.load(Ordering::SeqCst), 0);
    assert_eq!(named.app.status, SearchStatus::Idle);
    assert_eq!(named.app.committed, committed);
}

#[test]
fn combining_insertion_keeps_a_boundary_and_one_left_skips_the_cluster() {
    let mut bench = Fetching::new();
    let _entered = bench.runtime.enter();
    let fetches = &bench.fetches;
    let mut screen = screen();
    press(&mut screen, &mut bench.service, KeyCode::Char('/'));
    press_char(&mut screen, &mut bench.service, 'e');
    press_char(&mut screen, &mut bench.service, '\u{0301}');
    assert_eq!(screen.input.as_deref().unwrap().graphemes(true).count(), 1);
    press(&mut screen, &mut bench.service, KeyCode::Left);
    assert_eq!(keyword_cursor(&screen), 0);
    press(&mut screen, &mut bench.service, KeyCode::Delete);
    assert_eq!(screen.input.as_deref(), Some(""));

    press_char(&mut screen, &mut bench.service, '\u{0301}');
    press(&mut screen, &mut bench.service, KeyCode::Home);
    press_char(&mut screen, &mut bench.service, 'a');
    let cluster = "a\u{0301}";
    assert_eq!(screen.input.as_deref(), Some(cluster));
    assert_eq!(keyword_cursor(&screen), cluster.len());
    assert!(super::super::field::is_grapheme_boundary(
        screen.input.as_deref().unwrap(),
        keyword_cursor(&screen)
    ));
    press(&mut screen, &mut bench.service, KeyCode::Home);
    press(&mut screen, &mut bench.service, KeyCode::Right);
    assert_eq!(keyword_cursor(&screen), cluster.len());
    assert_ne!(keyword_cursor(&screen), 1);

    ctrl_u(&mut screen, &mut bench.service);
    press_char(&mut screen, &mut bench.service, 'か');
    press_char(&mut screen, &mut bench.service, '\u{3099}');
    assert_eq!(screen.input.as_deref().unwrap().graphemes(true).count(), 1);
    press(&mut screen, &mut bench.service, KeyCode::Left);
    assert_eq!(keyword_cursor(&screen), 0);

    ctrl_u(&mut screen, &mut bench.service);
    for c in "👨\u{200d}👩\u{200d}👧\u{200d}👦".chars() {
        press_char(&mut screen, &mut bench.service, c);
        assert!(super::super::field::is_grapheme_boundary(
            screen.input.as_deref().unwrap(),
            keyword_cursor(&screen)
        ));
    }
    assert_eq!(screen.input.as_deref().unwrap().graphemes(true).count(), 1);
    press(&mut screen, &mut bench.service, KeyCode::Left);
    assert_eq!(keyword_cursor(&screen), 0);
    assert_idle(&screen, fetches);
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
