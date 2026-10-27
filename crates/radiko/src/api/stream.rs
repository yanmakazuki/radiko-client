//! 明示実行する配信検証。セッション、署名付きURL、音声データはログ出力・保存しない。
pub mod chapters;
pub mod playback;
use super::http::{Fetch, Http, trusted};
use crate::{
    domain::broadcast_day,
    player::{
        DecodeProbe,
        aac::{AdtsDecoder, DecodedPcm},
        probe_declared_sample,
    },
    session::{MemberSession, Membership},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use chrono::{DateTime, Duration, FixedOffset, NaiveDateTime, TimeZone, Utc};
use m3u8_rs::{AlternativeMediaType, KeyMethod, Playlist};
use reqwest::{
    Url,
    header::{HeaderMap, HeaderValue},
};
use serde::Deserialize;
use serde_json::Value;
use std::fmt;

const API: &str = "https://api.radiko.jp";
const META: &str = "https://api.annex-cf.radiko.jp/v1/programs/list";
const TEXT_LIMIT: usize = 1024 * 1024;
const SAMPLE_LIMIT: usize = 8 * 1024 * 1024;
// Public application key from the official playerCommon.js; not a member credential.
const PLAYER_KEY: &[u8] = b"bcd151073c03b352e1ef2fd66c32209da9ca0afa";

#[derive(Debug, PartialEq, Eq)]
pub enum ProbeError {
    At(&'static str, Box<ProbeError>),
    InvalidInput,
    InvalidPosition,
    Network,
    Cancelled,
    Http(u16),
    Authentication,
    AuthExpired,
    Denied,
    NotFound,
    RateLimited,
    TooLarge,
    UnsafeUrl,
    InvalidAuth,
    OutsideJapan,
    InvalidXml,
    InvalidProgram,
    Upcoming,
    StillOnAir,
    Expired,
    ContractRequired,
    AvailabilityUnknown,
    Unavailable,
    InvalidPlaylist,
    /// Time-free playback missed segments while the network was down. Not a malformed playlist.
    WindowLost,
    NoEndpoint,
    ReconnectGaveUp,
}
impl fmt::Display for ProbeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::At(stage, error) => write!(f, "{stage}: {error}"),
            Self::Http(status) => write!(f, "配信サーバーがHTTP {status}を返しました"),
            other => f.write_str(match other {
                Self::InvalidPosition => {
                    "シーク位置は番組冒頭以上・放送枠末尾未満で指定してください"
                }
                Self::InvalidInput => "局IDまたは日時が不正です（日時は日本時間のYYYYMMDDHHmmss）",
                Self::Cancelled => "配信取得を取り消しました",
                Self::Network => "配信の検証中に通信に失敗しました",
                Self::Authentication => {
                    "配信認証が拒否されました。sessionで会員認証を確認してください"
                }
                Self::AuthExpired => {
                    "認証期限切れのため再生を停止しました。aで再確認するか、loginで再ログインしてください"
                }
                Self::Denied => {
                    "配信サーバーがアクセスを拒否しました（契約・地域・配信条件の確認が必要です）"
                }
                Self::NotFound => "対象の番組または配信が見つかりません",
                Self::RateLimited => "アクセス制限中です。時間をおいて再実行してください",
                Self::TooLarge => "応答または音声サンプルのサイズ上限を超えました",
                Self::UnsafeUrl => "未対応の配信先またはリダイレクトです",
                Self::InvalidAuth => "配信認証の応答が想定形式と異なります",
                Self::OutsideJapan => "国内エリアを確認できませんでした",
                Self::InvalidXml => "局または配信設定XMLの形式が不正です",
                Self::InvalidProgram => "指定時刻と番組の放送枠を対応付けできません",
                Self::Upcoming => "放送前のため配信取得できません",
                Self::StillOnAir => "放送中のためタイムフリーの検証対象外です",
                Self::Expired => "タイムフリーの候補期限を過ぎています",
                Self::ContractRequired => {
                    "対象の契約を確認できません（エリアフリーまたはタイムフリー30が必要です）"
                }
                Self::AvailabilityUnknown => "番組の配信条件が未確認のため取得を中止しました",
                Self::Unavailable => "番組情報でタイムフリー配信対象外とされています",
                Self::InvalidPlaylist => "HLSプレイリストまたはサンプルの構造が不正・未対応です",
                Self::WindowLost => Self::WINDOW_LOST,
                Self::NoEndpoint => "対象モードと地域に対応する配信先がありません",
                Self::ReconnectGaveUp => crate::reconnect::GAVE_UP,
                Self::Http(_) => unreachable!(),
                Self::At(..) => unreachable!(),
            }),
        }
    }
}
impl std::error::Error for ProbeError {}
impl ProbeError {
    pub(crate) const WINDOW_LOST: &'static str =
        "通信が途切れている間に、この部分が配信窓から外れました。番組を選び直すと再生できます";
    fn at(self, stage: &'static str) -> Self {
        Self::At(stage, Box::new(self))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeMode {
    Live,
    TimeFree,
}

#[derive(Debug)]
pub struct ProbeReport {
    pub station: String,
    pub starts_at: DateTime<FixedOffset>,
    pub ends_at: DateTime<FixedOffset>,
    pub mode: ProbeMode,
    pub area: String,
    pub outside_area: bool,
    pub failed_endpoints: usize,
    pub segments: usize,
    pub playlist_seconds: f64,
    pub end_list: bool,
    pub declared_codec: Option<String>,
    pub container: &'static str,
    pub sample_bytes: usize,
    pub decode: DecodeProbe,
    /// One validated segment, never serialized or logged.
    pub audio: Option<DecodedPcm>,
}

#[derive(Clone)]
struct ChapterEvent {
    id: String,
    origin: DateTime<FixedOffset>,
}
#[derive(Clone)]
struct Program {
    chapter_event: Option<ChapterEvent>,
    title: Option<String>,
    start: DateTime<FixedOffset>,
    end: DateTime<FixedOffset>,
    timefree: [Option<bool>; 4],
    failed_record: Option<bool>,
}
struct PlaybackAuth {
    headers: HeaderMap,
    area: String,
}

pub fn parse_time(text: &str) -> Result<DateTime<FixedOffset>, ProbeError> {
    if text.len() != 14 || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ProbeError::InvalidInput);
    }
    let naive = NaiveDateTime::parse_from_str(text, "%Y%m%d%H%M%S")
        .map_err(|_| ProbeError::InvalidInput)?;
    FixedOffset::east_opt(9 * 3600)
        .and_then(|zone| zone.from_local_datetime(&naive).single())
        .ok_or(ProbeError::InvalidInput)
}

