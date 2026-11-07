//! One-line editor for the keyword field, condition fields, and named-condition names.
//!
//! The cursor is a byte index on an extended grapheme boundary
//! (`unicode-segmentation` 1.13 `graphemes(true)`). Display columns use
//! `unicode-width` 0.2 `UnicodeWidthStr::width`, the same function Ratatui 0.29
//! uses. Ambiguous width is 1. `width_cjk` is not used.
//!
//! Keys insert only the confirmed character crossterm already delivered.
//! IME preedit and candidate windows are not controlled here.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Position, Rect},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct LineEditor {
    value: String,
    /// Byte index. Always an extended grapheme boundary of `value`.
    cursor: usize,
    byte_limit: usize,
    scalar_limit: Option<usize>,
}

/// The single-line window that follows the cursor.
///
/// `text` is a concatenation of whole graphemes. `origin` is its byte start in
/// the value. `cursor_column` is the display width of the visible prefix before
/// the cursor, and it is always `< columns` when `columns > 0`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FieldView {
    pub text: String,
    pub cursor_column: usize,
    pub origin: usize,
}

impl LineEditor {
    pub(super) fn new(value: impl Into<String>, byte_limit: usize) -> Self {
        let value = value.into();
        let cursor = value.len();
        Self {
            value,
            cursor,
            byte_limit,
            scalar_limit: None,
        }
    }

    pub(super) fn with_scalar_limit(mut self, limit: usize) -> Self {
        self.scalar_limit = Some(limit);
        self
    }

    pub(super) fn text(&self) -> &str {
        &self.value
    }

    #[cfg(test)]
    pub(super) fn cursor(&self) -> usize {
        self.cursor
    }

    #[cfg(test)]
    pub(super) fn set_byte_limit(&mut self, limit: usize) {
        self.byte_limit = limit;
    }

    pub(super) fn handle(&mut self, key: KeyEvent) {
        self.cursor = snap(&self.value, self.cursor.min(self.value.len()));
        match key.code {
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.value.clear();
                self.cursor = 0;
            }
            KeyCode::Char(c)
                if !c.is_control()
                    && !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.insert(c);
            }
            KeyCode::Backspace => self.backspace(),
            KeyCode::Delete => self.delete_forward(),
            KeyCode::Left => self.cursor = previous(&self.value, self.cursor),
            KeyCode::Right => self.cursor = next_boundary(&self.value, self.cursor),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.value.len(),
            _ => {}
        }
        self.cursor = snap(&self.value, self.cursor.min(self.value.len()));
    }

    fn insert(&mut self, c: char) {
        if self.value.len().saturating_add(c.len_utf8()) > self.byte_limit {
            return;
        }
        if self
            .scalar_limit
            .is_some_and(|limit| self.value.chars().count() >= limit)
        {
            return;
        }
        self.value.insert(self.cursor, c);
        self.cursor = self.cursor.saturating_add(c.len_utf8());
    }

    fn backspace(&mut self) {
        let start = previous(&self.value, self.cursor);
        if start == self.cursor {
            return;
        }
        self.value.drain(start..self.cursor);
        self.cursor = start;
    }

    fn delete_forward(&mut self) {
        let end = next_boundary(&self.value, self.cursor);
        if end == self.cursor {
            return;
        }
        self.value.drain(self.cursor..end);
    }

    pub(super) fn view(&self, columns: usize) -> FieldView {
        let cursor = snap(&self.value, self.cursor.min(self.value.len()));
        if columns == 0 {
            return FieldView {
                text: String::new(),
                cursor_column: 0,
                origin: cursor,
            };
        }
        let bounds = clusters(&self.value);
        let index = bounds
            .iter()
            .position(|(start, _)| *start == cursor)
            .unwrap_or(bounds.len());
        let budget = columns - 1;
        let mut first = index;
        let mut used = 0usize;
        while first > 0 {
            let (start, end) = bounds[first - 1];
            let width = display_width(&self.value[start..end]);
            if used.saturating_add(width) > budget {
                break;
            }
            used = used.saturating_add(width);
            first -= 1;
        }
        let mut before = String::new();
        for &(start, end) in bounds.iter().take(index).skip(first) {
            before.push_str(&self.value[start..end]);
        }
        while first < index && display_width(&before) > budget {
            let (start, end) = bounds[first];
            before.replace_range(0..end - start, "");
            first += 1;
        }
        let cursor_column = display_width(&before);
        let mut text = before;
        for &(start, end) in bounds.iter().skip(index) {
            let cluster = &self.value[start..end];
            if display_width(&text).saturating_add(display_width(cluster)) > columns {
                break;
            }
            text.push_str(cluster);
        }
        let origin = bounds.get(first).map(|(start, _)| *start).unwrap_or(cursor);
        FieldView {
            text,
            cursor_column,
            origin,
        }
    }
}

