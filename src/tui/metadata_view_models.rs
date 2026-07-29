//! Typed view models for the metadata editor read-only tabs.
//!
//! This module owns the pure aggregation layer for Details, ReplayGain, and
//! Artwork. Rendering modules should format these structs, not derive business
//! facts from editor state directly.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::app::{
    DiscTechnicalDetails, FileReadState, FileWriteEligibility, MetadataDetailsProbeState,
    MetadataEditorState, MetadataFileDetails, MetadataIssue, ProbeState,
};
use super::probe::{ArtworkInfo, SourceInfo};

#[derive(Debug, Clone)]
pub struct DetailsViewModel {
    pub location: Vec<DetailField>,
    pub probe_status: Option<String>,
    pub analysis_status: Option<String>,
    pub read_issues: Vec<IssueViewRow>,
    pub general: Vec<DetailField>,
}

#[derive(Debug, Clone)]
pub struct DetailField {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone)]
pub struct ReplayGainViewModel {
    pub has_data: bool,
    pub summary: Vec<DetailField>,
    pub rows: Vec<ReplayGainRow>,
    pub scan_status: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ReplayGainRow {
    pub index: usize,
    pub title: String,
    pub track_gain: String,
    pub album_gain: String,
    pub track_peak: String,
    pub album_peak: String,
}

#[derive(Debug, Clone)]
pub struct ArtworkViewModel {
    pub disc_not_applicable: bool,
    pub read_issues: Vec<IssueViewRow>,
    pub rows: Vec<ArtworkCoverageRow>,
}

#[derive(Debug, Clone)]
pub struct ArtworkCoverageRow {
    pub index: usize,
    pub kind: String,
    pub status: String,
    pub detail: String,
    pub picture_type: lofty::picture::PictureType,
    pub selected: bool,
}

#[derive(Debug, Clone)]
pub struct IssueViewRow {
    pub label: String,
    pub kind: String,
    pub reason: String,
}

fn detail_field(key: impl Into<String>, value: impl Into<String>) -> DetailField {
    DetailField {
        key: key.into(),
        value: value.into(),
    }
}

fn sample_rate_detail(sample_rate: u32) -> String {
    let suffix = tonepoet_pipeline::DsdRate::from_hz(sample_rate)
        .map(|rate| match rate {
            tonepoet_pipeline::DsdRate::Dsd64 => "DSD64",
            tonepoet_pipeline::DsdRate::Dsd128 => "DSD128",
            tonepoet_pipeline::DsdRate::Dsd256 => "DSD256",
            tonepoet_pipeline::DsdRate::Dsd512 => "DSD512",
            tonepoet_pipeline::DsdRate::Dsd1024 => "DSD1024",
        });
    match suffix {
        Some(label) => format!("{sample_rate} Hz ({label})"),
        None => format!("{sample_rate} Hz"),
    }
}

pub fn build_details_view_model(state: &MetadataEditorState) -> DetailsViewModel {
    let display_files = metadata_details_files_for_display(state);
    let mut location = Vec::new();
    if let Some(disc) = state.active_surface().technical_details.disc.as_ref() {
        location.extend(metadata_disc_location_fields(state, disc));
    } else {
        location.push(detail_field("File names", metadata_file_names(&state.active_surface().paths)));
        location.push(detail_field("Folder name", metadata_folder_name(&state.active_surface().paths)));
    }
    location.push(detail_field("Total size", metadata_total_size_cached(display_files)));
    location.push(detail_field(
        "Last modified",
        metadata_last_modified_cached(display_files),
    ));
    if !display_files.is_empty() && state.active_surface().technical_details.disc.is_none() {
        location.push(detail_field(
            "Save eligibility",
            metadata_save_eligibility_summary(display_files),
        ));
    }

    let mut general = Vec::new();
    if let Some(disc) = state.active_surface().technical_details.disc.as_ref() {
        general.push(detail_field(
            "Presentation",
            if disc.presentation_label.trim().is_empty() {
                state
                    .active_presentation_label()
                    .unwrap_or("presentation")
                    .to_string()
            } else {
                disc.presentation_label.clone()
            },
        ));
        general.push(detail_field("Tracks", disc.track_count.max(state.active_surface().paths.len()).to_string()));
        general.push(detail_field(
            "Duration",
            disc.duration_secs
                .or_else(|| active_disc_duration_secs(state))
                .map(|duration| {
                    metadata_duration_with_samples(
                        duration,
                        sample_count_from_duration(duration, disc.sample_rate),
                    )
                })
                .unwrap_or_else(|| "—".to_string()),
        ));
        general.push(detail_field(
            "Sample rate",
            disc.sample_rate
                .map(sample_rate_detail)
                .unwrap_or_else(|| "—".to_string()),
        ));
        general.push(detail_field("Channels", disc_channels_value(disc)));
        general.push(detail_field(
            "Bits per sample",
            disc.bit_depth
                .map(|depth| depth.to_string())
                .unwrap_or_else(|| "—".to_string()),
        ));
        general.push(detail_field("Avg. bitrate", disc_average_bitrate(disc)));
        general.push(detail_field(
            "Codec",
            disc.codec
                .as_ref()
                .map(|codec| codec.trim().to_string())
                .filter(|codec| !codec.is_empty())
                .or_else(|| active_disc_entry_value(state, &["CODEC", "AUDIO_CODEC", "FORMAT"]))
                .unwrap_or_else(|| "—".to_string()),
        ));
        general.push(detail_field("Encoding", disc_encoding_value(disc)));
        general.push(detail_field("Tool", metadata_tool_value(state)));
        general.push(detail_field("Embedded cue sheet", embedded_cuesheet_status(state)));
    } else {
        let infos: Vec<SourceInfo> = display_files
            .iter()
            .filter_map(|file| match &file.media_facts {
                ProbeState::Ready(facts) => Some(facts.clone().into()),
                _ => None,
            })
            .collect();
        if infos.is_empty() {
            let message = if display_files
                .iter()
                .any(|file| matches!(file.media_facts, ProbeState::Failed { .. }))
            {
                "No successfully cached audio probe data is available; see Read issues above."
            } else if matches!(
                state.active_surface().technical_details.details_probe_state,
                MetadataDetailsProbeState::Loading { .. }
            ) {
                "Audio details are loading in the background."
            } else {
                "Technical probe data is not loaded yet."
            };
            general.push(detail_field("Probe data", message));
        } else {
            let info_refs: Vec<&SourceInfo> = infos.iter().collect();
            let duration: f64 = infos.iter().map(|info| info.duration_secs).sum();
            general.push(detail_field(
                "Duration",
                metadata_duration_with_samples(duration, metadata_total_sample_count(&info_refs)),
            ));
            general.push(detail_field(
                "Sample rate",
                same_or_multiple(infos.iter().map(|info| sample_rate_detail(info.sample_rate))),
            ));
            general.push(detail_field(
                "Channels",
                same_or_multiple(infos.iter().map(file_channels_value)),
            ));
            general.push(detail_field(
                "Bits per sample",
                same_or_multiple(infos.iter().map(|info| {
                    info.bit_depth
                        .map(|depth| depth.to_string())
                        .unwrap_or_else(|| "unknown".to_string())
                })),
            ));
            general.push(detail_field("Avg. bitrate", metadata_average_bitrate(&info_refs)));
            general.push(detail_field(
                "Codec",
                same_or_multiple(infos.iter().map(|info| info.codec.clone())),
            ));
            general.push(detail_field("Encoding", metadata_encoding_kind(&info_refs).to_string()));
        }
        general.push(detail_field("Tool", metadata_tool_value(state)));
        general.push(detail_field("Embedded cue sheet", embedded_cuesheet_status(state)));
    }

    if metadata_hdcd_is_applicable_to_active_surface(state) {
        general.push(detail_field("HDCD", metadata_hdcd_status(state)));
    }
    if metadata_preemphasis_is_applicable_to_active_surface(state) {
        general.push(detail_field("Pre-emphasis", metadata_preemphasis_status(state)));
    }

    DetailsViewModel {
        location,
        probe_status: metadata_details_probe_status_text(state),
        analysis_status: metadata_details_analysis_status_text(state),
        read_issues: metadata_file_issue_rows_for_files(state, display_files, true, true),
        general,
    }
}

pub fn replaygain_action_row_count(state: &MetadataEditorState) -> usize {
    let track_gain = metadata_entry_values(state, &["REPLAYGAIN_TRACK_GAIN", "R128_TRACK_GAIN"]);
    let album_gain_values = metadata_entry_values(state, &["REPLAYGAIN_ALBUM_GAIN", "R128_ALBUM_GAIN"]);
    let track_peak = metadata_entry_values(state, &["REPLAYGAIN_TRACK_PEAK"]);
    let album_peak = metadata_entry_values(state, &["REPLAYGAIN_ALBUM_PEAK"]);
    let titles = metadata_track_titles(state);
    replaygain_row_count_from_parts(
        state,
        titles.len(),
        track_gain.len(),
        album_gain_values.len(),
        track_peak.len(),
        album_peak.len(),
    )
}

fn replaygain_row_count_from_parts(
    state: &MetadataEditorState,
    title_count: usize,
    track_gain_count: usize,
    album_gain_count: usize,
    track_peak_count: usize,
    album_peak_count: usize,
) -> usize {
    title_count
        .max(track_gain_count)
        .max(album_gain_count)
        .max(track_peak_count)
        .max(album_peak_count)
        .max(state.active_surface().paths.len())
}

pub fn build_replaygain_view_model(state: &MetadataEditorState) -> ReplayGainViewModel {
    let track_gain = metadata_entry_values(state, &["REPLAYGAIN_TRACK_GAIN", "R128_TRACK_GAIN"]);
    let album_gain_values = metadata_entry_values(state, &["REPLAYGAIN_ALBUM_GAIN", "R128_ALBUM_GAIN"]);
    let track_peak = metadata_entry_values(state, &["REPLAYGAIN_TRACK_PEAK"]);
    let album_peak = metadata_entry_values(state, &["REPLAYGAIN_ALBUM_PEAK"]);

    let has_data = has_metadata_values(&track_gain)
        || has_metadata_values(&album_gain_values)
        || has_metadata_values(&track_peak)
        || has_metadata_values(&album_peak);
    let scan_status = state.replaygain_scan.as_ref().map(|scan| {
        format!(
            "Scanning {} ReplayGain for {} file{}...",
            scan.mode.label(),
            scan.file_count,
            if scan.file_count == 1 { "" } else { "s" }
        )
    });

    let gains: Vec<f64> = track_gain
        .iter()
        .filter_map(|value| parse_db_value(value))
        .collect();
    let summary = vec![
        detail_field(
            "Track Gain",
            if has_metadata_values(&track_gain) {
                summarize_values(&track_gain)
            } else {
                "«not scanned»".to_string()
            },
        ),
        detail_field(
            "Album Gain",
            if has_metadata_values(&album_gain_values) {
                summarize_values(&album_gain_values)
            } else {
                "«not scanned»".to_string()
            },
        ),
        detail_field("Total Peak", summarize_peak(&track_peak, &album_peak)),
        detail_field(
            "Loudest track",
            gains
                .iter()
                .copied()
                .reduce(f64::min)
                .map(|gain| format!("{:+.2} dB", gain))
                .unwrap_or_else(|| "—".to_string()),
        ),
        detail_field(
            "Quietest track",
            gains
                .iter()
                .copied()
                .reduce(f64::max)
                .map(|gain| format!("{:+.2} dB", gain))
                .unwrap_or_else(|| "—".to_string()),
        ),
    ];

    let titles = metadata_track_titles(state);
    let row_count = replaygain_row_count_from_parts(
        state,
        titles.len(),
        track_gain.len(),
        album_gain_values.len(),
        track_peak.len(),
        album_peak.len(),
    );
    let mut rows = Vec::new();
    for idx in 0..row_count {
        let title = titles
            .get(idx)
            .cloned()
            .filter(|title| !title.trim().is_empty())
            .or_else(|| state.active_surface().file_labels.get(idx).cloned())
            .unwrap_or_else(|| format!("Track {}", idx + 1));
        rows.push(ReplayGainRow {
            index: idx,
            title,
            track_gain: replaygain_cell(&track_gain, idx),
            album_gain: replaygain_cell(&album_gain_values, idx),
            track_peak: replaygain_cell(&track_peak, idx),
            album_peak: replaygain_cell(&album_peak, idx),
        });
    }

    ReplayGainViewModel {
        has_data,
        summary,
        rows,
        scan_status,
    }
}

pub fn build_artwork_view_model(state: &MetadataEditorState) -> ArtworkViewModel {
    if state.shows_presentation_control() && state.active_surface().technical_details.disc.is_some() {
        return ArtworkViewModel {
            disc_not_applicable: true,
            read_issues: metadata_file_issue_rows(state, false, true),
            rows: Vec::new(),
        };
    }

    let files = &state.active_surface().technical_details.files;
    let file_count = files.len().max(state.active_surface().paths.len());
    let mut aggregates: BTreeMap<u8, ArtworkAggregate> = BTreeMap::new();

    for (file_idx, file) in files.iter().enumerate() {
        for art in &file.artwork_facts.entries {
            let picture_type = art.picture_type.clone();
            let label = artwork_type_label(&picture_type);
            let aggregate = aggregates
                .entry(picture_type.as_u8())
                .or_insert_with(|| ArtworkAggregate::new(label, picture_type));
            aggregate.present_files.insert(file_idx);
            aggregate.entry_count += 1;
            aggregate.details.insert(artwork_detail(art));
        }
    }

    let mut rows = Vec::new();
    for (bucket, picture_type) in canonical_artwork_rows() {
        let index = rows.len();
        if let Some(aggregate) = aggregates.remove(&picture_type.as_u8()) {
            rows.push(artwork_aggregate_view_row(
                &aggregate,
                file_count,
                index,
                state.artwork_cursor,
            ));
        } else {
            rows.push(ArtworkCoverageRow {
                index,
                kind: bucket.to_string(),
                status: "«not present»".to_string(),
                detail: String::new(),
                picture_type,
                selected: state.artwork_cursor == index,
            });
        }
    }
    for aggregate in aggregates.values() {
        let index = rows.len();
        rows.push(artwork_aggregate_view_row(
            aggregate,
            file_count,
            index,
            state.artwork_cursor,
        ));
    }

    ArtworkViewModel {
        disc_not_applicable: false,
        read_issues: metadata_file_issue_rows(state, false, true),
        rows,
    }
}


pub fn details_analyze_applicable(state: &MetadataEditorState) -> bool {
    if state.active_surface().technical_details.disc.is_some() {
        return false;
    }
    metadata_details_files_for_display(state).iter().any(|file| {
        file.file_facts.read_state.is_readable()
            && (hdcd_applicability_for_file(file) == HdcdApplicability::Applicable
                || preemphasis_applicable_for_file(file))
    })
}

fn metadata_hdcd_is_applicable_to_active_surface(state: &MetadataEditorState) -> bool {
    if state.active_surface().technical_details.disc.is_some() {
        return false;
    }
    metadata_details_files_for_display(state)
        .iter()
        .any(|file| hdcd_applicability_for_file(file) == HdcdApplicability::Applicable)
}

fn metadata_preemphasis_is_applicable_to_active_surface(state: &MetadataEditorState) -> bool {
    if state.active_surface().technical_details.disc.is_some() {
        return false;
    }
    metadata_details_files_for_display(state)
        .iter()
        .any(preemphasis_applicable_for_file)
}

fn preemphasis_applicable_for_file(file: &MetadataFileDetails) -> bool {
    let path = &file.file_facts.path;
    if path_is_disc_structure(path) || path_has_extension(path, &["dsf", "dff"]) {
        return false;
    }
    if path_has_extension(
        path,
        &["mp3", "aac", "ogg", "opus", "wma", "ac3", "dts"],
    ) {
        return false;
    }
    if let ProbeState::Ready(facts) = &file.media_facts {
        let combined = format!("{} {}", facts.format_name, facts.codec).to_ascii_lowercase();
        if combined.contains("dsd")
            || combined.contains("mp3")
            || combined.contains("aac")
            || combined.contains("opus")
            || combined.contains("vorbis")
            || combined.contains("ac-3")
            || combined.contains("e-ac-3")
            || combined.contains("dts")
        {
            return false;
        }
    }
    true
}

fn metadata_hdcd_status(state: &MetadataEditorState) -> String {
    if state.active_surface().technical_details.disc.is_some() {
        return "N/A".to_string();
    }
    let values: Vec<String> = metadata_details_files_for_display(state)
        .iter()
        .map(hdcd_status_for_file)
        .collect();
    same_or_multiple(values)
}

fn hdcd_status_for_file(file: &MetadataFileDetails) -> String {
    match hdcd_applicability_for_file(file) {
        HdcdApplicability::NotApplicable => "N/A".to_string(),
        HdcdApplicability::Applicable => match file.analysis_facts.hdcd_detected {
            Some(true) => file
                .analysis_facts
                .hdcd_detail
                .as_ref()
                .map(|detail| normalize_hdcd_detail(detail))
                .unwrap_or_else(|| "Detected".to_string()),
            Some(false) => "Not detected".to_string(),
            None => "«not scanned»".to_string(),
        },
    }
}

fn normalize_hdcd_detail(detail: &str) -> String {
    let detail = detail.trim();
    if detail.is_empty() {
        "Detected".to_string()
    } else if detail.to_ascii_lowercase().starts_with("hdcd") {
        detail.to_string()
    } else {
        format!("Detected ({detail})")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HdcdApplicability {
    Applicable,
    NotApplicable,
}

#[cfg(test)]
fn hdcd_applicable_for_file(file: &MetadataFileDetails) -> bool {
    hdcd_applicability_for_file(file) == HdcdApplicability::Applicable
}

fn hdcd_applicability_for_file(file: &MetadataFileDetails) -> HdcdApplicability {
    let path = &file.file_facts.path;
    if path_is_disc_structure(path) || path_has_extension(path, &["dsf", "dff"]) {
        return HdcdApplicability::NotApplicable;
    }
    if path_has_extension(
        path,
        &[
            "mp3", "aac", "ogg", "opus", "wma", "ac3", "dts",
        ],
    ) {
        return HdcdApplicability::NotApplicable;
    }

    let candidate_container = path_has_extension(path, &["flac", "wav", "aiff", "aif", "wv"]);
    let ProbeState::Ready(facts) = &file.media_facts else {
        // HDCD must not be exposed from path plausibility alone. A 24-bit file
        // can carry stale cached HDCD facts from an earlier identity, so the
        // Details row and Details analyzer are hidden until the authoritative
        // stream probe has confirmed 16-bit PCM/lossless applicability.
        return HdcdApplicability::NotApplicable;
    };

    if facts.bit_depth != Some(16) {
        return HdcdApplicability::NotApplicable;
    }
    let combined = format!("{} {}", facts.format_name, facts.codec).to_ascii_lowercase();
    if combined.contains("dsd")
        || combined.contains("mp3")
        || (combined.contains("aac") && !combined.contains("alac"))
        || combined.contains("opus")
        || combined.contains("vorbis")
    {
        return HdcdApplicability::NotApplicable;
    }
    if candidate_container
        || combined.contains("pcm")
        || combined.contains("flac")
        || combined.contains("wav")
        || combined.contains("aiff")
        || combined.contains("wavpack")
    {
        HdcdApplicability::Applicable
    } else {
        HdcdApplicability::NotApplicable
    }
}

fn path_is_disc_structure(path: &Path) -> bool {
    let text = path.to_string_lossy().to_ascii_lowercase();
    text.ends_with(".iso")
        || text.contains("/bdmv")
        || text.contains("\\bdmv")
        || text.contains("/audio_ts")
        || text.contains("\\audio_ts")
        || text.contains("/video_ts")
        || text.contains("\\video_ts")
}

fn path_has_extension(path: &Path, extensions: &[&str]) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| extensions.iter().any(|candidate| candidate.eq_ignore_ascii_case(ext)))
        .unwrap_or(false)
}

fn metadata_preemphasis_status(state: &MetadataEditorState) -> String {
    if state.active_surface().technical_details.disc.is_some() {
        return "N/A".to_string();
    }
    let values: Vec<String> = metadata_details_files_for_display(state)
        .iter()
        .map(preemphasis_status_for_file)
        .collect();
    same_or_multiple(values)
}

fn preemphasis_status_for_file(file: &MetadataFileDetails) -> String {
    use crate::tui::preemphasis::PreemphasisConfidence;
    let detail = non_empty_detail(file.analysis_facts.preemphasis_detail.as_deref());
    match file.analysis_facts.preemphasis {
        Some(PreemphasisConfidence::Detected) if preemphasis_detail_is_pre_flag(detail) => {
            "Detected (PRE flag)".to_string()
        }
        Some(PreemphasisConfidence::StrongCandidate) if preemphasis_detail_is_catalog(detail) => {
            preemphasis_catalog_status(detail)
        }
        Some(PreemphasisConfidence::Possible) if preemphasis_detail_is_catalog(detail) => {
            preemphasis_catalog_status(detail)
        }
        Some(PreemphasisConfidence::NotDetected) => "Not detected".to_string(),
        Some(PreemphasisConfidence::Detected)
        | Some(PreemphasisConfidence::StrongCandidate)
        | Some(PreemphasisConfidence::Possible)
        | Some(PreemphasisConfidence::Indeterminate) => "Not detected".to_string(),
        None => "«not scanned»".to_string(),
    }
}

fn preemphasis_detail_is_pre_flag(detail: Option<&str>) -> bool {
    detail
        .map(crate::tui::preemphasis::metadata_editor_detail_is_pre_flag)
        .unwrap_or(false)
}

fn preemphasis_detail_is_catalog(detail: Option<&str>) -> bool {
    detail
        .map(crate::tui::preemphasis::metadata_editor_detail_is_catalog)
        .unwrap_or(false)
}

fn preemphasis_catalog_status(detail: Option<&str>) -> String {
    let detail = detail.unwrap_or("catalog match").trim();
    if detail.is_empty() {
        "Candidate (catalog match)".to_string()
    } else {
        format!("Candidate ({detail})")
    }
}

fn non_empty_detail(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn metadata_details_files_for_display(state: &MetadataEditorState) -> &[MetadataFileDetails] {
    &state.active_surface().technical_details.files
}

fn metadata_details_analysis_status_text(state: &MetadataEditorState) -> Option<String> {
    state.details_analysis.as_ref().map(|scan| {
        format!(
            "Analyzing HDCD/PRE for {} file{}...",
            scan.file_count,
            if scan.file_count == 1 { "" } else { "s" }
        )
    })
}

fn metadata_details_probe_status_text(state: &MetadataEditorState) -> Option<String> {
    match &state.active_surface().technical_details.details_probe_state {
        MetadataDetailsProbeState::Loading {
            completed, total, ..
        } => Some(format!(
            "Audio details are loading in the background ({}/{}).",
            completed.min(total),
            total
        )),
        MetadataDetailsProbeState::Cancelled { .. } => {
            Some("Audio details load cancelled. Press Ctrl+R to retry.".to_string())
        }
        MetadataDetailsProbeState::Partial { issues } if !issues.is_empty() => Some(format!(
            "Audio details loaded with {} issue{}. Press Ctrl+R to retry failed probes.",
            issues.len(),
            if issues.len() == 1 { "" } else { "s" }
        )),
        MetadataDetailsProbeState::Unloaded => Some(
            "Audio details are not loaded yet; entering Details starts a background probe."
                .to_string(),
        ),
        MetadataDetailsProbeState::Ready | MetadataDetailsProbeState::Partial { .. } => None,
    }
}

fn metadata_disc_location_fields(
    state: &MetadataEditorState,
    disc: &DiscTechnicalDetails,
) -> Vec<DetailField> {
    metadata_disc_location_pairs(state, disc)
        .into_iter()
        .map(|(k, v)| detail_field(k, v))
        .collect()
}

fn metadata_disc_location_pairs(
    state: &MetadataEditorState,
    disc: &DiscTechnicalDetails,
) -> Vec<(String, String)> {
    let source_paths = unique_metadata_paths_from(
        state.active_surface()
            .technical_details
            .files
            .iter()
            .map(|file| &file.file_facts.path),
        &state.active_surface().paths,
    );

    let mut rows = Vec::new();
    rows.push(("Source".to_string(), metadata_file_names(&source_paths)));
    rows.push(("Source folder".to_string(), metadata_folder_name(&source_paths)));
    rows.push((
        "Presentation".to_string(),
        if disc.presentation_label.trim().is_empty() {
            state
                .active_presentation_label()
                .unwrap_or("presentation")
                .to_string()
        } else {
            disc.presentation_label.clone()
        },
    ));
    if let Some(context) = metadata_disc_context_value(state, disc) {
        rows.push(("Context".to_string(), context));
    }
    rows
}

fn metadata_disc_context_value(
    state: &MetadataEditorState,
    disc: &DiscTechnicalDetails,
) -> Option<String> {
    let mut parts = Vec::new();

    if let Some(playlist) = state.active_surface().bluray_playlist_number {
        parts.push(format!("playlist {:05}", playlist));
    }
    if let Some(pid) = state.active_surface().bluray_audio_pid {
        parts.push(format!("audio PID 0x{:04X}", pid));
    }
    if let Some(stream) = state.active_surface().bluray_audio_stream_index {
        parts.push(format!("audio stream index {}", stream));
    }
    if let Some(angle) = state.active_surface().bluray_angle_number.or(state.active_surface().dvdv_angle_number) {
        parts.push(format!("angle {}", angle));
    }

    if let Some(chapters) = state.active_surface()
        .dvdv_source_chapters
        .as_ref()
        .filter(|chapters| !chapters.is_empty())
    {
        parts.push(format!("chapters {}", format_u16_ranges(chapters)));
    } else if let Some(count) = state.active_surface()
        .bluray_chapter_durations
        .as_ref()
        .map(Vec::len)
        .filter(|count| *count > 0)
        .or_else(|| {
            state.active_surface()
                .dvdv_track_durations
                .as_ref()
                .map(Vec::len)
                .filter(|count| *count > 0)
        })
    {
        parts.push(format!("{} chapter{}", count, if count == 1 { "" } else { "s" }));
    }

    if let Some(area) = state.active_surface().sacd_area_kind {
        let label = match area {
            crate::tui::sacd::AreaKind::Stereo => "SACD stereo area",
            crate::tui::sacd::AreaKind::MultiChannel => "SACD multi-channel area",
        };
        parts.push(label.to_string());
    }

    let track_count = disc.track_count.max(state.active_surface().paths.len());
    if track_count > 0 {
        parts.push(format!("{} track{}", track_count, if track_count == 1 { "" } else { "s" }));
    }

    (!parts.is_empty()).then(|| parts.join(", "))
}

fn metadata_file_names(paths: &[PathBuf]) -> String {
    if paths.is_empty() {
        return "—".to_string();
    }

    paths
        .iter()
        .map(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string())
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn metadata_folder_name(paths: &[PathBuf]) -> String {
    let mut parents = paths.iter().filter_map(|path| path.parent());
    let Some(first) = parents.next() else {
        return "—".to_string();
    };
    if parents.all(|parent| parent == first) {
        first.display().to_string()
    } else {
        "«multiple folders»".to_string()
    }
}

fn unique_metadata_paths_from<'a>(
    primary: impl IntoIterator<Item = &'a PathBuf>,
    fallback: &'a [PathBuf],
) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut paths = Vec::new();

    for path in primary.into_iter().chain(fallback.iter()) {
        if seen.insert(path.clone()) {
            paths.push(path.clone());
        }
    }

    paths
}

fn format_u16_ranges(values: &[u16]) -> String {
    if values.is_empty() {
        return "—".to_string();
    }

    let mut ranges = Vec::new();
    let mut start = values[0];
    let mut prev = values[0];

    for &value in &values[1..] {
        if value == prev.saturating_add(1) {
            prev = value;
            continue;
        }
        push_u16_range(&mut ranges, start, prev);
        start = value;
        prev = value;
    }
    push_u16_range(&mut ranges, start, prev);
    ranges.join(", ")
}

fn push_u16_range(ranges: &mut Vec<String>, start: u16, end: u16) {
    if start == end {
        ranges.push(start.to_string());
    } else {
        ranges.push(format!("{}-{}", start, end));
    }
}

fn metadata_file_issue_rows(
    state: &MetadataEditorState,
    include_probe_errors: bool,
    include_metadata_errors: bool,
) -> Vec<IssueViewRow> {
    metadata_file_issue_rows_for_files(
        state,
        &state.active_surface().technical_details.files,
        include_probe_errors,
        include_metadata_errors,
    )
}

fn metadata_file_issue_rows_for_files(
    state: &MetadataEditorState,
    files: &[MetadataFileDetails],
    include_probe_errors: bool,
    include_metadata_errors: bool,
) -> Vec<IssueViewRow> {
    #[derive(Debug)]
    struct PendingIssue {
        label: String,
        kind: String,
        reason: String,
        path: PathBuf,
    }

    let include_file_label = files.len() > 1;
    let mut pending = Vec::new();

    for (idx, file) in files.iter().enumerate() {
        let label = metadata_issue_file_label(state, file, idx, include_file_label);
        let path = file.file_facts.path.clone();
        if !file.issues.is_empty() {
            for issue in &file.issues {
                let row = match issue {
                    MetadataIssue::Filesystem { path, reason } => {
                        Some((path, "filesystem", reason))
                    }
                    MetadataIssue::TagRead { path, reason }
                    | MetadataIssue::RecoverableTagWarning { path, reason }
                        if include_metadata_errors =>
                    {
                        Some((path, "tags/artwork", reason))
                    }
                    MetadataIssue::Unsupported { path, reason } if include_metadata_errors => {
                        Some((path, "unsupported", reason))
                    }
                    MetadataIssue::Probe { path, reason, .. } if include_probe_errors => {
                        Some((path, "audio probe", reason))
                    }
                    MetadataIssue::SaveBlocked { path, reason }
                        if include_probe_errors && include_metadata_errors =>
                    {
                        Some((path, "save blocked", reason))
                    }
                    MetadataIssue::Write { path, reason }
                        if include_probe_errors && include_metadata_errors =>
                    {
                        Some((path, "write", reason))
                    }
                    _ => None,
                };
                if let Some((issue_path, kind, reason)) = row {
                    pending.push(PendingIssue {
                        label: label.clone(),
                        kind: kind.to_string(),
                        reason: elide_metadata_issue_path(reason, issue_path),
                        path: issue_path.clone(),
                    });
                }
            }
            continue;
        }

        if let Some(err) = file
            .file_facts
            .filesystem_error
            .as_ref()
            .filter(|err| !err.trim().is_empty())
        {
            pending.push(PendingIssue {
                label: label.clone(),
                kind: "filesystem".to_string(),
                reason: elide_metadata_issue_path(err, &path),
                path: path.clone(),
            });
        }
        if include_probe_errors {
            if let ProbeState::Failed { reason, .. } = &file.media_facts {
                pending.push(PendingIssue {
                    label: label.clone(),
                    kind: "audio probe".to_string(),
                    reason: elide_metadata_issue_path(reason, &path),
                    path: path.clone(),
                });
            }
        }
        if include_probe_errors && include_metadata_errors {
            match &file.file_facts.read_state {
                FileReadState::Readable => {}
                FileReadState::Unreadable { reason } => pending.push(PendingIssue {
                    label: label.clone(),
                    kind: "unreadable".to_string(),
                    reason: elide_metadata_issue_path(reason, &path),
                    path: path.clone(),
                }),
                FileReadState::Unsupported { reason } => pending.push(PendingIssue {
                    label: label.clone(),
                    kind: "unsupported".to_string(),
                    reason: elide_metadata_issue_path(reason, &path),
                    path: path.clone(),
                }),
            }
            if let Some(reason) = file.file_facts.write_eligibility.block_reason() {
                pending.push(PendingIssue {
                    label: label.clone(),
                    kind: "save blocked".to_string(),
                    reason: elide_metadata_issue_path(reason, &path),
                    path,
                });
            }
        }
    }

    let mut counts = BTreeMap::<(String, String), BTreeSet<PathBuf>>::new();
    for issue in &pending {
        counts
            .entry((issue.kind.clone(), issue.reason.clone()))
            .or_default()
            .insert(issue.path.clone());
    }

    let mut emitted_collapsed = BTreeSet::new();
    let mut rows = Vec::new();
    for issue in pending {
        let signature = (issue.kind.clone(), issue.reason.clone());
        let count = counts.get(&signature).map_or(1, BTreeSet::len);
        if count >= 2 {
            if emitted_collapsed.insert(signature) {
                rows.push(issue_view_row(
                    &format!("{count} files"),
                    "",
                    &issue.reason,
                ));
            }
        } else {
            rows.push(issue_view_row(&issue.label, &issue.kind, &issue.reason));
        }
    }
    rows
}

fn elide_metadata_issue_path(reason: &str, path: &Path) -> String {
    let displayed = path.display().to_string();
    let mut value = reason.trim().to_string();
    for needle in [
        format!(" in '{displayed}'"),
        format!(" from '{displayed}'"),
        format!(" for '{displayed}'"),
        format!(" '{}':", displayed),
        format!("'{displayed}'"),
        displayed,
    ] {
        value = value.replace(&needle, "");
    }
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_matches(|c: char| c == ':' || c.is_whitespace())
        .to_string()
}

fn issue_view_row(label: &str, kind: &str, reason: &str) -> IssueViewRow {
    IssueViewRow {
        label: label.to_string(),
        kind: kind.to_string(),
        reason: reason.trim().to_string(),
    }
}

fn metadata_issue_file_label(
    state: &MetadataEditorState,
    file: &MetadataFileDetails,
    idx: usize,
    include_file_label: bool,
) -> String {
    if include_file_label {
        state.active_surface()
            .file_labels
            .get(idx)
            .cloned()
            .or_else(|| {
                file.file_facts
                    .path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| format!("File {}", idx + 1))
    } else {
        file.file_facts
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| file.file_facts.path.display().to_string())
    }
}

fn metadata_save_eligibility_summary(files: &[MetadataFileDetails]) -> String {
    let total = files.len();
    let writable = files
        .iter()
        .filter(|file| file.file_facts.write_eligibility.is_writable())
        .count();
    let readable_only = files
        .iter()
        .filter(|file| matches!(&file.file_facts.write_eligibility, FileWriteEligibility::Unknown { .. }))
        .count();
    let read_only = files
        .iter()
        .filter(|file| {
            matches!(&file.file_facts.write_eligibility, FileWriteEligibility::Blocked { .. })
                && matches!(&file.file_facts.read_state, FileReadState::Readable)
        })
        .count();
    let unreadable = files
        .iter()
        .filter(|file| matches!(&file.file_facts.read_state, FileReadState::Unreadable { .. }))
        .count();
    let unsupported = files
        .iter()
        .filter(|file| matches!(&file.file_facts.read_state, FileReadState::Unsupported { .. }))
        .count();

    if total == 0 {
        return "—".to_string();
    }
    if writable == total {
        return format!("{} writable", total);
    }

    let mut parts = Vec::new();
    if writable > 0 {
        parts.push(format!("{} writable", writable));
    }
    if readable_only > 0 {
        parts.push(format!("{} readable, save blocked", readable_only));
    }
    if read_only > 0 {
        parts.push(format!("{} read-only", read_only));
    }
    if unreadable > 0 {
        parts.push(format!("{} unreadable", unreadable));
    }
    if unsupported > 0 {
        parts.push(format!("{} unsupported", unsupported));
    }
    format!("{} of {} files eligible ({})", writable, total, parts.join(", "))
}

fn metadata_total_size_cached(files: &[MetadataFileDetails]) -> String {
    let total: u64 = files.iter().filter_map(|file| file.file_facts.file_size).sum();
    if total == 0 {
        "—".to_string()
    } else {
        format!("{} ({} bytes)", format_bytes(total), total)
    }
}

fn metadata_last_modified_cached(files: &[MetadataFileDetails]) -> String {
    files
        .iter()
        .filter_map(|file| file.file_facts.modified.as_ref().cloned())
        .max()
        .map(format_system_time_utc)
        .unwrap_or_else(|| "—".to_string())
}

fn format_system_time_utc(time: SystemTime) -> String {
    let seconds = match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_secs() as i64,
        Err(err) => -(err.duration().as_secs() as i64),
    };
    format_unix_utc_seconds(seconds)
}

