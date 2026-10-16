//! Region selection and bounded acquisition. Only the current request is retained.
pub mod search;
use crate::{
    domain::{BroadcastRange, Station},
    radiko::catalog::{CatalogClient, CatalogError, ProgramList, validate_area},
};
use chrono::NaiveDate;
use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
};

pub trait Source: Sync {
    fn stations(&self, area: &str) -> Result<Vec<Station>, CatalogError>;
    fn programs(&self, station: &str, day: NaiveDate) -> Result<ProgramList, CatalogError>;
}
impl Source for CatalogClient {
    fn stations(&self, area: &str) -> Result<Vec<Station>, CatalogError> {
        self.stations(area)
    }
    fn programs(&self, station: &str, day: NaiveDate) -> Result<ProgramList, CatalogError> {
        self.programs(station, day)
    }
}

#[derive(Debug)]
pub struct SelectedStation {
    pub station: Station,
    pub areas: Vec<String>,
}
#[derive(Debug)]
pub struct Failure {
    pub day: Option<NaiveDate>,
    pub target: String,
    pub error: CatalogError,
}
#[derive(Debug)]
pub struct Report {
    pub areas: Vec<String>,
    pub stations: Vec<SelectedStation>,
    pub lists: Vec<ProgramList>,
    pub area_failures: Vec<Failure>,
    pub schedule_failures: Vec<Failure>,
}
impl Report {
    pub fn complete(&self) -> bool {
        self.area_failures.is_empty()
            && self.schedule_failures.is_empty()
            && self.lists.iter().all(|l| l.invalid_count == 0)
    }
}
pub struct Progress<'a> {
    pub day: Option<NaiveDate>,
    pub phase: &'static str,
    pub target: &'a str,
    pub completed: usize,
    pub total: usize,
    pub failed: usize,
    pub list: Option<&'a ProgramList>,
}

