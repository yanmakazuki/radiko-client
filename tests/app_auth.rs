use radiko_client::{
    app::auth::{Service, State, Status},
    session::Membership,
};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
#[test]
fn checks_run_off_loop_and_only_latest_pending_refresh_is_executed() {
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
        let mut service = Service::with_check(move |cancelled| {
            let call = observed.fetch_add(1, Ordering::SeqCst);
            if call == 0 {
                entered.send(()).unwrap();
                gate.lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(3))
                    .unwrap();
                assert!(cancelled.load(Ordering::Acquire));
                Status::Expired
            } else {
                Status::Verified(Membership {
                    area_free: None,
                    time_free_30: Some(false),
                })
            }
        });
        let mut state = State::default();
        service.submit(state.begin().unwrap());
        seen.recv_timeout(Duration::from_secs(2)).unwrap();
        service.submit(state.begin().unwrap());
        service.submit(state.begin().unwrap());
        let began = Instant::now();
        assert!(service.poll().await.is_none());
        assert!(began.elapsed() < Duration::from_secs(1));
        release.send(()).unwrap();
        while !matches!(state.status, Status::Verified(_)) {
            assert!(began.elapsed() < Duration::from_secs(3));
            if let Some(event) = service.poll().await {
                state.apply(event);
                assert_ne!(state.status, Status::Expired);
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(state.status.detail().contains("未確認"));
        assert!(state.status.detail().contains("未加入"));
    });
}
#[test]
fn dropping_auth_service_signals_cancel_without_waiting_for_inflight_work() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    runtime.block_on(async {
        let (entered, seen) = mpsc::channel();
        let (finished, done) = mpsc::channel();
        let mut service = Service::with_check(move |cancelled| {
            entered.send(()).unwrap();
            let began = Instant::now();
            while !cancelled.load(Ordering::Acquire) {
                assert!(began.elapsed() < Duration::from_secs(3));
                std::thread::sleep(Duration::from_millis(2));
            }
            finished.send(()).unwrap();
            Status::NotChecked
        });
        let mut state = State::default();
        service.submit(state.begin().unwrap());
        seen.recv_timeout(Duration::from_secs(2)).unwrap();
        let began = Instant::now();
        drop(service);
        assert!(began.elapsed() < Duration::from_secs(1));
        done.recv_timeout(Duration::from_secs(2)).unwrap();
    });
}
#[test]
fn failed_worker_exits_checking_state_without_exposing_panic_text() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut service = Service::with_check(|_| panic!("synthetic failure"));
        let mut state = State::default();
        service.submit(state.begin().unwrap());
        let began = Instant::now();
        loop {
            assert!(began.elapsed() < Duration::from_secs(3));
            if let Some(event) = service.poll().await {
                state.apply(event);
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert_eq!(state.status, Status::WorkerFailed);
        assert!(!state.status.detail().contains("synthetic"));
    });
}

#[test]
fn owned_auth_cancellation_releases_the_worker_and_old_results_do_not_override_logout() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    runtime.block_on(async {
        let (entered, seen) = mpsc::channel();
        let mut service = Service::with_cancellable_check(move |cancel| {
            entered.send(cancel.clone()).unwrap();
            let began = Instant::now();
            while !cancel.load(Ordering::Acquire) {
                assert!(began.elapsed() < Duration::from_secs(2));
                std::thread::sleep(Duration::from_millis(1));
            }
            Status::CheckFailed(radiko_client::radiko::auth::AuthError::Cancelled)
        });
        let mut state = State::default();
        service.submit(state.begin().unwrap());
        let flag = seen.recv_timeout(Duration::from_secs(2)).unwrap();
        service.cancel();
        state.begin_logout().unwrap();
        assert!(flag.load(Ordering::Acquire));
        let began = Instant::now();
        let event = loop {
            if let Some(event) = service.poll().await {
                break event;
            }
            assert!(began.elapsed() < Duration::from_secs(2));
            tokio::time::sleep(Duration::from_millis(1)).await;
        };
        state.apply(event);
        assert_eq!(state.status, Status::LoggingOut);
    });
}
