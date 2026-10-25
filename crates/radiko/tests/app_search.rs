use chrono::NaiveDate;
use radiko::{
    api::{
        catalog::{CatalogError, ProgramList, parse_day, parse_programs},
        stream::{ProbeMode, playback::StreamInfo},
    },
    app::{
        AppState, SearchStatus,
        search::{Error, Event, Progress, Service},
    },
    catalog::{self, Failure, Report, Source},
    domain::{
        BroadcastRange, Station,
        search::{Filter, Scope, SearchCriteria},
    },
    player::{
        output::OutputSummary,
        state::{PlaybackSnapshot, PlaybackState},
    },
};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
fn criteria(station: &str) -> SearchCriteria {
    SearchCriteria::new(
        Scope::Station(station.into()),
        BroadcastRange::single(parse_day("20261015").unwrap()),
        Filter::default(),
    )
    .unwrap()
}
fn list(station: &str, day: NaiveDate) -> ProgramList {
    let date = day.format("%Y%m%d");
    let xml = format!(
        "<radiko><stations><station id=\"{station}\"><name>{station}</name><progs><prog id=\"same\" ft=\"{date}060000\" to=\"{date}070000\"><title>合成番組</title></prog></progs></station></stations></radiko>"
    );
    parse_programs(xml.as_bytes(), station, day).unwrap()
}
fn report(list: ProgramList) -> Report {
    Report {
        areas: vec![],
        stations: vec![],
        lists: vec![list],
        area_failures: vec![],
        schedule_failures: vec![],
    }
}
#[test]
fn draft_edit_cancel_and_stale_results_never_change_committed_data_or_playback() {
    let mut app = AppState::new(criteria("TBS"));
    let slot = list("TBS", parse_day("20261015").unwrap());
    let p = &slot.programs[0];
    app.playback = Some(PlaybackSnapshot {
        pipeline: None,
        info: StreamInfo {
            title: Some("再生中".into()),
            station: "OBS".into(),
            mode: ProbeMode::Live,
            starts_at: p.key.starts_at,
            ends_at: p.ends_at,
            position: Duration::ZERO,
        },
        state: PlaybackState::Playing,
        pending: None,
        volume: 25,
        position: None,
        output: Some(OutputSummary {
            frames: 1024,
            underrun_frames: 0,
            sample_rate: 48000,
            channels: 2,
        }),
        segments: 1,
    });
    app.begin_edit();
    app.edit_keyword(Some("入力中".into())).unwrap();
    assert_eq!(app.committed.filter().keyword(), None);
    app.cancel_edit();
    assert!(app.draft.is_none());
    let first = app.commit().unwrap();
    app.apply(Event::Progress {
        id: first,
        progress: Progress {
            phase: "番組表",
            target: "TBS".into(),
            day: Some(slot.broadcast_day),
            completed: 1,
            total: 1,
            failed: 0,
            programs: slot.programs.clone(),
        },
    });
    assert_eq!(app.visible.len(), 1);
    let second = app.commit().unwrap();
    assert!(app.programs.is_empty());
    app.apply(Event::Finished {
        id: first,
        result: Ok(report(slot)),
    });
    assert!(app.programs.is_empty());
    assert_eq!(app.status, SearchStatus::Loading);
    app.apply(Event::Finished {
        id: second,
        result: Err(Error::Catalog(CatalogError::Network)),
    });
    assert_eq!(app.status, SearchStatus::Failed);
    assert!(app.programs.is_empty());
    assert_eq!(app.playback.as_ref().unwrap().info.station, "OBS");
    assert_eq!(
        app.playback
            .as_ref()
            .unwrap()
            .output
            .as_ref()
            .unwrap()
            .frames,
        1024
    );
}
#[test]
fn partial_acquisition_remains_partial_even_when_current_filter_has_no_matches() {
    let mut c = criteria("TBS");
    c = SearchCriteria::new(
        c.scope().clone(),
        c.range().clone(),
        c.filter().with_keyword(Some("一致しない".into())).unwrap(),
    )
    .unwrap();
    let mut app = AppState::new(c);
    let id = app.commit().unwrap();
    let mut r = report(list("TBS", parse_day("20261015").unwrap()));
    r.schedule_failures.push(Failure {
        target: "OBS".into(),
        day: Some(parse_day("20261015").unwrap()),
        error: CatalogError::Network,
    });
    app.apply(Event::Finished { id, result: Ok(r) });
    assert_eq!(app.status, SearchStatus::Partial);
    assert_eq!(app.programs.len(), 1);
    assert!(app.visible.is_empty());
    assert_eq!(app.schedule_failures.len(), 1);
}
struct Mock {
    entered: Arc<AtomicBool>,
    release: Arc<AtomicBool>,
    calls: Arc<Mutex<Vec<String>>>,
}
impl Source for Mock {
    fn stations(&self, _: &str) -> Result<Vec<Station>, CatalogError> {
        panic!("station-only search must not discover regions")
    }
    fn programs(&self, station: &str, day: NaiveDate) -> Result<ProgramList, CatalogError> {
        self.calls.lock().unwrap().push(station.into());
        if station == "TBS" {
            self.entered.store(true, Ordering::Release);
            while !self.release.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        Ok(list(station, day))
    }
}
#[test]
fn newer_search_cancels_old_work_coalesces_pending_requests_and_ignores_stale_completion() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    runtime.block_on(async {
        let entered = Arc::new(AtomicBool::new(false));
        let release = Arc::new(AtomicBool::new(false));
        let calls = Arc::new(Mutex::new(vec![]));
        let (e, r, c) = (entered.clone(), release.clone(), calls.clone());
        let mut service = Service::new(move || {
            Ok(Mock {
                entered: e.clone(),
                release: r.clone(),
                calls: c.clone(),
            })
        });
        let mut app = AppState::new(criteria("TBS"));
        let id = app.commit().unwrap();
        service.submit(id, app.committed.clone());
        let began = Instant::now();
        while !entered.load(Ordering::Acquire) {
            assert!(began.elapsed() < Duration::from_secs(3));
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        for station in ["OBS", "RN1"] {
            app.committed = criteria(station);
            let id = app.commit().unwrap();
            service.submit(id, app.committed.clone());
        }
        assert_eq!(calls.lock().unwrap().len(), 1);
        release.store(true, Ordering::Release);
        while app.status == SearchStatus::Loading {
            for event in service.poll().await {
                app.apply(event);
            }
            assert!(began.elapsed() < Duration::from_secs(3));
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        assert_eq!(app.status, SearchStatus::Ready);
        assert_eq!(app.programs[0].key.station_id, "RN1");
        assert_eq!(*calls.lock().unwrap(), ["TBS", "RN1"]);
    });
}
#[test]
fn cancellation_stops_unstarted_catalog_jobs_and_does_not_report_an_incomplete_run_as_success() {
    struct Cancel<'a>(&'a AtomicBool, &'a std::sync::atomic::AtomicUsize);
    impl Source for Cancel<'_> {
        fn stations(&self, _: &str) -> Result<Vec<Station>, CatalogError> {
            panic!()
        }
        fn programs(&self, station: &str, day: NaiveDate) -> Result<ProgramList, CatalogError> {
            self.1.fetch_add(1, Ordering::SeqCst);
            self.0.store(true, Ordering::Release);
            Ok(list(station, day))
        }
    }
    let cancelled = AtomicBool::new(false);
    let calls = std::sync::atomic::AtomicUsize::new(0);
    let source = Cancel(&cancelled, &calls);
    let range = BroadcastRange::new(
        parse_day("20261013").unwrap(),
        parse_day("20261016").unwrap(),
    )
    .unwrap();
    assert!(matches!(
        catalog::acquire_stations_controlled(
            &source,
            &["TBS".into(), "OBS".into()],
            &range,
            &cancelled,
            |_| panic!("cancelled progress must not publish")
        ),
        Err(CatalogError::Cancelled)
    ));
    assert!((1..=4).contains(&calls.load(Ordering::SeqCst)));
    let before = calls.load(Ordering::SeqCst);
    assert!(matches!(
        catalog::acquire_stations_controlled(&source, &["TBS".into()], &range, &cancelled, |_| {}),
        Err(CatalogError::Cancelled)
    ));
    assert_eq!(calls.load(Ordering::SeqCst), before);
}

#[test]
fn cancellable_factory_receives_the_search_generation_and_releases_the_next_request() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    runtime.block_on(async {
        let (entered, seen) = std::sync::mpsc::channel();
        let mut service = Service::new_cancellable(
            move |cancel| -> Result<radiko::api::catalog::CatalogClient, CatalogError> {
                entered.send(cancel.clone()).unwrap();
                let began = Instant::now();
                while !cancel.load(Ordering::Acquire) {
                    assert!(began.elapsed() < Duration::from_secs(3));
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(CatalogError::Cancelled)
            },
        );
        service.submit(radiko::app::search::SearchId(1), criteria("TBS"));
        let first = seen.recv_timeout(Duration::from_secs(2)).unwrap();
        service.submit(radiko::app::search::SearchId(2), criteria("OBS"));
        assert!(first.load(Ordering::Acquire));
        let began = Instant::now();
        let second = loop {
            service.poll().await;
            if let Ok(flag) = seen.try_recv() {
                break flag;
            }
            assert!(began.elapsed() < Duration::from_secs(2));
            tokio::time::sleep(Duration::from_millis(1)).await;
        };
        assert!(!second.load(Ordering::Acquire));
        drop(service);
        assert!(second.load(Ordering::Acquire));
    });
}

#[test]
fn malformed_slot_provenance_keeps_valid_slots_and_clears_on_new_search() {
    let mut app = AppState::new(criteria("TBS"));
    let id = app.commit().unwrap();
    let day = parse_day("20261015").unwrap();
    let mut slots = list("TBS", day);
    slots.invalid_count = 2;
    app.apply(Event::Finished {
        id,
        result: Ok(report(slots)),
    });
    assert_eq!(app.status, SearchStatus::Partial);
    assert_eq!(app.programs.len(), 1);
    assert_eq!(app.invalid_count, 2);
    assert_eq!(app.invalid_schedules.len(), 1);
    assert_eq!(app.invalid_schedules[0].station_id, "TBS");
    assert_eq!(app.invalid_schedules[0].day, day);
    assert_eq!(app.invalid_schedules[0].count, 2);
    app.commit().unwrap();
    assert!(app.invalid_schedules.is_empty());
    app.apply(Event::Finished {
        id,
        result: Ok(report(list("OBS", day))),
    });
    assert!(app.invalid_schedules.is_empty());
    assert_eq!(app.status, SearchStatus::Loading);
}
