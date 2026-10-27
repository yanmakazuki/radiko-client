//! 状態を保持するHLS取得。URL・ヘッダー・音声データはDebug出力の対象にしない。
use super::*;
use crate::api::http::Reply;
pub use crate::api::http::{RequestMeasurements, RequestSnapshot, RequestStage};
use crate::reconnect::{BACKOFF_MAX, BACKOFF_START, LIMIT as RECONNECT_LIMIT};
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
                hls.live = mode == ProbeMode::Live;
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

/// Earliest segment whose start is within three target durations of the playlist end.
/// A short live window therefore joins at `MEDIA-SEQUENCE`. A longer one joins near the edge.
fn live_join_index(media: &m3u8_rs::MediaPlaylist) -> usize {
    let horizon = f64::from(u32::try_from(media.target_duration).unwrap_or(u32::MAX)) * 3.0;
    let mut from_end = 0.0;
    let mut index = media.segments.len().saturating_sub(1);
    for (i, segment) in media.segments.iter().enumerate().rev() {
        let start_from_end = from_end + f64::from(segment.duration);
        if start_from_end > horizon && i + 1 < media.segments.len() {
            break;
        }
        from_end = start_from_end;
        index = i;
        if start_from_end > horizon {
            break;
        }
    }
    index
}

const REAUTH_INTERVAL: Wait = Wait::from_secs(20);

