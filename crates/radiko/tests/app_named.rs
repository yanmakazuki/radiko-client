use radiko::app::named::{Operation, Outcome, Service, ViewId};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
#[test]
fn local_io_never_blocks_poll_and_busy_commands_are_not_queued() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    runtime.block_on(async {
        let (entered, seen) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let gate = Mutex::new(gate);
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let mut service = Service::with_store(move |operation| {
            if count.fetch_add(1, Ordering::SeqCst) == 0 {
                entered.send(()).unwrap();
                gate.lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(3))
                    .unwrap();
            }
            Ok(match operation {
                Operation::Delete(name) => Outcome::Deleted(name),
                _ => Outcome::Listed(Default::default()),
            })
        });
        service
            .submit(ViewId(1), Operation::Delete("一件".into()))
            .unwrap();
        seen.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(service.writing());
        let began = Instant::now();
        assert!(service.poll().await.is_none());
        assert!(began.elapsed() < Duration::from_secs(1));
        assert!(
            service
                .submit(ViewId(2), Operation::Delete("削除してはいけない".into()))
                .is_err()
        );
        release.send(()).unwrap();
        loop {
            assert!(began.elapsed() < Duration::from_secs(3));
            if let Some(event) = service.poll().await {
                assert_eq!(event.view, ViewId(1));
                assert!(event.mutating);
                assert!(matches!(event.result,Ok(Outcome::Deleted(name)) if name=="一件"));
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(!service.writing());
        service.submit(ViewId(2), Operation::List).unwrap();
        loop {
            assert!(began.elapsed() < Duration::from_secs(3));
            if let Some(event) = service.poll().await {
                assert!(!event.mutating);
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    });
}
#[test]
fn dropping_a_view_service_does_not_cancel_an_already_accepted_mutation() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    runtime.block_on(async {
        let (entered, seen) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let gate = Mutex::new(gate);
        let completed = Arc::new(AtomicBool::new(false));
        let flag = completed.clone();
        let mut service = Service::with_store(move |_| {
            entered.send(()).unwrap();
            gate.lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(3))
                .unwrap();
            flag.store(true, Ordering::Release);
            Ok(Outcome::Deleted("条件".into()))
        });
        service
            .submit(ViewId(1), Operation::Delete("条件".into()))
            .unwrap();
        seen.recv_timeout(Duration::from_secs(2)).unwrap();
        drop(service);
        release.send(()).unwrap();
        let began = Instant::now();
        while !completed.load(Ordering::Acquire) {
            assert!(began.elapsed() < Duration::from_secs(3));
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    });
}
