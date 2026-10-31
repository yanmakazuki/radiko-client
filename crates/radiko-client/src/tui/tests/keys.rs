use super::*;
#[test]
fn help_never_dispatches_search_authentication_or_playback_and_closes_without_quitting() {
    let mut screen = screen();
    let mut search = Service::new(|| -> Result<CatalogClient, _> { panic!("help must not fetch") });
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
        assert!(text.replace(' ', "").contains("q/Esc閉じる"));
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Surface {
    MainList,
    Live,
    Help,
    FetchStatus,
    Chapters,
    LogoutConfirm,
    Named,
    NamedBusy,
    Keyword,
    Filters,
    Login,
    Seconds,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KeyEffect {
    Close,
    Quit,
    Cancel,
    /// 通知がない通常画面。終了案内を出す。終了しない。
    Hint,
    /// 文字として入る。
    Insert,
    /// 秒数欄は数字以外を捨てる。終了も閉じもしない。
    Ignored,
    /// 未実装のまま。画面は変わらない。
    None,
}
#[derive(Clone, Copy, Debug)]
struct KeyCase {
    surface: Surface,
    code: KeyCode,
    modifiers: KeyModifiers,
    effect: KeyEffect,
}
fn case(surface: Surface, code: KeyCode, modifiers: KeyModifiers, effect: KeyEffect) -> KeyCase {
    KeyCase {
        surface,
        code,
        modifiers,
        effect,
    }
}
fn quit_keys(surface: Surface) -> Vec<KeyCase> {
    vec![
        case(
            surface,
            KeyCode::Char('q'),
            KeyModifiers::NONE,
            KeyEffect::Quit,
        ),
        case(
            surface,
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
            KeyEffect::Quit,
        ),
    ]
}
fn close_keys(surface: Surface) -> Vec<KeyCase> {
    vec![
        case(
            surface,
            KeyCode::Char('q'),
            KeyModifiers::NONE,
            KeyEffect::Close,
        ),
        case(surface, KeyCode::Esc, KeyModifiers::NONE, KeyEffect::Close),
        case(
            surface,
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
            KeyEffect::Quit,
        ),
    ]
}
fn text_keys(surface: Surface) -> Vec<KeyCase> {
    vec![
        case(
            surface,
            KeyCode::Char('q'),
            KeyModifiers::NONE,
            KeyEffect::Insert,
        ),
        case(surface, KeyCode::Esc, KeyModifiers::NONE, KeyEffect::Cancel),
        case(
            surface,
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
            KeyEffect::Quit,
        ),
    ]
}
fn key_cases() -> Vec<KeyCase> {
    let mut cases = Vec::new();
    for surface in [Surface::MainList, Surface::Live] {
        cases.extend(quit_keys(surface));
        // 通知がなければ Esc は終了案内を出す。終了しない。
        cases.push(case(
            surface,
            KeyCode::Esc,
            KeyModifiers::NONE,
            KeyEffect::Hint,
        ));
        // #22 の n / o と #23 の = は未実装。一覧とライブでは何もしない。
        for code in [KeyCode::Char('n'), KeyCode::Char('o'), KeyCode::Char('=')] {
            cases.push(case(surface, code, KeyModifiers::NONE, KeyEffect::None));
        }
    }
    cases.extend(close_keys(Surface::Help));
    cases.extend(close_keys(Surface::FetchStatus));
    // チャプター表示の q と Esc は閉じる。終了しない。
    cases.extend(close_keys(Surface::Chapters));
    cases.push(case(
        Surface::LogoutConfirm,
        KeyCode::Char('q'),
        KeyModifiers::NONE,
        KeyEffect::Cancel,
    ));
    cases.push(case(
        Surface::LogoutConfirm,
        KeyCode::Esc,
        KeyModifiers::NONE,
        KeyEffect::Cancel,
    ));
    cases.push(case(
        Surface::LogoutConfirm,
        KeyCode::Char('c'),
        KeyModifiers::CONTROL,
        KeyEffect::Quit,
    ));
    cases.extend(close_keys(Surface::Named));
    cases.extend(close_keys(Surface::NamedBusy));
    cases.extend(text_keys(Surface::Keyword));
    cases.extend(text_keys(Surface::Filters));
    cases.extend(text_keys(Surface::Login));
    cases.push(case(
        Surface::Seconds,
        KeyCode::Char('q'),
        KeyModifiers::NONE,
        KeyEffect::Ignored,
    ));
    cases.push(case(
        Surface::Seconds,
        KeyCode::Esc,
        KeyModifiers::NONE,
        KeyEffect::Cancel,
    ));
    cases.push(case(
        Surface::Seconds,
        KeyCode::Char('c'),
        KeyModifiers::CONTROL,
        KeyEffect::Quit,
    ));
    cases
}
fn program() -> Program {
    radiko::api::catalog::parse_programs(
        include_bytes!("../../../../../tests/fixtures/schedule.xml"),
        "TBS",
        radiko::api::catalog::parse_day("20261015").unwrap(),
    )
    .unwrap()
    .programs
    .remove(0)
}
fn timefree_snapshot() -> radiko::player::state::PlaybackSnapshot {
    let start = chrono::DateTime::parse_from_rfc3339("2026-10-16T12:00:00+09:00").unwrap();
    radiko::player::state::PlaybackSnapshot {
        pipeline: None,
        info: radiko::api::stream::playback::StreamInfo {
            station: "TBS".into(),
            title: None,
            mode: radiko::api::stream::ProbeMode::TimeFree,
            starts_at: start,
            ends_at: start + chrono::Duration::hours(1),
            position: Duration::ZERO,
        },
        state: PlaybackState::Playing,
        pending: None,
        volume: 25,
        position: Some(Duration::from_secs(25)),
        output: None,
        segments: 1,
        receiving_for: None,
        reconnecting_for: None,
    }
}
fn prepare(surface: Surface, search: &mut Service) -> Screen {
    let mut screen = screen();
    match surface {
        Surface::MainList => {}
        Surface::Live => screen.live_mode = true,
        Surface::Help => screen.help_open = true,
        Surface::FetchStatus => screen.failures_open = true,
        Surface::Chapters => screen.open_chapters(program()).unwrap(),
        Surface::LogoutConfirm => screen.logout_confirm = true,
        Surface::Named => {
            let mut manager = named::Manager::new(ViewId(1));
            manager.listed(Default::default());
            screen.named = Some(manager);
        }
        Surface::NamedBusy => screen.named = Some(named::Manager::new(ViewId(1))),
        Surface::Keyword => {
            screen
                .key(
                    KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE),
                    search,
                )
                .unwrap();
        }
        Surface::Filters => {
            screen
                .key(
                    KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE),
                    search,
                )
                .unwrap();
            for _ in 0..5 {
                screen
                    .key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), search)
                    .unwrap();
            }
        }
        Surface::Login => {
            screen
                .key(
                    KeyEvent::new(KeyCode::Char('I'), KeyModifiers::NONE),
                    search,
                )
                .unwrap();
        }
        Surface::Seconds => {
            screen.app.playback = Some(timefree_snapshot());
            screen
                .key(
                    KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE),
                    search,
                )
                .unwrap();
        }
    }
    screen
}
#[test]
fn surface_and_quit_keys_follow_one_table() {
    let mut search =
        Service::new(|| -> Result<CatalogClient, _> { panic!("key table must not fetch") });
    let cases = key_cases();
    for case in &cases {
        let mut screen = prepare(case.surface, &mut search);
        let committed = screen.app.committed.clone();
        let selected = screen.list.selected();
        let volume = screen.volume;
        let quit = screen
            .key(KeyEvent::new(case.code, case.modifiers), &mut search)
            .unwrap();
        assert!(screen.playback_action.is_none(), "{case:?}");
        assert!(!screen.logout_requested, "{case:?}");
        assert!(screen.login_requested.is_none(), "{case:?}");
        assert!(screen.named_action.is_none(), "{case:?}");
        assert_eq!(screen.app.committed, committed, "{case:?}");
        assert_eq!(screen.app.status, SearchStatus::Idle, "{case:?}");
        assert_eq!(screen.list.selected(), selected, "{case:?}");
        assert_eq!(screen.volume, volume, "{case:?}");
        match case.effect {
            KeyEffect::Quit => {
                assert!(quit, "{case:?}");
                match case.surface {
                    Surface::Help => assert!(screen.help_open, "{case:?}"),
                    Surface::FetchStatus => assert!(screen.failures_open, "{case:?}"),
                    Surface::LogoutConfirm => assert!(screen.logout_confirm, "{case:?}"),
                    Surface::Chapters => {
                        assert!(screen.app.chapters.target.is_some(), "{case:?}")
                    }
                    Surface::Named | Surface::NamedBusy => {
                        assert!(screen.named.is_some(), "{case:?}")
                    }
                    Surface::Keyword => assert!(screen.input.is_some(), "{case:?}"),
                    Surface::Filters => assert!(screen.filters.is_some(), "{case:?}"),
                    Surface::Login => assert!(screen.login.is_some(), "{case:?}"),
                    Surface::Seconds => assert!(screen.seek.is_some(), "{case:?}"),
                    Surface::MainList => assert!(!screen.live_mode, "{case:?}"),
                    Surface::Live => assert!(screen.live_mode, "{case:?}"),
                }
            }
            KeyEffect::Close => {
                assert!(!quit, "{case:?}");
                match case.surface {
                    Surface::Help => assert!(!screen.help_open, "{case:?}"),
                    Surface::FetchStatus => assert!(!screen.failures_open, "{case:?}"),
                    Surface::Chapters => {
                        assert!(screen.app.chapters.target.is_none(), "{case:?}")
                    }
                    Surface::Named | Surface::NamedBusy => {
                        assert!(screen.named.is_none(), "{case:?}")
                    }
                    Surface::MainList
                    | Surface::Live
                    | Surface::LogoutConfirm
                    | Surface::Keyword
                    | Surface::Filters
                    | Surface::Login
                    | Surface::Seconds => panic!("閉じるのはダイアログだけです: {case:?}"),
                }
            }
            KeyEffect::Cancel => {
                assert!(!quit, "{case:?}");
                match case.surface {
                    Surface::LogoutConfirm => assert!(!screen.logout_confirm, "{case:?}"),
                    Surface::Keyword => {
                        assert!(screen.input.is_none(), "{case:?}");
                        assert!(screen.app.draft.is_none(), "{case:?}");
                    }
                    Surface::Filters => {
                        assert!(screen.filters.is_none(), "{case:?}");
                        assert!(screen.app.draft.is_none(), "{case:?}");
                    }
                    Surface::Login => assert!(screen.login.is_none(), "{case:?}"),
                    Surface::Seconds => assert!(screen.seek.is_none(), "{case:?}"),
                    Surface::MainList
                    | Surface::Live
                    | Surface::Help
                    | Surface::FetchStatus
                    | Surface::Chapters
                    | Surface::Named
                    | Surface::NamedBusy => panic!("取消ではない画面です: {case:?}"),
                }
            }
            KeyEffect::Insert => {
                assert!(!quit, "{case:?}");
                match case.surface {
                    Surface::Keyword => {
                        assert_eq!(screen.input.as_deref(), Some("q"), "{case:?}")
                    }
                    Surface::Filters => assert_eq!(
                        screen.app.draft.as_ref().unwrap().filter().keyword(),
                        Some("q"),
                        "{case:?}"
                    ),
                    Surface::Login => {
                        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
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
                        assert!(
                            text.replace(' ', "").contains("メールアドレス:q"),
                            "{case:?}"
                        );
                    }
                    _ => panic!("文字入力ではない画面です: {case:?}"),
                }
            }
            KeyEffect::Ignored => {
                assert!(!quit, "{case:?}");
                assert_eq!(case.surface, Surface::Seconds, "{case:?}");
                assert_eq!(screen.seek.as_ref().unwrap().input, "", "{case:?}");
            }
            KeyEffect::Hint => {
                assert!(!quit, "{case:?}");
                assert_eq!(screen.playback_notice, QUIT_HINT, "{case:?}");
                assert!(screen.playback_action.is_none(), "{case:?}");
                assert_eq!(
                    screen.live_mode,
                    matches!(case.surface, Surface::Live),
                    "{case:?}"
                );
            }
            KeyEffect::None => {
                assert!(!quit, "{case:?}");
                assert!(
                    screen.input.is_none() && screen.filters.is_none(),
                    "{case:?}"
                );
                assert!(screen.named.is_none() && screen.login.is_none(), "{case:?}");
                assert!(screen.seek.is_none(), "{case:?}");
                assert!(!screen.help_open && !screen.failures_open, "{case:?}");
                assert!(!screen.logout_confirm, "{case:?}");
                assert_eq!(
                    screen.live_mode,
                    matches!(case.surface, Surface::Live),
                    "{case:?}"
                );
            }
        }
    }
    assert!(help::KEYS.contains(QUIT_KEY_WORDING));
    assert!(USAGE.contains(QUIT_KEY_WORDING));
    assert!(help::KEYS.contains(DISMISS_NOTICE_WORDING));
    assert!(USAGE.contains(DISMISS_NOTICE_WORDING));
    assert!(!QUIT_KEY_WORDING.contains("Esc"));
    assert!(help::KEYS.contains(LOGOUT_KEY_WORDING));
    assert!(USAGE.contains(LOGOUT_KEY_WORDING));
    assert!(LOGOUT_KEY_WORDING.contains("n / Esc / qで取消"));
    for surface in [Surface::MainList, Surface::Live] {
        let tokens: Vec<_> = cases
            .iter()
            .filter(|case| case.surface == surface && case.effect == KeyEffect::Quit)
            .map(|case| match (case.code, case.modifiers) {
                (KeyCode::Char('q'), KeyModifiers::NONE) => "q",
                (KeyCode::Char('c'), KeyModifiers::CONTROL) => "Ctrl+C",
                _ => panic!("終了キーが表にありません: {case:?}"),
            })
            .collect();
        assert_eq!(tokens, ["q", "Ctrl+C"]);
        for token in tokens {
            assert!(QUIT_KEY_WORDING.contains(token), "{token}");
        }
        let hint = prepare(surface, &mut search).control_hint();
        assert!(hint.contains("Esc 通知を消す"), "{hint}");
        assert!(hint.contains("q・Ctrl+C 終了"), "{hint}");
        assert!(!hint.contains("q・Esc"), "{hint}");
        assert!(!hint.contains("Esc・Ctrl+C"), "{hint}");
    }
    let logout_q = cases.iter().find(|case| {
        case.surface == Surface::LogoutConfirm
            && case.code == KeyCode::Char('q')
            && case.modifiers == KeyModifiers::NONE
    });
    assert!(matches!(
        logout_q.map(|case| case.effect),
        Some(KeyEffect::Cancel)
    ));
    let mut help = prepare(Surface::Help, &mut search);
    let mut failures = prepare(Surface::FetchStatus, &mut search);
    let logout = prepare(Surface::LogoutConfirm, &mut search);
    let named_screen = prepare(Surface::Named, &mut search);
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    for screen in [&mut help, &mut failures] {
        terminal
            .draw(|frame| draw(frame, screen, Utc::now().fixed_offset()))
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.replace(' ', "").contains("q/Esc閉じる"), "{text}");
    }
    assert!(logout.control_hint().contains(DIALOG_CLOSE_HINT));
    assert!(
        named_screen
            .named
            .as_ref()
            .unwrap()
            .controls()
            .contains(DIALOG_CLOSE_HINT)
    );
    let busy = named::Manager::new(ViewId(2));
    assert!(busy.controls().contains(DIALOG_CLOSE_HINT));
    let chapters = prepare(Surface::Chapters, &mut search);
    let hint = chapters.control_hint();
    assert!(hint.contains("q・Esc 詳細へ"), "{hint}");
    assert!(hint.contains("Ctrl+C 終了"), "{hint}");
    assert!(!hint.contains("Esc 通知を消す"), "{hint}");
    assert!(!hint.contains("q・Esc・Ctrl+C"), "{hint}");
}
fn press(
    screen: &mut Screen,
    search: &mut Service,
    code: KeyCode,
    modifiers: KeyModifiers,
) -> bool {
    screen.key(KeyEvent::new(code, modifiers), search).unwrap()
}
#[test]
fn escape_on_program_and_live_lists_returns_false_and_q_and_ctrl_c_return_true() {
    let mut search =
        Service::new(|| -> Result<CatalogClient, _> { panic!("quit keys must not fetch") });
    for surface in [Surface::MainList, Surface::Live] {
        let mut screen = prepare(surface, &mut search);
        assert!(!press(
            &mut screen,
            &mut search,
            KeyCode::Esc,
            KeyModifiers::NONE
        ));
        let mut screen = prepare(surface, &mut search);
        assert!(press(
            &mut screen,
            &mut search,
            KeyCode::Char('q'),
            KeyModifiers::NONE
        ));
        let mut screen = prepare(surface, &mut search);
        assert!(press(
            &mut screen,
            &mut search,
            KeyCode::Char('c'),
            KeyModifiers::CONTROL
        ));
    }
}
#[test]
fn escape_during_playback_dismisses_notices_without_stopping_audio() {
    let mut search =
        Service::new(|| -> Result<CatalogClient, _> { panic!("escape must not fetch") });
    for live in [false, true] {
        let mut screen = screen();
        screen.live_mode = live;
        screen.app.playback = Some(timefree_snapshot());
        screen.app.notification = "検索に失敗しました".into();
        screen.preferences_notice = "「朝」を保存しました".into();
        screen.show_notice("シーク不可");
        screen.list.select(Some(2));
        screen.volume = 40;
        let committed = screen.app.committed.clone();
        assert!(!press(
            &mut screen,
            &mut search,
            KeyCode::Esc,
            KeyModifiers::NONE
        ));
        assert!(screen.playback_action.is_none());
        assert!(!screen.playback_stopped);
        assert!(!screen.playback_ended);
        let playback = screen.app.playback.as_ref().unwrap();
        assert_eq!(playback.state, PlaybackState::Playing);
        assert_eq!(playback.position, Some(Duration::from_secs(25)));
        assert_eq!(playback.volume, 25);
        assert!(screen.playback_notice.is_empty(), "{live}");
        assert_ne!(screen.playback_notice, QUIT_HINT, "{live}");
        assert!(screen.app.notification.is_empty());
        assert!(screen.preferences_notice.is_empty());
        assert_eq!(screen.notice_hold, NoticeHold::Other);
        assert!(screen.notice_hold_until.is_none());
        assert_eq!(screen.list.selected(), Some(2));
        assert_eq!(screen.volume, 40);
        assert_eq!(screen.app.committed, committed);
        assert!(screen.app.draft.is_none());
        assert!(screen.filters.is_none());
        assert_eq!(screen.live_mode, live);
        assert!(!press(
            &mut screen,
            &mut search,
            KeyCode::Esc,
            KeyModifiers::NONE
        ));
        assert!(screen.playback_action.is_none());
        assert_eq!(
            screen.app.playback.as_ref().unwrap().state,
            PlaybackState::Playing
        );
        assert_eq!(
            screen.app.playback.as_ref().unwrap().position,
            Some(Duration::from_secs(25))
        );
        assert_eq!(screen.playback_notice, QUIT_HINT);
        let mut paused = screen;
        let mut snapshot = timefree_snapshot();
        snapshot.state = PlaybackState::Paused;
        paused.app.playback = Some(snapshot);
        paused.playback_notice = "一時停止".into();
        assert!(!press(
            &mut paused,
            &mut search,
            KeyCode::Esc,
            KeyModifiers::NONE
        ));
        assert!(paused.playback_action.is_none());
        assert!(paused.playback_notice.is_empty());
        assert_ne!(paused.playback_notice, QUIT_HINT);
        assert_eq!(
            paused.app.playback.as_ref().unwrap().state,
            PlaybackState::Paused
        );
        assert_eq!(paused.list.selected(), Some(2));
        assert_eq!(paused.app.committed, committed);
    }
}
#[test]
fn escape_dismisses_stop_notices_without_resuming_or_clearing_the_stop() {
    let mut search =
        Service::new(|| -> Result<CatalogClient, _> { panic!("escape must not fetch") });
    let sentences = [
        "認証期限切れのため再生を停止しました。aで再確認するか、loginで再ログインしてください",
        "再接続の上限を超えたため再生を停止しました。番組を選び直して再生してください",
    ];
    for sentence in sentences {
        let mut screen = screen();
        screen.playback_stopped = true;
        screen.app.playback = None;
        screen.playback_notice = sentence.into();
        screen.list.select(Some(1));
        let committed = screen.app.committed.clone();
        assert!(!press(
            &mut screen,
            &mut search,
            KeyCode::Esc,
            KeyModifiers::NONE
        ));
        assert!(screen.playback_notice.is_empty());
        assert_ne!(screen.playback_notice, QUIT_HINT);
        assert!(screen.playback_stopped);
        assert!(screen.app.playback.is_none());
        assert!(screen.playback_action.is_none());
        assert_eq!(screen.list.selected(), Some(1));
        assert_eq!(screen.app.committed, committed);
        assert!(screen.filters.is_none());
    }
}
#[test]
fn escape_twice_in_chapter_view_returns_to_details_without_quitting() {
    let mut search =
        Service::new(|| -> Result<CatalogClient, _> { panic!("chapters must not fetch") });
    let mut screen = prepare(Surface::Chapters, &mut search);
    screen.app.playback = Some(timefree_snapshot());
    screen.playback_notice = "再生準備中".into();
    screen.list.select(Some(0));
    let committed = screen.app.committed.clone();
    assert!(screen.app.chapters.target.is_some());
    assert!(!press(
        &mut screen,
        &mut search,
        KeyCode::Esc,
        KeyModifiers::NONE
    ));
    assert!(screen.app.chapters.target.is_none());
    assert_eq!(screen.playback_notice, "再生準備中");
    assert!(screen.playback_action.is_none());
    assert_eq!(
        screen.app.playback.as_ref().unwrap().state,
        PlaybackState::Playing
    );
    assert!(!press(
        &mut screen,
        &mut search,
        KeyCode::Esc,
        KeyModifiers::NONE
    ));
    assert!(screen.app.chapters.target.is_none());
    assert!(screen.playback_notice.is_empty());
    assert!(screen.playback_action.is_none());
    assert!(!screen.playback_stopped);
    assert_eq!(
        screen.app.playback.as_ref().unwrap().state,
        PlaybackState::Playing
    );
    assert_eq!(
        screen.app.playback.as_ref().unwrap().position,
        Some(Duration::from_secs(25))
    );
    assert_eq!(screen.list.selected(), Some(0));
    assert_eq!(screen.app.committed, committed);
    assert!(screen.filters.is_none());
    assert!(screen.app.draft.is_none());
}
#[test]
fn escape_without_a_notice_shows_the_quit_hint_and_the_next_escape_clears_it() {
    let mut search =
        Service::new(|| -> Result<CatalogClient, _> { panic!("escape must not fetch") });
    for live in [false, true] {
        for paused in [false, true] {
            let mut screen = screen();
            screen.live_mode = live;
            let mut snapshot = timefree_snapshot();
            if paused {
                snapshot.state = PlaybackState::Paused;
            }
            let state = snapshot.state;
            screen.app.playback = Some(snapshot);
            screen.list.select(Some(3));
            let committed = screen.app.committed.clone();
            assert!(screen.playback_notice.is_empty());
            assert!(!press(
                &mut screen,
                &mut search,
                KeyCode::Esc,
                KeyModifiers::NONE
            ));
            assert_eq!(screen.playback_notice, QUIT_HINT);
            assert_eq!(screen.notice_hold, NoticeHold::Other);
            assert!(screen.notice_hold_until.is_none());
            assert!(screen.playback_action.is_none());
            assert_eq!(screen.app.playback.as_ref().unwrap().state, state);
            assert_eq!(
                screen.app.playback.as_ref().unwrap().position,
                Some(Duration::from_secs(25))
            );
            assert_eq!(screen.list.selected(), Some(3));
            assert_eq!(screen.app.committed, committed);
            assert!(screen.filters.is_none());
            assert!(!press(
                &mut screen,
                &mut search,
                KeyCode::Esc,
                KeyModifiers::NONE
            ));
            assert!(screen.playback_notice.is_empty());
            assert!(screen.playback_action.is_none());
            assert_eq!(screen.app.playback.as_ref().unwrap().state, state);
            assert_eq!(
                screen.app.playback.as_ref().unwrap().position,
                Some(Duration::from_secs(25))
            );
            assert_eq!(screen.list.selected(), Some(3));
            assert_eq!(screen.app.committed, committed);
        }
    }
}
#[test]
fn quit_hint_does_not_hold_and_yields_immediately_to_reception_and_reconnect() {
    let mut search =
        Service::new(|| -> Result<CatalogClient, _> { panic!("escape must not fetch") });
    let mut screen = super::screen();
    screen.app.playback = Some(timefree_snapshot());
    assert!(!press(
        &mut screen,
        &mut search,
        KeyCode::Esc,
        KeyModifiers::NONE
    ));
    assert_eq!(screen.playback_notice, QUIT_HINT);
    assert_eq!(screen.notice_hold, NoticeHold::Other);
    assert!(screen.notice_hold_until.is_none());
    let mut receiving = timefree_snapshot();
    receiving.receiving_for = Some(Duration::from_secs(2));
    screen.apply_playback(playback::Update {
        snapshot: Some(receiving),
        ..playback::Update::default()
    });
    assert_eq!(
        screen.playback_notice,
        radiko::player::playback::RECEPTION_NOTICE
    );
    assert_ne!(screen.playback_notice, QUIT_HINT);
    assert_eq!(
        screen.app.playback.as_ref().unwrap().state,
        PlaybackState::Playing
    );

    let mut reconnect_screen = super::screen();
    assert!(!press(
        &mut reconnect_screen,
        &mut search,
        KeyCode::Esc,
        KeyModifiers::NONE
    ));
    assert_eq!(reconnect_screen.playback_notice, QUIT_HINT);
    let mut reconnecting = timefree_snapshot();
    reconnecting.reconnecting_for = Some(Duration::from_secs(30));
    reconnect_screen.apply_playback(playback::Update {
        snapshot: Some(reconnecting),
        ..playback::Update::default()
    });
    assert_eq!(reconnect_screen.playback_notice, "再接続中…（30秒）");
    assert_ne!(reconnect_screen.playback_notice, QUIT_HINT);
    assert_eq!(
        reconnect_screen.app.playback.as_ref().unwrap().state,
        PlaybackState::Playing
    );
    assert!(reconnect_screen.playback_action.is_none());
}
#[test]
fn escape_does_not_replace_stop_reception_or_reconnect_with_the_quit_hint() {
    let mut search =
        Service::new(|| -> Result<CatalogClient, _> { panic!("escape must not fetch") });
    let reconnect = radiko::player::playback::reconnect_notice(Duration::from_secs(30));
    assert_eq!(reconnect, "再接続中…（30秒）");
    let protected = [
        "認証期限切れのため再生を停止しました。aで再確認するか、loginで再ログインしてください",
        "再接続の上限を超えたため再生を停止しました。番組を選び直して再生してください",
        radiko::player::playback::RECEPTION_NOTICE,
        reconnect.as_str(),
    ];
    for (index, notice) in protected.into_iter().enumerate() {
        let stopped = index < 2;
        let mut screen = screen();
        screen.live_mode = index % 2 == 1;
        if stopped {
            screen.playback_stopped = true;
            screen.app.playback = None;
        } else {
            screen.app.playback = Some(timefree_snapshot());
        }
        screen.playback_notice = notice.into();
        screen.list.select(Some(4));
        let committed = screen.app.committed.clone();
        assert!(!press(
            &mut screen,
            &mut search,
            KeyCode::Esc,
            KeyModifiers::NONE
        ));
        assert!(screen.playback_notice.is_empty(), "{notice}");
        assert_ne!(screen.playback_notice, QUIT_HINT, "{notice}");
        assert!(screen.playback_action.is_none(), "{notice}");
        assert_eq!(screen.playback_stopped, stopped, "{notice}");
        assert_eq!(screen.list.selected(), Some(4), "{notice}");
        assert_eq!(screen.app.committed, committed, "{notice}");
        assert!(screen.filters.is_none(), "{notice}");
        if stopped {
            assert!(screen.app.playback.is_none(), "{notice}");
        } else {
            assert_eq!(
                screen.app.playback.as_ref().unwrap().state,
                PlaybackState::Playing,
                "{notice}"
            );
            assert_eq!(
                screen.app.playback.as_ref().unwrap().position,
                Some(Duration::from_secs(25)),
                "{notice}"
            );
        }
    }
}
fn drawn(screen: &mut Screen) -> String {
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    terminal
        .draw(|frame| draw(frame, screen, Utc::now().fixed_offset()))
        .unwrap();
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>()
        .replace(' ', "")
}
fn stop_with(screen: &mut Screen, notice: &str, reason: Option<playback::StopReason>) {
    screen.apply_playback(playback::Update {
        stopped: true,
        ended: true,
        stop_reason: reason,
        notice: Some(notice.to_owned()),
        ..playback::Update::default()
    });
}
#[test]
fn stopped_auth_and_reconnect_lines_stay_after_escape_until_playback_or_auth_succeeds() {
    let mut search =
        Service::new(|| -> Result<CatalogClient, _> { panic!("escape must not fetch") });
    let auth = radiko::api::stream::ProbeError::AuthExpired.to_string();
    let reconnect = radiko::player::playback::PlaybackError::ReconnectGaveUp.to_string();
    let auth_line = "停止中：認証期限切れ（aで再確認）";
    let reconnect_line = "停止中：再接続できませんでした（番組を選び直してください）";
    for (notice, line, auth_hint, reason) in [
        (
            auth.as_str(),
            auth_line,
            true,
            Some(playback::StopReason::AuthExpired),
        ),
        (
            reconnect.as_str(),
            reconnect_line,
            false,
            Some(playback::StopReason::ReconnectLimit),
        ),
    ] {
        let mut screen = super::screen();
        screen.app.authentication.status = AuthStatus::Verified(radiko::session::Membership {
            area_free: Some(true),
            time_free_30: Some(false),
        });
        stop_with(&mut screen, notice, reason);
        let before = drawn(&mut screen);
        assert!(before.contains(&notice.replace(' ', "")), "{before}");
        assert!(before.contains(line), "{before}");
        if auth_hint {
            assert!(before.contains("再生側で認証失敗"), "{before}");
            assert!(before.contains("a再確認"), "{before}");
        } else {
            assert!(!before.contains("再生側で認証失敗"), "{before}");
        }
        assert!(!press(
            &mut screen,
            &mut search,
            KeyCode::Esc,
            KeyModifiers::NONE
        ));
        assert!(screen.playback_notice.is_empty(), "{notice}");
        assert!(screen.playback_stopped, "{notice}");
        let after = drawn(&mut screen);
        assert!(after.contains(line), "{after}");
        assert!(
            !after.contains("認証期限切れのため再生を停止しました"),
            "{after}"
        );
        assert!(
            !after.contains("再接続の上限を超えたため再生を停止しました"),
            "{after}"
        );
        if auth_hint {
            assert!(after.contains("再生側で認証失敗"), "{after}");
        }
        assert!(!press(
            &mut screen,
            &mut search,
            KeyCode::Esc,
            KeyModifiers::NONE
        ));
        assert_eq!(screen.playback_notice, QUIT_HINT, "{notice}");
        let hinted = drawn(&mut screen);
        assert!(hinted.contains(line), "{hinted}");
        assert!(hinted.contains(&QUIT_HINT.replace(' ', "")), "{hinted}");
        if auth_hint {
            assert!(hinted.contains("再生側で認証失敗"), "{hinted}");
        }
    }

    let mut screen = super::screen();
    stop_with(&mut screen, &auth, Some(playback::StopReason::AuthExpired));
    screen.apply_playback(playback::Update {
        new_session: true,
        snapshot: Some(timefree_snapshot()),
        ..playback::Update::default()
    });
    let playing = drawn(&mut screen);
    assert!(!playing.contains("停止中："), "{playing}");
    assert!(playing.contains("再生中:"), "{playing}");

    let mut screen = super::screen();
    stop_with(
        &mut screen,
        &reconnect,
        Some(playback::StopReason::ReconnectLimit),
    );
    let id = screen.app.authentication.begin().unwrap();
    screen.apply_authentication(radiko::app::auth::Event {
        id,
        status: AuthStatus::Expired,
    });
    assert!(drawn(&mut screen).contains(reconnect_line));
    assert!(screen.playback_stopped);
    let id = screen.app.authentication.begin().unwrap();
    screen.apply_authentication(radiko::app::auth::Event {
        id,
        status: AuthStatus::Verified(radiko::session::Membership {
            area_free: Some(true),
            time_free_30: Some(false),
        }),
    });
    let cleared = drawn(&mut screen);
    assert!(!cleared.contains("停止中："), "{cleared}");
    assert!(cleared.contains("再生中:なし"), "{cleared}");
    assert!(screen.playback_stopped);

    let mut screen = super::screen();
    stop_with(&mut screen, &auth, Some(playback::StopReason::AuthExpired));
    let id = screen.app.authentication.begin_login().unwrap();
    screen.apply_authentication(radiko::app::auth::Event {
        id,
        status: AuthStatus::LoginFailed(radiko::api::auth::AuthError::InvalidCredentials),
    });
    assert!(drawn(&mut screen).contains(auth_line));
    let id = screen.app.authentication.begin_login().unwrap();
    screen.apply_authentication(radiko::app::auth::Event {
        id,
        status: AuthStatus::Verified(radiko::session::Membership {
            area_free: None,
            time_free_30: None,
        }),
    });
    assert!(!drawn(&mut screen).contains("停止中："));

    let mut screen = super::screen();
    let window = radiko::api::stream::ProbeError::WindowLost.to_string();
    stop_with(&mut screen, &window, None);
    let shown = drawn(&mut screen);
    assert!(shown.contains("再生中:なし"), "{shown}");
    assert!(!shown.contains("停止中："), "{shown}");
    assert!(shown.contains("通信が長く途切れたため"), "{shown}");
}
#[test]
fn the_same_stop_sentence_without_stopped_does_not_record_a_stop_cause() {
    let auth = radiko::api::stream::ProbeError::AuthExpired.to_string();
    let reconnect = radiko::player::playback::PlaybackError::ReconnectGaveUp.to_string();
    for (notice, reason) in [
        (auth.as_str(), playback::StopReason::AuthExpired),
        (reconnect.as_str(), playback::StopReason::ReconnectLimit),
    ] {
        let mut screen = super::screen();
        screen.apply_playback(playback::Update {
            stopped: false,
            notice: Some(notice.to_owned()),
            stop_reason: Some(reason),
            ..playback::Update::default()
        });
        assert!(screen.stop_cause.is_none(), "{notice}");
        assert!(!screen.playback_stopped, "{notice}");
        assert_eq!(screen.playback_notice, notice, "{notice}");
        let shown = drawn(&mut screen);
        assert!(!shown.contains("停止中："), "{shown}");
        assert!(!shown.contains("再生側で認証失敗"), "{shown}");

        screen.apply_playback(playback::Update {
            stopped: true,
            ended: true,
            notice: Some(notice.to_owned()),
            ..playback::Update::default()
        });
        assert!(screen.stop_cause.is_none(), "{notice}");
        assert!(screen.playback_stopped, "{notice}");
        assert!(!drawn(&mut screen).contains("停止中："), "{notice}");

        screen.apply_playback(playback::Update {
            stopped: true,
            ended: true,
            notice: Some(notice.to_owned()),
            stop_reason: Some(reason),
            ..playback::Update::default()
        });
        assert_eq!(
            screen.stop_cause,
            Some(StopCause::from_update(reason)),
            "{notice}"
        );
        assert!(drawn(&mut screen).contains("停止中："), "{notice}");
    }
}
#[test]
fn auth_expired_player_line_matches_the_stop_sentence_exactly() {
    assert_eq!(
        StopCause::AuthExpired.player_line(),
        "停止中：認証期限切れ（a で再確認）"
    );
}
#[test]
fn reconnect_limit_player_line_matches_the_stop_sentence_exactly() {
    assert_eq!(
        StopCause::ReconnectLimit.player_line(),
        "停止中：再接続できませんでした（番組を選び直してください）"
    );
}
