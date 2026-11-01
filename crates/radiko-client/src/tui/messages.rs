//! TUI sentences chosen from error and auth types. Library `Display` stays CLI wording.
use radiko::app::{auth::Status as AuthStatus, playback::NoticeKind};

pub(super) const NOT_LOGGED_IN: &str = "未ログインです。I でログイン";
pub(super) const SESSION_EXPIRED: &str = "セッション失効。I で再ログイン / a で再確認";
pub(super) const AUTH_EXPIRED_STOP: &str =
    "認証期限切れのため再生を停止しました。a で再確認 / I で再ログイン";
pub(super) const STREAM_AUTHENTICATION: &str =
    "配信認証が拒否されました。a で再確認 / I で再ログイン";
pub(super) const OUTSIDE_AREA: &str = "この局は現在地のエリア外です（エリアフリー未加入）";

pub(super) fn playback_notice(kind: NoticeKind) -> String {
    match kind {
        NoticeKind::LoginRequired => NOT_LOGGED_IN.to_owned(),
        NoticeKind::SessionExpired => SESSION_EXPIRED.to_owned(),
        NoticeKind::StreamAuthentication => STREAM_AUTHENTICATION.to_owned(),
        NoticeKind::AuthExpired => AUTH_EXPIRED_STOP.to_owned(),
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
    format!("{error} / I で再ログイン / a で再確認")
}
