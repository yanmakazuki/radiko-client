//! 番組一覧CLIの条件解析と表示。取得件数と絞り込み後の件数、取得失敗を区別する。
use chrono::{DateTime, FixedOffset, Utc};
use radiko_client::catalog::search::{Filter, ObservedClassifications};
use radiko_client::{
    domain::search::{Scope, SearchCriteria},
    storage::searches::{Error as SearchError, SearchStore},
};
use radiko_client::{
    domain::{BroadcastRange, BroadcastStatus, Classification, broadcast_day, broadcast_status},
    radiko::{catalog, stream},
};
use serde_json::{Value, json};
use std::path::PathBuf;

const USAGE: &str = "programs STATION [START [END]] [--json] / programs (--areas JP13,JP14 | --all) [START [END]] [--json] [--genre IDS] [--personality IDS] [--keyword TEXT] [--genre-unclassified] [--personality-unclassified] [--classifications]（START・END: YYYYMMDD、両端を含む最大30放送日）";
struct Request {
    station: Option<String>,
    areas: Vec<String>,
    day: chrono::NaiveDate,
    range: BroadcastRange,
    json: bool,
    filter: Filter,
    classifications: bool,
}

fn request(
    args: &[String],
    now: DateTime<FixedOffset>,
) -> Result<Request, Box<dyn std::error::Error>> {
    let mut genres = None;
    let mut personalities = None;
    let mut keyword = None;
    let mut unclassified_program = false;
    let mut unclassified_personality = false;
    let mut classifications = false;
    let mut positional = Vec::new();
    let mut json_output = false;
    let mut areas = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--json" if !json_output => json_output = true,
            "--classifications" if !classifications => classifications = true,
            "--genre" if genres.is_none() => {
                genres = Some(
                    iter.next()
                        .filter(|s| !s.starts_with("--"))
                        .ok_or(USAGE)?
                        .split(',')
                        .map(str::to_string)
                        .collect(),
                )
            }
            "--personality" if personalities.is_none() => {
                personalities = Some(
                    iter.next()
                        .filter(|s| !s.starts_with("--"))
                        .ok_or(USAGE)?
                        .split(',')
                        .map(str::to_string)
                        .collect(),
                )
            }
            "--keyword" if keyword.is_none() => {
                keyword = Some(
                    iter.next()
                        .filter(|s| !s.starts_with("--"))
                        .ok_or(USAGE)?
                        .clone(),
                )
            }
            "--genre-unclassified" if !unclassified_program => unclassified_program = true,
            "--personality-unclassified" if !unclassified_personality => {
                unclassified_personality = true
            }
            "--areas" if areas.is_none() => {
                let value = iter.next().ok_or(USAGE)?;
                let values: Vec<String> = value.split(',').map(String::from).collect();
                for area in &values {
                    catalog::validate_area(area)?;
                }
                areas = Some(values);
            }
            "--all" if areas.is_none() => {
                areas = Some((1..=47).map(|n| format!("JP{n}")).collect())
            }
            value if value.starts_with("--") => return Err(USAGE.into()),
            _ => positional.push(arg),
        }
    }
    let station = if areas.is_some() {
        None
    } else {
        if positional.is_empty() {
            return Err(USAGE.into());
        }
        let station = positional.remove(0);
        stream::validate_station(station)?;
        Some(station.clone())
    };
    if positional.len() > 2 {
        return Err(USAGE.into());
    }
    let day = match positional.first() {
        Some(day) => catalog::parse_day(day)?,
        None => broadcast_day(now).ok_or("放送日を計算できません")?,
    };
    let end = positional
        .get(1)
        .map(|value| catalog::parse_day(value))
        .transpose()?
        .unwrap_or(day);
    let range = BroadcastRange::new(day, end)?;
    Ok(Request {
        range,
        station,
        areas: areas.unwrap_or_default(),
        day,
        json: json_output,
        filter: Filter::new(
            genres.unwrap_or_default(),
            personalities.unwrap_or_default(),
            unclassified_program,
            unclassified_personality,
            keyword,
        )?,
        classifications,
    })
}

