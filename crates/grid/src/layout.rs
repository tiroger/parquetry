//! Pure grid geometry: column offsets, visible ranges, hit testing and scrollbars.
//!
//! Everything here works in plain `f32`/`f64` so it can be unit-tested without a
//! window. Vertical positions are measured in *rows* (`f64`) rather than pixels:
//! a 600-million-row table is 15 billion pixels tall, far beyond `f32` precision.

use std::ops::Range;

/// Horizontal layout of the displayed columns.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ColumnLayout {
    /// Left edge of each displayed column relative to its region (pinned or scrolling).
    starts: Vec<f32>,
    widths: Vec<f32>,
    /// How many leading display columns are pinned.
    pinned: usize,
}

impl ColumnLayout {
    pub fn new(widths: Vec<f32>, pinned: usize) -> Self {
        let pinned = pinned.min(widths.len());
        let mut starts = Vec::with_capacity(widths.len());
        let mut x = 0.0;
        for (ix, width) in widths.iter().enumerate() {
            if ix == pinned {
                x = 0.0;
            }
            starts.push(x);
            x += width;
        }
        Self {
            starts,
            widths,
            pinned,
        }
    }

    pub fn len(&self) -> usize {
        self.widths.len()
    }

    pub fn is_empty(&self) -> bool {
        self.widths.is_empty()
    }

    pub fn pinned(&self) -> usize {
        self.pinned
    }

    pub fn width(&self, ix: usize) -> f32 {
        self.widths[ix]
    }

    pub fn pinned_width(&self) -> f32 {
        self.widths[..self.pinned].iter().sum()
    }

    pub fn scrolling_width(&self) -> f32 {
        self.widths[self.pinned..].iter().sum()
    }

    pub fn is_pinned(&self, ix: usize) -> bool {
        ix < self.pinned
    }

    /// Left edge of a column in body coordinates (0 = right of the row gutter),
    /// given the horizontal scroll offset of the scrolling region.
    pub fn left(&self, ix: usize, scroll_x: f32) -> f32 {
        if ix < self.pinned {
            self.starts[ix]
        } else {
            self.pinned_width() + self.starts[ix] - scroll_x
        }
    }

    /// Scrolling columns intersecting `[scroll_x, scroll_x + viewport)`.
    pub fn visible_scrolling(&self, scroll_x: f32, viewport: f32) -> Range<usize> {
        if self.pinned >= self.widths.len() || viewport <= 0.0 {
            return self.pinned..self.pinned;
        }
        let first = self.first_scrolling_at(scroll_x);
        let mut last = first;
        while last < self.widths.len() && self.starts[last] < scroll_x + viewport {
            last += 1;
        }
        first..last
    }

    /// Index of the scrolling column containing offset `x` of the scrolling region.
    fn first_scrolling_at(&self, x: f32) -> usize {
        let region = &self.starts[self.pinned..];
        let ix = region.partition_point(|start| *start <= x);
        (self.pinned + ix.saturating_sub(1)).min(self.widths.len().saturating_sub(1).max(self.pinned))
    }

    /// Displayed column at body x (0 = right of the row gutter), if any.
    pub fn column_at(&self, x: f32, scroll_x: f32) -> Option<usize> {
        if x < 0.0 {
            return None;
        }
        let pinned_width = self.pinned_width();
        if x < pinned_width {
            return (0..self.pinned).find(|&ix| x >= self.starts[ix] && x < self.starts[ix] + self.widths[ix]);
        }
        let region_x = x - pinned_width + scroll_x;
        if self.pinned >= self.widths.len() {
            return None;
        }
        let ix = self.first_scrolling_at(region_x);
        if region_x >= self.starts[ix] && region_x < self.starts[ix] + self.widths[ix] {
            Some(ix)
        } else {
            None
        }
    }

    /// Column whose right edge is within `slop` of body x: the resize handle.
    pub fn resize_handle_at(&self, x: f32, scroll_x: f32, slop: f32) -> Option<usize> {
        let pinned_width = self.pinned_width();
        for ix in 0..self.widths.len() {
            let right = self.left(ix, scroll_x) + self.widths[ix];
            if ix >= self.pinned && right <= pinned_width {
                continue; // scrolled under the pinned region
            }
            if (x - right).abs() <= slop {
                return Some(ix);
            }
            if right > x + slop && ix >= self.pinned {
                break;
            }
        }
        None
    }

    /// Scroll offset that brings column `ix` fully into a viewport of `viewport` width.
    pub fn scroll_to_reveal(&self, ix: usize, scroll_x: f32, viewport: f32) -> f32 {
        if ix < self.pinned || ix >= self.widths.len() {
            return scroll_x;
        }
        let start = self.starts[ix];
        let end = start + self.widths[ix];
        let visible = (viewport - self.pinned_width()).max(0.0);
        if start < scroll_x {
            start
        } else if end > scroll_x + visible {
            (end - visible).min(start)
        } else {
            scroll_x
        }
    }

