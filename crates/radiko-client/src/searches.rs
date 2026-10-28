//! 保存した検索条件を管理するオフラインCLI。番組取得や認証通信は行わない。
use crate::stderr::stderr_ln;
use crate::stdout::stdout_ln;
use chrono::Utc;
use radiko::{
    domain::search::SearchCriteria,
    storage::searches::{Error, SearchStore},
};
use serde_json::json;
use std::path::PathBuf;
const USAGE: &str = "searches list / searches show NAME / searches restore / searches save NAME <programsの局・地域・日付・絞り込み> / searches delete NAME（通信なし）";
fn snapshot(value: &SearchCriteria) -> serde_json::Value {
    json!({"scope":value.scope(),"start":value.range().start().format("%Y%m%d").to_string(),"end":value.range().end().format("%Y%m%d").to_string(),"filter":value.filter()})
}
pub fn run(args: &[String], data_dir: Option<PathBuf>) -> Result<(), Box<dyn std::error::Error>> {
    if args == ["--help"] {
        stdout_ln!("{USAGE}");
        return Ok(());
    }
    let valid = match args.first().map(String::as_str) {
        Some("list" | "restore") => args.len() == 1,
        Some("show" | "delete") => args.len() == 2,
        Some("save") => args.len() >= 3,
        _ => false,
    };
    if !valid {
        return Err(USAGE.into());
    }
    let now = Utc::now().fixed_offset();
    // Reject invalid criteria before touching the filesystem.
    let criteria = if args[0] == "save" {
        if args[2..]
            .iter()
            .any(|arg| matches!(arg.as_str(), "--json" | "--classifications"))
        {
            return Err("保存操作には表示オプションを指定しないでください".into());
        }
        Some(crate::programs::criteria(&args[2..], now)?)
    } else {
        None
    };
    let store = match data_dir {
        Some(path) => SearchStore::new(path),
        None => SearchStore::default_location()?,
    };
    match args[0].as_str() {
        "list" => {
            for name in store.load()?.named.keys() {
                stdout_ln!("{name}");
            }
        }
        "show" => {
            let value = store
                .load()?
                .named
                .remove(&args[1])
                .ok_or(Error::NotFound)?;
            stdout_ln!("{}", serde_json::to_string_pretty(&snapshot(&value))?);
        }
        "restore" => {
            let criteria = match store.load() {
                Ok(library) => match library.last {
                    Some(last) => last.restore_startup(now)?,
                    None => SearchCriteria::initial(now)?,
                },
                Err(error) => {
                    stderr_ln!("検索条件を復元できません: {error}（当日・全国・指定なしを使用）");
                    SearchCriteria::initial(now)?
                }
            };
            stdout_ln!("{}", serde_json::to_string_pretty(&snapshot(&criteria))?);
        }
        "save" => {
            store.save_named(&args[1], criteria.ok_or(USAGE)?)?;
            stdout_ln!("名前付き検索条件を保存しました");
        }
        "delete" => {
            store.delete_named(&args[1])?;
            stdout_ln!("名前付き検索条件を削除しました");
        }
        _ => return Err(USAGE.into()),
    }
    Ok(())
}
