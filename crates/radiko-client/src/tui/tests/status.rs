use super::*;
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
        radiko::app::auth::Status::Verified(radiko::session::Membership {
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
fn playback_auth_failure_and_renewal_show_in_the_header() {
    use radiko::player::diagnostics::{Snapshot, Stage};
    let mut screen = screen();
    screen.app.authentication.status =
        radiko::app::auth::Status::Verified(radiko::session::Membership {
            area_free: Some(true),
            time_free_30: Some(false),
        });
    let expired = radiko::player::playback::failure_notice(
        &radiko::player::playback::PlaybackError::Source(radiko::api::stream::ProbeError::At(
            "HLS一覧更新",
            Box::new(radiko::api::stream::ProbeError::AuthExpired),
        )),
    );
    assert_eq!(
        expired,
        radiko::api::stream::ProbeError::AuthExpired.to_string()
    );
    assert!(!expired.contains("再生停止:"));
    assert!(!expired.contains("配信取得:"));
    assert!(!expired.contains("HLS一覧更新:"));
    assert!(!expired.contains("synthetic"));
    screen.playback_notice = expired;
    let now = Utc::now().fixed_offset();
    let mut terminal = Terminal::new(TestBackend::new(160, 40)).unwrap();
    let text = |terminal: &Terminal<TestBackend>| {
        let buffer = terminal.backend().buffer();
        let mut shown = String::new();
        for row in buffer.content.chunks(usize::from(buffer.area.width)) {
            let mut x = 0;
            while x < row.len() {
                let symbol = row[x].symbol();
                shown.push_str(symbol);
                x += ratatui::text::Span::raw(symbol).width().max(1);
            }
        }
        shown
    };
    terminal
        .draw(|frame| draw(frame, &mut screen, now))
        .unwrap();
    let shown = text(&terminal).replace(' ', "");
    assert!(shown.contains("再生側で認証失敗"), "{shown}");
    assert!(shown.contains("a再確認"), "{shown}");
    assert!(shown.contains("認証期限切れ"), "{shown}");
    assert!(shown.contains("ログイン確認済み"), "{shown}");
    let info = radiko::api::stream::playback::StreamInfo {
        title: Some("枠".into()),
        station: "TBS".into(),
        mode: radiko::api::stream::ProbeMode::Live,
        starts_at: now,
        ends_at: now + chrono::Duration::hours(1),
        position: Duration::ZERO,
    };
    screen.playback_notice.clear();
    screen.apply_playback(radiko::app::playback::Update {
        snapshot: Some(radiko::player::state::PlaybackSnapshot {
            info,
            state: PlaybackState::Playing,
            pending: None,
            volume: 25,
            position: None,
            output: None,
            segments: 1,
            reconnecting_for: None,
            pipeline: Some(Snapshot {
                stage: Stage::Acquisition,
                elapsed: Duration::from_millis(1),
                acquisition_max: Duration::ZERO,
                acquisition_max_pcm_start: Duration::ZERO,
                acquisition_max_pcm_min: Duration::ZERO,
                decode_max: Duration::ZERO,
                decode_last: Duration::ZERO,
                media_last: Duration::ZERO,
                decoded_segments: 1,
                waiting_pcm_segments: 0,
                waiting_pcm_media: Duration::ZERO,
                http: Some(radiko::api::stream::playback::RequestSnapshot {
                    stage: radiko::api::stream::playback::RequestStage::Idle,
                    response_status: Some(200),
                    elapsed: Duration::ZERO,
                    headers_max: Duration::ZERO,
                    body_max: Duration::ZERO,
                    attempts: 2,
                    retry_waits: 0,
                    network_failures: 0,
                    timeouts: 0,
                    connect_failures: 0,
                    reauthentications: 1,
                    skipped_segments: 0,
                    sequence_resets: 0,
                    joined_window_head: false,
                }),
            }),
        }),
        ..radiko::app::playback::Update::default()
    });
    assert_eq!(screen.playback_notice, "配信認証を更新して再生を続けます");
    terminal
        .draw(|frame| draw(frame, &mut screen, now))
        .unwrap();
    let shown = text(&terminal).replace(' ', "");
    assert!(shown.contains("配信認証を更新して継続"), "{shown}");
    assert!(shown.contains("再認証1"), "{shown}");
    let expired = radiko::player::playback::failure_notice(
        &radiko::player::playback::PlaybackError::Source(radiko::api::stream::ProbeError::At(
            "HLS音声取得",
            Box::new(radiko::api::stream::ProbeError::AuthExpired),
        )),
    );
    assert!(!expired.contains("再生停止:"));
    assert!(!expired.contains("HLS音声取得:"));
    screen.apply_playback(radiko::app::playback::Update {
        stopped: true,
        notice: Some(expired),
        ..radiko::app::playback::Update::default()
    });
    terminal
        .draw(|frame| draw(frame, &mut screen, now))
        .unwrap();
    let shown = text(&terminal).replace(' ', "");
    assert!(shown.contains("再生側で認証失敗"), "{shown}");
    assert!(!shown.contains("配信認証を更新して継続"), "{shown}");
}
#[test]
fn live_playlist_catch_up_is_shown_in_the_notice_and_http_diagnostics() {
    use radiko::player::diagnostics::{Snapshot, Stage};
    let mut screen = screen();
    let now = Utc::now().fixed_offset();
    let http = |skipped, resets, at_head| radiko::api::stream::playback::RequestSnapshot {
        stage: radiko::api::stream::playback::RequestStage::Idle,
        response_status: Some(200),
        elapsed: Duration::ZERO,
        headers_max: Duration::ZERO,
        body_max: Duration::ZERO,
        attempts: 1,
        retry_waits: 0,
        network_failures: 0,
        timeouts: 0,
        connect_failures: 0,
        reauthentications: 0,
        skipped_segments: skipped,
        sequence_resets: resets,
        joined_window_head: at_head,
    };
    let update = |skipped, resets, at_head| radiko::app::playback::Update {
        snapshot: Some(radiko::player::state::PlaybackSnapshot {
            info: radiko::api::stream::playback::StreamInfo {
                title: Some("枠".into()),
                station: "TBS".into(),
                mode: radiko::api::stream::ProbeMode::Live,
                starts_at: now,
                ends_at: now + chrono::Duration::hours(1),
                position: Duration::ZERO,
            },
            state: PlaybackState::Playing,
            pending: None,
            volume: 25,
            position: None,
            output: None,
            segments: 2,
            reconnecting_for: None,
            pipeline: Some(Snapshot {
                stage: Stage::Acquisition,
                elapsed: Duration::from_millis(1),
                acquisition_max: Duration::ZERO,
                acquisition_max_pcm_start: Duration::ZERO,
                acquisition_max_pcm_min: Duration::ZERO,
                decode_max: Duration::ZERO,
                decode_last: Duration::ZERO,
                media_last: Duration::ZERO,
                decoded_segments: 2,
                waiting_pcm_segments: 0,
                waiting_pcm_media: Duration::ZERO,
                http: Some(http(skipped, resets, at_head)),
            }),
        }),
        ..radiko::app::playback::Update::default()
    };
    screen.apply_playback(update(2, 0, true));
    assert_eq!(
        screen.playback_notice,
        "配信窓の先頭から再開しました（累計2）"
    );
    assert!(screen.pipeline_diagnostics.contains("飛ばしたセグメント2"));
    let mut terminal = Terminal::new(TestBackend::new(160, 40)).unwrap();
    terminal
        .draw(|frame| draw(frame, &mut screen, now))
        .unwrap();
    let shown: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>()
        .replace(' ', "");
    assert!(shown.contains("配信窓の先頭から再開しました"), "{shown}");
    assert!(shown.contains("累計2"), "{shown}");
    assert!(shown.contains("飛ばしたセグメント2"), "{shown}");
    screen.apply_playback(update(4, 0, false));
    assert_eq!(
        screen.playback_notice,
        "ライブ端に追いつきました（飛ばしたセグメントは累計4）"
    );
    terminal
        .draw(|frame| draw(frame, &mut screen, now))
        .unwrap();
    let shown: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>()
        .replace(' ', "");
    assert!(shown.contains("ライブ端に追いつきました"), "{shown}");
    assert!(shown.contains("飛ばしたセグメント4"), "{shown}");
    screen.apply_playback(update(4, 1, true));
    assert_eq!(
        screen.playback_notice,
        "配信窓の先頭から再開しました（累計4）"
    );
    assert!(screen.pipeline_diagnostics.contains("通し番号の巻き戻り1"));
    screen.playback_notice = "配信認証を更新して再生を続けます".into();
    screen.apply_playback(update(5, 1, false));
    assert_eq!(screen.playback_notice, "配信認証を更新して再生を続けます");
    assert!(screen.pipeline_diagnostics.contains("飛ばしたセグメント5"));
    screen.apply_playback(radiko::app::playback::Update {
        new_session: true,
        ..radiko::app::playback::Update::default()
    });
    assert!(screen.playback_notice.is_empty());
    screen.apply_playback(update(2, 0, false));
    assert_eq!(
        screen.playback_notice,
        "ライブ端に追いつきました（飛ばしたセグメントは累計2）"
    );
}
#[test]
fn reconnecting_notice_is_shown_and_yields_to_catch_up_and_stop() {
    use radiko::player::diagnostics::{Snapshot, Stage};
    let mut screen = screen();
    let now = Utc::now().fixed_offset();
    let snapshot = |reconnecting_for, skipped, at_head| radiko::player::state::PlaybackSnapshot {
        info: radiko::api::stream::playback::StreamInfo {
            title: Some("枠".into()),
            station: "TBS".into(),
            mode: radiko::api::stream::ProbeMode::Live,
            starts_at: now,
            ends_at: now + chrono::Duration::hours(1),
            position: Duration::ZERO,
        },
        state: PlaybackState::Playing,
        pending: None,
        volume: 25,
        position: None,
        output: None,
        segments: 1,
        reconnecting_for,
        pipeline: Some(Snapshot {
            stage: Stage::Waiting,
            elapsed: Duration::from_secs(1),
            acquisition_max: Duration::ZERO,
            acquisition_max_pcm_start: Duration::ZERO,
            acquisition_max_pcm_min: Duration::ZERO,
            decode_max: Duration::ZERO,
            decode_last: Duration::ZERO,
            media_last: Duration::ZERO,
            decoded_segments: 1,
            waiting_pcm_segments: 0,
            waiting_pcm_media: Duration::ZERO,
            http: Some(radiko::api::stream::playback::RequestSnapshot {
                stage: radiko::api::stream::playback::RequestStage::Idle,
                response_status: Some(200),
                elapsed: Duration::ZERO,
                headers_max: Duration::ZERO,
                body_max: Duration::ZERO,
                attempts: 1,
                retry_waits: 0,
                network_failures: 0,
                timeouts: 0,
                connect_failures: 0,
                reauthentications: 0,
                skipped_segments: skipped,
                sequence_resets: 0,
                joined_window_head: at_head,
            }),
        }),
    };
    screen.apply_playback(radiko::app::playback::Update {
        snapshot: Some(snapshot(Some(Duration::from_secs(42)), 0, false)),
        ..radiko::app::playback::Update::default()
    });
    assert_eq!(screen.playback_notice, "再接続中…（42秒）");
    let mut terminal = Terminal::new(TestBackend::new(160, 40)).unwrap();
    terminal
        .draw(|frame| draw(frame, &mut screen, now))
        .unwrap();
    let shown: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>()
        .replace(' ', "");
    assert!(shown.contains("再接続中…（42秒）"), "{shown}");
    screen.apply_playback(radiko::app::playback::Update {
        snapshot: Some(snapshot(None, 2, true)),
        ..radiko::app::playback::Update::default()
    });
    assert_eq!(
        screen.playback_notice,
        "配信窓の先頭から再開しました（累計2）"
    );
    screen.apply_playback(radiko::app::playback::Update {
        snapshot: Some(snapshot(None, 5, false)),
        ..radiko::app::playback::Update::default()
    });
    assert_eq!(
        screen.playback_notice,
        "ライブ端に追いつきました（飛ばしたセグメントは累計5）"
    );
    screen.playback_notice = "再生停止: 音声コールバックが進まないため出力を停止しました".into();
    screen.playback_stopped = true;
    screen.apply_playback(radiko::app::playback::Update {
        snapshot: Some(snapshot(Some(Duration::from_secs(9)), 5, false)),
        ..radiko::app::playback::Update::default()
    });
    assert!(screen.playback_notice.contains("再生停止"));
    screen.playback_stopped = false;
    screen.playback_notice = "配信認証を更新して再生を続けます".into();
    screen.apply_playback(radiko::app::playback::Update {
        snapshot: Some(snapshot(Some(Duration::from_secs(9)), 5, false)),
        ..radiko::app::playback::Update::default()
    });
    assert_eq!(screen.playback_notice, "配信認証を更新して再生を続けます");
}
#[test]
fn confirmed_stop_notices_survive_reconnect_catch_up_and_auth_renewal() {
    use radiko::api::stream::ProbeError;
    use radiko::player::diagnostics::{Snapshot, Stage};
    use radiko::player::playback::{PlaybackError, failure_notice};
    let now = Utc::now().fixed_offset();
    let stages = ["HLS一覧更新", "HLS初期化情報取得", "HLS音声取得", "別段階"];
    let mut errors = vec![
        PlaybackError::Source(ProbeError::WindowLost),
        PlaybackError::Source(ProbeError::ReconnectGaveUp),
        PlaybackError::Source(ProbeError::AuthExpired),
        PlaybackError::ReconnectGaveUp,
        PlaybackError::Output(radiko::player::output::OutputError::ReconnectGaveUp),
    ];
    for stage in stages {
        errors.push(PlaybackError::Source(ProbeError::At(
            stage,
            Box::new(ProbeError::AuthExpired),
        )));
        errors.push(PlaybackError::Source(ProbeError::At(
            stage,
            Box::new(ProbeError::WindowLost),
        )));
        errors.push(PlaybackError::Source(ProbeError::At(
            stage,
            Box::new(ProbeError::ReconnectGaveUp),
        )));
    }
    errors.push(PlaybackError::Source(ProbeError::At(
        "外側",
        Box::new(ProbeError::At(
            "HLS音声取得",
            Box::new(ProbeError::AuthExpired),
        )),
    )));
    let http = |skipped, reauth| radiko::api::stream::playback::RequestSnapshot {
        stage: radiko::api::stream::playback::RequestStage::Idle,
        response_status: Some(200),
        elapsed: Duration::ZERO,
        headers_max: Duration::ZERO,
        body_max: Duration::ZERO,
        attempts: 1,
        retry_waits: 0,
        network_failures: 0,
        timeouts: 0,
        connect_failures: 0,
        reauthentications: reauth,
        skipped_segments: skipped,
        sequence_resets: 0,
        joined_window_head: true,
    };
    let snapshot = |reconnecting_for, skipped, reauth| radiko::player::state::PlaybackSnapshot {
        info: radiko::api::stream::playback::StreamInfo {
            title: Some("枠".into()),
            station: "TBS".into(),
            mode: radiko::api::stream::ProbeMode::Live,
            starts_at: now,
            ends_at: now + chrono::Duration::hours(1),
            position: Duration::ZERO,
        },
        state: PlaybackState::Playing,
        pending: None,
        volume: 25,
        position: None,
        output: None,
        segments: 1,
        reconnecting_for,
        pipeline: Some(Snapshot {
            stage: Stage::Waiting,
            elapsed: Duration::from_secs(1),
            acquisition_max: Duration::ZERO,
            acquisition_max_pcm_start: Duration::ZERO,
            acquisition_max_pcm_min: Duration::ZERO,
            decode_max: Duration::ZERO,
            decode_last: Duration::ZERO,
            media_last: Duration::ZERO,
            decoded_segments: 1,
            waiting_pcm_segments: 0,
            waiting_pcm_media: Duration::ZERO,
            http: Some(http(skipped, reauth)),
        }),
    };
    for error in errors {
        let notice = failure_notice(&error);
        assert!(!notice.contains("再生停止:"), "{notice}");
        assert!(!notice.contains("配信取得:"), "{notice}");
        for stage in stages {
            assert!(!notice.contains(&format!("{stage}: ")), "{notice}");
        }
        let mut screen = screen();
        screen.apply_playback(radiko::app::playback::Update {
            stopped: true,
            ended: true,
            notice: Some(notice.clone()),
            snapshot: Some(snapshot(Some(Duration::from_secs(42)), 0, 0)),
            ..radiko::app::playback::Update::default()
        });
        assert_eq!(screen.playback_notice, notice);
        assert!(screen.playback_stopped);
        screen.apply_playback(radiko::app::playback::Update {
            snapshot: Some(snapshot(Some(Duration::from_secs(9)), 0, 1)),
            ..radiko::app::playback::Update::default()
        });
        assert_eq!(
            screen.playback_notice, notice,
            "reconnect or renewal replaced the stop"
        );
        screen.apply_playback(radiko::app::playback::Update {
            snapshot: Some(snapshot(None, 4, 1)),
            ..radiko::app::playback::Update::default()
        });
        assert_eq!(screen.playback_notice, notice, "catch-up replaced the stop");
        assert!(!screen.playback_notice.starts_with("再接続中"));
    }
}
#[test]
fn stopped_window_lost_notice_is_not_replaced_by_the_live_edge_catch_up() {
    use radiko::player::diagnostics::{Snapshot, Stage};
    let now = Utc::now().fixed_offset();
    let sentence = "通信が長く途切れたため、続きを再生できなくなりました。番組を選び直してください";
    let http = |skipped| radiko::api::stream::playback::RequestSnapshot {
        stage: radiko::api::stream::playback::RequestStage::Idle,
        response_status: Some(200),
        elapsed: Duration::ZERO,
        headers_max: Duration::ZERO,
        body_max: Duration::ZERO,
        attempts: 1,
        retry_waits: 0,
        network_failures: 0,
        timeouts: 0,
        connect_failures: 0,
        reauthentications: 0,
        skipped_segments: skipped,
        sequence_resets: 0,
        joined_window_head: false,
    };
    let snapshot = |skipped| radiko::player::state::PlaybackSnapshot {
        info: radiko::api::stream::playback::StreamInfo {
            title: Some("枠".into()),
            station: "TBS".into(),
            mode: radiko::api::stream::ProbeMode::Live,
            starts_at: now,
            ends_at: now + chrono::Duration::hours(1),
            position: Duration::ZERO,
        },
        state: PlaybackState::Playing,
        pending: None,
        volume: 25,
        position: None,
        output: None,
        segments: 1,
        reconnecting_for: None,
        pipeline: Some(Snapshot {
            stage: Stage::Waiting,
            elapsed: Duration::from_secs(1),
            acquisition_max: Duration::ZERO,
            acquisition_max_pcm_start: Duration::ZERO,
            acquisition_max_pcm_min: Duration::ZERO,
            decode_max: Duration::ZERO,
            decode_last: Duration::ZERO,
            media_last: Duration::ZERO,
            decoded_segments: 1,
            waiting_pcm_segments: 0,
            waiting_pcm_media: Duration::ZERO,
            http: Some(http(skipped)),
        }),
    };
    let mut screen = screen();
    screen.apply_playback(radiko::app::playback::Update {
        stopped: true,
        ended: true,
        notice: Some(sentence.into()),
        snapshot: Some(snapshot(0)),
        ..radiko::app::playback::Update::default()
    });
    assert!(screen.playback_stopped);
    screen.apply_playback(radiko::app::playback::Update {
        snapshot: Some(snapshot(5)),
        ..radiko::app::playback::Update::default()
    });
    assert_eq!(screen.playback_notice, sentence);
    assert!(!screen.playback_notice.contains("ライブ端に追いつきました"));
}
#[test]
fn pipeline_diagnostics_and_notices_fit_the_playback_footer() {
    use radiko::player::diagnostics::{Snapshot, Stage};
    let mut screen = screen();
    let now = Utc::now().fixed_offset();
    let visible = |terminal: &Terminal<TestBackend>| {
        let buffer = terminal.backend().buffer();
        let mut text = String::new();
        for row in buffer.content.chunks(usize::from(buffer.area.width)) {
            let mut x = 0;
            while x < row.len() {
                let symbol = row[x].symbol();
                text.push_str(symbol);
                x += ratatui::text::Span::raw(symbol).width().max(1);
            }
            text.push('\n');
        }
        text
    };
    screen.app.playback = Some(radiko::player::state::PlaybackSnapshot {
        info: radiko::api::stream::playback::StreamInfo {
            title: Some("長い番組名".repeat(40)),
            station: "TBS".into(),
            mode: radiko::api::stream::ProbeMode::TimeFree,
            starts_at: now,
            ends_at: now + chrono::Duration::hours(1),
            position: Duration::ZERO,
        },
        state: PlaybackState::Playing,
        pending: None,
        volume: 0,
        position: None,
        output: None,
        segments: 1,
        reconnecting_for: None,
        pipeline: Some(Snapshot {
            stage: Stage::Backpressure,
            elapsed: Duration::from_millis(4000),
            acquisition_max: Duration::from_millis(120),
            acquisition_max_pcm_start: Duration::from_millis(10000),
            acquisition_max_pcm_min: Duration::from_millis(5000),
            decode_last: Duration::from_millis(25),
            decode_max: Duration::from_millis(80),
            media_last: Duration::from_millis(5000),
            decoded_segments: 1,
            waiting_pcm_segments: 3,
            waiting_pcm_media: Duration::from_millis(15000),
            http: Some(radiko::api::stream::playback::RequestSnapshot {
                stage: radiko::api::stream::playback::RequestStage::Headers,
                response_status: Some(200),
                elapsed: Duration::from_millis(12),
                headers_max: Duration::from_millis(300),
                body_max: Duration::from_millis(40),
                attempts: 5,
                retry_waits: 1,
                network_failures: 2,
                timeouts: 1,
                connect_failures: 1,
                reauthentications: 0,
                skipped_segments: 0,
                sequence_resets: 0,
                joined_window_head: false,
            }),
        }),
    });
    screen.playback_notice = "NOTICE_END".into();
    let mut terminal = Terminal::new(TestBackend::new(160, 40)).unwrap();
    terminal
        .draw(|frame| draw(frame, &mut screen, now))
        .unwrap();
    let text = visible(&terminal);
    assert!(
        text.replace(' ', "").contains("処理診断配送待ち4000ms"),
        "{text}"
    );
    assert!(text.replace(' ', "").contains("音声5000ms"));
    assert!(text.contains("NOTICE_END"));
    assert!(text.replace(' ', "").contains("通信診断応答待ち12ms"));
    let mut narrow = Terminal::new(TestBackend::new(80, 20)).unwrap();
    narrow.draw(|frame| draw(frame, &mut screen, now)).unwrap();
    let text = visible(&narrow).replace(' ', "");
    assert!(text.contains("取得max120ms/復号25/80ms/音声5000ms"));
    assert!(text.contains("再生中:[TBS/TimeFree]"));
    screen.help_open = true;
    narrow.draw(|frame| draw(frame, &mut screen, now)).unwrap();
    let text = visible(&narrow).replace(' ', "");
    assert!(text.contains("再生中:[TBS/TimeFree]Playing/音量0%"));
    assert!(text.contains("通信診断応答待ち12ms"));
    assert!(text.replace('│', "").contains("通信失敗2・期限1・接続1"));
    assert!(text.contains("HTTP200"));
    assert!(text.replace(['│', '\n'], "").contains("待機PCM3個15000ms"));
    assert!(
        text.replace(['│', '\n'], "")
            .contains("取得max時待機10000→5000ms")
    );
    screen.help_open = false;
    let snapshot = screen.app.playback.take();
    let fresh = snapshot
        .as_ref()
        .map(|p| radiko::player::state::PlaybackSnapshot {
            info: radiko::api::stream::playback::StreamInfo {
                station: "OBS".into(),
                ..p.info.clone()
            },
            state: PlaybackState::Buffering,
            pending: None,
            volume: 0,
            position: None,
            output: None,
            segments: 0,
            reconnecting_for: None,
            pipeline: None,
        });
    screen.apply_playback(playback::Update {
        snapshot,
        ended: true,
        notice: Some("再生停止: 音声コールバックが進まないため出力を停止しました".into()),
        ..Default::default()
    });
    assert!(screen.app.playback.is_none());
    terminal
        .draw(|frame| draw(frame, &mut screen, now))
        .unwrap();
    let text = visible(&terminal);
    let compact = text.replace(' ', "");
    assert!(compact.contains("直近通知（終了時未集計）:処理診断配送待ち4000ms"));
    assert!(compact.contains("再生停止:音声コールバック"));
    assert!(compact.contains("直近通知（終了時未集計）:通信診断応答待ち12ms"));
    screen.help_open = true;
    narrow.draw(|frame| draw(frame, &mut screen, now)).unwrap();
    assert!(
        visible(&narrow)
            .replace(' ', "")
            .contains("直近通知（終了時未集計）:通信診断")
    );
    screen.help_open = false;
    screen.apply_playback(playback::Update {
        new_session: true,
        snapshot: fresh,
        notice: Some("新しい準備".into()),
        ..Default::default()
    });
    assert_eq!(screen.app.playback.as_ref().unwrap().info.station, "OBS");
    assert_eq!(screen.playback_notice, "新しい準備");
    assert!(screen.pipeline_diagnostics.is_empty());
    assert!(!screen.playback_diagnostics.contains("終了時未集計"));
    screen.apply_playback(playback::Update::default());
    assert!(screen.app.playback.is_some());
    screen.apply_playback(playback::Update {
        new_session: true,
        ..Default::default()
    });
    assert!(screen.app.playback.is_none());
    assert!(screen.playback_diagnostics.is_empty());
    assert!(screen.pipeline_diagnostics.is_empty());
    assert!(screen.playback_notice.is_empty());
}
#[test]
fn failure_browser_shows_all_missing_ranges_and_never_dispatches_actions() {
    let mut screen = screen();
    let mut search = Service::new(|| -> Result<CatalogClient, _> { panic!("read-only view") });
    screen.app.status = SearchStatus::Partial;
    for index in 0..100 {
        screen.app.schedule_failures.push(radiko::catalog::Failure {
            target: format!("MISSING_{index:03}"),
            day: Some(radiko::api::catalog::parse_day("20261015").unwrap()),
            error: radiko::api::catalog::CatalogError::Http(404),
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
        .push(radiko::app::InvalidSchedule {
            station_id: "BAD_SLOT_STATION".into(),
            day: radiko::api::catalog::parse_day("20261015").unwrap(),
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
    screen.app.schedule_failures.push(radiko::catalog::Failure {
        target: "NEW_FAILURE".into(),
        day: None,
        error: radiko::api::catalog::CatalogError::Network,
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
    let template = radiko::api::catalog::parse_programs(
        include_bytes!("../../../../../tests/fixtures/schedule.xml"),
        "TBS",
        radiko::api::catalog::parse_day("20261015").unwrap(),
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
    let template = radiko::api::catalog::parse_programs(
        include_bytes!("../../../../../tests/fixtures/schedule.xml"),
        "TBS",
        radiko::api::catalog::parse_day("20261015").unwrap(),
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
