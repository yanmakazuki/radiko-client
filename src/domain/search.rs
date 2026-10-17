//! 検索条件の検証と起動時の復元。番組ジャンルと出演者分類は独立した条件として保持する。
use super::{BroadcastRange, broadcast_day};
use chrono::{DateTime, FixedOffset};
use std::collections::BTreeSet;

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "FilterInput")]
pub struct Filter {
    pub(crate) program_genres: Vec<String>,
    pub(crate) personality_genres: Vec<String>,
    pub(crate) unclassified_program: bool,
    pub(crate) unclassified_personality: bool,
    pub(crate) keyword: Option<String>,
    #[serde(skip)]
    pub(crate) keyword_lowercase: Option<String>,
}
impl Filter {
    pub fn with_keyword(&self, keyword: Option<String>) -> Result<Self, &'static str> {
        Self::new(
            self.program_genres.clone(),
            self.personality_genres.clone(),
            self.unclassified_program,
            self.unclassified_personality,
            keyword,
        )
    }
    pub fn program_genres(&self) -> &[String] {
        &self.program_genres
    }
    pub fn personality_genres(&self) -> &[String] {
        &self.personality_genres
    }
    pub fn unclassified_program(&self) -> bool {
        self.unclassified_program
    }
    pub fn unclassified_personality(&self) -> bool {
        self.unclassified_personality
    }
    pub fn keyword(&self) -> Option<&str> {
        self.keyword.as_deref()
    }
    pub fn new(
        program_genres: Vec<String>,
        personality_genres: Vec<String>,
        unclassified_program: bool,
        unclassified_personality: bool,
        keyword: Option<String>,
    ) -> Result<Self, &'static str> {
        fn ids(values: Vec<String>) -> Result<Vec<String>, &'static str> {
            if values
                .iter()
                .any(|s| s.trim().is_empty() || s.len() > 128 || s.chars().any(char::is_control))
            {
                return Err("分類IDは空でない128バイト以内の値で指定してください");
            }
            Ok(values
                .into_iter()
                .map(|s| s.trim().to_owned())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect())
        }
        let keyword = keyword.map(|s| s.trim().to_owned());
        if keyword
            .as_ref()
            .is_some_and(|s| s.is_empty() || s.len() > 1024 || s.chars().any(char::is_control))
        {
            return Err("キーワードは空でない1024バイト以内の文字列で指定してください");
        }
        let keyword_lowercase = keyword.as_ref().map(|s| s.to_lowercase());
        Ok(Self {
            program_genres: ids(program_genres)?,
            personality_genres: ids(personality_genres)?,
            unclassified_program,
            unclassified_personality,
            keyword,
            keyword_lowercase,
        })
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FilterInput {
    program_genres: Vec<String>,
    personality_genres: Vec<String>,
    unclassified_program: bool,
    unclassified_personality: bool,
    keyword: Option<String>,
}
impl TryFrom<FilterInput> for Filter {
    type Error = &'static str;
    fn try_from(value: FilterInput) -> Result<Self, Self::Error> {
        Self::new(
            value.program_genres,
            value.personality_genres,
            value.unclassified_program,
            value.unclassified_personality,
            value.keyword,
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum Scope {
    Nationwide,
    Station(String),
    Areas(Vec<String>),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchCriteria {
    scope: Scope,
    range: BroadcastRange,
    filter: Filter,
}
impl SearchCriteria {
    pub fn new(
        mut scope: Scope,
        range: BroadcastRange,
        filter: Filter,
    ) -> Result<Self, &'static str> {
        match &mut scope {
            Scope::Nationwide => (),
            Scope::Station(id) => {
                if id.is_empty()
                    || id.len() > 32
                    || !id.bytes().all(|b| {
                        b.is_ascii_uppercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_')
                    })
                {
                    return Err("保存する局IDが不正です");
                }
            }
            Scope::Areas(areas) => {
                if areas.is_empty()
                    || areas
                        .iter()
                        .any(|area| !(1..=47).any(|n| area == &format!("JP{n}")))
                {
                    return Err("保存する地域指定が不正です");
                }
                areas.sort();
                areas.dedup();
            }
        }
        Ok(Self {
            scope,
            range,
            filter,
        })
    }
    pub fn scope(&self) -> &Scope {
        &self.scope
    }
    pub fn range(&self) -> &BroadcastRange {
        &self.range
    }
    pub fn filter(&self) -> &Filter {
        &self.filter
    }
    pub fn initial(now: DateTime<FixedOffset>) -> Result<Self, &'static str> {
        Self::new(
            Scope::Nationwide,
            BroadcastRange::single(broadcast_day(now).ok_or("放送日を計算できません")?),
            Filter::default(),
        )
    }
    /// Automatic startup restoration always resets both saved dates to today's broadcast day.
    pub fn restore_startup(&self, now: DateTime<FixedOffset>) -> Result<Self, &'static str> {
        Self::new(
            self.scope.clone(),
            BroadcastRange::single(broadcast_day(now).ok_or("放送日を計算できません")?),
            self.filter.clone(),
        )
    }
}
