use super::*;
use crate::tui::truncate::{display_width, fit_after_prefix, fit_label, list_text_columns};
use radiko::domain::{Chapter, ChapterSet};
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    text::Span,
    widgets::{Block, Borders},
};
use std::time::Duration;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const ACUTE: &str = "e\u{0301}";
const COENG: &str = "ក\u{17d2}ក";
const FAMILY: &str = "👨\u{200d}👩\u{200d}👧\u{200d}👦";

fn base_program() -> radiko::domain::Program {
    radiko::api::catalog::parse_programs(
        include_bytes!("../../../../../tests/fixtures/schedule.xml"),
        "TBS",
        radiko::api::catalog::parse_day("20261015").unwrap(),
    )
    .unwrap()
    .programs
    .remove(0)
}

fn on_air(program: &radiko::domain::Program) -> DateTime<FixedOffset> {
    program.key.starts_at + chrono::Duration::minutes(1)
}

fn program_with(title: &str, station: &str) -> radiko::domain::Program {
    let mut program = base_program();
    program.title = title.to_owned();
    program.key.station_id = station.to_owned();
    program
}

fn show_programs(programs: Vec<radiko::domain::Program>, selected: bool) -> Screen {
    let mut screen = screen();
    let visible: Vec<_> = (0..programs.len()).collect();
    screen.app.programs = programs;
    screen.app.visible = visible;
    screen.list.select(if selected { Some(0) } else { None });
    screen
}

fn program_prefix(program: &radiko::domain::Program, now: DateTime<FixedOffset>) -> String {
    let status = match radiko::domain::broadcast_status(program.key.starts_at, program.ends_at, now)
    {
        Ok(radiko::domain::BroadcastStatus::Upcoming) => "放送前",
        Ok(radiko::domain::BroadcastStatus::OnAir) => "放送中",
        Ok(radiko::domain::BroadcastStatus::Finished) => "放送終了",
        Err(_) => "日時不正",
    };
    format!(
        "{} {} [{status}] ",
        local(program.key.starts_at).format("%m/%d %H:%M"),
        program.key.station_id
    )
}

fn chapter_prefix(marker: bool, start: Duration, end: Duration) -> String {
    format!(
        "{}{:.1}〜{:.1}秒 ",
        if marker { "[再生位置] " } else { "" },
        start.as_secs_f64(),
        end.as_secs_f64()
    )
}

fn show_chapters(titles: &[&str], selected: bool) -> Screen {
    let mut screen = screen();
    let mut target = base_program();
    target.title = "対象番組".into();
    screen.app.chapters.target = Some(target);
    screen.app.chapters.candidates = Some(ChapterSet {
        chapters: titles
            .iter()
            .enumerate()
            .map(|(index, title)| Chapter {
                title: (*title).to_owned(),
                summary: String::new(),
                start: Duration::from_secs(index as u64),
                end: Duration::from_secs(index as u64 + 1),
                source_index: index,
            })
            .collect(),
        invalid_count: 0,
        music_count: 0,
    });
    screen
        .chapter_list
        .select(if selected { Some(0) } else { None });
    screen
}

/// Independent copy of the fitting rule. Production must match rendered cells.
fn spec_fit(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if UnicodeWidthStr::width(text) <= width {
        return text.to_owned();
    }
    let mark = UnicodeWidthStr::width("…");
    if mark == 0 || mark > width {
        return String::new();
    }
    let budget = width - mark;
    let mut kept = String::new();
    let mut used = 0usize;
    if budget > 0 {
        for cluster in text.graphemes(true) {
            let cluster_width = UnicodeWidthStr::width(cluster);
            if used + cluster_width > budget {
                break;
            }
            kept.push_str(cluster);
            used += cluster_width;
        }
    }
    kept.push('…');
    kept
}

fn frame_panes(width: u16, height: u16) -> (Rect, Rect) {
    let footer = 10 + u16::from(height >= 36);
    let middle = Layout::vertical([
        Constraint::Length(7),
        Constraint::Min(1),
        Constraint::Length(footer),
    ])
    .split(Rect::new(0, 0, width, height))[1];
    let program = Layout::horizontal([Constraint::Percentage(48), Constraint::Percentage(52)])
        .split(middle)[0];
    let chapter = Layout::vertical([
        Constraint::Length(7),
        Constraint::Min(1),
        Constraint::Length(3),
    ])
    .split(middle)[1];
    (program, chapter)
}

fn text_columns(area_width: u16, selected: bool) -> usize {
    let inner = Block::default()
        .borders(Borders::ALL)
        .inner(Rect::new(0, 0, area_width, 3));
    let gutter = if selected {
        UnicodeWidthStr::width("▶ ")
    } else {
        0
    };
    usize::from(inner.width).saturating_sub(gutter)
}

