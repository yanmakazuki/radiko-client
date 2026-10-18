//! 最新要求を優先する再生準備。準備失敗時も現在の音声出力を維持する。
use super::{
    output::StopToken,
    playback::{PlaybackError, Ready, prepare_ready},
};
use crate::radiko::stream::{
    ProbeMode,
    playback::{SegmentSource, StreamInfo},
};
use chrono::{DateTime, FixedOffset, Utc};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
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
}
pub struct Controller {
    commands: Receiver<Command>,
    preparation_worker: Option<thread::JoinHandle<()>>,
    reporter: Box<dyn FnMut(String) + Send>,
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
}
impl Controller {
    pub fn new(
        commands: Receiver<Command>,
        stop: StopToken,
        factory: impl Fn(PlaybackRequest) -> Result<Box<dyn SegmentSource>, PlaybackError>
        + Send
        + 'static,
    ) -> Self {
        let mut controller =
            Self::with_reporter(commands, stop, factory, |message| eprintln!("{message}"));
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
        let mut controller = Self::with_cancellable_reporter(commands, stop, factory, |message| {
            eprintln!("{message}")
        });
        controller.notification_interval = Duration::from_secs(5);
        controller
    }
    pub fn with_reporter(
        commands: Receiver<Command>,
        stop: StopToken,
        factory: impl Fn(PlaybackRequest) -> Result<Box<dyn SegmentSource>, PlaybackError>
        + Send
        + 'static,
        reporter: impl FnMut(String) + Send + 'static,
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
        reporter: impl FnMut(String) + Send + 'static,
    ) -> Self {
        let latest = Arc::new(Mutex::new(None::<Job>));
        let generation = Arc::new(AtomicU64::new(0));
        let (tx, results) = mpsc::sync_channel(1);
        let (mailbox, ids, shutdown) = (latest.clone(), generation.clone(), stop.clone());
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
                let result = factory(job.request.clone(), job.stop.clone()).and_then(|source| {
                    let info = source.info();
                    let same_slot = job.expected.as_ref().is_none_or(|expected| {
                        expected.station == info.station
                            && expected.starts_at == info.starts_at
                            && expected.ends_at == info.ends_at
                            && expected.mode == info.mode
                    });
                    if !job.request.matches(info) || !same_slot {
                        return Err(PlaybackError::Source(
                            crate::radiko::stream::ProbeError::InvalidPosition,
                        ));
                    }
                    if job.stop.is_cancelled() {
                        return Err(PlaybackError::Worker);
                    }
                    prepare_ready(source, job.stop.clone())
                });
                if job.id != ids.load(Ordering::Acquire) || job.stop.is_cancelled() {
                    continue;
                }
                let mut value = Completed { id: job.id, result };
                loop {
                    if shutdown.is_cancelled() || job.id != ids.load(Ordering::Acquire) {
                        break;
                    }
                    match tx.try_send(value) {
                        Ok(()) => break,
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
        }
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
        (self.reporter)(message);
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
                if crate::radiko::stream::validate_station(&target.station).is_err()
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
                    (self.reporter)("再生準備に失敗しました（現在の状態を維持）".into());
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
                    self.pending = None;
                    return Some(ready);
                }
                Err(error) => self.report(format!("再生準備失敗（現在の状態を維持）: {error}")),
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
    use crate::radiko::stream::{
        ProbeError,
        playback::{Next, Segment},
    };
    use std::sync::atomic::{AtomicBool, AtomicUsize};
    use std::time::Instant;
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
    fn wait_ready(control: &mut Controller, current: &StreamInfo) -> Ready {
        let began = Instant::now();
        loop {
            if let Some(ready) = control.poll(current, Duration::ZERO) {
                return ready;
            }
            assert!(began.elapsed() < Duration::from_secs(3));
            thread::sleep(Duration::from_millis(2));
        }
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
        received.recv().unwrap();
        tx.send(Command::Seek(Duration::from_secs(15))).unwrap();
        let next = wait_ready(&mut control, &ready.info);
        let request = received.recv().unwrap();
        assert_eq!(request.station, "OBS");
        assert_eq!(request.cursor, later);
        assert_eq!(next.info.position, Duration::from_secs(15));
    }
    #[test]
    fn selecting_twice_discards_old_station_even_when_preparation_finishes_late() {
        let (tx, rx) = mpsc::channel();
        let (entered, seen) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let pcm = bytes();
        let mut control = Controller::new(rx, StopToken::default(), move |request| {
            if request.station == "OBS" {
                entered.send(()).unwrap();
                gate.recv().unwrap();
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
        seen.recv_timeout(Duration::from_secs(2)).unwrap();
        let old = control.pending.as_ref().unwrap().clone();
        tx.send(Command::Select(target("QRR", info().starts_at)))
            .unwrap();
        control.poll(&info(), Duration::ZERO);
        assert!(old.is_cancelled());
        release.send(()).unwrap();
        let ready = wait_ready(&mut control, &info());
        assert_eq!(ready.info.station, "QRR");
        assert!(!ready.worker.stop.is_cancelled());
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
        let began = Instant::now();
        while control.pending.is_some() {
            assert!(control.poll(&info(), Duration::ZERO).is_none());
            assert!(began.elapsed() < Duration::from_secs(2));
            thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(!stop.is_cancelled());
    }
    #[test]
    fn pause_invalidates_pending_seek_and_resume_uses_frozen_or_latest_position() {
        for mode in [ProbeMode::TimeFree, ProbeMode::Live] {
            let (tx, rx) = mpsc::channel();
            let (seen_tx, seen_rx) = mpsc::channel();
            let (release, gate) = mpsc::channel();
            let stop = StopToken::default();
            let mut control = Controller::new(rx, stop.clone(), move |request| {
                let position = request.position;
                seen_tx.send((position, request.cursor)).unwrap();
                gate.recv_timeout(Duration::from_secs(2)).unwrap();
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
            assert_eq!(
                seen_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
                (
                    if mode == ProbeMode::Live {
                        Duration::ZERO
                    } else {
                        Duration::from_secs(65)
                    },
                    slot.starts_at
                )
            );
            let token = control.pending.as_ref().unwrap().clone();
            tx.send(Command::Pause).unwrap();
            control.poll(&slot, Duration::from_secs(65));
            assert!(token.is_cancelled());
            assert!(control.paused);
            assert!(!stop.is_cancelled());
            release.send(()).unwrap();
        }
    }
    #[test]
    fn newer_seek_cancels_factory_token_and_starts_only_the_latest_preparation() {
        let (tx, rx) = mpsc::channel();
        let (seen, events) = mpsc::channel();
        let stop = StopToken::default();
        let mut control = Controller::new_cancellable(rx, stop.clone(), move |request, cancel| {
            seen.send(request.position).unwrap();
            if request.position == Duration::from_secs(1) {
                let began = Instant::now();
                while !cancel.is_cancelled() {
                    assert!(began.elapsed() < Duration::from_secs(2));
                    thread::sleep(Duration::from_millis(1));
                }
            }
            Err(PlaybackError::Source(ProbeError::Cancelled))
        });
        assert_eq!(control.notification_interval, Duration::from_secs(5));
        tx.send(Command::Seek(Duration::from_secs(1))).unwrap();
        control.poll(&info(), Duration::ZERO);
        assert_eq!(
            events.recv_timeout(Duration::from_secs(2)).unwrap(),
            Duration::from_secs(1)
        );
        tx.send(Command::Seek(Duration::from_secs(2))).unwrap();
        control.poll(&info(), Duration::ZERO);
        assert_eq!(
            events.recv_timeout(Duration::from_secs(2)).unwrap(),
            Duration::from_secs(2)
        );
        assert!(!stop.is_cancelled());
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
    }
    #[test]
    fn stale_preparation_is_discarded_and_only_latest_position_becomes_ready() {
        let stop = StopToken::default();
        let entered = Arc::new(AtomicBool::new(false));
        let release = Arc::new(AtomicBool::new(false));
        let (seen, gate, shutdown) = (entered.clone(), release.clone(), stop.clone());
        let pcm = bytes();
        let (tx, rx) = mpsc::channel();
        let mut control = Controller::new(rx, stop.clone(), move |request| {
            let position = request.position;
            if position == Duration::from_secs(1) {
                seen.store(true, Ordering::Release);
                while !gate.load(Ordering::Acquire) && !shutdown.is_cancelled() {
                    thread::sleep(Duration::from_millis(1));
                }
            }
            let mut target = info();
            target.position = position;
            Ok(Box::new(Source {
                info: target,
                bytes: Some(pcm.clone()),
            }) as Box<dyn SegmentSource>)
        });
        tx.send(Command::Seek(Duration::from_secs(1))).unwrap();
        control.poll(&info(), Duration::ZERO);
        let began = Instant::now();
        while !entered.load(Ordering::Acquire) {
            assert!(began.elapsed() < Duration::from_secs(2));
            thread::sleep(Duration::from_millis(1));
        }
        tx.send(Command::Seek(Duration::from_secs(2))).unwrap();
        control.poll(&info(), Duration::ZERO);
        release.store(true, Ordering::Release);
        let ready = loop {
            if let Some(ready) = control.poll(&info(), Duration::ZERO) {
                break ready;
            }
            assert!(began.elapsed() < Duration::from_secs(2));
            thread::sleep(Duration::from_millis(2));
        };
        assert_eq!(ready.info.position, Duration::from_secs(2));
        assert!(!stop.is_cancelled());
        assert!(!ready.worker.stop.is_cancelled());
        assert!(ready.pcm.summary.media_duration >= Duration::from_secs(2));
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
        let began = Instant::now();
        while control.pending.is_some() {
            assert!(control.poll(&info(), Duration::ZERO).is_none());
            assert!(began.elapsed() < Duration::from_secs(2));
            thread::sleep(Duration::from_millis(2));
        }
        assert!(!stop.is_cancelled());
    }
}
