//! Unified disc browser support for the TUI.
//!
//! This module keeps Phase 4c disc-browsing logic in one place:
//! - async `DiscContents` probing for DVD-Audio and SACD sources;
//! - display formatting shared by the Browse info pane and overlay;
//! - overlay navigation state;
//! - conversion-source construction for a selected presentation.
//!
//! The functions here intentionally avoid UI side effects except for
//! spawning a probe message. Renderers and key handlers own state mutation.

use std::fs::{self, Metadata};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

use tokio::sync::mpsc;

use crate::disc::{
    DiscContents, DiscFormat, DiscPresentation, DiscTrack, PresentationId, SacdAreaId,
};
use crate::tui::app::{MultiTrackEntry, SourceMode};
use crate::tui::message::AppMessage;
use crate::tui::probe::{SourceInfo, SourceMetadata};


/// Stable metadata snapshot for a single filesystem object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileProbeFingerprint {
    pub len: u64,
    pub modified: Option<SystemTime>,
    /// Platform file identity. On Unix this is `(dev, ino)`. On platforms
    /// without a cheap stable identity this is `None`, so `len + modified`
    /// still force re-probe on normal file replacement or rewrite.
    pub platform_id: Option<(u64, u64)>,
}

impl FileProbeFingerprint {
    fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            platform_id: platform_file_identity(metadata),
        }
    }
}

/// Metadata snapshot used to decide whether a cached disc parse is still valid.
///
/// ISO files use the image file metadata. Directory DVD-Audio, DVD-Video, and
/// Blu-ray sources also carry their marker metadata because directory mtimes
/// alone are not a reliable proxy for disc-content changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscProbeFingerprint {
    pub source: FileProbeFingerprint,
    pub is_dir: bool,
    pub dvda_audio_ts_ifo: Option<FileProbeFingerprint>,
    pub dvdv_video_ts_ifo: Option<FileProbeFingerprint>,
    pub bluray_bdmv_index: Option<FileProbeFingerprint>,
}

impl DiscProbeFingerprint {
    /// Metadata most representative of disc-content identity. For ISO files this
    /// is the image itself; for disc directories this prefers the format marker
    /// file, because directory mtimes do not reliably change when a marker is
    /// replaced in place.
    pub fn primary_content(&self) -> &FileProbeFingerprint {
        self.dvda_audio_ts_ifo
            .as_ref()
            .or(self.dvdv_video_ts_ifo.as_ref())
            .or(self.bluray_bdmv_index.as_ref())
            .unwrap_or(&self.source)
    }

    pub fn primary_len(&self) -> u64 {
        self.primary_content().len
    }

    pub fn primary_modified(&self) -> Option<SystemTime> {
        self.primary_content().modified.clone()
    }
}

/// Parsed-disc cache entry with source identity and either a success or error.
///
/// Both successes and failures carry the same metadata fingerprint. A cached
/// error is only authoritative while that fingerprint still matches; replacing
/// an ISO or changing a disc-directory marker makes the entry stale and allows
/// normal probing again. Explicit re-probe actions
/// bypass this entry by removing it before scheduling a new probe.
#[derive(Debug, Clone)]
pub struct DiscProbeCacheEntry {
    pub fingerprint: DiscProbeFingerprint,
    /// Convenience copy of the primary content length used by diagnostics and
    /// tests. For DVD-Audio directories this is the IFO length when present,
    /// otherwise the directory metadata length.
    pub len: u64,
    /// Convenience copy of the primary content mtime. For DVD-Audio directories
    /// this is the IFO mtime when present.
    pub modified: Option<SystemTime>,
    pub result: Result<Arc<DiscContents>, String>,
}

impl DiscProbeCacheEntry {
    pub fn from_result(
        fingerprint: DiscProbeFingerprint,
        result: Result<DiscContents, String>,
    ) -> Self {
        let len = fingerprint.primary_len();
        let modified = fingerprint.primary_modified();
        let result = result.map(Arc::new);
        Self {
            fingerprint,
            len,
            modified,
            result,
        }
    }

    pub fn from_success(fingerprint: DiscProbeFingerprint, contents: DiscContents) -> Self {
        Self::from_result(fingerprint, Ok(contents))
    }

    pub fn from_error(fingerprint: DiscProbeFingerprint, error: String) -> Self {
        Self::from_result(fingerprint, Err(error))
    }

    /// Backwards-compatible constructor for existing call sites.
    pub fn new(fingerprint: DiscProbeFingerprint, contents: DiscContents) -> Self {
        Self::from_success(fingerprint, contents)
    }

    pub fn is_current_for(&self, path: &Path) -> bool {
        disc_probe_fingerprint(path)
            .map(|current| current == self.fingerprint)
            .unwrap_or(false)
    }

    pub fn contents_if_current(&self, path: &Path) -> Option<Arc<DiscContents>> {
        if !self.is_current_for(path) {
            return None;
        }
        match &self.result {
            Ok(contents) => Some(Arc::clone(contents)),
            Err(_) => None,
        }
    }

    pub fn error_if_current<'a>(&'a self, path: &Path) -> Option<&'a str> {
        if !self.is_current_for(path) {
            return None;
        }
        match &self.result {
            Ok(_) => None,
            Err(error) => Some(error.as_str()),
        }
    }
}

/// Snapshot the filesystem metadata used to validate parsed disc contents.
pub fn disc_probe_fingerprint(path: &Path) -> Result<DiscProbeFingerprint, String> {
    let metadata = fs::metadata(path)
        .map_err(|e| format!("Disc source metadata unavailable for '{}': {e}", path.display()))?;
    let is_dir = metadata.is_dir();
    let dvda_audio_ts_ifo = if is_dir {
        let marker = path.join("AUDIO_TS").join("AUDIO_TS.IFO");
        fs::metadata(&marker).ok().map(|m| FileProbeFingerprint::from_metadata(&m))
    } else {
        None
    };
    let dvdv_video_ts_ifo = if is_dir {
        crate::disc::dvdv_utils::directory_video_ts_file_path(path, "VIDEO_TS.IFO")
            .and_then(|marker| fs::metadata(marker).ok())
            .map(|m| FileProbeFingerprint::from_metadata(&m))
    } else {
        None
    };
    let bluray_bdmv_index = if is_dir {
        crate::disc::bluray_utils::bluray_directory_marker_path(path, "index.bdmv")
            .and_then(|marker| fs::metadata(marker).ok())
            .map(|m| FileProbeFingerprint::from_metadata(&m))
    } else {
        None
    };

    Ok(DiscProbeFingerprint {
        source: FileProbeFingerprint::from_metadata(&metadata),
        is_dir,
        dvda_audio_ts_ifo,
        dvdv_video_ts_ifo,
        bluray_bdmv_index,
    })
}