fn render(screen: &mut Screen, width: u16, height: u16, now: DateTime<FixedOffset>) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| draw(frame, screen, now)).unwrap();
    terminal.backend().buffer().clone()
}

fn line_glyphs(buffer: &Buffer, y: u16) -> Vec<(u16, String)> {
    let mut x = 0u16;
    let mut glyphs = Vec::new();
    while x < buffer.area.width {
        let symbol = buffer[(x, y)].symbol().to_owned();
        let advance = u16::try_from(Span::raw(&symbol).width().max(1)).unwrap_or(1);
        glyphs.push((x, symbol));
        x = x.saturating_add(advance);
    }
    glyphs
}

fn pane_text(buffer: &Buffer, pane: Rect, y: u16) -> String {
    if pane.width == 0 || y < pane.y || y >= pane.bottom() {
        return String::new();
    }
    line_glyphs(buffer, y)
        .into_iter()
        .filter(|(x, _)| *x >= pane.x && *x < pane.right())
        .map(|(_, symbol)| symbol)
        .collect()
}

fn rows_containing(buffer: &Buffer, pane: Rect, needle: &str) -> Vec<(u16, String)> {
    (pane.y..pane.bottom())
        .filter_map(|y| {
            let text = pane_text(buffer, pane, y);
            text.contains(needle).then_some((y, text))
        })
        .collect()
}

fn assert_border_intact(buffer: &Buffer, pane: Rect, y: u16) {
    assert!(pane.width >= 2, "pane {pane:?} has no frame");
    let right = pane.right() - 1;
    assert_eq!(
        buffer[(pane.x, y)].symbol(),
        "│",
        "left frame invaded at {y}"
    );
    assert_eq!(
        buffer[(right, y)].symbol(),
        "│",
        "right frame invaded at {y}: {:?}",
        pane_text(buffer, pane, y)
    );
    for (x, symbol) in line_glyphs(buffer, y) {
        if x <= pane.x || x >= right {
            continue;
        }
        let end = x + u16::try_from(Span::raw(&symbol).width().max(1)).unwrap_or(1);
        assert!(
            end <= right,
            "glyph {symbol:?} at {x} crosses the frame at {right}"
        );
    }
}

fn title_budget(content_width: usize, prefix: &str) -> usize {
    content_width.saturating_sub(UnicodeWidthStr::width(prefix))
}

fn station_for_budget(area_width: u16, budget: usize) -> String {
    let sample = program_with("", "S");
    let time = local(sample.key.starts_at).format("%m/%d %H:%M");
    let bare = format!("{time}  [放送中] ");
    let columns = text_columns(area_width, true);
    let station_width = columns
        .saturating_sub(budget)
        .saturating_sub(UnicodeWidthStr::width(bare.as_str()));
    "S".repeat(station_width)
}

fn rendered_program_line(
    title: &str,
    station: &str,
    width: u16,
    height: u16,
    selected: bool,
) -> (String, String, usize) {
    let program = program_with(title, station);
    let now = on_air(&program);
    let prefix = program_prefix(&program, now);
    let mut screen = show_programs(vec![program], selected);
    let buffer = render(&mut screen, width, height, now);
    let (pane, _) = frame_panes(width, height);
    let rows = rows_containing(&buffer, pane, &prefix);
    assert_eq!(rows.len(), 1, "program row missing\n{buffer:?}");
    let (y, text) = &rows[0];
    assert_border_intact(&buffer, pane, *y);
    let stored = screen.app.programs[0].title.clone();
    assert_eq!(stored, title);
    let budget = title_budget(text_columns(pane.width, selected), &prefix);
    (text.clone(), prefix, budget)
}

fn rendered_chapter_line(
    title: &str,
    width: u16,
    height: u16,
    selected: bool,
) -> (String, String, usize) {
    let start = Duration::from_secs(0);
    let end = Duration::from_secs(1);
    let prefix = chapter_prefix(false, start, end);
    let mut screen = show_chapters(&[title], selected);
    let now = on_air(screen.app.chapters.target.as_ref().unwrap());
    let buffer = render(&mut screen, width, height, now);
    let (_, pane) = frame_panes(width, height);
    assert!(
        pane.height >= 3,
        "chapter list body is not visible at {width}x{height}: {pane:?}"
    );
    let rows = rows_containing(&buffer, pane, &prefix);
    assert_eq!(rows.len(), 1, "chapter row missing\n{buffer:?}");
    let (y, text) = &rows[0];
    assert_border_intact(&buffer, pane, *y);
    assert_eq!(
        screen.app.chapters.candidates.as_ref().unwrap().chapters[0].title,
        title
    );
    let budget = title_budget(text_columns(pane.width, selected), &prefix);
    (text.clone(), prefix, budget)
}

