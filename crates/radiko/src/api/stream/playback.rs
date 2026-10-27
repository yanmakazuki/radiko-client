//! 状態を保持するHLS取得。URL・ヘッダー・音声データはDebug出力の対象にしない。
use super::*;
use crate::api::http::Reply;
pub use crate::api::http::{RequestMeasurements, RequestSnapshot, RequestStage};
use std::time::{Duration as Wait, Instant};

#[derive(Clone, Debug)]
pub struct StreamInfo {
    pub title: Option<String>,
    pub station: String,
    pub mode: ProbeMode,
    pub starts_at: DateTime<FixedOffset>,
    pub ends_at: DateTime<FixedOffset>,
    pub position: Wait,
}
pub struct Segment {
    pub bytes: Vec<u8>,
    pub initialization: Option<std::sync::Arc<[u8]>>,
    pub duration: Option<Wait>,
    pub codec: Option<String>,
    pub discontinuity: bool,
    pub starts_at: Option<DateTime<FixedOffset>>,
}
pub enum Next {
    Segment(Segment),
    Pending(Wait),
    End,
}
pub trait SegmentSource: Send + 'static {
    fn info(&self) -> &StreamInfo;
    fn enable_http_diagnostics(&mut self) -> Option<std::sync::Arc<RequestMeasurements>> {
        None
    }
    fn set_cancellation(&mut self, _cancelled: std::sync::Arc<dyn Fn() -> bool + Send + Sync>) {}
    fn poll_segment(&mut self) -> Result<Next, ProbeError>;
}
impl<T: SegmentSource + ?Sized> SegmentSource for Box<T> {
    fn enable_http_diagnostics(&mut self) -> Option<std::sync::Arc<RequestMeasurements>> {
        (**self).enable_http_diagnostics()
    }
    fn set_cancellation(&mut self, cancelled: std::sync::Arc<dyn Fn() -> bool + Send + Sync>) {
        (**self).set_cancellation(cancelled);
    }
    fn info(&self) -> &StreamInfo {
        (**self).info()
    }
    fn poll_segment(&mut self) -> Result<Next, ProbeError> {
        (**self).poll_segment()
    }
}
pub struct PreparedStream {
    info: StreamInfo,
    hls: HlsStream<Http>,
}
impl SegmentSource for PreparedStream {
    fn enable_http_diagnostics(&mut self) -> Option<std::sync::Arc<RequestMeasurements>> {
        Some(self.hls.http.enable_diagnostics())
    }
    fn set_cancellation(&mut self, cancelled: std::sync::Arc<dyn Fn() -> bool + Send + Sync>) {
        self.hls.http.set_cancellation(cancelled);
    }
    fn info(&self) -> &StreamInfo {
        &self.info
    }
    fn poll_segment(&mut self) -> Result<Next, ProbeError> {
        self.hls.poll(Instant::now())
    }
}

pub fn prepare(
    station: &str,
    mode: ProbeMode,
    cursor: DateTime<FixedOffset>,
    session: &MemberSession,
    membership: &Membership,
) -> Result<PreparedStream, ProbeError> {
    prepare_at(station, mode, cursor, session, membership, Wait::ZERO)
}

pub fn prepare_at(
    station: &str,
    mode: ProbeMode,
    cursor: DateTime<FixedOffset>,
    session: &MemberSession,
    membership: &Membership,
    position: Wait,
) -> Result<PreparedStream, ProbeError> {
    prepare_at_controlled(
        station,
        mode,
        cursor,
        session,
        membership,
        position,
        std::sync::Arc::new(|| false),
    )
}
/// Cancels request futures and retry waits; OS name resolution itself remains bounded and non-cancellable.
pub fn prepare_at_controlled(
    station: &str,
    mode: ProbeMode,
    cursor: DateTime<FixedOffset>,
    session: &MemberSession,
    membership: &Membership,
    position: Wait,
    cancelled: std::sync::Arc<dyn Fn() -> bool + Send + Sync>,
) -> Result<PreparedStream, ProbeError> {
    if cancelled() {
        return Err(ProbeError::Cancelled);
    }
    validate_station(station)?;
    if mode == ProbeMode::Live && !position.is_zero() {
        return Err(ProbeError::InvalidPosition);
    }
    let now = Utc::now().fixed_offset();
    if mode == ProbeMode::TimeFree && cursor > now {
        return Err(ProbeError::Upcoming);
    }
    let mut http = Http::new()?;
    http.set_cancellation(cancelled);
    let request = prepare_with(
        &http,
        station,
        mode,
        RequestedPosition {
            at: cursor,
            position,
        },
        now,
        session,
        membership,
    )?;
    let mut last_error = ProbeError::NoEndpoint;
    for endpoint in request.endpoints {
        match load_media(&http, endpoint, &request.auth.headers) {
            Ok((url, media, codec)) => {
                if !position.is_zero() {
                    let target = request
                        .program
                        .start
                        .checked_add_signed(
                            Duration::from_std(position).map_err(|_| ProbeError::InvalidInput)?,
                        )
                        .ok_or(ProbeError::InvalidInput)?;
                    let first = segment_time(&media, 0).ok_or_else(|| {
                        ProbeError::InvalidPlaylist.at("シーク時刻との対応を確認できません")
                    })?;
                    let lead = (target - first)
                        .to_std()
                        .map_err(|_| ProbeError::InvalidPlaylist.at("指定位置より後の配信です"))?;
                    if lead > Wait::from_secs(10 + media.target_duration + 1) {
                        return Err(ProbeError::InvalidPlaylist.at("シーク先に対応しない配信です"));
                    }
                }
                let mut hls = HlsStream::new(http, url, request.auth.headers, media, codec)?;
                hls.refresh = Some(playback_refresh(session.token().to_owned()));
                return Ok(PreparedStream {
                    info: StreamInfo {
                        title: request.program.title,
                        station: station.into(),
                        mode,
                        starts_at: request.program.start,
                        ends_at: request.program.end,
                        position,
                    },
                    hls,
                });
            }
            Err(error @ (ProbeError::Authentication | ProbeError::RateLimited)) => {
                return Err(error);
            }
            Err(error) => last_error = error,
        }
    }
    Err(last_error.at("連続配信の準備"))
}

