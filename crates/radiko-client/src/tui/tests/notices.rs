use super::*;
use radiko::player::playback::{PlaybackError, failure_notice};
use ratatui::style::{Color, Modifier};
use std::time::Duration;

fn playing() -> radiko::player::state::PlaybackSnapshot {
    let start = radiko::api::stream::parse_time("20261015120000").unwrap();
    radiko::player::state::PlaybackSnapshot {
        pipeline: None,
        info: radiko::api::stream::playback::StreamInfo {
            title: Some("合成テスト番組".into()),
            station: "TBS".into(),
            mode: radiko::api::stream::ProbeMode::TimeFree,
            starts_at: start,
            ends_at: start + chrono::Duration::hours(1),
            position: Duration::ZERO,
        },
        state: PlaybackState::Playing,
        pending: None,
        volume: 25,
        position: Some(Duration::from_secs(12)),
        output: None,
        segments: 1,
        receiving_for: None,
        reconnecting_for: None,
    }
}

fn rows(screen: &mut Screen, width: u16, height: u16) -> Vec<String> {
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

fn row_style(
    screen: &mut Screen,
    width: u16,
    height: u16,
    starts: &str,
) -> (String, Color, Color, bool) {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| draw(frame, screen, Utc::now().fixed_offset()))
        .unwrap();
    let columns = usize::from(terminal.backend().buffer().area.width);
    for row in terminal.backend().buffer().content.chunks(columns) {
        let mut line = String::new();
        let mut x = 0;
        while x < row.len() {
            let symbol = row[x].symbol();
            line.push_str(symbol);
            x += ratatui::text::Span::raw(symbol).width().max(1);
        }
        let text = line.strip_prefix('│').unwrap_or(&line);
        let text = text.strip_suffix('│').unwrap_or(text).trim_end().to_owned();
        if text.contains(starts) {
            let cell = row.iter().find(|cell| cell.symbol() != "│").unwrap();
            return (
                text,
                cell.fg,
                cell.bg,
                cell.modifier.contains(Modifier::BOLD),
            );
        }
    }
    panic!("no row contains {starts:?}");
}

fn continue_update(
    kind: radiko::app::playback::NoticeKind,
    raw: &str,
) -> radiko::app::playback::Update {
    radiko::app::playback::Update {
        snapshot: Some(playing()),
        notice: Some(raw.into()),
        notice_kind: Some(kind),
        notice_prefix: Some("再生切替失敗（現在の状態を維持）"),
        stopped: false,
        ..radiko::app::playback::Update::default()
    }
}

#[test]
fn while_playing_kinds_are_warnings_and_stops_are_errors() {
    use radiko::app::playback::NoticeKind;
    let warnings = [
        NoticeKind::AuthExpiredWhilePlaying,
        NoticeKind::ReconnectLimitWhilePlaying,
        NoticeKind::DeadlineWhilePlaying,
        NoticeKind::ProbeTimeoutWhilePlaying,
        NoticeKind::SourceStalledWhilePlaying,
        NoticeKind::EarlyEndWhilePlaying,
        NoticeKind::StreamFailedWhilePlaying,
        NoticeKind::InvalidPositionWhilePlaying,
        NoticeKind::HttpWhilePlaying,
    ];
    assert_eq!(warnings.len(), 9);
    for kind in warnings {
        assert!(kind.while_playing(), "{kind:?}");
        assert_eq!(
            super::super::notices::level_from_kind(kind),
            super::super::notices::NoticeLevel::Warning,
            "{kind:?} is a warning because playback continues"
        );
    }
    assert_eq!(
        super::super::notices::level_from_kind(NoticeKind::AuthExpired),
        super::super::notices::NoticeLevel::Error
    );
    assert!(
        !NoticeKind::AuthExpired.while_playing(),
        "a true auth-expiry stop is not a while-playing kind"
    );
}

