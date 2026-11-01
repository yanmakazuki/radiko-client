use super::*;
use radiko::player::playback::{PlaybackError, failure_notice};

fn content_rows(screen: &mut Screen, width: u16, height: u16) -> Vec<String> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| draw(frame, screen, Utc::now().fixed_offset()))
        .unwrap();
    let columns = usize::from(terminal.backend().buffer().area.width);
    terminal
        .backend()
        .buffer()
        .content
        .chunks(columns)
        .map(|row| {
            let mut line = String::new();
            let mut x = 0;
            while x < row.len() {
                let symbol = row[x].symbol();
                line.push_str(symbol);
                x += ratatui::text::Span::raw(symbol).width().max(1);
            }
            let line = line.strip_prefix('│').unwrap_or(&line);
            let line = line.strip_suffix('│').unwrap_or(line);
            line.trim_end().to_owned()
        })
        .collect()
}

fn assert_exact_rows(screen: &mut Screen, lines: &[&str]) {
    for (width, height) in [(80u16, 24u16), (160, 40)] {
        let rows = content_rows(screen, width, height);
        let start = rows
            .iter()
            .position(|row| row == lines[0])
            .unwrap_or_else(|| {
                panic!(
                    "{width}x{height} did not render {:?}\n{}",
                    lines[0],
                    rows.join("\n")
                )
            });
        for (offset, expected) in lines.iter().enumerate() {
            assert_eq!(
                rows[start + offset].as_str(),
                *expected,
                "{width}x{height} row {}",
                start + offset
            );
        }
    }
}

fn device_error(kind: radiko::app::playback::OutputFailure) -> PlaybackError {
    use radiko::player::output::OutputError;
    PlaybackError::Output(match kind {
        radiko::app::playback::OutputFailure::NoDevice => OutputError::NoDevice,
        radiko::app::playback::OutputFailure::UnsupportedFormat => OutputError::UnsupportedFormat,
        radiko::app::playback::OutputFailure::Device => OutputError::Device,
        radiko::app::playback::OutputFailure::Stalled => OutputError::Stalled,
    })
}

fn show_output_stop(screen: &mut Screen, error: &PlaybackError) {
    screen.apply_playback(radiko::app::playback::Update {
        stopped: true,
        ended: true,
        stop_reason: radiko::app::playback::stop_reason(error),
        output_failure: radiko::app::playback::output_failure(error),
        notice_kind: radiko::app::playback::notice_kind(error),
        notice: Some(failure_notice(error)),
        ..radiko::app::playback::Update::default()
    });
}

fn guide_lines(kind: radiko::app::playback::OutputFailure) -> &'static [&'static str] {
    match kind {
        radiko::app::playback::OutputFailure::NoDevice => &[
            "出力先がありません。OSのサウンド設定を確認し、Enterで再生し直してください",
            "自動再開はしません",
        ],
        radiko::app::playback::OutputFailure::UnsupportedFormat => &[
            "対応形式外です。OSの出力設定を確認し、Enterで再生し直してください",
            "自動再開はしません",
        ],
        radiko::app::playback::OutputFailure::Device => &[
            "出力機器を確認してください。OSの出力先を見て、Enterで再生し直してください",
            "自動再開はしません",
        ],
        radiko::app::playback::OutputFailure::Stalled => &[
            "スリープ復帰・機器変更後は出力先を確認し、Enterで再生し直してください",
            "自動再開はしません",
        ],
    }
}

