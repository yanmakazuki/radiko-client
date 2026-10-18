//! Named-criteria browser. Recalling edits a draft; it never starts acquisition.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use radiko_client::{
    app::named::{Operation, ViewId},
    domain::search::{Scope, SearchCriteria},
};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Style},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};
use std::collections::BTreeMap;
enum Mode {
    Browse,
    Name(String),
    Delete(String),
}
pub(super) enum Action {
    None,
    Close,
    Quit,
    Operation(Operation),
}
pub(super) struct Manager {
    pub view: ViewId,
    pub busy: bool,
    pub notice: String,
    items: BTreeMap<String, SearchCriteria>,
    list: ListState,
    mode: Mode,
}
impl Manager {
    pub fn new(view: ViewId) -> Self {
        Self {
            view,
            busy: true,
            notice: "読み込み中".into(),
            items: BTreeMap::new(),
            list: ListState::default(),
            mode: Mode::Browse,
        }
    }
    pub fn listed(&mut self, items: BTreeMap<String, SearchCriteria>) {
        let previous = self.selected().map(|(name, _)| name.clone());
        let position = previous
            .and_then(|name| items.keys().position(|key| key == &name))
            .unwrap_or_default();
        self.list.select(if items.is_empty() {
            None
        } else {
            Some(position.min(items.len() - 1))
        });
        self.items = items;
        self.busy = false;
        self.mode = Mode::Browse;
        self.notice = if self.items.is_empty() {
            "保存済み条件はありません。nで確定済み条件を保存できます".into()
        } else {
            "Enterで保存日付も含めて編集画面へ呼び出します（検索はまだ行いません）".into()
        };
    }
    fn selected(&self) -> Option<(&String, &SearchCriteria)> {
        self.items.iter().nth(self.list.selected()?)
    }
    pub fn handle(&mut self, key: KeyEvent, criteria: &SearchCriteria) -> Action {
        if self.busy {
            return match key.code {
                KeyCode::Esc => Action::Close,
                KeyCode::Char('q') => Action::Quit,
                _ => Action::None,
            };
        }
        match &mut self.mode {
            Mode::Name(name) => match key.code {
                KeyCode::Esc => {
                    self.mode = Mode::Browse;
                    Action::None
                }
                KeyCode::Enter => {
                    self.busy = true;
                    Action::Operation(Operation::Save(name.clone(), Box::new(criteria.clone())))
                }
                KeyCode::Backspace => {
                    name.pop();
                    Action::None
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    name.clear();
                    Action::None
                }
                KeyCode::Char(c)
                    if !c.is_control()
                        && !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                        && name.chars().count() < 64
                        && name.len() + c.len_utf8() <= 256 =>
                {
                    name.push(c);
                    Action::None
                }
                _ => Action::None,
            },
            Mode::Delete(name) => match key.code {
                KeyCode::Char('y') => {
                    self.busy = true;
                    Action::Operation(Operation::Delete(name.clone()))
                }
                KeyCode::Esc | KeyCode::Char('n') => {
                    self.mode = Mode::Browse;
                    Action::None
                }
                _ => Action::None,
            },
            Mode::Browse => match key.code {
                KeyCode::Esc => Action::Close,
                KeyCode::Char('q') => Action::Quit,
                KeyCode::Char('n') => {
                    self.mode = Mode::Name(String::new());
                    self.notice = "確定済み条件を保存します。同名の条件は更新します".into();
                    Action::None
                }
                KeyCode::Char('r') => {
                    self.busy = true;
                    Action::Operation(Operation::List)
                }
                KeyCode::Enter => {
                    if let Some((name, _)) = self.selected() {
                        let name = name.clone();
                        self.busy = true;
                        Action::Operation(Operation::Recall(name))
                    } else {
                        Action::None
                    }
                }
                KeyCode::Char('d') => {
                    if let Some((name, _)) = self.selected() {
                        self.mode = Mode::Delete(name.clone());
                    }
                    Action::None
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if !self.items.is_empty() {
                        self.list.select(Some(
                            (self.list.selected().unwrap_or_default() + 1)
                                .min(self.items.len() - 1),
                        ));
                    }
                    Action::None
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    if !self.items.is_empty() {
                        self.list.select(Some(
                            self.list.selected().unwrap_or_default().saturating_sub(1),
                        ));
                    }
                    Action::None
                }
                _ => Action::None,
            },
        }
    }
    pub fn controls(&self) -> &'static str {
        if self.busy {
            return "保存条件の操作中 / Esc 戻る / q・Ctrl+C 終了";
        }
        match self.mode {
            Mode::Browse => {
                "Enter 呼び出し / n 確定済み条件を保存 / d 削除 / r 再読込 / Esc 戻る / q 終了"
            }
            Mode::Name(_) => {
                "Enter 保存（同名は更新） / Backspace・Ctrl+U 編集 / Esc 取消 / Ctrl+C 終了"
            }
            Mode::Delete(_) => "y 削除 / n・Esc 取消 / Ctrl+C 終了",
        }
    }
    pub fn draw(&mut self, frame: &mut Frame, area: Rect, committed: &SearchCriteria) {
        let panes = Layout::vertical([Constraint::Min(1), Constraint::Length(8)]).split(area);
        let items: Vec<_> = self
            .items
            .keys()
            .map(|name| ListItem::new(name.clone()))
            .collect();
        frame.render_stateful_widget(
            List::new(items)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title("名前付き検索条件 / n 保存 / d 削除 / r 再読込 / Esc 戻る"),
                )
                .highlight_style(Style::default().bg(Color::DarkGray))
                .highlight_symbol("▶ "),
            panes[0],
            &mut self.list,
        );
        let text = match &self.mode {
            Mode::Name(name) => format!(
                "保存名: {name}▏\nEnter 保存（同名は更新） / Esc 取消 / Ctrl+U 消去\n{}",
                describe(committed)
            ),
            Mode::Delete(name) => format!("「{name}」を削除しますか？ y 削除 / n・Esc 取消"),
            Mode::Browse => self
                .selected()
                .map(|(_, criteria)| describe(criteria))
                .unwrap_or_default(),
        };
        frame.render_widget(
            Paragraph::new(format!(
                "{}{}\n{}",
                if self.busy { "処理中 / " } else { "" },
                self.notice,
                text
            ))
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("保存条件の詳細（通信なし）"),
            ),
            panes[1],
        );
    }
}
fn describe(criteria: &SearchCriteria) -> String {
    let filter = criteria.filter();
    let scope = match criteria.scope() {
        Scope::Nationwide => "全国".into(),
        Scope::Station(id) => format!("局:{id}"),
        Scope::Areas(ids) => ids.join(","),
    };
    let switch = |value| if value { "ON" } else { "OFF" };
    format!(
        "{} / {}〜{} JST\n番組ジャンル: {} / 未分類指定{}\n出演者分類: {} / 未分類指定{}\nキーワード: {}",
        scope,
        criteria.range().start(),
        criteria.range().end(),
        filter.program_genres().join(","),
        switch(filter.unclassified_program()),
        filter.personality_genres().join(","),
        switch(filter.unclassified_personality()),
        filter.keyword().unwrap_or("指定なし")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn criteria() -> SearchCriteria {
        SearchCriteria::initial(
            chrono::DateTime::parse_from_rfc3339("2026-10-17T06:00:00+09:00").unwrap(),
        )
        .unwrap()
    }
    #[test]
    fn deletion_requires_explicit_confirmation_and_cancel_never_produces_an_operation() {
        let mut manager = Manager::new(ViewId(1));
        manager.listed(BTreeMap::from([("日本語".into(), criteria())]));
        assert!(matches!(
            manager.handle(
                KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
                &criteria()
            ),
            Action::None
        ));
        assert!(matches!(
            manager.handle(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &criteria()),
            Action::None
        ));
        assert!(matches!(
            manager.handle(
                KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
                &criteria()
            ),
            Action::None
        ));
        assert!(
            matches!(manager.handle(KeyEvent::new(KeyCode::Char('y'),KeyModifiers::NONE),&criteria()),Action::Operation(Operation::Delete(name)) if name=="日本語")
        );
        assert!(manager.busy);
    }
    #[test]
    fn unicode_names_and_empty_lists_render_at_small_sizes() {
        use ratatui::{Terminal, backend::TestBackend};
        let mut manager = Manager::new(ViewId(1));
        manager.listed(Default::default());
        manager.handle(
            KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE),
            &criteria(),
        );
        for c in "日本語".chars() {
            manager.handle(
                KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
                &criteria(),
            );
        }
        let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
        terminal
            .draw(|f| manager.draw(f, f.area(), &criteria()))
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .filter(|c| c.symbol() != " ")
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("保存名:日本語"));
        let mut tiny = Terminal::new(TestBackend::new(8, 4)).unwrap();
        tiny.draw(|f| manager.draw(f, f.area(), &criteria()))
            .unwrap();
    }
}
