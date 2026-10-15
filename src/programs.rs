use chrono::{DateTime, FixedOffset, Utc};
use radiko_client::{
    domain::{BroadcastStatus, Classification, broadcast_day, broadcast_status},
    radiko::{catalog, stream},
};
use serde_json::{Value, json};

const USAGE: &str = "programs STATION [YYYYMMDD] [--json] / programs (--areas JP13,JP14 | --all) [YYYYMMDD] [--json]";
struct Request {
    station: Option<String>,
    areas: Vec<String>,
    day: chrono::NaiveDate,
    json: bool,
}

fn request(
    args: &[String],
    now: DateTime<FixedOffset>,
) -> Result<Request, Box<dyn std::error::Error>> {
    let mut positional = Vec::new();
    let mut json_output = false;
    let mut areas = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--json" if !json_output => json_output = true,
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
    if positional.len() > 1 {
        return Err(USAGE.into());
    }
    let day = match positional.first() {
        Some(day) => catalog::parse_day(day)?,
        None => broadcast_day(now).ok_or("放送日を計算できません")?,
    };
    Ok(Request {
        station,
        areas: areas.unwrap_or_default(),
        day,
        json: json_output,
    })
}

pub fn run(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    if args == ["--help"] {
        println!("{USAGE}");
        return Ok(());
    }
    let now = Utc::now().fixed_offset();
    let request = request(args, now)?;
    if let Some(station) = request.station {
        let list = catalog::list_programs(&station, request.day)?;
        if request.json {
            println!("{}", serde_json::to_string_pretty(&snapshot(&list, now)?)?);
        } else {
            print_list(&list, now)?;
        }
        return Ok(());
    }
    let client = catalog::CatalogClient::new()?;
    let mut display_error = None;
    let report =
        radiko_client::catalog::acquire(&client, &request.areas, request.day, |progress| {
            eprintln!(
                "{} {}/{} / 失敗: {} / {}",
                progress.phase,
                progress.completed,
                progress.total,
                progress.failed,
                progress.target
            );
            if !request.json {
                if let Some(list) = progress.list {
                    if let Err(error) = print_list(list, now) {
                        display_error = Some(error);
                    }
                }
            }
        })?;
    if let Some(error) = display_error {
        return Err(error);
    }
    if request.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report_snapshot(&report, request.day, now)?)?
        );
    } else {
        println!(
            "{} / 放送日 {} / 局一覧成功: {}/{}地域 / 番組表成功: {}/{}局",
            if report.complete() {
                "取得完了"
            } else {
                "部分取得・未取得あり"
            },
            request.day,
            report.areas.len() - report.area_failures.len(),
            report.areas.len(),
            report.lists.len(),
            report.stations.len()
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
                failure.target, request.day, failure.error
            );
        }
    }
    if !report.complete() {
        return Err("取得結果に欠落があります。同じコマンドの再実行で全件を再取得できます".into());
    }
    Ok(())
}

fn report_snapshot(
    report: &radiko_client::catalog::Report,
    day: chrono::NaiveDate,
    now: DateTime<FixedOffset>,
) -> Result<Value, &'static str> {
    let lists = report
        .lists
        .iter()
        .map(|l| snapshot(l, now))
        .collect::<Result<Vec<_>, _>>()?;
    let stations: Vec<_> = report
        .stations
        .iter()
        .map(|s| json!({"id":s.station.id,"name":s.station.name,"areas":s.areas}))
        .collect();
    let failures = |items: &[radiko_client::catalog::Failure]| -> Vec<Value> {
        items
            .iter()
            .map(|f| json!({"target":f.target,"reason":f.error.to_string()}))
            .collect()
    };
    Ok(
        json!({"broadcast_day":day.to_string(),"timezone":"Asia/Tokyo","as_of":now.to_rfc3339(),"complete":report.complete(),"areas":report.areas,"stations":stations,"successful_stations":report.lists.len(),"area_failures":failures(&report.area_failures),"schedule_failures":failures(&report.schedule_failures),"schedules":lists}),
    )
}

fn print_list(
    list: &catalog::ProgramList,
    now: DateTime<FixedOffset>,
) -> Result<(), Box<dyn std::error::Error>> {
    let value = snapshot(list, now)?;
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
        "{}件 / 不正・日付不一致: {}件 / 重複: {}件 / 全件: 再生可否未確認",
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
) -> Result<Value, &'static str> {
    let programs = list.programs.iter().map(|p| {
        let (status, label) = match broadcast_status(p.key.starts_at, p.ends_at, now)? {
            BroadcastStatus::Upcoming => ("upcoming", "放送前"),
            BroadcastStatus::OnAir => ("on_air", "放送中"),
            BroadcastStatus::Finished => ("finished", "放送終了"),
        };
        Ok(json!({"station_id":p.key.station_id,"program_id":p.program_id,"starts_at":p.key.starts_at.to_rfc3339(),"ends_at":p.ends_at.to_rfc3339(),"title":p.title,"performer":p.performer,"description":p.description,"info":p.info,"tags":p.tags,"program_genres":classes(&p.program_genres),"personality_genres":classes(&p.personality_genres),"status":status,"status_label":label,"listening_availability":"unknown"}))
    }).collect::<Result<Vec<_>, &'static str>>()?;
    Ok(
        json!({"station_id":list.station_id,"station_name":list.station_name,"broadcast_day":list.broadcast_day.to_string(),"timezone":"Asia/Tokyo","as_of":now.to_rfc3339(),"invalid_count":list.invalid_count,"duplicate_count":list.duplicate_count,"programs":programs}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
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
                target: "JP44".into(),
                error: catalog::CatalogError::Network,
            }],
            schedule_failures: vec![Failure {
                target: "TBS".into(),
                error: catalog::CatalogError::Http(404),
            }],
        };
        let value = report_snapshot(&report, day, now).unwrap();
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
        let value = snapshot(&list, stream::parse_time("20261015060000").unwrap()).unwrap();
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