fn load_media(
    http: &impl Fetch,
    mut url: Url,
    headers: &HeaderMap,
) -> Result<(Url, m3u8_rs::MediaPlaylist, Option<String>), ProbeError> {
    let mut codec = None;
    for _ in 0..4 {
        let reply = http.get(url, headers.clone(), TEXT_LIMIT, None, true)?;
        url = reply.url;
        match parse_hls(&reply.bytes)? {
            Playlist::MediaPlaylist(media) => {
                validate_media(&media)?;
                return Ok((url, media, codec));
            }
            Playlist::MasterPlaylist(master) => {
                if master
                    .session_key
                    .iter()
                    .any(|k| k.0.method != KeyMethod::None)
                {
                    return Err(ProbeError::InvalidPlaylist);
                }
                let variant = master
                    .variants
                    .iter()
                    .filter(|v| !v.is_i_frame)
                    .min_by_key(|v| v.bandwidth)
                    .ok_or(ProbeError::InvalidPlaylist)?;
                codec = variant.codecs.as_deref().and_then(safe_codec);
                let alternative = master
                    .alternatives
                    .iter()
                    .filter(|a| {
                        a.media_type == AlternativeMediaType::Audio
                            && Some(&a.group_id) == variant.audio.as_ref()
                            && a.uri.is_some()
                    })
                    .max_by_key(|a| (a.default, a.autoselect));
                url = url
                    .join(
                        alternative
                            .and_then(|a| a.uri.as_deref())
                            .unwrap_or(&variant.uri),
                    )
                    .map_err(|_| ProbeError::UnsafeUrl)?;
            }
        }
    }
    Err(ProbeError::InvalidPlaylist)
}
fn segment_time(media: &m3u8_rs::MediaPlaylist, index: usize) -> Option<DateTime<FixedOffset>> {
    let mut time = None;
    for (n, segment) in media.segments.iter().take(index + 1).enumerate() {
        if let Some(stamp) = segment.program_date_time {
            time = Some(stamp);
        }
        if n == index {
            return time;
        }
        time = time.and_then(|stamp| {
            stamp.checked_add_signed(Duration::microseconds(
                (f64::from(segment.duration) * 1_000_000.0).round() as i64,
            ))
        });
    }
    None
}

fn validate_media(media: &m3u8_rs::MediaPlaylist) -> Result<(), ProbeError> {
    if media.segments.is_empty()
        || media.segments.len() > 4096
        || media.target_duration == 0
        || media.target_duration > 60
        || media.i_frames_only
        || media.segments.iter().any(|s| {
            s.map
                .as_ref()
                .is_some_and(|map| map.byte_range.is_some() || map.uri.is_empty())
                || s.byte_range.is_some()
                || !s.duration.is_finite()
                || s.duration <= 0.0
                || s.duration > 120.0
                || s.key.as_ref().is_some_and(|k| k.method != KeyMethod::None)
        })
    {
        return Err(ProbeError::InvalidPlaylist);
    }
    media
        .media_sequence
        .checked_add(media.segments.len() as u64)
        .ok_or(ProbeError::InvalidPlaylist)?;
    Ok(())
}

const REAUTH_INTERVAL: Wait = Wait::from_secs(20);

type RefreshFn<F> = Box<dyn FnMut(&F) -> Result<HeaderMap, ProbeError> + Send>;

fn playback_refresh<F: Fetch + 'static>(token: String) -> RefreshFn<F> {
    Box::new(move |http| {
        let session = MemberSession::new(token.clone()).map_err(|_| ProbeError::AuthExpired)?;
        match authenticate(http, &session) {
            Ok(auth) => Ok(auth.headers),
            Err(ProbeError::Cancelled) => Err(ProbeError::Cancelled),
            Err(ProbeError::RateLimited) => Err(ProbeError::RateLimited),
            Err(ProbeError::Network) => Err(ProbeError::Network),
            Err(ProbeError::Denied) => Err(ProbeError::Denied),
            Err(_) => Err(ProbeError::AuthExpired),
        }
    })
}

