//! CPAL output consumes complete stereo frames without locks, allocation or waits.
use super::aac::DecodedPcm;
use cpal::{
    SampleFormat, SampleRate,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use rtrb::{Consumer, Producer, RingBuffer};
use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Debug, PartialEq, Eq)]
pub enum OutputError {
    InvalidPcm,
    NoDevice,
    UnsupportedFormat,
    Device,
    Stalled,
    Deadline,
}
impl fmt::Display for OutputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidPcm => "出力用PCMまたは再生時間が不正です",
            Self::NoDevice => "既定の音声出力機器がありません",
            Self::UnsupportedFormat => {
                "PCMと同じレートのmono/stereo・f32出力に機器が対応していません"
            }
            Self::Device => "音声出力機器の開始・実行に失敗しました",
            Self::Stalled => "音声コールバックが進まないため出力を停止しました",
            Self::Deadline => "番組終了：放送終了日時に達したため音声を停止しました",
        })
    }
}
impl std::error::Error for OutputError {}

#[derive(Default)]
struct StopState {
    cancelled: AtomicBool,
    parent: Option<StopToken>,
}
#[derive(Clone, Default)]
pub struct StopToken(Arc<StopState>);
impl StopToken {
    pub fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::Release);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::Acquire)
            || self.0.parent.as_ref().is_some_and(StopToken::is_cancelled)
    }
    pub fn child(&self) -> Self {
        Self(Arc::new(StopState {
            cancelled: AtomicBool::new(false),
            parent: Some(self.clone()),
        }))
    }
}

#[derive(Default)]
struct State {
    paused: AtomicBool,
    volume: AtomicU32,
    stopped: AtomicBool,
    enabled: AtomicBool,
    failed: AtomicBool,
    ended: AtomicBool,
    expired: AtomicBool,
    frames: AtomicU64,
    callbacks: AtomicU64,
    total: AtomicU64,
    underrun_frames: AtomicU64,
}
#[derive(Debug)]
pub struct OutputSummary {
    pub frames: u64,
    pub underrun_frames: u64,
    pub sample_rate: u32,
    pub channels: u16,
}

struct Renderer {
    queue: Consumer<[f32; 2]>,
    state: Arc<State>,
    cancel: StopToken,
    rate: u32,
    deadline: Option<Instant>,
}
impl Renderer {
    fn render(&mut self, data: &mut [f32], channels: usize, now: Instant) {
        self.state.callbacks.fetch_add(1, Ordering::Release);
        data.fill(0.0);
        if self.deadline.is_some_and(|deadline| now >= deadline) {
            self.state.expired.store(true, Ordering::Release);
            self.state.stopped.store(true, Ordering::Release);
        }
        if !self.state.enabled.load(Ordering::Acquire) || self.state.paused.load(Ordering::Acquire)
        {
            return;
        }
        let gain = self.state.volume.load(Ordering::Acquire) as f32 / 100.0;
        let mut frames = self.state.frames.load(Ordering::Relaxed);
        let mut missing = 0;
        let available = self.deadline.map_or(usize::MAX, |end| {
            (end.saturating_duration_since(now).as_secs_f64() * f64::from(self.rate)).floor()
                as usize
        });
        for (index, output) in data.chunks_exact_mut(channels).enumerate() {
            if self.cancel.is_cancelled()
                || self.state.stopped.load(Ordering::Acquire)
                || self.state.failed.load(Ordering::Acquire)
            {
                break;
            }
            if frames >= self.state.total.load(Ordering::Acquire) {
                break;
            }
            if index >= available {
                self.state.expired.store(true, Ordering::Release);
                self.state.stopped.store(true, Ordering::Release);
                break;
            }
            match self.queue.pop() {
                Ok(frame) => {
                    if channels == 1 {
                        output[0] = (frame[0] + frame[1]) * 0.5 * gain;
                    } else {
                        output[0] = frame[0] * gain;
                        output[1] = frame[1] * gain;
                    }
                    frames += 1;
                }
                Err(_) => missing += 1,
            }
        }
        self.state.frames.store(frames, Ordering::Release);
        self.state
            .underrun_frames
            .fetch_add(missing, Ordering::Relaxed);
        if frames >= self.state.total.load(Ordering::Acquire) {
            self.state.ended.store(true, Ordering::Release);
        }
    }
}