    pub fn max_scroll_x(&self, viewport: f32) -> f32 {
        (self.scrolling_width() - (viewport - self.pinned_width()).max(0.0)).max(0.0)
    }
}

/// Vertical scrolling in row units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RowViewport {
    /// Fractional index of the row at the top edge of the body.
    pub top: f64,
    /// Body height in rows (fractional).
    pub height_rows: f64,
    pub row_count: u64,
}

impl RowViewport {
    pub fn max_top(&self) -> f64 {
        (self.row_count as f64 - self.height_rows).max(0.0)
    }

    pub fn clamp_top(&self, top: f64) -> f64 {
        if top.is_nan() {
            return 0.0;
        }
        top.clamp(0.0, self.max_top())
    }

    /// Rows at least partly visible.
    pub fn visible_rows(&self) -> Range<u64> {
        let first = self.top.floor().max(0.0) as u64;
        let last = ((self.top + self.height_rows).ceil().max(0.0) as u64).min(self.row_count);
        first.min(last)..last
    }

    /// Top position that brings `row` fully into view with minimal movement.
    pub fn reveal(&self, row: u64) -> f64 {
        let row = row as f64;
        let fully_visible = self.height_rows.floor().max(1.0);
        if row < self.top {
            self.clamp_top(row)
        } else if row + 1.0 > self.top + self.height_rows {
            self.clamp_top(row + 1.0 - fully_visible.min(self.height_rows))
        } else {
            self.top
        }
    }
}

/// Scrollbar thumb geometry along one axis.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Thumb {
    pub start: f32,
    pub length: f32,
}

/// Thumb for content of `content` units shown through `visible` units at `offset`,
/// on a track `track` pixels long. `None` when everything fits.
pub fn thumb(content: f64, visible: f64, offset: f64, track: f32, min_length: f32) -> Option<Thumb> {
    if content <= visible || track <= 0.0 || content <= 0.0 {
        return None;
    }
    let length = ((visible / content) as f32 * track).max(min_length).min(track);
    let max_offset = (content - visible).max(1e-9);
    let start = ((offset / max_offset).clamp(0.0, 1.0) as f32) * (track - length);
    Some(Thumb { start, length })
}

/// Content offset for a thumb dragged so its start is at `thumb_start`.
pub fn offset_for_thumb(content: f64, visible: f64, thumb_start: f32, thumb_length: f32, track: f32) -> f64 {
    let travel = (track - thumb_length).max(1e-6);
    let fraction = (thumb_start / travel).clamp(0.0, 1.0) as f64;
    fraction * (content - visible).max(0.0)
}

/// Number of characters that fit in `width` at `char_width`, keeping room for an ellipsis.
pub fn fitting_chars(width: f32, char_width: f32) -> usize {
    if char_width <= 0.0 || width <= 0.0 {
        return 0;
    }
    (width / char_width).floor() as usize
}

