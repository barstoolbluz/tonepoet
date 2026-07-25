//! Shared click timing used by both the reusable picker and Browse view.

use std::time::{Duration, Instant};

pub const DOUBLE_CLICK_WINDOW: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClickDisposition {
    First,
    Double,
    DelayedRepeat,
}

/// Classify a click against an optional prior click on the same target type.
pub fn classify_click<T: Copy + Eq>(
    last: Option<(T, Instant)>,
    target: T,
    now: Instant,
    window: Duration,
) -> ClickDisposition {
    match last {
        Some((last_target, last_at)) if last_target == target => {
            if now.saturating_duration_since(last_at) <= window {
                ClickDisposition::Double
            } else {
                ClickDisposition::DelayedRepeat
            }
        }
        _ => ClickDisposition::First,
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ClickTracker<T> {
    last: Option<(T, Instant)>,
    window: Duration,
}

impl<T> Default for ClickTracker<T> {
    fn default() -> Self {
        Self { last: None, window: DOUBLE_CLICK_WINDOW }
    }
}

impl<T: Copy + Eq> ClickTracker<T> {
    pub fn classify(&mut self, target: T, now: Instant) -> ClickDisposition {
        let disposition = classify_click(self.last, target, now, self.window);
        self.last = Some((target, now));
        disposition
    }

    pub fn clear(&mut self) {
        self.last = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_double_and_delayed_repeat_at_shared_boundary() {
        let start = Instant::now();
        let mut tracker = ClickTracker::<u8>::default();

        assert_eq!(tracker.classify(7, start), ClickDisposition::First);
        assert_eq!(
            tracker.classify(7, start + DOUBLE_CLICK_WINDOW),
            ClickDisposition::Double
        );
        assert_eq!(
            tracker.classify(7, start + DOUBLE_CLICK_WINDOW + DOUBLE_CLICK_WINDOW + Duration::from_millis(1)),
            ClickDisposition::DelayedRepeat
        );
        assert_eq!(
            tracker.classify(8, start + DOUBLE_CLICK_WINDOW + DOUBLE_CLICK_WINDOW + Duration::from_millis(2)),
            ClickDisposition::First
        );
    }
}