#[test]
fn output_guides_render_exactly_at_80_and_160_from_the_error_type() {
    use radiko::player::output::{OfferedOutputFormat, OutputError};
    let incompatible = PlaybackError::Output(OutputError::IncompatibleOutput {
        requested_rate: 48_000,
        offered: vec![OfferedOutputFormat {
            channels: 2,
            min_sample_rate: 44_100,
            max_sample_rate: 44_100,
            sample_format: "f32",
        }],
    });
    let cases = [
        (
            device_error(radiko::app::playback::OutputFailure::NoDevice),
            radiko::app::playback::OutputFailure::NoDevice,
        ),
        (
            device_error(radiko::app::playback::OutputFailure::UnsupportedFormat),
            radiko::app::playback::OutputFailure::UnsupportedFormat,
        ),
        (
            incompatible,
            radiko::app::playback::OutputFailure::UnsupportedFormat,
        ),
        (
            device_error(radiko::app::playback::OutputFailure::Device),
            radiko::app::playback::OutputFailure::Device,
        ),
        (
            PlaybackError::Output(OutputError::DeviceInfo),
            radiko::app::playback::OutputFailure::Device,
        ),
        (
            device_error(radiko::app::playback::OutputFailure::Stalled),
            radiko::app::playback::OutputFailure::Stalled,
        ),
    ];
    for (error, kind) in cases {
        let mut screen = screen();
        show_output_stop(&mut screen, &error);
        assert_eq!(
            screen.output_guide,
            Some(super::super::OutputGuide::from_update(kind))
        );
        assert!(screen.stop_cause.is_none());
        assert!(screen.playback_action.is_none());
        let expected = guide_lines(kind);
        assert_exact_rows(&mut screen, expected);
        for (width, height) in [(80u16, 24u16), (160, 40)] {
            let shown = content_rows(&mut screen, width, height).join("\n");
            for line in expected {
                assert!(
                    shown.contains(line),
                    "{width}x{height} missing {line}\n{shown}"
                );
            }
        }
    }
}

#[test]
fn the_device_sentence_without_output_failure_does_not_show_a_guide() {
    let mut bare = screen();
    bare.apply_playback(radiko::app::playback::Update {
        stopped: true,
        ended: true,
        notice: Some("既定の音声出力機器がありません".into()),
        ..radiko::app::playback::Update::default()
    });
    assert!(bare.output_guide.is_none());
    let rows = content_rows(&mut bare, 80, 24);
    assert!(
        rows.iter().any(|row| row == "再生中: なし / 音量25%"),
        "{rows:?}"
    );
    assert!(
        rows.iter()
            .all(|row| row != super::super::messages::OUTPUT_NO_DEVICE)
    );

    let mut idle = super::screen();
    idle.apply_playback(radiko::app::playback::Update {
        stopped: false,
        output_failure: Some(radiko::app::playback::OutputFailure::NoDevice),
        notice: Some("再生切替失敗（現在の状態を維持）: 既定の音声出力機器がありません".into()),
        ..radiko::app::playback::Update::default()
    });
    assert_eq!(idle.output_guide, Some(super::super::OutputGuide::NoDevice));
    assert!(!idle.playback_stopped);
    assert!(idle.playback_action.is_none());
    assert_exact_rows(
        &mut idle,
        guide_lines(radiko::app::playback::OutputFailure::NoDevice),
    );

    let mut playing = super::screen();
    playing.app.playback = Some(playing_snapshot());
    playing.apply_playback(radiko::app::playback::Update {
        stopped: false,
        snapshot: Some(playing_snapshot()),
        output_failure: Some(radiko::app::playback::OutputFailure::NoDevice),
        notice: Some("再生切替失敗（現在の状態を維持）: 既定の音声出力機器がありません".into()),
        ..radiko::app::playback::Update::default()
    });
    assert!(playing.output_guide.is_none());
    assert!(!playing.playback_stopped);
    let continued = content_rows(&mut playing, 80, 24).join("\n");
    assert!(continued.contains("再生中:"), "{continued}");
    assert!(
        !continued.contains(super::super::messages::OUTPUT_NO_DEVICE),
        "{continued}"
    );
}