fn name_after<'a>(line: &'a str, prefix: &str) -> &'a str {
    list_body(line, true)
        .strip_prefix(prefix)
        .unwrap_or_else(|| panic!("prefix missing in {line}"))
        .trim_end()
}

fn list_body(line: &str, selected: bool) -> &str {
    let line = line.strip_prefix('│').unwrap_or(line);
    let line = if selected {
        line.strip_prefix("▶ ")
            .or_else(|| line.strip_prefix("  "))
            .unwrap_or_else(|| panic!("missing highlight gutter: {line}"))
    } else {
        line
    };
    line.strip_suffix('│').unwrap_or(line)
}

fn assert_name(line: &str, prefix: &str, title: &str, budget: usize, selected: bool) {
    let body = list_body(line, selected);
    let fitted = spec_fit(title, budget);
    let expected = format!("{prefix}{fitted}");
    assert!(
        body.starts_with(&expected),
        "budget {budget}\nbody {body:?}\nexpected {expected:?}"
    );
    assert!(
        body[expected.len()..].chars().all(|c| c == ' '),
        "unexpected trailing text in {body:?}"
    );
    assert_eq!(fitted, fit_label(title, budget));
    if UnicodeWidthStr::width(title) > budget {
        if budget == 0 {
            assert_eq!(fitted, "");
        } else {
            assert!(fitted.ends_with('…'), "{fitted}");
        }
        assert!(
            !body.contains(title),
            "full name leaked into the list: {body}"
        );
    } else {
        assert!(
            !fitted.contains('…'),
            "exact fit gained an ellipsis: {fitted}"
        );
    }
}

#[test]
fn fit_label_keeps_exact_fits_and_stops_on_grapheme_boundaries() {
    assert_eq!(fit_label("", 0), "");
    assert_eq!(fit_label("", 4), "");
    assert_eq!(fit_label("あいう", 0), "");
    assert_eq!(fit_label("A", 1), "A");
    assert_eq!(fit_label("AB", 2), "AB");
    assert_eq!(fit_label("あ", 2), "あ");
    assert_eq!(fit_label("ABC", 1), "…");
    assert_eq!(fit_label("ABC", 2), "A…");
    assert_eq!(fit_label("ああ", 2), "…");
    assert_eq!(fit_label("あX", 3), "あX");
    assert_eq!(fit_label("あXY", 3), "あ…");
    assert_eq!(fit_label("aあZ", 2), "a…");
    assert!(!fit_label("aあZ", 2).contains('あ'));
    assert!(!fit_label("aあZ", 2).contains('Z'));
    assert_eq!(fit_label(ACUTE, 1), ACUTE);
    assert_eq!(fit_label(&format!("{ACUTE}Q"), 2), format!("{ACUTE}Q"));
    assert_eq!(fit_label(&format!("{ACUTE}QQ"), 2), format!("{ACUTE}…"));
    assert_eq!(
        fit_label(&format!("{ACUTE}{COENG}QQ"), 3),
        format!("{ACUTE}{COENG}…")
    );
    assert_eq!(fit_label(&format!("{FAMILY}QQ"), 3), format!("{FAMILY}…"));
    assert_eq!(fit_label("\u{0301}", 1), "\u{0301}");
    assert_eq!(display_width("…"), 1);
    assert_eq!(display_width(FAMILY), 2);
    assert_eq!(UnicodeWidthStr::width(COENG), 1);
    assert!(
        UnicodeWidthStr::width(COENG) < COENG.chars().map(|c| c.to_string().width()).sum::<usize>()
    );
}

#[test]
fn width_60_fullwidth_names_end_with_ellipsis_and_stay_inside_the_frame() {
    let full = "あ".repeat(50);
    let (program_pane, chapter_pane) = frame_panes(60, 30);
    let station = station_for_budget(program_pane.width, 3);
    let (program_line, program_fixed, program_budget) =
        rendered_program_line(&full, &station, 60, 30, true);
    assert_eq!(program_budget, 3, "{program_line}");
    assert_name(&program_line, &program_fixed, &full, program_budget, true);
    assert!(program_line.contains("放送中"), "{program_line}");
    assert!(
        list_body(&program_line, true).ends_with('…'),
        "{program_line}"
    );
    assert_eq!(program_line.matches('あ').count(), 1, "{program_line}");
    assert!(!program_line.contains(&full));

    let (chapter_line, chapter_fixed, chapter_budget) = rendered_chapter_line(&full, 60, 30, true);
    assert!(chapter_budget > 3, "{chapter_budget} {chapter_pane:?}");
    assert_name(&chapter_line, &chapter_fixed, &full, chapter_budget, true);
    assert!(
        list_body(&chapter_line, true).ends_with('…'),
        "{chapter_line}"
    );
    assert!(chapter_line.matches('あ').count() < 50, "{chapter_line}");
    assert!(!chapter_line.contains(&full));
    let program_columns = list_text_columns(program_pane.width, true);
    let chapter_columns = list_text_columns(chapter_pane.width, true);
    assert_eq!(program_columns, text_columns(program_pane.width, true));
    assert_eq!(chapter_columns, text_columns(chapter_pane.width, true));
}

