//! 再生状態通知は公開メタデータとカウンターのみを所有し、認証情報や音声を含めない。
use super::{output::OutputSummary, seek::PlaybackRequest};
use crate::radiko::stream::{ProbeMode, playback::StreamInfo};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackState {
    Buffering,
    Playing,
    Paused,
}
#[derive(Debug)]
pub struct PlaybackSnapshot {
    pub info: StreamInfo,
    pub state: PlaybackState,
    pub pending: Option<PlaybackRequest>,
    pub volume: u32,
    /// Live elapsed output is not a verified position within the broadcast slot.
    pub position: Option<Duration>,
    pub output: Option<OutputSummary>,
    pub segments: u64,
    pub pipeline: Option<super::diagnostics::Snapshot>,
}
/// Counters describe only the current output generation, not accumulated listening time.
pub fn output_diagnostics(output: Option<&OutputSummary>, segments: u64) -> String {
    match output.filter(|o| o.sample_rate > 0) {
        Some(o) => format!(
            "今回の出力 {:.1}秒 / 不足 {}フレーム（{:.3}秒） / {}Hz {}ch / {}セグメント",
            o.frames as f64 / f64::from(o.sample_rate),
            o.underrun_frames,
            o.underrun_frames as f64 / f64::from(o.sample_rate),
            o.sample_rate,
            o.channels,
            segments
        ),
        None => format!("出力未確認 / {}セグメント", segments),
    }
}

impl PlaybackSnapshot {
    pub(crate) fn active(
        info: &StreamInfo,
        output: Option<OutputSummary>,
        segments: u64,
        paused: bool,
        volume: u32,
        pending: Option<PlaybackRequest>,
    ) -> Self {
        let elapsed = output
            .as_ref()
            .filter(|o| o.sample_rate > 0)
            .map(|o| Duration::from_secs_f64(o.frames as f64 / f64::from(o.sample_rate)))
            .unwrap_or_default();
        Self {
            info: info.clone(),
            state: if paused {
                PlaybackState::Paused
            } else if elapsed.is_zero() {
                PlaybackState::Buffering
            } else {
                PlaybackState::Playing
            },
            pending,
            volume,
            position: (info.mode == ProbeMode::TimeFree)
                .then(|| info.position.saturating_add(elapsed)),
            output,
            segments,
            pipeline: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn output_diagnostics_distinguish_output_time_underruns_and_missing_measurements() {
        let output = OutputSummary {
            frames: 72000,
            underrun_frames: 240,
            sample_rate: 48000,
            channels: 2,
        };
        let text = output_diagnostics(Some(&output), 3);
        assert!(text.contains("1.5秒"));
        assert!(text.contains("240フレーム（0.005秒）"));
        assert!(text.contains("48000Hz 2ch / 3セグメント"));
        assert_eq!(output_diagnostics(None, 0), "出力未確認 / 0セグメント");
        let unknown = OutputSummary {
            sample_rate: 0,
            ..output
        };
        assert_eq!(
            output_diagnostics(Some(&unknown), 4),
            "出力未確認 / 4セグメント"
        );
    }
    fn info(mode: ProbeMode) -> StreamInfo {
        let start = chrono::DateTime::parse_from_rfc3339("2026-10-15T03:00:00Z").unwrap();
        StreamInfo {
            title: Some("選択中の枠".into()),
            station: "TBS".into(),
            mode,
            starts_at: start,
            ends_at: start + chrono::Duration::hours(1),
            position: Duration::from_secs(60),
        }
    }
    fn output() -> OutputSummary {
        OutputSummary {
            frames: 144_000,
            sample_rate: 48_000,
            channels: 2,
            underrun_frames: 7,
        }
    }
    #[test]
    fn timefree_position_uses_output_frames_and_pause_keeps_identity_and_pending_separate() {
        let mut info = info(ProbeMode::TimeFree);
        let pending = PlaybackRequest {
            station: "OBS".into(),
            mode: ProbeMode::Live,
            cursor: info.starts_at,
            position: Duration::ZERO,
        };
        let snapshot = PlaybackSnapshot::active(&info, Some(output()), 2, true, 0, Some(pending));
        info.title = Some("外部で変更した題".into());
        assert_eq!(snapshot.state, PlaybackState::Paused);
        assert_eq!(snapshot.info.station, "TBS");
        assert_eq!(snapshot.info.title.as_deref(), Some("選択中の枠"));
        assert_eq!(snapshot.position, Some(Duration::from_secs(63)));
        assert_eq!(snapshot.volume, 0);
        assert_eq!(snapshot.pending.unwrap().station, "OBS");
        assert_eq!(snapshot.output.unwrap().underrun_frames, 7);
    }
    #[test]
    fn live_output_duration_is_not_a_claim_of_broadcast_position() {
        let snapshot =
            PlaybackSnapshot::active(&info(ProbeMode::Live), Some(output()), 1, false, 25, None);
        assert_eq!(snapshot.position, None);
        assert_eq!(snapshot.state, PlaybackState::Playing);
        assert_eq!(snapshot.output.unwrap().frames, 144_000);
    }
    #[test]
    fn preparation_without_output_and_pause_before_output_are_distinct() {
        let info = info(ProbeMode::TimeFree);
        let snapshot = PlaybackSnapshot::active(&info, None, 0, false, 25, None);
        assert_eq!(snapshot.state, PlaybackState::Buffering);
        assert!(snapshot.output.is_none());
        assert_eq!(snapshot.position, Some(Duration::from_secs(60)));
        let paused = PlaybackSnapshot::active(&info, None, 0, true, 25, None);
        assert_eq!(paused.state, PlaybackState::Paused);
    }
}
