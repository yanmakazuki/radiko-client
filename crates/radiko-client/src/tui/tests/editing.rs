use super::*;
    #[test]
    fn offline_keys_do_not_request_playback_login_chapters_or_stored_state() {
        let mut screen = screen();
        screen.offline = true;
        let mut search =
            Service::new(|| -> Result<CatalogClient, _> { panic!("offline keys must not fetch") });
        for key in ['I', 'a', 'c', 'l', 'O', 'b'] {
            screen
                .key(
                    KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE),
                    &mut search,
                )
                .unwrap();
            assert!(screen.playback_action.is_none());
            assert!(screen.login.is_none());
            assert!(!screen.auth_refresh);
            assert!(screen.chapter_action.is_none());
            assert!(!screen.logout_confirm);
            assert!(!screen.logout_requested);
            assert!(screen.named.is_none());
            assert!(screen.named_action.is_none());
            assert!(!screen.playback_notice.is_empty());
        }
        screen.logout_confirm = true;
        screen
            .key(
                KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE),
                &mut search,
            )
            .unwrap();
        assert!(!screen.logout_requested);
        assert!(!screen.logout_confirm);
        screen.playback_notice.clear();
        screen
            .key(
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                &mut search,
            )
            .unwrap();
        assert!(screen.playback_action.is_none());
        assert!(screen.playback_notice.contains("再生しません"));
    }
    #[test]
    fn volume_display_changes_only_after_the_playback_service_accepts_the_command() {
        let mut screen = screen();
        let mut search =
            Service::new(|| -> Result<CatalogClient, _> { panic!("volume keys must not fetch") });
        let mut player = PlaybackService::with_factory(|_| {
            Err(radiko::player::playback::PlaybackError::LoginRequired)
        });
        let now = Utc::now().fixed_offset();
        for expected in [20, 15, 10, 5, 0] {
            let before = screen.volume;
            screen
                .key(
                    KeyEvent::new(KeyCode::Char('-'), KeyModifiers::NONE),
                    &mut search,
                )
                .unwrap();
            assert_eq!(screen.volume, before);
            screen.dispatch_playback(&mut player, now);
            assert_eq!(screen.volume, expected);
            assert!(screen.playback_action.is_none());
        }
        let program = radiko::api::catalog::parse_programs(
            include_bytes!("../../../../../tests/fixtures/schedule.xml"),
            "TBS",
            radiko::api::catalog::parse_day("20261015").unwrap(),
        )
        .unwrap()
        .programs
        .remove(0);
        player.select(&program, program.ends_at).unwrap();
        player.stop();
        screen
            .key(
                KeyEvent::new(KeyCode::Char('+'), KeyModifiers::NONE),
                &mut search,
            )
            .unwrap();
        screen.dispatch_playback(&mut player, now);
        assert_eq!(screen.volume, 0);
        assert!(screen.playback_notice.contains("停止処理中"));
        assert!(screen.playback_action.is_none());
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
        let mut p = radiko::api::catalog::parse_programs(
            include_bytes!("../../../../../tests/fixtures/schedule.xml"),
            "TBS",
            radiko::api::catalog::parse_day("20261015").unwrap(),
        )
        .unwrap()
        .programs
        .remove(0);
        p.program_genres = vec![radiko::domain::Classification {
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
        let mut programs = radiko::api::catalog::parse_programs(
            include_bytes!("../../../../../tests/fixtures/schedule.xml"),
            "TBS",
            radiko::api::catalog::parse_day("20261015").unwrap(),
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
        let mut p = radiko::api::catalog::parse_programs(
            include_bytes!("../../../../../tests/fixtures/schedule.xml"),
            "TBS",
            radiko::api::catalog::parse_day("20261015").unwrap(),
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
        screen.app.playback = Some(radiko::player::state::PlaybackSnapshot {
            pipeline: None,
            info: radiko::api::stream::playback::StreamInfo {
                station: target.key.station_id.clone(),
                title: Some(target.title.clone()),
                mode: radiko::api::stream::ProbeMode::Live,
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
            reconnecting_for: None,
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
                Err(radiko::api::catalog::CatalogError::Network)
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
            radiko::domain::BroadcastRange::new(
                radiko::api::catalog::parse_day("20261006").unwrap(),
                radiko::api::catalog::parse_day("20261007").unwrap(),
            )
            .unwrap(),
            radiko::domain::search::Filter::default(),
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
                Err(radiko::api::catalog::CatalogError::InvalidDate)
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
