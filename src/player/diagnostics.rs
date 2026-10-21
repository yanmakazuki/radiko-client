//! Wall-clock worker measurements only; never called from the audio callback.
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Acquisition,
    Decode,
    Backpressure,
    Waiting,
    Finished,
}
impl Stage {
    fn label(self) -> &'static str {
        match self {
            Self::Acquisition => "取得",
            Self::Decode => "復号",
            Self::Backpressure => "配送待ち",
            Self::Waiting => "更新待ち",
            Self::Finished => "終了",
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct Snapshot {
    pub stage: Stage,
    pub elapsed: Duration,
    pub acquisition_max: Duration,
    pub decode_max: Duration,
    pub decode_last: Duration,
    pub media_last: Duration,
    pub decoded_segments: u64,
    pub http: Option<crate::radiko::stream::playback::RequestSnapshot>,
}
impl Snapshot {
    pub fn http_label(&self) -> String {
        self.http
            .map(|http| {
                use crate::radiko::stream::playback::RequestStage;
                let stage = match http.stage {
                    RequestStage::Idle => "待機",
                    RequestStage::Headers => "応答待ち",
                    RequestStage::Body => "本文受信",
                    RequestStage::RetryWait => "再試行待ち",
                };
                format!(
                    "通信診断 {stage}{:.0}ms / 応答max{:.0}ms / 本文max{:.0}ms / 試行{}・再試行待ち{}",
                    http.elapsed.as_secs_f64() * 1000.0,
                    http.headers_max.as_secs_f64() * 1000.0,
                    http.body_max.as_secs_f64() * 1000.0,
                    http.attempts,
                    http.retry_waits
                )
            })
            .unwrap_or_else(|| "通信診断 未取得".into())
    }
    pub fn lines(&self) -> String {
        format!("{}\n{}", self.label(), self.http_label())
    }
    pub fn label(&self) -> String {
        format!(
            "処理診断 {}{:.0}ms / 取得max{:.0}ms / 復号{:.0}/{:.0}ms / 音声{:.0}ms",
            self.stage.label(),
            self.elapsed.as_secs_f64() * 1000.0,
            self.acquisition_max.as_secs_f64() * 1000.0,
            self.decode_last.as_secs_f64() * 1000.0,
            self.decode_max.as_secs_f64() * 1000.0,
            self.media_last.as_secs_f64() * 1000.0,
        )
    }
}
pub(super) struct Tracker {
    began: Instant,
    snapshot: Snapshot,
}
impl Tracker {
    pub fn new(now: Instant) -> Self {
        Self {
            began: now,
            snapshot: Snapshot {
                stage: Stage::Acquisition,
                elapsed: Duration::ZERO,
                acquisition_max: Duration::ZERO,
                decode_max: Duration::ZERO,
                decode_last: Duration::ZERO,
                media_last: Duration::ZERO,
                decoded_segments: 0,
                http: None,
            },
        }
    }
    pub fn enter(&mut self, stage: Stage, now: Instant) {
        let elapsed = now.saturating_duration_since(self.began);
        if self.snapshot.stage == Stage::Acquisition && stage != Stage::Acquisition {
            self.snapshot.acquisition_max = self.snapshot.acquisition_max.max(elapsed);
        }
        if self.snapshot.stage == Stage::Decode {
            self.snapshot.decode_max = self.snapshot.decode_max.max(elapsed);
            self.snapshot.decode_last = elapsed;
        }
        self.snapshot.stage = stage;
        self.began = now;
    }
    pub fn decoded(&mut self, media: Duration) {
        self.snapshot.media_last = media;
        self.snapshot.decoded_segments = self.snapshot.decoded_segments.saturating_add(1);
    }
    pub fn snapshot(&self, now: Instant) -> Snapshot {
        Snapshot {
            elapsed: if self.snapshot.stage == Stage::Finished {
                Duration::ZERO
            } else {
                now.saturating_duration_since(self.began)
            },
            ..self.snapshot
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn backpressure_and_pending_wait_do_not_inflate_acquisition_or_decode() {
        let now = Instant::now();
        let mut tracker = Tracker::new(now);
        tracker.enter(Stage::Decode, now + Duration::from_millis(20));
        tracker.decoded(Duration::from_secs(5));
        tracker.enter(Stage::Backpressure, now + Duration::from_millis(30));
        tracker.enter(Stage::Waiting, now + Duration::from_secs(60));
        tracker.enter(Stage::Acquisition, now + Duration::from_secs(90));
        let snapshot = tracker.snapshot(now + Duration::from_secs(92));
        assert_eq!(snapshot.elapsed, Duration::from_secs(2));
        assert_eq!(snapshot.acquisition_max, Duration::from_millis(20));
        assert_eq!(snapshot.decode_max, Duration::from_millis(10));
        assert_eq!(snapshot.media_last, Duration::from_secs(5));
        assert_eq!(snapshot.decoded_segments, 1);
        tracker.enter(Stage::Finished, now + Duration::from_secs(93));
        assert_eq!(
            tracker.snapshot(now + Duration::from_secs(100)).elapsed,
            Duration::ZERO
        );
        assert_eq!(
            tracker.snapshot(now).acquisition_max,
            Duration::from_secs(3)
        );
        assert_eq!(Tracker::new(now).snapshot(now).decoded_segments, 0);
    }
}