#[test]
fn output_guide_stays_until_escape_or_the_next_playback_and_auth_stop_wins() {
    let mut search =
        Service::new(|| -> Result<CatalogClient, _> { panic!("guide must not fetch") });
    let mut view = super::screen();
    show_output_stop(
        &mut view,
        &device_error(radiko::app::playback::OutputFailure::Stalled),
    );
    let lines = guide_lines(radiko::app::playback::OutputFailure::Stalled);
    view.apply_playback(radiko::app::playback::Update::default());
    assert_exact_rows(&mut view, lines);
    assert!(
        !view
            .key(
                KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
                &mut search
            )
            .unwrap()
    );
    assert!(view.playback_action.is_none());
    assert_exact_rows(&mut view, lines);

    let program = radiko::api::catalog::parse_programs(
        include_bytes!("../../../../../tests/fixtures/schedule.xml"),
        "TBS",
        radiko::api::catalog::parse_day("20261015").unwrap(),
    )
    .unwrap()
    .programs
    .remove(0);
    view.app.programs = vec![program];
    view.app.visible = vec![0];
    view.list.select(Some(0));
    assert!(
        !view
            .key(
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                &mut search
            )
            .unwrap()
    );
    assert!(view.playback_action.is_some());
    assert_exact_rows(&mut view, lines);
    view.playback_action = None;

    assert!(
        !view
            .key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut search)
            .unwrap()
    );
    assert!(view.output_guide.is_none());
    assert!(view.playback_action.is_none());
    assert!(view.playback_stopped);
    let cleared = content_rows(&mut view, 80, 24);
    assert!(
        cleared.iter().any(|row| row == "再生中: なし / 音量25%"),
        "{cleared:?}"
    );
    assert!(cleared.iter().all(|row| row != lines[0]));

    show_output_stop(
        &mut view,
        &device_error(radiko::app::playback::OutputFailure::NoDevice),
    );
    view.apply_playback(radiko::app::playback::Update {
        new_session: true,
        snapshot: Some(playing_snapshot()),
        ..radiko::app::playback::Update::default()
    });
    assert!(view.output_guide.is_none());
    assert!(view.stop_cause.is_none());
    let playing = content_rows(&mut view, 160, 40).join("\n");
    assert!(playing.contains("再生中:"), "{playing}");
    assert!(
        !playing.contains(super::super::messages::OUTPUT_NO_DEVICE),
        "{playing}"
    );

    let mut both = super::screen();
    both.apply_playback(radiko::app::playback::Update {
        stopped: true,
        ended: true,
        stop_reason: Some(radiko::app::playback::StopReason::AuthExpired),
        output_failure: Some(radiko::app::playback::OutputFailure::Device),
        notice_kind: Some(radiko::app::playback::NoticeKind::AuthExpired),
        notice: Some(failure_notice(&PlaybackError::Source(
            radiko::api::stream::ProbeError::AuthExpired,
        ))),
        ..radiko::app::playback::Update::default()
    });
    assert_eq!(both.stop_cause, Some(super::super::StopCause::AuthExpired));
    assert!(both.output_guide.is_none());
    let auth = content_rows(&mut both, 80, 24);
    assert!(
        auth.iter()
            .any(|row| row == "停止中：認証期限切れ（a で再確認）"),
        "{auth:?}"
    );
    assert!(
        auth.iter()
            .all(|row| row != super::super::messages::OUTPUT_DEVICE)
    );
    assert!(
        !both
            .key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut search)
            .unwrap()
    );
    let kept = content_rows(&mut both, 160, 40);
    assert!(
        kept.iter()
            .any(|row| row == "停止中：認証期限切れ（a で再確認）"),
        "{kept:?}"
    );

    let mut later = super::screen();
    show_output_stop(
        &mut later,
        &device_error(radiko::app::playback::OutputFailure::Device),
    );
    assert!(later.output_guide.is_some());
    later.apply_playback(radiko::app::playback::Update {
        stopped: true,
        ended: true,
        stop_reason: Some(radiko::app::playback::StopReason::ReconnectLimit),
        notice: Some(radiko::player::playback::PlaybackError::ReconnectGaveUp.to_string()),
        ..radiko::app::playback::Update::default()
    });
    assert!(later.output_guide.is_none());
    assert_eq!(
        later.stop_cause,
        Some(super::super::StopCause::ReconnectLimit)
    );
    let replaced = content_rows(&mut later, 80, 24);
    assert!(
        replaced
            .iter()
            .any(|row| row == "停止中：再接続できませんでした（番組を選び直してください）"),
        "{replaced:?}"
    );
}