pub(crate) fn criteria(
    args: &[String],
    now: DateTime<FixedOffset>,
) -> Result<SearchCriteria, Box<dyn std::error::Error>> {
    let request = request(args, now)?;
    criteria_from_request(&request)
}
fn criteria_from_request(request: &Request) -> Result<SearchCriteria, Box<dyn std::error::Error>> {
    let scope = if let Some(station) = &request.station {
        Scope::Station(station.clone())
    } else if request
        .areas
        .iter()
        .collect::<std::collections::BTreeSet<_>>()
        .len()
        == 47
    {
        Scope::Nationwide
    } else {
        Scope::Areas(request.areas.clone())
    };
    Ok(SearchCriteria::new(
        scope,
        request.range.clone(),
        request.filter.clone(),
    )?)
}
fn from_criteria(criteria: &SearchCriteria, json: bool, classifications: bool) -> Request {
    let (station, areas) = match criteria.scope() {
        Scope::Station(station) => (Some(station.clone()), vec![]),
        Scope::Areas(areas) => (None, areas.clone()),
        Scope::Nationwide => (None, (1..=47).map(|n| format!("JP{n}")).collect()),
    };
    Request {
        station,
        areas,
        day: criteria.range().start(),
        range: criteria.range().clone(),
        filter: criteria.filter().clone(),
        json,
        classifications,
    }
}
fn restored_request(
    args: &[String],
    now: DateTime<FixedOffset>,
    store: Option<&SearchStore>,
) -> Result<(Request, bool), Box<dyn std::error::Error>> {
    let saved = args.first().is_some_and(|arg| arg == "--saved");
    let tail = if saved {
        args.get(2..).ok_or(USAGE)?
    } else {
        &args[1..]
    };
    let (mut json, mut classifications) = (false, false);
    for arg in tail {
        match arg.as_str() {
            "--json" if !json => json = true,
            "--classifications" if !classifications => classifications = true,
            _ => return Err(USAGE.into()),
        }
    }
    let criteria = if saved {
        let store = store.ok_or("検索条件保存先を解決できません")?;
        store
            .load()?
            .named
            .remove(&args[1])
            .ok_or(SearchError::NotFound)?
    } else if let Some(store) = store {
        match store.load() {
            Ok(library) => match library.last {
                Some(last) => last.restore_startup(now)?,
                None => SearchCriteria::initial(now)?,
            },
            Err(error) => {
                eprintln!("検索条件を復元できません: {error}（当日・全国・指定なしで検索）");
                return Ok((
                    from_criteria(&SearchCriteria::initial(now)?, json, classifications),
                    false,
                ));
            }
        }
    } else {
        SearchCriteria::initial(now)?
    };
    Ok((from_criteria(&criteria, json, classifications), true))
}
pub fn run(args: &[String], data_dir: Option<PathBuf>) -> Result<(), Box<dyn std::error::Error>> {
    if args == ["--help"] {
        println!(
            "{USAGE}\nprograms --saved NAME [--json] [--classifications] / programs --restore [--json] [--classifications]"
        );
        return Ok(());
    }
    let now = Utc::now().fixed_offset();
    let restoring = args
        .first()
        .is_some_and(|arg| matches!(arg.as_str(), "--saved" | "--restore"));
    let explicit = if restoring {
        None
    } else {
        Some(request(args, now)?)
    };
    let store = match data_dir {
        Some(path) => Some(SearchStore::new(path)),
        None => match SearchStore::default_location() {
            Ok(store) => Some(store),
            Err(error) => {
                eprintln!("検索条件を保存できません: {error}");
                None
            }
        },
    };
    let (request, allow_save) = match explicit {
        Some(request) => (request, true),
        None => restored_request(args, now, store.as_ref())?,
    };
    if allow_save {
        if let Some(store) = &store {
            if let Err(error) = store.save_last(criteria_from_request(&request)?) {
                eprintln!("検索条件を保存できません: {error}（現在の条件で検索を継続）");
            }
        }
    }
    if let Some(station) = request
        .station
        .as_ref()
        .filter(|_| request.range.days().len() == 1)
    {
        return print_station_day(station, &request, now);
    }
    acquire_range(&request, now)
}

