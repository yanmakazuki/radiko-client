//! Footer notice severity. The level comes from the notice kind or the
//! playback state, never from the displayed sentence.
use radiko::app::playback::NoticeKind;
use ratatui::style::{Color, Modifier, Style};

/// How urgently a footer notice is shown.
///
/// Higher [`Self::rank`] wins when several notices exist. Ties keep the newest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum NoticeLevel {
    Info,
    Warning,
    Error,
}

impl NoticeLevel {
    pub(super) fn rank(self) -> u8 {
        match self {
            Self::Info => 0,
            Self::Warning => 1,
            Self::Error => 2,
        }
    }
}

/// Level for a [`NoticeKind`].
///
/// All nine `*WhilePlaying` kinds are warnings: playback is still running.
/// [`NoticeKind::AuthExpired`] is an error because playback has stopped.
/// Login, session, stream-auth, and outside-area failures are warnings unless
/// the caller already knows playback stopped (see [`playback_level`]).
pub(super) fn level_from_kind(kind: NoticeKind) -> NoticeLevel {
    match kind {
        NoticeKind::AuthExpiredWhilePlaying
        | NoticeKind::ReconnectLimitWhilePlaying
        | NoticeKind::DeadlineWhilePlaying
        | NoticeKind::ProbeTimeoutWhilePlaying
        | NoticeKind::SourceStalledWhilePlaying
        | NoticeKind::EarlyEndWhilePlaying
        | NoticeKind::StreamFailedWhilePlaying
        | NoticeKind::InvalidPositionWhilePlaying
        | NoticeKind::HttpWhilePlaying => NoticeLevel::Warning,
        NoticeKind::AuthExpired => NoticeLevel::Error,
        NoticeKind::LoginRequired
        | NoticeKind::SessionExpired
        | NoticeKind::StreamAuthentication
        | NoticeKind::OutsideArea => NoticeLevel::Warning,
    }
}

/// Level for one playback notice.
///
/// `stopped` is a failed session. `idle_failure` is a `*WhilePlaying` kind
/// dropped because nothing is playing, so the stop sentence is shown.
/// `output_failure` is an output-device failure. Continuing playback without
/// those flags stays informational when the kind does not say otherwise.
pub(super) fn playback_level(
    kind: Option<NoticeKind>,
    stopped: bool,
    idle_failure: bool,
    output_failure: bool,
) -> NoticeLevel {
    match kind {
        // Continuing kinds take their level from the kind table, even if a
        // later flag would otherwise call the session stopped.
        Some(kind) if kind.while_playing() => level_from_kind(kind),
        Some(_) if stopped => NoticeLevel::Error,
        Some(kind) => level_from_kind(kind),
        None if stopped || idle_failure || output_failure => NoticeLevel::Error,
        None => NoticeLevel::Info,
    }
}

/// `NO_COLOR` disables color only when the variable is set and non-empty.
/// An empty value keeps colors. Callers pass the value; this does not read the environment.
pub(super) fn no_color_enabled(value: Option<&std::ffi::OsStr>) -> bool {
    value.is_some_and(|value| !value.is_empty())
}

pub(super) fn notice_mark(level: NoticeLevel) -> &'static str {
    match level {
        NoticeLevel::Error => "✕ ",
        NoticeLevel::Warning => "! ",
        NoticeLevel::Info => "",
    }
}

pub(super) fn notice_style(level: NoticeLevel, no_color: bool) -> Style {
    if no_color {
        return Style::default();
    }
    match level {
        NoticeLevel::Error => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        NoticeLevel::Warning => Style::default().fg(Color::Yellow),
        NoticeLevel::Info => Style::default(),
    }
}

pub(super) struct Candidate<'a> {
    pub level: NoticeLevel,
    pub seq: u64,
    pub text: &'a str,
}

/// At most one notice. Highest severity wins. Equal severity keeps the newest.
pub(super) fn choose_notice<'a>(items: &[Candidate<'a>]) -> Option<(NoticeLevel, &'a str)> {
    items
        .iter()
        .filter(|item| !item.text.is_empty())
        .max_by_key(|item| (item.level.rank(), item.seq))
        .map(|item| (item.level, item.text))
}
