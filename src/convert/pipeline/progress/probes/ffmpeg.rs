//! ffmpeg stderr progress parsing.
//!
//! ffmpeg commonly writes status lines containing `time=HH:MM:SS.xx` to
//! stderr. This parser extracts that timestamp and maps it into a caller-owned
//! phase-progress window.

use std::time::Duration;

use crate::convert::pipeline::progress::streaming::ProbeUpdate;

pub fn parse_time_token(line: &str) -> Option<Duration> {
    let (_, rest) = line.split_once("time=")?;
    let token = rest.split_whitespace().next()?.trim();
    parse_hms(token)
}

pub fn parse_line(
    line: &str,
    expected_duration: Duration,
    start_fraction: f32,
    end_fraction: f32,
    label: &str,
) -> Option<ProbeUpdate> {
    if expected_duration.is_zero() {
        return None;
    }
    let elapsed = parse_time_token(line)?;
    let raw = elapsed.as_secs_f64() / expected_duration.as_secs_f64();
    let track_progress = raw.clamp(0.0, 1.0) as f32;
    let phase_progress = start_fraction + (end_fraction - start_fraction).max(0.0) * track_progress;
    let pct = (track_progress * 100.0).round() as u32;
    Some(ProbeUpdate::measured(
        phase_progress,
        "ffmpeg-progress".to_string(),
        format!("{label} · {pct}% of current track"),
    ))
}

fn parse_hms(token: &str) -> Option<Duration> {
    let mut parts = token.split(':');
    let hours: u64 = parts.next()?.parse().ok()?;
    let minutes: u64 = parts.next()?.parse().ok()?;
    let seconds_text = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    let seconds: f64 = seconds_text.parse().ok()?;
    if !(0.0..60.0).contains(&seconds) || minutes >= 60 {
        return None;
    }
    let total = hours as f64 * 3600.0 + minutes as f64 * 60.0 + seconds;
    Some(Duration::from_secs_f64(total))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_normal_ffmpeg_time_line() {
        let parsed = parse_time_token("size=1kB time=00:02:14.56 bitrate=10kbits/s").unwrap();
        assert_eq!(parsed.as_secs(), 134);
        assert_eq!(parsed.subsec_millis(), 560);
    }

    #[test]
    fn malformed_ffmpeg_line_returns_none() {
        assert!(parse_time_token("no timestamp here").is_none());
        assert!(parse_time_token("time=bogus").is_none());
        assert!(parse_time_token("time=00:65:00.00").is_none());
    }

    #[test]
    fn maps_time_into_phase_window() {
        let update = parse_line(
            "frame=1 time=00:00:05.00 speed=1x",
            Duration::from_secs(10),
            0.20,
            0.80,
            "Converting track 1 of 1",
        )
        .unwrap();
        assert!((update.progress() - 0.50).abs() < 0.001);
        assert!(update.message().contains("50% of current track"));
    }
}
