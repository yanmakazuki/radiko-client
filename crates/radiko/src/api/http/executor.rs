//! One shared I/O driver; cancellation never creates request threads.
use super::ProbeError;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use std::{
    net::ToSocketAddrs,
    sync::{Arc, OnceLock},
};
use tokio::{
    runtime::{Builder, Runtime},
    sync::Semaphore,
};
type Lookup = dyn Fn(String) -> std::io::Result<Addrs> + Send + Sync;
pub(super) struct Resolver {
    slots: Arc<Semaphore>,
    lookup: Arc<Lookup>,
}
impl Resolve for Resolver {
    fn resolve(&self, name: Name) -> Resolving {
        let slots = self.slots.clone();
        let host = name.as_str().to_owned();
        let lookup = self.lookup.clone();
        Box::pin(async move {
            let permit = slots.acquire_owned().await?;
            let result = tokio::task::spawn_blocking(move || {
                // Keep the permit inside uncancellable OS DNS, even if its future is dropped.
                let _permit = permit;
                lookup(host)
            })
            .await?;
            Ok(result?)
        })
    }
}
pub(super) fn runtime() -> Result<&'static Runtime, ProbeError> {
    static RUNTIME: OnceLock<Option<Runtime>> = OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            Builder::new_current_thread()
                .enable_all()
                .max_blocking_threads(4)
                .build()
                .ok()
        })
        .as_ref()
        .ok_or(ProbeError::Network)
}
pub(super) fn resolver() -> Arc<Resolver> {
    static RESOLVER: OnceLock<Arc<Resolver>> = OnceLock::new();
    RESOLVER
        .get_or_init(|| {
            Arc::new(Resolver {
                slots: Arc::new(Semaphore::new(4)),
                lookup: Arc::new(|host| {
                    (host.as_str(), 0)
                        .to_socket_addrs()
                        .map(|addresses| Box::new(addresses) as Addrs)
                }),
            })
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    #[test]
    fn cancelled_dns_keeps_its_permit_until_os_work_finishes_and_does_not_queue_more_work() {
        let (entered, seen) = std::sync::mpsc::channel();
        let (release, gate) = std::sync::mpsc::channel();
        let gate = std::sync::Mutex::new(gate);
        let resolver = Arc::new(Resolver {
            slots: Arc::new(Semaphore::new(1)),
            lookup: Arc::new(move |_| {
                entered.send(()).unwrap();
                gate.lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(2))
                    .unwrap();
                Ok(Box::new(std::iter::empty()) as Addrs)
            }),
        });
        let rt = runtime().unwrap();
        let first = rt.spawn(resolver.resolve("first.invalid".parse().unwrap()));
        rt.block_on(async {
            let began = Instant::now();
            while seen.try_recv().is_err() {
                assert!(began.elapsed() < Duration::from_secs(2));
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        });
        first.abort();
        rt.block_on(async {
            let _ = first.await;
        });
        assert_eq!(resolver.slots.available_permits(), 0);
        let second = rt.spawn(resolver.resolve("second.invalid".parse().unwrap()));
        rt.block_on(async {
            tokio::time::sleep(Duration::from_millis(10)).await;
        });
        assert!(seen.try_recv().is_err());
        second.abort();
        rt.block_on(async {
            let _ = second.await;
        });
        release.send(()).unwrap();
        let began = Instant::now();
        while resolver.slots.available_permits() != 1 {
            assert!(began.elapsed() < Duration::from_secs(2));
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(seen.try_recv().is_err());
    }
}
