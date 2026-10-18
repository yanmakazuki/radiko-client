//! Bounded command handoff; the owning worker creates and drops CPAL on its own thread.
use crate::{
    domain::{BroadcastStatus, Program, broadcast_status},
    player::{
        output::StopToken,
        playback,
        seek::{Command, Controller, PlaybackRequest},
        state::PlaybackSnapshot,
    },
    radiko::{
        auth::MemberClient,
        stream::{
            ProbeMode,
            playback::{SegmentSource, StreamInfo, prepare_at_controlled},
        },
    },
    storage::SessionStore,
};
use chrono::{DateTime, FixedOffset};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex, mpsc},
    thread,
    time::Duration,
};

type Factory = Arc<
    dyn Fn(PlaybackRequest, StopToken) -> Result<Box<dyn SegmentSource>, playback::PlaybackError>
        + Send
        + Sync,
>;
#[derive(Default)]
pub struct Update {
    pub snapshot: Option<PlaybackSnapshot>,
    pub notice: Option<String>,
    pub ended: bool,
    pub final_diagnostics: Option<String>,
}
struct Running {
    stop: StopToken,
    commands: mpsc::SyncSender<Command>,
    handle: thread::JoinHandle<()>,
}
pub struct Service {
    factory: Factory,
    mailbox: Arc<Mutex<Update>>,
    running: Option<Running>,
    volume: u32,
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
                        crate::radiko::stream::ProbeError::Cancelled,
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
                    crate::radiko::stream::ProbeError::InvalidPosition,
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
        }
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
                *update = Update::default();
            }
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
                let result =
                    playback::play_session(idle, shutdown, &mut controls, move |snapshot| {
                        if let Ok(mut update) = progress.lock() {
                            update.snapshot = Some(snapshot);
                        }
                    });
                if let Ok(mut update) = finished.lock() {
                    update.ended = true;
                    if let Ok(report) = &result {
                        update.final_diagnostics = Some(crate::player::state::output_diagnostics(
                            report.output.as_ref(),
                            report.segments,
                        ));
                    }
                    // Preserve an undelivered snapshot so the UI can retain diagnostics on failure.
                    update.notice = Some(match result {
                        Ok(report) => match report.reason {
                            playback::EndReason::ProgramEnd => "番組終了".into(),
                            _ => "停止しました".into(),
                        },
                        Err(error) => format!("再生停止: {error}"),
                    });
                }
                // Output has already been dropped and ended is published. A new session waits
                // for this finite HTTP drain without blocking the UI or accumulating workers.
                controls.finish_preparation();
            });
            self.running = Some(Running {
                stop,
                commands,
                handle,
            });
        }
        self.command(Command::Volume(self.volume))?;
        self.command(Command::Select(target))
    }
    pub fn command(&mut self, command: Command) -> Result<(), &'static str> {
        if let Command::Volume(volume) = command {
            self.volume = volume.min(100);
            if self.running.is_none() {
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
            .map_err(|_| "再生操作の処理待ちです。少し待って再度操作してください")
    }
    pub fn stop(&self) {
        if let Some(running) = &self.running {
            running.stop.cancel();
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
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;
    fn program() -> Program {
        crate::radiko::catalog::parse_programs(
            include_bytes!("../../tests/fixtures/schedule.xml"),
            "TBS",
            crate::radiko::catalog::parse_day("20261015").unwrap(),
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
                crate::radiko::stream::ProbeError::Cancelled,
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
}
