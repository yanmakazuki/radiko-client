//! Bounded command handoff; the owning worker creates and drops CPAL on its own thread.
use crate::{
    api::{
        auth::MemberClient,
        stream::{
            ProbeMode,
            playback::{SegmentSource, StreamInfo, prepare_at_controlled},
        },
    },
    domain::{BroadcastStatus, ListeningAvailability, Program, broadcast_status},
    player::{
        output::StopToken,
        playback,
        seek::{Command, Controller, PlaybackRequest},
        state::PlaybackSnapshot,
    },
    storage::SessionStore,
};
use chrono::{DateTime, FixedOffset};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

type Factory = Arc<
    dyn Fn(PlaybackRequest, StopToken) -> Result<Box<dyn SegmentSource>, playback::PlaybackError>
        + Send
        + Sync,
>;
/// Only the active output is evidence of stream permission; no result is cached.
pub fn availability(
    program: &Program,
    mode: ProbeMode,
    now: DateTime<FixedOffset>,
    active: Option<&PlaybackSnapshot>,
) -> ListeningAvailability {
    match broadcast_status(program.key.starts_at, program.ends_at, now) {
        Err(_) => return ListeningAvailability::Unavailable("放送枠が不正なため再生できません"),
        Ok(BroadcastStatus::Upcoming) => {
            return ListeningAvailability::Unavailable("放送前のため再生できません");
        }
        Ok(BroadcastStatus::Finished) if mode == ProbeMode::Live => {
            return ListeningAvailability::Unavailable("放送終了のためライブ再生できません");
        }
        _ => (),
    }
    if active.is_some_and(|p| {
        p.info.station == program.key.station_id
            && p.info.starts_at == program.key.starts_at
            && p.info.ends_at == program.ends_at
            && p.info.mode == mode
            && p.output
                .as_ref()
                .is_some_and(|o| o.sample_rate > 0 && o.frames > 0)
    }) {
        ListeningAvailability::CurrentOutput
    } else {
        ListeningAvailability::Unknown
    }
}
/// Why playback stopped, taken from the error value rather than its display text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopReason {
    AuthExpired,
    ReconnectLimit,
}
/// `Some` only for an auth-expiry or reconnect-limit failure. Other errors, including
/// window loss and a normal program end, are `None`.
pub fn stop_reason(error: &playback::PlaybackError) -> Option<StopReason> {
    match error {
        playback::PlaybackError::ReconnectGaveUp => Some(StopReason::ReconnectLimit),
        playback::PlaybackError::Output(crate::player::output::OutputError::ReconnectGaveUp) => {
            Some(StopReason::ReconnectLimit)
        }
        playback::PlaybackError::Source(error) => probe_stop_reason(error),
        _ => None,
    }
}
fn probe_stop_reason(error: &crate::api::stream::ProbeError) -> Option<StopReason> {
    use crate::api::stream::ProbeError;
    match error {
        ProbeError::At(_, inner) => probe_stop_reason(inner),
        ProbeError::AuthExpired => Some(StopReason::AuthExpired),
        ProbeError::ReconnectGaveUp => Some(StopReason::ReconnectLimit),
        _ => None,
    }
}
#[derive(Default)]
pub struct Update {
    /// A previous control session has finished; reset its visible state before this update.
    pub new_session: bool,
    pub snapshot: Option<PlaybackSnapshot>,
    pub notice: Option<String>,
    /// `notice` is a playback failure. Reconnect and catch-up must not replace it.
    pub stopped: bool,
    /// Set only together with [`Self::stopped`], from [`stop_reason`].
    pub stop_reason: Option<StopReason>,
    pub ended: bool,
    pub final_diagnostics: Option<String>,
}
struct Running {
    stop: StopToken,
    commands: mpsc::SyncSender<Command>,
    handle: thread::JoinHandle<()>,
    /// Set after the control thread drops its output, before preparation is joined.
    output_released: Arc<AtomicBool>,
}
/// Signals that `play_session` has returned, so the device drop already ran.
struct OutputRelease(Arc<AtomicBool>);
impl Drop for OutputRelease {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}
#[cfg(test)]
type TestSink = Arc<
    dyn Fn(
            &crate::player::aac::DecodedPcm,
            u64,
            Option<Duration>,
            StopToken,
        ) -> Result<
            Box<dyn crate::player::playback::AudioSink + Send>,
            crate::player::output::OutputError,
        > + Send
        + Sync,
