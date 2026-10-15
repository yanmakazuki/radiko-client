use chrono::{DateTime, Duration, FixedOffset, NaiveDate, Timelike};
use std::time::Duration as Position;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ProgramKey {
    pub station_id: String,
    pub starts_at: DateTime<FixedOffset>,
}

/// radiko's broadcast day starts at 05:00 JST, regardless of input timezone.
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
    /// The most recent start wins; equal starts retain source order.
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
