//! 状態を保持するHLS取得。URL・ヘッダー・音声データはDebug出力の対象にしない。
use super::*;
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
    fn set_cancellation(&mut self, _cancelled: std::sync::Arc<dyn Fn() -> bool + Send + Sync>) {}
    fn poll_segment(&mut self) -> Result<Next, ProbeError>;
}
impl<T: SegmentSource + ?Sized> SegmentSource for Box<T> {
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
/// Cooperative cancellation between HTTP operations; in-flight blocking I/O retains its timeout.
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
                let hls = HlsStream::new(http, url, request.auth.headers, media, codec)?;
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
            s.map.is_some()
                || s.byte_range.is_some()
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

struct HlsStream<F> {
    http: F,
    url: Url,
    headers: HeaderMap,
    media: m3u8_rs::MediaPlaylist,
    codec: Option<String>,
    next: u64,
    discontinuity: Option<u64>,
    reload_at: Instant,
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
        })
    }
    fn poll(&mut self, now: Instant) -> Result<Next, ProbeError> {
        if self.next < self.media.media_sequence {
            return Err(ProbeError::InvalidPlaylist.at("未取得セグメントが配信窓から消失"));
        }
        let index = usize::try_from(self.next - self.media.media_sequence)
            .map_err(|_| ProbeError::InvalidPlaylist)?;
        if let Some(segment) = self.media.segments.get(index) {
            let discontinuity = self.media.segments[..=index]
                .iter()
                .filter(|s| s.discontinuity)
                .count() as u64;
            let sequence = self
                .media
                .discontinuity_sequence
                .checked_add(discontinuity)
                .ok_or(ProbeError::InvalidPlaylist)?;
            let target = self
                .url
                .join(&segment.uri)
                .map_err(|_| ProbeError::UnsafeUrl)?;
            let bytes = self
                .http
                .get(target, HeaderMap::new(), SAMPLE_LIMIT, None, true)?
                .bytes;
            if bytes.is_empty() {
                return Err(ProbeError::InvalidPlaylist);
            }
            let reset = self
                .discontinuity
                .is_some_and(|previous| previous != sequence);
            self.discontinuity = Some(sequence);
            self.next = self
                .next
                .checked_add(1)
                .ok_or(ProbeError::InvalidPlaylist)?;
            return Ok(Next::Segment(Segment {
                bytes,
                codec: self.codec.clone(),
                discontinuity: reset,
                starts_at: segment_time(&self.media, index),
            }));
        }
        if self.media.end_list {
            return Ok(Next::End);
        }
        if now < self.reload_at {
            return Ok(Next::Pending(self.reload_at.duration_since(now)));
        }
        let reply = self.http.get(
            self.url.clone(),
            self.headers.clone(),
            TEXT_LIMIT,
            None,
            true,
        )?;
        let Playlist::MediaPlaylist(media) = parse_hls(&reply.bytes)? else {
            return Err(ProbeError::InvalidPlaylist);
        };
        validate_media(&media)?;
        if media.media_sequence < self.media.media_sequence {
            return Err(ProbeError::InvalidPlaylist);
        }
        // Sequence numbers, not changing signed URLs, decide which segment is new.
        let changed = media.media_sequence != self.media.media_sequence
            || media.segments.len() != self.media.segments.len();
        let wait = Wait::from_secs(media.target_duration).div_f64(if changed { 1.0 } else { 2.0 });
        self.reload_at = now + wait;
        self.media = media;
        self.url = reply.url;
        if self.next < self.media.media_sequence {
            return Err(ProbeError::InvalidPlaylist.at("未取得セグメントが配信窓から消失"));
        }
        if self.next < self.media.media_sequence + self.media.segments.len() as u64 {
            return self.poll(now);
        }
        if self.media.end_list {
            Ok(Next::End)
        } else {
            Ok(Next::Pending(wait))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::radiko::http::Reply;
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
    fn unsupported_encryption_maps_and_ranges_are_rejected_before_segment_requests() {
        for tag in [
            "#EXT-X-KEY:METHOD=AES-128,URI=\"key\"\n",
            "#EXT-X-MAP:URI=\"init.mp4\"\n",
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
}
