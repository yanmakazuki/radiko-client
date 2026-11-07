//! Playback-area and help labels. Reads a snapshot and does not change it.
//!
//! `PlaybackState` is only `Buffering`, `Playing`, and `Paused`. There is no
//! preparing, stopped, or ended variant. Stopped player lines and notices stay
//! the sentences they already have. `Buffering` is `バッファ中`.
use chrono::{DateTime, FixedOffset};
use radiko::{
    api::stream::ProbeMode,
    player::state::{PlaybackSnapshot, PlaybackState},
};
use ratatui::{
    Frame,
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    symbols,
    widgets::LineGauge,
};
use std::time::Duration;
use unicode_width::UnicodeWidthStr;

pub(super) const FILLED: &str = "█";
pub(super) const UNFILLED: &str = "░";
/// Narrow rows keep the status and the clock. The bar needs a few cells of its own.
const BAR_MIN: u16 = 8;

pub(super) fn mode_label(mode: ProbeMode) -> &'static str {
    match mode {
        ProbeMode::Live => "ライブ",
        ProbeMode::TimeFree => "タイムフリー",
    }
}

pub(super) fn state_label(state: PlaybackState) -> &'static str {
    match state {
        PlaybackState::Playing => "再生中",
        PlaybackState::Paused => "一時停止",
        PlaybackState::Buffering => "バッファ中",
    }
}

/// `mm:ss`, flooring fractional seconds. Minutes stay a total: `3600s` is `60:00`.
pub(super) fn clock(duration: Duration) -> String {
    let seconds = duration.as_secs();
    let minutes = seconds / 60;
    let remain = seconds % 60;
    format!("{minutes:02}:{remain:02}")
}

pub(super) struct Presentation {
    pub mode: &'static str,
    pub state: &'static str,
    pub clock: String,
    /// Finite TimeFree ratio in `0.0..=1.0`, or none when a bar must not be drawn.
    pub ratio: Option<f64>,
}

pub(super) fn presentation(snapshot: &PlaybackSnapshot) -> Presentation {
    let progress = program_progress(
        snapshot.info.mode,
        snapshot.position,
        snapshot.info.starts_at,
        snapshot.info.ends_at,
    );
    Presentation {
        mode: mode_label(snapshot.info.mode),
        state: state_label(snapshot.state),
        clock: progress.text,
        ratio: progress.ratio,
    }
}

struct ProgramProgress {
    text: String,
    ratio: Option<f64>,
}

fn program_progress(
    mode: ProbeMode,
    position: Option<Duration>,
    starts_at: DateTime<FixedOffset>,
    ends_at: DateTime<FixedOffset>,
) -> ProgramProgress {
    let Some(position) = position else {
        return ProgramProgress {
            text: "番組内位置未確認".into(),
            ratio: None,
        };
    };
    let shown = clock(position);
    // Live elapsed output is not a position inside the slot, so the slot length
    // is never the denominator and there is no program-progress bar.
    let total = (mode == ProbeMode::TimeFree).then(|| slot_total(starts_at, ends_at));
    let Some(Some(total)) = total else {
        return ProgramProgress {
            text: format!("{shown} / --:--"),
            ratio: None,
        };
    };
    let ratio = ratio_from_seconds(position.as_secs_f64(), total.as_secs_f64());
    ProgramProgress {
        text: format!("{shown} / {}", clock(total)),
        ratio,
    }
}

fn slot_total(
    starts_at: DateTime<FixedOffset>,
    ends_at: DateTime<FixedOffset>,
) -> Option<Duration> {
    let delta = ends_at.signed_duration_since(starts_at);
    if delta <= chrono::Duration::zero() {
        return None;
    }
    delta.to_std().ok().filter(|total| !total.is_zero())
}

