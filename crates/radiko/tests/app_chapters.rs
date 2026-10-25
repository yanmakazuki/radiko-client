use radiko::{
    api::{
        ChapterError,
        catalog::{parse_day, parse_programs},
        stream::chapters::{self, BindingIssue, Outcome, Report},
    },
    app::chapters::{Error, Event, Service, State, Status},
    domain::Program,
};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
fn program() -> Program {
    parse_programs(
        include_bytes!("../../../tests/fixtures/schedule.xml"),
        "TBS",
        parse_day("20261015").unwrap(),
    )
    .unwrap()
    .programs
    .remove(0)
}
fn report(target: &Program, outcome: Outcome) -> Report {
    Report {
        station: target.key.station_id.clone(),
        title: Some(target.title.clone()),
        starts_at: target.key.starts_at,
        ends_at: target.ends_at,
        outcome,
    }
}
#[test]
fn reopening_same_slot_discards_delivered_old_success_and_close_clears_candidates() {
    let mut state = State::default();
    let target = program();
    let old = state.open(target.clone()).unwrap();
    let new = state.open(target.clone()).unwrap();
    state.apply(Event {
        id: old,
        key: target.key.clone(),
        result: Ok(report(&target, Outcome::Unavailable)),
    });
    assert_eq!(state.status, Status::Loading);
    state.apply(Event {
        id: new,
        key: target.key.clone(),
        result: Ok(report(
            &target,
            Outcome::Candidates(radiko::domain::ChapterSet {
                chapters: vec![],
                music_count: 1,
                invalid_count: 1,
            }),
        )),
    });
    assert_eq!(state.status, Status::Ready);
    assert!(state.jump_refusal().contains("未検証"));
    state.close();
    assert!(state.candidates.is_none());
    assert!(state.target.is_none());
    state.apply(Event {
        id: new,
        key: target.key.clone(),
        result: Ok(report(&target, Outcome::Unavailable)),
    });
    assert_eq!(state.status, Status::NotRequested);
}
#[test]
fn selected_key_and_response_start_end_are_checked_without_using_title_or_program_id() {
    let mut state = State::default();
    let first = program();
    let mut split = first.clone();
    split.key.starts_at += chrono::Duration::hours(1);
    split.ends_at += chrono::Duration::hours(1);
    let id = state.open(split.clone()).unwrap();
    state.apply(Event {
        id,
        key: first.key.clone(),
        result: Ok(report(&first, Outcome::Unavailable)),
    });
    assert_eq!(state.status, Status::Loading);
    for changed in [0, 1, 2] {
        let mut wrong = report(&split, Outcome::Unavailable);
        match changed {
            0 => wrong.station = "OBS".into(),
            1 => wrong.starts_at -= chrono::Duration::seconds(1),
            _ => wrong.ends_at += chrono::Duration::seconds(1),
        }
        state.apply(Event {
            id,
            key: split.key.clone(),
            result: Ok(wrong),
        });
        assert_eq!(state.status, Status::Unresolved(BindingIssue::SlotMismatch));
        assert!(state.candidates.is_none());
    }
}
#[test]
fn empty_success_unresolved_parse_failure_and_network_failure_remain_distinct() {
    let target = program();
    let mut state = State::default();
    let id = state.open(target.clone()).unwrap();
    for (outcome, expected) in [
        (Outcome::Unavailable, Status::Unavailable),
        (
            Outcome::Unresolved(BindingIssue::EventMetadataMissing),
            Status::Unresolved(BindingIssue::EventMetadataMissing),
        ),
        (
            Outcome::Unresolved(BindingIssue::OriginMismatch),
            Status::Unresolved(BindingIssue::OriginMismatch),
        ),
        (
            Outcome::Unresolved(BindingIssue::SlotMismatch),
            Status::Unresolved(BindingIssue::SlotMismatch),
        ),
    ] {
        state.apply(Event {
            id,
            key: target.key.clone(),
            result: Ok(report(&target, outcome)),
        });
        assert_eq!(state.status, expected);
    }
    for error in [
        chapters::Error::Parse(ChapterError::InvalidJson),
        chapters::Error::Request(radiko::api::stream::ProbeError::NotFound),
    ] {
        state.apply(Event {
            id,
            key: target.key.clone(),
            result: Err(Error::Acquire(error)),
        });
        assert!(matches!(state.status, Status::Failed(_)));
        assert!(state.label().contains("失敗"));
    }
}
#[test]
fn acquisition_coalesces_to_latest_request_and_refresh_refetches_same_slot() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    runtime.block_on(async {
        let first = program();
        let mut skipped = first.clone();
        skipped.key.station_id = "OBS".into();
        let mut last = first.clone();
        last.key.starts_at += chrono::Duration::hours(1);
        last.ends_at += chrono::Duration::hours(1);
        let (entered, seen) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let gate = Mutex::new(gate);
        let calls = Arc::new(Mutex::new(vec![]));
        let observed = calls.clone();
        let active = Arc::new(AtomicUsize::new(0));
        let count = active.clone();
        let original = first.key.clone();
        let mut service = Service::with_source(move |target, cancel| {
            assert_eq!(count.fetch_add(1, Ordering::SeqCst), 0);
            observed.lock().unwrap().push(target.key.clone());
            if target.key == original {
                entered.send(()).unwrap();
                gate.lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(3))
                    .unwrap();
                assert!(cancel.load(Ordering::Acquire));
            }
            count.fetch_sub(1, Ordering::SeqCst);
            Ok(report(target, Outcome::Unavailable))
        });
        let mut state = State::default();
        let id = state.open(first.clone()).unwrap();
        service.submit(id, first.clone());
        seen.recv_timeout(Duration::from_secs(2)).unwrap();
        let id = state.open(skipped.clone()).unwrap();
        service.submit(id, skipped);
        let id = state.open(last.clone()).unwrap();
        service.submit(id, last.clone());
        assert!(service.poll().await.is_none());
        release.send(()).unwrap();
        let began = Instant::now();
        while state.status != Status::Unavailable {
            assert!(began.elapsed() < Duration::from_secs(3));
            if let Some(event) = service.poll().await {
                state.apply(event);
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert_eq!(
            *calls.lock().unwrap(),
            vec![first.key.clone(), last.key.clone()]
        );
        let id = state.open(last.clone()).unwrap();
        service.submit(id, last.clone());
        while state.status != Status::Unavailable {
            assert!(began.elapsed() < Duration::from_secs(3));
            if let Some(event) = service.poll().await {
                state.apply(event);
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert_eq!(calls.lock().unwrap().len(), 3);
    });
}
#[test]
fn closing_cancels_active_and_pending_requests_without_waiting_for_http() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    runtime.block_on(async {
        let (entered, seen) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let gate = Mutex::new(gate);
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let mut service = Service::with_source(move |target, cancelled| {
            observed.fetch_add(1, Ordering::SeqCst);
            entered.send(()).unwrap();
            gate.lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(3))
                .unwrap();
            assert!(cancelled.load(Ordering::Acquire));
            Ok(report(target, Outcome::Unavailable))
        });
        let target = program();
        let mut state = State::default();
        let id = state.open(target.clone()).unwrap();
        service.submit(id, target.clone());
        seen.recv_timeout(Duration::from_secs(2)).unwrap();
        let id = state.open(target.clone()).unwrap();
        service.submit(id, target);
        let began = Instant::now();
        state.close();
        service.cancel();
        assert!(service.poll().await.is_none());
        assert!(began.elapsed() < Duration::from_secs(1));
        release.send(()).unwrap();
        loop {
            assert!(began.elapsed() < Duration::from_secs(3));
            if let Some(event) = service.poll().await {
                state.apply(event);
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert_eq!(state.status, Status::NotRequested);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    });
}

#[test]
fn dropping_service_signals_cancellation_without_waiting_for_blocking_work() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    runtime.block_on(async {
        let (entered, seen) = mpsc::channel();
        let (done, finished) = mpsc::channel();
        let mut service = Service::with_source(move |_, cancelled| {
            entered.send(()).unwrap();
            let began = Instant::now();
            while !cancelled.load(Ordering::Acquire) {
                assert!(began.elapsed() < Duration::from_secs(3));
                std::thread::sleep(Duration::from_millis(2));
            }
            done.send(()).unwrap();
            Err(chapters::Error::Cancelled)
        });
        let target = program();
        let mut state = State::default();
        let id = state.open(target.clone()).unwrap();
        service.submit(id, target);
        seen.recv_timeout(Duration::from_secs(2)).unwrap();
        let began = Instant::now();
        drop(service);
        assert!(began.elapsed() < Duration::from_secs(1));
        finished.recv_timeout(Duration::from_secs(2)).unwrap();
    });
}

#[test]
fn worker_panic_is_reported_as_failure_instead_of_success_or_an_endless_spinner() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut service = Service::with_source(|_, _| panic!("synthetic worker failure"));
        let target = program();
        let mut state = State::default();
        let id = state.open(target.clone()).unwrap();
        service.submit(id, target);
        let began = Instant::now();
        loop {
            assert!(began.elapsed() < Duration::from_secs(3));
            if let Some(event) = service.poll().await {
                state.apply(event);
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert!(matches!(&state.status, Status::Failed(message) if message.contains("異常終了")));
        assert!(state.candidates.is_none());
    });
}