#[cfg(unix)]
fn platform_file_identity(metadata: &Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    Some((metadata.dev(), metadata.ino()))
}

#[cfg(not(unix))]
fn platform_file_identity(_metadata: &Metadata) -> Option<(u64, u64)> {
    None
}

/// Modal overlay state for browsing the presentations on one disc source.
#[derive(Debug, Clone)]
pub struct DiscBrowserState {
    pub contents: DiscContents,
    pub cursor: usize,
    pub expanded: Vec<bool>,
    pub selected: Vec<bool>,
    /// Vertical scroll offset in the flattened overlay row list. The flattened
    /// list contains presentation rows and, for expanded presentations, track
    /// rows. It is deliberately not a presentation index.
    pub scroll: usize,
    pub source_path: PathBuf,
}

/// One logical row in the Audio Streams overlay after expansion is applied.
///
/// Rendering, clipping, and mouse target registration all consume this same
/// flattened list. That prevents row-coordinate drift when an expanded stream
/// contributes many track rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscBrowserVisibleRow {
    Presentation { index: usize },
    Track { presentation_index: usize, track_index: usize },
}

impl DiscBrowserState {
    pub fn new(contents: DiscContents, source_path: PathBuf) -> Self {
        let len = contents.presentations.len();
        Self {
            contents,
            cursor: 0,
            expanded: vec![false; len],
            selected: vec![false; len],
            scroll: 0,
            source_path,
        }
    }

    pub fn len(&self) -> usize {
        self.contents.presentations.len()
    }

    pub fn is_empty(&self) -> bool {
        self.contents.presentations.is_empty()
    }

    pub fn selected_presentation(&self) -> Option<&DiscPresentation> {
        self.contents.presentations.get(self.cursor)
    }

    pub fn move_cursor(&mut self, delta: isize) {
        let len = self.len();
        if len == 0 {
            self.cursor = 0;
            self.scroll = 0;
            return;
        }
        let max = len - 1;
        self.cursor = if delta < 0 {
            self.cursor.saturating_sub(delta.unsigned_abs())
        } else {
            self.cursor.saturating_add(delta as usize).min(max)
        };
    }

    pub fn toggle_expanded(&mut self, index: usize) {
        if let Some(expanded) = self.expanded.get_mut(index) {
            *expanded = !*expanded;
        }
    }

    pub fn toggle_selected(&mut self, index: usize) {
        if let Some(selected) = self.selected.get_mut(index) {
            *selected = !*selected;
        }
    }

    pub fn set_cursor(&mut self, index: usize) {
        if index < self.len() {
            self.cursor = index;
        }
    }

    pub fn selected_indices(&self) -> Vec<usize> {
        self.selected
            .iter()
            .enumerate()
            .filter_map(|(idx, selected)| (*selected).then_some(idx))
            .collect()
    }

    pub fn scroll_by_rows(&mut self, delta: isize) {
        if delta < 0 {
            self.scroll = self.scroll.saturating_sub(delta.unsigned_abs());
        } else {
            self.scroll = self.scroll.saturating_add(delta as usize);
        }
    }
}

/// Return the overlay's flattened row list.
///
/// The order of this list is the single source of truth for scroll clipping,
/// drawing, and mouse registration. Presentation rows are selectable; track rows
/// are display-only detail rows.
pub fn disc_browser_visible_rows(state: &DiscBrowserState) -> Vec<DiscBrowserVisibleRow> {
    let mut rows = Vec::new();
    for (presentation_index, presentation) in state.contents.presentations.iter().enumerate() {
        rows.push(DiscBrowserVisibleRow::Presentation {
            index: presentation_index,
        });

        if state.expanded.get(presentation_index).copied().unwrap_or(false) {
            rows.extend((0..presentation.tracks.len()).map(|track_index| {
                DiscBrowserVisibleRow::Track {
                    presentation_index,
                    track_index,
                }
            }));
        }
    }
    rows
}

/// Find the logical row occupied by the selected presentation.
pub fn cursor_row_index(rows: &[DiscBrowserVisibleRow], cursor: usize) -> usize {
    rows.iter()
        .position(|row| matches!(row, DiscBrowserVisibleRow::Presentation { index } if *index == cursor))
        .unwrap_or(0)
}

/// Clamp the requested scroll offset for a viewport and keep the cursor visible.
///
/// This function is intentionally pure so both the renderer and tests can use
/// the same behavior without requiring a terminal frame.
pub fn scroll_for_viewport(
    requested_scroll: usize,
    cursor_row: usize,
    row_count: usize,
    viewport_height: usize,
) -> usize {
    if row_count == 0 || viewport_height == 0 {
        return 0;
    }

    let max_scroll = row_count.saturating_sub(viewport_height);
    let mut scroll = requested_scroll.min(max_scroll);
    if cursor_row < scroll {
        scroll = cursor_row;
    } else if cursor_row >= scroll.saturating_add(viewport_height) {
        scroll = cursor_row.saturating_add(1).saturating_sub(viewport_height);
    }
    scroll.min(max_scroll)
}

