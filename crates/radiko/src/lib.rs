//! 番組表、認証、再生、保存を扱うライブラリです。画面は含みません。
//!
//! TUI と CLI は別クレート `radiko-client` がこのライブラリを呼びます。
//! 通信とデコードの結果は、呼び出し側が持つ要求 ID と一致するものだけを反映します。
//!
//! # 入口
//!
//! よく使う型は [`prelude`] にまとめてあります。セッションと検索条件の保存先を一つにするときは
//! [`storage::DataDir`] です。番組データと音声は保存しません。
//!
//! | やりたいこと | モジュール |
//! | --- | --- |
//! | 放送日、番組、検索条件 | [`domain`] |
//! | 地域と期間の番組表を、最大4並列で取る | [`catalog`] |
//! | 番組表 XML、会員認証、HLS、チャプターの通信 | [`api`] |
//! | デコードと音声出力 | [`player`] |
//! | セッションと検索条件のファイル | [`storage`] と [`session`] |
//! | 画面向けの世代付きサービス | [`app`] |
//!
//! [`catalog`] は、どの局のどの放送日を取るかを決めます。XML の取得と解析は [`api::catalog`] です。
//! 取得元の [`catalog::CatalogClient`] は、そのクライアントを [`catalog`] から使えるように再公開しています。
//!
//! [`domain`] は通信、端末、音声ライブラリに依存しません。署名付き URL と認証情報を持つ型は、
//! ログ向けの自動出力を持ちません。
//!
//! # 例
//!
//! 通信せずに、午前5時区切りの放送日と初期の検索条件を作ります。
//!
//! ```
//! use chrono::TimeZone;
//! use radiko::domain::search::{Scope, SearchCriteria};
//!
//! let jst = chrono::FixedOffset::east_opt(9 * 3600).unwrap();
//! let now = jst.with_ymd_and_hms(2026, 10, 25, 4, 30, 0).unwrap();
//! let criteria = SearchCriteria::initial(now).unwrap();
//! assert_eq!(criteria.range().start().to_string(), "2026-10-24");
//! assert!(matches!(criteria.scope(), Scope::Nationwide));
//! ```

pub mod api;
pub mod app;
pub mod catalog;
pub mod domain;
pub mod player;
mod reconnect;
pub mod session;
pub mod stderr;
pub mod storage;

/// 番組表、検索条件、セッション、再生要求で共通する型。
pub mod prelude {
    pub use crate::api::auth::{AuthError, MemberClient};
    pub use crate::api::stream::ProbeMode;
    pub use crate::api::{ChapterError, parse_chapters};
    pub use crate::app::AppState;
    pub use crate::catalog::{
        CatalogClient, CatalogError, Report, Source, acquire, acquire_range, acquire_stations,
    };
    pub use crate::domain::search::{Filter, Scope, SearchCriteria};
    pub use crate::domain::{
        BroadcastRange, BroadcastStatus, Chapter, ChapterSet, ListeningAvailability, Program,
        ProgramKey, Station, broadcast_day, broadcast_status,
    };
    pub use crate::player::playback::{PlaybackError, play};
    pub use crate::player::seek::{Command, Controller, PlaybackRequest};
    pub use crate::session::{MemberSession, Membership};
    pub use crate::storage::searches::SearchStore;
    pub use crate::storage::{DataDir, SessionStore, StorageError};
}
