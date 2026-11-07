//! Compact main screen for short terminals. Height selects it; width does not.
//!
//! The header is 2 rows and the footer is 3, borders included. Side borders sit
//! on those rows. Top and bottom borders are not added on top of the budget.
use unicode_width::UnicodeWidthStr;

/// `height <= MAX_HEIGHT` is compact. `30` and above keep the normal layout.
pub(super) const MAX_HEIGHT: u16 = 29;

pub(super) fn active(height: u16) -> bool {
    height <= MAX_HEIGHT
}

/// Highest priority first. Items are removed from the right, separator included.
pub(super) const HINT_ITEMS: [&str; 4] = ["? 操作一覧", "Enter 再生", "Space 一時停止", "q 終了"];

/// Whole items joined by ` / `, or empty when even the first item does not fit.
pub(super) fn hint(width: usize) -> String {
    let mut chosen = 0;
    for count in 1..=HINT_ITEMS.len() {
        let candidate = HINT_ITEMS[..count].join(" / ");
        if UnicodeWidthStr::width(candidate.as_str()) <= width {
            chosen = count;
        } else {
            break;
        }
    }
    HINT_ITEMS[..chosen].join(" / ")
}

fn fits(width: usize, text: &str) -> bool {
    UnicodeWidthStr::width(text) <= width
}

/// Identity, then state, then the whole clock. The clock is never split.
/// Volume is not on this line.
pub(super) fn playback_line(identity: &str, state: &str, clock: &str, width: usize) -> String {
    let with_state = if state.is_empty() {
        identity.to_owned()
    } else {
        format!("{identity} {state}")
    };
    let full = if clock.is_empty() {
        with_state.clone()
    } else {
        format!("{with_state} / {clock}")
    };
    if fits(width, &full) {
        full
    } else if fits(width, &with_state) {
        with_state
    } else if fits(width, identity) {
        identity.to_owned()
    } else {
        String::new()
    }
}

/// The first guide sentence, plus the follow-up when both fit on one row.
pub(super) fn guide_line(lines: &[&str], width: usize) -> String {
    let Some(first) = lines.first().copied() else {
        return String::new();
    };
    if let Some(second) = lines.get(1).copied() {
        let both = format!("{first} / {second}");
        if fits(width, &both) {
            return both;
        }
    }
    first.to_owned()
}

#[cfg(test)]
mod tests {
    use super::{guide_line, hint, playback_line};
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn hint_widths_are_the_literal_item_boundaries() {
        assert_eq!(UnicodeWidthStr::width("? 操作一覧"), 10);
        assert_eq!(UnicodeWidthStr::width("? 操作一覧 / Enter 再生"), 23);
        assert_eq!(
            UnicodeWidthStr::width("? 操作一覧 / Enter 再生 / Space 一時停止"),
            40
        );
        assert_eq!(
            UnicodeWidthStr::width("? 操作一覧 / Enter 再生 / Space 一時停止 / q 終了"),
            49
        );
    }

    #[test]
    fn hint_drops_rightmost_whole_items_and_omits_a_partial_first_item() {
        assert_eq!(
            hint(49),
            "? 操作一覧 / Enter 再生 / Space 一時停止 / q 終了"
        );
        assert_eq!(hint(48), "? 操作一覧 / Enter 再生 / Space 一時停止");
        assert_eq!(hint(40), "? 操作一覧 / Enter 再生 / Space 一時停止");
        assert_eq!(hint(39), "? 操作一覧 / Enter 再生");
        assert_eq!(hint(30), "? 操作一覧 / Enter 再生");
        assert_eq!(hint(23), "? 操作一覧 / Enter 再生");
        assert_eq!(hint(22), "? 操作一覧");
        assert_eq!(hint(10), "? 操作一覧");
        assert_eq!(hint(9), "");
        assert_eq!(hint(0), "");
        assert!(!hint(30).contains("Space"), "a partial third item leaked");
        assert!(!hint(30).contains('q'));
        assert!(!hint(9).contains('?'));
        assert!(!hint(9).contains("操作"));
        assert!(!hint(48).ends_with(" / "));
        assert!(!hint(22).contains("Enter"));
    }

    #[test]
    fn playback_line_keeps_state_and_a_whole_clock_or_drops_the_clock() {
        let identity = "再生中: [TBS / タイムフリー]";
        let state = "バッファ中";
        let clock = "12:34 / 55:00";
        let full = "再生中: [TBS / タイムフリー] バッファ中 / 12:34 / 55:00";
        let timed = "再生中: [TBS / タイムフリー] バッファ中";
        assert_eq!(UnicodeWidthStr::width(full), 55);
        assert_eq!(UnicodeWidthStr::width(timed), 39);
        assert_eq!(UnicodeWidthStr::width(identity), 28);
        assert_eq!(playback_line(identity, state, clock, 55), full);
        assert_eq!(playback_line(identity, state, clock, 54), timed);
        assert_eq!(playback_line(identity, state, clock, 39), timed);
        assert_eq!(playback_line(identity, state, clock, 38), identity);
        assert_eq!(playback_line(identity, state, clock, 28), identity);
        assert_eq!(playback_line(identity, state, clock, 27), "");
        let dropped = playback_line(identity, state, clock, 54);
        assert!(dropped.contains(state));
        assert!(!dropped.contains(clock));
        assert!(!dropped.contains("12:3"));
        assert!(!dropped.contains("音量"));
    }

    #[test]
    fn guide_follow_up_stays_off_the_row_unless_the_whole_second_sentence_fits() {
        let first = "出力先がありません。OSのサウンド設定を確認し、Enterで再生し直してください";
        let second = "自動再開はしません";
        assert_eq!(UnicodeWidthStr::width(first), 73);
        assert_eq!(UnicodeWidthStr::width(second), 18);
        let lines = [first, second];
        assert_eq!(guide_line(&lines, 78), first);
        assert!(!guide_line(&lines, 78).contains(second));
        let both = format!("{first} / {second}");
        assert_eq!(UnicodeWidthStr::width(both.as_str()), 94);
        assert_eq!(guide_line(&lines, 94), both);
        assert_eq!(guide_line(&lines, 93), first);
    }
}
