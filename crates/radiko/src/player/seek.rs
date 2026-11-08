//! 最新要求を優先する再生準備。準備失敗時も現在の音声出力を維持する。
use super::{
    output::StopToken,
    playback::{PlaybackError, Ready, prepare_ready},
};
use crate::api::stream::{
    ProbeMode,
    playback::{SegmentSource, StreamInfo},
};
use chrono::{DateTime, FixedOffset, Utc};
#[cfg(test)]
use std::sync::Condvar;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, TrySendError},
    },
    thread,
    time::Duration,
};

/// 対象を毎回解決し直し、署名付きURLやデコード済み音声を再利用しない。
#[derive(Clone, Debug)]
pub struct PlaybackRequest {
    pub station: String,
    pub mode: ProbeMode,
    pub cursor: DateTime<FixedOffset>,
    pub position: Duration,
}
impl PlaybackRequest {
    fn matches(&self, info: &StreamInfo) -> bool {
        info.station == self.station
            && info.mode == self.mode
            && info.starts_at <= self.cursor
            && self.cursor < info.ends_at
            && info.position == self.position
            && (info.ends_at - info.starts_at)
                .to_std()
                .is_ok_and(|length| self.position < length)
    }
}
#[derive(Debug)]
pub enum Command {
    Select(PlaybackRequest),
    Seek(Duration),
    SeekSlot {
        key: crate::domain::ProgramKey,
        position: Duration,
    },
    Relative(i64),
    Stop,
    Pause,
    Resume,
    Volume(u32),
    Status,
}
struct Job {
    id: u64,
    request: PlaybackRequest,
    expected: Option<StreamInfo>,
    stop: StopToken,
}
struct Completed {
    id: u64,
    result: Result<Ready, PlaybackError>,
    /// The prepared stream did not match the request or the current slot.
    /// The error stays `InvalidPosition` so Display and CLI text stay the same.
    /// While playback continues, the TUI must not call that a user position.
    slot_mismatch: bool,
}
type Reporter = dyn FnMut(
        String,
        Option<super::playback::NoticeKind>,
        Option<&'static str>,
        Option<super::playback::OutputFailure>,
    ) + Send;