#[test]
fn mixed_half_and_fullwidth_names_leave_fixed_columns_aligned() {
    let now = on_air(&base_program());
    let ascii = "A".repeat(80);
    let full = "あ".repeat(50);
    let mixed = "Aあ".repeat(30);
    let mut screen = show_programs(
        vec![
            program_with(&ascii, "TBS"),
            program_with(&full, "TBS"),
            program_with(&mixed, "TBS"),
        ],
        true,
    );
    let buffer = render(&mut screen, 80, 24, now);
    let (pane, _) = frame_panes(80, 24);
    let stamp = local(base_program().key.starts_at)
        .format("%m/%d %H:%M")
        .to_string();
    let rows = rows_containing(&buffer, pane, &stamp);
    assert_eq!(rows.len(), 3, "{rows:?}\n{buffer:?}");
    let columns = text_columns(pane.width, true);
    let mut title_starts = Vec::new();
    for (index, (y, line)) in rows.iter().enumerate() {
        assert_border_intact(&buffer, pane, *y);
        let program = &screen.app.programs[index];
        let prefix = program_prefix(program, now);
        let budget = title_budget(columns, &prefix);
        assert_name(line, &prefix, &program.title, budget, true);
        let item = list_body(line, true);
        assert!(item.contains("TBS [放送中] "), "{item}");
        let station_x = line_glyphs(&buffer, *y)
            .into_iter()
            .find(|(_, symbol)| symbol == "T")
            .map(|(x, _)| x);
        title_starts.push(station_x);
    }
    assert_eq!(title_starts[0], title_starts[1]);
    assert_eq!(title_starts[1], title_starts[2]);

    let mut chapters = show_chapters(&[&ascii, &full, &mixed], true);
    let chapter_now = on_air(chapters.app.chapters.target.as_ref().unwrap());
    let buffer = render(&mut chapters, 80, 40, chapter_now);
    let (_, pane) = frame_panes(80, 40);
    let rows = rows_containing(&buffer, pane, "秒 ");
    assert_eq!(rows.len(), 3, "{rows:?}");
    let columns = text_columns(pane.width, true);
    let mut starts = Vec::new();
    for (index, (y, line)) in rows.iter().enumerate() {
        assert_border_intact(&buffer, pane, *y);
        let prefix = chapter_prefix(
            false,
            Duration::from_secs(index as u64),
            Duration::from_secs(index as u64 + 1),
        );
        let budget = title_budget(columns, &prefix);
        let title = &chapters.app.chapters.candidates.as_ref().unwrap().chapters[index].title;
        assert_name(line, &prefix, title, budget, true);
        let wave_x = line_glyphs(&buffer, *y)
            .into_iter()
            .find(|(_, symbol)| symbol == "〜")
            .map(|(x, _)| x);
        starts.push(wave_x);
    }
    assert_eq!(starts[0], starts[1]);
    assert_eq!(starts[1], starts[2]);
}

#[test]
fn exact_fit_and_empty_names_omit_the_ellipsis() {
    let (pane, _) = frame_panes(80, 24);
    let columns = text_columns(pane.width, true);
    let station = "TBS";
    let probe = program_with("", station);
    let prefix = program_prefix(&probe, on_air(&probe));
    let budget = title_budget(columns, &prefix);
    assert!(budget > 2, "{budget}");
    let exact = "N".repeat(budget);
    let (line, prefix, seen) = rendered_program_line(&exact, station, 80, 24, true);
    assert_eq!(seen, budget);
    assert_name(&line, &prefix, &exact, budget, true);
    assert!(!list_body(&line, true).contains('…'));

    let (empty, empty_prefix, _) = rendered_program_line("", station, 80, 24, true);
    assert_name(
        &empty,
        &empty_prefix,
        "",
        title_budget(columns, &empty_prefix),
        true,
    );
    assert!(!list_body(&empty, true).contains('…'));

    let (chapter_pane_width, chapter_height) = (80u16, 30u16);
    let (_, chapter_pane) = frame_panes(chapter_pane_width, chapter_height);
    let chapter_columns = text_columns(chapter_pane.width, true);
    let chapter_prefix = chapter_prefix(false, Duration::from_secs(0), Duration::from_secs(1));
    let chapter_budget = title_budget(chapter_columns, &chapter_prefix);
    let exact = "C".repeat(chapter_budget);
    let (line, prefix, seen) =
        rendered_chapter_line(&exact, chapter_pane_width, chapter_height, true);
    assert_eq!(seen, chapter_budget);
    assert_name(&line, &prefix, &exact, chapter_budget, true);
    assert!(!list_body(&line, true).contains('…'));
    let (empty, prefix, _) = rendered_chapter_line("", chapter_pane_width, chapter_height, true);
    assert_name(&empty, &prefix, "", chapter_budget, true);
    assert!(!list_body(&empty, true).contains('…'));
}

