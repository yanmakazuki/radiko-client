//! 主スレッドで出力を制御し、通信・デコードは容量制限付きの単一ワーカーで実行する。
use super::{
    aac::{AacError, AdtsDecoder, DecodedPcm},
    diagnostics::{Stage, Tracker},
    fragmented::FragmentedAacDecoder,
    output::{ContinuousOutput, OutputError, OutputSummary, StopToken},
};
use crate::api::stream::{
    ProbeError, ProbeMode,
    playback::{Next, Segment, SegmentSource, StreamInfo},
};
use chrono::Utc;
use std::{
    fmt,
    sync::{
        Arc,
        mpsc::{self, Receiver, TryRecvError, TrySendError},
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Debug)]
pub enum PlaybackError {
    Source(ProbeError),
    Storage(crate::storage::StorageError),
    LoginRequired,
    Auth(crate::api::auth::AuthError),
    Decode(AacError),
    Output(OutputError),
    EarlyEnd,
    SourceStalled,
    ReconnectGaveUp,
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
            Self::ReconnectGaveUp => f.write_str(crate::reconnect::GAVE_UP),
            Self::Worker => f.write_str("再生ワーカーが予期せず終了しました"),
        }
    }
}
impl std::error::Error for PlaybackError {}

/// Device failures the TUI can explain. Chosen from [`OutputError`], not from display text.
/// PCM, probe, deadline, and reconnect-limit errors are absent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputFailure {
    NoDevice,
    UnsupportedFormat,
    Device,
    Stalled,
}
/// `Some` for a device failure the listener can recover by checking the output and playing again.
pub fn output_failure(error: &PlaybackError) -> Option<OutputFailure> {
    match error {
        PlaybackError::Output(output) => match output {
            OutputError::NoDevice => Some(OutputFailure::NoDevice),
            OutputError::UnsupportedFormat | OutputError::IncompatibleOutput { .. } => {
                Some(OutputFailure::UnsupportedFormat)
            }
            OutputError::Device | OutputError::DeviceInfo => Some(OutputFailure::Device),
            OutputError::Stalled => Some(OutputFailure::Stalled),
            OutputError::InvalidPcm
            | OutputError::ReconnectGaveUp
            | OutputError::ProbeTimeout
            | OutputError::Deadline => None,
        },
        PlaybackError::Source(_)
        | PlaybackError::Storage(_)
        | PlaybackError::LoginRequired
        | PlaybackError::Auth(_)
        | PlaybackError::Decode(_)
        | PlaybackError::EarlyEnd
        | PlaybackError::SourceStalled
        | PlaybackError::ReconnectGaveUp
        | PlaybackError::Worker => None,
    }
}
/// Playback failures whose TUI sentence differs from [`failure_notice`] and from `Display`.
/// Chosen from the error value. Absent means the TUI shows the supplied notice unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeKind {
    LoginRequired,
    SessionExpired,
    StreamAuthentication,
    AuthExpired,
    /// Preparation or switching failed, and the current playback is still running.
    AuthExpiredWhilePlaying,
    /// Preparation or switching failed, and the current playback is still running.
    ReconnectLimitWhilePlaying,
    OutsideArea,
}
/// `Some` when the TUI must replace the CLI sentence for this error.
pub fn notice_kind(error: &PlaybackError) -> Option<NoticeKind> {
    match error {
        PlaybackError::LoginRequired => Some(NoticeKind::LoginRequired),
        PlaybackError::Auth(crate::api::auth::AuthError::SessionExpired) => {
            Some(NoticeKind::SessionExpired)
        }
        PlaybackError::Source(error) => probe_notice_kind(error),
        _ => None,
    }
}
/// Kind for a failure that leaves the current playback running.
/// Auth expiry and the reconnect limit do not use their stop sentences here.
pub fn continuing_notice_kind(error: &PlaybackError) -> Option<NoticeKind> {
    if auth_expired_error(error) {
        return Some(NoticeKind::AuthExpiredWhilePlaying);
    }
    if reconnect_limit_error(error) {
        return Some(NoticeKind::ReconnectLimitWhilePlaying);
    }
    notice_kind(error)
}
fn auth_expired_error(error: &PlaybackError) -> bool {
    match error {
        PlaybackError::Source(error) => probe_auth_expired(error),
        PlaybackError::Storage(_)
        | PlaybackError::LoginRequired
        | PlaybackError::Auth(_)
        | PlaybackError::Decode(_)
        | PlaybackError::Output(_)
        | PlaybackError::EarlyEnd
        | PlaybackError::SourceStalled
        | PlaybackError::ReconnectGaveUp
        | PlaybackError::Worker => false,
    }
}
fn probe_auth_expired(error: &ProbeError) -> bool {
    match error {
        ProbeError::At(_, inner) => probe_auth_expired(inner),
        ProbeError::AuthExpired => true,
        ProbeError::InvalidInput
        | ProbeError::InvalidPosition
        | ProbeError::Network
        | ProbeError::Cancelled
        | ProbeError::Http(_)
        | ProbeError::Authentication
        | ProbeError::Denied
        | ProbeError::NotFound
        | ProbeError::RateLimited
        | ProbeError::TooLarge
        | ProbeError::UnsafeUrl
        | ProbeError::InvalidAuth
        | ProbeError::OutsideJapan
        | ProbeError::InvalidXml
        | ProbeError::InvalidProgram
        | ProbeError::Upcoming
        | ProbeError::StillOnAir
        | ProbeError::Expired
        | ProbeError::ContractRequired
        | ProbeError::OutsideArea
        | ProbeError::AvailabilityUnknown
        | ProbeError::Unavailable
        | ProbeError::InvalidPlaylist
        | ProbeError::NoEndpoint
        | ProbeError::WindowLost
        | ProbeError::ReconnectGaveUp => false,
    }
}
fn reconnect_limit_error(error: &PlaybackError) -> bool {
    match error {
        PlaybackError::ReconnectGaveUp => true,
        PlaybackError::Output(OutputError::ReconnectGaveUp) => true,
        PlaybackError::Source(error) => probe_reconnect_limit(error),
        PlaybackError::Storage(_)
        | PlaybackError::LoginRequired
        | PlaybackError::Auth(_)
        | PlaybackError::Decode(_)
        | PlaybackError::Output(_)
        | PlaybackError::EarlyEnd
        | PlaybackError::SourceStalled
        | PlaybackError::Worker => false,
    }
}
fn probe_reconnect_limit(error: &ProbeError) -> bool {
    match error {
        ProbeError::At(_, inner) => probe_reconnect_limit(inner),
        ProbeError::ReconnectGaveUp => true,
        ProbeError::InvalidInput
        | ProbeError::InvalidPosition
        | ProbeError::Network
        | ProbeError::Cancelled
        | ProbeError::Http(_)
        | ProbeError::Authentication
        | ProbeError::Denied
        | ProbeError::NotFound
        | ProbeError::RateLimited
        | ProbeError::TooLarge
        | ProbeError::UnsafeUrl
        | ProbeError::InvalidAuth
        | ProbeError::OutsideJapan
        | ProbeError::InvalidXml
        | ProbeError::InvalidProgram
        | ProbeError::Upcoming
        | ProbeError::StillOnAir
        | ProbeError::Expired
        | ProbeError::ContractRequired
        | ProbeError::OutsideArea
        | ProbeError::AvailabilityUnknown
        | ProbeError::Unavailable
        | ProbeError::InvalidPlaylist
        | ProbeError::NoEndpoint
        | ProbeError::WindowLost
        | ProbeError::AuthExpired => false,
    }
}
fn probe_notice_kind(error: &ProbeError) -> Option<NoticeKind> {
    match error {
        ProbeError::At(_, inner) => probe_notice_kind(inner),
        ProbeError::Authentication => Some(NoticeKind::StreamAuthentication),
        ProbeError::AuthExpired => Some(NoticeKind::AuthExpired),
        ProbeError::OutsideArea => Some(NoticeKind::OutsideArea),
        _ => None,
    }
}

/// TUI notice for a playback failure.
/// Window loss, the reconnect limit, and auth expiry are the confirmed sentence alone.
/// Every other failure keeps the `再生停止:` prefix and the Display chain.
pub fn failure_notice(error: &PlaybackError) -> String {
    match confirmed_message(error) {
        Some(message) => message.to_owned(),
        None => format!("再生停止: {error}"),
    }
}

/// Text after CLI's `エラー:` label.
/// Confirmed sentences are shown alone. When Display still has a stage or `配信取得:`,
/// that chain is the following line so the kind is not dropped.
pub fn cli_playback_body(error: &PlaybackError) -> String {
    retain_chain(confirmed_message(error), error.to_string())
}

/// Same rules for a playback `PlaybackError` or a bare `ProbeError`. Other errors are unchanged.
pub fn cli_error_body(error: &(dyn std::error::Error + 'static)) -> String {
    if let Some(error) = error.downcast_ref::<PlaybackError>() {
        return cli_playback_body(error);
    }
    if let Some(error) = error.downcast_ref::<ProbeError>() {
        return retain_chain(confirmed_probe(error), error.to_string());
    }
    error.to_string()
}

fn retain_chain(message: Option<&str>, chain: String) -> String {
    match message {
        Some(message) if chain != message => format!("{message}\n{chain}"),
        Some(message) => message.to_owned(),
        None => chain,
    }
}

fn confirmed_message(error: &PlaybackError) -> Option<&'static str> {
    match error {
        PlaybackError::ReconnectGaveUp => Some(crate::reconnect::GAVE_UP),
        PlaybackError::Output(OutputError::ReconnectGaveUp) => Some(crate::reconnect::GAVE_UP),
        PlaybackError::Source(error) => confirmed_probe(error),
        PlaybackError::Storage(_)
        | PlaybackError::LoginRequired
        | PlaybackError::Auth(_)
        | PlaybackError::Decode(_)
        | PlaybackError::Output(_)
        | PlaybackError::EarlyEnd
        | PlaybackError::SourceStalled
        | PlaybackError::Worker => None,
    }
}

fn confirmed_probe(error: &ProbeError) -> Option<&'static str> {
    match error {
        ProbeError::At(_, inner) => confirmed_probe(inner),
        ProbeError::WindowLost => Some(ProbeError::WINDOW_LOST),
        ProbeError::AuthExpired => Some(ProbeError::AUTH_EXPIRED),
        ProbeError::ReconnectGaveUp => Some(crate::reconnect::GAVE_UP),
        ProbeError::InvalidInput
        | ProbeError::InvalidPosition
        | ProbeError::Network
        | ProbeError::Cancelled
        | ProbeError::Http(_)
        | ProbeError::Authentication
        | ProbeError::Denied
        | ProbeError::NotFound
        | ProbeError::RateLimited
        | ProbeError::TooLarge
        | ProbeError::UnsafeUrl
        | ProbeError::InvalidAuth
        | ProbeError::OutsideJapan
        | ProbeError::InvalidXml
        | ProbeError::InvalidProgram
        | ProbeError::Upcoming
        | ProbeError::StillOnAir
        | ProbeError::Expired
        | ProbeError::ContractRequired
        | ProbeError::OutsideArea
        | ProbeError::AvailabilityUnknown
        | ProbeError::Unavailable
        | ProbeError::InvalidPlaylist
        | ProbeError::NoEndpoint => None,
    }
}

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
    Pcm(QueuedPcm),
    End,
    Failed(PlaybackError),
}
pub(super) struct QueuedPcm {
    pcm: DecodedPcm,
    charge: QueueCharge,
}
struct QueueCharge {
    measurements: std::sync::Arc<std::sync::Mutex<Tracker>>,
    media: Duration,
    active: bool,
}
impl QueueCharge {
    fn received(&mut self, tracker: Option<&mut Tracker>) {
        if self.active {
            if let Some(tracker) = tracker {
                tracker.received(self.media);
            }
            self.active = false;
        }
    }
}
impl Drop for QueueCharge {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        if let Ok(mut tracker) = self.measurements.lock() {
            tracker.received(self.media);
        }
    }
}
impl QueuedPcm {
    fn new(pcm: DecodedPcm, measurements: std::sync::Arc<std::sync::Mutex<Tracker>>) -> Self {
        let media = pcm.summary.media_duration;
        if let Ok(mut tracker) = measurements.lock() {
            tracker.queued(media);
        }
        Self {
            pcm,
            charge: QueueCharge {
                measurements,
                media,
                active: true,
            },
        }
    }
    fn into_pcm(self) -> DecodedPcm {
        let Self { pcm, charge } = self;
        drop(charge);
        pcm
    }
}