/// Spawn an asynchronous parse/map probe for a disc source.
pub fn spawn_disc_probe(
    path: PathBuf,
    cancel: Arc<AtomicBool>,
    tx: mpsc::Sender<AppMessage>,
) {
    tokio::spawn(async move {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let fingerprint = match disc_probe_fingerprint(&path) {
            Ok(fingerprint) => fingerprint,
            Err(err) => {
                if !cancel.load(Ordering::Relaxed) {
                    let _ = tx
                        .send(AppMessage::DiscProbeComplete {
                            path,
                            fingerprint: None,
                            result: Box::new(Err(err)),
                        })
                        .await;
                }
                return;
            }
        };

        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let probe_path = path.clone();
        let cancel_for_task = cancel.clone();
        let result = match tokio::task::spawn_blocking(move || {
            if cancel_for_task.load(Ordering::Relaxed) {
                return Err("disc probe cancelled".to_string());
            }
            let result = probe_disc_contents(&probe_path);
            if cancel_for_task.load(Ordering::Relaxed) {
                Err("disc probe cancelled".to_string())
            } else {
                result
            }
        }).await {
            Ok(result) => result,
            Err(err) => Err(format!("Disc probe task failed: {err}")),
        };
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let _ = tx
            .send(AppMessage::DiscProbeComplete {
                path,
                fingerprint: Some(fingerprint),
                result: Box::new(result),
            })
            .await;
    });
}

/// Parse a disc source and map it to the unified browsing model.
pub fn probe_disc_contents(path: &Path) -> Result<DiscContents, String> {
    if crate::tui::sacd::is_sacd_iso(path) {
        return probe_sacd_contents(path);
    }
    if crate::disc::dvda_utils::is_dvda_source(path) {
        return crate::disc::dvda_utils::map_dvda_source(path);
    }
    if crate::disc::dvdv_utils::is_dvdv_source(path) {
        return crate::disc::dvdv_utils::map_dvdv_source(path);
    }
    if crate::disc::bluray_utils::is_bluray_source(path) {
        return crate::disc::bluray_utils::map_bluray_source(path);
    }
    Err(format!("Not a supported browsable disc source: {}", path.display()))
}

fn probe_sacd_contents(path: &Path) -> Result<DiscContents, String> {
    let metadata = crate::tui::sacd::parse_sacd_iso(path)
        .map_err(|e| format!("SACD parse failed for '{}': {e}", path.display()))?;
    let sidecar = crate::tui::sacd_sidecar::find_sidecar_for_iso(path)
        .and_then(|p| crate::tui::sacd_sidecar::parse_sidecar(&p).ok());
    Ok(crate::disc::sacd_mapper::map_sacd_disc(
        &metadata,
        sidecar.as_ref(),
        path,
    ))
}

/// Compact disc summary for info panes.
pub fn disc_summary(contents: &DiscContents) -> String {
    let stream_count = contents.presentations.len();
    let track_count: usize = contents.presentations.iter().map(|p| p.tracks.len()).sum();
    let stereo_count = contents
        .presentations
        .iter()
        .filter(|presentation| presentation_is_stereo(presentation))
        .count();
    let multichannel_count = stream_count.saturating_sub(stereo_count);
    format!(
        "{} audio {} · {} {} · {} multichannel · {} stereo",
        stream_count,
        plural(stream_count, "stream", "streams"),
        track_count,
        plural(track_count, "track", "tracks"),
        multichannel_count,
        stereo_count,
    )
}

pub fn disc_content_summary_lines(contents: &DiscContents) -> Vec<String> {
    let stream_count = contents.presentations.len();
    let track_count: usize = contents.presentations.iter().map(|p| p.tracks.len()).sum();
    let stereo_count = contents
        .presentations
        .iter()
        .filter(|presentation| presentation_is_stereo(presentation))
        .count();
    let multichannel_count = stream_count.saturating_sub(stereo_count);
    vec![
        format!(
            "content: {} audio {} · {} {}",
            stream_count,
            plural(stream_count, "stream", "streams"),
            track_count,
            plural(track_count, "track", "tracks"),
        ),
        format!("         {multichannel_count} multichannel · {stereo_count} stereo"),
    ]
}

pub fn disc_stream_summary_lines(contents: &DiscContents, limit: usize) -> Vec<String> {
    let mut presentations: Vec<&DiscPresentation> = contents.presentations.iter().collect();
    presentations.sort_by(|left, right| compare_disc_presentations(left, right));
    presentations
        .into_iter()
        .take(limit)
        .map(|presentation| disc_stream_display(contents.format, presentation))
        .collect()
}

/// Summary row for a single presentation, without the leading row marker.
pub fn presentation_summary(index: usize, presentation: &DiscPresentation) -> String {
    let track_count = presentation.tracks.len();
    let mut suffix = format!("{} {}", track_count, plural(track_count, "track", "tracks"));
    if presentation.total_duration_secs > 0.0 {
        suffix.push_str(", ");
        suffix.push_str(&duration_display(presentation.total_duration_secs));
    }
    format!("Stream {}: {} ({})", index + 1, format_note(presentation), suffix)
}

fn compare_disc_presentations(left: &DiscPresentation, right: &DiscPresentation) -> std::cmp::Ordering {
    disc_codec_priority(left)
        .cmp(&disc_codec_priority(right))
        .then_with(|| presentation_channel_priority(left).cmp(&presentation_channel_priority(right)))
        .then_with(|| presentation_bit_depth_priority(left).cmp(&presentation_bit_depth_priority(right)))
        .then_with(|| presentation_sample_rate_priority(left).cmp(&presentation_sample_rate_priority(right)))
        .then_with(|| left.label.cmp(&right.label))
}

fn disc_codec_priority(presentation: &DiscPresentation) -> u8 {
    let codec = presentation
        .format
        .codec
        .as_deref()
        .unwrap_or(&presentation.label)
        .to_ascii_lowercase();
    if codec.contains("lpcm") || codec.contains("pcm") {
        0
    } else if codec.contains("truehd") {
        1
    } else if codec.contains("dts-hd") || codec.contains("dts hd") {
        2
    } else if codec.contains("dts") {
        3
    } else if codec.contains("ac3") || codec.contains("dolby digital") {
        4
    } else {
        5
    }
}

fn presentation_channel_priority(presentation: &DiscPresentation) -> u8 {
    if presentation_is_stereo(presentation) { 0 } else { 1 }
}

fn presentation_bit_depth_priority(presentation: &DiscPresentation) -> std::cmp::Reverse<u32> {
    std::cmp::Reverse(presentation.format.bit_depth.unwrap_or(0).into())
}

fn presentation_sample_rate_priority(presentation: &DiscPresentation) -> std::cmp::Reverse<u32> {
    std::cmp::Reverse(presentation.format.sample_rate.unwrap_or(0))
}

