//! Pure filtering of this acquisition only. This module performs no I/O or caching.
use crate::domain::{Classification, Program};
use std::collections::BTreeSet;

pub use crate::domain::search::Filter;
impl Filter {
    pub fn matches(&self, program: &Program) -> bool {
        fn matches_class(items: &[Classification], ids: &[String], empty: bool) -> bool {
            (ids.is_empty() && !empty)
                || (empty && items.is_empty())
                || items.iter().any(|item| ids.contains(&item.id))
        }
        matches_class(
            &program.program_genres,
            &self.program_genres,
            self.unclassified_program,
        ) && matches_class(
            &program.personality_genres,
            &self.personality_genres,
            self.unclassified_personality,
        ) && self.keyword_lowercase.as_ref().is_none_or(|keyword| {
            [
                &program.title,
                &program.performer,
                &program.description,
                &program.info,
            ]
            .into_iter()
            .chain(program.tags.iter())
            .any(|text| text.to_lowercase().contains(keyword))
        })
    }
}

#[derive(Debug, Default)]
pub struct ObservedClassifications {
    pub program_genres: Vec<Classification>,
    pub personality_genres: Vec<Classification>,
    pub unclassified_programs: usize,
    pub unclassified_personalities: usize,
}
impl ObservedClassifications {
    /// Observed ID/name pairs, including aliases, are not a complete classification catalog.
    pub fn collect<'a>(programs: impl IntoIterator<Item = &'a Program>) -> Self {
        let mut result = Self::default();
        let (mut genres, mut personalities) = (BTreeSet::new(), BTreeSet::new());
        for program in programs {
            result.unclassified_programs += usize::from(program.program_genres.is_empty());
            result.unclassified_personalities += usize::from(program.personality_genres.is_empty());
            for class in &program.program_genres {
                genres.insert((class.id.clone(), class.name.clone()));
            }
            for class in &program.personality_genres {
                personalities.insert((class.id.clone(), class.name.clone()));
            }
        }
        let classes = |items: BTreeSet<(String, String)>| {
            items
                .into_iter()
                .map(|(id, name)| Classification { id, name })
                .collect()
        };
        result.program_genres = classes(genres);
        result.personality_genres = classes(personalities);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn programs() -> Vec<Program> {
        crate::radiko::catalog::parse_programs(
            include_bytes!("../../tests/fixtures/schedule.xml"),
            "TBS",
            crate::radiko::catalog::parse_day("20261015").unwrap(),
        )
        .unwrap()
        .programs
    }
    fn filter(genres: &[&str], people: &[&str], empty: bool, keyword: Option<&str>) -> Filter {
        Filter::new(
            genres.iter().map(|s| s.to_string()).collect(),
            people.iter().map(|s| s.to_string()).collect(),
            empty,
            false,
            keyword.map(str::to_string),
        )
        .unwrap()
    }
    #[test]
    fn different_conditions_are_and_and_multiple_ids_within_a_class_are_or() {
        let items = programs();
        let program = &items[1];
        assert!(
            filter(
                &["other", "UNKNOWN"],
                &["C999", "other"],
                false,
                Some("出演者")
            )
            .matches(program)
        );
        assert!(!filter(&["UNKNOWN"], &["other"], false, None).matches(program));
        assert!(!filter(&["other"], &["C999"], false, None).matches(program));
        assert!(!filter(&["UNKNOWN"], &["C999"], false, Some("存在しない")).matches(program));
        // A personality classification is never interpreted as a program genre.
        assert!(!filter(&["C999"], &[], false, None).matches(program));
    }
    #[test]
    fn unclassified_is_selectable_and_can_be_combined_with_named_ids_using_or() {
        let items = programs();
        assert_eq!(
            items
                .iter()
                .filter(|p| filter(&[], &[], true, None).matches(p))
                .count(),
            2
        );
        assert_eq!(
            items
                .iter()
                .filter(|p| filter(&["UNKNOWN"], &[], true, None).matches(p))
                .count(),
            3
        );
        let people = Filter::new(vec![], vec![], false, true, None).unwrap();
        assert_eq!(items.iter().filter(|p| people.matches(p)).count(), 2);
    }
    #[test]
    fn keyword_is_case_insensitive_literal_substring_in_each_searchable_field() {
        for field in 0..5 {
            let mut p = programs().remove(0);
            p.title.clear();
            p.performer.clear();
            p.description.clear();
            p.info.clear();
            p.tags.clear();
            match field {
                0 => p.title = "An Example".into(),
                1 => p.performer = "An Example".into(),
                2 => p.description = "An Example".into(),
                3 => p.info = "An Example".into(),
                _ => p.tags = vec!["An Example".into()],
            }
            assert!(filter(&[], &[], false, Some("EXAMPLE")).matches(&p));
            assert!(!filter(&[], &[], false, Some("Ex.*")).matches(&p));
        }
        assert!(filter(&[], &[], false, Some(" タグ ")).matches(&programs()[1]));
    }
    #[test]
    fn filtering_preserves_separate_stations_and_split_slots_with_the_same_program_id() {
        let mut items = programs();
        let mut other = items[1].clone();
        other.key.station_id = "OBS".into();
        items.push(other);
        let selected: Vec<_> = items
            .iter()
            .filter(|p| Filter::default().matches(p))
            .collect();
        assert_eq!(selected.len(), 4);
        assert_eq!(selected[1].program_id, selected[2].program_id);
        assert_ne!(selected[1].key, selected[2].key);
        assert_ne!(selected[1].key, selected[3].key);
    }
    #[test]
    fn observed_classes_are_unfiltered_pairs_not_a_fixed_catalog() {
        let mut items = programs();
        items[1].program_genres.push(Classification {
            id: "UNKNOWN".into(),
            name: "別名".into(),
        });
        let observed = ObservedClassifications::collect(&items);
        assert_eq!(observed.program_genres.len(), 2);
        assert_eq!(observed.personality_genres[0].id, "C999");
        assert_eq!(observed.unclassified_programs, 2);
        assert_eq!(observed.unclassified_personalities, 2);
        assert_eq!(
            items
                .iter()
                .filter(|p| filter(&["absent"], &[], false, None).matches(p))
                .count(),
            0
        );
        assert!(!observed.program_genres.is_empty());
    }
    #[test]
    fn invalid_criteria_fail_and_serialization_excludes_precomputed_search_text() {
        for ids in [
            vec!["".into()],
            vec![" ".into()],
            vec!["a\nb".into()],
            vec!["x".repeat(129)],
        ] {
            assert!(Filter::new(ids, vec![], false, false, None).is_err());
        }
        for keyword in ["", " ", "a\nb"] {
            assert!(Filter::new(vec![], vec![], false, false, Some(keyword.into())).is_err());
        }
        let f = filter(&["UNKNOWN", "UNKNOWN"], &[], false, Some("Example"));
        let json = serde_json::to_value(f).unwrap();
        assert_eq!(json["program_genres"].as_array().unwrap().len(), 1);
        assert_eq!(json["keyword"], "Example");
        assert!(json.get("keyword_lowercase").is_none());
    }
}
