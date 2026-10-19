//! One-line catalog rows only. Preserve global selection while formatting visible items.
use ratatui::widgets::ListState;
use std::ops::Range;

pub fn visible(state: &mut ListState, count: usize, height: usize) -> Range<usize> {
    if count == 0 {
        state.select(None);
        return 0..0;
    }
    if let Some(selected) = state.selected() {
        state.select(Some(selected.min(count - 1)));
    }
    let mut start = state.offset().min(count.saturating_sub(height.max(1)));
    if height == 0 {
        *state.offset_mut() = start;
        return start..start;
    }
    if let Some(selected) = state.selected() {
        if selected < start {
            start = selected;
        } else if selected >= start.saturating_add(height) {
            start = selected.saturating_add(1).saturating_sub(height);
        }
    }
    *state.offset_mut() = start;
    start..start.saturating_add(height).min(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selection_stays_global_across_small_views_resizes_and_shrinking_results() {
        let mut state = ListState::default();
        state.select(Some(49999));
        for height in [1, 40, 4, 0, 12] {
            let range = visible(&mut state, 50000, height);
            assert_eq!(state.selected(), Some(49999));
            assert!(range.len() <= height);
            if height > 0 {
                assert!(range.contains(&49999));
            }
        }
        let range = visible(&mut state, 7, 3);
        assert_eq!(state.selected(), Some(6));
        assert!(range.contains(&6));
        state.select(Some(0));
        assert!(visible(&mut state, 7, 3).contains(&0));
        assert_eq!(visible(&mut state, 0, 3), 0..0);
        assert_eq!(state.selected(), None);
        assert_eq!(state.offset(), 0);
    }
}