fn presentation_is_stereo(presentation: &DiscPresentation) -> bool {
    if let Some(layout) = &presentation.format.channel_layout {
        let lower = layout.to_ascii_lowercase();
        if lower.contains("stereo") || lower == "2.0" || lower == "2ch" {
            return true;
        }
    }
    matches!(presentation.format.channels, Some(channels) if channels <= 2)
}

fn disc_stream_display(format: DiscFormat, presentation: &DiscPresentation) -> String {
    if matches!(format, DiscFormat::Sacd) {
        let layout = presentation
            .format
            .channel_layout
            .clone()
            .or_else(|| presentation.format.channels.map(|channels| format!("{channels}ch")))
            .unwrap_or_else(|| presentation.label.clone());
        return format!("DSD 2.8MHz {layout}");
    }

    let fmt = &presentation.format;
    let mut parts = Vec::new();
    if let Some(codec) = &fmt.codec {
        parts.push(codec.clone());
    } else {
        parts.push(presentation.label.clone());
    }
    if let Some(bits) = fmt.bit_depth {
        parts.push(format!("{bits}-bit"));
    }
    if let Some(rate) = fmt.sample_rate {
        parts.push(sample_rate_display(rate));
    }
    if let Some(layout) = &fmt.channel_layout {
        parts.push(layout.clone());
    } else if let Some(channels) = fmt.channels {
        parts.push(format!("{channels}ch"));
    }
    parts.join(" ")
}

pub fn format_note(presentation: &DiscPresentation) -> String {
    let fmt = &presentation.format;
    let mut parts = Vec::new();
    if let Some(codec) = &fmt.codec {
        parts.push(codec.clone());
    }
    if let Some(rate) = fmt.sample_rate {
        parts.push(sample_rate_display(rate));
    }
    if let Some(bits) = fmt.bit_depth {
        parts.push(format!("{bits}-bit"));
    }
    if let Some(layout) = &fmt.channel_layout {
        parts.push(layout.clone());
    } else if let Some(channels) = fmt.channels {
        parts.push(format!("{channels}ch"));
    }
    if parts.is_empty() {
        presentation.label.clone()
    } else {
        parts.join("/")
    }
}

pub fn track_summary(track: &DiscTrack) -> String {
    let title = track
        .title
        .as_ref()
        .filter(|s| !s.trim().is_empty())
        .cloned()
        .unwrap_or_else(|| format!("Track {:02}", track.number));
    match track.duration_secs {
        Some(secs) if secs > 0.0 => format!("{:>2}. {:<36} {}", track.number, title, duration_display(secs)),
        _ => format!("{:>2}. {title}", track.number),
    }
}

pub fn duration_display(seconds: f64) -> String {
    let total = seconds.max(0.0).round() as u64;
    let hours = total / 3600;
    let mins = (total % 3600) / 60;
    let secs = total % 60;
    if hours > 0 {
        format!("{hours}:{mins:02}:{secs:02}")
    } else {
        format!("{mins}:{secs:02}")
    }
}

pub fn sample_rate_display(rate: u32) -> String {
    if rate >= 1000 {
        let whole = rate / 1000;
        let frac = (rate % 1000) / 100;
        if frac == 0 {
            format!("{whole}kHz")
        } else {
            format!("{whole}.{frac}kHz")
        }
    } else {
        format!("{rate}Hz")
    }
}

fn plural<'a>(n: usize, one: &'a str, many: &'a str) -> &'a str {
    if n == 1 { one } else { many }
}

/// Build the existing MultiTrack source state for one selected presentation.
///
/// Phase 4c intentionally reuses MultiTrack for source-pane rendering. The
/// selected presentation id must also flow into queue construction; the patch
/// adds optional disc fields to the `MultiTrack` variant for that bridge.
pub fn source_mode_for_presentation(
    contents: DiscContents,
    presentation_index: usize,
    metadata: SourceMetadata,
) -> Result<SourceMode, String> {
    let presentation = contents
        .presentations
        .get(presentation_index)
        .ok_or_else(|| format!("No stream at index {}", presentation_index + 1))?
        .clone();

    let tracks: Vec<MultiTrackEntry> = presentation
        .tracks
        .iter()
        .map(|track| MultiTrackEntry {
            number: track.number,
            title: track.title.clone(),
            performer: track.performer.clone(),
            duration_display: track.duration_secs.map(duration_display),
        })
        .collect();

    let track_count = tracks.len();
    let info = source_info_for_presentation(&contents, &presentation);

    Ok(SourceMode::MultiTrack {
        path: contents.source_path.clone(),
        info: Some(info),
        metadata,
        tracks,
        area_label: Some(presentation.label.clone()),
        album_title: presentation.album_title.clone().or_else(|| contents.album_title.clone()),
        album_artist: presentation.album_artist.clone().or_else(|| contents.album_artist.clone()),
        probe_notice: None,
        scroll: 0,
        cursor: 0,
        selected: vec![true; track_count],
        disc_contents: Some(Box::new(contents)),
        selected_presentation_id: Some(presentation.id),
        archive_preview: None,
    })
}

pub fn source_info_for_presentation(
    contents: &DiscContents,
    presentation: &DiscPresentation,
) -> SourceInfo {
    let file_size = std::fs::metadata(&contents.source_path).map(|m| m.len()).unwrap_or(0);
    let fmt = &presentation.format;
    SourceInfo {
        format_name: match contents.format {
            DiscFormat::DvdAudio => "DVD-Audio".to_string(),
            DiscFormat::Sacd => "SACD ISO".to_string(),
            DiscFormat::DvdVideo => "DVD-Video".to_string(),
            DiscFormat::BluRay => "Blu-ray".to_string(),
        },
        codec: fmt.codec.clone().unwrap_or_else(|| presentation.label.clone()),
        bit_depth: fmt.bit_depth,
        sample_rate: fmt.sample_rate.unwrap_or(0),
        channels: fmt.channels.map(u32::from).unwrap_or(0),
        channel_layout: fmt.channel_layout.clone().unwrap_or_default(),
        duration_secs: presentation.total_duration_secs,
        file_size,
    }
}