fn format_unix_utc_seconds(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400);
    let secs_of_day = seconds.rem_euclid(86_400) as u32;
    let (year, month, day) = civil_from_days(days);
    let hour = secs_of_day / 3_600;
    let minute = (secs_of_day / 60) % 60;
    let second = secs_of_day % 60;
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        year, month, day, hour, minute, second
    )
}

// Howard Hinnant's civil-from-days algorithm, adjusted for Unix epoch days.
fn civil_from_days(days_since_epoch: i64) -> (i32, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    let year = y + if m <= 2 { 1 } else { 0 };
    (year as i32, m as u32, d as u32)
}

fn format_bytes(bytes: u64) -> String {
    let value = bytes as f64;
    if bytes >= 1_073_741_824 {
        format!("{:.1} GB", value / 1_073_741_824.0)
    } else if bytes >= 1_048_576 {
        format!("{:.1} MB", value / 1_048_576.0)
    } else if bytes >= 1024 {
        format!("{:.1} KB", value / 1024.0)
    } else {
        format!("{} B", bytes)
    }
}

fn same_or_multiple<I>(values: I) -> String
where
    I: IntoIterator<Item = String>,
{
    let mut iter = values.into_iter().filter(|value| !value.trim().is_empty());
    let Some(first) = iter.next() else {
        return "—".to_string();
    };
    if iter.all(|value| value == first) {
        first
    } else {
        "«multiple values»".to_string()
    }
}