#[test]
fn title_budgets_zero_one_and_two_cover_ellipsis_and_fullwidth_overflow() {
    let (pane, _) = frame_panes(80, 24);
    for budget in [0usize, 1, 2] {
        let station = station_for_budget(pane.width, budget);
        let overflowing = format!("Q{COENG}あZ");
        let (line, prefix, seen) = rendered_program_line(&overflowing, &station, 80, 24, true);
        assert_eq!(seen, budget, "station {station:?} line {line}");
        assert_name(&line, &prefix, &overflowing, budget, true);
        let name = name_after(&line, &prefix);
        match budget {
            0 => assert_eq!(name, ""),
            1 => assert_eq!(name, "…"),
            2 => assert_eq!(name, "Q…"),
            _ => unreachable!(),
        }
        assert!(!name.contains('あ'), "{name}");
        assert!(!name.contains('Z'), "{name}");
    }
    let station = station_for_budget(pane.width, 2);
    let (exact, _, budget) = rendered_program_line("あ", &station, 80, 24, true);
    assert_eq!(budget, 2);
    assert!(
        list_body(&exact, true).trim_end().ends_with('あ'),
        "{exact}"
    );
    assert!(!list_body(&exact, true).contains('…'), "{exact}");
    let (only_mark, _, _) = rendered_program_line("あZ", &station, 80, 24, true);
    assert!(
        list_body(&only_mark, true).trim_end().ends_with('…'),
        "{only_mark}"
    );
    assert!(!list_body(&only_mark, true).contains('あ'), "{only_mark}");
    assert!(!list_body(&only_mark, true).contains('Z'), "{only_mark}");

    for budget in [0usize, 1, 2] {
        let width = (10..80).find(|width| {
            let (_, chapter) = frame_panes(*width, 40);
            let prefix = chapter_prefix(false, Duration::from_secs(0), Duration::from_secs(1));
            let columns = text_columns(chapter.width, true);
            chapter.height >= 3
                && columns >= UnicodeWidthStr::width(prefix.as_str())
                && title_budget(columns, &prefix) == budget
        });
        let width = width.unwrap_or_else(|| panic!("no chapter width for budget {budget}"));
        let (line, prefix, seen) = rendered_chapter_line("QあZ", width, 40, true);
        assert_eq!(seen, budget, "{line}");
        assert_name(&line, &prefix, "QあZ", budget, true);
        let name = name_after(&line, &prefix);
        match budget {
            0 => assert_eq!(name, ""),
            1 => assert_eq!(name, "…"),
            2 => assert_eq!(name, "Q…"),
            _ => unreachable!(),
        }
    }
}