/// Build a metadata object from disc-level labels without inventing track tags.
pub fn metadata_for_disc(contents: &DiscContents) -> SourceMetadata {
    metadata_for_disc_presentation(contents, None)
}

pub fn metadata_for_disc_presentation(
    contents: &DiscContents,
    presentation: Option<&DiscPresentation>,
) -> SourceMetadata {
    let mut metadata = SourceMetadata::default();
    // Per-presentation metadata takes priority over disc-level.
    metadata.album = presentation
        .and_then(|p| p.album_title.clone())
        .or_else(|| contents.album_title.clone());
    metadata.artist = presentation
        .and_then(|p| p.album_artist.clone())
        .or_else(|| contents.album_artist.clone());
    metadata.genre = presentation
        .and_then(|p| p.genre.clone())
        .or_else(|| contents.genre.clone());
    metadata.year = presentation
        .and_then(|p| p.year.clone())
        .or_else(|| contents.year.clone());
    metadata
}

/// Convert a selected presentation id to a short user-facing label.
pub fn presentation_id_label(id: &PresentationId) -> String {
    id.display_label()
}

/// Return whether the current conversion pipeline can honor a specific
/// selected disc-presentation identity. Keeping this as a single predicate
/// keeps context-menu, overlay, and source-option bridge behavior aligned.
#[must_use]
pub fn presentation_id_supports_stream_conversion(id: &PresentationId) -> bool {
    match id {
        PresentationId::DvdAudioGroup(_)
        | PresentationId::DvdVideoTitle { .. }
        | PresentationId::SacdArea(_)
        | PresentationId::BluRayTitle { .. } => true,
    }
}

/// Return whether a presentation row can be loaded as an explicit stream
/// conversion from UI affordances such as `Convert Stream` and `Enter Convert`.
#[must_use]
pub fn presentation_supports_stream_conversion(presentation: &DiscPresentation) -> bool {
    presentation_id_supports_stream_conversion(&presentation.id)
}


/// One-shot action to perform after an async disc probe has populated a
/// current `DiscProbeCacheEntry`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscProbeFollowup {
    /// Open the Audio Streams overlay once the selected disc probe completes.
    OpenDiscBrowser,
    /// Load the scored default presentation into the Convert screen.
    ConvertDefaultStream,
}

/// Apply a selected presentation to the extraction `SourceOptions` used by the
/// conversion pipeline. This is the critical bridge from TUI stream selection
/// to the actual demux/materialize stage.
pub fn apply_presentation_to_source_options(
    options: &mut crate::convert::pipeline::SourceOptions,
    id: &PresentationId,
) -> bool {
    match id {
        PresentationId::DvdAudioGroup(group) => {
            options.dvda_group_selection =
                crate::convert::pipeline::DvdaGroupSelection::Group(*group);
            true
        }
        PresentationId::DvdVideoTitle { vts_number, title_number, audio_stream_index } => {
            options.dvdv_vts = Some(*vts_number);
            options.dvdv_title = Some(*title_number);
            options.dvdv_audio_stream = Some(*audio_stream_index);
            options.dvdv_angle = Some(1);
            true
        }
        PresentationId::SacdArea(SacdAreaId::Stereo) => {
            options.sacd_area = Some(crate::convert::pipeline::SacdArea::Stereo);
            true
        }
        PresentationId::SacdArea(SacdAreaId::MultiChannel) => {
            options.sacd_area = Some(crate::convert::pipeline::SacdArea::MultiChannel);
            true
        }
        PresentationId::BluRayTitle {
            playlist_number,
            audio_pid,
            audio_stream_index,
            display_angle,
        } => {
            options.bluray_playlist = Some(*playlist_number);
            options.bluray_audio_pid = Some(*audio_pid);
            options.bluray_audio_stream = Some(*audio_stream_index);
            options.bluray_angle = Some(display_angle.get());
            true
        }
    }
}

/// Apply any selected disc presentation stored on `SourceMode::MultiTrack` to
/// the pipeline source options. Call this at the ConversionItem/PipelineRequest
/// construction boundary after creating the normal source options.
pub fn apply_source_mode_disc_selection_to_source_options(
    mode: &SourceMode,
    options: &mut crate::convert::pipeline::SourceOptions,
) -> bool {
    if let SourceMode::MultiTrack {
        selected_presentation_id: Some(id),
        ..
    } = mode
    {
        apply_presentation_to_source_options(options, id)
    } else {
        false
    }
}

