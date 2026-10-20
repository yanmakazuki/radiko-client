//! 検索とライブ／タイムフリー再生のTUI。描画処理は状態を参照するだけで、通信や再生を実行しない。
mod areas;
mod failures;
mod filters;
mod help;
mod login;
mod named;
mod picker;
mod scroll;
mod seek;
mod viewport;
use chrono::{DateTime, FixedOffset, Utc};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use radiko_client::{
    app::{
        AppState, SearchStatus,
        auth::{
            Credentials, LoginService, LogoutService, Service as AuthService, Status as AuthStatus,
        },
        chapters::{
            RequestId as ChapterRequestId, Service as ChapterService, Status as ChapterStatus,
        },
        named::{
            Event as NamedEvent, Operation as NamedOperation, Outcome as NamedOutcome,
            Service as NamedService, ViewId,
        },
        playback::{self, Service as PlaybackService},
        search::Service,
    },
    domain::{
        BroadcastStatus, Program, broadcast_status,
        search::{Scope, SearchCriteria},
    },
    player::{seek::Command, state::PlaybackState},
    radiko::catalog::CatalogClient,
    storage::searches::SearchStore,
};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Style},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};
use std::{
    io::{self, IsTerminal},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
const USAGE: &str = "tui [programsと同じ局・地域・期間・絞り込み]\n/ キーワード編集 / f 全条件編集 / b 名前付き条件 / Enter 検索確定 / Esc 編集取消 / j k・上下 選択 / PageUp・PageDown ページ / Home・End 先頭・末尾 / e 取得状況 / r 再取得 / q・Ctrl+C 終了\nEnter TF再生 / v ライブ画面 / l ライブ / g 秒数指定 / Space 一時停止・再開 / s 停止 / 左右 15秒シーク / + - 音量\n引数省略は前回条件を当日1日に復元。再生は保存済みログインを使用。起動時にセッションを確認し、aで再確認。I ログイン / O ログアウト（yで確定、n・Escで取消）。c チャプター閲覧 / 一覧のr 再取得・Esc 詳細へ。ライブ画面のEnterはライブ再生、vで戻ります。チャプターのEnterは原点を照合できた枠のみ指定位置から再生します。";
fn named_receipt(result: &Result<NamedOutcome, radiko_client::app::named::Error>) -> String {
    match result {
        Ok(NamedOutcome::Saved(name)) => format!("「{name}」を保存しました"),
        Ok(NamedOutcome::Deleted(name)) => format!("「{name}」を削除しました"),
        Err(error) => format!("保存条件の操作失敗: {error}"),
        _ => String::new(),
    }
}
struct RestoreTerminal;
impl Drop for RestoreTerminal {
    fn drop(&mut self) {
        ratatui::restore();
    }
}
struct Runtime(Option<tokio::runtime::Runtime>);
impl Drop for Runtime {
    fn drop(&mut self) {
        if let Some(runtime) = self.0.take() {
            runtime.shutdown_timeout(Duration::from_millis(200));
        }
    }
}
enum ChapterAction {
    Fetch(ChapterRequestId, Box<Program>),
    Cancel,
}
enum PlaybackAction {
    Select(Program),
    Chapter(Program, Duration),
    Live(Program),
    Command(Command),
    Stop,
}
struct Screen {
    help_open: bool,
    help_scroll: scroll::Scroll,
    failures_open: bool,
    failure_list: ListState,
    failure_height: usize,
    login: Option<login::Editor>,
    login_requested: Option<Credentials>,
    logout_confirm: bool,
    logout_requested: bool,
    live_mode: bool,
    live_rows: Vec<usize>,
    live_refresh_at: Option<DateTime<FixedOffset>>,
    last_live_time: Option<DateTime<FixedOffset>>,
    timefree_criteria: Option<SearchCriteria>,
    seek: Option<seek::Editor>,
    named: Option<named::Manager>,
    named_action: Option<NamedOperation>,
    named_generation: u64,
    filters: Option<filters::Editor>,
    auth_refresh: bool,
    chapter_action: Option<ChapterAction>,
    chapter_list: ListState,
    chapter_height: usize,
    chapter_summary_focused: bool,
    chapter_summary_scroll: scroll::Scroll,
    chapter_notice: String,
    playback_action: Option<PlaybackAction>,
    playback_notice: String,
    playback_diagnostics: String,
    volume: u32,
    app: AppState,
    list: ListState,
    list_height: usize,
    input: Option<String>,
    preferences_notice: String,
}
impl Screen {
    fn rows(&self) -> &[usize] {
        if self.live_mode {
            &self.live_rows
        } else {
            &self.app.visible
        }
    }
    fn rebuild_live(&mut self, now: DateTime<FixedOffset>) {
        self.last_live_time = Some(now);
        self.live_rows = self
            .app
            .visible
            .iter()
            .copied()
            .filter(|i| {
                let p = &self.app.programs[*i];
                p.key.starts_at <= now && now < p.ends_at
            })
            .collect();
        self.live_refresh_at = self
            .app
            .visible
            .iter()
            .flat_map(|i| {
                let p = &self.app.programs[*i];
                [p.key.starts_at, p.ends_at]
            })
            .filter(|time| *time > now)
            .min();
    }
    fn restore_selection(&mut self, key: Option<radiko_client::domain::ProgramKey>) {
        let position = key.and_then(|key| {
            self.rows()
                .iter()
                .position(|i| self.app.programs[*i].key == key)
        });
        let fallback = self
            .list
            .selected()
            .unwrap_or_default()
            .min(self.rows().len().saturating_sub(1));
        let selected = if self.rows().is_empty() {
            None
        } else {
            Some(position.unwrap_or(fallback))
        };
        self.list.select(selected);
    }
    fn refresh_live(&mut self, now: DateTime<FixedOffset>) {
        if !self.live_mode {
            return;
        }
        if self.live_refresh_at.is_some_and(|time| now >= time)
            || self.last_live_time.is_some_and(|time| now < time)
        {
            let key = self.selected().map(|p| p.key.clone());
            self.rebuild_live(now);
            self.restore_selection(key);
        }
        self.last_live_time = Some(now);
    }
    fn toggle_live(&mut self, service: &mut Service) -> Result<(), &'static str> {
        let criteria = if self.live_mode {
            self.timefree_criteria
                .take()
                .unwrap_or_else(|| self.app.committed.clone())
        } else {
            let criteria = self
                .app
                .committed
                .restore_startup(Utc::now().fixed_offset())?;
            self.timefree_criteria = Some(self.app.committed.clone());
            criteria
        };
        self.live_mode = !self.live_mode;
        self.app.begin_edit();
        self.app.edit_criteria(criteria)?;
        self.submit(service)
    }
    fn selected(&self) -> Option<&Program> {
        self.list
            .selected()
            .and_then(|i| self.rows().get(i))
            .and_then(|i| self.app.programs.get(*i))
    }
    fn submit(&mut self, service: &mut Service) -> Result<(), &'static str> {
        if self.app.chapters.target.is_some() {
            self.close_chapters();
        }
        if self.live_mode {
            let criteria = self
                .app
                .draft
                .as_ref()
                .unwrap_or(&self.app.committed)
                .restore_startup(Utc::now().fixed_offset())?;
            if self
                .app
                .draft
                .as_ref()
                .is_some_and(|draft| draft.range() != criteria.range())
            {
                // Explicit saved dates and form edits keep their meaning in the time-free view.
                self.live_mode = false;
                self.timefree_criteria = None;
            } else {
                self.app.begin_edit();
                self.app.edit_criteria(criteria)?;
            }
        }
        let id = self.app.commit()?;
        self.live_rows.clear();
        self.live_refresh_at = None;
        self.last_live_time = None;
        self.list.select(None);
        service.submit(id, self.app.committed.clone());
        Ok(())
    }
    fn apply(&mut self, event: radiko_client::app::search::Event) {
        let key = self.selected().map(|p| p.key.clone());
        self.app.apply(event);
        self.rebuild_live(Utc::now().fixed_offset());
        self.restore_selection(key);
    }
    fn open_chapters(&mut self, target: Program) -> Result<(), &'static str> {
        let id = self.app.chapters.open(target.clone())?;
        self.chapter_list.select(None);
        self.chapter_summary_focused = false;
        self.chapter_summary_scroll = scroll::Scroll::default();
        self.chapter_notice.clear();
        self.chapter_action = Some(ChapterAction::Fetch(id, Box::new(target)));
        Ok(())
    }
    fn close_chapters(&mut self) {
        self.app.chapters.close();
        self.chapter_list.select(None);
        self.chapter_summary_focused = false;
        self.chapter_summary_scroll = scroll::Scroll::default();
        self.chapter_notice.clear();
        self.chapter_action = Some(ChapterAction::Cancel);
    }
    fn named_event(&mut self, event: NamedEvent) -> Result<(), &'static str> {
        if event.mutating {
            self.preferences_notice = named_receipt(&event.result);
        }
        let Some(manager) = &mut self.named else {
            return Ok(());
        };
        if manager.view != event.view {
            if manager.busy {
                self.named_action = Some(NamedOperation::List);
            }
            return Ok(());
        }
        manager.busy = false;
        match event.result {
            Ok(NamedOutcome::Listed(items)) => manager.listed(items),
            Ok(NamedOutcome::Saved(_) | NamedOutcome::Deleted(_)) => {
                manager.busy = true;
                self.named_action = Some(NamedOperation::List);
            }
            Ok(NamedOutcome::Recalled(criteria)) => {
                self.app.begin_edit();
                self.app.edit_criteria((*criteria).clone())?;
                self.filters = Some(filters::Editor::new(&criteria, &self.app.programs));
                self.named = None;
            }
            Err(error) => manager.notice = error.to_string(),
        }
        Ok(())
    }
    fn dispatch_named(&mut self, service: &mut NamedService) {
        if let Some(operation) = self.named_action.take() {
            if let Some(manager) = &mut self.named {
                if let Err(error) = service.submit(manager.view, operation) {
                    manager.notice = error.into();
                }
            }
        }
    }
    fn key(&mut self, key: KeyEvent, service: &mut Service) -> Result<bool, &'static str> {
        if key.kind == KeyEventKind::Release
            || (key.kind == KeyEventKind::Repeat && key.code == KeyCode::Enter)
        {
            return Ok(false);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Ok(true);
        }
        if self.help_open {
            if matches!(key.code, KeyCode::Esc | KeyCode::Char('?' | 'q')) {
                self.help_open = false;
            } else {
                self.help_scroll.key(key.code);
            }
            return Ok(false);
        }
        if self.failures_open {
            match key.code {
                KeyCode::Esc | KeyCode::Char('e' | 'q') => self.failures_open = false,
                key => viewport::navigate(
                    &mut self.failure_list,
                    failures::count(&self.app),
                    self.failure_height,
                    key,
                ),
            }
            return Ok(false);
        }
        if self.logout_confirm {
            match key.code {
                KeyCode::Char('y') => {
                    self.logout_confirm = false;
                    self.logout_requested = true;
                }
                KeyCode::Char('n') | KeyCode::Esc => self.logout_confirm = false,
                KeyCode::Char('q') => return Ok(true),
                _ => (),
            }
            return Ok(false);
        }
        if let Some(editor) = &mut self.login {
            match editor.handle(key) {
                login::Action::Close => self.login = None,
                login::Action::Submit(credentials) => {
                    self.login_requested = Some(credentials);
                    self.login = None;
                }
                login::Action::None => (),
            }
            return Ok(false);
        }
        if let Some(editor) = &mut self.seek {
            match editor.handle(key) {
                seek::Action::Close => self.seek = None,
                seek::Action::Submit(command) => {
                    self.playback_action = Some(PlaybackAction::Command(command));
                    self.seek = None;
                }
                seek::Action::None => (),
            }
            return Ok(false);
        }
        if let Some(manager) = &mut self.named {
            match manager.handle(key, &self.app.committed) {
                named::Action::Close => self.named = None,
                named::Action::Quit => return Ok(true),
                named::Action::Operation(operation) => self.named_action = Some(operation),
                named::Action::None => (),
            }
            return Ok(false);
        }
        if let Some(editor) = &mut self.filters {
            match editor.handle(key) {
                filters::Action::Cancel => {
                    self.filters = None;
                    self.app.cancel_edit();
                }
                filters::Action::Submit => match editor.criteria() {
                    Ok(criteria) => {
                        self.app.edit_criteria(criteria)?;
                        self.submit(service)?;
                        self.filters = None;
                    }
                    Err(reason) => editor.error = reason.into(),
                },
                filters::Action::Edit => {
                    editor.error.clear();
                    if let Ok(criteria) = editor.criteria() {
                        self.app.edit_criteria(criteria)?;
                    }
                }
                filters::Action::None => (),
            }
            return Ok(false);
        }
        if let Some(input) = self.input.as_mut() {
            match key.code {
                KeyCode::Esc => {
                    self.input = None;
                    self.app.cancel_edit();
                }
                KeyCode::Enter => {
                    let keyword = (!input.trim().is_empty()).then(|| input.clone());
                    match self.app.edit_keyword(keyword) {
                        Ok(()) => {
                            self.input = None;
                            self.submit(service)?;
                        }
                        Err(error) => self.app.notification = error.into(),
                    }
                }
                KeyCode::Backspace => {
                    input.pop();
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    input.clear()
                }
                KeyCode::Char(c)
                    if !c.is_control()
                        && !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                        && input.len() + c.len_utf8() <= 1024 =>
                {
                    input.push(c)
                }
                _ => (),
            }
            if let Some(input) = &self.input {
                self.app
                    .edit_keyword((!input.trim().is_empty()).then(|| input.clone()))?;
            }
            return Ok(false);
        }
        if matches!(
            self.app.authentication.status,
            AuthStatus::LoggingOut | AuthStatus::LoggingIn
        ) && matches!(key.code, KeyCode::Char('I' | 'O' | 'a'))
        {
            self.playback_notice = "認証情報の更新処理中です".into();
            return Ok(false);
        }
        if matches!(self.app.authentication.status, AuthStatus::LoggingOut)
            && matches!(
                key.code,
                KeyCode::Enter
                    | KeyCode::Left
                    | KeyCode::Right
                    | KeyCode::Char('a' | 'O' | 'l' | 'g' | ' ')
            )
        {
            self.playback_notice = "ログアウト処理中です".into();
            return Ok(false);
        }
        if self.app.chapters.target.is_some() {
            match key.code {
                KeyCode::Tab | KeyCode::BackTab => {
                    self.chapter_summary_focused = !self.chapter_summary_focused;
                    return Ok(false);
                }
                KeyCode::Esc => {
                    self.close_chapters();
                    return Ok(false);
                }
                KeyCode::Char('r') | KeyCode::Char('c') => {
                    if let Some(target) = self.app.chapters.target.clone() {
                        self.open_chapters(target)?;
                    }
                    return Ok(false);
                }
                KeyCode::Down
                | KeyCode::Char('j')
                | KeyCode::Up
                | KeyCode::Char('k')
                | KeyCode::PageDown
                | KeyCode::PageUp
                | KeyCode::Home
                | KeyCode::End => {
                    let count = self
                        .app
                        .chapters
                        .candidates
                        .as_ref()
                        .map_or(0, |set| set.chapters.len());
                    if self.chapter_summary_focused {
                        self.chapter_summary_scroll.key(key.code);
                    } else {
                        let previous = self.chapter_list.selected();
                        viewport::navigate(
                            &mut self.chapter_list,
                            count,
                            self.chapter_height,
                            key.code,
                        );
                        if self.chapter_list.selected() != previous {
                            self.chapter_summary_scroll = scroll::Scroll::default();
                        }
                    }
                    return Ok(false);
                }
                KeyCode::Enter => {
                    let index = self.chapter_list.selected().unwrap_or_default();
                    match self.app.chapters.jump(index) {
                        Ok((program, position)) => {
                            match radiko_client::app::playback::request(
                                program,
                                Utc::now().fixed_offset(),
                            ) {
                                Err(reason) => self.chapter_notice = reason.into(),
                                Ok(_) => {
                                    let same = self.app.playback.as_ref().is_some_and(|p| {
                                        p.info.station == program.key.station_id
                                            && p.info.starts_at == program.key.starts_at
                                            && p.info.mode == radiko_client::radiko::stream::ProbeMode::TimeFree
                                    });
                                    self.playback_action = Some(if same {
                                        PlaybackAction::Command(Command::SeekSlot {
                                            key: program.key.clone(),
                                            position,
                                        })
                                    } else {
                                        PlaybackAction::Chapter(program.clone(), position)
                                    });
                                    self.chapter_notice =
                                        "チャプター再生を準備します（成功後に切替）".into();
                                }
                            }
                        }
                        Err(reason) => self.chapter_notice = reason.into(),
                    }
                    return Ok(false);
                }
                KeyCode::Char('/') | KeyCode::Char('f') | KeyCode::Char('b') => return Ok(false),
                _ => (),
            }
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(true),
            KeyCode::Char('?') => {
                self.help_open = true;
                self.help_scroll = scroll::Scroll::default();
            }
            KeyCode::Char('e') => {
                self.failures_open = true;
                self.failure_list = ListState::default();
                if failures::count(&self.app) > 0 {
                    self.failure_list.select(Some(0));
                }
            }
            KeyCode::Char('a') => self.auth_refresh = true,
            KeyCode::Char('O') => self.logout_confirm = true,
            KeyCode::Char('I') => self.login = Some(login::Editor::default()),
            KeyCode::Char('b') => {
                self.named_generation = self
                    .named_generation
                    .checked_add(1)
                    .ok_or("保存条件の表示番号の上限です")?;
                self.named = Some(named::Manager::new(ViewId(self.named_generation)));
                self.named_action = Some(NamedOperation::List);
            }
            KeyCode::Char('f') => {
                self.app.begin_edit();
                self.filters = Some(filters::Editor::new(
                    &self.app.committed,
                    &self.app.programs,
                ));
            }
            KeyCode::Char('c') => {
                if let Some(target) = self.selected().cloned() {
                    self.open_chapters(target)?;
                }
            }
            KeyCode::Char('/') => {
                self.app.begin_edit();
                self.input = Some(
                    self.app
                        .committed
                        .filter()
                        .keyword()
                        .unwrap_or_default()
                        .into(),
                );
            }
            KeyCode::Char('v') if self.app.chapters.target.is_none() => {
                self.toggle_live(service)?
            }
            KeyCode::Char('r') => self.submit(service)?,
            KeyCode::Down
            | KeyCode::Char('j')
            | KeyCode::Up
            | KeyCode::Char('k')
            | KeyCode::PageDown
            | KeyCode::PageUp
            | KeyCode::Home
            | KeyCode::End => {
                let count = self.rows().len();
                viewport::navigate(&mut self.list, count, self.list_height, key.code);
            }
            KeyCode::Enter => {
                if let Some(program) = self.selected() {
                    let request = if self.live_mode {
                        playback::live_request
                    } else {
                        playback::request
                    };
                    match request(program, Utc::now().fixed_offset()) {
                        Ok(_) => {
                            self.playback_action = Some(if self.live_mode {
                                PlaybackAction::Live(program.clone())
                            } else {
                                PlaybackAction::Select(program.clone())
                            })
                        }
                        Err(reason) => self.playback_notice = reason.into(),
                    }
                }
            }
            KeyCode::Char('l') if self.app.chapters.target.is_none() => {
                if let Some(program) = self.selected() {
                    match playback::live_request(program, Utc::now().fixed_offset()) {
                        Ok(_) => self.playback_action = Some(PlaybackAction::Live(program.clone())),
                        Err(reason) => self.playback_notice = reason.into(),
                    }
                }
            }
            KeyCode::Char('g') => {
                if let Some(snapshot) = &self.app.playback {
                    match seek::Editor::new(snapshot) {
                        Ok(editor) => self.seek = Some(editor),
                        Err(reason) => self.playback_notice = reason.into(),
                    }
                } else {
                    self.playback_notice = "再生中の番組はありません".into();
                }
            }
            KeyCode::Char('s') => self.playback_action = Some(PlaybackAction::Stop),
            KeyCode::Char(' ') => {
                if let Some(snapshot) = &self.app.playback {
                    self.playback_action = Some(PlaybackAction::Command(
                        if snapshot.state == PlaybackState::Paused {
                            Command::Resume
                        } else {
                            Command::Pause
                        },
                    ));
                }
            }
            KeyCode::Left | KeyCode::Right if self.app.playback.is_some() => {
                self.playback_action = Some(PlaybackAction::Command(Command::Relative(
                    if key.code == KeyCode::Left { -15 } else { 15 },
                )));
            }
            KeyCode::Char('+') | KeyCode::Char('-') => {
                let volume = self.volume;
                self.volume = if key.code == KeyCode::Char('+') {
                    (volume + 5).min(100)
                } else {
                    volume.saturating_sub(5)
                };
                self.playback_action = Some(PlaybackAction::Command(Command::Volume(self.volume)));
            }
            _ => (),
        }
        Ok(false)
    }
}
fn local(time: DateTime<FixedOffset>) -> DateTime<FixedOffset> {
    FixedOffset::east_opt(9 * 3600)
        .map(|zone| time.with_timezone(&zone))
        .unwrap_or(time)
}
fn status(p: &Program, now: DateTime<FixedOffset>) -> &'static str {
    match broadcast_status(p.key.starts_at, p.ends_at, now) {
        Ok(BroadcastStatus::Upcoming) => "放送前",
        Ok(BroadcastStatus::OnAir) => "放送中",
        Ok(BroadcastStatus::Finished) => "放送終了",
        Err(_) => "日時不正",
    }
}
fn classes(ids: &[String], empty: bool) -> String {
    let mut names = ids.to_vec();
    if empty {
        names.push("未分類".into());
    }
    if names.is_empty() {
        "指定なし".into()
    } else {
        names.join(",")
    }
}
// Display a bounded preview of common schedule HTML; this is not a browser renderer.
fn preview(text: &str) -> String {
    let mut chars = text.chars().take(12_000).peekable();
    let mut output = String::new();
    while let Some(c) = chars.next() {
        if c == '<'
            && chars
                .peek()
                .is_some_and(|next| next.is_ascii_alphabetic() || *next == '/')
        {
            let mut tag = String::new();
            let mut quoted = None;
            for value in chars.by_ref() {
                if quoted == Some(value) {
                    quoted = None;
                } else if quoted.is_none() && matches!(value, '\'' | '"') {
                    quoted = Some(value);
                } else if quoted.is_none() && value == '>' {
                    break;
                }
                tag.push(value);
            }
            let name = tag
                .trim_start_matches('/')
                .split(|c: char| c.is_whitespace() || c == '/')
                .next()
                .unwrap_or_default();
            if ["br", "p", "div", "li"]
                .iter()
                .any(|known| name.eq_ignore_ascii_case(known))
            {
                output.push('\n');
            }
        } else if !c.is_control() || matches!(c, '\n' | '\t') {
            output.push(c);
        }
    }
    for (encoded, decoded) in [
        ("&lt;", "<"),
        ("&gt;", ">"),
        ("&quot;", "\""),
        ("&#39;", "'"),
        ("&nbsp;", " "),
        ("&amp;", "&"),
    ] {
        output = output.replace(encoded, decoded);
    }
    output.chars().take(1200).collect()
}
fn draw_chapters(frame: &mut Frame, screen: &mut Screen, area: Rect, now: DateTime<FixedOffset>) {
    let state = &screen.app.chapters;
    let Some(target) = &state.target else {
        return;
    };
    let constraints = if screen.chapter_summary_focused {
        [
            Constraint::Length(7),
            Constraint::Length(3),
            Constraint::Min(1),
        ]
    } else {
        [
            Constraint::Length(7),
            Constraint::Min(1),
            Constraint::Length(3),
        ]
    };
    let panes = Layout::vertical(constraints).split(area);
    screen.chapter_height = usize::from(panes[1].height.saturating_sub(2));
    let counts = state
        .candidates
        .as_ref()
        .map(|set| {
            format!(
                "{}件 / 楽曲除外{}件 / 不正位置等{}件",
                set.chapters.len(),
                set.music_count,
                set.invalid_count
            )
        })
        .unwrap_or_default();
    let failure = if let ChapterStatus::Failed(error) = &state.status {
        error.as_str()
    } else {
        screen.chapter_notice.as_str()
    };
    frame.render_widget(Paragraph::new(format!("{} / {}\n{}〜{} JST\n{} / {}\nr 再取得 / Esc 番組詳細へ（選択移動では再生しません）\n{}",
        target.title, target.key.station_id, local(target.key.starts_at).format("%Y/%m/%d %H:%M"), local(target.ends_at).format("%Y/%m/%d %H:%M"),
        state.label(), counts, failure)).block(Block::default().borders(Borders::ALL).title("公式AIチャプター・閲覧中")), panes[0]);
    let current = screen.app.playback.as_ref().and_then(|p| {
        if p.info.mode != radiko_client::radiko::stream::ProbeMode::TimeFree {
            return None;
        }
        let (bound, _) = state.jump(0).ok()?;
        if radiko_client::app::playback::availability(
            bound,
            radiko_client::radiko::stream::ProbeMode::TimeFree,
            now,
            Some(p),
        ) != radiko_client::domain::ListeningAvailability::CurrentOutput
        {
            return None;
        }
        state
            .candidates
            .as_ref()?
            .current(p.position?)
            .map(|c| c.source_index)
    });
    let items: Vec<_> = state
        .candidates
        .as_ref()
        .map(|set| {
            set.chapters
                .iter()
                .map(|chapter| {
                    ListItem::new(format!(
                        "{}{:.1}〜{:.1}秒 {}",
                        if current == Some(chapter.source_index) {
                            "[再生位置] "
                        } else {
                            ""
                        },
                        chapter.start.as_secs_f64(),
                        chapter.end.as_secs_f64(),
                        chapter.title
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    frame.render_stateful_widget(
        List::new(items)
            .block(
                Block::default().borders(Borders::ALL).title(
                    if state
                        .jump(screen.chapter_list.selected().unwrap_or_default())
                        .is_ok()
                    {
                        "チャプター（Enterで再生・選択と再生位置は別表示）"
                    } else {
                        "参考位置（音声との時刻対応は未検証）"
                    },
                ),
            )
            .highlight_style(Style::default().bg(Color::DarkGray))
            .highlight_symbol("▶ "),
        panes[1],
        &mut screen.chapter_list,
    );
    let summary = screen
        .chapter_list
        .selected()
        .and_then(|index| state.candidates.as_ref()?.chapters.get(index))
        .map(|chapter| preview(&chapter.summary))
        .unwrap_or_default();
    let paragraph = Paragraph::new(summary).wrap(Wrap { trim: false });
    let scroll = &mut screen.chapter_summary_scroll;
    let lines = paragraph.line_count(panes[2].width.saturating_sub(2));
    scroll.fit(lines, panes[2].height.saturating_sub(2));
    let title = if screen.chapter_summary_focused {
        "要約（先頭1200文字まで）・上下/ページ スクロール・Tab 章一覧"
    } else {
        "選択したチャプターの要約（Tabで拡大・スクロール）"
    };
    frame.render_widget(
        paragraph.scroll((scroll.offset, 0)).block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_style(if screen.chapter_summary_focused {
                    Style::default().fg(Color::Yellow)
                } else {
                    Style::default()
                }),
        ),
        panes[2],
    );
}
fn draw(frame: &mut Frame, screen: &mut Screen, now: DateTime<FixedOffset>) {
    if screen.failures_open {
        failures::draw(
            frame,
            &screen.app,
            &mut screen.failure_list,
            &mut screen.failure_height,
        );
        return;
    }
    if screen.help_open {
        help::draw(
            frame,
            screen.app.playback.as_ref(),
            screen.volume,
            &mut screen.help_scroll,
        );
        return;
    }
    let areas = Layout::vertical([
        Constraint::Length(7),
        Constraint::Min(1),
        Constraint::Length(if screen.input.is_some() {
            4
        } else if screen.seek.is_some() {
            10
        } else {
            9
        }),
    ])
    .split(frame.area());
    let scope = match screen.app.committed.scope() {
        Scope::Nationwide => "全国".into(),
        Scope::Station(id) => id.clone(),
        Scope::Areas(ids) => ids.join(","),
    };
    let filter = screen.app.committed.filter();
    let header = format!(
        "{} / {} / {}〜{}（日本時間・05:00区切り）\nキーワード: {}\n番組ジャンル: {} / 出演者分類: {}\n認証: {}\n{}",
        if screen.live_mode
            && radiko_client::domain::broadcast_day(now)
                .is_some_and(|day| day != screen.app.committed.range().start())
        {
            "ライブ（取得日が当日と異なります・rで更新）"
        } else if screen.live_mode {
            "ライブ（放送中のみ）"
        } else {
            "タイムフリー"
        },
        scope,
        screen.app.committed.range().start(),
        screen.app.committed.range().end(),
        filter.keyword().unwrap_or("指定なし"),
        classes(filter.program_genres(), filter.unclassified_program()),
        classes(
            filter.personality_genres(),
            filter.unclassified_personality()
        ),
        screen.app.authentication.status.summary(),
        screen.app.authentication.status.detail()
    );
    frame.render_widget(
        Paragraph::new(header).block(
            Block::default()
                .title("radiko-client")
                .borders(Borders::ALL),
        ),
        areas[0],
    );
    if let Some(editor) = &screen.login {
        editor.draw(frame, areas[1]);
    } else if let Some(manager) = &mut screen.named {
        manager.draw(frame, areas[1], &screen.app.committed);
    } else if let Some(editor) = &mut screen.filters {
        editor.draw(frame, areas[1]);
    } else if screen.app.chapters.target.is_some() {
        draw_chapters(frame, screen, areas[1], now);
    } else {
        let panes = Layout::horizontal([Constraint::Percentage(48), Constraint::Percentage(52)])
            .split(areas[1]);
        screen.list_height = usize::from(panes[0].height.saturating_sub(2));
        let count = screen.rows().len();
        let visible = viewport::visible(&mut screen.list, count, screen.list_height);
        let mut local_list = ListState::default();
        local_list.select(
            screen
                .list
                .selected()
                .filter(|i| visible.contains(i))
                .map(|i| i - visible.start),
        );
        let items: Vec<_> = screen.rows()[visible]
            .iter()
            .map(|i| {
                let p = &screen.app.programs[*i];
                ListItem::new(format!(
                    "{} {} [{}] {}",
                    local(p.key.starts_at).format("%m/%d %H:%M"),
                    p.key.station_id,
                    status(p, now),
                    p.title.chars().take(120).collect::<String>()
                ))
            })
            .collect();
        frame.render_stateful_widget(
            List::new(items)
                .block(Block::default().borders(Borders::ALL).title(format!(
                    "番組一覧 一致{} / 取得{}枠",
                    screen.rows().len(),
                    screen.app.programs.len()
                )))
                .highlight_style(Style::default().bg(Color::DarkGray))
                .highlight_symbol("▶ "),
            panes[0],
            &mut local_list,
        );
        let mut detail = screen
            .selected()
            .map(|p| {
                format!(
                    "{}\n{} / {}〜{} JST\n{} / {}\n出演: {}\n\n{}\n{}",
                    p.title,
                    p.key.station_id,
                    local(p.key.starts_at).format("%m/%d %H:%M"),
                    local(p.ends_at).format("%m/%d %H:%M"),
                    status(p, now),
                    radiko_client::app::playback::availability(
                        p,
                        if screen.live_mode {
                            radiko_client::radiko::stream::ProbeMode::Live
                        } else {
                            radiko_client::radiko::stream::ProbeMode::TimeFree
                        },
                        now,
                        screen.app.playback.as_ref(),
                    )
                    .label(),
                    p.performer,
                    preview(&p.description),
                    preview(&p.info)
                )
            })
            .unwrap_or_else(|| "選択できる番組はありません".into());
        for failure in screen
            .app
            .area_failures
            .iter()
            .chain(&screen.app.schedule_failures)
            .take(5)
        {
            detail.push_str(&format!(
                "\n未取得: {} {} {}",
                failure.target,
                failure.day.map(|d| d.to_string()).unwrap_or_default(),
                failure.error
            ));
        }
        frame.render_widget(
            Paragraph::new(detail).wrap(Wrap { trim: false }).block(
                Block::default()
                    .title("番組詳細（閲覧中）")
                    .borders(Borders::ALL),
            ),
            panes[1],
        );
    }
    if let Some(input) = &screen.input {
        frame.render_widget(
            Paragraph::new(format!(
                "キーワード: {input}\nEnter 検索 / Ctrl+U 消去 / Esc 取消（編集中は取得しません）"
            ))
            .block(Block::default().borders(Borders::ALL).title("条件編集中")),
            areas[2],
        );
    } else {
        let state = match screen.app.status {
            SearchStatus::Idle => "未取得",
            SearchStatus::Loading => "取得中",
            SearchStatus::Ready => "取得完了",
            SearchStatus::Partial => "部分取得・未取得あり",
            SearchStatus::Failed => "取得失敗",
        };
        let progress = screen
            .app
            .progress
            .as_ref()
            .map(|p| {
                format!(
                    "{} {}/{} 失敗{} / {} {}",
                    p.phase,
                    p.completed,
                    p.total,
                    p.failed,
                    p.target,
                    p.day.map(|d| d.to_string()).unwrap_or_default()
                )
            })
            .unwrap_or_default();
        let playing = screen
            .app
            .playback
            .as_ref()
            .map(|p| {
                format!(
                    "再生中: {} / {} / {:?}\n{}〜{} JST / {:?} / 音量{}% / {}",
                    p.info.title.as_deref().unwrap_or("番組名未取得"),
                    p.info.station,
                    p.info.mode,
                    local(p.info.starts_at).format("%m/%d %H:%M"),
                    local(p.info.ends_at).format("%m/%d %H:%M"),
                    p.state,
                    p.volume,
                    p.position
                        .map(|v| format!("位置{:.1}秒", v.as_secs_f64()))
                        .unwrap_or_else(|| "番組内位置未確認".into())
                )
            })
            .unwrap_or_else(|| format!("再生中: なし / 音量{}%", screen.volume));
        let controls = if screen.login.is_some() {
            "Tab 入力欄 / Enter 次の欄・ログイン確定 / Ctrl+U 消去 / Esc 取消 / Ctrl+C 終了".into()
        } else if screen.logout_confirm {
            "ログアウトしますか？ 再生を停止し保存セッションを削除 / y 確定 / n・Esc 取消".into()
        } else if let Some(editor) = &screen.seek {
            editor.prompt()
        } else if let Some(manager) = &screen.named {
            manager.controls().into()
        } else if let Some(editor) = &screen.filters {
            editor.controls().into()
        } else {
            let enter = if screen.app.chapters.target.is_some() {
                if screen
                    .app
                    .chapters
                    .jump(screen.chapter_list.selected().unwrap_or_default())
                    .is_ok()
                {
                    "Enter チャプターから再生 / r 再取得 / Esc 詳細へ"
                } else {
                    "Enter ジャンプ不可（位置未検証）"
                }
            } else {
                if screen.live_mode {
                    "Enter ライブ再生 / v TF画面 / f 条件 / b 保存条件 / c チャプター"
                } else {
                    "Enter TF再生 / v ライブ画面 / l ライブ / f 条件 / b 保存条件 / c チャプター"
                }
            };
            format!(
                "? 操作一覧 / e 取得状況 / {enter} / Space 一時停止・再開 / s 停止 / ←→ 15秒 / g 位置 / +- 音量 / q 終了"
            )
        };
        let footer = format!(
            "{} / {} / 不正枠{}\n{}\n{}\n{}\n{}\n{} {}",
            state,
            progress,
            screen.app.invalid_count,
            playing,
            screen.playback_diagnostics,
            controls,
            screen.playback_notice,
            screen
                .filters
                .as_ref()
                .map(|editor| editor.error.as_str())
                .unwrap_or(&screen.app.notification),
            screen.preferences_notice
        );
        frame.render_widget(
            Paragraph::new(footer).block(Block::default().borders(Borders::ALL)),
            areas[2],
        );
    }
}
pub fn run(args: &[String], data_dir: Option<PathBuf>) -> Result<(), Box<dyn std::error::Error>> {
    if args == ["--help"] {
        println!("{USAGE}");
        return Ok(());
    }
    if args
        .iter()
        .any(|arg| matches!(arg.as_str(), "--json" | "--classifications"))
    {
        return Err("TUIでは表示用CLIオプションは使用できません".into());
    }
    let now = Utc::now().fixed_offset();
    let explicit = if args.is_empty() {
        None
    } else {
        Some(crate::programs::criteria(args, now)?)
    };
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err("tuiは対話端末で実行してください".into());
    }
    let mut preferences_notice = String::new();
    let playback_dir = data_dir.clone();
    let auth_dir = data_dir.clone();
    let named_dir = data_dir.clone();
    let store = match data_dir {
        Some(path) => Some(SearchStore::new(path)),
        None => match SearchStore::default_location() {
            Ok(store) => Some(store),
            Err(error) => {
                preferences_notice = format!("条件保存先の取得失敗: {error}");
                None
            }
        },
    };
    let criteria = if let Some(criteria) = explicit {
        criteria
    } else if let Some(store) = &store {
        match store.load() {
            Ok(library) => match library.last {
                Some(last) => last.restore_startup(now)?,
                None => SearchCriteria::initial(now)?,
            },
            Err(error) => {
                preferences_notice = format!("条件復元失敗: {error}");
                SearchCriteria::initial(now)?
            }
        }
    } else {
        SearchCriteria::initial(now)?
    };
    let shutdown = Arc::new(AtomicBool::new(false));
    let signal = shutdown.clone();
    ctrlc::set_handler(move || signal.store(true, Ordering::Release))
        .map_err(|_| "終了シグナルの処理を登録できません")?;
    let runtime = Runtime(Some(
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()?,
    ));
    let mut login_pending = false;
    let mut login_exit_notice = None;
    let mut logout_pending = false;
    let mut logout_exit_notice = None;
    let mut named_write_pending = false;
    let mut named_exit_notice = None;
    let result = {
        let _restore = RestoreTerminal;
        let mut terminal = ratatui::try_init()?;
        let mut screen = Screen {
            help_open: false,
            help_scroll: scroll::Scroll::default(),
            failures_open: false,
            failure_list: ListState::default(),
            failure_height: 1,
            login: None,
            login_requested: None,
            logout_confirm: false,
            logout_requested: false,
            live_mode: false,
            live_rows: vec![],
            live_refresh_at: None,
            last_live_time: None,
            timefree_criteria: None,
            seek: None,
            named: None,
            named_action: None,
            named_generation: 0,
            filters: None,
            auth_refresh: false,
            chapter_action: None,
            chapter_list: ListState::default(),
            chapter_height: 1,
            chapter_summary_focused: false,
            chapter_summary_scroll: scroll::Scroll::default(),
            chapter_notice: String::new(),
            playback_action: None,
            playback_notice: String::new(),
            playback_diagnostics: String::new(),
            volume: 25,
            app: AppState::new(criteria),
            list: ListState::default(),
            list_height: 1,
            input: None,
            preferences_notice,
        };
        let rt = runtime.0.as_ref().ok_or("実行環境を開始できません")?;
        rt.block_on(async {
            let mut service = Service::new_cancellable(CatalogClient::new_cancellable);
            let mut player = PlaybackService::new(playback_dir);
            let mut chapters = ChapterService::default();
            let mut authentication = AuthService::new(auth_dir.clone());
            let mut login_service = LoginService::new(auth_dir.clone());
            let mut logout = LogoutService::new(auth_dir);
            let mut named_service = NamedService::new(named_dir);
            authentication.submit(screen.app.authentication.begin()?);
            screen.submit(&mut service)?;
            let mut tick = tokio::time::interval(Duration::from_millis(50));
            loop {
                if shutdown.load(Ordering::Acquire) {
                    named_write_pending = named_service.writing();
                    break;
                }
                if let Some(event) = named_service.poll().await {
                    screen.named_event(event)?;
                }
                screen.dispatch_named(&mut named_service);
                if let Some(event) = login_service.poll().await {
                    screen.app.authentication.apply(event);
                }
                if let Some(event) = logout.poll().await {
                    screen.app.authentication.apply(event);
                }
                if let Some(event) = authentication.poll().await {
                    screen.app.authentication.apply(event);
                }
                for event in service.poll().await {
                    screen.apply(event);
                }
                if let Some(event) = chapters.poll().await {
                    screen.app.chapters.apply(event);
                    if screen.chapter_list.selected().is_none()
                        && screen
                            .app
                            .chapters
                            .candidates
                            .as_ref()
                            .is_some_and(|set| !set.chapters.is_empty())
                    {
                        screen.chapter_list.select(Some(0));
                    }
                }
                let update = player.poll();
                if let Some(snapshot) = &update.snapshot {
                    screen.playback_diagnostics = radiko_client::player::state::output_diagnostics(
                        snapshot.output.as_ref(),
                        snapshot.segments,
                    );
                }
                if let Some(diagnostics) = update.final_diagnostics {
                    screen.playback_diagnostics = format!("停止時: {diagnostics}");
                } else if update.ended && !screen.playback_diagnostics.is_empty() {
                    screen.playback_diagnostics =
                        format!("直近通知（終了時未集計）: {}", screen.playback_diagnostics);
                }
                if update.ended {
                    screen.app.playback = None;
                } else if let Some(snapshot) = update.snapshot {
                    screen.app.playback = Some(snapshot);
                }
                if let Some(notice) = update.notice {
                    screen.playback_notice = notice;
                }
                screen.refresh_live(Utc::now().fixed_offset());
                terminal.draw(|frame| draw(frame, &mut screen, Utc::now().fixed_offset()))?;
                let mut quit = false;
                for _ in 0..32 {
                    if !event::poll(Duration::ZERO)? {
                        break;
                    }
                    if let Event::Key(key) = event::read()? {
                        let should_quit = screen.key(key, &mut service)?;
                        screen.dispatch_named(&mut named_service);
                        if let Some(credentials) = screen.login_requested.take() {
                            screen.auth_refresh = false;
                            authentication.cancel();
                            login_service
                                .submit(screen.app.authentication.begin_login()?, credentials)?;
                        }
                        if std::mem::take(&mut screen.logout_requested) {
                            player.stop();
                            screen.playback_action = None;
                            screen.auth_refresh = false;
                            authentication.cancel();
                            logout.submit(screen.app.authentication.begin_logout()?)?;
                        }
                        if std::mem::take(&mut screen.auth_refresh) {
                            authentication.submit(screen.app.authentication.begin()?);
                        }
                        if let Some(action) = screen.chapter_action.take() {
                            match action {
                                ChapterAction::Fetch(id, target) => chapters.submit(id, *target),
                                ChapterAction::Cancel => chapters.cancel(),
                            }
                        }
                        if let Some(action) = screen.playback_action.take() {
                            let result = match action {
                                PlaybackAction::Select(program) => {
                                    player.select(&program, Utc::now().fixed_offset())
                                }
                                PlaybackAction::Chapter(program, position) => {
                                    player.select_at(&program, position, Utc::now().fixed_offset())
                                }
                                PlaybackAction::Live(program) => {
                                    player.select_live(&program, Utc::now().fixed_offset())
                                }
                                PlaybackAction::Command(command) => player.command(command),
                                PlaybackAction::Stop => {
                                    player.stop();
                                    Ok(())
                                }
                            };
                            if let Err(reason) = result {
                                screen.playback_notice = reason.into();
                            }
                        }
                        if should_quit {
                            player.stop();
                            let deadline = std::time::Instant::now() + Duration::from_millis(200);
                            while named_service.writing() && std::time::Instant::now() < deadline {
                                if let Some(event) = named_service.poll().await {
                                    named_exit_notice = Some(named_receipt(&event.result));
                                }
                                if named_service.writing() {
                                    tokio::time::sleep(Duration::from_millis(5)).await;
                                }
                            }
                            named_write_pending = named_service.writing();
                            quit = true;
                            break;
                        }
                    }
                }
                if quit {
                    break;
                }
                tick.tick().await;
            }
            player.stop();
            let logout_deadline = std::time::Instant::now() + Duration::from_millis(200);
            while logout.busy() && std::time::Instant::now() < logout_deadline {
                if let Some(event) = logout.poll().await {
                    logout_exit_notice = Some(format!(
                        "{} / {}",
                        event.status.summary(),
                        event.status.detail()
                    ));
                    screen.app.authentication.apply(event);
                }
                if logout.busy() {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            }
            logout_pending = logout.busy();
            let login_deadline = std::time::Instant::now() + Duration::from_millis(200);
            while login_service.busy() && std::time::Instant::now() < login_deadline {
                if let Some(event) = login_service.poll().await {
                    login_exit_notice = Some(format!(
                        "{} / {}",
                        event.status.summary(),
                        event.status.detail()
                    ));
                    screen.app.authentication.apply(event);
                }
                if login_service.busy() {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            }
            login_pending = login_service.busy();
            Ok::<_, Box<dyn std::error::Error>>(screen.app.committed.clone())
        })
    };
    if let Some(notice) = login_exit_notice {
        eprintln!("ログイン: {notice}");
    }
    if login_pending {
        eprintln!("ログイン結果が未確認です。sessionで保存状態を確認してください");
    }
    if let Some(notice) = logout_exit_notice {
        eprintln!("ログアウト: {notice}");
    }
    if logout_pending {
        eprintln!("ログアウト結果が未確認です。保存セッションの削除を確認してください");
    }
    if named_write_pending {
        eprintln!("保存条件の書き込み結果が未確認です。前回条件の終了時保存は省略しました");
    } else if let Ok(criteria) = &result {
        if let Some(store) = store {
            if let Err(error) = store.save_last(criteria.clone()) {
                eprintln!("条件保存失敗: {error}");
            }
        }
    }
    if let Some(notice) = named_exit_notice {
        eprintln!("{notice}");
    }
    result.map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};
    fn screen() -> Screen {
        let now = radiko_client::radiko::stream::parse_time("20261015060000").unwrap();
        let criteria = crate::programs::criteria(&["TBS".into(), "20261015".into()], now).unwrap();
        Screen {
            help_open: false,
            help_scroll: scroll::Scroll::default(),
            failures_open: false,
            failure_list: ListState::default(),
            failure_height: 1,
            login: None,
            login_requested: None,
            logout_confirm: false,
            logout_requested: false,
            live_mode: false,
            live_rows: vec![],
            live_refresh_at: None,
            last_live_time: None,
            timefree_criteria: None,
            seek: None,
            named: None,
            named_action: None,
            named_generation: 0,
            filters: None,
            auth_refresh: false,
            chapter_action: None,
            chapter_list: ListState::default(),
            chapter_height: 1,
            chapter_summary_focused: false,
            chapter_summary_scroll: scroll::Scroll::default(),
            chapter_notice: String::new(),
            playback_action: None,
            playback_notice: String::new(),
            playback_diagnostics: String::new(),
            volume: 25,
            app: AppState::new(criteria),
            list: ListState::default(),
            list_height: 1,
            input: None,
            preferences_notice: String::new(),
        }
    }
    #[test]
    fn region_name_picker_does_not_acquire_until_the_form_is_confirmed() {
        let mut screen = screen();
        let original = screen.app.committed.clone();
        let mut search = Service::new(|| -> Result<CatalogClient, _> {
            panic!("region picker must not acquire")
        });
        for code in [
            KeyCode::Char('f'),
            KeyCode::Tab,
            KeyCode::Tab,
            KeyCode::F(2),
            KeyCode::End,
            KeyCode::Char(' '),
            KeyCode::Enter,
        ] {
            screen
                .key(KeyEvent::new(code, KeyModifiers::NONE), &mut search)
                .unwrap();
            assert_eq!(screen.app.status, SearchStatus::Idle);
            assert!(screen.playback_action.is_none());
        }
        assert_eq!(screen.app.committed, original);
        assert_eq!(
            screen.app.draft.as_ref().unwrap().scope(),
            &Scope::Areas(vec!["JP47".into()])
        );
        screen
            .key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut search)
            .unwrap();
        assert_eq!(screen.app.committed, original);
    }
    #[test]
    fn named_classification_selection_edits_only_the_draft_without_acquisition() {
        let mut screen = screen();
        let original = screen.app.committed.clone();
        let mut p = radiko_client::radiko::catalog::parse_programs(
            include_bytes!("../tests/fixtures/schedule.xml"),
            "TBS",
            radiko_client::radiko::catalog::parse_day("20261015").unwrap(),
        )
        .unwrap()
        .programs
        .remove(0);
        p.program_genres = vec![radiko_client::domain::Classification {
            id: "P777".into(),
            name: "synthetic observed".into(),
        }];
        screen.app.programs = vec![p];
        let mut search =
            Service::new(|| -> Result<CatalogClient, _> { panic!("picker must not acquire") });
        for code in [
            KeyCode::Char('f'),
            KeyCode::Tab,
            KeyCode::Tab,
            KeyCode::Tab,
            KeyCode::F(2),
            KeyCode::End,
            KeyCode::Char(' '),
            KeyCode::Enter,
        ] {
            screen
                .key(KeyEvent::new(code, KeyModifiers::NONE), &mut search)
                .unwrap();
            assert!(screen.playback_action.is_none());
            assert_eq!(screen.app.status, SearchStatus::Idle);
        }
        assert_eq!(screen.app.committed, original);
        assert_eq!(
            screen.app.draft.as_ref().unwrap().filter().program_genres(),
            &["P777"]
        );
        assert!(screen.filters.is_some());
        screen
            .key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut search)
            .unwrap();
        assert!(screen.filters.is_none());
        assert_eq!(screen.app.committed, original);
    }
    #[test]
    fn login_form_cancels_without_io_and_submits_only_after_password_confirmation() {
        let mut screen = screen();
        let mut search =
            Service::new(|| -> Result<CatalogClient, _> { panic!("login input must not search") });
        let original = screen.app.committed.clone();
        screen
            .key(
                KeyEvent::new(KeyCode::Char('I'), KeyModifiers::NONE),
                &mut search,
            )
            .unwrap();
        assert!(screen.login.is_some());
        assert!(
            !screen
                .key(
                    KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
                    &mut search
                )
                .unwrap()
        );
        screen
            .key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut search)
            .unwrap();
        assert!(screen.login.is_none());
        assert!(screen.login_requested.is_none());
        screen
            .key(
                KeyEvent::new(KeyCode::Char('I'), KeyModifiers::NONE),
                &mut search,
            )
            .unwrap();
        for c in "user@example.invalid".chars() {
            screen
                .key(
                    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
                    &mut search,
                )
                .unwrap();
        }
        screen
            .key(
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                &mut search,
            )
            .unwrap();
        assert!(screen.login_requested.is_none());
        for c in "SYNTHETIC_PASSWORD_99".chars() {
            screen
                .key(
                    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
                    &mut search,
                )
                .unwrap();
        }
        let mut terminal = Terminal::new(TestBackend::new(160, 40)).unwrap();
        terminal
            .draw(|frame| draw(frame, &mut screen, Utc::now().fixed_offset()))
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(!text.contains("SYNTHETIC"));
        assert!(text.replace(' ', "").contains("非表示"));
        screen
            .key(
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                &mut search,
            )
            .unwrap();
        assert!(screen.login.is_none());
        assert!(screen.login_requested.is_some());
        assert!(screen.playback_action.is_none());
        assert_eq!(screen.app.committed, original);
        screen.app.authentication.begin_login().unwrap();
        for c in ['I', 'O', 'a'] {
            screen
                .key(
                    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
                    &mut search,
                )
                .unwrap();
            assert!(screen.login.is_none());
            assert!(!screen.logout_confirm);
            assert!(!screen.auth_refresh);
        }
    }
    #[test]
    fn logout_requires_confirmation_and_blocks_auth_or_playback_while_deleting() {
        let mut screen = screen();
        let mut search =
            Service::new(|| -> Result<CatalogClient, _> { panic!("logout must not search") });
        let original = screen.app.committed.clone();
        for cancel in [KeyCode::Char('n'), KeyCode::Esc] {
            screen
                .key(
                    KeyEvent::new(KeyCode::Char('O'), KeyModifiers::NONE),
                    &mut search,
                )
                .unwrap();
            assert!(screen.logout_confirm);
            screen
                .key(KeyEvent::new(cancel, KeyModifiers::NONE), &mut search)
                .unwrap();
            assert!(!screen.logout_confirm);
            assert!(!screen.logout_requested);
            assert!(screen.playback_action.is_none());
        }
        for code in [KeyCode::Char('O'), KeyCode::Char('y')] {
            screen
                .key(KeyEvent::new(code, KeyModifiers::NONE), &mut search)
                .unwrap();
        }
        assert!(screen.logout_requested);
        screen.app.authentication.begin_logout().unwrap();
        for code in [
            KeyCode::Enter,
            KeyCode::Char('l'),
            KeyCode::Char('a'),
            KeyCode::Char(' '),
            KeyCode::Char('O'),
        ] {
            screen
                .key(KeyEvent::new(code, KeyModifiers::NONE), &mut search)
                .unwrap();
            assert!(screen.playback_action.is_none());
            assert!(!screen.auth_refresh);
            assert!(!screen.logout_confirm);
        }
        assert_eq!(screen.app.committed, original);
        assert!(
            screen
                .key(
                    KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
                    &mut search
                )
                .unwrap()
        );
    }
    #[test]
    fn live_rows_update_at_slot_boundaries_without_fetching_or_starting_playback() {
        let mut screen = screen();
        let mut programs = radiko_client::radiko::catalog::parse_programs(
            include_bytes!("../tests/fixtures/schedule.xml"),
            "TBS",
            radiko_client::radiko::catalog::parse_day("20261015").unwrap(),
        )
        .unwrap()
        .programs;
        let mut first = programs.remove(0);
        let start = first.key.starts_at;
        first.ends_at = start + chrono::Duration::minutes(1);
        let mut next = first.clone();
        next.key.starts_at = first.ends_at;
        next.ends_at = next.key.starts_at + chrono::Duration::minutes(1);
        screen.app.programs = vec![first, next];
        screen.app.visible = vec![0, 1];
        screen.live_mode = true;
        screen.rebuild_live(start - chrono::Duration::seconds(1));
        assert!(screen.rows().is_empty());
        screen.refresh_live(start);
        assert_eq!(screen.rows(), &[0]);
        assert_eq!(screen.selected().unwrap().key.starts_at, start);
        screen.refresh_live(start + chrono::Duration::minutes(1));
        assert_eq!(screen.rows(), &[1]);
        assert_eq!(
            screen.selected().unwrap().key.starts_at,
            start + chrono::Duration::minutes(1)
        );
        screen.refresh_live(start + chrono::Duration::minutes(2));
        assert!(screen.rows().is_empty());
        assert!(screen.selected().is_none());
        assert!(screen.playback_action.is_none());
        assert_eq!(screen.app.visible, vec![0, 1]);
        screen.refresh_live(start + chrono::Duration::seconds(30));
        assert_eq!(screen.rows(), &[0]);
        screen.refresh_live(start - chrono::Duration::seconds(1));
        assert!(screen.rows().is_empty());
        screen.refresh_live(start);
        assert_eq!(screen.rows(), &[0]);
    }
    #[test]
    fn live_enter_uses_the_selected_on_air_slot_without_searching() {
        let mut screen = screen();
        let mut search =
            Service::new(|| -> Result<CatalogClient, _> { panic!("enter must not search") });
        let mut p = radiko_client::radiko::catalog::parse_programs(
            include_bytes!("../tests/fixtures/schedule.xml"),
            "TBS",
            radiko_client::radiko::catalog::parse_day("20261015").unwrap(),
        )
        .unwrap()
        .programs
        .remove(0);
        let now = Utc::now().fixed_offset();
        p.key.starts_at = now - chrono::Duration::minutes(1);
        p.ends_at = now + chrono::Duration::minutes(1);
        screen.app.programs = vec![p];
        screen.app.visible = vec![0];
        screen.live_mode = true;
        screen.rebuild_live(now);
        screen.restore_selection(None);
        screen
            .key(
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                &mut search,
            )
            .unwrap();
        assert!(matches!(
            screen.playback_action.take(),
            Some(PlaybackAction::Live(_))
        ));
        let target = &screen.app.programs[0];
        screen.app.playback = Some(radiko_client::player::state::PlaybackSnapshot {
            info: radiko_client::radiko::stream::playback::StreamInfo {
                station: target.key.station_id.clone(),
                title: Some(target.title.clone()),
                mode: radiko_client::radiko::stream::ProbeMode::Live,
                starts_at: target.key.starts_at,
                ends_at: target.ends_at,
                position: Duration::ZERO,
            },
            state: PlaybackState::Playing,
            pending: None,
            volume: 25,
            position: None,
            output: None,
            segments: 1,
        });
        let mut terminal = Terminal::new(TestBackend::new(160, 40)).unwrap();
        terminal
            .draw(|frame| draw(frame, &mut screen, now))
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.replace(' ', "").contains("Enterライブ"));
        assert!(text.replace(' ', "").contains("番組内位置未確認"));
        assert!(!text.contains("未確認秒"));
    }
    #[test]
    fn live_view_switch_refetches_today_and_restores_original_timefree_range() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        runtime.block_on(async {
            let mut screen = screen();
            let original = screen.app.committed.clone();
            let mut search = Service::new(|| -> Result<CatalogClient, _> {
                Err(radiko_client::radiko::catalog::CatalogError::Network)
            });
            screen.toggle_live(&mut search).unwrap();
            assert!(screen.live_mode);
            assert_eq!(
                screen.app.committed,
                original.restore_startup(Utc::now().fixed_offset()).unwrap()
            );
            assert_eq!(screen.app.status, SearchStatus::Loading);
            assert!(screen.rows().is_empty());
            assert!(screen.playback_action.is_none());
            screen.toggle_live(&mut search).unwrap();
            assert!(!screen.live_mode);
            assert_eq!(screen.app.committed, original);
            screen.toggle_live(&mut search).unwrap();
            screen.app.begin_edit();
            screen.app.edit_criteria(original.clone()).unwrap();
            screen.submit(&mut search).unwrap();
            assert!(!screen.live_mode);
            assert_eq!(screen.app.committed, original);

            assert_eq!(screen.app.status, SearchStatus::Loading);
            assert!(screen.playback_action.is_none());
        });
    }
    #[test]
    fn named_save_and_recall_do_not_search_and_recall_preserves_saved_dates_in_draft() {
        let mut screen = screen();
        let original = screen.app.committed.clone();
        let mut search = Service::new(|| -> Result<CatalogClient, _> {
            panic!("named operations must not search")
        });
        screen
            .key(
                KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE),
                &mut search,
            )
            .unwrap();
        let view = screen.named.as_ref().unwrap().view;
        assert!(matches!(
            screen.named_action.take(),
            Some(NamedOperation::List)
        ));
        screen
            .named_event(NamedEvent {
                view,
                mutating: false,
                result: Ok(NamedOutcome::Listed(Default::default())),
            })
            .unwrap();
        screen
            .key(
                KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE),
                &mut search,
            )
            .unwrap();
        for c in "昼番組".chars() {
            screen
                .key(
                    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
                    &mut search,
                )
                .unwrap();
        }
        screen
            .key(
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                &mut search,
            )
            .unwrap();
        let Some(NamedOperation::Save(name, criteria)) = screen.named_action.take() else {
            panic!()
        };
        assert_eq!(name, "昼番組");
        assert_eq!(*criteria, original);
        screen
            .named_event(NamedEvent {
                view,
                mutating: true,
                result: Ok(NamedOutcome::Saved(name.clone())),
            })
            .unwrap();
        assert!(screen.preferences_notice.contains("保存しました"));
        screen.named_action.take();
        let saved = SearchCriteria::new(
            Scope::Areas(vec!["JP44".into()]),
            radiko_client::domain::BroadcastRange::new(
                radiko_client::radiko::catalog::parse_day("20261006").unwrap(),
                radiko_client::radiko::catalog::parse_day("20261007").unwrap(),
            )
            .unwrap(),
            radiko_client::domain::search::Filter::default(),
        )
        .unwrap();
        screen
            .named_event(NamedEvent {
                view,
                mutating: false,
                result: Ok(NamedOutcome::Listed(std::collections::BTreeMap::from([(
                    name.clone(),
                    saved.clone(),
                )]))),
            })
            .unwrap();
        screen
            .key(
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                &mut search,
            )
            .unwrap();
        assert!(
            matches!(screen.named_action.take(),Some(NamedOperation::Recall(key)) if key==name)
        );
        screen
            .named_event(NamedEvent {
                view,
                mutating: false,
                result: Ok(NamedOutcome::Recalled(Box::new(saved.clone()))),
            })
            .unwrap();
        assert!(screen.named.is_none());
        assert_eq!(screen.filters.as_ref().unwrap().criteria().unwrap(), saved);
        assert_eq!(screen.app.draft, Some(saved));
        assert_eq!(screen.app.committed, original);
        assert_eq!(screen.app.status, SearchStatus::Idle);
        assert!(screen.playback_action.is_none());
        screen
            .key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut search)
            .unwrap();
        assert_eq!(screen.app.committed, original);
        assert!(screen.app.draft.is_none());
    }
    #[test]
    fn closed_or_reopened_named_views_ignore_old_recalls_but_keep_mutation_receipts() {
        let mut screen = screen();
        let mut search = Service::new(|| -> Result<CatalogClient, _> { panic!("no search") });
        screen
            .key(
                KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE),
                &mut search,
            )
            .unwrap();
        let old = screen.named.as_ref().unwrap().view;
        screen.named_action.take();
        screen
            .key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut search)
            .unwrap();
        screen
            .key(
                KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE),
                &mut search,
            )
            .unwrap();
        let current = screen.named.as_ref().unwrap().view;
        assert_ne!(old, current);
        screen.named_action.take();
        screen
            .named_event(NamedEvent {
                view: old,
                mutating: false,
                result: Ok(NamedOutcome::Recalled(Box::new(
                    screen.app.committed.clone(),
                ))),
            })
            .unwrap();
        assert!(screen.filters.is_none());
        assert_eq!(screen.named.as_ref().unwrap().view, current);
        assert!(matches!(
            screen.named_action.take(),
            Some(NamedOperation::List)
        ));
        screen
            .key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut search)
            .unwrap();
        screen
            .named_event(NamedEvent {
                view: old,
                mutating: true,
                result: Ok(NamedOutcome::Deleted("削除済み".into())),
            })
            .unwrap();
        assert!(screen.preferences_notice.contains("削除しました"));
        assert!(screen.named.is_none());
        assert!(screen.filters.is_none());
        assert!(screen.playback_action.is_none());
        assert_eq!(screen.app.status, SearchStatus::Idle);
    }
    #[test]
    fn filter_edit_invalid_confirmation_and_cancel_preserve_committed_conditions_without_requests()
    {
        let mut screen = screen();
        let original = screen.app.committed.clone();
        let mut search =
            Service::new(|| -> Result<CatalogClient, _> { panic!("editing must not acquire") });
        screen
            .key(
                KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE),
                &mut search,
            )
            .unwrap();
        screen
            .key(
                KeyEvent::new_with_kind(KeyCode::Enter, KeyModifiers::NONE, KeyEventKind::Repeat),
                &mut search,
            )
            .unwrap();
        assert!(screen.filters.is_some());
        assert_eq!(screen.app.status, SearchStatus::Idle);
        screen
            .key(
                KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
                &mut search,
            )
            .unwrap();
        screen
            .key(
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                &mut search,
            )
            .unwrap();
        assert!(!screen.filters.as_ref().unwrap().error.is_empty());
        assert_eq!(screen.app.committed, original);
        assert_eq!(screen.app.status, SearchStatus::Idle);
        assert!(screen.playback_action.is_none());
        assert!(!screen.auth_refresh);
        assert!(
            !screen
                .key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut search)
                .unwrap()
        );
        assert!(screen.filters.is_none());
        assert!(screen.app.draft.is_none());
        assert_eq!(screen.app.committed, original);
    }
    #[test]
    fn filter_confirmation_commits_once_and_runs_a_new_search_without_playback_actions() {
        use std::sync::atomic::AtomicUsize;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        runtime.block_on(async {
            let mut screen = screen();
            let calls = Arc::new(AtomicUsize::new(0));
            let observed = calls.clone();
            let mut search = Service::new(move || -> Result<CatalogClient, _> {
                observed.fetch_add(1, Ordering::SeqCst);
                Err(radiko_client::radiko::catalog::CatalogError::InvalidDate)
            });
            screen
                .key(
                    KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE),
                    &mut search,
                )
                .unwrap();
            for _ in 0..5 {
                screen
                    .key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &mut search)
                    .unwrap();
            }
            for c in "日本語".chars() {
                screen
                    .key(
                        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
                        &mut search,
                    )
                    .unwrap();
            }
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            assert_eq!(screen.app.committed.filter().keyword(), None);
            assert_eq!(
                screen.app.draft.as_ref().unwrap().filter().keyword(),
                Some("日本語")
            );
            screen
                .key(
                    KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                    &mut search,
                )
                .unwrap();
            assert!(screen.filters.is_none());
            assert!(screen.app.draft.is_none());
            assert_eq!(screen.app.committed.filter().keyword(), Some("日本語"));
            assert_eq!(screen.app.status, SearchStatus::Loading);
            let began = std::time::Instant::now();
            while screen.app.status == SearchStatus::Loading {
                assert!(began.elapsed() < Duration::from_secs(3));
                for event in search.poll().await {
                    screen.apply(event);
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            assert_eq!(screen.app.status, SearchStatus::Failed);
            assert!(screen.playback_action.is_none());
        });
    }
    #[test]
    fn auth_refresh_is_independent_of_search_and_playback_and_is_text_during_editing() {
        let mut screen = screen();
        let mut search =
            Service::new(|| -> Result<CatalogClient, _> { panic!("auth key must not search") });
        screen
            .key(
                KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
                &mut search,
            )
            .unwrap();
        assert!(std::mem::take(&mut screen.auth_refresh));
        assert!(screen.playback_action.is_none());
        assert!(screen.chapter_action.is_none());
        assert_eq!(screen.app.status, SearchStatus::Idle);
        screen.app.authentication.status =
            radiko_client::app::auth::Status::Verified(radiko_client::session::Membership {
                area_free: Some(true),
                time_free_30: None,
            });
        let mut terminal = Terminal::new(TestBackend::new(160, 40)).unwrap();
        terminal
            .draw(|frame| draw(frame, &mut screen, Utc::now().fixed_offset()))
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .filter(|cell| cell.symbol() != " ")
            .map(|cell| cell.symbol())
            .collect();
        for word in [
            "ログイン確認済み",
            "エリアフリー:有効",
            "30日:未確認",
            "再生可否は番組ごとに確認",
        ] {
            assert!(text.contains(word), "{word}");
        }
        screen
            .key(
                KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE),
                &mut search,
            )
            .unwrap();
        screen
            .key(
                KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
                &mut search,
            )
            .unwrap();
        assert!(!screen.auth_refresh);
        assert_eq!(screen.input.as_deref(), Some("a"));
    }
    #[test]
    fn failure_browser_shows_all_missing_ranges_and_never_dispatches_actions() {
        let mut screen = screen();
        let mut search = Service::new(|| -> Result<CatalogClient, _> { panic!("read-only view") });
        screen.app.status = SearchStatus::Partial;
        for index in 0..100 {
            screen
                .app
                .schedule_failures
                .push(radiko_client::catalog::Failure {
                    target: format!("MISSING_{index:03}"),
                    day: Some(radiko_client::radiko::catalog::parse_day("20261015").unwrap()),
                    error: radiko_client::radiko::catalog::CatalogError::Http(404),
                });
        }
        screen
            .key(
                KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE),
                &mut search,
            )
            .unwrap();
        let mut terminal = Terminal::new(TestBackend::new(160, 40)).unwrap();
        terminal
            .draw(|frame| draw(frame, &mut screen, Utc::now().fixed_offset()))
            .unwrap();
        for key in [
            KeyCode::End,
            KeyCode::Enter,
            KeyCode::Char('r'),
            KeyCode::Char('I'),
            KeyCode::Char('s'),
        ] {
            assert!(
                !screen
                    .key(KeyEvent::new(key, KeyModifiers::NONE), &mut search)
                    .unwrap()
            );
        }
        terminal
            .draw(|frame| draw(frame, &mut screen, Utc::now().fixed_offset()))
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("MISSING_099"));
        assert!(text.contains("2026-10-15"));
        assert!(text.replace(' ', "").contains("局・日失敗100"));
        assert_eq!(screen.failure_list.selected(), Some(99));
        assert!(screen.playback_action.is_none());
        assert!(screen.chapter_action.is_none());
        assert!(screen.login.is_none());
        screen
            .app
            .invalid_schedules
            .push(radiko_client::app::InvalidSchedule {
                station_id: "BAD_SLOT_STATION".into(),
                day: radiko_client::radiko::catalog::parse_day("20261015").unwrap(),
                count: 2,
            });
        screen.app.invalid_count = 2;
        screen
            .key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE), &mut search)
            .unwrap();
        terminal
            .draw(|frame| draw(frame, &mut screen, Utc::now().fixed_offset()))
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("BAD_SLOT_STATION"));
        assert!(text.replace(' ', "").contains("2件（正常な枠は表示）"));
        // A new acquisition clears failures; an open browser must not retain the old range.
        screen.app.commit().unwrap();
        terminal
            .draw(|frame| draw(frame, &mut screen, Utc::now().fixed_offset()))
            .unwrap();
        assert_eq!(screen.failure_list.selected(), None);
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(!text.contains("MISSING_"));
        assert!(text.replace(' ', "").contains("取得中"));
        screen
            .app
            .schedule_failures
            .push(radiko_client::catalog::Failure {
                target: "NEW_FAILURE".into(),
                day: None,
                error: radiko_client::radiko::catalog::CatalogError::Network,
            });
        terminal
            .draw(|frame| draw(frame, &mut screen, Utc::now().fixed_offset()))
            .unwrap();
        assert_eq!(screen.failure_list.selected(), Some(0));

        assert!(
            !screen
                .key(
                    KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
                    &mut search
                )
                .unwrap()
        );
        assert!(!screen.failures_open);
    }

    #[test]
    fn catalog_page_keys_only_change_selection_and_use_the_rendered_height() {
        let mut screen = screen();
        let mut search =
            Service::new(|| -> Result<CatalogClient, _> { panic!("navigation must not fetch") });
        let template = radiko_client::radiko::catalog::parse_programs(
            include_bytes!("../tests/fixtures/schedule.xml"),
            "TBS",
            radiko_client::radiko::catalog::parse_day("20261015").unwrap(),
        )
        .unwrap()
        .programs
        .remove(0);
        screen.app.programs = vec![template; 100];
        screen.app.visible = (0..100).collect();
        screen.list.select(Some(0));
        for height in [30, 40] {
            let mut terminal = Terminal::new(TestBackend::new(140, height)).unwrap();
            terminal
                .draw(|frame| draw(frame, &mut screen, Utc::now().fixed_offset()))
                .unwrap();
            let page = screen.list_height;
            let before = screen.list.selected().unwrap();
            screen
                .key(
                    KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE),
                    &mut search,
                )
                .unwrap();
            assert_eq!(screen.list.selected(), Some(before + page));
        }
        for (key, expected) in [(KeyCode::End, 99), (KeyCode::Home, 0)] {
            screen
                .key(KeyEvent::new(key, KeyModifiers::NONE), &mut search)
                .unwrap();
            assert_eq!(screen.list.selected(), Some(expected));
        }
        screen.live_mode = true;
        screen.live_rows = vec![2, 7];
        screen
            .key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE), &mut search)
            .unwrap();
        assert_eq!(screen.list.selected(), Some(1));
        assert!(screen.playback_action.is_none());
        assert!(screen.chapter_action.is_none());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        assert!(runtime.block_on(search.poll()).is_empty());
    }

    #[test]
    fn large_catalog_keeps_global_selection_visible_at_the_end_and_after_moving_up() {
        let mut screen = screen();
        let mut search =
            Service::new(|| -> Result<CatalogClient, _> { panic!("navigation must not fetch") });
        let template = radiko_client::radiko::catalog::parse_programs(
            include_bytes!("../tests/fixtures/schedule.xml"),
            "TBS",
            radiko_client::radiko::catalog::parse_day("20261015").unwrap(),
        )
        .unwrap()
        .programs
        .remove(0);
        const ROWS: usize = 50000;
        screen.app.programs = (0..ROWS)
            .map(|i| {
                let mut program = template.clone();
                program.title = format!("CATALOG_{i:05}");
                program.key.station_id = format!("S{}", i % 100);
                program.key.starts_at += chrono::Duration::minutes((i / 100) as i64 * 30);
                program.ends_at = program.key.starts_at + chrono::Duration::minutes(30);
                program
            })
            .collect();
        screen.app.visible = (0..ROWS).collect();
        screen.list.select(Some(ROWS - 1));
        let mut terminal = Terminal::new(TestBackend::new(140, 30)).unwrap();
        for expected in [ROWS - 1, ROWS - 2] {
            let began = std::time::Instant::now();
            terminal
                .draw(|frame| draw(frame, &mut screen, Utc::now().fixed_offset()))
                .unwrap();
            eprintln!(
                "catalog rows={ROWS}, draw_us={}",
                began.elapsed().as_micros()
            );
            assert_eq!(screen.list.selected(), Some(expected));
            let mut left = String::new();
            for y in 7..21 {
                for x in 0..67 {
                    left.push_str(terminal.backend().buffer()[(x, y)].symbol());
                }
            }
            assert!(left.contains(&format!("CATALOG_{expected:05}")));
            assert_eq!(
                screen.selected().unwrap().title,
                format!("CATALOG_{expected:05}")
            );
            screen
                .key(
                    KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE),
                    &mut search,
                )
                .unwrap();
        }
    }
    #[test]
    fn help_never_dispatches_search_authentication_or_playback_and_closes_without_quitting() {
        let mut screen = screen();
        let mut search =
            Service::new(|| -> Result<CatalogClient, _> { panic!("help must not fetch") });
        screen.volume = 17;
        screen
            .key(
                KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE),
                &mut search,
            )
            .unwrap();
        assert!(screen.help_open);
        for code in [
            KeyCode::Enter,
            KeyCode::Char('s'),
            KeyCode::Char('I'),
            KeyCode::Char('O'),
            KeyCode::Char('r'),
            KeyCode::Char('v'),
            KeyCode::Char(' '),
            KeyCode::Char('+'),
        ] {
            assert!(
                !screen
                    .key(KeyEvent::new(code, KeyModifiers::NONE), &mut search)
                    .unwrap()
            );
        }
        assert!(screen.playback_action.is_none());
        assert!(screen.chapter_action.is_none());
        assert!(screen.login_requested.is_none());
        assert!(!screen.logout_requested);
        assert!(!screen.auth_refresh);
        assert_eq!(screen.volume, 17);
        assert_eq!(screen.app.status, SearchStatus::Idle);
        for size in [(100, 30), (40, 10)] {
            let mut terminal = Terminal::new(TestBackend::new(size.0, size.1)).unwrap();
            terminal
                .draw(|frame| draw(frame, &mut screen, Utc::now().fixed_offset()))
                .unwrap();
            let text: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect();
            assert!(text.replace(' ', "").contains("操作一覧を閉じる"));
        }
        for close in [KeyCode::Esc, KeyCode::Char('?'), KeyCode::Char('q')] {
            screen.help_open = true;
            assert!(
                !screen
                    .key(KeyEvent::new(close, KeyModifiers::NONE), &mut search)
                    .unwrap()
            );
            assert!(!screen.help_open);
        }
        screen.help_open = true;
        assert!(
            screen
                .key(
                    KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
                    &mut search
                )
                .unwrap()
        );
    }
    #[test]
    fn question_mark_in_keyword_draft_does_not_open_help_or_fetch() {
        let mut screen = screen();
        let mut search =
            Service::new(|| -> Result<CatalogClient, _> { panic!("draft must not fetch") });
        screen
            .key(
                KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE),
                &mut search,
            )
            .unwrap();
        screen
            .key(
                KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE),
                &mut search,
            )
            .unwrap();
        assert_eq!(screen.input.as_deref(), Some("?"));
        assert!(!screen.help_open);
        assert_eq!(screen.app.status, SearchStatus::Idle);
    }
    #[test]
    fn aligned_chapter_enter_seeks_only_same_timefree_slot_and_switches_other_targets() {
        use radiko_client::radiko::stream::{
            ProbeMode,
            chapters::{EventBinding, Outcome, Report},
        };
        let mut screen = screen();
        let mut search =
            Service::new(|| -> Result<CatalogClient, _> { panic!("jump must not search") });
        let program = radiko_client::radiko::catalog::parse_programs(
            include_bytes!("../tests/fixtures/schedule.xml"),
            "TBS",
            radiko_client::radiko::catalog::parse_day("20261015").unwrap(),
        )
        .unwrap()
        .programs
        .remove(0);
        let id = screen.app.chapters.open(program.clone()).unwrap();
        screen
            .app
            .chapters
            .apply(radiko_client::app::chapters::Event {
                id,
                key: program.key.clone(),
                result: Ok(Report {
                    station: program.key.station_id.clone(),
                    starts_at: program.key.starts_at,
                    ends_at: program.ends_at,
                    title: Some(program.title.clone()),
                    outcome: Outcome::Aligned(
                        radiko_client::radiko::parse_chapters(
                            include_bytes!("../tests/fixtures/chapters.json"),
                            Duration::from_secs(90),
                            0.0,
                        )
                        .unwrap(),
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
        let expected = screen.app.chapters.jump(0).unwrap().1;
        for (station, start, mode, same) in [
            ("TBS", program.key.starts_at, ProbeMode::TimeFree, true),
            ("OBS", program.key.starts_at, ProbeMode::TimeFree, false),
            (
                "TBS",
                program.key.starts_at + chrono::Duration::hours(1),
                ProbeMode::TimeFree,
                false,
            ),
            ("TBS", program.key.starts_at, ProbeMode::Live, false),
        ] {
            screen.app.playback = Some(radiko_client::player::state::PlaybackSnapshot {
                info: radiko_client::radiko::stream::playback::StreamInfo {
                    title: Some(program.title.clone()),
                    station: station.into(),
                    starts_at: start,
                    ends_at: program.ends_at,
                    mode,
                    position: expected,
                },
                state: PlaybackState::Playing,
                pending: None,
                volume: 25,
                position: Some(expected),
                output: Some(radiko_client::player::output::OutputSummary {
                    frames: 1,
                    underrun_frames: 0,
                    sample_rate: 48000,
                    channels: 2,
                }),
                segments: 1,
            });
            screen
                .key(
                    KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                    &mut search,
                )
                .unwrap();
            match screen.playback_action.take().unwrap() {
                PlaybackAction::Command(Command::SeekSlot { key, position }) if same => {
                    assert_eq!(key, program.key);
                    assert_eq!(position, expected);
                }
                PlaybackAction::Chapter(target, position) if !same => {
                    assert_eq!(target.key, program.key);
                    assert_eq!(position, expected);
                }
                _ => panic!("wrong jump path"),
            }
            let mut terminal = Terminal::new(TestBackend::new(140, 30)).unwrap();
            terminal
                .draw(|frame| draw(frame, &mut screen, Utc::now().fixed_offset()))
                .unwrap();
            let text: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect();
            assert_eq!(text.replace(' ', "").contains("[再生位置]"), same);
            screen.app.playback.as_mut().unwrap().output = None;
            terminal
                .draw(|frame| draw(frame, &mut screen, Utc::now().fixed_offset()))
                .unwrap();
            let preparing: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect();
            assert!(!preparing.replace(' ', "").contains("[再生位置]"));
        }
        screen.app.authentication.status = AuthStatus::LoggingOut;
        screen
            .key(
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                &mut search,
            )
            .unwrap();
        assert!(screen.playback_action.is_none());
    }
    #[test]
    fn chapter_navigation_refresh_and_refused_enter_never_generate_playback_or_search() {
        let mut screen = screen();
        let mut search =
            Service::new(|| -> Result<CatalogClient, _> { panic!("chapter keys must not search") });
        let program = radiko_client::radiko::catalog::parse_programs(
            include_bytes!("../tests/fixtures/schedule.xml"),
            "TBS",
            radiko_client::radiko::catalog::parse_day("20261015").unwrap(),
        )
        .unwrap()
        .programs
        .remove(0);
        screen.app.programs = vec![program.clone()];
        screen.app.visible = vec![0];
        screen.list.select(Some(0));
        screen.app.playback = Some(radiko_client::player::state::PlaybackSnapshot {
            info: radiko_client::radiko::stream::playback::StreamInfo {
                title: Some("現在の別番組".into()),
                station: "OBS".into(),
                starts_at: program.key.starts_at,
                ends_at: program.ends_at,
                mode: radiko_client::radiko::stream::ProbeMode::TimeFree,
                position: Duration::ZERO,
            },
            state: PlaybackState::Playing,
            pending: None,
            volume: 25,
            position: Some(Duration::from_secs(8)),
            output: None,
            segments: 1,
        });
        screen
            .key(
                KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE),
                &mut search,
            )
            .unwrap();
        let Some(ChapterAction::Fetch(id, target)) = screen.chapter_action.take() else {
            panic!()
        };
        assert_eq!(target.key, program.key);
        assert_eq!(screen.app.chapters.status, ChapterStatus::Loading);
        screen
            .app
            .chapters
            .apply(radiko_client::app::chapters::Event {
                id,
                key: program.key.clone(),
                result: Ok(radiko_client::radiko::stream::chapters::Report {
                    station: program.key.station_id.clone(),
                    starts_at: program.key.starts_at,
                    ends_at: program.ends_at,
                    title: Some(program.title.clone()),
                    outcome: radiko_client::radiko::stream::chapters::Outcome::Candidates(
                        radiko_client::radiko::parse_chapters(
                            include_bytes!("../tests/fixtures/chapters.json"),
                            Duration::from_secs(90),
                            0.0,
                        )
                        .unwrap(),
                    ),
                }),
            });
        screen.chapter_list.select(Some(0));
        for code in [
            KeyCode::End,
            KeyCode::Home,
            KeyCode::PageDown,
            KeyCode::Char('j'),
            KeyCode::Enter,
        ] {
            screen
                .key(KeyEvent::new(code, KeyModifiers::NONE), &mut search)
                .unwrap();
            assert!(screen.playback_action.is_none());
        }
        assert_eq!(screen.chapter_list.selected(), Some(1));
        assert_eq!(screen.list.selected(), Some(0));
        assert!(screen.chapter_notice.contains("未検証"));
        let mut terminal = Terminal::new(TestBackend::new(160, 40)).unwrap();
        terminal
            .draw(|frame| draw(frame, &mut screen, Utc::now().fixed_offset()))
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .filter(|cell| cell.symbol() != " ")
            .map(|cell| cell.symbol())
            .collect();
        for word in [
            "公式AIチャプター",
            "オープニング",
            "本題",
            "音声位置未検証",
            "現在の別番組",
            "OBS",
        ] {
            assert!(text.contains(word), "{word}");
        }

        screen.app.chapters.candidates.as_mut().unwrap().chapters[1].summary =
            format!("{}SUMMARY_END", "説明 ".repeat(200));
        screen
            .key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &mut search)
            .unwrap();
        let mut small = Terminal::new(TestBackend::new(80, 30)).unwrap();
        small
            .draw(|frame| draw(frame, &mut screen, Utc::now().fixed_offset()))
            .unwrap();
        assert!(screen.chapter_summary_focused);
        assert!(screen.chapter_summary_scroll.maximum > 0);
        screen
            .key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE), &mut search)
            .unwrap();
        small
            .draw(|frame| draw(frame, &mut screen, Utc::now().fixed_offset()))
            .unwrap();
        let text: String = small
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("SUMMARY_END"));
        assert_eq!(screen.chapter_list.selected(), Some(1));
        assert!(screen.playback_action.is_none());
        screen
            .key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), &mut search)
            .unwrap();
        assert_eq!(screen.chapter_list.selected(), Some(1));
        screen
            .key(
                KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT),
                &mut search,
            )
            .unwrap();
        screen
            .key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), &mut search)
            .unwrap();
        assert_eq!(screen.chapter_list.selected(), Some(0));
        assert_eq!(screen.chapter_summary_scroll.offset, 0);
        screen
            .key(
                KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
                &mut search,
            )
            .unwrap();
        screen
            .key(
                KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
                &mut search,
            )
            .unwrap();
        let Some(ChapterAction::Fetch(refreshed, _)) = screen.chapter_action.take() else {
            panic!()
        };
        assert_ne!(id, refreshed);
        assert!(screen.app.chapters.candidates.is_none());
        assert!(
            !screen
                .key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut search)
                .unwrap()
        );
        assert!(matches!(
            screen.chapter_action.take(),
            Some(ChapterAction::Cancel)
        ));
        assert!(screen.app.chapters.target.is_none());
        assert!(screen.playback_action.is_none());
        assert_eq!(screen.app.status, SearchStatus::Idle);
        let playback = screen.app.playback.as_ref().unwrap();
        assert_eq!(playback.info.station, "OBS");
        assert_eq!(playback.position, Some(Duration::from_secs(8)));
    }
    #[test]
    fn chapter_loading_unavailable_unresolved_and_failure_render_distinctly_at_small_sizes() {
        let mut screen = screen();
        let target = radiko_client::radiko::catalog::parse_programs(
            include_bytes!("../tests/fixtures/schedule.xml"),
            "TBS",
            radiko_client::radiko::catalog::parse_day("20261015").unwrap(),
        )
        .unwrap()
        .programs
        .remove(0);
        screen.open_chapters(target).unwrap();
        for (state, label) in [
            (ChapterStatus::Loading, "取得中"),
            (ChapterStatus::Unavailable, "未提供"),
            (
                ChapterStatus::Unresolved(
                    radiko_client::radiko::stream::chapters::BindingIssue::EventMetadataMissing,
                ),
                "イベント情報を確認できません",
            ),
            (
                ChapterStatus::Unresolved(
                    radiko_client::radiko::stream::chapters::BindingIssue::OriginMismatch,
                ),
                "補正未対応",
            ),
            (
                ChapterStatus::Unresolved(
                    radiko_client::radiko::stream::chapters::BindingIssue::SlotMismatch,
                ),
                "放送枠が一致しません",
            ),
            (ChapterStatus::Failed("合成エラー".into()), "取得失敗"),
        ] {
            screen.app.chapters.status = state;
            let mut terminal = Terminal::new(TestBackend::new(160, 40)).unwrap();
            terminal
                .draw(|frame| draw(frame, &mut screen, Utc::now().fixed_offset()))
                .unwrap();
            let text: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .filter(|cell| cell.symbol() != " ")
                .map(|cell| cell.symbol())
                .collect();
            assert!(text.contains(label), "{label}");
            let mut tiny = Terminal::new(TestBackend::new(8, 4)).unwrap();
            tiny.draw(|frame| draw(frame, &mut screen, Utc::now().fixed_offset()))
                .unwrap();
        }
    }
    #[test]
    fn playback_keys_are_separate_from_search_editing_and_future_slots() {
        let mut screen = screen();
        let mut search = Service::new(|| -> Result<CatalogClient, _> {
            panic!("playback keys must not fetch search")
        });
        let mut programs = radiko_client::radiko::catalog::parse_programs(
            include_bytes!("../tests/fixtures/schedule.xml"),
            "TBS",
            radiko_client::radiko::catalog::parse_day("20261015").unwrap(),
        )
        .unwrap()
        .programs;
        let program = programs.remove(0);
        screen.app.programs = vec![program.clone()];
        screen.app.visible = vec![0];
        screen.list.select(Some(0));
        screen
            .key(
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                &mut search,
            )
            .unwrap();
        assert!(matches!(
            screen.playback_action.take(),
            Some(PlaybackAction::Select(_))
        ));
        screen
            .key(
                KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE),
                &mut search,
            )
            .unwrap();
        assert!(screen.playback_action.is_none());
        assert!(screen.playback_notice.contains("放送終了"));
        screen.app.programs[0].key.starts_at =
            Utc::now().fixed_offset() - chrono::Duration::minutes(1);
        screen.app.programs[0].ends_at = Utc::now().fixed_offset() + chrono::Duration::minutes(1);
        screen
            .key(
                KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE),
                &mut search,
            )
            .unwrap();
        assert!(matches!(
            screen.playback_action.take(),
            Some(PlaybackAction::Live(_))
        ));
        screen.app.programs[0].key.starts_at =
            Utc::now().fixed_offset() + chrono::Duration::hours(1);
        screen.app.programs[0].ends_at = Utc::now().fixed_offset() + chrono::Duration::hours(2);
        screen
            .key(
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                &mut search,
            )
            .unwrap();
        assert!(screen.playback_action.is_none());
        assert!(screen.playback_notice.contains("放送前"));
        screen.app.playback = Some(radiko_client::player::state::PlaybackSnapshot {
            info: radiko_client::radiko::stream::playback::StreamInfo {
                title: Some(program.title),
                station: program.key.station_id,
                starts_at: program.key.starts_at,
                ends_at: program.ends_at,
                mode: radiko_client::radiko::stream::ProbeMode::TimeFree,
                position: Duration::ZERO,
            },
            state: PlaybackState::Playing,
            pending: None,
            position: Some(Duration::from_secs(10)),
            volume: 25,
            output: None,
            segments: 1,
        });
        for code in [KeyCode::Char('g'), KeyCode::Char('1'), KeyCode::Char('5')] {
            screen
                .key(KeyEvent::new(code, KeyModifiers::NONE), &mut search)
                .unwrap();
            assert!(screen.playback_action.is_none());
        }
        let mut terminal = Terminal::new(TestBackend::new(160, 40)).unwrap();
        terminal
            .draw(|frame| draw(frame, &mut screen, Utc::now().fixed_offset()))
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.replace(' ', "").contains("位置指定:"));
        screen
            .key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut search)
            .unwrap();
        assert!(screen.seek.is_none());
        assert!(screen.playback_action.is_none());
        for code in [KeyCode::Char('g'), KeyCode::Char('0'), KeyCode::Enter] {
            screen
                .key(KeyEvent::new(code, KeyModifiers::NONE), &mut search)
                .unwrap();
        }
        assert!(matches!(
            screen.playback_action.take(),
            Some(PlaybackAction::Command(Command::SeekSlot {
                position: Duration::ZERO,
                ..
            }))
        ));
        for (key, expected) in [
            (KeyCode::Char(' '), "Pause"),
            (KeyCode::Left, "Relative(-15)"),
            (KeyCode::Char('+'), "Volume(30)"),
        ] {
            screen
                .key(KeyEvent::new(key, KeyModifiers::NONE), &mut search)
                .unwrap();
            let Some(PlaybackAction::Command(command)) = screen.playback_action.take() else {
                panic!()
            };
            assert_eq!(format!("{command:?}"), expected);
        }
        screen
            .key(
                KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE),
                &mut search,
            )
            .unwrap();
        for key in [
            KeyCode::Char('s'),
            KeyCode::Char(' '),
            KeyCode::Char('+'),
            KeyCode::Right,
        ] {
            screen
                .key(KeyEvent::new(key, KeyModifiers::NONE), &mut search)
                .unwrap();
            assert!(screen.playback_action.is_none());
        }
        screen
            .key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut search)
            .unwrap();
        assert!(screen.app.playback.is_some());
        assert_eq!(screen.app.status, SearchStatus::Idle);
    }
    #[test]
    fn description_preview_hides_html_attributes_and_preserves_human_text() {
        assert_eq!(
            preview("a<br><a href='https://example.test/?x=>'>b &amp; c</a>"),
            "a\nb & c"
        );
        assert_eq!(preview("2 < 3 &lt; 4 &amp;lt;"), "2 < 3 < 4 &lt;");
        assert_eq!(preview(&"あ".repeat(20_000)).chars().count(), 1200);
    }
    #[test]
    fn keyword_clear_and_modified_keys_do_not_fetch_or_dispatch_global_commands() {
        let mut screen = screen();
        let mut service =
            Service::new(|| -> Result<CatalogClient, _> { panic!("editing must not fetch") });
        for key in [
            KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('あ'), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
            KeyEvent::new(KeyCode::Char('r'), KeyModifiers::ALT),
        ] {
            screen.key(key, &mut service).unwrap();
        }
        assert_eq!(screen.input.as_deref(), Some("あ"));
        screen
            .key(
                KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
                &mut service,
            )
            .unwrap();
        assert_eq!(screen.input.as_deref(), Some(""));
        for c in ['?', 'e'] {
            screen
                .key(
                    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
                    &mut service,
                )
                .unwrap();
        }
        assert_eq!(screen.input.as_deref(), Some("?e"));
        assert!(!screen.help_open && !screen.failures_open);
        assert!(screen.playback_action.is_none());
        assert_eq!(screen.app.status, SearchStatus::Idle);
        screen
            .key(
                KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
                &mut service,
            )
            .unwrap();
        assert_eq!(screen.app.committed.filter().keyword(), None);
    }

    #[test]
    fn unicode_editing_and_cancel_do_not_start_a_search() {
        let mut screen = screen();
        let mut service =
            Service::new(|| -> Result<CatalogClient, _> { panic!("editing must not fetch") });
        for code in [
            KeyCode::Char('/'),
            KeyCode::Char('あ'),
            KeyCode::Char('い'),
            KeyCode::Backspace,
        ] {
            screen
                .key(KeyEvent::new(code, KeyModifiers::NONE), &mut service)
                .unwrap();
        }
        assert_eq!(screen.input.as_deref(), Some("あ"));
        assert_eq!(
            screen.app.draft.as_ref().unwrap().filter().keyword(),
            Some("あ")
        );
        assert_eq!(screen.app.committed.filter().keyword(), None);
        screen
            .key(
                KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
                &mut service,
            )
            .unwrap();
        assert!(screen.input.is_none());
        assert!(screen.app.draft.is_none());
        assert_eq!(screen.app.status, SearchStatus::Idle);
    }
    #[test]
    fn rendering_shows_status_and_unverified_permission_and_survives_small_terminals() {
        let mut screen = screen();
        let id = screen.app.commit().unwrap();
        let list = radiko_client::radiko::catalog::parse_programs(
            include_bytes!("../tests/fixtures/schedule.xml"),
            "TBS",
            radiko_client::radiko::catalog::parse_day("20261015").unwrap(),
        )
        .unwrap();
        screen.apply(radiko_client::app::search::Event::Progress {
            id,
            progress: radiko_client::app::search::Progress {
                phase: "番組表",
                target: "TBS".into(),
                day: Some(list.broadcast_day),
                completed: 1,
                total: 1,
                failed: 0,
                programs: list.programs,
            },
        });
        let now = radiko_client::radiko::stream::parse_time("20261015060000").unwrap();
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal
            .draw(|frame| draw(frame, &mut screen, now))
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .filter(|cell| cell.symbol() != " ")
            .map(|cell| cell.symbol())
            .collect();
        for word in [
            "タイムフリー",
            "番組一覧",
            "放送前",
            "放送中",
            "放送終了",
            "再生可否未確認",
        ] {
            assert!(text.contains(word), "{word}");
        }
        let mut tiny = Terminal::new(TestBackend::new(8, 4)).unwrap();
        tiny.draw(|frame| draw(frame, &mut screen, now)).unwrap();
    }
}
