//! Display-width fitting for program titles and chapter names.
//!
//! Width uses `unicode-width` 0.2 `UnicodeWidthStr::width`, the same function
//! Ratatui 0.29 uses when it places grapheme clusters into cells. Ambiguous
//! characters, including `…` and `▶`, are one column. `width_cjk` is not used.
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub(super) const ELLIPSIS: &str = "…";
pub(super) const HIGHLIGHT: &str = "▶ ";

pub(super) fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

/// Fit `text` into `width` columns.
///
/// A string that fits, including an exact fit, is returned unchanged. On
/// overflow the ellipsis is reserved first, then the longest prefix of whole
/// grapheme clusters that fits in the remainder is kept. The next cluster is
/// never split or skipped. Width 0 is empty. When only the ellipsis fits, it
/// is returned alone. A cluster wider than the remainder stops the scan.
pub(super) fn fit_label(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if display_width(text) <= width {
        return text.to_owned();
    }
    let mark = display_width(ELLIPSIS);
    if mark == 0 || mark > width {
        return String::new();
    }
    let budget = width - mark;
    let mut kept = String::new();
    let mut used = 0usize;
    if budget > 0 {
        for cluster in text.graphemes(true) {
            let cluster_width = display_width(cluster);
            let Some(next) = used.checked_add(cluster_width) else {
                break;
            };
            if next > budget {
                break;
            }
            kept.push_str(cluster);
            used = next;
        }
    }
    kept.push_str(ELLIPSIS);
    kept
}

/// Columns left for a list item after the bordered frame and, when a row is
/// selected, the highlight gutter Ratatui reserves on every row.
pub(super) fn list_text_columns(area_width: u16, selected: bool) -> usize {
    use ratatui::{
        layout::Rect,
        widgets::{Block, Borders},
    };
    let inner = Block::default()
        .borders(Borders::ALL)
        .inner(Rect::new(0, 0, area_width, 3));
    let gutter = if selected {
        display_width(HIGHLIGHT)
    } else {
        0
    };
    usize::from(inner.width).saturating_sub(gutter)
}

pub(super) fn fit_after_prefix(prefix: &str, text: &str, content_width: usize) -> String {
    fit_label(text, content_width.saturating_sub(display_width(prefix)))
}
