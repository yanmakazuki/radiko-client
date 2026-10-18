//! Local form editing and validation. No network or storage operations occur here.
use chrono::NaiveDate;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use radiko_client::{
    catalog::search::ObservedClassifications,
    domain::{
        BroadcastRange, Classification, Program,
        search::{Filter, Scope, SearchCriteria},
    },
};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Style},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
};

#[derive(Clone)]
struct Text {
    value: String,
    cursor: usize,
}
impl Text {
    fn new(value: String) -> Self {
        let cursor = value.len();
        Self { value, cursor }
    }
    fn key(&mut self, key: KeyEvent, limit: usize) {
        let previous = self.value[..self.cursor]
            .char_indices()
            .next_back()
            .map(|(i, _)| i);
        let next = self.value[self.cursor..]
            .chars()
            .next()
            .map(|c| self.cursor + c.len_utf8());
        match key.code {
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.value.clear();
                self.cursor = 0;
            }
            KeyCode::Char(c)
                if !c.is_control()
                    && !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                    && self.value.len() + c.len_utf8() <= limit =>
            {
                self.value.insert(self.cursor, c);
                self.cursor += c.len_utf8();
            }
            KeyCode::Backspace => {
                if let Some(previous) = previous {
                    self.value.drain(previous..self.cursor);
                    self.cursor = previous;
                }
            }
            KeyCode::Delete => {
                if let Some(next) = next {
                    self.value.drain(self.cursor..next);
                }
            }
            KeyCode::Left => self.cursor = previous.unwrap_or(0),
            KeyCode::Right => self.cursor = next.unwrap_or(self.value.len()),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.value.len(),
            _ => (),
        }
    }
    fn display(&self, focused: bool) -> String {
        if !focused {
            return self.value.chars().take(50).collect();
        }
        let mut before: Vec<_> = self.value[..self.cursor].chars().rev().take(28).collect();
        before.reverse();
        format!(
            "{}▏{}",
            before.into_iter().collect::<String>(),
            self.value[self.cursor..]
                .chars()
                .take(12)
                .collect::<String>()
        )
    }
}
pub(super) enum Action {
    Cancel,
    Submit,
    Edit,
    None,
}
pub(super) struct Editor {
    picker: Option<super::picker::Picker>,
    area_picker: Option<super::areas::Picker>,
    choices: [Vec<Classification>; 2],
    selected_ids: [Option<Vec<String>>; 2],
    fields: [Text; 6],
    unclassified: [bool; 2],
    focus: usize,
    list: ListState,
    original: SearchCriteria,
    observed: Vec<String>,
    observed_offset: usize,
    pub error: String,
}
impl Editor {
    pub fn new(criteria: &SearchCriteria, programs: &[Program]) -> Self {
        let filter = criteria.filter();
        let scope = match criteria.scope() {
            Scope::Nationwide => "全国".into(),
            Scope::Areas(ids) => ids.join(","),
            Scope::Station(id) => format!("局:{id}"),
        };
        let observed = ObservedClassifications::collect(programs);
        let choices = [
            observed.program_genres.clone(),
            observed.personality_genres.clone(),
        ];
        let mut options = vec![
            "編集開始時点で観測した分類（全分類ではありません） / PageUp・PageDownで閲覧".into(),
        ];
        options.extend(
            observed
                .program_genres
                .into_iter()
                .map(|c| format!("番組 {}: {}", c.id, c.name)),
        );
        options.extend(
            observed
                .personality_genres
                .into_iter()
                .map(|c| format!("出演者 {}: {}", c.id, c.name)),
        );
        options.push(format!(
            "未分類: 番組{}枠 / 出演者{}枠",
            observed.unclassified_programs, observed.unclassified_personalities
        ));
        Self {
            picker: None,
            area_picker: None,
            choices,
            selected_ids: [None, None],
            fields: [
                Text::new(criteria.range().start().format("%Y%m%d").to_string()),
                Text::new(if criteria.range().start() == criteria.range().end() {
                    String::new()
                } else {
                    criteria.range().end().format("%Y%m%d").to_string()
                }),
                Text::new(scope),
                Text::new(filter.program_genres().join(",")),
                Text::new(filter.personality_genres().join(",")),
                Text::new(filter.keyword().unwrap_or_default().into()),
            ],
            unclassified: [
                filter.unclassified_program(),
                filter.unclassified_personality(),
            ],
            focus: 0,
            list: ListState::default().with_selected(Some(0)),
            original: criteria.clone(),
            observed: options,
            observed_offset: 0,
            error: String::new(),
        }
    }
    pub fn handle(&mut self, key: KeyEvent) -> Action {
        if let Some(picker) = &mut self.area_picker {
            match picker.handle(key) {
                super::areas::Action::Cancel => self.area_picker = None,
                super::areas::Action::Apply(value) => {
                    self.fields[2] = Text::new(value);
                    self.area_picker = None;
                    return Action::Edit;
                }
                super::areas::Action::None => (),
            }
            return Action::None;
        }
        if let Some(picker) = &mut self.picker {
            match picker.handle(key) {
                super::picker::Action::Cancel => {
                    self.picker = None;
                }
                super::picker::Action::Apply => {
                    let axis = picker.axis;
                    let ids: Vec<_> = picker.ids.iter().cloned().collect();
                    self.fields[axis + 3] = Text::new(ids.join(","));
                    self.unclassified[axis] = picker.unclassified;
                    self.selected_ids[axis] = Some(ids);
                    self.picker = None;
                    return Action::Edit;
                }
                super::picker::Action::None => (),
            }
            return Action::None;
        }
        match key.code {
            KeyCode::F(2) if self.focus == 2 => {
                self.area_picker = Some(super::areas::Picker::new(&self.fields[2].value))
            }
            KeyCode::F(2) if (3..5).contains(&self.focus) => {
                let axis = self.focus - 3;
                self.picker = Some(super::picker::Picker::new(
                    axis,
                    self.choices[axis].clone(),
                    self.ids(axis),
                    self.unclassified[axis],
                ));
            }
            KeyCode::Esc => return Action::Cancel,
            KeyCode::Enter => return Action::Submit,
            KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.focus = (self.focus + 8) % 9
            }
            KeyCode::Tab | KeyCode::Down => self.focus = (self.focus + 1) % 9,
            KeyCode::BackTab | KeyCode::Up => self.focus = (self.focus + 8) % 9,
            KeyCode::PageDown => {
                self.observed_offset =
                    (self.observed_offset + 3).min(self.observed.len().saturating_sub(1))
            }
            KeyCode::PageUp => self.observed_offset = self.observed_offset.saturating_sub(3),
            KeyCode::Char(' ') if (6..8).contains(&self.focus) => {
                self.unclassified[self.focus - 6] = !self.unclassified[self.focus - 6];
                return Action::Edit;
            }
            _ if self.focus < 6 => {
                let limit = [8, 8, 512, 4096, 4096, 1024][self.focus];
                let before = self.fields[self.focus].value.clone();
                self.fields[self.focus].key(key, limit);
                if (3..5).contains(&self.focus) && before != self.fields[self.focus].value {
                    self.selected_ids[self.focus - 3] = None;
                }
                return Action::Edit;
            }
            _ => (),
        }
        self.list.select(Some(self.focus));
        Action::None
    }
    pub fn controls(&self) -> &'static str {
        if self.picker.is_some() || self.area_picker.is_some() {
            "上下 選択 / Space 切替 / Enter 条件へ反映（検索しません） / Esc 取消"
        } else {
            "Tab・上下 項目 / F2 地域・分類を名称選択 / Space 未分類 / Ctrl+U 消去 / Enter 検索 / Esc 取消"
        }
    }
    fn ids(&self, axis: usize) -> Vec<String> {
        if let Some(ids) = &self.selected_ids[axis] {
            return ids.clone();
        }
        let original = if axis == 0 {
            self.original.filter().program_genres()
        } else {
            self.original.filter().personality_genres()
        };
        let text = self.fields[axis + 3].value.trim();
        if text == original.join(",") {
            original.to_vec()
        } else if text.is_empty() {
            vec![]
        } else {
            text.split(',').map(|s| s.trim().to_owned()).collect()
        }
    }
    pub fn criteria(&self) -> Result<SearchCriteria, &'static str> {
        fn day(value: &str) -> Result<NaiveDate, &'static str> {
            if value.len() != 8 || !value.bytes().all(|b| b.is_ascii_digit()) {
                return Err("日付はYYYYMMDDの8桁で入力してください");
            }
            NaiveDate::parse_from_str(value, "%Y%m%d").map_err(|_| "日付が不正です")
        }
        let start = day(self.fields[0].value.trim())?;
        let end = if self.fields[1].value.trim().is_empty() {
            start
        } else {
            day(self.fields[1].value.trim())?
        };
        let scope = self.fields[2].value.trim();
        let scope = if scope == "全国" || scope.eq_ignore_ascii_case("all") {
            Scope::Nationwide
        } else if let Some(id) = scope
            .strip_prefix("局:")
            .or_else(|| scope.strip_prefix("station:"))
        {
            Scope::Station(id.trim().into())
        } else {
            let areas: Vec<_> = scope.split(',').map(|s| s.trim().to_owned()).collect();
            if areas
                .iter()
                .any(|s| !(1..=47).any(|n| s == &format!("JP{n}")))
            {
                return Err("地域は全国、JP13,JP14、または局:TBSの形式で入力してください");
            }
            Scope::Areas(areas)
        };
        let keyword = self.fields[5].value.trim();
        SearchCriteria::new(
            scope,
            BroadcastRange::new(start, end)?,
            Filter::new(
                self.ids(0),
                self.ids(1),
                self.unclassified[0],
                self.unclassified[1],
                (!keyword.is_empty()).then(|| keyword.into()),
            )?,
        )
    }
    pub fn draw(&mut self, frame: &mut Frame, area: Rect) {
        if let Some(picker) = &mut self.area_picker {
            picker.draw(frame, area);
            return;
        }
        if let Some(picker) = &mut self.picker {
            picker.draw(frame, area);
            return;
        }
        let panes = Layout::vertical([Constraint::Min(3), Constraint::Length(5)]).split(area);
        let labels = [
            "開始日 YYYYMMDD",
            "終了日（空欄は単日）",
            "地域（全国 / JP13,JP14 / 局:TBS）",
            "番組ジャンルID（カンマ区切り）",
            "出演者分類ID（カンマ区切り）",
            "キーワード",
        ];
        let mut items: Vec<_> = labels
            .iter()
            .enumerate()
            .map(|(i, label)| {
                ListItem::new(format!(
                    "{label}: {}",
                    self.fields[i].display(i == self.focus)
                ))
            })
            .collect();
        items.extend([
            ListItem::new(format!(
                "[{}] 番組ジャンルの未分類を選択（IDとOR）",
                if self.unclassified[0] { "x" } else { " " }
            )),
            ListItem::new(format!(
                "[{}] 出演者分類の未分類を選択（IDとOR）",
                if self.unclassified[1] { "x" } else { " " }
            )),
            ListItem::new("検索（Enterで確定）"),
        ]);
        frame.render_stateful_widget(
            List::new(items)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title("検索条件編集中・確定まで通信しません"),
                )
                .highlight_style(Style::default().bg(Color::DarkGray))
                .highlight_symbol("▶ "),
            panes[0],
            &mut self.list,
        );
        let options = self
            .observed
            .iter()
            .skip(self.observed_offset)
            .take(3)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        frame.render_widget(
            Paragraph::new(options).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("観測した分類ID・名称"),
            ),
            panes[1],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};
    fn criteria() -> SearchCriteria {
        SearchCriteria::new(
            Scope::Station("TBS".into()),
            BroadcastRange::single(NaiveDate::from_ymd_opt(2026, 10, 15).unwrap()),
            Filter::default(),
        )
        .unwrap()
    }
    fn set(editor: &mut Editor, index: usize, value: &str) {
        editor.fields[index] = Text::new(value.into());
    }
    fn programs() -> Vec<Program> {
        radiko_client::radiko::catalog::parse_programs(
            include_bytes!("../../tests/fixtures/schedule.xml"),
            "TBS",
            NaiveDate::from_ymd_opt(2026, 10, 15).unwrap(),
        )
        .unwrap()
        .programs
    }
    #[test]
    fn region_picker_cancels_station_replacement_and_applies_only_to_the_form() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-15T12:00:00+09:00").unwrap();
        let criteria = SearchCriteria::new(
            Scope::Station("TBS".into()),
            BroadcastRange::single(now.date_naive()),
            Filter::default(),
        )
        .unwrap();
        let mut editor = Editor::new(&criteria, &[]);
        editor.focus = 2;
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        for code in [
            KeyCode::F(2),
            KeyCode::End,
            KeyCode::Char(' '),
            KeyCode::Esc,
        ] {
            editor.handle(key(code));
        }
        assert_eq!(editor.criteria().unwrap(), criteria);
        for code in [KeyCode::F(2), KeyCode::End, KeyCode::Char(' ')] {
            editor.handle(key(code));
        }
        assert!(matches!(editor.handle(key(KeyCode::Enter)), Action::Edit));
        assert_eq!(
            editor.criteria().unwrap().scope(),
            &Scope::Areas(vec!["JP47".into()])
        );
        assert!(matches!(editor.handle(key(KeyCode::Enter)), Action::Submit));
    }
    #[test]
    fn classification_picker_applies_to_draft_only_and_cancel_preserves_the_field() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-15T12:00:00+09:00").unwrap();
        let criteria = SearchCriteria::initial(now).unwrap();
        let mut programs = radiko_client::radiko::catalog::parse_programs(
            include_bytes!("../../tests/fixtures/schedule.xml"),
            "TBS",
            radiko_client::radiko::catalog::parse_day("20261015").unwrap(),
        )
        .unwrap()
        .programs;
        programs[0].program_genres = vec![Classification {
            id: "P777".into(),
            name: "observed".into(),
        }];
        let mut editor = Editor::new(&criteria, &programs[..1]);
        editor.focus = 3;
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        editor.handle(key(KeyCode::F(2)));
        editor.handle(key(KeyCode::End));
        editor.handle(key(KeyCode::Char(' ')));
        assert_eq!(editor.criteria().unwrap(), criteria);
        assert!(matches!(editor.handle(key(KeyCode::Esc)), Action::None));
        assert_eq!(editor.criteria().unwrap(), criteria);
        editor.handle(key(KeyCode::F(2)));
        editor.handle(key(KeyCode::End));
        editor.handle(key(KeyCode::Char(' ')));
        assert!(matches!(editor.handle(key(KeyCode::Enter)), Action::Edit));
        assert_eq!(
            editor.criteria().unwrap().filter().program_genres(),
            &["P777"]
        );
        assert!(matches!(editor.handle(key(KeyCode::Enter)), Action::Submit));
    }
    #[test]
    fn picker_preserves_saved_ids_with_commas_and_cursor_motion_does_not_reparse_them() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-15T12:00:00+09:00").unwrap();
        let criteria = SearchCriteria::new(
            Scope::Nationwide,
            BroadcastRange::single(now.date_naive()),
            Filter::new(vec!["saved,unusual".into()], vec![], false, false, None).unwrap(),
        )
        .unwrap();
        let mut editor = Editor::new(&criteria, &[]);
        editor.focus = 3;
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        editor.handle(key(KeyCode::F(2)));
        editor.handle(key(KeyCode::Down));
        editor.handle(key(KeyCode::Char(' ')));
        editor.handle(key(KeyCode::Enter));
        editor.handle(key(KeyCode::Left));
        assert_eq!(
            editor.criteria().unwrap().filter().program_genres(),
            &["saved,unusual"]
        );
        assert!(editor.criteria().unwrap().filter().unclassified_program());
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(8, 4)).unwrap();
        editor.handle(key(KeyCode::F(2)));
        terminal
            .draw(|frame| editor.draw(frame, frame.area()))
            .unwrap();
    }
    #[test]
    fn roundtrip_preserves_single_day_axes_and_unusual_existing_ids_without_changing_unedited_values()
     {
        let initial = SearchCriteria::new(
            Scope::Areas(vec!["JP14".into(), "JP13".into()]),
            criteria().range().clone(),
            Filter::new(
                vec!["UNKNOWN,ALIAS".into()],
                vec!["C999".into()],
                true,
                false,
                Some("日本語".into()),
            )
            .unwrap(),
        )
        .unwrap();
        let editor = Editor::new(&initial, &[]);
        assert!(editor.fields[1].value.is_empty());
        assert_eq!(editor.criteria().unwrap(), initial);
    }
    #[test]
    fn invalid_dates_ranges_regions_and_classification_ids_are_rejected_before_submission() {
        for (index, text) in [
            (0, "20260230"),
            (0, "2026101"),
            (1, "20261014"),
            (1, "20261114"),
            (2, "JP0"),
            (2, "JP48"),
            (2, "JP13,"),
            (2, "局:../TBS"),
            (3, "A,,B"),
        ] {
            let mut editor = Editor::new(&criteria(), &[]);
            set(&mut editor, index, text);
            assert!(editor.criteria().is_err(), "{index} {text}");
        }
        let mut editor = Editor::new(&criteria(), &[]);
        set(&mut editor, 4, &"x".repeat(129));
        assert!(editor.criteria().is_err());
    }
    #[test]
    fn all_axes_build_normalized_criteria_with_separate_unclassified_switches() {
        let mut editor = Editor::new(&criteria(), &[]);
        for (index, text) in [
            (1, "20261016"),
            (2, "JP14, JP13,JP14"),
            (3, "UNKNOWN,OTHER"),
            (4, "C999"),
            (5, "朝"),
        ] {
            set(&mut editor, index, text);
        }
        editor.focus = 6;
        editor.handle(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        let parsed = editor.criteria().unwrap();
        assert_eq!(
            parsed.scope(),
            &Scope::Areas(vec!["JP13".into(), "JP14".into()])
        );
        assert_eq!(parsed.range().days().len(), 2);
        assert!(parsed.filter().unclassified_program());
        assert!(!parsed.filter().unclassified_personality());
        assert_eq!(
            programs()
                .iter()
                .filter(|p| parsed.filter().matches(p))
                .count(),
            1
        );
        set(&mut editor, 2, "all");
        assert_eq!(editor.criteria().unwrap().scope(), &Scope::Nationwide);
        set(&mut editor, 2, "station:OBS");
        assert_eq!(
            editor.criteria().unwrap().scope(),
            &Scope::Station("OBS".into())
        );
    }
    #[test]
    fn unicode_cursor_editing_and_input_limits_preserve_character_boundaries() {
        let mut text = Text::new("aあb".into());
        for code in [
            KeyCode::Left,
            KeyCode::Backspace,
            KeyCode::Char('い'),
            KeyCode::Home,
            KeyCode::Right,
            KeyCode::Delete,
        ] {
            text.key(KeyEvent::new(code, KeyModifiers::NONE), 1024);
        }
        assert_eq!(text.value, "ab");
        assert_eq!(text.cursor, 1);
        text.key(
            KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
            1024,
        );
        assert!(text.value.is_empty());
        assert_eq!(text.cursor, 0);
        text.key(KeyEvent::new(KeyCode::Char('あ'), KeyModifiers::NONE), 2);
        assert!(text.value.is_empty());
        text.key(KeyEvent::new(KeyCode::Char('é'), KeyModifiers::NONE), 2);
        text.key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE), 2);
        assert_eq!(text.value, "é");
    }
    #[test]
    fn observed_classifications_come_from_unfiltered_results_and_form_handles_small_terminals() {
        let mut editor = Editor::new(&criteria(), &programs());
        set(&mut editor, 5, "一致しないキーワード");
        let mut terminal = Terminal::new(TestBackend::new(140, 25)).unwrap();
        terminal
            .draw(|frame| editor.draw(frame, frame.area()))
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .filter(|cell| cell.symbol() != " ")
            .map(|cell| cell.symbol())
            .collect();
        for word in ["検索条件編集中", "全分類ではありません", "UNKNOWN", "C999"] {
            assert!(text.contains(word), "{word}");
        }
        editor.handle(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
        assert!(editor.observed_offset > 0);
        let mut small = Terminal::new(TestBackend::new(8, 4)).unwrap();
        small
            .draw(|frame| editor.draw(frame, frame.area()))
            .unwrap();
    }
}
