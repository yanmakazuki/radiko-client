//! 検索とライブ／タイムフリー再生のTUI。描画処理は状態を参照するだけで、通信や再生を実行しない。
macro_rules! quit_key_wording {
    () => {
        "通常画面のq / Esc、またはCtrl+Cで終了"
    };
}
macro_rules! logout_key_wording {
    () => {
        "ログアウトはyで確定、n / Esc / qで取消"
    };
}
macro_rules! dialog_close_hint {
    () => {
        "q / Esc 閉じる"
    };
}
mod areas;
mod draw;
mod failures;
mod filters;
mod help;
mod login;
mod named;
mod offline;
mod picker;
mod screen;
mod scroll;
mod seek;
mod viewport;
use crate::stdout::stdout_ln;
use chrono::{DateTime, FixedOffset, Utc};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
#[cfg(test)]
use draw::preview;
use draw::{draw, local};
use radiko::{
    api::catalog::CatalogClient,
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
/// 番組一覧の終了キー。画面内ヘルプと `tui --help` は同じ句を使う。
#[cfg(test)]
const QUIT_KEY_WORDING: &str = quit_key_wording!();
/// ログアウト確認。どのダイアログでも `q` は閉じるだけで、ここでは取消でありログアウトしない。
#[cfg(test)]
const LOGOUT_KEY_WORDING: &str = logout_key_wording!();
#[cfg(test)]
const DIALOG_CLOSE_HINT: &str = dialog_close_hint!();
const USAGE: &str = concat!(
    "tui [programsと同じ局・地域・期間・絞り込み]\n/ キーワード編集 / f 全条件編集 / b 名前付き条件 / Enter 検索確定 / Esc 編集取消 / j k・上下 選択 / PageUp・PageDown ページ / Home・End 先頭・末尾 / e 取得状況 / r 再取得 / ",
    quit_key_wording!(),
    "\nEnter TF再生 / v ライブ画面 / l ライブ / g 秒数指定 / Space 一時停止・再開 / s 停止 / 左右 15秒シーク / + - 音量\n引数省略は前回条件を当日1日に復元。再生は保存済みログインを使用。起動時にセッションを確認し、aで再確認。I ログイン / O ",
    logout_key_wording!(),
    "。c チャプター閲覧 / 一覧のr 再取得・Esc 詳細へ。ライブ画面のEnterはライブ再生、vで戻ります。チャプターのEnterは原点を照合できた枠のみ指定位置から再生します。"
);
fn named_receipt(result: &Result<NamedOutcome, radiko::app::named::Error>) -> String {
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
/// How long a catch-up or auth notice keeps a later outage from replacing it.
///
/// Five seconds is long enough to read the line. After that, a new outage shows
/// 「受信待ち…」. Confirmed stops are not on this timer.
const NOTICE_HOLD: std::time::Duration = std::time::Duration::from_secs(5);

/// Why `playback_notice` is set. Gap text is cleared from this, not from the wording.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NoticeHold {
    Other,
    Receiving,
    Reconnecting,
    CatchUp,
    AuthRenewal,
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
    notice_hold: NoticeHold,
    /// Catch-up and auth text stay until this instant. Gap notices wait.
    notice_hold_until: Option<std::time::Instant>,
    /// Set from a failed playback update. Not inferred from the notice text.
    playback_stopped: bool,
    /// The control session ended (program end, stop, or failure). Gap notices stay off.
    playback_ended: bool,
    seen_reauthentications: u64,
    seen_skipped_segments: u64,
    seen_sequence_resets: u64,
    playback_diagnostics: String,
    pipeline_diagnostics: String,
    volume: u32,
    app: AppState,
    list: ListState,
    list_height: usize,
    input: Option<String>,
    preferences_notice: String,
    offline: bool,
}
fn saved_criteria(
    explicit: Option<SearchCriteria>,
    data_dir: Option<PathBuf>,
    now: DateTime<FixedOffset>,
) -> Result<(SearchCriteria, Option<SearchStore>, String), Box<dyn std::error::Error>> {
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
    Ok((criteria, store, preferences_notice))
}
struct Services<'a> {
    named: &'a mut NamedService,
    login: &'a mut LoginService,
    logout: &'a mut LogoutService,
    authentication: &'a mut AuthService,
    search: &'a mut Service,
    chapters: &'a mut ChapterService,
    player: &'a mut PlaybackService,
}
async fn poll_services(screen: &mut Screen, jobs: &mut Services<'_>) -> Result<(), &'static str> {
    if let Some(event) = jobs.named.poll().await {
        screen.named_event(event)?;
    }
    screen.dispatch_named(jobs.named);
    if let Some(event) = jobs.login.poll().await {
        screen.app.authentication.apply(event);
    }
    if let Some(event) = jobs.logout.poll().await {
        screen.app.authentication.apply(event);
    }
    if let Some(event) = jobs.authentication.poll().await {
        screen.app.authentication.apply(event);
    }
    for event in jobs.search.poll().await {
        screen.apply(event);
    }
    if let Some(event) = jobs.chapters.poll().await {
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
    screen.apply_playback(jobs.player.poll());
    screen.refresh_live(Utc::now().fixed_offset());
    Ok(())
}
pub fn run(args: &[String], data_dir: Option<PathBuf>) -> Result<(), Box<dyn std::error::Error>> {
    let (offline, args) = offline::split_args(args);
    if args == ["--help"] {
        stdout_ln!("{USAGE}");
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
        Some(crate::programs::criteria(&args, now)?)
    };
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err("tuiは対話端末で実行してください".into());
    }
    let playback_dir = data_dir.clone();
    let auth_dir = data_dir.clone();
    let named_dir = data_dir.clone();
    // Offline mode never opens session.json, searches.json, or the default data directory.
    let (criteria, store, preferences_notice) = if offline {
        (
            match explicit {
                Some(criteria) => criteria,
                None => SearchCriteria::initial(now)?,
            },
            None,
            String::new(),
        )
    } else {
        saved_criteria(explicit, data_dir, now)?
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
        let mut screen = Screen::new(criteria, preferences_notice);
        if offline {
            screen.offline = true;
            if !screen.preferences_notice.is_empty() {
                screen.preferences_notice.push('\n');
            }
            screen.preferences_notice.push_str(offline::NOTICE);
        }
        let rt = runtime.0.as_ref().ok_or("実行環境を開始できません")?;
        rt.block_on(async {
            let mut service = if offline {
                Service::new(|| Ok(offline::Fixture))
            } else {
                Service::new_cancellable(CatalogClient::new_cancellable)
            };
            let mut player = if offline {
                PlaybackService::with_factory(|_| {
                    Err(radiko::player::playback::PlaybackError::Source(
                        radiko::api::stream::ProbeError::AvailabilityUnknown,
                    ))
                })
            } else {
                PlaybackService::new(playback_dir)
            };
            let mut chapters = if offline {
                ChapterService::with_cancellable_source(|_, _| {
                    Err(radiko::api::stream::chapters::Error::Cancelled)
                })
            } else {
                ChapterService::default()
            };
            let mut authentication = if offline {
                AuthService::with_cancellable_check(|_| AuthStatus::NotChecked)
            } else {
                AuthService::new(auth_dir.clone())
            };
            let mut login_service = if offline {
                LoginService::with_login(|_| {
                    AuthStatus::LoginFailed(radiko::api::auth::AuthError::Cancelled)
                })
            } else {
                LoginService::new(auth_dir.clone())
            };
            let mut logout = if offline {
                LogoutService::with_delete(|| Ok(()))
            } else {
                LogoutService::new(auth_dir)
            };
            let mut named_service = if offline {
                NamedService::with_store(|_| Err(radiko::storage::searches::Error::NotFound))
            } else {
                NamedService::new(named_dir)
            };
            authentication.submit(screen.app.authentication.begin()?);
            screen.submit(&mut service)?;
            let mut tick = tokio::time::interval(Duration::from_millis(50));
            loop {
                if shutdown.load(Ordering::Acquire) {
                    named_write_pending = named_service.writing();
                    break;
                }
                poll_services(
                    &mut screen,
                    &mut Services {
                        named: &mut named_service,
                        login: &mut login_service,
                        logout: &mut logout,
                        authentication: &mut authentication,
                        search: &mut service,
                        chapters: &mut chapters,
                        player: &mut player,
                    },
                )
                .await?;
                terminal.draw(|frame| draw(frame, &mut screen, Utc::now().fixed_offset()))?;
                let mut quit = false;
                for _ in 0..32 {
                    if !event::poll(Duration::ZERO)? {
                        break;
                    }
                    if let Event::Key(key) = event::read()? {
                        let should_quit = screen.key(key, &mut service)?;
                        screen.dispatch_named(&mut named_service);
                        screen.dispatch_requested(
                            &mut player,
                            &mut authentication,
                            &mut login_service,
                            &mut logout,
                            &mut chapters,
                        )?;
                        if should_quit {
                            player.release_output();
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
            player.release_output();
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
mod tests;
