//! 公式チャプターの取得・参照。候補の秒数が得られても、シーク可能とは判定しない。
use super::*;
use crate::{
    api::{ChapterError, MAX_CHAPTER_BYTES, parse_chapters},
    domain::ChapterSet,
};
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug)]
pub enum Error {
    Cancelled,
    Request(ProbeError),
    Parse(ChapterError),
    MismatchedResponse,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => f.write_str("チャプター取得を取り消しました"),
            Self::Request(error) => write!(f, "チャプター取得失敗: {error}"),
            Self::Parse(error) => write!(f, "チャプター解析失敗: {error}"),
            Self::MismatchedResponse => {
                f.write_str("チャプター応答の局・イベントが対象と一致しません")
            }
        }
    }
}
impl std::error::Error for Error {}
impl From<ProbeError> for Error {
    fn from(value: ProbeError) -> Self {
        match value {
            ProbeError::Cancelled => Self::Cancelled,
            other => Self::Request(other),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindingIssue {
    EventMetadataMissing,
    OriginMismatch,
    SlotMismatch,
}
impl BindingIssue {
    pub fn label(self) -> &'static str {
        match self {
            Self::EventMetadataMissing => "番組との対応付け未確認（イベント情報を確認できません）",
            Self::OriginMismatch => "番組との対応付け未確認（開始位置のずれは補正未対応）",
            Self::SlotMismatch => "番組との対応付け未確認（放送枠が一致しません）",
        }
    }
}

#[derive(Debug)]
pub enum Outcome {
    /// Metadata agrees, but audio synchronization remains unverified. Not a seek target.
    Candidates(ChapterSet),
    Aligned(ChapterSet, EventBinding),
    Unavailable,
    Unresolved(BindingIssue),
}
#[derive(Debug)]
pub struct Report {
    pub station: String,
    pub title: Option<String>,
    pub starts_at: DateTime<FixedOffset>,
    pub ends_at: DateTime<FixedOffset>,
    pub outcome: Outcome,
}
/// Same-origin annex timelines were checked against actual audio on 2026-10-18.
/// A binding is not listening permission. Different episode origins remain unsupported.
#[derive(Debug)]
pub struct EventBinding {
    key: crate::domain::ProgramKey,
    ends_at: DateTime<FixedOffset>,
    event_id: String,
}
impl EventBinding {
    pub fn matching_origin(
        key: crate::domain::ProgramKey,
        ends_at: DateTime<FixedOffset>,
        event_id: String,
        origin: DateTime<FixedOffset>,
    ) -> Option<Self> {
        if validate_station(&key.station_id).is_err()
            || origin != key.starts_at
            || ends_at <= key.starts_at
            || event_id.is_empty()
            || event_id.len() > 128
            || !event_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return None;
        }
        Some(Self {
            key,
            ends_at,
            event_id,
        })
    }
    pub fn matches(&self, key: &crate::domain::ProgramKey, ends_at: DateTime<FixedOffset>) -> bool {
        self.key == *key && self.ends_at == ends_at && !self.event_id.is_empty()
    }
}

pub fn inspect(station: &str, cursor: DateTime<FixedOffset>) -> Result<Report, Error> {
    inspect_with(&Http::new()?, station, cursor)
}
fn inspect_with(
    http: &impl Fetch,
    station: &str,
    cursor: DateTime<FixedOffset>,
) -> Result<Report, Error> {
    inspect_with_cancel(http, station, cursor, &AtomicBool::new(false))
}
pub fn inspect_controlled(
    station: &str,
    cursor: DateTime<FixedOffset>,
    cancelled: &AtomicBool,
) -> Result<Report, Error> {
    check_cancelled(cancelled)?;
    inspect_with_cancel(&Http::new()?, station, cursor, cancelled)
}
pub fn inspect_cancellable(
    station: &str,
    cursor: DateTime<FixedOffset>,
    cancelled: std::sync::Arc<AtomicBool>,
) -> Result<Report, Error> {
    check_cancelled(&cancelled)?;
    let mut http = Http::new()?;
    let flag = cancelled.clone();
    http.set_cancellation(std::sync::Arc::new(move || flag.load(Ordering::Acquire)));
    inspect_with_cancel(&http, station, cursor, &cancelled)
}
fn check_cancelled(cancelled: &AtomicBool) -> Result<(), Error> {
    if cancelled.load(Ordering::Acquire) {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}
fn inspect_with_cancel(
    http: &impl Fetch,
    station: &str,
    cursor: DateTime<FixedOffset>,
    cancelled: &AtomicBool,
) -> Result<Report, Error> {
    check_cancelled(cancelled)?;
    validate_station(station)?;
    let selected = program(http, station, cursor)?;
    check_cancelled(cancelled)?;
    let mut report = Report {
        station: station.into(),
        title: selected.title,
        starts_at: selected.start,
        ends_at: selected.end,
        outcome: Outcome::Unresolved(BindingIssue::EventMetadataMissing),
    };
    let Some(event) = selected.chapter_event else {
        return Ok(report);
    };
    // The meaning of a different episode origin is not yet established. Never guess a correction.
    if event.origin != selected.start {
        report.outcome = Outcome::Unresolved(BindingIssue::OriginMismatch);
        return Ok(report);
    }
    let endpoint = url(&format!(
        "https://api.annex-cf.radiko.jp/v1/programs/{}/chapters",
        event.id
    ))?;
    check_cancelled(cancelled)?;
    let reply = http.get(endpoint, HeaderMap::new(), MAX_CHAPTER_BYTES, None, false)?;
    check_cancelled(cancelled)?;
    if reply.bytes.len() > MAX_CHAPTER_BYTES {
        return Err(Error::Parse(ChapterError::TooLarge));
    }
    let root: Value = serde_json::from_slice(&reply.bytes)
        .map_err(|_| Error::Parse(ChapterError::InvalidJson))?;
    if root.get("stationId").and_then(Value::as_str) != Some(station)
        || root.get("programId").and_then(Value::as_str) != Some(event.id.as_str())
    {
        return Err(Error::MismatchedResponse);
    }
    let length = (selected.end - selected.start)
        .to_std()
        .map_err(|_| Error::Parse(ChapterError::InvalidTimeline))?;
    // Only the empirically checked same-origin form may receive a binding.
    let chapters = parse_chapters(&reply.bytes, length, 0.0).map_err(Error::Parse)?;
    report.outcome = if chapters.chapters.is_empty() && chapters.invalid_count == 0 {
        Outcome::Unavailable
    } else {
        match EventBinding::matching_origin(
            crate::domain::ProgramKey {
                station_id: station.into(),
                starts_at: selected.start,
            },
            selected.end,
            event.id,
            event.origin,
        ) {
            Some(binding) => Outcome::Aligned(chapters, binding),
            None => Outcome::Unresolved(BindingIssue::SlotMismatch),
        }
    };
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::http::Reply;
    use std::{cell::RefCell, collections::VecDeque};
    struct Mock {
        replies: RefCell<VecDeque<Result<Vec<u8>, ProbeError>>>,
        paths: RefCell<Vec<String>>,
    }
    impl Fetch for Mock {
        fn get(
            &self,
            url: Url,
            headers: HeaderMap,
            limit: usize,
            range: Option<(u64, u64)>,
            redirects: bool,
        ) -> Result<Reply, ProbeError> {
            assert!(headers.is_empty());
            assert_eq!(limit, MAX_CHAPTER_BYTES);
            assert!(range.is_none());
            assert!(!redirects);
            self.paths.borrow_mut().push(url.path().to_string());
            let bytes = self.replies.borrow_mut().pop_front().unwrap()?;
            Ok(Reply {
                headers: HeaderMap::new(),
                bytes,
                url,
            })
        }
    }
    fn metadata() -> Value {
        serde_json::json!({"programs":[{"stationId":"TBS", "title":"合成分割枠 (2)",
            "startAt":"2026-10-15T03:00:00Z", "endAt":"2026-10-15T03:01:30Z",
            "eventId":"SYNTHETIC-2", "episodeIntroductionStartAt":"2026-10-15T03:00:00Z"}]})
    }
    fn response(items: Value) -> Value {
        serde_json::json!({"stationId":"TBS", "programId":"SYNTHETIC-2", "chapters":items})
    }
    fn mock(values: Vec<Result<Value, ProbeError>>) -> Mock {
        Mock {
            replies: RefCell::new(
                values
                    .into_iter()
                    .map(|value| value.map(|v| serde_json::to_vec(&v).unwrap()))
                    .collect(),
            ),
            paths: RefCell::new(vec![]),
        }
    }
    fn inspect_mock(mock: &Mock) -> Result<Report, Error> {
        inspect_with(mock, "TBS", parse_time("20261015120030").unwrap())
    }
    #[test]
    fn owned_cancellation_rejects_before_metadata_and_maps_to_the_cancelled_status() {
        let flag = std::sync::Arc::new(AtomicBool::new(true));
        let cursor = parse_time("20261015120000").unwrap();
        assert!(matches!(
            inspect_cancellable("TBS", cursor, flag),
            Err(Error::Cancelled)
        ));
        assert!(matches!(
            Error::from(ProbeError::Cancelled),
            Error::Cancelled
        ));
    }
    #[test]
    fn cancellation_before_or_after_metadata_skips_chapter_http() {
        let cancelled = AtomicBool::new(true);
        let untouched = mock(vec![]);
        let cursor = parse_time("20261015120000").unwrap();
        assert!(matches!(
            inspect_with_cancel(&untouched, "TBS", cursor, &cancelled),
            Err(Error::Cancelled)
        ));
        assert!(untouched.paths.borrow().is_empty());
        struct Cancelling<'a> {
            mock: Mock,
            flag: &'a AtomicBool,
        }
        impl Fetch for Cancelling<'_> {
            fn get(
                &self,
                url: Url,
                headers: HeaderMap,
                limit: usize,
                range: Option<(u64, u64)>,
                redirects: bool,
            ) -> Result<Reply, ProbeError> {
                let reply = self.mock.get(url, headers, limit, range, redirects);
                self.flag.store(true, Ordering::Release);
                reply
            }
        }
        cancelled.store(false, Ordering::Release);
        let source = Cancelling {
            mock: mock(vec![Ok(metadata())]),
            flag: &cancelled,
        };
        assert!(matches!(
            inspect_with_cancel(&source, "TBS", cursor, &cancelled),
            Err(Error::Cancelled)
        ));
        assert_eq!(source.mock.paths.borrow().len(), 1);
    }
    #[test]
    fn metadata_event_matches_selected_split_slot_and_music_and_bad_positions_are_excluded() {
        let items: Value = serde_json::from_slice(include_bytes!(
            "../../../../../tests/fixtures/chapters.json"
        ))
        .unwrap();
        let mock = mock(vec![
            Ok(metadata()),
            Ok(response(items["chapters"].clone())),
        ]);
        let report = inspect_mock(&mock).unwrap();
        assert_eq!(report.title.as_deref(), Some("合成分割枠 (2)"));
        let Outcome::Aligned(set, _) = report.outcome else {
            panic!()
        };
        assert_eq!(set.chapters.len(), 2);
        assert_eq!(set.music_count, 1);
        assert_eq!(set.invalid_count, 1);
        assert_eq!(set.chapters[0].start, std::time::Duration::ZERO);
        assert_eq!(
            mock.paths.borrow().as_slice(),
            ["/v1/programs/list", "/v1/programs/SYNTHETIC-2/chapters"]
        );
    }
    #[test]
    fn unresolved_event_or_origin_never_fetches_chapters_or_guesses_a_split_offset() {
        for (key, value) in [
            ("eventId", Value::Null),
            ("stationId", Value::Null),
            ("eventId", Value::String("../other?token=x".into())),
            ("episodeIntroductionStartAt", Value::Null),
            (
                "episodeIntroductionStartAt",
                Value::String("2026-10-15T02:00:00Z".into()),
            ),
        ] {
            let expected = if key == "episodeIntroductionStartAt" && value.is_string() {
                BindingIssue::OriginMismatch
            } else {
                BindingIssue::EventMetadataMissing
            };
            let mut meta = metadata();
            meta["programs"][0][key] = value;
            let mock = mock(vec![Ok(meta)]);
            let Outcome::Unresolved(issue) = inspect_mock(&mock).unwrap().outcome else {
                panic!("unresolved metadata")
            };
            assert_eq!(issue, expected);
            assert_eq!(mock.paths.borrow().len(), 1);
        }
    }
    #[test]
    fn wrong_station_or_event_response_is_rejected_including_missing_identity() {
        for (key, value) in [
            ("stationId", Value::String("OBS".into())),
            ("programId", Value::String("SYNTHETIC-1".into())),
            ("programId", Value::Null),
        ] {
            let mut chapters = response(serde_json::json!([]));
            chapters[key] = value;
            let mock = mock(vec![Ok(metadata()), Ok(chapters)]);
            assert!(matches!(
                inspect_mock(&mock),
                Err(Error::MismatchedResponse)
            ));
        }
    }
    #[test]
    fn empty_success_is_unavailable_but_404_and_malformed_responses_are_failures() {
        let empty = mock(vec![Ok(metadata()), Ok(response(serde_json::json!([])))]);
        assert!(matches!(
            inspect_mock(&empty).unwrap().outcome,
            Outcome::Unavailable
        ));
        let failed = mock(vec![Ok(metadata()), Err(ProbeError::NotFound)]);
        assert!(matches!(
            inspect_mock(&failed),
            Err(Error::Request(ProbeError::NotFound))
        ));
        let wrong = mock(vec![Ok(metadata()), Ok(response(Value::Null))]);
        assert!(matches!(
            inspect_mock(&wrong),
            Err(Error::Parse(ChapterError::MissingArray))
        ));
        let bad = mock(vec![
            Ok(metadata()),
            Ok(response(serde_json::json!([{"fromSec":-1,"toSec":5}]))),
        ]);
        assert!(matches!(
            inspect_mock(&bad),
            Err(Error::Parse(ChapterError::NoValidChapters))
        ));
    }
    #[test]
    fn repeated_inspection_refetches_metadata_and_chapters_without_cache() {
        let empty = response(serde_json::json!([]));
        let mock = mock(vec![
            Ok(metadata()),
            Ok(empty.clone()),
            Ok(metadata()),
            Ok(empty),
        ]);
        inspect_mock(&mock).unwrap();
        inspect_mock(&mock).unwrap();
        assert_eq!(mock.paths.borrow().len(), 4);
        let invalid = mock.paths.borrow().len();
        assert!(inspect_with(&mock, "../TBS", parse_time("20261015120000").unwrap()).is_err());
        assert_eq!(mock.paths.borrow().len(), invalid);
    }
}
