use radiko_client::radiko::catalog::{CatalogError, MAX_SCHEDULE_BYTES, parse_day, parse_programs};
const XML: &[u8] = include_bytes!("fixtures/schedule.xml");

#[test]
fn preserves_split_slots_unknown_classes_and_unclassified_boundary_slots() {
    let list = parse_programs(XML, "TBS", parse_day("20261015").unwrap()).unwrap();
    assert_eq!(list.programs.len(), 3);
    assert_eq!((list.invalid_count, list.duplicate_count), (1, 1));
    assert_eq!(list.programs[0].title, "境界 & 番組");
    assert!(list.programs[0].program_genres.is_empty());
    assert_eq!(list.programs[1].program_id, list.programs[2].program_id);
    assert_ne!(list.programs[1].key, list.programs[2].key);
    assert_eq!(list.programs[1].program_genres[0].id, "UNKNOWN");
    assert_eq!(list.programs[1].personality_genres[0].id, "C999");
    assert_eq!(list.programs[1].tags, ["タグ"]);
}

#[test]
fn rejects_wrong_station_dates_malformed_xml_and_oversized_responses() {
    let day = parse_day("20261015").unwrap();
    assert!(matches!(
        parse_programs(XML, "OBS", day),
        Err(CatalogError::WrongStation)
    ));
    for date in ["20260230", "2026-10-15", "2026101", "abcdefgh"] {
        assert!(parse_day(date).is_err());
    }
    for xml in [
        "<radiko>",
        "<html/>",
        "<radiko/><radiko/>",
        "<!DOCTYPE radiko><radiko/>",
        "<radiko><stations/></radiko>",
    ] {
        assert!(parse_programs(xml.as_bytes(), "TBS", day).is_err());
    }
    assert!(matches!(
        parse_programs(&vec![b' '; MAX_SCHEDULE_BYTES + 1], "TBS", day),
        Err(CatalogError::TooLarge)
    ));
    assert!(matches!(
        parse_programs(XML, "../TBS", day),
        Err(CatalogError::InvalidStation)
    ));
}

#[test]
fn distinguishes_empty_schedule_from_invalid_or_wrong_day_data() {
    let day = parse_day("20261015").unwrap();
    let empty = b"<radiko><stations><station id=\"TBS\"><name>TBS</name><progs/></station></stations></radiko>";
    assert!(
        parse_programs(empty, "TBS", day)
            .unwrap()
            .programs
            .is_empty()
    );
    assert!(matches!(
        parse_programs(XML, "TBS", parse_day("20261017").unwrap()),
        Err(CatalogError::NoValidPrograms(5))
    ));
}

#[test]
fn backwards_overnight_end_is_excluded_without_guessing_a_replacement_date() {
    let xml = br#"<radiko><stations><station id="TBS"><name>synthetic</name><progs>
      <prog id="valid" ft="20261015060000" to="20261015070000"><title>valid</title></prog>
      <prog id="backwards" ft="20261016041000" to="20261015051000"><title>invalid</title></prog>
    </progs></station></stations></radiko>"#;
    let list = parse_programs(xml, "TBS", parse_day("20261015").unwrap()).unwrap();
    assert_eq!(list.invalid_count, 1);
    assert_eq!(list.programs.len(), 1);
    assert_eq!(list.programs[0].program_id, "valid");
}