fn file_channels_value(info: &SourceInfo) -> String {
    if info.channel_layout.trim().is_empty() {
        info.channels.to_string()
    } else {
        format!("{} ({})", info.channels, info.channel_layout)
    }
}

fn metadata_average_bitrate(infos: &[&SourceInfo]) -> String {
    let total_size: u64 = infos.iter().map(|info| info.file_size).sum();
    let total_duration: f64 = infos.iter().map(|info| info.duration_secs).sum();
    if total_size == 0 || total_duration <= 0.0 {
        "—".to_string()
    } else {
        format!("{:.0} kbps", (total_size as f64 * 8.0 / total_duration) / 1000.0)
    }
}

fn metadata_encoding_kind(infos: &[&SourceInfo]) -> &'static str {
    let has_lossless = infos.iter().any(|info| codec_is_lossless(&info.codec));
    let has_non_lossless = infos.iter().any(|info| !codec_is_lossless(&info.codec));

    match (has_lossless, has_non_lossless) {
        (true, false) => "lossless",
        (false, true) => "lossy",
        (true, true) => "mixed lossless/lossy",
        (false, false) => "—",
    }
}

fn disc_channels_value(disc: &DiscTechnicalDetails) -> String {
    match (
        disc.channels,
        disc.channel_layout
            .as_ref()
            .map(|layout| layout.trim())
            .filter(|layout| !layout.is_empty()),
    ) {
        (Some(channels), Some(layout)) => format!("{} ({})", channels, layout),
        (Some(channels), None) => channels.to_string(),
        (None, Some(layout)) => layout.to_string(),
        (None, None) => "—".to_string(),
    }
}