#[test]
fn notice_cells_use_the_level_from_the_kind() {
    use radiko::app::playback::NoticeKind;
    let raw = format!(
        "再生切替失敗（現在の状態を維持）: {}",
        radiko::api::stream::ProbeError::AuthExpired
    );
    assert!(
        raw.contains("再生を停止しました"),
        "the library sentence still contains the stop words: {raw}"
    );
    let mut playing_screen = screen();
    playing_screen.app.playback = Some(playing());
    playing_screen.apply_playback(continue_update(NoticeKind::AuthExpiredWhilePlaying, &raw));
    assert_eq!(
        playing_screen.playback_notice,
        "認証期限切れです。再生は続いています。a で再確認 / I で再ログイン"
    );
    let (text, fg, bg, bold) = row_style(&mut playing_screen, 80, 24, "再生は続いています");
    assert_eq!(
        text, "! 認証期限切れです。再生は続いています。a で再確認 / I で再ログイン",
        "warning mark and sentence must occupy one row"
    );
    assert_eq!(
        fg,
        Color::Yellow,
        "AuthExpiredWhilePlaying is yellow, not {fg:?}"
    );
    assert_eq!(bg, Color::Reset, "warning has no background");
    assert!(!bold, "warnings are not bold");
    assert_eq!(
        playing_screen.notice_level,
        super::super::notices::NoticeLevel::Warning
    );

    let mut stopped = screen();
    stopped.apply_playback(radiko::app::playback::Update {
        stopped: true,
        ended: true,
        stop_reason: Some(radiko::app::playback::StopReason::AuthExpired),
        notice_kind: Some(NoticeKind::AuthExpired),
        notice: Some(failure_notice(&PlaybackError::Source(
            radiko::api::stream::ProbeError::AuthExpired,
        ))),
        ..radiko::app::playback::Update::default()
    });
    assert_eq!(
        stopped.notice_level,
        super::super::notices::NoticeLevel::Error
    );
    assert!(stopped.playback_notice.contains("再生を停止しました"));
    let (text, fg, bg, bold) = row_style(&mut stopped, 80, 24, "✕");
    assert!(
        text.starts_with("✕ 認証期限切れのため再生を停止しました"),
        "{text}"
    );
    assert_eq!(fg, Color::Red, "a real stop is red, not {fg:?}");
    assert_eq!(bg, Color::Reset, "error has no background");
    assert!(bold, "errors are bold");

    let mut info = screen();
    info.show_notice("一時停止");
    assert_eq!(info.notice_level, super::super::notices::NoticeLevel::Info);
    let (text, fg, bg, bold) = row_style(&mut info, 80, 24, "一時停止");
    assert_eq!(text, "一時停止");
    assert_eq!(fg, Color::Reset, "info keeps the default foreground");
    assert_eq!(bg, Color::Reset);
    assert!(!bold);
    assert!(!text.starts_with('✕') && !text.starts_with('!'));
}

#[test]
fn no_color_drops_foreground_and_background_but_keeps_the_marks() {
    use radiko::app::playback::NoticeKind;
    assert!(!super::super::notices::no_color_enabled(None));
    assert!(!super::super::notices::no_color_enabled(Some(
        std::ffi::OsStr::new("")
    )));
    assert!(super::super::notices::no_color_enabled(Some(
        std::ffi::OsStr::new("1")
    )));
    let raw = format!(
        "再生準備失敗（現在の状態を維持）: {}",
        PlaybackError::ReconnectGaveUp
    );
    let mut colored = screen();
    colored.no_color = false;
    colored.app.playback = Some(playing());
    colored.apply_playback(continue_update(
        NoticeKind::ReconnectLimitWhilePlaying,
        &raw,
    ));
    let (_, fg, _, _) = row_style(&mut colored, 160, 40, "!");
    assert_eq!(fg, Color::Yellow);

    let mut plain = screen();
    plain.no_color = true;
    plain.app.playback = Some(playing());
    plain.apply_playback(continue_update(
        NoticeKind::ReconnectLimitWhilePlaying,
        &raw,
    ));
    let (text, fg, bg, bold) = row_style(&mut plain, 80, 24, "!");
    assert!(
        text.starts_with("! 再接続の上限です。再生は続いています"),
        "{text}"
    );
    assert_eq!(fg, Color::Reset, "NO_COLOR removes the warning foreground");
    assert_eq!(bg, Color::Reset, "NO_COLOR removes the warning background");
    assert!(!bold);

    let mut stopped = screen();
    stopped.no_color = true;
    stopped.apply_playback(radiko::app::playback::Update {
        stopped: true,
        ended: true,
        notice_kind: Some(NoticeKind::AuthExpired),
        notice: Some(super::super::messages::AUTH_EXPIRED_STOP.into()),
        ..radiko::app::playback::Update::default()
    });
    let (text, fg, bg, bold) = row_style(&mut stopped, 160, 40, "✕");
    assert!(text.contains("再生を停止しました"), "{text}");
    assert_eq!(fg, Color::Reset, "NO_COLOR removes the error foreground");
    assert_eq!(bg, Color::Reset, "NO_COLOR removes the error background");
    assert!(!bold, "NO_COLOR removes bold as well as color");
}