struct HlsStream<F> {
    http: F,
    url: Url,
    headers: HeaderMap,
    media: m3u8_rs::MediaPlaylist,
    codec: Option<String>,
    next: u64,
    discontinuity: Option<u64>,
    reload_at: Instant,
    initialization: Option<(Url, std::sync::Arc<[u8]>)>,
    refresh: Option<RefreshFn<F>>,
    /// One renewal is enough for the current failure. Cleared after a successful poll.
    auth_episode: bool,
    last_reauth: Option<Instant>,
}
enum Renew {
    Unavailable,
    Refreshed,
    Reused,
}
impl<F: Fetch> HlsStream<F> {
    fn new(
        http: F,
        url: Url,
        headers: HeaderMap,
        media: m3u8_rs::MediaPlaylist,
        codec: Option<String>,
    ) -> Result<Self, ProbeError> {
        validate_media(&media)?;
        let next = media.media_sequence;
        Ok(Self {
            http,
            url,
            headers,
            codec,
            next,
            discontinuity: None,
            reload_at: Instant::now(),
            media,
            initialization: None,
            refresh: None,
            auth_episode: false,
            last_reauth: None,
        })
    }
    fn poll(&mut self, now: Instant) -> Result<Next, ProbeError> {
        let result = self.poll_at(now);
        if result.is_ok() {
            self.auth_episode = false;
        }
        result
    }
    fn poll_at(&mut self, now: Instant) -> Result<Next, ProbeError> {
        if self.next < self.media.media_sequence {
            return Err(ProbeError::InvalidPlaylist.at("未取得セグメントが配信窓から消失"));
        }
        if self.current_index()?.is_some() {
            return self.read_segment(now);
        }
        if self.media.end_list {
            return Ok(Next::End);
        }
        if now < self.reload_at {
            return Ok(Next::Pending(self.reload_at.duration_since(now)));
        }
        let reply = self.fetch_playlist()?;
        let wait = self.install_playlist(reply, now)?;
        if self.next < self.media.media_sequence + self.media.segments.len() as u64 {
            return self.poll(now);
        }
        if self.media.end_list {
            Ok(Next::End)
        } else {
            Ok(Next::Pending(wait))
        }
    }
    fn current_index(&self) -> Result<Option<usize>, ProbeError> {
        if self.next < self.media.media_sequence {
            return Err(ProbeError::InvalidPlaylist.at("未取得セグメントが配信窓から消失"));
        }
        let index = usize::try_from(self.next - self.media.media_sequence)
            .map_err(|_| ProbeError::InvalidPlaylist)?;
        Ok((index < self.media.segments.len()).then_some(index))
    }
    fn read_segment(&mut self, now: Instant) -> Result<Next, ProbeError> {
        let Some(index) = self.current_index()? else {
            return Err(ProbeError::InvalidPlaylist);
        };
        // Resolve initialization before the media bytes so a plain fetch keeps its old order.
        let _ = self.initialization_for(index, now)?;
        let (bytes, index) = self.fetch_segment_bytes(now)?;
        if bytes.is_empty() {
            return Err(ProbeError::InvalidPlaylist);
        }
        let initialization = self.initialization_for(index, now)?;
        let duration = self
            .media
            .segments
            .get(index)
            .ok_or(ProbeError::InvalidPlaylist)?
            .duration;
        let discontinuity = self.media.segments[..=index]
            .iter()
            .filter(|item| item.discontinuity)
            .count() as u64;
        let sequence = self
            .media
            .discontinuity_sequence
            .checked_add(discontinuity)
            .ok_or(ProbeError::InvalidPlaylist)?;
        let starts_at = segment_time(&self.media, index);
        let reset = self
            .discontinuity
            .is_some_and(|previous| previous != sequence);
        let codec = self.codec.clone();
        self.discontinuity = Some(sequence);
        self.next = self
            .next
            .checked_add(1)
            .ok_or(ProbeError::InvalidPlaylist)?;
        Ok(Next::Segment(Segment {
            bytes,
            initialization,
            duration: Some(
                Wait::try_from_secs_f64(f64::from(duration))
                    .map_err(|_| ProbeError::InvalidPlaylist)?,
            ),
            codec,
            discontinuity: reset,
            starts_at,
        }))
    }
    fn initialization_for(
        &mut self,
        index: usize,
        now: Instant,
    ) -> Result<Option<std::sync::Arc<[u8]>>, ProbeError> {
        let Some(url) = self.init_url(index)? else {
            self.initialization = None;
            return Ok(None);
        };
        if self
            .initialization
            .as_ref()
            .is_some_and(|(current, _)| current == &url)
        {
            return Ok(self.initialization.as_ref().map(|(_, bytes)| bytes.clone()));
        }
        let Some((bytes, fetched)) = self.fetch_init_bytes(url, now)? else {
            self.initialization = None;
            return Ok(None);
        };
        if bytes.is_empty() {
            return Err(ProbeError::InvalidPlaylist);
        }
        let shared: std::sync::Arc<[u8]> = bytes.into();
        self.initialization = Some((fetched, shared.clone()));
        Ok(Some(shared))
    }
    fn init_url(&self, index: usize) -> Result<Option<Url>, ProbeError> {
        let Some(map) = self
            .media
            .segments
            .get(..=index)
            .ok_or(ProbeError::InvalidPlaylist)?
            .iter()
            .rev()
            .find_map(|item| item.map.as_ref())
        else {
            return Ok(None);
        };
        self.url
            .join(&map.uri)
            .map(Some)
            .map_err(|_| ProbeError::UnsafeUrl)
    }
    fn fetch_init_bytes(
        &mut self,
        url: Url,
        now: Instant,
    ) -> Result<Option<(Vec<u8>, Url)>, ProbeError> {
        const STAGE: &str = "HLS初期化情報取得";
        match self
            .http
            .get(url.clone(), HeaderMap::new(), TEXT_LIMIT, None, true)
        {
            Ok(reply) => Ok(Some((reply.bytes, url))),
            Err(ProbeError::Authentication) => {
                self.recover_media(STAGE, now)?;
                let Some(index) = self.current_index()? else {
                    return Err(ProbeError::InvalidPlaylist.at(STAGE));
                };
                let Some(url) = self.init_url(index)? else {
                    return Ok(None);
                };
                match self
                    .http
                    .get(url.clone(), self.headers.clone(), TEXT_LIMIT, None, true)
                {
                    Ok(reply) => Ok(Some((reply.bytes, url))),
                    Err(ProbeError::Authentication) => Err(ProbeError::AuthExpired.at(STAGE)),
                    Err(error) => Err(error.at(STAGE)),
                }
            }
            Err(error) => Err(error.at(STAGE)),
        }
    }
    fn fetch_segment_bytes(&mut self, now: Instant) -> Result<(Vec<u8>, usize), ProbeError> {
        const STAGE: &str = "HLS音声取得";
        let Some(index) = self.current_index()? else {
            return Err(ProbeError::InvalidPlaylist);
        };
        let url = self.segment_url(index)?;
        match self
            .http
            .get(url, HeaderMap::new(), SAMPLE_LIMIT, None, true)
        {
            Ok(reply) => Ok((reply.bytes, index)),
            Err(ProbeError::Authentication) => {
                self.recover_media(STAGE, now)?;
                let Some(index) = self.current_index()? else {
                    return Err(ProbeError::InvalidPlaylist.at(STAGE));
                };
                let url = self.segment_url(index)?;
                match self
                    .http
                    .get(url, self.headers.clone(), SAMPLE_LIMIT, None, true)
                {
                    Ok(reply) => Ok((reply.bytes, index)),
                    Err(ProbeError::Authentication) => Err(ProbeError::AuthExpired.at(STAGE)),
                    Err(error) => Err(error.at(STAGE)),
                }
            }
            Err(error) => Err(error.at(STAGE)),
        }
    }
    fn segment_url(&self, index: usize) -> Result<Url, ProbeError> {
        self.url
            .join(
                &self
                    .media
                    .segments
                    .get(index)
                    .ok_or(ProbeError::InvalidPlaylist)?
                    .uri,
            )
            .map_err(|_| ProbeError::UnsafeUrl)
    }
    /// HTTP 401 only. 403, 429, and every other status keep the caller's existing error.
    fn renew(&mut self, stage: &'static str) -> Result<Renew, ProbeError> {
        if self.refresh.is_none() {
            return Ok(Renew::Unavailable);
        }
        let recent = self
            .last_reauth
            .is_some_and(|at| at.elapsed() < REAUTH_INTERVAL);
        if self.auth_episode || recent {
            return if self.headers.get("x-radiko-authtoken").is_some() {
                Ok(Renew::Reused)
            } else {
                Err(ProbeError::AuthExpired.at(stage))
            };
        }
        self.reauthenticate(stage)?;
        Ok(Renew::Refreshed)
    }
    fn reauthenticate(&mut self, stage: &'static str) -> Result<(), ProbeError> {
        let Some(refresh) = self.refresh.as_mut() else {
            return Err(ProbeError::Authentication.at(stage));
        };
        self.auth_episode = true;
        self.last_reauth = Some(Instant::now());
        match refresh(&self.http) {
            Ok(headers) => {
                if headers.get("x-radiko-authtoken").is_none() {
                    return Err(ProbeError::AuthExpired.at(stage));
                }
                self.headers = headers;
                self.http.note_reauthentication();
                Ok(())
            }
            Err(ProbeError::Cancelled) => Err(ProbeError::Cancelled.at(stage)),
            Err(ProbeError::RateLimited) => Err(ProbeError::RateLimited.at(stage)),
            Err(ProbeError::Network) => Err(ProbeError::Network.at(stage)),
            Err(ProbeError::Denied) => Err(ProbeError::Denied.at(stage)),
            Err(_) => Err(ProbeError::AuthExpired.at(stage)),
        }
    }
    fn recover_media(&mut self, stage: &'static str, now: Instant) -> Result<(), ProbeError> {
        match self.renew(stage)? {
            Renew::Unavailable => return Err(ProbeError::Authentication.at(stage)),
            Renew::Refreshed | Renew::Reused => {}
        }
        let reply = match self.http.get(
            self.url.clone(),
            self.headers.clone(),
            TEXT_LIMIT,
            None,
            true,
        ) {
            Ok(reply) => reply,
            Err(ProbeError::Authentication) => return Err(ProbeError::AuthExpired.at(stage)),
            Err(error) => return Err(error.at(stage)),
        };
        self.install_playlist(reply, now)
            .map(|_| ())
            .map_err(|error| match error {
                ProbeError::At(_, _) => error,
                other => other.at(stage),
            })
    }
    fn fetch_playlist(&mut self) -> Result<Reply, ProbeError> {
        const STAGE: &str = "HLS一覧更新";
        let url = self.url.clone();
        match self
            .http
            .get(url.clone(), self.headers.clone(), TEXT_LIMIT, None, true)
        {
            Ok(reply) => Ok(reply),
            Err(ProbeError::Authentication) => match self.renew(STAGE)? {
                Renew::Unavailable => Err(ProbeError::Authentication.at(STAGE)),
                Renew::Refreshed | Renew::Reused => {
                    match self
                        .http
                        .get(url, self.headers.clone(), TEXT_LIMIT, None, true)
                    {
                        Ok(reply) => Ok(reply),
                        Err(ProbeError::Authentication) => Err(ProbeError::AuthExpired.at(STAGE)),
                        Err(error) => Err(error.at(STAGE)),
                    }
                }
            },
            Err(error) => Err(error.at(STAGE)),
        }
    }
    // Sequence numbers, not changing signed URLs, decide which segment is new.
    fn install_playlist(&mut self, reply: Reply, now: Instant) -> Result<Wait, ProbeError> {
        let Playlist::MediaPlaylist(media) = parse_hls(&reply.bytes)? else {
            return Err(ProbeError::InvalidPlaylist);
        };
        validate_media(&media)?;
        if media.media_sequence < self.media.media_sequence {
            return Err(ProbeError::InvalidPlaylist);
        }
        let changed = media.media_sequence != self.media.media_sequence
            || media.segments.len() != self.media.segments.len();
        let wait = Wait::from_secs(media.target_duration).div_f64(if changed { 1.0 } else { 2.0 });
        self.reload_at = now + wait;
        self.media = media;
        self.url = reply.url;
        if self.next < self.media.media_sequence {
            return Err(ProbeError::InvalidPlaylist.at("未取得セグメントが配信窓から消失"));
        }
        Ok(wait)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::http::Reply;
    use std::{cell::RefCell, collections::VecDeque};
    struct Mock {
        steps: RefCell<VecDeque<(String, Vec<u8>)>>,
    }
    impl Fetch for Mock {
        fn get(
            &self,
            url: Url,
            headers: HeaderMap,
            limit: usize,
            range: Option<(u64, u64)>,
            redirects: bool,
        ) -> Result<Reply, ProbeError> {
            let (path, bytes) = self
                .steps
                .borrow_mut()
                .pop_front()
                .expect("unexpected request");
            assert_eq!(url.path(), path);
            assert!(redirects);
            assert_eq!(range, None);
            if path.ends_with(".aac") {
                assert!(headers.is_empty());
                assert_eq!(limit, SAMPLE_LIMIT);
            } else {
                assert_eq!(limit, TEXT_LIMIT);
            }
            Ok(Reply {
                headers: HeaderMap::new(),
                bytes,
                url,
            })
        }
    }
    fn media(first: u64, count: u64, end: bool, signature: &str, discontinuity: bool) -> Vec<u8> {
        let mut text = format!("#EXTM3U\n#EXT-X-TARGETDURATION:5\n#EXT-X-MEDIA-SEQUENCE:{first}\n");
        for n in first..first + count {
            if discontinuity && n == first + count - 1 {
                text.push_str("#EXT-X-DISCONTINUITY\n");
            }
            text.push_str(&format!("#EXTINF:5,\ns{n}.aac?signature={signature}\n"));
        }
        if end {
            text.push_str("#EXT-X-ENDLIST\n");
        }
        text.into_bytes()
    }
    fn parsed(bytes: &[u8]) -> m3u8_rs::MediaPlaylist {
        match parse_hls(bytes).unwrap() {
            Playlist::MediaPlaylist(m) => m,
            _ => panic!(),
        }
    }
    fn reader(initial: &[u8], steps: Vec<(&str, Vec<u8>)>) -> HlsStream<Mock> {
        HlsStream::new(
            Mock {
                steps: RefCell::new(steps.into_iter().map(|(p, b)| (p.into(), b)).collect()),
            },
            Url::parse("https://radiko.jp/audio/media.m3u8").unwrap(),
            HeaderMap::new(),
            parsed(initial),
            Some("mp4a.40.2".into()),
        )
        .unwrap()
    }
    #[test]
    fn runtime_http_failures_preserve_the_cause_and_only_add_a_static_stage() {
        struct FailingFetch {
            error: RefCell<Option<ProbeError>>,
            calls: std::cell::Cell<usize>,
        }
        impl Fetch for FailingFetch {
            fn get(
                &self,
                _: Url,
                _: HeaderMap,
                _: usize,
                _: Option<(u64, u64)>,
                _: bool,
            ) -> Result<Reply, ProbeError> {
                self.calls.set(self.calls.get() + 1);
                Err(self
                    .error
                    .borrow_mut()
                    .take()
                    .expect("only one request is allowed"))
            }
        }
        for target in ["segment", "playlist", "initialization"] {
            let reload = target == "playlist";
            for cause in [
                ProbeError::NotFound,
                ProbeError::Network,
                ProbeError::Http(503),
                ProbeError::Authentication,
                ProbeError::RateLimited,
                ProbeError::Cancelled,
            ] {
                let original = cause.to_string();
                let mut initial =
                    String::from_utf8(media(0, 1, false, "synthetic-secret", false)).unwrap();
                if target == "initialization" {
                    initial = initial.replacen(
                        "#EXTINF",
                        "#EXT-X-MAP:URI=\"init.mp4?signature=synthetic-secret\"\n#EXTINF",
                        1,
                    );
                }
                let mut source = HlsStream::new(
                    FailingFetch {
                        error: RefCell::new(Some(cause)),
                        calls: std::cell::Cell::new(0),
                    },
                    Url::parse("https://radiko.jp/secret/media.m3u8?signature=synthetic-secret")
                        .unwrap(),
                    HeaderMap::new(),
                    parsed(initial.as_bytes()),
                    None,
                )
                .unwrap();
                source.next = u64::from(reload);
                let Err(ProbeError::At(stage, error)) = source.poll(Instant::now()) else {
                    panic!("runtime request failure must identify its stage");
                };
                assert_eq!(
                    stage,
                    match target {
                        "playlist" => "HLS一覧更新",
                        "initialization" => "HLS初期化情報取得",
                        _ => "HLS音声取得",
                    }
                );
                assert_eq!(error.to_string(), original);
                assert!(!format!("{stage}: {error}").contains("synthetic-secret"));
                assert_eq!(source.next, u64::from(reload));
                assert_eq!(source.http.calls.get(), 1);
            }
        }
    }
    #[test]
    fn segment_timestamp_is_inferred_only_from_an_explicit_anchor() {
        let plain = parsed(&media(0, 2, true, "a", false));
        assert!(segment_time(&plain, 0).is_none());
        let xml = String::from_utf8(media(0, 2, true, "a", false))
            .unwrap()
            .replace(
                "#EXTINF:5,",
                "#EXT-X-PROGRAM-DATE-TIME:2026-10-15T12:00:00+09:00\n#EXTINF:5,",
            );
        let mut anchored = parsed(xml.as_bytes());
        anchored.segments[1].program_date_time = None;
        let start = DateTime::parse_from_rfc3339("2026-10-15T12:00:00+09:00").unwrap();
        assert_eq!(segment_time(&anchored, 0), Some(start));
        assert_eq!(
            segment_time(&anchored, 1),
            Some(start + Duration::seconds(5))
        );
    }
    #[test]
    fn reload_skips_overlap_even_when_signed_urls_change_and_resets_only_at_discontinuity() {
        let mut source = reader(
            &media(10, 2, false, "old", false),
            vec![
                ("/audio/s10.aac", vec![10]),
                ("/audio/s11.aac", vec![11]),
                ("/audio/media.m3u8", media(11, 2, true, "new", true)),
                ("/audio/s12.aac", vec![12]),
            ],
        );
        for (n, reset) in [(10, false), (11, false), (12, true)] {
            let Next::Segment(segment) = source.poll(Instant::now()).unwrap() else {
                panic!()
            };
            assert_eq!(segment.bytes, [n]);
            assert_eq!(segment.discontinuity, reset);
        }
        assert!(matches!(source.poll(Instant::now()).unwrap(), Next::End));
        assert!(source.http.steps.borrow().is_empty());
    }
    #[test]
    fn unchanged_playlist_waits_before_reload_and_does_not_duplicate_audio() {
        let mut source = reader(
            &media(0, 1, false, "a", false),
            vec![
                ("/audio/s0.aac", vec![1]),
                ("/audio/media.m3u8", media(0, 1, false, "b", false)),
            ],
        );
        assert!(matches!(
            source.poll(Instant::now()).unwrap(),
            Next::Segment(_)
        ));
        assert!(matches!(
            source.poll(Instant::now()).unwrap(),
            Next::Pending(_)
        ));
        assert!(matches!(
            source
                .poll(source.reload_at - Wait::from_millis(1))
                .unwrap(),
            Next::Pending(_)
        ));
        assert!(source.http.steps.borrow().is_empty());
    }
    #[test]
    fn initialization_is_bounded_shared_and_reacquired_only_when_its_uri_changes() {
        let mapped = |first, count, end, name| {
            String::from_utf8(media(first, count, end, "a", false))
                .unwrap()
                .replacen("#EXTINF", &format!("#EXT-X-MAP:URI=\"{name}\"\n#EXTINF"), 1)
                .into_bytes()
        };
        let mut source = reader(
            &mapped(0, 2, false, "init.mp4?sig=a"),
            vec![
                ("/audio/init.mp4", vec![8]),
                ("/audio/s0.aac", vec![0]),
                ("/audio/s1.aac", vec![1]),
                ("/audio/media.m3u8", mapped(1, 2, true, "init.mp4?sig=b")),
                ("/audio/init.mp4", vec![8]),
                ("/audio/s2.aac", vec![2]),
            ],
        );
        let Next::Segment(first) = source.poll(Instant::now()).unwrap() else {
            panic!()
        };
        let Next::Segment(second) = source.poll(Instant::now()).unwrap() else {
            panic!()
        };
        assert_eq!(first.duration, Some(Wait::from_secs(5)));
        assert!(std::sync::Arc::ptr_eq(
            first.initialization.as_ref().unwrap(),
            second.initialization.as_ref().unwrap()
        ));
        let Next::Segment(third) = source.poll(Instant::now()).unwrap() else {
            panic!()
        };
        assert_eq!(first.initialization, third.initialization);
        assert!(!third.discontinuity);
        assert!(matches!(source.poll(Instant::now()).unwrap(), Next::End));
        assert!(source.http.steps.borrow().is_empty());
    }

    #[test]
    fn invalid_segment_durations_are_rejected_before_fetching_initialization() {
        for duration in [f32::NAN, f32::INFINITY, 0.0, -1.0, 121.0] {
            let mut media = parsed(&media(0, 1, true, "a", false));
            media.segments[0].duration = duration;
            assert!(validate_media(&media).is_err());
        }
    }
    #[test]
    fn missing_or_rewound_sequence_stops_instead_of_skipping_or_replaying_segments() {
        for first in [9, 12] {
            let mut source = reader(
                &media(10, 1, false, "a", false),
                vec![
                    ("/audio/s10.aac", vec![1]),
                    ("/audio/media.m3u8", media(first, 1, false, "b", false)),
                ],
            );
            assert!(matches!(
                source.poll(Instant::now()).unwrap(),
                Next::Segment(_)
            ));
            assert!(source.poll(Instant::now()).is_err());
            assert!(source.http.steps.borrow().is_empty());
        }
    }
    #[test]
    fn unsupported_encryption_and_ranges_are_rejected_before_segment_requests() {
        for tag in [
            "#EXT-X-KEY:METHOD=AES-128,URI=\"key\"\n",
            "#EXT-X-MAP:URI=\"init.mp4\",BYTERANGE=\"10@0\"\n",
            "#EXT-X-BYTERANGE:10@0\n",
        ] {
            let text = format!("#EXTM3U\n#EXT-X-TARGETDURATION:5\n{tag}#EXTINF:5,\ns0.aac\n");
            assert!(validate_media(&parsed(text.as_bytes())).is_err());
        }
        let overflow = String::from_utf8(media(0, 1, false, "a", false))
            .unwrap()
            .replace(
                "#EXT-X-MEDIA-SEQUENCE:0",
                &format!("#EXT-X-MEDIA-SEQUENCE:{}", u64::MAX),
            );
        assert!(validate_media(&parsed(overflow.as_bytes())).is_err());
    }

    use std::cell::Cell;
    struct Step {
        path: &'static str,
        query: Option<&'static str>,
        token: Option<&'static str>,
        result: Result<Vec<u8>, ProbeError>,
    }
    struct Scripted {
        steps: RefCell<VecDeque<Step>>,
        auth_calls: Cell<usize>,
        notes: Cell<usize>,
        cancel_auth: Cell<bool>,
        fail_auth: Cell<bool>,
    }
    impl Fetch for Scripted {
        fn note_reauthentication(&self) {
            self.notes.set(self.notes.get() + 1);
        }
        fn get(
            &self,
            url: Url,
            headers: HeaderMap,
            limit: usize,
            range: Option<(u64, u64)>,
            redirects: bool,
        ) -> Result<Reply, ProbeError> {
            let path = url.path();
            if path.ends_with("/auth1") || path.ends_with("/auth2") {
                assert!(!redirects);
                assert!(range.is_none());
            } else {
                assert!(redirects);
                assert!(range.is_none());
            }
            let token = headers
                .get("x-radiko-authtoken")
                .map(|value| value.to_str().unwrap().to_owned());
            if let Some(value) = headers.get("x-radiko-authtoken") {
                assert!(value.is_sensitive());
            }
            if path.ends_with("/auth1") {
                self.auth_calls.set(self.auth_calls.get() + 1);
                assert_eq!(
                    headers.get("cookie").and_then(|value| value.to_str().ok()),
                    Some("radiko_session=synthetic-member")
                );
                if self.cancel_auth.get() {
                    return Err(ProbeError::Cancelled);
                }
                if self.fail_auth.get() {
                    return Err(ProbeError::Authentication);
                }
                let mut reply_headers = HeaderMap::new();
                let mut renewed = HeaderValue::from_static("synthetic-renewed-token");
                renewed.set_sensitive(true);
                reply_headers.insert("x-radiko-authtoken", renewed);
                reply_headers.insert("x-radiko-keyoffset", HeaderValue::from_static("1"));
                reply_headers.insert("x-radiko-keylength", HeaderValue::from_static("4"));
                return Ok(Reply {
                    headers: reply_headers,
                    bytes: b"OK".to_vec(),
                    url,
                });
            }
            if path.ends_with("/auth2") {
                assert_eq!(token.as_deref(), Some("synthetic-renewed-token"));
                assert!(headers.get("x-radiko-partialkey").is_some());
                return Ok(Reply {
                    headers: HeaderMap::new(),
                    bytes: b"JP13,Tokyo\n".to_vec(),
                    url,
                });
            }
            let step = self
                .steps
                .borrow_mut()
                .pop_front()
                .expect("unexpected request");
            assert_eq!(path, step.path);
            assert_eq!(url.query(), step.query);
            assert_eq!(token.as_deref(), step.token);
            assert_eq!(
                limit,
                if path.ends_with(".aac") {
                    SAMPLE_LIMIT
                } else {
                    TEXT_LIMIT
                }
            );
            step.result.map(|bytes| Reply {
                headers: HeaderMap::new(),
                bytes,
                url,
            })
        }
    }
    fn step(
        path: &'static str,
        query: Option<&'static str>,
        token: Option<&'static str>,
        result: Result<Vec<u8>, ProbeError>,
    ) -> Step {
        Step {
            path,
            query,
            token,
            result,
        }
    }
    fn stamped(first: u64, signature: &str, origin: &str) -> Vec<u8> {
        format!(
            "#EXTM3U\n#EXT-X-TARGETDURATION:5\n#EXT-X-MEDIA-SEQUENCE:{first}\n#EXT-X-PROGRAM-DATE-TIME:{origin}\n#EXTINF:5,\ns{first}.aac?signature={signature}\n"
        )
        .into_bytes()
    }
    fn authed(initial: &[u8], steps: Vec<Step>) -> HlsStream<Scripted> {
        let mut headers = HeaderMap::new();
        let mut expired = HeaderValue::from_static("synthetic-expired-token");
        expired.set_sensitive(true);
        headers.insert("x-radiko-authtoken", expired);
        let mut source = HlsStream::new(
            Scripted {
                steps: RefCell::new(steps.into()),
                auth_calls: Cell::new(0),
                notes: Cell::new(0),
                cancel_auth: Cell::new(false),
                fail_auth: Cell::new(false),
            },
            Url::parse("https://radiko.jp/audio/media.m3u8").unwrap(),
            headers,
            parsed(initial),
            Some("mp4a.40.2".into()),
        )
        .unwrap();
        source.refresh = Some(playback_refresh("synthetic-member".into()));
        source
    }
    fn assert_secret_free(error: &ProbeError) {
        let text = format!("{error} {error:?}");
        for secret in [
            "synthetic-renewed-token",
            "synthetic-expired-token",
            "synthetic-member",
            "signature=",
        ] {
            assert!(!text.contains(secret), "{text}");
        }
    }
    #[test]
    fn playlist_401_reauthenticates_once_and_keeps_media_time_monotonic() {
        let mut source = authed(
            &stamped(10, "old", "2026-10-15T12:00:00+09:00"),
            vec![
                step("/audio/s10.aac", Some("signature=old"), None, Ok(vec![10])),
                step(
                    "/audio/media.m3u8",
                    None,
                    Some("synthetic-expired-token"),
                    Err(ProbeError::Authentication),
                ),
                step(
                    "/audio/media.m3u8",
                    None,
                    Some("synthetic-renewed-token"),
                    Ok(stamped(11, "new", "2026-10-15T12:00:05+09:00")),
                ),
                step("/audio/s11.aac", Some("signature=new"), None, Ok(vec![11])),
            ],
        );
        let Next::Segment(first) = source.poll(Instant::now()).unwrap() else {
            panic!("first segment");
        };
        let Next::Segment(second) = source.poll(Instant::now()).unwrap() else {
            panic!("segment after re-auth");
        };
        assert_eq!(first.bytes, vec![10]);
        assert_eq!(second.bytes, vec![11]);
        assert!(second.starts_at > first.starts_at);
        assert_eq!(
            second.starts_at.unwrap() - first.starts_at.unwrap(),
            Duration::seconds(5)
        );
        assert!(!second.discontinuity);
        assert_eq!(second.duration, Some(Wait::from_secs(5)));
        assert_eq!(source.next, 12);
        assert_eq!(source.http.auth_calls.get(), 1);
        assert_eq!(source.http.notes.get(), 1);
        assert!(source.http.steps.borrow().is_empty());
    }
    #[test]
    fn segment_401_refreshes_the_signed_url_once_and_does_not_advance_twice() {
        let mut source = authed(
            &media(0, 1, false, "old", false),
            vec![
                step(
                    "/audio/s0.aac",
                    Some("signature=old"),
                    None,
                    Err(ProbeError::Authentication),
                ),
                step(
                    "/audio/media.m3u8",
                    None,
                    Some("synthetic-renewed-token"),
                    Ok(media(0, 1, true, "new", false)),
                ),
                step(
                    "/audio/s0.aac",
                    Some("signature=new"),
                    Some("synthetic-renewed-token"),
                    Ok(vec![7]),
                ),
            ],
        );
        let Next::Segment(segment) = source.poll(Instant::now()).unwrap() else {
            panic!("recovered segment");
        };
        assert_eq!(segment.bytes, vec![7]);
        assert!(!segment.discontinuity);
        assert_eq!(source.next, 1);
        assert_eq!(source.http.auth_calls.get(), 1);
        assert_eq!(source.http.notes.get(), 1);
        assert!(matches!(source.poll(Instant::now()).unwrap(), Next::End));
        assert!(source.http.steps.borrow().is_empty());
    }
    #[test]
    fn a_second_401_after_reauthentication_stops_without_another_auth_or_sequence_advance() {
        for playlist in [false, true] {
            let initial = media(0, 1, false, "old", false);
            let steps = if playlist {
                vec![
                    step(
                        "/audio/media.m3u8",
                        None,
                        Some("synthetic-expired-token"),
                        Err(ProbeError::Authentication),
                    ),
                    step(
                        "/audio/media.m3u8",
                        None,
                        Some("synthetic-renewed-token"),
                        Err(ProbeError::Authentication),
                    ),
                ]
            } else {
                vec![
                    step(
                        "/audio/s0.aac",
                        Some("signature=old"),
                        None,
                        Err(ProbeError::Authentication),
                    ),
                    step(
                        "/audio/media.m3u8",
                        None,
                        Some("synthetic-renewed-token"),
                        Ok(media(0, 1, false, "new", false)),
                    ),
                    step(
                        "/audio/s0.aac",
                        Some("signature=new"),
                        Some("synthetic-renewed-token"),
                        Err(ProbeError::Authentication),
                    ),
                ]
            };
            let mut source = authed(&initial, steps);
            if playlist {
                source.next = 1;
            }
            let Err(ProbeError::At(stage, error)) = source.poll(Instant::now()) else {
                panic!("persistent 401 must stop");
            };
            assert_eq!(
                stage,
                if playlist {
                    "HLS一覧更新"
                } else {
                    "HLS音声取得"
                }
            );
            assert!(matches!(error.as_ref(), ProbeError::AuthExpired));
            assert!(error.to_string().contains("認証期限切れ"));
            assert_secret_free(&ProbeError::At(stage, error));
            assert_eq!(source.next, u64::from(playlist));
            assert_eq!(source.http.auth_calls.get(), 1);
            assert_eq!(source.http.notes.get(), 1);
            assert!(source.http.steps.borrow().is_empty());
        }
    }
    #[test]
    fn rate_limit_and_forbidden_do_not_reauthenticate() {
        for (error, stage, playlist) in [
            (ProbeError::RateLimited, "HLS音声取得", false),
            (ProbeError::RateLimited, "HLS一覧更新", true),
            (ProbeError::Denied, "HLS一覧更新", true),
            (ProbeError::Http(503), "HLS音声取得", false),
        ] {
            let expected = error.to_string();
            let mut source = authed(
                &media(0, 1, false, "old", false),
                vec![step(
                    if playlist {
                        "/audio/media.m3u8"
                    } else {
                        "/audio/s0.aac"
                    },
                    if playlist {
                        None
                    } else {
                        Some("signature=old")
                    },
                    if playlist {
                        Some("synthetic-expired-token")
                    } else {
                        None
                    },
                    Err(error),
                )],
            );
            if playlist {
                source.next = 1;
            }
            let Err(ProbeError::At(found, inner)) = source.poll(Instant::now()) else {
                panic!("non-401 must keep the original failure");
            };
            assert_eq!(found, stage);
            assert_eq!(inner.to_string(), expected);
            assert_eq!(source.http.auth_calls.get(), 0);
            assert_eq!(source.http.notes.get(), 0);
            assert_eq!(source.next, u64::from(playlist));
            assert_secret_free(&ProbeError::At(found, inner));
        }
    }
    #[test]
    fn failed_reauthentication_stops_once_with_auth_expired() {
        let mut source = authed(
            &media(0, 1, false, "old", false),
            vec![step(
                "/audio/media.m3u8",
                None,
                Some("synthetic-expired-token"),
                Err(ProbeError::Authentication),
            )],
        );
        source.next = 1;
        source.http.fail_auth.set(true);
        let Err(ProbeError::At(stage, error)) = source.poll(Instant::now()) else {
            panic!("a failed renewal must stop");
        };
        assert_eq!(stage, "HLS一覧更新");
        assert!(matches!(error.as_ref(), ProbeError::AuthExpired));
        assert!(error.to_string().contains("認証期限切れ"));
        assert_eq!(source.http.auth_calls.get(), 1);
        assert_eq!(source.http.notes.get(), 0);
        assert_eq!(source.next, 1);
        assert!(source.http.steps.borrow().is_empty());
        assert_secret_free(&ProbeError::At(stage, error));
    }
    #[test]
    fn cancellation_during_reauthentication_is_not_retried() {
        let mut source = authed(
            &media(0, 1, false, "old", false),
            vec![step(
                "/audio/media.m3u8",
                None,
                Some("synthetic-expired-token"),
                Err(ProbeError::Authentication),
            )],
        );
        source.next = 1;
        source.http.cancel_auth.set(true);
        let Err(ProbeError::At(stage, error)) = source.poll(Instant::now()) else {
            panic!("cancelled renewal must stop");
        };
        assert_eq!(stage, "HLS一覧更新");
        assert!(matches!(error.as_ref(), ProbeError::Cancelled));
        assert_eq!(source.http.auth_calls.get(), 1);
        assert_eq!(source.http.notes.get(), 0);
        assert_eq!(source.next, 1);
        assert!(source.http.steps.borrow().is_empty());
        assert_secret_free(&ProbeError::At(stage, error));
    }
    #[test]
    fn a_second_401_inside_the_reauth_interval_does_not_authenticate_again() {
        let mut source = authed(
            &stamped(10, "old", "2026-10-15T12:00:00+09:00"),
            vec![
                step(
                    "/audio/media.m3u8",
                    None,
                    Some("synthetic-expired-token"),
                    Err(ProbeError::Authentication),
                ),
                step(
                    "/audio/media.m3u8",
                    None,
                    Some("synthetic-renewed-token"),
                    Ok(stamped(11, "new", "2026-10-15T12:00:05+09:00")),
                ),
                step("/audio/s11.aac", Some("signature=new"), None, Ok(vec![11])),
                step(
                    "/audio/media.m3u8",
                    None,
                    Some("synthetic-renewed-token"),
                    Err(ProbeError::Authentication),
                ),
                step(
                    "/audio/media.m3u8",
                    None,
                    Some("synthetic-renewed-token"),
                    Err(ProbeError::Authentication),
                ),
            ],
        );
        source.next = 11;
        let Next::Segment(segment) = source.poll(Instant::now()).unwrap() else {
            panic!("first recovery should continue");
        };
        assert_eq!(segment.bytes, vec![11]);
        assert_eq!(source.next, 12);
        let Err(ProbeError::At(_, error)) = source.poll(source.reload_at) else {
            panic!("a 401 inside the interval must stop");
        };
        assert!(matches!(error.as_ref(), ProbeError::AuthExpired));
        assert_eq!(source.next, 12);
        assert_eq!(source.http.auth_calls.get(), 1);
        assert_eq!(source.http.notes.get(), 1);
        assert!(source.http.steps.borrow().is_empty());
    }
    #[test]
    fn reauthentication_is_allowed_again_after_the_interval() {
        let mut source = authed(
            &media(0, 1, false, "old", false),
            vec![
                step(
                    "/audio/media.m3u8",
                    None,
                    Some("synthetic-expired-token"),
                    Err(ProbeError::Authentication),
                ),
                step(
                    "/audio/media.m3u8",
                    None,
                    Some("synthetic-renewed-token"),
                    Ok(media(1, 1, false, "new", false)),
                ),
                step("/audio/s1.aac", Some("signature=new"), None, Ok(vec![1])),
                step(
                    "/audio/media.m3u8",
                    None,
                    Some("synthetic-renewed-token"),
                    Err(ProbeError::Authentication),
                ),
                step(
                    "/audio/media.m3u8",
                    None,
                    Some("synthetic-renewed-token"),
                    Ok(media(2, 1, true, "later", false)),
                ),
                step("/audio/s2.aac", Some("signature=later"), None, Ok(vec![2])),
            ],
        );
        source.next = 1;
        assert!(matches!(
            source.poll(Instant::now()).unwrap(),
            Next::Segment(_)
        ));
        source.last_reauth = Some(
            Instant::now()
                .checked_sub(REAUTH_INTERVAL + Wait::from_secs(1))
                .expect("test clock"),
        );
        let due = source.reload_at;
        let Next::Segment(segment) = source.poll(due).unwrap() else {
            panic!("a later 401 may renew once more");
        };
        assert_eq!(segment.bytes, vec![2]);
        assert_eq!(source.next, 3);
        assert_eq!(source.http.auth_calls.get(), 2);
        assert_eq!(source.http.notes.get(), 2);
        assert!(matches!(source.poll(Instant::now()).unwrap(), Next::End));
    }
    #[test]
    fn initialization_401_retries_the_same_sequence_with_the_new_token() {
        let mapped = String::from_utf8(media(0, 1, true, "old", false))
            .unwrap()
            .replacen("#EXTINF", "#EXT-X-MAP:URI=\"init.mp4?sig=a\"\n#EXTINF", 1);
        let refreshed = mapped.replace("signature=old", "signature=new");
        let mut source = authed(
            mapped.as_bytes(),
            vec![
                step(
                    "/audio/init.mp4",
                    Some("sig=a"),
                    None,
                    Err(ProbeError::Authentication),
                ),
                step(
                    "/audio/media.m3u8",
                    None,
                    Some("synthetic-renewed-token"),
                    Ok(refreshed.into_bytes()),
                ),
                step(
                    "/audio/init.mp4",
                    Some("sig=a"),
                    Some("synthetic-renewed-token"),
                    Ok(vec![9]),
                ),
                step("/audio/s0.aac", Some("signature=new"), None, Ok(vec![4])),
            ],
        );
        let Next::Segment(segment) = source.poll(Instant::now()).unwrap() else {
            panic!("init recovery");
        };
        assert_eq!(segment.initialization.unwrap().as_ref(), &[9]);
        assert_eq!(segment.bytes, vec![4]);
        assert_eq!(source.next, 1);
        assert_eq!(source.http.auth_calls.get(), 1);
        assert!(matches!(source.poll(Instant::now()).unwrap(), Next::End));
        assert!(source.http.steps.borrow().is_empty());
    }
}