>;
pub struct Service {
    factory: Factory,
    mailbox: Arc<Mutex<Update>>,
    running: Option<Running>,
    volume: u32,
    #[cfg(test)]
    test_sink: Option<TestSink>,
}
/// This preflight does not establish listening permission. The stream resolver checks it again.
pub fn request(
    program: &Program,
    now: DateTime<FixedOffset>,
) -> Result<PlaybackRequest, &'static str> {
    match broadcast_status(program.key.starts_at, program.ends_at, now)? {
        BroadcastStatus::Upcoming => return Err("放送前のため再生できません（現在の再生を継続）"),
        BroadcastStatus::OnAir | BroadcastStatus::Finished => (),
    }
    Ok(PlaybackRequest {
        station: program.key.station_id.clone(),
        mode: ProbeMode::TimeFree,
        cursor: program.key.starts_at,
        position: Duration::ZERO,
    })
}
/// Resolve the selected slot again; an ended slot must never become the next live program.
pub fn live_request(
    program: &Program,
    now: DateTime<FixedOffset>,
) -> Result<PlaybackRequest, &'static str> {
    let mut target = request(program, now)?;
    if broadcast_status(program.key.starts_at, program.ends_at, now)? != BroadcastStatus::OnAir {
        return Err("放送終了のためライブ再生できません（現在の再生を継続）");
    }
    target.mode = ProbeMode::Live;
    Ok(target)
}