fn queue(rate: u32, total: u64, deadline: Option<Instant>) -> (Producer<[f32; 2]>, Renderer) {
    let (producer, consumer) = RingBuffer::new(rate as usize * 2);
    let state = Arc::new(State::default());
    state.volume.store(100, Ordering::Release);
    state.total.store(total, Ordering::Release);
    state.enabled.store(true, Ordering::Release);
    (
        producer,
        Renderer {
            queue: consumer,
            state,
            cancel: StopToken::default(),
            rate,
            deadline,
        },
    )
}

struct Output {
    stream: cpal::Stream,
    producer: Producer<[f32; 2]>,
    state: Arc<State>,
    channels: u16,
}
impl Drop for Output {
    fn drop(&mut self) {
        self.state.stopped.store(true, Ordering::Release);
        let _ = self.stream.pause();
        // Dropping the stream and its consumer discards the remaining queue.
    }
}
impl Output {
    fn open(rate: u32, total: u64, deadline: Option<Instant>) -> Result<Self, OutputError> {
        Self::open_with_stop(rate, total, deadline, StopToken::default())
    }
    fn open_with_stop(
        rate: u32,
        total: u64,
        deadline: Option<Instant>,
        stop: StopToken,
    ) -> Result<Self, OutputError> {
        let device = cpal::default_host()
            .default_output_device()
            .ok_or(OutputError::NoDevice)?;
        let supported = device
            .supported_output_configs()
            .map_err(|_| OutputError::Device)?
            .filter(|c| {
                c.sample_format() == SampleFormat::F32
                    && matches!(c.channels(), 1 | 2)
                    && c.min_sample_rate().0 <= rate
                    && c.max_sample_rate().0 >= rate
            })
            .max_by_key(|c| c.channels())
            .ok_or(OutputError::UnsupportedFormat)?;
        let config = supported.with_sample_rate(SampleRate(rate)).config();
        let channels = config.channels;
        let (producer, mut renderer) = queue(rate, total, deadline);
        renderer.state.enabled.store(false, Ordering::Release);
        renderer.cancel = stop;
        let state = renderer.state.clone();
        let failed = state.clone();
        let stream = device
            .build_output_stream(
                &config,
                move |data: &mut [f32], info| {
                    let timestamp = info.timestamp();
                    let delay = timestamp
                        .playback
                        .duration_since(&timestamp.callback)
                        .unwrap_or_default();
                    let now = Instant::now();
                    renderer.render(
                        data,
                        usize::from(channels),
                        now.checked_add(delay).unwrap_or(now),
                    );
                },
                move |_| {
                    failed.failed.store(true, Ordering::Release);
                },
                None,
            )
            .map_err(|_| OutputError::Device)?;
        Ok(Self {
            stream,
            producer,
            state,
            channels,
        })
    }
    fn check(&self) -> Result<(), OutputError> {
        if self.state.failed.load(Ordering::Acquire) {
            Err(OutputError::Device)
        } else if self.state.expired.load(Ordering::Acquire) {
            Err(OutputError::Deadline)
        } else {
            Ok(())
        }
    }
}

