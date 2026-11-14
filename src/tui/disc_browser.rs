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

use std::cell::RefCell;
use std::fs::{self, Metadata};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tokio_util::sync::CancellationToken;

static NEXT_EXTRACT_PROBE_ID: AtomicU64 = AtomicU64::new(1);
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

/// One proposed destination, computed with the worker's naming functions.
/// These values are pure selection facts; no filesystem work is done while
/// building a plan, so a redraw can compare plans cheaply.
#[derive(Debug, Clone, PartialEq, Eq)]
struct NativeSpaceOutput {
    path: PathBuf,
    program_index: usize,
    audio_bytes: u64,
    largest_item: u64,
    item_count: usize,
    taggable_dsf: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NativeSpaceKey {
    outputs: Vec<NativeSpaceOutput>,
    format: DiscFormat,
    continuous: bool,
    overwrite: crate::convert::pipeline::native_extract::NativeOverwritePolicy,
    artwork: bool,
    source: PathBuf,
    cue: bool,
    extraction_log: bool,
}

#[derive(Debug, Clone)]
struct NativeSpaceCache {
    key: NativeSpaceKey,
    /// Shared-volume peak, retaining the existing sequential/replace budget.
    required: u64,
    /// Independent peaks when private working storage and output are on
    /// different volumes. No scratch bytes are charged to the destination.
    working_required: u64,
    destination_required: u64,
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
    /// Native extraction uses one selection bitmap per presentation. Chapter
    /// selections are synchronized across streams in the same authored title.
    pub items_checked: Vec<Vec<bool>>,
    pub focus: ExtractFocus,
    /// Same title-bar window state as Folder Properties and Recovery.
    pub maximized: bool,
    pub item_cursor: usize,
    pub item_scroll: usize,
    pub output_cursor: usize,
    pub show_hidden: bool,
    pub panel: Option<ExtractPanel>,
    pub panel_cursor: usize,
    pub editing_destination: bool,
    pub destination_input: String,
    pub editing_template: bool,
    pub naming_template: String,
    pub program_subfolders: bool,
    pub overwrite_policy: crate::convert::pipeline::native_extract::NativeOverwritePolicy,
    pub extraction_log: bool,
    pub destination: PathBuf,
    pub wav_container: crate::convert::pipeline::native_extract::NativeWavContainer,
    pub keep_dst: Vec<bool>,
    /// One authored angle per title/playlist, shared by all its audio streams.
    pub selected_angles: Vec<u8>,
    pub tags: bool,
    pub tag_source: crate::convert::pipeline::native_extract::NativeTagSource,
    pub tag_fields: u16,
    pub tag_artwork: bool,
    /// Cached destination inventory for capacity display. Path existence is
    /// checked on a selection/settings change, not on every TUI redraw; the
    /// Extract action always refreshes immediately before queuing the job.
    native_space_cache: RefCell<Option<NativeSpaceCache>>,
    pub assume_decrypted: bool,
    pub downmix: crate::convert::pipeline::DvdaDownmixPolicy,
    pub file_per_program: bool,
    pub cue: bool,
    pub dvda_group_preset: u8,
    /// Source reads for the pre-extraction eligibility probe never run on the
    /// UI reducer. A late completion only applies to this overlay generation.
    pub probe_id: u64,
    pub probe_cancel: CancellationToken,
    /// One bounded evidence result per presentation. Pending programs cannot
    /// be extracted, and a failure disables only its own stream, not unrelated
    /// playable streams. Convert is never gated by these results.
    pub probe_statuses: Vec<Option<Result<crate::convert::pipeline::native_extract::NativeProbeFacts, String>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtractFocus { Programs, Items, Output }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtractPanel { Format, Files, Tags, Destination, Downmix, AssumeDecrypted, Diagnostics, More, ConvertChoice }

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
        let selected: Vec<bool> = contents.presentations.iter().enumerate().map(|(index, p)| {
            p.extract.ineligible_reason.is_none()
                && !matches!(p.id, PresentationId::BluRayTitle { display_angle, .. } if display_angle.get() != 1)
                && match contents.format {
                DiscFormat::Sacd | DiscFormat::DvdAudio => true,
                DiscFormat::DvdVideo | DiscFormat::BluRay =>
                    (0..index).all(|earlier| contents.presentations[earlier].extract.ineligible_reason.is_some()),
            }
        }).collect();
        let items_checked = contents.presentations.iter().map(|p| vec![true; p.tracks.len()]).collect();
        // Selection can change before the user starts Extract. Probe every
        // eligible source needing media evidence, not just initially checked
        // rows, so checking an alternate language/playlist cannot bypass a
        // previously completed preflight for a different presentation.
        let probe_statuses = contents.presentations.iter().map(|p| {
            if p.extract.ineligible_reason.is_none()
                && !matches!(p.id, PresentationId::BluRayTitle { display_angle, .. }
                    if display_angle.get() != 1)
                && (
                matches!(contents.format, DiscFormat::DvdVideo | DiscFormat::BluRay)
                || (contents.format != DiscFormat::Sacd && p.format.lossless
                    && p.format.bit_depth.is_none())
            ) { None } else { Some(Ok(Default::default())) }
        }).collect();
        Self {
            contents,
            cursor: 0,
            expanded: vec![false; len],
            selected,
            scroll: 0,
            source_path: source_path.clone(),
            items_checked,
            focus: ExtractFocus::Programs,
            maximized: false,
            item_cursor: 0,
            item_scroll: 0,
            output_cursor: 0,
            show_hidden: false,
            panel: None,
            panel_cursor: 0,
            editing_destination: false,
            destination_input: String::new(),
            editing_template: false,
            naming_template: "{track} - {title}".into(),
            program_subfolders: true,
            overwrite_policy: crate::convert::pipeline::native_extract::NativeOverwritePolicy::Fail,
            extraction_log: false,
            destination: source_path.parent().unwrap_or(Path::new(".")).join("Extracted"),
            wav_container: crate::convert::pipeline::native_extract::NativeWavContainer::Automatic,
            keep_dst: vec![false; len],
            selected_angles: vec![1; len],
            tags: true,
            tag_source: crate::convert::pipeline::native_extract::NativeTagSource::DiscAndSidecar,
            tag_fields: 0x7f,
            tag_artwork: true,
            native_space_cache: RefCell::new(None),
            assume_decrypted: false,
            downmix: crate::convert::pipeline::DvdaDownmixPolicy::None,
            file_per_program: false,
            cue: false,
            dvda_group_preset: 0,
            probe_id: NEXT_EXTRACT_PROBE_ID.fetch_add(1, Ordering::Relaxed),
            probe_cancel: CancellationToken::new(),
            probe_statuses,
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
        let mut next = self.cursor;
        for _ in 0..delta.unsigned_abs() {
            let mut candidate = next;
            loop {
                let following = if delta < 0 { candidate.saturating_sub(1) }
                    else { candidate.saturating_add(1).min(max) };
                if following == candidate { break; }
                candidate = following;
                if self.is_visible_program(candidate) { next = candidate; break; }
            }
        }
        self.cursor = next;
    }

    pub fn toggle_expanded(&mut self, index: usize) {
        if let Some(expanded) = self.expanded.get_mut(index) {
            *expanded = !*expanded;
        }
    }

    /// Only angle-one presentations are drawn as audio-stream rows: the angle
    /// selector controls the authored angle for *every* stream in the group.
    pub fn is_visible_program(&self, index: usize) -> bool {
        self.contents.presentations.get(index).is_some_and(|p|
            !matches!(p.id, PresentationId::BluRayTitle { display_angle, .. } if display_angle.get() != 1))
    }

    pub fn selected_angle_for(&self, index: usize) -> u8 {
        self.selected_angles.get(index).copied().unwrap_or(1)
    }

    pub fn cycle_angle_for(&mut self, index: usize, step: i32) {
        if !self.is_visible_program(index) { return; }
        let max = self.contents.presentations.get(index).map_or(1, |p| p.extract.angle_count.max(1));
        if max <= 1 { return; }
        let current = self.selected_angle_for(index);
        let next = ((i32::from(current) - 1 + step).rem_euclid(i32::from(max)) + 1) as u8;
        for i in 0..self.len() {
            if self.shared_program(index, i) { self.selected_angles[i] = next; }
        }
    }