#[test]
fn continued_playback_does_not_say_that_playback_stopped() {
    let cli_auth = format!(
        "再生準備失敗（現在の状態を維持）: {}",
        radiko::api::stream::ProbeError::AuthExpired
    );
    assert!(cli_auth.contains("再生を停止しました"));
    let mut preparing = screen();
    preparing.app.playback = Some(playing_snapshot());
    preparing.apply_playback(radiko::app::playback::Update {
        snapshot: Some(playing_snapshot()),
        stopped: false,
        notice: Some(cli_auth),
        notice_kind: Some(radiko::app::playback::NoticeKind::AuthExpiredWhilePlaying),
        notice_prefix: Some("再生準備失敗（現在の状態を維持）"),
        ..radiko::app::playback::Update::default()
    });
    assert_eq!(
        preparing.playback_notice,
        "認証期限切れです。再生は続いています。a で再確認 / I で再ログイン"
    );
    assert!(!preparing.playback_notice.contains("再生を停止しました"));
    assert!(!preparing.playback_stopped);
    let shown = content_rows(&mut preparing, 160, 40).join("\n");
    assert!(shown.contains("再生中:"), "{shown}");
    assert!(shown.contains("再生は続いています"), "{shown}");
    assert!(!shown.contains("再生を停止しました"), "{shown}");

    let cli_switch = format!(
        "再生切替失敗（現在の状態を維持）: {}",
        radiko::player::playback::PlaybackError::ReconnectGaveUp
    );
    assert!(cli_switch.contains("再生を停止しました"));
    let mut switching = screen();
    switching.app.playback = Some(playing_snapshot());
    switching.apply_playback(radiko::app::playback::Update {
        snapshot: Some(playing_snapshot()),
        notice: Some(cli_switch),
        notice_kind: Some(radiko::app::playback::NoticeKind::ReconnectLimitWhilePlaying),
        notice_prefix: Some("再生切替失敗（現在の状態を維持）"),
        ..radiko::app::playback::Update::default()
    });
    assert_eq!(
        switching.playback_notice,
        "再接続の上限です。再生は続いています。番組を選び直してください"
    );
    assert!(!switching.playback_notice.contains("再生を停止しました"));
    let narrow = content_rows(&mut switching, 80, 24).join("\n");
    assert!(narrow.contains("再生は続いています"), "{narrow}");
    assert!(!narrow.contains("再生を停止しました"), "{narrow}");
    assert!(narrow.contains("再生中:"), "{narrow}");

    let mut stopped = screen();
    stopped.apply_playback(radiko::app::playback::Update {
        stopped: true,
        ended: true,
        stop_reason: Some(radiko::app::playback::StopReason::AuthExpired),
        notice_kind: Some(radiko::app::playback::NoticeKind::AuthExpired),
        notice: Some(failure_notice(&PlaybackError::Source(
            radiko::api::stream::ProbeError::AuthExpired,
        ))),
        ..radiko::app::playback::Update::default()
    });
    assert_eq!(
        stopped.playback_notice,
        super::super::messages::AUTH_EXPIRED_STOP
    );
    assert!(stopped.playback_notice.contains("再生を停止しました"));
}

#[test]
fn preparation_failure_with_nothing_playing_uses_the_stop_sentence() {
    let auth_stop = "認証期限切れのため再生を停止しました。a で再確認 / I で再ログイン";
    let reconnect_stop =
        "再接続の上限を超えたため再生を停止しました。番組を選び直して再生してください";
    let cases = [
        (
            radiko::app::playback::NoticeKind::AuthExpiredWhilePlaying,
            "再生準備失敗（現在の状態を維持）",
            format!(
                "再生準備失敗（現在の状態を維持）: {}",
                radiko::api::stream::ProbeError::AuthExpired
            ),
            format!("再生準備失敗（現在の状態を維持）: {auth_stop}"),
            auth_stop,
        ),
        (
            radiko::app::playback::NoticeKind::ReconnectLimitWhilePlaying,
            "再生準備失敗（現在の状態を維持）",
            format!(
                "再生準備失敗（現在の状態を維持）: {}",
                radiko::player::playback::PlaybackError::ReconnectGaveUp
            ),
            format!("再生準備失敗（現在の状態を維持）: {reconnect_stop}"),
            reconnect_stop,
        ),
    ];
    for (kind, prefix, cli, expected, stop_sentence) in cases {
        let mut fresh = screen();
        fresh.apply_playback(radiko::app::playback::Update {
            notice: Some(cli.clone()),
            notice_kind: Some(kind),
            notice_prefix: Some(prefix),
            ..radiko::app::playback::Update::default()
        });
        assert_eq!(fresh.playback_notice, expected);
        assert!(!fresh.playback_notice.contains("再生は続いています"));
        assert!(fresh.playback_notice.contains(stop_sentence));
        let narrow = content_rows(&mut fresh, 80, 24).join("\n");
        assert!(narrow.contains("再生中: なし"), "{narrow}");
        assert!(narrow.contains("再生を停止しました"), "{narrow}");
        assert!(!narrow.contains("再生は続いています"), "{narrow}");

        let mut after_stop = screen();
        after_stop.app.playback = Some(playing_snapshot());
        after_stop.apply_playback(radiko::app::playback::Update {
            ended: true,
            notice: Some("停止しました".into()),
            ..radiko::app::playback::Update::default()
        });
        assert!(after_stop.app.playback.is_none());
        after_stop.apply_playback(radiko::app::playback::Update {
            notice: Some(cli),
            notice_kind: Some(kind),
            notice_prefix: Some(prefix),
            ..radiko::app::playback::Update::default()
        });
        assert_eq!(after_stop.playback_notice, expected);
        assert!(!after_stop.playback_notice.contains("再生は続いています"));
        let again = content_rows(&mut after_stop, 80, 24).join("\n");
        assert!(again.contains("再生中: なし"), "{again}");
        assert!(!again.contains("再生は続いています"), "{again}");
    }
}