impl std::ops::Deref for LineEditor {
    type Target = str;

    fn deref(&self) -> &str {
        &self.value
    }
}

impl From<&str> for LineEditor {
    fn from(value: &str) -> Self {
        Self::new(value, 1024)
    }
}

impl From<String> for LineEditor {
    fn from(value: String) -> Self {
        Self::new(value, 1024)
    }
}

pub(super) fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

#[cfg(test)]
pub(super) fn is_grapheme_boundary(value: &str, index: usize) -> bool {
    index <= value.len()
        && (index == 0
            || index == value.len()
            || clusters(value).iter().any(|(start, _)| *start == index))
}

/// Place the terminal cursor `column` display cells into `area`.
///
/// Out of range columns are not placed. A zero-size area is a no-op.
pub(super) fn place_cursor(frame: &mut Frame, area: Rect, column: usize) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let Ok(column) = u16::try_from(column) else {
        return;
    };
    let Some(x) = area.x.checked_add(column) else {
        return;
    };
    let y = area.y;
    if x >= area.right() || y >= area.bottom() {
        return;
    }
    let frame_area = frame.area();
    if x >= frame_area.width || y >= frame_area.height {
        return;
    }
    frame.set_cursor_position(Position::new(x, y));
}

fn clusters(value: &str) -> Vec<(usize, usize)> {
    let mut bounds = Vec::new();
    let mut start = 0usize;
    for cluster in value.graphemes(true) {
        let end = start + cluster.len();
        bounds.push((start, end));
        start = end;
    }
    bounds
}

fn snap(value: &str, cursor: usize) -> usize {
    if cursor >= value.len() {
        return value.len();
    }
    let mut start = 0usize;
    for cluster in value.graphemes(true) {
        let end = start + cluster.len();
        if cursor <= start {
            return start;
        }
        if cursor < end {
            return end;
        }
        start = end;
    }
    value.len()
}

fn previous(value: &str, cursor: usize) -> usize {
    let cursor = snap(value, cursor.min(value.len()));
    let mut prior = 0usize;
    for (start, _) in clusters(value) {
        if start == cursor {
            return prior;
        }
        prior = start;
    }
    prior
}