    /// Extraction restrictions do not affect the existing Convert dispatcher.
    pub fn is_extractable(&self, index: usize) -> bool {
        self.is_visible_program(index) && self.contents.presentations.get(index)
            .is_some_and(|p| p.extract.ineligible_reason.is_none())
            && !matches!(self.probe_statuses.get(index), Some(Some(Err(_))))
    }

    pub fn toggle_selected(&mut self, index: usize) {
        if !self.is_extractable(index) { return; }
        if let Some(selected) = self.selected.get_mut(index) {
            *selected = !*selected;
            if *selected && !self.items_checked[index].iter().any(|checked| *checked) {
                self.items_checked[index].fill(true);
            }
        }
    }

    pub fn set_cursor(&mut self, index: usize) {
        if index < self.len() && self.is_visible_program(index) {
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

impl DiscBrowserState {
    pub fn selected_item_count(&self) -> usize {
        self.selected.iter().enumerate().filter(|(_, selected)| **selected)
            .map(|(i, _)| self.items_checked.get(i).map_or(0, |items| items.iter().filter(|v| **v).count()))
            .sum()
    }

    pub fn selected_program_count(&self) -> usize {
        self.selected.iter().filter(|v| **v).count()
    }

    fn native_item_payload_bytes(&self, p: &DiscPresentation, track: &DiscTrack) -> Option<u64> {
        let rate = p.format.sample_rate? as f64;
        let channels = f64::from(p.format.channels?);
        let bits = if self.contents.format == DiscFormat::Sacd { 1 }
            else if p.format.lossless { p.format.bit_depth? } else { 24 };
        let bytes_per_sample = if bits == 1 { 1.0 / 8.0 }
            else { f64::from(bits.div_ceil(8)) };
        let seconds = track.duration_secs?;
        if !seconds.is_finite() || seconds < 0.0 { return None; }
        let estimate = (seconds * rate * channels * bytes_per_sample).ceil();
        if !estimate.is_finite() || estimate > u64::MAX as f64 { return None; }
        Some(estimate as u64)
    }

    /// Native payload estimate, largest individual item and largest selected
    /// program. The latter bounds the simultaneous intermediates retained by
    /// continuous-file concatenation, even for many small chapters.
    pub fn selected_item_bytes(&self) -> Option<(u64, u64, u64)> {
        let mut total = 0u64;
        let mut largest = 0u64;
        let mut largest_program = 0u64;
        for (i, selected) in self.selected.iter().enumerate() {
            if !*selected { continue; }
            let mut program_bytes = 0u64;
            let p = self.contents.presentations.get(i)?;
            for (j, track) in p.tracks.iter().enumerate() {
                if !self.items_checked.get(i)?.get(j).copied().unwrap_or(false) { continue; }
                let bytes = self.native_item_payload_bytes(p, track)?;
                total = total.checked_add(bytes)?;
                program_bytes = program_bytes.checked_add(bytes)?;
                largest = largest.max(bytes);
            }
            largest_program = largest_program.max(program_bytes);
        }
        Some((total, largest, largest_program))
    }

    /// Combined peak for staging and publication on a shared filesystem.
    /// For continuous PCM, retain chapter outputs, the full raw concatenation
    /// spool, and the final container, plus the largest decoded carrier.
    /// SACD has no decoded WAV carrier. Separate tracks are published one at
    /// a time; continuous SACD retains its track files while joining them.
    pub(crate) fn required_native_volume_bytes(&self, total: u64, largest: u64, largest_program: u64) -> u64 {
        self.native_bytes_with_allowance(total, largest, largest_program, self.selected_item_count())
    }

    fn native_bytes_with_allowance(&self, total: u64, largest: u64, largest_program: u64, items: usize) -> u64 {
        let allowance = (items as u64).saturating_mul(128 * 1024);
        if self.contents.format == DiscFormat::Sacd {
            let payload = if self.file_per_program { total.saturating_mul(2) } else { total };
            return payload.saturating_add(allowance);
        }
        let intermediate = if self.file_per_program { largest_program.saturating_mul(2) } else { 0 };
        total.saturating_add(intermediate)
            .saturating_add(largest.saturating_mul(3))
            .saturating_add(allowance)
    }

    /// Build destination paths through the worker's own template resolver.
    /// No path is stat'ed here; this is safe to compare on TUI redraws.
    fn native_space_key(&self) -> Option<NativeSpaceKey> {
        use crate::convert::pipeline::native_extract::{proposed_destination, proposed_destination_for_program};
        let job = self.native_job(PathBuf::new());
        let mut outputs = Vec::new();
        let indices = self.selected.iter().enumerate()
            .filter(|(index, checked)| **checked && self.is_extractable(*index)
                && self.items_checked.get(*index).is_some_and(|items| items.iter().any(|item| *item)))
            .map(|(index, _)| index);
        for (index, program) in indices.zip(&job.programs) {
            let presentation = self.contents.presentations.get(index)?;
            let mut program_bytes = 0u64;
            let mut largest = 0u64;
            for track in &program.tracks {
                let number = usize::try_from(track.number).ok()?;
                let disc_track = presentation.tracks.get(number.checked_sub(1)?)?;
                let bytes = self.native_item_payload_bytes(presentation, disc_track)?;
                program_bytes = program_bytes.checked_add(bytes)?;
                largest = largest.max(bytes);
                if !self.file_per_program {
                    outputs.push(NativeSpaceOutput {
                        path: proposed_destination(&job, program, track),
                        program_index: index, audio_bytes: bytes, largest_item: bytes,
                        item_count: 1,
                        taggable_dsf: self.contents.format == DiscFormat::Sacd
                            && self.tags && !program.keep_dst,
                    });
                }
            }
            if self.file_per_program {
                outputs.push(NativeSpaceOutput {
                    path: proposed_destination_for_program(&job, program),
                    program_index: index, audio_bytes: program_bytes,
                    largest_item: largest, item_count: program.tracks.len(),
                    taggable_dsf: self.contents.format == DiscFormat::Sacd
                        && self.tags && !program.keep_dst,
                });
            }
        }
        Some(NativeSpaceKey {
            outputs, format: self.contents.format, continuous: self.file_per_program,
            overwrite: self.overwrite_policy, artwork: self.tags && self.tag_artwork,
            source: self.source_path.clone(), cue: self.cue,
            extraction_log: self.extraction_log,
        })
    }

    /// Budget for a shared filesystem; the same cached inventory also
    /// produces separate working/destination budgets for cross-volume output.
    /// Skip excludes untouched targets. Replace budgets the
    /// peak of sequential private outputs, net of previously replaced files,
    /// rather than requiring a second copy of every existing destination.
    /// Cached results avoid directory scans on every redraw. The launch action
    /// calls this with `refresh` so external destination changes are observed.
    pub(crate) fn native_required_volume_bytes(&self, refresh: bool) -> Option<u64> {
        use crate::convert::pipeline::native_extract::{native_artwork_path, NativeOverwritePolicy};
        let key = self.native_space_key()?;
        if !refresh {
            if let Some(cache) = self.native_space_cache.borrow().as_ref() {
                if cache.key == key { return Some(cache.required); }
            }
        }
        // An already discovered cover is known additional DSF ID3 payload.
        // Other materializer-provided art is not available until media prep.
        let artwork_bytes = if key.artwork && key.format == DiscFormat::Sacd {
            native_artwork_path(&Default::default(), &key.source)
                .and_then(|path| fs::metadata(path).ok().map(|info| info.len()))
                .unwrap_or(0)
        } else { 0 };
        let mut active = Vec::with_capacity(key.outputs.len());
        for output in &key.outputs {
            let existing = if key.overwrite == NativeOverwritePolicy::Fail { None }
                else { fs::symlink_metadata(&output.path).ok() };
            if key.overwrite == NativeOverwritePolicy::Skip && existing.is_some() { continue; }
            let extra = if output.taggable_dsf {
                crate::dsf_tags::REWRITE_PADDING_BYTES.saturating_add(artwork_bytes)
            } else { 0 };
            active.push((output, existing.filter(|meta| meta.is_file()).map(|meta| meta.len()), extra));
        }
        let required = if key.overwrite == NativeOverwritePolicy::Replace {
            // The continuous worker retains completed component files in its
            // private directory until the job finishes; the PCM join spool is
            // additionally live while the final container is being written.
            let mut previous_delta: i128 = 0;
            let mut retained_parts = 0u64;
            let mut peak: i128 = 0;
            for (output, existing, extra) in &active {
                let overhead = (output.item_count as u64).saturating_mul(128 * 1024);
                let final_bytes = output.audio_bytes.saturating_add(*extra).saturating_add(overhead);
                // After artwork-first DSF writes, an ordinary text edit may
                // journal the existing ID3 tail in place. The journal holds
                // metadata, never a second copy of the audio payload.
                let tail_journal = if key.artwork && output.taggable_dsf { *extra } else { 0 };
                let work = if key.format == DiscFormat::Sacd {
                    final_bytes.saturating_add(if key.continuous { output.audio_bytes } else { 0 })
                } else if key.continuous {
                    final_bytes.saturating_add(output.audio_bytes.saturating_mul(2))
                        .saturating_add(output.largest_item.saturating_mul(3))
                } else {
                    final_bytes.saturating_add(output.audio_bytes.saturating_mul(3))
                };
                peak = peak.max(previous_delta + i128::from(retained_parts)
                    + i128::from(work.saturating_add(tail_journal)));
                previous_delta += i128::from(final_bytes) - i128::from(existing.unwrap_or(0));
                if key.continuous { retained_parts = retained_parts.saturating_add(output.audio_bytes); }
                peak = peak.max(previous_delta + i128::from(retained_parts));
            }
            peak.max(0).min(i128::from(u64::MAX)) as u64
        } else {
            let mut total = 0u64;
            let mut largest = 0u64;
            let mut per_program = std::collections::BTreeMap::<usize,u64>::new();
            let mut items = 0usize;
            let mut extra = 0u64;
            for (output, _, metadata_extra) in &active {
                total = total.saturating_add(output.audio_bytes);
                largest = largest.max(output.largest_item);
                let group = per_program.entry(output.program_index).or_default();
                *group = group.saturating_add(output.audio_bytes);
                items = items.saturating_add(output.item_count);
                extra = extra.saturating_add(*metadata_extra);
            }
            let largest_program = per_program.values().copied().max().unwrap_or(0);
            self.native_bytes_with_allowance(total, largest, largest_program, items)
                .saturating_add(extra)
        };
        // Only one DSF text update runs at a time. The largest metadata tail
        // may be duplicated temporarily in its rollback journal.
        let tail_journal = if key.overwrite != NativeOverwritePolicy::Replace && key.artwork {
            active.iter().filter(|(output, _, _)| output.taggable_dsf)
                .map(|(_, _, extra)| *extra).max().unwrap_or(0)
        } else { 0 };
        let extras = (if key.extraction_log { 128 * 1024 } else { 0 })
            + (if key.cue { (active.len() as u64).saturating_mul(128 * 1024) } else { 0 });
        let required = required.saturating_add(extras).saturating_add(tail_journal);

        // With a separate scratch volume, completed files are published one
        // at a time. Scratch holds the private output and its intermediates;
        // destination holds only the net published media. During a replacement
        // the old destination remains allocated until the atomic rename, so
        // charge the full new output *before* subtracting the old allocation.
        let mut destination_delta: i128 = 0;
        let mut destination_peak: i128 = 0;
        let mut retained_parts = 0u64;
        let mut working_peak = 0u64;
        for (output, existing, extra) in &active {
            let overhead = (output.item_count as u64).saturating_mul(128 * 1024);
            let final_bytes = output.audio_bytes.saturating_add(*extra).saturating_add(overhead);
            destination_peak = destination_peak.max(destination_delta + i128::from(final_bytes));
            destination_delta += i128::from(final_bytes) - i128::from(existing.unwrap_or(0));
            destination_peak = destination_peak.max(destination_delta);

            let work = if key.format == DiscFormat::Sacd {
                final_bytes.saturating_add(if key.continuous { output.audio_bytes } else { 0 })
            } else if key.continuous {
                final_bytes.saturating_add(output.audio_bytes.saturating_mul(2))
                    .saturating_add(output.largest_item.saturating_mul(3))
            } else {
                final_bytes.saturating_add(output.largest_item.saturating_mul(3))
            };
            let journal = if key.artwork && output.taggable_dsf { *extra } else { 0 };
            working_peak = working_peak.max(retained_parts.saturating_add(work).saturating_add(journal));
            if key.continuous { retained_parts = retained_parts.saturating_add(output.audio_bytes); }
            working_peak = working_peak.max(retained_parts);
        }
        let destination_required = destination_peak.max(0).min(i128::from(u64::MAX)) as u64;
        let destination_required = destination_required.saturating_add(extras);
        let working_required = working_peak.saturating_add(extras);
        *self.native_space_cache.borrow_mut() = Some(NativeSpaceCache {
            key, required, working_required, destination_required,
        });
        Some(required)
    }

    /// Required free bytes on (destination, scratch) when the volumes differ.
    /// The same inventory/cache as the shared-volume estimator is used.
    pub(crate) fn native_split_volume_bytes(&self, refresh: bool) -> Option<(u64, u64)> {
        self.native_required_volume_bytes(refresh)?;
        self.native_space_cache.borrow().as_ref()
            .map(|cache| (cache.destination_required, cache.working_required))
    }

    pub(crate) fn native_storage_shares_destination_volume(&self, scratch: &Path) -> bool {
        match (existing_ancestor(&self.destination), existing_ancestor(scratch)) {
            (Ok(destination), Ok(working)) => same_filesystem_paths(destination, working),
            _ => false,
        }
    }

    pub fn shared_program(&self, left: usize, right: usize) -> bool {
        let Some(a) = self.contents.presentations.get(left) else { return false; };
        let Some(b) = self.contents.presentations.get(right) else { return false; };
        match (a.id, b.id) {
            (PresentationId::DvdVideoTitle { vts_number: av, title_number: at, .. },
             PresentationId::DvdVideoTitle { vts_number: bv, title_number: bt, .. }) => av == bv && at == bt,
            (PresentationId::BluRayTitle { playlist_number: ap, .. },
             PresentationId::BluRayTitle { playlist_number: bp, .. }) => ap == bp,
            _ => left == right,
        }
    }

    /// Chapter membership is shared within a DVD title/Blu-ray playlist,
    /// but checking a chapter must not silently select an audio stream the
    /// user explicitly unchecked. Only the focused stream is reselected.
    pub fn set_item_checked(&mut self, index: usize, checked: bool) {
        if index >= self.contents.presentations.get(self.cursor).map_or(0, |p| p.tracks.len()) {
            return;
        }
        for i in 0..self.len() {
            if self.shared_program(self.cursor, i) && index < self.items_checked[i].len() {
                self.items_checked[i][index] = checked;
                if !self.items_checked[i].iter().any(|value| *value) {
                    self.selected[i] = false;
                }
            }
        }
        if checked && self.is_extractable(self.cursor) { self.selected[self.cursor] = true; }
    }

    pub fn toggle_item(&mut self, index: usize) {
        if let Some(checked) = self.items_checked.get(self.cursor).and_then(|row| row.get(index)) {
            self.set_item_checked(index, !*checked);
        }
    }

    /// Extend selection using explicit check operations, not a series of
    /// toggles. Repeating Shift+Down is idempotent over the selected range.
    pub fn check_item_range(&mut self, from: usize, to: usize) {
        let len = self.items_checked.get(self.cursor).map_or(0, Vec::len);
        if len == 0 { return; }
        for index in from.min(to)..=from.max(to).min(len - 1) {
            self.set_item_checked(index, true);
        }
    }

    pub fn check_focus(&mut self, mode: char) {
        match self.focus {
            ExtractFocus::Programs => {
                for i in 0..self.len() {
                    let old = self.selected[i];
                    self.selected[i] = self.is_extractable(i) && !self.items_checked[i].is_empty() &&
                        match mode { 'a' => true, 'n' => false, _ => !old };
                    if self.selected[i] && !self.items_checked[i].iter().any(|checked| *checked) {
                        self.items_checked[i].fill(true);
                    }
                }
            }
            ExtractFocus::Items => {
                let n = self.items_checked.get(self.cursor).map_or(0, Vec::len);
                for i in 0..n {
                    let old = self.items_checked[self.cursor][i];
                    let wanted = match mode { 'a' => true, 'n' => false, _ => !old };
                    if old != wanted { self.set_item_checked(i, wanted); }
                }
            }
            ExtractFocus::Output => {}
        }
    }

    /// DVD-Audio More presets select groups only; authored track checkboxes
    /// and the Convert pipeline retain their existing state. Ineligible groups
    /// stay uncheckable under every preset.
    pub fn cycle_dvda_group_preset(&mut self) {
        if self.contents.format != DiscFormat::DvdAudio { return; }
        self.dvda_group_preset = (self.dvda_group_preset + 1) % 4;
        let max = self.contents.presentations.iter().enumerate()
            .filter(|(i,_)|self.is_extractable(*i))
            .map(|(_,p)| (p.format.sample_rate.unwrap_or(0),p.format.bit_depth.unwrap_or(0)))
            .max().unwrap_or((0,0));
        for i in 0..self.len() {
            let p = &self.contents.presentations[i];
            self.selected[i] = self.is_extractable(i) && match self.dvda_group_preset {
                0 => true,
                1 => p.format.channels.is_some_and(|channels| channels <= 2),
                2 => p.format.channels.is_some_and(|channels| channels > 2),
                _ => (p.format.sample_rate.unwrap_or(0),p.format.bit_depth.unwrap_or(0)) == max,
            };
        }
    }

    pub fn output_rows(&self) -> Vec<&'static str> {
        let mut rows = vec!["Format", "Files", "Tags", "Destination"];
        if self.contents.format == DiscFormat::DvdAudio && self.contents.presentations.iter().enumerate()
            .any(|(i,p)| self.selected.get(i) == Some(&true) && p.format.channels.unwrap_or(0) > 2) {
            rows.push("Downmix");
        }
        if self.contents.format == DiscFormat::DvdAudio && self.contents.copy_protection.description.to_lowercase().contains("mkb") {
            rows.push("Assume decrypted");
        }
        rows
    }

    pub fn current_output_panel(&self) -> ExtractPanel {
        match self.output_rows().get(self.output_cursor).copied().unwrap_or("Format") {
            "Files" => ExtractPanel::Files,
            "Tags" => ExtractPanel::Tags,
            "Destination" => ExtractPanel::Destination,
            "Downmix" => ExtractPanel::Downmix,
            "Assume decrypted" => ExtractPanel::AssumeDecrypted,
            _ => ExtractPanel::Format,
        }
    }

    pub fn protection_display(&self) -> String {
        if self.contents.format != DiscFormat::DvdVideo {
            return self.contents.copy_protection.description.clone();
        }
        if self.probe_statuses.iter().any(Option::is_none) {
            "CSS sample probes pending".into()
        } else if self.probe_statuses.iter().flatten().any(Result::is_err) {
            "One or more sampled DVD-Video streams could not be read".into()
        } else {
            "No scrambling observed in sampled chapter starts".into()
        }
    }

    pub fn protection_blocker(&self) -> Option<String> {
        let description = self.contents.copy_protection.description.to_ascii_lowercase();
        match self.contents.format {
            DiscFormat::Sacd => None,
            DiscFormat::DvdAudio if description.contains("mkb") && !self.assume_decrypted =>
                Some("CPPM/MKB found. Use Assume decrypted only if the copy is already decrypted".into()),
            DiscFormat::BluRay if description.contains("not handled") || description.contains("cannot") || description.contains("not readable") || description.contains("unknown / probe failed") =>
                Some("Blu-ray protection cannot be read by the installed libraries".into()),
            // DVD-Video mapper describes its CSS *preflight capability*, not
            // positive CSS evidence. The per-chapter demuxer rejects encrypted
            // sectors; do not mark every ordinary DVD-Video as protected.
            _ => None,
        }
    }

    pub fn blockers(&self, scratch: &Path) -> Vec<String> {
        self.blockers_with_capacity_refresh(scratch, false)
    }

    /// The extract action rechecks existence and sizes immediately before
    /// queue submission; painting the overlay uses the cached result.
    pub(crate) fn blockers_with_capacity_refresh(&self, scratch: &Path, refresh: bool) -> Vec<String> {
        let mut problems = Vec::new();
        if self.selected_item_count() == 0 { problems.push("Check at least one extractable track or chapter".into()); }
        if self.selected.iter().enumerate().any(|(i,checked)| *checked && !self.is_extractable(i)) {
            problems.push("One or more checked programs cannot be extracted".into());
        }
        if let Some(message) = self.protection_blocker() { problems.push(message); }
        for (index, selected) in self.selected.iter().enumerate() {
            if !*selected { continue; }
            match self.probe_statuses.get(index) {
                Some(None) => problems.push(format!("Probe pending for program {}: source eligibility and depth", index + 1)),
                Some(Some(Err(reason))) => problems.push(format!("Program {} cannot be extracted: {reason}", index + 1)),
                _ => {},
            }
        }
        if self.cue && self.contents.format == DiscFormat::Sacd {
            problems.push("CUE indexes cannot describe DSF/DFF media; disable CUE for continuous native SACD".into());
        }
        if !self.destination.is_absolute() || !scratch.is_absolute() {
            problems.push("Choose an absolute destination and working-storage path".into());
        } else if refresh {
            // Launch-time validation uses the worker's canonical path check;
            // redraws do not repeat this filesystem walk.
            if let Err(error) = crate::convert::pipeline::native_extract::native_working_storage(
                scratch, &self.destination)
            {
                problems.push(error);
            }
        }
        if self.selected_item_count() != 0 {
            if let Some((total, largest, largest_program)) = self.selected_item_bytes() {
                let destination_ancestor = existing_ancestor(&self.destination);
                let scratch_ancestor = existing_ancestor(scratch);
                if let (Ok(destination_ancestor), Ok(scratch_ancestor)) =
                    (&destination_ancestor, &scratch_ancestor)
                {
                    if same_filesystem_paths(destination_ancestor, scratch_ancestor) {
                        if let Ok(free) = fs2::available_space(destination_ancestor) {
                            let needed = self.native_required_volume_bytes(refresh)
                                .unwrap_or_else(||self.required_native_volume_bytes(total, largest, largest_program));
                            if free < needed {
                                problems.push(format!("Destination/working filesystem needs {}, has {}",
                                    bytes_display(needed), bytes_display(free)));
                            }
                        }
                    } else if let Some((destination_needed, working_needed)) =
                        self.native_split_volume_bytes(refresh)
                    {
                        if let Ok(free) = fs2::available_space(destination_ancestor) {
                            if free < destination_needed {
                                problems.push(format!("Destination filesystem needs {}, has {}",
                                    bytes_display(destination_needed), bytes_display(free)));
                            }
                        }
                        if let Ok(free) = fs2::available_space(scratch_ancestor) {
                            if free < working_needed {
                                problems.push(format!("Working filesystem needs {}, has {}",
                                    bytes_display(working_needed), bytes_display(free)));
                            }
                        }
                    }
                } else if refresh {
                    problems.push("Cannot inspect destination or working-storage filesystem".into());
                }
            } else {
                problems.push("Source size pending: selected duration/rate/depth is not yet known".into());
            }
        }
        problems
    }

    pub fn native_job(&self, working_root: PathBuf) -> crate::convert::pipeline::native_extract::NativeExtractionJob {
        use crate::convert::pipeline::native_extract::*;
        let programs = self.contents.presentations.iter().enumerate().filter(|(i,_)| self.selected[*i] && self.is_extractable(*i))
            .filter_map(|(i,p)| {
                let sacd_tags = self.contents.format == DiscFormat::Sacd;
                let sidecar_first = self.tag_source == NativeTagSource::DiscAndSidecar;
                let tags_to_native = |tags: crate::disc::model::DiscExtractTagFields| NativeTagValues {
                    title: tags.title, artist: tags.artist, album: tags.album,
                    genre: tags.genre, year: tags.year, composer: tags.composer,
                    performer: tags.performer,
                };
                let tracks: Vec<NativeTrackChoice> = p.tracks.iter().enumerate()
                    .filter(|(j,_)| self.items_checked[i].get(*j) == Some(&true))
                    .map(|(j,t)| NativeTrackChoice { number: (j + 1) as u32,
                        name: t.title.clone().unwrap_or_else(|| format!("Track {}", t.number)),
                        estimated_bytes: self.native_item_payload_bytes(p, t),
                        // DVD-Video/Blu-ray mappers synthesize "Chapter n"
                        // for navigation. It is a filename fallback, never a
                        // discovered metadata field that should become a tag.
                        tags: if sacd_tags {
                            // Resolve track-to-album fallback *within* each
                            // authority before applying the selected source
                            // precedence. Otherwise a lower-priority sidecar
                            // track DATE/GENRE can override authored album data.
                            let authored = p.extract.authored_track_tags.get(j).cloned()
                                .unwrap_or_default().prioritized(&p.extract.authored_album_tags);
                            let sidecar = p.extract.sidecar_track_tags.get(j).cloned()
                                .unwrap_or_default().prioritized(&p.extract.sidecar_album_tags);
                            tags_to_native(if sidecar_first { sidecar.prioritized(&authored) }
                                else { authored.prioritized(&sidecar) })
                        } else { NativeTagValues {
                            title: if matches!(self.contents.format, DiscFormat::DvdVideo | DiscFormat::BluRay) {
                                None
                            } else { t.title.clone() },
                            performer:t.performer.clone(), artist:t.performer.clone(), ..Default::default()
                        } } }).collect();
                (!tracks.is_empty()).then(|| NativeProgramChoice{
                    id:p.id, angle:self.selected_angle_for(i), label:p.id.compact_label(), tracks,
                    keep_dst:self.keep_dst.get(i) == Some(&true),
                    source_depth:p.format.bit_depth, source_channels:p.format.channels,
                    source_rate:p.format.sample_rate,
                    // The DVD-Video IFO mapper labels 5.1 from *count*, not
                    // speaker assignments. Do not treat it as an authoritative
                    // channel mask. Keep observed nonzero WAV masks instead.
                    source_layout: if self.contents.format == DiscFormat::DvdVideo && p.format.channels.unwrap_or(0) > 2 {
                        None
                    } else { p.format.channel_layout.clone() },
                    is_lossless:p.format.lossless,
                    tags: if sacd_tags {
                        let authored = &p.extract.authored_album_tags;
                        let sidecar = &p.extract.sidecar_album_tags;
                        tags_to_native(if sidecar_first { sidecar.prioritized(authored) }
                            else { authored.prioritized(sidecar) })
                    } else {
                        let metadata = metadata_for_disc_presentation(&self.contents, Some(p));
                        NativeTagValues {title:None,artist:metadata.artist,album:metadata.album,
                            genre:metadata.genre,year:metadata.year,..Default::default()}
                    } })
            }).collect();
        NativeExtractionJob{ source:self.source_path.clone(),format:self.contents.format,
            programs,destination:self.destination.clone(), working_root,
            wav_container:self.wav_container, assume_decrypted:self.assume_decrypted,downmix:self.downmix,
            naming_template:self.naming_template.clone(), program_subfolders:self.program_subfolders,
            overwrite_policy:self.overwrite_policy, extraction_log:self.extraction_log,
            file_per_program:self.file_per_program, cue:self.cue,
            transfer_tags:self.tags,tag_source:self.tag_source,
            tag_fields:self.tag_fields,tag_artwork:self.tag_artwork }
    }
}

fn bytes_display(bytes: u64) -> String {
    let mut value = bytes as f64;
    let mut unit = "B";
    for next in ["KiB", "MiB", "GiB", "TiB"] {
        if value < 1024.0 { break; }
        value /= 1024.0;
        unit = next;
    }
    format!("{value:.1} {unit}")
}

fn existing_ancestor(path: &Path) -> Result<&Path, String> {
    let mut p = path;
    while !p.exists() { p = p.parent().ok_or("Destination has no existing ancestor")?; }
    Ok(p)
}

#[cfg(unix)]
fn same_filesystem_paths(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (fs::metadata(a), fs::metadata(b)) { (Ok(a), Ok(b)) => a.dev() == b.dev(), _ => false }
}
#[cfg(windows)]
fn same_filesystem_paths(a: &Path, b: &Path) -> bool {
    // GetFileInformationByHandle accepts directory handles opened with
    // FILE_FLAG_BACKUP_SEMANTICS. Neither that flag nor access_mode(0)
    // requires nightly Rust or read permission on the directory contents.
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_ID_INFO_CLASS: i32 = 0x12;
    #[repr(C)]
    struct ByHandleFileInfo {
        _attributes: u32,
        _creation_time: [u32; 2],
        _access_time: [u32; 2],
        _write_time: [u32; 2],
        volume_serial: u32,
        _size_high: u32,
        _size_low: u32,
        _links: u32,
        _index_high: u32,
        _index_low: u32,
    }
    #[repr(C)]
    struct FileIdInfo {
        volume_serial: u64,
        _file_id: [u8; 16],
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetFileInformationByHandle(
            handle: *mut std::ffi::c_void,
            info: *mut ByHandleFileInfo,
        ) -> i32;
        fn GetFileInformationByHandleEx(
            handle: *mut std::ffi::c_void,
            class: i32,
            info: *mut std::ffi::c_void,
            size: u32,
        ) -> i32;
    }

    fn volume_identity(path: &Path) -> Option<(u32, Option<u64>)> {
        use std::os::windows::fs::OpenOptionsExt;
        use std::os::windows::io::AsRawHandle;
        let dir = fs::OpenOptions::new()
            .access_mode(0)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path).ok()?;
        let handle = dir.as_raw_handle();
        let mut basic = std::mem::MaybeUninit::<ByHandleFileInfo>::uninit();
        if unsafe { GetFileInformationByHandle(handle, basic.as_mut_ptr()) } == 0 {
            return None;
        }
        let serial = unsafe { basic.assume_init() }.volume_serial;
        // Modern filesystems provide a 64-bit volume identity. The 32-bit
        // API remains the fallback for FAT and older/network filesystems.
        let mut extended = std::mem::MaybeUninit::<FileIdInfo>::uninit();
        let extended_serial = (unsafe {
            GetFileInformationByHandleEx(
                handle,
                FILE_ID_INFO_CLASS,
                extended.as_mut_ptr().cast(),
                std::mem::size_of::<FileIdInfo>() as u32,
            )
        } != 0).then(|| unsafe { extended.assume_init().volume_serial });
        Some((serial, extended_serial))
    }

    match (volume_identity(a), volume_identity(b)) {
        (Some((legacy_a, full_a)), Some((legacy_b, full_b))) => {
            if let (Some(full_a), Some(full_b)) = (full_a, full_b) {
                full_a == full_b
            } else {
                legacy_a == legacy_b
            }
        }
        _ => false,
    }
}
#[cfg(not(any(unix, windows)))]
fn same_filesystem_paths(_a: &Path, _b: &Path) -> bool { false }

#[cfg(all(test, windows))]
mod windows_native_volume_tests {
    use super::*;

