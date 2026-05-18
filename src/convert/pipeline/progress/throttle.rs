//! Source-side progress throttling.

use std::time::{Duration, Instant};

/// Minimum progress delta that bypasses the interval throttle.
pub const DEFAULT_MIN_PROGRESS_DELTA: f32 = 0.005;

/// Minimum interval for repeated progress updates with the same material key
/// and less than the minimum progress delta.
pub const DEFAULT_MIN_INTERVAL: Duration = Duration::from_millis(500);

/// Coalesces high-frequency operation progress near the source.
///
/// The `material_key` is intentionally separate from the user-facing message.
/// Future tool probes can keep volatile counters, byte counts, speeds, and
/// timestamps out of the key so those changing strings do not bypass the
/// throttle just because their display text differs byte-for-byte.
#[derive(Debug, Clone)]
pub struct ProgressThrottle {
    min_progress_delta: f32,
    min_interval: Duration,
    last_progress: Option<f32>,
    last_material_key: Option<String>,
    last_sent_at: Option<Instant>,
}

impl Default for ProgressThrottle {
    fn default() -> Self {
        Self::new(DEFAULT_MIN_PROGRESS_DELTA, DEFAULT_MIN_INTERVAL)
    }
}

impl ProgressThrottle {
    pub fn new(min_progress_delta: f32, min_interval: Duration) -> Self {
        Self {
            min_progress_delta,
            min_interval,
            last_progress: None,
            last_material_key: None,
            last_sent_at: None,
        }
    }

    /// Return true when an update should pass through.
    ///
    /// This method does not allocate on rejected updates. It stores the
    /// material key only after an update has been accepted for emission.
    pub fn should_send(
        &mut self,
        progress: f32,
        material_key: &str,
        force: bool,
        now: Instant,
    ) -> bool {
        let progress = progress.clamp(0.0, 1.0);

        let should_send = force
            || self.last_progress.is_none()
            || self.last_material_key.as_deref() != Some(material_key)
            || self
                .last_progress
                .map(|last| progress - last >= self.min_progress_delta)
                .unwrap_or(true)
            || self
                .last_sent_at
                .map(|last| now.duration_since(last) >= self.min_interval)
                .unwrap_or(true);

        if should_send {
            self.last_progress = Some(progress);
            self.last_material_key = Some(material_key.to_string());
            self.last_sent_at = Some(now);
        }

        should_send
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coalesces_small_same_key_updates() {
        let mut throttle = ProgressThrottle::default();
        let now = Instant::now();

        assert!(throttle.should_send(0.100, "convert-track", false, now));
        assert!(!throttle.should_send(
            0.102,
            "convert-track",
            false,
            now + Duration::from_millis(100),
        ));
    }

    #[test]
    fn material_key_change_sends_immediately() {
        let mut throttle = ProgressThrottle::default();
        let now = Instant::now();

        assert!(throttle.should_send(0.100, "track-1", false, now));
        assert!(throttle.should_send(
            0.101,
            "track-2",
            false,
            now + Duration::from_millis(100),
        ));
    }

    #[test]
    fn volatile_display_text_can_share_stable_key() {
        let mut throttle = ProgressThrottle::default();
        let now = Instant::now();

        assert!(throttle.should_send(0.100, "ffmpeg-track-1", false, now));
        assert!(!throttle.should_send(
            0.101,
            "ffmpeg-track-1",
            false,
            now + Duration::from_millis(100),
        ));
    }

    #[test]
    fn progress_delta_at_threshold_sends_immediately() {
        let mut throttle = ProgressThrottle::default();
        let now = Instant::now();

        assert!(throttle.should_send(0.000, "working", false, now));
        assert!(throttle.should_send(
            DEFAULT_MIN_PROGRESS_DELTA,
            "working",
            false,
            now + Duration::from_millis(100),
        ));
    }

    #[test]
    fn progress_delta_below_threshold_is_suppressed() {
        let mut throttle = ProgressThrottle::default();
        let now = Instant::now();

        assert!(throttle.should_send(0.000, "working", false, now));
        assert!(!throttle.should_send(
            DEFAULT_MIN_PROGRESS_DELTA - 0.000_1,
            "working",
            false,
            now + Duration::from_millis(100),
        ));
    }

    #[test]
    fn progress_delta_sends_immediately() {
        let mut throttle = ProgressThrottle::default();
        let now = Instant::now();

        assert!(throttle.should_send(0.100, "working", false, now));
        assert!(throttle.should_send(
            0.106,
            "working",
            false,
            now + Duration::from_millis(100),
        ));
    }

    #[test]
    fn interval_sends_repeated_progress() {
        let mut throttle = ProgressThrottle::default();
        let now = Instant::now();

        assert!(throttle.should_send(0.100, "working", false, now));
        assert!(throttle.should_send(
            0.101,
            "working",
            false,
            now + Duration::from_millis(501),
        ));
    }

    #[test]
    fn force_bypasses_throttle() {
        let mut throttle = ProgressThrottle::default();
        let now = Instant::now();

        assert!(throttle.should_send(0.100, "working", false, now));
        assert!(throttle.should_send(
            0.100,
            "working",
            true,
            now + Duration::from_millis(1),
        ));
    }
}