/// None unless both inputs are finite, `total` is positive, and the ratio is finite.
/// The returned value is clamped to `0.0..=1.0` so `LineGauge` is not given a value it rejects.
fn ratio_from_seconds(position: f64, total: f64) -> Option<f64> {
    if !position.is_finite() || !total.is_finite() || total <= 0.0 || position < 0.0 {
        return None;
    }
    let ratio = position / total;
    if !ratio.is_finite() {
        return None;
    }
    let ratio = ratio.clamp(0.0, 1.0);
    (0.0..=1.0).contains(&ratio).then_some(ratio)
}

/// Paint a TimeFree bar into leftover cells. Ratatui 0.29's `LineGauge` draws one
/// glyph for both ends and distinguishes them by style, which disappears without
/// color. The gauge still places the fill; the glyphs are then `█` and `░`.
pub(super) fn paint_bar(frame: &mut Frame, area: Rect, ratio: f64) {
    if area.width < BAR_MIN || area.height == 0 || !(0.0..=1.0).contains(&ratio) {
        return;
    }
    frame.render_widget(gauge(ratio), area);
    let buf = frame.buffer_mut();
    let y = area.y;
    let right = area.x.saturating_add(area.width);
    let mut x = area.x;
    while x < right {
        let cell = &mut buf[(x, y)];
        let width = UnicodeWidthStr::width(cell.symbol()).max(1) as u16;
        if cell.symbol() == symbols::line::NORMAL.horizontal {
            let modifier = cell.modifier;
            if modifier.contains(Modifier::BOLD) {
                cell.set_symbol(FILLED).set_style(Style::default());
            } else if modifier.contains(Modifier::DIM) {
                cell.set_symbol(UNFILLED).set_style(Style::default());
            }
        }
        x = x.saturating_add(width);
    }
}

fn gauge(ratio: f64) -> LineGauge<'static> {
    LineGauge::default()
        .ratio(ratio)
        .label("")
        .line_set(symbols::line::NORMAL)
        .filled_style(Style::default().add_modifier(Modifier::BOLD))
        .unfilled_style(Style::default().add_modifier(Modifier::DIM))
}

/// A bar fits on this row when `needle` ends the visible text and at least
/// [`BAR_MIN`] blank cells remain before the border.
pub(super) fn trailing_bar_area(buf: &Buffer, inner: Rect, needle: &str) -> Option<Rect> {
    if needle.is_empty() || inner.width < BAR_MIN || inner.height == 0 {
        return None;
    }
    let bottom = inner.y.saturating_add(inner.height);
    let right = inner.x.saturating_add(inner.width);
    for y in inner.y..bottom {
        let mut text = String::new();
        let mut x = inner.x;
        while x < right {
            let symbol = buf[(x, y)].symbol();
            let width = UnicodeWidthStr::width(symbol).max(1) as u16;
            text.push_str(symbol);
            x = x.saturating_add(width);
            if text.ends_with(needle)
                && right.saturating_sub(x) >= BAR_MIN
                && (x..right).all(|column| buf[(column, y)].symbol() == " ")
            {
                return Some(Rect {
                    x,
                    y,
                    width: right - x,
                    height: 1,
                });
            }
        }
    }
    None
}

