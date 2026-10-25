use chrono::DateTime;
use radiko::{
    api::{ChapterError, parse_chapters},
    domain::{BroadcastStatus, broadcast_day, broadcast_status},
};
use std::time::Duration;

#[test]
fn broadcast_day_boundary_is_japanese_time() {
    for (input, expected) in [
        ("2026-10-15T04:59:59+09:00", "2026-10-14"),
        ("2026-10-15T05:00:00+09:00", "2026-10-15"),
        ("2026-10-14T20:00:00+00:00", "2026-10-15"),
    ] {
        let day = broadcast_day(DateTime::parse_from_rfc3339(input).unwrap()).unwrap();
        assert_eq!(day.to_string(), expected);
    }
}
#[test]
fn broadcast_status_does_not_imply_listening_permission() {
    let start = DateTime::parse_from_rfc3339("2026-10-15T01:00:00+09:00").unwrap();
    let end = start + chrono::Duration::hours(1);
    assert_eq!(
        broadcast_status(start, end, start).unwrap(),
        BroadcastStatus::OnAir
    );
    assert_eq!(
        broadcast_status(start, end, end).unwrap(),
        BroadcastStatus::Finished
    );
    assert!(broadcast_status(end, start, start).is_err());
}
#[test]
fn normalizes_fixture_and_uses_half_open_intervals() {
    let set = parse_chapters(
        include_bytes!("../../../tests/fixtures/chapters.json"),
        Duration::from_secs(90),
        0.0,
    )
    .unwrap();
    assert_eq!(
        (set.chapters.len(), set.invalid_count, set.music_count),
        (2, 1, 1)
    );
    assert_eq!(set.current(Duration::ZERO).unwrap().title, "オープニング");
    assert_eq!(set.current(Duration::from_secs(30)).unwrap().title, "本題");
    assert!(set.current(Duration::from_secs(60)).is_none());
}
#[test]
fn split_slot_offset_rejects_other_slots_without_clipping() {
    let set = parse_chapters(
        include_bytes!("../../../tests/fixtures/chapters.json"),
        Duration::from_secs(30),
        -30.0,
    )
    .unwrap();
    assert_eq!(set.chapters.len(), 1);
    assert_eq!(set.chapters[0].title, "本題");
    assert_eq!(set.chapters[0].start, Duration::ZERO);
}
#[test]
fn unavailable_is_distinct_from_malformed() {
    assert!(
        parse_chapters(br#"{"chapters":[]}"#, Duration::from_secs(10), 0.0)
            .unwrap()
            .chapters
            .is_empty()
    );
    assert_eq!(
        parse_chapters(b"{}", Duration::from_secs(10), 0.0).unwrap_err(),
        ChapterError::MissingArray
    );
    assert_eq!(
        parse_chapters(
            br#"{"chapters":[{"fromSec":0,"toSec":100}]}"#,
            Duration::from_secs(10),
            0.0
        )
        .unwrap_err(),
        ChapterError::NoValidChapters
    );
}
#[test]
fn overlap_ties_and_terminal_text_are_deterministic() {
    let json = br#"{"chapters":[
        {"title":"first\u001b\n","fromSec":0,"toSec":30},
        {"title":"second","fromSec":10,"toSec":20},
        {"title":"third","fromSec":10,"toSec":25}] }"#;
    let set = parse_chapters(json, Duration::from_secs(40), 0.0).unwrap();
    assert_eq!(set.chapters[0].title, "first");
    assert_eq!(
        set.current(Duration::from_secs(15)).unwrap().title,
        "second"
    );
    assert_eq!(set.current(Duration::from_secs(20)).unwrap().title, "third");
}
