//! 明示的な会員認証の検証。HTTP本文やトークンをエラーへ含めない。
use crate::session::{MemberSession, Membership};
use reqwest::{
    blocking::Client,
    header::{COOKIE, HeaderValue},
    redirect::Policy,
};
use serde_json::Value;
use std::{
    fmt,
    io::Read,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const BASE: &str = "https://radiko.jp";
const LOGIN: &str = "/ap/member/webapi/member/login";
const CHECK: &str = "/ap/member/webapi/v2/member/login/check";
const MAX_RESPONSE: usize = 64 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum AuthError {
    InvalidCredentials,
    SessionExpired,
    Network,
    Http(u16),
    RateLimited,
    InvalidResponse,
    ResponseTooLarge,
    InvalidInput,
}
impl fmt::Display for AuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCredentials => {
                f.write_str("ログインに失敗しました。メールアドレスとパスワードを確認してください")
            }
            Self::SessionExpired => {
                f.write_str("セッションが失効しました。loginで再ログインしてください")
            }
            Self::Network => f.write_str("認証サーバーと通信できません"),
            Self::Http(status) => write!(f, "認証サーバーがHTTP {status}を返しました"),
            Self::RateLimited => {
                f.write_str("認証サーバーのアクセス制限中です。時間をおいて再実行してください")
            }
            Self::InvalidResponse => f.write_str("認証応答が想定形式と異なります"),
            Self::ResponseTooLarge => f.write_str("認証応答のサイズ上限を超えました"),
            Self::InvalidInput => f.write_str("メールアドレスまたはパスワードが空です"),
        }
    }
}
impl std::error::Error for AuthError {}

// Deliberately no Debug: a response can contain reusable authentication material.
struct Response {
    status: u16,
    bytes: Vec<u8>,
}
trait Transport {
    fn login(&self, mail: &str, password: &str) -> Result<Response, AuthError>;
    fn check(&self, session: &MemberSession) -> Result<Response, AuthError>;
}

pub struct MemberClient {
    transport: HttpTransport,
}

impl MemberClient {
    pub fn new() -> Result<Self, AuthError> {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(20))
            .redirect(Policy::none())
            .https_only(true)
            .user_agent(concat!("radiko-client/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| AuthError::Network)?;
        Ok(Self {
            transport: HttpTransport { client },
        })
    }

    /// POST is not retried: a lost response may already have created a session.
    pub fn login(
        &self,
        mail: &str,
        password: &str,
    ) -> Result<(MemberSession, Membership), AuthError> {
        login(&self.transport, mail, password, wait)
    }

    pub fn check(&self, session: &MemberSession) -> Result<Membership, AuthError> {
        check(&self.transport, session, wait)
    }
}

fn login(
    transport: &impl Transport,
    mail: &str,
    password: &str,
    sleep: impl Fn(Duration),
) -> Result<(MemberSession, Membership), AuthError> {
    if mail.trim().is_empty() || password.is_empty() {
        return Err(AuthError::InvalidInput);
    }
    let response = transport.login(mail, password)?;
    if response.status == 401 {
        return Err(AuthError::InvalidCredentials);
    }
    ensure_success(response.status)?;
    let value: Value =
        serde_json::from_slice(&response.bytes).map_err(|_| AuthError::InvalidResponse)?;
    // Do not persist account metadata, privileges, email, password, or the whole body.
    let token = value
        .get("radiko_session")
        .and_then(Value::as_str)
        .ok_or(AuthError::InvalidResponse)?;
    let session = MemberSession::new(token.to_owned()).map_err(|_| AuthError::InvalidResponse)?;
    // Login alone is insufficient: independently verify the returned session.
    let membership = check(transport, &session, sleep)?;
    Ok((session, membership))
}

fn check(
    transport: &impl Transport,
    session: &MemberSession,
    sleep: impl Fn(Duration),
) -> Result<Membership, AuthError> {
    let mut retries = 0;
    let response = loop {
        let response = transport.check(session);
        let retry = matches!(response, Err(AuthError::Network))
            || matches!(&response, Ok(r) if r.status == 408 || (500..600).contains(&r.status));
        if retry && retries < 3 {
            sleep(Duration::from_millis(100 * (1 << retries)));
            retries += 1;
            continue;
        }
        break response?;
    };
    if matches!(response.status, 400 | 401) {
        return Err(AuthError::SessionExpired);
    }
    ensure_success(response.status)?;
    parse_membership(&response.bytes)
}

fn wait(duration: Duration) {
    let jitter = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|v| v.subsec_millis() % 50)
        .unwrap_or(0);
    std::thread::sleep(duration + Duration::from_millis(u64::from(jitter)));
}