/// Draw the Audio Streams overlay and register mouse targets.
pub fn draw_disc_browser(
    f: &mut ratatui::Frame,
    state: &DiscBrowserState,
    buttons: &mut crate::tui::button_map::ButtonRenderMap,
    theme: super::theme::Theme,
) {
    use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
    use ratatui::style::{Modifier, Style};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::{Block, Borders, Clear, Paragraph};

    let area = f.size();
    let width_pct = if area.width >= 100 { 70 } else { 90 };
    let height_pct = if area.height >= 36 { 70 } else { 86 };
    let popup = centered_rect(width_pct, height_pct, area);

    f.render_widget(Clear, popup);
    let title = state
        .source_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| state.source_path.display().to_string());
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.purple))
        .title(Span::styled(
            format!(" Audio Streams: {title} "),
            Style::default()
                .fg(theme.purple)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);

    let visible_rows = chunks[0].height as usize;
    let cursor = state.cursor.min(state.len().saturating_sub(1));
    let rows = disc_browser_visible_rows(state);
    let cursor_row = cursor_row_index(&rows, cursor);
    let scroll = scroll_for_viewport(state.scroll, cursor_row, rows.len(), visible_rows);
    let content_width = chunks[0].width.saturating_sub(1) as usize;

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut row_targets: Vec<(usize, u16)> = Vec::new();
    for (visible_offset, row) in rows
        .iter()
        .copied()
        .skip(scroll)
        .take(visible_rows)
        .enumerate()
    {
        let row_y = chunks[0].y + visible_offset as u16;
        match row {
            DiscBrowserVisibleRow::Presentation { index } => {
                let Some(presentation) = state.contents.presentations.get(index) else {
                    continue;
                };
                row_targets.push((index, row_y));
                let selected = index == cursor;
                let expanded = state.expanded.get(index).copied().unwrap_or(false);
                let checked = state.selected.get(index).copied().unwrap_or(false);
                let marker = if selected { "▶" } else { " " };
                let disclosure = if expanded { "▾" } else { "▸" };
                let check = if checked { "●" } else { "○" };
                let style = if selected {
                    Style::default()
                        .fg(theme.pill_active_fg)
                        .bg(theme.purple)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.text)
                };
                let text = format!(
                    " {marker} {disclosure} {check} {}",
                    presentation_summary(index, presentation)
                );
                lines.push(Line::from(vec![Span::styled(truncate_text(&text, content_width), style)]));
            }
            DiscBrowserVisibleRow::Track {
                presentation_index,
                track_index,
            } => {
                let Some(track) = state
                    .contents
                    .presentations
                    .get(presentation_index)
                    .and_then(|presentation| presentation.tracks.get(track_index))
                else {
                    continue;
                };
                let text = format!("      {}", track_summary(track));
                lines.push(Line::from(vec![Span::styled(
                    truncate_text(&text, content_width),
                    Style::default().fg(theme.text_dim),
                )]));
            }
        }
    }

    let paragraph = Paragraph::new(lines);
    f.render_widget(paragraph, chunks[0]);

    for (index, y) in row_targets {
        buttons.record_button(
            crate::tui::button_map::TuiButton::DiscBrowserStream(index),
            Rect::new(chunks[0].x, y, chunks[0].width, 1),
        );
        buttons.record_button(
            crate::tui::button_map::TuiButton::DiscBrowserExpand(index),
            Rect::new(chunks[0].x + 3, y, 2, 1),
        );
    }

    let can_convert_selected = state
        .selected_presentation()
        .is_some_and(presentation_supports_stream_conversion);
    let convert_footer_label = if can_convert_selected {
        "Enter Convert"
    } else {
        "Stream Convert N/A"
    };
    let footer = Line::from(vec![
        footer_pill(convert_footer_label, theme.purple, theme),
        Span::raw("  "),
        footer_pill("E Expand", theme.purple, theme),
        Span::raw("  "),
        footer_pill("Space Select", theme.purple, theme),
        Span::raw("  "),
        footer_pill("Esc Close", theme.purple, theme),
    ]);
    f.render_widget(Paragraph::new(footer).alignment(Alignment::Center), chunks[1]);

    let footer_y = chunks[1].y;
    let footer_w = chunks[1].width;
    let convert_w = convert_footer_label.chars().count() as u16;
    let gap_w = 2u16;
    let expand_w = "E Expand".len() as u16;
    let select_w = "Space Select".len() as u16;
    let close_w = "Esc Close".len() as u16;
    let total_w = convert_w + expand_w + select_w + close_w + gap_w * 3;
    let center_x = chunks[1].x + footer_w.saturating_sub(total_w) / 2;
    if can_convert_selected {
        buttons.record_button(
            crate::tui::button_map::TuiButton::DiscBrowserConvert,
            Rect::new(center_x, footer_y, convert_w, 1),
        );
    }
    buttons.record_button(
        crate::tui::button_map::TuiButton::DiscBrowserClose,
        Rect::new(
            center_x
                .saturating_add(convert_w)
                .saturating_add(gap_w)
                .saturating_add(expand_w)
                .saturating_add(gap_w)
                .saturating_add(select_w)
                .saturating_add(gap_w),
            footer_y,
            close_w,
            1,
        ),
    );
}

fn centered_rect(percent_x: u16, percent_y: u16, area: ratatui::layout::Rect) -> ratatui::layout::Rect {
    use ratatui::layout::{Constraint, Direction, Layout};

    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);

    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1]);

    horizontal[1]
}

fn truncate_text(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let mut chars = text.chars();
    let truncated: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() && max_chars > 1 {
        let mut with_ellipsis: String = truncated.chars().take(max_chars - 1).collect();
        with_ellipsis.push('…');
        with_ellipsis
    } else {
        truncated
    }
}

fn footer_pill(label: &str, bg: ratatui::style::Color, theme: super::theme::Theme) -> ratatui::text::Span<'static> {
    ratatui::text::Span::styled(
        format!(" {label} "),
        ratatui::style::Style::default()
            .fg(theme.pill_active_fg)
            .bg(bg)
            .add_modifier(ratatui::style::Modifier::BOLD),
    )
}

