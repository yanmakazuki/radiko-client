use chrono::NaiveDate;
use radiko::{
    api::catalog::{CatalogError, ProgramList, parse_day, parse_programs, parse_stations},
    catalog::{Source, acquire},
    domain::Station,
};
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};

struct Mock {
    calls: Mutex<Vec<String>>,
    fail_tbs: AtomicBool,
}
impl Mock {
    fn new() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            fail_tbs: AtomicBool::new(false),
        }
    }
}
impl Source for Mock {
    fn stations(&self, area: &str) -> Result<Vec<Station>, CatalogError> {
        self.calls.lock().unwrap().push(area.into());
        let ids = match area {
            "JP13" => vec!["TBS", "RN1"],
            "JP14" => vec!["TBS", "RN1", "OBS", "BAD"],
            _ => return Err(CatalogError::Http(403)),
        };
        Ok(ids
            .into_iter()
            .map(|id| Station {
                id: id.into(),
                name: "同名番組を放送する局".into(),
            })
            .collect())
    }
    fn programs(&self, station: &str, day: NaiveDate) -> Result<ProgramList, CatalogError> {
        self.calls.lock().unwrap().push(format!("{station}:{day}"));
        if station == "BAD" || (station == "TBS" && self.fail_tbs.load(Ordering::Relaxed)) {
            return Err(CatalogError::Network);
        }
        let xml = include_str!("../../../tests/fixtures/schedule.xml").replace("TBS", station);
        parse_programs(xml.as_bytes(), station, day)
    }
}

#[test]
fn selects_union_of_available_stations_and_preserves_distinct_station_and_split_slots() {
    let source = Mock::new();
    let areas = ["JP13", "JP14", "JP13", "JP44"].map(String::from);
    let day = parse_day("20261015").unwrap();
    let mut batches = 0;
    let report = acquire(&source, &areas, day, |p| {
        assert!(p.completed <= p.total);
        if let Some(list) = p.list {
            batches += 1;
            assert_eq!(list.programs.len(), 3);
        }
    })
    .unwrap();
    assert_eq!(report.areas.len(), 3);
    assert_eq!(report.stations.len(), 4);
    assert_eq!(report.lists.len(), 3);
    assert_eq!(batches, 3);
    assert_eq!(report.area_failures[0].target, "JP44");
    assert_eq!(report.schedule_failures[0].target, "BAD");
    assert!(!report.complete());
    let tbs = report
        .stations
        .iter()
        .find(|s| s.station.id == "TBS")
        .unwrap();
    assert_eq!(tbs.areas, ["JP13", "JP14"]);
    let calls = source.calls.lock().unwrap();
    assert_eq!(calls.len(), 7);
    assert_eq!(calls.iter().filter(|s| *s == "TBS:2026-10-15").count(), 1);
    assert!(
        report
            .lists
            .iter()
            .all(|list| list.programs[1].title == "朝 & 続編"
                && list.programs[1].key != list.programs[2].key)
    );
}

#[test]
fn reacquires_regions_and_schedules_and_never_uses_previous_success_after_failure() {
    let source = Mock::new();
    let areas = ["JP13".into()];
    let day = parse_day("20261015").unwrap();
    assert_eq!(
        acquire(&source, &areas, day, |_| {}).unwrap().lists.len(),
        2
    );
    source.fail_tbs.store(true, Ordering::Relaxed);
    let report = acquire(&source, &areas, day, |_| {}).unwrap();
    assert_eq!(report.lists.len(), 1);
    assert_eq!(report.schedule_failures[0].target, "TBS");
    assert!(report.lists.iter().all(|list| list.station_id != "TBS"));
    assert_eq!(source.calls.lock().unwrap().len(), 6);
}

#[test]
fn invalid_selection_stops_before_requests_and_failed_regions_have_no_invented_stations() {
    let source = Mock::new();
    let day = parse_day("20261015").unwrap();
    for areas in [
        vec![],
        vec!["JP0".into()],
        vec!["JP01".into()],
        vec!["JP48".into()],
        vec!["JP13".into(), "../JP14".into()],
    ] {
        assert!(acquire(&source, &areas, day, |_| {}).is_err());
    }
    assert!(source.calls.lock().unwrap().is_empty());
    let report = acquire(&source, &["JP44".into()], day, |_| {}).unwrap();
    assert!(report.stations.is_empty());
    assert!(report.lists.is_empty());
    assert!(!report.complete());
    assert_eq!(source.calls.lock().unwrap().as_slice(), ["JP44"]);
}

#[test]
fn station_xml_checks_region_structure_and_ids_instead_of_assuming_empty_success() {
    let xml = include_bytes!("../../../tests/fixtures/stations.xml");
    let stations = parse_stations(xml, "JP13").unwrap();
    assert_eq!(stations.len(), 2);
    assert_eq!(stations[0].name, "テスト & 局");
    assert!(matches!(
        parse_stations(xml, "JP14"),
        Err(CatalogError::WrongArea)
    ));
    for xml in [
        "<stations area_id=\"JP13\"/>",
        "<stations area_id=\"JP13\"><station><id>../BAD</id><name>bad</name></station></stations>",
        "<radiko/>",
        "<stations>",
        "<!DOCTYPE stations><stations/>",
    ] {
        assert!(parse_stations(xml.as_bytes(), "JP13").is_err());
    }
}