fn disc_average_bitrate(disc: &DiscTechnicalDetails) -> String {
    match (disc.sample_rate, disc.bit_depth, disc.channels) {
        (Some(sample_rate), Some(bit_depth), Some(channels))
            if codec_is_pcm_like(disc.codec.as_deref().unwrap_or_default()) =>
        {
            let kbps = sample_rate as f64 * bit_depth as f64 * channels as f64 / 1000.0;
            format!("{:.0} kbps", kbps)
        }
        _ => "—".to_string(),
    }
}

fn disc_encoding_value(disc: &DiscTechnicalDetails) -> String {
    match disc.lossless {
        Some(true) => "lossless".to_string(),
        Some(false) => "lossy".to_string(),
        None => disc
            .codec
            .as_deref()
            .map(|codec| if codec_is_lossless(codec) { "lossless" } else { "lossy" }.to_string())
            .unwrap_or_else(|| "—".to_string()),
    }
}

fn codec_is_pcm_like(codec: &str) -> bool {
    let codec = codec.trim().to_ascii_lowercase();
    matches!(
        codec.as_str(),
        "pcm" | "lpcm" | "wav" | "wave" | "aiff" | "aifc"
    ) || codec.contains("pcm")
}

fn codec_is_lossless(codec: &str) -> bool {
    let codec = codec.trim().to_ascii_lowercase();
    matches!(
        codec.as_str(),
        "flac" | "alac" | "wavpack" | "pcm" | "lpcm" | "wav" | "wave" | "aiff" | "aifc" | "dsd" | "dst/dsd"
    ) || codec.contains("truehd")
        || codec.contains("dts-hd ma")
        || codec.contains("mlp")
        || codec.contains("lossless")
        || codec.contains("pcm")
}

