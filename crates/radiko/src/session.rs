//! 再利用可能な会員セッションだけを保持する。パスワードやアカウント応答全体は保存しない。
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemberSession {
    token: String,
}

impl MemberSession {
    pub fn new(token: String) -> Result<Self, &'static str> {
        // RFC 6265 cookie-octet; exclude separators and all control characters.
        if token.is_empty()
            || token.len() > 4096
            || !token
                .bytes()
                .all(|b| matches!(b, 0x21 | 0x23..=0x2b | 0x2d..=0x3a | 0x3c..=0x5b | 0x5d..=0x7e))
        {
            return Err("セッション形式が不正です");
        }
        Ok(Self { token })
    }

    pub(crate) fn token(&self) -> &str {
        &self.token
    }

    pub(crate) fn validate(&self) -> bool {
        Self::new(self.token.clone()).is_ok()
    }
}

impl fmt::Debug for MemberSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MemberSession([REDACTED])")
    }
}

/// Contract flags are independent of login and of per-program listening rights.
#[derive(Debug, PartialEq, Eq)]
pub struct Membership {
    pub area_free: Option<bool>,
    pub time_free_30: Option<bool>,
}