/// Region discovery finishes before schedules start, so shared stations are fetched once.
/// The callback runs on the caller's thread; at most four network calls are active.
pub fn acquire(
    source: &impl Source,
    areas: &[String],
    day: NaiveDate,
    progress: impl FnMut(Progress<'_>),
) -> Result<Report, CatalogError> {
    acquire_range(source, areas, &BroadcastRange::single(day), progress)
}

pub fn acquire_range(
    source: &impl Source,
    areas: &[String],
    range: &BroadcastRange,
    mut progress: impl FnMut(Progress<'_>),
) -> Result<Report, CatalogError> {
    for area in areas {
        validate_area(area)?;
    }
    if areas.is_empty() {
        return Err(CatalogError::InvalidArea);
    }
    let areas: Vec<_> = areas
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut report = Report {
        areas: areas.clone(),
        stations: Vec::new(),
        lists: Vec::new(),
        area_failures: Vec::new(),
        schedule_failures: Vec::new(),
    };
    let mut selected = BTreeMap::<String, SelectedStation>::new();
    parallel(
        &areas,
        |area| source.stations(area),
        |area, result, completed| {
            match result {
                Ok(stations) => {
                    for station in stations {
                        let entry =
                            selected
                                .entry(station.id.clone())
                                .or_insert_with(|| SelectedStation {
                                    station,
                                    areas: Vec::new(),
                                });
                        if !entry.areas.contains(area) {
                            entry.areas.push(area.clone());
                        }
                    }
                }
                Err(error) => report.area_failures.push(Failure {
                    day: None,
                    target: area.clone(),
                    error,
                }),
            }
            progress(Progress {
                day: None,
                phase: "局一覧",
                target: area,
                completed,
                total: areas.len(),
                failed: report.area_failures.len(),
                list: None,
            });
        },
    );
    report.stations = selected.into_values().collect();
    for station in &mut report.stations {
        station.areas.sort();
    }
    acquire_schedules(source, range, &mut report, &mut progress);
    Ok(report)
}

/// Explicit station selections skip regional discovery, sharing the same global worker bound.
pub fn acquire_stations(
    source: &impl Source,
    stations: &[String],
    range: &BroadcastRange,
    mut progress: impl FnMut(Progress<'_>),
) -> Result<Report, CatalogError> {
    if stations.is_empty() {
        return Err(CatalogError::InvalidStation);
    }
    for station in stations {
        crate::radiko::stream::validate_station(station)
            .map_err(|_| CatalogError::InvalidStation)?;
    }
    let stations = stations
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|id| SelectedStation {
            station: Station {
                name: id.clone(),
                id,
            },
            areas: vec![],
        })
        .collect();
    let mut report = Report {
        areas: vec![],
        stations,
        lists: vec![],
        area_failures: vec![],
        schedule_failures: vec![],
    };
    acquire_schedules(source, range, &mut report, &mut progress);
    for station in &mut report.stations {
        if let Some(list) = report
            .lists
            .iter()
            .find(|list| list.station_id == station.station.id)
        {
            station.station.name = list.station_name.clone();
        }
    }
    Ok(report)
}

fn acquire_schedules(
    source: &impl Source,
    range: &BroadcastRange,
    report: &mut Report,
    progress: &mut impl FnMut(Progress<'_>),
) {
    let jobs: Vec<_> = report
        .stations
        .iter()
        .flat_map(|station| {
            range
                .days()
                .iter()
                .map(move |day| (station.station.id.clone(), *day))
        })
        .collect();
    let mut keys = HashSet::new();
    parallel(
        &jobs,
        |(station, day)| source.programs(station, *day),
        |(station, day), result, completed| {
            let list = match result {
                Ok(mut list) => {
                    list.programs.retain(|program| {
                        if keys.insert(program.key.clone()) {
                            true
                        } else {
                            list.duplicate_count += 1;
                            false
                        }
                    });
                    Some(list)
                }
                Err(error) => {
                    report.schedule_failures.push(Failure {
                        target: station.clone(),
                        day: Some(*day),
                        error,
                    });
                    None
                }
            };
            progress(Progress {
                day: Some(*day),
                phase: "番組表",
                target: station,
                completed,
                total: jobs.len(),
                failed: report.schedule_failures.len(),
                list: list.as_ref(),
            });
            if let Some(list) = list {
                report.lists.push(list);
            }
        },
    );
    report
        .lists
        .sort_by(|a, b| (&a.station_id, a.broadcast_day).cmp(&(&b.station_id, b.broadcast_day)));
    report.area_failures.sort_by(|a, b| a.target.cmp(&b.target));
    report
        .schedule_failures
        .sort_by(|a, b| (&a.target, a.day).cmp(&(&b.target, b.day)));
}

fn parallel<J: Sync, T: Send>(
    jobs: &[J],
    work: impl Fn(&J) -> T + Sync,
    mut consume: impl FnMut(&J, T, usize),
) {
    let next = AtomicUsize::new(0);
    thread::scope(|scope| {
        let (tx, rx) = mpsc::sync_channel(4);
        for _ in 0..jobs.len().min(4) {
            let (tx, work, next) = (tx.clone(), &work, &next);
            scope.spawn(move || {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(job) = jobs.get(index) else {
                        break;
                    };
                    if tx.send((index, work(job))).is_err() {
                        break;
                    }
                }
            });
        }
        drop(tx);
        for (done, (index, value)) in rx.into_iter().enumerate() {
            consume(&jobs[index], value, done + 1);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;
    #[test]
    fn worker_pool_reaches_but_never_exceeds_four_in_flight_requests() {
        let jobs: Vec<_> = (0..12).map(|n| n.to_string()).collect();
        let barrier = Barrier::new(4);
        let active = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let mut results = Vec::new();
        parallel(
            &jobs,
            |job| {
                let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(current, Ordering::SeqCst);
                barrier.wait();
                // Finish this batch before allowing another batch to enter the barrier.
                active.fetch_sub(1, Ordering::SeqCst);
                barrier.wait();
                job.to_owned()
            },
            |job, result, _| {
                assert_eq!(job, &result);
                results.push(result);
            },
        );
        assert_eq!(peak.load(Ordering::SeqCst), 4);
        assert_eq!(active.load(Ordering::SeqCst), 0);
        results.sort();
        let mut expected = jobs;
        expected.sort();
        assert_eq!(results, expected);
    }
}