fn ensure_success(status: u16) -> Result<(), AuthError> {
    match status {
        200 => Ok(()),
        429 => Err(AuthError::RateLimited), // No automatic request before Retry-After.
        other => Err(AuthError::Http(other)),
    }
}

fn parse_membership(bytes: &[u8]) -> Result<Membership, AuthError> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| AuthError::InvalidResponse)?;
    match value.get("status") {
        Some(Value::String(s)) if s == "200" => (),
        Some(Value::Number(n)) if n.as_u64() == Some(200) => (),
        Some(Value::String(s)) if matches!(s.as_str(), "400" | "401") => {
            return Err(AuthError::SessionExpired);
        }
        Some(Value::Number(n)) if matches!(n.as_u64(), Some(400 | 401)) => {
            return Err(AuthError::SessionExpired);
        }
        _ => return Err(AuthError::InvalidResponse),
    }
    Ok(Membership {
        area_free: flag(value.get("areafree"))?,
        time_free_30: flag(value.get("timefreeplus"))?,
    })
}

fn flag(value: Option<&Value>) -> Result<Option<bool>, AuthError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(Value::String(s)) if s == "1" => Ok(Some(true)),
        Some(Value::String(s)) if s == "0" => Ok(Some(false)),
        Some(Value::Number(n)) if n.as_u64() == Some(1) => Ok(Some(true)),
        Some(Value::Number(n)) if n.as_u64() == Some(0) => Ok(Some(false)),
        _ => Err(AuthError::InvalidResponse),
    }
}

struct HttpTransport {
    client: Client,
}
impl Transport for HttpTransport {
    fn login(&self, mail: &str, password: &str) -> Result<Response, AuthError> {
        read_response(
            self.client
                .post(format!("{BASE}{LOGIN}"))
                .form(&[("mail", mail), ("pass", password)])
                .send(),
        )
    }

    fn check(&self, session: &MemberSession) -> Result<Response, AuthError> {
        let mut cookie = HeaderValue::from_str(&format!("radiko_session={}", session.token()))
            .map_err(|_| AuthError::InvalidResponse)?;
        cookie.set_sensitive(true);
        read_response(
            self.client
                .get(format!("{BASE}{CHECK}"))
                .header(COOKIE, cookie)
                .send(),
        )
    }
}

