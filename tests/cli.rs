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

#[test]
fn output_commands_reject_bad_arguments_and_require_session_without_hardware_or_network() {
    for args in [
        vec!["probe-output", "0"],
        vec!["probe-output", "11"],
        vec!["probe-output", "bad"],
        vec!["probe-output", "1", "2"],
        vec!["play-sample"],
        vec!["play-sample", "../TBS"],
    ] {
        assert!(!run(&args).status.success());
    }
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("absent");
    for args in [
        vec!["play-sample", "TBS"],
        vec!["play-sample", "TBS", "20261014120000"],
    ] {
        let mut full = vec!["--data-dir", path.to_str().unwrap()];
        full.extend(args);
        let output = run(&full);
        assert!(!output.status.success());
        assert!(
            String::from_utf8(output.stderr)
                .unwrap()
                .contains("未ログイン")
        );
    }
    assert!(!path.exists());
}

#[test]
fn continuous_playback_validates_before_reading_session_or_starting_network() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("absent");
    for args in [
        vec!["play-live", "../TBS"],
        vec!["play-live", "TBS", "--seconds", "0"],
        vec!["play-timefree", "TBS", "20990101000000"],
        vec!["play-live", "TBS"],
        vec!["play-timefree", "TBS", "20261006120000"],
    ] {
        let mut full = vec!["--data-dir", data.to_str().unwrap()];
        full.extend(args);
        assert!(!run(&full).status.success());
    }
    assert!(!data.exists());
    assert!(run(&["play-live", "--help"]).status.success());
}

#[test]
fn chapters_help_and_invalid_arguments_are_offline_and_do_not_touch_session_storage() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("unused");
    let path = path.to_str().unwrap();
    let output = run(&["--data-dir", path, "chapters", "--help"]);
    assert!(output.status.success());
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("chapters STATION")
    );
    for args in [
        vec!["chapters"],
        vec!["chapters", "../TBS", "20261015120000"],
        vec!["chapters", "TBS", "20260230000000"],
    ] {
        assert!(!run(&args).status.success());
    }
    assert!(!std::path::Path::new(path).exists());
}

#[test]
fn program_filter_help_and_invalid_criteria_are_offline() {
    let output = run(&["programs", "--help"]);
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    for flag in ["--genre", "--personality", "--keyword", "--classifications"] {
        assert!(help.contains(flag));
    }
    for values in [
        vec!["programs", "TBS", "--keyword", ""],
        vec!["programs", "TBS", "--genre", "A,"],
        vec!["programs", "--all", "--personality"],
    ] {
        assert!(!run(&values).status.success());
    }
}

#[test]
fn invalid_date_ranges_are_rejected_offline() {
    for args in [
        vec!["programs", "TBS", "20261015", "20261014"],
        vec!["programs", "--all", "20260915", "20261015"],
        vec!["programs", "TBS", "20261015", "20260230"],
    ] {
        assert!(!run(&args).status.success());
    }
}

#[test]
fn named_searches_save_show_delete_and_restore_work_without_network_or_session() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("criteria");
    let path = directory.to_str().unwrap();
    assert!(
        run(&["--data-dir", path, "searches", "list"])
            .status
            .success()
    );
    assert!(!directory.exists());
    let save = run(&[
        "--data-dir",
        path,
        "searches",
        "save",
        "生活",
        "TBS",
        "20261014",
        "20261015",
        "--keyword",
        "生活",
    ]);
    assert!(
        save.status.success(),
        "{}",
        String::from_utf8_lossy(&save.stderr)
    );
    let shown = run(&["--data-dir", path, "searches", "show", "生活"]);
    assert!(shown.status.success());
    let json: serde_json::Value = serde_json::from_slice(&shown.stdout).unwrap();
    assert_eq!(json["start"], "20261014");
    assert_eq!(json["end"], "20261015");
    assert_eq!(json["scope"]["value"], "TBS");
    assert_eq!(json["filter"]["keyword"], "生活");
    assert!(
        String::from_utf8(run(&["--data-dir", path, "searches", "list"]).stdout)
            .unwrap()
            .contains("生活")
    );
    let initial = run(&["--data-dir", path, "searches", "restore"]);
    let json: serde_json::Value = serde_json::from_slice(&initial.stdout).unwrap();
    assert_eq!(json["scope"]["kind"], "nationwide");
    assert_eq!(json["start"], json["end"]);
    assert!(
        run(&["--data-dir", path, "searches", "delete", "生活"])
            .status
            .success()
    );
    assert!(
        !run(&["--data-dir", path, "searches", "show", "生活"])
            .status
            .success()
    );
    assert!(!directory.join("session.json").exists());
}
#[test]
fn saved_search_invalid_arguments_and_corrupt_restore_do_not_overwrite_storage() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("data");
    let path = directory.to_str().unwrap();
    for args in [
        vec!["searches", "save", "name", "../TBS"],
        vec!["searches", "save", "name", "TBS", "20260230"],
        vec!["programs", "--saved", "missing"],
        vec!["programs", "--saved"],
        vec!["programs", "--restore", "TBS"],
    ] {
        let mut all = vec!["--data-dir", path];
        all.extend(args);
        assert!(!run(&all).status.success());
    }
    assert!(!directory.exists());
    assert!(
        run(&["--data-dir", path, "searches", "save", "one", "TBS"])
            .status
            .success()
    );
    let file = directory.join("searches.json");
    fs::write(&file, b"broken").unwrap();
    let restore = run(&["--data-dir", path, "searches", "restore"]);
    assert!(restore.status.success());
    assert!(
        String::from_utf8(restore.stderr)
            .unwrap()
            .contains("復元できません")
    );
    assert_eq!(fs::read(&file).unwrap(), b"broken");
    assert!(
        !run(&["--data-dir", path, "searches", "save", "two", "TBS"])
            .status
            .success()
    );
    assert_eq!(fs::read(&file).unwrap(), b"broken");
}

#[test]
fn tui_help_and_nonterminal_rejection_do_not_start_network_or_write_preferences() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("data");
    let path = directory.to_str().unwrap();
    let help = run(&["--data-dir", path, "tui", "--help"]);
    assert!(help.status.success());
    assert!(
        String::from_utf8(help.stdout)
            .unwrap()
            .contains("g 秒数指定")
    );
    let output = run(&["--data-dir", path, "tui"]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("対話端末")
    );
    assert!(!directory.exists());
}
