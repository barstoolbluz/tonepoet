//! Coarse elapsed-time formatting for progress messages.

use std::time::Duration;

/// Elapsed time shorter than this is omitted from progress messages.
pub const DEFAULT_ELAPSED_THRESHOLD: Duration = Duration::from_secs(5);

/// Format elapsed time with coarse user-facing precision.
///
/// Examples: `48s`, `2m 14s`, `1h 03m`.
pub fn format_elapsed(elapsed: Duration) -> String {
    let total_secs = elapsed.as_secs();
    if total_secs < 60 {
        return format!("{total_secs}s");
    }

    let total_minutes = total_secs / 60;
    let seconds = total_secs % 60;
    if total_minutes < 60 {
        return format!("{total_minutes}m {seconds:02}s");
    }

    let hours = total_minutes / 60;
    let minutes = total_minutes % 60;
    format!("{hours}h {minutes:02}m")
}

/// Append elapsed time when `elapsed` is at least `threshold`.
pub fn append_elapsed(message: &str, elapsed: Duration, threshold: Duration) -> String {
    if elapsed < threshold {
        message.to_string()
    } else {
        format!("{} · elapsed {}", message, format_elapsed(elapsed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_seconds_minutes_and_hours_coarsely() {
        assert_eq!(format_elapsed(Duration::from_secs(48)), "48s");
        assert_eq!(format_elapsed(Duration::from_secs(134)), "2m 14s");
        assert_eq!(format_elapsed(Duration::from_secs(3_780)), "1h 03m");
    }

    #[test]
    fn appends_elapsed_only_after_threshold() {
        assert_eq!(
            append_elapsed(
                "Converting track 1 of 2",
                Duration::from_secs(4),
                DEFAULT_ELAPSED_THRESHOLD,
            ),
            "Converting track 1 of 2"
        );
        assert_eq!(
            append_elapsed(
                "Converting track 1 of 2",
                Duration::from_secs(48),
                DEFAULT_ELAPSED_THRESHOLD,
            ),
            "Converting track 1 of 2 · elapsed 48s"
        );
    }
}