#[test]
fn cancellable_chapter_source_owns_the_flag_and_closing_releases_in_flight_work() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    runtime.block_on(async {
        let (entered, seen) = mpsc::channel();
        let (done, finished) = mpsc::channel();
        let mut service = Service::with_cancellable_source(move |_, cancel| {
            entered.send(cancel.clone()).unwrap();
            let began = Instant::now();
            while !cancel.load(Ordering::Acquire) {
                assert!(began.elapsed() < Duration::from_secs(2));
                std::thread::sleep(Duration::from_millis(1));
            }
            done.send(()).unwrap();
            Err(chapters::Error::Cancelled)
        });
        let target = program();
        let mut state = State::default();
        let id = state.open(target.clone()).unwrap();
        service.submit(id, target);
        let flag = seen.recv_timeout(Duration::from_secs(2)).unwrap();
        service.cancel();
        state.close();
        assert!(flag.load(Ordering::Acquire));
        finished.recv_timeout(Duration::from_secs(2)).unwrap();
    });
}

#[test]
fn aligned_binding_allows_only_the_same_slot_and_is_cleared_on_refresh() {
    use chapters::EventBinding;
    let target = program();
    let binding = || {
        EventBinding::matching_origin(
            target.key.clone(),
            target.ends_at,
            "SYNTHETIC-event".into(),
            target.key.starts_at,
        )
        .unwrap()
    };
    assert!(
        EventBinding::matching_origin(
            target.key.clone(),
            target.ends_at,
            "SYNTHETIC-event".into(),
            target.key.starts_at - chrono::Duration::seconds(1)
        )
        .is_none()
    );
    assert!(
        EventBinding::matching_origin(
            target.key.clone(),
            target.ends_at,
            "../invalid".into(),
            target.key.starts_at
        )
        .is_none()
    );
    let set = || {
        radiko::api::parse_chapters(
            include_bytes!("../../../tests/fixtures/chapters.json"),
            Duration::from_secs(90),
            0.0,
        )
        .unwrap()
    };
    let mut state = State::default();
    let id = state.open(target.clone()).unwrap();
    state.apply(Event {
        id,
        key: target.key.clone(),
        result: Ok(report(&target, Outcome::Aligned(set(), binding()))),
    });
    let (bound, position) = state.jump(0).unwrap();
    assert_eq!(bound.key, target.key);
    assert_eq!(
        position,
        state.candidates.as_ref().unwrap().chapters[0].start
    );
    assert!(state.jump(usize::MAX).is_err());
    state.candidates.as_mut().unwrap().chapters[0].end = Duration::from_secs(86400);
    assert!(state.jump(0).is_err());
    let next = state.open(target.clone()).unwrap();
    assert!(state.jump(0).is_err());
    state.apply(Event {
        id,
        key: target.key.clone(),
        result: Ok(report(&target, Outcome::Aligned(set(), binding()))),
    });
    assert_eq!(state.status, Status::Loading);
    let mut other = target.clone();
    other.key.station_id = "OBS".into();
    let wrong = EventBinding::matching_origin(
        other.key,
        other.ends_at,
        "SYNTHETIC-event".into(),
        target.key.starts_at,
    )
    .unwrap();
    state.apply(Event {
        id: next,
        key: target.key.clone(),
        result: Ok(report(&target, Outcome::Aligned(set(), wrong))),
    });
    assert_eq!(state.status, Status::Unresolved(BindingIssue::SlotMismatch));
    assert!(state.jump(0).is_err());
}
