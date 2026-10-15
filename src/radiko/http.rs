//! Bounded probe HTTP. Raw reqwest errors, URLs and bodies never escape this module.
use super::stream::ProbeError;
use reqwest::{
    Url,
    blocking::Client,
    header::{HeaderMap, LOCATION, RANGE},
    redirect::Policy,
};
use std::{
    io::Read,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub(crate) struct Http {
    client: Client,
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
        let client = Client::builder()
            .https_only(true)
            .redirect(Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(20))
            .user_agent(concat!("radiko-client/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| ProbeError::Network)?;
        Ok(Self { client })
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
            if !trusted(&url) {
                return Err(ProbeError::UnsafeUrl);
            }
            let mut retries = 0;
            let response = loop {
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
                let response = request.send();
                let transient = match &response {
                    Err(_) => true,
                    Ok(r) => r.status().is_server_error() || r.status().as_u16() == 408,
                };
                if transient && retries < 3 {
                    let jitter = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map(|n| n.subsec_millis() % 50)
                        .unwrap_or(0);
                    std::thread::sleep(Duration::from_millis(
                        100 * (1 << retries) + u64::from(jitter),
                    ));
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
            let mut bytes = Vec::new();
            response
                .take((limit + 1) as u64)
                .read_to_end(&mut bytes)
                .map_err(|_| ProbeError::Network)?;
            if bytes.len() > limit {
                return Err(ProbeError::TooLarge);
            }
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
