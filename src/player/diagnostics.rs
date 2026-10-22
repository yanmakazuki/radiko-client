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
    /// Waiting PCM at the start/minimum of the acquisition recorded by acquisition_max.
    pub acquisition_max_pcm_start: Duration,
    pub acquisition_max_pcm_min: Duration,
    pub decode_max: Duration,
    pub decode_last: Duration,
    pub media_last: Duration,
    pub decoded_segments: u64,
    /// Decoded PCM awaiting main-loop receipt, including a blocked sender (at most three).
    pub waiting_pcm_segments: u32,
    pub waiting_pcm_media: Duration,
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
                let response = http.response_status.map(|status| status.to_string()).unwrap_or_else(|| "未取得".into());
                format!(
                    "通信診断 {stage}{:.0}ms / 応答max{:.0}ms / 本文max{:.0}ms / 試行{}・再試行待ち{} / 通信失敗{}・期限{}・接続{} / HTTP{response}",
                    http.elapsed.as_secs_f64() * 1000.0,
                    http.headers_max.as_secs_f64() * 1000.0,
                    http.body_max.as_secs_f64() * 1000.0,
                    http.attempts,
                    http.retry_waits,
                    http.network_failures,
                    http.timeouts,
                    http.connect_failures
                )
            })
            .unwrap_or_else(|| "通信診断 未取得".into())
    }
    pub fn lines(&self) -> String {
        format!("{}\n{}", self.label(), self.http_label())
    }
    pub fn label(&self) -> String {
        format!(
            "処理診断 {}{:.0}ms / 取得max{:.0}ms / 復号{:.0}/{:.0}ms / 音声{:.0}ms / 待機PCM{}個{:.0}ms / 取得max時待機{:.0}→{:.0}ms",
            self.stage.label(),
            self.elapsed.as_secs_f64() * 1000.0,
            self.acquisition_max.as_secs_f64() * 1000.0,
            self.decode_last.as_secs_f64() * 1000.0,
            self.decode_max.as_secs_f64() * 1000.0,
            self.media_last.as_secs_f64() * 1000.0,
            self.waiting_pcm_segments,
            self.waiting_pcm_media.as_secs_f64() * 1000.0,
            self.acquisition_max_pcm_start.as_secs_f64() * 1000.0,
            self.acquisition_max_pcm_min.as_secs_f64() * 1000.0,
        )
    }
}
pub(super) struct Tracker {
    began: Instant,
    snapshot: Snapshot,
    acquisition_pcm_start: Duration,
    acquisition_pcm_min: Duration,
}
impl Tracker {
    pub fn new(now: Instant) -> Self {
        Self {
            began: now,
            acquisition_pcm_start: Duration::ZERO,
            acquisition_pcm_min: Duration::ZERO,
            snapshot: Snapshot {
                stage: Stage::Acquisition,
                elapsed: Duration::ZERO,
                acquisition_max: Duration::ZERO,
                acquisition_max_pcm_start: Duration::ZERO,
                acquisition_max_pcm_min: Duration::ZERO,
                decode_max: Duration::ZERO,
                decode_last: Duration::ZERO,
                media_last: Duration::ZERO,
                decoded_segments: 0,
                waiting_pcm_segments: 0,
                waiting_pcm_media: Duration::ZERO,
                http: None,
            },
        }
    }
    pub fn enter(&mut self, stage: Stage, now: Instant) {
        let elapsed = now.saturating_duration_since(self.began);
        if self.snapshot.stage == Stage::Acquisition
            && stage != Stage::Acquisition
            && elapsed > self.snapshot.acquisition_max
        {
            self.snapshot.acquisition_max = elapsed;
            self.snapshot.acquisition_max_pcm_start = self.acquisition_pcm_start;
            self.snapshot.acquisition_max_pcm_min = self.acquisition_pcm_min;
        }
        if self.snapshot.stage == Stage::Decode {
            self.snapshot.decode_max = self.snapshot.decode_max.max(elapsed);
            self.snapshot.decode_last = elapsed;
        }
        self.snapshot.stage = stage;
        if stage == Stage::Acquisition {
            self.acquisition_pcm_start = self.snapshot.waiting_pcm_media;
            self.acquisition_pcm_min = self.snapshot.waiting_pcm_media;
        }
        self.began = now;
    }
    pub fn decoded(&mut self, media: Duration) {
        self.snapshot.media_last = media;
        self.snapshot.decoded_segments = self.snapshot.decoded_segments.saturating_add(1);
    }
    pub fn queued(&mut self, media: Duration) {
        self.snapshot.waiting_pcm_segments = self.snapshot.waiting_pcm_segments.saturating_add(1);
        self.snapshot.waiting_pcm_media = self.snapshot.waiting_pcm_media.saturating_add(media);
    }
    pub fn received(&mut self, media: Duration) {
        self.snapshot.waiting_pcm_segments = self.snapshot.waiting_pcm_segments.saturating_sub(1);
        self.snapshot.waiting_pcm_media = self.snapshot.waiting_pcm_media.saturating_sub(media);
        if self.snapshot.stage == Stage::Acquisition {
            self.acquisition_pcm_min = self
                .acquisition_pcm_min
                .min(self.snapshot.waiting_pcm_media);
        }
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
    fn longest_acquisition_keeps_its_own_start_and_minimum_waiting_pcm() {
        let now = Instant::now();
        let mut tracker = Tracker::new(now);
        tracker.enter(Stage::Backpressure, now);
        tracker.queued(Duration::from_secs(5));
        tracker.queued(Duration::from_secs(5));
        tracker.enter(Stage::Acquisition, now + Duration::from_secs(1));
        tracker.received(Duration::from_secs(5));
        tracker.received(Duration::from_secs(5));
        tracker.enter(Stage::Decode, now + Duration::from_secs(12));
        let snapshot = tracker.snapshot(now + Duration::from_secs(12));
        assert_eq!(snapshot.acquisition_max, Duration::from_secs(11));
        assert_eq!(snapshot.acquisition_max_pcm_start, Duration::from_secs(10));
        assert_eq!(snapshot.acquisition_max_pcm_min, Duration::ZERO);
        tracker.enter(Stage::Backpressure, now + Duration::from_secs(12));
        tracker.queued(Duration::from_secs(5));
        tracker.enter(Stage::Acquisition, now + Duration::from_secs(13));
        tracker.enter(Stage::Waiting, now + Duration::from_secs(15));
        assert_eq!(
            tracker.snapshot(now).acquisition_max_pcm_start,
            Duration::from_secs(10)
        );
        tracker.enter(Stage::Acquisition, now + Duration::from_secs(16));
        tracker.enter(Stage::Finished, now + Duration::from_secs(28));
        let snapshot = tracker.snapshot(now + Duration::from_secs(28));
        assert_eq!(snapshot.acquisition_max, Duration::from_secs(12));
        assert_eq!(snapshot.acquisition_max_pcm_start, Duration::from_secs(5));
        assert_eq!(snapshot.acquisition_max_pcm_min, Duration::from_secs(5));
        assert_eq!(
            Tracker::new(now).snapshot(now).acquisition_max_pcm_start,
            Duration::ZERO
        );
    }
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
