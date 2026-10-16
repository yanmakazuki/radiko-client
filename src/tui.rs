//! Read-only TUI. Rendering has no network or playback side effects.
use chrono::{DateTime, FixedOffset, Utc};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use radiko_client::{
    app::{AppState, SearchStatus, search::Service},
    domain::{
        BroadcastStatus, Program, broadcast_status,
        search::{Scope, SearchCriteria},
    },
    radiko::catalog::CatalogClient,
    storage::searches::SearchStore,
};
use ratatui::{
    Frame,
    layout::{Constraint, Layout},
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
const USAGE: &str = "tui [programsと同じ局・地域・期間・絞り込み]（閲覧専用）\n/ キーワード編集 / Enter 検索確定 / Esc 編集取消 / j k・上下 選択 / r 再取得 / q・Ctrl+C 終了\n引数省略は前回条件を当日1日に復元。画面からの再生・チャプター操作は未対応です。";
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
struct Screen {
    app: AppState,
    list: ListState,
    input: Option<String>,
    preferences_notice: String,
}
impl Screen {
    fn selected(&self) -> Option<&Program> {
        self.list
            .selected()
            .and_then(|i| self.app.visible.get(i))
            .and_then(|i| self.app.programs.get(*i))
    }
    fn submit(&mut self, service: &mut Service) -> Result<(), &'static str> {
        let id = self.app.commit()?;
        self.list.select(None);
        service.submit(id, self.app.committed.clone());
        Ok(())
    }
    fn apply(&mut self, event: radiko_client::app::search::Event) {
        let key = self.selected().map(|p| p.key.clone());
        self.app.apply(event);
        let position = key.and_then(|key| {
            self.app
                .visible
                .iter()
                .position(|i| self.app.programs[*i].key == key)
        });
        let fallback = self
            .list
            .selected()
            .unwrap_or_default()
            .min(self.app.visible.len().saturating_sub(1));
        self.list.select(if self.app.visible.is_empty() {
            None
        } else {
            Some(position.unwrap_or(fallback))
        });
    }
    fn key(&mut self, key: KeyEvent, service: &mut Service) -> Result<bool, &'static str> {
        if key.kind == KeyEventKind::Release {
            return Ok(false);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Ok(true);
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
                KeyCode::Char(c) if !c.is_control() && input.len() + c.len_utf8() <= 1024 => {
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
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(true),
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
            KeyCode::Char('r') => self.submit(service)?,
            KeyCode::Down | KeyCode::Char('j') => {
                if !self.app.visible.is_empty() {
                    self.list.select(Some(
                        (self.list.selected().unwrap_or_default() + 1)
                            .min(self.app.visible.len() - 1),
                    ));
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if !self.app.visible.is_empty() {
                    self.list.select(Some(
                        self.list.selected().unwrap_or_default().saturating_sub(1),
                    ));
                }
            }
            KeyCode::Enter => self.app.notification = "この画面からの再生には未対応です".into(),
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
fn draw(frame: &mut Frame, screen: &mut Screen, now: DateTime<FixedOffset>) {
    let areas = Layout::vertical([
        Constraint::Length(5),
        Constraint::Min(1),
        Constraint::Length(if screen.input.is_some() { 4 } else { 5 }),
    ])
    .split(frame.area());
    let scope = match screen.app.committed.scope() {
        Scope::Nationwide => "全国".into(),
        Scope::Station(id) => id.clone(),
        Scope::Areas(ids) => ids.join(","),
    };
    let filter = screen.app.committed.filter();
    let header = format!(
        "タイムフリー・閲覧専用 / {} / {}〜{}（日本時間・05:00区切り）\nキーワード: {}\n番組ジャンル: {} / 出演者分類: {}",
        scope,
        screen.app.committed.range().start(),
        screen.app.committed.range().end(),
        filter.keyword().unwrap_or("指定なし"),
        classes(filter.program_genres(), filter.unclassified_program()),
        classes(
            filter.personality_genres(),
            filter.unclassified_personality()
        )
    );
    frame.render_widget(
        Paragraph::new(header).block(
            Block::default()
                .title("radiko-client")
                .borders(Borders::ALL),
        ),
        areas[0],
    );
    let panes = Layout::horizontal([Constraint::Percentage(48), Constraint::Percentage(52)])
        .split(areas[1]);
    let items: Vec<_> = screen
        .app
        .visible
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
                screen.app.visible.len(),
                screen.app.programs.len()
            )))
            .highlight_style(Style::default().bg(Color::DarkGray))
            .highlight_symbol("▶ "),
        panes[0],
        &mut screen.list,
    );
    let mut detail = screen
        .selected()
        .map(|p| {
            format!(
                "{}\n{} / {}〜{} JST\n{} / 再生可否未確認\n出演: {}\n\n{}\n{}",
                p.title,
                p.key.station_id,
                local(p.key.starts_at).format("%m/%d %H:%M"),
                local(p.ends_at).format("%m/%d %H:%M"),
                status(p, now),
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
    if let Some(input) = &screen.input {
        frame.render_widget(
            Paragraph::new(format!(
                "キーワード: {input}\nEnter 検索 / Esc 取消（編集中は取得しません）"
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
        let footer = format!(
            "{} / {} / 不正枠{}\n再生中: なし / / 編集・Enter検索 / r 再取得 / j k 選択 / q 終了\n{} {}",
            state,
            progress,
            screen.app.invalid_count,
            screen.app.notification,
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
    let result = {
        let _restore = RestoreTerminal;
        let mut terminal = ratatui::try_init()?;
        let mut screen = Screen {
            app: AppState::new(criteria),
            list: ListState::default(),
            input: None,
            preferences_notice,
        };
        let rt = runtime.0.as_ref().ok_or("実行環境を開始できません")?;
        rt.block_on(async {
            let mut service = Service::new(CatalogClient::new);
            screen.submit(&mut service)?;
            let mut tick = tokio::time::interval(Duration::from_millis(50));
            loop {
                if shutdown.load(Ordering::Acquire) {
                    break;
                }
                for event in service.poll().await {
                    screen.apply(event);
                }
                terminal.draw(|frame| draw(frame, &mut screen, Utc::now().fixed_offset()))?;
                let mut quit = false;
                for _ in 0..32 {
                    if !event::poll(Duration::ZERO)? {
                        break;
                    }
                    if let Event::Key(key) = event::read()? {
                        if screen.key(key, &mut service)? {
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
            Ok::<_, Box<dyn std::error::Error>>(screen.app.committed.clone())
        })
    };
    if let Ok(criteria) = &result {
        if let Some(store) = store {
            if let Err(error) = store.save_last(criteria.clone()) {
                eprintln!("条件保存失敗: {error}");
            }
        }
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
            app: AppState::new(criteria),
            list: ListState::default(),
            input: None,
            preferences_notice: String::new(),
        }
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
            "閲覧専用",
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
