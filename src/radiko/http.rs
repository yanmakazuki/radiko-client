//! サイズ・時間・再試行回数を制限するHTTP処理。生のエラー・URL・本文を外部へ返さない。
use super::stream::ProbeError;
mod executor;
use reqwest::{
    Client, Url,
    header::{HeaderMap, LOCATION, RANGE},
    redirect::Policy,
};
use std::{
    future::{Future, poll_fn},
    sync::Arc,
    task::Poll,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub(crate) struct Http {
    client: Client,
    cancelled: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
}
pub(crate) trait Fetch {
    fn get(
        &self,
        url: Url,
        headers: HeaderMap,
        limit: usize,
        range: Option<(u64, u64)>,
        redirects: bool,
    ) -> Result<Reply, ProbeError>;
}
pub(crate) struct Reply {
    pub headers: HeaderMap,
    pub bytes: Vec<u8>,
    pub url: Url,
}

// Authentication bodies can contain reusable credentials. Deliberately no Debug.
pub(crate) struct RawReply {
    pub status: u16,
    pub bytes: Vec<u8>,
}

pub(crate) fn trusted(url: &Url) -> bool {
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port_or_known_default() == Some(443)
        && url.host_str().is_some_and(|host| {
            ["radiko.jp", "radiko-cf.com", "smartstream.ne.jp"]
                .iter()
                .any(|suffix| host == *suffix || host.ends_with(&format!(".{suffix}")))
        })
}

impl Http {
    pub fn new() -> Result<Self, ProbeError> {
        executor::runtime()?;
        let client = Client::builder()
            .dns_resolver(executor::resolver())
            .https_only(true)
            .redirect(Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(20))
            .user_agent(concat!("radiko-client/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| ProbeError::Network)?;
        Ok(Self {
            client,
            cancelled: None,
        })
    }
    pub(crate) fn set_cancellation(&mut self, cancelled: Arc<dyn Fn() -> bool + Send + Sync>) {
        self.cancelled = Some(cancelled);
    }
    /// No redirects or retries; callers own authentication-specific status handling.
    pub(crate) fn request_once(
        &self,
        url: Url,
        headers: HeaderMap,
        form: Option<&[(&str, &str)]>,
        limit: usize,
    ) -> Result<RawReply, ProbeError> {
        self.check_cancelled()?;
        if !trusted(&url) {
            return Err(ProbeError::UnsafeUrl);
        }
        let request = match form {
            Some(fields) => self.client.post(url).headers(headers).form(fields),
            None => self.client.get(url).headers(headers),
        };
        let mut response = self
            .run(|| request.send())?
            .map_err(|_| ProbeError::Network)?;
        let status = response.status().as_u16();
        let bytes = if status == 200 {
            if response.content_length().is_some_and(|n| n > limit as u64) {
                return Err(ProbeError::TooLarge);
            }
            self.response_bytes(&mut response, limit)?
        } else {
            vec![]
        };
        Ok(RawReply { status, bytes })
    }
    fn response_bytes(
        &self,
        response: &mut reqwest::Response,
        limit: usize,
    ) -> Result<Vec<u8>, ProbeError> {
        self.read_chunks(
            || {
                let response = &mut *response;
                self.run(move || response.chunk())?
                    .map_err(|_| ProbeError::Network)
            },
            limit,
        )
    }
    fn check_cancelled(&self) -> Result<(), ProbeError> {
        if self.cancelled.as_ref().is_some_and(|check| check()) {
            Err(ProbeError::Cancelled)
        } else {
            Ok(())
        }
    }
    /// Runs on the existing acquisition thread; a dropped future cancels socket I/O.
    fn run<T, F: Future<Output = T>>(&self, make: impl FnOnce() -> F) -> Result<T, ProbeError> {
        executor::runtime()?.block_on(async {
            self.check_cancelled()?;
            let mut future = Box::pin(make());
            let mut tick = Box::pin(tokio::time::sleep(Duration::ZERO));
            poll_fn(|cx| {
                if let Err(error) = self.check_cancelled() {
                    return Poll::Ready(Err(error));
                }
                if let Poll::Ready(value) = future.as_mut().poll(cx) {
                    return Poll::Ready(self.check_cancelled().map(|()| value));
                }
                if tick.as_mut().poll(cx).is_ready() {
                    tick.as_mut()
                        .reset(tokio::time::Instant::now() + Duration::from_millis(10));
                    cx.waker().wake_by_ref();
                }
                Poll::Pending
            })
            .await
        })
    }
    fn read_chunks<B: AsRef<[u8]>>(
        &self,
        mut next: impl FnMut() -> Result<Option<B>, ProbeError>,
        limit: usize,
    ) -> Result<Vec<u8>, ProbeError> {
        let mut bytes = Vec::new();
        loop {
            self.check_cancelled()?;
            let chunk = next()?;
            self.check_cancelled()?;
            let Some(chunk) = chunk else {
                return Ok(bytes);
            };
            let chunk = chunk.as_ref();
            if chunk.len() > limit.saturating_sub(bytes.len()) {
                return Err(ProbeError::TooLarge);
            }
            bytes.extend_from_slice(chunk);
        }
    }
    pub(crate) fn retry_wait(&self, delay: Duration) -> Result<(), ProbeError> {
        let start = std::time::Instant::now();
        loop {
            self.check_cancelled()?;
            let remaining = delay.saturating_sub(start.elapsed());
            if remaining.is_zero() {
                return Ok(());
            }
            std::thread::sleep(remaining.min(Duration::from_millis(10)));
        }
    }
}
impl Fetch for Http {
    fn get(
        &self,
        url: Url,
        headers: HeaderMap,
        limit: usize,
        range: Option<(u64, u64)>,
        redirects: bool,
    ) -> Result<Reply, ProbeError> {
        let mut url = url;
        for _ in 0..=5 {
            self.check_cancelled()?;
            if !trusted(&url) {
                return Err(ProbeError::UnsafeUrl);
            }
            let mut retries = 0;
            let mut response = loop {
                self.check_cancelled()?;
                let mut request = self.client.get(url.clone()).headers(headers.clone());
                if let Some((offset, length)) = range {
                    let end = offset
                        .checked_add(length)
                        .and_then(|n| n.checked_sub(1))
                        .ok_or(ProbeError::InvalidPlaylist)?;
                    if length == 0 || length > limit as u64 {
                        return Err(ProbeError::TooLarge);
                    }
                    request = request.header(RANGE, format!("bytes={offset}-{end}"));
                }
                let response = self.run(|| request.send())?;
                self.check_cancelled()?;
                let transient = match &response {
                    Err(_) => true,
                    Ok(r) => r.status().is_server_error() || r.status().as_u16() == 408,
                };
                if transient && retries < 3 {
                    let jitter = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map(|n| n.subsec_millis() % 50)
                        .unwrap_or(0);
                    self.retry_wait(Duration::from_millis(
                        100 * (1 << retries) + u64::from(jitter),
                    ))?;
                    retries += 1;
                    continue;
                }
                break response.map_err(|_| ProbeError::Network)?;
            };
            let status = response.status().as_u16();
            if matches!(status, 301 | 302 | 303 | 307 | 308) && redirects {
                let location = response
                    .headers()
                    .get(LOCATION)
                    .and_then(|h| h.to_str().ok())
                    .ok_or(ProbeError::UnsafeUrl)?;
                url = url.join(location).map_err(|_| ProbeError::UnsafeUrl)?;
                continue;
            }
            match status {
                200 if range.is_none() => (),
                206 if range.is_some() => (),
                401 => return Err(ProbeError::Authentication),
                403 => return Err(ProbeError::Denied),
                404 => return Err(ProbeError::NotFound),
                429 => return Err(ProbeError::RateLimited),
                other => return Err(ProbeError::Http(other)),
            }
            if response.content_length().is_some_and(|n| n > limit as u64) {
                return Err(ProbeError::TooLarge);
            }
            if let Some((offset, length)) = range {
                let end = offset
                    .checked_add(length)
                    .and_then(|n| n.checked_sub(1))
                    .ok_or(ProbeError::InvalidPlaylist)?;
                let prefix = format!("bytes {offset}-{end}/");
                if !response
                    .headers()
                    .get("content-range")
                    .and_then(|h| h.to_str().ok())
                    .is_some_and(|h| h.starts_with(&prefix))
                {
                    return Err(ProbeError::InvalidPlaylist);
                }
            }
            let headers = response.headers().clone();
            let bytes = self.response_bytes(&mut response, limit)?;
            if range.is_some_and(|(_, length)| bytes.len() as u64 != length) {
                return Err(ProbeError::InvalidPlaylist);
            }
            return Ok(Reply {
                headers,
                bytes,
                url,
            });
        }
        Err(ProbeError::UnsafeUrl)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancelled_auth_requests_never_start_get_or_post_and_unsafe_targets_are_rejected() {
        let mut http = Http::new().unwrap();
        http.set_cancellation(Arc::new(|| true));
        for form in [None, Some(&[("pass", "SYNTHETIC_PASSWORD")][..])] {
            assert!(matches!(
                http.request_once(
                    Url::parse("https://radiko.jp/never-requested").unwrap(),
                    HeaderMap::new(),
                    form,
                    65536
                ),
                Err(ProbeError::Cancelled)
            ));
        }
        let ordinary = Http::new().unwrap();
        assert!(matches!(
            ordinary.request_once(
                Url::parse("https://example.invalid/never-requested").unwrap(),
                HeaderMap::new(),
                None,
                65536
            ),
            Err(ProbeError::UnsafeUrl)
        ));
    }
    #[test]
    fn cancellation_drops_an_in_flight_future_without_waiting_for_its_timeout() {
        use std::sync::atomic::{AtomicBool, Ordering};
        struct Pending(Arc<AtomicBool>);
        impl Future for Pending {
            type Output = ();
            fn poll(self: std::pin::Pin<&mut Self>, _: &mut std::task::Context<'_>) -> Poll<()> {
                Poll::Pending
            }
        }
        impl Drop for Pending {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }
        let flag = Arc::new(AtomicBool::new(false));
        let dropped = Arc::new(AtomicBool::new(false));
        let check = flag.clone();
        let mut http = Http::new().unwrap();
        http.set_cancellation(Arc::new(move || check.load(Ordering::Acquire)));
        let (started, entered) = std::sync::mpsc::channel();
        let cancel = std::thread::spawn(move || {
            entered.recv_timeout(Duration::from_secs(2)).unwrap();
            flag.store(true, Ordering::Release);
        });
        let began = std::time::Instant::now();
        assert_eq!(
            http.run(|| {
                started.send(()).unwrap();
                Pending(dropped.clone())
            }),
            Err(ProbeError::Cancelled)
        );
        cancel.join().unwrap();
        assert!(began.elapsed() < Duration::from_secs(1));
        assert!(dropped.load(Ordering::Acquire));
    }
    #[test]
    fn cancelling_one_request_does_not_cancel_another_on_the_shared_driver() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let flag = Arc::new(AtomicBool::new(false));
        let check = flag.clone();
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let first_barrier = barrier.clone();
        let mut first = Http::new().unwrap();
        first.set_cancellation(Arc::new(move || check.load(Ordering::Acquire)));
        let first = std::thread::spawn(move || {
            first.run(|| async {
                tokio::time::timeout(Duration::from_secs(2), first_barrier.wait())
                    .await
                    .unwrap();
                std::future::pending::<()>().await
            })
        });
        let second = Http::new().unwrap();
        assert_eq!(
            second
                .run(|| async {
                    tokio::time::timeout(Duration::from_secs(2), barrier.wait())
                        .await
                        .unwrap();
                    flag.store(true, Ordering::Release);
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    7
                })
                .unwrap(),
            7
        );
        assert_eq!(first.join().unwrap(), Err(ProbeError::Cancelled));
    }
    #[test]
    fn concurrent_acquisitions_share_a_driver_without_serializing_their_futures() {
        let http = Arc::new(Http::new().unwrap());
        let barrier = Arc::new(tokio::sync::Barrier::new(4));
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let http = http.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    http.run(|| async {
                        tokio::time::timeout(Duration::from_secs(2), barrier.wait())
                            .await
                            .unwrap();
                        42
                    })
                    .unwrap()
                })
            })
            .collect();
        for thread in threads {
            assert_eq!(thread.join().unwrap(), 42);
        }
    }
    #[test]
    fn cancellation_prevents_requests_and_interrupts_retry_backoff_without_network() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let mut http = Http::new().unwrap();
        http.set_cancellation(Arc::new(|| true));
        assert!(matches!(
            http.get(
                Url::parse("https://radiko.jp/never-requested").unwrap(),
                HeaderMap::new(),
                1024,
                None,
                true
            ),
            Err(ProbeError::Cancelled)
        ));
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        http.set_cancellation(Arc::new(move || seen.fetch_add(1, Ordering::SeqCst) >= 1));
        let began = std::time::Instant::now();
        assert_eq!(
            http.retry_wait(Duration::from_secs(10)),
            Err(ProbeError::Cancelled)
        );
        assert!(began.elapsed() < Duration::from_secs(1));
        assert!(calls.load(Ordering::SeqCst) >= 2);
        let ordinary = Http::new().unwrap();
        assert!(ordinary.retry_wait(Duration::ZERO).is_ok());
    }
    #[test]
    fn bounded_chunks_discard_cancelled_partial_responses_and_keep_size_limits() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let flag = Arc::new(AtomicBool::new(false));
        let check = flag.clone();
        let mut http = Http::new().unwrap();
        http.set_cancellation(Arc::new(move || check.load(Ordering::Acquire)));
        assert_eq!(
            http.read_chunks(
                || {
                    flag.store(true, Ordering::Release);
                    Ok(Some(b"partial"))
                },
                16
            ),
            Err(ProbeError::Cancelled)
        );
        let ordinary = Http::new().unwrap();
        let mut chunks = [b"ab".as_slice(), b"c".as_slice()].into_iter();
        assert_eq!(
            ordinary.read_chunks(|| Ok(chunks.next()), 3).unwrap(),
            b"abc"
        );
        assert_eq!(
            ordinary.read_chunks(|| Ok(Some(b"abcd")), 3),
            Err(ProbeError::TooLarge)
        );
        assert!(
            ordinary
                .read_chunks(|| Ok(None::<&[u8]>), 0)
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn credentials_and_tokens_cannot_follow_urls_outside_the_service() {
        for text in [
            "https://radiko.jp/a",
            "https://api.radiko.jp/a",
            "https://dr-wowza.radiko-cf.com/a",
            "https://si-f-radiko.smartstream.ne.jp/a",
        ] {
            assert!(trusted(&Url::parse(text).unwrap()));
        }
        for text in [
            "http://radiko.jp/a",
            "https://radiko.jp.evil.invalid/a",
            "https://evilradiko.jp/a",
            "https://127.0.0.1/a",
            "https://radiko.jp:444/a",
            "https://secret@radiko.jp/a",
        ] {
            assert!(!trusted(&Url::parse(text).unwrap()));
        }
    }
}
