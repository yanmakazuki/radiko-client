//! Wrap-aware scroll state for bounded read-only text.
use crossterm::event::KeyCode;
#[derive(Default)]
pub(super) struct Scroll {
    pub(super) offset: u16,
    pub(super) maximum: u16,
    pub(super) page: u16,
}
impl Scroll {
    pub fn fit(&mut self, lines: usize, height: u16) {
        self.page = height;
        self.maximum = lines
            .saturating_sub(usize::from(height))
            .min(usize::from(u16::MAX)) as u16;
        self.offset = self.offset.min(self.maximum);
    }

    pub fn key(&mut self, key: KeyCode) {
        self.offset = match key {
            KeyCode::Up | KeyCode::Char('k') => self.offset.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.offset.saturating_add(1),
            KeyCode::PageUp => self.offset.saturating_sub(self.page.max(1)),
            KeyCode::PageDown => self.offset.saturating_add(self.page.max(1)),
            KeyCode::Home => 0,
            KeyCode::End => self.maximum,
            _ => return,
        }
        .min(self.maximum);
    }
}