    #[test]
    fn existing_directories_on_one_volume_use_the_combined_budget() {
        let temp = tempfile::tempdir().expect("tempdir");
        let destination = temp.path().join("destination");
        let scratch = temp.path().join("scratch");
        fs::create_dir_all(&destination).expect("destination dir");
        fs::create_dir_all(&scratch).expect("scratch dir");
        assert!(same_filesystem_paths(&destination, &scratch));

        let contents = super::disc_selection_bridge_tests::disc_contents(
            DiscFormat::Sacd,
            PresentationId::SacdArea(SacdAreaId::Stereo),
            "DSD Stereo",
        );
        let mut state = DiscBrowserState::new(contents, temp.path().join("disc.iso"));
        state.contents.presentations[0].format.sample_rate = Some(2_822_400);
        state.contents.presentations[0].format.bit_depth = Some(1);
        state.destination = destination;
        assert!(state.native_storage_shares_destination_volume(&scratch));
        assert!(state.native_required_volume_bytes(true).is_some());
    }

    #[test]
    fn distinct_volume_uses_separate_destination_and_scratch_budgets() {
        // Set this to an existing directory on a physically different volume
        // (e.g. D:\test-scratch). Single-volume CI hosts skip this case.
        let Some(other_volume) = std::env::var_os("TONEPOET_TEST_OTHER_VOLUME") else {
            return;
        };
        let other_volume = PathBuf::from(other_volume);
        assert!(other_volume.is_dir(), "other-volume directory does not exist");
        let temp = tempfile::tempdir().expect("tempdir");
        assert!(!same_filesystem_paths(temp.path(), &other_volume),
            "TONEPOET_TEST_OTHER_VOLUME must be a distinct volume");
        let contents = super::disc_selection_bridge_tests::disc_contents(
            DiscFormat::Sacd,
            PresentationId::SacdArea(SacdAreaId::Stereo),
            "DSD Stereo",
        );
        let mut state = DiscBrowserState::new(contents, temp.path().join("disc.iso"));
        state.contents.presentations[0].format.sample_rate = Some(2_822_400);
        state.contents.presentations[0].format.bit_depth = Some(1);
        state.destination = temp.path().join("destination");
        assert!(!state.native_storage_shares_destination_volume(&other_volume));
        assert!(state.native_split_volume_bytes(true).is_some());
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

    let mut parts = vec![format!(
        "{} audio {}",
        stream_count,
        plural(stream_count, "stream", "streams")
    )];
    parts.push(format!(
        "{} {}",
        track_count,
        plural(track_count, "track", "tracks")
    ));
    append_nonzero_channel_summary_parts(&mut parts, multichannel_count, stereo_count);
    parts.join(" · ")
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
    let mut lines = vec![format!(
        "content: {} audio {} · {} {}",
        stream_count,
        plural(stream_count, "stream", "streams"),
        track_count,
        plural(track_count, "track", "tracks"),
    )];

    let mut channel_parts = Vec::new();
    append_nonzero_channel_summary_parts(&mut channel_parts, multichannel_count, stereo_count);
    if !channel_parts.is_empty() {
        lines.push(format!("         {}", channel_parts.join(" · ")));
    }

    lines
}

fn append_nonzero_channel_summary_parts(
    parts: &mut Vec<String>,
    multichannel_count: usize,
    stereo_count: usize,
) {
    if multichannel_count > 0 {
        parts.push(format!("{multichannel_count} multichannel"));
    }
    if stereo_count > 0 {
        parts.push(format!("{stereo_count} stereo"));
    }
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
    parts.push(
        fmt.codec
            .clone()
            .unwrap_or_else(|| presentation.label.clone()),
    );

    let mut technical = Vec::new();
    if let Some(bits) = fmt.bit_depth {
        technical.push(format!("{bits}-bit"));
    }
    if let Some(rate) = fmt.sample_rate {
        technical.push(sample_rate_display(rate));
    }
    if !technical.is_empty() {
        parts.push(technical.join("/"));
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
        sample_format_is_float: None,
        compression_is_lossless: None,
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
            sidecar_cue_track_metadata: None,
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
            sidecar_cue_track_metadata: None,
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
            sidecar_cue_track_metadata: None,
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
            extract: crate::disc::model::PresentationExtractInfo::default(),
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

    #[test]
    fn disc_summary_suppresses_zero_channel_counts() {
        let stereo_only = contents(vec![
            presentation("Stereo A", "LPCM", 2, "stereo", 24, 96_000, 1),
            presentation("Stereo B", "LPCM", 2, "stereo", 24, 96_000, 2),
        ]);
        assert_eq!(disc_summary(&stereo_only), "2 audio streams · 2 tracks · 2 stereo");
        assert_eq!(
            disc_content_summary_lines(&stereo_only),
            vec![
                "content: 2 audio streams · 2 tracks".to_string(),
                "         2 stereo".to_string(),
            ]
        );

        let multichannel_only = contents(vec![presentation("Surround", "DTS-HD MA", 6, "5.1", 24, 96_000, 1)]);
        assert_eq!(disc_summary(&multichannel_only), "1 audio stream · 1 track · 1 multichannel");
        assert_eq!(
            disc_content_summary_lines(&multichannel_only),
            vec![
                "content: 1 audio stream · 1 track".to_string(),
                "         1 multichannel".to_string(),
            ]
        );
    }
}

#[cfg(test)]
mod disc_selection_bridge_tests {
    use super::*;
    use crate::disc::model::{AudioPresentationFormat, CopyProtectionSummary, DiscFormat, DiscPresentation, DiscTrack, FormatProvenance, PresentationId};
    use crate::convert::pipeline::{CueSidecarPolicy, DvdaDownmixPolicy, DvdaGroupSelection, SourceOptions, TrackSelection};
    use std::path::PathBuf;

    pub(super) fn disc_contents(
        format: DiscFormat,
        id: PresentationId,
        label: &str,
    ) -> DiscContents {
        DiscContents {
            format,
            label: "Disc".to_string(),
            source_path: PathBuf::from("disc.iso"),
            presentations: vec![DiscPresentation {
                extract: crate::disc::model::PresentationExtractInfo::default(),
                id,
                label: label.to_string(),
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

    fn blank_source_options() -> SourceOptions {
        SourceOptions {
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
            sidecar_cue_track_metadata: None,
            cue_sidecar: CueSidecarPolicy::PreferSidecar,
            track_selection: TrackSelection::All,
        }
    }

    #[test]
    fn source_probe_blocks_only_its_own_program_and_not_convert() {
        let good = disc_contents(DiscFormat::DvdVideo,
            PresentationId::dvd_video(1, 1, 0), "English");
        let mut contents = good.clone();
        let mut alternate = good.presentations[0].clone();
        alternate.id = PresentationId::dvd_video(1, 1, 1);
        alternate.label = "Commentary".into();
        contents.presentations.push(alternate);
        let mut state = DiscBrowserState::new(contents, "dvd".into());
        assert!(state.probe_statuses.iter().all(Option::is_none));
        state.probe_statuses[0] = Some(Ok(Default::default()));
        state.probe_statuses[1] = Some(Err("CSS in source sample".into()));
        assert!(state.is_extractable(0));
        assert!(!state.is_extractable(1));
        // The same stream remains valid for the unchanged Convert route.
        assert!(presentation_id_supports_stream_conversion(&state.contents.presentations[1].id));
        state.selected[1] = false;
        assert!(!state.blockers(std::path::Path::new("/tmp")).iter()
            .any(|reason| reason.contains("CSS")));
    }

    #[test]
    fn sacd_native_job_retains_selected_authored_vs_sidecar_precedence() {
        use crate::disc::model::{SacdAreaId, DiscExtractTagFields};
        use crate::convert::pipeline::native_extract::NativeTagSource;
        let contents = disc_contents(DiscFormat::Sacd, PresentationId::SacdArea(SacdAreaId::Stereo), "DSD Stereo");
        let mut state = DiscBrowserState::new(contents, "disc.iso".into());
        let tags = &mut state.contents.presentations[0].extract;
        tags.authored_album_tags = DiscExtractTagFields { album: Some("disc album".into()),
            artist: Some("disc artist".into()), ..Default::default() };
        tags.sidecar_album_tags = DiscExtractTagFields { album: Some("sidecar album".into()),
            ..Default::default() };
        tags.authored_track_tags = vec![DiscExtractTagFields { title: Some("disc title".into()),
            composer: Some("disc composer".into()), ..Default::default() }];
        tags.sidecar_track_tags = vec![DiscExtractTagFields { title: Some("sidecar title".into()),
            composer: Some("sidecar composer".into()), ..Default::default() }];
        state.tag_source = NativeTagSource::DiscAndSidecar;
        let job = state.native_job("/tmp".into());
        assert_eq!(job.programs[0].tags.album.as_deref(), Some("sidecar album"));
        assert_eq!(job.programs[0].tags.artist.as_deref(), Some("disc artist"));
        assert_eq!(job.programs[0].tracks[0].tags.title.as_deref(), Some("sidecar title"));
        state.tag_source = NativeTagSource::AuthoredTrack;
        let job = state.native_job("/tmp".into());
        assert_eq!(job.programs[0].tags.album.as_deref(), Some("disc album"));
        assert_eq!(job.programs[0].tracks[0].tags.title.as_deref(), Some("disc title"));
        assert_eq!(job.programs[0].tracks[0].tags.composer.as_deref(), Some("disc composer"));
    }

    #[test]
    fn sacd_tag_precedence_resolves_track_and_album_within_each_source() {
        use crate::disc::model::{DiscExtractTagFields, SacdAreaId};
        use crate::convert::pipeline::native_extract::NativeTagSource;

        let contents = disc_contents(DiscFormat::Sacd,
            PresentationId::SacdArea(SacdAreaId::Stereo), "DSD Stereo");
        let mut state = DiscBrowserState::new(contents, "disc.iso".into());
        let presentation = &mut state.contents.presentations[0];
        presentation.extract.authored_album_tags = DiscExtractTagFields {
            album: Some("Authored album".into()),
            artist: Some("Authored album artist".into()),
            genre: Some("Classical".into()), year: Some("1998".into()),
            ..Default::default()
        };
        presentation.extract.sidecar_album_tags = DiscExtractTagFields {
            album: Some("Sidecar album".into()),
            artist: Some("Sidecar album artist".into()),
            ..Default::default()
        };
        presentation.extract.authored_track_tags = vec![DiscExtractTagFields {
            title: Some("Authored title".into()), ..Default::default()
        }];
        presentation.extract.sidecar_track_tags = vec![DiscExtractTagFields {
            title: Some("Sidecar title".into()),
            year: Some("2024".into()), genre: Some("Alternative".into()),
            ..Default::default()
        }];
        state.tags = true;

        state.tag_source = NativeTagSource::AuthoredTrack;
        let job = state.native_job("/tmp".into());
        let track = &job.programs[0].tracks[0].tags;
        assert_eq!(track.title.as_deref(), Some("Authored title"));
        assert_eq!(track.artist.as_deref(), Some("Authored album artist"));
        assert_eq!(track.album.as_deref(), Some("Authored album"));
        assert_eq!(track.year.as_deref(), Some("1998"));
        assert_eq!(track.genre.as_deref(), Some("Classical"));
        assert!(job.transfer_tags);

        state.tag_source = NativeTagSource::DiscAndSidecar;
        let job = state.native_job("/tmp".into());
        let track = &job.programs[0].tracks[0].tags;
        assert_eq!(track.title.as_deref(), Some("Sidecar title"));
        assert_eq!(track.year.as_deref(), Some("2024"));
        assert_eq!(track.genre.as_deref(), Some("Alternative"));
        assert_eq!(track.artist.as_deref(), Some("Sidecar album artist"));

        // Missing values within an authority fall through only after its
        // album fields have been tried. A missing authored genre may then
        // use the sidecar track's genre.
        state.contents.presentations[0].extract.authored_album_tags.genre = None;
        state.contents.presentations[0].extract.authored_track_tags[0].title = None;
        state.tag_source = NativeTagSource::AuthoredTrack;
        let job = state.native_job("/tmp".into());
        let track = &job.programs[0].tracks[0].tags;
        assert_eq!(track.title.as_deref(), Some("Sidecar title"));
        assert_eq!(track.genre.as_deref(), Some("Alternative"));
        assert_eq!(track.year.as_deref(), Some("1998"));

        state.tags = false;
        assert!(!state.native_job("/tmp".into()).transfer_tags);
    }

    #[test]
    fn sacd_continuous_storage_budget_excludes_pcm_join_spool() {
        use crate::disc::model::SacdAreaId;
        const GIB: u64 = 1024 * 1024 * 1024;
        let contents = disc_contents(DiscFormat::Sacd,
            PresentationId::SacdArea(SacdAreaId::MultiChannel), "DSD Multichannel");
        let mut state = DiscBrowserState::new(contents, "disc.iso".into());
        state.file_per_program = true;
        state.tags = false;
        state.tag_artwork = false;
        let needed = state.required_native_volume_bytes(10 * GIB, GIB, 10 * GIB);
        assert!(needed <= 25 * GIB, "25 GiB must admit continuous 10 GiB SACD");
        assert!(needed > 19 * GIB, "19 GiB must block continuous 10 GiB SACD");
        assert_eq!(needed, 20 * GIB + 128 * 1024);
        assert_eq!(state.native_required_volume_bytes(true),
            state.selected_item_bytes().map(|(total, largest, program)|
                state.required_native_volume_bytes(total, largest, program)));
        state.file_per_program = false;
        assert_eq!(state.required_native_volume_bytes(10 * GIB, GIB, 10 * GIB),
            10 * GIB + 128 * 1024);
    }

    #[test]
    fn sacd_separate_tracks_preflight_uses_selected_payload_without_pcm_reserve() {
        use crate::disc::model::SacdAreaId;
        const GIB: u64 = 1024 * 1024 * 1024;
        let contents = disc_contents(DiscFormat::Sacd,
            PresentationId::SacdArea(SacdAreaId::Stereo), "DSD Stereo");
        let mut state = DiscBrowserState::new(contents, "disc.iso".into());
        let presentation = &mut state.contents.presentations[0];
        presentation.format.sample_rate = Some(2_822_400);
        presentation.format.bit_depth = Some(1);
        presentation.tracks = (1..=4).map(|number| DiscTrack {
            number, title: None, performer: None, duration_secs: Some(15.0 * 60.0),
            format_note: None,
        }).collect();
        state.items_checked[0] = vec![true; 4];
        state.file_per_program = false;
        state.tags = false;
        state.tag_artwork = false;
        let (total, largest, largest_program) = state.selected_item_bytes().unwrap();
        assert_eq!(total, 2_540_160_000); // Four 15-minute stereo DSD64 tracks.
        assert_eq!(largest * 4, total);
        assert_eq!(largest_program, total);
        let required = state.required_native_volume_bytes(total, largest, largest_program);
        assert_eq!(required, total + 4 * 128 * 1024);
        assert!(required < 3 * GIB, "3 GiB must admit separate-track SACD");
        assert!(required > 2 * GIB, "insufficient output capacity must block");
        // When storage is on another volume, the scratch budget must not
        // accumulate already-published separate tracks a second time.
        let (dest_needed, scratch_needed) = state.native_split_volume_bytes(true).unwrap();
        assert_eq!(dest_needed, required);
        assert!(scratch_needed >= largest && scratch_needed < 2 * largest);
        // The continuous SACD path retains completed parts during joining.
        state.file_per_program = true;
        assert_eq!(state.required_native_volume_bytes(total, largest, largest_program),
            total * 2 + 4 * 128 * 1024);
        let (dest_needed, scratch_needed) = state.native_split_volume_bytes(true).unwrap();
        assert_eq!(dest_needed, total + 4 * 128 * 1024);
        assert_eq!(scratch_needed, total * 2 + 4 * 128 * 1024);
        // The estimator must still reserve additional space for the actual
        // default DSF tag/cover workflow. Audio-only assertions above are
        // explicitly opt-out; this is not a request to reduce the budget.
        let audio_only = state.native_required_volume_bytes(true).unwrap();
        state.tags = true;
        state.tag_artwork = true;
        let with_tags = state.native_required_volume_bytes(true).unwrap();
        assert!(with_tags > audio_only, "default DSF tags must add capacity");
    }

    #[test]
    fn native_space_skip_and_replace_follow_the_workers_output_paths() {
        use crate::convert::pipeline::native_extract::{
            proposed_destination, proposed_destination_for_program, NativeOverwritePolicy,
        };
        use crate::disc::model::SacdAreaId;
        const GIB: u64 = 1024 * 1024 * 1024;
        let temp = tempfile::tempdir().unwrap();
        let contents = disc_contents(DiscFormat::Sacd,
            PresentationId::SacdArea(SacdAreaId::Stereo), "DSD Stereo");
        let mut state = DiscBrowserState::new(contents, temp.path().join("album.iso"));
        state.destination = temp.path().join("out");
        state.contents.presentations[0].format.sample_rate = Some(2_822_400);
        state.contents.presentations[0].format.bit_depth = Some(1);
        state.contents.presentations[0].tracks = (1..=4).map(|number| DiscTrack {
            number, title: Some(format!("Track {number}")), performer: None,
            duration_secs: Some(900.0), format_note: None,
        }).collect();
        state.items_checked[0] = vec![true; 4];
        let job = state.native_job(temp.path().into());
        let targets = job.programs[0].tracks.iter().map(|track|
            proposed_destination(&job, &job.programs[0], track)).collect::<Vec<_>>();
        for target in targets.iter().take(3) {
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::File::create(target).unwrap().set_len(635_040_000).unwrap();
        }
        state.overwrite_policy = NativeOverwritePolicy::Skip;
        let missing = state.native_required_volume_bytes(true).unwrap();
        assert!(missing < GIB, "1 GiB must admit the one missing 606 MiB track: {missing}");
        assert!(missing > 600 * 1024 * 1024);

        // Redraw uses the prior inventory, but launching a job refreshes it.
        std::fs::File::create(&targets[3]).unwrap().set_len(635_040_000).unwrap();
        assert_eq!(state.native_required_volume_bytes(false), Some(missing));
        assert_eq!(state.native_required_volume_bytes(true), Some(0));

        state.overwrite_policy = NativeOverwritePolicy::Replace;
        let replacement = state.native_required_volume_bytes(true).unwrap();
        assert!(replacement < GIB, "same-sized replacements only need one staged track");
        let (destination_needed, scratch_needed) = state.native_split_volume_bytes(false).unwrap();
        assert!(destination_needed < GIB, "replacement must budget one temporary destination copy");
        assert!(scratch_needed < GIB, "replacement scratch must not accumulate all four tracks");
        state.overwrite_policy = NativeOverwritePolicy::Fail;
        assert!(state.native_required_volume_bytes(true).unwrap() > 2 * GIB);

        // The same native path calculation works for flattened filenames.
        state.program_subfolders = false;
        let job = state.native_job(temp.path().into());
        for track in &job.programs[0].tracks {
            let target = proposed_destination(&job, &job.programs[0], track);
            assert_eq!(target.parent(), Some(state.destination.as_path()));
            std::fs::File::create(&target).unwrap().set_len(635_040_000).unwrap();
        }
        state.overwrite_policy = NativeOverwritePolicy::Skip;
        assert_eq!(state.native_required_volume_bytes(true), Some(0));

        // In continuous mode Skip must bypass the *entire* existing output.
        state.file_per_program = true;
        let job = state.native_job(temp.path().into());
        let continuous = proposed_destination_for_program(&job, &job.programs[0]);
        std::fs::create_dir_all(continuous.parent().unwrap()).unwrap();
        std::fs::File::create(&continuous).unwrap().set_len(2_540_160_000).unwrap();
        assert_eq!(state.native_required_volume_bytes(true), Some(0));
        std::fs::remove_file(continuous).unwrap();
        let continuous_need = state.native_required_volume_bytes(true).unwrap();
        assert!(continuous_need > 4 * GIB && continuous_need < 5 * GIB);
    }

    #[test]
    fn native_space_includes_known_dsf_artwork_and_writer_padding() {
        use crate::disc::model::SacdAreaId;
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("album.iso");
        let cover = temp.path().join("cover.jpg");
        std::fs::write(&cover, vec![0u8; 2 * 1024 * 1024]).unwrap();
        let contents = disc_contents(DiscFormat::Sacd,
            PresentationId::SacdArea(SacdAreaId::Stereo), "DSD Stereo");
        let mut state = DiscBrowserState::new(contents, source);
        state.destination = temp.path().join("out");
        state.tags = true;
        state.tag_artwork = true;
        let (total, largest, program) = state.selected_item_bytes().unwrap();
        let ordinary = state.required_native_volume_bytes(total, largest, program);
        let needed = state.native_required_volume_bytes(true).unwrap();
        let tail = 2 * 1024 * 1024 + crate::dsf_tags::REWRITE_PADDING_BYTES;
        assert_eq!(needed, ordinary + 2 * tail);
        state.file_per_program = true;
        assert_eq!(state.native_required_volume_bytes(true).unwrap(),
            state.required_native_volume_bytes(total, largest, program) + 2 * tail);
    }

    #[test]
    fn continuous_preflight_accounts_for_all_retained_chapters_and_join_spool() {
        let contents = disc_contents(DiscFormat::DvdVideo, PresentationId::dvd_video(1, 1, 0), "English");
        let mut state = DiscBrowserState::new(contents, "dvd".into());
        let template = state.contents.presentations[0].tracks[0].clone();
        state.contents.presentations[0].tracks = (0..20).map(|index| {
            let mut item = template.clone();
            item.number = index + 1;
            // 500 MiB per item at 96 kHz, 24-bit, stereo.
            item.duration_secs = Some((500f64 * 1024f64 * 1024f64) / (96_000f64 * 2f64 * 3f64));
            item
        }).collect();
        state.items_checked[0] = vec![true; 20];
        state.file_per_program = true;
        let (total, largest, program) = state.selected_item_bytes().unwrap();
        assert_eq!(program, total);
        assert!(largest > 490 * 1024 * 1024);
        // A 12 GiB volume cannot hold 10 GiB of parts, 10 GiB of
        // join spool and 10 GiB of completed private container at once.
        assert!(state.required_native_volume_bytes(total, largest, program) > 12 * 1024 * 1024 * 1024);
        assert_eq!(state.native_required_volume_bytes(true),
            Some(state.required_native_volume_bytes(total, largest, program)));
        state.file_per_program = false;
        assert!(state.required_native_volume_bytes(total, largest, program)
            < total.saturating_add(program.saturating_mul(2)));
    }

    #[test]
    fn source_mode_disc_selection_bridge_applies_selected_dvd_video_presentation() {
        let mode = source_mode_for_presentation(
            disc_contents(
                DiscFormat::DvdVideo,
                PresentationId::dvd_video(2, 3, 1),
                "VTS 02 Title 03 Stream 2",
            ),
            0,
            SourceMetadata::default(),
        ).expect("source mode");
        let mut options = blank_source_options();

        assert!(apply_source_mode_disc_selection_to_source_options(&mode, &mut options));

        assert_eq!(options.dvdv_vts, Some(2));
        assert_eq!(options.dvdv_title, Some(3));
        assert_eq!(options.dvdv_audio_stream, Some(1));
        assert_eq!(options.dvdv_angle, Some(1));
    }

    #[test]
    fn source_mode_disc_selection_bridge_applies_selected_bluray_presentation() {
        let id = PresentationId::try_blu_ray_title(12, 0x1100, 0, 2)
            .expect("valid Blu-ray presentation id");
        let mode = source_mode_for_presentation(
            disc_contents(DiscFormat::BluRay, id, "Playlist 00012 Stream 1"),
            0,
            SourceMetadata::default(),
        ).expect("source mode");
        let mut options = blank_source_options();

        assert!(apply_source_mode_disc_selection_to_source_options(&mode, &mut options));

        assert_eq!(options.bluray_playlist, Some(12));
        assert_eq!(options.bluray_audio_pid, Some(0x1100));
        assert_eq!(options.bluray_audio_stream, Some(0));
        assert_eq!(options.bluray_angle, Some(2));
    }
}
