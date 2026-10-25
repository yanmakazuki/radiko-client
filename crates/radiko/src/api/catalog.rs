//! キャッシュを使わない局一覧・日別番組表の取得。セッションや再生要求は扱わない。
use super::{
    http::{Fetch, Http},
    stream::{ProbeError, parse_time, validate_station},
};
use crate::domain::{Classification, Program, ProgramKey};
use chrono::{Duration, FixedOffset, NaiveDate, TimeZone};
use quick_xml::{Reader, events::Event};
use reqwest::{Url, header::HeaderMap};
use serde::Deserialize;
use std::{collections::HashSet, fmt, io::Read};

pub const MAX_SCHEDULE_BYTES: usize = 4 * 1024 * 1024;
const MAX_PROGRAMS: usize = 2048;

#[derive(Debug, PartialEq, Eq)]
pub enum CatalogError {
    Cancelled,
    InvalidStation,
    InvalidDate,
    InvalidArea,
    WrongArea,
    Network,
    Http(u16),
    RateLimited,
    TooLarge,
    InvalidXml,
    WrongStation,
    NoValidPrograms(usize),
}
impl fmt::Display for CatalogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => f.write_str("検索を取り消しました"),
            Self::InvalidArea => f.write_str("地域IDが不正です（JP1〜JP47）"),
            Self::WrongArea => f.write_str("取得した局一覧が指定した地域と一致しません"),
            Self::InvalidStation => f.write_str("局IDが不正です（例: TBS、OBS、FMT）"),
            Self::InvalidDate => f.write_str("放送日が不正です（YYYYMMDDで指定してください）"),
            Self::Network => f.write_str("番組表サーバーと通信できません"),
            Self::Http(404) => f.write_str("指定した局・放送日の番組表が見つかりません"),
            Self::Http(status) => write!(f, "番組表サーバーがHTTP {status}を返しました"),
            Self::RateLimited => {
                f.write_str("番組表へのアクセス制限中です。時間をおいて再実行してください")
            }
            Self::TooLarge => f.write_str("番組表のサイズまたは番組件数の上限を超えました"),
            Self::InvalidXml => f.write_str("番組表XMLの構造が不正です"),
            Self::WrongStation => f.write_str("取得した番組表が指定した局と一致しません"),
            Self::NoValidPrograms(count) => write!(
                f,
                "番組表に指定日の有効な放送枠がありません（不正・日付不一致: {count}件）"
            ),
        }
    }
}
impl std::error::Error for CatalogError {}
impl From<ProbeError> for CatalogError {
    fn from(error: ProbeError) -> Self {
        match error {
            ProbeError::Network => Self::Network,
            ProbeError::Cancelled => Self::Cancelled,
            ProbeError::Http(status) => Self::Http(status),
            ProbeError::Authentication => Self::Http(401),
            ProbeError::Denied => Self::Http(403),
            ProbeError::NotFound => Self::Http(404),
            ProbeError::RateLimited => Self::RateLimited,
            ProbeError::TooLarge => Self::TooLarge,
            _ => Self::InvalidXml,
        }
    }
}

#[derive(Debug)]
pub struct ProgramList {
    pub station_id: String,
    pub station_name: String,
    pub broadcast_day: NaiveDate,
    pub programs: Vec<Program>,
    pub invalid_count: usize,
    pub duplicate_count: usize,
}

pub fn parse_day(text: &str) -> Result<NaiveDate, CatalogError> {
    if text.len() != 8 || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(CatalogError::InvalidDate);
    }
    NaiveDate::parse_from_str(text, "%Y%m%d").map_err(|_| CatalogError::InvalidDate)
}

pub fn list_programs(station: &str, day: NaiveDate) -> Result<ProgramList, CatalogError> {
    validate_station(station).map_err(|_| CatalogError::InvalidStation)?;
    let http = Http::new().map_err(CatalogError::from)?;
    fetch_programs(&http, station, day)
}

fn fetch_programs(
    http: &impl Fetch,
    station: &str,
    day: NaiveDate,
) -> Result<ProgramList, CatalogError> {
    validate_station(station).map_err(|_| CatalogError::InvalidStation)?;
    let url = Url::parse(&format!(
        "https://radiko.jp/v3/program/station/date/{}/{station}.xml",
        day.format("%Y%m%d")
    ))
    .map_err(|_| CatalogError::InvalidDate)?;
    let bytes = fetch_xml(http, url)?;
    parse_programs(&bytes, station, day)
}

