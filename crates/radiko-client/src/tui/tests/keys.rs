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
        case(surface, KeyCode::Esc, KeyModifiers::NONE, KeyEffect::Quit),
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
    assert!(help::KEYS.contains(LOGOUT_KEY_WORDING));
    assert!(USAGE.contains(LOGOUT_KEY_WORDING));
    assert!(LOGOUT_KEY_WORDING.contains("n / Esc / qで取消"));
    for surface in [Surface::MainList, Surface::Live] {
        let tokens: Vec<_> = cases
            .iter()
            .filter(|case| case.surface == surface && case.effect == KeyEffect::Quit)
            .map(|case| match (case.code, case.modifiers) {
                (KeyCode::Char('q'), KeyModifiers::NONE) => "q",
                (KeyCode::Esc, KeyModifiers::NONE) => "Esc",
                (KeyCode::Char('c'), KeyModifiers::CONTROL) => "Ctrl+C",
                _ => panic!("終了キーが表にありません: {case:?}"),
            })
            .collect();
        assert_eq!(tokens, ["q", "Esc", "Ctrl+C"]);
        for token in tokens {
            assert!(QUIT_KEY_WORDING.contains(token), "{token}");
        }
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
}
