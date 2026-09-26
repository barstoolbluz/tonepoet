//! Generate foobar2000-style DR analysis reports.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::analyze::AnalysisResult;

/// Generate DR reports grouped by physical source parent directory.
/// Returns a Vec of (directory, report_text) pairs.
pub fn format_dr_reports(results: &[AnalysisResult]) -> Vec<(PathBuf, String)> {
    if results.is_empty() {
        return Vec::new();
    }

    let mut groups: BTreeMap<PathBuf, Vec<&AnalysisResult>> = BTreeMap::new();
    for result in results {
        let dir = result
            .source_path
            .parent()
            .unwrap_or(Path::new("."))
            .to_path_buf();
        groups.entry(dir).or_default().push(result);
    }

    groups
        .into_iter()
        .map(|(dir, group)| (dir, format_one_report(&group)))
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PhysicalSourceFacts {
    sample_rate: Option<u32>,
    channels: Option<u32>,
    bit_depth: Option<u32>,
    codec: String,
}

fn format_one_report(results: &[&AnalysisResult]) -> String {
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");

    // Album metadata belongs to the physical carrier, not a synthetic CUE row.
    let (artist, album) = read_artist_album(&results[0].source_path);
    let analyzed_label = match (artist.as_deref(), album.as_deref()) {
        (Some(a), Some(b)) => format!("{} / {}", a, b),
        (Some(a), None) => a.to_string(),
        (None, Some(b)) => b.to_string(),
        (None, None) => results[0]
            .source_path
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "Unknown".to_string()),
    };

    let num_tracks = results.len();
    let source_facts = physical_source_facts(results);
    let sample_rate = summarize_optional_u32(
        source_facts.iter().map(|facts| facts.sample_rate),
        " Hz",
        "unknown",
    );
    let channels = summarize_optional_u32(
        source_facts.iter().map(|facts| facts.channels),
        "",
        "unknown",
    );
    let bit_depth = summarize_optional_u32(
        source_facts.iter().map(|facts| facts.bit_depth),
        "",
        "unknown / not applicable",
    );
    let codec = summarize_strings(source_facts.iter().map(|facts| facts.codec.as_str()));

    // A single-image CUE contributes its carrier bytes once, not once per row.
    let source_paths: BTreeSet<&Path> = results.iter().map(|r| r.source_path.as_path()).collect();
    let total_size: u64 = source_paths
        .iter()
        .filter_map(|path| std::fs::metadata(path).ok().map(|metadata| metadata.len()))
        .sum();
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
    out.push_str("tonepoet / Dynamic Range Meter\n");
    out.push_str(&format!("log date: {}\n", now));
    out.push('\n');
    out.push_str(&separator);
    out.push('\n');
    out.push_str(&format!("Analyzed: {}\n", analyzed_label));
    out.push_str(&separator);
    out.push('\n');
    out.push('\n');
    out.push_str("DR         Peak         RMS     Duration Track\n");
    out.push_str(&separator);
    out.push('\n');

    for result in results {
        let label = display_track_label(result);
        let mins = result.duration_secs as u64 / 60;
        let secs = result.duration_secs as u64 % 60;

        out.push_str(&format!(
            "DR{:<3}  {:>8.2} dB  {:>8.2} dB  {:>4}:{:02} {}\n",
            result.dr_value, result.peak_db, result.rms_db, mins, secs, label,
        ));
    }

    out.push_str(&separator);
    out.push('\n');
    out.push('\n');
    out.push_str(&format!("Number of tracks:  {}\n", num_tracks));
    out.push_str(&format!("Official DR value: DR{}\n", official_dr));
    out.push('\n');
    out.push_str(&format!("Samplerate:        {}\n", sample_rate));
    out.push_str(&format!("Channels:          {}\n", channels));
    out.push_str(&format!("Bits per sample:   {}\n", bit_depth));
    out.push_str(&format!("Bitrate:           {} kbps\n", avg_bitrate));
    out.push_str(&format!("Codec:             {}\n", codec));
    out.push_str(&double_sep);
    out.push('\n');

    out
}

fn display_track_label(result: &AnalysisResult) -> String {
    let display = super::analyze::display_name(result);
    if result.path != result.source_path {
        return display
            .strip_suffix(".flac")
            .unwrap_or(display.as_str())
            .to_string();
    }
    Path::new(&display)
        .file_stem()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or(display)
}

fn physical_source_facts(results: &[&AnalysisResult]) -> Vec<PhysicalSourceFacts> {
    let mut by_path: BTreeMap<&Path, &AnalysisResult> = BTreeMap::new();
    for result in results {
        by_path.entry(result.source_path.as_path()).or_insert(result);
    }

    by_path
        .into_iter()
        .map(|(path, fallback)| match super::probe::probe_audio(path) {
            Ok(info) => PhysicalSourceFacts {
                sample_rate: Some(info.sample_rate),
                channels: Some(info.channels),
                bit_depth: reportable_bit_depth(&info),
                codec: if info.codec.trim().is_empty() {
                    "unknown".to_string()
                } else {
                    info.codec
                },
            },
            Err(_) => PhysicalSourceFacts {
                sample_rate: Some(fallback.sample_rate),
                channels: Some(fallback.channels),
                bit_depth: super::analyze::known_bit_depth(fallback),
                codec: "unknown".to_string(),
            },
        })
        .collect()
}