#[test]
fn combining_mark_and_zwj_emoji_are_kept_or_dropped_as_whole_clusters() {
    let (pane, _) = frame_panes(120, 30);
    let combining = format!("{ACUTE}{COENG}QQ");
    let station = station_for_budget(pane.width, 3);
    let program = program_with(&combining, &station);
    let now = on_air(&program);
    let mut screen = show_programs(vec![program], true);
    let buffer = render(&mut screen, 120, 30, now);
    let (program_pane, _) = frame_panes(120, 30);
    let prefix = program_prefix(&screen.app.programs[0], now);
    let rows = rows_containing(&buffer, program_pane, &prefix);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_border_intact(&buffer, program_pane, rows[0].0);
    let symbols: Vec<_> = line_glyphs(&buffer, rows[0].0)
        .into_iter()
        .filter(|(x, _)| *x >= program_pane.x && *x < program_pane.right())
        .map(|(_, symbol)| symbol)
        .collect();
    assert!(
        symbols.iter().any(|symbol| symbol == ACUTE),
        "combining acute was split: {symbols:?}"
    );
    assert!(
        symbols.iter().any(|symbol| symbol == COENG),
        "coeng cluster was split into code points: {symbols:?}"
    );
    assert!(!symbols.iter().any(|symbol| symbol == "Q"), "{symbols:?}");
    assert_eq!(screen.app.programs[0].title, combining);

    let zwj = format!("{FAMILY}QQ");
    let (line, prefix, budget) = rendered_program_line(&zwj, &station, 120, 30, true);
    assert_eq!(budget, 3, "{line}");
    assert_name(&line, &prefix, &zwj, budget, true);
    let y = rows_containing(
        &render(
            &mut show_programs(vec![program_with(&zwj, &station)], true),
            120,
            30,
            now,
        ),
        program_pane,
        &prefix,
    )[0]
    .0;
    let buffer = render(
        &mut show_programs(vec![program_with(&zwj, &station)], true),
        120,
        30,
        now,
    );
    let symbols: Vec<_> = line_glyphs(&buffer, y)
        .into_iter()
        .filter(|(x, _)| *x >= program_pane.x && *x < program_pane.right())
        .map(|(_, symbol)| symbol)
        .collect();
    assert!(
        symbols.iter().any(|symbol| symbol == FAMILY),
        "ZWJ sequence was split: {symbols:?}"
    );
    assert!(!symbols.iter().any(|symbol| symbol == "Q"), "{symbols:?}");
    assert!(
        !symbols
            .iter()
            .any(|symbol| symbol == "👨" || symbol == "👨\u{200d}")
    );

    let width = (10..100)
        .find(|width| {
            let (_, chapter) = frame_panes(*width, 40);
            chapter.height >= 3
                && title_budget(
                    text_columns(chapter.width, true),
                    &chapter_prefix(false, Duration::from_secs(0), Duration::from_secs(1)),
                ) == 3
        })
        .unwrap();
    let mut screen = show_chapters(&[&combining, &zwj], true);
    let chapter_now = on_air(screen.app.chapters.target.as_ref().unwrap());
    let buffer = render(&mut screen, width, 40, chapter_now);
    let (_, chapter_pane) = frame_panes(width, 40);
    let first = chapter_prefix(false, Duration::from_secs(0), Duration::from_secs(1));
    let second = chapter_prefix(false, Duration::from_secs(1), Duration::from_secs(2));
    let combining_rows = rows_containing(&buffer, chapter_pane, &first);
    let zwj_rows = rows_containing(&buffer, chapter_pane, &second);
    assert_eq!(combining_rows.len(), 1, "{combining_rows:?}");
    assert_eq!(zwj_rows.len(), 1, "{zwj_rows:?}");
    let combining_symbols: Vec<_> = line_glyphs(&buffer, combining_rows[0].0)
        .into_iter()
        .map(|(_, symbol)| symbol)
        .collect();
    let zwj_symbols: Vec<_> = line_glyphs(&buffer, zwj_rows[0].0)
        .into_iter()
        .map(|(_, symbol)| symbol)
        .collect();
    assert!(
        combining_symbols.iter().any(|symbol| symbol == ACUTE),
        "{combining_symbols:?}"
    );
    assert!(
        combining_symbols.iter().any(|symbol| symbol == COENG),
        "{combining_symbols:?}"
    );
    assert!(
        zwj_symbols.iter().any(|symbol| symbol == FAMILY),
        "{zwj_symbols:?}"
    );
    assert!(
        !zwj_symbols
            .iter()
            .any(|symbol| symbol == "👨" || symbol == "👨\u{200d}")
    );
    assert_eq!(
        screen.app.chapters.candidates.as_ref().unwrap().chapters[0].title,
        combining
    );
    assert_eq!(
        screen.app.chapters.candidates.as_ref().unwrap().chapters[1].title,
        zwj
    );
}

#[test]
fn names_follow_the_frame_at_80x24_160x40_and_after_resize() {
    let full = "あ".repeat(50);
    let ascii = "B".repeat(200);
    let mut screen = show_programs(
        vec![program_with(&full, "TBS"), program_with(&ascii, "TBS")],
        true,
    );
    let now = on_air(&screen.app.programs[0]);
    let mut terminal = Terminal::new(TestBackend::new(160, 40)).unwrap();
    let mut check = |width: u16, height: u16| {
        terminal.backend_mut().resize(width, height);
        terminal.resize(Rect::new(0, 0, width, height)).unwrap();
        terminal
            .draw(|frame| draw(frame, &mut screen, now))
            .unwrap();
        assert_eq!(terminal.backend().buffer().area.width, width);
        assert_eq!(terminal.backend().buffer().area.height, height);
        let buffer = terminal.backend().buffer().clone();
        let (pane, _) = frame_panes(width, height);
        let columns = text_columns(pane.width, true);
        let stamp = local(screen.app.programs[0].key.starts_at)
            .format("%m/%d %H:%M")
            .to_string();
        let rows = rows_containing(&buffer, pane, &stamp);
        assert_eq!(rows.len(), 2, "{width}x{height} {rows:?}");
        for (index, (y, line)) in rows.iter().enumerate() {
            assert_border_intact(&buffer, pane, *y);
            let prefix = program_prefix(&screen.app.programs[index], now);
            let budget = title_budget(columns, &prefix);
            assert_name(
                line,
                &prefix,
                &screen.app.programs[index].title,
                budget,
                true,
            );
            if budget == 0 {
                assert_eq!(spec_fit(&screen.app.programs[index].title, budget), "");
            } else {
                assert!(
                    list_body(line, true).trim_end().ends_with('…'),
                    "{width}x{height} {line}"
                );
            }
        }
        assert_eq!(screen.app.programs[0].title, full);
        assert_eq!(screen.app.programs[1].title, ascii);
    };
    check(160, 40);
    check(80, 24);
    check(60, 30);
    check(60, 24);

    let mut chapters = show_chapters(&[&full, &ascii], true);
    let chapter_now = on_air(chapters.app.chapters.target.as_ref().unwrap());
    for (width, height) in [(160u16, 40u16), (80, 40), (60, 40), (80, 24)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| draw(frame, &mut chapters, chapter_now))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let (_, pane) = frame_panes(width, height);
        if pane.height < 3 {
            assert!(
                rows_containing(&buffer, pane, "秒 ").is_empty(),
                "chapter body should be hidden at {width}x{height}"
            );
            continue;
        }
        let columns = text_columns(pane.width, true);
        for index in 0..2 {
            let prefix = chapter_prefix(
                false,
                Duration::from_secs(index),
                Duration::from_secs(index + 1),
            );
            let rows = rows_containing(&buffer, pane, &prefix);
            assert_eq!(rows.len(), 1, "{width}x{height} {prefix} {rows:?}");
            assert_border_intact(&buffer, pane, rows[0].0);
            let title =
                &chapters.app.chapters.candidates.as_ref().unwrap().chapters[index as usize].title;
            assert_name(
                &rows[0].1,
                &prefix,
                title,
                title_budget(columns, &prefix),
                true,
            );
        }
    }
    assert_eq!(
        chapters.app.chapters.candidates.as_ref().unwrap().chapters[0].title,
        full
    );
}