/// One validated, bounded sample. No resampling or OS decoding is performed.
/// `remaining` is the selected live slot's remaining wall-clock time.
pub fn play_pcm(
    pcm: &DecodedPcm,
    remaining: Option<Duration>,
) -> Result<OutputSummary, OutputError> {
    let rate = pcm.summary.sample_rate;
    let channels = usize::from(pcm.summary.channels);
    if !(8_000..=192_000).contains(&rate)
        || !(1..=2).contains(&channels)
        || pcm.samples.is_empty()
        || pcm.samples.len() > 1_048_576
        || pcm.samples.len() % channels != 0
        || pcm.samples.len() / channels != pcm.summary.frames
        || pcm.samples.iter().any(|v| !v.is_finite())
    {
        return Err(OutputError::InvalidPcm);
    }
    let now = Instant::now();
    let deadline = remaining
        .map(|duration| now.checked_add(duration).ok_or(OutputError::InvalidPcm))
        .transpose()?;
    if remaining == Some(Duration::ZERO) {
        return Err(OutputError::Deadline);
    }
    let total = pcm.summary.frames as u64;
    let mut output = Output::open(rate, total, deadline)?;
    let mut cursor = 0;
    let mut started = false;
    let mut last_frames = 0;
    let mut last_progress = Instant::now();
    loop {
        output.check()?;
        if deadline.is_some_and(|end| Instant::now() >= end) {
            return Err(OutputError::Deadline);
        }
        while cursor < pcm.samples.len() && output.producer.slots() > 0 {
            let left = pcm.samples[cursor].clamp(-1.0, 1.0) * 0.25;
            let right = if channels == 2 {
                pcm.samples[cursor + 1].clamp(-1.0, 1.0) * 0.25
            } else {
                left
            };
            if output.producer.push([left, right]).is_err() {
                break;
            }
            cursor += channels;
        }
        if !started {
            output.stream.play().map_err(|_| OutputError::Device)?;
            output.state.enabled.store(true, Ordering::Release);
            started = true;
        }
        let frames = output.state.frames.load(Ordering::Acquire);
        if output.state.ended.load(Ordering::Acquire) {
            // The last callback has submitted audio, not necessarily played it yet.
            // Keep the stream alive for a bounded hardware drain, checking errors/deadline.
            let drain_end = Instant::now() + Duration::from_millis(250);
            while Instant::now() < drain_end {
                output.check()?;
                if deadline.is_some_and(|end| Instant::now() >= end) {
                    return Err(OutputError::Deadline);
                }
                thread::sleep(Duration::from_millis(5));
            }
            return Ok(OutputSummary {
                frames: output.state.frames.load(Ordering::Acquire),
                underrun_frames: output.state.underrun_frames.load(Ordering::Relaxed),
                sample_rate: rate,
                channels: output.channels,
            });
        }
        if frames != last_frames {
            last_frames = frames;
            last_progress = Instant::now();
        }
        if last_progress.elapsed() > Duration::from_secs(3) {
            return Err(OutputError::Stalled);
        }
        thread::sleep(Duration::from_millis(5));
    }
}