fn reportable_bit_depth(info: &super::probe::SourceInfo) -> Option<u32> {
    if info.sample_format_is_float == Some(true) || info.compression_is_lossless == Some(false) {
        return None;
    }
    let combined = format!("{} {}", info.format_name, info.codec).to_ascii_lowercase();
    if ["aac", "mp3", "opus", "vorbis", "ac-3", "e-ac-3"]
        .iter()
        .any(|needle| combined.contains(needle))
    {
        return None;
    }
    info.bit_depth.filter(|depth| *depth > 0)
}

fn summarize_optional_u32(
    values: impl IntoIterator<Item = Option<u32>>,
    suffix: &str,
    none_label: &str,
) -> String {
    let values = values.into_iter().collect::<Vec<_>>();
    let Some(first) = values.first().copied() else {
        return none_label.to_string();
    };
    if values.iter().all(|value| *value == first) {
        return first
            .map(|value| format!("{}{}", value, suffix))
            .unwrap_or_else(|| none_label.to_string());
    }
    "mixed".to_string()
}

fn summarize_strings<'a>(values: impl IntoIterator<Item = &'a str>) -> String {
    let values = values.into_iter().collect::<Vec<_>>();
    let Some(first) = values.first() else {
        return "unknown".to_string();
    };
    if values.iter().all(|value| *value == *first) {
        (*first).to_string()
    } else {
        "mixed".to_string()
    }
}

/// Read artist and album tags from a physical source using lofty.
fn read_artist_album(path: &Path) -> (Option<String>, Option<String>) {
    match super::probe::read_metadata(path) {
        Ok(meta) => (meta.artist, meta.album),
        Err(_) => (None, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(source: PathBuf, display: PathBuf, sample_rate: u32, channels: u32, bit_depth: u32) -> AnalysisResult {
        AnalysisResult {
            source_path: source,
            path: display,
            flac_wrappers: None,
            flac_wrapper_repair_path: None,
            dr_value: 10,
            peak_db: -1.0,
            rms_db: -12.0,
            clipping_count: 0,
            dc_bias: 0.0,
            actual_bit_depth: bit_depth,
            declared_bit_depth: if bit_depth > 0 { Some(bit_depth) } else { None },
            sample_rate,
            channels,
            duration_secs: 1.0,
            lufs: None,
            true_peak_dbtp: None,
            loudness_status: crate::tui::analyze::LoudnessAnalysisStatus::NotScanned,
            preemphasis: None,
            preemphasis_corr: None,
            preemphasis_detail: None,
            hdcd_detected: None,
            hdcd_detail: None,
        }
    }

    #[test]
    fn shared_cue_carrier_is_grouped_and_sized_once_while_labels_remain_track_scoped() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("album.flac");
        std::fs::write(&source, vec![0u8; 1_000]).expect("physical carrier");
        let first = result(
            source.clone(),
            temp.path().join("01 - AC/DC.flac"),
            44_100,
            2,
            16,
        );
        let second = result(
            source.clone(),
            temp.path().join("02 - Track Two.flac"),
            44_100,
            2,
            16,
        );

        let reports = format_dr_reports(&[first, second]);
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].0, temp.path());
        let report = &reports[0].1;
        assert!(report.contains("01 - AC/DC"));
        assert!(report.contains("02 - Track Two"));
        assert!(report.contains("Bitrate:           4 kbps"));
        assert!(report.contains("Samplerate:        44100 Hz"));
        assert!(report.contains("Bits per sample:   16"));
    }

    #[test]
    fn report_property_summaries_do_not_copy_first_row_when_sources_differ() {
        assert_eq!(summarize_optional_u32([Some(44_100), Some(48_000)], " Hz", "unknown"), "mixed");
        assert_eq!(summarize_optional_u32([Some(2), Some(6)], "", "unknown"), "mixed");
        assert_eq!(summarize_optional_u32([Some(16), None], "", "unknown / not applicable"), "mixed");
        assert_eq!(summarize_optional_u32([None, None], "", "unknown / not applicable"), "unknown / not applicable");
        assert_eq!(summarize_strings(["ALAC", "AAC"]), "mixed");
        assert_eq!(summarize_strings(["ALAC", "ALAC"]), "ALAC");
    }

    #[test]
    fn lossy_or_float_source_depth_is_not_reported_as_integer_bits() {
        let aac = super::super::probe::SourceInfo {
            format_name: "M4A".to_string(),
            codec: "AAC".to_string(),
            bit_depth: Some(32),
            sample_format_is_float: None,
            compression_is_lossless: None,
            sample_rate: 48_000,
            channels: 2,
            channel_layout: "stereo".to_string(),
            duration_secs: 1.0,
            file_size: 1_000,
        };
        assert_eq!(reportable_bit_depth(&aac), None);

        let float_pcm = super::super::probe::SourceInfo {
            format_name: "WAV".to_string(),
            codec: "PCM f32le".to_string(),
            bit_depth: Some(32),
            sample_format_is_float: Some(true),
            compression_is_lossless: None,
            sample_rate: 48_000,
            channels: 2,
            channel_layout: "stereo".to_string(),
            duration_secs: 1.0,
            file_size: 1_000,
        };
        assert_eq!(reportable_bit_depth(&float_pcm), None);
    }
}