#[test]
fn longest_while_playing_notice_with_the_mark_is_one_row() {
    let sentence = "認証期限切れです。再生は続いています。a で再確認 / I で再ログイン";
    let marked = format!("! {sentence}");
    let columns = ratatui::text::Span::raw(&marked).width();
    assert_eq!(
        columns, 67,
        "the mark and the space are 2 columns; the sentence plus them is {columns}"
    );
    let mut screen = screen();
    screen.app.playback = Some(playing());
    screen.apply_playback(continue_update(
        radiko::app::playback::NoticeKind::AuthExpiredWhilePlaying,
        "再生切替失敗（現在の状態を維持）: 合成",
    ));
    for (width, height) in [(80u16, 24u16), (160, 40)] {
        let shown = rows(&mut screen, width, height);
        assert!(
            shown.iter().any(|row| row == &marked),
            "{width}x{height} did not keep {marked:?} on one row\n{}",
            shown.join("\n")
        );
    }
}

#[test]
fn warnings_ignore_the_clock_and_clear_on_the_next_key() {
    use radiko::app::playback::NoticeKind;
    let mut search = Service::new(|| -> Result<CatalogClient, _> { panic!("keys must not fetch") });
    let raised = std::time::Instant::now() + Duration::from_secs(86_400);
    let mut screen = screen();
    screen.set_notice_clock(raised);
    screen.app.playback = Some(playing());
    screen.apply_playback_at(
        continue_update(
            NoticeKind::HttpWhilePlaying,
            "再生準備失敗（現在の状態を維持）: 合成",
        ),
        raised,
    );
    let sentence = "切り替え先の配信を取得できませんでした。再生は続いています";
    assert_eq!(screen.playback_notice, sentence);
    assert_eq!(screen.notice_hold, NoticeHold::Warning);
    assert!(screen.notice_hold_until.is_none());
    let mut later = playing();
    later.receiving_for = Some(Duration::from_secs(4));
    screen.apply_playback_at(
        radiko::app::playback::Update {
            snapshot: Some(later),
            ..radiko::app::playback::Update::default()
        },
        raised + Duration::from_secs(10),
    );
    assert_eq!(
        screen.playback_notice, sentence,
        "a warning must still be shown 10s later, not 受信待ち"
    );
    assert!(
        !screen
            .key(
                KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
                &mut search
            )
            .unwrap()
    );
    assert!(screen.playback_notice.is_empty(), "j clears a warning");
    assert_ne!(screen.playback_notice, QUIT_HINT);
}

#[test]
fn errors_stay_through_navigation_and_clear_on_a_user_action() {
    let mut search = Service::new(|| -> Result<CatalogClient, _> { panic!("keys must not fetch") });
    let raised = std::time::Instant::now() + Duration::from_secs(86_400);
    let mut screen = screen();
    screen.set_notice_clock(raised);
    let sentence = super::super::messages::AUTH_EXPIRED_STOP;
    screen.apply_playback_at(
        radiko::app::playback::Update {
            stopped: true,
            ended: true,
            stop_reason: Some(radiko::app::playback::StopReason::AuthExpired),
            notice_kind: Some(radiko::app::playback::NoticeKind::AuthExpired),
            notice: Some(sentence.into()),
            ..radiko::app::playback::Update::default()
        },
        raised,
    );
    assert_eq!(
        screen.notice_level,
        super::super::notices::NoticeLevel::Error
    );
    let mut later = playing();
    later.receiving_for = Some(Duration::from_secs(8));
    screen.apply_playback_at(
        radiko::app::playback::Update {
            snapshot: Some(later),
            ..radiko::app::playback::Update::default()
        },
        raised + Duration::from_secs(30),
    );
    assert_eq!(screen.playback_notice, sentence);
    assert!(
        !screen
            .key(
                KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
                &mut search
            )
            .unwrap()
    );
    assert_eq!(
        screen.playback_notice, sentence,
        "j does not clear an error"
    );
    assert!(
        !screen
            .key(
                KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE),
                &mut search
            )
            .unwrap()
    );
    assert!(
        screen.playback_notice.is_empty(),
        "s is a user action and clears the error"
    );
    assert!(screen.playback_stopped);
}

#[test]
fn info_clears_on_the_next_key_before_the_hold_elapses() {
    let mut search = Service::new(|| -> Result<CatalogClient, _> { panic!("keys must not fetch") });
    let raised = std::time::Instant::now() + Duration::from_secs(86_400);
    let mut screen = screen();
    screen.set_notice_clock(raised);
    screen.show_notice("一時停止");
    assert_eq!(screen.notice_hold, NoticeHold::Action);
    assert!(
        !screen
            .key(
                KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE),
                &mut search
            )
            .unwrap()
    );
    assert!(screen.playback_notice.is_empty());
    assert_ne!(screen.playback_notice, QUIT_HINT);
}

