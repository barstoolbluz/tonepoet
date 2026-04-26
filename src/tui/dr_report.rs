//! Generate foobar2000-style DR analysis reports.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::analyze::AnalysisResult;

/// Generate DR reports grouped by parent directory.
/// Returns a Vec of (directory, report_text) pairs.
pub fn format_dr_reports(results: &[AnalysisResult]) -> Vec<(PathBuf, String)> {
    if results.is_empty() {
        return Vec::new();
    }

    // Group results by parent directory.
    let mut groups: BTreeMap<PathBuf, Vec<&AnalysisResult>> = BTreeMap::new();
    for r in results {
        let dir = r.path.parent().unwrap_or(Path::new(".")).to_path_buf();
        groups.entry(dir).or_default().push(r);
    }

    let mut reports = Vec::new();

    for (dir, group) in &groups {
        let report = format_one_report(group);
        reports.push((dir.clone(), report));
    }

    reports
}

fn format_one_report(results: &[&AnalysisResult]) -> String {
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");

    // Read artist/album from the first file's tags.
    let (artist, album) = read_artist_album(&results[0].path);
    let analyzed_label = match (artist.as_deref(), album.as_deref()) {
        (Some(a), Some(b)) => format!("{} / {}", a, b),
        (Some(a), None) => a.to_string(),
        (None, Some(b)) => b.to_string(),
        (None, None) => results[0].path.parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "Unknown".to_string()),
    };

    // Aggregate stats from all tracks.
    let num_tracks = results.len();
    let sample_rate = results[0].sample_rate;
    let channels = results[0].channels;
    let bit_depth = results[0].actual_bit_depth;
    let codec = detect_codec(&results[0].path);

    // Compute average bitrate across all tracks.
    let total_size: u64 = results.iter().filter_map(|r| {
        std::fs::metadata(&r.path).ok().map(|m| m.len())
    }).sum();
    let total_duration: f64 = results.iter().map(|r| r.duration_secs).sum();
    let avg_bitrate = if total_duration > 0.0 {
        ((total_size as f64 * 8.0) / total_duration / 1000.0).round() as u64
    } else {
        0
    };

    // Official DR = round(mean of all track DRs).
    let dr_sum: f64 = results.iter().map(|r| r.dr_value as f64).sum();
    let official_dr = (dr_sum / num_tracks as f64).round() as i32;

    let separator = "-".repeat(80);
    let double_sep = "=".repeat(80);

    let mut out = String::new();

    // Header
    out.push_str(&format!("tonepoet / Dynamic Range Meter\n"));
    out.push_str(&format!("log date: {}\n", now));
    out.push('\n');
    out.push_str(&separator);
    out.push('\n');
    out.push_str(&format!("Analyzed: {}\n", analyzed_label));
    out.push_str(&separator);
    out.push('\n');
    out.push('\n');

    // Column headers
    out.push_str("DR         Peak         RMS     Duration Track\n");
    out.push_str(&separator);
    out.push('\n');

    // Per-track rows
    for r in results {
        let filename = r.path.file_stem()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let mins = r.duration_secs as u64 / 60;
        let secs = r.duration_secs as u64 % 60;

        out.push_str(&format!(
            "DR{:<3}  {:>8.2} dB  {:>8.2} dB  {:>4}:{:02} {}\n",
            r.dr_value, r.peak_db, r.rms_db, mins, secs, filename,
        ));
    }

    // Footer
    out.push_str(&separator);
    out.push('\n');
    out.push('\n');
    out.push_str(&format!("Number of tracks:  {}\n", num_tracks));
    out.push_str(&format!("Official DR value: DR{}\n", official_dr));
    out.push('\n');
    out.push_str(&format!("Samplerate:        {} Hz\n", sample_rate));
    out.push_str(&format!("Channels:          {}\n", channels));
    out.push_str(&format!("Bits per sample:   {}\n", bit_depth));
    out.push_str(&format!("Bitrate:           {} kbps\n", avg_bitrate));
    out.push_str(&format!("Codec:             {}\n", codec));
    out.push_str(&double_sep);
    out.push('\n');

    out
}

/// Read artist and album tags from a file using lofty.
fn read_artist_album(path: &Path) -> (Option<String>, Option<String>) {
    match super::probe::read_metadata(path) {
        Ok(meta) => (meta.artist, meta.album),
        Err(_) => (None, None),
    }
}

/// Detect codec name from file extension.
fn detect_codec(path: &Path) -> String {
    let ext = path.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "flac" => "FLAC".to_string(),
        "wav" => "WAV".to_string(),
        "aiff" | "aif" => "AIFF".to_string(),
        "wv" => "WavPack".to_string(),
        "mp3" => "MP3".to_string(),
        "m4a" | "aac" => "AAC".to_string(),
        "opus" => "Opus".to_string(),
        "ogg" => "Ogg Vorbis".to_string(),
        "dsf" => "DSF".to_string(),
        "dff" => "DFF".to_string(),
        _ => ext.to_uppercase(),
    }
}