#[test]
fn truncation_keeps_stored_titles_searchable_and_leaves_playback_unchanged() {
    let tail = "TAILMARKER";
    let title = format!("{}{tail}", "あ".repeat(50));
    let program = program_with(&title, "TBS");
    let now = on_air(&program);
    let filter =
        radiko::domain::search::Filter::new(vec![], vec![], false, false, Some(tail.into()))
            .unwrap();
    assert!(filter.matches(&program));
    let mut screen = show_programs(vec![program.clone()], true);
    screen.app.playback = Some(radiko::player::state::PlaybackSnapshot {
        pipeline: None,
        info: radiko::api::stream::playback::StreamInfo {
            title: Some(title.clone()),
            station: program.key.station_id.clone(),
            mode: radiko::api::stream::ProbeMode::TimeFree,
            starts_at: program.key.starts_at,
            ends_at: program.ends_at,
            position: Duration::ZERO,
        },
        state: PlaybackState::Playing,
        pending: None,
        volume: 25,
        position: Some(Duration::from_secs(3)),
        output: None,
        segments: 1,
        receiving_for: None,
        reconnecting_for: None,
    });
    let buffer = render(&mut screen, 80, 24, now);
    let (pane, _) = frame_panes(80, 24);
    let prefix = program_prefix(&screen.app.programs[0], now);
    let rows = rows_containing(&buffer, pane, &prefix);
    assert_eq!(rows.len(), 1);
    assert!(!rows[0].1.contains(tail), "{}", rows[0].1);
    assert!(pane_text_region(&buffer, pane.right(), buffer.area.width).contains(tail));
    assert!(filter.matches(&screen.app.programs[0]));
    assert_eq!(screen.app.programs[0].title, title);
    assert_eq!(
        screen.app.playback.as_ref().unwrap().info.title.as_deref(),
        Some(title.as_str())
    );
    assert_eq!(fit_after_prefix(&prefix, &title, 0), "");
}

fn pane_text_region(buffer: &Buffer, x0: u16, x1: u16) -> String {
    let mut text = String::new();
    for y in 0..buffer.area.height {
        for (x, symbol) in line_glyphs(buffer, y) {
            if x >= x0 && x < x1 {
                text.push_str(&symbol);
            }
        }
        text.push('\n');
    }
    text
}