#[test]
fn one_notice_keeps_the_higher_level_and_otherwise_the_newer_one() {
    use radiko::app::playback::NoticeKind;
    let mut screen = screen();
    screen.app.playback = Some(playing());
    screen.apply_playback(continue_update(
        NoticeKind::EarlyEndWhilePlaying,
        "再生準備失敗（現在の状態を維持）: 合成",
    ));
    let warning = "切り替え先の配信が途中で終わりました。再生は続いています";
    assert_eq!(screen.playback_notice, warning);
    screen.preferences_notice = "「朝」を保存しました".into();
    screen.note_preferences(super::super::notices::NoticeLevel::Info);
    let shown = rows(&mut screen, 80, 24).join("\n");
    assert!(shown.contains(&format!("! {warning}")), "{shown}");
    assert!(
        !shown.contains("「朝」を保存しました"),
        "a newer info notice loses to a warning\n{shown}"
    );

    screen.preferences_notice.clear();
    screen.app.notification = "検索に失敗しました".into();
    screen.notification_level = super::super::notices::NoticeLevel::Warning;
    screen.notification_seq = screen.playback_seq + 1;
    let shown = rows(&mut screen, 160, 40).join("\n");
    assert!(shown.contains("! 検索に失敗しました"), "{shown}");
    assert!(
        !shown.contains(warning),
        "the newer warning replaces the older one\n{shown}"
    );

    let mut stopped = screen;
    stopped.app.notification.clear();
    stopped.playback_notice = super::super::messages::AUTH_EXPIRED_STOP.into();
    stopped.notice_level = super::super::notices::NoticeLevel::Error;
    stopped.playback_seq = 1;
    stopped.preferences_notice = "「朝」を保存しました".into();
    stopped.preferences_level = super::super::notices::NoticeLevel::Info;
    stopped.preferences_seq = 9;
    stopped.preferences_until = Some(std::time::Instant::now() + Duration::from_secs(5));
    let shown = rows(&mut stopped, 80, 24).join("\n");
    assert!(
        shown.contains("✕ 認証期限切れのため再生を停止しました"),
        "{shown}"
    );
    assert!(!shown.contains("「朝」を保存しました"), "{shown}");
}

