use chrono::DateTime;
use radiko::{
    api::catalog::parse_day,
    domain::{
        BroadcastRange,
        search::{Filter, Scope, SearchCriteria},
    },
    storage::searches::{Error, SearchStore},
};
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
fn criteria() -> SearchCriteria {
    SearchCriteria::new(
        Scope::Areas(vec!["JP14".into(), "JP13".into(), "JP13".into()]),
        BroadcastRange::new(
            parse_day("20261014").unwrap(),
            parse_day("20261015").unwrap(),
        )
        .unwrap(),
        Filter::new(
            vec!["P005".into()],
            vec!["C_NEW".into()],
            true,
            false,
            Some("Example".into()),
        )
        .unwrap(),
    )
    .unwrap()
}
#[test]
fn missing_file_is_read_only_and_roundtrip_keeps_last_and_named_separate() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("data");
    let store = SearchStore::new(directory.clone());
    assert!(store.load().unwrap().last.is_none());
    assert!(!directory.exists());
    store.save_named("条件", criteria()).unwrap();
    assert!(store.load().unwrap().last.is_none());
    store.save_last(criteria()).unwrap();
    let library = store.load().unwrap();
    assert_eq!(library.last.as_ref(), Some(&criteria()));
    assert_eq!(library.named["条件"], criteria());
    let mut programs = radiko::api::catalog::parse_programs(
        include_bytes!("../../../tests/fixtures/schedule.xml"),
        "TBS",
        parse_day("20261015").unwrap(),
    )
    .unwrap()
    .programs;
    let program = &mut programs[0];
    program.title = "eXaMpLe".into();
    program.program_genres = vec![];
    program.personality_genres = vec![radiko::domain::Classification {
        id: "C_NEW".into(),
        name: "new".into(),
    }];
    assert!(library.named["条件"].filter().matches(program));
    assert_eq!(
        library.named["条件"].scope(),
        &Scope::Areas(vec!["JP13".into(), "JP14".into()])
    );
    let raw: serde_json::Value = serde_json::from_slice(&fs::read(store.path()).unwrap()).unwrap();
    assert_eq!(raw["version"], 1);
    assert_eq!(raw.as_object().unwrap().len(), 3);
    assert!(raw["last"]["filter"].get("keyword_lowercase").is_none());
    assert!(!directory.join("session.json").exists());
    store.delete_named("条件").unwrap();
    assert!(store.load().unwrap().named.is_empty());
    assert!(store.load().unwrap().last.is_some());
}
#[test]
fn startup_uses_five_am_broadcast_day_and_named_recall_keeps_saved_dates() {
    let saved = criteria();
    for (now, day) in [
        ("2026-10-16T04:59:59+09:00", "20261015"),
        ("2026-10-16T05:00:00+09:00", "20261016"),
    ] {
        let restored = saved
            .restore_startup(DateTime::parse_from_rfc3339(now).unwrap())
            .unwrap();
        assert_eq!(restored.range().days(), &[parse_day(day).unwrap()]);
        assert_eq!(restored.filter(), saved.filter());
        assert_eq!(restored.scope(), saved.scope());
    }
    assert_eq!(saved.range().days().len(), 2);
    let initial =
        SearchCriteria::initial(DateTime::parse_from_rfc3339("2026-10-16T05:00:00+09:00").unwrap())
            .unwrap();
    assert_eq!(initial.scope(), &Scope::Nationwide);
    assert_eq!(initial.filter(), &Filter::default());
}
#[test]
fn corrupt_unsupported_or_invalid_criteria_are_never_overwritten_by_any_mutation() {
    let root = tempfile::tempdir().unwrap();
    let store = SearchStore::new(root.path().join("data"));
    store.save_last(criteria()).unwrap();
    let valid: serde_json::Value =
        serde_json::from_slice(&fs::read(store.path()).unwrap()).unwrap();
    let mut version = valid.clone();
    version["version"] = 2.into();
    let mut dates = valid.clone();
    dates["last"]["end"] = "20260101".into();
    let mut scope = valid.clone();
    scope["last"]["scope"] = serde_json::json!({"kind":"station","value":"../TBS"});
    let mut filter = valid.clone();
    filter["last"]["filter"]["keyword"] = "".into();
    let mut unknown = valid.clone();
    unknown["last"]["filter"]["keyword_lowercase"] = "injected".into();
    for bytes in [
        b"broken".to_vec(),
        serde_json::to_vec(&version).unwrap(),
        serde_json::to_vec(&dates).unwrap(),
        serde_json::to_vec(&scope).unwrap(),
        serde_json::to_vec(&filter).unwrap(),
        serde_json::to_vec(&unknown).unwrap(),
        vec![b' '; 128 * 1024 + 1],
    ] {
        fs::write(store.path(), &bytes).unwrap();
        assert!(store.load().is_err());
        assert!(store.save_last(criteria()).is_err());
        assert!(store.save_named("new", criteria()).is_err());
        assert!(store.delete_named("old").is_err());
        assert_eq!(fs::read(store.path()).unwrap(), bytes);
    }
}
#[test]
fn names_are_bounded_and_updates_preserve_other_entries_and_session_file() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("data");
    let store = SearchStore::new(directory.clone());
    for name in ["", " leading", "trailing ", "a\nb"] {
        assert!(store.save_named(name, criteria()).is_err());
    }
    assert!(!directory.exists());
    store.save_named("one", criteria()).unwrap();
    fs::write(
        directory.join("session.json"),
        b"untouched synthetic session",
    )
    .unwrap();
    for n in 1..64 {
        store.save_named(&format!("name-{n}"), criteria()).unwrap();
    }
    let before = fs::read(store.path()).unwrap();
    assert!(matches!(
        store.save_named("overflow", criteria()),
        Err(Error::TooMany)
    ));
    assert_eq!(fs::read(store.path()).unwrap(), before);
    store
        .save_named(
            "one",
            SearchCriteria::initial(
                DateTime::parse_from_rfc3339("2026-10-16T05:00:00+09:00").unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
    assert_eq!(store.load().unwrap().named.len(), 64);
    assert_eq!(
        fs::read(directory.join("session.json")).unwrap(),
        b"untouched synthetic session"
    );
    assert!(matches!(
        store.delete_named("missing"),
        Err(Error::NotFound)
    ));
}
#[cfg(unix)]
#[test]
fn permissions_and_symlinks_are_checked_without_modifying_targets() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("data");
    let store = SearchStore::new(directory.clone());
    store.save_named("one", criteria()).unwrap();
    assert_eq!(
        fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(store.path()).unwrap().permissions().mode() & 0o777,
        0o600
    );
    fs::set_permissions(store.path(), fs::Permissions::from_mode(0o644)).unwrap();
    assert!(store.load().is_err());
    assert!(store.save_last(criteria()).is_err());
    fs::set_permissions(store.path(), fs::Permissions::from_mode(0o600)).unwrap();
    let target = root.path().join("original");
    fs::rename(store.path(), &target).unwrap();
    symlink(&target, store.path()).unwrap();
    let before = fs::read(&target).unwrap();
    assert!(store.load().is_err());
    assert!(store.save_named("two", criteria()).is_err());
    assert_eq!(fs::read(&target).unwrap(), before);
    let alias = root.path().join("alias");
    symlink(&directory, &alias).unwrap();
    assert!(SearchStore::new(alias).load().is_err());
}

#[cfg(unix)]
#[test]
fn concurrent_writer_is_rejected_instead_of_overwriting_another_library_update() {
    use std::os::fd::AsRawFd;
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("data");
    let store = SearchStore::new(directory.clone());
    store.save_named("one", criteria()).unwrap();
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(directory.join("searches.lock"))
        .unwrap();
    // SAFETY: the owned file keeps this descriptor valid.
    assert_eq!(
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    let before = fs::read(store.path()).unwrap();
    assert!(store.save_named("two", criteria()).is_err());
    assert_eq!(fs::read(store.path()).unwrap(), before);
    drop(file);
    store.save_named("two", criteria()).unwrap();
    assert_eq!(store.load().unwrap().named.len(), 2);
}