/// Receive and release accounting under one lock, before the producer can charge another PCM.
pub(super) struct PcmReceiver {
    receiver: Receiver<Event>,
    measurements: Option<std::sync::Arc<std::sync::Mutex<Tracker>>>,
}
impl PcmReceiver {
    fn disconnected() -> Self {
        let (_sender, receiver) = mpsc::channel();
        Self {
            receiver,
            measurements: None,
        }
    }
    pub(super) fn try_recv(&self) -> Result<Event, TryRecvError> {
        let mut tracker = self
            .measurements
            .as_ref()
            .and_then(|value| value.lock().ok());
        let mut event = self.receiver.try_recv()?;
        if let Event::Pcm(pcm) = &mut event {
            pcm.charge.received(tracker.as_deref_mut());
        }
        Ok(event)
    }
    pub(super) fn recv_timeout(&self, timeout: Duration) -> Result<Event, mpsc::RecvTimeoutError> {
        let began = Instant::now();
        loop {
            match self.try_recv() {
                Ok(event) => return Ok(event),
                Err(TryRecvError::Disconnected) => {
                    return Err(mpsc::RecvTimeoutError::Disconnected);
                }
                Err(TryRecvError::Empty) => (),
            }
            let remaining = timeout.saturating_sub(began.elapsed());
            if remaining.is_zero() {
                return Err(mpsc::RecvTimeoutError::Timeout);
            }
            thread::sleep(remaining.min(Duration::from_millis(1)));
        }
    }
    #[cfg(test)]
    fn try_iter(&self) -> impl Iterator<Item = Event> + '_ {
        std::iter::from_fn(move || self.try_recv().ok())
    }
}

pub(super) struct Worker {
    pub(super) stop: StopToken,
    diagnostics: std::sync::Arc<std::sync::Mutex<Tracker>>,
    http_diagnostics: Option<std::sync::Arc<crate::api::stream::playback::RequestMeasurements>>,
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

enum SegmentDecoder {
    Adts(Box<AdtsDecoder>),
    Fragmented {
        initialization: std::sync::Arc<[u8]>,
        decoder: Box<FragmentedAacDecoder>,
        stamp: Option<(u64, chrono::DateTime<chrono::FixedOffset>)>,
    },
}

fn decode_segment_pcm(
    state: &mut Option<SegmentDecoder>,
    segment: &Segment,
    stop: &StopToken,
) -> Result<DecodedPcm, AacError> {
    if stop.is_cancelled() {
        return Err(AacError::Cancelled);
    }
    if segment.discontinuity {
        *state = None;
    }
    if state.is_none() {
        *state = Some(match &segment.initialization {
            Some(init) => {
                if init.len() > 1024 * 1024 {
                    return Err(AacError::TooLarge);
                }
                SegmentDecoder::Fragmented {
                    decoder: Box::new(FragmentedAacDecoder::new(init.to_vec())?),
                    initialization: init.clone(),
                    stamp: None,
                }
            }
            None => SegmentDecoder::Adts(Box::default()),
        });
    }
    match (
        state.as_mut().ok_or(AacError::DecodeFailed)?,
        &segment.initialization,
    ) {
        (SegmentDecoder::Adts(decoder), None) => {
            decoder.decode_segment_pcm_controlled(&segment.bytes, segment.codec.as_deref(), || {
                stop.is_cancelled()
            })
        }
        (
            SegmentDecoder::Fragmented {
                initialization,
                decoder,
                stamp,
            },
            Some(init),
        ) => {
            if !std::sync::Arc::ptr_eq(initialization, init)
                && initialization.as_ref() != init.as_ref()
            {
                return Err(AacError::FormatChanged);
            }
            *initialization = init.clone();
            let result =
                decoder.decode_pcm_controlled(&segment.bytes, segment.codec.as_deref(), || {
                    stop.is_cancelled()
                })?;
            let expected = segment.duration.ok_or(AacError::InvalidContainer)?;
            if result.pcm.summary.media_duration.abs_diff(expected) > Duration::from_millis(2) {
                return Err(AacError::InvalidContainer);
            }
            if let Some(current) = segment.starts_at {
                if let Some((time, previous)) = *stamp {
                    let ticks = result
                        .decode_time
                        .checked_sub(time)
                        .ok_or(AacError::InvalidContainer)?;
                    let media =
                        Duration::try_from_secs_f64(ticks as f64 / f64::from(result.timescale))
                            .map_err(|_| AacError::InvalidContainer)?;
                    let wall = current
                        .signed_duration_since(previous)
                        .to_std()
                        .map_err(|_| AacError::InvalidContainer)?;
                    if media.abs_diff(wall) > Duration::from_millis(2) {
                        return Err(AacError::InvalidContainer);
                    }
                }
                *stamp = Some((result.decode_time, current));
            }
            Ok(result.pcm)
        }
        _ => Err(AacError::FormatChanged),
    }
}

pub(crate) trait MonoClock: Send + Sync {
    fn now(&self) -> Instant;
    fn sleep(&self, duration: Duration);
    /// Block until `duration` has passed on this clock, or `stop` is cancelled.
    fn park(&self, duration: Duration, stop: &StopToken);
}

struct WallClock;

impl MonoClock for WallClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
    fn sleep(&self, duration: Duration) {
        thread::sleep(duration);
    }
    fn park(&self, duration: Duration, stop: &StopToken) {
        let until = Instant::now() + duration;
        while !stop.is_cancelled() && Instant::now() < until {
            thread::sleep(Duration::from_millis(10));
        }
    }
}

/// Virtual time for playback tests. `sleep` advances it. `park` waits for that advance.
#[cfg(test)]
pub(crate) struct ManualClock {
    origin: Instant,
    millis: std::sync::Mutex<u64>,
    cv: std::sync::Condvar,
}

#[cfg(test)]
impl ManualClock {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            origin: Instant::now(),
            millis: std::sync::Mutex::new(0),
            cv: std::sync::Condvar::new(),
        })
    }
    pub(crate) fn elapsed(&self) -> Duration {
        Duration::from_millis(*self.millis.lock().expect("clock"))
    }
}

#[cfg(test)]
impl MonoClock for ManualClock {
    fn now(&self) -> Instant {
        self.origin + self.elapsed()
    }
    fn sleep(&self, duration: Duration) {
        let mut millis = self.millis.lock().expect("clock");
        *millis = millis.saturating_add(u64::try_from(duration.as_millis()).unwrap_or(u64::MAX));
        self.cv.notify_all();
        drop(millis);
        // A short real pause lets the worker run while virtual time moves in 5ms steps.
        thread::sleep(Duration::from_micros(20));
    }
    fn park(&self, duration: Duration, stop: &StopToken) {
        let deadline = self.now() + duration;
        let mut millis = self.millis.lock().expect("clock");
        while self.origin + Duration::from_millis(*millis) < deadline {
            if stop.is_cancelled() {
                return;
            }
            let (guard, _) = self
                .cv
                .wait_timeout(millis, Duration::from_millis(50))
                .expect("clock");
            millis = guard;
        }
    }
}

