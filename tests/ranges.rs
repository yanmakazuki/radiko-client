use chrono::NaiveDate;
use radiko_client::{
    catalog::{Source, acquire_range, acquire_stations},
    domain::{BroadcastRange, Station},
    radiko::catalog::{CatalogError, ProgramList, parse_day, parse_programs},
};
use std::sync::{
    Barrier, Mutex,
    atomic::{AtomicUsize, Ordering},
};

struct Mock {
    calls: Mutex<Vec<(String, NaiveDate)>>,
    areas: AtomicUsize,
    fail: Option<NaiveDate>,
    barrier: Option<Barrier>,
    active: AtomicUsize,
    peak: AtomicUsize,
}
impl Mock {
    fn new(fail: Option<NaiveDate>, barrier: Option<Barrier>) -> Self {
        Self {
            calls: Mutex::new(vec![]),
            areas: AtomicUsize::new(0),
            fail,
            barrier,
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
        }
    }
}
impl Source for Mock {
    fn stations(&self, area: &str) -> Result<Vec<Station>, CatalogError> {
        self.areas.fetch_add(1, Ordering::SeqCst);
        if area == "JP44" {
            return Err(CatalogError::Network);
        }
        Ok(["TBS", "OBS"]
            .map(|id| Station {
                id: id.into(),
                name: id.into(),
            })
            .into())
    }
    fn programs(&self, station: &str, day: NaiveDate) -> Result<ProgramList, CatalogError> {
        self.calls.lock().unwrap().push((station.into(), day));
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(active, Ordering::SeqCst);
        if let Some(barrier) = &self.barrier {
            barrier.wait();
        }
        let result = if station == "TBS" && Some(day) == self.fail {
            Err(CatalogError::Network)
        } else {
            let date = day.format("%Y%m%d");
            let boundary = if [
                parse_day("20261015").unwrap(),
                parse_day("20261016").unwrap(),
            ]
            .contains(&day)
            {
                "<prog id=\"boundary\" ft=\"20261016045900\" to=\"20261016060000\"><title>日付境界</title></prog>"
            } else {
                ""
            };
            let xml = format!(
                "<radiko><stations><station id=\"{station}\"><name>{station}</name><progs>{boundary}<prog id=\"same\" ft=\"{date}060000\" to=\"{date}070000\"><title>同名</title></prog><prog id=\"same\" ft=\"{date}070000\" to=\"{date}080000\"><title>同名</title></prog></progs></station></stations></radiko>"
            );
            parse_programs(xml.as_bytes(), station, day)
        };
        self.active.fetch_sub(1, Ordering::SeqCst);
        if let Some(barrier) = &self.barrier {
            barrier.wait();
        }
        result
    }
}
fn day(value: &str) -> NaiveDate {
    parse_day(value).unwrap()
}
#[test]
fn range_bounds_are_inclusive_and_reject_reversed_or_more_than_thirty_days() {
    let range = BroadcastRange::new(day("20260917"), day("20261016")).unwrap();
    assert_eq!(range.days().len(), 30);
    assert_eq!(range.days().first(), Some(&range.start()));
    assert_eq!(range.days().last(), Some(&range.end()));
    assert!(BroadcastRange::new(day("20260916"), day("20261016")).is_err());
    assert!(BroadcastRange::new(day("20261016"), day("20261015")).is_err());
    assert_eq!(
        BroadcastRange::new(day("20261016"), day("20261016"))
            .unwrap()
            .days()
            .len(),
        1
    );
    assert_eq!(
        BroadcastRange::single(NaiveDate::MAX).days(),
        &[NaiveDate::MAX]
    );
}
#[test]
fn range_fetches_discovery_once_and_deduplicates_overlapping_slots_but_not_split_or_other_stations()
{
    let source = Mock::new(None, None);
    let range = BroadcastRange::new(day("20261015"), day("20261016")).unwrap();
    let mut completed = 0;
    let report = acquire_range(
        &source,
        &["JP13".into(), "JP14".into(), "JP13".into()],
        &range,
        |progress| {
            if progress.phase == "番組表" {
                completed += 1;
                assert!(range.days().contains(&progress.day.unwrap()));
                assert_eq!(progress.total, 4);
            }
        },
    )
    .unwrap();
    assert_eq!(source.areas.load(Ordering::SeqCst), 2);
    assert_eq!(source.calls.lock().unwrap().len(), 4);
    assert_eq!(completed, 4);
    assert!(report.complete());
    let programs: Vec<_> = report
        .lists
        .iter()
        .flat_map(|list| &list.programs)
        .collect();
    assert_eq!(programs.len(), 10); // Four split slots and one shared boundary slot per station.
    assert_eq!(
        programs
            .iter()
            .filter(|p| p.program_id == "boundary")
            .count(),
        2
    );
    assert_eq!(
        programs.iter().filter(|p| p.program_id == "same").count(),
        8
    );
    assert_eq!(
        report
            .lists
            .iter()
            .map(|list| list.duplicate_count)
            .sum::<usize>(),
        2
    );
}
#[test]
fn failures_identify_station_and_day_and_repeated_requests_never_fill_gaps_from_old_success() {
    let range = BroadcastRange::new(day("20261015"), day("20261016")).unwrap();
    let first = Mock::new(None, None);
    assert!(
        acquire_range(&first, &["JP13".into()], &range, |_| {})
            .unwrap()
            .complete()
    );
    let failed = Mock::new(Some(day("20261016")), None);
    let report = acquire_range(&failed, &["JP13".into(), "JP44".into()], &range, |_| {}).unwrap();
    assert!(!report.complete());
    assert_eq!(report.lists.len(), 3);
    assert_eq!(report.schedule_failures[0].target, "TBS");
    assert_eq!(report.schedule_failures[0].day, Some(day("20261016")));
    assert_eq!(report.area_failures[0].day, None);
    assert!(
        !report
            .lists
            .iter()
            .any(|list| list.station_id == "TBS" && list.broadcast_day == day("20261016"))
    );
    acquire_range(&failed, &["JP13".into()], &range, |_| {}).unwrap();
    assert_eq!(failed.calls.lock().unwrap().len(), 8);
}
#[test]
fn station_date_jobs_share_one_four_request_limit_and_do_not_discover_regions() {
    let source = Mock::new(None, Some(Barrier::new(4)));
    let range = BroadcastRange::new(day("20261013"), day("20261016")).unwrap();
    let report = acquire_stations(
        &source,
        &["TBS".into(), "OBS".into(), "TBS".into()],
        &range,
        |_| {},
    )
    .unwrap();
    assert!(report.complete());
    assert_eq!(report.lists.len(), 8);
    assert_eq!(source.areas.load(Ordering::SeqCst), 0);
    assert_eq!(source.calls.lock().unwrap().len(), 8);
    assert_eq!(source.peak.load(Ordering::SeqCst), 4);
    assert!(acquire_stations(&source, &["../TBS".into()], &range, |_| {}).is_err());
    assert_eq!(source.calls.lock().unwrap().len(), 8);
}
