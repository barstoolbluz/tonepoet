//! sox progress parsing.
//!
//! `sox -S` output is not as stable as ffmpeg's status output across versions,
//! so this parser is intentionally conservative. It accepts percentage-bearing
//! progress lines only when they match known sox status prefixes. Warning text
//! containing a percent sign must not become measured progress.

use crate::convert::pipeline::progress::streaming::ProbeUpdate;

fn is_sox_status_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("In:") || trimmed.starts_with("Out:")
}

pub fn parse_percent(line: &str) -> Option<f32> {
    if !is_sox_status_line(line) {
        return None;
    }
    let percent_index = line.find('%')?;
    let before = &line[..percent_index];
    let start = before
        .rfind(|ch: char| !(ch.is_ascii_digit() || ch == '.'))
        .map(|idx| idx + 1)
        .unwrap_or(0);
    let number = before[start..].trim();
    if number.is_empty() {
        return None;
    }
    let value: f32 = number.parse().ok()?;
    Some((value / 100.0).clamp(0.0, 1.0))
}

pub fn parse_line(
    line: &str,
    start_fraction: f32,
    end_fraction: f32,
    label: &str,
) -> Option<ProbeUpdate> {
    let tool_progress = parse_percent(line)?;
    let phase_progress = start_fraction + (end_fraction - start_fraction).max(0.0) * tool_progress;
    let pct = (tool_progress * 100.0).round() as u32;
    Some(ProbeUpdate::measured(
        phase_progress,
        "sox-progress".to_string(),
        format!("{label} · {pct}% of current track"),
    ))
}

pub fn unknown_fallback_message(label: &str) -> String {
    format!("{label} · progress unavailable from sox · still running")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_sox_progress_prefixes() {
        assert!((parse_percent("In:25.0% 00:00:15.00").unwrap() - 0.25).abs() < 0.001);
        assert!((parse_percent("Out: 100.0% 00:03:15.00").unwrap() - 1.0).abs() < 0.001);
    }

    #[test]
    fn warning_percentages_do_not_parse_as_progress() {
        assert!(parse_percent("sox WARN rate: clipping affected 25% of samples").is_none());
        assert!(parse_percent("Done: 100%").is_none());
    }

    #[test]
    fn unparseable_line_returns_none() {
        assert!(parse_percent("sox WARN rate: rate clipped 1 samples").is_none());
        assert!(parse_line("sox progress unavailable", 0.0, 1.0, "Converting").is_none());
    }

    #[test]
    fn maps_sox_percentage_into_phase_window() {
        let update = parse_line("In:50.0%", 0.20, 0.80, "Converting track 1 of 1").unwrap();
        assert!((update.progress() - 0.50).abs() < 0.001);
        assert!(update.message().contains("50% of current track"));
    }
}