fn metadata_tool_value(state: &MetadataEditorState) -> String {
    if let Some(value) = state.active_surface()
        .technical_details
        .disc
        .as_ref()
        .and_then(|disc| disc.tool.as_ref())
        .map(|tool| tool.trim().to_string())
        .filter(|tool| !tool.is_empty())
        .or_else(|| {
            metadata_entry_value(
                state,
                &["ENCODER", "ENCODED_BY", "ENCODED BY", "VENDOR", "TOOL", "SOFTWARE"],
            )
        })
    {
        return value;
    }

    let values = state.active_surface().technical_details.files.iter().map(|file| {
        file.file_facts
            .tool
            .as_ref()
            .map(|tool| tool.trim().to_string())
            .filter(|tool| !tool.is_empty())
            .unwrap_or_default()
    });
    let cached = same_or_multiple(values);
    if cached != "—" {
        return cached;
    }

    metadata_entry_value(
        state,
        &["ENCODER", "ENCODED_BY", "ENCODED BY", "VENDOR", "TOOL", "SOFTWARE"],
    )
    .unwrap_or_else(|| "—".to_string())
}

fn embedded_cuesheet_status(state: &MetadataEditorState) -> String {
    if state.active_surface().entries.iter().any(|entry| {
        entry.display_key.eq_ignore_ascii_case("CUESHEET") && !entry.value.trim().is_empty()
    }) {
        "yes".to_string()
    } else {
        "no".to_string()
    }
}

fn active_disc_duration_secs(state: &MetadataEditorState) -> Option<f64> {
    if let Some(durations) = state.active_surface().bluray_chapter_durations.as_ref() {
        return sum_positive(durations.iter().copied());
    }
    if let Some(durations) = state.active_surface().dvdv_track_durations.as_ref() {
        return sum_positive(durations.iter().copied());
    }
    match state.active_surface().sacd_area_kind {
        Some(crate::tui::sacd::AreaKind::Stereo) => state.active_surface()
            .sacd_stereo_durations
            .as_ref()
            .and_then(|durations| sum_positive(durations.iter().copied())),
        Some(crate::tui::sacd::AreaKind::MultiChannel) => state.active_surface()
            .sacd_multi_channel_durations
            .as_ref()
            .and_then(|durations| sum_positive(durations.iter().copied())),
        None => None,
    }
}

fn sum_positive<I>(durations: I) -> Option<f64>
where
    I: IntoIterator<Item = f64>,
{
    let total: f64 = durations
        .into_iter()
        .filter(|duration| duration.is_finite() && *duration > 0.0)
        .sum();
    (total > 0.0).then_some(total)
}

fn sample_count_from_duration(duration_secs: f64, sample_rate: Option<u32>) -> Option<u64> {
    let sample_rate = sample_rate?;
    if sample_rate == 0 || !duration_secs.is_finite() || duration_secs <= 0.0 {
        return None;
    }

    let samples = duration_secs * f64::from(sample_rate);
    if !samples.is_finite() || samples < 0.0 {
        return None;
    }

    Some(samples.round().min(u64::MAX as f64) as u64)
}

fn metadata_total_sample_count(infos: &[&SourceInfo]) -> Option<u64> {
    let mut total = 0u64;
    let mut any = false;
    for info in infos {
        if info.sample_rate == 0 || !info.duration_secs.is_finite() || info.duration_secs <= 0.0 {
            continue;
        }
        total = total.saturating_add((info.duration_secs * info.sample_rate as f64).round() as u64);
        any = true;
    }
    any.then_some(total)
}

fn metadata_duration_with_samples(duration_secs: f64, sample_count: Option<u64>) -> String {
    let duration = metadata_duration_precise(duration_secs);
    match sample_count {
        Some(samples) => format!("{} ({} samples)", duration, format_count_with_spaces(samples)),
        None => duration,
    }
}

fn metadata_duration_precise(duration_secs: f64) -> String {
    let total_ms = (duration_secs.max(0.0) * 1000.0).round() as u64;
    let hours = total_ms / 3_600_000;
    let minutes = (total_ms / 60_000) % 60;
    let seconds = (total_ms / 1000) % 60;
    let millis = total_ms % 1000;
    if hours > 0 {
        format!("{}:{:02}:{:02}.{:03}", hours, minutes, seconds, millis)
    } else {
        format!("{}:{:02}.{:03}", minutes, seconds, millis)
    }
}

