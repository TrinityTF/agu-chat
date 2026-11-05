//! Port of src/renderer/layout/tabWindow.ts.
//! Which tabs the tab bar shows when they don't all fit. The bar never scrolls: it shows a run of whole tabs
//! that always includes the selected one, and keeps the run where it was, so tabs don't slide around.
//! A tab selected past the right end pushes the run right (the leftmost tab drops out); one selected before
//! it starts the run there. Leftover room after a close pulls hidden tabs back in from the left.

/// Inclusive range of tab indexes to show (`end` is -1 for "no tabs", like the JS).
/// `widths` are the tabs' widths, `room` the space for them.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct TabRange {
    pub start: usize,
    pub end: isize,
}

pub fn tab_window(widths: &[f32], room: f32, active: usize, prev_start: usize) -> TabRange {
    let n = widths.len();
    if n == 0 {
        return TabRange { start: 0, end: -1 };
    }
    let a = active.min(n - 1);
    let mut start = prev_start.min(a);
    let mut used: f32 = widths[start..=a].iter().sum();
    while used > room && start < a {
        used -= widths[start];
        start += 1;
    }
    let mut end = a;
    while end + 1 < n && used + widths[end + 1] <= room {
        end += 1;
        used += widths[end];
    }
    while start > 0 && used + widths[start - 1] <= room {
        start -= 1;
        used += widths[start];
    }
    TabRange { start, end: end as isize }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(n: usize) -> Vec<f32> {
        vec![100.0; n]
    }
    fn r(start: usize, end: isize) -> TabRange {
        TabRange { start, end }
    }

    // Port of test/tabWindow.test.ts
    #[test]
    fn everything_that_fits_from_the_start_the_selected_tab_included() {
        assert_eq!(tab_window(&w(6), 350.0, 1, 0), r(0, 2));
    }

    #[test]
    fn the_run_stays_put_while_the_selected_tab_is_inside_it() {
        assert_eq!(tab_window(&w(6), 350.0, 3, 2), r(2, 4));
    }

    #[test]
    fn selecting_past_the_right_end_drops_tabs_off_the_left() {
        assert_eq!(tab_window(&w(6), 350.0, 4, 0), r(2, 4));
    }

    #[test]
    fn selecting_before_the_run_starts_it_there() {
        assert_eq!(tab_window(&w(6), 350.0, 1, 3), r(1, 3));
    }

    #[test]
    fn leftover_room_pulls_hidden_tabs_back_in_from_the_left() {
        assert_eq!(tab_window(&w(4), 350.0, 3, 3), r(1, 3));
    }

    #[test]
    fn a_selected_tab_wider_than_the_room_still_shows_alone() {
        assert_eq!(tab_window(&[100.0, 500.0, 100.0], 300.0, 1, 0), r(1, 1));
    }
}
