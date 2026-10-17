//! 公式チャプターの取得・参照。候補の秒数が得られても、シーク可能とは判定しない。
use super::*;
use crate::{
    domain::ChapterSet,
    radiko::{ChapterError, MAX_CHAPTER_BYTES, parse_chapters},
};

#[derive(Debug)]
pub enum Error {
    Request(ProbeError),
    Parse(ChapterError),
    MismatchedResponse,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
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
        Self::Request(value)
    }
}

#[derive(Debug)]
pub enum Outcome {
    /// Metadata agrees, but audio synchronization remains unverified. Not a seek target.
    Candidates(ChapterSet),
    Unavailable,
    Unresolved,
}
#[derive(Debug)]
pub struct Report {
    pub station: String,
    pub title: Option<String>,
    pub starts_at: DateTime<FixedOffset>,
    pub ends_at: DateTime<FixedOffset>,
    pub outcome: Outcome,
}

pub fn inspect(station: &str, cursor: DateTime<FixedOffset>) -> Result<Report, Error> {
    inspect_with(&Http::new()?, station, cursor)
}
fn inspect_with(
    http: &impl Fetch,
    station: &str,
    cursor: DateTime<FixedOffset>,
) -> Result<Report, Error> {
    validate_station(station)?;
    let selected = program(http, station, cursor)?;
    let mut report = Report {
        station: station.into(),
        title: selected.title,
        starts_at: selected.start,
        ends_at: selected.end,
        outcome: Outcome::Unresolved,
    };
    let Some(event) = selected.chapter_event else {
        return Ok(report);
    };
    // The meaning of a different episode origin is not yet established. Never guess a correction.
    if event.origin != selected.start {
        return Ok(report);
    }
    let endpoint = url(&format!(
        "https://api.annex-cf.radiko.jp/v1/programs/{}/chapters",
        event.id
    ))?;
    let reply = http.get(endpoint, HeaderMap::new(), MAX_CHAPTER_BYTES, None, false)?;
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
    // These are inspection candidates only; API metadata alone does not prove audio alignment.
    let chapters = parse_chapters(&reply.bytes, length, 0.0).map_err(Error::Parse)?;
    report.outcome = if chapters.chapters.is_empty() && chapters.invalid_count == 0 {
        Outcome::Unavailable
    } else {
        Outcome::Candidates(chapters)
    };
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::radiko::http::Reply;
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
    fn metadata_event_matches_selected_split_slot_and_music_and_bad_positions_are_excluded() {
        let items: Value =
            serde_json::from_slice(include_bytes!("../../../tests/fixtures/chapters.json"))
                .unwrap();
        let mock = mock(vec![
            Ok(metadata()),
            Ok(response(items["chapters"].clone())),
        ]);
        let report = inspect_mock(&mock).unwrap();
        assert_eq!(report.title.as_deref(), Some("合成分割枠 (2)"));
        let Outcome::Candidates(set) = report.outcome else {
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
            let mut meta = metadata();
            meta["programs"][0][key] = value;
            let mock = mock(vec![Ok(meta)]);
            assert!(matches!(
                inspect_mock(&mock).unwrap().outcome,
                Outcome::Unresolved
            ));
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