fn fetch_xml(http: &impl Fetch, url: Url) -> Result<Vec<u8>, CatalogError> {
    let mut headers = HeaderMap::new();
    headers.insert(
        "accept-encoding",
        "identity".parse().map_err(|_| CatalogError::Network)?,
    );
    let reply = http.get(url, headers, MAX_SCHEDULE_BYTES, None, false)?;
    decode_body(
        &reply.bytes,
        reply
            .headers
            .get("content-encoding")
            .and_then(|h| h.to_str().ok()),
    )
}

fn decode_body(bytes: &[u8], encoding: Option<&str>) -> Result<Vec<u8>, CatalogError> {
    if bytes.len() > MAX_SCHEDULE_BYTES {
        return Err(CatalogError::TooLarge);
    }
    match encoding {
        None | Some("identity") => Ok(bytes.to_vec()),
        Some("gzip") => {
            let mut output = Vec::new();
            flate2::read::MultiGzDecoder::new(bytes)
                .take((MAX_SCHEDULE_BYTES + 1) as u64)
                .read_to_end(&mut output)
                .map_err(|_| CatalogError::InvalidXml)?;
            if output.len() > MAX_SCHEDULE_BYTES {
                return Err(CatalogError::TooLarge);
            }
            Ok(output)
        }
        _ => Err(CatalogError::InvalidXml),
    }
}

pub fn parse_programs(
    bytes: &[u8],
    station: &str,
    day: NaiveDate,
) -> Result<ProgramList, CatalogError> {
    validate_station(station).map_err(|_| CatalogError::InvalidStation)?;
    if bytes.len() > MAX_SCHEDULE_BYTES {
        return Err(CatalogError::TooLarge);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| CatalogError::InvalidXml)?;
    validate_xml(text, b"radiko")?;
    let document: Document = quick_xml::de::from_str(text).map_err(|_| CatalogError::InvalidXml)?;
    let mut stations = document.stations.stations;
    if stations.len() != 1 || stations[0].id != station {
        return Err(CatalogError::WrongStation);
    }
    let selected = stations.remove(0);
    let items = selected.progs.programs;
    if items.len() > MAX_PROGRAMS {
        return Err(CatalogError::TooLarge);
    }
    let zone = FixedOffset::east_opt(9 * 3600).ok_or(CatalogError::InvalidDate)?;
    let start = day
        .and_hms_opt(5, 0, 0)
        .and_then(|d| zone.from_local_datetime(&d).single())
        .ok_or(CatalogError::InvalidDate)?;
    let end = start
        .checked_add_signed(Duration::days(1))
        .ok_or(CatalogError::InvalidDate)?;
    let mut result = ProgramList {
        station_id: station.into(),
        station_name: clean_text(&selected.name),
        broadcast_day: day,
        programs: Vec::new(),
        invalid_count: 0,
        duplicate_count: 0,
    };
    let mut seen = HashSet::new();
    for item in items {
        let (Ok(starts_at), Ok(ends_at)) = (parse_time(&item.ft), parse_time(&item.to)) else {
            result.invalid_count += 1;
            continue;
        };
        // Include slots that straddle 05:00; never clip or merge split slots.
        if ends_at <= starts_at || starts_at >= end || ends_at <= start {
            result.invalid_count += 1;
            continue;
        }
        let key = ProgramKey {
            station_id: station.into(),
            starts_at,
        };
        if !seen.insert(key.clone()) {
            result.duplicate_count += 1;
            continue;
        }
        let title = clean_text(&item.title);
        let tags: Vec<_> = item
            .tag
            .items
            .into_iter()
            .map(|t| clean_text(&t.name))
            .filter(|t| !t.is_empty())
            .collect();
        let mut seen_tags = HashSet::new();
        result.programs.push(Program {
            key,
            program_id: clean_text(&item.id),
            ends_at,
            title: if title.is_empty() {
                "無題".into()
            } else {
                title
            },
            performer: clean_text(&item.pfm),
            description: clean_text(&item.desc),
            info: clean_text(&item.info),
            tags: tags
                .into_iter()
                .filter(|t| seen_tags.insert(t.clone()))
                .collect(),
            program_genres: classifications(item.genre.program),
            personality_genres: classifications(item.genre.personality),
        });
    }
    if result.programs.is_empty() && result.invalid_count > 0 {
        return Err(CatalogError::NoValidPrograms(result.invalid_count));
    }
    result.programs.sort_by_key(|p| p.key.starts_at);
    Ok(result)
}

