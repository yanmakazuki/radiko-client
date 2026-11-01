use super::*;
use radiko::player::playback::{PlaybackError, failure_notice};

fn buffer(screen: &mut Screen) -> String {
    let mut terminal = Terminal::new(TestBackend::new(180, 40)).unwrap();
    terminal
        .draw(|frame| draw(frame, screen, Utc::now().fixed_offset()))
        .unwrap();
    let area = terminal.backend().buffer().area;
    let width = usize::from(area.width);
    terminal
        .backend()
        .buffer()
        .content
        .chunks(width)
        .map(|row| {
            let mut line = String::new();
            let mut x = 0;
            while x < row.len() {
                let symbol = row[x].symbol();
                line.push_str(symbol);
                x += ratatui::text::Span::raw(symbol).width().max(1);
            }
            line.trim_end().to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn assert_no_cli_commands(text: &str) {
    assert!(!text.contains("loginで"), "{text}");
    assert!(!text.contains("login で"), "{text}");
    assert!(!text.contains("sessionで"), "{text}");
    assert!(!text.contains("session で"), "{text}");
}

fn show_failure(screen: &mut Screen, error: &PlaybackError) {
    screen.apply_playback(radiko::app::playback::Update {
        stopped: true,
        ended: true,
        stop_reason: radiko::app::playback::stop_reason(error),
        notice_kind: radiko::app::playback::notice_kind(error),
        notice: Some(failure_notice(error)),
        ..radiko::app::playback::Update::default()
    });
}

#[test]
fn tui_auth_screens_use_keys_instead_of_cli_commands() {
    let not_logged_in = super::super::messages::NOT_LOGGED_IN;
    let session_expired = super::super::messages::SESSION_EXPIRED;
    let auth_stop = super::super::messages::AUTH_EXPIRED_STOP;
    let stream_auth = super::super::messages::STREAM_AUTHENTICATION;
    let outside = super::super::messages::OUTSIDE_AREA;
    let contract = radiko::api::stream::ProbeError::ContractRequired.to_string();

    let mut signed_out = screen();
    signed_out.app.authentication.status = radiko::app::auth::Status::SignedOut;
    let signed_out = buffer(&mut signed_out);
    assert!(signed_out.contains(not_logged_in), "{signed_out}");
    assert_no_cli_commands(&signed_out);

    let mut missing = screen();
    show_failure(&mut missing, &PlaybackError::LoginRequired);
    assert_eq!(missing.playback_notice, not_logged_in);
    let missing = buffer(&mut missing);
    assert!(missing.contains(not_logged_in), "{missing}");
    assert_no_cli_commands(&missing);

    let mut corrupt = screen();
    corrupt.app.authentication.status =
        radiko::app::auth::Status::StorageFailed(radiko::storage::StorageError::Invalid);
    let corrupt_text = buffer(&mut corrupt);
    assert!(
        corrupt_text.contains("セッションファイルが破損しています / a で再確認 / I で再ログイン"),
        "{corrupt_text}"
    );
    assert_no_cli_commands(&corrupt_text);

    let mut expired = screen();
    expired.app.authentication.status = radiko::app::auth::Status::Expired;
    let expired_text = buffer(&mut expired);
    assert!(expired_text.contains(session_expired), "{expired_text}");
    assert_no_cli_commands(&expired_text);

    let mut session = screen();
    show_failure(
        &mut session,
        &PlaybackError::Auth(radiko::api::auth::AuthError::SessionExpired),
    );
    assert_eq!(session.playback_notice, session_expired);
    let session_text = buffer(&mut session);
    assert!(session_text.contains(session_expired), "{session_text}");
    assert_no_cli_commands(&session_text);

    let mut denied = screen();
    show_failure(
        &mut denied,
        &PlaybackError::Source(radiko::api::stream::ProbeError::Denied),
    );
    assert!(denied.playback_notice.contains("アクセスを拒否"));
    let denied_text = buffer(&mut denied);
    assert!(
        denied_text.contains("アクセスを拒否しました"),
        "{denied_text}"
    );
    assert_no_cli_commands(&denied_text);

    let mut authentication = screen();
    show_failure(
        &mut authentication,
        &PlaybackError::Source(radiko::api::stream::ProbeError::At(
            "HLS一覧更新",
            Box::new(radiko::api::stream::ProbeError::Authentication),
        )),
    );
    assert_eq!(authentication.playback_notice, stream_auth);
    let authentication_text = buffer(&mut authentication);
    assert!(
        authentication_text.contains(stream_auth),
        "{authentication_text}"
    );
    assert_no_cli_commands(&authentication_text);

    let mut stopped = screen();
    show_failure(
        &mut stopped,
        &PlaybackError::Source(radiko::api::stream::ProbeError::At(
            "HLS音声取得",
            Box::new(radiko::api::stream::ProbeError::AuthExpired),
        )),
    );
    assert_eq!(stopped.playback_notice, auth_stop);
    assert_eq!(
        stopped.stop_cause,
        Some(super::super::StopCause::AuthExpired)
    );
    assert_eq!(
        super::super::StopCause::AuthExpired.player_line(&stopped.app.authentication.status),
        "停止中：認証期限切れ（a で再確認）"
    );
    let stopped_text = buffer(&mut stopped);
    assert!(stopped_text.contains(auth_stop), "{stopped_text}");
    assert!(
        stopped_text.contains("停止中：認証期限切れ（a で再確認）"),
        "{stopped_text}"
    );
    assert_no_cli_commands(&stopped_text);

    let mut save_failed = screen();
    save_failed.app.authentication.status = radiko::app::auth::Status::LoginSaveFailed(
        radiko::storage::StorageError::Io(std::io::ErrorKind::PermissionDenied),
    );
    let save_text = buffer(&mut save_failed);
    assert!(
        save_text.contains("ログイン後のセッション保存失敗"),
        "{save_text}"
    );
    assert!(
        save_text.contains("セッションファイルの操作に失敗しました"),
        "{save_text}"
    );
    assert_no_cli_commands(&save_text);

    let mut outside_area = screen();
    show_failure(
        &mut outside_area,
        &PlaybackError::Source(radiko::api::stream::ProbeError::OutsideArea),
    );
    assert_eq!(outside_area.playback_notice, outside);
    let outside_text = buffer(&mut outside_area);
    assert!(outside_text.contains(outside), "{outside_text}");
    assert!(!outside_text.contains(&contract), "{outside_text}");
    assert_no_cli_commands(&outside_text);

    let mut in_area = screen();
    show_failure(
        &mut in_area,
        &PlaybackError::Source(radiko::api::stream::ProbeError::ContractRequired),
    );
    assert!(in_area.playback_notice.contains(&contract));
    assert!(!in_area.playback_notice.contains("エリア外"));
    let in_area_text = buffer(&mut in_area);
    assert!(in_area_text.contains(&contract), "{in_area_text}");
    assert!(!in_area_text.contains(outside), "{in_area_text}");
    assert_no_cli_commands(&in_area_text);
}

#[test]
fn playback_wording_follows_the_error_kind_and_not_the_notice_text() {
    let cli = "未ログインです。loginでログインしてください";
    let mut typed = screen();
    typed.apply_playback(radiko::app::playback::Update {
        stopped: true,
        notice_kind: Some(radiko::app::playback::NoticeKind::LoginRequired),
        notice: Some(cli.to_owned()),
        ..radiko::app::playback::Update::default()
    });
    assert_eq!(typed.playback_notice, super::super::messages::NOT_LOGGED_IN);
    assert_no_cli_commands(&buffer(&mut typed));

    let mut preparing = screen();
    preparing.apply_playback(radiko::app::playback::Update {
        notice: Some(
            "再生準備失敗（現在の状態を維持）: 未ログインです。loginでログインしてください".into(),
        ),
        notice_kind: Some(radiko::app::playback::NoticeKind::LoginRequired),
        notice_prefix: Some("再生準備失敗（現在の状態を維持）"),
        ..radiko::app::playback::Update::default()
    });
    assert_eq!(
        preparing.playback_notice,
        "再生準備失敗（現在の状態を維持）: 未ログインです。I でログイン"
    );
    assert_no_cli_commands(&buffer(&mut preparing));

    let mut untyped = screen();
    untyped.apply_playback(radiko::app::playback::Update {
        stopped: true,
        notice: Some(cli.to_owned()),
        ..radiko::app::playback::Update::default()
    });
    assert_eq!(untyped.playback_notice, cli);
}

#[test]
fn final_auth_expired_result_is_the_tui_stop_sentence() {
    let program = radiko::api::catalog::parse_programs(
        include_bytes!("../../../../../tests/fixtures/schedule.xml"),
        "TBS",
        radiko::api::catalog::parse_day("20261015").unwrap(),
    )
    .unwrap()
    .programs
    .remove(0);
    let update =
        radiko::app::playback::Service::auth_expired_final_update(&program, program.ends_at);
    let mut screen = screen();
    screen.apply_playback(update);
    assert_eq!(
        screen.playback_notice,
        "認証期限切れのため再生を停止しました。a で再確認 / I で再ログイン"
    );
}

#[test]
fn paired_key_guidance_matches_the_literals_with_recheck_before_login() {
    assert_eq!(
        super::super::messages::SESSION_EXPIRED,
        "セッション失効。a で再確認 / I で再ログイン"
    );
    assert_eq!(
        super::super::messages::AUTH_EXPIRED_STOP,
        "認証期限切れのため再生を停止しました。a で再確認 / I で再ログイン"
    );
    assert_eq!(
        super::super::messages::STREAM_AUTHENTICATION,
        "配信認証が拒否されました。a で再確認 / I で再ログイン"
    );
    let help = super::super::help::KEYS;
    let help_line = |prefix: &str| {
        help.lines()
            .find(|line| line.starts_with(prefix))
            .unwrap_or_else(|| panic!("missing help line: {prefix}"))
    };
    assert_eq!(
        help_line("未ログインは"),
        "未ログインは「未ログインです。I でログイン」。二つのキーを並べる案内は「a で再確認 / I で再ログイン」の順です。"
    );
    assert_eq!(
        help_line("セッション失効は"),
        "セッション失効は「セッション失効。a で再確認 / I で再ログイン」。"
    );
    assert_eq!(
        help_line("認証期限切れで止めた通知は"),
        "認証期限切れで止めた通知は「認証期限切れのため再生を停止しました。a で再確認 / I で再ログイン」。"
    );
    assert_eq!(
        radiko::app::auth::Status::LoginFailed(radiko::api::auth::AuthError::InvalidCredentials)
            .detail(),
        "ログインに失敗しました。メールアドレスとパスワードを確認してください / a で再確認 / I で再ログイン"
    );
    assert_eq!(
        radiko::app::auth::Status::SignedOut.detail(),
        "a で再確認 / I でログインしてください"
    );
    assert_eq!(
        radiko::app::auth::Status::Expired.detail(),
        "a で再確認 / I で再ログインしてください"
    );
}

#[test]
fn auth_expired_tui_notice_survives_a_later_reconnect_snapshot() {
    let mut screen = screen();
    show_failure(
        &mut screen,
        &PlaybackError::Source(radiko::api::stream::ProbeError::AuthExpired),
    );
    let sentence = screen.playback_notice.clone();
    assert_eq!(sentence, super::super::messages::AUTH_EXPIRED_STOP);
    screen.apply_playback(radiko::app::playback::Update {
        snapshot: None,
        ..radiko::app::playback::Update::default()
    });
    assert_eq!(screen.playback_notice, sentence);
    assert_eq!(
        screen.stop_cause,
        Some(super::super::StopCause::AuthExpired)
    );
}

fn visible_lines(screen: &mut Screen, width: u16, height: u16) -> Vec<String> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| draw(frame, screen, Utc::now().fixed_offset()))
        .unwrap();
    let area = terminal.backend().buffer().area;
    let columns = usize::from(area.width);
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
            line.trim_end().to_owned()
        })
        .collect()
}

#[test]
fn login_failure_rechecks_before_login_and_keeps_a_visible_at_80_columns() {
    let error = radiko::api::auth::AuthError::InvalidCredentials;
    let sentence = format!("{error} / a で再確認 / I で再ログイン");
    let mut screen = screen();
    screen.app.authentication.status = radiko::app::auth::Status::LoginFailed(error);
    let wide = visible_lines(&mut screen, 180, 40).join("\n");
    assert!(wide.contains(&sentence), "{wide}");
    let narrow = visible_lines(&mut screen, 80, 24);
    let detail = narrow
        .iter()
        .find(|line| line.contains("ログインに失敗しました"))
        .unwrap_or_else(|| panic!("login failure was clipped:\n{}", narrow.join("\n")));
    assert!(
        detail.contains("a で再確認") || detail.contains(" / a"),
        "a is not visible at 80 columns: {detail}"
    );
}