pub fn validate_station(station: &str) -> Result<(), ProbeError> {
    if station.is_empty()
        || station.len() > 32
        || !station
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_'))
    {
        return Err(ProbeError::InvalidInput);
    }
    Ok(())
}

pub fn probe(
    station: &str,
    mode: ProbeMode,
    cursor: DateTime<FixedOffset>,
    session: &MemberSession,
    membership: &Membership,
) -> Result<ProbeReport, ProbeError> {
    execute_probe(station, mode, cursor, session, membership, false)
}

pub fn probe_for_output(
    station: &str,
    mode: ProbeMode,
    cursor: DateTime<FixedOffset>,
    session: &MemberSession,
    membership: &Membership,
) -> Result<ProbeReport, ProbeError> {
    execute_probe(station, mode, cursor, session, membership, true)
}

struct ProbeOptions {
    now: DateTime<FixedOffset>,
    retain_audio: bool,
}
impl From<DateTime<FixedOffset>> for ProbeOptions {
    fn from(now: DateTime<FixedOffset>) -> Self {
        Self {
            now,
            retain_audio: false,
        }
    }
}

fn execute_probe(
    station: &str,
    mode: ProbeMode,
    cursor: DateTime<FixedOffset>,
    session: &MemberSession,
    membership: &Membership,
    retain_audio: bool,
) -> Result<ProbeReport, ProbeError> {
    validate_station(station)?;
    let now = Utc::now().fixed_offset();
    if mode == ProbeMode::TimeFree && cursor > now {
        return Err(ProbeError::Upcoming);
    }
    let http = Http::new()?;
    probe_with(
        &http,
        station,
        mode,
        cursor,
        ProbeOptions { now, retain_audio },
        session,
        membership,
    )
}

fn probe_with(
    http: &impl Fetch,
    station: &str,
    mode: ProbeMode,
    cursor: DateTime<FixedOffset>,
    options: impl Into<ProbeOptions>,
    session: &MemberSession,
    membership: &Membership,
) -> Result<ProbeReport, ProbeError> {
    let ProbeOptions { now, retain_audio } = options.into();
    let PreparedRequest {
        program,
        auth,
        outside_area,
        endpoints,
    } = prepare_with(http, station, mode, cursor, now, session, membership)?;
    let mut last_error = ProbeError::NoEndpoint;
    for (failed, endpoint) in endpoints.into_iter().enumerate() {
        match inspect_hls(http, endpoint, &auth.headers, retain_audio) {
            Ok(sample) => {
                return Ok(ProbeReport {
                    station: station.into(),
                    starts_at: program.start,
                    ends_at: program.end,
                    mode,
                    area: auth.area,
                    outside_area,
                    failed_endpoints: failed,
                    segments: sample.segments,
                    playlist_seconds: sample.seconds,
                    end_list: sample.end_list,
                    declared_codec: sample.codec,
                    container: sample.container,
                    sample_bytes: sample.bytes,
                    decode: sample.decode,
                    audio: sample.audio,
                });
            }
            Err(error @ (ProbeError::Authentication | ProbeError::RateLimited)) => {
                return Err(error);
            }
            Err(error) => last_error = error,
        }
    }
    Err(last_error.at("HLS・サンプル取得"))
}

struct PreparedRequest {
    program: Program,
    auth: PlaybackAuth,
    outside_area: bool,
    endpoints: Vec<Url>,
}
struct RequestedPosition {
    at: DateTime<FixedOffset>,
    position: std::time::Duration,
}
impl From<DateTime<FixedOffset>> for RequestedPosition {
    fn from(at: DateTime<FixedOffset>) -> Self {
        Self {
            at,
            position: std::time::Duration::ZERO,
        }
    }
}
fn prepare_with(
    http: &impl Fetch,
    station: &str,
    mode: ProbeMode,
    cursor: impl Into<RequestedPosition>,
    now: DateTime<FixedOffset>,
    session: &MemberSession,
    membership: &Membership,
) -> Result<PreparedRequest, ProbeError> {
    let cursor = cursor.into();
    let program = program(http, station, cursor.at).map_err(|e| e.at("放送枠取得"))?;
    eligible(&program, mode, now, membership)?;
    if (mode == ProbeMode::Live && !cursor.position.is_zero())
        || cursor.position
            >= (program.end - program.start)
                .to_std()
                .map_err(|_| ProbeError::InvalidInput)?
    {
        return Err(ProbeError::InvalidPosition);
    }
    let auth = authenticate(http, session).map_err(|e| e.at("配信認証"))?;
    let outside_area =
        !local_station(http, &auth.area, station).map_err(|e| e.at("地域の局一覧取得"))?;
    if outside_area && membership.area_free != Some(true) {
        return Err(ProbeError::ContractRequired);
    }
    if mode == ProbeMode::TimeFree {
        availability(&program, now, outside_area)?;
    }
    let config = http
        .get(
            url(&format!(
                "https://radiko.jp/v3/station/stream/pc_html5/{station}.xml"
            ))?,
            HeaderMap::new(),
            TEXT_LIMIT,
            None,
            false,
        )
        .map_err(|e| e.at("配信先設定の取得"))?;
    let endpoints = endpoints(&config.bytes, mode, outside_area)?
        .into_iter()
        .map(|endpoint| {
            let mut url = playlist_url(endpoint, station, mode, &program, outside_area)?;
            if !cursor.position.is_zero() {
                let seconds = cursor.position.as_secs().saturating_sub(10);
                let seek = program
                    .start
                    .checked_add_signed(Duration::seconds(
                        i64::try_from(seconds).map_err(|_| ProbeError::InvalidInput)?,
                    ))
                    .ok_or(ProbeError::InvalidInput)?;
                let zone = FixedOffset::east_opt(9 * 3600).ok_or(ProbeError::InvalidInput)?;
                url.query_pairs_mut().append_pair(
                    "seek",
                    &seek.with_timezone(&zone).format("%Y%m%d%H%M%S").to_string(),
                );
            }
            Ok(url)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(PreparedRequest {
        program,
        auth,
        outside_area,
        endpoints,
    })
}

fn url(text: &str) -> Result<Url, ProbeError> {
    Url::parse(text).map_err(|_| ProbeError::UnsafeUrl)
}
fn secret(value: &str) -> Result<HeaderValue, ProbeError> {
    let mut header = HeaderValue::from_str(value).map_err(|_| ProbeError::InvalidAuth)?;
    header.set_sensitive(true);
    Ok(header)
}
fn header<'a>(headers: &'a HeaderMap, name: &str) -> Result<&'a str, ProbeError> {
    headers
        .get(name)
        .and_then(|h| h.to_str().ok())
        .ok_or(ProbeError::InvalidAuth)
}
fn partial_key(offset: usize, length: usize) -> Result<String, ProbeError> {
    let end = offset.checked_add(length).ok_or(ProbeError::InvalidAuth)?;
    let slice = PLAYER_KEY
        .get(offset..end)
        .filter(|s| !s.is_empty())
        .ok_or(ProbeError::InvalidAuth)?;
    Ok(STANDARD.encode(slice))
}
fn authenticate(http: &impl Fetch, session: &MemberSession) -> Result<PlaybackAuth, ProbeError> {
    let mut headers = HeaderMap::new();
    headers.insert("x-radiko-app", HeaderValue::from_static("pc_html5"));
    headers.insert("x-radiko-app-version", HeaderValue::from_static("0.0.1"));
    headers.insert("x-radiko-device", HeaderValue::from_static("pc"));
    headers.insert("x-radiko-user", HeaderValue::from_static("dummy_user"));
    // Only the official API receives member credentials. No credentials in query parameters.
    headers.insert(
        "cookie",
        secret(&format!("radiko_session={}", session.token()))?,
    );
    let first = http.get(
        url(&format!("{API}/v2/api/auth1"))?,
        headers.clone(),
        64 * 1024,
        None,
        false,
    )?;
    let token = secret(header(&first.headers, "x-radiko-authtoken")?)?;
    let offset = header(&first.headers, "x-radiko-keyoffset")?
        .parse()
        .map_err(|_| ProbeError::InvalidAuth)?;
    let length = header(&first.headers, "x-radiko-keylength")?
        .parse()
        .map_err(|_| ProbeError::InvalidAuth)?;
    headers.insert("x-radiko-authtoken", token.clone());
    headers.insert(
        "x-radiko-partialkey",
        secret(&partial_key(offset, length)?)?,
    );
    headers.insert("x-radiko-session", secret(session.token())?);
    headers.insert("x-radiko-connection", HeaderValue::from_static("wifi"));
    let second = http.get(
        url(&format!("{API}/v2/api/auth2"))?,
        headers,
        64 * 1024,
        None,
        false,
    )?;
    let area = std::str::from_utf8(&second.bytes)
        .map_err(|_| ProbeError::InvalidAuth)?
        .trim()
        .split(',')
        .next()
        .ok_or(ProbeError::InvalidAuth)?;
    let number = area
        .strip_prefix("JP")
        .and_then(|s| s.parse::<u8>().ok())
        .filter(|n| (1..=47).contains(n))
        .ok_or(ProbeError::OutsideJapan)?;
    let area = format!("JP{number}");
    let mut headers = HeaderMap::new();
    headers.insert("x-radiko-authtoken", token);
    headers.insert(
        "x-radiko-areaid",
        HeaderValue::from_str(&area).map_err(|_| ProbeError::InvalidAuth)?,
    );
    Ok(PlaybackAuth { headers, area })
}