/// Collapse whitespace and remove terminal control characters before any display.
fn clean_text(text: &str) -> String {
    let cleaned: String = text
        .chars()
        .filter_map(|c| {
            if c.is_whitespace() {
                Some(' ')
            } else if c.is_control() {
                None
            } else {
                Some(c)
            }
        })
        .collect();
    cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
}
fn classifications(items: Vec<ClassDto>) -> Vec<Classification> {
    let mut seen = HashSet::new();
    items
        .into_iter()
        .map(|c| Classification {
            id: clean_text(&c.id),
            name: clean_text(&c.name),
        })
        .filter(|c| !c.id.is_empty() || !c.name.is_empty())
        .filter(|c| seen.insert(c.clone()))
        .collect()
}

fn validate_xml(text: &str, root: &[u8]) -> Result<(), CatalogError> {
    let mut reader = Reader::from_str(text);
    let mut depth = 0usize;
    let mut seen_root = false;
    loop {
        match reader.read_event().map_err(|_| CatalogError::InvalidXml)? {
            Event::Start(tag) => {
                if depth == 0 {
                    if seen_root || tag.name().as_ref() != root {
                        return Err(CatalogError::InvalidXml);
                    }
                    seen_root = true;
                }
                depth += 1;
                if depth > 32 {
                    return Err(CatalogError::InvalidXml);
                }
            }
            Event::Empty(tag) if depth == 0 => {
                if seen_root || tag.name().as_ref() != root {
                    return Err(CatalogError::InvalidXml);
                }
                seen_root = true;
            }
            Event::End(_) => {
                depth = depth.checked_sub(1).ok_or(CatalogError::InvalidXml)?;
            }
            Event::DocType(_) => return Err(CatalogError::InvalidXml),
            Event::Text(t) if depth == 0 && !t.as_ref().iter().all(u8::is_ascii_whitespace) => {
                return Err(CatalogError::InvalidXml);
            }
            Event::Eof => {
                return if seen_root && depth == 0 {
                    Ok(())
                } else {
                    Err(CatalogError::InvalidXml)
                };
            }
            _ => (),
        }
    }
}

#[derive(Deserialize)]
struct Document {
    stations: StationsDto,
}
#[derive(Deserialize)]
struct StationsDto {
    #[serde(rename = "station", default)]
    stations: Vec<StationDto>,
}
#[derive(Deserialize)]
struct StationDto {
    #[serde(rename = "@id")]
    id: String,
    name: String,
    progs: ProgsDto,
}
#[derive(Deserialize)]
struct ProgsDto {
    #[serde(rename = "prog", default)]
    programs: Vec<ProgramDto>,
}
#[derive(Deserialize, Default)]
#[serde(default)]
struct ProgramDto {
    #[serde(rename = "@id")]
    id: String,
    #[serde(rename = "@ft")]
    ft: String,
    #[serde(rename = "@to")]
    to: String,
    title: String,
    pfm: String,
    desc: String,
    info: String,
    tag: TagsDto,
    genre: GenresDto,
}
#[derive(Deserialize, Default)]
struct TagsDto {
    #[serde(rename = "item", default)]
    items: Vec<TagDto>,
}
#[derive(Deserialize)]
struct TagDto {
    #[serde(default)]
    name: String,
}
#[derive(Deserialize, Default)]
struct GenresDto {
    #[serde(default)]
    program: Vec<ClassDto>,
    #[serde(default)]
    personality: Vec<ClassDto>,
}
#[derive(Deserialize)]
struct ClassDto {
    #[serde(rename = "@id", default)]
    id: String,
    #[serde(default)]
    name: String,
}

#[cfg(test)]
mod tests {
    use super::super::http::Reply;
    use super::*;
    use std::cell::Cell;

