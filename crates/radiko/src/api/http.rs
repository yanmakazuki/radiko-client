//! サイズ・時間・再試行回数を制限するHTTP処理。生のエラー・URL・本文を外部へ返さない。
use super::stream::ProbeError;
mod diagnostics;
mod executor;
pub use diagnostics::{
    Measurements as RequestMeasurements, Snapshot as RequestSnapshot, Stage as RequestStage,
};
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
    diagnostics: Option<Arc<RequestMeasurements>>,
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
    /// Counts a completed stream-token renewal. The default keeps non-playback clients unchanged.
    fn note_reauthentication(&self) {}
    /// Counts segments a live playlist skipped while catching up. Other clients ignore it.
    fn note_skipped_segments(&self, _count: u64) {}
    /// Counts a live timeline restart. Other clients ignore it.
    fn note_sequence_reset(&self) {}
    /// Records whether the latest live restart joined the first playlist segment.
    fn note_joined_window_head(&self, _at_head: bool) {}
    /// Playback cancellation. The default is for clients that never sleep between attempts.
    fn cancelled(&self) -> bool {
        false
    }
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
            diagnostics: None,
            cancelled: None,
        })
    }
    pub(crate) fn enable_diagnostics(&mut self) -> Arc<RequestMeasurements> {
        self.diagnostics
            .get_or_insert_with(|| Arc::new(RequestMeasurements::new()))
            .clone()
    }
    fn observe(&self, stage: RequestStage) {
        if let Some(measurements) = &self.diagnostics {
            measurements.enter(stage);
        }
    }
    fn network_error(&self, error: reqwest::Error) -> ProbeError {
        if let Some(measurements) = &self.diagnostics {
            measurements.failed(error.is_timeout(), error.is_connect());
        }
        // Drop the original error here: it can contain signed URLs.
        ProbeError::Network
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
                    .map_err(|error| self.network_error(error))
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
enum GetStep {
    Reply(Reply),
    Redirect(Url),
}

// Only complete read-only GET attempts use this budget, including body reception.
fn retry_get<T>(
    mut attempt: impl FnMut() -> Result<T, ProbeError>,
    mut backoff: impl FnMut(u32) -> Result<(), ProbeError>,
) -> Result<T, ProbeError> {
    let mut retries = 0;
    loop {
        match attempt() {
            Err(error)
                if retries < 3
                    && matches!(
                        error,
                        ProbeError::Network | ProbeError::Http(408) | ProbeError::Http(500..=599)
                    ) =>
            {
                backoff(retries)?;
                retries += 1;
            }
            result => return result,
        }
    }
}
impl Http {
    fn get_once(
        &self,
        url: &Url,
        headers: &HeaderMap,
        limit: usize,
        range: Option<(u64, u64)>,
        redirects: bool,
    ) -> Result<GetStep, ProbeError> {
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
        self.observe(RequestStage::Headers);
        let mut response = self
            .run(|| request.send())?
            .map_err(|error| self.network_error(error))?;
        self.check_cancelled()?;
        let status = response.status().as_u16();
        if let Some(measurements) = &self.diagnostics {
            measurements.received(status);
        }
        if matches!(status, 301 | 302 | 303 | 307 | 308) && redirects {
            let location = response
                .headers()
                .get(LOCATION)
                .and_then(|h| h.to_str().ok())
                .ok_or(ProbeError::UnsafeUrl)?;
            return Ok(GetStep::Redirect(
                url.join(location).map_err(|_| ProbeError::UnsafeUrl)?,
            ));
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
        self.observe(RequestStage::Body);
        let bytes = self.response_bytes(&mut response, limit)?;
        if range.is_some_and(|(_, length)| bytes.len() as u64 != length) {
            return Err(ProbeError::InvalidPlaylist);
        }
        Ok(GetStep::Reply(Reply {
            headers,
            bytes,
            url: url.clone(),
        }))
    }
}
struct FinishRequest<'a>(Option<&'a RequestMeasurements>);
impl Drop for FinishRequest<'_> {
    fn drop(&mut self) {
        if let Some(measurements) = self.0 {
            measurements.enter(RequestStage::Idle);
        }
    }
}
/// One plain HTTP GET for shutdown tests. Production clients stay HTTPS-only.
#[cfg(test)]
pub(crate) fn loopback_get(
    url: &str,
    cancelled: Arc<dyn Fn() -> bool + Send + Sync>,
) -> Result<(), ProbeError> {
    let mut http = Http::new()?;
    http.set_cancellation(cancelled);
    http.client = Client::builder()
        .no_proxy()
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|_| ProbeError::Network)?;
    let url = Url::parse(url).map_err(|_| ProbeError::UnsafeUrl)?;
    let result = http.get_once(&url, &HeaderMap::new(), 64, None, false);
    drop(http);
    // The shared current-thread runtime must be polled again or the dropped
    // connection stays open in the reactor.
    executor::runtime()?.block_on(async {
        tokio::time::sleep(Duration::from_millis(20)).await;
    });
    result.map(|_| ())
}
impl Fetch for Http {
    fn note_reauthentication(&self) {
        if let Some(measurements) = &self.diagnostics {
            measurements.note_reauthentication();
        }
    }
    fn note_skipped_segments(&self, count: u64) {
        if let Some(measurements) = &self.diagnostics {
            measurements.note_skipped_segments(count);
        }
    }
    fn note_sequence_reset(&self) {
        if let Some(measurements) = &self.diagnostics {
            measurements.note_sequence_reset();
        }
    }
    fn note_joined_window_head(&self, at_head: bool) {
        if let Some(measurements) = &self.diagnostics {
            measurements.note_joined_window_head(at_head);
        }
    }
    fn cancelled(&self) -> bool {
        self.cancelled.as_ref().is_some_and(|check| check())
    }
    fn get(
        &self,
        mut url: Url,
        headers: HeaderMap,
        limit: usize,
        range: Option<(u64, u64)>,
        redirects: bool,
    ) -> Result<Reply, ProbeError> {
        let _finish = FinishRequest(self.diagnostics.as_deref());
        for _ in 0..=5 {
            self.check_cancelled()?;
            if !trusted(&url) {
                return Err(ProbeError::UnsafeUrl);
            }
            let step = retry_get(
                || self.get_once(&url, &headers, limit, range, redirects),
                |retries| {
                    self.observe(RequestStage::RetryWait);
                    let jitter = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map(|n| n.subsec_millis() % 50)
                        .unwrap_or(0);
                    self.retry_wait(Duration::from_millis(
                        100 * (1 << retries) + u64::from(jitter),
                    ))
                },
            )?;
            match step {
                GetStep::Reply(reply) => return Ok(reply),
                GetStep::Redirect(next) => url = next,
            }
        }
        Err(ProbeError::UnsafeUrl)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "explicit loopback mock: requires local listening permission, no external network"]
    fn local_mock_http_retry_discards_partial_body_and_keeps_numeric_diagnostics() {
        use std::{
            io::{Read, Write},
            net::TcpListener,
            thread,
        };
        for partial in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let address = listener.local_addr().unwrap();
            let server = thread::spawn(move || {
                let deadline = std::time::Instant::now() + Duration::from_secs(3);
                for attempt in 0..2 {
                    let mut socket = loop {
                        match listener.accept() {
                            Ok((socket, _)) => break socket,
                            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                                assert!(std::time::Instant::now() < deadline);
                                thread::sleep(Duration::from_millis(1));
                            }
                            Err(error) => panic!("local mock accept: {error}"),
                        }
                    };
                    socket
                        .set_read_timeout(Some(Duration::from_secs(1)))
                        .unwrap();
                    let mut request = [0; 1024];
                    assert!(socket.read(&mut request).unwrap() > 0);
                    let response: &[u8] = if attempt == 1 {
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"
                    } else if partial {
                        b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nx"
                    } else {
                        b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    };
                    socket.write_all(response).unwrap();
                }
            });
            let mut http = Http::new().unwrap();
            // The production client and trusted-target policy remain HTTPS-only.
            http.client = Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(1))
                .build()
                .unwrap();
            let measurements = http.enable_diagnostics();
            let target = Url::parse(&format!("http://{address}/synthetic")).unwrap();
            let result = {
                let _finish = FinishRequest(Some(&measurements));
                retry_get(
                    || http.get_once(&target, &HeaderMap::new(), 16, None, false),
                    |_| {
                        http.observe(RequestStage::RetryWait);
                        let snapshot = measurements.snapshot().unwrap();
                        assert_eq!(
                            snapshot.response_status,
                            Some(if partial { 200 } else { 503 })
                        );
                        Ok(())
                    },
                )
            };
            server.join().unwrap();
            let Ok(GetStep::Reply(reply)) = result else {
                panic!("local retry failed")
            };
            assert_eq!(reply.bytes, b"ok");
            let snapshot = measurements.snapshot().unwrap();
            assert_eq!(snapshot.stage, RequestStage::Idle);
            assert_eq!((snapshot.attempts, snapshot.retry_waits), (2, 1));
            assert_eq!(snapshot.response_status, Some(200));
            assert_eq!(snapshot.network_failures, u64::from(partial));
            assert_eq!((snapshot.timeouts, snapshot.connect_failures), (0, 0));
        }
    }

    #[test]
    #[ignore = "explicit loopback mock: requires local listening permission, no external network"]
    fn local_mock_http_retains_header_body_timeout_and_404_diagnostics() {
        use std::{
            io::{Read, Write},
            net::TcpListener,
            sync::mpsc,
            thread,
        };
        for status in [None, Some(200), Some(404)] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let address = listener.local_addr().unwrap();
            let (release, wait) = mpsc::channel();
            let server = thread::spawn(move || {
                let deadline = std::time::Instant::now() + Duration::from_secs(3);
                let mut socket = loop {
                    match listener.accept() {
                        Ok((socket, _)) => break socket,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(
                                std::time::Instant::now() < deadline,
                                "local client never connected"
                            );
                            thread::sleep(Duration::from_millis(1));
                        }
                        Err(error) => panic!("local mock accept: {error}"),
                    }
                };
                socket
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut request = [0; 1024];
                assert!(socket.read(&mut request).unwrap() > 0);
                if status == Some(200) {
                    socket
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nx",
                        )
                        .unwrap();
                }
                if status == Some(404) {
                    socket.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
                }
                let _ = wait.recv_timeout(Duration::from_secs(3));
            });
            let mut http = Http::new().unwrap();
            // Only this loopback mock uses HTTP. The production constructor and trusted
            // target checks remain HTTPS-only; no account, DNS, or radiko request is used.
            http.client = Client::builder()
                .no_proxy()
                .timeout(Duration::from_millis(250))
                .build()
                .unwrap();
            let measurements = http.enable_diagnostics();
            let result = {
                let _finish = FinishRequest(Some(&measurements));
                http.get_once(
                    &Url::parse(&format!(
                        "http://{address}/synthetic?signature=synthetic-secret"
                    ))
                    .unwrap(),
                    &HeaderMap::new(),
                    16,
                    None,
                    false,
                )
            };
            let _ = release.send(());
            server.join().unwrap();
            assert!(matches!(
                (&result, status),
                (Err(ProbeError::NotFound), Some(404))
                    | (Err(ProbeError::Network), None | Some(200))
            ));
            let snapshot = measurements.snapshot().unwrap();
            assert_eq!(snapshot.stage, RequestStage::Idle);
            assert_eq!((snapshot.attempts, snapshot.retry_waits), (1, 0));
            assert_eq!(
                (
                    snapshot.network_failures,
                    snapshot.timeouts,
                    snapshot.connect_failures
                ),
                (
                    u64::from(status != Some(404)),
                    u64::from(status != Some(404)),
                    0
                )
            );
            assert!(!snapshot.headers_max.is_zero());
            assert_eq!(snapshot.response_status, status);
            assert_eq!(snapshot.body_max.is_zero(), status != Some(200));
            assert!(!format!("{snapshot:?}").contains("synthetic-secret"));
        }
    }
    #[test]
    fn network_error_keeps_only_numeric_classification_and_never_the_original_url() {
        let mut http = Http::new().unwrap();
        let measurements = http.enable_diagnostics();
        let url = Url::parse("https://radiko.jp/synthetic?signature=synthetic-secret").unwrap();
        let error = http
            .client
            .get(url.clone())
            .header("invalid\nheader", "value")
            .build()
            .unwrap_err()
            .with_url(url);
        let safe = http.network_error(error);
        assert!(matches!(safe, ProbeError::Network));
        let snapshot = measurements.snapshot().unwrap();
        assert_eq!(
            (
                snapshot.network_failures,
                snapshot.timeouts,
                snapshot.connect_failures
            ),
            (1, 0, 0)
        );
        assert!(!format!("{safe:?} {snapshot:?}").contains("synthetic-secret"));
    }
    #[test]
    fn request_measurement_is_opt_in_and_invalid_or_cancelled_gets_never_start_attempts() {
        let mut http = Http::new().unwrap();
        assert!(http.diagnostics.is_none());
        let measurements = http.enable_diagnostics();
        assert!(Arc::ptr_eq(&measurements, &http.enable_diagnostics()));
        assert!(matches!(
            http.get(
                Url::parse("https://untrusted.example/secret").unwrap(),
                HeaderMap::new(),
                1024,
                None,
                true
            ),
            Err(ProbeError::UnsafeUrl)
        ));
        http.set_cancellation(Arc::new(|| true));
        assert!(matches!(
            http.get(
                Url::parse("https://radiko.jp/secret").unwrap(),
                HeaderMap::new(),
                1024,
                None,
                true
            ),
            Err(ProbeError::Cancelled)
        ));
        let snapshot = measurements.snapshot().unwrap();
        assert_eq!(snapshot.stage, RequestStage::Idle);
        assert_eq!((snapshot.attempts, snapshot.retry_waits), (0, 0));
        assert_eq!(
            (
                snapshot.network_failures,
                snapshot.timeouts,
                snapshot.connect_failures
            ),
            (0, 0, 0)
        );
        assert!(!format!("{snapshot:?}").contains("secret"));
    }
    #[test]
    fn readonly_get_discards_interrupted_body_and_restarts_with_a_shared_retry_budget() {
        let http = Http::new().unwrap();
        let mut attempts = 0;
        let mut delays = Vec::new();
        let bytes = retry_get(
            || {
                attempts += 1;
                match attempts {
                    1 => Err(ProbeError::Http(503)),
                    2 => {
                        let mut chunks =
                            [Ok(Some(b"discard".as_slice())), Err(ProbeError::Network)].into_iter();
                        http.read_chunks(|| chunks.next().unwrap(), 32)
                    }
                    3 => {
                        let mut chunks = [b"fresh".as_slice(), b"-body".as_slice()].into_iter();
                        http.read_chunks(|| Ok(chunks.next()), 32)
                    }
                    _ => panic!("unexpected retry"),
                }
            },
            |delay| {
                delays.push(delay);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(bytes, b"fresh-body");
        assert_eq!(attempts, 3);
        assert_eq!(delays, [0, 1]);
    }
    #[test]
    fn readonly_get_bounds_mixed_failures_and_cancels_before_another_attempt() {
        let mut attempts = 0;
        let mut delays = Vec::new();
        let result: Result<(), _> = retry_get(
            || {
                attempts += 1;
                Err(match attempts {
                    1 => ProbeError::Http(503),
                    2 => ProbeError::Http(408),
                    _ => ProbeError::Network,
                })
            },
            |delay| {
                delays.push(delay);
                Ok(())
            },
        );
        assert_eq!(result, Err(ProbeError::Network));
        assert_eq!(attempts, 4);
        assert_eq!(delays, [0, 1, 2]);
        let mut attempts = 0;
        let cancelled: Result<(), _> = retry_get(
            || {
                attempts += 1;
                Err(ProbeError::Network)
            },
            |_| Err(ProbeError::Cancelled),
        );
        assert_eq!(cancelled, Err(ProbeError::Cancelled));
        assert_eq!(attempts, 1);
    }
    #[test]
    fn readonly_get_never_retries_cancellation_denials_limits_or_malformed_ranges() {
        for error in [
            ProbeError::Cancelled,
            ProbeError::Authentication,
            ProbeError::Denied,
            ProbeError::NotFound,
            ProbeError::RateLimited,
            ProbeError::TooLarge,
            ProbeError::InvalidPlaylist,
            ProbeError::UnsafeUrl,
            ProbeError::Http(400),
            ProbeError::Http(302),
        ] {
            let mut error = Some(error);
            let result: Result<(), _> = retry_get(
                || Err(error.take().expect("permanent failure must not retry")),
                |_| panic!("permanent failure must not wait"),
            );
            assert!(result.is_err());
            assert!(error.is_none());
        }
    }
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