pub(super) fn worker(source: impl SegmentSource, stop: StopToken) -> (Worker, PcmReceiver) {
    worker_timed(source, stop, Arc::new(WallClock))
}
fn worker_timed(
    mut source: impl SegmentSource,
    stop: StopToken,
    clock: Arc<dyn MonoClock>,
) -> (Worker, PcmReceiver) {
    let (tx, rx) = mpsc::sync_channel(PCM_QUEUE_CAPACITY);
    let http_diagnostics = source.enable_http_diagnostics();
    let control = stop.clone();
    let cancelled = control.clone();
    source.set_cancellation(std::sync::Arc::new(move || cancelled.is_cancelled()));
    let diagnostics = std::sync::Arc::new(std::sync::Mutex::new(Tracker::new(clock.now())));
    let measurements = diagnostics.clone();
    let pace = clock.clone();
    let handle = thread::spawn(move || {
        let enter = |stage| {
            if let Ok(mut tracker) = measurements.lock() {
                tracker.enter(stage, Instant::now());
            }
        };
        let mut decoder = None;
        let info = source.info().clone();
        let target = chrono::Duration::from_std(info.position)
            .ok()
            .and_then(|offset| info.starts_at.checked_add_signed(offset));
        let mut seeking = !info.position.is_zero();
        let mut last_segment = pace.now();
        loop {
            if control.is_cancelled() {
                break;
            }
            enter(Stage::Acquisition);
            let next = match source.poll_segment() {
                Ok(next) => next,
                Err(error) => {
                    enter(Stage::Finished);
                    let failed = if reconnect_gave_up(&error) {
                        PlaybackError::ReconnectGaveUp
                    } else {
                        PlaybackError::Source(error)
                    };
                    send(&tx, Event::Failed(failed), &control);
                    break;
                }
            };
            match next {
                Next::Pending(delay) => {
                    enter(Stage::Waiting);
                    if pace.now().saturating_duration_since(last_segment) > crate::reconnect::LIMIT
                    {
                        send(&tx, Event::Failed(PlaybackError::ReconnectGaveUp), &control);
                        break;
                    }
                    pace.park(delay.min(Duration::from_secs(1)), &control);
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
                    enter(Stage::Decode);
                    let decoded = decode_segment_pcm(&mut decoder, &segment, &control);
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
                            let pcm = QueuedPcm::new(pcm, measurements.clone());
                            if !send(&tx, Event::Pcm(pcm), &control) {
                                break;
                            }
                            last_segment = pace.now();
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
    let rx = PcmReceiver {
        receiver: rx,
        measurements: Some(diagnostics.clone()),
    };
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
    pub rx: PcmReceiver,
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
                let pcm = pcm.into_pcm();
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

pub(crate) trait AudioSink {
    fn configure(&self, _paused: bool, _volume: u32) {}
    fn enqueue(&mut self, pcm: &DecodedPcm, cursor: &mut usize) -> Result<bool, OutputError>;
    fn accepted(&self) -> u64;
    fn summary(&self) -> OutputSummary;
    fn poll(&mut self) -> Result<bool, OutputError>;
    fn finish(&mut self);
    fn reconnecting_for(&self) -> Option<Duration> {
        None
    }
    /// Frames still queued for the device. Zero means the local buffer is empty.
    fn queued_frames(&self) -> u64 {
        0
    }
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
    fn reconnecting_for(&self) -> Option<Duration> {
        ContinuousOutput::reconnecting_for(self)
    }
    fn queued_frames(&self) -> u64 {
        ContinuousOutput::queued_frames(self)
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
/// Test sessions use a stand-in device. Production playback stays on `play_session`.
pub(crate) struct BoxedSink(pub Box<dyn AudioSink + Send>);
impl AudioSink for BoxedSink {
    fn configure(&self, paused: bool, volume: u32) {
        self.0.configure(paused, volume);
    }
    fn enqueue(&mut self, pcm: &DecodedPcm, cursor: &mut usize) -> Result<bool, OutputError> {
        self.0.enqueue(pcm, cursor)
    }
    fn accepted(&self) -> u64 {
        self.0.accepted()
    }
    fn summary(&self) -> OutputSummary {
        self.0.summary()
    }
    fn poll(&mut self) -> Result<bool, OutputError> {
        self.0.poll()
    }
    fn finish(&mut self) {
        self.0.finish();
    }
    fn reconnecting_for(&self) -> Option<Duration> {
        self.0.reconnecting_for()
    }
    fn queued_frames(&self) -> u64 {
        self.0.queued_frames()
    }
}
pub(crate) fn play_idle_with<S: AudioSink>(
    idle: StreamInfo,
    stop: StopToken,
    controls: &mut super::seek::Controller,
    progress: impl FnMut(Progress),
    create: impl FnMut(&DecodedPcm, u64, Option<Duration>, StopToken) -> Result<S, OutputError>,
) -> Result<PlaybackReport, PlaybackError> {
    play_loop(
        Initial::<Box<dyn SegmentSource>>::Idle(idle),
        None,
        stop,
        progress,
        create,
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
/// 「受信待ち…」。再接続の秒数表示に切り替える前の、バッファが空いたあとの状態。
pub const RECEPTION_NOTICE: &str = "受信待ち…";

pub use crate::reconnect::{NOTICE_AFTER, RECEPTION_AFTER};

/// 「再接続中…（N秒）」。N は新しい PCM が来ない時間で、`NOTICE_AFTER` 以降だけである。
pub fn reconnect_notice(elapsed: Duration) -> String {
    format!("再接続中…（{}秒）", elapsed.as_secs())
}

/// Inputs to [`audible_wait`]. Durations are measured by the caller; this function does not sleep.
#[derive(Clone, Copy, Debug)]
pub struct AudibleGap {
    /// At least one output frame has been played.
    pub heard: bool,
    pub paused: bool,
    /// Program end, including the time-free tail drain.
    pub ended: bool,
    /// The worker is still running and no PCM chunk is waiting to be enqueued.
    pub waiting: bool,
    /// How long the device queue has been empty while `waiting` and `heard`.
    pub empty_for: Duration,
    /// How long since the play loop last received a PCM chunk. Pause does not count.
    pub since_pcm: Duration,
    /// Output-device rebuild. Independent of `NOTICE_AFTER`.
    pub output_reconnecting: Option<Duration>,
}

/// What the status line should say about a quiet buffer. `GaveUp` stops playback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudiblePhase {
    Quiet,
    Receiving,
    Reconnecting(Duration),
    GaveUp,
}

impl AudiblePhase {
    /// TUI text for this phase. `None` leaves the current notice to other state.
    pub fn notice(self) -> Option<String> {
        match self {
            Self::Receiving => Some(RECEPTION_NOTICE.to_owned()),
            Self::Reconnecting(elapsed) => Some(reconnect_notice(elapsed)),
            Self::Quiet | Self::GaveUp => None,
        }
    }
}

/// Buffer-empty 「受信待ち…」, then 「再接続中…（N秒）」 at [`NOTICE_AFTER`].
///
/// `NOTICE_AFTER`, playlist backoff, and [`crate::reconnect::LIMIT`] are unchanged.
/// The reception notice is withheld while paused, before the first played frame,
/// and at program end. Output-device rebuild still reports reconnecting immediately.
pub fn audible_wait(gap: AudibleGap) -> AudiblePhase {
    if gap.waiting && gap.since_pcm > crate::reconnect::LIMIT {
        return AudiblePhase::GaveUp;
    }
    if gap.paused || gap.ended || !gap.heard {
        return AudiblePhase::Quiet;
    }
    let input = (gap.waiting && gap.since_pcm >= NOTICE_AFTER).then_some(gap.since_pcm);
    match (input, gap.output_reconnecting) {
        (Some(left), Some(right)) => return AudiblePhase::Reconnecting(left.max(right)),
        (Some(elapsed), None) | (None, Some(elapsed)) => {
            return AudiblePhase::Reconnecting(elapsed);
        }
        (None, None) => {}
    }
    if gap.waiting && gap.empty_for >= RECEPTION_AFTER {
        return AudiblePhase::Receiving;
    }
    AudiblePhase::Quiet
}
fn reconnect_gave_up(error: &ProbeError) -> bool {
    match error {
        ProbeError::ReconnectGaveUp => true,
        ProbeError::At(_, inner) => reconnect_gave_up(inner),
        _ => false,
    }
}
fn play_loop<S: AudioSink>(
    initial: Initial<impl SegmentSource>,
    seconds: Option<u32>,
    stop: StopToken,
    progress: impl FnMut(Progress),
    create: impl FnMut(&DecodedPcm, u64, Option<Duration>, StopToken) -> Result<S, OutputError>,
    controls: Option<&mut super::seek::Controller>,
) -> Result<PlaybackReport, PlaybackError> {
    play_loop_clocked(
        initial,
        seconds,
        stop,
        progress,
        create,
        controls,
        Arc::new(WallClock),
    )
}
#[cfg(test)]
pub(crate) fn play_with_clock<S: AudioSink>(
    source: impl SegmentSource,
    stop: StopToken,
    clock: Arc<dyn MonoClock>,
    progress: impl FnMut(Progress),
    create: impl FnMut(&DecodedPcm, u64, Option<Duration>, StopToken) -> Result<S, OutputError>,
) -> Result<PlaybackReport, PlaybackError> {
    play_loop_clocked(
        Initial::Source(source),
        None,
        stop,
        progress,
        create,
        None,
        clock,
    )
}
fn play_loop_clocked<S: AudioSink>(
    initial: Initial<impl SegmentSource>,
    seconds: Option<u32>,
    stop: StopToken,
    mut progress: impl FnMut(Progress),
    mut create: impl FnMut(&DecodedPcm, u64, Option<Duration>, StopToken) -> Result<S, OutputError>,
    mut controls: Option<&mut super::seek::Controller>,
    clock: Arc<dyn MonoClock>,
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
            let (worker, rx) = worker_timed(source, local.clone(), clock.clone());
            (info, Some(worker), rx)
        }
        Initial::Idle(info) => (info, None, PcmReceiver::disconnected()),
    };
    let mut current = None;
    let mut cursor = 0;
    let mut output: Option<S> = None;
    let mut maximum = 0;
    let mut reason = EndReason::ProgramEnd;
    let mut segments = 0;
    let mut eof = false;
    let mut report_at = clock.now();
    let mut last_pcm_at = clock.now();
    let mut empty_since: Option<Instant> = None;
    let result = 'playback: loop {
        if stop.is_cancelled() {
            break 'playback Ok(PlaybackReport {
                output: output.as_ref().map(|o| o.summary()),
                segments,
                reason: EndReason::Cancelled,
            });
        }
        if let Some(control) = controls.as_mut() {
            control.note_playback_active(output.is_some());
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
                        control.report_failure("再生切替失敗（現在の状態を維持）", &error);
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
                        last_pcm_at = clock.now();
                        empty_since = None;
                        report_at = clock.now();
                        control.report(format!("再生切替完了: {} / {:?}", info.station, info.mode));
                    }
                }
            }
        }
        if controls
            .as_mut()
            .is_some_and(|c| std::mem::take(&mut c.changed))
        {
            report_at = clock.now();
        }
        let paused = controls.as_ref().is_some_and(|c| c.paused);
        let volume = controls.as_ref().map_or(25, |c| c.volume);
        if let Some(out) = &output {
            out.configure(paused, volume);
        }
        if paused {
            last_pcm_at = clock.now();
        }
        if running_worker.is_some() && info.mode == ProbeMode::Live && Utc::now() >= info.ends_at {
            break 'playback Ok(PlaybackReport {
                output: output.as_ref().map(|o| o.summary()),
                segments,
                reason: EndReason::ProgramEnd,
            });
        }
        if running_worker.is_some()
            && !paused
            && current.is_none()
            && !eof
            && output.as_ref().is_none_or(|o| o.accepted() < maximum)
        {
            let event = rx.try_recv();
            if stop.is_cancelled() {
                continue;
            }
            match event {
                Ok(Event::Pcm(pcm)) => {
                    let pcm = pcm.into_pcm();
                    last_pcm_at = clock.now();
                    if output.is_none() {
                        (maximum, reason) =
                            match frame_limit(&info, pcm.summary.sample_rate, seconds) {
                                Ok(limit) => limit,
                                Err(error) => break 'playback Err(error),
                            };
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
                                break 'playback failure_or_cancelled(
                                    &stop,
                                    output.as_ref(),
                                    segments,
                                    error.into(),
                                );
                            }
                        };
                        sink.configure(paused, volume);
                        output = Some(sink);
                        report_at = clock.now();
                    }
                    segments += 1;
                    current = Some(pcm);
                    cursor = 0;
                }
                Ok(Event::Failed(error)) => {
                    break 'playback failure_or_cancelled(&stop, output.as_ref(), segments, error);
                }
                Ok(Event::End) => {
                    let Some(out) = output.as_mut() else {
                        break 'playback failure_or_cancelled(
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
                        break 'playback failure_or_cancelled(
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
                    break 'playback failure_or_cancelled(
                        &stop,
                        output.as_ref(),
                        segments,
                        PlaybackError::Worker,
                    );
                }
            }
        }
        let waiting_for_audio = running_worker.is_some()
            && current.is_none()
            && !eof
            && !paused
            && output.as_ref().is_none_or(|o| o.accepted() < maximum);
        let silent_for = clock.now().saturating_duration_since(last_pcm_at);
        let heard = output
            .as_ref()
            .is_some_and(|sink| sink.summary().frames > 0);
        let queued = output.as_ref().map(AudioSink::queued_frames).unwrap_or(0);
        let buffer_empty = waiting_for_audio && heard && queued == 0;
        if buffer_empty {
            if empty_since.is_none() {
                empty_since = Some(clock.now());
            }
        } else {
            empty_since = None;
        }
        let empty_for = empty_since
            .map(|start| clock.now().saturating_duration_since(start))
            .unwrap_or_default();
        let phase = audible_wait(AudibleGap {
            heard,
            paused,
            ended: eof,
            waiting: waiting_for_audio,
            empty_for,
            since_pcm: silent_for,
            output_reconnecting: output.as_ref().and_then(AudioSink::reconnecting_for),
        });
        if let AudiblePhase::GaveUp = phase {
            break 'playback failure_or_cancelled(
                &stop,
                output.as_ref(),
                segments,
                PlaybackError::ReconnectGaveUp,
            );
        }
        if let Some(out) = output.as_mut() {
            if let Some(pcm) = current.as_ref() {
                match out.enqueue(pcm, &mut cursor) {
                    Ok(true) => current = None,
                    Ok(false) => (),
                    Err(OutputError::Deadline) => {
                        break 'playback Ok(PlaybackReport {
                            output: Some(out.summary()),
                            segments,
                            reason: EndReason::ProgramEnd,
                        });
                    }
                    Err(error) => {
                        break 'playback failure_or_cancelled(
                            &stop,
                            Some(&*out),
                            segments,
                            error.into(),
                        );
                    }
                }
            }
            match out.poll() {
                Ok(true) => {
                    break 'playback Ok(PlaybackReport {
                        output: Some(out.summary()),
                        segments,
                        reason,
                    });
                }
                Ok(false) => (),
                Err(OutputError::Deadline) => {
                    break 'playback Ok(PlaybackReport {
                        output: Some(out.summary()),
                        segments,
                        reason: EndReason::ProgramEnd,
                    });
                }
                Err(error) => {
                    break 'playback failure_or_cancelled(
                        &stop,
                        Some(&*out),
                        segments,
                        error.into(),
                    );
                }
            }
        }
        let refresh = matches!(
            phase,
            AudiblePhase::Receiving | AudiblePhase::Reconnecting(_)
        );
        if refresh && report_at > clock.now() + Duration::from_secs(1) {
            report_at = clock.now();
        }
        if running_worker.is_some() && clock.now() >= report_at {
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
            match phase {
                AudiblePhase::Reconnecting(elapsed) => {
                    snapshot.reconnecting_for = Some(elapsed);
                }
                AudiblePhase::Receiving => snapshot.receiving_for = Some(empty_for),
                AudiblePhase::Quiet | AudiblePhase::GaveUp => {}
            }
            progress(snapshot);
            let interval = controls.as_ref().map_or(Duration::from_secs(5), |control| {
                control.notification_interval
            });
            let interval = if refresh {
                interval.min(Duration::from_secs(1))
            } else {
                interval
            };
            report_at = clock.now() + interval;
        }
        clock.sleep(Duration::from_millis(5));
    };
    // An error can arrive between periodic notifications. Read the current generation once
    // more before teardown; this is an observation, not a post-shutdown final aggregate.
    if result.is_err() && running_worker.is_some() {
        let mut snapshot = Progress::active(
            &info,
            output.as_ref().map(AudioSink::summary),
            segments,
            controls.as_ref().is_some_and(|control| control.paused),
            controls.as_ref().map_or(25, |control| control.volume),
            controls
                .as_ref()
                .and_then(|control| control.pending_target.clone()),
        );
        snapshot.pipeline = running_worker.as_ref().and_then(Worker::diagnostics);
        // A reporting callback must not keep an errored output generation running.
        local.cancel();
        progress(snapshot);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "explicit native output: always muted synthetic AAC, no account/network, about 50 seconds"]
    fn native_muted_acquisition_gap_distinguishes_startup_from_filled_prefetch() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        struct Delayed {
            info: StreamInfo,
            adts: Vec<u8>,
            started: Arc<AtomicBool>,
            polls: usize,
            gap_after: usize,
            delayed: bool,
            cancelled: Option<std::sync::Arc<dyn Fn() -> bool + Send + Sync>>,
        }
        impl SegmentSource for Delayed {
            fn info(&self) -> &StreamInfo {
                &self.info
            }
            fn set_cancellation(
                &mut self,
                cancelled: std::sync::Arc<dyn Fn() -> bool + Send + Sync>,
            ) {
                self.cancelled = Some(cancelled);
            }
            fn poll_segment(&mut self) -> Result<Next, ProbeError> {
                if self.polls == self.gap_after && !self.delayed {
                    if !self.started.load(Ordering::Acquire) {
                        return Ok(Next::Pending(Duration::from_millis(10)));
                    }
                    let began = Instant::now();
                    while began.elapsed() < Duration::from_secs(11) {
                        if self.cancelled.as_ref().is_some_and(|check| check()) {
                            return Err(ProbeError::Cancelled);
                        }
                        thread::sleep(Duration::from_millis(5));
                    }
                    self.delayed = true;
                }
                self.polls += 1;
                event(self.adts.clone(), false)
            }
        }
        let bytes = packet(48_000).repeat(240); // 5.12s segments, under the 4MiB PCM bound.
        for (gap_after, seconds) in [(1, 12), (4, 25)] {
            let started = Arc::new(AtomicBool::new(false));
            let stop = StopToken::default();
            let (_sender, receiver) = mpsc::channel();
            let mut control = super::super::seek::Controller::new(receiver, stop.clone(), |_| {
                panic!("no stream preparation is requested")
            });
            control.volume = 0; // Applied before the first enqueue starts the real stream.
            control.notification_interval = Duration::from_millis(250);
            let mut acquisition = Duration::ZERO;
            let mut acquisition_pcm_start = Duration::ZERO;
            let mut acquisition_pcm_min = Duration::ZERO;
            let mut segment_media = Duration::ZERO;
            let mut gate_frames = 0;
            let mut gap_waiting_min = u32::MAX;
            let mut gap_waiting_max = 0;
            let began = Instant::now();
            let report = play_controlled(
                Delayed {
                    info: info(60_000),
                    adts: bytes.clone(),
                    started: started.clone(),
                    polls: 0,
                    gap_after,
                    delayed: false,
                    cancelled: None,
                },
                Some(seconds),
                stop.clone(),
                &mut control,
                |snapshot| {
                    assert_eq!(snapshot.volume, 0);
                    if snapshot
                        .output
                        .as_ref()
                        .is_some_and(|output| output.frames >= u64::from(output.sample_rate))
                    {
                        if gate_frames == 0 {
                            gate_frames =
                                snapshot.output.as_ref().map_or(0, |output| output.frames);
                        }
                        started.store(true, Ordering::Release);
                    }
                    if began.elapsed() > Duration::from_secs(50) {
                        stop.cancel();
                    }
                    if let Some(pipeline) = snapshot.pipeline {
                        assert!(pipeline.waiting_pcm_segments <= 3);
                        if pipeline.stage == Stage::Acquisition
                            && pipeline.elapsed >= Duration::from_secs(1)
                        {
                            gap_waiting_min = gap_waiting_min.min(pipeline.waiting_pcm_segments);
                            gap_waiting_max = gap_waiting_max.max(pipeline.waiting_pcm_segments);
                        }
                        if pipeline.acquisition_max > acquisition {
                            acquisition = pipeline.acquisition_max;
                            acquisition_pcm_start = pipeline.acquisition_max_pcm_start;
                            acquisition_pcm_min = pipeline.acquisition_max_pcm_min;
                        }
                        segment_media = segment_media.max(pipeline.media_last);
                    }
                },
            )
            .unwrap();
            let output = report.output.unwrap();
            println!(
                "synthetic_muted gap_after={gap_after} gate_frames={gate_frames} gap_waiting_min={gap_waiting_min} gap_waiting_max={gap_waiting_max} output_frames={} underrun_frames={} acquisition_max_ms={} acquisition_pcm_start_ms={} acquisition_pcm_min_ms={} segment_media_ms={} wall_ms={}",
                output.frames,
                output.underrun_frames,
                acquisition.as_millis(),
                acquisition_pcm_start.as_millis(),
                acquisition_pcm_min.as_millis(),
                segment_media.as_millis(),
                began.elapsed().as_millis()
            );
            assert_eq!(report.reason, EndReason::Limit);
            assert_eq!(output.frames, u64::from(seconds) * 48_000);
            assert!(acquisition >= Duration::from_secs(11));
            assert_eq!(segment_media, Duration::from_millis(5120));
            assert_ne!(gap_waiting_min, u32::MAX);
            assert_eq!(acquisition_pcm_min, Duration::ZERO);
            if gap_after == 1 {
                assert!(output.underrun_frames > 0);
                assert_eq!(gap_waiting_max, 0);
                assert_eq!(acquisition_pcm_start, Duration::ZERO);
            } else {
                assert_eq!(output.underrun_frames, 0);
                assert!(gap_waiting_max > 0);
                assert!(acquisition_pcm_start > Duration::ZERO);
            }
        }
    }

    use crate::api::stream::playback::Segment;
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
            if snapshot.decoded_segments == 3
                && snapshot.stage == Stage::Backpressure
                && snapshot.waiting_pcm_segments == 3
            {
                assert!(!snapshot.media_last.is_zero());
                assert_eq!(
                    snapshot.waiting_pcm_media,
                    snapshot.media_last.saturating_mul(3)
                );
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
        assert_eq!(worker.diagnostics().unwrap().waiting_pcm_segments, 2);
        assert_eq!(polls.load(Ordering::SeqCst), 3);
        let queued: Vec<_> = receiver.try_iter().collect();
        assert_eq!(queued.len(), 2);
        for event in queued {
            let Event::Pcm(pcm) = event else {
                panic!("queue must contain decoded PCM only");
            };
            let pcm = pcm.into_pcm();
            assert!(pcm.samples.len() * std::mem::size_of::<f32>() <= 4 * 1024 * 1024);
        }
        assert_eq!(worker.diagnostics().unwrap().stage, Stage::Finished);
        assert_eq!(worker.diagnostics().unwrap().waiting_pcm_segments, 0);
        assert_eq!(
            worker.diagnostics().unwrap().waiting_pcm_media,
            Duration::ZERO
        );
    }
    #[test]
    fn received_pcm_is_not_counted_while_the_consumer_is_paused() {
        let bytes = packet(24_000).repeat(50);
        let stop = StopToken::default();
        let (mut worker, receiver) = worker(
            Source {
                info: info(60_000),
                events: (0..4).map(|_| event(bytes.clone(), false)).collect(),
            },
            stop.clone(),
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        while worker.diagnostics().unwrap().waiting_pcm_segments != 3 {
            assert!(Instant::now() < deadline, "prefetch never filled");
            thread::sleep(Duration::from_millis(1));
        }
        let Event::Pcm(first) = receiver.recv_timeout(Duration::from_secs(2)).unwrap() else {
            panic!("expected PCM");
        };
        // Keep the returned value alive without converting it to the consumer's current PCM.
        while worker.diagnostics().unwrap().decoded_segments != 4 {
            assert!(Instant::now() < deadline, "next PCM was not decoded");
            thread::sleep(Duration::from_millis(1));
        }
        let snapshot = worker.diagnostics().unwrap();
        assert!(snapshot.waiting_pcm_segments <= 3);
        stop.cancel();
        worker.handle.take().unwrap().join().unwrap();
        let snapshot = worker.diagnostics().unwrap();
        assert_eq!(snapshot.waiting_pcm_segments, 2);
        assert_eq!(
            snapshot.waiting_pcm_media,
            first.pcm.summary.media_duration.saturating_mul(2)
        );
        drop(first);
        assert_eq!(worker.diagnostics().unwrap().waiting_pcm_segments, 2);
        drop(receiver);
        assert_eq!(worker.diagnostics().unwrap().waiting_pcm_segments, 0);
    }
    #[test]
    fn acquisition_pair_tracks_receipts_while_the_source_is_blocked() {
        struct Gated {
            source: Source,
            entered: mpsc::Sender<()>,
            release: Receiver<()>,
        }
        impl SegmentSource for Gated {
            fn info(&self) -> &StreamInfo {
                self.source.info()
            }
            fn poll_segment(&mut self) -> Result<Next, ProbeError> {
                if self.source.events.is_empty() {
                    self.entered.send(()).unwrap();
                    self.release.recv_timeout(Duration::from_secs(2)).unwrap();
                    return Ok(Next::End);
                }
                self.source.poll_segment()
            }
        }
        let (entered, observed) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let bytes = packet(24_000).repeat(50);
        let (mut worker, receiver) = worker(
            Gated {
                source: Source {
                    info: info(60_000),
                    events: (0..3).map(|_| event(bytes.clone(), false)).collect(),
                },
                entered,
                release: gate,
            },
            StopToken::default(),
        );
        // Receiving the first PCM allows the third send to complete and acquisition to start.
        let Event::Pcm(first) = receiver.recv_timeout(Duration::from_secs(2)).unwrap() else {
            panic!("expected PCM");
        };
        let media = first.into_pcm().summary.media_duration;
        observed.recv_timeout(Duration::from_secs(2)).unwrap();
        let before = worker.diagnostics().unwrap();
        assert_eq!(before.stage, Stage::Acquisition);
        assert_eq!(before.waiting_pcm_media, media.saturating_mul(2));
        for _ in 0..2 {
            let Event::Pcm(pcm) = receiver.recv_timeout(Duration::from_secs(2)).unwrap() else {
                panic!("expected queued PCM");
            };
            drop(pcm.into_pcm());
        }
        // Make this the longest acquisition without assuming a particular scheduler speed.
        let deadline = Instant::now() + Duration::from_secs(1);
        while worker.diagnostics().unwrap().elapsed <= before.acquisition_max {
            assert!(
                Instant::now() < deadline,
                "acquisition clock did not advance"
            );
            thread::yield_now();
        }
        release.send(()).unwrap();
        assert!(matches!(
            receiver.recv_timeout(Duration::from_secs(2)).unwrap(),
            Event::End
        ));
        worker.handle.take().unwrap().join().unwrap();
        let after = worker.diagnostics().unwrap();
        assert_eq!(after.acquisition_max_pcm_start, media.saturating_mul(2));
        assert_eq!(after.acquisition_max_pcm_min, Duration::ZERO);
        assert_eq!(after.waiting_pcm_segments, 0);
        assert!(after.acquisition_max > before.acquisition_max);
    }
    #[test]
    fn waiting_pcm_measurements_clear_when_the_output_receiver_disconnects() {
        let bytes = packet(24_000).repeat(50);
        let (mut worker, receiver) = worker(
            Source {
                info: info(60_000),
                events: (0..4).map(|_| event(bytes.clone(), false)).collect(),
            },
            StopToken::default(),
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        while worker.diagnostics().unwrap().waiting_pcm_segments != 3 {
            assert!(Instant::now() < deadline, "prefetch never filled");
            thread::sleep(Duration::from_millis(5));
        }
        drop(receiver);
        while !worker.handle.as_ref().unwrap().is_finished() {
            assert!(
                Instant::now() < deadline,
                "disconnected worker did not exit"
            );
            thread::sleep(Duration::from_millis(5));
        }
        worker.handle.take().unwrap().join().unwrap();
        let snapshot = worker.diagnostics().unwrap();
        assert_eq!(snapshot.waiting_pcm_segments, 0);
        assert_eq!(snapshot.waiting_pcm_media, Duration::ZERO);
    }
    #[test]
    fn boxed_source_receives_generation_cancellation_before_polling() {
        use std::sync::Arc;
        struct Waiting {
            info: StreamInfo,
            cancelled: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
            entered: mpsc::Sender<()>,
            done: mpsc::Sender<()>,
            http: Arc<crate::api::stream::playback::RequestMeasurements>,
        }
        impl SegmentSource for Waiting {
            fn enable_http_diagnostics(
                &mut self,
            ) -> Option<Arc<crate::api::stream::playback::RequestMeasurements>> {
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
                    .enter(crate::api::stream::playback::RequestStage::Headers);
                self.entered.send(()).unwrap();
                let began = Instant::now();
                while !check() {
                    assert!(began.elapsed() < Duration::from_secs(2));
                    thread::sleep(Duration::from_millis(1));
                }
                self.http
                    .enter(crate::api::stream::playback::RequestStage::Idle);
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
            http: Arc::new(crate::api::stream::playback::RequestMeasurements::new()),
            cancelled: None,
            entered,
            done,
        });
        let parent = StopToken::default();
        let (mut worker, events) = worker(source, parent.child());
        seen.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(
            worker.diagnostics().unwrap().http.unwrap().stage,
            crate::api::stream::playback::RequestStage::Headers
        );
        parent.cancel();
        finished.recv_timeout(Duration::from_secs(2)).unwrap();
        worker.handle.take().unwrap().join().unwrap();
        assert!(events.try_recv().is_err());
        assert_eq!(
            worker.diagnostics().unwrap().http.unwrap().stage,
            crate::api::stream::playback::RequestStage::Idle
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
                sample_format: crate::player::output::OutputSampleFormat::F32,
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
    fn mapped_segment(raw: &[u8], time: u64, sequence: u32) -> Segment {
        let (init, bytes) = crate::player::fixtures::fragmented_access_units(
            &[0x13, 0x08],
            &[raw; 3],
            24000,
            1,
            time,
            sequence,
        );
        Segment {
            bytes,
            initialization: Some(init.into()),
            duration: Some(Duration::from_millis(128)),
            codec: Some("mp4a.40.2".into()),
            discontinuity: false,
            starts_at: None,
        }
    }

    #[test]
    fn mapped_fragment_worker_delivers_continuous_pcm_without_opening_an_output_device() {
        let (_, media) = crate::player::fixtures::mp4_aac(&[0x13, 0x08]);
        let raw = &media[8..];
        let source_info = info(60_000);
        let mut first = mapped_segment(raw, 1000, 1);
        let mut second = mapped_segment(raw, 4072, 2);
        first.starts_at = Some(source_info.starts_at);
        second.starts_at = Some(source_info.starts_at + chrono::Duration::milliseconds(128));
        let (mut worker, receiver) = worker(
            Source {
                info: source_info,
                events: [
                    Ok(Next::Segment(first)),
                    Ok(Next::Segment(second)),
                    Ok(Next::End),
                ]
                .into(),
            },
            StopToken::default(),
        );
        let mut reference = crate::player::aac::RawAacDecoder::new(&[0x13, 0x08]).unwrap();
        for _ in 0..2 {
            let Event::Pcm(pcm) = receiver.recv_timeout(Duration::from_secs(2)).unwrap() else {
                panic!()
            };
            assert_eq!(
                pcm.into_pcm().samples,
                reference
                    .decode_packets_pcm(&[raw; 3], None)
                    .unwrap()
                    .samples
            );
        }
        assert!(matches!(
            receiver.recv_timeout(Duration::from_secs(2)).unwrap(),
            Event::End
        ));
        worker.handle.take().unwrap().join().unwrap();
    }

    #[test]
    fn mapped_fragments_reach_the_program_end_through_the_output_loop() {
        let (_, media) = crate::player::fixtures::mp4_aac(&[0x13, 0x08]);
        let raw = &media[8..];
        let source = Source {
            info: info(384),
            events: (0..3)
                .map(|i| {
                    Ok(Next::Segment(mapped_segment(
                        raw,
                        1000 + u64::from(i) * 3072,
                        i + 1,
                    )))
                })
                .collect(),
        };
        let report = play_with(source, None, StopToken::default(), |_| {}, sink).unwrap();
        assert_eq!(report.reason, EndReason::ProgramEnd);
        assert_eq!(report.output.unwrap().frames, 9216);
    }

    #[test]
    fn mapped_seek_uses_the_broadcast_anchor_and_not_the_large_media_decode_time() {
        let (_, media) = crate::player::fixtures::mp4_aac(&[0x13, 0x08]);
        let raw = &media[8..];
        let mut source_info = info(384);
        source_info.position = Duration::from_millis(64);
        let start = source_info.starts_at;
        let source = Source {
            info: source_info.clone(),
            events: (0..3)
                .map(|i| {
                    let mut segment =
                        mapped_segment(raw, (1u64 << 40) + u64::from(i) * 3072, i + 1);
                    segment.starts_at =
                        Some(start + chrono::Duration::milliseconds(i64::from(i) * 128));
                    Ok(Next::Segment(segment))
                })
                .collect(),
        };
        let report = play_with(source, None, StopToken::default(), |_| {}, sink).unwrap();
        assert_eq!(report.reason, EndReason::ProgramEnd);
        assert_eq!(report.output.unwrap().frames, 7680);
        // A tfdt alone never supplies the missing wall-clock broadcast anchor.
        let source = Source {
            info: source_info,
            events: [Ok(Next::Segment(mapped_segment(raw, 1u64 << 40, 1)))].into(),
        };
        let result = play_with(
            source,
            None,
            StopToken::default(),
            |_| {},
            |_, _, _, _| -> Result<Sink, OutputError> {
                panic!("an unbound seek must fail before opening output")
            },
        );
        assert!(matches!(
            result,
            Err(PlaybackError::Source(ProbeError::InvalidPlaylist))
        ));
    }

    #[test]
    #[ignore = "explicit muted output, no network/account; five seconds, RADIKO_FRAGMENT_SECONDS=1..1800"]
    fn native_muted_fragmented_aac_reaches_the_explicit_output_limit() {
        struct Fragments {
            info: StreamInfo,
            raw: Vec<u8>,
            sequence: u32,
        }
        impl SegmentSource for Fragments {
            fn info(&self) -> &StreamInfo {
                &self.info
            }
            fn poll_segment(&mut self) -> Result<Next, ProbeError> {
                self.sequence += 1;
                let (init, bytes) = crate::player::fixtures::fragmented_access_units(
                    &[0x11, 0x88],
                    &vec![self.raw.as_slice(); 100],
                    48000,
                    1,
                    (1u64 << 40) + u64::from(self.sequence - 1) * 102400,
                    self.sequence,
                );
                Ok(Next::Segment(Segment {
                    bytes,
                    initialization: Some(init.into()),
                    duration: Some(Duration::from_secs_f64(102400.0 / 48000.0)),
                    codec: Some("mp4a.40.2".into()),
                    discontinuity: false,
                    starts_at: None,
                }))
            }
        }
        let seconds = match std::env::var("RADIKO_FRAGMENT_SECONDS") {
            Ok(value) => value
                .parse::<u32>()
                .ok()
                .filter(|n| (1..=1800).contains(n))
                .expect("RADIKO_FRAGMENT_SECONDS must be 1..1800"),
            Err(std::env::VarError::NotPresent) => 5,
            Err(_) => panic!("RADIKO_FRAGMENT_SECONDS must be numeric"),
        };
        let adts = packet(48000);
        let source = Fragments {
            info: info((i64::from(seconds) + 60) * 1000),
            raw: adts[7..].to_vec(),
            sequence: 0,
        };
        let stop = StopToken::default();
        let (_sender, receiver) = mpsc::channel();
        let mut control = super::super::seek::Controller::new(receiver, stop.clone(), |_| {
            panic!("no preparation requested")
        });
        control.volume = 0;
        control.notification_interval = Duration::from_secs(30);
        let began = Instant::now();
        let deadline_stop = stop.clone();
        let watchdog = thread::spawn(move || {
            while !deadline_stop.is_cancelled()
                && began.elapsed() < Duration::from_secs(u64::from(seconds) + 30)
            {
                thread::sleep(Duration::from_millis(50));
            }
            deadline_stop.cancel();
        });
        let result = play_controlled(source, Some(seconds), stop, &mut control, |snapshot| {
            if let Some(output) = snapshot.output {
                println!(
                    "synthetic_fragmented_progress frames={} underrun_frames={}",
                    output.frames, output.underrun_frames
                );
            }
        });
        watchdog.join().unwrap();
        let report = result.unwrap();
        let output = report.output.unwrap();
        println!(
            "synthetic_fragmented_muted seconds={seconds} frames={} rate={} channels={} underrun_frames={} wall_ms={}",
            output.frames,
            output.sample_rate,
            output.channels,
            output.underrun_frames,
            began.elapsed().as_millis()
        );
        assert_eq!(report.reason, EndReason::Limit);
        assert_eq!(output.frames, u64::from(seconds) * 48000);
        assert_eq!(output.underrun_frames, 0);
    }

    #[test]
    fn mapped_fragments_reject_changed_initialization_and_wrong_hls_timing_until_discontinuity() {
        let (_, media) = crate::player::fixtures::mp4_aac(&[0x13, 0x08]);
        let raw = &media[8..];
        let stop = StopToken::default();
        let mut first = mapped_segment(raw, 1000, 1);
        first.starts_at = Some(info(60_000).starts_at);
        for wrong_stamp in [false, true] {
            let mut state = None;
            decode_segment_pcm(&mut state, &first, &stop).unwrap();
            let mut next = mapped_segment(raw, 4072, 2);
            if wrong_stamp {
                next.starts_at = first
                    .starts_at
                    .map(|time| time + chrono::Duration::seconds(2));
            } else {
                next.duration = Some(Duration::from_secs(2));
            }
            assert_eq!(
                decode_segment_pcm(&mut state, &next, &stop).unwrap_err(),
                AacError::InvalidContainer
            );
        }
        let mut state = None;
        let baseline = decode_segment_pcm(&mut state, &first, &stop).unwrap();
        let mut changed = mapped_segment(raw, 4072, 2);
        let mut init = changed.initialization.as_ref().unwrap().to_vec();
        let mvhd = init.windows(4).position(|w| w == b"mvhd").unwrap();
        init[mvhd + 8..mvhd + 12].copy_from_slice(&1u32.to_be_bytes());
        changed.initialization = Some(init.into());
        assert_eq!(
            decode_segment_pcm(&mut state, &changed, &stop).unwrap_err(),
            AacError::FormatChanged
        );
        changed.discontinuity = true;
        assert_eq!(
            decode_segment_pcm(&mut state, &changed, &stop)
                .unwrap()
                .samples,
            baseline.samples
        );
        let mut adts = Segment {
            bytes: packet(24000),
            initialization: None,
            duration: None,
            codec: None,
            discontinuity: false,
            starts_at: None,
        };
        assert_eq!(
            decode_segment_pcm(&mut state, &adts, &stop).unwrap_err(),
            AacError::FormatChanged
        );
        adts.discontinuity = true;
        assert!(decode_segment_pcm(&mut state, &adts, &stop).is_ok());
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
            initialization: None,
            duration: None,
            bytes,
            codec: Some("mp4a.40.2".into()),
            discontinuity,
            starts_at: None,
        }))
    }
    #[test]
    fn failure_reports_latest_counters_before_the_next_periodic_notification() {
        struct FailingSink {
            polls: u64,
        }
        impl AudioSink for FailingSink {
            fn enqueue(&mut self, _: &DecodedPcm, _: &mut usize) -> Result<bool, OutputError> {
                Ok(false)
            }
            fn accepted(&self) -> u64 {
                0
            }
            fn summary(&self) -> OutputSummary {
                OutputSummary {
                    frames: self.polls * 240,
                    underrun_frames: self.polls * 17,
                    sample_rate: 24_000,
                    channels: 1,
                    sample_format: crate::player::output::OutputSampleFormat::F32,
                }
            }
            fn poll(&mut self) -> Result<bool, OutputError> {
                self.polls += 1;
                if self.polls == 2 {
                    Err(OutputError::Device)
                } else {
                    Ok(false)
                }
            }
            fn finish(&mut self) {}
        }
        let output_stop = std::cell::RefCell::new(None::<StopToken>);
        let mut snapshots = Vec::new();
        let result = play_with(
            Source {
                info: info(60_000),
                events: VecDeque::from([event(packet(24_000), false)]),
            },
            None,
            StopToken::default(),
            |snapshot| {
                if snapshot
                    .output
                    .as_ref()
                    .is_some_and(|output| output.frames == 480)
                {
                    assert!(output_stop.borrow().as_ref().unwrap().is_cancelled());
                }
                snapshots.push(snapshot);
            },
            |_, _, _, stop| {
                *output_stop.borrow_mut() = Some(stop);
                Ok(FailingSink { polls: 0 })
            },
        );
        assert!(matches!(
            result,
            Err(PlaybackError::Output(OutputError::Device))
        ));
        let last = snapshots.last().unwrap();
        assert_eq!(last.output.as_ref().unwrap().frames, 480);
        assert_eq!(last.output.as_ref().unwrap().underrun_frames, 34);
        assert!(last.pipeline.is_some());
    }
    #[test]
    fn failure_before_pcm_reports_no_output_and_preserves_the_original_error() {
        use crate::api::stream::playback::{RequestMeasurements, RequestStage};
        use std::sync::Arc;
        struct FailingHttp {
            info: StreamInfo,
            measurements: Arc<RequestMeasurements>,
            timeout: bool,
        }
        impl SegmentSource for FailingHttp {
            fn info(&self) -> &StreamInfo {
                &self.info
            }
            fn enable_http_diagnostics(&mut self) -> Option<Arc<RequestMeasurements>> {
                Some(self.measurements.clone())
            }
            fn poll_segment(&mut self) -> Result<Next, ProbeError> {
                self.measurements.enter(RequestStage::Headers);
                if self.timeout {
                    self.measurements.failed(true, false);
                } else {
                    self.measurements.received(404);
                }
                self.measurements.enter(RequestStage::Idle);
                Err(if self.timeout {
                    ProbeError::Network
                } else {
                    ProbeError::NotFound
                })
            }
        }
        for timeout in [false, true] {
            let source: Box<dyn SegmentSource> = Box::new(FailingHttp {
                info: info(60_000),
                measurements: Arc::new(RequestMeasurements::new()),
                timeout,
            });
            let mut snapshots = Vec::new();
            let result = play_with::<Sink>(
                source,
                None,
                StopToken::default(),
                |snapshot| snapshots.push(snapshot),
                |_, _, _, _| panic!("a source failure cannot open an output device"),
            );
            assert!(matches!(
                (&result, timeout),
                (Err(PlaybackError::Source(ProbeError::NotFound)), false)
                    | (Err(PlaybackError::Source(ProbeError::Network)), true)
            ));
            let last = snapshots.last().unwrap();
            assert!(last.output.is_none());
            assert_eq!(last.segments, 0);
            let pipeline = last.pipeline.as_ref().unwrap();
            assert_eq!(pipeline.stage, Stage::Finished);
            let http = pipeline.http.as_ref().unwrap();
            assert_eq!(http.stage, RequestStage::Idle);
            assert_eq!((http.attempts, http.retry_waits), (1, 0));
            assert_eq!(http.response_status, if timeout { None } else { Some(404) });
            assert_eq!(
                (http.network_failures, http.timeouts, http.connect_failures),
                (u64::from(timeout), u64::from(timeout), 0)
            );
        }
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
            move |message, _kind, _prefix, _output| {
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
            |_, _, _, _| {},
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
                        initialization: None,
                        duration: None,
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
                        initialization: None,
                        duration: None,
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
                        initialization: None,
                        duration: None,
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
            let mut snapshots = Vec::new();
            assert!(
                play_with(
                    source,
                    None,
                    stop.clone(),
                    |snapshot| snapshots.push(snapshot),
                    sink
                )
                .is_err()
            );
            assert!(stop.is_cancelled());
            let last = snapshots.last().unwrap();
            assert_eq!(last.segments, 1);
            assert!(last.output.as_ref().is_some_and(|output| output.frames > 0));
            assert!(last.pipeline.is_some());
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
    fn gap(empty_for: Duration, since_pcm: Duration) -> AudibleGap {
        AudibleGap {
            heard: true,
            paused: false,
            ended: false,
            waiting: true,
            empty_for,
            since_pcm,
            output_reconnecting: None,
        }
    }
    #[test]
    fn silence_shorter_than_thirty_seconds_continues_and_the_limit_gives_up() {
        assert_eq!(RECEPTION_AFTER, Duration::from_secs(2));
        assert_eq!(NOTICE_AFTER, Duration::from_secs(30));
        let below = Duration::from_secs(2) - Duration::from_nanos(1);
        assert_eq!(audible_wait(gap(below, below)).notice(), None);
        assert_eq!(
            audible_wait(gap(Duration::from_secs(2), Duration::from_secs(2))).notice(),
            Some("受信待ち…".into())
        );
        assert_eq!(
            audible_wait(gap(Duration::from_secs(29), Duration::from_secs(29))),
            AudiblePhase::Receiving
        );
        assert!(matches!(
            audible_wait(gap(Duration::from_secs(30), Duration::from_secs(30))),
            AudiblePhase::Reconnecting(elapsed) if elapsed == Duration::from_secs(30)
        ));
        assert!(matches!(
            audible_wait(gap(Duration::from_secs(90), Duration::from_secs(90))),
            AudiblePhase::Reconnecting(elapsed) if elapsed == Duration::from_secs(90)
        ));
        assert!(matches!(
            audible_wait(gap(crate::reconnect::LIMIT, crate::reconnect::LIMIT)),
            AudiblePhase::Reconnecting(_)
        ));
        assert_eq!(
            audible_wait(gap(
                crate::reconnect::LIMIT + Duration::from_secs(1),
                crate::reconnect::LIMIT + Duration::from_secs(1)
            )),
            AudiblePhase::GaveUp
        );
        let mut idle = gap(
            crate::reconnect::LIMIT + Duration::from_secs(1),
            crate::reconnect::LIMIT + Duration::from_secs(1),
        );
        idle.waiting = false;
        assert_eq!(audible_wait(idle), AudiblePhase::Quiet);
        let mut device = gap(Duration::ZERO, Duration::ZERO);
        device.waiting = false;
        device.output_reconnecting = Some(Duration::from_secs(2));
        assert!(matches!(
            audible_wait(device),
            AudiblePhase::Reconnecting(elapsed) if elapsed == Duration::from_secs(2)
        ));
        for (label, mut suppressed) in [
            ("paused", {
                let mut gap = gap(Duration::from_secs(20), Duration::from_secs(20));
                gap.paused = true;
                gap
            }),
            ("before audio", {
                let mut gap = gap(Duration::from_secs(40), Duration::from_secs(40));
                gap.heard = false;
                gap
            }),
            ("program end", {
                let mut gap = gap(Duration::from_secs(20), Duration::from_secs(40));
                gap.ended = true;
                gap
            }),
        ] {
            assert_eq!(audible_wait(suppressed).notice(), None, "{label}");
            suppressed.output_reconnecting = Some(Duration::from_secs(3));
            assert_eq!(
                audible_wait(suppressed).notice(),
                None,
                "{label} must hide reconnect text"
            );
        }
        let mut still_buffered = gap(Duration::ZERO, Duration::from_secs(20));
        still_buffered.empty_for = Duration::ZERO;
        assert_eq!(audible_wait(still_buffered), AudiblePhase::Quiet);
    }
    #[test]
    fn pause_volume_and_stop_apply_while_the_source_stays_quiet() {
        use std::sync::{Arc, Mutex};
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = seen.clone();
        struct Watching {
            frames: u64,
            maximum: u64,
            rate: u32,
            channels: u16,
            seen: Arc<Mutex<Vec<(bool, u32)>>>,
        }
        impl AudioSink for Watching {
            fn configure(&self, paused: bool, volume: u32) {
                self.seen.lock().unwrap().push((paused, volume));
            }
            fn enqueue(
                &mut self,
                pcm: &DecodedPcm,
                cursor: &mut usize,
            ) -> Result<bool, OutputError> {
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
                    sample_format: crate::player::output::OutputSampleFormat::F32,
                }
            }
            fn poll(&mut self) -> Result<bool, OutputError> {
                Ok(self.frames == self.maximum)
            }
            fn finish(&mut self) {}
        }
        let stop = StopToken::default();
        let (tx, rx) = mpsc::channel();
        let mut control = super::super::seek::Controller::with_reporter(
            rx,
            stop.clone(),
            |_| panic!("no prepare"),
            |_, _, _, _| {},
        );
        let signal = stop.clone();
        let commands = tx.clone();
        let handle = thread::spawn(move || {
            thread::sleep(Duration::from_millis(300));
            commands
                .send(super::super::seek::Command::Volume(10))
                .unwrap();
            commands.send(super::super::seek::Command::Pause).unwrap();
            thread::sleep(Duration::from_millis(80));
            signal.cancel();
        });
        let bytes = packet(48_000);
        struct Quiet {
            info: StreamInfo,
            once: Option<Vec<u8>>,
        }
        impl SegmentSource for Quiet {
            fn info(&self) -> &StreamInfo {
                &self.info
            }
            fn poll_segment(&mut self) -> Result<Next, ProbeError> {
                if let Some(bytes) = self.once.take() {
                    return event(bytes, false);
                }
                Ok(Next::Pending(Duration::from_millis(20)))
            }
        }
        let report = play_loop(
            Initial::Source(Quiet {
                info: info(60_000),
                once: Some(bytes),
            }),
            Some(30),
            stop,
            |_| {},
            move |pcm, maximum, _, _| {
                Ok(Watching {
                    frames: 0,
                    maximum,
                    rate: pcm.summary.sample_rate,
                    channels: pcm.summary.channels,
                    seen: record.clone(),
                })
            },
            Some(&mut control),
        )
        .unwrap();
        handle.join().unwrap();
        assert_eq!(report.reason, EndReason::Cancelled);
        let seen = seen.lock().unwrap();
        assert!(
            seen.iter().any(|(paused, volume)| *paused && *volume == 10),
            "pause and volume must be applied while waiting, saw {seen:?}"
        );
    }

    #[test]
    fn continuing_failure_kind_comes_from_the_error_and_keeps_the_cli_sentence() {
        let expired = PlaybackError::Source(ProbeError::At(
            "HLS音声取得",
            Box::new(ProbeError::AuthExpired),
        ));
        let reconnect = PlaybackError::Output(OutputError::ReconnectGaveUp);
        assert_eq!(notice_kind(&expired), Some(NoticeKind::AuthExpired));
        assert_eq!(
            continuing_notice_kind(&expired),
            Some(NoticeKind::AuthExpiredWhilePlaying)
        );
        assert_eq!(
            continuing_notice_kind(&PlaybackError::Source(ProbeError::AuthExpired)),
            Some(NoticeKind::AuthExpiredWhilePlaying)
        );
        assert_eq!(notice_kind(&reconnect), None);
        assert_eq!(
            continuing_notice_kind(&reconnect),
            Some(NoticeKind::ReconnectLimitWhilePlaying)
        );
        assert_eq!(
            continuing_notice_kind(&PlaybackError::ReconnectGaveUp),
            Some(NoticeKind::ReconnectLimitWhilePlaying)
        );
        assert_eq!(
            continuing_notice_kind(&PlaybackError::Source(ProbeError::At(
                "HLS一覧更新",
                Box::new(ProbeError::ReconnectGaveUp),
            ))),
            Some(NoticeKind::ReconnectLimitWhilePlaying)
        );
        assert_eq!(
            continuing_notice_kind(&PlaybackError::LoginRequired),
            Some(NoticeKind::LoginRequired)
        );
        assert_eq!(
            continuing_notice_kind(&PlaybackError::Output(OutputError::NoDevice)),
            None
        );
        assert!(expired.to_string().contains("再生を停止しました"));
        assert!(failure_notice(&expired).contains("再生を停止しました"));
        assert!(reconnect.to_string().contains("再生を停止しました"));
        assert!(failure_notice(&reconnect).contains("再生を停止しました"));
        assert!(!format!("{expired}").contains("再生は続いています"));
    }
    #[test]
    fn idle_preparation_failure_reports_the_stop_kind_not_the_continuing_sentence() {
        use super::super::seek::{Command, Controller, PlaybackRequest};
        for reconnect in [false, true] {
            let (tx, rx) = mpsc::channel();
            let (found, received) = mpsc::channel();
            let mut idle = info(1000);
            idle.starts_at -= chrono::Duration::hours(1);
            idle.ends_at -= chrono::Duration::hours(1);
            let stop = StopToken::default();
            let mut controller = Controller::with_reporter(
                rx,
                stop.clone(),
                move |_| {
                    Err(if reconnect {
                        PlaybackError::ReconnectGaveUp
                    } else {
                        PlaybackError::Source(ProbeError::At(
                            "HLS音声取得",
                            Box::new(ProbeError::AuthExpired),
                        ))
                    })
                },
                move |message, kind, prefix, _output| {
                    if message.contains("再生準備失敗") {
                        found.send((message, kind, prefix)).unwrap();
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
                let (message, kind, prefix) =
                    received.recv_timeout(Duration::from_secs(2)).unwrap();
                assert!(message.starts_with("再生準備失敗（現在の状態を維持）: "));
                assert!(message.contains("再生を停止しました"), "{message}");
                assert!(!message.contains("再生は続いています"), "{message}");
                if reconnect {
                    assert_eq!(kind, None);
                    assert_eq!(prefix, None);
                    assert!(message.contains(
                        "再接続の上限を超えたため再生を停止しました。番組を選び直して再生してください"
                    ));
                } else {
                    assert_eq!(kind, Some(NoticeKind::AuthExpired));
                    assert_eq!(prefix, Some("再生準備失敗（現在の状態を維持）"));
                    assert!(message.contains(
                        "認証期限切れのため再生を停止しました。aで再確認するか、loginで再ログインしてください"
                    ));
                }
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
            handle.join().unwrap();
        }
    }

    const WINDOW_LOST: &str =
        "通信が長く途切れたため、続きを再生できなくなりました。番組を選び直してください";
    const RECONNECT_LIMIT: &str =
        "再接続の上限を超えたため再生を停止しました。番組を選び直して再生してください";
    const AUTH_EXPIRED: &str =
        "認証期限切れのため再生を停止しました。aで再確認するか、loginで再ログインしてください";

    /// stderr of main's `エラー: {}` line, using `cli_error_body`.
    fn cli_stderr(error: &PlaybackError) -> String {
        format!(
            "エラー: {}",
            cli_error_body(error as &(dyn std::error::Error + 'static))
        )
    }

    fn assert_plain(error: PlaybackError, chain: &str) {
        assert_eq!(error.to_string(), chain);
        assert_eq!(failure_notice(&error), format!("再生停止: {chain}"));
        assert_eq!(cli_stderr(&error), format!("エラー: {chain}"));
    }

    fn assert_confirmed(error: PlaybackError, sentence: &str, chain: &str) {
        assert_eq!(failure_notice(&error), sentence);
        assert_eq!(error.to_string(), chain);
        let stderr = if chain == sentence {
            format!("エラー: {sentence}")
        } else {
            format!("エラー: {sentence}\n{chain}")
        };
        assert_eq!(cli_stderr(&error), stderr);
    }

    fn probe_code(error: &ProbeError) -> &'static str {
        match error {
            ProbeError::At(_, _) => "At",
            ProbeError::InvalidInput => "InvalidInput",
            ProbeError::InvalidPosition => "InvalidPosition",
            ProbeError::Network => "Network",
            ProbeError::Cancelled => "Cancelled",
            ProbeError::Http(_) => "Http",
            ProbeError::Authentication => "Authentication",
            ProbeError::AuthExpired => "AuthExpired",
            ProbeError::Denied => "Denied",
            ProbeError::NotFound => "NotFound",
            ProbeError::RateLimited => "RateLimited",
            ProbeError::TooLarge => "TooLarge",
            ProbeError::UnsafeUrl => "UnsafeUrl",
            ProbeError::InvalidAuth => "InvalidAuth",
            ProbeError::OutsideJapan => "OutsideJapan",
            ProbeError::InvalidXml => "InvalidXml",
            ProbeError::InvalidProgram => "InvalidProgram",
            ProbeError::Upcoming => "Upcoming",
            ProbeError::StillOnAir => "StillOnAir",
            ProbeError::Expired => "Expired",
            ProbeError::ContractRequired => "ContractRequired",
            ProbeError::OutsideArea => "OutsideArea",
            ProbeError::AvailabilityUnknown => "AvailabilityUnknown",
            ProbeError::Unavailable => "Unavailable",
            ProbeError::InvalidPlaylist => "InvalidPlaylist",
            ProbeError::WindowLost => "WindowLost",
            ProbeError::NoEndpoint => "NoEndpoint",
            ProbeError::ReconnectGaveUp => "ReconnectGaveUp",
        }
    }

    #[test]
    fn failure_text_changes_only_for_the_three_confirmed_messages() {
        use crate::api::auth::AuthError;
        use crate::player::output::OfferedOutputFormat;
        use crate::storage::StorageError;
        use std::collections::BTreeSet;
        let mut seen = BTreeSet::new();
        let mut plain_source = |error: ProbeError, text: &str| {
            assert!(seen.insert(probe_code(&error)), "duplicate probe leaf");
            assert_plain(PlaybackError::Source(error), &format!("配信取得: {text}"));
        };
        plain_source(
            ProbeError::InvalidInput,
            "局IDまたは日時が不正です（日時は日本時間のYYYYMMDDHHmmss）",
        );
        plain_source(
            ProbeError::InvalidPosition,
            "シーク位置は番組冒頭以上・放送枠末尾未満で指定してください",
        );
        plain_source(ProbeError::Network, "配信の検証中に通信に失敗しました");
        plain_source(ProbeError::Cancelled, "配信取得を取り消しました");
        plain_source(ProbeError::Http(503), "配信サーバーがHTTP 503を返しました");
        plain_source(
            ProbeError::Authentication,
            "配信認証が拒否されました。sessionで会員認証を確認してください",
        );
        plain_source(
            ProbeError::Denied,
            "配信サーバーがアクセスを拒否しました（契約・地域・配信条件の確認が必要です）",
        );
        plain_source(ProbeError::NotFound, "対象の番組または配信が見つかりません");
        plain_source(
            ProbeError::RateLimited,
            "アクセス制限中です。時間をおいて再実行してください",
        );
        plain_source(
            ProbeError::TooLarge,
            "応答または音声サンプルのサイズ上限を超えました",
        );
        plain_source(
            ProbeError::UnsafeUrl,
            "未対応の配信先またはリダイレクトです",
        );
        plain_source(
            ProbeError::InvalidAuth,
            "配信認証の応答が想定形式と異なります",
        );
        plain_source(ProbeError::OutsideJapan, "国内エリアを確認できませんでした");
        plain_source(
            ProbeError::InvalidXml,
            "局または配信設定XMLの形式が不正です",
        );
        plain_source(
            ProbeError::InvalidProgram,
            "指定時刻と番組の放送枠を対応付けできません",
        );
        plain_source(ProbeError::Upcoming, "放送前のため配信取得できません");
        plain_source(
            ProbeError::StillOnAir,
            "放送中のためタイムフリーの検証対象外です",
        );
        plain_source(ProbeError::Expired, "タイムフリーの候補期限を過ぎています");
        plain_source(
            ProbeError::ContractRequired,
            "対象の契約を確認できません（エリアフリーまたはタイムフリー30が必要です）",
        );
        plain_source(
            ProbeError::OutsideArea,
            "対象の契約を確認できません（エリアフリーまたはタイムフリー30が必要です）",
        );
        plain_source(
            ProbeError::AvailabilityUnknown,
            "番組の配信条件が未確認のため取得を中止しました",
        );
        plain_source(
            ProbeError::Unavailable,
            "番組情報でタイムフリー配信対象外とされています",
        );
        plain_source(
            ProbeError::InvalidPlaylist,
            "HLSプレイリストまたはサンプルの構造が不正・未対応です",
        );
        plain_source(
            ProbeError::NoEndpoint,
            "対象モードと地域に対応する配信先がありません",
        );
        assert!(seen.insert("WindowLost"));
        assert!(seen.insert("AuthExpired"));
        assert!(seen.insert("ReconnectGaveUp"));
        assert_eq!(seen.len(), 27, "one row per ProbeError leaf, saw {seen:?}");
        assert_confirmed(
            PlaybackError::Source(ProbeError::WindowLost),
            WINDOW_LOST,
            &format!("配信取得: {WINDOW_LOST}"),
        );
        assert_confirmed(
            PlaybackError::Source(ProbeError::AuthExpired),
            AUTH_EXPIRED,
            &format!("配信取得: {AUTH_EXPIRED}"),
        );
        assert_confirmed(
            PlaybackError::Source(ProbeError::ReconnectGaveUp),
            RECONNECT_LIMIT,
            &format!("配信取得: {RECONNECT_LIMIT}"),
        );
        for (stage, not_found_chain, network_chain, http_chain, playlist_chain) in [
            (
                "HLS一覧更新",
                "配信取得: HLS一覧更新: 対象の番組または配信が見つかりません",
                "配信取得: HLS一覧更新: 配信の検証中に通信に失敗しました",
                "配信取得: HLS一覧更新: 配信サーバーがHTTP 503を返しました",
                "配信取得: HLS一覧更新: HLSプレイリストまたはサンプルの構造が不正・未対応です",
            ),
            (
                "HLS初期化情報取得",
                "配信取得: HLS初期化情報取得: 対象の番組または配信が見つかりません",
                "配信取得: HLS初期化情報取得: 配信の検証中に通信に失敗しました",
                "配信取得: HLS初期化情報取得: 配信サーバーがHTTP 503を返しました",
                "配信取得: HLS初期化情報取得: HLSプレイリストまたはサンプルの構造が不正・未対応です",
            ),
            (
                "HLS音声取得",
                "配信取得: HLS音声取得: 対象の番組または配信が見つかりません",
                "配信取得: HLS音声取得: 配信の検証中に通信に失敗しました",
                "配信取得: HLS音声取得: 配信サーバーがHTTP 503を返しました",
                "配信取得: HLS音声取得: HLSプレイリストまたはサンプルの構造が不正・未対応です",
            ),
            (
                "別段階",
                "配信取得: 別段階: 対象の番組または配信が見つかりません",
                "配信取得: 別段階: 配信の検証中に通信に失敗しました",
                "配信取得: 別段階: 配信サーバーがHTTP 503を返しました",
                "配信取得: 別段階: HLSプレイリストまたはサンプルの構造が不正・未対応です",
            ),
        ] {
            assert_plain(
                PlaybackError::Source(ProbeError::At(stage, Box::new(ProbeError::NotFound))),
                not_found_chain,
            );
            assert_plain(
                PlaybackError::Source(ProbeError::At(stage, Box::new(ProbeError::Network))),
                network_chain,
            );
            assert_plain(
                PlaybackError::Source(ProbeError::At(stage, Box::new(ProbeError::Http(503)))),
                http_chain,
            );
            assert_plain(
                PlaybackError::Source(ProbeError::At(stage, Box::new(ProbeError::InvalidPlaylist))),
                playlist_chain,
            );
            assert_confirmed(
                PlaybackError::Source(ProbeError::At(stage, Box::new(ProbeError::WindowLost))),
                WINDOW_LOST,
                &format!("配信取得: {stage}: {WINDOW_LOST}"),
            );
            assert_confirmed(
                PlaybackError::Source(ProbeError::At(stage, Box::new(ProbeError::AuthExpired))),
                AUTH_EXPIRED,
                &format!("配信取得: {stage}: {AUTH_EXPIRED}"),
            );
            assert_confirmed(
                PlaybackError::Source(ProbeError::At(stage, Box::new(ProbeError::ReconnectGaveUp))),
                RECONNECT_LIMIT,
                &format!("配信取得: {stage}: {RECONNECT_LIMIT}"),
            );
        }
        assert_confirmed(
            PlaybackError::Source(ProbeError::At(
                "外側",
                Box::new(ProbeError::At(
                    "HLS音声取得",
                    Box::new(ProbeError::AuthExpired),
                )),
            )),
            AUTH_EXPIRED,
            &format!("配信取得: 外側: HLS音声取得: {AUTH_EXPIRED}"),
        );
        assert_plain(
            PlaybackError::LoginRequired,
            "未ログインです。loginでログインしてください",
        );
        assert_plain(
            PlaybackError::EarlyEnd,
            "放送枠末尾より前に配信が終了しました",
        );
        assert_plain(
            PlaybackError::SourceStalled,
            "30秒間新しいセグメントを確認できないため停止しました",
        );
        assert_confirmed(
            PlaybackError::ReconnectGaveUp,
            RECONNECT_LIMIT,
            RECONNECT_LIMIT,
        );
        assert_plain(PlaybackError::Worker, "再生ワーカーが予期せず終了しました");
        assert_plain(
            PlaybackError::Auth(AuthError::InvalidCredentials),
            "ログインに失敗しました。メールアドレスとパスワードを確認してください",
        );
        assert_plain(
            PlaybackError::Auth(AuthError::SessionExpired),
            "セッションが失効しました。loginで再ログインしてください",
        );
        assert_plain(
            PlaybackError::Auth(AuthError::Network),
            "認証サーバーと通信できません",
        );
        assert_plain(
            PlaybackError::Auth(AuthError::Cancelled),
            "認証処理を取り消しました",
        );
        assert_plain(
            PlaybackError::Auth(AuthError::Http(401)),
            "認証サーバーがHTTP 401を返しました",
        );
        assert_plain(
            PlaybackError::Auth(AuthError::RateLimited),
            "認証サーバーのアクセス制限中です。時間をおいて再実行してください",
        );
        assert_plain(
            PlaybackError::Auth(AuthError::InvalidResponse),
            "認証応答が想定形式と異なります",
        );
        assert_plain(
            PlaybackError::Auth(AuthError::ResponseTooLarge),
            "認証応答のサイズ上限を超えました",
        );
        assert_plain(
            PlaybackError::Auth(AuthError::InvalidInput),
            "メールアドレスまたはパスワードが空です",
        );
        assert_plain(
            PlaybackError::Storage(StorageError::Io(std::io::ErrorKind::PermissionDenied)),
            "セッションファイルの操作に失敗しました (PermissionDenied)",
        );
        assert_plain(
            PlaybackError::Storage(StorageError::Invalid),
            "セッションファイルが破損しています",
        );
        assert_plain(
            PlaybackError::Storage(StorageError::UnsupportedVersion),
            "未対応のセッション保存形式です",
        );
        assert_plain(
            PlaybackError::Storage(StorageError::UnsafePermissions),
            "セッションのディレクトリは0700、ファイルは0600にしてください",
        );
        assert_plain(
            PlaybackError::Storage(StorageError::UnsafePath),
            "セッション保存先にシンボリックリンクや通常ファイル以外は使用できません",
        );
        assert_plain(
            PlaybackError::Storage(StorageError::NoDataDirectory),
            "データ保存先を解決できません。--data-dirを指定してください",
        );
        assert_plain(
            PlaybackError::Decode(AacError::Cancelled),
            "デコード: AACデコードを取り消しました",
        );
        assert_plain(
            PlaybackError::Decode(AacError::TooLarge),
            "デコード: AACセグメントのサイズ上限を超えました",
        );
        assert_plain(
            PlaybackError::Decode(AacError::InvalidMetadata),
            "デコード: AACに付加されたID3の形式が不正です",
        );
        assert_plain(
            PlaybackError::Decode(AacError::InvalidContainer),
            "デコード: AACコンテナの時刻またはデータ構造が不正です",
        );
        assert_plain(
            PlaybackError::Decode(AacError::UnsupportedContainer),
            "デコード: AACコンテナの構成が未対応です",
        );
        assert_plain(
            PlaybackError::Decode(AacError::InvalidConfig),
            "デコード: AACの設定情報（ASC）が不正・未対応です",
        );
        assert_plain(
            PlaybackError::Decode(AacError::InvalidPacket),
            "デコード: AACアクセスユニットに余分なデータがあります",
        );
        assert_plain(
            PlaybackError::Decode(AacError::InvalidHeader),
            "デコード: ADTSヘッダーが不正です",
        );
        assert_plain(
            PlaybackError::Decode(AacError::TruncatedFrame),
            "デコード: ADTSフレームが途中で切れています",
        );
        assert_plain(
            PlaybackError::Decode(AacError::Empty),
            "デコード: AAC音声フレームがありません",
        );
        assert_plain(
            PlaybackError::Decode(AacError::TooManyPackets),
            "デコード: AACセグメントのパケット数上限を超えました",
        );
        assert_plain(
            PlaybackError::Decode(AacError::UnsupportedTransport),
            "デコード: CRC付きまたは複数raw blockのADTSは検証対象外です",
        );
        assert_plain(
            PlaybackError::Decode(AacError::UnsupportedCodec),
            "デコード: AACの構成がデコーダーで未対応です",
        );
        assert_plain(
            PlaybackError::Decode(AacError::DecodeFailed),
            "デコード: AACパケットのデコードに失敗しました",
        );
        assert_plain(
            PlaybackError::Decode(AacError::FormatChanged),
            "デコード: AACの構成が変わりました。新しいデコーダーで再開してください",
        );
        assert_plain(
            PlaybackError::Decode(AacError::InvalidPcm),
            "デコード: デコードしたPCMの形式またはサンプル値が不正です",
        );
        assert_plain(
            PlaybackError::Decode(AacError::DeclaredSbrMissing),
            "デコード: HLSはHE-AACを宣言していますが、このサンプルではSBR再構成を確認できません",
        );
        assert_plain(
            PlaybackError::Decode(AacError::SbrNotReconstructed),
            "デコード: SBRデータを検出しましたが出力がコアレートのままです",
        );
        assert_plain(
            PlaybackError::Output(OutputError::InvalidPcm),
            "出力用PCMまたは再生時間が不正です",
        );
        assert_plain(
            PlaybackError::Output(OutputError::NoDevice),
            "既定の音声出力機器がありません",
        );
        assert_plain(
            PlaybackError::Output(OutputError::DeviceInfo),
            "音声出力機器の情報を取得できません",
        );
        assert_plain(
            PlaybackError::Output(OutputError::UnsupportedFormat),
            "出力中のサンプルレートまたはチャンネル数が、開いた音声と一致しません",
        );
        assert_plain(
            PlaybackError::Output(OutputError::IncompatibleOutput {
                requested_rate: 48_000,
                offered: vec![OfferedOutputFormat {
                    channels: 2,
                    min_sample_rate: 44_100,
                    max_sample_rate: 44_100,
                    sample_format: "f32",
                }],
            }),
            "必要な形式は48000Hzのmonoまたはstereoのf32・i16・i32です。機器の対応範囲: 44100Hz・f32・2ch",
        );
        assert_plain(
            PlaybackError::Output(OutputError::Device),
            "音声出力機器の開始・実行に失敗しました",
        );
        assert_plain(
            PlaybackError::Output(OutputError::Stalled),
            "音声コールバックが進まないため出力を停止しました。スリープ復帰・機器変更後は出力先を確認し、番組を選び直して再生してください",
        );
        assert_confirmed(
            PlaybackError::Output(OutputError::ReconnectGaveUp),
            RECONNECT_LIMIT,
            RECONNECT_LIMIT,
        );
        assert_plain(
            PlaybackError::Output(OutputError::ProbeTimeout),
            "無音出力診断の実時間上限を超えたため停止しました",
        );
        assert_plain(
            PlaybackError::Output(OutputError::Deadline),
            "番組終了：放送終了日時に達したため音声を停止しました",
        );
        let unrelated = std::io::Error::other("unchanged");
        assert_eq!(cli_error_body(&unrelated), "unchanged");
    }
}