#[test]
fn help_quotes_the_output_guide_sentences() {
    let help = super::super::help::KEYS;
    for sentence in [
        super::super::messages::OUTPUT_NO_DEVICE,
        super::super::messages::OUTPUT_UNSUPPORTED,
        super::super::messages::OUTPUT_DEVICE,
        super::super::messages::OUTPUT_STALLED,
        super::super::messages::OUTPUT_NO_RETRY,
    ] {
        assert!(help.contains(sentence), "{sentence}");
    }
    assert!(help.contains("再生が続いているあいだは「再生を停止しました」を出しません。"));
    assert!(help.contains("番組は終了しています。再生は続いています"));
    assert!(help.contains("切り替えられませんでした。再生は続いています"));
    assert!(help.contains("切り替え先の音声が届きませんでした。再生は続いています"));
    assert!(help.contains("切り替え先の配信が途中で終わりました。再生は続いています"));
    assert!(help.contains("指定した位置は番組の範囲外です。再生は続いています"));
    assert!(help.contains("切り替え先の配信を取得できませんでした。再生は続いています"));
    assert!(help.contains(
        "再生準備失敗（現在の状態を維持）」も「再生切替失敗（現在の状態を維持）」も付けません。"
    ));
}

#[test]
fn deadline_and_probe_timeout_switch_text_follows_whether_playback_continues() {
    let prefix = "再生切替失敗（現在の状態を維持）";
    let cases = [
        (
            radiko::app::playback::NoticeKind::DeadlineWhilePlaying,
            "番組終了：放送終了日時に達したため音声を停止しました",
            "番組は終了しています。再生は続いています",
        ),
        (
            radiko::app::playback::NoticeKind::ProbeTimeoutWhilePlaying,
            "無音出力診断の実時間上限を超えたため停止しました",
            "切り替えられませんでした。再生は続いています",
        ),
    ];
    for (kind, stop_body, continues) in cases {
        let raw = format!("{prefix}: {stop_body}");
        let mut playing = super::screen();
        playing.app.playback = Some(playing_snapshot());
        playing.apply_playback(radiko::app::playback::Update {
            snapshot: Some(playing_snapshot()),
            notice: Some(raw.clone()),
            notice_kind: Some(kind),
            notice_prefix: Some(prefix),
            ..radiko::app::playback::Update::default()
        });
        assert_eq!(playing.playback_notice, continues);
        assert!(!playing.playback_notice.contains("音声を停止しました"));
        assert!(!playing.playback_notice.contains("停止しました"));
        let shown = content_rows(&mut playing, 160, 40).join("\n");
        assert!(shown.contains("再生中:"), "{shown}");
        assert!(shown.contains(continues), "{shown}");
        assert!(!shown.contains("音声を停止しました"), "{shown}");
        assert!(!shown.contains("停止しました"), "{shown}");

        let mut idle = super::screen();
        idle.apply_playback(radiko::app::playback::Update {
            notice: Some(raw.clone()),
            notice_kind: Some(kind),
            notice_prefix: Some(prefix),
            ..radiko::app::playback::Update::default()
        });
        assert_eq!(idle.playback_notice, raw);
        assert!(idle.app.playback.is_none());
        let narrow = content_rows(&mut idle, 80, 24).join("\n");
        assert!(narrow.contains("再生中: なし"), "{narrow}");
        assert!(!narrow.contains(continues), "{narrow}");
    }
}

