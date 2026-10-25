//! Observed classification selection. A private draft; never authorizes acquisition.
use crossterm::event::{KeyCode, KeyEvent};
use radiko::domain::Classification;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Style},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
};
use std::collections::BTreeSet;
pub struct Picker {
    pub axis: usize,
    pub ids: BTreeSet<String>,
    pub unclassified: bool,
    choices: Vec<Classification>,
    list: ListState,
    error: String,
}
pub enum Action {
    None,
    Apply,
    Cancel,
}
impl Picker {
    pub fn new(
        axis: usize,
        mut choices: Vec<Classification>,
        ids: Vec<String>,
        unclassified: bool,
    ) -> Self {
        for id in &ids {
            if !choices.iter().any(|class| class.id == *id) {
                choices.push(Classification {
                    id: id.clone(),
                    name: "今回未観測の保存済み指定".into(),
                });
            }
        }
        Self {
            axis,
            ids: ids.into_iter().collect(),
            unclassified,
            choices,
            list: ListState::default().with_selected(Some(0)),
            error: String::new(),
        }
    }
    pub fn handle(&mut self, key: KeyEvent) -> Action {
        let current = self.list.selected().unwrap_or_default();
        match key.code {
            KeyCode::Esc => return Action::Cancel,
            KeyCode::Enter => return Action::Apply,
            KeyCode::Down | KeyCode::Char('j') => self
                .list
                .select(Some((current + 1).min(self.choices.len() + 1))),
            KeyCode::Up | KeyCode::Char('k') => self.list.select(Some(current.saturating_sub(1))),
            KeyCode::Home => self.list.select(Some(0)),
            KeyCode::End => self.list.select(Some(self.choices.len() + 1)),
            KeyCode::Char(' ') => match current {
                0 => {
                    self.ids.clear();
                    self.unclassified = false;
                    self.error.clear();
                }
                1 => {
                    self.unclassified = !self.unclassified;
                    self.error.clear();
                }
                _ => {
                    if let Some(class) = self.choices.get(current - 2) {
                        if self.ids.remove(&class.id) {
                            self.error.clear();
                        } else {
                            let size = self.ids.iter().fold(0usize, |size, id| {
                                size.saturating_add(id.len()).saturating_add(1)
                            });
                            if class.id.trim().is_empty()
                                || class.id.len() > 128
                                || class.id.chars().any(char::is_control)
                            {
                                self.error = "この候補の分類IDは不正なため選択できません".into();
                            } else if size.saturating_add(class.id.len()) > 4096 {
                                self.error =
                                    "分類IDの合計は4096バイトまでです。選択を減らしてください"
                                        .into();
                            } else {
                                self.ids.insert(class.id.clone());
                                self.error.clear();
                            }
                        }
                    }
                }
            },
            _ => (),
        }
        Action::None
    }
    pub fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let panes = Layout::vertical([Constraint::Min(1), Constraint::Length(2)]).split(area);
        let clean = |value: &str| {
            value
                .chars()
                .filter(|c| !c.is_control())
                .take(160)
                .collect::<String>()
        };
        let check = |selected| if selected { "x" } else { " " };
        let mut items = vec![
            ListItem::new(format!(
                "[{}] 全て（分類指定なし）",
                check(self.ids.is_empty() && !self.unclassified)
            )),
            ListItem::new(format!(
                "[{}] 未分類（選択IDとOR）",
                check(self.unclassified)
            )),
        ];
        items.extend(self.choices.iter().map(|c| {
            ListItem::new(format!(
                "[{}] {} / {}",
                check(self.ids.contains(&c.id)),
                clean(&c.name),
                clean(&c.id)
            ))
        }));
        let axis = if self.axis == 0 {
            "番組ジャンル"
        } else {
            "出演者分類"
        };
        frame.render_stateful_widget(
            List::new(items)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(format!("{axis}：今回観測した候補（全分類ではありません）")),
                )
                .highlight_style(Style::default().bg(Color::DarkGray))
                .highlight_symbol("> "),
            panes[0],
            &mut self.list,
        );
        frame.render_widget(Paragraph::new(self.error.as_str()), panes[1]);
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
    fn additions_respect_the_text_limit_without_truncating_existing_selections() {
        let choices: Vec<_> = (0..100)
            .map(|i| Classification {
                id: format!("ID{i:03}_{}", "x".repeat(100)),
                name: "observed".into(),
            })
            .collect();
        let mut picker = Picker::new(0, choices.clone(), vec![], false);
        for row in 0..choices.len() {
            picker.list.select(Some(row + 2));
            picker.handle(key(KeyCode::Char(' ')));
        }
        assert!(
            picker
                .ids
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(",")
                .len()
                <= 4096
        );
        assert!(!picker.error.is_empty());
        let original: Vec<_> = choices.iter().map(|c| c.id.clone()).collect();
        let mut preserved = Picker::new(0, choices, original.clone(), false);
        assert_eq!(preserved.ids.len(), original.len());
        preserved.list.select(Some(2));
        preserved.handle(key(KeyCode::Char(' ')));
        assert_eq!(preserved.ids.len(), original.len() - 1);
        let invalid = Classification {
            id: "bad\u{001b}id".into(),
            name: "invalid metadata".into(),
        };
        let mut invalid = Picker::new(0, vec![invalid], vec![], false);
        invalid.list.select(Some(2));
        invalid.handle(key(KeyCode::Char(' ')));
        assert!(invalid.ids.is_empty());
        assert!(!invalid.error.is_empty());
    }
    #[test]
    fn aliases_share_an_id_and_unobserved_ids_are_preserved_until_deselected() {
        let choices = vec![
            Classification {
                id: "P001".into(),
                name: "news".into(),
            },
            Classification {
                id: "P001".into(),
                name: "alias".into(),
            },
        ];
        let mut picker = Picker::new(0, choices, vec!["saved,unusual".into()], false);
        assert_eq!(picker.choices.len(), 3);
        picker.list.select(Some(2));
        picker.handle(key(KeyCode::Char(' ')));
        assert!(picker.ids.contains("P001"));
        assert!(picker.ids.contains("saved,unusual"));
        picker.list.select(Some(3));
        picker.handle(key(KeyCode::Char(' ')));
        assert!(!picker.ids.contains("P001"));
        assert!(picker.ids.contains("saved,unusual"));
        assert!(matches!(picker.handle(key(KeyCode::Enter)), Action::Apply));
    }
    #[test]
    fn unclassified_and_unrestricted_are_distinct_and_empty_choices_are_navigable() {
        let mut picker = Picker::new(1, vec![], vec![], false);
        picker.handle(key(KeyCode::End));
        picker.handle(key(KeyCode::Char(' ')));
        assert!(picker.unclassified);
        picker.handle(key(KeyCode::Home));
        picker.handle(key(KeyCode::Char(' ')));
        assert!(!picker.unclassified);
        assert!(picker.ids.is_empty());
        assert!(matches!(picker.handle(key(KeyCode::Esc)), Action::Cancel));
    }
}
