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
/// ISO files use the image file metadata. Directory DVD-Audio sources also carry
/// the `AUDIO_TS/AUDIO_TS.IFO` metadata because the directory mtime alone is not
/// a reliable proxy for disc-content changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscProbeFingerprint {
    pub source: FileProbeFingerprint,
    pub is_dir: bool,
    pub dvda_audio_ts_ifo: Option<FileProbeFingerprint>,
}

impl DiscProbeFingerprint {
    /// Metadata most representative of disc-content identity. For ISO files this
    /// is the image itself; for DVD-Audio directories this prefers
    /// `AUDIO_TS/AUDIO_TS.IFO`, because directory mtimes do not reliably change
    /// when the IFO is replaced in place.
    pub fn primary_content(&self) -> &FileProbeFingerprint {
        self.dvda_audio_ts_ifo.as_ref().unwrap_or(&self.source)
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
/// an ISO or changing `AUDIO_TS/AUDIO_TS.IFO` for a DVD-Audio directory makes
/// the entry stale and allows normal probing again. Explicit re-probe actions
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

    Ok(DiscProbeFingerprint {
        source: FileProbeFingerprint::from_metadata(&metadata),
        is_dir,
        dvda_audio_ts_ifo,
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
pub fn spawn_disc_probe(path: PathBuf, tx: mpsc::Sender<AppMessage>) {
    tokio::spawn(async move {
        let fingerprint = match disc_probe_fingerprint(&path) {
            Ok(fingerprint) => fingerprint,
            Err(err) => {
                let _ = tx
                    .send(AppMessage::DiscProbeComplete {
                        path,
                        fingerprint: None,
                        result: Box::new(Err(err)),
                    })
                    .await;
                return;
            }
        };

        let probe_path = path.clone();
        let result = match tokio::task::spawn_blocking(move || probe_disc_contents(&probe_path)).await {
            Ok(result) => result,
            Err(err) => Err(format!("Disc probe task failed: {err}")),
        };
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

/// One-line disc summary for info panes.
pub fn disc_summary(contents: &DiscContents) -> String {
    let stream_count = contents.presentations.len();
    let track_count: usize = contents.presentations.iter().map(|p| p.tracks.len()).sum();
    format!(
        "{} · {} audio {} · {} {}",
        contents.format.name(),
        stream_count,
        plural(stream_count, "stream", "streams"),
        track_count,
        plural(track_count, "track", "tracks"),
    )
}

/// Summary row for a single presentation, without the leading row marker.
pub fn presentation_summary(index: usize, presentation: &DiscPresentation) -> String {
    let track_count = presentation.tracks.len();
    let mut suffix = format!("{} {}", track_count, plural(track_count, "track", "tracks"));
    if presentation.total_duration_secs > 0.0 {
        suffix.push_str(", ");
        suffix.push_str(&duration_display(presentation.total_duration_secs));
    }
    format!("Stream {}: {} ({})", index + 1, presentation.label, suffix)
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
        album_title: Some(contents.label.clone()),
        album_artist: contents.album_artist.clone(),
        probe_notice: None,
        scroll: 0,
        cursor: 0,
        selected: vec![true; track_count],
        disc_contents: Some(Box::new(contents)),
        selected_presentation_id: Some(presentation.id),
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
    let mut metadata = SourceMetadata::default();
    if !contents.label.trim().is_empty() {
        metadata.album = Some(contents.label.clone());
    }
    metadata.artist = contents.album_artist.clone();
    metadata.genre = contents.genre.clone();
    metadata.year = contents.year.clone();
    metadata
}

/// Convert a selected presentation id to a short user-facing label.
pub fn presentation_id_label(id: &PresentationId) -> String {
    match id {
        PresentationId::DvdAudioGroup(n) => format!("DVD-Audio group {n}"),
        PresentationId::SacdArea(SacdAreaId::Stereo) => "SACD stereo area".to_string(),
        PresentationId::SacdArea(SacdAreaId::MultiChannel) => {
            "SACD multichannel area".to_string()
        }
    }
}


/// One-shot action to perform after an async disc probe has populated a
/// current `DiscProbeCacheEntry`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscProbeFollowup {
    /// Open the Audio Streams overlay once the selected disc probe completes.
    OpenDiscBrowser,
    /// Load presentation 0 into the Convert screen.
    ConvertDefaultStream,
}

/// Apply a selected presentation to the extraction `SourceOptions` used by the
/// conversion pipeline. This is the critical bridge from TUI stream selection
/// to the actual demux/materialize stage.
pub fn apply_presentation_to_source_options(
    options: &mut crate::convert::pipeline::SourceOptions,
    id: &PresentationId,
) {
    match id {
        PresentationId::DvdAudioGroup(group) => {
            options.dvda_group_selection =
                crate::convert::pipeline::DvdaGroupSelection::Group(*group);
        }
        PresentationId::SacdArea(SacdAreaId::Stereo) => {
            options.sacd_area = Some(crate::convert::pipeline::SacdArea::Stereo);
        }
        PresentationId::SacdArea(SacdAreaId::MultiChannel) => {
            options.sacd_area = Some(crate::convert::pipeline::SacdArea::MultiChannel);
        }
    }
}

/// Apply any selected disc presentation stored on `SourceMode::MultiTrack` to
/// the pipeline source options. Call this at the ConversionItem/PipelineRequest
/// construction boundary after creating the normal source options.
pub fn apply_source_mode_disc_selection_to_source_options(
    mode: &SourceMode,
    options: &mut crate::convert::pipeline::SourceOptions,
) {
    if let SourceMode::MultiTrack {
        selected_presentation_id: Some(id),
        ..
    } = mode
    {
        apply_presentation_to_source_options(options, id);
    }
}

/// Draw the Audio Streams overlay and register mouse targets.
pub fn draw_disc_browser(
    f: &mut ratatui::Frame,
    state: &DiscBrowserState,
    buttons: &mut crate::tui::button_map::ButtonRenderMap,
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
        .border_style(Style::default().fg(crate::tui::theme::PURPLE))
        .title(Span::styled(
            format!(" Audio Streams: {title} "),
            Style::default()
                .fg(crate::tui::theme::PURPLE)
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
                        .fg(crate::tui::theme::PILL_ACTIVE_FG)
                        .bg(crate::tui::theme::PURPLE)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(crate::tui::theme::TEXT)
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
                    Style::default().fg(crate::tui::theme::TEXT_DIM),
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

    let footer = Line::from(vec![
        footer_pill("Enter Convert", crate::tui::theme::PURPLE),
        Span::raw("  "),
        footer_pill("E Expand", crate::tui::theme::PURPLE),
        Span::raw("  "),
        footer_pill("Space Select", crate::tui::theme::PURPLE),
        Span::raw("  "),
        footer_pill("Esc Close", crate::tui::theme::PURPLE),
    ]);
    f.render_widget(Paragraph::new(footer).alignment(Alignment::Center), chunks[1]);

    let footer_y = chunks[1].y;
    let footer_w = chunks[1].width;
    let center_x = chunks[1].x + footer_w.saturating_sub(44) / 2;
    buttons.record_button(
        crate::tui::button_map::TuiButton::DiscBrowserConvert,
        Rect::new(center_x, footer_y, 15, 1),
    );
    buttons.record_button(
        crate::tui::button_map::TuiButton::DiscBrowserClose,
        Rect::new(center_x.saturating_add(33), footer_y, 11, 1),
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

fn footer_pill(label: &str, bg: ratatui::style::Color) -> ratatui::text::Span<'static> {
    ratatui::text::Span::styled(
        format!(" {label} "),
        ratatui::style::Style::default()
            .fg(crate::tui::theme::PILL_ACTIVE_FG)
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
}