type RefreshFn<F> = Box<dyn FnMut(&F) -> Result<HeaderMap, ProbeError> + Send>;
type RetrySleeper = Box<dyn FnMut(Wait) -> Result<(), ProbeError> + Send>;

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
    /// Sequence of the segment most recently returned. Used to recognize a slightly older live window.
    last_played: Option<u64>,
    /// Live windows catch up instead of stopping when the next sequence leaves the playlist.
    live: bool,
    discontinuity: Option<u64>,
    pending_discontinuity: bool,
    reload_at: Instant,
    initialization: Option<(Url, std::sync::Arc<[u8]>)>,
    refresh: Option<RefreshFn<F>>,
    /// One renewal is enough for the current failure. Cleared after a successful poll.
    auth_episode: bool,
    last_reauth: Option<Instant>,
    /// Grows while a played stream is retrying a transport failure. Reset when a segment is returned.
    backoff: Wait,
    reconnect_waited: Wait,
    /// Tests advance this instead of sleeping. Production leaves it empty and sleeps.
    sleeper: Option<RetrySleeper>,
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
            last_played: None,
            live: false,
            discontinuity: None,
            pending_discontinuity: false,
            reload_at: Instant::now(),
            media,
            initialization: None,
            refresh: None,
            auth_episode: false,
            last_reauth: None,
            backoff: BACKOFF_START,
            reconnect_waited: Wait::ZERO,
            sleeper: None,
        })
    }
    fn poll(&mut self, now: Instant) -> Result<Next, ProbeError> {
        let result = self.poll_at(now);
        if result.is_ok() {
            self.auth_episode = false;
        }
        if matches!(result, Ok(Next::Segment(_))) {
            self.backoff = BACKOFF_START;
            self.reconnect_waited = Wait::ZERO;
        }
        result
    }
    fn poll_at(&mut self, now: Instant) -> Result<Next, ProbeError> {
        if self.next < self.media.media_sequence {
            if self.live {
                self.realign(Self::forward_gap_is_reset(self.next, &self.media))?;
            } else {
                return Err(ProbeError::WindowLost);
            }
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
            return Err(if self.live {
                ProbeError::InvalidPlaylist
            } else {
                ProbeError::WindowLost
            });
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
        let reset = self.pending_discontinuity
            || self
                .discontinuity
                .is_some_and(|previous| previous != sequence);
        self.pending_discontinuity = false;
        let codec = self.codec.clone();
        self.discontinuity = Some(sequence);
        self.last_played = Some(self.next);
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
        let mut url = url;
        loop {
            match self
                .http
                .get(url.clone(), HeaderMap::new(), TEXT_LIMIT, None, true)
            {
                Ok(reply) => return Ok(Some((reply.bytes, url))),
                Err(ProbeError::Authentication) => {
                    self.recover_media(STAGE, now)?;
                    let Some(index) = self.current_index()? else {
                        return Err(ProbeError::InvalidPlaylist.at(STAGE));
                    };
                    let Some(url) = self.init_url(index)? else {
                        return Ok(None);
                    };
                    return match self.http.get(
                        url.clone(),
                        self.headers.clone(),
                        TEXT_LIMIT,
                        None,
                        true,
                    ) {
                        Ok(reply) => Ok(Some((reply.bytes, url))),
                        Err(ProbeError::Authentication) => Err(ProbeError::AuthExpired.at(STAGE)),
                        Err(error) => Err(error.at(STAGE)),
                    };
                }
                Err(error) if self.can_retry(&error) => {
                    self.wait_backoff()?;
                    let reply = self.fetch_playlist()?;
                    self.install_playlist(reply, now)?;
                    let Some(index) = self.current_index()? else {
                        return Err(ProbeError::InvalidPlaylist.at(STAGE));
                    };
                    let Some(next) = self.init_url(index)? else {
                        self.initialization = None;
                        return Ok(None);
                    };
                    url = next;
                }
                Err(error) => return Err(error.at(STAGE)),
            }
        }
    }
    fn fetch_segment_bytes(&mut self, now: Instant) -> Result<(Vec<u8>, usize), ProbeError> {
        const STAGE: &str = "HLS音声取得";
        loop {
            let Some(index) = self.current_index()? else {
                return Err(ProbeError::InvalidPlaylist);
            };
            let url = self.segment_url(index)?;
            match self
                .http
                .get(url, HeaderMap::new(), SAMPLE_LIMIT, None, true)
            {
                Ok(reply) => return Ok((reply.bytes, index)),
                Err(ProbeError::Authentication) => {
                    self.recover_media(STAGE, now)?;
                    let Some(index) = self.current_index()? else {
                        return Err(ProbeError::InvalidPlaylist.at(STAGE));
                    };
                    let url = self.segment_url(index)?;
                    return match self
                        .http
                        .get(url, self.headers.clone(), SAMPLE_LIMIT, None, true)
                    {
                        Ok(reply) => Ok((reply.bytes, index)),
                        Err(ProbeError::Authentication) => Err(ProbeError::AuthExpired.at(STAGE)),
                        Err(error) => Err(error.at(STAGE)),
                    };
                }
                Err(error) if self.can_retry(&error) => {
                    self.wait_backoff()?;
                    let reply = self.fetch_playlist()?;
                    self.install_playlist(reply, now)?;
                }
                Err(error) => return Err(error.at(STAGE)),
            }
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
        loop {
            match self
                .http
                .get(url.clone(), self.headers.clone(), TEXT_LIMIT, None, true)
            {
                Ok(reply) => return Ok(reply),
                Err(ProbeError::Authentication) => match self.renew(STAGE)? {
                    Renew::Unavailable => return Err(ProbeError::Authentication.at(STAGE)),
                    Renew::Refreshed | Renew::Reused => {
                        match self.http.get(
                            url.clone(),
                            self.headers.clone(),
                            TEXT_LIMIT,
                            None,
                            true,
                        ) {
                            Ok(reply) => return Ok(reply),
                            Err(ProbeError::Authentication) => {
                                return Err(ProbeError::AuthExpired.at(STAGE));
                            }
                            Err(error) if self.can_retry(&error) => self.wait_backoff()?,
                            Err(error) => return Err(error.at(STAGE)),
                        }
                    }
                },
                Err(error) if self.can_retry(&error) => self.wait_backoff()?,
                Err(error) => return Err(error.at(STAGE)),
            }
        }
    }
    // Sequence numbers, not changing signed URLs, decide which segment is new.
    fn install_playlist(&mut self, reply: Reply, now: Instant) -> Result<Wait, ProbeError> {
        let Playlist::MediaPlaylist(media) = parse_hls(&reply.bytes)? else {
            return Err(ProbeError::InvalidPlaylist);
        };
        validate_media(&media)?;
        let previous_sequence = self.media.media_sequence;
        let rewound = media.media_sequence < previous_sequence;
        if rewound && !self.live {
            return Err(ProbeError::InvalidPlaylist);
        }
        let changed = media.media_sequence != previous_sequence
            || media.segments.len() != self.media.segments.len();
        let wait = Wait::from_secs(media.target_duration).div_f64(if changed { 1.0 } else { 2.0 });
        self.reload_at = now + wait;
        self.media = media;
        self.url = reply.url;
        if self.live {
            if let Some(reset) = self.live_realignment(previous_sequence) {
                self.realign(reset)?;
            }
        } else if self.next < self.media.media_sequence {
            return Err(ProbeError::WindowLost);
        }
        Ok(wait)
    }
    /// `Some(true)` restarts the timeline. `Some(false)` catches up inside one window.
    /// `None` keeps the next sequence. A live playlist that is only slightly older is `None`.
    fn live_realignment(&self, previous_sequence: u64) -> Option<bool> {
        let start = self.media.media_sequence;
        let len = self.media.segments.len() as u64;
        let end = start.checked_add(len)?;
        if start < previous_sequence {
            if self.rewind_is_stale(start, end, len) {
                return None;
            }
            return Some(true);
        }
        if self.next < start {
            return Some(Self::forward_gap_is_reset(self.next, &self.media));
        }
        None
    }
    /// A backward playlist is stale when its tail is at most one window behind the last
    /// played sequence, or when the window still covers that sequence. Anything older is a restart.
    fn rewind_is_stale(&self, start: u64, end: u64, len: u64) -> bool {
        let contains_next = start <= self.next && self.next < end;
        if contains_next || end == self.next {
            return true;
        }
        let Some(last_played) = self.last_played else {
            return false;
        };
        let tail = end.saturating_sub(1);
        if start < last_played && end > last_played {
            return true;
        }
        last_played >= tail && last_played - tail <= len
    }
    /// The advertised window is the only scale for "the same stream moved on".
    /// A gap larger than that window is a new timeline, not a count of missed segments.
    fn forward_gap_is_reset(next: u64, media: &m3u8_rs::MediaPlaylist) -> bool {
        media.media_sequence.saturating_sub(next) > media.segments.len() as u64
    }
    fn realign(&mut self, reset: bool) -> Result<(), ProbeError> {
        let index = live_join_index(&self.media);
        let offset = u64::try_from(index).map_err(|_| ProbeError::InvalidPlaylist)?;
        let target = self
            .media
            .media_sequence
            .checked_add(offset)
            .ok_or(ProbeError::InvalidPlaylist)?;
        let skipped = if reset {
            offset
        } else {
            target
                .checked_sub(self.next)
                .ok_or(ProbeError::InvalidPlaylist)?
        };
        if skipped > 0 {
            self.http.note_skipped_segments(skipped);
        }
        if reset {
            self.http.note_sequence_reset();
        }
        self.http.note_joined_window_head(index == 0);
        self.next = target;
        self.pending_discontinuity = true;
        self.initialization = None;
        Ok(())
    }
    /// Startup failures stay immediate. A segment already handed to the player may be retried.
    fn can_retry(&self, error: &ProbeError) -> bool {
        self.last_played.is_some() && retryable_transport(error)
    }
    fn wait_backoff(&mut self) -> Result<(), ProbeError> {
        if self.http.cancelled() {
            return Err(ProbeError::Cancelled);
        }
        if self.reconnect_waited >= RECONNECT_LIMIT || self.backoff.is_zero() {
            return Err(ProbeError::ReconnectGaveUp);
        }
        let room = RECONNECT_LIMIT - self.reconnect_waited;
        let step = self.backoff.min(BACKOFF_MAX).min(room);
        if step.is_zero() {
            return Err(ProbeError::ReconnectGaveUp);
        }
        if let Some(sleeper) = self.sleeper.as_mut() {
            sleeper(step)?;
        } else {
            self.sleep_interruptible(step)?;
        }
        self.reconnect_waited = self.reconnect_waited.saturating_add(step);
        self.backoff = self.backoff.saturating_mul(2).min(BACKOFF_MAX);
        Ok(())
    }
    fn sleep_interruptible(&self, delay: Wait) -> Result<(), ProbeError> {
        let end = Instant::now() + delay;
        while Instant::now() < end {
            if self.http.cancelled() {
                return Err(ProbeError::Cancelled);
            }
            let remaining = end.saturating_duration_since(Instant::now());
            std::thread::sleep(remaining.min(Wait::from_millis(10)));
        }
        if self.http.cancelled() {
            return Err(ProbeError::Cancelled);
        }
        Ok(())
    }
}

fn retryable_transport(error: &ProbeError) -> bool {
    match error {
        ProbeError::At(_, inner) => retryable_transport(inner),
        ProbeError::Network | ProbeError::Http(408) | ProbeError::Http(500..=599) => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::http::Reply;
    use std::{
        cell::{Cell, RefCell},
        collections::VecDeque,
    };
    struct Mock {
        steps: RefCell<VecDeque<(String, Vec<u8>)>>,
        skipped: Cell<u64>,
        resets: Cell<u64>,
    }
    impl Fetch for Mock {
        fn note_skipped_segments(&self, count: u64) {
            self.skipped.set(self.skipped.get().saturating_add(count));
        }
        fn note_sequence_reset(&self) {
            self.resets.set(self.resets.get().saturating_add(1));
        }
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
                skipped: Cell::new(0),
                resets: Cell::new(0),
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
            let error = expect_err(source.poll(Instant::now()));
            if first < 10 {
                assert!(matches!(error, ProbeError::InvalidPlaylist));
            } else {
                assert!(matches!(error, ProbeError::WindowLost));
                assert_eq!(error.to_string(), ProbeError::WINDOW_LOST);
            }
            assert_eq!(source.next, 11);
            assert_eq!(source.http.skipped.get(), 0);
            assert_eq!(source.http.resets.get(), 0);
            assert!(source.http.steps.borrow().is_empty());
        }
    }
    fn live_reader(initial: &[u8], steps: Vec<(&str, Vec<u8>)>) -> HlsStream<Mock> {
        let mut source = reader(initial, steps);
        source.live = true;
        source
    }
    fn segment_at(source: &mut HlsStream<Mock>, now: Instant) -> Segment {
        match source.poll(now) {
            Ok(Next::Segment(segment)) => segment,
            Ok(Next::Pending(_)) => panic!("expected a segment, playlist reload is still waiting"),
            Ok(Next::End) => panic!("expected a segment, playlist ended"),
            Err(error) => panic!("expected a segment, got {error}"),
        }
    }
    fn segment(source: &mut HlsStream<Mock>) -> Segment {
        segment_at(source, Instant::now())
    }
    #[test]
    fn live_sequence_advances_normally_without_skipping_or_resetting_the_decoder() {
        let mut source = live_reader(
            &media(10, 2, false, "old", false),
            vec![
                ("/audio/s10.aac", vec![10]),
                ("/audio/s11.aac", vec![11]),
                ("/audio/media.m3u8", media(11, 2, false, "new", false)),
                ("/audio/s12.aac", vec![12]),
            ],
        );
        for (n, reset) in [(10u8, false), (11, false), (12, false)] {
            let segment = segment(&mut source);
            assert_eq!(segment.bytes, [n]);
            assert_eq!(segment.discontinuity, reset);
        }
        assert_eq!(source.next, 13);
        assert_eq!(source.http.skipped.get(), 0);
        assert_eq!(source.http.resets.get(), 0);
        assert!(source.http.steps.borrow().is_empty());
    }
    #[test]
    fn live_window_running_ahead_joins_the_playlist_head_and_marks_a_discontinuity() {
        let mut source = live_reader(
            &media(10, 1, false, "a", false),
            vec![
                ("/audio/s10.aac", vec![10]),
                ("/audio/media.m3u8", media(13, 3, false, "b", false)),
                ("/audio/s13.aac", vec![13]),
                ("/audio/s14.aac", vec![14]),
            ],
        );
        let first = segment(&mut source);
        assert_eq!(first.bytes, [10]);
        assert!(!first.discontinuity);
        let caught = segment(&mut source);
        assert_eq!(caught.bytes, [13]);
        assert!(caught.discontinuity);
        assert_eq!(source.http.skipped.get(), 2);
        assert_eq!(source.http.resets.get(), 0);
        let following = segment(&mut source);
        assert_eq!(following.bytes, [14]);
        assert!(!following.discontinuity);
        assert_eq!(source.next, 15);
        assert!(source.http.steps.borrow().is_empty());
    }
    #[test]
    fn live_window_longer_than_three_target_durations_joins_near_the_edge() {
        let mut source = live_reader(
            &media(10, 1, false, "a", false),
            vec![
                ("/audio/s10.aac", vec![10]),
                ("/audio/media.m3u8", media(12, 6, false, "b", false)),
                ("/audio/s15.aac", vec![15]),
            ],
        );
        assert_eq!(segment(&mut source).bytes, [10]);
        let caught = segment(&mut source);
        assert_eq!(caught.bytes, [15]);
        assert!(caught.discontinuity);
        assert_eq!(source.next, 16);
        assert_eq!(source.http.skipped.get(), 4);
        assert_eq!(source.http.resets.get(), 0);
        assert!(source.http.steps.borrow().is_empty());
    }
    #[test]
    fn live_playlist_one_sequence_older_waits_for_13_without_replaying_11() {
        let mut source = live_reader(
            &media(11, 1, false, "a", false),
            vec![
                ("/audio/s11.aac", vec![11]),
                ("/audio/media.m3u8", media(12, 1, false, "b", false)),
                ("/audio/s12.aac", vec![12]),
                ("/audio/media.m3u8", media(11, 2, false, "old", false)),
                ("/audio/media.m3u8", media(13, 2, false, "c", false)),
                ("/audio/s13.aac", vec![13]),
                ("/audio/s14.aac", vec![14]),
            ],
        );
        let first = segment(&mut source);
        assert_eq!(first.bytes, [11]);
        assert!(!first.discontinuity);
        let second = segment(&mut source);
        assert_eq!(second.bytes, [12]);
        assert!(!second.discontinuity);
        assert_eq!(source.next, 13);
        let stale_at = source.reload_at;
        assert!(matches!(source.poll(stale_at), Ok(Next::Pending(_))));
        assert_eq!(source.next, 13);
        assert!(!source.pending_discontinuity);
        assert_eq!(source.http.skipped.get(), 0);
        assert_eq!(source.http.resets.get(), 0);
        let reload_at = source.reload_at;
        let continued = segment_at(&mut source, reload_at);
        assert_eq!(continued.bytes, [13]);
        assert!(!continued.discontinuity);
        let following = segment(&mut source);
        assert_eq!(following.bytes, [14]);
        assert!(!following.discontinuity);
        assert_eq!(source.next, 15);
        assert_eq!(source.http.skipped.get(), 0);
        assert_eq!(source.http.resets.get(), 0);
        assert!(source.http.steps.borrow().is_empty());
    }
    #[test]
    fn one_segment_window_one_older_waits_and_then_continues() {
        let mut source = live_reader(
            &media(12, 1, false, "a", false),
            vec![
                ("/audio/s12.aac", vec![12]),
                ("/audio/media.m3u8", media(11, 1, false, "old", false)),
                ("/audio/media.m3u8", media(13, 1, false, "next", false)),
                ("/audio/s13.aac", vec![13]),
            ],
        );
        let played = segment(&mut source);
        assert_eq!(played.bytes, [12]);
        assert!(!played.discontinuity);
        let stale_at = source.reload_at;
        assert!(matches!(source.poll(stale_at), Ok(Next::Pending(_))));
        assert_eq!(source.next, 13);
        assert_eq!(source.last_played, Some(12));
        assert!(!source.pending_discontinuity);
        assert_eq!(source.http.skipped.get(), 0);
        assert_eq!(source.http.resets.get(), 0);
        let reload_at = source.reload_at;
        let continued = segment_at(&mut source, reload_at);
        assert_eq!(continued.bytes, [13]);
        assert!(!continued.discontinuity);
        assert_eq!(source.http.resets.get(), 0);
        assert!(source.http.steps.borrow().is_empty());
    }
    #[test]
    fn six_segment_window_slid_back_by_one_does_not_replay_or_reset() {
        let mut source = live_reader(
            &media(12, 6, false, "a", false),
            vec![
                ("/audio/s12.aac", vec![12]),
                ("/audio/s13.aac", vec![13]),
                ("/audio/s14.aac", vec![14]),
                ("/audio/s15.aac", vec![15]),
                ("/audio/s16.aac", vec![16]),
                ("/audio/s17.aac", vec![17]),
                ("/audio/media.m3u8", media(11, 6, false, "old", false)),
                ("/audio/media.m3u8", media(18, 1, false, "next", false)),
                ("/audio/s18.aac", vec![18]),
            ],
        );
        for n in 12u8..=17 {
            let played = segment(&mut source);
            assert_eq!(played.bytes, [n]);
            assert!(!played.discontinuity);
        }
        assert_eq!(source.next, 18);
        let stale_at = source.reload_at;
        assert!(matches!(source.poll(stale_at), Ok(Next::Pending(_))));
        assert_eq!(source.next, 18);
        assert_eq!(source.last_played, Some(17));
        assert!(!source.pending_discontinuity);
        assert_eq!(source.http.skipped.get(), 0);
        assert_eq!(source.http.resets.get(), 0);
        let reload_at = source.reload_at;
        let continued = segment_at(&mut source, reload_at);
        assert_eq!(continued.bytes, [18]);
        assert!(!continued.discontinuity);
        assert_eq!(source.http.resets.get(), 0);
        assert!(source.http.steps.borrow().is_empty());
    }
    #[test]
    fn stale_tail_exactly_one_window_behind_is_ignored_and_one_older_resets() {
        let mut source = live_reader(
            &media(17, 1, false, "a", false),
            vec![
                ("/audio/s17.aac", vec![17]),
                ("/audio/media.m3u8", media(6, 6, false, "edge", false)),
            ],
        );
        assert_eq!(segment(&mut source).bytes, [17]);
        let stale_at = source.reload_at;
        assert!(matches!(source.poll(stale_at), Ok(Next::Pending(_))));
        assert_eq!(source.next, 18);
        assert!(!source.pending_discontinuity);
        assert_eq!(source.http.skipped.get(), 0);
        assert_eq!(source.http.resets.get(), 0);
        assert!(source.http.steps.borrow().is_empty());

        let mut source = live_reader(
            &media(17, 1, false, "a", false),
            vec![
                ("/audio/s17.aac", vec![17]),
                ("/audio/media.m3u8", media(5, 6, false, "older", false)),
                ("/audio/s8.aac", vec![8]),
            ],
        );
        assert_eq!(segment(&mut source).bytes, [17]);
        let restarted = segment(&mut source);
        assert_eq!(restarted.bytes, [8]);
        assert!(restarted.discontinuity);
        assert_eq!(source.http.resets.get(), 1);
        assert_eq!(source.next, 9);
        assert!(source.http.steps.borrow().is_empty());
    }
    #[test]
    fn nonoverlapping_live_rewind_restarts_as_a_reset() {
        let mut source = live_reader(
            &media(10, 1, false, "a", false),
            vec![
                ("/audio/s10.aac", vec![10]),
                ("/audio/media.m3u8", media(3, 1, false, "b", false)),
                ("/audio/s3.aac", vec![3]),
            ],
        );
        assert_eq!(segment(&mut source).bytes, [10]);
        let restarted = segment(&mut source);
        assert_eq!(restarted.bytes, [3]);
        assert!(restarted.discontinuity);
        assert_eq!(source.http.skipped.get(), 0);
        assert_eq!(source.http.resets.get(), 1);
        assert_eq!(source.next, 4);
        assert!(source.http.steps.borrow().is_empty());

        let mut source = live_reader(
            &media(30, 1, false, "a", false),
            vec![
                ("/audio/s30.aac", vec![30]),
                ("/audio/media.m3u8", media(0, 6, false, "b", false)),
                ("/audio/s3.aac", vec![3]),
            ],
        );
        assert_eq!(segment(&mut source).bytes, [30]);
        let edge = segment(&mut source);
        assert_eq!(edge.bytes, [3]);
        assert!(edge.discontinuity);
        assert_eq!(source.next, 4);
        assert_eq!(source.http.skipped.get(), 3);
        assert_eq!(source.http.resets.get(), 1);
        assert!(source.http.steps.borrow().is_empty());
    }
    #[test]
    fn live_sequence_jump_past_the_playlist_length_resets_without_counting_skipped_segments() {
        let mut source = live_reader(
            &media(10, 1, false, "a", false),
            vec![
                ("/audio/s10.aac", vec![10]),
                ("/audio/media.m3u8", media(1_000_000, 3, false, "b", false)),
                ("/audio/s1000000.aac", vec![7]),
            ],
        );
        assert_eq!(segment(&mut source).bytes, [10]);
        let restarted = segment(&mut source);
        assert_eq!(restarted.bytes, [7]);
        assert!(restarted.discontinuity);
        assert_eq!(source.next, 1_000_001);
        assert_eq!(source.http.skipped.get(), 0);
        assert_eq!(source.http.resets.get(), 1);
        assert!(source.http.steps.borrow().is_empty());
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

    struct OutageFetch {
        waited: std::sync::Arc<std::sync::atomic::AtomicU64>,
        until: Wait,
        playlist: Vec<u8>,
        authentication: bool,
        cancel: bool,
        calls: Cell<usize>,
        audio: RefCell<Vec<String>>,
        skipped: Cell<u64>,
        resets: Cell<u64>,
        at_head: Cell<bool>,
    }
    impl Fetch for OutageFetch {
        fn note_skipped_segments(&self, count: u64) {
            self.skipped.set(self.skipped.get().saturating_add(count));
        }
        fn note_sequence_reset(&self) {
            self.resets.set(self.resets.get() + 1);
        }
        fn note_joined_window_head(&self, at_head: bool) {
            self.at_head.set(at_head);
        }
        fn cancelled(&self) -> bool {
            self.cancel
        }
        fn get(
            &self,
            url: Url,
            _: HeaderMap,
            _: usize,
            _: Option<(u64, u64)>,
            _: bool,
        ) -> Result<Reply, ProbeError> {
            if url.path().ends_with(".aac") {
                self.audio.borrow_mut().push(url.path().to_owned());
                return Ok(Reply {
                    headers: HeaderMap::new(),
                    bytes: vec![1],
                    url,
                });
            }
            self.calls.set(self.calls.get() + 1);
            if self.authentication {
                return Err(ProbeError::Authentication);
            }
            let waited = Wait::from_millis(self.waited.load(std::sync::atomic::Ordering::Relaxed));
            if waited < self.until {
                return Err(ProbeError::Network);
            }
            Ok(Reply {
                headers: HeaderMap::new(),
                bytes: self.playlist.clone(),
                url,
            })
        }
    }
    fn outage(
        live: bool,
        until: Wait,
        playlist: Vec<u8>,
        authentication: bool,
        cancel: bool,
    ) -> HlsStream<OutageFetch> {
        let waited = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let clock = waited.clone();
        let mut source = HlsStream::new(
            OutageFetch {
                waited,
                until,
                playlist,
                authentication,
                cancel,
                calls: Cell::new(0),
                audio: RefCell::new(Vec::new()),
                skipped: Cell::new(0),
                resets: Cell::new(0),
                at_head: Cell::new(false),
            },
            Url::parse("https://radiko.jp/audio/media.m3u8").unwrap(),
            HeaderMap::new(),
            parsed(&media(10, 1, false, "a", false)),
            Some("mp4a.40.2".into()),
        )
        .unwrap();
        source.live = live;
        source.sleeper = Some(Box::new(move |step| {
            clock.fetch_add(
                u64::try_from(step.as_millis()).unwrap_or(u64::MAX),
                std::sync::atomic::Ordering::Relaxed,
            );
            Ok(())
        }));
        let Next::Segment(first) = source.poll(Instant::now()).unwrap() else {
            panic!("the first segment is already in the playlist");
        };
        assert_eq!(first.bytes, vec![1]);
        assert!(!first.discontinuity);
        source
    }
    fn next_segment<F: Fetch>(source: &mut HlsStream<F>) -> Segment {
        match source.poll(Instant::now()) {
            Ok(Next::Segment(segment)) => segment,
            Ok(Next::Pending(_)) => panic!("expected a segment, playlist reload is still waiting"),
            Ok(Next::End) => panic!("expected a segment, playlist ended"),
            Err(error) => panic!("expected a segment, got {error}"),
        }
    }
    fn expect_err(result: Result<Next, ProbeError>) -> ProbeError {
        match result {
            Err(error) => error,
            Ok(_) => panic!("expected a playlist error"),
        }
    }
    fn waited(source: &HlsStream<OutageFetch>) -> Wait {
        Wait::from_millis(
            source
                .http
                .waited
                .load(std::sync::atomic::Ordering::Relaxed),
        )
    }
    #[test]
    fn live_outage_of_twenty_seconds_rejoins_the_window_head_with_the_cumulative_skip() {
        let mut source = outage(
            true,
            Wait::from_secs(20),
            media(13, 3, false, "b", false),
            false,
            false,
        );
        let segment = next_segment(&mut source);
        assert_eq!(segment.bytes, vec![1]);
        assert!(segment.discontinuity);
        assert_eq!(source.http.skipped.get(), 2);
        assert!(source.http.at_head.get());
        assert_eq!(source.http.resets.get(), 0);
        assert!(waited(&source) >= Wait::from_secs(20));
        assert!(waited(&source) < crate::reconnect::LIMIT);
        assert_eq!(source.next, 14);
        let audio = source.http.audio.borrow().clone();
        assert!(audio.iter().any(|path| path.ends_with("/s10.aac")));
        assert!(audio.iter().any(|path| path.ends_with("/s13.aac")));
        assert!(
            audio
                .iter()
                .all(|path| !path.ends_with("/s11.aac") && !path.ends_with("/s12.aac"))
        );
    }
    #[test]
    fn live_outage_of_ninety_seconds_rejoins_near_the_live_edge() {
        let mut source = outage(
            true,
            Wait::from_secs(90),
            media(13, 6, false, "b", false),
            false,
            false,
        );
        let segment = next_segment(&mut source);
        assert!(segment.discontinuity);
        assert_eq!(source.http.skipped.get(), 5);
        assert!(!source.http.at_head.get());
        assert_eq!(source.next, 17);
        assert!(waited(&source) >= Wait::from_secs(90));
        assert!(waited(&source) < crate::reconnect::LIMIT);
    }
    #[test]
    fn playlist_retry_waits_grow_until_the_cap_and_the_limit_uses_the_reselect_wording() {
        let mut source = outage(
            true,
            Wait::from_secs(10_000),
            media(13, 3, false, "b", false),
            false,
            false,
        );
        let steps = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = steps.clone();
        let clock = source.http.waited.clone();
        source.sleeper = Some(Box::new(move |step| {
            recorded.lock().unwrap().push(step);
            clock.fetch_add(
                u64::try_from(step.as_millis()).unwrap_or(u64::MAX),
                std::sync::atomic::Ordering::Relaxed,
            );
            Ok(())
        }));
        let error = expect_err(source.poll(Instant::now()));
        assert!(matches!(error, ProbeError::ReconnectGaveUp));
        assert!(error.to_string().contains("番組を選び直して"));
        assert!(waited(&source) >= crate::reconnect::LIMIT);
        assert_eq!(source.next, 11);
        assert_eq!(source.http.skipped.get(), 0);
        let steps = steps.lock().unwrap().clone();
        assert_eq!(
            steps[..4],
            [
                Wait::from_secs(1),
                Wait::from_secs(2),
                Wait::from_secs(4),
                Wait::from_secs(8),
            ]
        );
        assert!(steps[4..].iter().all(|step| *step <= Wait::from_secs(10)));
        assert!(steps.iter().any(|step| *step == Wait::from_secs(10)));
    }
    #[test]
    fn authentication_failure_during_playback_is_not_retried() {
        let mut source = outage(true, Wait::ZERO, Vec::new(), true, false);
        let error = expect_err(source.poll(Instant::now()));
        assert!(
            matches!(error, ProbeError::At(_, inner) if matches!(inner.as_ref(), ProbeError::Authentication))
        );
        assert_eq!(source.http.calls.get(), 1);
        assert_eq!(waited(&source), Wait::ZERO);
        assert_eq!(source.next, 11);
    }
    #[test]
    fn cancellation_interrupts_a_playlist_retry_without_waiting() {
        let mut source = outage(
            true,
            Wait::from_secs(20),
            media(13, 1, false, "b", false),
            false,
            true,
        );
        source.sleeper = None;
        let error = expect_err(source.poll(Instant::now()));
        assert!(matches!(error, ProbeError::Cancelled));
        assert_eq!(waited(&source), Wait::ZERO);
    }
    #[test]
    fn timefree_outage_resumes_the_same_sequence_and_a_lost_window_does_not_skip() {
        let mut source = outage(
            false,
            Wait::from_secs(20),
            media(11, 1, false, "b", false),
            false,
            false,
        );
        let segment = next_segment(&mut source);
        assert!(!segment.discontinuity);
        assert_eq!(source.next, 12);
        assert_eq!(source.http.skipped.get(), 0);
        assert!(
            source
                .http
                .audio
                .borrow()
                .iter()
                .any(|path| path.ends_with("/s11.aac"))
        );
        let mut lost = outage(
            false,
            Wait::from_secs(60),
            media(30, 1, false, "b", false),
            false,
            false,
        );
        let error = expect_err(lost.poll(Instant::now()));
        assert!(matches!(error, ProbeError::WindowLost));
        assert_eq!(error.to_string(), ProbeError::WINDOW_LOST);
        assert!(!matches!(error, ProbeError::InvalidPlaylist));
        assert!(
            !error
                .to_string()
                .contains("HLSプレイリストまたはサンプルの構造が不正")
        );
        assert_eq!(lost.next, 11);
        assert_eq!(lost.http.skipped.get(), 0);
    }
    #[test]
    fn timefree_lost_window_is_window_lost_and_does_not_skip_ahead() {
        let mut source = reader(
            &media(10, 1, false, "a", false),
            vec![
                ("/audio/s10.aac", vec![1]),
                ("/audio/media.m3u8", media(12, 1, false, "b", false)),
            ],
        );
        assert!(matches!(
            source.poll(Instant::now()).unwrap(),
            Next::Segment(_)
        ));
        let error = expect_err(source.poll(Instant::now()));
        assert!(matches!(error, ProbeError::WindowLost));
        assert_eq!(
            error.to_string(),
            "通信が長く途切れたため、続きを再生できなくなりました。番組を選び直してください"
        );
        assert_eq!(error.to_string(), ProbeError::WINDOW_LOST);
        assert!(
            !error
                .to_string()
                .contains("HLSプレイリストまたはサンプルの構造が不正・未対応です")
        );
        assert_eq!(source.next, 11);
        assert_eq!(source.http.skipped.get(), 0);
        assert_eq!(source.http.resets.get(), 0);
        assert!(source.http.steps.borrow().is_empty());
    }
}