    #[test]
    fn cancelled_catalog_client_rejects_construction_before_network_and_preserves_error_kind() {
        let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        assert!(matches!(
            CatalogClient::new_cancellable(flag),
            Err(CatalogError::Cancelled)
        ));
        assert_eq!(
            CatalogError::from(ProbeError::Cancelled),
            CatalogError::Cancelled
        );
    }
    #[test]
    fn gzip_is_bounded_after_decompression_and_corruption_is_rejected() {
        use std::io::Write;
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder
            .write_all(include_bytes!("../../../../tests/fixtures/schedule.xml"))
            .unwrap();
        let compressed = encoder.finish().unwrap();
        assert_eq!(
            decode_body(&compressed, Some("gzip")).unwrap(),
            include_bytes!("../../../../tests/fixtures/schedule.xml")
        );
        assert!(decode_body(&compressed[..compressed.len() - 4], Some("gzip")).is_err());
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder
            .write_all(&vec![b' '; MAX_SCHEDULE_BYTES + 1])
            .unwrap();
        assert!(matches!(
            decode_body(&encoder.finish().unwrap(), Some("gzip")),
            Err(CatalogError::TooLarge)
        ));
    }
    struct Mock(Cell<usize>);
    impl Fetch for Mock {
        fn get(
            &self,
            url: Url,
            headers: HeaderMap,
            limit: usize,
            range: Option<(u64, u64)>,
            redirects: bool,
        ) -> Result<Reply, ProbeError> {
            assert_eq!(
                url.as_str(),
                "https://radiko.jp/v3/program/station/date/20261015/TBS.xml"
            );
            assert_eq!(headers.len(), 1);
            assert_eq!(headers["accept-encoding"], "identity");
            assert_eq!(limit, MAX_SCHEDULE_BYTES);
            assert_eq!(range, None);
            assert!(!redirects);
            self.0.set(self.0.get() + 1);
            if self.0.get() == 2 {
                return Err(ProbeError::Network);
            }
            Ok(Reply {
                headers: HeaderMap::new(),
                bytes: include_bytes!("../../../../tests/fixtures/schedule.xml").to_vec(),
                url,
            })
        }
    }
    #[test]
    fn every_request_refetches_without_credentials_or_stale_fallback() {
        let mock = Mock(Cell::new(0));
        let day = parse_day("20261015").unwrap();
        assert!(fetch_programs(&mock, "../TBS", day).is_err());
        assert_eq!(mock.0.get(), 0);
        assert_eq!(fetch_programs(&mock, "TBS", day).unwrap().programs.len(), 3);
        assert!(matches!(
            fetch_programs(&mock, "TBS", day),
            Err(CatalogError::Network)
        ));
        assert_eq!(mock.0.get(), 2);
    }
}

/// A bounded, session-free metadata client. Instances never cache responses.
pub struct CatalogClient {
    http: Http,
}
impl CatalogClient {
    pub fn new() -> Result<Self, CatalogError> {
        Ok(Self { http: Http::new()? })
    }
    pub fn new_cancellable(
        cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<Self, CatalogError> {
        use std::sync::atomic::Ordering;
        if cancel.load(Ordering::Acquire) {
            return Err(CatalogError::Cancelled);
        }
        let mut http = Http::new()?;
        http.set_cancellation(std::sync::Arc::new(move || cancel.load(Ordering::Acquire)));
        Ok(Self { http })
    }
    pub fn programs(&self, station: &str, day: NaiveDate) -> Result<ProgramList, CatalogError> {
        fetch_programs(&self.http, station, day)
    }
    pub fn stations(&self, area: &str) -> Result<Vec<crate::domain::Station>, CatalogError> {
        fetch_stations(&self.http, area)
    }
}

pub fn validate_area(area: &str) -> Result<(), CatalogError> {
    if (1..=47).any(|n| area == format!("JP{n}")) {
        Ok(())
    } else {
        Err(CatalogError::InvalidArea)
    }
}

fn fetch_stations(
    http: &impl Fetch,
    area: &str,
) -> Result<Vec<crate::domain::Station>, CatalogError> {
    validate_area(area)?;
    let url = Url::parse(&format!("https://radiko.jp/v3/station/list/{area}.xml"))
        .map_err(|_| CatalogError::InvalidArea)?;
    parse_stations(&fetch_xml(http, url)?, area)
}

pub fn parse_stations(
    bytes: &[u8],
    area: &str,
) -> Result<Vec<crate::domain::Station>, CatalogError> {
    validate_area(area)?;
    if bytes.len() > MAX_SCHEDULE_BYTES {
        return Err(CatalogError::TooLarge);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| CatalogError::InvalidXml)?;
    validate_xml(text, b"stations")?;
    let document: AreaStationsDto =
        quick_xml::de::from_str(text).map_err(|_| CatalogError::InvalidXml)?;
    if document.area_id != area {
        return Err(CatalogError::WrongArea);
    }
    if document.stations.len() > 512 {
        return Err(CatalogError::TooLarge);
    }
    // A missing/empty station list is an acquisition failure, not an empty region.
    if document.stations.is_empty() {
        return Err(CatalogError::InvalidXml);
    }
    let mut seen = HashSet::new();
    let mut stations = Vec::new();
    for item in document.stations {
        validate_station(&item.id).map_err(|_| CatalogError::InvalidStation)?;
        if seen.insert(item.id.clone()) {
            stations.push(crate::domain::Station {
                id: item.id,
                name: clean_text(&item.name),
            });
        }
    }
    Ok(stations)
}

#[derive(Deserialize)]
struct AreaStationsDto {
    #[serde(rename = "@area_id")]
    area_id: String,
    #[serde(rename = "station", default)]
    stations: Vec<AreaStationDto>,
}
#[derive(Deserialize)]
struct AreaStationDto {
    id: String,
    name: String,
}
