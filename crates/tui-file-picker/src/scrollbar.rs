//! Shared proportional-scrollbar geometry.
//!
//! The application and standalone picker can render these metrics using their
//! own theme and hit-target systems. Keeping the arithmetic here guarantees the
//! same rounding, minimum-thumb, and endpoint behavior everywhere.

/// Result of mapping a logical viewport onto a vertical scrollbar track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollbarMetrics {
    /// Number of rows in the rendered track.
    pub track_len: usize,
    /// First thumb row, relative to the track origin.
    pub thumb_start: usize,
    /// Number of rows occupied by the thumb.
    pub thumb_len: usize,
    /// Largest valid logical scroll offset.
    pub max_offset: usize,
}

/// Meaning of a pointer press on a scrollbar track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollbarPress {
    PageUp,
    Thumb { grab_offset: usize },
    PageDown,
}

impl ScrollbarMetrics {
    /// Build metrics for `total` logical rows, `visible` viewport rows,
    /// `offset` current first row, and a rendered track of `track_len` rows.
    /// Returns `None` when no scrollbar is necessary or renderable.
    pub fn new(total: usize, visible: usize, offset: usize, track_len: usize) -> Option<Self> {
        if track_len == 0 || visible == 0 || total <= visible {
            return None;
        }

        let max_offset = total.saturating_sub(visible);
        // `usize * usize` always fits in `u128`, even on 64-bit targets.
        // Use the wider type so geometry remains exact instead of silently
        // saturating for very large virtual result sets.
        let thumb_len = (((visible as u128) * (track_len as u128)
            + (total.saturating_sub(1) as u128))
            / (total as u128)) as usize;
        let thumb_len = thumb_len.clamp(1, track_len);
        let travel = track_len.saturating_sub(thumb_len);
        let clamped_offset = offset.min(max_offset);
        let thumb_start = if travel == 0 || max_offset == 0 {
            0
        } else {
            // Round to nearest rather than floor so the final logical offset is
            // guaranteed to reach the final track cell.
            (((clamped_offset as u128) * (travel as u128)
                + ((max_offset / 2) as u128))
                / (max_offset as u128)) as usize
        };

        Some(Self {
            track_len,
            thumb_start: thumb_start.min(travel),
            thumb_len,
            max_offset,
        })
    }

    pub fn thumb_end(self) -> usize {
        self.thumb_start.saturating_add(self.thumb_len)
    }

    pub fn contains_thumb_row(self, row: usize) -> bool {
        row >= self.thumb_start && row < self.thumb_end()
    }

    pub fn press(self, row: usize) -> ScrollbarPress {
        let row = row.min(self.track_len.saturating_sub(1));
        if row < self.thumb_start {
            ScrollbarPress::PageUp
        } else if self.contains_thumb_row(row) {
            ScrollbarPress::Thumb {
                grab_offset: row.saturating_sub(self.thumb_start),
            }
        } else {
            ScrollbarPress::PageDown
        }
    }

    /// Convert a dragged pointer row into a logical offset while preserving the
    /// row inside the thumb that was originally grabbed.
    pub fn offset_for_drag(self, pointer_row: usize, grab_offset: usize) -> usize {
        let travel = self.track_len.saturating_sub(self.thumb_len);
        if travel == 0 || self.max_offset == 0 {
            return 0;
        }
        let start = pointer_row
            .saturating_sub(grab_offset.min(self.thumb_len.saturating_sub(1)))
            .min(travel);
        (((start as u128) * (self.max_offset as u128)
            + ((travel / 2) as u128))
            / (travel as u128)) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_when_content_fits() {
        assert_eq!(ScrollbarMetrics::new(10, 10, 0, 8), None);
        assert_eq!(ScrollbarMetrics::new(9, 10, 0, 8), None);
    }

    #[test]
    fn thumb_has_minimum_one_row_and_reaches_both_ends() {
        let top = ScrollbarMetrics::new(10_000, 1, 0, 7).expect("metrics");
        let bottom = ScrollbarMetrics::new(10_000, 1, 9_999, 7).expect("metrics");
        assert_eq!(top.thumb_len, 1);
        assert_eq!(top.thumb_start, 0);
        assert_eq!(bottom.thumb_start + bottom.thumb_len, 7);
    }

    #[test]
    fn thumb_size_tracks_visible_fraction_with_ceiling() {
        let metrics = ScrollbarMetrics::new(100, 25, 0, 10).expect("metrics");
        assert_eq!(metrics.thumb_len, 3);
    }

    #[test]
    fn track_press_classifies_page_and_thumb_regions() {
        let metrics = ScrollbarMetrics::new(100, 20, 40, 10).expect("metrics");
        assert_eq!(metrics.press(0), ScrollbarPress::PageUp);
        assert!(matches!(metrics.press(metrics.thumb_start), ScrollbarPress::Thumb { .. }));
        assert_eq!(metrics.press(9), ScrollbarPress::PageDown);
    }

    #[test]
    fn drag_mapping_reaches_exact_logical_endpoints() {
        let metrics = ScrollbarMetrics::new(100, 20, 0, 10).expect("metrics");
        assert_eq!(metrics.offset_for_drag(0, 0), 0);
        assert_eq!(metrics.offset_for_drag(9, metrics.thumb_len - 1), 80);
    }
    #[test]
    fn geometry_remains_exact_near_usize_limits() {
        let total = usize::MAX;
        let visible = total / 2;
        let metrics = ScrollbarMetrics::new(total, visible, total - visible, 9)
            .expect("metrics");
        assert_eq!(metrics.thumb_len, 5);
        assert_eq!(metrics.thumb_start + metrics.thumb_len, 9);
        assert_eq!(
            metrics.offset_for_drag(8, metrics.thumb_len - 1),
            total - visible
        );
    }

}