#[test]
fn switch_or_seek_failure_while_playing_does_not_say_playback_stopped() {
    use radiko::app::playback::NoticeKind;
    let cases = [
        (
            "再生切替失敗（現在の状態を維持）",
            NoticeKind::AuthExpiredWhilePlaying,
            format!(
                "再生切替失敗（現在の状態を維持）: {}",
                radiko::api::stream::ProbeError::AuthExpired
            ),
            "認証期限切れです。再生は続いています。a で再確認 / I で再ログイン",
        ),
        (
            "再生準備失敗（現在の状態を維持）",
            NoticeKind::ReconnectLimitWhilePlaying,
            format!(
                "再生準備失敗（現在の状態を維持）: {}",
                PlaybackError::ReconnectGaveUp
            ),
            "再接続の上限です。再生は続いています。番組を選び直してください",
        ),
    ];
    for (prefix, kind, raw, sentence) in cases {
        assert!(
            raw.contains("再生を停止しました"),
            "the unchanged library line contains the stop sentence: {raw}"
        );
        let mut screen = screen();
        screen.app.playback = Some(playing());
        screen.apply_playback(radiko::app::playback::Update {
            snapshot: Some(playing()),
            notice: Some(raw),
            notice_kind: Some(kind),
            notice_prefix: Some(prefix),
            stopped: false,
            ..radiko::app::playback::Update::default()
        });
        assert_eq!(screen.playback_notice, sentence);
        assert!(
            !screen.playback_notice.contains("再生を停止しました"),
            "{sentence}"
        );
        assert_eq!(
            screen.notice_level,
            super::super::notices::NoticeLevel::Warning
        );
        for (width, height) in [(80u16, 24u16), (160, 40)] {
            let shown = rows(&mut screen, width, height).join("\n");
            assert!(shown.contains("再生中:"), "{shown}");
            assert!(shown.contains(&format!("! {sentence}")), "{shown}");
            assert!(
                !shown.contains("再生を停止しました"),
                "{width}x{height} showed the stop sentence while playback continued\n{shown}"
            );
        }
    }

    let mut stopped = screen();
    stopped.apply_playback(radiko::app::playback::Update {
        stopped: true,
        ended: true,
        stop_reason: Some(radiko::app::playback::StopReason::AuthExpired),
        notice_kind: Some(NoticeKind::AuthExpired),
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
    assert_eq!(
        stopped.notice_level,
        super::super::notices::NoticeLevel::Error
    );
    let shown = rows(&mut stopped, 80, 24).join("\n");
    assert!(shown.contains("再生を停止しました"), "{shown}");
    assert!(shown.contains("✕ "), "{shown}");

    let mut user_stopped = screen();
    user_stopped.app.playback = Some(playing());
    user_stopped.apply_playback(radiko::app::playback::Update {
        ended: true,
        stopped: false,
        snapshot: Some(playing()),
        notice: Some("停止しました".into()),
        ..radiko::app::playback::Update::default()
    });
    assert_eq!(user_stopped.playback_notice, "停止しました");
    assert!(!user_stopped.playback_notice.contains("再生を停止しました"));
    assert_eq!(
        user_stopped.notice_level,
        super::super::notices::NoticeLevel::Info
    );
    let shown = rows(&mut user_stopped, 80, 24).join("\n");
    assert!(shown.contains("停止しました"), "{shown}");
    assert!(!shown.contains("再生を停止しました"), "{shown}");
    assert!(!shown.contains("✕ "), "{shown}");

    let mut idle = screen();
    let raw = format!(
        "再生準備失敗（現在の状態を維持）: {}",
        radiko::api::stream::ProbeError::AuthExpired
    );
    idle.apply_playback(radiko::app::playback::Update {
        notice: Some(raw),
        notice_kind: Some(NoticeKind::AuthExpiredWhilePlaying),
        notice_prefix: Some("再生準備失敗（現在の状態を維持）"),
        ..radiko::app::playback::Update::default()
    });
    assert!(idle.playback_notice.contains("再生を停止しました"));
    assert!(!idle.playback_notice.contains("再生は続いています"));
    assert_eq!(idle.notice_level, super::super::notices::NoticeLevel::Error);
}

#[test]
fn output_device_failure_is_an_error_without_reading_the_sentence() {
    let mut screen = screen();
    screen.apply_playback(radiko::app::playback::Update {
        stopped: true,
        ended: true,
        output_failure: Some(radiko::app::playback::OutputFailure::NoDevice),
        notice: Some("再生停止: 既定の音声出力機器がありません".into()),
        ..radiko::app::playback::Update::default()
    });
    assert_eq!(
        screen.notice_level,
        super::super::notices::NoticeLevel::Error
    );
    let (text, fg, bg, bold) = row_style(&mut screen, 80, 24, "✕");
    assert!(text.contains("既定の音声出力機器がありません"), "{text}");
    assert_eq!(fg, Color::Red);
    assert_eq!(bg, Color::Reset);
    assert!(bold);
}

#[test]
fn informational_preferences_expire_on_the_injected_clock() {
    let raised = std::time::Instant::now() + Duration::from_secs(86_400);
    let mut screen = screen();
    screen.set_notice_clock(raised);
    screen.preferences_notice = "「朝」を保存しました".into();
    screen.note_preferences(super::super::notices::NoticeLevel::Info);
    screen.expire_preferences();
    assert!(screen.preferences_notice.contains("保存しました"));
    screen.set_notice_clock(raised + Duration::from_secs(5) + Duration::from_nanos(1));
    screen.expire_preferences();
    assert!(screen.preferences_notice.is_empty());
}

#[test]
fn idle_failures_are_errors_for_every_kind_and_for_a_kindless_failure() {
    use radiko::app::playback::NoticeKind;
    let prefix = "再生準備失敗（現在の状態を維持）";
    let cases = [
        (
            NoticeKind::LoginRequired,
            format!("{prefix}: 未ログインです。I でログイン"),
        ),
        (
            NoticeKind::SessionExpired,
            format!("{prefix}: セッション失効。a で再確認 / I で再ログイン"),
        ),
        (
            NoticeKind::StreamAuthentication,
            format!("{prefix}: 配信認証が拒否されました。a で再確認 / I で再ログイン"),
        ),
        (
            NoticeKind::AuthExpired,
            format!("{prefix}: 認証期限切れのため再生を停止しました。a で再確認 / I で再ログイン"),
        ),
        (
            NoticeKind::OutsideArea,
            format!("{prefix}: この局は現在地のエリア外です（エリアフリー未加入）"),
        ),
        (
            NoticeKind::AuthExpiredWhilePlaying,
            format!("{prefix}: 認証期限切れのため再生を停止しました。a で再確認 / I で再ログイン"),
        ),
        (
            NoticeKind::ReconnectLimitWhilePlaying,
            format!("{prefix}: 再接続の上限を超えたため再生を停止しました"),
        ),
        (
            NoticeKind::DeadlineWhilePlaying,
            format!("{prefix}: 番組終了：放送終了日時に達したため音声を停止しました"),
        ),
        (
            NoticeKind::ProbeTimeoutWhilePlaying,
            format!("{prefix}: 無音出力診断の実時間上限を超えたため停止しました"),
        ),
        (
            NoticeKind::SourceStalledWhilePlaying,
            format!("{prefix}: 30秒間新しいセグメントを確認できないため停止しました"),
        ),
        (
            NoticeKind::EarlyEndWhilePlaying,
            format!("{prefix}: 放送枠末尾より前に配信が終了しました"),
        ),
        (
            NoticeKind::StreamFailedWhilePlaying,
            format!("{prefix}: 配信取得: 通信に失敗しました"),
        ),
        (
            NoticeKind::InvalidPositionWhilePlaying,
            format!(
                "{prefix}: 配信取得: シーク位置は番組冒頭以上・放送枠末尾未満で指定してください"
            ),
        ),
        (
            NoticeKind::HttpWhilePlaying,
            format!("{prefix}: 配信取得: 配信サーバーがHTTP 503を返しました"),
        ),
    ];
    for (kind, sentence) in cases {
        let mut screen = screen();
        screen.apply_playback(radiko::app::playback::Update {
            notice: Some(sentence.clone()),
            notice_kind: Some(kind),
            notice_prefix: Some(prefix),
            ..radiko::app::playback::Update::default()
        });
        assert_eq!(screen.playback_notice, sentence, "{kind:?}");
        assert_eq!(
            screen.notice_level,
            super::super::notices::NoticeLevel::Error,
            "{kind:?} while nothing is playing must be Error; idle_failure was ignored"
        );
        let (text, fg, bg, bold) = row_style(&mut screen, 80, 24, "✕");
        assert!(text.starts_with("✕ "), "{kind:?} {text}");
        assert_eq!(fg, Color::Red, "{kind:?}");
        assert_eq!(bg, Color::Reset, "{kind:?}");
        assert!(bold, "{kind:?}");
    }

    let raw = "再生準備失敗（現在の状態を維持）: 配信取得: 通信に失敗しました";
    let mut kindless = screen();
    kindless.apply_playback(radiko::app::playback::Update {
        notice: Some(raw.into()),
        notice_kind: None,
        notice_prefix: Some(prefix),
        ..radiko::app::playback::Update::default()
    });
    assert_eq!(kindless.playback_notice, raw);
    assert_eq!(
        kindless.notice_level,
        super::super::notices::NoticeLevel::Error,
        "a kind-less idle failure must be Error; idle_failure was ignored"
    );
    let (text, fg, bg, bold) = row_style(&mut kindless, 80, 24, "✕");
    assert_eq!(text, format!("✕ {raw}"));
    assert_eq!(fg, Color::Red);
    assert_eq!(bg, Color::Reset);
    assert!(bold);

    let mut playing_area = screen();
    playing_area.app.playback = Some(playing());
    let area = format!("{prefix}: この局は現在地のエリア外です（エリアフリー未加入）");
    playing_area.apply_playback(radiko::app::playback::Update {
        snapshot: Some(playing()),
        notice: Some(area.clone()),
        notice_kind: Some(NoticeKind::OutsideArea),
        notice_prefix: Some(prefix),
        ..radiko::app::playback::Update::default()
    });
    assert_eq!(playing_area.playback_notice, area);
    assert_eq!(
        playing_area.notice_level,
        super::super::notices::NoticeLevel::Warning
    );
    let (text, fg, _, _) = row_style(&mut playing_area, 80, 24, "!");
    assert!(text.starts_with("! "), "{text}");
    assert_eq!(fg, Color::Yellow);
}

#[test]
fn playback_info_notices_expire_on_the_injected_clock() {
    let hold = super::super::NOTICE_HOLD;
    assert_eq!(hold, Duration::from_secs(5));
    for sentence in ["一時停止", "停止しました", "番組終了"] {
        let raised = std::time::Instant::now() + Duration::from_secs(86_400);
        let mut screen = screen();
        screen.set_notice_clock(raised);
        if sentence == "一時停止" {
            screen.show_notice(sentence);
        } else {
            screen.apply_playback_at(
                radiko::app::playback::Update {
                    ended: true,
                    notice: Some(sentence.into()),
                    ..radiko::app::playback::Update::default()
                },
                raised,
            );
        }
        assert_eq!(screen.playback_notice, sentence, "{sentence}");
        assert_eq!(
            screen.notice_level,
            super::super::notices::NoticeLevel::Info,
            "{sentence}"
        );
        screen.set_notice_clock(raised + hold - Duration::from_nanos(1));
        screen.expire_preferences();
        assert_eq!(
            screen.playback_notice, sentence,
            "{sentence} must still be shown before 5s"
        );
        screen.set_notice_clock(raised + hold + Duration::from_nanos(1));
        screen.expire_preferences();
        assert!(
            screen.playback_notice.is_empty(),
            "{sentence} must be gone at 5s+1ns"
        );
    }
}

#[test]
fn info_is_cleared_at_exactly_notice_hold() {
    // `expire_timed_notices` uses `now >= raised + NOTICE_HOLD`, so the
    // instant of the hold is already cleared. One nanosecond later stays cleared.
    let hold = super::super::NOTICE_HOLD;
    assert_eq!(hold, Duration::from_secs(5));
    let raised = std::time::Instant::now() + Duration::from_secs(86_400);
    let mut screen = screen();
    screen.set_notice_clock(raised);
    screen.show_notice("一時停止");
    screen.set_notice_clock(raised + hold - Duration::from_nanos(1));
    screen.expire_preferences();
    assert_eq!(screen.playback_notice, "一時停止");
    screen.set_notice_clock(raised + hold);
    screen.expire_preferences();
    assert!(
        screen.playback_notice.is_empty(),
        "info is cleared at exactly NOTICE_HOLD"
    );
    screen.set_notice_clock(raised);
    screen.show_notice("一時停止");
    screen.set_notice_clock(raised + hold + Duration::from_nanos(1));
    screen.expire_preferences();
    assert!(
        screen.playback_notice.is_empty(),
        "info stays cleared at NOTICE_HOLD + 1ns"
    );
}

fn show_auth_stop(screen: &mut Screen) {
    screen.apply_playback(radiko::app::playback::Update {
        stopped: true,
        ended: true,
        stop_reason: Some(radiko::app::playback::StopReason::AuthExpired),
        notice_kind: Some(radiko::app::playback::NoticeKind::AuthExpired),
        notice: Some(super::super::messages::AUTH_EXPIRED_STOP.into()),
        ..radiko::app::playback::Update::default()
    });
}

#[test]
fn hidden_warning_is_drawn_after_the_error_clears_then_the_next_key_drops_it() {
    let warning = "認証期限切れです。再生は続いています。a で再確認 / I で再ログイン";
    let mut screen = screen();
    let mut search =
        Service::new(|| -> Result<CatalogClient, _> { panic!("hidden warning must not fetch") });
    show_auth_stop(&mut screen);
    screen.app.notification = warning.into();
    screen.notification_level = super::super::notices::NoticeLevel::Warning;
    screen.notification_seq = screen.playback_seq + 1;
    let covered = rows(&mut screen, 80, 24).join("\n");
    assert!(covered.contains("✕ "), "{covered}");
    assert!(covered.contains("再生を停止しました"), "{covered}");
    assert!(
        !covered.contains("再生は続いています"),
        "the warning must stay hidden under the error\n{covered}"
    );
    assert!(
        !screen
            .key(
                KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
                &mut search
            )
            .unwrap()
    );
    assert_eq!(
        screen.playback_notice,
        super::super::messages::AUTH_EXPIRED_STOP
    );
    assert_eq!(
        screen.app.notification, warning,
        "j must not drop the hidden warning"
    );
    assert!(
        !screen
            .key(
                KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE),
                &mut search
            )
            .unwrap()
    );
    let revealed = rows(&mut screen, 80, 24).join("\n");
    assert!(
        revealed.contains("再生は続いています") && revealed.contains("a で再確認"),
        "the warning must be drawn once the error clears\n{revealed}"
    );
    let (text, fg, bg, bold) = row_style(&mut screen, 80, 24, "再生は続いています");
    assert!(text.contains("a で再確認"), "{text}");
    assert!(text.starts_with("! "), "{text}");
    assert_eq!(fg, Color::Yellow, "{text}");
    assert_eq!(bg, Color::Reset);
    assert!(!bold);
    assert!(!text.contains("再生を停止しました"), "{text}");
    assert!(
        !screen
            .key(
                KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
                &mut search
            )
            .unwrap()
    );
    assert!(screen.app.notification.is_empty());
    let after = rows(&mut screen, 80, 24).join("\n");
    assert!(
        !after.contains("再生は続いています"),
        "the following key clears the warning\n{after}"
    );
}

#[test]
fn escape_in_the_list_draws_the_warning_that_was_hidden_under_the_error() {
    let warning = "認証期限切れです。再生は続いています。a で再確認 / I で再ログイン";
    let mut screen = screen();
    let mut search =
        Service::new(|| -> Result<CatalogClient, _> { panic!("escape must not fetch") });
    show_auth_stop(&mut screen);
    screen.app.notification = warning.into();
    screen.notification_level = super::super::notices::NoticeLevel::Warning;
    screen.notification_seq = screen.playback_seq + 1;
    let covered = rows(&mut screen, 80, 24).join("\n");
    assert!(covered.contains("再生を停止しました"), "{covered}");
    assert!(!covered.contains("再生は続いています"), "{covered}");
    assert!(
        !screen
            .key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut search)
            .unwrap()
    );
    let revealed = rows(&mut screen, 80, 24).join("\n");
    assert!(
        revealed.contains("再生は続いています") && revealed.contains("a で再確認"),
        "Esc must draw the warning that was hidden under the error\n{revealed}"
    );
    let (text, fg, bg, bold) = row_style(&mut screen, 80, 24, "再生は続いています");
    assert!(text.starts_with("! "), "{text}");
    assert!(text.contains("a で再確認"), "{text}");
    assert_eq!(fg, Color::Yellow, "{text}");
    assert_eq!(bg, Color::Reset);
    assert!(!bold);
    assert!(
        !screen
            .key(
                KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
                &mut search
            )
            .unwrap()
    );
    let after = rows(&mut screen, 80, 24).join("\n");
    assert!(
        !after.contains("再生は続いています"),
        "the next key clears the warning\n{after}"
    );
}

