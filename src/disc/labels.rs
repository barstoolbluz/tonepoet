use super::model::{AudioPresentationFormat, DiscFormat};

/// Derive a human-readable channel layout label from a DVD-Audio channel
/// assignment code (0-20). Falls back to "{N}ch" for unrecognized codes.
pub fn channel_layout_label(code: u8, total_channels: u8) -> String {
    if let Some(ca) = crate::tui::dvda::channel_assignment(code) {
        let total = ca.group1_channels + ca.group2_channels;
        let has_lfe = ca.group2.contains(&"LFE");
        if total == 1 {
            "Mono".to_string()
        } else if total == 2 && !has_lfe {
            "Stereo".to_string()
        } else if has_lfe {
            format!("{}.1", total - 1)
        } else {
            format!("{}.0", total)
        }
    } else {
        format!("{}ch", total_channels)
    }
}

/// Format a sample rate as a human-readable string (e.g. "96kHz", "44.1kHz").
pub fn format_rate(rate: u32) -> String {
    if rate % 1000 == 0 {
        format!("{}kHz", rate / 1000)
    } else {
        let khz = rate as f64 / 1000.0;
        format!("{}kHz", khz)
    }
}

/// Build a canonical presentation label from structured audio format fields.
/// Example: "MLP 96kHz/24-bit 5.0" or "DSD64 Stereo".
pub fn presentation_label(format: &AudioPresentationFormat) -> String {
    let codec = format.codec.as_deref().unwrap_or("");
    let rate = format
        .sample_rate
        .map(|r| format_rate(r))
        .unwrap_or_else(|| "Unknown".to_string());
    let depth = format
        .bit_depth
        .map(|d| format!("{}-bit", d))
        .unwrap_or_else(|| "Unknown".to_string());
    let layout = format
        .channel_layout
        .as_deref()
        .unwrap_or("Unknown");

    if codec.is_empty() {
        format!("{}/{} {}", rate, depth, layout)
    } else {
        format!("{} {}/{} {}", codec, rate, depth, layout)
    }
}

/// Build a disc label using the fallback chain from the guidance document:
/// sidecar/album title → non-empty provider_id → file stem → generic format label.
pub fn disc_label(
    album_title: Option<&str>,
    provider_id: &str,
    file_stem: &str,
    format: DiscFormat,
) -> String {
    if let Some(title) = album_title {
        if !title.is_empty() {
            return title.to_string();
        }
    }
    if !provider_id.is_empty() {
        return provider_id.to_string();
    }
    if !file_stem.is_empty() {
        return file_stem.to_string();
    }
    format!("{} Disc", format.name())
}

/// Format a duration in seconds as "M:SS".
pub fn format_duration(secs: f64) -> String {
    let total = secs.round() as u64;
    let m = total / 60;
    let s = total % 60;
    format!("{}:{:02}", m, s)
}
