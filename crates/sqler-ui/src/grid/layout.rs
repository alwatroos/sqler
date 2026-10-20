// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `ColumnLayout`: the width, order and pinning of the columns of one results grid.
//!
//! Columns are named by their *model* index (the position in the result's header). The layout maps
//! model indices to *display* positions. Pinned columns are always the display prefix
//! `0..pinned_count()`, so they stay left while the rest scrolls.

/// The narrowest a column can be, in pixels.
pub const MIN_COLUMN_PX: f32 = 40.0;
/// The widest a column can be, in pixels.
pub const MAX_COLUMN_PX: f32 = 1200.0;

/// The default width of a column of this type label, in pixels.
#[must_use]
pub fn default_width(type_label: &str) -> f32 {
    match type_label {
        "integer" => 110.0,
        "float" => 120.0,
        "boolean" => 80.0,
        "text" => 220.0,
        _ => 160.0,
    }
}

/// The text with the most characters (the first one on a tie); `None` for no text.
#[must_use]
pub fn widest_text<'a>(texts: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let mut best: Option<(&str, usize)> = None;
    for t in texts {
        let n = t.chars().count();
        if best.is_none_or(|(_, m)| n > m) {
            best = Some((t, n));
        }
    }
    best.map(|(t, _)| t)
}

fn clamp(px: f32) -> f32 {
    if px.is_nan() {
        MIN_COLUMN_PX
    } else {
        px.clamp(MIN_COLUMN_PX, MAX_COLUMN_PX)
    }
}

/// Width, order and pinning of the columns.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ColumnLayout {
    /// Model column at each display position.
    order: Vec<usize>,
    /// Width by model column.
    widths: Vec<f32>,
    /// Default width by model column.
    defaults: Vec<f32>,
    /// How many display positions, from the left, are pinned.
    pinned: usize,
}

impl ColumnLayout {
    /// A layout in model order, nothing pinned, with these default widths (by model column).
    #[must_use]
    pub fn new(default_widths: Vec<f32>) -> Self {
        let defaults: Vec<f32> = default_widths.into_iter().map(clamp).collect();
        Self {
            order: (0..defaults.len()).collect(),
            widths: defaults.clone(),
            defaults,
            pinned: 0,
        }
    }

    /// The number of columns.
    #[must_use]
    pub fn len(&self) -> usize {
        self.order.len()
    }

    /// Whether there are no columns.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// The model column shown at this display position.
    #[must_use]
    pub fn model_column(&self, display: usize) -> Option<usize> {
        self.order.get(display).copied()
    }

    /// The display position of this model column.
    #[must_use]
    pub fn display_column(&self, model: usize) -> Option<usize> {
        self.order.iter().position(|&m| m == model)
    }

    /// The width of a model column, in pixels (the minimum for an unknown column).
    #[must_use]
    pub fn width(&self, model: usize) -> f32 {
        self.widths.get(model).copied().unwrap_or(MIN_COLUMN_PX)
    }

    /// Sets the width, clamped to `MIN_COLUMN_PX..=MAX_COLUMN_PX`. `false` if nothing changed.
    pub fn set_width(&mut self, model: usize, px: f32) -> bool {
        let px = clamp(px);
        match self.widths.get_mut(model) {
            Some(w) if (*w - px).abs() > f32::EPSILON => {
                *w = px;
                true
            }
            _ => false,
        }
    }

    /// Applies widths given in display order (the order of `TableEvent::ColumnWidthsChanged`).
    pub fn set_display_widths(&mut self, widths: &[f32]) {
        for (display, px) in widths.iter().enumerate() {
            if let Some(model) = self.model_column(display) {
                self.set_width(model, *px);
            }
        }
    }

    /// How many columns are pinned.
    #[must_use]
    pub fn pinned_count(&self) -> usize {
        self.pinned
    }

    /// Whether this model column is pinned.
    #[must_use]
    pub fn is_pinned(&self, model: usize) -> bool {
        self.display_column(model).is_some_and(|d| d < self.pinned)
    }