#[test]
fn typing_into_keyword_or_filters_keeps_an_error_until_enter_or_esc() {
    let error = super::super::messages::AUTH_EXPIRED_STOP;
    let mut search =
        Service::new(|| -> Result<CatalogClient, _> { panic!("typing must not fetch") });
    for ch in ['a', 's', 'x', 'k'] {
        let mut keyword = screen();
        show_auth_stop(&mut keyword);
        keyword.app.begin_edit();
        keyword.input = Some(String::new().into());
        assert!(
            !keyword
                .key(
                    KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE),
                    &mut search
                )
                .unwrap()
        );
        assert_eq!(
            keyword.playback_notice, error,
            "keyword {ch} must not clear the error"
        );
        let typed = ch.to_string();
        assert_eq!(keyword.input.as_deref(), Some(typed.as_str()));

        let mut filters = screen();
        show_auth_stop(&mut filters);
        filters.app.begin_edit();
        filters.filters = Some(super::super::filters::Editor::new(
            &filters.app.committed,
            &filters.app.programs,
        ));
        assert!(
            !filters
                .key(
                    KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE),
                    &mut search
                )
                .unwrap()
        );
        assert_eq!(
            filters.playback_notice, error,
            "filter {ch} must not clear the error"
        );
    }

    let mut keyword_esc = screen();
    show_auth_stop(&mut keyword_esc);
    keyword_esc.app.begin_edit();
    keyword_esc.input = Some("朝".into());
    assert!(
        !keyword_esc
            .key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut search)
            .unwrap()
    );
    assert!(keyword_esc.input.is_none());
    assert!(
        keyword_esc.playback_notice.is_empty(),
        "Esc cancels keyword input and clears the error"
    );

    let mut keyword_enter = screen();
    show_auth_stop(&mut keyword_enter);
    keyword_enter.app.begin_edit();
    keyword_enter.input = Some("朝".into());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    let _guard = runtime.enter();
    let mut fetching = Service::new(|| {
        Ok::<_, radiko::api::catalog::CatalogError>(super::super::offline::Fixture)
    });
    assert!(
        !keyword_enter
            .key(
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                &mut fetching
            )
            .unwrap()
    );
    assert!(keyword_enter.input.is_none());
    assert_ne!(keyword_enter.playback_notice, error);

    let mut filter_esc = screen();
    show_auth_stop(&mut filter_esc);
    filter_esc.app.begin_edit();
    filter_esc.filters = Some(super::super::filters::Editor::new(
        &filter_esc.app.committed,
        &filter_esc.app.programs,
    ));
    assert!(
        !filter_esc
            .key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut search)
            .unwrap()
    );
    assert!(filter_esc.filters.is_none());
    assert!(
        filter_esc.playback_notice.is_empty(),
        "Esc cancels filter editing and clears the error"
    );

    let mut filter_enter = screen();
    show_auth_stop(&mut filter_enter);
    filter_enter.app.begin_edit();
    filter_enter.filters = Some(super::super::filters::Editor::new(
        &filter_enter.app.committed,
        &filter_enter.app.programs,
    ));
    assert!(
        !filter_enter
            .key(
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                &mut fetching
            )
            .unwrap()
    );
    assert_ne!(filter_enter.playback_notice, error);
}
