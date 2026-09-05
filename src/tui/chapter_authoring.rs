//! Chapter-authoring state and sample-domain geometry.
//!
//! There is deliberately no second persistent chapter-boundary model here.
//! `CueAlbumTrackSource` remains the editable row carrier used by the metadata
//! editor.  A row may carry an exact sample-domain override for INDEX 00/01;
//! when that override is absent, an existing CUE frame remains authoritative.
//! CUE serialization floors exact sample positions onto the 75 Hz CUE grid.
//! Derived views in this module are ephemeral validation/rendering projections.

use std::path::{Path, PathBuf};

use super::app::{CueAlbumSyntheticSheet, CueAlbumTrackSource};
use super::metadata_autonumber::{format_numbering_values, NumberingScheme};
use super::text_input::TextInputState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChapterStructureOrigin {
    None,
    SidecarCue,
    EmbeddedCue,
    EmbeddedChapters,
    Authored,
}

impl ChapterStructureOrigin {
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::SidecarCue => "sidecar CUE",
            Self::EmbeddedCue => "embedded CUE",
            Self::EmbeddedChapters => "embedded chapters",
            Self::Authored => "authored",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChapterAuthoringLoadState {
    NotLoaded,
    Loading { path: PathBuf },
    Ready { path: PathBuf },
    Failed { path: PathBuf, reason: String },
}

impl Default for ChapterAuthoringLoadState {
    fn default() -> Self {
        Self::NotLoaded
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChapterColumn {
    Start,
    Pregap,
    Title,
}

impl ChapterColumn {
    pub const ALL: [Self; 3] = [Self::Start, Self::Pregap, Self::Title];

    pub fn next(self) -> Self {
        match self {
            Self::Start => Self::Pregap,
            Self::Pregap => Self::Title,
            Self::Title => Self::Start,
        }
    }

    pub fn previous(self) -> Self {
        match self {
            Self::Start => Self::Title,
            Self::Pregap => Self::Start,
            Self::Title => Self::Pregap,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChapterEditKind {
    Start,
    Pregap,
    Title,
    InsertStart,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChapterGenerationMode {
    FixedDuration,
    UniformCount,
}

impl ChapterGenerationMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::FixedDuration => "Fixed duration",
            Self::UniformCount => "Uniform count",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::FixedDuration => Self::UniformCount,
            Self::UniformCount => Self::FixedDuration,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChapterGenerationField {
    Mode,
    Value,
    BaseTitle,
    Numbering,
}

impl ChapterGenerationField {
    pub fn next(self) -> Self {
        match self {
            Self::Mode => Self::Value,
            Self::Value => Self::BaseTitle,
            Self::BaseTitle => Self::Numbering,
            Self::Numbering => Self::Mode,
        }
    }

    pub fn previous(self) -> Self {
        match self {
            Self::Mode => Self::Numbering,
            Self::Value => Self::Mode,
            Self::BaseTitle => Self::Value,
            Self::Numbering => Self::BaseTitle,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ChapterGenerationState {
    pub field: ChapterGenerationField,
    pub mode: ChapterGenerationMode,
    pub value: TextInputState,
    pub base_title: TextInputState,
    pub numbering: NumberingScheme,
    /// Title-only pattern application leaves the current division map intact.
    /// The operator's UI direction exposes this as a distinct Titles action,
    /// while boundary generation reuses the same base-title/numbering fields.
    pub titles_only: bool,
}

impl Default for ChapterGenerationState {
    fn default() -> Self {
        Self {
            field: ChapterGenerationField::Mode,
            mode: ChapterGenerationMode::FixedDuration,
            value: TextInputState::empty(),
            base_title: TextInputState::new("Chapter ".to_string()),
            numbering: NumberingScheme::NN,
            titles_only: false,
        }
    }
}

impl ChapterGenerationState {
    pub fn titles_only() -> Self {
        let mut state = Self::default();
        state.field = ChapterGenerationField::BaseTitle;
        state.titles_only = true;
        state
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChapterInFileDestination {
    EmbeddedChapters,
    EmbeddedCue,
}

impl ChapterInFileDestination {
    pub fn label(self) -> &'static str {
        match self {
            Self::EmbeddedChapters => "Chapter entries in file",
            Self::EmbeddedCue => "Embedded CUESHEET tag",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ChapterSaveDialog {
    pub cursor: usize,
    pub sidecar_selected: bool,
    pub in_file: Option<ChapterInFileDestination>,
    pub in_file_selected: bool,
    /// Split is a conversion consequence, not a second stored structure.  It
    /// remains visible in the dialog because the operator's design asks for it.
    /// When selected, save requires at least one durable structure carrier so
    /// the next conversion can rediscover the authored map.
    pub split_on_conversion: bool,
    /// When selected, all authored sample positions are deliberately snapped
    /// down to the CUE 75 Hz grid before any selected destination is written.
    /// This is opt-in because MP4 chapter entries otherwise retain exact
    /// sample positions.
    pub snap_to_cue_grid: bool,
}

impl ChapterSaveDialog {
    pub fn row_count(&self) -> usize {
        // Sidecar, optional in-file carrier, split-on-next-conversion, snap.
        3 + usize::from(self.in_file.is_some())
    }

    pub fn clamp_cursor(&mut self) {
        self.cursor = self.cursor.min(self.row_count().saturating_sub(1));
    }
}

#[derive(Debug, Clone)]
pub struct ChapterSidecarSnapshot {
    pub path: PathBuf,
    /// Exact bytes observed by the background probe when the Chapters surface
    /// opened. Structural sidecar replacement is permitted only while this
    /// snapshot still matches, so an invalid CUE can be repaired without
    /// weakening Tonepoet's ordinary stale-write protection.
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct ChapterAuthoringState {
    pub load_generation: u64,
    pub save_generation: u64,
    pub saving: bool,
    pub load_state: ChapterAuthoringLoadState,
    pub origin: ChapterStructureOrigin,
    pub cursor: usize,
    pub scroll: usize,
    pub column: ChapterColumn,
    pub edit_kind: Option<ChapterEditKind>,
    pub edit_input: Option<TextInputState>,
    pub generation: Option<ChapterGenerationState>,
    pub save_dialog: Option<ChapterSaveDialog>,
    pub import_notes: Vec<String>,
    pub sidecar_snapshot: Option<ChapterSidecarSnapshot>,
    pub sidecar_snapshot_error: Option<String>,
    /// True when the Chapters surface introduced a temporary unified-CUE
    /// projection for a source that was not already a CUE album. The projection
    /// enables row-oriented chapter editing, but must not reroute ordinary
    /// Metadata-tab saves into CUE-carrier writes until a CUE carrier is
    /// actually committed.
    pub projection_only: bool,
    /// Structural dirt is intentionally separate from ordinary metadata dirt;
    /// a caller must serialize a supported structure destination before this
    /// bit is cleared.
    pub dirty: bool,
}

impl Default for ChapterAuthoringState {
    fn default() -> Self {
        Self {
            load_generation: 0,
            save_generation: 0,
            saving: false,
            load_state: ChapterAuthoringLoadState::NotLoaded,
            origin: ChapterStructureOrigin::None,
            cursor: 0,
            scroll: 0,
            column: ChapterColumn::Start,
            edit_kind: None,
            edit_input: None,
            generation: None,
            save_dialog: None,
            import_notes: Vec::new(),
            sidecar_snapshot: None,
            sidecar_snapshot_error: None,
            projection_only: false,
            dirty: false,
        }
    }
}

impl ChapterAuthoringState {
    pub fn begin_load(&mut self, path: PathBuf) -> u64 {
        self.load_generation = self.load_generation.wrapping_add(1);
        self.load_state = ChapterAuthoringLoadState::Loading { path };
        self.load_generation
    }

    pub fn ready_for(&self, path: &Path) -> bool {
        matches!(&self.load_state, ChapterAuthoringLoadState::Ready { path: loaded } if same_path(loaded, path))
    }

    pub fn is_loading(&self) -> bool {
        matches!(self.load_state, ChapterAuthoringLoadState::Loading { .. })
    }

    pub fn begin_save(&mut self) -> u64 {
        self.save_generation = self.save_generation.wrapping_add(1);
        self.saving = true;
        self.save_generation
    }

    pub fn complete_save(&mut self, generation: u64) -> bool {
        if self.save_generation != generation || !self.saving {
            return false;
        }
        self.saving = false;
        true
    }

    pub fn cancel_save(&mut self) {
        self.save_generation = self.save_generation.wrapping_add(1);
        self.saving = false;
    }

    pub fn clamp_cursor(&mut self, rows: usize) {
        self.cursor = self.cursor.min(rows.saturating_sub(1));
        if rows == 0 {
            self.scroll = 0;
        }
    }
}

fn same_path(left: &Path, right: &Path) -> bool {
    #[cfg(windows)]
    {
        left.to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy())
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChapterSaveOutcome {
    pub sidecar_path: Option<PathBuf>,
    /// Exact post-commit sidecar bytes, when a sidecar was written. Retaining
    /// them keeps optimistic-concurrency checks valid for another edit/save in
    /// the same editor session.
    pub sidecar_snapshot: Option<Vec<u8>>,
    pub in_file_path: Option<PathBuf>,
    pub in_file_destination: Option<ChapterInFileDestination>,
    /// Exact CUESHEET payload committed to an embedded-CUE carrier.
    pub embedded_cuesheet_snapshot: Option<String>,
    pub split_on_conversion: bool,
    pub snapped_to_cue_grid: bool,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChapterBoundaryView {
    pub row: usize,
    pub start_sample: u64,
    pub end_sample: u64,
    pub pregap_start_sample: Option<u64>,
}

impl ChapterBoundaryView {
    pub fn samples(&self) -> u64 {
        self.end_sample.saturating_sub(self.start_sample)
    }

    pub fn pregap_samples(&self) -> Option<u64> {
        self.pregap_start_sample
            .map(|start| self.start_sample.saturating_sub(start))
            .filter(|samples| *samples != 0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChapterProblem {
    pub row: Option<usize>,
    pub message: String,
}

pub fn cue_frames_to_samples(frames: u32, sample_rate: u32) -> Result<u64, String> {
    if sample_rate == 0 {
        return Err("chapter authoring requires a non-zero sample rate".to_string());
    }
    let samples = u128::from(frames)
        .checked_mul(u128::from(sample_rate))
        .ok_or_else(|| "CUE frame conversion overflowed".to_string())?
        / 75;
    u64::try_from(samples).map_err(|_| "CUE frame position exceeds u64 sample range".to_string())
}

pub fn samples_to_cue_frames_floor(samples: u64, sample_rate: u32) -> Result<u32, String> {
    if sample_rate == 0 {
        return Err("chapter authoring requires a non-zero sample rate".to_string());
    }
    let frames = u128::from(samples)
        .checked_mul(75)
        .ok_or_else(|| "sample-to-CUE conversion overflowed".to_string())?
        / u128::from(sample_rate);
    u32::try_from(frames).map_err(|_| "CUE frame position exceeds the CUE u32 range".to_string())
}

pub fn authoritative_start_sample(
    source: &CueAlbumTrackSource,
    sample_rate: u32,
) -> Result<u64, String> {
    if let Some(samples) = source.index01_sample {
        return Ok(samples);
    }
    let frames = source.index01_frames.ok_or_else(|| {
        format!(
            "chapter {} has neither an exact start nor INDEX 01",
            source.original_track_number
        )
    })?;
    cue_frames_to_samples(frames, sample_rate)
}

pub fn authoritative_pregap_start_sample(
    source: &CueAlbumTrackSource,
    sample_rate: u32,
) -> Result<Option<u64>, String> {
    if let Some(samples) = source.index00_sample {
        return Ok(Some(samples));
    }
    source
        .index00_frames
        .map(|frames| cue_frames_to_samples(frames, sample_rate))
        .transpose()
}

pub fn cue_index01_frames(
    source: &CueAlbumTrackSource,
    sample_rate: Option<u32>,
) -> Result<Option<u32>, String> {
    match (source.index01_sample, sample_rate) {
        (Some(samples), Some(rate)) => samples_to_cue_frames_floor(samples, rate).map(Some),
        (Some(_), None) => Err("cannot serialize exact chapter starts to CUE without a sample rate".to_string()),
        (None, _) => Ok(source.index01_frames),
    }
}

pub fn cue_index00_frames(
    source: &CueAlbumTrackSource,
    sample_rate: Option<u32>,
) -> Result<Option<u32>, String> {
    match (source.index00_sample, sample_rate) {
        (Some(samples), Some(rate)) => samples_to_cue_frames_floor(samples, rate).map(Some),
        (Some(_), None) => Err("cannot serialize exact chapter pregaps to CUE without a sample rate".to_string()),
        (None, _) => Ok(source.index00_frames),
    }
}

pub fn boundary_views(sheet: &CueAlbumSyntheticSheet) -> Result<Vec<ChapterBoundaryView>, String> {
    let sample_rate = sheet
        .program_sample_rate
        .filter(|rate| *rate != 0)
        .ok_or_else(|| "chapter authoring has no source sample rate".to_string())?;
    let total_samples = sheet
        .program_total_samples
        .filter(|samples| *samples != 0)
        .ok_or_else(|| "chapter authoring has no positive source duration".to_string())?;
    if sheet.track_sources.is_empty() {
        return Ok(Vec::new());
    }

    let mut starts = Vec::with_capacity(sheet.track_sources.len());
    let mut pregaps = Vec::with_capacity(sheet.track_sources.len());
    for source in &sheet.track_sources {
        starts.push(authoritative_start_sample(source, sample_rate)?);
        pregaps.push(authoritative_pregap_start_sample(source, sample_rate)?);
    }

    Ok(starts
        .iter()
        .enumerate()
        .map(|(index, start)| ChapterBoundaryView {
            row: index,
            start_sample: *start,
            end_sample: starts.get(index + 1).copied().unwrap_or(total_samples),
            pregap_start_sample: pregaps[index],
        })
        .collect())
}

pub fn validate_cue_projection(sheet: &CueAlbumSyntheticSheet) -> Result<(), String> {
    let sample_rate = sheet
        .program_sample_rate
        .filter(|rate| *rate != 0)
        .ok_or_else(|| "chapter authoring has no source sample rate".to_string())?;
    let views = boundary_views(sheet)?;
    let mut previous_start_frame = None;

    for view in views {
        let source = &sheet.track_sources[view.row];
        // Validate the frames that CUE serialization will actually emit. An
        // untouched legacy CUE row is already frame-native and must not be
        // round-tripped through integer samples: at rates such as 32 kHz,
        // floor(frame * Fs / 75) -> floor(samples * 75 / Fs) can move it one
        // frame early even though the original CUE timestamp is exact.
        let start_frame = cue_index01_frames(source, Some(sample_rate))?
            .ok_or_else(|| format!("chapter {} has no CUE INDEX 01", view.row + 1))?;
        if previous_start_frame.is_some_and(|previous| previous >= start_frame) {
            return Err(format!(
                "CUE projection would collapse or reorder chapter {} at frame {}; move adjacent division points at least one CUE frame apart",
                view.row + 1,
                start_frame
            ));
        }
        previous_start_frame = Some(start_frame);

        if view.pregap_start_sample.is_some_and(|pregap_start| pregap_start < view.start_sample) {
            if let Some(pregap_frame) = cue_index00_frames(source, Some(sample_rate))? {
                if pregap_frame >= start_frame {
                    return Err(format!(
                        "chapter {} pregap is shorter than the CUE grid can represent at this position; move its pregap start into an earlier CUE frame",
                        view.row + 1
                    ));
                }
            }
        }
    }

    Ok(())
}

pub fn validate(sheet: &CueAlbumSyntheticSheet) -> Vec<ChapterProblem> {
    let mut problems = Vec::new();
    let sample_rate = match sheet.program_sample_rate.filter(|rate| *rate != 0) {
        Some(rate) => rate,
        None => {
            problems.push(ChapterProblem {
                row: None,
                message: "sample rate is unavailable".to_string(),
            });
            return problems;
        }
    };
    let total_samples = match sheet.program_total_samples.filter(|samples| *samples != 0) {
        Some(samples) => samples,
        None => {
            problems.push(ChapterProblem {
                row: None,
                message: "program duration is unavailable".to_string(),
            });
            return problems;
        }
    };
    if sheet.track_sources.is_empty() {
        problems.push(ChapterProblem {
            row: None,
            message: "program has no chapter rows".to_string(),
        });
        return problems;
    }

    let mut previous_start = None;
    for (index, source) in sheet.track_sources.iter().enumerate() {
        let start = match authoritative_start_sample(source, sample_rate) {
            Ok(start) => start,
            Err(reason) => {
                problems.push(ChapterProblem {
                    row: Some(index),
                    message: reason,
                });
                continue;
            }
        };
        if index == 0 && start != 0 {
            problems.push(ChapterProblem {
                row: Some(index),
                message: format!("first chapter starts at sample {start}; it must start at 0"),
            });
        }
        if start >= total_samples {
            problems.push(ChapterProblem {
                row: Some(index),
                message: format!(
                    "chapter {} starts at sample {start}, outside the {}-sample program",
                    index + 1,
                    total_samples
                ),
            });
        }
        if let Some(previous) = previous_start {
            if start <= previous {
                problems.push(ChapterProblem {
                    row: Some(index),
                    message: format!(
                        "chapter {} start ({start}) must be after chapter {} start ({previous})",
                        index + 1,
                        index
                    ),
                });
            }
        }

        match authoritative_pregap_start_sample(source, sample_rate) {
            Ok(Some(pregap)) => {
                if pregap > start {
                    problems.push(ChapterProblem {
                        row: Some(index),
                        message: format!(
                            "chapter {} pregap starts after its chapter start",
                            index + 1
                        ),
                    });
                }
                if index == 0 && pregap != 0 {
                    problems.push(ChapterProblem {
                        row: Some(index),
                        message: "chapter 1 cannot place an in-program pregap before sample 0"
                            .to_string(),
                    });
                }
                if let Some(previous) = previous_start {
                    if pregap < previous {
                        problems.push(ChapterProblem {
                            row: Some(index),
                            message: format!(
                                "chapter {} pregap starts before chapter {}",
                                index + 1,
                                index
                            ),
                        });
                    }
                }
            }
            Ok(None) => {}
            Err(reason) => problems.push(ChapterProblem {
                row: Some(index),
                message: reason,
            }),
        }
        previous_start = Some(start);
    }

    problems
}

pub fn parse_position_samples(
    text: &str,
    sample_rate: u32,
    current: Option<u64>,
) -> Result<u64, String> {
    if sample_rate == 0 {
        return Err("chapter authoring requires a non-zero sample rate".to_string());
    }
    let text = text.trim();
    if text.is_empty() {
        return Err("enter a chapter position".to_string());
    }

    let (relative, body) = match text.as_bytes().first().copied() {
        Some(b'+') => (Some(true), text[1..].trim()),
        Some(b'-') => (Some(false), text[1..].trim()),
        _ => (None, text),
    };
    let magnitude = parse_absolute_position_samples(body, sample_rate)?;
    match relative {
        Some(add) => {
            let current = current.ok_or_else(|| "relative positions require a current boundary".to_string())?;
            if add {
                current
                    .checked_add(magnitude)
                    .ok_or_else(|| "relative chapter position overflowed".to_string())
            } else {
                current
                    .checked_sub(magnitude)
                    .ok_or_else(|| "relative chapter position would be negative".to_string())
            }
        }
        None => Ok(magnitude),
    }
}

fn parse_absolute_position_samples(text: &str, sample_rate: u32) -> Result<u64, String> {
    let lower = text.trim().to_ascii_lowercase();
    if let Some(number) = lower.strip_suffix("smp") {
        return number
            .trim()
            .parse::<u64>()
            .map_err(|_| "sample positions use an integer followed by 'smp'".to_string());
    }
    if let Some(cue) = lower.strip_suffix("cue") {
        let cue = cue.trim();
        let parts = cue.split(':').collect::<Vec<_>>();
        if parts.len() != 3 {
            return Err("CUE positions use MM:SS:FF cue".to_string());
        }
        let minutes = parts[0]
            .parse::<u64>()
            .map_err(|_| "invalid CUE minute field".to_string())?;
        let seconds = parts[1]
            .parse::<u64>()
            .map_err(|_| "invalid CUE second field".to_string())?;
        let frames = parts[2]
            .parse::<u64>()
            .map_err(|_| "invalid CUE frame field".to_string())?;
        if seconds >= 60 || frames >= 75 {
            return Err("CUE seconds must be <60 and frames <75".to_string());
        }
        let total_frames = minutes
            .checked_mul(60)
            .and_then(|value| value.checked_add(seconds))
            .and_then(|value| value.checked_mul(75))
            .and_then(|value| value.checked_add(frames))
            .ok_or_else(|| "CUE position overflowed".to_string())?;
        return cue_frames_to_samples(
            u32::try_from(total_frames).map_err(|_| "CUE position exceeds u32 frame range".to_string())?,
            sample_rate,
        );
    }

    let seconds_text = lower.strip_suffix('s').unwrap_or(lower.as_str()).trim();
    let fields = seconds_text.split(':').collect::<Vec<_>>();
    if fields.len() > 3 {
        return Err("time positions use SS, MM:SS, or HH:MM:SS".to_string());
    }
    let (hours, minutes, seconds) = match fields.as_slice() {
        [seconds] => (0u64, 0u64, parse_decimal_seconds(seconds)?),
        [minutes, seconds] => (
            0,
            minutes
                .parse::<u64>()
                .map_err(|_| "invalid minute field".to_string())?,
            parse_decimal_seconds(seconds)?,
        ),
        [hours, minutes, seconds] => (
            hours
                .parse::<u64>()
                .map_err(|_| "invalid hour field".to_string())?,
            minutes
                .parse::<u64>()
                .map_err(|_| "invalid minute field".to_string())?,
            parse_decimal_seconds(seconds)?,
        ),
        _ => return Err("enter a chapter position".to_string()),
    };
    if fields.len() == 3 && minutes >= 60 {
        return Err("minute fields after an hour must be <60".to_string());
    }
    if fields.len() >= 2 && seconds.0 >= 60 {
        return Err("second fields after a colon must be <60".to_string());
    }

    let whole_seconds = hours
        .checked_mul(3600)
        .and_then(|value| value.checked_add(minutes.saturating_mul(60)))
        .and_then(|value| value.checked_add(seconds.0))
        .ok_or_else(|| "time position overflowed".to_string())?;
    let numerator = u128::from(whole_seconds)
        .checked_mul(1_000_000_000)
        .and_then(|value| value.checked_add(u128::from(seconds.1)))
        .and_then(|value| value.checked_mul(u128::from(sample_rate)))
        .ok_or_else(|| "time-to-sample conversion overflowed".to_string())?;
    let rounded = numerator
        .checked_add(500_000_000)
        .ok_or_else(|| "time-to-sample conversion overflowed".to_string())?
        / 1_000_000_000;
    u64::try_from(rounded).map_err(|_| "time position exceeds u64 sample range".to_string())
}

/// Return (whole seconds, nanoseconds).
fn parse_decimal_seconds(text: &str) -> Result<(u64, u64), String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("empty second field".to_string());
    }
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    let whole = whole
        .parse::<u64>()
        .map_err(|_| "invalid second field".to_string())?;
    if !fraction.chars().all(|ch| ch.is_ascii_digit()) || fraction.len() > 9 {
        return Err("fractional seconds may contain up to 9 digits".to_string());
    }
    let mut nanos = fraction.to_string();
    while nanos.len() < 9 {
        nanos.push('0');
    }
    let nanos = if nanos.is_empty() {
        0
    } else {
        nanos
            .parse::<u64>()
            .map_err(|_| "invalid fractional second field".to_string())?
    };
    Ok((whole, nanos))
}

pub fn format_position(samples: u64, sample_rate: u32) -> String {
    if sample_rate == 0 {
        return format!("{samples} smp");
    }
    let total_millis = u128::from(samples)
        .saturating_mul(1_000)
        .saturating_add(u128::from(sample_rate) / 2)
        / u128::from(sample_rate);
    let millis = (total_millis % 1_000) as u64;
    let total_seconds = (total_millis / 1_000) as u64;
    let seconds = total_seconds % 60;
    let total_minutes = total_seconds / 60;
    let minutes = total_minutes % 60;
    let hours = total_minutes / 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}.{millis:03}")
    } else {
        format!("{minutes:02}:{seconds:02}.{millis:03}")
    }
}

pub fn cue_floor_error_samples(samples: u64, sample_rate: u32) -> Result<u64, String> {
    let frames = samples_to_cue_frames_floor(samples, sample_rate)?;
    let floored = cue_frames_to_samples(frames, sample_rate)?;
    Ok(samples.saturating_sub(floored))
}

/// Return how far an INDEX 01 would move if projected to the CUE grid.
///
/// A frame-native source is already exactly representable in CUE and therefore
/// has zero projection movement. Only an exact sample override needs a
/// sample -> frame -> sample projection for preview purposes.
pub fn cue_index01_projection_error_samples(
    source: &CueAlbumTrackSource,
    sample_rate: u32,
) -> Result<u64, String> {
    match source.index01_sample {
        Some(samples) => cue_floor_error_samples(samples, sample_rate),
        None => Ok(0),
    }
}

/// INDEX 00 counterpart of [`cue_index01_projection_error_samples`].
pub fn cue_index00_projection_error_samples(
    source: &CueAlbumTrackSource,
    sample_rate: u32,
) -> Result<u64, String> {
    match source.index00_sample {
        Some(samples) => cue_floor_error_samples(samples, sample_rate),
        None => Ok(0),
    }
}

/// Make the editable geometry match the exact frames a CUE serialization emits.
///
/// This is intentionally frame-native. Converting the serialized frame back to
/// integer samples and then clearing the frame would add a second floor on the
/// next save at sample rates that are not divisible by 75. Keeping the frame
/// and clearing only the exact-sample override makes CUE-only/global-snap saves
/// idempotent while staying within the existing `CueAlbumTrackSource` model.
pub fn canonicalize_cue_projection_to_frames(
    sheet: &mut CueAlbumSyntheticSheet,
) -> Result<(), String> {
    let sample_rate = sheet
        .program_sample_rate
        .filter(|rate| *rate != 0)
        .ok_or_else(|| "chapter authoring has no source sample rate".to_string())?;

    for source in &mut sheet.track_sources {
        let index01 = cue_index01_frames(source, Some(sample_rate))?.ok_or_else(|| {
            format!(
                "chapter {} has no CUE INDEX 01",
                source.original_track_number
            )
        })?;
        let index00 = cue_index00_frames(source, Some(sample_rate))?;
        source.index01_frames = Some(index01);
        source.index01_sample = None;
        source.index00_frames = index00;
        source.index00_sample = None;
    }

    let problems = validate(sheet);
    if let Some(problem) = problems.first() {
        return Err(format!(
            "saved CUE-grid projection is not editable: {}",
            problem.message
        ));
    }
    validate_cue_projection(sheet)
}

pub fn nudge_samples_one_cue_frame(
    current: u64,
    sample_rate: u32,
    forward: bool,
) -> Result<u64, String> {
    if sample_rate == 0 {
        return Err("chapter authoring requires a non-zero sample rate".to_string());
    }
    // Common audio rates are divisible by 75. Round for unusual rates so one
    // keypress remains a useful human-sized nudge without changing CUE
    // serialization's exact floor rule.
    let step = ((u64::from(sample_rate) + 37) / 75).max(1);
    if forward {
        current
            .checked_add(step)
            .ok_or_else(|| "chapter nudge overflowed".to_string())
    } else {
        Ok(current.saturating_sub(step))
    }
}

pub fn fixed_duration_starts(total_samples: u64, duration_samples: u64) -> Result<Vec<u64>, String> {
    if total_samples == 0 {
        return Err("program duration is zero".to_string());
    }
    if duration_samples == 0 {
        return Err("chapter duration must be positive".to_string());
    }
    let mut starts = Vec::new();
    let mut start = 0u64;
    while start < total_samples {
        starts.push(start);
        start = start
            .checked_add(duration_samples)
            .ok_or_else(|| "generated chapter position overflowed".to_string())?;
        if starts.len() > 100_000 {
            return Err("generated chapter count is unreasonably large".to_string());
        }
    }
    Ok(starts)
}

pub fn uniform_count_starts(total_samples: u64, count: usize) -> Result<Vec<u64>, String> {
    if total_samples == 0 {
        return Err("program duration is zero".to_string());
    }
    if count == 0 {
        return Err("chapter count must be positive".to_string());
    }
    if u128::try_from(count).unwrap_or(u128::MAX) > u128::from(total_samples) {
        return Err("chapter count exceeds the number of source samples".to_string());
    }
    let count_u128 = count as u128;
    let total = u128::from(total_samples);
    let mut starts = Vec::with_capacity(count);
    for index in 0..count {
        let start = total
            .checked_mul(index as u128)
            .ok_or_else(|| "uniform chapter position overflowed".to_string())?
            / count_u128;
        starts.push(
            u64::try_from(start)
                .map_err(|_| "uniform chapter position exceeds u64".to_string())?,
        );
    }
    if starts.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err("uniform chapter generation produced a zero-length chapter".to_string());
    }
    Ok(starts)
}

pub fn generated_titles(
    base_title: &str,
    scheme: NumberingScheme,
    count: usize,
) -> Result<Vec<String>, String> {
    if scheme.is_side() {
        return Err("chapter titles support N, NN, N/NN, or NN/NN numbering".to_string());
    }
    let numbers = format_numbering_values(scheme, count, None)?;
    let base = base_title.trim_end();
    Ok(numbers
        .into_iter()
        .map(|number| {
            if base.is_empty() {
                number
            } else {
                format!("{base} {number}")
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convert::cue_parser::CueUserMetadata;

    fn source(start_frames: u32) -> CueAlbumTrackSource {
        CueAlbumTrackSource {
            cue_path: PathBuf::from("book.cue"),
            audio_path: PathBuf::from("book.flac"),
            local_track_index: 0,
            original_track_number: 1,
            file_ref: "book.flac".to_string(),
            index00_frames: None,
            index01_frames: Some(start_frames),
            index00_sample: None,
            index01_sample: None,
            isrc: None,
            album_user_metadata: CueUserMetadata::default(),
            user_metadata: CueUserMetadata::default(),
            tonepoet_metadata_present: false,
            directives: Vec::new(),
        }
    }

    fn sheet() -> CueAlbumSyntheticSheet {
        CueAlbumSyntheticSheet {
            cue_paths: vec![PathBuf::from("book.cue")],
            audio_paths: vec![PathBuf::from("book.flac")],
            track_sources: vec![source(0), source(75), source(150)],
            album_title: None,
            album_performer: None,
            album_date: None,
            album_genre: None,
            album_catalog: None,
            user_metadata: CueUserMetadata::default(),
            program_sample_rate: Some(48_000),
            program_total_samples: Some(144_000),
        }
    }

    #[test]
    fn exact_sample_override_wins_but_cue_projection_floors() {
        let mut source = source(75);
        source.index01_sample = Some(48_639);
        assert_eq!(authoritative_start_sample(&source, 48_000).unwrap(), 48_639);
        assert_eq!(cue_index01_frames(&source, Some(48_000)).unwrap(), Some(75));
        assert_eq!(cue_floor_error_samples(48_639, 48_000).unwrap(), 639);
    }

    #[test]
    fn legacy_cue_frames_remain_authoritative_without_an_edit() {
        let source = source(75);
        assert_eq!(authoritative_start_sample(&source, 48_000).unwrap(), 48_000);
        assert_eq!(cue_index01_frames(&source, Some(48_000)).unwrap(), Some(75));
    }

    #[test]
    fn validation_catches_non_monotonic_zero_length_geometry() {
        let mut sheet = sheet();
        sheet.track_sources[2].index01_sample = Some(48_000);
        let problems = validate(&sheet);
        assert!(problems.iter().any(|problem| problem.row == Some(2) && problem.message.contains("must be after")));
    }

    #[test]
    fn fixed_duration_generation_keeps_open_ended_tail() {
        assert_eq!(
            fixed_duration_starts(1_000, 300).unwrap(),
            vec![0, 300, 600, 900]
        );
    }

    #[test]
    fn uniform_generation_uses_integer_partition_without_drift() {
        assert_eq!(uniform_count_starts(10, 3).unwrap(), vec![0, 3, 6]);
    }

    #[test]
    fn time_parser_supports_exact_samples_relative_times_and_explicit_cue_units() {
        assert_eq!(parse_position_samples("44100 smp", 44_100, None).unwrap(), 44_100);
        assert_eq!(parse_position_samples("+0.5", 44_100, Some(44_100)).unwrap(), 66_150);
        assert_eq!(parse_position_samples("00:01:00 cue", 44_100, None).unwrap(), 44_100);
        assert_eq!(parse_position_samples("1:02.500", 48_000, None).unwrap(), 3_000_000);
        assert_eq!(parse_position_samples("90:00", 48_000, None).unwrap(), 259_200_000);
    }

    #[test]
    fn cue_projection_rejects_sample_domain_boundaries_that_collapse_to_one_frame() {
        let mut sheet = sheet();
        sheet.track_sources.truncate(2);
        sheet.program_total_samples = Some(48_000);
        sheet.track_sources[1].index01_frames = None;
        sheet.track_sources[1].index01_sample = Some(100);

        assert!(validate(&sheet).is_empty());
        let error = validate_cue_projection(&sheet).unwrap_err();
        assert!(error.contains("collapse or reorder chapter 2"));
    }

    #[test]
    fn cue_projection_rejects_positive_pregap_that_would_disappear() {
        let mut sheet = sheet();
        sheet.track_sources.truncate(2);
        sheet.program_total_samples = Some(48_000);
        sheet.track_sources[1].index01_frames = None;
        sheet.track_sources[1].index01_sample = Some(1_000);
        sheet.track_sources[1].index00_frames = None;
        sheet.track_sources[1].index00_sample = Some(900);

        assert!(validate(&sheet).is_empty());
        let error = validate_cue_projection(&sheet).unwrap_err();
        assert!(error.contains("pregap is shorter than the CUE grid"));
    }

    #[test]
    fn cue_projection_preserves_frame_native_geometry_at_non_divisible_rate() {
        let mut sheet = sheet();
        sheet.program_sample_rate = Some(32_000);
        sheet.program_total_samples = Some(6_500_000);
        sheet.track_sources = vec![source(0), source(7_501), source(15_000)];

        // At 32 kHz, frame 7501 maps to floored sample 3,200,426; converting
        // that sample back to CUE would incorrectly yield frame 7500. The
        // frame-native source itself must remain exactly frame 7501.
        assert_eq!(
            authoritative_start_sample(&sheet.track_sources[1], 32_000).unwrap(),
            3_200_426
        );
        assert_eq!(
            cue_index01_frames(&sheet.track_sources[1], Some(32_000)).unwrap(),
            Some(7_501)
        );
        assert_eq!(
            cue_index01_projection_error_samples(&sheet.track_sources[1], 32_000).unwrap(),
            0
        );
        assert!(validate_cue_projection(&sheet).is_ok());

        canonicalize_cue_projection_to_frames(&mut sheet).unwrap();
        assert_eq!(sheet.track_sources[1].index01_frames, Some(7_501));
        assert_eq!(sheet.track_sources[1].index01_sample, None);
        assert_eq!(
            cue_index01_frames(&sheet.track_sources[1], Some(32_000)).unwrap(),
            Some(7_501)
        );
    }

    #[test]
    fn cue_only_canonicalization_is_idempotent_for_sample_native_start_and_pregap() {
        let mut sheet = sheet();
        sheet.program_sample_rate = Some(32_000);
        sheet.program_total_samples = Some(64_000);
        sheet.track_sources.truncate(2);
        let source = &mut sheet.track_sources[1];
        source.index01_frames = None;
        source.index01_sample = Some(1_000);
        source.index00_frames = None;
        source.index00_sample = Some(900);

        assert_eq!(cue_index01_frames(source, Some(32_000)).unwrap(), Some(2));
        assert_eq!(cue_index00_frames(source, Some(32_000)).unwrap(), Some(2));
        // A same-frame pregap is invalid, so use a representable INDEX 00 for
        // the canonicalization/idempotency check.
        source.index00_sample = Some(400);
        assert_eq!(cue_index00_frames(source, Some(32_000)).unwrap(), Some(0));

        canonicalize_cue_projection_to_frames(&mut sheet).unwrap();
        let source = &sheet.track_sources[1];
        assert_eq!(source.index01_frames, Some(2));
        assert_eq!(source.index01_sample, None);
        assert_eq!(source.index00_frames, Some(0));
        assert_eq!(source.index00_sample, None);
        assert_eq!(cue_index01_frames(source, Some(32_000)).unwrap(), Some(2));
        assert_eq!(cue_index00_frames(source, Some(32_000)).unwrap(), Some(0));
        assert_eq!(cue_index01_projection_error_samples(source, 32_000).unwrap(), 0);
        assert_eq!(cue_index00_projection_error_samples(source, 32_000).unwrap(), 0);

        canonicalize_cue_projection_to_frames(&mut sheet).unwrap();
        assert_eq!(sheet.track_sources[1].index01_frames, Some(2));
        assert_eq!(sheet.track_sources[1].index00_frames, Some(0));
    }

    #[test]
    fn generated_titles_reuse_existing_numbering_semantics() {
        // `decimal_width` floors the pad at two digits
        // (`total.max(1).to_string().len().max(2)`), so a three-entry program
        // still renders 01/03 rather than 1/3. Asserting the narrower form
        // would have been asserting a numbering scheme this project does not
        // have -- the point of this test is that chapter titles inherit the
        // existing semantics rather than inventing parallel ones.
        assert_eq!(
            generated_titles("Chapter", NumberingScheme::NNOverNN, 3).unwrap(),
            vec!["Chapter 01/03", "Chapter 02/03", "Chapter 03/03"]
        );
        // A total that genuinely needs three digits widens for real.
        let wide = generated_titles("Chapter", NumberingScheme::NNOverNN, 100).unwrap();
        assert_eq!(wide[0], "Chapter 001/100");
    }
}