#[cfg(test)]
mod cache_tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_path(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("dawdiolab-phase4c-{label}-{nanos}"))
    }

    #[test]
    fn iso_cache_error_becomes_stale_when_file_len_changes() {
        let path = unique_path("iso-cache-error");
        fs::write(&path, b"old").expect("write initial image");
        let fingerprint = disc_probe_fingerprint(&path).expect("fingerprint initial image");
        let entry = DiscProbeCacheEntry::from_error(fingerprint, "parse failed".to_string());

        assert_eq!(entry.error_if_current(&path), Some("parse failed"));

        fs::write(&path, b"replacement image bytes").expect("replace image");
        assert!(entry.error_if_current(&path).is_none());
        assert!(!entry.is_current_for(&path));

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn dvda_directory_fingerprint_tracks_audio_ts_ifo() {
        let root = unique_path("dvda-dir-cache");
        let audio_ts = root.join("AUDIO_TS");
        fs::create_dir_all(&audio_ts).expect("create AUDIO_TS");
        let ifo = audio_ts.join("AUDIO_TS.IFO");
        fs::write(&ifo, b"ifo-v1").expect("write initial IFO");

        let first = disc_probe_fingerprint(&root).expect("fingerprint DVD-Audio dir");
        assert!(first.is_dir);
        assert!(first.dvda_audio_ts_ifo.is_some());

        fs::write(&ifo, b"ifo-v2-with-different-length").expect("replace IFO");
        let second = disc_probe_fingerprint(&root).expect("fingerprint replaced DVD-Audio dir");
        assert_ne!(first, second);
        assert_ne!(first.primary_len(), second.primary_len());

        let _ = fs::remove_dir_all(&root);
    }


    #[test]
    fn dvdv_directory_fingerprint_tracks_lowercase_video_ts_ifo() {
        let root = unique_path("dvdv-dir-cache");
        let video_ts = root.join("video_ts");
        fs::create_dir_all(&video_ts).expect("create lowercase VIDEO_TS");
        let ifo = video_ts.join("video_ts.ifo");
        fs::write(&ifo, b"DVDVIDEO-VMG-v1").expect("write initial IFO");

        let first = disc_probe_fingerprint(&root).expect("fingerprint DVD-Video dir");
        assert!(first.is_dir);
        assert!(first.dvdv_video_ts_ifo.is_some());

        fs::write(&ifo, b"DVDVIDEO-VMG-v2-with-different-length").expect("replace IFO");
        let second = disc_probe_fingerprint(&root).expect("fingerprint replaced DVD-Video dir");
        assert_ne!(first, second);
        assert_ne!(first.primary_len(), second.primary_len());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn bluray_directory_fingerprint_tracks_bdmv_index_case_insensitively() {
        let root = unique_path("bluray-dir-cache");
        let bdmv = root.join("BDMV");
        fs::create_dir_all(&bdmv).expect("create BDMV");
        let index = bdmv.join("INDEX.BDMV");
        fs::write(&index, b"bdmv-index-v1").expect("write initial index");

        let first = disc_probe_fingerprint(&root).expect("fingerprint Blu-ray dir");
        assert!(first.is_dir);
        assert!(first.bluray_bdmv_index.is_some());

        fs::write(&index, b"bdmv-index-v2-with-different-length").expect("replace index");
        let second = disc_probe_fingerprint(&root).expect("fingerprint replaced Blu-ray dir");
        assert_ne!(first, second);
        assert_ne!(first.primary_len(), second.primary_len());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn selected_dvd_video_presentation_sets_vts_title_and_stream() {
        let mut options = crate::convert::pipeline::SourceOptions {
            archive_password: None,
            sacd_area: None,
            dvda_group_selection: crate::convert::pipeline::DvdaGroupSelection::Default,
            dvda_group: None,
            dvda_assume_decrypted: false,
            dvda_downmix_policy: crate::convert::pipeline::DvdaDownmixPolicy::Auto,
            dvdv_vts: None,
            dvdv_title: None,
            dvdv_audio_stream: None,
            dvdv_angle: None,
            bluray_playlist: None,
            bluray_audio_pid: None,
            bluray_audio_stream: None,
            bluray_angle: None,
            cue_sidecar: crate::convert::pipeline::CueSidecarPolicy::PreferSidecar,
            track_selection: crate::convert::pipeline::TrackSelection::All,
        };
        assert!(apply_presentation_to_source_options(
            &mut options,
            &PresentationId::dvd_video(2, 7, 3),
        ));
        assert_eq!(options.dvdv_vts, Some(2));
        assert_eq!(options.dvdv_title, Some(7));
        assert_eq!(options.dvdv_audio_stream, Some(3));
        assert_eq!(options.dvdv_angle, Some(1));
    }

    #[test]
    fn selected_dvd_audio_presentation_sets_explicit_group_selection() {
        let mut options = crate::convert::pipeline::SourceOptions {
            archive_password: None,
            sacd_area: None,
            dvda_group_selection: crate::convert::pipeline::DvdaGroupSelection::Default,
            dvda_group: None,
            dvda_assume_decrypted: false,
            dvda_downmix_policy: crate::convert::pipeline::DvdaDownmixPolicy::Auto,
            dvdv_vts: None,
            dvdv_title: None,
            dvdv_audio_stream: None,
            dvdv_angle: None,
            bluray_playlist: None,
            bluray_audio_pid: None,
            bluray_audio_stream: None,
            bluray_angle: None,
            cue_sidecar: crate::convert::pipeline::CueSidecarPolicy::PreferSidecar,
            track_selection: crate::convert::pipeline::TrackSelection::All,
        };

        assert!(apply_presentation_to_source_options(
            &mut options,
            &PresentationId::DvdAudioGroup(3),
        ));

        assert_eq!(
            options.dvda_group_selection,
            crate::convert::pipeline::DvdaGroupSelection::Group(3)
        );
        assert_eq!(
            options.effective_dvda_group_selection(),
            crate::convert::pipeline::DvdaGroupSelection::Group(3)
        );
    }

    #[test]
    fn disc_browser_stream_conversion_populates_all_bluray_source_options() {
        let id = PresentationId::try_blu_ray_title(12, 0x1100, 0, 2)
            .expect("valid Blu-ray presentation id");
        let mut options = crate::convert::pipeline::SourceOptions {
            archive_password: None,
            sacd_area: None,
            dvda_group_selection: crate::convert::pipeline::DvdaGroupSelection::Default,
            dvda_group: None,
            dvda_assume_decrypted: false,
            dvda_downmix_policy: crate::convert::pipeline::DvdaDownmixPolicy::Auto,
            dvdv_vts: None,
            dvdv_title: None,
            dvdv_audio_stream: None,
            dvdv_angle: None,
            bluray_playlist: None,
            bluray_audio_pid: None,
            bluray_audio_stream: None,
            bluray_angle: None,
            cue_sidecar: crate::convert::pipeline::CueSidecarPolicy::PreferSidecar,
            track_selection: crate::convert::pipeline::TrackSelection::All,
        };

        assert!(presentation_id_supports_stream_conversion(&id));
        assert!(apply_presentation_to_source_options(&mut options, &id));
        assert_eq!(options.bluray_playlist, Some(12));
        assert_eq!(options.bluray_audio_pid, Some(0x1100));
        assert_eq!(options.bluray_audio_stream, Some(0));
        assert_eq!(options.bluray_angle, Some(2));
    }

}


#[cfg(test)]
mod disc_stream_summary_tests {
    use super::*;
    use crate::disc::model::{
        AudioPresentationFormat, CopyProtectionSummary, DiscFormat, DiscPresentation,
        DiscTrack, FormatProvenance, PresentationId,
    };
    use std::path::PathBuf;

    fn presentation(
        label: &str,
        codec: &str,
        channels: u32,
        layout: &str,
        bit_depth: u32,
        sample_rate: u32,
        playlist: u32,
    ) -> DiscPresentation {
        DiscPresentation {
            id: PresentationId::try_blu_ray_title(playlist, 0x1100 + playlist as u16, 0, 1)
                .expect("valid Blu-ray id"),
            label: label.to_string(),
            format: AudioPresentationFormat {
                codec: Some(codec.to_string()),
                sample_rate: Some(sample_rate),
                bit_depth: Some(bit_depth),
                channels: Some(channels as u8),
                channel_layout: Some(layout.to_string()),
                lossless: codec != "AC3",
                provenance: FormatProvenance::IfoAttributes,
            },
            tracks: vec![DiscTrack {
                number: 1,
                title: None,
                performer: None,
                duration_secs: Some(60.0),
                format_note: None,
            }],
            total_duration_secs: 60.0,
            album_title: None,
            album_artist: None,
            genre: None,
            year: None,
        }
    }

    fn contents(presentations: Vec<DiscPresentation>) -> DiscContents {
        DiscContents {
            format: DiscFormat::BluRay,
            label: "BD".to_string(),
            source_path: PathBuf::from("disc"),
            presentations,
            suppressed: Vec::new(),
            copy_protection: CopyProtectionSummary { description: "none".to_string() },
            diagnostics: Vec::new(),
            album_title: None,
            album_artist: None,
            genre: None,
            year: None,
        }
    }

    #[test]
    fn stream_summary_sorts_by_codec_channels_depth_rate_and_caps_limit() {
        let contents = contents(vec![
            presentation("AC3 5.1", "AC3", 6, "5.1", 16, 48_000, 1),
            presentation("DTS 5.1", "DTS-HD MA", 6, "5.1", 24, 96_000, 2),
            presentation("TrueHD stereo", "TrueHD", 2, "stereo", 24, 96_000, 3),
            presentation("LPCM 5.1", "LPCM", 6, "5.1", 24, 96_000, 4),
            presentation("LPCM stereo 16", "LPCM", 2, "stereo", 16, 44_100, 5),
            presentation("LPCM stereo 24/96", "LPCM", 2, "stereo", 24, 96_000, 6),
            presentation("LPCM stereo 24/192", "LPCM", 2, "stereo", 24, 192_000, 7),
        ]);

        let lines = disc_stream_summary_lines(&contents, 6);

        assert_eq!(lines.len(), 6);
        assert_eq!(lines[0], "LPCM 24-bit/192kHz stereo");
        assert_eq!(lines[1], "LPCM 24-bit/96kHz stereo");
        assert_eq!(lines[2], "LPCM 16-bit/44.1kHz stereo");
        assert_eq!(lines[3], "LPCM 24-bit/96kHz 5.1");
        assert_eq!(lines[4], "TrueHD 24-bit/96kHz stereo");
        assert_eq!(lines[5], "DTS-HD MA 24-bit/96kHz 5.1");
    }

    #[test]
    fn sacd_stream_summary_uses_compact_dsd_labels() {
        let mut contents = contents(vec![
            presentation("Stereo", "DSD", 2, "stereo", 1, 2_822_400, 1),
            presentation("Multichannel", "DSD", 6, "5.1", 1, 2_822_400, 2),
        ]);
        contents.format = DiscFormat::Sacd;

        let lines = disc_stream_summary_lines(&contents, 6);

        assert_eq!(lines, vec!["DSD 2.8MHz stereo", "DSD 2.8MHz 5.1"]);
    }
}

#[cfg(test)]
mod disc_selection_bridge_tests {
    use super::*;
    use crate::disc::model::{AudioPresentationFormat, CopyProtectionSummary, DiscFormat, DiscPresentation, DiscTrack, FormatProvenance, PresentationId};
    use crate::convert::pipeline::{CueSidecarPolicy, DvdaDownmixPolicy, DvdaGroupSelection, SourceOptions, TrackSelection};
    use std::path::PathBuf;

    fn dvdv_contents() -> DiscContents {
        DiscContents {
            format: DiscFormat::DvdVideo,
            label: "DVDV".to_string(),
            source_path: PathBuf::from("disc.iso"),
            presentations: vec![DiscPresentation {
                id: PresentationId::dvd_video(2, 3, 1),
                label: "VTS 02 Title 03 Stream 2".to_string(),
                format: AudioPresentationFormat {
                    codec: Some("LPCM".to_string()),
                    sample_rate: Some(96_000),
                    bit_depth: Some(24),
                    channels: Some(2),
                    channel_layout: Some("Stereo".to_string()),
                    lossless: true,
                    provenance: FormatProvenance::IfoAttributes,
                },
                tracks: vec![DiscTrack {
                    number: 1,
                    title: None,
                    performer: None,
                    duration_secs: Some(180.0),
                    format_note: None,
                }],
                total_duration_secs: 180.0,
                album_title: None,
                album_artist: None,
                genre: None,
                year: None,
            }],
            suppressed: Vec::new(),
            copy_protection: CopyProtectionSummary { description: String::new() },
            diagnostics: Vec::new(),
            album_title: None,
            album_artist: None,
            genre: None,
            year: None,
        }
    }

    #[test]
    fn source_mode_disc_selection_bridge_applies_selected_presentation() {
        let mode = source_mode_for_presentation(
            dvdv_contents(),
            0,
            SourceMetadata::default(),
        ).expect("source mode");
        let mut options = SourceOptions {
            archive_password: None,
            sacd_area: None,
            dvda_group_selection: DvdaGroupSelection::Default,
            dvda_group: None,
            dvda_assume_decrypted: false,
            dvda_downmix_policy: DvdaDownmixPolicy::Auto,
            dvdv_vts: None,
            dvdv_title: None,
            dvdv_audio_stream: None,
            dvdv_angle: None,
            bluray_playlist: None,
            bluray_audio_pid: None,
            bluray_audio_stream: None,
            bluray_angle: None,
            cue_sidecar: CueSidecarPolicy::PreferSidecar,
            track_selection: TrackSelection::All,
        };

        assert!(apply_source_mode_disc_selection_to_source_options(&mode, &mut options));

        assert_eq!(options.dvdv_vts, Some(2));
        assert_eq!(options.dvdv_title, Some(3));
        assert_eq!(options.dvdv_audio_stream, Some(1));
        assert_eq!(options.dvdv_angle, Some(1));
    }
}