fn print_station_day(
    station: &str,
    request: &Request,
    now: DateTime<FixedOffset>,
) -> Result<(), Box<dyn std::error::Error>> {
    let list = catalog::list_programs(station, request.day)?;
    if request.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&snapshot(&list, now, &request.filter)?)?
        );
    } else {
        print_list(&list, now, &request.filter)?;
        if request.classifications {
            print_classifications(&ObservedClassifications::collect(&list.programs));
        }
    }
    if list.invalid_count > 0 {
        return Err("取得結果に不正な放送枠があり、結果が不完全です".into());
    }
    Ok(())
}

fn acquire_range(
    request: &Request,
    now: DateTime<FixedOffset>,
) -> Result<(), Box<dyn std::error::Error>> {
    let client = catalog::CatalogClient::new()?;
    let mut display_error = None;
    let mut on_progress = |progress: radiko_client::catalog::Progress<'_>| {
        eprintln!(
            "{} {}/{} / 失敗: {} / {} {}",
            progress.phase,
            progress.completed,
            progress.total,
            progress.failed,
            progress.target,
            progress.day.map(|day| day.to_string()).unwrap_or_default()
        );
        if !request.json {
            if let Some(list) = progress.list {
                if let Err(error) = print_list(list, now, &request.filter) {
                    display_error = Some(error);
                }
            }
        }
    };
    let report = if let Some(station) = &request.station {
        radiko_client::catalog::acquire_stations(
            &client,
            std::slice::from_ref(station),
            &request.range,
            &mut on_progress,
        )?
    } else {
        radiko_client::catalog::acquire_range(
            &client,
            &request.areas,
            &request.range,
            &mut on_progress,
        )?
    };
    if let Some(error) = display_error {
        return Err(error);
    }
    if request.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report_snapshot(
                &report,
                &request.range,
                now,
                &request.filter
            )?)?
        );
    } else {
        println!(
            "{} / 放送日 {}〜{} / 局一覧成功: {}/{}地域 / 番組表成功: {}/{}局・日",
            if report.complete() {
                "取得完了"
            } else {
                "部分取得・未取得あり"
            },
            request.range.start(),
            request.range.end(),
            report.areas.len() - report.area_failures.len(),
            report.areas.len(),
            report.lists.len(),
            report.stations.len() * request.range.days().len()
        );
        for failure in &report.area_failures {
            eprintln!(
                "未取得地域 {}（対象局も未確定）: {}",
                failure.target, failure.error
            );
        }
        for failure in &report.schedule_failures {
            eprintln!(
                "未取得番組表 {} {}: {}",
                failure.target,
                failure.day.map(|day| day.to_string()).unwrap_or_default(),
                failure.error
            );
        }
    }
    if !request.json && request.classifications {
        print_classifications(&ObservedClassifications::collect(
            report.lists.iter().flat_map(|list| &list.programs),
        ));
    }
    if !report.complete() {
        return Err("取得結果に欠落があります。同じコマンドの再実行で全件を再取得できます".into());
    }
    Ok(())
}

fn observed_snapshot(observed: &ObservedClassifications) -> Value {
    json!({"scope":"observed_in_current_acquisition", "program_genres":classes(&observed.program_genres), "personality_genres":classes(&observed.personality_genres), "unclassified_programs":observed.unclassified_programs, "unclassified_personalities":observed.unclassified_personalities})
}
fn print_classifications(observed: &ObservedClassifications) {
    println!("今回の取得範囲で観測した分類（全分類を保証しません）");
    for (label, items) in [
        ("番組ジャンル", &observed.program_genres),
        ("出演者分類", &observed.personality_genres),
    ] {
        for class in items {
            println!("{}: {} / {}", label, class.id, class.name);
        }
    }
    println!(
        "未分類: 番組ジャンル{}枠 / 出演者分類{}枠",
        observed.unclassified_programs, observed.unclassified_personalities
    );
}