fn next_boundary(value: &str, cursor: usize) -> usize {
    let cursor = snap(value, cursor.min(value.len()));
    for (start, end) in clusters(value) {
        if start == cursor {
            return end;
        }
    }
    value.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEventKind;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn editor(value: &str) -> LineEditor {
        LineEditor::new(value, 1024)
    }

    #[test]
    fn movement_stays_on_extended_grapheme_boundaries() {
        let accent = "e\u{0301}";
        assert_eq!(accent.graphemes(true).count(), 1);
        let mut text = editor(accent);
        text.handle(key(KeyCode::Left));
        assert_eq!(text.cursor(), 0);
        assert!(is_grapheme_boundary(text.text(), text.cursor()));
        text.handle(key(KeyCode::Right));
        assert_eq!(text.cursor(), accent.len());

        let voiced = "か\u{3099}";
        assert_eq!(voiced.graphemes(true).count(), 1);
        let mut text = editor(voiced);
        text.handle(key(KeyCode::Left));
        assert_eq!(text.cursor(), 0, "decomposed か+dakuten is one cluster");

        let family = "👨\u{200d}👩\u{200d}👧\u{200d}👦";
        assert_eq!(family.graphemes(true).count(), 1);
        let mut text = editor(family);
        text.handle(key(KeyCode::Home));
        text.handle(key(KeyCode::Right));
        assert_eq!(text.cursor(), family.len());
        text.handle(key(KeyCode::Delete));
        assert_eq!(text.text(), family);
        text.handle(key(KeyCode::Home));
        text.handle(key(KeyCode::Delete));
        assert!(text.text().is_empty());
        assert_eq!(text.cursor(), 0);
    }

    #[test]
    fn inserting_a_combining_mark_next_to_a_base_keeps_a_boundary() {
        let mut text = editor("\u{0301}い");
        text.handle(key(KeyCode::Home));
        text.handle(key(KeyCode::Char('a')));
        let cluster = "a\u{0301}";
        assert_eq!(text.text(), "a\u{0301}い");
        assert_eq!(text.cursor(), cluster.len());
        assert!(is_grapheme_boundary(text.text(), text.cursor()));
        text.handle(key(KeyCode::Home));
        text.handle(key(KeyCode::Right));
        assert_eq!(text.cursor(), cluster.len());
        assert_ne!(text.cursor(), 1);

        let mut text = editor("あい");
        text.handle(key(KeyCode::Home));
        text.handle(key(KeyCode::Right));
        assert_eq!(text.cursor(), "あ".len());
        text.handle(key(KeyCode::Char('\u{0301}')));
        assert_eq!(text.text(), "あ\u{0301}い");
        assert_eq!(text.cursor(), "あ\u{0301}".len());
        text.handle(key(KeyCode::Left));
        assert_eq!(text.cursor(), 0);
    }

    #[test]
    fn view_keeps_whole_graphemes_and_uses_display_width() {
        assert_eq!(display_width("あ"), 2);
        assert_eq!("あ".len(), 3);
        let end = editor("あ");
        let wide = end.view(10);
        assert_eq!(wide.text, "あ");
        assert_eq!(wide.cursor_column, 2);
        assert_ne!(wide.cursor_column, "あ".len());

        let mixed = editor("aあ");
        let shown = mixed.view(10);
        assert_eq!(shown.cursor_column, display_width("aあ"));
        assert_eq!(shown.cursor_column, 3);
        assert_ne!(shown.cursor_column, "aあ".len());

        let narrow = editor("あい");
        let scrolled = narrow.view(3);
        assert_eq!(scrolled.text, "い");
        assert_eq!(scrolled.origin, "あ".len());
        assert!(is_grapheme_boundary(narrow.text(), scrolled.origin));
        assert_eq!(scrolled.cursor_column, 2);
        assert!(display_width(&scrolled.text) <= 3);

        let cluster = editor("あか\u{3099}");
        let window = cluster.view(3);
        assert!(window.text.starts_with('か'));
        assert!(window.text.contains('\u{3099}'));
        assert!(!window.text.starts_with('\u{3099}'));
        assert!(is_grapheme_boundary(cluster.text(), window.origin));
        assert_eq!(
            &cluster.text()[window.origin..window.origin + window.text.len()],
            window.text
        );

        assert!(editor("あ").view(0).text.is_empty());
        assert_eq!(editor("").view(4).cursor_column, 0);
    }

    #[test]
    fn edges_and_limits_do_not_panic_or_grow() {
        let mut text = editor("");
        for code in [
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Home,
            KeyCode::End,
            KeyCode::Backspace,
            KeyCode::Delete,
        ] {
            text.handle(key(code));
        }
        assert!(text.text().is_empty());
        assert_eq!(text.cursor(), 0);

        text.handle(key(KeyCode::Char('a')));
        text.handle(key(KeyCode::Home));
        text.handle(key(KeyCode::Backspace));
        assert_eq!(text.text(), "a");
        text.handle(key(KeyCode::End));
        text.handle(key(KeyCode::Delete));
        assert_eq!(text.text(), "a");

        text.handle(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        text.set_byte_limit(2);
        text.handle(key(KeyCode::Char('あ')));
        assert!(text.text().is_empty());
        text.handle(key(KeyCode::Char('é')));
        text.handle(key(KeyCode::Char('x')));
        assert_eq!(text.text(), "é");

        let mut named = LineEditor::new(String::new(), 256).with_scalar_limit(64);
        for _ in 0..64 {
            named.handle(key(KeyCode::Char('名')));
        }
        assert_eq!(named.chars().count(), 64);
        named.handle(key(KeyCode::Char('名')));
        assert_eq!(named.chars().count(), 64);
        named.handle(KeyEvent::new_with_kind(
            KeyCode::Char('u'),
            KeyModifiers::CONTROL,
            KeyEventKind::Press,
        ));
        assert!(named.text().is_empty());
    }
}