fn format_count_with_spaces(value: u64) -> String {
    let raw = value.to_string();
    let mut out = String::with_capacity(raw.len() + raw.len() / 3);
    for (idx, ch) in raw.chars().rev().enumerate() {
        if idx > 0 && idx % 3 == 0 {
            out.push(' ');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}

fn active_disc_entry_value(state: &MetadataEditorState, keys: &[&str]) -> Option<String> {
    metadata_entry_value(state, keys)
}

fn metadata_entry_value(state: &MetadataEditorState, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        state.active_surface()
            .entries
            .iter()
            .find(|entry| entry.display_key.eq_ignore_ascii_case(key))
            .map(|entry| entry.value.trim().to_string())
            .filter(|value| !value.is_empty())
    })
}

fn metadata_entry_values(state: &MetadataEditorState, keys: &[&str]) -> Vec<String> {
    let Some(entry) = keys.iter().find_map(|key| {
        state.active_surface()
            .entries
            .iter()
            .find(|entry| entry.display_key.eq_ignore_ascii_case(key))
    }) else {
        return Vec::new();
    };
    if entry.per_file_values.len() > 1 {
        // Preserve per-file positions exactly. Missing values must stay as empty
        // cells so Track N values cannot slide into Track N-1 rows.
        entry
            .per_file_values
            .iter()
            .map(|value| value.trim().to_string())
            .collect()
    } else if !entry.value.trim().is_empty() {
        vec![entry.value.trim().to_string()]
    } else {
        Vec::new()
    }
}

fn has_metadata_values(values: &[String]) -> bool {
    values.iter().any(|value| !value.trim().is_empty())
}

fn summarize_values(values: &[String]) -> String {
    let mut iter = values
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty());
    let Some(first) = iter.next() else {
        return "—".to_string();
    };
    if iter.all(|value| value == first) {
        first.to_string()
    } else {
        "«multiple values»".to_string()
    }
}

fn summarize_peak(track_peak: &[String], album_peak: &[String]) -> String {
    if has_metadata_values(album_peak) {
        return summarize_values(album_peak);
    }
    if !has_metadata_values(track_peak) {
        return "—".to_string();
    }
    let max_peak = track_peak
        .iter()
        .filter_map(|value| value.trim().parse::<f64>().ok())
        .reduce(f64::max);
    max_peak
        .map(|peak| format!("{:.6}", peak))
        .unwrap_or_else(|| summarize_values(track_peak))
}

fn parse_db_value(value: &str) -> Option<f64> {
    value
        .split_whitespace()
        .next()
        .and_then(|number| number.parse::<f64>().ok())
}

fn metadata_track_titles(state: &MetadataEditorState) -> Vec<String> {
    metadata_entry_values(state, &["TITLE"])
}

fn replaygain_cell(values: &[String], idx: usize) -> String {
    if values.is_empty() {
        "—".to_string()
    } else if values.len() == 1 {
        let value = values[0].trim();
        if value.is_empty() {
            "—".to_string()
        } else {
            value.to_string()
        }
    } else {
        values
            .get(idx)
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| "—".to_string())
    }
}

struct ArtworkAggregate {
    label: String,
    picture_type: lofty::picture::PictureType,
    present_files: BTreeSet<usize>,
    entry_count: usize,
    details: BTreeSet<String>,
}

impl ArtworkAggregate {
    fn new(label: String, picture_type: lofty::picture::PictureType) -> Self {
        Self {
            label,
            picture_type,
            present_files: BTreeSet::new(),
            entry_count: 0,
            details: BTreeSet::new(),
        }
    }
}

fn artwork_aggregate_view_row(
    aggregate: &ArtworkAggregate,
    file_count: usize,
    index: usize,
    cursor: usize,
) -> ArtworkCoverageRow {
    ArtworkCoverageRow {
        index,
        kind: aggregate.label.clone(),
        status: artwork_presence_status(
            aggregate.present_files.len(),
            file_count,
            aggregate.entry_count,
        ),
        detail: artwork_detail_summary(&aggregate.details),
        picture_type: aggregate.picture_type.clone(),
        selected: cursor == index,
    }
}

pub fn artwork_action_row_count(state: &MetadataEditorState) -> usize {
    build_artwork_view_model(state).rows.len()
}

