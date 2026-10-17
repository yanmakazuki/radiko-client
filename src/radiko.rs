//! 外部データの解析。チャプターの対応付けや聴取権限を解析結果だけから推定しない。
pub mod auth;
pub mod catalog;
mod http;
pub mod stream;
use crate::domain::{Chapter, ChapterSet};
use serde_json::Value;
use std::{fmt, time::Duration};

pub const MAX_CHAPTER_BYTES: usize = 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum ChapterError {
    TooLarge,
    InvalidJson,
    MissingArray,
    InvalidTimeline,
    NoValidChapters,
}
impl fmt::Display for ChapterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::TooLarge => "チャプター応答のサイズ上限を超えました",
            Self::InvalidJson => "チャプターJSONを解析できません",
            Self::MissingArray => "chapters配列がありません",
            Self::InvalidTimeline => "放送枠の長さまたは時刻補正が不正です",
            Self::NoValidChapters => "有効なチャプターがありません（不正データ）",
        })
    }
}
impl std::error::Error for ChapterError {}

fn display_text(value: Option<&Value>) -> Option<String> {
    let text: String = value?
        .as_str()?
        .chars()
        .filter(|c| !c.is_control())
        .collect();
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// `offset_seconds` is event origin minus selected slot origin for arithmetic conversion.
/// Parsing a response does not establish that binding or authorize playback.
/// The caller must verify the audio origin before using positions for playback.
pub fn parse_chapters(
    bytes: &[u8],
    slot_duration: Duration,
    offset_seconds: f64,
) -> Result<ChapterSet, ChapterError> {
    if bytes.len() > MAX_CHAPTER_BYTES {
        return Err(ChapterError::TooLarge);
    }
    if slot_duration.is_zero() || !offset_seconds.is_finite() {
        return Err(ChapterError::InvalidTimeline);
    }
    let root: Value = serde_json::from_slice(bytes).map_err(|_| ChapterError::InvalidJson)?;
    let items = root
        .get("chapters")
        .and_then(Value::as_array)
        .ok_or(ChapterError::MissingArray)?;
    let mut result = ChapterSet {
        chapters: Vec::new(),
        invalid_count: 0,
        music_count: 0,
    };
    for (source_index, item) in items.iter().enumerate() {
        if item.get("isMusic").and_then(Value::as_bool) == Some(true) {
            result.music_count += 1;
            continue;
        }
        // Mistyped flags must not accidentally turn a music item into speech.
        if item.get("isMusic").is_some_and(|v| !v.is_boolean()) {
            result.invalid_count += 1;
            continue;
        }
        let interval = item
            .get("fromSec")
            .and_then(Value::as_f64)
            .zip(item.get("toSec").and_then(Value::as_f64))
            .and_then(|(a, b)| {
                if a < 0.0 || b <= a {
                    return None;
                }
                let (a, b) = (a + offset_seconds, b + offset_seconds);
                if !a.is_finite()
                    || !b.is_finite()
                    || a < 0.0
                    || b <= a
                    || b > slot_duration.as_secs_f64()
                {
                    return None;
                }
                let start = Duration::try_from_secs_f64(a).ok()?;
                let end = Duration::try_from_secs_f64(b).ok()?;
                (start < end && end <= slot_duration).then_some((start, end))
            });
        let Some((start, end)) = interval else {
            result.invalid_count += 1;
            continue;
        };
        result.chapters.push(Chapter {
            title: display_text(item.get("titleLong"))
                .or_else(|| display_text(item.get("title")))
                .unwrap_or_else(|| "無題".into()),
            summary: display_text(item.get("summaryLong")).unwrap_or_default(),
            start,
            end,
            source_index,
        });
    }
    result.chapters.sort_by_key(|c| c.start);
    if result.chapters.is_empty() && result.invalid_count > 0 {
        return Err(ChapterError::NoValidChapters);
    }
    Ok(result)
}