fn program(
    http: &impl Fetch,
    station: &str,
    cursor: DateTime<FixedOffset>,
) -> Result<Program, ProbeError> {
    let mut target = url(META)?;
    let day = broadcast_day(cursor)
        .ok_or(ProbeError::InvalidInput)?
        .format("%Y%m%d")
        .to_string();
    let after = cursor
        .checked_add_signed(Duration::seconds(1))
        .ok_or(ProbeError::InvalidInput)?
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    target.query_pairs_mut().extend_pairs([
        ("broadcastDate", day.as_str()),
        ("lastStartAt", after.as_str()),
        ("limit", "1"),
        ("stationId", station),
    ]);
    let reply = http.get(target, HeaderMap::new(), TEXT_LIMIT, None, false)?;
    parse_program(&reply.bytes, station, cursor)
}
fn parse_program(
    bytes: &[u8],
    station: &str,
    cursor: DateTime<FixedOffset>,
) -> Result<Program, ProbeError> {
    let root: Value = serde_json::from_slice(bytes).map_err(|_| ProbeError::InvalidProgram)?;
    let programs = root
        .get("programs")
        .and_then(Value::as_array)
        .ok_or(ProbeError::InvalidProgram)?;
    if programs.is_empty() {
        return Err(ProbeError::NotFound);
    }
    if programs.len() != 1 {
        return Err(ProbeError::InvalidProgram);
    }
    let value = &programs[0];
    if value
        .get("stationId")
        .and_then(Value::as_str)
        .is_some_and(|s| s != station)
    {
        return Err(ProbeError::InvalidProgram);
    }
    let read_time = |key| {
        value
            .get(key)
            .and_then(Value::as_str)
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .ok_or(ProbeError::InvalidProgram)
    };
    let start = read_time("startAt")?;
    let end = read_time("endAt")?;
    if end <= start || !(start <= cursor && cursor < end) {
        return Err(ProbeError::InvalidProgram);
    }
    let conditions = value.get("blackout");
    let read_flag = |key| {
        conditions.and_then(|v| v.get(key)).and_then(|v| {
            v.as_i64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
    };
    let allowed = |key| match read_flag(key) {
        Some(0) => Some(true),
        Some(2) => Some(false),
        _ => None,
    };
    let title = value
        .get("title")
        .and_then(Value::as_str)
        .map(|value| {
            value
                .chars()
                .filter(|c| !c.is_control())
                .take(512)
                .collect::<String>()
        })
        .filter(|value| !value.trim().is_empty());
    // eventId belongs to this API response, never to the schedule XML ID namespace.
    let chapter_event = value
        .get("eventId")
        .and_then(Value::as_str)
        .filter(|id| {
            !id.is_empty()
                && id.len() <= 128
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        })
        .zip(
            value
                .get("episodeIntroductionStartAt")
                .and_then(Value::as_str)
                .and_then(|text| DateTime::parse_from_rfc3339(text).ok()),
        )
        .filter(|_| value.get("stationId").and_then(Value::as_str) == Some(station))
        .map(|(id, origin)| ChapterEvent {
            id: id.into(),
            origin,
        });
    Ok(Program {
        chapter_event,
        title,
        start,
        end,
        timefree: [
            allowed("tsInNg"),
            allowed("tsplusInNg"),
            allowed("tsOutNg"),
            allowed("tsplusOutNg"),
        ],
        failed_record: read_flag("failedRecord").map(|n| n != 0),
    })
}
fn eligible(
    program: &Program,
    mode: ProbeMode,
    now: DateTime<FixedOffset>,
    membership: &Membership,
) -> Result<(), ProbeError> {
    if now < program.start {
        return Err(ProbeError::Upcoming);
    }
    if mode == ProbeMode::Live {
        return if now < program.end {
            Ok(())
        } else {
            Err(ProbeError::InvalidProgram)
        };
    }
    if now < program.end {
        return Err(ProbeError::StillOnAir);
    }
    // Provisional expiry follows the public implementation, based on the end's broadcast day.
    // The actual playlist response remains authoritative for per-program listening permission.
    let day = broadcast_day(program.end).ok_or(ProbeError::InvalidProgram)?;
    let boundary = day
        .succ_opt()
        .and_then(|d| d.and_hms_opt(5, 0, 0))
        .and_then(|d| {
            FixedOffset::east_opt(9 * 3600)?
                .from_local_datetime(&d)
                .single()
        })
        .ok_or(ProbeError::InvalidProgram)?;
    let free_end = boundary
        .checked_add_signed(Duration::days(7))
        .ok_or(ProbeError::InvalidProgram)?;
    let paid_end = boundary
        .checked_add_signed(Duration::days(30))
        .ok_or(ProbeError::InvalidProgram)?;
    if now >= paid_end {
        return Err(ProbeError::Expired);
    }
    if now >= free_end && membership.time_free_30 != Some(true) {
        return Err(ProbeError::ContractRequired);
    }
    Ok(())
}

fn availability(
    program: &Program,
    now: DateTime<FixedOffset>,
    outside: bool,
) -> Result<(), ProbeError> {
    match program.failed_record {
        Some(true) => return Err(ProbeError::Unavailable),
        None => return Err(ProbeError::AvailabilityUnknown),
        Some(false) => (),
    }
    let day = broadcast_day(program.end).ok_or(ProbeError::InvalidProgram)?;
    let boundary = day
        .succ_opt()
        .and_then(|d| d.and_hms_opt(5, 0, 0))
        .and_then(|d| {
            FixedOffset::east_opt(9 * 3600)?
                .from_local_datetime(&d)
                .single()
        })
        .and_then(|d| d.checked_add_signed(Duration::days(7)))
        .ok_or(ProbeError::InvalidProgram)?;
    let index = usize::from(outside) * 2 + usize::from(now >= boundary);
    match program.timefree[index] {
        Some(true) => Ok(()),
        Some(false) => Err(ProbeError::Unavailable),
        None => Err(ProbeError::AvailabilityUnknown),
    }
}

#[derive(Deserialize)]
struct Stations {
    #[serde(rename = "station", default)]
    stations: Vec<Station>,
}
#[derive(Deserialize)]
struct Station {
    id: String,
}
fn local_station(http: &impl Fetch, area: &str, station: &str) -> Result<bool, ProbeError> {
    let reply = http.get(
        url(&format!("https://radiko.jp/v3/station/list/{area}.xml"))?,
        HeaderMap::new(),
        TEXT_LIMIT,
        None,
        false,
    )?;
    let text = std::str::from_utf8(&reply.bytes).map_err(|_| ProbeError::InvalidXml)?;
    let list: Stations = quick_xml::de::from_str(text).map_err(|_| ProbeError::InvalidXml)?;
    if list.stations.is_empty() {
        return Err(ProbeError::InvalidXml);
    }
    Ok(list.stations.iter().any(|s| s.id == station))
}
#[derive(Deserialize)]
struct Endpoints {
    #[serde(rename = "url", default)]
    urls: Vec<Endpoint>,
}
#[derive(Deserialize)]
struct Endpoint {
    #[serde(rename = "@areafree")]
    area: u8,
    #[serde(rename = "@timefree")]
    timefree: u8,
    playlist_create_url: String,
}
fn endpoints(bytes: &[u8], mode: ProbeMode, outside: bool) -> Result<Vec<Url>, ProbeError> {
    let text = std::str::from_utf8(bytes).map_err(|_| ProbeError::InvalidXml)?;
    let config: Endpoints = quick_xml::de::from_str(text).map_err(|_| ProbeError::InvalidXml)?;
    let mut urls = Vec::new();
    for endpoint in config.urls {
        if endpoint.area != u8::from(outside)
            || endpoint.timefree != u8::from(mode == ProbeMode::TimeFree)
        {
            continue;
        }
        let target = url(&endpoint.playlist_create_url)?;
        if !trusted(&target) {
            return Err(ProbeError::UnsafeUrl);
        }
        if !urls.contains(&target) {
            urls.push(target);
        }
    }
    if urls.is_empty() {
        return Err(ProbeError::NoEndpoint);
    }
    Ok(urls)
}
fn playlist_url(
    mut target: Url,
    station: &str,
    mode: ProbeMode,
    program: &Program,
    outside: bool,
) -> Result<Url, ProbeError> {
    let mut random = [0u8; 16];
    getrandom::getrandom(&mut random).map_err(|_| ProbeError::Network)?;
    let lsid: String = random.iter().map(|b| format!("{b:02x}")).collect();
    let mut pairs = target.query_pairs_mut();
    pairs
        .append_pair("station_id", station)
        .append_pair("lsid", &lsid)
        // Match the already verified seek request for all time-free starts.
        // This only enlarges the requested playlist, never the bounded PCM queue.
        .append_pair(
            "l",
            if mode == ProbeMode::TimeFree {
                "60"
            } else {
                "15"
            },
        )
        .append_pair("type", if outside { "c" } else { "b" });
    if mode == ProbeMode::TimeFree {
        let jst = FixedOffset::east_opt(9 * 3600).unwrap_or_else(|| *program.start.offset());
        let start = program
            .start
            .with_timezone(&jst)
            .format("%Y%m%d%H%M%S")
            .to_string();
        let end = program
            .end
            .with_timezone(&jst)
            .format("%Y%m%d%H%M%S")
            .to_string();
        pairs
            .append_pair("start_at", &start)
            .append_pair("ft", &start)
            .append_pair("end_at", &end)
            .append_pair("to", &end);
    }
    drop(pairs);
    Ok(target)
}

struct HlsSample {
    segments: usize,
    seconds: f64,
    end_list: bool,
    codec: Option<String>,
    container: &'static str,
    bytes: usize,
    decode: DecodeProbe,
    audio: Option<DecodedPcm>,
}
fn inspect_hls(
    http: &impl Fetch,
    mut target: Url,
    headers: &HeaderMap,
    retain_audio: bool,
) -> Result<HlsSample, ProbeError> {
    let mut codec = None;
    let mut master_encrypted = false;
    for _ in 0..4 {
        let reply = http.get(target, headers.clone(), TEXT_LIMIT, None, true)?;
        target = reply.url;
        let playlist = parse_hls(&reply.bytes)?;
        match playlist {
            Playlist::MasterPlaylist(master) => {
                master_encrypted |= master
                    .session_key
                    .iter()
                    .any(|k| k.0.method != KeyMethod::None);
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
                let uri = alternative
                    .and_then(|a| a.uri.as_deref())
                    .unwrap_or(&variant.uri);
                target = target.join(uri).map_err(|_| ProbeError::UnsafeUrl)?;
            }
            Playlist::MediaPlaylist(media) => {
                let first = media.segments.first().ok_or(ProbeError::InvalidPlaylist)?;
                let encrypted = master_encrypted
                    || first
                        .key
                        .as_ref()
                        .is_some_and(|k| k.method != KeyMethod::None);
                let mut sample = Vec::new();
                if let Some(map) = &first.map {
                    let range = byte_range(map.byte_range.as_ref())?;
                    sample = http
                        .get(
                            target.join(&map.uri).map_err(|_| ProbeError::UnsafeUrl)?,
                            HeaderMap::new(),
                            TEXT_LIMIT,
                            range,
                            true,
                        )?
                        .bytes;
                }
                let segment = http
                    .get(
                        target.join(&first.uri).map_err(|_| ProbeError::UnsafeUrl)?,
                        HeaderMap::new(),
                        SAMPLE_LIMIT,
                        byte_range(first.byte_range.as_ref())?,
                        true,
                    )?
                    .bytes;
                if segment.is_empty() {
                    return Err(ProbeError::InvalidPlaylist);
                }
                let container = if encrypted {
                    "encrypted"
                } else {
                    container(&segment)
                };
                sample.extend_from_slice(&segment);
                let bytes = sample.len();
                let (decode, audio) = if encrypted {
                    (DecodeProbe::Encrypted, None)
                } else if retain_audio
                    && crate::player::audio_offset(&sample)
                        .and_then(|offset| sample.get(offset..))
                        .is_some_and(rusty_aac::is_adts)
                {
                    match AdtsDecoder::new().decode_segment_pcm(&sample, codec.as_deref()) {
                        Ok(pcm) => (DecodeProbe::AacDecoded(pcm.summary), Some(pcm)),
                        Err(error) => (DecodeProbe::AacFailed(error), None),
                    }
                } else {
                    (probe_declared_sample(sample, codec.as_deref()), None)
                };
                return Ok(HlsSample {
                    segments: media.segments.len(),
                    seconds: media.segments.iter().map(|s| f64::from(s.duration)).sum(),
                    end_list: media.end_list,
                    codec,
                    container,
                    bytes,
                    decode,
                    audio,
                });
            }
        }
    }
    Err(ProbeError::InvalidPlaylist)
}
fn byte_range(range: Option<&m3u8_rs::ByteRange>) -> Result<Option<(u64, u64)>, ProbeError> {
    range
        .map(|r| {
            r.offset
                .filter(|_| r.length > 0)
                .map(|offset| (offset, r.length))
                .ok_or(ProbeError::InvalidPlaylist)
        })
        .transpose()
}
fn parse_hls(bytes: &[u8]) -> Result<Playlist, ProbeError> {
    if bytes.len() > TEXT_LIMIT || !bytes.starts_with(b"#EXTM3U") {
        return Err(ProbeError::InvalidPlaylist);
    }
    let (remaining, playlist) =
        m3u8_rs::parse_playlist(bytes).map_err(|_| ProbeError::InvalidPlaylist)?;
    if !remaining.iter().all(u8::is_ascii_whitespace) {
        return Err(ProbeError::InvalidPlaylist);
    }
    if let Playlist::MediaPlaylist(media) = &playlist {
        if media.segments.is_empty()
            || media
                .segments
                .iter()
                .any(|s| s.uri.is_empty() || !s.duration.is_finite() || s.duration <= 0.0)
        {
            return Err(ProbeError::InvalidPlaylist);
        }
    }
    Ok(playlist)
}
fn safe_codec(text: &str) -> Option<String> {
    (text.len() <= 100
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b',' | b'-' | b'_' | b' ')))
    .then(|| text.to_owned())
}
fn container(bytes: &[u8]) -> &'static str {
    let Some(offset) = crate::player::audio_offset(bytes) else {
        return "invalid ID3";
    };
    let bytes = &bytes[offset..];
    if bytes.len() >= 8 && matches!(&bytes[4..8], b"ftyp" | b"styp" | b"moof" | b"sidx") {
        "ISO BMFF / fragmented MP4"
    } else if bytes.len() >= 2 && bytes[0] == 0xff && bytes[1] & 0xf6 == 0xf0 {
        if bytes[1] & 8 != 0 {
            "MPEG-2 AAC ADTS"
        } else if bytes[1] & 1 == 0 {
            "MPEG-4 AAC ADTS (CRC)"
        } else {
            "MPEG-4 AAC ADTS"
        }
    } else if bytes.len() > 188 && bytes[0] == 0x47 && bytes[188] == 0x47 {
        "MPEG-TS"
    } else if offset > 0 {
        "ID3 + unknown payload"
    } else {
        "unknown"
    }
}

