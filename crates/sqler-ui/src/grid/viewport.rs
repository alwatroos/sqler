// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

use std::ops::Range;

/// Row height in pixels of the results table (`gpui-component` `Size::Medium`).
pub const ROW_HEIGHT_PX: f64 = 32.0;

/// A pure model of virtual-list windowing: a scroll offset in pixels maps to the range of rows
/// that must be drawn. It holds no UI types, so the formula is testable without a window.
///
/// All math is `f64`: 1M rows of 32 px is 32e6 px, far inside the 2^53 exact-integer range.
#[derive(Debug, Clone, PartialEq)]
pub struct ScrollViewport {
    row_height: f64,
    height: f64,
    offset: f64,
    rows: usize,
    overscan: usize,
}

impl ScrollViewport {
    /// A viewport of `height` px over rows of `row_height` px, drawing `overscan` extra rows on
    /// each side. `row_height` is at least 1 and `height` at least 0.
    #[must_use]
    pub fn new(row_height: f64, height: f64, overscan: usize) -> Self {
        Self {
            row_height: row_height.max(1.0),
            height: height.max(0.0),
            offset: 0.0,
            rows: 0,
            overscan,
        }
    }

    /// Sets the number of rows and re-clamps the offset.
    pub fn set_rows(&mut self, rows: usize) {
        self.rows = rows;
        self.clamp();
    }

    /// Sets the viewport height in pixels and re-clamps the offset.
    pub fn set_height(&mut self, h: f64) {
        self.height = h.max(0.0);
        self.clamp();
    }

    /// The number of rows.
    #[must_use]
    pub fn rows(&self) -> usize {
        self.rows
    }

    /// The scroll offset in pixels from the top.
    #[must_use]
    pub fn offset(&self) -> f64 {
        self.offset
    }

    /// The largest scroll offset: content height minus viewport height, never negative.
    #[must_use]
    pub fn max_offset(&self) -> f64 {
        (self.content_height() - self.height).max(0.0)
    }

    #[allow(clippy::cast_precision_loss)] // Row counts stay far below 2^53.
    fn content_height(&self) -> f64 {
        self.rows as f64 * self.row_height
    }

    fn clamp(&mut self) {
        self.offset = self.offset.clamp(0.0, self.max_offset());
    }

    /// Scrolls by `delta` pixels (negative is up), clamped to the content.
    pub fn scroll_by(&mut self, delta: f64) {
        self.offset += delta;
        self.clamp();
    }

    /// Scrolls so `row` is at the top, clamped to the content.
    #[allow(clippy::cast_precision_loss)] // Row indices stay far below 2^53.
    pub fn scroll_to_row(&mut self, row: usize) {
        self.offset = row as f64 * self.row_height;
        self.clamp();
    }

    /// Scrolls to the first row.
    pub fn jump_to_start(&mut self) {
        self.offset = 0.0;
    }

    /// Scrolls to the end of the content.
    pub fn jump_to_end(&mut self) {
        self.offset = self.max_offset();
    }

    /// The rows to draw: those intersecting the viewport plus the overscan on both sides.
    #[must_use]
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Both values are finite and non-negative; saturating cast is intended.
    pub fn visible(&self) -> Range<usize> {
        if self.rows == 0 {
            return 0..0;
        }
        let first = (self.offset / self.row_height).floor() as usize;
        let last = ((self.offset + self.height) / self.row_height).ceil() as usize;
        let end = self.rows.min(last.saturating_add(self.overscan));
        let start = first.saturating_sub(self.overscan).min(end);
        start..end
    }

    /// The most rows [`ScrollViewport::visible`] can ever return: `ceil(h / rh) + 1 + 2 * overscan`.
    #[must_use]
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Finite, non-negative value; saturating cast is intended.
    pub fn max_visible_rows(&self) -> usize {
        let fit = (self.height / self.row_height).ceil() as usize;
        fit.saturating_add(1)
            .saturating_add(self.overscan.saturating_mul(2))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vp(rows: usize) -> ScrollViewport {
        let mut v = ScrollViewport::new(ROW_HEIGHT_PX, 760.0, 2);
        v.set_rows(rows);
        v
    }

    #[test]
    fn million_rows_scrolled_in_steps_stay_bounded_and_reach_the_end() {
        let mut v = vp(1_000_000);
        let cap = v.max_visible_rows();
        let mut reached = false;
        let mut steps = 0_u64;
        loop {
            let r = v.visible();
            assert!(r.len() <= cap, "{r:?} at {}", v.offset());
            reached |= r.end == 1_000_000;
            if v.offset() >= v.max_offset() {
                break;
            }
            v.scroll_by(96.0);
            steps += 1;
        }
        assert!(reached);
        assert_eq!(steps, 333_326);
    }

    #[test]
    fn jump_to_end_contains_last_row() {
        let mut v = vp(1_000_000);
        v.jump_to_end();
        let r = v.visible();
        assert_eq!(r.end, 1_000_000);
        assert!(r.contains(&999_999));
        assert!(r.len() <= v.max_visible_rows());
        v.jump_to_start();
        assert_eq!(v.visible().start, 0);
    }

    #[test]
    fn clamps_at_both_ends() {
        let mut v = vp(100);
        v.scroll_by(-500.0);
        assert_eq!(v.offset(), 0.0);
        v.scroll_by(1e12);
        assert_eq!(v.offset(), v.max_offset());
        assert_eq!(v.max_offset(), 100.0 * 32.0 - 760.0);
        v.scroll_to_row(10_000);
        assert_eq!(v.offset(), v.max_offset());
        v.scroll_to_row(3);
        assert_eq!(v.offset(), 96.0);
    }

    #[test]
    fn resize_and_row_changes_reclamp() {
        let mut v = vp(100);
        v.jump_to_end();
        v.set_height(1_600.0);
        assert_eq!(v.offset(), v.max_offset());
        assert_eq!(v.offset(), 3_200.0 - 1_600.0);
        v.set_rows(10);
        assert_eq!(v.offset(), 0.0);
        assert_eq!(v.visible(), 0..10);
        v.set_height(-5.0);
        assert_eq!(v.max_visible_rows(), 1 + 4);
    }

    #[test]
    fn no_rows_is_empty() {
        let mut v = vp(0);
        assert_eq!(v.visible(), 0..0);
        v.jump_to_end();
        v.scroll_by(50.0);
        assert_eq!(v.offset(), 0.0);
        assert_eq!(v.visible(), 0..0);
    }

    #[test]
    fn overscan_at_edges() {
        let mut v = vp(1_000);
        // 760 / 32 = 23.75 -> 24 rows, +2 overscan below; none above at the top.
        assert_eq!(v.visible(), 0..26);
        v.scroll_to_row(100);
        assert_eq!(v.visible(), 98..(100 + 24 + 2));
        v.jump_to_end();
        assert_eq!(v.visible().end, 1_000);
        let no_overscan = {
            let mut n = ScrollViewport::new(32.0, 64.0, 0);
            n.set_rows(10);
            n.scroll_to_row(2);
            n.visible()
        };
        assert_eq!(no_overscan, 2..4);
    }

    #[test]
    fn offset_is_exact_at_one_million_rows() {
        let mut v = vp(1_000_000);
        v.scroll_to_row(999_000);
        assert_eq!(v.offset(), 999_000.0 * 32.0);
        let r = v.visible();
        assert_eq!(r.start, 999_000 - 2);
        assert!(r.contains(&999_000));
    }

    #[test]
    fn constructor_sanitises() {
        let v = ScrollViewport::new(0.0, -3.0, 1);
        assert_eq!(v.max_visible_rows(), 1 + 2);
    }
}