    /// Pins a column, placing it at the end of the pinned prefix. `false` if already pinned or
    /// unknown.
    pub fn pin(&mut self, model: usize) -> bool {
        match self.display_column(model) {
            Some(d) if d >= self.pinned => {
                self.order.remove(d);
                self.order.insert(self.pinned, model);
                self.pinned += 1;
                true
            }
            _ => false,
        }
    }

    /// Unpins a column, placing it at the first position of the scrolling region. `false` if it
    /// is not pinned.
    pub fn unpin(&mut self, model: usize) -> bool {
        match self.display_column(model) {
            Some(d) if d < self.pinned => {
                self.order.remove(d);
                self.pinned -= 1;
                self.order.insert(self.pinned, model);
                true
            }
            _ => false,
        }
    }

    /// The display range of the region that holds this position.
    fn region(&self, display: usize) -> std::ops::Range<usize> {
        if display < self.pinned {
            0..self.pinned
        } else {
            self.pinned..self.order.len()
        }
    }

    /// Moves the column at display `from` to display `to`. Only inside one region (pinned or
    /// scrolling); otherwise, or when nothing moves, `false`.
    pub fn move_display(&mut self, from: usize, to: usize) -> bool {
        if from == to || from >= self.order.len() || to >= self.order.len() {
            return false;
        }
        if !self.region(from).contains(&to) {
            return false;
        }
        let model = self.order.remove(from);
        self.order.insert(to, model);
        true
    }

    /// Moves a column one position left inside its region. `false` at the region's edge.
    pub fn move_left(&mut self, model: usize) -> bool {
        match self.display_column(model) {
            Some(d) if d > self.region(d).start => self.move_display(d, d - 1),
            _ => false,
        }
    }

    /// Moves a column one position right inside its region. `false` at the region's edge.
    pub fn move_right(&mut self, model: usize) -> bool {
        match self.display_column(model) {
            Some(d) if d + 1 < self.region(d).end => self.move_display(d, d + 1),
            _ => false,
        }
    }