/// Keeps one device stream alive across segment boundaries.
pub struct ContinuousOutput {
    output: Output,
    rate: u32,
    source_channels: usize,
    accepted: u64,
    maximum: u64,
    started: bool,
    last_callback: u64,
    last_progress: Instant,
    drained_at: Option<Instant>,
}
impl ContinuousOutput {
    pub fn new(
        pcm: &DecodedPcm,
        maximum: u64,
        remaining: Option<Duration>,
        stop: StopToken,
    ) -> Result<Self, OutputError> {
        validate_pcm(pcm)?;
        if maximum == 0 || remaining == Some(Duration::ZERO) {
            return Err(OutputError::Deadline);
        }
        let deadline = remaining
            .map(|d| Instant::now().checked_add(d).ok_or(OutputError::InvalidPcm))
            .transpose()?;
        let rate = pcm.summary.sample_rate;
        let output = Output::open_with_stop(rate, maximum, deadline, stop)?;
        output.state.volume.store(25, Ordering::Release);
        Ok(Self {
            output,
            rate,
            source_channels: usize::from(pcm.summary.channels),
            accepted: 0,
            maximum,
            started: false,
            last_callback: 0,
            last_progress: Instant::now(),
            drained_at: None,
        })
    }
    /// Nonblocking producer step; caller retains the current segment until complete.
    pub fn enqueue(&mut self, pcm: &DecodedPcm, cursor: &mut usize) -> Result<bool, OutputError> {
        if *cursor == 0 {
            validate_pcm(pcm)?;
        }
        if pcm.summary.sample_rate != self.rate
            || usize::from(pcm.summary.channels) != self.source_channels
        {
            return Err(OutputError::UnsupportedFormat);
        }
        self.output.check()?;
        while *cursor < pcm.samples.len()
            && self.accepted < self.maximum
            && self.output.producer.slots() > 0
        {
            let left = pcm.samples[*cursor].clamp(-1.0, 1.0);
            let right = if self.source_channels == 2 {
                pcm.samples[*cursor + 1].clamp(-1.0, 1.0)
            } else {
                left
            };
            if self.output.producer.push([left, right]).is_err() {
                break;
            }
            *cursor += self.source_channels;
            self.accepted += 1;
        }
        if !self.started && self.accepted >= self.maximum.min(u64::from(self.rate) * 2) {
            self.output.stream.play().map_err(|_| OutputError::Device)?;
            self.output.state.enabled.store(true, Ordering::Release);
            self.started = true;
            self.last_progress = Instant::now();
        }
        Ok(*cursor == pcm.samples.len() || self.accepted == self.maximum)
    }
    pub fn finish(&mut self) {
        if !self.started {
            match self.output.stream.play() {
                Ok(()) => {
                    self.output.state.enabled.store(true, Ordering::Release);
                    self.started = true;
                    self.last_progress = Instant::now();
                }
                Err(_) => self.output.state.failed.store(true, Ordering::Release),
            }
        }
        self.output
            .state
            .total
            .store(self.accepted, Ordering::Release);
    }
    pub fn accepted(&self) -> u64 {
        self.accepted
    }
    pub fn summary(&self) -> OutputSummary {
        OutputSummary {
            frames: self.output.state.frames.load(Ordering::Acquire),
            underrun_frames: self.output.state.underrun_frames.load(Ordering::Relaxed),
            sample_rate: self.rate,
            channels: self.output.channels,
        }
    }
    pub fn configure(&self, paused: bool, volume: u32) {
        self.output.state.paused.store(paused, Ordering::Release);
        self.output
            .state
            .volume
            .store(volume.min(100), Ordering::Release);
    }
    pub fn poll(&mut self) -> Result<bool, OutputError> {
        self.output.check()?;
        let callbacks = self.output.state.callbacks.load(Ordering::Acquire);
        if callbacks != self.last_callback {
            self.last_callback = callbacks;
            self.last_progress = Instant::now();
        }
        if self.output.state.ended.load(Ordering::Acquire) {
            let start = self.drained_at.get_or_insert_with(Instant::now);
            return Ok(start.elapsed() >= Duration::from_millis(250));
        }
        if self.started && self.last_progress.elapsed() > Duration::from_secs(3) {
            return Err(OutputError::Stalled);
        }
        Ok(false)
    }
}
fn validate_pcm(pcm: &DecodedPcm) -> Result<(), OutputError> {
    let channels = usize::from(pcm.summary.channels);
    if !(8_000..=192_000).contains(&pcm.summary.sample_rate)
        || !(1..=2).contains(&channels)
        || pcm.samples.is_empty()
        || pcm.samples.len() > 1_048_576
        || pcm.samples.len() % channels != 0
        || pcm.samples.len() / channels != pcm.summary.frames
        || pcm.samples.iter().any(|v| !v.is_finite())
    {
        return Err(OutputError::InvalidPcm);
    }
    Ok(())
}

