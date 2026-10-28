use super::*;
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
        use radiko::api::stream::{
            ProbeMode,
            chapters::{EventBinding, Outcome, Report},
        };
        let mut screen = screen();
        let mut search =
            Service::new(|| -> Result<CatalogClient, _> { panic!("jump must not search") });
        let program = radiko::api::catalog::parse_programs(
            include_bytes!("../../../../../tests/fixtures/schedule.xml"),
            "TBS",
            radiko::api::catalog::parse_day("20261015").unwrap(),
        )
        .unwrap()
        .programs
        .remove(0);
        let id = screen.app.chapters.open(program.clone()).unwrap();
        screen.app.chapters.apply(radiko::app::chapters::Event {
            id,
            key: program.key.clone(),
            result: Ok(Report {
                station: program.key.station_id.clone(),
                starts_at: program.key.starts_at,
                ends_at: program.ends_at,
                title: Some(program.title.clone()),
                outcome: Outcome::Aligned(
                    radiko::api::parse_chapters(
                        include_bytes!("../../../../../tests/fixtures/chapters.json"),
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
            screen.app.playback = Some(radiko::player::state::PlaybackSnapshot {
                pipeline: None,
                info: radiko::api::stream::playback::StreamInfo {
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
                output: Some(radiko::player::output::OutputSummary {
                    frames: 1,
                    underrun_frames: 0,
                    sample_rate: 48000,
                    channels: 2,
                    sample_format: radiko::player::output::OutputSampleFormat::F32,
                }),
                segments: 1,
                reconnecting_for: None,
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
        let program = radiko::api::catalog::parse_programs(
            include_bytes!("../../../../../tests/fixtures/schedule.xml"),
            "TBS",
            radiko::api::catalog::parse_day("20261015").unwrap(),
        )
        .unwrap()
        .programs
        .remove(0);
        screen.app.programs = vec![program.clone()];
        screen.app.visible = vec![0];
        screen.list.select(Some(0));
        screen.app.playback = Some(radiko::player::state::PlaybackSnapshot {
            pipeline: None,
            info: radiko::api::stream::playback::StreamInfo {
                title: Some("現在の別番組".into()),
                station: "OBS".into(),
                starts_at: program.key.starts_at,
                ends_at: program.ends_at,
                mode: radiko::api::stream::ProbeMode::TimeFree,
                position: Duration::ZERO,
            },
            state: PlaybackState::Playing,
            pending: None,
            volume: 25,
            position: Some(Duration::from_secs(8)),
            output: None,
            segments: 1,
            reconnecting_for: None,
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
        screen.app.chapters.apply(radiko::app::chapters::Event {
            id,
            key: program.key.clone(),
            result: Ok(radiko::api::stream::chapters::Report {
                station: program.key.station_id.clone(),
                starts_at: program.key.starts_at,
                ends_at: program.ends_at,
                title: Some(program.title.clone()),
                outcome: radiko::api::stream::chapters::Outcome::Candidates(
                    radiko::api::parse_chapters(
                        include_bytes!("../../../../../tests/fixtures/chapters.json"),
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
        let target = radiko::api::catalog::parse_programs(
            include_bytes!("../../../../../tests/fixtures/schedule.xml"),
            "TBS",
            radiko::api::catalog::parse_day("20261015").unwrap(),
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
                    radiko::api::stream::chapters::BindingIssue::EventMetadataMissing,
                ),
                "イベント情報を確認できません",
            ),
            (
                ChapterStatus::Unresolved(
                    radiko::api::stream::chapters::BindingIssue::OriginMismatch,
                ),
                "補正未対応",
            ),
            (
                ChapterStatus::Unresolved(
                    radiko::api::stream::chapters::BindingIssue::SlotMismatch,
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
        let mut programs = radiko::api::catalog::parse_programs(
            include_bytes!("../../../../../tests/fixtures/schedule.xml"),
            "TBS",
            radiko::api::catalog::parse_day("20261015").unwrap(),
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
        screen.app.playback = Some(radiko::player::state::PlaybackSnapshot {
            pipeline: None,
            info: radiko::api::stream::playback::StreamInfo {
                title: Some(program.title),
                station: program.key.station_id,
                starts_at: program.key.starts_at,
                ends_at: program.ends_at,
                mode: radiko::api::stream::ProbeMode::TimeFree,
                position: Duration::ZERO,
            },
            state: PlaybackState::Playing,
            pending: None,
            position: Some(Duration::from_secs(10)),
            volume: 25,
            output: None,
            segments: 1,
            reconnecting_for: None,
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
        let list = radiko::api::catalog::parse_programs(
            include_bytes!("../../../../../tests/fixtures/schedule.xml"),
            "TBS",
            radiko::api::catalog::parse_day("20261015").unwrap(),
        )
        .unwrap();
        screen.apply(radiko::app::search::Event::Progress {
            id,
            progress: radiko::app::search::Progress {
                phase: "番組表",
                target: "TBS".into(),
                day: Some(list.broadcast_day),
                completed: 1,
                total: 1,
                failed: 0,
                programs: list.programs,
            },
        });
        let now = radiko::api::stream::parse_time("20261015060000").unwrap();
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
