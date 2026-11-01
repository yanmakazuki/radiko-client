//! TUI sentences chosen from error and auth types. Library `Display` stays CLI wording.
use radiko::app::{auth::Status as AuthStatus, playback::NoticeKind};

pub(super) const NOT_LOGGED_IN: &str = "未ログインです。I でログイン";
pub(super) const SESSION_EXPIRED: &str = "セッション失効。a で再確認 / I で再ログイン";
pub(super) const AUTH_EXPIRED_STOP: &str =
    "認証期限切れのため再生を停止しました。a で再確認 / I で再ログイン";
pub(super) const AUTH_EXPIRED_CONTINUES: &str =
    "認証期限切れです。再生は続いています。a で再確認 / I で再ログイン";
pub(super) const RECONNECT_CONTINUES: &str =
    "再接続の上限です。再生は続いています。番組を選び直してください";
pub(super) const DEADLINE_CONTINUES: &str = "番組は終了しています。再生は続いています";
pub(super) const PROBE_TIMEOUT_CONTINUES: &str = "切り替えられませんでした。再生は続いています";
pub(super) const SOURCE_STALLED_CONTINUES: &str =
    "切り替え先の音声が届きませんでした。再生は続いています";
pub(super) const EARLY_END_CONTINUES: &str =
    "切り替え先の配信が途中で終わりました。再生は続いています";
pub(super) const STREAM_AUTHENTICATION: &str =
    "配信認証が拒否されました。a で再確認 / I で再ログイン";
pub(super) const OUTSIDE_AREA: &str = "この局は現在地のエリア外です（エリアフリー未加入）";
pub(super) const OUTPUT_NO_DEVICE: &str =
    "出力先がありません。OSのサウンド設定を確認し、Enterで再生し直してください";
pub(super) const OUTPUT_UNSUPPORTED: &str =
    "対応形式外です。OSの出力設定を確認し、Enterで再生し直してください";
pub(super) const OUTPUT_DEVICE: &str =
    "出力機器を確認してください。OSの出力先を見て、Enterで再生し直してください";
pub(super) const OUTPUT_STALLED: &str =
    "スリープ復帰・機器変更後は出力先を確認し、Enterで再生し直してください";
pub(super) const OUTPUT_NO_RETRY: &str = "自動再開はしません";

pub(super) fn playback_notice(kind: NoticeKind) -> String {
    match kind {
        NoticeKind::LoginRequired => NOT_LOGGED_IN.to_owned(),
        NoticeKind::SessionExpired => SESSION_EXPIRED.to_owned(),
        NoticeKind::StreamAuthentication => STREAM_AUTHENTICATION.to_owned(),
        NoticeKind::AuthExpired => AUTH_EXPIRED_STOP.to_owned(),
        NoticeKind::AuthExpiredWhilePlaying => AUTH_EXPIRED_CONTINUES.to_owned(),
        NoticeKind::ReconnectLimitWhilePlaying => RECONNECT_CONTINUES.to_owned(),
        NoticeKind::DeadlineWhilePlaying => DEADLINE_CONTINUES.to_owned(),
        NoticeKind::ProbeTimeoutWhilePlaying | NoticeKind::StreamFailedWhilePlaying => {
            PROBE_TIMEOUT_CONTINUES.to_owned()
        }
        NoticeKind::SourceStalledWhilePlaying => SOURCE_STALLED_CONTINUES.to_owned(),
        NoticeKind::EarlyEndWhilePlaying => EARLY_END_CONTINUES.to_owned(),
        NoticeKind::OutsideArea => OUTSIDE_AREA.to_owned(),
    }
}

/// Auth detail for the on-screen header. Exit stderr keeps [`AuthStatus::detail`].
pub(super) fn auth_detail(status: &AuthStatus) -> String {
    match status {
        AuthStatus::SignedOut => NOT_LOGGED_IN.to_owned(),
        AuthStatus::Expired
        | AuthStatus::CheckFailed(radiko::api::auth::AuthError::SessionExpired)
        | AuthStatus::LoginFailed(radiko::api::auth::AuthError::SessionExpired) => {
            SESSION_EXPIRED.to_owned()
        }
        AuthStatus::StorageFailed(error) => storage_failure(error),
        other => other.detail(),
    }
}

fn storage_failure(error: &impl std::fmt::Display) -> String {
    format!("{error} / a で再確認 / I で再ログイン")
}