#[test]
fn playback_position_marker_is_reserved_before_the_chapter_name() {
    use radiko::api::stream::chapters::{EventBinding, Outcome, Report};
    let mut screen = screen();
    let program = base_program();
    let id = screen.app.chapters.open(program.clone()).unwrap();
    let long = "あ".repeat(50);
    screen.app.chapters.apply(radiko::app::chapters::Event {
        id,
        key: program.key.clone(),
        result: Ok(Report {
            station: program.key.station_id.clone(),
            starts_at: program.key.starts_at,
            ends_at: program.ends_at,
            title: Some("対象番組".into()),
            outcome: Outcome::Aligned(
                ChapterSet {
                    chapters: vec![
                        Chapter {
                            title: long.clone(),
                            summary: String::new(),
                            start: Duration::from_secs(0),
                            end: Duration::from_secs(10),
                            source_index: 0,
                        },
                        Chapter {
                            title: long.clone(),
                            summary: String::new(),
                            start: Duration::from_secs(10),
                            end: Duration::from_secs(20),
                            source_index: 1,
                        },
                    ],
                    invalid_count: 0,
                    music_count: 0,
                },
                EventBinding::matching_origin(
                    program.key.clone(),
                    program.ends_at,
                    "SYNTHETIC-event".into(),
                    program.key.starts_at,
                )
                .unwrap(),
            ),
        }),
    });
    screen.chapter_list.select(Some(0));
    screen.app.playback = Some(radiko::player::state::PlaybackSnapshot {
        pipeline: None,
        info: radiko::api::stream::playback::StreamInfo {
            title: Some(program.title.clone()),
            station: program.key.station_id.clone(),
            starts_at: program.key.starts_at,
            ends_at: program.ends_at,
            mode: radiko::api::stream::ProbeMode::TimeFree,
            position: Duration::from_secs(1),
        },
        state: PlaybackState::Playing,
        pending: None,
        volume: 25,
        position: Some(Duration::from_secs(1)),
        output: Some(radiko::player::output::OutputSummary {
            frames: 1,
            underrun_frames: 0,
            sample_rate: 48_000,
            channels: 2,
            sample_format: radiko::player::output::OutputSampleFormat::F32,
        }),
        segments: 1,
        receiving_for: None,
        reconnecting_for: None,
    });
    let now = on_air(&program);
    let buffer = render(&mut screen, 60, 40, now);
    let (_, pane) = frame_panes(60, 40);
    let marked = chapter_prefix(true, Duration::from_secs(0), Duration::from_secs(10));
    let plain = chapter_prefix(false, Duration::from_secs(10), Duration::from_secs(20));
    let marked_rows = rows_containing(&buffer, pane, &marked);
    let plain_rows = rows_containing(&buffer, pane, &plain);
    assert_eq!(
        marked_rows.len(),
        1,
        "marked {marked:?}\nplain {plain:?}\n{}",
        (pane.y..pane.bottom())
            .map(|y| pane_text(&buffer, pane, y))
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert_eq!(plain_rows.len(), 1, "{plain_rows:?}");
    let columns = text_columns(pane.width, true);
    assert_border_intact(&buffer, pane, marked_rows[0].0);
    assert_border_intact(&buffer, pane, plain_rows[0].0);
    assert_name(
        &marked_rows[0].1,
        &marked,
        &long,
        title_budget(columns, &marked),
        true,
    );
    assert_name(
        &plain_rows[0].1,
        &plain,
        &long,
        title_budget(columns, &plain),
        true,
    );
    assert!(marked_rows[0].1.contains("[再生位置] "));
    assert!(!plain_rows[0].1.contains("[再生位置]"));
    assert!(marked_rows[0].1.matches('あ').count() < plain_rows[0].1.matches('あ').count());
    assert_eq!(
        screen.app.chapters.target.as_ref().unwrap().title,
        program.title
    );
}

#[test]
fn unselected_lists_do_not_reserve_the_highlight_gutter() {
    let title = "あ".repeat(50);
    let (selected, _, selected_budget) = rendered_program_line(&title, "TBS", 80, 24, true);
    let (open, _, open_budget) = rendered_program_line(&title, "TBS", 80, 24, false);
    assert!(
        open_budget > selected_budget,
        "{open_budget} vs {selected_budget}"
    );
    assert!(selected.contains('▶'), "{selected}");
    assert!(!open.contains('▶'), "{open}");
    let prefix = program_prefix(&program_with(&title, "TBS"), on_air(&base_program()));
    assert!(list_body(&open, false).starts_with(&prefix), "{open}");
    assert!(
        list_body(&selected, true).starts_with(&prefix),
        "{selected}"
    );
}

#[test]
fn narrow_frames_do_not_panic_or_draw_outside_the_buffer() {
    let title = "あ".repeat(50);
    let mut programs = show_programs(vec![program_with(&title, "TBS")], true);
    let now = on_air(&programs.app.programs[0]);
    for (width, height) in [(0u16, 0u16), (1, 1), (2, 2), (8, 4), (60, 24)] {
        let buffer = render(&mut programs, width, height, now);
        assert_eq!(buffer.area.width, width);
        assert_eq!(buffer.area.height, height);
    }
    let mut chapters = show_chapters(&[&title], true);
    let chapter_now = on_air(chapters.app.chapters.target.as_ref().unwrap());
    for (width, height) in [(0u16, 0u16), (1, 1), (2, 2), (8, 4), (60, 24), (80, 24)] {
        let buffer = render(&mut chapters, width, height, chapter_now);
        assert_eq!(buffer.area.width, width);
        assert_eq!(programs.app.programs[0].title, title);
        assert_eq!(
            chapters.app.chapters.candidates.as_ref().unwrap().chapters[0].title,
            title
        );
        let _ = buffer;
    }
}