#[test]
fn prep_failure_while_playing_replaces_the_stop_sentence() {
    let prefix = "再生準備失敗（現在の状態を維持）";
    let cases = [
        (
            radiko::app::playback::NoticeKind::SourceStalledWhilePlaying,
            "30秒間新しいセグメントを確認できないため停止しました",
            "切り替え先の音声が届きませんでした。再生は続いています",
        ),
        (
            radiko::app::playback::NoticeKind::EarlyEndWhilePlaying,
            "放送枠末尾より前に配信が終了しました",
            "切り替え先の配信が途中で終わりました。再生は続いています",
        ),
        (
            radiko::app::playback::NoticeKind::StreamFailedWhilePlaying,
            "配信取得: 配信の検証中に通信に失敗しました",
            "切り替えられませんでした。再生は続いています",
        ),
    ];
    for (kind, stop_body, continues) in cases {
        let raw = format!("{prefix}: {stop_body}");
        let mut playing = super::screen();
        playing.app.playback = Some(playing_snapshot());
        playing.apply_playback(radiko::app::playback::Update {
            snapshot: Some(playing_snapshot()),
            notice: Some(raw.clone()),
            notice_kind: Some(kind),
            notice_prefix: Some(prefix),
            ..radiko::app::playback::Update::default()
        });
        assert_eq!(playing.playback_notice, continues);
        assert!(
            !playing.playback_notice.contains("停止しました"),
            "{continues}"
        );
        let shown = content_rows(&mut playing, 160, 40).join("\n");
        assert!(shown.contains("再生中:"), "{shown}");
        assert!(shown.contains(continues), "{shown}");
        assert!(!shown.contains("停止しました"), "{shown}");

        let mut idle = super::screen();
        idle.apply_playback(radiko::app::playback::Update {
            notice: Some(raw.clone()),
            notice_kind: Some(kind),
            notice_prefix: Some(prefix),
            ..radiko::app::playback::Update::default()
        });
        assert_eq!(idle.playback_notice, raw);
        assert!(idle.app.playback.is_none());
        assert!(!idle.playback_notice.contains("再生は続いています"));
        let narrow = content_rows(&mut idle, 80, 24).join("\n");
        assert!(narrow.contains("再生中: なし"), "{narrow}");
        assert!(!narrow.contains(continues), "{narrow}");
    }
}

fn assert_full_notice_row(screen: &mut Screen, width: u16, height: u16, sentence: &str) {
    let rows = content_rows(screen, width, height);
    assert!(
        rows.iter().any(|row| row == sentence),
        "{width}x{height} did not show {sentence:?} as one full row\n{}",
        rows.join("\n")
    );
}

