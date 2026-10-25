//! 放送枠・放送日・状態・チャプターの共通型と判定。放送状態と聴取可否は別に扱う。
pub mod search;
use chrono::{DateTime, Duration, FixedOffset, NaiveDate, Timelike};
use std::time::Duration as Position;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ProgramKey {
    pub station_id: String,
    pub starts_at: DateTime<FixedOffset>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Classification {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Program {
    pub key: ProgramKey,
    /// 番組表XMLのID。チャプター用のeventIdと同じものだと仮定しない。
    pub program_id: String,
    pub ends_at: DateTime<FixedOffset>,
    pub title: String,
    pub performer: String,
    pub description: String,
    pub info: String,
    pub tags: Vec<String>,
    pub program_genres: Vec<Classification>,
    pub personality_genres: Vec<Classification>,
}

/// 入力のタイムゾーンによらず、日本時間の午前5時を放送日の境界にする。
pub fn broadcast_day(now: DateTime<FixedOffset>) -> Option<NaiveDate> {
    let jst = FixedOffset::east_opt(9 * 3600)?;
    let local = now.with_timezone(&jst);
    if local.hour() < 5 {
        local.date_naive().checked_sub_signed(Duration::days(1))
    } else {
        Some(local.date_naive())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum BroadcastStatus {
    Upcoming,
    OnAir,
    Finished,
}

/// A display assessment, never authorization to skip the stream resolver.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListeningAvailability {
    Unknown,
    Unavailable(&'static str),
    CurrentOutput,
}
impl ListeningAvailability {
    pub fn label(self) -> &'static str {
        match self {
            Self::Unknown => "再生可否未確認",
            Self::Unavailable(reason) => reason,
            Self::CurrentOutput => "現在の再生で配信確認済み（再操作時は再確認）",
        }
    }
}

pub fn broadcast_status(
    start: DateTime<FixedOffset>,
    end: DateTime<FixedOffset>,
    now: DateTime<FixedOffset>,
) -> Result<BroadcastStatus, &'static str> {
    if end <= start {
        return Err("放送終了日時が開始日時以前です");
    }
    Ok(if now < start {
        BroadcastStatus::Upcoming
    } else if now < end {
        BroadcastStatus::OnAir
    } else {
        BroadcastStatus::Finished
    })
}

#[derive(Debug)]
pub struct Chapter {
    pub title: String,
    pub summary: String,
    pub start: Position,
    pub end: Position,
    pub source_index: usize,
}

#[derive(Debug)]
pub struct ChapterSet {
    pub chapters: Vec<Chapter>,
    pub invalid_count: usize,
    pub music_count: usize,
}

impl ChapterSet {
    /// 重複時は開始位置が最も後の項目を選び、同じ開始位置なら元の順序を優先する。
    pub fn current(&self, position: Position) -> Option<&Chapter> {
        self.chapters
            .iter()
            .filter(|c| c.start <= position && position < c.end)
            .max_by(|a, b| {
                a.start
                    .cmp(&b.start)
                    .then_with(|| b.source_index.cmp(&a.source_index))
            })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Station {
    pub id: String,
    pub name: String,
}

/// 両端を含む放送日の選択範囲。最大30日までに制限する。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BroadcastRange {
    start: NaiveDate,
    end: NaiveDate,
    days: Vec<NaiveDate>,
}
impl BroadcastRange {
    pub fn single(day: NaiveDate) -> Self {
        Self {
            start: day,
            end: day,
            days: vec![day],
        }
    }
    pub fn new(start: NaiveDate, end: NaiveDate) -> Result<Self, &'static str> {
        let span = end.signed_duration_since(start).num_days();
        if !(0..30).contains(&span) {
            return Err("期間は開始日から終了日までの1〜30放送日で指定してください");
        }
        let mut days = vec![start];
        let mut day = start;
        while day < end {
            day = day.succ_opt().ok_or("放送日の範囲外です")?;
            days.push(day);
        }
        Ok(Self { start, end, days })
    }
    pub fn start(&self) -> NaiveDate {
        self.start
    }
    pub fn end(&self) -> NaiveDate {
        self.end
    }
    pub fn days(&self) -> &[NaiveDate] {
        &self.days
    }
}