    /// Identity order, default widths, nothing pinned.
    pub fn reset(&mut self) {
        self.order = (0..self.defaults.len()).collect();
        self.widths.clone_from(&self.defaults);
        self.pinned = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(n: usize) -> ColumnLayout {
        ColumnLayout::new(vec![100.0; n])
    }

    fn order(l: &ColumnLayout) -> Vec<usize> {
        (0..l.len()).filter_map(|d| l.model_column(d)).collect()
    }

    #[test]
    fn default_widths_by_type() {
        assert_eq!(default_width("integer"), 110.0);
        assert_eq!(default_width("float"), 120.0);
        assert_eq!(default_width("boolean"), 80.0);
        assert_eq!(default_width("text"), 220.0);
        assert_eq!(default_width("blob"), 160.0);
    }

    #[test]
    fn widest_text_counts_chars_and_keeps_first_on_tie() {
        assert_eq!(widest_text([]), None);
        assert_eq!(widest_text(["ab", "abcd", "abc"]), Some("abcd"));
        assert_eq!(widest_text(["ab", "cd"]), Some("ab"));
        // 3 chars but 6 bytes loses against 4 ASCII chars.
        assert_eq!(widest_text(["ééé", "abcd"]), Some("abcd"));
    }

    #[test]
    fn new_uses_defaults_in_model_order() {
        let l = ColumnLayout::new(vec![110.0, 220.0, 80.0]);
        assert_eq!(l.len(), 3);
        assert!(!l.is_empty());
        assert_eq!(order(&l), [0, 1, 2]);
        assert_eq!(l.width(1), 220.0);
        assert_eq!(l.pinned_count(), 0);
        assert!(ColumnLayout::new(vec![]).is_empty());
        assert_eq!(l.width(9), MIN_COLUMN_PX);
    }

    #[test]
    fn set_width_clamps_and_reports_change() {
        let mut l = layout(2);
        assert!(l.set_width(0, 250.0));
        assert_eq!(l.width(0), 250.0);
        assert!(!l.set_width(0, 250.0));
        assert!(l.set_width(0, 1.0));
        assert_eq!(l.width(0), MIN_COLUMN_PX);
        assert!(l.set_width(0, 1e9));
        assert_eq!(l.width(0), MAX_COLUMN_PX);
        assert!(!l.set_width(7, 100.0));
        assert!(l.set_width(1, f32::NAN));
        assert_eq!(l.width(1), MIN_COLUMN_PX);
    }

    #[test]
    fn set_display_widths_follows_display_order() {
        let mut l = layout(3);
        assert!(l.move_display(2, 0));
        assert_eq!(order(&l), [2, 0, 1]);
        l.set_display_widths(&[50.0, 60.0, 70.0]);
        assert_eq!(l.width(2), 50.0);
        assert_eq!(l.width(0), 60.0);
        assert_eq!(l.width(1), 70.0);
        // Extra entries are ignored.
        l.set_display_widths(&[80.0, 80.0, 80.0, 99.0]);
        assert_eq!(l.width(1), 80.0);
    }

    #[test]
    fn model_display_round_trip() {
        let mut l = layout(5);
        l.pin(3);
        l.move_display(4, 2);
        for d in 0..5 {
            let m = l.model_column(d).unwrap();
            assert_eq!(l.display_column(m), Some(d));
        }
        assert_eq!(l.model_column(5), None);
        assert_eq!(l.display_column(5), None);
    }

    #[test]
    fn pin_moves_to_end_of_pinned_prefix() {
        let mut l = layout(4);
        assert!(l.pin(2));
        assert_eq!(order(&l), [2, 0, 1, 3]);
        assert!(l.pin(3));
        assert_eq!(order(&l), [2, 3, 0, 1]);
        assert_eq!(l.pinned_count(), 2);
        assert!(l.is_pinned(2) && l.is_pinned(3));
        assert!(!l.is_pinned(0));
        assert!(!l.pin(2), "already pinned");
        assert!(!l.pin(9), "unknown");
    }

    #[test]
    fn unpin_moves_to_first_scrolling_position() {
        let mut l = layout(4);
        l.pin(2);
        l.pin(3);
        assert!(l.unpin(2));
        assert_eq!(order(&l), [3, 2, 0, 1]);
        assert_eq!(l.pinned_count(), 1);
        assert!(!l.unpin(2), "not pinned");
        assert!(!l.unpin(9));
        assert!(l.unpin(3));
        assert_eq!(order(&l), [3, 2, 0, 1]);
        assert_eq!(l.pinned_count(), 0);
    }

    #[test]
    fn move_display_stays_in_region() {
        let mut l = layout(5);
        l.pin(0);
        l.pin(1);
        assert!(l.move_display(0, 1));
        assert_eq!(order(&l), [1, 0, 2, 3, 4]);
        assert!(!l.move_display(0, 2), "pinned to scrolling");
        assert!(!l.move_display(3, 1), "scrolling to pinned");
        assert!(l.move_display(4, 2));
        assert_eq!(order(&l), [1, 0, 4, 2, 3]);
        assert!(!l.move_display(2, 2), "no-op");
        assert!(!l.move_display(2, 9), "out of range");
        assert!(!l.move_display(9, 2));
        assert_eq!(l.pinned_count(), 2);
    }

    #[test]
    fn move_left_and_right_stop_at_region_edges() {
        let mut l = layout(5);
        l.pin(0);
        l.pin(1);
        assert!(!l.move_left(0), "start of pinned");
        assert!(l.move_right(0));
        assert_eq!(order(&l), [1, 0, 2, 3, 4]);
        assert!(!l.move_right(0), "end of pinned");
        assert!(!l.move_left(2), "start of scrolling");
        assert!(l.move_right(2));
        assert_eq!(order(&l), [1, 0, 3, 2, 4]);
        assert!(l.move_right(2));
        assert_eq!(order(&l), [1, 0, 3, 4, 2]);
        assert!(!l.move_right(2), "end of scrolling");
        assert!(!l.move_right(9));
        assert!(!l.move_left(9));
    }

    #[test]
    fn reset_restores_defaults() {
        let mut l = ColumnLayout::new(vec![110.0, 220.0, 80.0]);
        l.pin(2);
        l.move_right(0);
        l.set_width(1, 500.0);
        l.reset();
        assert_eq!(order(&l), [0, 1, 2]);
        assert_eq!(l.width(1), 220.0);
        assert_eq!(l.pinned_count(), 0);
    }
}
