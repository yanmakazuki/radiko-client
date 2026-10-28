//! In-memory catalog for `tui --offline`. No HTTP client and no audio device.
use chrono::NaiveDate;
use radiko::{
    api::catalog::{self, CatalogError, ProgramList},
    app::auth::Status,
    catalog::Source,
    domain::Station,
    storage::SessionStore,
};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
};

pub const NOTICE: &str = "オフライン確認: 試験用の番組表です。通信と音声出力はしません";

/// One synthetic station and one program per requested day. Nothing is stored or fetched.
#[derive(Clone, Copy)]
pub struct Fixture;

impl Source for Fixture {
    fn stations(&self, area: &str) -> Result<Vec<Station>, CatalogError> {
        catalog::validate_area(area)?;
        Ok(vec![Station {
            id: "TBS".into(),
            name: "オフライン試験局".into(),
        }])
    }

    fn programs(&self, station: &str, day: NaiveDate) -> Result<ProgramList, CatalogError> {
        radiko::api::stream::validate_station(station).map_err(|_| CatalogError::InvalidStation)?;
        let stamp = day.format("%Y%m%d");
        let xml = format!(
            "<radiko><stations><station id=\"{station}\"><name>オフライン試験局</name><progs>\
<prog id=\"offline\" ft=\"{stamp}060000\" to=\"{stamp}070000\"><title>オフライン番組</title><pfm>試験</pfm></prog>\
</progs></station></stations></radiko>"
        );
        catalog::parse_programs(xml.as_bytes(), station, day)
    }
}

/// Read the session file only. A saved token is not sent anywhere.
pub fn session_status(data_dir: Option<PathBuf>, cancel: &AtomicBool) -> Status {
    if cancel.load(Ordering::Acquire) {
        return Status::NotChecked;
    }
    let store = match data_dir {
        Some(path) => SessionStore::new(path),
        None => match SessionStore::default_location() {
            Ok(store) => store,
            Err(error) => return Status::StorageFailed(error),
        },
    };
    match store.load() {
        Ok(None) => Status::SignedOut,
        Ok(Some(_)) => Status::NotChecked,
        Err(error) => Status::StorageFailed(error),
    }
}

pub fn split_args(args: &[String]) -> (bool, Vec<String>) {
    let offline = args.iter().any(|arg| arg == "--offline");
    let rest = args
        .iter()
        .filter(|arg| arg.as_str() != "--offline")
        .cloned()
        .collect();
    (offline, rest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use radiko::session::MemberSession;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn directory() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        dir
    }

    #[test]
    fn fixture_returns_one_program_and_one_station_without_network() {
        let day = NaiveDate::from_ymd_opt(2026, 10, 15).unwrap();
        let list = Fixture.programs("TBS", day).unwrap();
        assert_eq!(list.programs.len(), 1);
        assert_eq!(list.programs[0].title, "オフライン番組");
        assert_eq!(list.station_id, "TBS");
        assert_eq!(list.invalid_count, 0);
        let stations = Fixture.stations("JP13").unwrap();
        assert_eq!(stations.len(), 1);
        assert_eq!(stations[0].id, "TBS");
        assert!(matches!(
            Fixture.stations("JP48"),
            Err(CatalogError::InvalidArea)
        ));
        assert!(matches!(
            Fixture.programs("tbs", day),
            Err(CatalogError::InvalidStation)
        ));
    }

    #[test]
    fn session_file_is_not_checked_online() {
        let dir = directory();
        let path = dir.path().to_owned();
        assert!(matches!(
            session_status(Some(path.clone()), &AtomicBool::new(false)),
            Status::SignedOut
        ));
        SessionStore::new(path.clone())
            .save(MemberSession::new("SYNTHETIC_SESSION_ONLY".into()).unwrap())
            .unwrap();
        assert!(matches!(
            session_status(Some(path), &AtomicBool::new(false)),
            Status::NotChecked
        ));
        assert!(matches!(
            session_status(Some(dir.path().to_owned()), &AtomicBool::new(true)),
            Status::NotChecked
        ));
    }

    #[test]
    fn offline_flag_is_removed_before_search_arguments() {
        let (offline, rest) = split_args(&["--offline".into(), "TBS".into(), "--offline".into()]);
        assert!(offline);
        assert_eq!(rest, vec!["TBS".to_string()]);
        let (offline, rest) = split_args(&["TBS".into()]);
        assert!(!offline);
        assert_eq!(rest, vec!["TBS".to_string()]);
    }
}