fn report_snapshot(
    report: &radiko_client::catalog::Report,
    range: &BroadcastRange,
    now: DateTime<FixedOffset>,
    filter: &Filter,
) -> Result<Value, &'static str> {
    let lists = report
        .lists
        .iter()
        .map(|l| snapshot(l, now, filter))
        .collect::<Result<Vec<_>, _>>()?;
    let stations: Vec<_> = report
        .stations
        .iter()
        .map(|s| json!({"id":s.station.id,"name":s.station.name,"areas":s.areas}))
        .collect();
    let failures = |items: &[radiko_client::catalog::Failure]| -> Vec<Value> {
        items
            .iter()
            .map(|f| json!({"target":f.target,"broadcast_day":f.day.map(|day|day.to_string()),"reason":f.error.to_string()}))
            .collect()
    };
    Ok(
        json!({"broadcast_day":if range.days().len()==1 {Some(range.start().to_string())} else {None},"date_range":{"start":range.start().to_string(),"end":range.end().to_string(),"days":range.days().len()},"timezone":"Asia/Tokyo","as_of":now.to_rfc3339(),"complete":report.complete(),"areas":report.areas,"stations":stations,"successful_stations":report.lists.iter().map(|list|&list.station_id).collect::<std::collections::BTreeSet<_>>().len(),"successful_schedules":report.lists.len(),"requested_schedules":report.stations.len()*range.days().len(),"area_failures":failures(&report.area_failures),"schedule_failures":failures(&report.schedule_failures),"filters":filter,"observed_classifications":observed_snapshot(&ObservedClassifications::collect(report.lists.iter().flat_map(|list| &list.programs))),"source_program_count":report.lists.iter().map(|l|l.programs.len()).sum::<usize>(),"matched_program_count":lists.iter().filter_map(|l|l["matched_program_count"].as_u64()).sum::<u64>(),"schedules":lists}),
    )
}

fn print_list(
    list: &catalog::ProgramList,
    now: DateTime<FixedOffset>,
    filter: &Filter,
) -> Result<(), Box<dyn std::error::Error>> {
    let value = snapshot(list, now, filter)?;
    println!(
        "{} ({}) / 放送日 {} / 日本時間（実行時点の状態）",
        list.station_name, list.station_id, list.broadcast_day
    );
    for item in value["programs"]
        .as_array()
        .ok_or("一覧の生成に失敗しました")?
    {
        println!(
            "{}–{} [{}] {}",
            item["starts_at"].as_str().unwrap_or_default(),
            item["ends_at"].as_str().unwrap_or_default(),
            item["status_label"].as_str().unwrap_or_default(),
            item["title"].as_str().unwrap_or_default()
        );
        println!(
            "  出演: {} / 番組ジャンル: {} / 出演者分類: {}",
            item["performer"].as_str().unwrap_or_default(),
            labels(item["program_genres"].as_array()),
            labels(item["personality_genres"].as_array())
        );
    }
    println!(
        "一致{}件 / 取得{}件 / 不正・日付不一致: {}件 / 重複: {}件 / 全件: 再生可否未確認",
        value["matched_program_count"],
        list.programs.len(),
        list.invalid_count,
        list.duplicate_count
    );
    Ok(())
}

fn labels(items: Option<&Vec<Value>>) -> String {
    let names: Vec<_> = items
        .into_iter()
        .flatten()
        .map(|v| {
            v["name"]
                .as_str()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| v["id"].as_str().unwrap_or_default())
        })
        .collect();
    if names.is_empty() {
        "未分類".into()
    } else {
        names.join("、")
    }
}