/// 準備結果を載せるたびに試験を起こす。リリースビルドには含めない。
#[cfg(test)]
struct PrepNotify {
    lock: Mutex<()>,
    seq: AtomicU64,
    cv: Condvar,
}
#[cfg(test)]
impl PrepNotify {
    fn new() -> Self {
        Self {
            lock: Mutex::new(()),
            seq: AtomicU64::new(0),
            cv: Condvar::new(),
        }
    }
    fn publish(&self) {
        let _guard = self
            .lock
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        self.seq.fetch_add(1, Ordering::Release);
        self.cv.notify_all();
    }
    fn seq(&self) -> u64 {
        self.seq.load(Ordering::Acquire)
    }
    fn wait_after(&self, seen: u64, deadline: std::time::Instant) -> bool {
        let mut guard = self
            .lock
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        loop {
            if self.seq.load(Ordering::Acquire) != seen {
                return true;
            }
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return false;
            }
            let (next, status) = self
                .cv
                .wait_timeout(guard, remaining)
                .unwrap_or_else(|poison| poison.into_inner());
            guard = next;
            if status.timed_out() && self.seq.load(Ordering::Acquire) == seen {
                return false;
            }
        }
    }
}
pub struct Controller {
    commands: Receiver<Command>,
    preparation_worker: Option<thread::JoinHandle<()>>,
    reporter: Box<Reporter>,
    pub(super) notification_interval: Duration,
    latest: Arc<Mutex<Option<Job>>>,
    results: Receiver<Completed>,
    generation: Arc<AtomicU64>,
    stop: StopToken,
    pending: Option<StopToken>,
    pub(super) pending_target: Option<PlaybackRequest>,
    pub(super) changed: bool,
    pub(super) paused: bool,
    pub(super) volume: u32,
    /// Set by the play loop from whether an output is open. Preparation before that is not playback.
    playback_active: bool,
    /// TUI selections use the schedule slot start as `cursor`. A stream that
    /// only contains that instant is another slot. CLI cursors are the
    /// requested time, so this stays off for the CLI controller.
    compare_schedule_start: Arc<AtomicBool>,
    /// Last completed job's slot-mismatch bit. Tests read it after the play loop returns.
    #[cfg(test)]
    last_slot_mismatch: AtomicBool,
    #[cfg(test)]
    prepare_stall_ms: Arc<AtomicU64>,
    #[cfg(test)]
    prep_notify: Arc<PrepNotify>,
}
impl Controller {
    pub fn new(
        commands: Receiver<Command>,
        stop: StopToken,
        factory: impl Fn(PlaybackRequest) -> Result<Box<dyn SegmentSource>, PlaybackError>
        + Send
        + 'static,
    ) -> Self {
        let mut controller = Self::with_reporter(
            commands,
            stop,
            factory,
            |message, _kind, _prefix, _output| {
                crate::stderr::write_line(format_args!("{message}"));
            },
        );
        controller.notification_interval = Duration::from_secs(5);
        controller
    }
    /// CLI reporting cadence with cancellation-aware preparation.
    pub fn new_cancellable(
        commands: Receiver<Command>,
        stop: StopToken,
        factory: impl Fn(PlaybackRequest, StopToken) -> Result<Box<dyn SegmentSource>, PlaybackError>
        + Send
        + 'static,
    ) -> Self {
        let mut controller = Self::with_cancellable_reporter(
            commands,
            stop,
            factory,
            |message, _kind, _prefix, _output| {
                crate::stderr::write_line(format_args!("{message}"));
            },
        );
        controller.notification_interval = Duration::from_secs(5);
        controller
    }
    pub fn with_reporter(
        commands: Receiver<Command>,
        stop: StopToken,
        factory: impl Fn(PlaybackRequest) -> Result<Box<dyn SegmentSource>, PlaybackError>
        + Send
        + 'static,
        reporter: impl FnMut(
            String,
            Option<crate::player::playback::NoticeKind>,
            Option<&'static str>,
            Option<crate::player::playback::OutputFailure>,
        ) + Send
        + 'static,
    ) -> Self {
        Self::with_cancellable_reporter(
            commands,
            stop,
            move |request, _| factory(request),
            reporter,
        )
    }
    pub fn with_cancellable_reporter(
        commands: Receiver<Command>,
        stop: StopToken,
        factory: impl Fn(PlaybackRequest, StopToken) -> Result<Box<dyn SegmentSource>, PlaybackError>
        + Send
        + 'static,
        reporter: impl FnMut(
            String,
            Option<crate::player::playback::NoticeKind>,
            Option<&'static str>,
            Option<crate::player::playback::OutputFailure>,
        ) + Send
        + 'static,
    ) -> Self {
        let latest = Arc::new(Mutex::new(None::<Job>));
        let generation = Arc::new(AtomicU64::new(0));
        let (tx, results) = mpsc::sync_channel(1);
        let stall_ms = Arc::new(AtomicU64::new(30_000));
        #[cfg(test)]
        let prepare_stall_ms = stall_ms.clone();
        #[cfg(test)]
        let prep_notify = Arc::new(PrepNotify::new());
        #[cfg(test)]
        let prep_notify_worker = Arc::clone(&prep_notify);
        let compare_schedule_start = Arc::new(AtomicBool::new(false));
        let (mailbox, ids, shutdown, stall_ms, compare_schedule_start_flag) = (
            latest.clone(),
            generation.clone(),
            stop.clone(),
            stall_ms,
            compare_schedule_start.clone(),
        );
        let preparation_worker = thread::spawn(move || {
            while !shutdown.is_cancelled() {
                let job = match mailbox.lock() {
                    Ok(mut slot) => slot.take(),
                    Err(_) => break,
                };
                let Some(job) = job else {
                    thread::sleep(Duration::from_millis(5));
                    continue;
                };
                if job.stop.is_cancelled() {
                    continue;
                }
                let (result, slot_mismatch) = match factory(job.request.clone(), job.stop.clone()) {
                    Err(error) => (Err(error), false),
                    Ok(source) => {
                        let info = source.info();
                        let same_slot = job.expected.as_ref().is_none_or(|expected| {
                            expected.station == info.station
                                && expected.starts_at == info.starts_at
                                && expected.ends_at == info.ends_at
                                && expected.mode == info.mode
                        });
                        // A live slot can move, or the source can disagree with the
                        // request, without the user naming a position. Keep
                        // `InvalidPosition` for Display and CLI. The flag tells the
                        // TUI this is not a user position error.
                        // `starts_at != cursor` is the TUI schedule slot versus the
                        // stream slot. CLI cursors are the requested time, so the
                        // comparison runs only when the app service opts in.
                        let schedule_start = compare_schedule_start_flag.load(Ordering::Acquire)
                            && info.starts_at != job.request.cursor;
                        if !job.request.matches(info) || !same_slot || schedule_start {
                            (
                                Err(PlaybackError::Source(
                                    crate::api::stream::ProbeError::InvalidPosition,
                                )),
                                true,
                            )
                        } else if job.stop.is_cancelled() {
                            (Err(PlaybackError::Worker), false)
                        } else {
                            let stall = Duration::from_millis(stall_ms.load(Ordering::Acquire));
                            (prepare_ready(source, job.stop.clone(), stall), false)
                        }
                    }
                };
                // `prepare_ready` drops its worker on failure. That drop cancels a
                // child token, not `job.stop`. A cancelled `job.stop` is a user
                // cancel (another selection, pause, or stop) and is not reported.
                if job.id != ids.load(Ordering::Acquire) || job.stop.is_cancelled() {
                    continue;
                }
                let mut value = Completed {
                    id: job.id,
                    result,
                    slot_mismatch,
                };
                loop {
                    if shutdown.is_cancelled() || job.id != ids.load(Ordering::Acquire) {
                        break;
                    }
                    match tx.try_send(value) {
                        Ok(()) => {
                            #[cfg(test)]
                            prep_notify_worker.publish();
                            break;
                        }
                        Err(TrySendError::Disconnected(_)) => return,
                        Err(TrySendError::Full(v)) => {
                            value = v;
                            thread::sleep(Duration::from_millis(5));
                        }
                    }
                }
            }
        });
        Self {
            commands,
            preparation_worker: Some(preparation_worker),
            reporter: Box::new(reporter),
            notification_interval: Duration::from_millis(250),
            latest,
            results,
            generation,
            stop,
            pending: None,
            pending_target: None,
            changed: true,
            paused: false,
            volume: 25,
            playback_active: false,
            compare_schedule_start,
            #[cfg(test)]
            last_slot_mismatch: AtomicBool::new(false),
            #[cfg(test)]
            prepare_stall_ms,
            #[cfg(test)]
            prep_notify,
        }
    }
    /// Whether the latest completed preparation was an internal slot mismatch.
    #[cfg(test)]
    pub(crate) fn last_slot_mismatch(&self) -> bool {
        self.last_slot_mismatch.load(Ordering::Acquire)
    }
    /// The TUI identifies a slot by its schedule start. Enable the
    /// `starts_at != cursor` mismatch check. The CLI controller leaves this off.
    pub(crate) fn reject_contained_schedule_start(&self) {
        self.compare_schedule_start.store(true, Ordering::Release);
    }
    /// Shortens only the silence limit inside `prepare_ready`. Production stays at 30 seconds.
    #[cfg(test)]
    pub(crate) fn set_prepare_stall(&self, stall: Duration) {
        let millis = u64::try_from(stall.as_millis()).unwrap_or(u64::MAX);
        self.prepare_stall_ms.store(millis, Ordering::Release);
    }
    /// 準備結果がチャネルに載った回数。試験が結果を待つのに使う。
    #[cfg(test)]
    fn preparation_seq(&self) -> u64 {
        self.prep_notify.seq()
    }
    /// 新しい準備結果が載るまで待つ。期限はハング検出であり、成功時の性能条件ではない。
    #[cfg(test)]
    fn wait_preparation_after(&self, seen: u64, deadline: std::time::Instant) -> bool {
        self.prep_notify.wait_after(seen, deadline)
    }
    /// Call only from the owning background control thread, after output has been stopped.
    /// Prevents a replacement session from accumulating workers blocked in old HTTP requests.
    pub fn finish_preparation(&mut self) {
        self.stop.cancel();
        if let Some(worker) = self.preparation_worker.take() {
            let _ = worker.join();
        }
    }
    pub(super) fn report(&mut self, message: String) {
        (self.reporter)(message, None, None, None);
    }
    /// The play loop sets this from the open output. Idle preparation is not playback.
    pub(super) fn note_playback_active(&mut self, active: bool) {
        self.playback_active = active;
    }
    /// CLI text stays `{prefix}: {error}`. The kind and prefix let the TUI replace that error.
    /// The continuing sentence is only for a failure while output is already open.
    pub(super) fn report_failure(&mut self, prefix: &'static str, error: &PlaybackError) {
        self.report_failure_with(prefix, error, None);
    }
    /// `kind_override` is the path decision. Display and the `{prefix}: {error}` CLI line stay.
    fn report_failure_with(
        &mut self,
        prefix: &'static str,
        error: &PlaybackError,
        kind_override: Option<crate::player::playback::NoticeKind>,
    ) {
        let kind = kind_override.or(if self.playback_active {
            crate::player::playback::continuing_notice_kind(error)
        } else {
            crate::player::playback::notice_kind(error)
        });
        // The prefix is sent even when the kind is absent. The CLI line stays
        // `{prefix}: {error}`. The TUI uses the prefix to treat an idle,
        // kind-less failure as an error without reading the sentence.
        let shown_prefix = Some(prefix);
        let output = crate::player::playback::output_failure(error);
        (self.reporter)(format!("{prefix}: {error}"), kind, shown_prefix, output);
    }
    pub(super) fn poll(&mut self, info: &StreamInfo, current: Duration) -> Option<Ready> {
        // 結果より先に操作を処理し、配送済みの古い準備結果も採用しない。
        for command in self.commands.try_iter().take(32).collect::<Vec<_>>() {
            self.changed = true;
            let resuming = matches!(command, Command::Resume);
            let selection = if let Command::Select(ref target) = command {
                Some(target.clone())
            } else {
                None
            };
            let position = match command {
                Command::Status => continue,
                Command::Select(ref target) => Some(target.position),
                Command::Volume(value) => {
                    self.volume = value.min(100);
                    continue;
                }
                Command::Pause => {
                    self.paused = true;
                    self.pending_target = None;
                    self.generation.fetch_add(1, Ordering::AcqRel);
                    if let Some(old) = self.pending.take() {
                        old.cancel();
                    }
                    self.report("一時停止".into());
                    continue;
                }
                Command::Resume => {
                    if !self.paused {
                        continue;
                    }
                    Some(if info.mode == ProbeMode::Live {
                        Duration::ZERO
                    } else {
                        current
                    })
                }
                Command::Stop => {
                    self.stop.cancel();
                    continue;
                }
                Command::SeekSlot { key, position } => {
                    if info.station != key.station_id
                        || info.starts_at != key.starts_at
                        || info.mode != ProbeMode::TimeFree
                    {
                        self.report("位置指定を取消: 再生対象が変わりました（再生を継続）".into());
                        continue;
                    }
                    Some(position)
                }
                Command::Seek(position) => Some(position),
                Command::Relative(seconds) => {
                    if seconds < 0 {
                        Some(current.saturating_sub(Duration::from_secs(seconds.unsigned_abs())))
                    } else {
                        current.checked_add(Duration::from_secs(seconds as u64))
                    }
                }
            };
            let request = if let Some(target) = selection.as_ref() {
                if crate::api::stream::validate_station(&target.station).is_err()
                    || target.position > Duration::from_secs(86400)
                    || (target.mode == ProbeMode::Live && target.position != Duration::ZERO)
                    || target.cursor > Utc::now()
                {
                    self.report("切替不可: 局・日時・開始位置を確認してください".into());
                    continue;
                }
                target.clone()
            } else {
                let valid = (info.ends_at - info.starts_at)
                    .to_std()
                    .ok()
                    .zip(position)
                    .filter(|(length, p)| {
                        (resuming || info.mode == ProbeMode::TimeFree) && p < length
                    });
                let Some((_, position)) = valid else {
                    self.report(
                        "シーク不可: タイムフリーの放送枠内を指定してください（再生を継続）".into(),
                    );
                    continue;
                };
                PlaybackRequest {
                    station: info.station.clone(),
                    mode: info.mode,
                    cursor: info.starts_at,
                    position,
                }
            };
            if let Some(old) = self.pending.take() {
                old.cancel();
            }
            let id = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
            let token = self.stop.child();
            self.pending = Some(token.clone());
            self.pending_target = Some(request.clone());
            match self.latest.lock() {
                Ok(mut slot) => {
                    *slot = Some(Job {
                        id,
                        request: request.clone(),
                        expected: if selection.is_some() {
                            None
                        } else {
                            Some(info.clone())
                        },
                        stop: token,
                    })
                }
                Err(_) => {
                    if let Some(pending) = self.pending.take() {
                        pending.cancel();
                    }
                    self.pending_target = None;
                    (self.reporter)(
                        "再生準備に失敗しました（現在の状態を維持）".into(),
                        None,
                        None,
                        None,
                    );
                    continue;
                }
            }
            if self.paused {
                self.report(format!(
                    "再生準備中: {} / {:?} / {:.3}秒（一時停止を維持）",
                    request.station,
                    request.mode,
                    request.position.as_secs_f64()
                ));
            } else {
                self.report(format!(
                    "再生準備中: {} / {:?} / {:.3}秒（現在の再生を継続）",
                    request.station,
                    request.mode,
                    request.position.as_secs_f64()
                ));
            }
        }
        while let Ok(completed) = self.results.try_recv() {
            if completed.id != self.generation.load(Ordering::Acquire) {
                continue;
            }
            self.changed = true;
            self.pending_target = None;
            match completed.result {
                Ok(ready) => {
                    #[cfg(test)]
                    self.last_slot_mismatch
                        .store(completed.slot_mismatch, Ordering::Release);
                    self.pending = None;
                    return Some(ready);
                }
                Err(error) => {
                    #[cfg(test)]
                    self.last_slot_mismatch
                        .store(completed.slot_mismatch, Ordering::Release);
                    let kind_override = (completed.slot_mismatch && self.playback_active)
                        .then_some(crate::player::playback::NoticeKind::StreamFailedWhilePlaying);
                    self.report_failure_with(
                        "再生準備失敗（現在の状態を維持）",
                        &error,
                        kind_override,
                    );
                }
            }
            self.pending = None;
        }
        None
    }
}
impl Drop for Controller {
    fn drop(&mut self) {
        self.stop.cancel();
        if let Some(pending) = &self.pending {
            pending.cancel();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::stream::{
        ProbeError,
        playback::{Next, Segment},
    };
    use std::panic::AssertUnwindSafe;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;
    /// ハング検出の上限。成功時はイベントが着き次第進み、この時間は性能条件ではない。
    /// 同期イベントが来ないときだけ失敗させ、チャネルの無限待ちにはしない。
    const HANG_WATCHDOG: Duration = Duration::from_secs(20);
    struct Source {
        info: StreamInfo,
        bytes: Option<Vec<u8>>,
    }
    impl SegmentSource for Source {
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
                None => Next::Pending(Duration::from_secs(1)),
            })
        }
    }
    fn info() -> StreamInfo {
        let start = chrono::DateTime::parse_from_rfc3339("2026-10-15T12:00:00+09:00").unwrap();
        StreamInfo {
            title: Some("合成テスト番組".into()),
            station: "TBS".into(),
            mode: ProbeMode::TimeFree,
            starts_at: start,
            ends_at: start + chrono::Duration::hours(1),
            position: Duration::ZERO,
        }
    }
    fn bytes() -> Vec<u8> {
        use rusty_aac::{AacEncoder, AacEncoderConfig, AdtsHeader, write_adts_header};
        let mut encoder = AacEncoder::new(AacEncoderConfig::default());
        let samples: Vec<f32> = (0..2048).map(|i| (i as f32 * 0.05).sin() * 0.1).collect();
        encoder.push_pcm(&samples, 1, 24_000).unwrap();
        encoder.finish();
        let p = encoder.next_packet().unwrap();
        let mut packet = write_adts_header(&AdtsHeader {
            object_type: 2,
            sample_rate: 24_000,
            channels: 1,
            frame_length: p.data.len() + 7,
            header_len: 7,
        });
        packet.extend(p.data);
        packet.repeat(50)
    }
    fn target(station: &str, start: chrono::DateTime<chrono::FixedOffset>) -> PlaybackRequest {
        PlaybackRequest {
            station: station.into(),
            mode: ProbeMode::TimeFree,
            cursor: start,
            position: Duration::ZERO,
        }
    }
    fn recv_within<T>(rx: &mpsc::Receiver<T>, bound: Duration, phase: &str) -> T {
        match rx.recv_timeout(bound) {
            Ok(value) => value,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                panic!("synchronization event missing: {phase} (hang watchdog {bound:?})")
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("synchronization channel disconnected: {phase}")
            }
        }
    }
    fn recv_phase<T>(rx: &mpsc::Receiver<T>, phase: &str) -> T {
        recv_within(rx, HANG_WATCHDOG, phase)
    }
    /// 試験側が送信端を落としたときは false。イベントが来ないときだけハング上限で失敗する。
    fn released(rx: &mpsc::Receiver<()>, phase: &str) -> bool {
        match rx.recv_timeout(HANG_WATCHDOG) {
            Ok(()) => true,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                panic!("synchronization event missing: {phase} (hang watchdog {HANG_WATCHDOG:?})")
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => false,
        }
    }
    fn wait_ready(control: &mut Controller, current: &StreamInfo) -> Ready {
        let deadline = Instant::now() + HANG_WATCHDOG;
        loop {
            let seen = control.preparation_seq();
            if let Some(ready) = control.poll(current, Duration::ZERO) {
                return ready;
            }
            if !control.wait_preparation_after(seen, deadline) {
                panic!(
                    "synchronization event missing: preparation result (hang watchdog {HANG_WATCHDOG:?})"
                );
            }
        }
    }
    fn wait_pending_cleared(control: &mut Controller, current: &StreamInfo) {
        let deadline = Instant::now() + HANG_WATCHDOG;
        loop {
            let seen = control.preparation_seq();
            let _ = control.poll(current, Duration::ZERO);
            if control.pending.is_none() {
                return;
            }
            if !control.wait_preparation_after(seen, deadline) {
                panic!(
                    "synchronization event missing: preparation finished (hang watchdog {HANG_WATCHDOG:?})"
                );
            }
        }
    }
    #[test]
    fn missing_synchronization_event_fails_inside_the_hang_watchdog() {
        let (tx, rx) = mpsc::channel::<()>();
        let bound = Duration::from_millis(200);
        let began = Instant::now();
        let panicked = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let _hold_sender = &tx;
            recv_within(&rx, bound, "fixture phase");
        }));
        let elapsed = began.elapsed();
        assert!(
            panicked.is_err(),
            "a missing synchronization event must fail"
        );
        assert!(
            elapsed + Duration::from_millis(50) >= bound,
            "watchdog returned early: {elapsed:?} bound {bound:?}"
        );
        assert!(
            elapsed < bound + Duration::from_secs(1),
            "watchdog exceeded its finite bound: {elapsed:?} bound {bound:?}"
        );
    }
    #[test]
    fn status_only_requests_a_notification_without_fetching_or_cancelling() {
        let (tx, rx) = mpsc::channel();
        let stop = StopToken::default();
        let mut control = Controller::new(rx, stop.clone(), |_| panic!("status must not fetch"));
        control.changed = false;
        tx.send(Command::Status).unwrap();
        assert!(control.poll(&info(), Duration::ZERO).is_none());
        assert!(control.changed);
        assert!(control.pending_target.is_none());
        assert!(!control.paused);
        assert!(!stop.is_cancelled());
        control.finish_preparation();
    }
    #[test]
    fn selected_station_and_split_slot_become_the_target_of_following_seek() {
        let (tx, rx) = mpsc::channel();
        let (sent, received) = mpsc::channel();
        let pcm = bytes();
        let mut control = Controller::new(rx, StopToken::default(), move |request| {
            sent.send(request.clone()).unwrap();
            let mut selected = info();
            selected.station = request.station;
            selected.starts_at = request.cursor;
            selected.ends_at = request.cursor + chrono::Duration::hours(1);
            selected.position = request.position;
            Ok(Box::new(Source {
                info: selected,
                bytes: Some(pcm.clone()),
            }))
        });
        tx.send(Command::Volume(45)).unwrap();
        tx.send(Command::Pause).unwrap();
        let later = info().starts_at + chrono::Duration::hours(1);
        tx.send(Command::Select(target("OBS", later))).unwrap();
        let ready = wait_ready(&mut control, &info());
        assert_eq!(ready.info.station, "OBS");
        assert_eq!(ready.info.starts_at, later);
        assert!(control.paused); // Output creation must succeed before clearing pause.
        assert_eq!(control.volume, 45);
        recv_phase(&received, "selected station request");
        tx.send(Command::Seek(Duration::from_secs(15))).unwrap();
        let next = wait_ready(&mut control, &ready.info);
        let request = recv_phase(&received, "following seek request");
        assert_eq!(request.station, "OBS");
        assert_eq!(request.cursor, later);
        assert_eq!(next.info.position, Duration::from_secs(15));
        control.finish_preparation();
    }
    #[test]
    fn selecting_twice_discards_old_station_even_when_preparation_finishes_late() {
        let (tx, rx) = mpsc::channel();
        let (entered, seen) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let pcm = bytes();
        let mut control = Controller::new(rx, StopToken::default(), move |request| {
            if request.station == "OBS" {
                entered.send(()).expect("station observation channel open");
                if !released(&gate, "old station preparation release") {
                    return Err(PlaybackError::Source(ProbeError::Cancelled));
                }
            }
            let mut selected = info();
            selected.station = request.station;
            Ok(Box::new(Source {
                info: selected,
                bytes: Some(pcm.clone()),
            }))
        });
        tx.send(Command::Select(target("OBS", info().starts_at)))
            .unwrap();
        control.poll(&info(), Duration::ZERO);
        recv_phase(&seen, "old station preparation entered");
        let old = control.pending.as_ref().unwrap().clone();
        tx.send(Command::Select(target("QRR", info().starts_at)))
            .unwrap();
        control.poll(&info(), Duration::ZERO);
        assert!(
            old.is_cancelled(),
            "a newer selection must cancel the previous preparation"
        );
        release.send(()).unwrap();
        let ready = wait_ready(&mut control, &info());
        assert_eq!(ready.info.station, "QRR");
        assert!(!ready.worker.stop.is_cancelled());
        control.finish_preparation();
    }
    #[test]
    fn invalid_selection_never_fetches_and_wrong_slot_response_is_not_decoded() {
        let (tx, rx) = mpsc::channel();
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let stop = StopToken::default();
        let mut control = Controller::new(rx, stop.clone(), move |_| {
            count.fetch_add(1, Ordering::SeqCst);
            // A mismatching response must be rejected before polling/decoding this source.
            Ok(Box::new(Source {
                info: info(),
                bytes: Some(vec![0]),
            }))
        });
        tx.send(Command::Select(target("../OBS", info().starts_at)))
            .unwrap();
        let mut future = target("OBS", info().starts_at);
        future.cursor = Utc::now().fixed_offset() + chrono::Duration::days(1);
        tx.send(Command::Select(future)).unwrap();
        control.poll(&info(), Duration::ZERO);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        tx.send(Command::Select(target("TBS", info().ends_at)))
            .unwrap();
        control.poll(&info(), Duration::ZERO);
        wait_pending_cleared(&mut control, &info());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(!stop.is_cancelled());
        control.finish_preparation();
    }
    #[test]
    fn pause_invalidates_pending_seek_and_resume_uses_frozen_or_latest_position() {
        for mode in [ProbeMode::TimeFree, ProbeMode::Live] {
            let (tx, rx) = mpsc::channel();
            let (seen_tx, seen_rx) = mpsc::channel();
            let (release, gate) = mpsc::channel();
            let stop = StopToken::default();
            let mut control = Controller::new(rx, stop.clone(), move |request| {
                seen_tx
                    .send((request.position, request.cursor))
                    .expect("resume observation channel open");
                if !released(&gate, "pause test released the in-flight resume") {
                    return Err(PlaybackError::EarlyEnd);
                }
                Err(PlaybackError::EarlyEnd)
            });
            let mut slot = info();
            slot.mode = mode;
            tx.send(Command::Pause).unwrap();
            tx.send(Command::Volume(40)).unwrap();
            control.poll(&slot, Duration::from_secs(65));
            assert!(control.paused);
            assert_eq!(control.volume, 40);
            tx.send(Command::Resume).unwrap();
            control.poll(&slot, Duration::from_secs(65));
            let expected = if mode == ProbeMode::Live {
                Duration::ZERO
            } else {
                Duration::from_secs(65)
            };
            assert_eq!(
                recv_phase(&seen_rx, "resume preparation request"),
                (expected, slot.starts_at),
                "resume must use the paused position (time-free 65s, live 0)"
            );
            let token = control.pending.as_ref().unwrap().clone();
            tx.send(Command::Pause).unwrap();
            control.poll(&slot, Duration::from_secs(65));
            assert!(
                token.is_cancelled(),
                "pause must cancel the in-flight resume preparation"
            );
            assert!(control.paused);
            assert!(!stop.is_cancelled());
            release.send(()).unwrap();
            control.finish_preparation();
        }
    }
    #[test]
    fn newer_seek_cancels_factory_token_and_starts_only_the_latest_preparation() {
        let (tx, rx) = mpsc::channel();
        let (seen, events) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let (saw_tx, saw_rx) = mpsc::channel();
        let stop = StopToken::default();
        let mut control = Controller::new_cancellable(rx, stop.clone(), move |request, cancel| {
            seen.send(request.position)
                .expect("preparation observation channel open");
            if request.position == Duration::from_secs(1)
                && released(&gate, "newer seek released the old factory")
            {
                saw_tx
                    .send(cancel.is_cancelled())
                    .expect("cancellation observation channel open");
            }
            Err(PlaybackError::Source(ProbeError::Cancelled))
        });
        assert_eq!(control.notification_interval, Duration::from_secs(5));
        tx.send(Command::Seek(Duration::from_secs(1))).unwrap();
        control.poll(&info(), Duration::ZERO);
        assert_eq!(
            recv_phase(&events, "first preparation"),
            Duration::from_secs(1)
        );
        let old = control
            .pending
            .as_ref()
            .expect("first preparation is in flight")
            .clone();
        tx.send(Command::Seek(Duration::from_secs(2))).unwrap();
        control.poll(&info(), Duration::ZERO);
        assert!(
            old.is_cancelled(),
            "newer seek must cancel the previous factory token"
        );
        assert!(
            !stop.is_cancelled(),
            "newer seek must not cancel the current playback stop"
        );
        release.send(()).unwrap();
        assert!(
            recv_phase(&saw_rx, "old factory cancellation observation"),
            "the old factory token must already be cancelled"
        );
        assert_eq!(
            recv_phase(&events, "latest preparation"),
            Duration::from_secs(2)
        );
        control.finish_preparation();
    }
    #[test]
    fn stale_slot_seek_never_calls_factory_or_cancels_current_playback() {
        let stop = StopToken::default();
        let (tx, rx) = mpsc::channel();
        let mut control =
            Controller::new(rx, stop.clone(), |_| panic!("stale seek must not prepare"));
        let slot = info();
        for (station, start, mode) in [
            ("OBS", slot.starts_at, ProbeMode::TimeFree),
            (
                "TBS",
                slot.starts_at + chrono::Duration::hours(1),
                ProbeMode::TimeFree,
            ),
            ("TBS", slot.starts_at, ProbeMode::Live),
        ] {
            let mut current = slot.clone();
            current.station = station.into();
            current.starts_at = start;
            current.mode = mode;
            tx.send(Command::SeekSlot {
                key: crate::domain::ProgramKey {
                    station_id: slot.station.clone(),
                    starts_at: slot.starts_at,
                },
                position: Duration::from_secs(15),
            })
            .unwrap();
            assert!(control.poll(&current, Duration::ZERO).is_none());
            assert!(control.pending.is_none());
            assert!(!stop.is_cancelled());
        }
        control.finish_preparation();
    }
    #[test]
    fn invalid_seek_and_live_requests_never_call_factory_or_cancel_current_playback() {
        let stop = StopToken::default();
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let (tx, rx) = mpsc::channel();
        let mut control = Controller::new(rx, stop.clone(), move |_| {
            count.fetch_add(1, Ordering::SeqCst);
            Err(PlaybackError::EarlyEnd)
        });
        tx.send(Command::Seek(Duration::from_secs(3600))).unwrap();
        assert!(control.poll(&info(), Duration::ZERO).is_none());
        let mut live = info();
        live.mode = ProbeMode::Live;
        tx.send(Command::Relative(15)).unwrap();
        assert!(control.poll(&live, Duration::ZERO).is_none());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(!stop.is_cancelled());
        control.finish_preparation();
    }
    #[test]
    fn stale_preparation_is_discarded_and_only_latest_position_becomes_ready() {
        let stop = StopToken::default();
        let pcm = bytes();
        let (tx, rx) = mpsc::channel();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (go_tx, go_rx) = mpsc::channel();
        let mut control = Controller::new(rx, stop.clone(), move |request| {
            if request.position == Duration::from_secs(1) {
                entered_tx
                    .send(())
                    .expect("first preparation observation channel open");
                if !released(&go_rx, "first preparation may finish") {
                    return Err(PlaybackError::Source(ProbeError::Cancelled));
                }
            }
            let mut target = info();
            target.position = request.position;
            Ok(Box::new(Source {
                info: target,
                bytes: Some(pcm.clone()),
            }) as Box<dyn SegmentSource>)
        });
        tx.send(Command::Seek(Duration::from_secs(1))).unwrap();
        control.poll(&info(), Duration::ZERO);
        recv_phase(&entered_rx, "first preparation entered");
        let seen = control.preparation_seq();
        go_tx.send(()).unwrap();
        let deadline = Instant::now() + HANG_WATCHDOG;
        assert!(
            control.wait_preparation_after(seen, deadline),
            "synchronization event missing: first preparation queued (hang watchdog {HANG_WATCHDOG:?})"
        );
        tx.send(Command::Seek(Duration::from_secs(2))).unwrap();
        let ready = wait_ready(&mut control, &info());
        assert_eq!(
            ready.info.position,
            Duration::from_secs(2),
            "a stale preparation must be discarded"
        );
        assert!(!stop.is_cancelled());
        assert!(!ready.worker.stop.is_cancelled());
        assert!(ready.pcm.summary.media_duration >= Duration::from_secs(2));
        control.finish_preparation();
    }
    #[test]
    fn authentication_failure_does_not_cancel_current_audio() {
        let stop = StopToken::default();
        let (tx, rx) = mpsc::channel();
        let mut control = Controller::new(rx, stop.clone(), move |_| {
            Err(PlaybackError::Source(ProbeError::Authentication))
        });
        tx.send(Command::Seek(Duration::from_secs(60))).unwrap();
        control.poll(&info(), Duration::ZERO);
        wait_pending_cleared(&mut control, &info());
        assert!(!stop.is_cancelled());
        control.finish_preparation();
    }
}