impl Service {
    pub fn new(data_dir: Option<PathBuf>) -> Self {
        Self::with_cancellable_factory(move |target, stop| {
            let check = || {
                if stop.is_cancelled() {
                    Err(playback::PlaybackError::Source(
                        crate::api::stream::ProbeError::Cancelled,
                    ))
                } else {
                    Ok(())
                }
            };
            check()?;
            let store = match &data_dir {
                Some(path) => SessionStore::new(path.clone()),
                None => {
                    SessionStore::default_location().map_err(playback::PlaybackError::Storage)?
                }
            };
            let session = store
                .load()
                .map_err(playback::PlaybackError::Storage)?
                .ok_or(playback::PlaybackError::LoginRequired)?;
            check()?;
            let auth_stop = stop.clone();
            let membership =
                MemberClient::new_cancellable(Arc::new(move || auth_stop.is_cancelled()))
                    .and_then(|client| client.check(&session))
                    .map_err(playback::PlaybackError::Auth)?;
            check()?;
            let source = prepare_at_controlled(
                &target.station,
                target.mode,
                target.cursor,
                &session,
                &membership,
                target.position,
                Arc::new(move || stop.is_cancelled()),
            )
            .map_err(playback::PlaybackError::Source)?;
            // UI selection identifies a slot, not merely a timestamp contained in another slot.
            if source.info().starts_at != target.cursor {
                return Err(playback::PlaybackError::Source(
                    crate::api::stream::ProbeError::InvalidPosition,
                ));
            }
            Ok(Box::new(source))
        })
    }
    pub fn with_factory(
        factory: impl Fn(PlaybackRequest) -> Result<Box<dyn SegmentSource>, playback::PlaybackError>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self::with_cancellable_factory(move |request, _| factory(request))
    }
    pub fn with_cancellable_factory(
        factory: impl Fn(
            PlaybackRequest,
            StopToken,
        ) -> Result<Box<dyn SegmentSource>, playback::PlaybackError>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self {
            factory: Arc::new(factory),
            mailbox: Arc::new(Mutex::new(Update::default())),
            running: None,
            volume: 25,
            #[cfg(test)]
            test_sink: None,
        }
    }
    #[cfg(test)]
    fn with_test_sink(
        factory: impl Fn(
            PlaybackRequest,
            StopToken,
        ) -> Result<Box<dyn SegmentSource>, playback::PlaybackError>
        + Send
        + Sync
        + 'static,
        sink: TestSink,
    ) -> Self {
        let mut service = Self::with_cancellable_factory(factory);
        service.test_sink = Some(sink);
        service
    }
    pub fn select(
        &mut self,
        program: &Program,
        now: DateTime<FixedOffset>,
    ) -> Result<(), &'static str> {
        let target = request(program, now)?;
        self.select_target(program, target)
    }
    pub fn select_live(
        &mut self,
        program: &Program,
        now: DateTime<FixedOffset>,
    ) -> Result<(), &'static str> {
        let target = live_request(program, now)?;
        self.select_target(program, target)
    }
    pub fn select_at(
        &mut self,
        program: &Program,
        position: Duration,
        now: DateTime<FixedOffset>,
    ) -> Result<(), &'static str> {
        let mut target = request(program, now)?;
        let length = (program.ends_at - program.key.starts_at)
            .to_std()
            .map_err(|_| "放送枠の長さが不正です")?;
        if position >= length {
            return Err("放送枠外にはジャンプできません（現在の再生を継続）");
        }
        target.position = position;
        self.select_target(program, target)
    }
    fn select_target(
        &mut self,
        program: &Program,
        target: PlaybackRequest,
    ) -> Result<(), &'static str> {
        if self
            .running
            .as_ref()
            .is_some_and(|r| r.handle.is_finished())
        {
            if let Some(running) = self.running.take() {
                let _ = running.handle.join();
            }
        }
        if self.running.is_none() {
            let stop = StopToken::default();
            let (commands, rx) = mpsc::sync_channel(4);
            let factory = self.factory.clone();
            let (progress, notices, finished) = (
                self.mailbox.clone(),
                self.mailbox.clone(),
                self.mailbox.clone(),
            );
            let shutdown = stop.clone();
            let idle = StreamInfo {
                station: target.station.clone(),
                title: None,
                mode: target.mode,
                starts_at: target.cursor,
                ends_at: program.ends_at,
                position: Duration::ZERO,
            };
            if let Ok(mut update) = self.mailbox.lock() {
                *update = Update {
                    new_session: true,
                    ..Update::default()
                };
            }
            #[cfg(test)]
            let test_sink = self.test_sink.clone();
            let output_released = Arc::new(AtomicBool::new(false));
            let released_flag = output_released.clone();
            let handle = thread::spawn(move || {
                let mut controls = Controller::with_cancellable_reporter(
                    rx,
                    shutdown.clone(),
                    move |request, stop| factory(request, stop),
                    move |notice| {
                        if let Ok(mut update) = notices.lock() {
                            update.notice = Some(notice);
                        }
                    },
                );
                // Dropped when playback returns, including unwind, and before preparation is joined.
                let released = OutputRelease(released_flag);
                let report = move |snapshot| {
                    if let Ok(mut update) = progress.lock() {
                        update.snapshot = Some(snapshot);
                    }
                };
                #[cfg(test)]
                let result = if let Some(create) = test_sink {
                    playback::play_idle_with(
                        idle,
                        shutdown,
                        &mut controls,
                        report,
                        move |pcm, maximum, remaining, stop| {
                            create(pcm, maximum, remaining, stop).map(playback::BoxedSink)
                        },
                    )
                } else {
                    playback::play_session(idle, shutdown, &mut controls, report)
                };
                #[cfg(not(test))]
                let result = playback::play_session(idle, shutdown, &mut controls, report);
                drop(released);
                if let Ok(mut update) = finished.lock() {
                    update.ended = true;
                    if let Ok(report) = &result {
                        update.final_diagnostics = Some(crate::player::state::output_diagnostics(
                            report.output.as_ref(),
                            report.segments,
                        ));
                    }
                    // Preserve an undelivered snapshot so the UI can retain diagnostics on failure.
                    update.stopped = result.is_err();
                    update.stop_reason = result.as_ref().err().and_then(stop_reason);
                    update.notice = Some(match &result {
                        Ok(report) => match report.reason {
                            playback::EndReason::ProgramEnd => "番組終了".into(),
                            _ => "停止しました".into(),
                        },
                        Err(error) => playback::failure_notice(error),
                    });
                }
                // Output is already dropped. Join only so a later session does not accumulate
                // workers; callers waiting on `output_released` do not wait for this join.
                controls.finish_preparation();
            });
            self.running = Some(Running {
                stop,
                commands,
                handle,
                output_released,
            });
        }
        self.command(Command::Volume(self.volume))?;
        self.command(Command::Select(target))
    }
    pub fn command(&mut self, mut command: Command) -> Result<(), &'static str> {
        let volume = if let Command::Volume(value) = &mut command {
            *value = (*value).min(100);
            Some(*value)
        } else {
            None
        };
        if self.running.is_none() {
            if let Some(volume) = volume {
                self.volume = volume;
                return Ok(());
            }
        }
        let running = self.running.as_ref().ok_or("再生中の番組はありません")?;
        if running.stop.is_cancelled() {
            return Err("停止処理中です。停止後に再度操作してください");
        }
        running
            .commands
            .try_send(command)
            .map_err(|_| "再生操作の処理待ちです。少し待って再度操作してください")?;
        if let Some(volume) = volume {
            self.volume = volume;
        }
        Ok(())
    }
    pub fn stop(&self) {
        if let Some(running) = &self.running {
            running.stop.cancel();
        }
    }
    /// Cancel playback and wait until the device has been dropped.
    /// The wait is at most one second and does not include in-flight HTTP.
    pub fn release_output(&mut self) {
        self.stop();
        self.wait_until_output_released(Duration::from_secs(1));
    }
    fn wait_until_output_released(&self, bound: Duration) {
        let Some(flag) = self
            .running
            .as_ref()
            .map(|running| running.output_released.clone())
        else {
            return;
        };
        let deadline = Instant::now() + bound;
        while !flag.load(Ordering::Acquire) {
            if Instant::now() >= deadline {
                return;
            }
            thread::sleep(Duration::from_millis(1));
        }
    }
    pub fn poll(&mut self) -> Update {
        let mut update = self
            .mailbox
            .lock()
            .map(|mut slot| std::mem::take(&mut *slot))
            .unwrap_or_default();
        if self
            .running
            .as_ref()
            .is_some_and(|r| r.handle.is_finished())
        {
            if let Some(running) = self.running.take() {
                if running.handle.join().is_err() {
                    update.ended = true;
                    update.notice = Some("再生ワーカーが予期せず終了しました".into());
                }
            }
        }
        update
    }
}
impl Drop for Service {
    fn drop(&mut self) {
        // Keep the control thread attached until the device is dropped or the bound elapses.
        // Dropping `running` afterwards detaches it; that drop does not join HTTP.
        self.release_output();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;
    fn program() -> Program {
        crate::api::catalog::parse_programs(
            include_bytes!("../../../../tests/fixtures/schedule.xml"),
            "TBS",
            crate::api::catalog::parse_day("20261015").unwrap(),
        )
        .unwrap()
        .programs
        .remove(0)
    }
    fn until(mut predicate: impl FnMut() -> bool) {
        let start = Instant::now();
        while !predicate() {
            assert!(start.elapsed() < Duration::from_secs(3));
            thread::sleep(Duration::from_millis(5));
        }
    }
    #[test]
    fn stop_reason_is_taken_from_the_error_and_not_from_other_failures() {
        use crate::api::stream::ProbeError;
        let expired = playback::PlaybackError::Source(ProbeError::At(
            "HLS一覧更新",
            Box::new(ProbeError::AuthExpired),
        ));
        assert_eq!(stop_reason(&expired), Some(StopReason::AuthExpired));
        assert_eq!(
            stop_reason(&playback::PlaybackError::Source(ProbeError::AuthExpired)),
            Some(StopReason::AuthExpired)
        );
        assert_eq!(
            stop_reason(&playback::PlaybackError::ReconnectGaveUp),
            Some(StopReason::ReconnectLimit)
        );
        assert_eq!(
            stop_reason(&playback::PlaybackError::Output(
                crate::player::output::OutputError::ReconnectGaveUp
            )),
            Some(StopReason::ReconnectLimit)
        );
        assert_eq!(
            stop_reason(&playback::PlaybackError::Source(
                ProbeError::ReconnectGaveUp
            )),
            Some(StopReason::ReconnectLimit)
        );
        assert_eq!(
            stop_reason(&playback::PlaybackError::Source(ProbeError::WindowLost)),
            None
        );
        assert_eq!(stop_reason(&playback::PlaybackError::LoginRequired), None);
        let update = Update {
            stopped: false,
            notice: Some(expired.to_string()),
            stop_reason: None,
            ..Update::default()
        };
        assert!(!update.stopped);
        assert!(update.stop_reason.is_none());
    }
    #[test]
    fn rejected_volume_commands_never_change_the_value_used_by_the_next_session() {
        let mut service = Service::with_factory(|_| Err(playback::PlaybackError::LoginRequired));
        service.command(Command::Volume(35)).unwrap();
        let (commands, queued) = mpsc::sync_channel(1);
        commands.try_send(Command::Status).unwrap();
        let (release, waiting) = mpsc::channel();
        service.running = Some(Running {
            stop: StopToken::default(),
            commands,
            handle: thread::spawn(move || {
                let _ = waiting.recv();
            }),
            output_released: Arc::new(AtomicBool::new(true)),
        });
        assert!(service.command(Command::Volume(0)).is_err());
        assert_eq!(service.volume, 35);
        assert!(matches!(queued.try_recv().unwrap(), Command::Status));
        service.command(Command::Volume(150)).unwrap();
        assert_eq!(service.volume, 100);
        assert!(matches!(queued.try_recv().unwrap(), Command::Volume(100)));
        service.stop();
        assert!(service.command(Command::Volume(0)).is_err());
        assert_eq!(service.volume, 100);
        release.send(()).unwrap();
        service.running.take().unwrap().handle.join().unwrap();

        let (commands, queued) = mpsc::sync_channel(1);
        drop(queued);
        service.running = Some(Running {
            stop: StopToken::default(),
            commands,
            handle: thread::spawn(|| {}),
            output_released: Arc::new(AtomicBool::new(true)),
        });
        assert!(service.command(Command::Volume(0)).is_err());
        assert_eq!(service.volume, 100);
        service.running.take().unwrap().handle.join().unwrap();
    }
    #[test]
    fn restarting_before_reading_the_end_notification_publishes_a_new_session_boundary() {
        let mut service = Service::with_factory(|_| Err(playback::PlaybackError::LoginRequired));
        let p = program();
        service.select(&p, p.ends_at).unwrap();
        assert!(service.poll().new_session);
        service.stop();
        // Deliberately leave the old terminal notification unread.
        until(|| {
            service
                .running
                .as_ref()
                .is_some_and(|r| r.handle.is_finished())
        });
        assert!(service.mailbox.lock().unwrap().ended);
        service.select(&p, p.ends_at).unwrap();
        let update = service.poll();
        assert!(update.new_session);
        assert!(!update.ended);
        assert!(update.snapshot.is_none());
    }
    #[test]
    fn availability_never_infers_permission_from_finished_status_or_another_slot() {
        use crate::player::{output::OutputSummary, state::PlaybackState};
        let p = program();
        let mut snapshot = PlaybackSnapshot {
            pipeline: None,
            info: StreamInfo {
                station: p.key.station_id.clone(),
                title: Some(p.title.clone()),
                starts_at: p.key.starts_at,
                ends_at: p.ends_at,
                mode: ProbeMode::TimeFree,
                position: Duration::ZERO,
            },
            state: PlaybackState::Playing,
            pending: None,
            volume: 0,
            position: Some(Duration::from_secs(1)),
            segments: 1,
            receiving_for: None,
            reconnecting_for: None,
            output: Some(OutputSummary {
                frames: 48000,
                underrun_frames: 0,
                sample_rate: 48000,
                channels: 2,
                sample_format: crate::player::output::OutputSampleFormat::F32,
            }),
        };
        assert_eq!(
            availability(&p, ProbeMode::TimeFree, p.ends_at, None),
            ListeningAvailability::Unknown
        );
        assert_eq!(
            availability(&p, ProbeMode::TimeFree, p.ends_at, Some(&snapshot)),
            ListeningAvailability::CurrentOutput
        );
        snapshot.state = PlaybackState::Paused;
        assert_eq!(
            availability(&p, ProbeMode::TimeFree, p.ends_at, Some(&snapshot)),
            ListeningAvailability::CurrentOutput
        );
        assert!(matches!(
            availability(
                &p,
                ProbeMode::TimeFree,
                p.key.starts_at - chrono::Duration::seconds(1),
                Some(&snapshot)
            ),
            ListeningAvailability::Unavailable(_)
        ));
        assert!(matches!(
            availability(&p, ProbeMode::Live, p.ends_at, Some(&snapshot)),
            ListeningAvailability::Unavailable(_)
        ));
        assert_eq!(
            availability(&p, ProbeMode::Live, p.key.starts_at, Some(&snapshot)),
            ListeningAvailability::Unknown
        );
        snapshot.output.as_mut().unwrap().frames = 0;
        assert_eq!(
            availability(&p, ProbeMode::TimeFree, p.ends_at, Some(&snapshot)),
            ListeningAvailability::Unknown
        );
        snapshot.output.as_mut().unwrap().frames = 48000;
        for changed in 0..3 {
            let mut different = p.clone();
            match changed {
                0 => different.key.station_id = "OBS".into(),
                1 => different.key.starts_at += chrono::Duration::seconds(1),
                _ => different.ends_at += chrono::Duration::seconds(1),
            }
            assert_eq!(
                availability(
                    &different,
                    ProbeMode::TimeFree,
                    different.ends_at,
                    Some(&snapshot)
                ),
                ListeningAvailability::Unknown
            );
        }
        assert_eq!(
            availability(&p, ProbeMode::TimeFree, p.ends_at, None),
            ListeningAvailability::Unknown
        );
    }
    #[test]
    fn chapter_position_rejection_never_starts_output_or_fetches() {
        let mut service = Service::with_factory(|_| panic!("rejected jump must not fetch"));
        let p = program();
        let length = (p.ends_at - p.key.starts_at).to_std().unwrap();
        assert!(service.select_at(&p, length, p.ends_at).is_err());
        assert!(
            service
                .select_at(
                    &p,
                    Duration::ZERO,
                    p.key.starts_at - chrono::Duration::seconds(1)
                )
                .is_err()
        );
        assert!(service.running.is_none());
    }
    #[test]
    fn upcoming_selection_never_starts_worker_or_stops_existing_preparation() {
        let mut service = Service::with_factory(|_| panic!("upcoming must not fetch"));
        let p = program();
        let before = p.key.starts_at - chrono::Duration::seconds(1);
        assert!(service.select(&p, before).unwrap_err().contains("放送前"));
        assert!(service.running.is_none());
        let target = request(&p, p.key.starts_at).unwrap();
        assert_eq!(target.cursor, p.key.starts_at);
        assert_eq!(target.station, p.key.station_id);
        assert_eq!(request(&p, p.ends_at).unwrap().cursor, target.cursor);
    }
    #[test]
    fn live_selection_is_bound_to_the_on_air_slot_and_rejects_boundaries_without_io() {
        let p = program();
        let mut service = Service::with_factory(|_| panic!("rejected live must not fetch"));
        assert!(
            service
                .select_live(&p, p.key.starts_at - chrono::Duration::seconds(1))
                .is_err()
        );
        assert!(
            service
                .select_live(&p, p.ends_at)
                .unwrap_err()
                .contains("放送終了")
        );
        assert!(service.running.is_none());
        for now in [p.key.starts_at, p.ends_at - chrono::Duration::seconds(1)] {
            let target = live_request(&p, now).unwrap();
            assert_eq!(target.mode, ProbeMode::Live);
            assert_eq!(target.cursor, p.key.starts_at);
            assert_eq!(target.station, p.key.station_id);
            assert_eq!(target.position, Duration::ZERO);
        }
    }
    #[test]
    fn missing_and_corrupt_session_errors_are_reported_without_replacing_files() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut service = Service::new(Some(directory.path().to_owned()));
        let p = program();
        service.select(&p, p.ends_at).unwrap();
        until(|| {
            service
                .poll()
                .notice
                .is_some_and(|n| n.contains("未ログイン"))
        });
        assert!(!directory.path().join("session.json").exists());
        let file = directory.path().join("session.json");
        std::fs::write(&file, b"broken").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        service.select(&p, p.ends_at).unwrap();
        until(|| service.poll().notice.is_some_and(|n| n.contains("破損")));
        assert_eq!(std::fs::read(file).unwrap(), b"broken");
        service.stop();
        until(|| {
            let update = service.poll();
            if update.ended {
                assert_eq!(
                    update.final_diagnostics.as_deref(),
                    Some("出力未確認 / 0セグメント")
                );
                true
            } else {
                false
            }
        });
    }
    #[test]
    fn blocked_preparation_does_not_block_poll_stop_or_rejecting_future_program() {
        let (entered, seen) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let gate = Mutex::new(gate);
        let mut service = Service::with_factory(move |_| {
            entered.send(()).unwrap();
            let _ = gate.lock().unwrap().recv_timeout(Duration::from_secs(3));
            Err(playback::PlaybackError::LoginRequired)
        });
        let p = program();
        service.select(&p, p.ends_at).unwrap();
        seen.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(
            service
                .select(&p, p.key.starts_at - chrono::Duration::seconds(1))
                .is_err()
        );
        assert!(!service.running.as_ref().unwrap().stop.is_cancelled());
        let began = Instant::now();
        assert!(service.poll().snapshot.is_none());
        service.stop();
        until(|| service.poll().ended);
        assert!(began.elapsed() < Duration::from_secs(1));
        assert!(
            service
                .select(&p, p.ends_at)
                .unwrap_err()
                .contains("停止処理中")
        );
        release.send(()).unwrap();
        until(|| {
            service.poll();
            service.running.is_none()
        });
    }
    #[test]
    fn service_stop_reaches_in_flight_factory_without_waiting_for_a_result() {
        let (entered, seen) = mpsc::channel();
        let (done, finished) = mpsc::channel();
        let mut service = Service::with_cancellable_factory(move |_, stop| {
            entered.send(()).unwrap();
            let began = Instant::now();
            while !stop.is_cancelled() {
                assert!(began.elapsed() < Duration::from_secs(2));
                thread::sleep(Duration::from_millis(1));
            }
            done.send(()).unwrap();
            Err(playback::PlaybackError::Source(
                crate::api::stream::ProbeError::Cancelled,
            ))
        });
        let p = program();
        service.select(&p, p.ends_at).unwrap();
        seen.recv_timeout(Duration::from_secs(2)).unwrap();
        service.stop();
        finished.recv_timeout(Duration::from_secs(2)).unwrap();
        until(|| {
            service.poll();
            service.running.is_none()
        });
    }
    #[test]
    fn dropping_service_cancels_output_token_even_while_factory_is_waiting() {
        let (entered, seen) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let gate = Mutex::new(gate);
        let mut service = Service::with_factory(move |_| {
            entered.send(()).unwrap();
            let _ = gate.lock().unwrap().recv_timeout(Duration::from_secs(3));
            Err(playback::PlaybackError::LoginRequired)
        });
        let p = program();
        service.select(&p, p.ends_at).unwrap();
        seen.recv_timeout(Duration::from_secs(2)).unwrap();
        let stop = service.running.as_ref().unwrap().stop.clone();
        drop(service);
        assert!(stop.is_cancelled());
        release.send(()).unwrap();
    }
    struct FlagSink {
        closed: Arc<AtomicBool>,
        accepted: u64,
        rate: u32,
        channels: u16,
    }
    impl Drop for FlagSink {
        fn drop(&mut self) {
            self.closed.store(true, Ordering::Release);
        }
    }
    impl crate::player::playback::AudioSink for FlagSink {
        fn enqueue(
            &mut self,
            pcm: &crate::player::aac::DecodedPcm,
            cursor: &mut usize,
        ) -> Result<bool, crate::player::output::OutputError> {
            self.accepted = self.accepted.saturating_add(pcm.summary.frames as u64);
            *cursor = pcm.samples.len();
            Ok(true)
        }
        fn accepted(&self) -> u64 {
            self.accepted
        }
        fn summary(&self) -> crate::player::output::OutputSummary {
            crate::player::output::OutputSummary {
                frames: self.accepted,
                underrun_frames: 0,
                sample_rate: self.rate,
                channels: self.channels,
                sample_format: crate::player::output::OutputSampleFormat::F32,
            }
        }
        fn poll(&mut self) -> Result<bool, crate::player::output::OutputError> {
            Ok(false)
        }
        fn finish(&mut self) {}
    }
    fn flag_sink(opened: Arc<AtomicBool>, closed: Arc<AtomicBool>) -> TestSink {
        Arc::new(move |pcm, _, _, _| {
            opened.store(true, Ordering::Release);
            Ok(Box::new(FlagSink {
                closed: closed.clone(),
                accepted: 0,
                rate: pcm.summary.sample_rate,
                channels: pcm.summary.channels,
            }))
        })
    }
    fn adts_frames() -> Vec<u8> {
        use rusty_aac::{AacEncoder, AacEncoderConfig, AdtsHeader, write_adts_header};
        let mut encoder = AacEncoder::new(AacEncoderConfig::default());
        let samples: Vec<f32> = (0..2048).map(|i| (i as f32 * 0.1).sin() * 0.1).collect();
        encoder.push_pcm(&samples, 1, 24_000).unwrap();
        encoder.finish();
        let packet = encoder.next_packet().unwrap();
        let mut bytes = write_adts_header(&AdtsHeader {
            object_type: 2,
            sample_rate: 24_000,
            channels: 1,
            frame_length: packet.data.len() + 7,
            header_len: 7,
        });
        bytes.extend(packet.data);
        bytes.repeat(40)
    }
    fn short_info(program: &Program) -> StreamInfo {
        StreamInfo {
            title: Some(program.title.clone()),
            station: program.key.station_id.clone(),
            mode: ProbeMode::TimeFree,
            starts_at: program.key.starts_at,
            ends_at: program.key.starts_at + chrono::Duration::milliseconds(200),
            position: Duration::ZERO,
        }
    }
    struct ScriptedSource {
        info: StreamInfo,
        bytes: Option<Vec<u8>>,
        hang_url: Option<String>,
        cancelled: Arc<dyn Fn() -> bool + Send + Sync>,
    }
    impl SegmentSource for ScriptedSource {
        fn info(&self) -> &StreamInfo {
            &self.info
        }
        fn set_cancellation(&mut self, cancelled: Arc<dyn Fn() -> bool + Send + Sync>) {
            self.cancelled = cancelled;
        }
        fn poll_segment(
            &mut self,
        ) -> Result<crate::api::stream::playback::Next, crate::api::stream::ProbeError> {
            if let Some(bytes) = self.bytes.take() {
                return Ok(crate::api::stream::playback::Next::Segment(
                    crate::api::stream::playback::Segment {
                        initialization: None,
                        duration: None,
                        bytes,
                        codec: Some("mp4a.40.2".into()),
                        discontinuity: false,
                        starts_at: Some(self.info.starts_at),
                    },
                ));
            }
            if let Some(url) = self.hang_url.clone() {
                let _ = crate::api::loopback_get(&url, self.cancelled.clone());
                return Err(crate::api::stream::ProbeError::Cancelled);
            }
            Ok(crate::api::stream::playback::Next::Pending(
                Duration::from_millis(50),
            ))
        }
    }
    #[test]
    fn dropping_service_closes_output_while_preparation_blocks_for_five_seconds() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (blocked_tx, blocked_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        let (unblocked_tx, unblocked_rx) = mpsc::channel();
        let opened = Arc::new(AtomicBool::new(false));
        let closed = Arc::new(AtomicBool::new(false));
        let p = program();
        let info = short_info(&p);
        let bytes = adts_frames();
        let mut service = Service::with_test_sink(
            move |_, _| {
                if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    return Ok(Box::new(ScriptedSource {
                        info: info.clone(),
                        bytes: Some(bytes.clone()),
                        hang_url: None,
                        cancelled: Arc::new(|| false),
                    }));
                }
                blocked_tx.send(()).unwrap();
                let _ = release_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(5));
                unblocked_tx.send(()).unwrap();
                Err(playback::PlaybackError::LoginRequired)
            },
            flag_sink(opened.clone(), closed.clone()),
        );
        service.select(&p, p.ends_at).unwrap();
        until(|| opened.load(Ordering::Acquire));
        assert!(!closed.load(Ordering::Acquire));
        service.select(&p, p.ends_at).unwrap();
        blocked_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let began = Instant::now();
        drop(service);
        assert!(closed.load(Ordering::Acquire));
        assert!(began.elapsed() < Duration::from_millis(1500));
        release_tx.send(()).unwrap();
        unblocked_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    }
    #[test]
    fn shutdown_during_hanging_http_response_releases_output_within_bound() {
        use std::{
            io::{ErrorKind, Read},
            net::TcpListener,
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let (accepted_tx, accepted_rx) = mpsc::channel();
        let (disconnect_tx, disconnect_rx) = mpsc::channel::<Result<Duration, &'static str>>();
        let server = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(3);
            let mut socket = loop {
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(error) if error.kind() == ErrorKind::WouldBlock => {
                        if Instant::now() >= deadline {
                            let _ = disconnect_tx.send(Err("client never connected"));
                            return;
                        }
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => {
                        let _ = disconnect_tx.send(Err("accept failed"));
                        return;
                    }
                }
            };
            socket.set_nonblocking(false).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_millis(200)))
                .unwrap();
            let mut request = Vec::new();
            let mut tmp = [0u8; 1024];
            let header_deadline = Instant::now() + Duration::from_secs(2);
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                if Instant::now() >= header_deadline {
                    let _ = disconnect_tx.send(Err("headers never arrived"));
                    return;
                }
                match socket.read(&mut tmp) {
                    Ok(0) => {
                        let _ = disconnect_tx.send(Err("closed before headers"));
                        return;
                    }
                    Ok(n) => request.extend_from_slice(&tmp[..n]),
                    Err(error)
                        if error.kind() == ErrorKind::WouldBlock
                            || error.kind() == ErrorKind::TimedOut => {}
                    Err(_) => {
                        let _ = disconnect_tx.send(Err("header read failed"));
                        return;
                    }
                }
            }
            if accepted_tx.send(()).is_err() {
                return;
            }
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let started = Instant::now();
            let aborted = match socket.read(&mut tmp) {
                Ok(0) => true,
                Ok(_) => false,
                Err(error) => {
                    error.kind() != ErrorKind::WouldBlock && error.kind() != ErrorKind::TimedOut
                }
            };
            let _ = disconnect_tx.send(if aborted {
                Ok(started.elapsed())
            } else {
                Err("hung response was not aborted")
            });
        });
        let opened = Arc::new(AtomicBool::new(false));
        let closed = Arc::new(AtomicBool::new(false));
        let p = program();
        let info = short_info(&p);
        let bytes = adts_frames();
        let url = format!("http://{address}/hang");
        let mut service = Service::with_test_sink(
            move |_, _| {
                Ok(Box::new(ScriptedSource {
                    info: info.clone(),
                    bytes: Some(bytes.clone()),
                    hang_url: Some(url.clone()),
                    cancelled: Arc::new(|| false),
                }))
            },
            flag_sink(opened.clone(), closed.clone()),
        );
        service.select(&p, p.ends_at).unwrap();
        let ready = Instant::now() + Duration::from_secs(3);
        let mut accepted = false;
        while !opened.load(Ordering::Acquire) || !accepted {
            assert!(
                Instant::now() < ready,
                "output or hanging request did not start"
            );
            if accepted_rx.try_recv().is_ok() {
                accepted = true;
            }
            thread::sleep(Duration::from_millis(5));
        }
        assert!(!closed.load(Ordering::Acquire));
        let began = Instant::now();
        drop(service);
        assert!(closed.load(Ordering::Acquire));
        assert!(began.elapsed() < Duration::from_millis(1500));
        let disconnected = disconnect_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let elapsed = disconnected.expect("in-flight response was not cancelled");
        assert!(elapsed < Duration::from_secs(2));
        server.join().unwrap();
    }
    #[test]
    fn failed_session_sets_stopped_and_publishes_the_window_lost_sentence() {
        use crate::api::stream::playback::{Next, Segment};
        struct FailAfter {
            info: StreamInfo,
            bytes: Option<Vec<u8>>,
        }
        impl SegmentSource for FailAfter {
            fn info(&self) -> &StreamInfo {
                &self.info
            }
            fn poll_segment(&mut self) -> Result<Next, crate::api::stream::ProbeError> {
                if let Some(bytes) = self.bytes.take() {
                    return Ok(Next::Segment(Segment {
                        initialization: None,
                        duration: None,
                        bytes,
                        codec: Some("mp4a.40.2".into()),
                        discontinuity: false,
                        starts_at: Some(self.info.starts_at),
                    }));
                }
                Err(crate::api::stream::ProbeError::WindowLost)
            }
        }
        struct HoldSink;
        impl crate::player::playback::AudioSink for HoldSink {
            fn enqueue(
                &mut self,
                pcm: &crate::player::aac::DecodedPcm,
                cursor: &mut usize,
            ) -> Result<bool, crate::player::output::OutputError> {
                *cursor = pcm.samples.len();
                Ok(true)
            }
            fn accepted(&self) -> u64 {
                0
            }
            fn summary(&self) -> crate::player::output::OutputSummary {
                crate::player::output::OutputSummary {
                    frames: 0,
                    underrun_frames: 0,
                    sample_rate: 48_000,
                    channels: 2,
                    sample_format: crate::player::output::OutputSampleFormat::F32,
                }
            }
            fn poll(&mut self) -> Result<bool, crate::player::output::OutputError> {
                Ok(false)
            }
            fn finish(&mut self) {}
        }
        let p = program();
        let info = short_info(&p);
        let bytes = adts_frames();
        let mut service = Service::with_test_sink(
            move |_, _| {
                Ok(Box::new(FailAfter {
                    info: info.clone(),
                    bytes: Some(bytes.clone()),
                }))
            },
            Arc::new(|_, _, _, _| Ok(Box::new(HoldSink))),
        );
        service.select(&p, p.ends_at).unwrap();
        let start = Instant::now();
        let mut last_notice = String::new();
        let update = loop {
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "failed session did not finish; last notice: {last_notice}"
            );
            let update = service.poll();
            if let Some(notice) = update.notice.clone() {
                last_notice = notice;
            }
            if update.ended && update.notice.is_some() {
                break update;
            }
            thread::sleep(Duration::from_millis(5));
        };
        assert!(update.stopped, "a failed session must set Update.stopped");
        assert_eq!(
            update.notice.as_deref(),
            Some("通信が長く途切れたため、続きを再生できなくなりました。番組を選び直してください")
        );
    }
}