#[test]
fn while_playing_notices_fit_one_row_without_the_failure_prefix() {
    let playing = [
        (
            radiko::app::playback::NoticeKind::AuthExpiredWhilePlaying,
            "認証期限切れです。再生は続いています。a で再確認 / I で再ログイン",
            "再生準備失敗（現在の状態を維持）",
        ),
        (
            radiko::app::playback::NoticeKind::ReconnectLimitWhilePlaying,
            "再接続の上限です。再生は続いています。番組を選び直してください",
            "再生切替失敗（現在の状態を維持）",
        ),
        (
            radiko::app::playback::NoticeKind::DeadlineWhilePlaying,
            "番組は終了しています。再生は続いています",
            "再生切替失敗（現在の状態を維持）",
        ),
        (
            radiko::app::playback::NoticeKind::ProbeTimeoutWhilePlaying,
            "切り替えられませんでした。再生は続いています",
            "再生切替失敗（現在の状態を維持）",
        ),
        (
            radiko::app::playback::NoticeKind::SourceStalledWhilePlaying,
            "切り替え先の音声が届きませんでした。再生は続いています",
            "再生準備失敗（現在の状態を維持）",
        ),
        (
            radiko::app::playback::NoticeKind::EarlyEndWhilePlaying,
            "切り替え先の配信が途中で終わりました。再生は続いています",
            "再生準備失敗（現在の状態を維持）",
        ),
        (
            radiko::app::playback::NoticeKind::StreamFailedWhilePlaying,
            "切り替えられませんでした。再生は続いています",
            "再生準備失敗（現在の状態を維持）",
        ),
        (
            radiko::app::playback::NoticeKind::InvalidPositionWhilePlaying,
            "指定した位置は番組の範囲外です。再生は続いています",
            "再生準備失敗（現在の状態を維持）",
        ),
        (
            radiko::app::playback::NoticeKind::HttpWhilePlaying,
            "切り替え先の配信を取得できませんでした。再生は続いています",
            "再生準備失敗（現在の状態を維持）",
        ),
    ];
    for (kind, sentence, prefix) in playing {
        let mut screen = super::screen();
        screen.app.playback = Some(playing_snapshot());
        screen.apply_playback(radiko::app::playback::Update {
            snapshot: Some(playing_snapshot()),
            notice: Some(format!("{prefix}: 配信取得: 合成の失敗")),
            notice_kind: Some(kind),
            notice_prefix: Some(prefix),
            ..radiko::app::playback::Update::default()
        });
        assert_eq!(screen.playback_notice, sentence);
        assert!(!screen.playback_notice.starts_with(prefix), "{sentence}");
        for (width, height) in [(80u16, 24u16), (160, 40)] {
            assert_full_notice_row(&mut screen, width, height, sentence);
        }
    }

    let mut named = super::screen();
    named.app.playback = Some(playing_snapshot());
    named.apply_playback(radiko::app::playback::Update {
        snapshot: Some(playing_snapshot()),
        notice: Some(
            "再生準備失敗（現在の状態を維持）: 未ログインです。loginでログインしてください".into(),
        ),
        notice_kind: Some(radiko::app::playback::NoticeKind::LoginRequired),
        notice_prefix: Some("再生準備失敗（現在の状態を維持）"),
        ..radiko::app::playback::Update::default()
    });
    assert_eq!(
        named.playback_notice,
        "再生準備失敗（現在の状態を維持）: 未ログインです。I でログイン"
    );

    let idle_cases = [
        (
            radiko::app::playback::NoticeKind::SourceStalledWhilePlaying,
            "再生準備失敗（現在の状態を維持）: 30秒間新しいセグメントを確認できないため停止しました",
        ),
        (
            radiko::app::playback::NoticeKind::HttpWhilePlaying,
            "再生準備失敗（現在の状態を維持）: 配信取得: 配信サーバーがHTTP 503を返しました",
        ),
        (
            radiko::app::playback::NoticeKind::InvalidPositionWhilePlaying,
            "再生切替失敗（現在の状態を維持）: 配信取得: シーク位置は番組冒頭以上・放送枠末尾未満で指定してください",
        ),
    ];
    for (kind, raw) in idle_cases {
        let mut idle = super::screen();
        idle.apply_playback(radiko::app::playback::Update {
            notice: Some(raw.to_owned()),
            notice_kind: Some(kind),
            notice_prefix: Some(if raw.starts_with("再生切替失敗") {
                "再生切替失敗（現在の状態を維持）"
            } else {
                "再生準備失敗（現在の状態を維持）"
            }),
            ..radiko::app::playback::Update::default()
        });
        assert_eq!(idle.playback_notice, raw);
        assert!(idle.app.playback.is_none());
        let rows = content_rows(&mut idle, 80, 24);
        assert!(
            rows.iter()
                .any(|row| row.starts_with("再生準備失敗（現在の状態を維持）:")
                    || row.starts_with("再生切替失敗（現在の状態を維持）:")),
            "{rows:?}"
        );
    }
}

fn playing_snapshot() -> radiko::player::state::PlaybackSnapshot {
    let start = radiko::api::stream::parse_time("20261015120000").unwrap();
    radiko::player::state::PlaybackSnapshot {
        pipeline: None,
        info: radiko::api::stream::playback::StreamInfo {
            title: Some("合成テスト番組".into()),
            station: "TBS".into(),
            mode: radiko::api::stream::ProbeMode::TimeFree,
            starts_at: start,
            ends_at: start + chrono::Duration::hours(1),
            position: std::time::Duration::ZERO,
        },
        state: PlaybackState::Playing,
        pending: None,
        volume: 25,
        position: Some(std::time::Duration::from_secs(12)),
        output: None,
        segments: 1,
        receiving_for: None,
        reconnecting_for: None,
    }
}