fn classes(items: &[Classification]) -> Vec<Value> {
    items
        .iter()
        .map(|c| json!({"id":c.id,"name":c.name}))
        .collect()
}

fn snapshot(
    list: &catalog::ProgramList,
    now: DateTime<FixedOffset>,
    filter: &Filter,
) -> Result<Value, &'static str> {
    let programs = list.programs.iter().filter(|program| filter.matches(program)).map(|p| {
        let (status, label) = match broadcast_status(p.key.starts_at, p.ends_at, now)? {
            BroadcastStatus::Upcoming => ("upcoming", "放送前"),
            BroadcastStatus::OnAir => ("on_air", "放送中"),
            BroadcastStatus::Finished => ("finished", "放送終了"),
        };
        Ok(json!({"station_id":p.key.station_id,"program_id":p.program_id,"starts_at":p.key.starts_at.to_rfc3339(),"ends_at":p.ends_at.to_rfc3339(),"title":p.title,"performer":p.performer,"description":p.description,"info":p.info,"tags":p.tags,"program_genres":classes(&p.program_genres),"personality_genres":classes(&p.personality_genres),"status":status,"status_label":label,"listening_availability":"unknown"}))
    }).collect::<Result<Vec<_>, &'static str>>()?;
    let matched = programs.len();
    let observed = observed_snapshot(&ObservedClassifications::collect(&list.programs));
    Ok(
        json!({"complete":list.invalid_count == 0,"filters":filter,"observed_classifications":observed,"source_program_count":list.programs.len(),"matched_program_count":matched,"station_id":list.station_id,"station_name":list.station_name,"broadcast_day":list.broadcast_day.to_string(),"timezone":"Asia/Tokyo","as_of":now.to_rfc3339(),"invalid_count":list.invalid_count,"duplicate_count":list.duplicate_count,"programs":programs}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn named_recall_preserves_period_and_startup_restore_resets_it_before_acquisition() {
        let root = tempfile::tempdir().unwrap();
        let store = SearchStore::new(root.path().join("data"));
        let now = stream::parse_time("20261016050000").unwrap();
        let saved = criteria(
            &["TBS", "20261014", "20261015", "--keyword", "生活"].map(str::to_string),
            now,
        )
        .unwrap();
        store.save_named("生活", saved.clone()).unwrap();
        store.save_last(saved.clone()).unwrap();
        let (named, _) = restored_request(
            &["--saved", "生活", "--json"].map(str::to_string),
            now,
            Some(&store),
        )
        .unwrap();
        assert_eq!(named.range.days().len(), 2);
        assert!(named.json);
        assert_eq!(named.station.as_deref(), Some("TBS"));
        let (last, _) = restored_request(&["--restore".into()], now, Some(&store)).unwrap();
        assert_eq!(
            last.range.days(),
            &[catalog::parse_day("20261016").unwrap()]
        );
        assert_eq!(&last.filter, saved.filter());
        for args in [
            vec!["--saved"],
            vec!["--saved", "生活", "--keyword", "変更"],
            vec!["--restore", "TBS"],
        ] {
            assert!(
                restored_request(
                    &args.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                    now,
                    Some(&store)
                )
                .is_err()
            );
        }
        std::fs::write(store.path(), b"broken").unwrap();
        let (fallback, write) = restored_request(&["--restore".into()], now, Some(&store)).unwrap();
        assert!(!write);
        assert_eq!(fallback.areas.len(), 47);
        assert_eq!(std::fs::read(store.path()).unwrap(), b"broken");
    }
    #[test]
    fn duplicate_region_arguments_never_turn_into_nationwide_saved_conditions() {
        let now = stream::parse_time("20261016050000").unwrap();
        let criteria = criteria(&["--areas".into(), vec!["JP13"; 47].join(",")], now).unwrap();
        assert_eq!(criteria.scope(), &Scope::Areas(vec!["JP13".into()]));
    }
    #[test]
    fn date_range_arguments_validate_before_acquisition() {
        let now = stream::parse_time("20261016045900").unwrap();
        let args = |items: &[&str]| items.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let default = request(&args(&["--all"]), now).unwrap();
        assert_eq!(
            default.range.start(),
            catalog::parse_day("20261015").unwrap()
        );
        assert_eq!(default.range.days().len(), 1);
        let range = request(
            &args(&["TBS", "20261014", "20261015", "--keyword", "生活"]),
            now,
        )
        .unwrap();
        assert_eq!(range.range.days().len(), 2);
        for values in [
            vec!["TBS", "20261015", "20261014"],
            vec!["--all", "20260915", "20261015"],
            vec!["TBS", "20261015", "20260230"],
            vec!["TBS", "20261014", "20261015", "20261016"],
        ] {
            assert!(request(&args(&values), now).is_err());
        }
    }
    #[test]
    fn range_json_separates_schedule_counts_from_station_counts_and_dates_failures() {
        use radiko_client::catalog::{Failure, Report};
        let day = catalog::parse_day("20261015").unwrap();
        let next = catalog::parse_day("20261016").unwrap();
        let mut list =
            catalog::parse_programs(include_bytes!("../tests/fixtures/schedule.xml"), "TBS", day)
                .unwrap();
        list.invalid_count = 0;
        let report = Report {
            areas: vec!["JP13".into()],
            stations: vec![radiko_client::catalog::SelectedStation {
                station: radiko_client::domain::Station {
                    id: "TBS".into(),
                    name: "テスト".into(),
                },
                areas: vec!["JP13".into()],
            }],
            lists: vec![list],
            area_failures: vec![],
            schedule_failures: vec![Failure {
                target: "TBS".into(),
                day: Some(next),
                error: catalog::CatalogError::Network,
            }],
        };
        let json = report_snapshot(
            &report,
            &BroadcastRange::new(day, next).unwrap(),
            stream::parse_time("20261016120000").unwrap(),
            &Filter::default(),
        )
        .unwrap();
        assert!(json["broadcast_day"].is_null());
        assert_eq!(json["date_range"]["days"], 2);
        assert_eq!(json["requested_schedules"], 2);
        assert_eq!(json["successful_stations"], 1);
        assert_eq!(json["successful_schedules"], 1);
        assert_eq!(json["schedule_failures"][0]["broadcast_day"], "2026-10-16");
        assert_eq!(json["complete"], false);
    }
    #[test]
    fn search_arguments_validate_before_acquisition() {
        let now = stream::parse_time("20261015120000").unwrap();
        for args in [
            vec!["TBS", "--keyword"],
            vec!["TBS", "--keyword", " "],
            vec!["TBS", "--keyword", "--json"],
            vec!["TBS", "--genre", ""],
            vec!["TBS", "--genre", "A,"],
            vec!["TBS", "--genre", "--json"],
            vec!["TBS", "--genre", "A", "--genre", "B"],
        ] {
            assert!(
                request(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>(), now).is_err(),
                "{args:?}"
            );
        }
        let request = request(
            &[
                "TBS",
                "--genre",
                "UNKNOWN,OTHER",
                "--personality",
                "C999",
                "--keyword",
                "出演者",
                "--classifications",
            ]
            .map(str::to_string),
            now,
        )
        .unwrap();
        assert!(request.classifications);
        let list = catalog::parse_programs(
            include_bytes!("../tests/fixtures/schedule.xml"),
            "TBS",
            request.day,
        )
        .unwrap();
        assert!(request.filter.matches(&list.programs[1]));
    }
    #[test]
    fn zero_matches_preserve_source_counts_observed_classes_and_partial_failure() {
        let now = stream::parse_time("20261015060000").unwrap();
        let day = catalog::parse_day("20261015").unwrap();
        let list =
            catalog::parse_programs(include_bytes!("../tests/fixtures/schedule.xml"), "TBS", day)
                .unwrap();
        let filter = Filter::new(vec!["absent".into()], vec![], false, false, None).unwrap();
        let value = snapshot(&list, now, &filter).unwrap();
        assert_eq!(value["matched_program_count"], 0);
        assert_eq!(value["source_program_count"], 3);
        assert_eq!(
            value["observed_classifications"]["program_genres"][0]["id"],
            "UNKNOWN"
        );
        assert_eq!(value["complete"], false);
        assert_eq!(value["invalid_count"], 1);
        let report = radiko_client::catalog::Report {
            areas: vec!["JP13".into()],
            stations: vec![],
            lists: vec![list],
            area_failures: vec![],
            schedule_failures: vec![],
        };
        let all = report_snapshot(&report, &BroadcastRange::single(day), now, &filter).unwrap();
        assert_eq!(all["complete"], false);
        assert_eq!(all["matched_program_count"], 0);
        assert_eq!(all["source_program_count"], 3);
    }
    #[test]
    fn region_arguments_keep_five_am_default_and_reject_ambiguous_selections() {
        let args = |items: &[&str]| items.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let before = stream::parse_time("20261015045900").unwrap();
        let after = stream::parse_time("20261015050000").unwrap();
        assert_eq!(
            request(&args(&["--areas", "JP13,JP14"]), before)
                .unwrap()
                .day,
            catalog::parse_day("20261014").unwrap()
        );
        let all = request(&args(&["--all", "--json"]), after).unwrap();
        assert_eq!(all.day, catalog::parse_day("20261015").unwrap());
        assert_eq!(all.areas.len(), 47);
        assert!(all.json);
        assert_eq!(
            request(&args(&["--areas", "JP44", "20261001"]), after)
                .unwrap()
                .day,
            catalog::parse_day("20261001").unwrap()
        );
        for values in [
            vec!["--all", "--areas", "JP13"],
            vec!["TBS", "--all"],
            vec!["--areas"],
            vec!["--areas", "JP13,"],
            vec!["--areas", "JP48"],
            vec!["--all", "20260230"],
        ] {
            assert!(request(&args(&values), after).is_err());
        }
    }

    #[test]
    fn partial_json_names_missing_regions_and_stations_without_inventing_results() {
        use radiko_client::catalog::{Failure, Report};
        let now = stream::parse_time("20261015060000").unwrap();
        let day = catalog::parse_day("20261015").unwrap();
        let report = Report {
            areas: vec!["JP13".into(), "JP44".into()],
            stations: Vec::new(),
            lists: Vec::new(),
            area_failures: vec![Failure {
                day: None,
                target: "JP44".into(),
                error: catalog::CatalogError::Network,
            }],
            schedule_failures: vec![Failure {
                day: Some(day),
                target: "TBS".into(),
                error: catalog::CatalogError::Http(404),
            }],
        };
        let value = report_snapshot(
            &report,
            &BroadcastRange::single(day),
            now,
            &Filter::default(),
        )
        .unwrap();
        assert_eq!(value["complete"], false);
        assert_eq!(value["area_failures"][0]["target"], "JP44");
        assert_eq!(value["schedule_failures"][0]["target"], "TBS");
        assert_eq!(value["broadcast_day"], "2026-10-15");
        assert!(value["schedules"].as_array().unwrap().is_empty());
    }
    #[test]
    fn snapshot_keeps_future_programs_and_does_not_infer_permission() {
        let list = catalog::parse_programs(
            include_bytes!("../tests/fixtures/schedule.xml"),
            "TBS",
            catalog::parse_day("20261015").unwrap(),
        )
        .unwrap();
        let value = snapshot(
            &list,
            stream::parse_time("20261015060000").unwrap(),
            &Filter::default(),
        )
        .unwrap();
        assert_eq!(value["programs"][0]["status"], "finished");
        assert_eq!(value["programs"][1]["status"], "on_air");
        assert_eq!(value["programs"][2]["status"], "upcoming");
        assert!(
            value["programs"]
                .as_array()
                .unwrap()
                .iter()
                .all(|p| p["listening_availability"] == "unknown")
        );
    }
}