#[cfg(test)]
mod tests {
    use super::super::http::Reply;
    use super::*;
    use std::{cell::RefCell, collections::VecDeque};

    struct Step {
        path: &'static str,
        bytes: Vec<u8>,
        headers: HeaderMap,
    }
    struct Mock {
        steps: RefCell<VecDeque<Step>>,
        seen: RefCell<Vec<Url>>,
    }
    impl Fetch for Mock {
        fn get(
            &self,
            url: Url,
            headers: HeaderMap,
            _: usize,
            range: Option<(u64, u64)>,
            _: bool,
        ) -> Result<Reply, ProbeError> {
            let step = self
                .steps
                .borrow_mut()
                .pop_front()
                .expect("unexpected request");
            assert_eq!(url.path(), step.path);
            if url.path().contains("/auth") {
                assert_eq!(
                    headers.get("cookie").unwrap(),
                    "radiko_session=synthetic-member"
                );
                if url.path().ends_with("auth2") {
                    assert_eq!(headers.get("x-radiko-session").unwrap(), "synthetic-member");
                }
            } else {
                assert!(!headers.contains_key("cookie"));
                assert!(!headers.contains_key("x-radiko-session"));
            }
            if url.path().ends_with("init.mp4") {
                assert_eq!(range, Some((0, 8)));
            }
            if url.path().ends_with("master.m3u8") {
                let pairs: std::collections::HashMap<_, _> = url.query_pairs().collect();
                assert_eq!(pairs.get("station_id").unwrap(), "TBS");
                let lsid = pairs.get("lsid").unwrap();
                assert_eq!(lsid.len(), 32);
                assert!(lsid.bytes().all(|b| b.is_ascii_hexdigit()));
                assert!(headers.get("x-radiko-authtoken").unwrap().is_sensitive());
            }
            self.seen.borrow_mut().push(url.clone());
            Ok(Reply {
                url,
                headers: step.headers,
                bytes: step.bytes,
            })
        }
    }
    fn step(path: &'static str, bytes: &[u8]) -> Step {
        Step {
            path,
            bytes: bytes.to_vec(),
            headers: HeaderMap::new(),
        }
    }
    fn member() -> MemberSession {
        MemberSession::new("synthetic-member".into()).unwrap()
    }
    fn membership() -> Membership {
        Membership {
            area_free: Some(true),
            time_free_30: Some(true),
        }
    }
    fn dt(text: &str) -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339(text).unwrap()
    }
    fn program_json(start: &str, end: &str, flag: i64) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({"programs":[{"stationId":"TBS","startAt":start,"endAt":end,"blackout":{"tsInNg":flag,"tsplusInNg":flag,"tsOutNg":flag,"tsplusOutNg":flag,"failedRecord":0}}]})).unwrap()
    }
    fn flow() -> Mock {
        let mut first = step("/v2/api/auth1", b"OK");
        first.headers.insert(
            "x-radiko-authtoken",
            HeaderValue::from_static("synthetic-playback"),
        );
        first
            .headers
            .insert("x-radiko-keyoffset", HeaderValue::from_static("1"));
        first
            .headers
            .insert("x-radiko-keylength", HeaderValue::from_static("4"));
        let steps = vec![
            step(
                "/v1/programs/list",
                &program_json("2026-10-15T11:00:00+09:00", "2026-10-15T13:00:00+09:00", 2),
            ),
            first,
            step("/v2/api/auth2", b"JP13,Tokyo,Tokyo\n"),
            step(
                "/v3/station/list/JP13.xml",
                b"<stations><station><id>TBS</id></station></stations>",
            ),
            step(
                "/v3/station/stream/pc_html5/TBS.xml",
                include_bytes!("../../../../tests/fixtures/streams.xml"),
            ),
            step(
                "/live/master.m3u8",
                include_bytes!("../../../../tests/fixtures/hls/master.m3u8"),
            ),
            step(
                "/live/audio/media.m3u8",
                include_bytes!("../../../../tests/fixtures/hls/media.m3u8"),
            ),
            step("/live/audio/init.mp4", b"\0\0\0\x08ftyp"),
            step("/live/audio/chunk.m4s", b"\0\0\0\x08moof"),
        ];
        Mock {
            steps: RefCell::new(steps.into()),
            seen: RefCell::new(Vec::new()),
        }
    }

    #[test]
    fn seek_preserves_original_slot_uses_preroll_and_rejects_out_of_slot_before_auth() {
        let start = "2026-10-14T11:00:00+09:00";
        let end = "2026-10-14T12:00:00+09:00";
        let mock = flow();
        mock.steps.borrow_mut()[0].bytes = program_json(start, end, 0);
        let prepared = prepare_with(
            &mock,
            "TBS",
            ProbeMode::TimeFree,
            RequestedPosition {
                at: dt(start),
                position: std::time::Duration::from_secs(60),
            },
            dt("2026-10-15T12:00:00+09:00"),
            &member(),
            &membership(),
        )
        .unwrap();
        assert_eq!(mock.seen.borrow().len(), 5);
        for url in prepared.endpoints {
            let pairs: std::collections::HashMap<_, _> = url.query_pairs().collect();
            assert_eq!(pairs.get("ft").unwrap(), "20261014110000");
            assert_eq!(pairs.get("start_at").unwrap(), "20261014110000");
            assert_eq!(pairs.get("end_at").unwrap(), "20261014120000");
            assert_eq!(pairs.get("seek").unwrap(), "20261014110050");
            assert_eq!(pairs.get("l").unwrap(), "60");
            assert_eq!(url.query_pairs().filter(|(key, _)| key == "l").count(), 1);
        }
        let mock = flow();
        mock.steps.borrow_mut()[0].bytes = program_json(start, end, 0);
        assert!(matches!(
            prepare_with(
                &mock,
                "TBS",
                ProbeMode::TimeFree,
                RequestedPosition {
                    at: dt(start),
                    position: std::time::Duration::from_secs(3600)
                },
                dt("2026-10-15T12:00:00+09:00"),
                &member(),
                &membership()
            ),
            Err(ProbeError::InvalidPosition)
        ));
        assert_eq!(mock.seen.borrow().len(), 1);
    }
    #[test]
    fn sample_pcm_is_retained_only_for_explicit_output_and_is_never_in_debug_logs() {
        use rusty_aac::{AacEncoder, AacEncoderConfig, AdtsHeader, write_adts_header};
        let mut encoder = AacEncoder::new(AacEncoderConfig::default());
        let samples: Vec<f32> = (0..2048).map(|i| (i as f32 * 0.05).sin() * 0.1).collect();
        encoder.push_pcm(&samples, 1, 24_000).unwrap();
        encoder.finish();
        let mut bytes = Vec::new();
        while let Ok(packet) = encoder.next_packet() {
            bytes.extend(write_adts_header(&AdtsHeader {
                object_type: 2,
                sample_rate: 24_000,
                channels: 1,
                frame_length: packet.data.len() + 7,
                header_len: 7,
            }));
            bytes.extend(packet.data);
        }
        for retain in [false, true] {
            let mock = Mock {steps: RefCell::new(vec![step("/audio.m3u8", b"#EXTM3U\n#EXT-X-TARGETDURATION:5\n#EXTINF:5,\nchunk.aac\n#EXT-X-ENDLIST\n"),step("/chunk.aac", &bytes)].into()),seen: RefCell::new(Vec::new())};
            let sample = inspect_hls(
                &mock,
                Url::parse("https://radiko.jp/audio.m3u8").unwrap(),
                &HeaderMap::new(),
                retain,
            )
            .unwrap();
            assert!(matches!(sample.decode, DecodeProbe::AacDecoded(_)));
            assert_eq!(sample.audio.is_some(), retain);
            if let Some(pcm) = sample.audio {
                assert_eq!(pcm.samples.len(), pcm.summary.frames);
            }
            assert!(mock.steps.borrow().is_empty());
        }
    }
    #[test]
    fn mapped_mp4_probe_decodes_only_the_first_packet_without_retaining_output_pcm() {
        let (init, media) = crate::player::fixtures::mp4_aac(&[0x13, 0x08]);
        for retain in [false, true] {
            let mock = Mock {
                steps: RefCell::new(vec![
                    step("/variants.m3u8", b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=64000,CODECS=\"mp4a.40.2\"\naudio.m3u8\n"),
                    step("/audio.m3u8", b"#EXTM3U\n#EXT-X-TARGETDURATION:5\n#EXT-X-MAP:URI=\"setup.mp4\"\n#EXTINF:5,\nchunk.m4s\n#EXT-X-ENDLIST\n"),
                    step("/setup.mp4", &init), step("/chunk.m4s", &media),
                ].into()), seen: RefCell::new(Vec::new()),
            };
            let result = inspect_hls(
                &mock,
                Url::parse("https://radiko.jp/variants.m3u8").unwrap(),
                &HeaderMap::new(),
                retain,
            )
            .unwrap();
            let DecodeProbe::RawAacDecoded(summary) = result.decode else {
                panic!("ASC AAC probe failed");
            };
            assert_eq!(
                (
                    summary.sample_rate,
                    summary.channels,
                    summary.frames,
                    summary.packets
                ),
                (24000, 1, 1024, 1)
            );
            assert!(result.audio.is_none());
            assert!(mock.steps.borrow().is_empty());
        }
    }
    #[test]
    fn live_probe_resolves_auth_and_hls_without_leaking_member_credentials_to_cdn() {
        let mock = flow();
        let now = dt("2026-10-15T12:00:00+09:00");
        let report = probe_with(
            &mock,
            "TBS",
            ProbeMode::Live,
            now,
            now,
            &member(),
            &membership(),
        )
        .unwrap();
        assert_eq!(report.segments, 1);
        assert_eq!(report.playlist_seconds, 5.0);
        assert!(report.end_list);
        assert!(!report.outside_area);
        assert_eq!(report.declared_codec.as_deref(), Some("mp4a.40.5"));
        assert_eq!(report.sample_bytes, 16);
        assert_eq!(report.container, "ISO BMFF / fragmented MP4");
        assert_eq!(report.decode, DecodeProbe::UnsupportedDeclaredCodec);
        assert!(mock.steps.borrow().is_empty());
        let seen = mock.seen.borrow();
        assert_eq!(
            seen[0]
                .query_pairs()
                .find(|(key, _)| key == "broadcastDate")
                .unwrap()
                .1,
            "20261015"
        );
        assert!(!format!("{report:?}").contains("synthetic"));
        assert!(!format!("{report:?}").contains("https://"));
    }

    #[test]
    fn timefree_probes_use_normal_and_30_day_conditions_and_slot_queries() {
        for (start, end) in [
            ("2026-10-14T11:00:00+09:00", "2026-10-14T13:00:00+09:00"),
            ("2026-10-05T11:00:00+09:00", "2026-10-05T13:00:00+09:00"),
        ] {
            let mock = flow();
            {
                let mut steps = mock.steps.borrow_mut();
                steps[0].bytes = program_json(start, end, 0);
                steps[5].path = "/timefree/master.m3u8";
                steps[6].path = "/timefree/audio/media.m3u8";
                steps[7].path = "/timefree/audio/init.mp4";
                steps[8].path = "/timefree/audio/chunk.m4s";
            }
            let report = probe_with(
                &mock,
                "TBS",
                ProbeMode::TimeFree,
                dt(start),
                dt("2026-10-15T12:00:00+09:00"),
                &member(),
                &membership(),
            )
            .unwrap();
            assert_eq!(report.mode, ProbeMode::TimeFree);
            assert_eq!(report.starts_at, dt(start));
            assert!(mock.steps.borrow().is_empty());
            let seen = mock.seen.borrow();
            assert!(seen[5].query_pairs().any(|(key, value)| key == "start_at"
                && value == dt(start).format("%Y%m%d%H%M%S").to_string()));
        }
    }

    #[test]
    fn insufficient_area_contract_stops_before_stream_configuration() {
        let mock = flow();
        mock.steps.borrow_mut()[3].bytes =
            b"<stations><station><id>FMT</id></station></stations>".to_vec();
        let now = dt("2026-10-15T12:00:00+09:00");
        let membership = Membership {
            area_free: Some(false),
            time_free_30: Some(true),
        };
        assert_eq!(
            probe_with(
                &mock,
                "TBS",
                ProbeMode::Live,
                now,
                now,
                &member(),
                &membership
            )
            .unwrap_err(),
            ProbeError::ContractRequired
        );
        assert_eq!(mock.seen.borrow().len(), 4);
    }

    #[test]
    fn metadata_title_is_optional_bounded_and_safe_for_terminal_display() {
        let mut value = serde_json::json!({"programs":[{"stationId":"TBS", "startAt":"2026-10-15T03:00:00Z", "endAt":"2026-10-15T04:00:00Z", "title":"合成\n番組\u{1b}"}]});
        let parse = |value: &Value| {
            parse_program(
                &serde_json::to_vec(value).unwrap(),
                "TBS",
                dt("2026-10-15T12:00:00+09:00"),
            )
            .unwrap()
        };
        assert_eq!(parse(&value).title.as_deref(), Some("合成番組"));
        value["programs"][0]["title"] = Value::String("あ".repeat(600));
        assert_eq!(parse(&value).title.unwrap().chars().count(), 512);
        for title in [Value::Null, Value::Bool(true), Value::String(" \n".into())] {
            value["programs"][0]["title"] = title;
            assert!(parse(&value).title.is_none());
        }
    }
    #[test]
    fn observed_blackout_flags_select_local_paid_and_outside_conditions() {
        let bytes = br#"{"programs":[{"stationId":"TBS","startAt":"2026-10-01T11:00:00+09:00","endAt":"2026-10-01T12:00:00+09:00","blackout":{"tsInNg":0,"tsplusInNg":2,"tsOutNg":2,"tsplusOutNg":0,"failedRecord":0}}]}"#;
        let program = parse_program(bytes, "TBS", dt("2026-10-01T11:00:00+09:00")).unwrap();
        let recent = dt("2026-10-02T12:00:00+09:00");
        let older = dt("2026-10-15T12:00:00+09:00");
        assert!(availability(&program, recent, false).is_ok());
        assert_eq!(
            availability(&program, older, false).unwrap_err(),
            ProbeError::Unavailable
        );
        assert_eq!(
            availability(&program, recent, true).unwrap_err(),
            ProbeError::Unavailable
        );
        assert!(availability(&program, older, true).is_ok());
        let failed = Program {
            failed_record: Some(true),
            ..program
        };
        assert_eq!(
            availability(&failed, recent, false).unwrap_err(),
            ProbeError::Unavailable
        );
    }

    #[test]
    fn upcoming_and_ended_targets_do_not_start_playback_authentication() {
        for (start, end, now, mode, error) in [
            (
                "2026-10-15T13:00:00+09:00",
                "2026-10-15T14:00:00+09:00",
                "2026-10-15T12:00:00+09:00",
                ProbeMode::TimeFree,
                ProbeError::Upcoming,
            ),
            (
                "2026-10-15T11:00:00+09:00",
                "2026-10-15T13:00:00+09:00",
                "2026-10-15T12:00:00+09:00",
                ProbeMode::TimeFree,
                ProbeError::StillOnAir,
            ),
            (
                "2026-10-15T11:00:00+09:00",
                "2026-10-15T13:00:00+09:00",
                "2026-10-15T13:00:00+09:00",
                ProbeMode::Live,
                ProbeError::InvalidProgram,
            ),
        ] {
            let mock = Mock {
                steps: RefCell::new(
                    vec![step("/v1/programs/list", &program_json(start, end, 2))].into(),
                ),
                seen: RefCell::new(Vec::new()),
            };
            assert_eq!(
                probe_with(
                    &mock,
                    "TBS",
                    mode,
                    dt(start),
                    dt(now),
                    &member(),
                    &membership()
                )
                .unwrap_err(),
                error
            );
            assert_eq!(mock.seen.borrow().len(), 1);
        }
    }

    #[test]
    fn timefree_contract_and_expiry_boundaries_and_blackouts_are_separate() {
        let program = Program {
            chapter_event: None,
            title: None,
            start: dt("2026-10-01T11:00:00+09:00"),
            end: dt("2026-10-01T12:00:00+09:00"),
            timefree: [Some(true); 4],
            failed_record: Some(false),
        };
        let free = Membership {
            area_free: Some(false),
            time_free_30: Some(false),
        };
        assert!(
            eligible(
                &program,
                ProbeMode::TimeFree,
                dt("2026-10-09T04:59:59+09:00"),
                &free
            )
            .is_ok()
        );
        assert_eq!(
            eligible(
                &program,
                ProbeMode::TimeFree,
                dt("2026-10-09T05:00:00+09:00"),
                &free
            )
            .unwrap_err(),
            ProbeError::ContractRequired
        );
        assert!(
            eligible(
                &program,
                ProbeMode::TimeFree,
                dt("2026-10-20T00:00:00+09:00"),
                &membership()
            )
            .is_ok()
        );
        assert_eq!(
            eligible(
                &program,
                ProbeMode::TimeFree,
                dt("2026-11-01T05:00:00+09:00"),
                &membership()
            )
            .unwrap_err(),
            ProbeError::Expired
        );
        let now = dt("2026-10-02T12:00:00+09:00");
        for (flag, error) in [
            (None, ProbeError::AvailabilityUnknown),
            (Some(false), ProbeError::Unavailable),
        ] {
            let program = Program {
                timefree: [flag; 4],
                ..program.clone()
            };
            assert_eq!(availability(&program, now, false).unwrap_err(), error);
        }
    }

    #[test]
    fn station_and_time_are_validated_and_wrong_programs_are_rejected() {
        for station in ["", "tbs", "../TBS", "TBS?x=y"] {
            assert!(validate_station(station).is_err());
        }
        for time in ["20261015250000", "20260230010000", "202610150100", "日本語"] {
            assert!(parse_time(time).is_err());
        }
        assert_eq!(
            parse_time("20261015010000").unwrap(),
            dt("2026-10-15T01:00:00+09:00")
        );
        let bytes = program_json("2026-10-15T11:00:00+09:00", "2026-10-15T12:00:00+09:00", 2);
        assert!(parse_program(&bytes, "FMT", dt("2026-10-15T11:30:00+09:00")).is_err());
        assert!(parse_program(&bytes, "TBS", dt("2026-10-15T12:00:00+09:00")).is_err());
    }

    #[test]
    fn endpoint_selection_preserves_modes_and_deduplicates() {
        let xml = include_bytes!("../../../../tests/fixtures/streams.xml");
        assert_eq!(
            endpoints(xml, ProbeMode::Live, false).unwrap()[0].path(),
            "/live/master.m3u8"
        );
        assert_eq!(
            endpoints(xml, ProbeMode::Live, true).unwrap()[0].path(),
            "/area/master.m3u8"
        );
        let urls = endpoints(xml, ProbeMode::TimeFree, false).unwrap();
        assert_eq!(urls.len(), 1);
        assert_eq!(urls[0].path(), "/timefree/master.m3u8");
        assert_eq!(
            endpoints(xml, ProbeMode::TimeFree, true).unwrap()[0].path(),
            "/paid/master.m3u8"
        );
        assert!(endpoints(b"<urls/>", ProbeMode::Live, false).is_err());
    }

    #[test]
    fn timefree_query_uses_actual_jst_slot_not_requested_cursor_or_utc() {
        let program = Program {
            chapter_event: None,
            title: None,
            start: dt("2026-10-14T16:00:00Z"),
            end: dt("2026-10-14T17:00:00Z"),
            timefree: [Some(true); 4],
            failed_record: Some(false),
        };
        let target = playlist_url(
            url("https://radiko.jp/playlist.m3u8").unwrap(),
            "TBS",
            ProbeMode::TimeFree,
            &program,
            true,
        )
        .unwrap();
        let pairs: std::collections::HashMap<_, _> = target.query_pairs().collect();
        assert_eq!(pairs.get("start_at").unwrap(), "20261015010000");
        assert_eq!(pairs.get("to").unwrap(), "20261015020000");
        assert_eq!(pairs.get("type").unwrap(), "c");
        assert_eq!(pairs.get("l").unwrap(), "60");
        assert_eq!(
            target.query_pairs().filter(|(key, _)| key == "l").count(),
            1
        );
        assert!(!pairs.contains_key("seek"));
        let live = playlist_url(
            url("https://radiko.jp/playlist.m3u8").unwrap(),
            "TBS",
            ProbeMode::Live,
            &program,
            false,
        )
        .unwrap();
        let live_pairs: std::collections::HashMap<_, _> = live.query_pairs().collect();
        assert_eq!(live_pairs.get("l").unwrap(), "15");
        assert!(!live_pairs.contains_key("start_at"));
        assert!(!live_pairs.contains_key("seek"));
    }

    #[test]
    fn partial_key_bounds_and_playlist_structure_are_checked() {
        assert_eq!(partial_key(0, 3).unwrap(), STANDARD.encode(b"bcd"));
        for (offset, len) in [(0, 0), (PLAYER_KEY.len(), 1), (usize::MAX, 2)] {
            assert!(partial_key(offset, len).is_err());
        }
        for bytes in [
            b"not hls".as_slice(),
            b"#EXTM3U\n",
            b"#EXTM3U\n#EXTINF:NaN,\na.aac\n",
            b"#EXTM3U\n#EXTINF:-1,\na.aac\n",
        ] {
            assert!(parse_hls(bytes).is_err());
        }
        assert!(safe_codec("mp4a.40.5\u{1b}").is_none());
        assert!(
            byte_range(Some(&m3u8_rs::ByteRange {
                length: 4,
                offset: None
            }))
            .is_err()
        );
    }
}