/// Width of a help-line bar beside `prefix`, when the row can keep that prefix.
pub(super) fn reserved_bar(area_width: u16, prefix_width: usize) -> Option<u16> {
    let Ok(prefix_width) = u16::try_from(prefix_width) else {
        return None;
    };
    [16u16, BAR_MIN]
        .into_iter()
        .find(|&bar| area_width >= prefix_width.saturating_add(1).saturating_add(bar))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    fn instant() -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339("2026-10-15T03:00:00+09:00").unwrap()
    }

    fn slot(seconds: i64) -> (DateTime<FixedOffset>, DateTime<FixedOffset>) {
        let start = instant();
        (start, start + chrono::Duration::seconds(seconds))
    }

    #[test]
    fn every_mode_and_state_has_a_distinct_japanese_label_and_not_its_debug_name() {
        let modes = [
            (ProbeMode::Live, "ライブ"),
            (ProbeMode::TimeFree, "タイムフリー"),
        ];
        let states = [
            (PlaybackState::Playing, "再生中"),
            (PlaybackState::Paused, "一時停止"),
            (PlaybackState::Buffering, "バッファ中"),
        ];
        assert_eq!(modes.len(), 2, "ProbeMode gained a variant");
        assert_eq!(states.len(), 3, "PlaybackState gained a variant");
        let mut seen = std::collections::BTreeSet::new();
        for (mode, label) in modes {
            assert_eq!(mode_label(mode), label);
            assert_ne!(label, format!("{mode:?}"));
            assert!(seen.insert(label), "{label}");
            match mode {
                ProbeMode::Live | ProbeMode::TimeFree => {}
            }
        }
        for (state, label) in states {
            assert_eq!(state_label(state), label);
            assert_ne!(label, format!("{state:?}"));
            assert!(seen.insert(label), "{label}");
            match state {
                PlaybackState::Playing | PlaybackState::Paused | PlaybackState::Buffering => {}
            }
        }
    }

    #[test]
    fn clock_floors_fractional_seconds_and_keeps_total_minutes() {
        let cases = [
            (Duration::ZERO, "00:00"),
            (Duration::from_secs(59), "00:59"),
            (Duration::new(59, 999_999_999), "00:59"),
            (Duration::from_secs(60), "01:00"),
            (Duration::from_millis(60_000), "01:00"),
            (Duration::from_secs(754), "12:34"),
            (Duration::new(754, 900_000_000), "12:34"),
            (Duration::from_secs(3300), "55:00"),
            (Duration::from_secs(3599), "59:59"),
            (Duration::from_secs(3600), "60:00"),
            (Duration::from_secs(3661), "61:01"),
            (
                Duration::from_secs(u64::MAX),
                &format!("{:02}:{:02}", u64::MAX / 60, u64::MAX % 60),
            ),
        ];
        for (duration, expected) in cases {
            assert_eq!(clock(duration), expected, "{duration:?}");
        }
    }

    #[test]
    fn timefree_clock_uses_position_over_positive_total_and_clamps_the_ratio() {
        let (start, end) = slot(3300);
        let progress = program_progress(
            ProbeMode::TimeFree,
            Some(Duration::new(754, 900_000_000)),
            start,
            end,
        );
        assert_eq!(progress.text, "12:34 / 55:00");
        let ratio = progress.ratio.unwrap();
        assert!((ratio - (754.9 / 3300.0)).abs() < 1e-9);
        assert!((0.0..=1.0).contains(&ratio));

        let at_start = program_progress(ProbeMode::TimeFree, Some(Duration::ZERO), start, end);
        assert_eq!(at_start.text, "00:00 / 55:00");
        assert_eq!(at_start.ratio, Some(0.0));

        let at_end = program_progress(
            ProbeMode::TimeFree,
            Some(Duration::from_secs(3300)),
            start,
            end,
        );
        assert_eq!(at_end.text, "55:00 / 55:00");
        assert_eq!(at_end.ratio, Some(1.0));

        let past = program_progress(
            ProbeMode::TimeFree,
            Some(Duration::from_secs(4000)),
            start,
            end,
        );
        assert_eq!(past.text, "66:40 / 55:00");
        assert_eq!(past.ratio, Some(1.0));
        assert_eq!(Duration::from_secs(4000).as_secs(), 4000);
    }

    #[test]
    fn missing_position_zero_total_and_live_do_not_produce_a_ratio() {
        let (start, end) = slot(3300);
        let missing = program_progress(ProbeMode::TimeFree, None, start, end);
        assert_eq!(missing.text, "番組内位置未確認");
        assert_eq!(missing.ratio, None);

        let zero = program_progress(
            ProbeMode::TimeFree,
            Some(Duration::from_secs(754)),
            start,
            start,
        );
        assert_eq!(zero.text, "12:34 / --:--");
        assert_eq!(zero.ratio, None);

        let negative = program_progress(
            ProbeMode::TimeFree,
            Some(Duration::from_secs(754)),
            end,
            start,
        );
        assert_eq!(negative.text, "12:34 / --:--");
        assert_eq!(negative.ratio, None);

        let live = program_progress(ProbeMode::Live, Some(Duration::from_secs(754)), start, end);
        assert_eq!(live.text, "12:34 / --:--");
        assert_eq!(live.ratio, None);
        assert!(!live.text.contains("55:00"));

        let live_missing = program_progress(ProbeMode::Live, None, start, end);
        assert_eq!(live_missing.text, "番組内位置未確認");
        assert_eq!(live_missing.ratio, None);

        let brief = program_progress(
            ProbeMode::TimeFree,
            Some(Duration::from_millis(100)),
            start,
            start + chrono::Duration::milliseconds(500),
        );
        assert_eq!(brief.text, "00:00 / 00:00");
        let ratio = brief.ratio.unwrap();
        assert!((ratio - 0.2).abs() < 1e-9);
        assert!((0.0..=1.0).contains(&ratio));
    }

    #[test]
    fn nonfinite_and_negative_seconds_do_not_yield_a_gauge_ratio() {
        for (position, total) in [
            (f64::NAN, 10.0),
            (10.0, f64::NAN),
            (f64::INFINITY, 10.0),
            (10.0, f64::INFINITY),
            (f64::NEG_INFINITY, 10.0),
            (10.0, f64::NEG_INFINITY),
            (-1.0, 10.0),
            (10.0, -5.0),
            (10.0, 0.0),
            (0.0, 0.0),
            (f64::NAN, f64::NAN),
        ] {
            assert_eq!(
                ratio_from_seconds(position, total),
                None,
                "{position} / {total}"
            );
        }
        assert_eq!(ratio_from_seconds(0.0, 10.0), Some(0.0));
        assert_eq!(ratio_from_seconds(10.0, 10.0), Some(1.0));
        assert_eq!(ratio_from_seconds(25.0, 10.0), Some(1.0));
    }

    fn painted(ratio: f64, width: u16) -> (usize, usize) {
        let mut terminal = Terminal::new(TestBackend::new(width, 1)).unwrap();
        terminal
            .draw(|frame| paint_bar(frame, frame.area(), ratio))
            .unwrap();
        let mut filled = 0;
        let mut empty = 0;
        for cell in &terminal.backend().buffer().content {
            match cell.symbol() {
                FILLED => filled += 1,
                UNFILLED => empty += 1,
                _ => {}
            }
        }
        (filled, empty)
    }

    #[test]
    fn gauge_cells_track_the_ratio_and_reject_constants_and_the_wrong_denominator() {
        let (start_filled, start_empty) = painted(0.0, 40);
        assert_eq!(start_filled, 0);
        assert!(start_empty > 0, "a zero ratio still draws the empty bar");

        let (end_filled, end_empty) = painted(1.0, 40);
        assert!(end_filled > 0);
        assert_eq!(end_empty, 0);

        let ratio = 754.0 / 3300.0;
        let (mid_filled, mid_empty) = painted(ratio, 40);
        let bar = mid_filled + mid_empty;
        assert!(bar > 0);
        assert_eq!(mid_filled, (bar as f64 * ratio).floor() as usize);
        assert!(mid_filled > start_filled);
        assert!(mid_filled < end_filled);
        assert_ne!(mid_filled, (bar as f64 * 0.5).floor() as usize);
        assert_ne!(
            mid_filled,
            (bar as f64 * f64::min(754.0 / 100.0, 1.0)).floor() as usize
        );

        let (narrow_filled, narrow_empty) = painted(ratio, BAR_MIN - 1);
        assert_eq!((narrow_filled, narrow_empty), (0, 0));
        let (invalid_filled, invalid_empty) = painted(f64::NAN, 40);
        assert_eq!((invalid_filled, invalid_empty), (0, 0));
        let (negative_filled, negative_empty) = painted(-0.2, 40);
        assert_eq!((negative_filled, negative_empty), (0, 0));
        let (overflow_filled, overflow_empty) = painted(1.1, 40);
        assert_eq!((overflow_filled, overflow_empty), (0, 0));
    }
}