fn read_response(
    response: Result<reqwest::blocking::Response, reqwest::Error>,
) -> Result<Response, AuthError> {
    let response = response.map_err(|_| AuthError::Network)?;
    let status = response.status().as_u16();
    if status != 200 {
        return Ok(Response {
            status,
            bytes: Vec::new(),
        });
    }
    if response
        .content_length()
        .is_some_and(|n| n > MAX_RESPONSE as u64)
    {
        return Err(AuthError::ResponseTooLarge);
    }
    let mut bytes = Vec::new();
    response
        .take((MAX_RESPONSE + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| AuthError::Network)?;
    if bytes.len() > MAX_RESPONSE {
        return Err(AuthError::ResponseTooLarge);
    }
    Ok(Response { status, bytes })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::{Cell, RefCell},
        collections::VecDeque,
    };

    struct Mock {
        login: RefCell<Option<Result<Response, AuthError>>>,
        checks: RefCell<VecDeque<Result<Response, AuthError>>>,
        calls: Cell<usize>,
    }
    impl Transport for Mock {
        fn login(&self, _: &str, _: &str) -> Result<Response, AuthError> {
            self.login.borrow_mut().take().unwrap()
        }
        fn check(&self, _: &MemberSession) -> Result<Response, AuthError> {
            self.calls.set(self.calls.get() + 1);
            self.checks.borrow_mut().pop_front().unwrap()
        }
    }
    fn response(status: u16, json: &str) -> Result<Response, AuthError> {
        Ok(Response {
            status,
            bytes: json.as_bytes().to_vec(),
        })
    }
    fn mock(login: Result<Response, AuthError>, checks: Vec<Result<Response, AuthError>>) -> Mock {
        Mock {
            login: RefCell::new(Some(login)),
            checks: RefCell::new(checks.into()),
            calls: Cell::new(0),
        }
    }

    #[test]
    fn login_requires_verified_session_not_just_privileges() {
        let transport = mock(
            response(
                200,
                r#"{"radiko_session":"synthetic-token","privileges":["1","2"]}"#,
            ),
            vec![response(
                200,
                r#"{"status":"200","areafree":false,"timefreeplus":"0"}"#,
            )],
        );
        let (session, membership) = login(
            &transport,
            "example@example.invalid",
            "synthetic-password",
            |_| {},
        )
        .unwrap();
        assert_eq!(
            membership,
            Membership {
                area_free: Some(false),
                time_free_30: Some(false)
            }
        );
        assert!(!format!("{session:?}").contains("synthetic-token"));
        assert_eq!(transport.calls.get(), 1);
    }

    #[test]
    fn invalid_login_and_missing_token_never_check() {
        for result in [
            response(401, ""),
            response(200, r#"{"privileges":["1"]}"#),
            response(200, r#"{"radiko_session":"x; injected=true"}"#),
        ] {
            let transport = mock(result, vec![]);
            assert!(login(&transport, "mail", "pass", |_| {}).is_err());
            assert_eq!(transport.calls.get(), 0);
        }
    }

    #[test]
    fn failed_verification_does_not_return_session() {
        let transport = mock(
            response(200, r#"{"radiko_session":"synthetic"}"#),
            vec![response(401, "")],
        );
        assert_eq!(
            login(&transport, "mail", "pass", |_| {}).unwrap_err(),
            AuthError::SessionExpired
        );
    }

    #[test]
    fn check_retry_is_bounded_and_does_not_retry_expiry_or_rate_limits() {
        let session = MemberSession::new("synthetic".into()).unwrap();
        let transport = mock(
            response(500, ""),
            (0..4).map(|_| Err(AuthError::Network)).collect(),
        );
        let waits = RefCell::new(Vec::new());
        assert_eq!(
            check(&transport, &session, |d| waits.borrow_mut().push(d)).unwrap_err(),
            AuthError::Network
        );
        assert_eq!(transport.calls.get(), 4);
        assert_eq!(waits.borrow().len(), 3);
        for (status, error) in [
            (400, AuthError::SessionExpired),
            (401, AuthError::SessionExpired),
            (403, AuthError::Http(403)),
            (429, AuthError::RateLimited),
        ] {
            let transport = mock(response(500, ""), vec![response(status, "")]);
            assert_eq!(
                check(&transport, &session, |_| panic!("must not retry")).unwrap_err(),
                error
            );
            assert_eq!(transport.calls.get(), 1);
        }
    }

    #[test]
    fn transient_check_failure_recovers_and_unknown_contract_remains_unknown() {
        let transport = mock(
            response(500, ""),
            vec![
                response(503, ""),
                response(200, r#"{"status":200,"areafree":1}"#),
            ],
        );
        let session = MemberSession::new("synthetic".into()).unwrap();
        let membership = check(&transport, &session, |_| {}).unwrap();
        assert_eq!(
            membership,
            Membership {
                area_free: Some(true),
                time_free_30: None
            }
        );
    }

    #[test]
    fn malformed_or_unrecognised_status_never_means_logged_in() {
        for json in [
            "{}",
            "not json",
            r#"{"status":"500"}"#,
            r#"{"status":200,"areafree":"false"}"#,
        ] {
            assert_eq!(
                parse_membership(json.as_bytes()).unwrap_err(),
                AuthError::InvalidResponse
            );
        }
        assert_eq!(
            parse_membership(br#"{"status":"400"}"#).unwrap_err(),
            AuthError::SessionExpired
        );
    }
}
