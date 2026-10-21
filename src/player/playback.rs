//! 主スレッドで出力を制御し、通信・デコードは容量制限付きの単一ワーカーで実行する。
use super::{
    aac::{AacError, AdtsDecoder, DecodedPcm},
    diagnostics::{Stage, Tracker},
    output::{ContinuousOutput, OutputError, OutputSummary, StopToken},
};
use crate::radiko::stream::{
    ProbeError, ProbeMode,
    playback::{Next, SegmentSource, StreamInfo},
};
use chrono::Utc;
use std::{
    fmt,
    sync::mpsc::{self, Receiver, TryRecvError, TrySendError},
    thread,
    time::{Duration, Instant},
};

#[derive(Debug)]
pub enum PlaybackError {
    Source(ProbeError),
    Storage(crate::storage::StorageError),
    LoginRequired,
    Auth(crate::radiko::auth::AuthError),
    Decode(AacError),
    Output(OutputError),
    EarlyEnd,
    SourceStalled,
    Worker,
}
impl fmt::Display for PlaybackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Storage(e) => e.fmt(f),
            Self::LoginRequired => f.write_str("未ログインです。loginでログインしてください"),
            Self::Auth(e) => e.fmt(f),
            Self::Source(e) => write!(f, "配信取得: {e}"),
            Self::Decode(e) => write!(f, "デコード: {e}"),
            Self::Output(e) => e.fmt(f),
            Self::EarlyEnd => f.write_str("放送枠末尾より前に配信が終了しました"),
            Self::SourceStalled => {
                f.write_str("30秒間新しいセグメントを確認できないため停止しました")
            }
            Self::Worker => f.write_str("再生ワーカーが予期せず終了しました"),
        }
    }
}
impl std::error::Error for PlaybackError {}
impl From<OutputError> for PlaybackError {
    fn from(e: OutputError) -> Self {
        Self::Output(e)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum EndReason {
    ProgramEnd,
    Limit,
    Cancelled,
}
pub struct PlaybackReport {
    pub output: Option<OutputSummary>,
    pub segments: u64,
    pub reason: EndReason,
}
pub use super::state::PlaybackSnapshot as Progress;
pub(super) enum Event {
    Pcm(DecodedPcm),
    End,
    Failed(PlaybackError),
}

pub(super) struct Worker {
    pub(super) stop: StopToken,
    diagnostics: std::sync::Arc<std::sync::Mutex<Tracker>>,
    http_diagnostics: Option<std::sync::Arc<crate::radiko::stream::playback::RequestMeasurements>>,
    handle: Option<thread::JoinHandle<()>>,
}
impl Worker {
    fn diagnostics(&self) -> Option<super::diagnostics::Snapshot> {
        let mut snapshot = self.diagnostics.lock().ok()?.snapshot(Instant::now());
        snapshot.http = self
            .http_diagnostics
            .as_ref()
            .and_then(|measurements| measurements.snapshot());
        Some(snapshot)
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.stop.cancel();
        if self.handle.as_ref().is_some_and(|h| h.is_finished()) {
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
        // Blocking HTTP already in flight has a finite timeout. Never block audio shutdown on it.
    }
}
fn send(tx: &mpsc::SyncSender<Event>, mut event: Event, stop: &StopToken) -> bool {
    loop {
        if stop.is_cancelled() {
            return false;
        }
        match tx.try_send(event) {
            Ok(()) => return true,
            Err(TrySendError::Disconnected(_)) => return false,
            Err(TrySendError::Full(value)) => {
                event = value;
                thread::sleep(Duration::from_millis(5));
            }
        }
    }
}
// One extra decoded segment absorbs acquisition jitter without enlarging the device ring.
const PCM_QUEUE_CAPACITY: usize = 2;

pub(super) fn worker(mut source: impl SegmentSource, stop: StopToken) -> (Worker, Receiver<Event>) {
    let (tx, rx) = mpsc::sync_channel(PCM_QUEUE_CAPACITY);
    let http_diagnostics = source.enable_http_diagnostics();
    let control = stop.clone();
    let cancelled = control.clone();
    source.set_cancellation(std::sync::Arc::new(move || cancelled.is_cancelled()));
    let diagnostics = std::sync::Arc::new(std::sync::Mutex::new(Tracker::new(Instant::now())));
    let measurements = diagnostics.clone();
    let handle = thread::spawn(move || {
        let enter = |stage| {
            if let Ok(mut tracker) = measurements.lock() {
                tracker.enter(stage, Instant::now());
            }
        };
        let mut decoder = AdtsDecoder::new();
        let info = source.info().clone();
        let target = chrono::Duration::from_std(info.position)
            .ok()
            .and_then(|offset| info.starts_at.checked_add_signed(offset));
        let mut seeking = !info.position.is_zero();
        let mut last_segment = Instant::now();
        loop {
            if control.is_cancelled() {
                break;
            }
            enter(Stage::Acquisition);
            let next = match source.poll_segment() {
                Ok(next) => next,
                Err(error) => {
                    enter(Stage::Finished);
                    send(&tx, Event::Failed(PlaybackError::Source(error)), &control);
                    break;
                }
            };
            match next {
                Next::Pending(delay) => {
                    enter(Stage::Waiting);
                    if last_segment.elapsed() > Duration::from_secs(30) {
                        send(&tx, Event::Failed(PlaybackError::SourceStalled), &control);
                        break;
                    }
                    let until = Instant::now() + delay.min(Duration::from_secs(1));
                    while !control.is_cancelled() && Instant::now() < until {
                        thread::sleep(Duration::from_millis(10));
                    }
                }
                Next::End => {
                    enter(Stage::Finished);
                    send(&tx, Event::End, &control);
                    break;
                }
                Next::Segment(segment) => {
                    if control.is_cancelled() {
                        break;
                    }
                    if segment.discontinuity {
                        decoder = AdtsDecoder::new();
                    }
                    enter(Stage::Decode);
                    let decoded = decoder.decode_segment_pcm_controlled(
                        &segment.bytes,
                        segment.codec.as_deref(),
                        || control.is_cancelled(),
                    );
                    enter(Stage::Backpressure);
                    match decoded {
                        Ok(mut pcm) => {
                            if let Ok(mut tracker) = measurements.lock() {
                                tracker.decoded(pcm.summary.media_duration);
                            }
                            if seeking {
                                match trim_to(&mut pcm, segment.starts_at, target) {
                                    Ok(false) => continue,
                                    Ok(true) => seeking = false,
                                    Err(error) => {
                                        send(&tx, Event::Failed(error), &control);
                                        break;
                                    }
                                }
                            }
                            if !send(&tx, Event::Pcm(pcm), &control) {
                                break;
                            }
                            last_segment = Instant::now();
                        }
                        Err(error) => {
                            send(&tx, Event::Failed(PlaybackError::Decode(error)), &control);
                            break;
                        }
                    }
                }
            }
        }
        enter(Stage::Finished);
    });
    (
        Worker {
            stop,
            diagnostics,
            http_diagnostics,
            handle: Some(handle),
        },
        rx,
    )
}

pub(super) struct Ready {
    pub info: StreamInfo,
    pub worker: Worker,
    pub rx: Receiver<Event>,
    pub pcm: DecodedPcm,
    pub segments: u64,
}
pub(super) fn prepare_ready(
    source: Box<dyn SegmentSource>,
    stop: StopToken,
) -> Result<Ready, PlaybackError> {
    let info = source.info().clone();
    let (worker, rx) = worker(source, stop.clone());
    let began = Instant::now();
    let mut primed: Option<DecodedPcm> = None;
    let mut segments = 0;
    loop {
        if stop.is_cancelled() {
            return Err(PlaybackError::Worker);
        }
        if began.elapsed() > Duration::from_secs(30) {
            return Err(PlaybackError::SourceStalled);
        }
        match rx.recv_timeout(Duration::from_millis(10)) {
            Ok(Event::Pcm(pcm)) => {
                segments += 1;
                if let Some(first) = primed.as_mut() {
                    if first.summary.sample_rate != pcm.summary.sample_rate
                        || first.summary.channels != pcm.summary.channels
                    {
                        return Err(PlaybackError::Output(OutputError::UnsupportedFormat));
                    }
                    if first.samples.len() + pcm.samples.len() > 1_048_576 {
                        return Err(PlaybackError::Decode(AacError::TooLarge));
                    }
                    first.samples.extend_from_slice(&pcm.samples);
                    first.summary.frames += pcm.summary.frames;
                    first.summary.packets += pcm.summary.packets;
                    first.summary.nonzero_samples += pcm.summary.nonzero_samples;
                    first.summary.media_duration += pcm.summary.media_duration;
                } else {
                    primed = Some(pcm);
                }
                if let Some(pcm) = primed.as_ref() {
                    let needed = (info.ends_at - info.starts_at)
                        .to_std()
                        .map_err(|_| PlaybackError::EarlyEnd)?
                        .checked_sub(info.position)
                        .ok_or(PlaybackError::EarlyEnd)?
                        .min(Duration::from_secs(2));
                    if pcm.summary.frames as u128
                        >= needed.as_nanos() * u128::from(pcm.summary.sample_rate) / 1_000_000_000
                    {
                        return Ok(Ready {
                            info,
                            worker,
                            rx,
                            pcm: primed.take().ok_or(PlaybackError::Worker)?,
                            segments,
                        });
                    }
                }
            }
            Ok(Event::Failed(error)) => return Err(error),
            Ok(Event::End) => return Err(PlaybackError::EarlyEnd),
            Err(mpsc::RecvTimeoutError::Timeout) => (),
            Err(_) => return Err(PlaybackError::Worker),
        }
    }
}
fn trim_to(
    pcm: &mut DecodedPcm,
    start: Option<chrono::DateTime<chrono::FixedOffset>>,
    target: Option<chrono::DateTime<chrono::FixedOffset>>,
) -> Result<bool, PlaybackError> {
    let (Some(start), Some(target)) = (start, target) else {
        return Err(PlaybackError::Source(ProbeError::InvalidPlaylist));
    };
    let delta = (target - start)
        .to_std()
        .map_err(|_| PlaybackError::Source(ProbeError::InvalidPlaylist))?;
    let skip = delta
        .as_nanos()
        .checked_mul(u128::from(pcm.summary.sample_rate))
        .ok_or(PlaybackError::EarlyEnd)?
        .div_ceil(1_000_000_000);
    if skip >= pcm.summary.frames as u128 {
        return Ok(false);
    }
    let skip = skip as usize;
    pcm.samples
        .drain(..skip * usize::from(pcm.summary.channels));
    pcm.summary.frames -= skip;
    pcm.summary.media_duration =
        Duration::from_secs_f64(pcm.summary.frames as f64 / f64::from(pcm.summary.sample_rate));
    pcm.summary.nonzero_samples = pcm.samples.iter().filter(|s| s.abs() > 0.000001).count();
    Ok(true)
}
fn frame_limit(
    info: &StreamInfo,
    rate: u32,
    seconds: Option<u32>,
) -> Result<(u64, EndReason), PlaybackError> {
    let duration = (info.ends_at - info.starts_at)
        .to_std()
        .map_err(|_| PlaybackError::EarlyEnd)?
        .checked_sub(info.position)
        .ok_or(PlaybackError::EarlyEnd)?;
    let (duration, reason) = match seconds {
        Some(seconds) if Duration::from_secs(u64::from(seconds)) < duration => {
            (Duration::from_secs(u64::from(seconds)), EndReason::Limit)
        }
        _ => (duration, EndReason::ProgramEnd),
    };
    let frames = duration
        .as_nanos()
        .checked_mul(u128::from(rate))
        .and_then(|n| u64::try_from(n / 1_000_000_000).ok())
        .filter(|n| *n > 0)
        .ok_or(PlaybackError::EarlyEnd)?;
    Ok((frames, reason))
}

trait AudioSink {
    fn configure(&self, _paused: bool, _volume: u32) {}
    fn enqueue(&mut self, pcm: &DecodedPcm, cursor: &mut usize) -> Result<bool, OutputError>;
    fn accepted(&self) -> u64;
    fn summary(&self) -> OutputSummary;
    fn poll(&mut self) -> Result<bool, OutputError>;
    fn finish(&mut self);
}
// Explicit parent shutdown wins over errors observed during the same loop iteration.
// Check before CancelOnExit runs; otherwise its teardown cancellation would hide real errors.
fn failure_or_cancelled<S: AudioSink>(
    stop: &StopToken,
    output: Option<&S>,
    segments: u64,
    error: PlaybackError,
) -> Result<PlaybackReport, PlaybackError> {
    if stop.is_cancelled() {
        Ok(PlaybackReport {
            output: output.map(AudioSink::summary),
            segments,
            reason: EndReason::Cancelled,
        })
    } else {
        Err(error)
    }
}

impl AudioSink for ContinuousOutput {
    fn configure(&self, paused: bool, volume: u32) {
        self.configure(paused, volume);
    }
    fn enqueue(&mut self, pcm: &DecodedPcm, cursor: &mut usize) -> Result<bool, OutputError> {
        self.enqueue(pcm, cursor)
    }
    fn accepted(&self) -> u64 {
        self.accepted()
    }
    fn summary(&self) -> OutputSummary {
        self.summary()
    }
    fn poll(&mut self) -> Result<bool, OutputError> {
        self.poll()
    }
    fn finish(&mut self) {
        self.finish();
    }
}

pub fn play(
    source: impl SegmentSource,
    seconds: Option<u32>,
    stop: StopToken,
    progress: impl FnMut(Progress),
) -> Result<PlaybackReport, PlaybackError> {
    play_with(source, seconds, stop, progress, ContinuousOutput::new)
}

pub fn play_controlled(
    source: impl SegmentSource,
    seconds: Option<u32>,
    stop: StopToken,
    controls: &mut super::seek::Controller,
    progress: impl FnMut(Progress),
) -> Result<PlaybackReport, PlaybackError> {
    play_loop(
        Initial::Source(source),
        seconds,
        stop,
        progress,
        ContinuousOutput::new,
        Some(controls),
    )
}
/// Wait without decoding/output until a validated Select is prepared by the controller.
/// `idle` is only a control context; no snapshot claims it is playing.
pub fn play_session(
    idle: StreamInfo,
    stop: StopToken,
    controls: &mut super::seek::Controller,
    progress: impl FnMut(Progress),
) -> Result<PlaybackReport, PlaybackError> {
    play_loop::<ContinuousOutput>(
        Initial::<Box<dyn SegmentSource>>::Idle(idle),
        None,
        stop,
        progress,
        ContinuousOutput::new,
        Some(controls),
    )
}
fn play_with<S: AudioSink>(
    source: impl SegmentSource,
    seconds: Option<u32>,
    stop: StopToken,
    progress: impl FnMut(Progress),
    create: impl FnMut(&DecodedPcm, u64, Option<Duration>, StopToken) -> Result<S, OutputError>,
) -> Result<PlaybackReport, PlaybackError> {
    play_loop(
        Initial::Source(source),
        seconds,
        stop,
        progress,
        create,
        None,
    )
}
enum Initial<T> {
    Source(T),
    Idle(StreamInfo),
}
fn play_loop<S: AudioSink>(
    initial: Initial<impl SegmentSource>,
    seconds: Option<u32>,
    stop: StopToken,
    mut progress: impl FnMut(Progress),
    mut create: impl FnMut(&DecodedPcm, u64, Option<Duration>, StopToken) -> Result<S, OutputError>,
    mut controls: Option<&mut super::seek::Controller>,
) -> Result<PlaybackReport, PlaybackError> {
    struct CancelOnExit(StopToken);
    impl Drop for CancelOnExit {
        fn drop(&mut self) {
            self.0.cancel();
        }
    }
    let _cancel = CancelOnExit(stop.clone());
    let mut local = stop.child();
    let (mut info, mut running_worker, mut rx) = match initial {
        Initial::Source(source) => {
            let info = source.info().clone();
            let (worker, rx) = worker(source, local.clone());
            (info, Some(worker), rx)
        }
        Initial::Idle(info) => (info, None, mpsc::channel().1),
    };
    let mut current = None;
    let mut cursor = 0;
    let mut output: Option<S> = None;
    let mut maximum = 0;
    let mut reason = EndReason::ProgramEnd;
    let mut segments = 0;
    let mut eof = false;
    let mut report_at = Instant::now();
    let mut last_pcm_at = Instant::now();
    loop {
        if stop.is_cancelled() {
            return Ok(PlaybackReport {
                output: output.as_ref().map(|o| o.summary()),
                segments,
                reason: EndReason::Cancelled,
            });
        }
        if let Some(control) = controls.as_mut() {
            let position = output
                .as_ref()
                .map(|o| {
                    let v = o.summary();
                    info.position
                        + Duration::from_secs_f64(v.frames as f64 / f64::from(v.sample_rate))
                })
                .unwrap_or(info.position);
            if let Some(ready) = control.poll(&info, position) {
                let new_limit = frame_limit(&ready.info, ready.pcm.summary.sample_rate, seconds);
                match new_limit.and_then(|(limit, why)| {
                    create(
                        &ready.pcm,
                        limit,
                        if ready.info.mode == ProbeMode::Live {
                            Some(
                                (ready.info.ends_at - Utc::now().fixed_offset())
                                    .to_std()
                                    .unwrap_or(Duration::ZERO),
                            )
                        } else {
                            None
                        },
                        ready.worker.stop.clone(),
                    )
                    .map(|sink| (sink, limit, why))
                    .map_err(PlaybackError::Output)
                }) {
                    Err(error) => {
                        control.report(format!("再生切替失敗（現在の状態を維持）: {error}"))
                    }
                    Ok((sink, limit, why)) => {
                        control.paused = false;
                        local.cancel();
                        output = Some(sink);
                        local = ready.worker.stop.clone();
                        running_worker = Some(ready.worker);
                        rx = ready.rx;
                        info = ready.info;
                        current = Some(ready.pcm);
                        cursor = 0;
                        maximum = limit;
                        reason = why;
                        segments = ready.segments;
                        eof = false;
                        last_pcm_at = Instant::now();
                        report_at = Instant::now();
                        control.report(format!("再生切替完了: {} / {:?}", info.station, info.mode));
                    }
                }
            }
        }
        if controls
            .as_mut()
            .is_some_and(|c| std::mem::take(&mut c.changed))
        {
            report_at = Instant::now();
        }
        let paused = controls.as_ref().is_some_and(|c| c.paused);
        let volume = controls.as_ref().map_or(25, |c| c.volume);
        if let Some(out) = &output {
            out.configure(paused, volume);
        }
        if paused {
            last_pcm_at = Instant::now();
        }
        if running_worker.is_some() && info.mode == ProbeMode::Live && Utc::now() >= info.ends_at {
            return Ok(PlaybackReport {
                output: output.as_ref().map(|o| o.summary()),
                segments,
                reason: EndReason::ProgramEnd,
            });
        }
        if running_worker.is_some()
            && !paused
            && current.is_none()
            && !eof
            && !output.as_ref().is_some_and(|o| o.accepted() >= maximum)
        {
            let event = rx.try_recv();
            if stop.is_cancelled() {
                continue;
            }
            match event {
                Ok(Event::Pcm(pcm)) => {
                    last_pcm_at = Instant::now();
                    if output.is_none() {
                        (maximum, reason) = frame_limit(&info, pcm.summary.sample_rate, seconds)?;
                        let remaining = if info.mode == ProbeMode::Live {
                            Some(
                                (info.ends_at - Utc::now().fixed_offset())
                                    .to_std()
                                    .unwrap_or(Duration::ZERO),
                            )
                        } else {
                            None
                        };
                        let sink = match create(&pcm, maximum, remaining, local.clone()) {
                            Ok(sink) => sink,
                            Err(error) => {
                                return failure_or_cancelled(
                                    &stop,
                                    output.as_ref(),
                                    segments,
                                    error.into(),
                                );
                            }
                        };
                        sink.configure(paused, volume);
                        output = Some(sink);
                        report_at = Instant::now();
                    }
                    segments += 1;
                    current = Some(pcm);
                    cursor = 0;
                }
                Ok(Event::Failed(error)) => {
                    return failure_or_cancelled(&stop, output.as_ref(), segments, error);
                }
                Ok(Event::End) => {
                    let Some(out) = output.as_mut() else {
                        return failure_or_cancelled(
                            &stop,
                            output.as_ref(),
                            segments,
                            PlaybackError::EarlyEnd,
                        );
                    };
                    if info.mode == ProbeMode::Live
                        || maximum.saturating_sub(out.accepted())
                            > u64::from(out.summary().sample_rate) / 10
                    {
                        return failure_or_cancelled(
                            &stop,
                            output.as_ref(),
                            segments,
                            PlaybackError::EarlyEnd,
                        );
                    }
                    out.finish();
                    eof = true;
                }
                Err(TryRecvError::Empty) => (),
                Err(TryRecvError::Disconnected) => {
                    return failure_or_cancelled(
                        &stop,
                        output.as_ref(),
                        segments,
                        PlaybackError::Worker,
                    );
                }
            }
        }
        if running_worker.is_some()
            && current.is_none()
            && !eof
            && !output.as_ref().is_some_and(|o| o.accepted() >= maximum)
            && last_pcm_at.elapsed() > Duration::from_secs(30)
        {
            return failure_or_cancelled(
                &stop,
                output.as_ref(),
                segments,
                PlaybackError::SourceStalled,
            );
        }
        if let Some(out) = output.as_mut() {
            if let Some(pcm) = current.as_ref() {
                match out.enqueue(pcm, &mut cursor) {
                    Ok(true) => current = None,
                    Ok(false) => (),
                    Err(OutputError::Deadline) => {
                        return Ok(PlaybackReport {
                            output: Some(out.summary()),
                            segments,
                            reason: EndReason::ProgramEnd,
                        });
                    }
                    Err(error) => {
                        return failure_or_cancelled(&stop, Some(&*out), segments, error.into());
                    }
                }
            }
            match out.poll() {
                Ok(true) => {
                    return Ok(PlaybackReport {
                        output: Some(out.summary()),
                        segments,
                        reason,
                    });
                }
                Ok(false) => (),
                Err(OutputError::Deadline) => {
                    return Ok(PlaybackReport {
                        output: Some(out.summary()),
                        segments,
                        reason: EndReason::ProgramEnd,
                    });
                }
                Err(error) => {
                    return failure_or_cancelled(&stop, Some(&*out), segments, error.into());
                }
            }
        }
        if running_worker.is_some() && Instant::now() >= report_at {
            let mut snapshot = Progress::active(
                &info,
                output.as_ref().map(|out| out.summary()),
                segments,
                paused,
                volume,
                controls
                    .as_ref()
                    .and_then(|control| control.pending_target.clone()),
            );
            snapshot.pipeline = running_worker.as_ref().and_then(Worker::diagnostics);
            progress(snapshot);
            report_at = Instant::now()
                + controls.as_ref().map_or(Duration::from_secs(5), |control| {
                    control.notification_interval
                });
        }
        thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::radiko::stream::playback::Segment;
    use std::collections::VecDeque;
    #[test]
    fn two_segment_prefetch_is_bounded_and_cancel_does_not_acquire_a_fourth_segment() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        struct Counted {
            source: Source,
            polls: Arc<AtomicUsize>,
        }
        impl SegmentSource for Counted {
            fn info(&self) -> &StreamInfo {
                self.source.info()
            }
            fn poll_segment(&mut self) -> Result<Next, ProbeError> {
                self.polls.fetch_add(1, Ordering::SeqCst);
                self.source.poll_segment()
            }
        }
        let polls = Arc::new(AtomicUsize::new(0));
        let pcm = packet(24_000).repeat(50);
        let stop = StopToken::default();
        let (mut worker, receiver) = worker(
            Counted {
                source: Source {
                    info: info(3_600_000),
                    events: (0..4).map(|_| event(pcm.clone(), false)).collect(),
                },
                polls: polls.clone(),
            },
            stop.clone(),
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let snapshot = worker.diagnostics().unwrap();
            if snapshot.decoded_segments == 3 && snapshot.stage == Stage::Backpressure {
                assert!(!snapshot.media_last.is_zero());
                break;
            }
            assert!(
                Instant::now() < deadline,
                "worker did not fill its bounded queue"
            );
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(polls.load(Ordering::SeqCst), 3);
        stop.cancel();
        worker.handle.take().unwrap().join().unwrap();
        assert_eq!(polls.load(Ordering::SeqCst), 3);
        let queued: Vec<_> = receiver.try_iter().collect();
        assert_eq!(queued.len(), 2);
        for event in queued {
            let Event::Pcm(pcm) = event else {
                panic!("queue must contain decoded PCM only");
            };
            assert!(pcm.samples.len() * std::mem::size_of::<f32>() <= 4 * 1024 * 1024);
        }
        assert_eq!(worker.diagnostics().unwrap().stage, Stage::Finished);
    }
    #[test]
    fn boxed_source_receives_generation_cancellation_before_polling() {
        use std::sync::Arc;
        struct Waiting {
            info: StreamInfo,
            cancelled: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
            entered: mpsc::Sender<()>,
            done: mpsc::Sender<()>,
            http: Arc<crate::radiko::stream::playback::RequestMeasurements>,
        }
        impl SegmentSource for Waiting {
            fn enable_http_diagnostics(
                &mut self,
            ) -> Option<Arc<crate::radiko::stream::playback::RequestMeasurements>> {
                Some(self.http.clone())
            }
            fn info(&self) -> &StreamInfo {
                &self.info
            }
            fn set_cancellation(&mut self, cancelled: Arc<dyn Fn() -> bool + Send + Sync>) {
                self.cancelled = Some(cancelled);
            }
            fn poll_segment(&mut self) -> Result<Next, ProbeError> {
                let check = self
                    .cancelled
                    .as_ref()
                    .expect("hook installed before polling");
                self.http
                    .enter(crate::radiko::stream::playback::RequestStage::Headers);
                self.entered.send(()).unwrap();
                let began = Instant::now();
                while !check() {
                    assert!(began.elapsed() < Duration::from_secs(2));
                    thread::sleep(Duration::from_millis(1));
                }
                self.http
                    .enter(crate::radiko::stream::playback::RequestStage::Idle);
                self.done.send(()).unwrap();
                Err(ProbeError::Cancelled)
            }
        }
        let (entered, seen) = mpsc::channel();
        let (done, finished) = mpsc::channel();
        let start = chrono::DateTime::parse_from_rfc3339("2026-10-16T12:00:00+09:00").unwrap();
        let source: Box<dyn SegmentSource> = Box::new(Waiting {
            info: StreamInfo {
                title: None,
                station: "TBS".into(),
                mode: ProbeMode::TimeFree,
                starts_at: start,
                ends_at: start + chrono::Duration::hours(1),
                position: Duration::ZERO,
            },
            http: Arc::new(crate::radiko::stream::playback::RequestMeasurements::new()),
            cancelled: None,
            entered,
            done,
        });
        let parent = StopToken::default();
        let (mut worker, events) = worker(source, parent.child());
        seen.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(
            worker.diagnostics().unwrap().http.unwrap().stage,
            crate::radiko::stream::playback::RequestStage::Headers
        );
        parent.cancel();
        finished.recv_timeout(Duration::from_secs(2)).unwrap();
        worker.handle.take().unwrap().join().unwrap();
        assert!(events.try_recv().is_err());
        assert_eq!(
            worker.diagnostics().unwrap().http.unwrap().stage,
            crate::radiko::stream::playback::RequestStage::Idle
        );
    }
    struct Source {
        info: StreamInfo,
        events: VecDeque<Result<Next, ProbeError>>,
    }
    impl SegmentSource for Source {
        fn info(&self) -> &StreamInfo {
            &self.info
        }
        fn poll_segment(&mut self) -> Result<Next, ProbeError> {
            self.events.pop_front().unwrap_or(Ok(Next::End))
        }
    }
    struct Sink {
        frames: u64,
        maximum: u64,
        rate: u32,
        channels: u16,
    }
    impl AudioSink for Sink {
        fn enqueue(&mut self, pcm: &DecodedPcm, cursor: &mut usize) -> Result<bool, OutputError> {
            if pcm.summary.sample_rate != self.rate || pcm.summary.channels != self.channels {
                return Err(OutputError::UnsupportedFormat);
            }
            self.frames += (pcm.summary.frames as u64).min(self.maximum - self.frames);
            *cursor = pcm.samples.len();
            Ok(true)
        }
        fn accepted(&self) -> u64 {
            self.frames
        }
        fn summary(&self) -> OutputSummary {
            OutputSummary {
                frames: self.frames,
                underrun_frames: 0,
                sample_rate: self.rate,
                channels: self.channels,
            }
        }
        fn poll(&mut self) -> Result<bool, OutputError> {
            Ok(self.frames == self.maximum)
        }
        fn finish(&mut self) {
            self.maximum = self.frames;
        }
    }
    fn sink(
        pcm: &DecodedPcm,
        maximum: u64,
        _: Option<Duration>,
        _: StopToken,
    ) -> Result<Sink, OutputError> {
        Ok(Sink {
            frames: 0,
            maximum,
            rate: pcm.summary.sample_rate,
            channels: pcm.summary.channels,
        })
    }
    fn info(milliseconds: i64) -> StreamInfo {
        let start = Utc::now().fixed_offset();
        StreamInfo {
            title: Some("合成テスト番組".into()),
            station: "TBS".into(),
            mode: ProbeMode::TimeFree,
            starts_at: start,
            ends_at: start + chrono::Duration::milliseconds(milliseconds),
            position: Duration::ZERO,
        }
    }
    fn packet(rate: u32) -> Vec<u8> {
        use rusty_aac::{AacEncoder, AacEncoderConfig, AdtsHeader, write_adts_header};
        let mut encoder = AacEncoder::new(AacEncoderConfig::default());
        let samples: Vec<f32> = (0..2048).map(|i| (i as f32 * 0.1).sin() * 0.1).collect();
        encoder.push_pcm(&samples, 1, rate).unwrap();
        encoder.finish();
        let packet = encoder.next_packet().unwrap();
        let mut bytes = write_adts_header(&AdtsHeader {
            object_type: 2,
            sample_rate: rate,
            channels: 1,
            frame_length: packet.data.len() + 7,
            header_len: 7,
        });
        bytes.extend(packet.data);
        bytes
    }
    fn event(bytes: Vec<u8>, discontinuity: bool) -> Result<Next, ProbeError> {
        Ok(Next::Segment(Segment {
            bytes,
            codec: Some("mp4a.40.2".into()),
            discontinuity,
            starts_at: None,
        }))
    }
    #[test]
    fn idle_session_reports_preparation_failure_without_output_or_active_snapshot() {
        use super::super::seek::{Command, Controller, PlaybackRequest};
        let (tx, rx) = mpsc::channel();
        let (notice, received) = mpsc::channel();
        let mut idle = info(1000);
        idle.starts_at -= chrono::Duration::hours(1);
        idle.ends_at -= chrono::Duration::hours(1);
        let stop = StopToken::default();
        let mut controller = Controller::with_reporter(
            rx,
            stop.clone(),
            |_| Err(PlaybackError::LoginRequired),
            move |message| {
                if message.contains("未ログイン") {
                    notice.send(message).unwrap();
                }
            },
        );
        tx.send(Command::Select(PlaybackRequest {
            station: idle.station.clone(),
            mode: idle.mode,
            cursor: idle.starts_at,
            position: Duration::ZERO,
        }))
        .unwrap();
        let quit = tx.clone();
        let handle = thread::spawn(move || {
            received.recv_timeout(Duration::from_secs(2)).unwrap();
            quit.send(Command::Stop).unwrap();
        });
        let report = play_loop::<Sink>(
            Initial::<Source>::Idle(idle),
            None,
            stop,
            |_| panic!("idle is not active playback"),
            |_, _, _, _| panic!("failed preparation must never open output"),
            Some(&mut controller),
        )
        .unwrap();
        assert_eq!(report.reason, EndReason::Cancelled);
        assert!(report.output.is_none());
        handle.join().unwrap();
    }
    #[test]
    fn idle_session_starts_only_the_prepared_slot_and_stops_at_its_end() {
        use super::super::seek::{Command, Controller, PlaybackRequest};
        let (tx, rx) = mpsc::channel();
        let mut selected = info(100);
        selected.starts_at -= chrono::Duration::hours(1);
        selected.ends_at -= chrono::Duration::hours(1);
        let expected = selected.clone();
        tx.send(Command::Select(PlaybackRequest {
            station: selected.station.clone(),
            mode: selected.mode,
            cursor: selected.starts_at,
            position: Duration::ZERO,
        }))
        .unwrap();
        let stop = StopToken::default();
        let bytes = packet(24_000).repeat(8);
        let mut controller = Controller::with_reporter(
            rx,
            stop.clone(),
            move |_| {
                Ok(Box::new(Source {
                    info: selected.clone(),
                    events: VecDeque::from([event(bytes.clone(), false), Ok(Next::End)]),
                }))
            },
            |_| {},
        );
        let report = play_loop(
            Initial::<Source>::Idle(expected.clone()),
            None,
            stop,
            |snapshot| {
                assert_eq!(snapshot.info.starts_at, expected.starts_at);
            },
            sink,
            Some(&mut controller),
        )
        .unwrap();
        assert_eq!(report.reason, EndReason::ProgramEnd);
        assert_eq!(report.output.unwrap().frames, 2400);
    }
    #[test]
    fn successful_station_switch_cancels_old_output_and_preserves_volume() {
        use super::super::seek::{Command, Controller, PlaybackRequest};
        use std::{
            cell::Cell,
            sync::{
                Arc,
                atomic::{AtomicUsize, Ordering},
            },
        };
        struct Waiting {
            info: StreamInfo,
            bytes: Option<Vec<u8>>,
        }
        impl SegmentSource for Waiting {
            fn info(&self) -> &StreamInfo {
                &self.info
            }
            fn poll_segment(&mut self) -> Result<Next, ProbeError> {
                Ok(match self.bytes.take() {
                    Some(bytes) => Next::Segment(Segment {
                        bytes,
                        codec: None,
                        discontinuity: false,
                        starts_at: Some(self.info.starts_at),
                    }),
                    None => Next::Pending(Duration::from_millis(10)),
                })
            }
        }
        struct Output {
            inner: Sink,
            previous: Option<StopToken>,
            token: StopToken,
            settings: Cell<(bool, u32)>,
            drops: Arc<AtomicUsize>,
            commands: mpsc::Sender<Command>,
        }
        impl Drop for Output {
            fn drop(&mut self) {
                self.drops.fetch_add(1, Ordering::SeqCst);
            }
        }
        impl AudioSink for Output {
            fn configure(&self, paused: bool, volume: u32) {
                self.settings.set((paused, volume));
            }
            fn enqueue(&mut self, p: &DecodedPcm, c: &mut usize) -> Result<bool, OutputError> {
                if let Some(previous) = &self.previous {
                    assert!(previous.is_cancelled());
                    assert!(!self.token.is_cancelled());
                    assert_eq!(self.drops.load(Ordering::SeqCst), 1);
                    assert_eq!(self.settings.get(), (false, 44));
                    self.commands.send(Command::Stop).unwrap();
                }
                self.inner.enqueue(p, c)
            }
            fn accepted(&self) -> u64 {
                self.inner.accepted()
            }
            fn summary(&self) -> OutputSummary {
                self.inner.summary()
            }
            fn poll(&mut self) -> Result<bool, OutputError> {
                self.inner.poll()
            }
            fn finish(&mut self) {
                self.inner.finish();
            }
        }
        let mut original = info(3_600_000);
        original.starts_at -= chrono::Duration::hours(2);
        original.ends_at -= chrono::Duration::hours(2);
        let later = original.starts_at + chrono::Duration::hours(1);
        let pcm = packet(24_000).repeat(50);
        let source = Waiting {
            info: original.clone(),
            bytes: Some(pcm.clone()),
        };
        let (tx, rx) = mpsc::channel();
        let stop = StopToken::default();
        let mut controls = Controller::new(rx, stop.clone(), move |request| {
            assert_eq!(request.station, "OBS");
            assert_eq!(request.cursor, later);
            let mut selected = original.clone();
            selected.station = request.station;
            selected.starts_at = later;
            selected.ends_at = later + chrono::Duration::hours(1);
            Ok(Box::new(Waiting {
                info: selected,
                bytes: Some(pcm.clone()),
            }))
        });
        let mut requested = false;
        let send = tx.clone();
        let drops = Arc::new(AtomicUsize::new(0));
        let mut previous = None;
        let result = play_loop(
            Initial::Source(source),
            None,
            stop,
            |snapshot| {
                if !requested && snapshot.output.is_some() {
                    requested = true;
                    send.send(Command::Volume(44)).unwrap();
                    send.send(Command::Select(PlaybackRequest {
                        station: "OBS".into(),
                        mode: ProbeMode::TimeFree,
                        cursor: later,
                        position: Duration::ZERO,
                    }))
                    .unwrap();
                }
            },
            |pcm, max, deadline, token| {
                let old = previous.replace(token.clone());
                Ok(Output {
                    inner: sink(pcm, max, deadline, token.clone())?,
                    previous: old,
                    token,
                    settings: Cell::new((false, 25)),
                    drops: drops.clone(),
                    commands: tx.clone(),
                })
            },
            Some(&mut controls),
        )
        .unwrap();
        assert_eq!(result.reason, EndReason::Cancelled);
        assert_eq!(drops.load(Ordering::SeqCst), 2);
        assert_eq!(result.output.unwrap().frames, 51_200);
    }
    #[test]
    fn stop_between_loop_check_and_source_end_returns_cancelled_with_final_counters() {
        struct EndingSource {
            inner: Source,
            dropped: mpsc::Sender<()>,
        }
        impl SegmentSource for EndingSource {
            fn info(&self) -> &StreamInfo {
                self.inner.info()
            }
            fn poll_segment(&mut self) -> Result<Next, ProbeError> {
                self.inner.poll_segment()
            }
        }
        impl Drop for EndingSource {
            fn drop(&mut self) {
                let _ = self.dropped.send(());
            }
        }
        struct StoppingSink {
            inner: Sink,
            stop: StopToken,
            ended: std::rc::Rc<Receiver<()>>,
        }
        impl AudioSink for StoppingSink {
            fn configure(&self, _: bool, _: u32) {
                if self.inner.frames > 0 {
                    // The loop already checked stop. Ensure EOF is queued before it receives.
                    self.stop.cancel();
                    self.ended.recv_timeout(Duration::from_secs(2)).unwrap();
                }
            }
            fn enqueue(&mut self, p: &DecodedPcm, c: &mut usize) -> Result<bool, OutputError> {
                self.inner.enqueue(p, c)
            }
            fn accepted(&self) -> u64 {
                self.inner.accepted()
            }
            fn summary(&self) -> OutputSummary {
                self.inner.summary()
            }
            fn poll(&mut self) -> Result<bool, OutputError> {
                self.inner.poll()
            }
            fn finish(&mut self) {
                self.inner.finish();
            }
        }
        let (dropped, ended) = mpsc::channel();
        let ended = std::rc::Rc::new(ended);
        let stop = StopToken::default();
        let cancel = stop.clone();
        let result = play_loop(
            Initial::Source(EndingSource {
                inner: Source {
                    info: info(3_600_000),
                    events: VecDeque::from([
                        event(packet(24_000).repeat(50), false),
                        Ok(Next::End),
                    ]),
                },
                dropped,
            }),
            None,
            stop,
            |_| (),
            |p, n, d, s| {
                Ok(StoppingSink {
                    inner: sink(p, n, d, s)?,
                    stop: cancel.clone(),
                    ended: ended.clone(),
                })
            },
            None,
        )
        .unwrap();
        assert_eq!(result.reason, EndReason::Cancelled);
        assert_eq!(result.output.unwrap().frames, 51200);
    }

    #[test]
    fn running_output_device_failure_drops_output_and_stops_the_source_worker() {
        struct PendingSource {
            info: StreamInfo,
            bytes: Option<Vec<u8>>,
            dropped: mpsc::Sender<()>,
        }
        impl SegmentSource for PendingSource {
            fn info(&self) -> &StreamInfo {
                &self.info
            }
            fn poll_segment(&mut self) -> Result<Next, ProbeError> {
                Ok(match self.bytes.take() {
                    Some(bytes) => Next::Segment(Segment {
                        bytes,
                        codec: None,
                        discontinuity: false,
                        starts_at: Some(self.info.starts_at),
                    }),
                    None => Next::Pending(Duration::from_millis(10)),
                })
            }
        }
        impl Drop for PendingSource {
            fn drop(&mut self) {
                let _ = self.dropped.send(());
            }
        }
        struct FailedSink {
            inner: Sink,
            dropped: mpsc::Sender<()>,
        }
        impl Drop for FailedSink {
            fn drop(&mut self) {
                let _ = self.dropped.send(());
            }
        }
        impl AudioSink for FailedSink {
            fn enqueue(
                &mut self,
                pcm: &DecodedPcm,
                cursor: &mut usize,
            ) -> Result<bool, OutputError> {
                self.inner.enqueue(pcm, cursor)
            }
            fn accepted(&self) -> u64 {
                self.inner.accepted()
            }
            fn summary(&self) -> OutputSummary {
                self.inner.summary()
            }
            fn poll(&mut self) -> Result<bool, OutputError> {
                Err(OutputError::Device)
            }
            fn finish(&mut self) {
                self.inner.finish();
            }
        }
        let (source_drop, source_dropped) = mpsc::channel();
        let (sink_drop, sink_dropped) = mpsc::channel();
        let result = play_loop(
            Initial::Source(PendingSource {
                info: info(3_600_000),
                bytes: Some(packet(24_000).repeat(50)),
                dropped: source_drop,
            }),
            None,
            StopToken::default(),
            |_| (),
            |pcm, maximum, remaining, stop| {
                Ok(FailedSink {
                    inner: sink(pcm, maximum, remaining, stop)?,
                    dropped: sink_drop.clone(),
                })
            },
            None,
        );
        assert!(matches!(
            result,
            Err(PlaybackError::Output(OutputError::Device))
        ));
        sink_dropped.recv_timeout(Duration::from_secs(2)).unwrap();
        source_dropped.recv_timeout(Duration::from_secs(2)).unwrap();
    }

    #[test]
    fn replacement_device_failure_keeps_old_output_alive_until_explicit_stop() {
        use super::super::seek::{Command, Controller, PlaybackRequest};
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        struct Waiting {
            info: StreamInfo,
            bytes: Option<Vec<u8>>,
        }
        impl SegmentSource for Waiting {
            fn info(&self) -> &StreamInfo {
                &self.info
            }
            fn poll_segment(&mut self) -> Result<Next, ProbeError> {
                Ok(match self.bytes.take() {
                    Some(bytes) => Next::Segment(Segment {
                        bytes,
                        codec: None,
                        discontinuity: false,
                        starts_at: Some(
                            self.info.starts_at
                                + chrono::Duration::from_std(self.info.position).unwrap(),
                        ),
                    }),
                    None => Next::Pending(Duration::from_millis(10)),
                })
            }
        }
        struct Tracked {
            inner: Sink,
            drops: Arc<AtomicUsize>,
        }
        impl Drop for Tracked {
            fn drop(&mut self) {
                self.drops.fetch_add(1, Ordering::SeqCst);
            }
        }
        impl AudioSink for Tracked {
            fn enqueue(&mut self, p: &DecodedPcm, c: &mut usize) -> Result<bool, OutputError> {
                self.inner.enqueue(p, c)
            }
            fn accepted(&self) -> u64 {
                self.inner.accepted()
            }
            fn summary(&self) -> OutputSummary {
                self.inner.summary()
            }
            fn poll(&mut self) -> Result<bool, OutputError> {
                self.inner.poll()
            }
            fn finish(&mut self) {
                self.inner.finish();
            }
        }
        let data = packet(24_000).repeat(50);
        let original = info(3_600_000);
        let source = Waiting {
            info: original.clone(),
            bytes: Some(data.clone()),
        };
        let (tx, rx) = mpsc::channel();
        let stop = StopToken::default();
        let selected = PlaybackRequest {
            station: "OBS".into(),
            mode: ProbeMode::TimeFree,
            cursor: original.starts_at,
            position: Duration::from_secs(60),
        };
        let mut control = Controller::new(rx, stop.clone(), move |request| {
            let position = request.position;
            let mut info = original.clone();
            info.position = position;
            info.station = request.station;
            Ok(Box::new(Waiting {
                info,
                bytes: Some(data.clone()),
            }) as Box<dyn SegmentSource>)
        });
        let drops = Arc::new(AtomicUsize::new(0));
        let observed = drops.clone();
        let request = tx.clone();
        let mut requested = false;
        let mut attempts = 0;
        let report = play_loop(
            Initial::Source(source),
            None,
            stop,
            |snapshot| {
                if !requested && snapshot.output.is_some() {
                    requested = true;
                    request.send(Command::Select(selected.clone())).unwrap();
                }
            },
            |p, n, d, s| {
                attempts += 1;
                if attempts == 2 {
                    assert_eq!(observed.load(Ordering::SeqCst), 0);
                    tx.send(Command::Stop).unwrap();
                    return Err(OutputError::Device);
                }
                Ok(Tracked {
                    inner: sink(p, n, d, s)?,
                    drops: observed.clone(),
                })
            },
            Some(&mut control),
        )
        .unwrap();
        assert_eq!(attempts, 2);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert_eq!(report.reason, EndReason::Cancelled);
        assert_eq!(report.output.unwrap().frames, 51_200);
    }
    #[test]
    fn seek_trim_uses_ceil_frames_preserves_channels_and_rejects_unknown_or_late_origin() {
        let mut pcm = AdtsDecoder::new()
            .decode_segment_pcm(&packet(24_000), None)
            .unwrap();
        let original = pcm.samples.clone();
        let start = Utc::now().fixed_offset();
        let target = start + chrono::Duration::microseconds(100);
        assert!(trim_to(&mut pcm, Some(start), Some(target)).unwrap());
        assert_eq!(pcm.samples, original[3..]);
        assert_eq!(pcm.summary.frames, original.len() - 3);
        assert!(trim_to(&mut pcm, None, Some(target)).is_err());
        assert!(
            trim_to(
                &mut pcm,
                Some(target + chrono::Duration::seconds(1)),
                Some(target)
            )
            .is_err()
        );
        assert!(
            !trim_to(
                &mut pcm,
                Some(start),
                Some(start + chrono::Duration::seconds(10))
            )
            .unwrap()
        );
        let mut slot = info(1000);
        slot.position = Duration::from_millis(900);
        assert_eq!(frame_limit(&slot, 24_000, None).unwrap().0, 2400);
        slot.position = Duration::from_secs(1);
        assert!(frame_limit(&slot, 24_000, None).is_err());
    }
    #[test]
    fn successive_segments_share_output_and_clip_exactly_at_slot_end() {
        let bytes = packet(24_000);
        let source = Source {
            info: info(50),
            events: (0..5).map(|_| event(bytes.clone(), false)).collect(),
        };
        let stop = StopToken::default();
        let mut opens = 0;
        let report = play_with(
            source,
            None,
            stop.clone(),
            |_| {},
            |p, n, d, s| {
                opens += 1;
                sink(p, n, d, s)
            },
        )
        .unwrap();
        assert_eq!(opens, 1);
        assert_eq!(report.segments, 2);
        assert_eq!(report.output.unwrap().frames, 1200);
        assert_eq!(report.reason, EndReason::ProgramEnd);
        assert!(stop.is_cancelled());
    }
    #[test]
    fn explicit_duration_limits_media_frames_without_advancing_to_another_program() {
        let bytes = packet(24_000);
        let source = Source {
            info: info(2000),
            events: (0..40).map(|_| event(bytes.clone(), false)).collect(),
        };
        let report = play_with(source, Some(1), StopToken::default(), |_| {}, sink).unwrap();
        assert_eq!(report.output.unwrap().frames, 24_000);
        assert_eq!(report.reason, EndReason::Limit);
        assert!(report.segments > 1);
    }
    #[test]
    fn premature_end_network_and_decode_errors_stop_and_cancel_worker() {
        let bytes = packet(24_000);
        for tail in [
            Ok(Next::End),
            Err(ProbeError::Authentication),
            event(b"bad ADTS".to_vec(), false),
        ] {
            let source = Source {
                info: info(3_600_000),
                events: vec![event(bytes.clone(), false), tail].into(),
            };
            let stop = StopToken::default();
            assert!(play_with(source, None, stop.clone(), |_| {}, sink).is_err());
            assert!(stop.is_cancelled());
        }
    }
    #[test]
    fn decoder_format_changes_are_rejected_until_discontinuity_and_output_still_rejects_new_rate() {
        let source = Source {
            info: info(3_600_000),
            events: vec![event(packet(24_000), false), event(packet(48_000), false)].into(),
        };
        assert!(matches!(
            play_with(source, None, StopToken::default(), |_| {}, sink),
            Err(PlaybackError::Decode(AacError::FormatChanged))
        ));
        let source = Source {
            info: info(3_600_000),
            events: vec![event(packet(24_000), false), event(packet(48_000), true)].into(),
        };
        assert!(matches!(
            play_with(source, None, StopToken::default(), |_| {}, sink),
            Err(PlaybackError::Output(OutputError::UnsupportedFormat))
        ));
    }
    #[test]
    fn cancellation_works_before_audio_is_ready_and_stops_pending_waits() {
        let stop = StopToken::default();
        let signal = stop.clone();
        let source = Source {
            info: info(3_600_000),
            events: vec![Ok(Next::Pending(Duration::from_secs(1)))].into(),
        };
        let handle = thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            signal.cancel();
        });
        let began = Instant::now();
        let report = play_with(source, None, stop, |_| {}, sink).unwrap();
        assert_eq!(report.reason, EndReason::Cancelled);
        assert!(report.output.is_none());
        assert!(began.elapsed() < Duration::from_secs(1));
        handle.join().unwrap();
    }
}