/// Truncate `text` to `max_chars` characters, ending with `…` when cut.
pub fn truncate_chars(text: &str, max_chars: usize) -> Option<String> {
    if max_chars == 0 {
        return Some(String::new());
    }
    let mut end = None;
    for (count, (ix, _)) in text.char_indices().enumerate() {
        if count == max_chars.saturating_sub(1) {
            end = Some(ix);
        }
        if count == max_chars {
            let cut = end.unwrap_or(0);
            return Some(format!("{}…", &text[..cut]));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_offsets_and_hits() {
        let layout = ColumnLayout::new(vec![100.0, 50.0, 80.0, 120.0], 1);
        assert_eq!(layout.pinned_width(), 100.0);
        assert_eq!(layout.scrolling_width(), 250.0);
        assert_eq!(layout.left(0, 30.0), 0.0);
        assert_eq!(layout.left(1, 0.0), 100.0);
        assert_eq!(layout.left(2, 30.0), 100.0 + 50.0 - 30.0);
        assert_eq!(layout.column_at(10.0, 30.0), Some(0));
        assert_eq!(layout.column_at(100.0, 0.0), Some(1));
        assert_eq!(layout.column_at(149.0, 0.0), Some(1));
        assert_eq!(layout.column_at(150.0, 0.0), Some(2));
        assert_eq!(layout.column_at(100.0, 50.0), Some(2));
        assert_eq!(layout.column_at(400.0, 0.0), None);
        assert_eq!(layout.column_at(-1.0, 0.0), None);
        assert_eq!(layout.visible_scrolling(0.0, 60.0), 1..3);
        assert_eq!(layout.visible_scrolling(55.0, 10.0), 2..3);
        assert_eq!(layout.visible_scrolling(0.0, 1000.0), 1..4);
        assert_eq!(layout.max_scroll_x(200.0), 150.0);
        assert_eq!(layout.max_scroll_x(1000.0), 0.0);
    }

    #[test]
    fn resize_handles() {
        let layout = ColumnLayout::new(vec![100.0, 50.0, 80.0], 0);
        assert_eq!(layout.resize_handle_at(99.0, 0.0, 4.0), Some(0));
        assert_eq!(layout.resize_handle_at(152.0, 0.0, 4.0), Some(1));
        assert_eq!(layout.resize_handle_at(120.0, 0.0, 4.0), None);
        assert_eq!(layout.resize_handle_at(50.0, 100.0, 4.0), Some(1));
    }

    #[test]
    fn reveal_columns() {
        let layout = ColumnLayout::new(vec![100.0, 100.0, 100.0, 100.0], 1);
        // Viewport 250: pinned 100, 150 visible for scrolling columns.
        assert_eq!(layout.scroll_to_reveal(3, 0.0, 250.0), 150.0);
        assert_eq!(layout.scroll_to_reveal(1, 150.0, 250.0), 0.0);
        assert_eq!(layout.scroll_to_reveal(2, 50.0, 250.0), 50.0);
        assert_eq!(layout.scroll_to_reveal(2, 20.0, 250.0), 50.0);
        assert_eq!(layout.scroll_to_reveal(0, 70.0, 250.0), 70.0);
    }

    #[test]
    fn empty_and_all_pinned() {
        let layout = ColumnLayout::new(vec![], 3);
        assert!(layout.is_empty());
        assert_eq!(layout.column_at(5.0, 0.0), None);
        assert_eq!(layout.visible_scrolling(0.0, 100.0), 0..0);
        let layout = ColumnLayout::new(vec![10.0, 10.0], 5);
        assert_eq!(layout.pinned(), 2);
        assert_eq!(layout.visible_scrolling(0.0, 100.0), 2..2);
        assert_eq!(layout.column_at(15.0, 0.0), Some(1));
        assert_eq!(layout.column_at(25.0, 0.0), None);
    }

    #[test]
    fn rows_in_huge_tables() {
        let viewport = RowViewport {
            top: 599_999_990.5,
            height_rows: 30.0,
            row_count: 600_000_000,
        };
        assert_eq!(viewport.max_top(), 599_999_970.0);
        assert_eq!(viewport.clamp_top(1e12), 599_999_970.0);
        assert_eq!(viewport.clamp_top(-5.0), 0.0);
        assert_eq!(viewport.clamp_top(f64::NAN), 0.0);
        let clamped = RowViewport { top: viewport.max_top(), ..viewport };
        assert_eq!(clamped.visible_rows(), 599_999_970..600_000_000);
        let small = RowViewport { top: 0.0, height_rows: 30.0, row_count: 5 };
        assert_eq!(small.visible_rows(), 0..5);
        assert_eq!(small.max_top(), 0.0);
    }

    #[test]
    fn reveal_rows() {
        let viewport = RowViewport { top: 100.0, height_rows: 10.5, row_count: 1000 };
        assert_eq!(viewport.reveal(105), 100.0);
        assert_eq!(viewport.reveal(50), 50.0);
        assert_eq!(viewport.reveal(200), 191.0);
        assert_eq!(viewport.reveal(999), 989.5);
    }

    #[test]
    fn thumbs() {
        assert!(thumb(10.0, 20.0, 0.0, 100.0, 10.0).is_none());
        let t = thumb(1000.0, 100.0, 0.0, 200.0, 10.0).unwrap();
        assert_eq!(t.start, 0.0);
        assert_eq!(t.length, 20.0);
        let t = thumb(1000.0, 100.0, 900.0, 200.0, 10.0).unwrap();
        assert_eq!(t.start, 180.0);
        // Huge content: thumb keeps its minimum length and reaches the end.
        let t = thumb(6e8, 30.0, 6e8 - 30.0, 500.0, 24.0).unwrap();
        assert_eq!(t.length, 24.0);
        assert_eq!(t.start, 476.0);
        let offset = offset_for_thumb(6e8, 30.0, 238.0, 24.0, 500.0);
        assert!((offset - (6e8 - 30.0) / 2.0).abs() < 1.0);
    }

    #[test]
    fn truncation() {
        assert_eq!(truncate_chars("hello", 10), None);
        assert_eq!(truncate_chars("hello", 5), None);
        assert_eq!(truncate_chars("hello world", 5).as_deref(), Some("hell…"));
        assert_eq!(truncate_chars("héllo wörld", 3).as_deref(), Some("hé…"));
        assert_eq!(truncate_chars("ab", 1).as_deref(), Some("…"));
        assert_eq!(truncate_chars("ab", 0).as_deref(), Some(""));
        assert_eq!(fitting_chars(100.0, 8.0), 12);
    }
}
