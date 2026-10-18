//! Local prefecture selection; station discovery occurs only after search confirmation.
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    widgets::{Block, Borders, List, ListItem, ListState},
};
use std::collections::BTreeSet;
const PREFECTURES: [&str; 47] = [
    "北海道",
    "青森県",
    "岩手県",
    "宮城県",
    "秋田県",
    "山形県",
    "福島県",
    "茨城県",
    "栃木県",
    "群馬県",
    "埼玉県",
    "千葉県",
    "東京都",
    "神奈川県",
    "新潟県",
    "富山県",
    "石川県",
    "福井県",
    "山梨県",
    "長野県",
    "岐阜県",
    "静岡県",
    "愛知県",
    "三重県",
    "滋賀県",
    "京都府",
    "大阪府",
    "兵庫県",
    "奈良県",
    "和歌山県",
    "鳥取県",
    "島根県",
    "岡山県",
    "広島県",
    "山口県",
    "徳島県",
    "香川県",
    "愛媛県",
    "高知県",
    "福岡県",
    "佐賀県",
    "長崎県",
    "熊本県",
    "大分県",
    "宮崎県",
    "鹿児島県",
    "沖縄県",
];
pub struct Picker {
    selected: BTreeSet<usize>,
    list: ListState,
}
pub enum Action {
    None,
    Apply(String),
    Cancel,
}
impl Picker {
    pub fn new(value: &str) -> Self {
        let selected = value
            .split(',')
            .filter_map(|part| {
                let part = part.trim();
                (1..=47).find(|id| part == format!("JP{id}"))
            })
            .collect();
        Self {
            selected,
            list: ListState::default().with_selected(Some(0)),
        }
    }
    pub fn handle(&mut self, key: KeyEvent) -> Action {
        let current = self.list.selected().unwrap_or_default();
        match key.code {
            KeyCode::Esc => return Action::Cancel,
            KeyCode::Enter => {
                return Action::Apply(if self.selected.is_empty() {
                    "全国".into()
                } else {
                    self.selected
                        .iter()
                        .map(|id| format!("JP{id}"))
                        .collect::<Vec<_>>()
                        .join(",")
                });
            }
            KeyCode::Down | KeyCode::Char('j') => self.list.select(Some((current + 1).min(47))),
            KeyCode::Up | KeyCode::Char('k') => self.list.select(Some(current.saturating_sub(1))),
            KeyCode::Home => self.list.select(Some(0)),
            KeyCode::End => self.list.select(Some(47)),
            KeyCode::PageDown => self.list.select(Some((current + 10).min(47))),
            KeyCode::PageUp => self.list.select(Some(current.saturating_sub(10))),
            KeyCode::Char(' ') => {
                if current == 0 {
                    self.selected.clear();
                } else if !self.selected.remove(&current) {
                    self.selected.insert(current);
                }
            }
            _ => (),
        }
        Action::None
    }
    pub fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let mut items = vec![ListItem::new(format!(
            "[{}] 全国（地域指定なし）",
            if self.selected.is_empty() { "x" } else { " " }
        ))];
        items.extend(PREFECTURES.iter().enumerate().map(|(index, name)| {
            ListItem::new(format!(
                "[{}] {} / JP{}",
                if self.selected.contains(&(index + 1)) {
                    "x"
                } else {
                    " "
                },
                name,
                index + 1
            ))
        }));
        frame.render_stateful_widget(
            List::new(items)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title("聴取可能局を調べる地域（適用時に単一局指定を置換）"),
                )
                .highlight_style(Style::default().bg(Color::DarkGray))
                .highlight_symbol("> "),
            area,
            &mut self.list,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;
    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    #[test]
    fn existing_regions_toggle_without_duplicates_and_all_clears_selection() {
        let mut picker = Picker::new("JP13, JP44,JP13");
        assert!(
            matches!(picker.handle(key(KeyCode::Enter)), Action::Apply(value) if value == "JP13,JP44")
        );
        picker.handle(key(KeyCode::End));
        picker.handle(key(KeyCode::Char(' ')));
        assert!(
            matches!(picker.handle(key(KeyCode::Enter)), Action::Apply(value) if value == "JP13,JP44,JP47")
        );
        picker.handle(key(KeyCode::Home));
        picker.handle(key(KeyCode::Char(' ')));
        assert!(
            matches!(picker.handle(key(KeyCode::Enter)), Action::Apply(value) if value == "全国")
        );
        assert!(matches!(picker.handle(key(KeyCode::Esc)), Action::Cancel));
    }
    #[test]
    fn every_prefecture_has_a_roundtripping_id_and_tiny_terminals_are_safe() {
        assert_eq!(PREFECTURES[12], "東京都");
        assert_eq!(PREFECTURES[43], "大分県");
        assert_eq!(PREFECTURES[46], "沖縄県");
        assert_eq!(PREFECTURES.into_iter().collect::<BTreeSet<_>>().len(), 47);
        for id in 1..=47 {
            let value = format!("JP{id}");
            let mut picker = Picker::new(&value);
            assert!(
                matches!(picker.handle(key(KeyCode::Enter)), Action::Apply(selected) if selected == value)
            );
        }
        let mut picker = Picker::new("局:TBS");
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(8, 4)).unwrap();
        terminal
            .draw(|frame| picker.draw(frame, frame.area()))
            .unwrap();
    }
}