pub fn probe_tone(seconds: u32) -> Result<OutputSummary, OutputError> {
    if !(1..=10).contains(&seconds) {
        return Err(OutputError::InvalidPcm);
    }
    let rate = 48_000;
    let frames = rate as usize * seconds as usize;
    let samples = (0..frames)
        .flat_map(|i| {
            let envelope = (i.min(frames - 1 - i) as f32 / 960.0).min(1.0);
            let value =
                (i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin() * 0.1 * envelope;
            [value, value]
        })
        .collect();
    play_pcm(
        &DecodedPcm {
            samples,
            summary: super::aac::AacSummary {
                core_sample_rate: rate,
                core_channels: 2,
                sample_rate: rate,
                channels: 2,
                packets: 0,
                frames,
                sbr_detected: false,
                stereo_expansion: false,
                media_duration: Duration::from_secs(u64::from(seconds)),
                decode_duration: Duration::ZERO,
                nonzero_samples: 0,
            },
        },
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paused_queue_keeps_position_and_volume_applies_to_already_queued_audio() {
        let (mut producer, mut renderer) = queue(48_000, 10, None);
        producer.push([0.8, -0.4]).unwrap();
        renderer.state.paused.store(true, Ordering::Release);
        let mut data = [1.0; 2];
        for _ in 0..3 {
            renderer.render(&mut data, 2, Instant::now());
        }
        assert_eq!(data, [0.0; 2]);
        assert_eq!(renderer.state.frames.load(Ordering::Acquire), 0);
        assert_eq!(renderer.state.underrun_frames.load(Ordering::Acquire), 0);
        assert_eq!(renderer.queue.slots(), 1);
        renderer.state.volume.store(50, Ordering::Release);
        renderer.state.paused.store(false, Ordering::Release);
        renderer.render(&mut data, 2, Instant::now());
        assert_eq!(data, [0.4, -0.2]);
        assert_eq!(renderer.state.frames.load(Ordering::Acquire), 1);
        producer.push([0.8, 0.8]).unwrap();
        renderer.state.volume.store(0, Ordering::Release);
        renderer.render(&mut data, 2, Instant::now());
        assert_eq!(data, [0.0; 2]);
        assert_eq!(renderer.state.frames.load(Ordering::Acquire), 2);
    }
    #[test]
    fn live_deadline_expires_even_while_paused() {
        let now = Instant::now();
        let (mut producer, mut renderer) = queue(48_000, 10, Some(now));
        producer.push([0.5, 0.5]).unwrap();
        renderer.state.paused.store(true, Ordering::Release);
        renderer.render(&mut [1.0; 2], 2, now);
        assert!(renderer.state.expired.load(Ordering::Acquire));
        assert_eq!(renderer.state.frames.load(Ordering::Acquire), 0);
    }
    #[test]
    fn prebuffered_audio_stays_silent_until_enabled_and_old_generation_cannot_cancel_new() {
        let parent = StopToken::default();
        let old = parent.child();
        let new = parent.child();
        let (mut producer, mut renderer) = queue(48_000, 10, None);
        renderer.cancel = new.clone();
        renderer.state.enabled.store(false, Ordering::Release);
        producer.push([0.5, 0.5]).unwrap();
        let mut samples = [1.0; 2];
        renderer.render(&mut samples, 2, Instant::now());
        assert_eq!(samples, [0.0; 2]);
        assert_eq!(renderer.state.frames.load(Ordering::Acquire), 0);
        old.cancel();
        assert!(!parent.is_cancelled());
        assert!(!new.is_cancelled());
        renderer.state.enabled.store(true, Ordering::Release);
        renderer.render(&mut samples, 2, Instant::now());
        assert_eq!(samples, [0.5, 0.5]);
        parent.cancel();
        assert!(new.is_cancelled());
        producer.push([1.0, 1.0]).unwrap();
        renderer.render(&mut samples, 2, Instant::now());
        assert_eq!(samples, [0.0; 2]);
    }
    #[test]
    fn shared_cancellation_mutes_queue_and_end_limit_can_shrink_after_eof() {
        let (mut producer, mut renderer) = queue(48_000, 100, None);
        let stop = renderer.cancel.clone();
        producer.push([0.5, 0.5]).unwrap();
        stop.cancel();
        let mut samples = [1.0; 4];
        renderer.render(&mut samples, 2, Instant::now());
        assert_eq!(samples, [0.0; 4]);
        assert_eq!(renderer.state.frames.load(Ordering::Acquire), 0);
        let (mut producer, mut renderer) = queue(48_000, 100, None);
        producer.push([0.5, 0.5]).unwrap();
        producer.push([0.5, 0.5]).unwrap();
        renderer.state.total.store(1, Ordering::Release);
        renderer.render(&mut samples, 2, Instant::now());
        assert_eq!(samples, [0.5, 0.5, 0.0, 0.0]);
        assert!(renderer.state.ended.load(Ordering::Acquire));
    }
    #[test]
    fn callback_crossing_live_end_does_not_submit_frames_after_deadline() {
        let now = Instant::now();
        let (mut producer, mut renderer) =
            queue(48_000, 100, Some(now + Duration::from_micros(50)));
        for _ in 0..4 {
            producer.push([0.5, 0.5]).unwrap();
        }
        let mut samples = [1.0; 8];
        renderer.render(&mut samples, 2, now);
        assert_eq!(samples, [0.5, 0.5, 0.5, 0.5, 0.0, 0.0, 0.0, 0.0]);
        assert!(renderer.state.expired.load(Ordering::Acquire));
        assert_eq!(renderer.state.frames.load(Ordering::Acquire), 2);
    }
    #[test]
    fn underrun_is_silence_and_does_not_advance_media_position_or_split_stereo_frames() {
        let (mut producer, mut renderer) = queue(48_000, 3, None);
        producer.push([0.2, -0.4]).unwrap();
        let mut samples = [1.0; 6];
        renderer.render(&mut samples, 2, Instant::now());
        assert_eq!(samples, [0.2, -0.4, 0.0, 0.0, 0.0, 0.0]);
        assert_eq!(renderer.state.frames.load(Ordering::Relaxed), 1);
        assert_eq!(renderer.state.underrun_frames.load(Ordering::Relaxed), 2);
        assert!(!renderer.state.ended.load(Ordering::Relaxed));
        assert_eq!(renderer.state.callbacks.load(Ordering::Acquire), 1);
        renderer.render(&mut samples[..2], 2, Instant::now());
        assert_eq!(renderer.state.frames.load(Ordering::Acquire), 1);
        assert_eq!(renderer.state.callbacks.load(Ordering::Acquire), 2);
        producer.push([0.6, 0.2]).unwrap();
        renderer.render(&mut samples[..1], 1, Instant::now());
        assert!((samples[0] - 0.4).abs() < 0.00001);
        assert_eq!(renderer.state.frames.load(Ordering::Relaxed), 2);
    }
    #[test]
    fn bounded_queue_rejects_excess_frames_and_sample_end_leaves_only_silence() {
        let (mut producer, mut renderer) = queue(2, 2, None);
        for _ in 0..4 {
            producer.push([0.5, 0.5]).unwrap();
        }
        assert!(producer.push([1.0, 1.0]).is_err());
        let mut samples = [1.0; 8];
        renderer.render(&mut samples, 2, Instant::now());
        assert_eq!(samples, [0.5, 0.5, 0.5, 0.5, 0.0, 0.0, 0.0, 0.0]);
        assert!(renderer.state.ended.load(Ordering::Relaxed));
        renderer.render(&mut samples, 2, Instant::now());
        assert_eq!(samples, [0.0; 8]);
        assert_eq!(renderer.state.frames.load(Ordering::Relaxed), 2);
    }
    #[test]
    fn stop_device_failure_and_live_deadline_mute_queued_audio() {
        let now = Instant::now();
        for reason in 0..3 {
            let (mut producer, mut renderer) = queue(48_000, 10, (reason == 2).then_some(now));
            producer.push([1.0, 1.0]).unwrap();
            if reason == 0 {
                renderer.state.stopped.store(true, Ordering::Release);
            }
            if reason == 1 {
                renderer.state.failed.store(true, Ordering::Release);
            }
            let mut samples = [1.0; 4];
            renderer.render(&mut samples, 2, now);
            assert_eq!(samples, [0.0; 4]);
            assert_eq!(renderer.state.frames.load(Ordering::Relaxed), 0);
            assert_eq!(renderer.state.expired.load(Ordering::Relaxed), reason == 2);
            drop(renderer);
            assert!(producer.is_abandoned());
        }
    }
    #[test]
    fn invalid_pcm_and_expired_slot_are_rejected_without_accessing_hardware() {
        let summary = super::super::aac::AacSummary {
            core_sample_rate: 48_000,
            core_channels: 2,
            sample_rate: 48_000,
            channels: 2,
            packets: 1,
            frames: 1,
            sbr_detected: false,
            stereo_expansion: false,
            media_duration: Duration::ZERO,
            decode_duration: Duration::ZERO,
            nonzero_samples: 0,
        };
        for samples in [vec![], vec![f32::NAN, 0.0], vec![0.0]] {
            assert_eq!(
                play_pcm(&DecodedPcm { summary, samples }, None).unwrap_err(),
                OutputError::InvalidPcm
            );
        }
        assert_eq!(
            play_pcm(
                &DecodedPcm {
                    summary,
                    samples: vec![0.0, 0.0]
                },
                Some(Duration::ZERO)
            )
            .unwrap_err(),
            OutputError::Deadline
        );
        assert_eq!(probe_tone(0).unwrap_err(), OutputError::InvalidPcm);
        assert_eq!(probe_tone(11).unwrap_err(), OutputError::InvalidPcm);
    }
}
