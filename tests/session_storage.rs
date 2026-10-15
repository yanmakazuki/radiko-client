use radiko_client::{
    session::MemberSession,
    storage::{SessionStore, StorageError},
};
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::{PermissionsExt, symlink};

fn setup() -> (tempfile::TempDir, SessionStore) {
    let root = tempfile::tempdir().unwrap();
    let store = SessionStore::new(root.path().join("data"));
    (root, store)
}

#[test]
fn missing_load_is_read_only_and_logout_is_idempotent() {
    let (_root, store) = setup();
    assert!(store.load().unwrap().is_none());
    store.delete().unwrap();
    assert!(!store.path().parent().unwrap().exists());
}

#[test]
fn save_reload_replace_and_logout_use_selected_directory() {
    let (_root, store) = setup();
    store
        .save(MemberSession::new("synthetic-first".into()).unwrap())
        .unwrap();
    assert!(store.load().unwrap().is_some());
    let value: serde_json::Value =
        serde_json::from_slice(&fs::read(store.path()).unwrap()).unwrap();
    assert_eq!(
        value,
        serde_json::json!({"version":1,"session":{"token":"synthetic-first"}})
    );
    store
        .save(MemberSession::new("synthetic-second".into()).unwrap())
        .unwrap();
    assert!(
        !fs::read_to_string(store.path())
            .unwrap()
            .contains("synthetic-first")
    );
    assert_eq!(
        fs::read_dir(store.path().parent().unwrap())
            .unwrap()
            .count(),
        1
    );
    #[cfg(unix)]
    {
        assert_eq!(
            fs::metadata(store.path()).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(store.path().parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
    store.delete().unwrap();
    store.delete().unwrap();
    assert!(store.load().unwrap().is_none());
}

#[test]
fn corrupt_unsupported_and_oversized_files_are_preserved_until_explicit_save() {
    let (_root, store) = setup();
    store
        .save(MemberSession::new("synthetic".into()).unwrap())
        .unwrap();
    for (bytes, error) in [
        (b"not json".to_vec(), StorageError::Invalid),
        (
            br#"{"version":2}"#.to_vec(),
            StorageError::UnsupportedVersion,
        ),
        (
            br#"{"version":1,"session":{"token":"bad; cookie"}}"#.to_vec(),
            StorageError::Invalid,
        ),
        (
            br#"{"version":1,"session":{"token":"synthetic","password":"must-not-load"}}"#.to_vec(),
            StorageError::Invalid,
        ),
        (vec![b' '; 16385], StorageError::Invalid),
    ] {
        fs::write(store.path(), &bytes).unwrap();
        assert_eq!(store.load().unwrap_err(), error);
        assert_eq!(fs::read(store.path()).unwrap(), bytes);
    }
    store
        .save(MemberSession::new("synthetic-new".into()).unwrap())
        .unwrap();
    assert!(store.load().unwrap().is_some());
}

#[cfg(unix)]
#[test]
fn insecure_permissions_and_symlinks_are_rejected() {
    let (root, store) = setup();
    store
        .save(MemberSession::new("synthetic".into()).unwrap())
        .unwrap();
    fs::set_permissions(store.path(), fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(store.load().unwrap_err(), StorageError::UnsafePermissions);
    store.delete().unwrap();
    let target = root.path().join("unrelated");
    fs::write(&target, b"keep").unwrap();
    symlink(&target, store.path()).unwrap();
    assert_eq!(store.load().unwrap_err(), StorageError::UnsafePath);
    assert_eq!(
        store
            .save(MemberSession::new("synthetic".into()).unwrap())
            .unwrap_err(),
        StorageError::UnsafePath
    );
    assert_eq!(fs::read(target).unwrap(), b"keep");
    let linked = root.path().join("linked");
    symlink(store.path().parent().unwrap(), &linked).unwrap();
    assert_eq!(
        SessionStore::new(linked).load().unwrap_err(),
        StorageError::UnsafePath
    );
    fs::set_permissions(
        store.path().parent().unwrap(),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    assert_eq!(store.load().unwrap_err(), StorageError::UnsafePermissions);
}