fn canonical_artwork_rows() -> [(&'static str, lofty::picture::PictureType); 5] {
    [
        ("Front Cover", lofty::picture::PictureType::CoverFront),
        ("Back Cover", lofty::picture::PictureType::CoverBack),
        ("Artist", lofty::picture::PictureType::Artist),
        ("Disc", lofty::picture::PictureType::Media),
        ("Icon", lofty::picture::PictureType::Icon),
    ]
}

fn artwork_presence_status(present_files: usize, file_count: usize, entry_count: usize) -> String {
    if present_files == 0 {
        return "«not present»".to_string();
    }

    if file_count <= 1 {
        return if entry_count <= 1 {
            "Embedded artwork".to_string()
        } else {
            format!("{} embedded entries", entry_count)
        };
    }

    let base = if present_files == file_count {
        format!("Embedded in {}/{} files", present_files, file_count)
    } else {
        format!("Present in {}/{} files", present_files, file_count)
    };

    if entry_count > present_files {
        format!("{} ({} entries)", base, entry_count)
    } else {
        base
    }
}

fn artwork_detail_summary(details: &BTreeSet<String>) -> String {
    match details.len() {
        0 => String::new(),
        1 => details.iter().next().cloned().unwrap_or_default(),
        len => {
            let shown: Vec<&str> = details.iter().take(4).map(String::as_str).collect();
            let suffix = if len > shown.len() {
                format!("; +{} more", len - shown.len())
            } else {
                String::new()
            };
            format!("mixed ({} variants): {}{}", len, shown.join("; "), suffix)
        }
    }
}

fn artwork_detail(art: &ArtworkInfo) -> String {
    let mut parts = vec![format_bytes(art.data_size as u64)];
    if let (Some(width), Some(height)) = (art.width, art.height) {
        parts.push(format!("{}×{}", width, height));
    }
    if !art.mime_type.trim().is_empty() {
        parts.push(art.mime_type.clone());
    }
    parts.join(", ")
}

fn artwork_type_label(picture_type: &lofty::picture::PictureType) -> String {
    artwork_type_label_from_id3_code(picture_type.as_u8())
}

fn artwork_type_label_from_id3_code(code: u8) -> String {
    match code {
        0 => "Other".to_string(),
        1 => "Icon".to_string(),
        2 => "Other Icon".to_string(),
        3 => "Front Cover".to_string(),
        4 => "Back Cover".to_string(),
        5 => "Leaflet".to_string(),
        6 => "Disc".to_string(),
        7 => "Lead Artist".to_string(),
        8 => "Artist".to_string(),
        9 => "Conductor".to_string(),
        10 => "Band".to_string(),
        11 => "Composer".to_string(),
        12 => "Lyricist".to_string(),
        13 => "Recording Location".to_string(),
        14 => "During Recording".to_string(),
        15 => "During Performance".to_string(),
        16 => "Movie Screen Capture".to_string(),
        17 => "Bright Colored Fish".to_string(),
        18 => "Illustration".to_string(),
        19 => "Band Logo".to_string(),
        20 => "Publisher Logo".to_string(),
        other => format!("Picture type {}", other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_rate_detail_appends_recognized_dsd_rate() {
        assert_eq!(sample_rate_detail(11_289_600), "11289600 Hz (DSD256)");
        assert_eq!(sample_rate_detail(44_100), "44100 Hz");
    }

    fn details_state_for_file(file: MetadataFileDetails) -> MetadataEditorState {
        let path = file.file_facts.path.clone();
        MetadataEditorState::for_files(
            vec![path],
            Vec::new(),
            Vec::new(),
            super::super::app::MetadataTechnicalDetails::from_files(vec![file]),
        )
    }

    fn file_with_probe(
        path: &str,
        format_name: &str,
        codec: &str,
        bit_depth: Option<u32>,
    ) -> MetadataFileDetails {
        let mut file = MetadataFileDetails::from_open_cache(
            PathBuf::from(path),
            Some(100),
            None,
            None,
            None,
            None,
            FileReadState::Readable,
            FileWriteEligibility::Writable,
            super::super::probe::SourceMetadata::default(),
        );
        file.set_probe_ready(SourceInfo {
            format_name: format_name.to_string(),
            codec: codec.to_string(),
            sample_rate: 44_100,
            bit_depth,
            channels: 2,
            channel_layout: "stereo".to_string(),
            duration_secs: 1.0,
            file_size: 100,
        });
        file
    }

    #[test]
    fn replaygain_cell_preserves_missing_positions() {
        let values = vec!["+1.0 dB".to_string(), String::new(), "-2.0 dB".to_string()];
        assert_eq!(replaygain_cell(&values, 0), "+1.0 dB");
        assert_eq!(replaygain_cell(&values, 1), "—");
        assert_eq!(replaygain_cell(&values, 2), "-2.0 dB");
    }

    #[test]
    fn details_view_hides_hdcd_and_preemphasis_when_source_is_lossy() {
        let file = file_with_probe("/tmp/lossy.mp3", "mp3", "mp3", None);
        let state = details_state_for_file(file);
        let vm = build_details_view_model(&state);

        assert!(!vm.general.iter().any(|row| row.key == "HDCD"));
        assert!(!vm.general.iter().any(|row| row.key == "Pre-emphasis"));
    }

    #[test]
    fn details_view_shows_cached_hdcd_for_applicable_sixteen_bit_pcm() {
        let mut file = file_with_probe("/tmp/cd_rip.flac", "flac", "flac", Some(16));
        file.analysis_facts.hdcd_detected = Some(false);
        let state = details_state_for_file(file);
        let vm = build_details_view_model(&state);

        assert_eq!(
            vm.general
                .iter()
                .find(|row| row.key == "HDCD")
                .map(|row| row.value.as_str()),
            Some("Not detected")
        );
    }

    #[test]
    fn details_analyze_is_available_for_pcm_details_targets() {
        let file = file_with_probe("/tmp/cd_rip.flac", "flac", "flac", Some(16));
        let state = details_state_for_file(file);

        assert!(details_analyze_applicable(&state));
    }

    #[test]
    fn details_analyze_is_hidden_for_lossy_details_targets() {
        let file = file_with_probe("/tmp/lossy.mp3", "mp3", "mp3", None);
        let state = details_state_for_file(file);

        assert!(!details_analyze_applicable(&state));
    }

    #[test]
    fn replaygain_view_model_has_unconditional_summary_and_no_selection_state() {
        let file = file_with_probe("/tmp/01.flac", "flac", "flac", Some(16));
        let state = details_state_for_file(file);
        let vm = build_replaygain_view_model(&state);

        let keys: Vec<&str> = vm.summary.iter().map(|row| row.key.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "Track Gain",
                "Album Gain",
                "Total Peak",
                "Loudest track",
                "Quietest track"
            ]
        );
        assert_eq!(vm.summary[0].value, "«not scanned»");
        assert_eq!(vm.summary[1].value, "«not scanned»");
        assert_eq!(vm.rows.len(), 1);
    }

    #[test]
    fn replaygain_row_count_includes_tag_vectors_not_only_paths() {
        let file = file_with_probe("/tmp/01.flac", "flac", "flac", Some(16));
        let mut state = details_state_for_file(file);
        let surface = state.active_surface_mut();
        surface.entries.push(super::super::probe::TagEntry {
            row_scope: crate::tui::probe::RowScope::File,
            display_key: "REPLAYGAIN_TRACK_GAIN".to_string(),
            item_key: lofty::tag::ItemKey::ReplayGainTrackGain,
            value: "-1.0 dB".to_string(),
            original: "-1.0 dB".to_string(),
            is_binary: false,
            is_mixed: true,
            has_multiple_stored_values: false,
            per_file_stored_value_counts: Vec::new(),
            per_file_values: vec![
                "-1.0 dB".to_string(),
                "-2.0 dB".to_string(),
                "-3.0 dB".to_string(),
            ],
            per_file_originals: vec![
                "-1.0 dB".to_string(),
                "-2.0 dB".to_string(),
                "-3.0 dB".to_string(),
            ],
            mb_proposed_value: None,
            mb_proposed_per_file: None,
        });

        assert_eq!(state.active_surface().paths.len(), 1);
        assert_eq!(replaygain_action_row_count(&state), 3);
        assert_eq!(build_replaygain_view_model(&state).rows.len(), 3);
    }

    #[test]
    fn hdcd_row_is_hidden_until_probe_confirms_sixteen_bit_pcm() {
        let mut file = MetadataFileDetails::from_open_cache(
            PathBuf::from("/tmp/unprobed.flac"),
            Some(100),
            None,
            None,
            None,
            None,
            FileReadState::Readable,
            FileWriteEligibility::Writable,
            super::super::probe::SourceMetadata::default(),
        );
        file.analysis_facts.hdcd_detected = Some(false);
        let mut state = details_state_for_file(file);

        let vm = build_details_view_model(&state);
        assert!(!vm.general.iter().any(|row| row.key == "HDCD"));
        assert!(details_analyze_applicable(&state)); // PRE metadata scan is still valid for this source.

        state.active_surface_mut().technical_details.files[0].set_probe_ready(SourceInfo {
            format_name: "flac".to_string(),
            codec: "flac".to_string(),
            sample_rate: 44_100,
            bit_depth: Some(16),
            channels: 2,
            channel_layout: "stereo".to_string(),
            duration_secs: 1.0,
            file_size: 100,
        });

        let vm = build_details_view_model(&state);
        assert_eq!(
            vm.general
                .iter()
                .find(|row| row.key == "HDCD")
                .map(|row| row.value.as_str()),
            Some("Not detected")
        );
    }

    #[test]
    fn artwork_presence_reports_partial_coverage() {
        assert_eq!(artwork_presence_status(1, 12, 1), "Present in 1/12 files");
        assert_eq!(artwork_presence_status(12, 12, 12), "Embedded in 12/12 files");
        assert_eq!(
            artwork_presence_status(2, 12, 3),
            "Present in 2/12 files (3 entries)"
        );
    }

    #[test]
    fn mixed_encoding_is_not_collapsed_to_lossy() {
        let flac = SourceInfo {
            format_name: "flac".to_string(),
            codec: "flac".to_string(),
            sample_rate: 44_100,
            bit_depth: Some(16),
            channels: 2,
            channel_layout: String::new(),
            duration_secs: 1.0,
            file_size: 100,
        };
        let mp3 = SourceInfo {
            format_name: "mp3".to_string(),
            codec: "mp3".to_string(),
            sample_rate: 44_100,
            bit_depth: None,
            channels: 2,
            channel_layout: String::new(),
            duration_secs: 1.0,
            file_size: 100,
        };
        assert_eq!(metadata_encoding_kind(&[&flac, &mp3]), "mixed lossless/lossy");
    }

    #[test]
    fn artwork_detail_summary_reports_mixed_variants() {
        let mut details = BTreeSet::new();
        details.insert("184 KB, 500×500, image/jpeg".to_string());
        details.insert("220 KB, 600×600, image/png".to_string());

        let summary = artwork_detail_summary(&details);
        assert!(summary.starts_with("mixed (2 variants): "));
        assert!(summary.contains("184 KB, 500×500, image/jpeg"));
        assert!(summary.contains("220 KB, 600×600, image/png"));
    }

    #[test]
    fn artwork_canonical_rows_keep_exact_picture_types() {
        let rows = canonical_artwork_rows();
        assert_eq!(rows[0], ("Front Cover", lofty::picture::PictureType::CoverFront));
        assert_eq!(rows[1], ("Back Cover", lofty::picture::PictureType::CoverBack));
        assert_eq!(rows[2], ("Artist", lofty::picture::PictureType::Artist));
        assert_eq!(rows[3], ("Disc", lofty::picture::PictureType::Media));
        assert_eq!(rows[4], ("Icon", lofty::picture::PictureType::Icon));
    }

    #[test]
    fn artwork_rows_preserve_non_canonical_picture_types() {
        let mut metadata = super::super::probe::SourceMetadata::default();
        metadata.artwork.push(ArtworkInfo {
            picture_type: lofty::picture::PictureType::LeadArtist,
            mime_type: "image/jpeg".to_string(),
            data_size: 184_000,
            width: Some(500),
            height: Some(500),
        });
        metadata.artwork.push(ArtworkInfo {
            picture_type: lofty::picture::PictureType::Leaflet,
            mime_type: "image/png".to_string(),
            data_size: 92_000,
            width: Some(640),
            height: Some(480),
        });
        metadata.artwork.push(ArtworkInfo {
            picture_type: lofty::picture::PictureType::Composer,
            mime_type: "image/webp".to_string(),
            data_size: 64_000,
            width: Some(256),
            height: Some(256),
        });
        let state = details_state_for_file(MetadataFileDetails::from_open_cache(
            PathBuf::from("/tmp/artwork.flac"),
            Some(100),
            None,
            None,
            None,
            None,
            FileReadState::Readable,
            FileWriteEligibility::Writable,
            metadata,
        ));

        let vm = build_artwork_view_model(&state);
        let lead_artist = vm.rows.iter().find(|row| row.kind == "Lead Artist").unwrap();
        assert_eq!(lead_artist.picture_type, lofty::picture::PictureType::LeadArtist);
        assert_eq!(lead_artist.status, "Embedded artwork");

        let leaflet = vm.rows.iter().find(|row| row.kind == "Leaflet").unwrap();
        assert_eq!(leaflet.picture_type, lofty::picture::PictureType::Leaflet);

        let composer = vm.rows.iter().find(|row| row.kind == "Composer").unwrap();
        assert_eq!(composer.picture_type, lofty::picture::PictureType::Composer);

        let canonical_artist = vm.rows.iter().find(|row| row.kind == "Artist").unwrap();
        assert_eq!(canonical_artist.picture_type, lofty::picture::PictureType::Artist);
        assert_eq!(canonical_artist.status, "«not present»");
    }


    #[test]
    fn hdcd_status_is_hidden_until_authoritative_bit_depth_arrives() {
        let mut file = MetadataFileDetails::from_open_cache(
            PathBuf::from("/tmp/twenty_four_bit.flac"),
            Some(100),
            None,
            None,
            None,
            None,
            FileReadState::Readable,
            FileWriteEligibility::Writable,
            super::super::probe::SourceMetadata::default(),
        );
        file.analysis_facts.hdcd_detected = Some(false);

        assert_eq!(hdcd_applicability_for_file(&file), HdcdApplicability::NotApplicable);
        assert_eq!(hdcd_status_for_file(&file), "N/A");

        file.media_facts = ProbeState::Loading { generation: 9 };
        assert_eq!(hdcd_applicability_for_file(&file), HdcdApplicability::NotApplicable);
        assert_eq!(hdcd_status_for_file(&file), "N/A");

        file.set_probe_ready(SourceInfo {
            format_name: "flac".to_string(),
            codec: "flac".to_string(),
            sample_rate: 96_000,
            bit_depth: Some(24),
            channels: 2,
            channel_layout: "stereo".to_string(),
            duration_secs: 1.0,
            file_size: 100,
        });

        assert!(!hdcd_applicable_for_file(&file));
        assert_eq!(hdcd_status_for_file(&file), "N/A");
    }

    #[test]
    fn hdcd_status_uses_cache_only_for_confirmed_sixteen_bit_pcm() {
        let mut file = MetadataFileDetails::from_open_cache(
            PathBuf::from("/tmp/cd_rip.flac"),
            Some(100),
            None,
            None,
            None,
            None,
            FileReadState::Readable,
            FileWriteEligibility::Writable,
            super::super::probe::SourceMetadata::default(),
        );
        file.analysis_facts.hdcd_detected = Some(false);
        file.set_probe_ready(SourceInfo {
            format_name: "flac".to_string(),
            codec: "flac".to_string(),
            sample_rate: 44_100,
            bit_depth: Some(16),
            channels: 2,
            channel_layout: "stereo".to_string(),
            duration_secs: 1.0,
            file_size: 100,
        });

        assert!(hdcd_applicable_for_file(&file));
        assert_eq!(hdcd_status_for_file(&file), "Not detected");
    }

    #[test]
    fn identical_recoverable_tag_warnings_collapse_without_repeating_paths() {
        let paths = [PathBuf::from("/music/a.wv"), PathBuf::from("/music/b.wv")];
        let mut files = paths
            .iter()
            .map(|path| {
                MetadataFileDetails::from_open_cache(
                    path.clone(),
                    Some(100),
                    None,
                    None,
                    None,
                    None,
                    FileReadState::Readable,
                    FileWriteEligibility::Writable,
                    super::super::probe::SourceMetadata::default(),
                )
            })
            .collect::<Vec<_>>();
        for file in &mut files {
            let path = file.file_facts.path.clone();
            file.issues.push(MetadataIssue::RecoverableTagWarning {
                path: path.clone(),
                reason: format!(
                    "1 invalid APE key skipped in '{}': '&год'",
                    path.display()
                ),
            });
        }
        let state = MetadataEditorState::for_files(
            paths.to_vec(),
            Vec::new(),
            Vec::new(),
            super::super::app::MetadataTechnicalDetails::from_files(files.clone()),
        );

        let rows = metadata_file_issue_rows_for_files(&state, &files, true, true);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].label, "2 files");
        assert_eq!(rows[0].kind, "");
        assert_eq!(rows[0].reason, "1 invalid APE key skipped: '&год'");
        assert!(!rows[0].reason.contains("/music/"));
    }
    #[test]
    fn artwork_type_label_has_numbered_fallback() {
        assert_eq!(artwork_type_label_from_id3_code(2), "Other Icon");
        assert_eq!(artwork_type_label_from_id3_code(7), "Lead Artist");
        assert_eq!(artwork_type_label_from_id3_code(16), "Movie Screen Capture");
        assert_eq!(artwork_type_label_from_id3_code(17), "Bright Colored Fish");
        assert_eq!(artwork_type_label_from_id3_code(221), "Picture type 221");
    }


    #[test]
    fn preemphasis_status_uses_pre_flag_and_catalog_only() {
        let mut detected = MetadataFileDetails::from_open_cache(
            PathBuf::from("/tmp/detected.flac"),
            Some(100),
            None,
            None,
            None,
            None,
            FileReadState::Readable,
            FileWriteEligibility::Writable,
            super::super::probe::SourceMetadata::default(),
        );
        detected.analysis_facts.preemphasis =
            Some(crate::tui::preemphasis::PreemphasisConfidence::Detected);
        detected.analysis_facts.preemphasis_detail = Some("PRE flag".to_string());

        let mut catalog = detected.clone();
        catalog.analysis_facts.preemphasis =
            Some(crate::tui::preemphasis::PreemphasisConfidence::StrongCandidate);
        catalog.analysis_facts.preemphasis_detail = Some("catalog match: 35DP-4".to_string());

        let mut spectral_possible = detected.clone();
        spectral_possible.analysis_facts.preemphasis =
            Some(crate::tui::preemphasis::PreemphasisConfidence::Possible);
        spectral_possible.analysis_facts.preemphasis_detail =
            Some("spectral analysis suggests pre-emphasis boost may be present".to_string());

        let mut comment_tag = detected.clone();
        comment_tag.analysis_facts.preemphasis =
            Some(crate::tui::preemphasis::PreemphasisConfidence::Detected);
        comment_tag.analysis_facts.preemphasis_detail = Some("comment tag".to_string());

        let mut log_file = detected.clone();
        log_file.analysis_facts.preemphasis =
            Some(crate::tui::preemphasis::PreemphasisConfidence::Detected);
        log_file.analysis_facts.preemphasis_detail = Some("log file".to_string());

        let mut explicit_tag = detected.clone();
        explicit_tag.analysis_facts.preemphasis =
            Some(crate::tui::preemphasis::PreemphasisConfidence::Detected);
        explicit_tag.analysis_facts.preemphasis_detail = Some("tag".to_string());

        assert_eq!(preemphasis_status_for_file(&detected), "Detected (PRE flag)");
        assert_eq!(
            preemphasis_status_for_file(&catalog),
            "Candidate (catalog match: 35DP-4)"
        );
        assert_eq!(preemphasis_status_for_file(&spectral_possible), "Not detected");
        assert_eq!(preemphasis_status_for_file(&comment_tag), "Not detected");
        assert_eq!(preemphasis_status_for_file(&log_file), "Not detected");
        assert_eq!(preemphasis_status_for_file(&explicit_tag), "Detected (PRE flag)");
    }

    #[test]
    fn preemphasis_status_has_not_scanned_and_mixed_values() {
        let mut not_scanned = MetadataFileDetails::from_open_cache(
            PathBuf::from("/tmp/unscanned.flac"),
            Some(100),
            None,
            None,
            None,
            None,
            FileReadState::Readable,
            FileWriteEligibility::Writable,
            super::super::probe::SourceMetadata::default(),
        );
        assert_eq!(preemphasis_status_for_file(&not_scanned), "«not scanned»");

        not_scanned.analysis_facts.preemphasis =
            Some(crate::tui::preemphasis::PreemphasisConfidence::NotDetected);
        assert_eq!(preemphasis_status_for_file(&not_scanned), "Not detected");
    }

    #[test]
    fn hdcd_status_is_na_for_lossy_dsd_and_disc_sources_even_with_cache() {
        let cases = [
            (
                "/tmp/lossy.mp3",
                SourceInfo {
                    format_name: "mp3".to_string(),
                    codec: "mp3".to_string(),
                    sample_rate: 44_100,
                    bit_depth: None,
                    channels: 2,
                    channel_layout: "stereo".to_string(),
                    duration_secs: 1.0,
                    file_size: 100,
                },
            ),
            (
                "/tmp/dsd.dsf",
                SourceInfo {
                    format_name: "dsf".to_string(),
                    codec: "dsd".to_string(),
                    sample_rate: 2_822_400,
                    bit_depth: Some(1),
                    channels: 2,
                    channel_layout: "stereo".to_string(),
                    duration_secs: 1.0,
                    file_size: 100,
                },
            ),
            (
                "/tmp/disc.iso",
                SourceInfo {
                    format_name: "wav".to_string(),
                    codec: "pcm_s16le".to_string(),
                    sample_rate: 44_100,
                    bit_depth: Some(16),
                    channels: 2,
                    channel_layout: "stereo".to_string(),
                    duration_secs: 1.0,
                    file_size: 100,
                },
            ),
        ];

        for (path, info) in cases {
            let mut file = MetadataFileDetails::from_open_cache(
                PathBuf::from(path),
                Some(100),
                None,
                None,
                None,
                None,
                FileReadState::Readable,
                FileWriteEligibility::Writable,
                super::super::probe::SourceMetadata::default(),
            );
            file.analysis_facts.hdcd_detected = Some(false);
            file.set_probe_ready(info);

            assert!(!hdcd_applicable_for_file(&file), "{} should not be HDCD-applicable", path);
            assert_eq!(hdcd_status_for_file(&file), "N/A", "{} should render N/A", path);
        }
    }

}
