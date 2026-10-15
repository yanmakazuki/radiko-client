use std::{
    fs,
    process::{Command, Stdio},
};

fn run(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_radiko-client"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

#[test]
fn stream_probes_validate_arguments_and_require_a_session_before_network_requests() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("data");
    let path = directory.to_str().unwrap();
    for (args, expected) in [
        (vec!["probe-live", "TBS"], "未ログイン"),
        (vec!["probe-live", "../TBS"], "局ID"),
        (vec!["probe-timefree", "TBS", "20260230010000"], "日時"),
        (vec!["probe-timefree", "TBS", "20990101000000"], "放送前"),
        (
            vec!["probe-timefree", "TBS", "20261001010000"],
            "未ログイン",
        ),
    ] {
        let mut full = vec!["--data-dir", path];
        full.extend(args);
        let output = run(&full);
        assert!(!output.status.success());
        assert!(String::from_utf8(output.stderr).unwrap().contains(expected));
    }
    assert!(!directory.exists());
}

#[test]
fn session_without_file_and_logout_work_offline_without_creating_data_directory() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("data");
    let path = directory.to_str().unwrap();
    let output = run(&["--data-dir", path, "session"]);
    assert!(output.status.success());
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("未ログイン")
    );
    assert!(run(&["logout", "--data-dir", path]).status.success());
    assert!(!directory.exists());
}

#[test]
fn corrupt_session_is_preserved_and_logout_removes_it() {
    let root = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    }
    let path = root.path().join("session.json");
    fs::write(&path, b"corrupt-synthetic-session").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    let directory = root.path().to_str().unwrap();
    let output = run(&["session", "--data-dir", directory]);
    assert!(output.status.success());
    assert!(String::from_utf8(output.stderr).unwrap().contains("破損"));
    assert_eq!(fs::read(&path).unwrap(), b"corrupt-synthetic-session");
    assert!(run(&["logout", "--data-dir", directory]).status.success());
    assert!(!path.exists());
}

#[test]
fn login_requires_interactive_terminal_and_never_accepts_password_arguments() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("data");
    let path = directory.to_str().unwrap();
    let output = run(&["login", "--data-dir", path]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("対話端末")
    );
    let output = run(&[
        "login",
        "--password",
        "synthetic-password",
        "--data-dir",
        path,
    ]);
    assert!(!output.status.success());
    assert!(
        !String::from_utf8(output.stderr)
            .unwrap()
            .contains("synthetic-password")
    );
    assert!(!directory.exists());
}

#[test]
fn invalid_data_directory_arguments_fail_and_inspector_still_works() {
    for args in [
        vec!["session", "--data-dir"],
        vec!["session", "--data-dir", "a", "--data-dir", "b"],
    ] {
        assert!(!run(&args).status.success());
    }
    let output = run(&[
        "inspect-chapters",
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/chapters.json"),
        "90",
        "0",
    ]);
    assert!(output.status.success());
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("不正項目: 1 / 除外した楽曲: 1")
    );
}

#[test]
fn programs_rejects_invalid_arguments_without_creating_session_storage() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("absent");
    for args in [
        vec!["programs"],
        vec!["programs", "../TBS"],
        vec!["programs", "TBS", "20260230"],
        vec!["programs", "TBS", "--json", "--json"],
    ] {
        let mut full = vec!["--data-dir", data.to_str().unwrap()];
        full.extend(args);
        assert!(!run(&full).status.success());
    }
    assert!(!data.exists());
}

#[test]
fn region_commands_validate_before_network_and_document_both_selectors() {
    for args in [
        vec!["programs", "--areas"],
        vec!["programs", "--areas", "JP0"],
        vec!["programs", "--areas", "JP01"],
        vec!["programs", "--all", "--areas", "JP13"],
        vec!["programs", "--all", "20260230"],
    ] {
        assert!(!run(&args).status.success());
    }
    let output = run(&["programs", "--help"]);
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(help.contains("--areas") && help.contains("--all"));
}
