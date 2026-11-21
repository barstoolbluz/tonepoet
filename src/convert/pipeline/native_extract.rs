//! Native disc extraction, separate from the Convert DSP/encoder pipeline.
//!
//! Materializers resolve the authored track/chapter, realizers decode its
//! carrier into a private WAV, and only a *completed* final file is published
//! into the destination. No conversion settings, ReplayGain, resampler, DSP
//! effect, or lossy encoder is involved. All staging stays in TonePoet's cache
//! or the configured scratch directory; cross-volume publication copies through
//! a cleaned-up temporary file at the final target before atomic rename.

mod matroska;
mod cue;
mod archive;
mod options;

/// File-backed stream routes do not invoke optical-disc materializers.
fn is_file_stream_format(format: DiscFormat) -> bool {
    matches!(format, DiscFormat::Matroska | DiscFormat::TransportFile)
}

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use sha2::{Digest, Sha256};
use super::native_wav::{inspect_carrier, NativePcmFormat};
use super::actions::ActionPipeline;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use crate::disc::{DiscFormat, PresentationId};
use super::bluray_realize::realize_bluray_track_native;
use super::dvda_realize::{realize_dvda_track_native, DvdaRealizationAudioPolicy, DvdaSourceAudioExpectation};
use super::dvdv_realize::realize_dvdv_track_native;
use super::materializer_bluray::BlurayMaterializer;
use super::materializer_dvda::DvdaAudioMaterializer;
use super::materializer_dvdv::DvdVideoMaterializer;
use super::materializer_sacd::SacdIsoMaterializer;
use super::stages::Materializer;
use super::tool::{NativeToolPause, RealToolRunner, ToolBinary, ToolCommand, ToolRunner};
use super::types::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NativeWavContainer { Automatic, Riff, Rf64, W64 }

/// The user-facing extraction receipt. This name is shared by the planner,
/// path reservation, initial publication, and detached recovery.
pub const EXTRACTION_LOG_FILE: &str = "extraction.log";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NativeOverwritePolicy { Fail, Skip, Replace }

/// Source-CUE gap ownership. These settings affect only CUE-image splitting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NativeCueBetweenGap { AppendPrevious, PrependNext, Drop }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NativeCueFirstGap { PrependFirst, OwnTrack00, Drop }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NativeCopyFolderOptions {
    pub artwork: bool,
    pub art_folders: bool,
    pub log: bool,
    pub cue: bool,
    pub txt: bool,
    pub nfo: bool,
    pub md: bool,
    pub pdf: bool,
    pub md5: bool,
    pub other: bool,
}

impl Default for NativeCopyFolderOptions {
    fn default() -> Self {
        Self { artwork: true, art_folders: true, log: true, cue: false,
            txt: true, nfo: true, md: true, pdf: true, md5: false, other: false }
    }
}

impl NativeCopyFolderOptions {
    pub fn enabled_count(&self) -> usize {
        [self.artwork, self.art_folders, self.log, self.cue, self.txt,
            self.nfo, self.md, self.pdf, self.md5, self.other]
            .into_iter().filter(|enabled|*enabled).count()
    }
}

/// The extraction Options window and the saved defaults share one schema.
/// Selection-specific applicability is evaluated in the worker, not by
/// changing the user's stored choices.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NativeExtractSettings {
    pub write_log: bool,
    pub verify_after: bool,
    pub write_chapters: bool,
    pub create_cue: bool,
    pub embed_cue: bool,
    pub keep_preemphasis: bool,
    pub between_gap: NativeCueBetweenGap,
    pub first_gap: NativeCueFirstGap,
    pub copy_folder: NativeCopyFolderOptions,
}

impl Default for NativeExtractSettings {
    fn default() -> Self {
        Self { write_log: true, verify_after: false, write_chapters: true,
            create_cue: true, embed_cue: false, keep_preemphasis: true,
            between_gap: NativeCueBetweenGap::AppendPrevious,
            first_gap: NativeCueFirstGap::OwnTrack00,
            copy_folder: NativeCopyFolderOptions::default() }
    }
}


/// Tags collected from actually probed disc/sidecar evidence. A missing
/// field stays missing; generic filename labels never become fabricated tags.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NativeTagValues {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub genre: Option<String>,
    pub year: Option<String>,
    pub composer: Option<String>,
    pub performer: Option<String>,
    #[serde(default)]
    pub catalog: Option<String>,
    #[serde(default)]
    pub publisher: Option<String>,
}

impl NativeTagValues {
    /// Resolve one field at a time. A blank higher-priority value is absent,
    /// not an instruction to erase a value supplied by the other authority.
    pub(crate) fn prioritized(&self, fallback: &Self) -> Self {
        let pick = |primary: &Option<String>, secondary: &Option<String>| {
            primary.as_ref().filter(|value| !value.trim().is_empty()).cloned()
                .or_else(|| secondary.as_ref().filter(|value| !value.trim().is_empty()).cloned())
        };
        Self {
            title: pick(&self.title, &fallback.title),
            artist: pick(&self.artist, &fallback.artist),
            album: pick(&self.album, &fallback.album),
            genre: pick(&self.genre, &fallback.genre),
            year: pick(&self.year, &fallback.year),
            composer: pick(&self.composer, &fallback.composer),
            performer: pick(&self.performer, &fallback.performer),
            catalog: pick(&self.catalog, &fallback.catalog),
            publisher: pick(&self.publisher, &fallback.publisher),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum NativeTagSource { DiscAndSidecar, AuthoredTrack, MusicBrainz }
fn default_tag_source() -> NativeTagSource { NativeTagSource::DiscAndSidecar }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeTrackChoice {
    pub number: u32,
    pub name: String,
    #[serde(default)]
    pub tags: NativeTagValues,
    /// Preflight's uncompressed item-size estimate; never a measurement.
    #[serde(default)]
    pub estimated_bytes: Option<u64>,
    /// Duration from disc metadata, not calculated from estimated bytes.
    #[serde(default)]
    pub duration_secs: Option<f64>,
}

fn default_angle_one() -> u8 { 1 }

/// Authoritative text fields captured independently of the display-oriented
/// disc navigation model. Only a mapper that actually distinguishes disc text
/// from a saved tag file can populate these values.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NativeAuthoredTags {
    pub album: NativeTagValues,
    pub tracks: BTreeMap<u32, NativeTagValues>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeProgramChoice {
    pub id: PresentationId,
    #[serde(default = "default_angle_one")]
    pub angle: u8,
    pub label: String,
    pub tracks: Vec<NativeTrackChoice>,
    pub keep_dst: bool,
    pub source_depth: Option<u32>,
    /// Source codec captured when the extraction job is created. Required for
    /// native Matroska carrier selection; no earlier Phase B jobs exist.
    pub source_codec: String,
    pub source_channels: Option<u8>,
    pub source_rate: Option<u32>,
    /// Source-reported layout, not derived from the channel count.
    #[serde(default)]
    pub source_layout: Option<String>,
    pub is_lossless: bool,
    #[serde(default)]
    pub tags: NativeTagValues,
    /// Optional independent authored evidence for non-SACD tag precedence.
    /// Older recovery records have none and continue to use prepared metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authored_tags: Option<NativeAuthoredTags>,
}

/// Disc identity as reported by the probed source, not inferred from filenames.
/// Optional fields also allow recovery of jobs created by earlier versions.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NativeDiscIdentity {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub catalog: Option<String>,
}

/// An interval on the following physical CUE image assigned to the previous
/// track under Append to previous. Admission binds its immutable geometry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeCueAppendedGap {
    pub image: PathBuf,
    pub end_sample: u64,
    pub total_samples: u64,
    pub source_length: u64,
    pub source_modified: Option<std::time::SystemTime>,
}

/// Exact sample geometry and source carrier for one CUE track. These facts are
/// bound at admission and checked again before encoding and publication.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeCueSegment {
    pub presentation: PresentationId,
    pub number: u32,
    pub cue_path: PathBuf,
    pub image: PathBuf,
    pub start_sample: u64,
    pub end_sample: u64,
    /// Original INDEX 00 and 01 from the admitted CUE, before gap ownership.
    /// INDEX 00 is present only when it addresses this same audio image.
    #[serde(default)]
    pub index00_sample: Option<u64>,
    #[serde(default)]
    pub index01_sample: u64,
    #[serde(default)]
    pub pre_emphasis: bool,
    /// Recovery has already published this program's hidden track 00.
    #[serde(default)]
    pub hidden_track_published: bool,
    /// Additional audio from the start of the next CUE FILE, when the
    /// between-track gap is appended to this selected output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub appended_gap: Option<NativeCueAppendedGap>,
    pub total_samples: u64,
    pub sample_rate: u32,
    pub extension: String,
    pub encoder: String,
    pub muxer: String,
    pub sample_bits: Option<u32>,
    pub channels: u8,
    pub source_length: u64,
    pub source_modified: Option<std::time::SystemTime>,
    pub cue_length: u64,
    pub cue_modified: Option<std::time::SystemTime>,
    /// Source stream bitrate, or a conservative whole-image mean for VBR
    /// carriers lacking stream metadata. None for lossless images.
    #[serde(default)]
    pub source_bit_rate: Option<u64>,
    /// Complete packet partition of this image, including unchecked tracks.
    /// Cut positions use source PCM samples; the segment muxer rounds up to
    /// packet edges. The first part includes any initial CUE pregap.
    #[serde(default)]
    pub packet_cut_samples: Vec<u64>,
    #[serde(default)]
    pub packet_part_index: usize,
}

/// Resolve the two independent CUE gap policies from the full admitted CUE,
/// including unchecked tracks. A selected track never inherits a different
/// sample boundary merely because its neighbor was unchecked. For lossy
/// images, cuts also describe discarded parts, so packet verification still
/// covers every source packet without encoding any of it again.
pub(crate) fn configure_cue_gap_segments(
    all: &[NativeCueSegment], selected: &mut [NativeCueSegment],
    settings: &NativeExtractSettings,
) -> Result<(), String> {
    // Reconfiguration must not leave an old appended interval attached.
    for segment in selected.iter_mut() {segment.appended_gap=None;}
    // Within an admitted CUE sheet, FILE order is the physical album order.
    // A later image's first INDEX 00..01 interval belongs to the preceding
    // track (Append), the following track (Prepend), or nobody (Drop).
    let transitions: Vec<_> = all.windows(2).filter(|pair|
        pair[0].presentation==pair[1].presentation &&
        pair[0].cue_path==pair[1].cue_path && pair[0].image!=pair[1].image &&
        pair[1].index01_sample>0).map(|pair|(&pair[0],&pair[1])).collect();
    let mut images = BTreeSet::new();
    for s in all { images.insert(s.image.clone()); }
    for image in images {
        let mut tracks: Vec<_> = all.iter().filter(|s|s.image==image).collect();
        tracks.sort_by_key(|s|s.index01_sample);
        if tracks.is_empty() {continue;}
        let total=tracks[0].total_samples;
        if tracks.iter().any(|s|s.total_samples!=total || s.index01_sample>=total)
            || tracks.windows(2).any(|pair|pair[0].index01_sample>=pair[1].index01_sample) {
            return Err(format!("Invalid indexed CUE image: {}",image.display()));
        }
        let mut geometry: Vec<(u64,u64)> = tracks.iter().map(|s|(s.index01_sample,s.end_sample)).collect();
        if (tracks[0].number==1 && settings.first_gap==NativeCueFirstGap::PrependFirst)
            || (settings.between_gap==NativeCueBetweenGap::PrependNext &&
                transitions.iter().any(|(_,next)|
                    next.presentation==tracks[0].presentation && next.number==tracks[0].number &&
                    next.image==tracks[0].image)) {
            geometry[0].0=0;
        }
        for index in 1..tracks.len() {
            let previous=tracks[index-1];
            let next=tracks[index];
            let gap_start=next.index00_sample.unwrap_or(next.index01_sample);
            if gap_start<previous.index01_sample || gap_start>next.index01_sample {
                return Err(format!("Invalid INDEX 00 boundary in {}",image.display()));
            }
            if settings.between_gap!=NativeCueBetweenGap::AppendPrevious {
                geometry[index-1].1=gap_start;
            }
            if settings.between_gap==NativeCueBetweenGap::PrependNext {
                geometry[index].0=gap_start;
            }
        }
        if geometry.iter().any(|(start,end)|start>=end || *end>total) {
            return Err(format!("A CUE gap choice makes an empty track in {}",image.display()));
        }
        let mut boundaries=vec![0,total];
        for &(start,end) in &geometry {boundaries.extend([start,end]);}
        boundaries.sort_unstable();boundaries.dedup();
        let cuts=boundaries[1..boundaries.len()-1].to_vec();
        for (position,original) in tracks.iter().enumerate() {
            if let Some(s)=selected.iter_mut().find(|s|s.image==image &&
                s.presentation==original.presentation && s.number==original.number) {
                s.start_sample=geometry[position].0;
                s.end_sample=geometry[position].1;
                if matches!(s.encoder.as_str(),"libmp3lame"|"aac"|"libvorbis"|"libopus") {
                    s.packet_cut_samples=cuts.clone();
                    s.packet_part_index=boundaries.binary_search(&s.start_sample)
                        .map_err(|_|"CUE packet partition missing track boundary")?;
                }
            }
        }
    }
    if settings.between_gap==NativeCueBetweenGap::AppendPrevious {
        for (previous,next) in transitions {
            if let Some(current)=selected.iter_mut().find(|segment|
                segment.presentation==previous.presentation && segment.number==previous.number) {
                // A mixed or compressed packet join cannot promise sample- or
                // packet-identical output via the qualified native splitter.
                // Refuse it before the first destination is published.
                if current.encoder!=next.encoder || current.extension!=next.extension ||
                    current.sample_bits!=next.sample_bits || current.sample_rate!=next.sample_rate ||
                    current.channels!=next.channels ||
                    matches!(current.encoder.as_str(),"libmp3lame"|"aac"|"libvorbis"|"libopus") {
                    return Err(format!("Cannot append cross-image CUE pregap without altering the carrier: {} and {}",
                        current.image.display(),next.image.display()));
                }
                current.appended_gap=Some(NativeCueAppendedGap {
                    image:next.image.clone(), end_sample:next.index01_sample,
                    total_samples:next.total_samples,source_length:next.source_length,
                    source_modified:next.source_modified,
                });
            }
        }
    }
    Ok(())
}

/// One regular member discovered from the unpacked archive, bound to its
/// original relative path and size. It is never decoded or retagged.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeArchiveMember {
    pub number: u32,
    pub relative_path: PathBuf,
    pub size: u64,
    pub tagged_audio: bool,
    /// Number from the member's own tags, separate from selection identity.
    #[serde(default)]
    pub tag_track_number: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeExtractionJob {
    #[serde(default)]
    pub disc_identity: NativeDiscIdentity,
    pub source: PathBuf,
    pub format: DiscFormat,
    pub programs: Vec<NativeProgramChoice>,
    #[serde(default)]
    pub cue_segments: Vec<NativeCueSegment>,
    /// All CUE/image inputs from the admitted view, including unchecked
    /// tracks; Replace must never overwrite an unchecked source image.
    #[serde(default)]
    pub cue_input_paths: Vec<PathBuf>,
    #[serde(default)]
    pub archive_members: Vec<NativeArchiveMember>,
    #[serde(default)]
    pub archive_length: u64,
    #[serde(default)]
    pub archive_modified: Option<std::time::SystemTime>,
    #[serde(default)]
    pub archive_password: Option<String>,
    pub destination: PathBuf,
    pub working_root: PathBuf,
    pub wav_container: NativeWavContainer,
    pub assume_decrypted: bool,
    pub downmix: DvdaDownmixPolicy,
    #[serde(default = "default_naming_template")]
    pub naming_template: String,
    #[serde(default = "default_yes")]
    pub program_subfolders: bool,
    #[serde(default = "default_overwrite_policy")]
    pub overwrite_policy: NativeOverwritePolicy,
    #[serde(default)]
    pub extraction_log: bool,
    #[serde(default)]
    pub options: NativeExtractSettings,
    /// Round-4 log destinations are chosen once, before queueing, and stored
    /// in the durable job. An empty list preserves pre-R34 journal semantics.
    #[serde(default)]
    pub program_log_paths: Vec<PathBuf>,
    /// Older flattened jobs prefix filenames even when only one area was
    /// selected. Keep that recorded path contract across recovery upgrades.
    #[serde(default)]
    pub round4_layout: bool,
    #[serde(default)]
    pub file_per_program: bool,
    #[serde(default)]
    pub cue: bool,
    #[serde(default)]
    pub transfer_tags: bool,
    /// Plain DSDIFF/DSD output for SACD; DST passthrough still uses the
    /// per-area keep_dst flag. Missing in older recovery journals means DSF.
    #[serde(default)]
    pub sacd_dff: bool,
    #[serde(default = "default_tag_source")]
    pub tag_source: NativeTagSource,
    #[serde(default = "default_tag_fields")]
    pub tag_fields: u16,
    #[serde(default)]
    pub tag_artwork: bool,
}
fn default_tag_fields() -> u16 { 0x7f }
fn default_naming_template() -> String { "%TRACKNUMBER% - %TITLE%".into() }
fn default_yes() -> bool { true }
fn default_overwrite_policy() -> NativeOverwritePolicy { NativeOverwritePolicy::Fail }

#[derive(Debug, Clone)]
pub struct NativeExtractionProgress {
    pub done: usize,
    pub total: usize,
    pub published: usize,
    /// Completed filenames, reported only after atomic publication. The
    /// detached file-task journal records newly published files once each.
    pub published_paths: Vec<PathBuf>,
    pub skipped: usize,
    pub current: String,
    /// Private file currently being written. The file-task host samples its
    /// size periodically; this path is never displayed as an output path.
    pub current_file: Option<PathBuf>,
    pub current_estimated_bytes: Option<u64>,
    pub estimated_bytes_done: u64,
    /// Estimated bytes of fully published audio; continuous-mode staged tracks
    /// are not counted until their assembled program has been published.
    pub estimated_bytes_published: u64,
    pub estimated_bytes_skipped: u64,
    pub estimated_bytes_total: Option<u64>,
}

/// A confirmed user/policy skip is a disposition of a selected track (or of
/// an entire continuous program), not a publication of an output pathname.
/// The authored presentation identity stays stable when Recovery removes
/// completed selections from the work left for a new helper generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NativeSkipReason { ExistingFile, RequestedByUser, NoAudioInChapter }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeSkippedSelection {
    pub presentation: PresentationId,
    pub angle: u8,
    /// None identifies a whole continuous program, including its final join.
    pub track_number: Option<u32>,
    /// Older durable receipts did not record why the selection was skipped.
    #[serde(default)]
    pub reason: Option<NativeSkipReason>,
}

/// Exact output whose processing was interrupted. A later recovery may
/// complete it; the log projection gives a completed publication precedence
/// over an earlier interruption for the same path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeUnfinishedOutput {
    pub destination: PathBuf,
    pub reason: String,
    pub cancelled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NativeLogOutcome { Complete, Cancelled, Failed }

impl NativeSkippedSelection {
    pub fn track(program: &NativeProgramChoice, number: u32) -> Self {
        Self { presentation: program.id, angle: program.angle, track_number: Some(number), reason: None }
    }

    pub fn program(program: &NativeProgramChoice) -> Self {
        Self { presentation: program.id, angle: program.angle, track_number: None, reason: None }
    }

    pub fn with_reason(mut self, reason: NativeSkipReason) -> Self {
        self.reason = Some(reason);
        self
    }

    pub fn same_selection(&self, other: &Self) -> bool {
        self.presentation == other.presentation && self.angle == other.angle
            && self.track_number == other.track_number
    }
}

/// One private file or a bounded private staging directory observed by the
/// detached progress sampler. Its path is *never* a UI source/destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeWatchSource { File(PathBuf), Tree(PathBuf) }

#[derive(Debug, Clone)]
pub struct NativeWriteWatch {
    pub label: String,
    pub source: NativeWatchSource,
    pub baseline: u64,
    pub estimated_bytes: Option<u64>,
    pub finalizing: bool,
    /// Portion of the estimated final audio bytes represented by this phase.
    pub fraction_start: u8,
    pub fraction_end: u8,
}

impl NativeWriteWatch {
    pub fn measured_bytes(&self) -> u64 {
        let measured = match &self.source {
            NativeWatchSource::File(path) => fs::metadata(path).ok().map_or(0, |meta|meta.len()),
            NativeWatchSource::Tree(path) => sum_private_staging_bytes(path),
        };
        measured.saturating_sub(self.baseline)
    }
}

/// Count only regular private staging files. Never follow symlinks or leave
/// the specific temporary tree. A bounded scan on the periodic UI sampler is
/// less invasive than adding per-frame progress events to three realizers.
fn sum_private_staging_bytes(root: &Path) -> u64 {
    const MAX_ENTRIES: usize = 4096;
    let mut stack = vec![root.to_path_buf()];
    let mut total = 0u64;
    let mut visited = 0;
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else { continue; };
        for entry in entries.flatten() {
            visited += 1;
            if visited > MAX_ENTRIES { return total; }
            let Ok(meta) = entry.metadata() else { continue; };
            // Do not count external symlinks mounted beneath temporary paths.
            if let Ok(plain) = fs::symlink_metadata(entry.path()) {
                if plain.file_type().is_symlink() { continue; }
            }
            if meta.is_dir() { stack.push(entry.path()); }
            else if meta.is_file() { total = total.saturating_add(meta.len()); }
        }
    }
    total
}

#[derive(Clone)]
pub struct NativePublicationHook {
    pub job_id: String,
    pub generation: u64,
    pub notify: Arc<dyn Fn(&Path, &Path, bool) -> Result<(), String> + Send + Sync>,
    /// Both callbacks append to the existing file-task journal synchronously.
    /// They run before the worker acknowledges a skip or publishes CUE audio.
    pub skipped: Arc<dyn Fn(&NativeSkippedSelection) -> Result<(), String> + Send + Sync>,
    pub prepared_cue: Arc<dyn Fn(&Path, &str) -> Result<(), String> + Send + Sync>,
    pub unfinished: Arc<dyn Fn(&NativeUnfinishedOutput) -> Result<(), String> + Send + Sync>,
}

impl std::fmt::Debug for NativePublicationHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativePublicationHook")
            .field("job_id", &self.job_id).field("generation", &self.generation).finish()
    }
}

/// Independent, cooperative file-task controls. Per-item cancellation is a
/// child of the job token; Skip never cancels the whole extraction. An Abort
/// always cancels both. Native loops and supervised tools acknowledge pause.
/// Reference taken from the live producer's audio sample stream, not by
/// reopening a completed private container. `decoder` is FFmpeg's canonical
/// raw PCM representation used for both the producer tap and readback.
#[derive(Debug, Clone)]
enum NativeFingerprintFormat {
    // Encoded containers may change tags, so compare decoded raw samples.
    Pcm(String),
    // Identity-copy sources have no transformation or tag writes.
    IdentityCopy,
    // Compare compressed audio packets, independent of container metadata.
    Packets,
    // Canonical, channel-interleaved MSB-first DSD, including decoded DST.
    DsdFrames,
}

#[derive(Debug, Clone)]
struct NativeAudioFingerprint {
    digest: [u8; 32],
    format: NativeFingerprintFormat,
}

#[derive(Debug, Default)]
pub struct NativeExtractionControl {
    tool_pause: Arc<NativeToolPause>,
    skip_requested: AtomicBool,
    active_item: Mutex<Option<CancellationToken>>,
    watch: Mutex<Option<NativeWriteWatch>>,
    publication: Mutex<Option<NativePublicationHook>>,
    recorded_skips: Mutex<Vec<NativeSkippedSelection>>,
    attempted_output: Mutex<Option<PathBuf>>,
    unfinished_outputs: Mutex<Vec<NativeUnfinishedOutput>>,
    audio_fingerprints: Mutex<HashMap<PathBuf,NativeAudioFingerprint>>,
    execution_started: AtomicBool,
}

impl NativeExtractionControl {
    fn record_audio_fingerprint(&self, path: &Path, digest: [u8;32], decoder: &str) {
        self.record_fingerprint(path, digest, NativeFingerprintFormat::Pcm(decoder.into()));
    }
    fn record_identity_fingerprint(&self, path: &Path, digest: [u8;32]) {
        self.record_fingerprint(path, digest, NativeFingerprintFormat::IdentityCopy);
    }
    fn record_packet_fingerprint(&self, path: &Path, digest: [u8;32]) {
        self.record_fingerprint(path, digest, NativeFingerprintFormat::Packets);
    }
    fn record_dsd_fingerprint(&self, path: &Path, digest: [u8;32]) {
        self.record_fingerprint(path, digest, NativeFingerprintFormat::DsdFrames);
    }
    fn record_fingerprint(&self, path: &Path, digest: [u8;32], format: NativeFingerprintFormat) {
        self.audio_fingerprints.lock().unwrap_or_else(|e|e.into_inner()).insert(
            path.to_path_buf(),NativeAudioFingerprint {digest,format});
    }
    fn take_audio_fingerprint(&self, path: &Path) -> Option<NativeAudioFingerprint> {
        self.audio_fingerprints.lock().unwrap_or_else(|e|e.into_inner()).remove(path)
    }
    pub fn pause(&self) { self.tool_pause.request(true); }
    pub fn resume(&self) { self.tool_pause.request(false); }
    pub fn is_paused(&self) -> bool { self.tool_pause.requested() }
    pub fn is_pause_acknowledged(&self) -> bool { self.tool_pause.acknowledged() }
    pub(crate) fn tool_pause(&self) -> Arc<NativeToolPause> { Arc::clone(&self.tool_pause) }
    pub fn set_publication_hook(&self, hook: NativePublicationHook) {
        *self.publication.lock().unwrap_or_else(|e|e.into_inner()) = Some(hook);
    }
    fn publication_hook(&self) -> Option<NativePublicationHook> {
        self.publication.lock().unwrap_or_else(|e|e.into_inner()).clone()
    }
    fn record_skipped(&self, selection: NativeSkippedSelection) -> Result<(), String> {
        if let Some(hook) = self.publication_hook() { (hook.skipped)(&selection)?; }
        self.recorded_skips.lock().unwrap_or_else(|e|e.into_inner()).push(selection);
        Ok(())
    }
    pub(crate) fn skipped_receipts(&self) -> Vec<NativeSkippedSelection> {
        self.recorded_skips.lock().unwrap_or_else(|e|e.into_inner()).clone()
    }
    fn set_attempted_output(&self, destination: PathBuf) {
        *self.attempted_output.lock().unwrap_or_else(|e| e.into_inner()) = Some(destination);
    }
    fn clear_attempted_output(&self) {
        *self.attempted_output.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
    fn record_interrupted_output(&self, reason: &str, cancelled: bool) -> Result<(), String> {
        let Some(destination) = self.attempted_output.lock()
            .unwrap_or_else(|e| e.into_inner()).take() else { return Ok(()); };
        let receipt = NativeUnfinishedOutput { destination, reason: reason.to_owned(), cancelled };
        if let Some(hook) = self.publication_hook() { (hook.unfinished)(&receipt)?; }
        self.unfinished_outputs.lock().unwrap_or_else(|e| e.into_inner()).push(receipt);
        Ok(())
    }
    pub(crate) fn unfinished_receipts(&self) -> Vec<NativeUnfinishedOutput> {
        self.unfinished_outputs.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    pub(crate) fn execution_has_started(&self) -> bool {
        self.execution_started.load(Ordering::Acquire)
    }
    fn record_prepared_cue(&self, destination: &Path, content: &str) -> Result<(), String> {
        if let Some(hook) = self.publication_hook() { (hook.prepared_cue)(destination, content)?; }
        Ok(())
    }
    pub fn watch_snapshot(&self) -> Option<NativeWriteWatch> {
        self.watch.lock().unwrap_or_else(|e|e.into_inner()).clone()
    }
    fn watch_file(&self, path: PathBuf, label: String, estimated_bytes: Option<u64>, finalizing: bool) {
        let (start, end) = if finalizing { (85, 100) } else { (0, 85) };
        self.watch_file_range(path, label, estimated_bytes, start, end);
    }
    fn watch_file_range(&self, path: PathBuf, label: String,
        estimated_bytes: Option<u64>, start: u8, end: u8,
    ) {
        *self.watch.lock().unwrap_or_else(|e|e.into_inner()) = Some(NativeWriteWatch {
            label, source: NativeWatchSource::File(path), baseline: 0, estimated_bytes,
            finalizing: end == 100 || start >= 75,
            fraction_start: start, fraction_end: end,
        });
    }
    fn watch_tree(&self, path: PathBuf, label: String, estimated_bytes: Option<u64>) {
        self.watch_tree_range(path, label, estimated_bytes, 0, 30);
    }
    fn watch_tree_range(&self, path: PathBuf, label: String,
        estimated_bytes: Option<u64>, start: u8, end: u8,
    ) {
        let baseline = sum_private_staging_bytes(&path);
        *self.watch.lock().unwrap_or_else(|e|e.into_inner()) = Some(NativeWriteWatch {
            label, source: NativeWatchSource::Tree(path), baseline, estimated_bytes,
            finalizing: end == 100 || start >= 75,
            fraction_start: start, fraction_end: end,
        });
    }
    fn clear_watch(&self) { *self.watch.lock().unwrap_or_else(|e|e.into_inner()) = None; }
    pub fn skip_current(&self) {
        let active = self.active_item.lock().unwrap_or_else(|e| e.into_inner());
        // Skip-current never arms the *next* item when no item is active.
        if let Some(token) = active.as_ref() {
            self.skip_requested.store(true, Ordering::Release);
            token.cancel();
        }
    }
    fn begin_item(&self, cancel: &CancellationToken) -> CancellationToken {
        let mut active = self.active_item.lock().unwrap_or_else(|e| e.into_inner());
        let token = cancel.child_token();
        if self.skip_requested.load(Ordering::Acquire) { token.cancel(); }
        *active = Some(token.clone());
        token
    }
    fn finish_item(&self) -> bool {
        let mut active = self.active_item.lock().unwrap_or_else(|e| e.into_inner());
        *active = None;
        self.skip_requested.swap(false, Ordering::AcqRel)
    }
    async fn wait_ready(&self, job: &CancellationToken, item: &CancellationToken) -> Result<(), String> {
        loop {
            if job.is_cancelled() { self.tool_pause.acknowledge(false); return Err("Extraction cancelled".into()); }
            if item.is_cancelled() { self.tool_pause.acknowledge(false); return Err("Current extraction item skipped".into()); }
            if !self.is_paused() { self.tool_pause.acknowledge(false); return Ok(()); }
            self.tool_pause.acknowledge(true);
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    }
    fn wait_blocking(&self, job: &CancellationToken, item: &CancellationToken) -> bool {
        while self.is_paused() && !job.is_cancelled() && !item.is_cancelled() {
            self.tool_pause.acknowledge(true);
            std::thread::sleep(Duration::from_millis(35));
        }
        self.tool_pause.acknowledge(false);
        job.is_cancelled() || item.is_cancelled()
    }
}


#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeExtractionReport {
    pub published: Vec<PathBuf>,
    pub skipped: usize,
}

/// Facts observed by the pre-extraction materializer. A successful probe must
/// not discard the format information needed by the size estimator and writer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NativeProbeFacts {
    pub bit_depth: Option<u32>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u8>,
}

/// Make one private temporary directory per extraction.  `TempDir` owns and
/// removes the entire hierarchy on ordinary completion, error and cancellation.
/// An OS crash can leave only files in working storage, never partial outputs.
#[cfg_attr(not(test), allow(dead_code))] // Compatibility entrypoint exercised by focused tests.
pub async fn execute(
    job: NativeExtractionJob,
    cancel: CancellationToken,
    on_progress: impl FnMut(NativeExtractionProgress),
) -> Result<NativeExtractionReport, String> {
    execute_with_controls(job, cancel, Arc::new(NativeExtractionControl::default()), on_progress).await
}

/// File-task entrypoint. It shares the existing materializers and native
/// publisher but does not submit to, observe, or cancel the conversion queue.
pub async fn execute_with_controls(
    job: NativeExtractionJob,
    cancel: CancellationToken,
    controls: Arc<NativeExtractionControl>,
    mut on_progress: impl FnMut(NativeExtractionProgress),
) -> Result<NativeExtractionReport, String> {
    let mut published_paths = Vec::new();
    let original_job = job.clone();
    let execution_controls = Arc::clone(&controls);
    let execution_cancel = cancel.clone();
    let result = execute_inner(job, cancel, controls, |progress| {
        published_paths = progress.published_paths.clone();
        on_progress(progress);
    }).await;
    result.map_err(|error| {
        let mut error = error;
        if execution_controls.execution_started.load(Ordering::Acquire) {
            let cancelled = execution_cancel.is_cancelled();
            if let Err(journal_error) = execution_controls.record_interrupted_output(&error, cancelled) {
                error.push_str(&format!("; cannot record interrupted output: {journal_error}"));
            }
            // The detached helper handles its own terminal log, using the
            // durable receipts across generations. This branch serves direct
            // callers that requested a log on the media job itself.
            if original_job.extraction_log && execution_controls.publication_hook().is_none() {
                for (index, target) in native_log_targets(&original_job).into_iter().enumerate() {
                    let content = native_log_content_for_index(&original_job,index,
                        &published_paths, &execution_controls.skipped_receipts(),
                        &execution_controls.unfinished_receipts(),
                        if cancelled { NativeLogOutcome::Cancelled } else { NativeLogOutcome::Failed },
                        Some(&error));
                    // Cancellation cannot suppress a requested terminal receipt.
                    if let Err(log_error) = publish_native_recovery_sidecar(&original_job, &target,
                        &content, &CancellationToken::new(), &NativeExtractionControl::default()) {
                        error.push_str(&format!("; extraction log could not be written: {log_error}"));
                    }
                }
            }
        }
        if published_paths.is_empty() { error } else {
            format!("{error}; {} completed file(s) remain published under '{}'",
                published_paths.len(), original_job.destination.display())
        }
    })
}

/// Perform bounded media preflight for the extraction overlay. DVD-Video's
/// existing materializer scans up to 512 authored sectors per selected chapter
/// for positive MPEG PES scrambling evidence. For a fast preview, inspect one
/// chapter per title rather than reading every chapter (the extraction worker
/// still checks each selected chapter independently before publication).
/// Unresolved lossless source depth is a blocker, never a guessed 24-bit tag.
pub async fn probe_selected_sources(
    job: NativeExtractionJob, cancel: CancellationToken,
) -> Result<NativeProbeFacts, String> {
    if job.format == DiscFormat::CueImage {
        return cue::probe_selected_sources(&job, &cancel).await;
    }
    if job.format == DiscFormat::Archive {
        return archive::probe_selected_sources(&job, &cancel).await;
    }
    if is_file_stream_format(job.format) {
        return matroska::probe_selected_sources(&job, &cancel).await;
    }
    let temporary = tempfile::tempdir()
        .map_err(|e| format!("Create private probe storage: {e}"))?;
    let staging = StagingDir::borrowed(temporary.path().to_path_buf(), "native-extract-probe".into());
    let runner = RealToolRunner::new(HashMap::new());
    let tools = HashMap::new();
    let mut seen_dvd_titles = HashSet::new();
    let mut observed = NativeProbeFacts::default();
    for (index, program) in job.programs.iter().enumerate() {
        if cancel.is_cancelled() { return Err("Native source probe cancelled".into()); }
        if job.format == DiscFormat::DvdVideo {
            if let PresentationId::DvdVideoTitle { vts_number, title_number, .. } = program.id {
                if !seen_dvd_titles.insert((vts_number, title_number)) { continue; }
            }
        }
        let mut request = materializer_request(&job, program, index)?;
        let number = program.tracks.first().ok_or("Program has no probeable chapter")?.number;
        request.source.track_selection = TrackSelection::Set(BTreeSet::from([number]));
        let materializer: Box<dyn Materializer> = match job.format {
            DiscFormat::Sacd => Box::new(SacdIsoMaterializer),
            DiscFormat::DvdAudio => Box::new(DvdaAudioMaterializer),
            DiscFormat::DvdVideo => Box::new(DvdVideoMaterializer),
            DiscFormat::BluRay => Box::new(BlurayMaterializer),
            DiscFormat::Matroska | DiscFormat::TransportFile | DiscFormat::CueImage | DiscFormat::Archive => unreachable!("File audio streams use the dedicated native extractor"),
        };
        let prepared = materializer.materialize(
            &request, &staging, &runner, None, &tools, &cancel
        ).await.map_err(|e| format!("{}: {e}",program.label))?;
        let first = prepared.tracks.first().ok_or_else(||format!("{}: no probed audio",program.label))?;
        let depth = first.bit_depth.or(first.source_audio.bit_depth).or(program.source_depth);
        if job.format != DiscFormat::Sacd && program.is_lossless && depth.is_none() {
            return Err(format!("{}: source depth remains unknown after media probe",program.label));
        }
        let channels = match &first.source_ref {
            TrackSourceRef::DvdVideoTrack { channels, .. } | TrackSourceRef::BluRayTrack { channels, .. } => *channels,
            _ => None,
        }.or(program.source_channels);
        observed = NativeProbeFacts {
            bit_depth: depth,
            sample_rate: first.scalar_sample_rate().or(program.source_rate),
            channels,
        };
    }
    Ok(observed)
}

async fn execute_inner(
    job: NativeExtractionJob,
    cancel: CancellationToken,
    controls: Arc<NativeExtractionControl>,
    mut on_progress: impl FnMut(NativeExtractionProgress),
) -> Result<NativeExtractionReport, String> {
    if job.format == DiscFormat::CueImage {
        return cue::execute_inner(job, cancel, controls, on_progress).await;
    }
    if job.format == DiscFormat::Archive {
        return archive::execute_inner(job, cancel, controls, on_progress).await;
    }
    if is_file_stream_format(job.format) {
        return matroska::execute_inner(job, cancel, controls, on_progress).await;
    }
    validate_job(&job)?;
    controls.execution_started.store(true, Ordering::Release);
    // Reject overlapping scratch before creating anything in working storage.
    // Older persisted jobs may contain the R5 implicit destination-sibling
    // staging path. Move that obsolete default to TonePoet's cache too.
    let work_path = native_working_storage(&job.working_root, &job.destination)?;
    fs::create_dir_all(&work_path).map_err(|e| format!("Working storage: {e}"))?;
    let temporary = tempfile::Builder::new()
        .prefix("tonepoet-extract-")
        .tempdir_in(&work_path)
        .map_err(|e| format!("Cannot allocate private working storage: {e}"))?;
    let staging = StagingDir::borrowed(temporary.path().to_path_buf(), "native-extract".to_string());
    let runner = RealToolRunner::new(HashMap::new()).with_native_pause(controls.tool_pause());
    let tool_paths = HashMap::new();
    let total: usize = job.programs.iter().map(|p| p.tracks.len()).sum();
    let mut done = 0;
    let mut skipped = 0usize;
    let mut estimated_bytes_done = 0u64;
    let mut estimated_bytes_published = 0u64;
    let mut estimated_bytes_skipped = 0u64;
    let estimated_bytes_total = job.programs.iter().flat_map(|p| &p.tracks)
        .try_fold(0u64, |sum, t| sum.checked_add(t.estimated_bytes?));
    let mut published = Vec::new();
    let mut reserved_names = HashSet::new();
    // One snapshot shape for all phases, including skips and continuous mode.
    let mut emit = |done: usize, published: usize, skipped: usize, current: String,
        current_file: Option<PathBuf>, current_estimated_bytes: Option<u64>,
        estimated_bytes_done: u64, estimated_bytes_skipped: u64,
        estimated_bytes_published: u64, published_paths: Vec<PathBuf>| {
        on_progress(NativeExtractionProgress {
            done, total, published, published_paths, skipped, current, current_file,
            current_estimated_bytes, estimated_bytes_done, estimated_bytes_published,
            estimated_bytes_skipped, estimated_bytes_total,
        });
    };
    emit(0, 0, 0, "Preparing selected disc streams".to_owned(), None, None, 0, 0, 0, Vec::new());

    for (pi, program) in job.programs.iter().enumerate() {
        if cancel.is_cancelled() { return Err("Extraction cancelled".into()); }
        let grouped_target = proposed_destination_for_program(&job, program);
        if job.file_per_program && job.overwrite_policy == NativeOverwritePolicy::Skip
            && fs::symlink_metadata(&grouped_target).is_ok() {
            controls.record_skipped(NativeSkippedSelection::program(program)
                .with_reason(NativeSkipReason::ExistingFile))?;
            done += program.tracks.len();
            skipped += program.tracks.len();
            let skipped_bytes = program.tracks.iter().filter_map(|t| t.estimated_bytes)
                .fold(0u64, u64::saturating_add);
            estimated_bytes_done = estimated_bytes_done.saturating_add(skipped_bytes);
            estimated_bytes_skipped = estimated_bytes_skipped.saturating_add(skipped_bytes);
            emit(done, published.len(), skipped, format!("Skipped existing {}",program.label), None, None, estimated_bytes_done, estimated_bytes_skipped, estimated_bytes_published, published.clone());
            continue;
        }
        let mut grouped_outputs: Vec<(u32, PathBuf, String)> = Vec::new();
        // Preparation may examine the entire program, so an error there
        // cannot be attributed to an unattempted first output. The exact
        // destination becomes known when we start processing each track.
        let mut req = materializer_request(&job, program, pi)?;
        let selection: BTreeSet<u32> = program.tracks.iter().map(|track| track.number).collect();
        req.source.track_selection = TrackSelection::Set(selection);
        let materializer: Box<dyn Materializer> = match job.format {
            DiscFormat::Sacd => Box::new(SacdIsoMaterializer),
            DiscFormat::DvdAudio => Box::new(DvdaAudioMaterializer),
            DiscFormat::DvdVideo => Box::new(DvdVideoMaterializer),
            DiscFormat::BluRay => Box::new(BlurayMaterializer),
            DiscFormat::Matroska | DiscFormat::TransportFile | DiscFormat::CueImage | DiscFormat::Archive => unreachable!("File audio streams use the dedicated native extractor"),
        };
        controls.wait_ready(&cancel, &cancel).await?;
        let program_estimate = program.tracks.iter()
            .try_fold(0u64, |sum, track| sum.checked_add(track.estimated_bytes?));
        controls.watch_tree(temporary.path().to_path_buf(),
            format!("Preparing {}", program.label), program_estimate);
        let prepared = materializer.materialize(
            &req, &staging, &runner, None, &tool_paths, &cancel,
        ).await.map_err(|e| format!("{}: {e}", program.label))?;
        controls.clear_watch();
        if prepared.tracks.len() != program.tracks.len() {
            return Err(format!("{}: selected track inventory changed; refusing partial extraction", program.label));
        }
        for track in &prepared.tracks {
            if cancel.is_cancelled() { return Err("Extraction cancelled".into()); }
            let requested = program.tracks.iter().find(|chosen| chosen.number == track.id.source_ordinal)
                .ok_or_else(|| format!("{}: materializer returned unexpected track {}", program.label, track.id.source_ordinal))?;
            let item_cancel = controls.begin_item(&cancel);
            let extension = extraction_extension(&job, program);
            let dest = proposed_destination(&job, program, requested);
            controls.set_attempted_output(if job.file_per_program { grouped_target.clone() } else { dest.clone() });
            if !job.file_per_program {
                if !reserved_names.insert(native_destination_collision_key(&dest)) {
                    return Err(format!("Two selected items would publish to '{}'; choose different names", dest.display()));
                }
                if job.overwrite_policy == NativeOverwritePolicy::Skip && fs::symlink_metadata(&dest).is_ok() {
                    controls.record_skipped(NativeSkippedSelection::track(program, requested.number)
                        .with_reason(NativeSkipReason::ExistingFile))?;
                    done += 1;
                    skipped += 1;
                    estimated_bytes_done = estimated_bytes_done.saturating_add(requested.estimated_bytes.unwrap_or(0));
                    estimated_bytes_skipped = estimated_bytes_skipped.saturating_add(requested.estimated_bytes.unwrap_or(0));
                    controls.finish_item();
                    controls.clear_attempted_output();
                    emit(done, published.len(), skipped, format!("Skipped existing {}", requested.name), None, None, estimated_bytes_done, estimated_bytes_skipped, estimated_bytes_published, published.clone());
                    continue;
                }
                if job.overwrite_policy == NativeOverwritePolicy::Fail && fs::symlink_metadata(&dest).is_ok() {
                    return Err(format!("'{}' already exists; extraction never overwrites", dest.display()));
                }
            }

            let complete = temporary.path().join(format!("completed-{pi}-{}.{extension}", track.id.source_ordinal));
            controls.watch_file(complete.clone(), format!("Writing {}", requested.name),
                requested.estimated_bytes, false);
            let watched = complete.clone();
            emit(done, published.len(), skipped, format!("{} · {}", program.label, requested.name),
                Some(watched), requested.estimated_bytes, estimated_bytes_done, estimated_bytes_skipped, estimated_bytes_published, published.clone());
            let item_result: Result<(), String> = async {
            controls.wait_ready(&cancel, &item_cancel).await?;
            if job.format == DiscFormat::Sacd {
                let src = track.source_ref.clone();
                let dst = complete.clone();
                let cancel_copy = cancel.clone();
                let item_copy = item_cancel.clone();
                let controls_copy = controls.clone();
                let keep_dst = program.keep_dst;
                let dff = job.sacd_dff || keep_dst;
                let job_verify = job.options.verify_after;
                tokio::task::spawn_blocking(move || extract_sacd_native(&src, &dst, dff, keep_dst,
                    job_verify, &cancel_copy, &item_copy, &controls_copy))
                    .await.map_err(|e| format!("SACD extraction task: {e}"))??;
            } else {
                if job.format == DiscFormat::DvdAudio {
                    let rates: BTreeSet<u32> = track.source_audio.channel_groups.iter()
                        .filter_map(|g| g.sample_rate).collect();
                    if rates.len() > 1 ||
                        (track.source_audio.channel_groups.len() > 1 &&
                         track.source_audio.channel_groups.iter().any(|g| g.sample_rate.is_none())) {
                        return Err(format!("{}: channel-group sample rates are mixed or unknown; use Convert", requested.name));
                    }
                }
                controls.watch_tree(temporary.path().to_path_buf(),
                    format!("Realizing {}", requested.name), requested.estimated_bytes);
                let carrier = match &track.source_ref {
                    TrackSourceRef::DvdaTrack { .. } => {
                        let expectation = DvdaSourceAudioExpectation::from_prepared_track_and_source(Some(track), &track.source_ref);
                        let policy = DvdaRealizationAudioPolicy::new("native".to_string(), None, job.downmix).for_native_extraction();
                        realize_dvda_track_native(&track.source_ref, expectation, policy, &staging, &runner, &item_cancel, None, None, controls.tool_pause()).await
                    }
                    TrackSourceRef::DvdVideoTrack { .. } => {
                        realize_dvdv_track_native(&track.source_ref, &staging, &runner, &item_cancel, None, None, controls.tool_pause()).await
                    }
                    TrackSourceRef::BluRayTrack { .. } => {
                        realize_bluray_track_native(&track.source_ref, &staging, &runner, &item_cancel, None, None, controls.tool_pause()).await
                    }
                    _ => return Err("Unexpected prepared track kind".into()),
                }.map_err(|e| format!("{}: {e}", requested.name))?;
                let depth = if program.is_lossless {
                    track.bit_depth.or(track.source_audio.bit_depth).or(program.source_depth).ok_or_else(||
                        format!("{}: native bit depth is unknown; refusing an unverified output", requested.name)
                    )?
                } else { 24 };
                let output_depth = match depth { 16 => 16, 20 | 24 => 24, _ => return Err(format!("Unsupported native bit depth {depth}")) };
                encode_pcm_container(&runner, &carrier, &complete, output_depth, depth, job.wav_container,
                    track.scalar_sample_rate().or(program.source_rate),
                    if job.downmix.is_active() { None } else { program.source_channels },
                    if job.downmix.is_active() { None } else { program.source_layout.as_deref() },
                    &item_cancel, &cancel, Arc::clone(&controls), requested.estimated_bytes,
                    job.options.verify_after).await?;
                // This carrier was made in private staging, never in source.
                // Reclaim it before decoding the next selected item.
                let _ = fs::remove_file(&carrier);
            }
            controls.wait_ready(&cancel, &item_cancel).await?;
            if job.transfer_tags && !job.file_per_program && !(job.format == DiscFormat::Sacd && (job.sacd_dff || program.keep_dst)) {
                let entries = extraction_tags(&job, program, requested, track, &prepared.album_metadata);
                let art = if job.tag_artwork { native_artwork_path(&prepared.album_metadata, &job.source) } else { None };
                if !entries.is_empty() || art.is_some() {
                    let complete_copy = complete.clone();
                    let cancel_copy = item_cancel.clone();
                    tokio::task::spawn_blocking(move || {
                        if cancel_copy.is_cancelled() { return Err("Extraction cancelled before metadata".to_string()); }
                        write_completed_native_metadata(&complete_copy, &entries, art.as_deref())?;
                        if cancel_copy.is_cancelled() { return Err("Extraction cancelled after metadata".to_string()); }
                        Ok::<(),String>(())
                    }).await.map_err(|e|format!("Metadata worker: {e}"))??;
                }
            }
            controls.wait_ready(&cancel, &item_cancel).await?;
            if fs::metadata(&complete).map_err(|e| format!("Completed file not found: {e}"))?.len() < 64 {
                return Err(format!("{}: extraction produced an empty or truncated output", requested.name));
            }
            File::open(&complete).and_then(|f| f.sync_all())
                .map_err(|e| format!("Cannot synchronize completed file: {e}"))?;
            if job.file_per_program {
                grouped_outputs.push((requested.number, complete.clone(), requested.name.clone()));
            } else {
                // Journal sidecar contents before audio publication; after
                // a crash Recovery can finish the companion without replaying
                // extraction or replacing already completed DFF audio.
                let dff_cue=if job.format==DiscFormat::Sacd &&
                    native_program_cue_applies(&job,program) && !job.file_per_program &&
                    dest.extension().and_then(|ext|ext.to_str())==Some("dff") {
                    let file_name=dest.file_name().and_then(|s|s.to_str())
                        .ok_or("Invalid DFF output file name")?;
                    let rate=track.scalar_sample_rate().or(program.source_rate)
                        .ok_or("DFF needs a known source DSD sample rate for CUE")?;
                    let text=options::cue_text(file_name,
                        &[(requested.name.clone(),0,false)],rate)?;
                    let cue_path=options::stage_text(temporary.path(),
                        &format!("track-{pi}-{}.cue",requested.number),&text)?;
                    let cue_target=dest.with_extension("cue");
                    controls.record_prepared_cue(&cue_target,&text)?;
                    Some((cue_path,cue_target))
                } else {None};
                publish_finished_file_controlled(&job, &complete, &dest, &cancel, &item_cancel, &controls)?;
                published.push(dest.clone());
                if let Some((cue_path,cue_target))=dff_cue {
                    controls.set_attempted_output(cue_target.clone());
                    publish_finished_file_controlled(&job,&cue_path,&cue_target,
                        &cancel,&item_cancel,&controls)?;
                    published.push(cue_target);
                }
                estimated_bytes_published = estimated_bytes_published
                    .saturating_add(requested.estimated_bytes.unwrap_or(0));
                // Persist the destination before any next-track work. This
                // closes the successful-publication / abrupt-exit window.
                emit(done, published.len(), skipped, format!("Published {}", requested.name),
                    None, None, estimated_bytes_done, estimated_bytes_skipped, estimated_bytes_published, published.clone());
            }
            Ok(())
            }.await;
            controls.clear_watch();
            let item_was_skipped = controls.finish_item();
            if cancel.is_cancelled() { return Err("Extraction cancelled".into()); }
            if item_was_skipped && item_result.is_err() {
                let _ = fs::remove_file(&complete);
                let _ = fs::remove_file(complete.with_extension("raw-pcm-in-working-storage"));
                controls.record_skipped(NativeSkippedSelection::track(program, requested.number)
                        .with_reason(NativeSkipReason::RequestedByUser))?;
                done += 1;
                skipped += 1;
                estimated_bytes_done = estimated_bytes_done.saturating_add(requested.estimated_bytes.unwrap_or(0));
                estimated_bytes_skipped = estimated_bytes_skipped.saturating_add(requested.estimated_bytes.unwrap_or(0));
                controls.clear_attempted_output();
                emit(done, published.len(), skipped, format!("Skipped {}", requested.name), None, None, estimated_bytes_done, estimated_bytes_skipped, estimated_bytes_published, published.clone());
                continue;
            }
            item_result?;
            controls.clear_attempted_output();
            done += 1;
            estimated_bytes_done = estimated_bytes_done.saturating_add(requested.estimated_bytes.unwrap_or(0));
            emit(done, published.len(), skipped, requested.name.clone(), None, None, estimated_bytes_done, estimated_bytes_skipped, estimated_bytes_published, published.clone());
        }
        if job.file_per_program && !grouped_outputs.is_empty() {
            if cancel.is_cancelled() { return Err("Extraction cancelled".into()); }
            let join_item = controls.begin_item(&cancel);
            controls.set_attempted_output(grouped_target.clone());
            let mut program_published = false;
            let join_result: Result<(), String> = async {
            controls.wait_ready(&cancel, &join_item).await?;
            grouped_outputs.sort_by_key(|(ordinal,_,_)| *ordinal);
            let parts = grouped_outputs.iter().map(|(_,path,_)|path.clone()).collect::<Vec<_>>();
            let grouped_file = temporary.path().join(format!("continuous-{pi}.{}", extraction_extension(&job, program)));
            let grouped = grouped_file.clone();
            let cancel_copy = cancel.clone();
            let join_copy = join_item.clone();
            let container = job.wav_container;
            let sacd = job.format == DiscFormat::Sacd;
            let keep_dst = program.keep_dst;
            let dff = job.sacd_dff || keep_dst;
            let join_bytes = grouped_outputs.iter()
                .filter_map(|(_, path, _)| fs::metadata(path).ok().map(|m| m.len()))
                .fold(0u64, u64::saturating_add);
            controls.watch_tree_range(temporary.path().to_path_buf(),
                format!("Joining {}", program.label), Some(join_bytes), 75, 90);
            emit(done, published.len(), skipped, format!("Joining {}", program.label),
                Some(grouped_file.clone()), Some(join_bytes),
                estimated_bytes_done, estimated_bytes_skipped, estimated_bytes_published, published.clone());
            let join_controls = Arc::clone(&controls);
            let verify_after=job.options.verify_after;
            let (sample_counts, rate) = tokio::task::spawn_blocking(move || {
                let checkpoint = || if join_controls.wait_blocking(&cancel_copy, &join_copy) {
                    Err("Continuous extraction cancelled".to_owned())
                } else { Ok(()) };
                if sacd {
                    let mut hash=verify_after.then(Sha256::new);
                    let mut tap=|bytes:&[u8]| {
                        if let Some(digest)=hash.as_mut() {digest.update(bytes);}
                    };
                    let result=concatenate_sacd_tracks_checked_tapped(
                        &parts,&grouped,dff,keep_dst,&cancel_copy,checkpoint,
                        if verify_after {Some(&mut tap)} else {None})?;
                    if let Some(hash)=hash {
                        join_controls.record_dsd_fingerprint(&grouped,hash.finalize().into());
                    }
                    Ok(result)
                } else {
                    let mut hash=verify_after.then(Sha256::new);
                    let mut tap=|bytes:&[u8]| {
                        if let Some(digest)=hash.as_mut() {digest.update(bytes);}
                    };
                    let result=super::native_wav::concatenate_generated_waves_with_tap(
                        &parts,&grouped,container,&cancel_copy,checkpoint,
                        if verify_after {Some(&mut tap)}else{None})?;
                    if let Some(digest)=hash {
                        // The producer's WAV packet width is carried through
                        // the original native join, not guessed from filenames.
                        let (format, _, _)=super::native_wav::generated_wave_span(&grouped)?;
                        let codec=match format.container_bits {
                            16=>"pcm_s16le",24=>"pcm_s24le",
                            other=>return Err(format!("Unsupported continuous verified PCM precision: {other}")),
                        };
                        join_controls.record_audio_fingerprint(&grouped,digest.finalize().into(),codec);
                    }
                    Ok(result)
                }
            }).await.map_err(|e| format!("Continuous native audio worker: {e}"))??;
            controls.wait_ready(&cancel, &join_item).await?;
            controls.watch_file_range(grouped_file.clone(), format!("Finishing {}", program.label),
                Some(join_bytes), 90, 90);
            let names=grouped_outputs.iter().map(|(_,_,name)|name.clone()).collect::<Vec<_>>();
            let starts=native_track_starts(&names,&sample_counts)?;
            let filename=grouped_target.file_name().and_then(|s|s.to_str())
                .ok_or("Invalid continuous output name")?;
            let cue_content=if native_program_cue_applies(&job,program) {
                let entries=starts.iter().map(|(name,start)|
                    (name.clone(),*start,false)).collect::<Vec<_>>();
                Some(options::cue_text(filename,&entries,rate)?)
            } else {None};
            let cue_file=if let Some(text)=cue_content.as_ref() {
                let path=options::stage_text(temporary.path(),
                    &format!("continuous-{pi}.cue"),text)?;
                // Recovery learns the authored indexes before audio publication.
                controls.record_prepared_cue(&grouped_target.with_extension("cue"),text)?;
                Some(path)
            } else {None};
            let chapters_file=if native_program_chapters_apply(&job,program) {
                let content=options::chapter_text(&starts,rate)?;
                controls.record_prepared_cue(&native_chapters_target(&job,&grouped_target),
                    &content)?;
                Some(options::stage_text(temporary.path(),
                    &format!("continuous-{pi}.chapters.txt"),&content)?)
            } else {None};
            if job.transfer_tags && !(job.format == DiscFormat::Sacd && (job.sacd_dff || program.keep_dst)) {
                let root_tags = if job.format == DiscFormat::Sacd { program.tags.clone() }
                else { native_non_sacd_album_tags(job.tag_source, program, &prepared.album_metadata) };
                let fields = job.tag_fields;
                let art = if job.tag_artwork { native_artwork_path(&prepared.album_metadata, &job.source) } else { None };
                let grouped_copy = grouped_file.clone();
                tokio::task::spawn_blocking(move || {
                    let values = selected_tag_items(&root_tags, fields);
                    write_completed_native_metadata(&grouped_copy, &values, art.as_deref())
                }).await.map_err(|e|format!("Metadata worker: {e}"))??;
            }
            // Embed only in a carrier that actually accepts a CUESHEET tag.
            // This is independent of publication of an external .cue file.
            if job.options.embed_cue && matches!(grouped_target.extension()
                .and_then(|ext|ext.to_str()),Some("flac"|"wv")) {
                let entries=starts.iter().map(|(name,start)|
                    (name.clone(),*start,false)).collect::<Vec<_>>();
                let text=options::cue_text(filename,&entries,rate)?;
                options::embed_cue(&grouped_file,&text)?;
            }
            controls.wait_ready(&cancel, &join_item).await?;
            publish_finished_file_controlled(&job, &grouped_file, &grouped_target, &cancel, &join_item, &controls)?;
            program_published = true;
            published.push(grouped_target.clone());
            estimated_bytes_published = estimated_bytes_published.saturating_add(
                estimated_bytes_for_staged_continuous_tracks(program, &grouped_outputs));
            emit(done, published.len(), skipped, format!("Published {}", program.label),
                None, None, estimated_bytes_done, estimated_bytes_skipped, estimated_bytes_published, published.clone());
            if let Some(cue_file) = cue_file {
                let cue_target = grouped_target.with_extension("cue");
                controls.set_attempted_output(cue_target.clone());
                controls.wait_ready(&cancel, &join_item).await?;
                publish_finished_file_controlled(&job, &cue_file, &cue_target, &cancel, &join_item, &controls)?;
                published.push(cue_target);
                emit(done, published.len(), skipped, format!("Published CUE for {}", program.label),
                    None, None, estimated_bytes_done, estimated_bytes_skipped, estimated_bytes_published, published.clone());
            }
            if let Some(staged)=chapters_file {
                let target=native_chapters_target(&job,&grouped_target);
                controls.set_attempted_output(target.clone());
                controls.wait_ready(&cancel,&join_item).await?;
                publish_finished_file_controlled(&job,&staged,&target,&cancel,&join_item,&controls)?;
                published.push(target);
                emit(done,published.len(),skipped,format!("Published chapters for {}",program.label),
                    None,None,estimated_bytes_done,estimated_bytes_skipped,estimated_bytes_published,
                    published.clone());
            }
            controls.clear_watch();
            emit(done, published.len(), skipped, format!("Completed {}",program.label), None, None, estimated_bytes_done, estimated_bytes_skipped, estimated_bytes_published, published.clone());
            Ok(())
            }.await;
            controls.clear_watch();
            let join_was_skipped = controls.finish_item();
            if cancel.is_cancelled() { return Err("Extraction cancelled".into()); }
            if join_was_skipped && join_result.is_err() && !program_published {
                controls.record_skipped(NativeSkippedSelection::program(program)
                    .with_reason(NativeSkipReason::RequestedByUser))?;
                // Tracks previously skipped in this program have already been
                // counted. The join skip disposes only the successfully
                // staged tracks that would otherwise form the final output.
                let skipped_bytes = grouped_outputs.iter().filter_map(|(number, _, _)|
                    program.tracks.iter().find(|track|track.number == *number)
                        .and_then(|track|track.estimated_bytes))
                    .fold(0u64, u64::saturating_add);
                skipped += grouped_outputs.len();
                estimated_bytes_skipped = estimated_bytes_skipped.saturating_add(skipped_bytes);
                controls.clear_attempted_output();
                emit(done, published.len(), skipped, format!("Skipped continuous {}", program.label),
                    None, None, estimated_bytes_done, estimated_bytes_skipped,
                    estimated_bytes_published, published.clone());
                continue;
            }
            join_result?;
            controls.clear_attempted_output();
        }
        controls.clear_attempted_output();
    }
    if job.extraction_log {
        let targets = native_log_targets(&job);
        for (index, log_target) in targets.into_iter().enumerate() {
            let log = temporary.path().join(format!("extraction-receipt-{index}.log"));
            let mut output = OpenOptions::new().create_new(true).write(true).open(&log)
                .map_err(|e| format!("Cannot stage extraction log: {e}"))?;
            let content = native_log_content_for_index(&job,index,&published,
                &controls.skipped_receipts(),&[],NativeLogOutcome::Complete,None);
            output.write_all(content.as_bytes())
                .map_err(|e|format!("Write extraction log: {e}"))?;
            output.sync_all().map_err(|e|format!("Cannot sync extraction log: {e}"))?;
            publish_finished_file_controlled(&job, &log, &log_target, &cancel, &cancel, &controls)?;
            published.push(log_target);
            emit(done, published.len(), skipped, "Published extraction log".to_owned(),
                None, None, estimated_bytes_done, estimated_bytes_skipped, estimated_bytes_published, published.clone());
            controls.clear_watch();
        }
    }
    Ok(NativeExtractionReport { published, skipped })
}

/// A continuous output contains only successfully staged tracks. A skipped
/// selection remains in the preflight total and skip counter, but contributes
/// no published bytes even after the joined file has been published.
fn estimated_bytes_for_staged_continuous_tracks(
    program: &NativeProgramChoice, grouped_outputs: &[(u32, PathBuf, String)],
) -> u64 {
    grouped_outputs.iter().filter_map(|(number, _, _)|
        program.tracks.iter().find(|track| track.number == *number)
            .and_then(|track| track.estimated_bytes))
        .fold(0u64, u64::saturating_add)
}

/// Produce the same human-readable receipt for a direct run and a resumed
/// run. `published` and `skipped` are execution receipts, not estimates or a
/// fresh scan of filenames that happen to exist in the destination. The
/// log's own pathname is deliberately absent from the output inventory.
#[cfg(test)] // Test fixture helper; production uses the outcome-aware renderer directly.
pub(crate) fn render_native_extraction_log(
    job: &NativeExtractionJob,
    published: &[PathBuf],
    skipped: &[NativeSkippedSelection],
) -> String {
    render_native_extraction_log_with_outcome(job, published, skipped, &[],
        NativeLogOutcome::Complete, None)
}

pub(crate) fn render_native_extraction_log_with_outcome(
    job: &NativeExtractionJob,
    published: &[PathBuf],
    skipped: &[NativeSkippedSelection],
    unfinished: &[NativeUnfinishedOutput],
    outcome: NativeLogOutcome,
    terminal_error: Option<&str>,
) -> String {
    let log_targets = native_log_targets(job).into_iter().collect::<HashSet<_>>();
    let published: HashSet<&Path> = published.iter().map(PathBuf::as_path)
        .filter(|path| !log_targets.contains(*path)).collect();
    // An older journal may contain a selection without a reason. Respect the
    // identity of the selection, but never invent why it was skipped.
    let skip_for = |program: &NativeProgramChoice, number: Option<u32>| {
        skipped.iter().find(|entry| entry.presentation == program.id
            && entry.angle == program.angle && entry.track_number == number)
            .or_else(|| if number.is_some() {skipped.iter().find(|entry|
                entry.presentation == program.id && entry.angle == program.angle
                    && entry.track_number.is_none())} else {None})
    };
    let quoted = |text: &str| format!("{text:?}");
    let yes_no = |flag: bool| if flag { "yes" } else { "no" };
    let duration = |seconds: Option<f64>| -> String {
        match seconds {
            Some(value) if value.is_finite() && value >= 0.0 => {
                let whole = value.round() as u64;
                format!("{}:{:02}", whole / 60, whole % 60)
            }
            _ => "not reported by disc".to_owned(),
        }
    };
    let skip_label = |receipt: &NativeSkippedSelection| match receipt.reason {
        Some(NativeSkipReason::ExistingFile) => "Skipped: file already existed",
        Some(NativeSkipReason::RequestedByUser) => "Skipped: requested by user",
        Some(NativeSkipReason::NoAudioInChapter) => "Skipped: chapter contains no audio",
        None => "Skipped: reason unavailable (earlier run)",
    };
    let saved_size = |path: &Path| -> String {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_file() => format!("{} bytes", metadata.len()),
            _ => "not available".to_owned(),
        }
    };
    let program_name = |p: &NativeProgramChoice| p.id.display_label();
    let unit = match job.format {
        DiscFormat::Sacd => "area",
        DiscFormat::DvdAudio => "group",
        DiscFormat::DvdVideo => "title",
        DiscFormat::BluRay => "playlist",
        DiscFormat::Matroska | DiscFormat::TransportFile => "stream",
        DiscFormat::CueImage => "album",
        DiscFormat::Archive => "archive",
    };
    let item_kind = if job.format == DiscFormat::Archive {"files"}
        else if matches!(job.format, DiscFormat::DvdVideo | DiscFormat::BluRay | DiscFormat::Matroska) {
            "chapters"
        } else { "tracks" };
    let format_name = |p: &NativeProgramChoice| -> String { match job.format {
        DiscFormat::CueImage => {
            let segments=job.cue_segments.iter().filter(|s|s.presentation==p.id)
                .collect::<Vec<_>>();
            let lossy=segments.iter().any(|s|matches!(s.encoder.as_str(),
                "libmp3lame"|"aac"|"libvorbis"|"libopus"));
            let lossless=segments.iter().any(|s|!matches!(s.encoder.as_str(),
                "libmp3lame"|"aac"|"libvorbis"|"libopus"));
            let policy=if lossy && lossless {"lossy packets copied; lossless tracks re-encoded"}
                else if lossy {"source compressed packets copied (no re-encoding)"}
                else {"original lossless format, sample-exact re-encoding"};
            format!("CUE tracks ({policy})")
        },
        DiscFormat::Archive => "Original archived bytes (renamed; no conversion or tag write)".into(),
        DiscFormat::Sacd if p.keep_dst => "DFF (DST kept)".into(),
        DiscFormat::Sacd if job.sacd_dff => "DFF (DSD native; DST decoded if present)".into(),
        DiscFormat::Sacd => "DSF (DSD native; DST decoded if present)".into(),
        DiscFormat::TransportFile => {
            let carrier = file_stream_copy_format(job.format, &p.source_codec);
            if native_pcm_unpack_required(&p.source_codec) {
                "WAV (lossless unpack of transport LPCM; no lossy encoding)".into()
            } else {
                format!("{} (audio packets copied; no re-encoding{})",
                    carrier.extension.to_uppercase(),
                    if job.transfer_tags && !carrier.text_tags {"; carrier cannot store tags"}
                    else {""})
            }
        },
        DiscFormat::Matroska => if job.file_per_program ||
            matches!(p.id, PresentationId::MatroskaStream { chapter_count: 0, .. }) {
            let carrier = matroska_copy_format(&p.source_codec);
            format!("{} (original audio packets; no re-encoding{}{})",
                carrier.extension.to_uppercase(),
                if carrier.chapters {"; chapters retained"} else {"; chapters not embedded"},
                if job.transfer_tags && !carrier.text_tags {"; native carrier cannot store tags"}
                else if job.transfer_tags && job.tag_artwork && !carrier.artwork {
                    "; artwork cannot be embedded in this carrier"
                } else {""})
        } else if p.is_lossless {
            format!("WAV (lossless PCM, {}-bit source depth)",p.source_depth.unwrap_or(32))
        } else { "WAV (24-bit PCM, decoded from lossy source)".into() },
        _ => match job.wav_container {
            NativeWavContainer::Automatic => "WAV (RIFF or RF64 as needed)",
            NativeWavContainer::Riff => "RIFF WAV",
            NativeWavContainer::Rf64 => "RF64 WAV",
            NativeWavContainer::W64 => "W64",
        }.into(),
    }};
    let mut lines = vec![
        format!("TonePoet {} · Extraction log", env!("CARGO_PKG_VERSION")),
        format!("Generated: {}", chrono::Utc::now().to_rfc3339()),
        format!("Source: {}", quoted(&job.source.to_string_lossy())),
        format!("Disc format: {}", job.format.name()),
    ];
    if let Some(title) = &job.disc_identity.title {
        lines.push(format!("Disc title: {}", quoted(title)));
    }
    if let Some(artist) = &job.disc_identity.artist {
        lines.push(format!("Disc artist: {}", quoted(artist)));
    }
    if let Some(catalog) = &job.disc_identity.catalog {
        lines.push(format!("Catalog: {}", quoted(catalog)));
    }
    lines.push(String::new());
    lines.push("Settings".to_owned());
    lines.push(format!("Folder: {}", quoted(&job.destination.to_string_lossy())));
    lines.push(format!("Naming: {}", quoted(&job.naming_template)));
    lines.push(format!("Separate folders by {unit}: {}", yes_no(job.program_subfolders)));
    lines.push(format!("Files: {}", if job.file_per_program {
        format!("one per {unit}")
    } else { format!("one per {}", if item_kind == "chapters" {"chapter"}else{"track"}) }));
    lines.push(format!("CUE sheets: {}", yes_no(job.cue)));
    lines.push(format!("Transfer tags: {}", yes_no(job.transfer_tags)));
    lines.push(format!("Tag fields selected: {}", job.tag_fields.count_ones()));
    lines.push(format!("Artwork: {}", if job.transfer_tags && job.tag_artwork {
        "requested where the selected output carrier supports pictures"
    } else { "off" }));
    lines.push(format!("Tag source: {}", match job.tag_source {
        NativeTagSource::DiscAndSidecar => "tag file, then disc metadata",
        NativeTagSource::AuthoredTrack => "disc metadata, then tag file",
        NativeTagSource::MusicBrainz => "MusicBrainz match saved to tag file",
    }));
    lines.push(format!("When a file exists: {}", match job.overwrite_policy {
        NativeOverwritePolicy::Fail => "stop",
        NativeOverwritePolicy::Skip => "skip existing files",
        NativeOverwritePolicy::Replace => "replace existing files",
    }));
    if let Some(error) = terminal_error {
        lines.push(format!("Interrupted: {}", quoted(error)));
    }
    lines.push(String::new());
    lines.push("Output files".into());
    let mut written = 0usize;
    let mut omitted = 0usize;
    let mut failed = 0usize;
    let mut listed = HashSet::new();
    for program in &job.programs {
        lines.push(format!("{} · {}", program_name(program), format_name(program)));
        // Distinct authored selections, never counts of overlapping skip
        // receipts. A whole-program skip covers only the as-yet uncounted
        // selections; it does not add a second count for earlier track skips.
        let program_skip = skip_for(program, None);
        let mut skipped_numbers = program.tracks.iter()
            .filter(|track| skipped.iter().any(|entry| entry.presentation == program.id
                && entry.angle == program.angle && entry.track_number == Some(track.number)))
            .map(|track| track.number).collect::<HashSet<_>>();
        if program_skip.is_some() {
            skipped_numbers.extend(program.tracks.iter().map(|track| track.number));
        }
        let all_skipped = skipped_numbers.len() == program.tracks.len();
        let audio_path = job.file_per_program.then(||proposed_destination_for_program(job, program));
        let items: Vec<(PathBuf, String, Option<f64>, Option<&NativeSkippedSelection>)> =
            if let Some(path) = &audio_path {
                let whole = program.tracks.iter().try_fold(0.0_f64, |total, track| {
                    // An existing continuous file was skipped as a whole: its
                    // duration is the selected source duration, not zero.
                    if program_skip.is_none() && skip_for(program, Some(track.number)).is_some() {
                        Some(total)
                    } else {
                        track.duration_secs.map(|seconds| total + seconds)
                    }
                });
                vec![(path.clone(),
                    format!("{} ({} selected {item_kind})", program_name(program), program.tracks.len()),
                    whole, program_skip)]
            } else {
                program.tracks.iter().map(|track|(
                    proposed_destination(job, program, track),
                    format!("{} · {:02} {}",program_name(program),track.number,track.name),
                    track.duration_secs,skip_for(program, Some(track.number)),
                )).collect()
            };
        for (path, item, secs, skip) in items {
            if !listed.insert(path.clone()) {continue;}
            lines.push(format!("  File: {}",quoted(&path.to_string_lossy())));
            lines.push(format!("    Item: {}",quoted(&item)));
            let was_published = published.contains(path.as_path());
            let duration_text = if job.file_per_program && all_skipped && !was_published {
                if program_skip.is_some() {
                    format!("{} (selected source; output skipped)", duration(secs))
                } else {
                    "unavailable (no continuous output)".to_owned()
                }
            } else {
                duration(secs)
            };
            lines.push(format!("    Duration: {duration_text}"));
            if was_published {
                written += 1;
                lines.push(format!("    Size: {}",saved_size(&path)));
                lines.push("    Outcome: written".into());
            } else if job.file_per_program && all_skipped {
                lines.push("    Size: not written".into());
                lines.push(format!("    Outcome: {}",skip.map_or(
                    "Skipped: every selected track was individually skipped", skip_label)));
            } else if let Some(receipt) = skip {
                lines.push("    Size: not written".into());
                lines.push(format!("    Outcome: {}",skip_label(receipt)));
            } else if let Some(issue) = unfinished.iter().rev().find(|entry| entry.destination == path) {
                if !issue.cancelled { failed += 1; }
                lines.push("    Size: not written".into());
                lines.push(format!("    Outcome: {} ({})",
                    if issue.cancelled {"cancelled"} else {"failed"}, quoted(&issue.reason)));
            } else if outcome == NativeLogOutcome::Complete {
                failed += 1;
                lines.push("    Size: not written".into());
                lines.push("    Outcome: failed (no completed publication recorded)".into());
            } else {
                lines.push("    Size: not written".into());
                lines.push("    Outcome: not reached".into());
            }
        }
        if job.file_per_program {
            omitted += skipped_numbers.len();
            // A skipped track may be omitted from a successfully published
            // continuous file. The log records both facts, without counting
            // the omitted track twice if the program join was also skipped.
            for track in &program.tracks {
                if let Some(receipt) = skip_for(program, Some(track.number)) {
                    lines.push(format!("  Omitted {:02} {}: {}",track.number,
                        quoted(&track.name),skip_label(receipt)));
                }
            }
        } else {
            omitted += skipped_numbers.len();
        }
        if job.cue && job.file_per_program &&
            (job.format != DiscFormat::Matroska || matroska_cue_for_program(job, program)) {
            let cue = proposed_destination_for_program(job, program).with_extension("cue");
            if listed.insert(cue.clone()) {
                if published.contains(cue.as_path()) {
                    written += 1;
                    lines.push(format!("  CUE: {} · {} · written",quoted(&cue.to_string_lossy()),saved_size(&cue)));
                } else if all_skipped {
                    lines.push(format!("  CUE: {} · skipped (no continuous audio)",quoted(&cue.to_string_lossy())));
                } else if let Some(issue) = unfinished.iter().rev().find(|entry|entry.destination == cue) {
                    if !issue.cancelled { failed += 1; }
                    lines.push(format!("  CUE: {} · {} ({})",quoted(&cue.to_string_lossy()),
                        if issue.cancelled {"cancelled"}else{"failed"},quoted(&issue.reason)));
                } else if outcome == NativeLogOutcome::Complete && audio_path.as_ref()
                    .is_some_and(|path| published.contains(path.as_path())) {
                    failed += 1;
                    lines.push(format!("  CUE: {} · failed (no completed publication recorded)",quoted(&cue.to_string_lossy())));
                } else {
                    lines.push(format!("  CUE: {} · not reached",quoted(&cue.to_string_lossy())));
                }
            }
        }
    }
    lines.push(String::new());
    lines.push(format!("Written files: {written}"));
    lines.push(format!("Skipped items: {omitted}"));
    lines.push(format!("Failed outputs: {failed}"));
    lines.push(format!("Outcome: {}", match outcome {
        NativeLogOutcome::Cancelled => "cancelled",
        NativeLogOutcome::Failed => "finished with failed items",
        NativeLogOutcome::Complete if failed > 0 => "finished with failed items",
        NativeLogOutcome::Complete if omitted > 0 => "finished with skipped items",
        NativeLogOutcome::Complete => "complete",
    }));
    lines.join("\n") + "\n"
}

/// Complete a journaled CUE or extraction log without realizing any audio.
/// The private staging, checked cross-volume publisher, cancellation and
/// journal-owned destination temporary are exactly those of normal extraction.
pub(crate) fn publish_native_recovery_sidecar(
    job: &NativeExtractionJob, target: &Path, content: &str,
    cancel: &CancellationToken, controls: &NativeExtractionControl,
) -> Result<(), String> {
    let work_path = native_working_storage(&job.working_root, &job.destination)?;
    fs::create_dir_all(&work_path).map_err(|e| format!("Sidecar working storage: {e}"))?;
    let staged_dir = tempfile::Builder::new().prefix("tonepoet-extract-sidecar-")
        .tempdir_in(&work_path).map_err(|e|format!("Stage extraction sidecar: {e}"))?;
    let staged = staged_dir.path().join("completed-sidecar");
    let mut output = OpenOptions::new().create_new(true).write(true).open(&staged)
        .map_err(|e|format!("Create extraction sidecar: {e}"))?;
    output.write_all(content.as_bytes()).and_then(|()|output.sync_all())
        .map_err(|e|format!("Write extraction sidecar: {e}"))?;
    drop(output);
    if controls.wait_blocking(cancel, cancel) { return Err("Extraction cancelled".into()); }
    controls.watch_file_range(staged.clone(),
        format!("Publishing {}", target.file_name().unwrap_or_default().to_string_lossy()),
        Some(content.len() as u64), 90, 100);
    let result = publish_finished_file_controlled(job, &staged, target, cancel, cancel, controls);
    controls.clear_watch();
    result
}

/// Publish only completed private artifacts. On a different filesystem the
/// shared Convert publisher copies through a disposable file in the target
/// directory and atomically renames it only after flushing the complete data.
/// A failed copy never exposes a partial result at the requested filename.
#[cfg_attr(not(test), allow(dead_code))] // Compatibility entrypoint exercised by focused tests.
fn publish_finished_file(job: &NativeExtractionJob, complete: &Path, target: &Path) -> Result<(), String> {
    publish_finished_file_checked(job, complete, target, || Ok(()))
}

fn publish_finished_file_controlled(
    job: &NativeExtractionJob, complete: &Path, target: &Path,
    cancel: &CancellationToken, item: &CancellationToken, controls: &NativeExtractionControl,
) -> Result<(), String> {
    publish_finished_file_checked_observed(job, complete, target, || {
        if controls.wait_blocking(cancel, item) {
            Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "Extraction stopped during publication"))
        } else { Ok(()) }
    }, Some(controls))
}

#[cfg_attr(not(test), allow(dead_code))] // Compatibility entrypoint exercised by focused tests.
fn publish_finished_file_checked(
    job: &NativeExtractionJob, complete: &Path, target: &Path,
    checkpoint: impl FnMut() -> std::io::Result<()>,
) -> Result<(), String> {
    publish_finished_file_checked_observed(job, complete, target, checkpoint, None)
}

/// Parse only FFmpeg's complete `-f hash -hash SHA256` receipt, not a
/// filename or a staged container. Its producer invocation writes the receipt
/// while it emits audio, and the destination invocation hashes decoded audio.
fn native_parse_audio_hash(content: &str) -> Result<[u8;32],String> {
    let hex=content.trim().strip_prefix("SHA256=")
        .ok_or("Missing producer-time SHA256 audio receipt")?;
    if hex.len()!=64 || !hex.bytes().all(|ch|ch.is_ascii_hexdigit()) {
        return Err("Invalid producer-time SHA256 audio receipt".into());
    }
    let mut digest=[0u8;32];
    for (i,part) in digest.iter_mut().enumerate() {
        *part=u8::from_str_radix(&hex[i*2..i*2+2],16)
            .map_err(|_|"Invalid SHA256 hex byte")?;
    }
    Ok(digest)
}

fn native_audio_hash_receipt(path: &Path) -> Result<[u8;32],String> {
    let content=fs::read_to_string(path).map_err(|e|
        format!("Read producer-time audio checksum {}: {e}",path.display()))?;
    native_parse_audio_hash(&content)
}

/// Read the completed destination as decoded audio, not container bytes.
/// The private checksum receipt is outside the requested destination. This
/// path runs only for opt-in verified extraction.
fn native_readback_audio_hash(
    path: &Path, decoder: &str, source_format: Option<&str>,
    checkpoint: &mut impl FnMut() -> std::io::Result<()>,
) -> Result<[u8;32],String> {
    let temp=tempfile::Builder::new().prefix("tonepoet-audio-check-").tempdir()
        .map_err(|e|format!("Private verification storage: {e}"))?;
    let receipt=temp.path().join("decoded.sha256");
    let mut command=std::process::Command::new("ffmpeg");
    command.args(["-hide_banner","-nostdin","-loglevel","error","-xerror","-y"]);
    // Cross-volume verified publication checks the journaled .tmp name, not
    // the final file. Explicitly name raw-stream demuxers where suffix-based
    // input identification would otherwise be lost.
    if let Some(format)=source_format {command.args(["-f",format]);}
    let mut child=command.arg("-i").arg(path)
        .args(["-map","0:a:0","-vn","-sn","-dn","-c:a",decoder,
            "-f","hash","-hash","SHA256"])
        .arg(&receipt)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn().map_err(|e|format!("Start audio readback decoder: {e}"))?;
    let start=std::time::Instant::now();
    loop {
        if let Err(e)=checkpoint() {
            let _=child.kill(); let _=child.wait();
            return Err(format!("Audio verification interrupted: {e}"));
        }
        if start.elapsed()>Duration::from_secs(6*60*60) {
            let _=child.kill(); let _=child.wait();
            return Err("Audio verification decoder exceeded six hours".into());
        }
        match child.try_wait().map_err(|e|format!("Wait for audio readback: {e}"))? {
            Some(status) if status.success() => break,
            Some(status) => return Err(format!("Audio readback decoder failed: {status}")),
            None => std::thread::sleep(Duration::from_millis(35)),
        }
    }
    native_audio_hash_receipt(&receipt)
}

/// Compare the extraction-time producer reference in its own representation.
/// Containers and tags are ignored for PCM, packets and DSD frames; archive
/// copies are exact identity transfers and therefore use their written bytes.
fn native_readback_fingerprint(
    path: &Path, target: &Path, reference: &NativeAudioFingerprint,
    checkpoint: &mut dyn FnMut() -> std::io::Result<()>,
) -> Result<[u8;32],String> {
    match &reference.format {
        NativeFingerprintFormat::Pcm(decoder) => {
            let raw_format=if path==target {None} else {
                match target.extension().and_then(|ext|ext.to_str())
                    .map(|ext|ext.to_ascii_lowercase()).as_deref() {
                    Some("aac")=>Some("aac"),Some("ac3")=>Some("ac3"),
                    Some("eac3")=>Some("eac3"),Some("dts")=>Some("dts"),
                    Some("thd")=>Some("truehd"),Some("mp2")=>Some("mp3"),
                    _=>None,
                }
            };
            native_readback_audio_hash(path,decoder,raw_format,&mut ||checkpoint())
        },
        NativeFingerprintFormat::IdentityCopy => {
            let mut input=File::open(path).map_err(|e|format!("Open identity readback: {e}"))?;
            let mut digest=Sha256::new();
            let mut bytes=[0u8;256*1024];
            loop {
                checkpoint().map_err(|e|format!("Identity readback interrupted: {e}"))?;
                let read=input.read(&mut bytes).map_err(|e|format!("Read identity output: {e}"))?;
                if read==0 {break;}
                digest.update(&bytes[..read]);
            }
            Ok(digest.finalize().into())
        }
        NativeFingerprintFormat::Packets => cue::packet_audio_fingerprint(path,checkpoint),
        NativeFingerprintFormat::DsdFrames => {
            use sacd_rs::dsd_file::{open_dsd_as_decoded_reader,DsdFrameReader};
            let input=File::open(path).map_err(|e|format!("Open DSD readback: {e}"))?;
            let mut reader=open_dsd_as_decoded_reader(input)
                .map_err(|e|format!("Open decoded DSD frames for verification: {e}"))?;
            let mut digest=Sha256::new();
            while let Some(frame) = {
                checkpoint().map_err(|e|format!("DSD readback interrupted: {e}"))?;
                reader.next_dsd_frame().map_err(|e|format!("Read verified DSD frames: {e}"))?
            } {
                digest.update(&frame.data);
            }
            Ok(digest.finalize().into())
        }
    }
}

fn native_is_audio_target(path: &Path) -> bool {
    path.extension().and_then(|ext|ext.to_str()).is_some_and(|ext|matches!(
        ext.to_ascii_lowercase().as_str(),
        "wav"|"w64"|"rf64"|"dsf"|"dff"|"flac"|"wv"|"mka"|"mkv"|"m4a"|
        "aac"|"ac3"|"eac3"|"dts"|"thd"|"mp3"|"mp2"|"ogg"|"opus"|"ape"|"tta"|
        "aiff"|"aif"|"alac"|"pcm"))
}

fn publish_finished_file_checked_observed(
    job: &NativeExtractionJob, complete: &Path, target: &Path,
    mut checkpoint: impl FnMut() -> std::io::Result<()>,
    controls: Option<&NativeExtractionControl>,
) -> Result<(), String> {
    checkpoint().map_err(|e|format!("Publication cancelled: {e}"))?;
    // A real producer tap is mandatory when Verify is selected. Never
    // replace a missing live reference with a post-extraction file read.
    let expected_audio = if job.options.verify_after && native_is_audio_target(target) {
        Some(controls.and_then(|control|control.take_audio_fingerprint(complete))
            .ok_or_else(||format!("Cannot verify {}: this extraction route did not capture audio while writing",
                target.display()))?)
    } else {None};
    if job.overwrite_policy != NativeOverwritePolicy::Replace && fs::symlink_metadata(target).is_ok() {
        return Err(format!("Destination already exists: '{}'",target.display()));
    }
    let parent = target.parent().ok_or("Invalid destination path")?;
    let parent_existed = parent.exists();
    let root_existed = job.destination.exists();
    fs::create_dir_all(parent).map_err(|e|format!("Cannot create destination parent: {e}"))?;
    let hook = controls.and_then(NativeExtractionControl::publication_hook);
    let private_temp = hook.as_ref().map(|hook| {
        let name = target.file_name().and_then(|s|s.to_str()).unwrap_or("audio");
        parent.join(format!(".{name}.tonepoet-part-{}-{}-{}-{}.tmp",
            hook.job_id, hook.generation, std::process::id(), uuid::Uuid::new_v4().as_u128()))
    });
    let observed_temp = private_temp.clone();
    let mut notify_temporary = |temporary: Option<&Path>| {
        if let Some(hook) = &hook {
            let temp = observed_temp.as_deref().ok_or_else(||std::io::Error::other(
                "Missing native publication temporary"))?;
            if let Some(controls) = controls {
                if temporary.is_some() {
                    controls.watch_file_range(temp.to_path_buf(),
                        format!("Publishing {}",target.file_name().unwrap_or_default().to_string_lossy()),
                        fs::metadata(complete).ok().map(|m|m.len()),
                        if job.file_per_program { 90 } else { 85 }, 100);
                }
            }
            (hook.notify)(temp, target, temporary.is_some())
                .map_err(std::io::Error::other)?;
        }
        Ok(())
    };
    let result = if let Some(expected) = expected_audio.as_ref() {
        super::transactional_state::publish_validated_to_final_checked_preverified_observed(
            complete, target, &mut checkpoint, private_temp,
            &mut notify_temporary,
            |ready, check| {
                let observed = native_readback_fingerprint(ready, target, expected, check)
                    .map_err(std::io::Error::other)?;
                if expected.digest != observed {
                    return Err(std::io::Error::other(
                        "Verification failed: completed audio differs from extraction checksum"));
                }
                Ok(())
            })
    } else {
        // Keep the ordinary, unverified publisher entirely unchanged.
        super::transactional_state::publish_validated_to_final_checked_observed(
            complete, target, &mut checkpoint, private_temp, &mut notify_temporary)
    };
    if let Err(error) = result {
        if !parent_existed { let _ = fs::remove_dir(parent); }
        if !root_existed { let _ = fs::remove_dir(&job.destination); }
        return Err(format!("Cannot publish complete file '{}': {error}", target.display()));
    }
    Ok(())
}

/// Only literal program tokens remove the need for implicit program folders.
/// File metadata tokens such as %ARTIST% do not uniquely identify an area.
pub(crate) fn native_template_has_program_token(template: &str) -> bool {
    ["%AREA%", "%GROUP%", "%DVDTITLE%", "%PLAYLIST%", "%STREAM%"]
        .iter().any(|token| template.contains(token))
}

fn prefix_implicit_program_name(job: &NativeExtractionJob) -> bool {
    !job.program_subfolders && (!job.round4_layout ||
        (job.programs.len() > 1 && !native_template_has_program_token(&job.naming_template)))
}

/// Contextual sidecars are planned with the audio destination so log naming,
/// admission and the worker agree on collision handling.
fn native_program_cue_applies(job: &NativeExtractionJob,
    program: &NativeProgramChoice) -> bool {
    if !(job.cue || job.options.create_cue) {return false;}
    match job.format {
        DiscFormat::Sacd => job.file_per_program || job.sacd_dff || program.keep_dst,
        DiscFormat::DvdAudio|DiscFormat::DvdVideo|DiscFormat::BluRay => job.file_per_program,
        DiscFormat::Matroska|DiscFormat::TransportFile => {
            let carrier=file_stream_copy_format(job.format,&program.source_codec);
            let splits_chapters=matches!(program.id,
                PresentationId::MatroskaStream{chapter_count,..} if chapter_count>0)
                && !job.file_per_program;
            !splits_chapters && !carrier.chapters &&
                !matches!(carrier.extension,"mka"|"mkv"|"m4a")
        },
        DiscFormat::CueImage|DiscFormat::Archive => false,
    }
}

fn native_program_chapters_apply(job: &NativeExtractionJob,
    program: &NativeProgramChoice) -> bool {
    if !job.options.write_chapters {return false;}
    match job.format {
        DiscFormat::Sacd|DiscFormat::DvdAudio|DiscFormat::DvdVideo|DiscFormat::BluRay =>
            job.file_per_program && program.tracks.len()>1,
        DiscFormat::Matroska => job.file_per_program &&
            matches!(program.id,PresentationId::MatroskaStream{chapter_count,..} if chapter_count>0) &&
            !file_stream_copy_format(job.format,&program.source_codec).chapters,
        _ => false,
    }
}

fn native_chapters_target(job: &NativeExtractionJob, audio: &Path) -> PathBuf {
    let stem=audio.file_stem().unwrap_or_default().to_string_lossy();
    // When multiple outputs share one directory, names remain distinct.
    let name=if job.programs.len()>1 && !job.program_subfolders {
        format!("{stem}.chapters.txt")
    } else {"chapters.txt".into()};
    audio.with_file_name(name)
}

fn native_track_starts(titles:&[String], samples:&[u64])
    -> Result<Vec<(String,u64)>,String> {
    if titles.len()!=samples.len() || titles.is_empty() {return Err("Track geometry mismatch".into());}
    let mut offset=0u64;
    let mut starts=Vec::with_capacity(samples.len());
    for (name,count) in titles.iter().zip(samples) {
        if *count==0 {return Err("Empty track in continuous output".into());}
        starts.push((name.clone(),offset));
        offset=offset.checked_add(*count).ok_or("Track offset overflow")?;
    }
    Ok(starts)
}

/// All selected media paths for one authored program, as the worker will
/// publish them. The same source drives output roots and log reservations.
pub(crate) fn native_program_media_paths(job: &NativeExtractionJob, program: &NativeProgramChoice) -> Vec<PathBuf> {
    if job.file_per_program {
        let audio = proposed_destination_for_program(job, program);
        let mut paths = vec![audio.clone()];
        if native_program_cue_applies(job,program) {paths.push(audio.with_extension("cue"));}
        if native_program_chapters_apply(job,program) {
            paths.push(native_chapters_target(job,&audio));
        }
        paths
    } else {
        let mut paths=program.tracks.iter().map(|track|
            proposed_destination(job, program, track)).collect::<Vec<_>>();
        if let Some(hidden)=native_cue_hidden_target(job,program) {paths.push(hidden);}
        if job.format==DiscFormat::Sacd && native_program_cue_applies(job,program) {
            paths.extend(program.tracks.iter().map(|track|
                proposed_destination(job,program,track).with_extension("cue")));
        }
        if is_file_stream_format(job.format) && native_program_cue_applies(job,program) {
            paths.extend(program.tracks.iter().map(|track|
                proposed_destination(job,program,track).with_extension("cue")));
        }
        paths
    }
}

/// A hidden first-track pregap is an additional, explicitly requested
/// audio output. Its target is derived from the first real track's folder,
/// and is reserved alongside every other selected destination.
pub(crate) fn native_cue_hidden_target(job: &NativeExtractionJob,
    program: &NativeProgramChoice) -> Option<PathBuf> {
    if job.format!=DiscFormat::CueImage ||
        job.options.first_gap!=NativeCueFirstGap::OwnTrack00 {return None;}
    let first=program.tracks.iter().find(|track|track.number==1)?;
    let segment=job.cue_segments.iter().find(|s|
        s.presentation==program.id && s.number==1 && s.index01_sample>0 && !s.hidden_track_published)?;
    let track_target=proposed_destination(job,program,first);
    Some(track_target.with_file_name(format!("00 - Hidden Track.{}",segment.extension)))
}

/// Deepest common output directory for this program's selected files.
/// A template may introduce subdirectories: choose the common parent rather
/// than the top-level Folder or the directory of an arbitrary first track.
pub(crate) fn native_program_output_root(job: &NativeExtractionJob, program: &NativeProgramChoice) -> PathBuf {
    let paths = native_program_media_paths(job, program);
    let mut parents = paths.iter().filter_map(|path|path.parent());
    let Some(first) = parents.next() else { return job.destination.clone(); };
    let mut common = first.to_path_buf();
    for parent in parents {
        while !parent.starts_with(&common) {
            if !common.pop() { return job.destination.clone(); }
        }
    }
    if common.starts_with(&job.destination) { common } else { job.destination.clone() }
}

/// A planned log path is an immutable part of the queued job. Suffixes protect
/// an existing user receipt even under Stop/Skip. Replace explicitly permits
/// replacing the base name. Avoid collisions with *all* planned media and
/// other programs' logs, including on case-insensitive filesystems.
pub(crate) fn plan_native_program_logs(job: &mut NativeExtractionJob) -> Result<(), String> {
    job.program_log_paths.clear();
    if !job.extraction_log { return Ok(()); }
    let mut reserved = HashSet::new();
    for program in &job.programs {
        for path in native_program_media_paths(job, program) {
            reserved.insert(native_destination_collision_key(&path));
        }
    }
    for program in &job.programs {
        let root = native_program_output_root(job, program);
        let mut chosen = None;
        for suffix in 1..=100_000u32 {
            let name = if suffix == 1 { EXTRACTION_LOG_FILE.to_owned() }
                else { format!("extraction-{suffix}.log") };
            let candidate = root.join(name);
            if reserved.contains(&native_destination_collision_key(&candidate)) { continue; }
            match fs::symlink_metadata(&candidate) {
                Ok(_) if job.overwrite_policy != NativeOverwritePolicy::Replace => continue,
                Ok(_) => {},
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
                Err(error) => return Err(format!("Inspect extraction log '{}': {error}",candidate.display())),
            }
            reserved.insert(native_destination_collision_key(&candidate));
            chosen = Some(candidate);
            break;
        }
        job.program_log_paths.push(chosen.ok_or_else(||
            format!("No free extraction log name in '{}'",root.display()))?);
    }
    Ok(())
}

/// Bind authored log paths to the same admitted filesystem root as the
/// audio outputs. The journal stores logical paths, but the detached worker
/// publishes through resolved endpoints; never mix those path namespaces.
pub(crate) fn admit_native_destination(job: &mut NativeExtractionJob, root: PathBuf) -> Result<(),String> {
    if !job.program_log_paths.is_empty() {
        let paths = job.program_log_paths.iter().map(|path| {
            path.strip_prefix(&job.destination)
                .map(|relative| root.join(relative))
                .map_err(|_|format!("Extraction log is outside the selected destination: {}",path.display()))
        }).collect::<Result<Vec<_>,_>>()?;
        job.program_log_paths = paths;
    }
    job.destination = root;
    Ok(())
}

/// Empty list means a pre-R34 job with its single legacy root log.
pub(crate) fn native_log_targets(job: &NativeExtractionJob) -> Vec<PathBuf> {
    if !job.extraction_log { return Vec::new(); }
    if job.program_log_paths.is_empty() { vec![job.destination.join(EXTRACTION_LOG_FILE)] }
    else { job.program_log_paths.clone() }
}

/// A new receipt describes only its own program; old journals keep their
/// historical combined receipt to avoid changing published contract on resume.
pub(crate) fn native_log_content_for_index(
    job: &NativeExtractionJob, index: usize, published: &[PathBuf],
    skipped: &[NativeSkippedSelection], unfinished: &[NativeUnfinishedOutput],
    outcome: NativeLogOutcome, error: Option<&str>,
) -> String {
    if job.program_log_paths.is_empty() {
        return render_native_extraction_log_with_outcome(job,published,skipped,unfinished,outcome,error);
    }
    let mut per_program = job.clone();
    per_program.programs = vec![job.programs[index].clone()];
    render_native_extraction_log_with_outcome(&per_program,published,skipped,unfinished,outcome,error)
}

pub(crate) fn proposed_destination_for_program(job: &NativeExtractionJob, program: &NativeProgramChoice) -> PathBuf {
    let virtual_track = NativeTrackChoice { duration_secs: None, number: 0, name: program.label.clone(), tags: NativeTagValues::default(), estimated_bytes: None };
    let legacy = legacy_naming_template(&job.naming_template);
    // Already-queued/recoverable jobs still store the pre-R27 default. Their
    // continuous output must retain its existing name across an upgrade.
    let mut stem = if job.naming_template == default_naming_template()
        || job.naming_template == "{track} - {title}" {
        sanitize_component(&program.label)
    } else { render_naming_template_for_extension(&job.naming_template, program, &virtual_track,
        extraction_extension(job, program), &job.disc_identity) };
    if prefix_implicit_program_name(job) {
        stem = if legacy {sanitize_component(&format!("{} - {stem}",program.label))}
            else {prefix_flat_native_name(&program.label,stem)};
    }
    let parent = if job.program_subfolders { job.destination.join(sanitize_component(&program.label)) }
        else { job.destination.clone() };
    if legacy {parent.join(format!("{stem}.{}",extraction_extension(job,program)))}
    else {parent.join(with_native_extension(stem, extraction_extension(job, program)))}
}

/// CUE indexes have 1/75-second resolution. Round *only the CUE index* to
/// the nearest frame; never insert/remove samples or change the native media.
fn cue_for_continuous(filename: &str, titles: &[String], samples: &[u64], rate: u32) -> Result<String,String> {
    if titles.len() != samples.len() || titles.len() > 99 || rate == 0 {
        return Err("CUE cannot encode these native sample offsets".into());
    }
    let mut cursor = 0u64;
    let mut lines = vec![format!("FILE \"{}\" WAVE", filename.replace('"', "'")),
        "REM CUE indexes rounded to nearest 1/75 second; native audio samples are unchanged".into()];
    for (index,(title,count)) in titles.iter().zip(samples).enumerate() {
        let cue_frame = (u128::from(cursor) * 75 + u128::from(rate / 2)) / u128::from(rate);
        let cue_frame = u64::try_from(cue_frame).map_err(|_| "CUE index exceeds available range")?;
        let mm = cue_frame / (60 * 75);
        let ss = (cue_frame / 75) % 60;
        let ff = cue_frame % 75;
        lines.push(format!("  TRACK {:02} AUDIO",index+1));
        lines.push(format!("    TITLE \"{}\"",title.replace('"', "'").replace('\r', " " ).replace('\n', " " )));
        lines.push(format!("    INDEX 01 {mm:02}:{ss:02}:{ff:02}"));
        cursor = cursor.checked_add(*count).ok_or("CUE sample-count overflow")?;
    }
    Ok(format!("{}\n",lines.join("\n")))
}

/// Both staged output modes use this order. DSF's first ID3 write allocates
/// the supplied artwork plus its 1 MiB edit reserve, allowing ordinary later
/// text edits in place; text-first can require a full-file audio-prefix copy.
/// All non-DSF writers retain their original text-then-artwork order.
fn write_completed_native_metadata(
    path: &Path,
    entries: &[(lofty::tag::ItemKey, Option<String>)],
    artwork: Option<&Path>,
) -> Result<(), String> {
    let write_tags = || {
        if !entries.is_empty() {
            crate::tui::probe::write_all_tags(path, entries)
                .map_err(|error| format!("Write native tags to completed private media: {error}"))?;
        }
        Ok::<(), String>(())
    };
    let write_artwork = || {
        if let Some(image) = artwork {
            let target = path.to_path_buf();
            crate::tui::probe::write_artwork_to_files_with_cancel(
                std::slice::from_ref(&target), image,
                lofty::picture::PictureType::CoverFront, None,
            ).map_err(|error| format!("Embed native artwork: {error}"))?;
        }
        Ok::<(), String>(())
    };
    if crate::dsf_tags::is_dsf(path) {
        write_artwork()?;
        write_tags()
    } else {
        write_tags()?;
        write_artwork()
    }
}

/// Supported tag keys are sent to the existing metadata persistence writers.
/// This runs on completed private media only; a failure never publishes a
/// partially tagged file. The caller's field mask is the exact user choice.
pub(crate) fn selected_tag_items(values: &NativeTagValues, fields: u16) -> Vec<(lofty::tag::ItemKey, Option<String>)> {
    use lofty::tag::ItemKey;
    let items = [
        (0, ItemKey::TrackTitle, &values.title),
        (1, ItemKey::TrackArtist, &values.artist),
        (2, ItemKey::AlbumTitle, &values.album),
        (3, ItemKey::Genre, &values.genre),
        (4, ItemKey::Year, &values.year),
        (5, ItemKey::Composer, &values.composer),
        (6, ItemKey::Performer, &values.performer),
        (7, ItemKey::CatalogNumber, &values.catalog),
        (8, ItemKey::Publisher, &values.publisher),
    ];
    items.into_iter().filter_map(|(i, key, value)| {
        if fields & (1 << i) == 0 { return None; }
        value.as_ref().filter(|v| !v.trim().is_empty()).map(|v|(key,Some(v.clone())))
    }).collect()
}

/// Exact tag values used by the individual native writer when they can be
/// resolved before materialization. For authored PCM the worker uses the
/// prepared track instead, so the UI must mark those fields as pending.
pub(crate) fn native_selected_track_tags(
    format: DiscFormat, source: NativeTagSource,
    program: &NativeProgramChoice, requested: &NativeTrackChoice,
) -> Option<NativeTagValues> {
    if format != DiscFormat::Sacd && source != NativeTagSource::DiscAndSidecar {
        return None;
    }
    Some(NativeTagValues {
        title: requested.tags.title.clone(),
        artist: requested.tags.artist.clone().or_else(||program.tags.artist.clone()),
        album: if format == DiscFormat::Sacd {
            requested.tags.album.clone().or_else(||program.tags.album.clone())
        } else { program.tags.album.clone() },
        genre: requested.tags.genre.clone().or_else(||program.tags.genre.clone()),
        year: requested.tags.year.clone().or_else(||program.tags.year.clone()),
        composer: requested.tags.composer.clone().or_else(||
            (format == DiscFormat::Sacd).then(||program.tags.composer.clone()).flatten()),
        performer: requested.tags.performer.clone().or_else(||
            (format == DiscFormat::Sacd).then(||program.tags.performer.clone()).flatten()),
        catalog: requested.tags.catalog.clone().or_else(||program.tags.catalog.clone()),
        publisher: requested.tags.publisher.clone().or_else(||program.tags.publisher.clone()),
    })
}

fn native_non_sacd_album_tags(
    source: NativeTagSource, program: &NativeProgramChoice, album: &AlbumMetadata,
) -> NativeTagValues {
    // The materializer has already applied the saved tag file. Only an
    // independently observed authored field can override it: the navigation
    // projection is often itself sidecar-merged (notably for DVD-Audio).
    let prepared = NativeTagValues {
        album: album.album.clone(),
        artist: album.album_artist.as_deref().map(str::to_string),
        genre: album.genre.as_deref().map(str::to_string),
        year: album.date.clone(),
        ..Default::default()
    };
    match source {
        NativeTagSource::DiscAndSidecar => program.tags.clone(),
        NativeTagSource::AuthoredTrack => program.authored_tags.as_ref()
            .map(|tags| tags.album.prioritized(&prepared)).unwrap_or(prepared),
        NativeTagSource::MusicBrainz => prepared,
    }
}

fn native_prepared_track_tags(
    job: &NativeExtractionJob, requested: &NativeTrackChoice,
    prepared: &PreparedTrack, album: &AlbumMetadata,
) -> NativeTagValues {
    NativeTagValues {
        title: prepared.metadata.title.as_ref().filter(|title| {
            !matches!(job.format, DiscFormat::DvdVideo | DiscFormat::BluRay)
                || *title != &format!("Chapter {}", requested.number)
        }).cloned(),
        artist: prepared.metadata.artist.as_deref().map(str::to_string),
        album: album.album.clone(),
        genre: prepared.metadata.genre.as_deref().map(str::to_string),
        year: prepared.metadata.date.clone().or_else(||album.date.clone()),
        composer: prepared.metadata.composer.as_deref().map(str::to_string),
        performer: prepared.metadata.performer.as_deref().map(str::to_string),
        catalog: prepared.metadata.extra.get("CATALOGNUMBER").cloned(),
        publisher: prepared.metadata.publisher.clone(),
    }
}

fn native_non_sacd_track_tags(
    format: DiscFormat, source: NativeTagSource, program: &NativeProgramChoice,
    requested: &NativeTrackChoice, prepared: NativeTagValues,
) -> NativeTagValues {
    // The navigation projection is still used by the old default, but must
    // never be mistaken for independent authored evidence in authored-first
    // mode: DVD-A metabase titles, for example, already include sidecar tags.
    match source {
        NativeTagSource::DiscAndSidecar => native_selected_track_tags(format,
            source, program, requested).unwrap_or_default(),
        NativeTagSource::AuthoredTrack => program.authored_tags.as_ref().map(|tags| {
            let authored_track = tags.tracks.get(&requested.number).cloned().unwrap_or_default();
            authored_track.prioritized(&tags.album).prioritized(&prepared)
        }).unwrap_or(prepared),
        NativeTagSource::MusicBrainz => prepared,
    }
}

fn extraction_tags(
    job: &NativeExtractionJob, program: &NativeProgramChoice,
    requested: &NativeTrackChoice, prepared: &PreparedTrack,
    album: &AlbumMetadata,
) -> Vec<(lofty::tag::ItemKey, Option<String>)> {
    let values = if job.format == DiscFormat::Sacd {
        // SACD source authority was resolved at job creation and is unchanged.
        native_selected_track_tags(job.format, job.tag_source, program, requested)
            .unwrap_or_default()
    } else {
        let from_sidecar = native_prepared_track_tags(job, requested, prepared, album);
        native_non_sacd_track_tags(job.format, job.tag_source,
            program, requested, from_sidecar)
    };
    selected_tag_items(&values, job.tag_fields)
}

/// Artwork path from the established materializer or the same shallow disc-
/// folder cover-name policy used by Browse. Never descend recursively or
/// synthesize an image. No file is copied to the destination: the existing
/// metadata writer embeds the chosen original into completed private media.
pub(crate) fn native_artwork_path(album: &AlbumMetadata, source: &Path) -> Option<PathBuf> {
    let embedded = [STAGED_SOURCE_ARTWORK_PATH_EXTRA_KEY, CUE_ARTWORK_PATH_EXTRA_KEY]
        .into_iter().filter_map(|key|album.extra.get(key))
        .map(PathBuf::from).find(|path|path.is_file());
    if embedded.is_some() { return embedded; }

    let directory = if source.is_dir() { source } else { source.parent()? };
    let parent = directory.file_name().and_then(|name|name.to_str())
        .filter(|name|matches!(name.to_ascii_uppercase().as_str(), "BDMV" | "VIDEO_TS" | "AUDIO_TS"))
        .and_then(|_|directory.parent());
    let preferred = crate::config::BrowseArtworkConfig::default().filenames;
    for folder in [Some(directory), parent].into_iter().flatten() {
        // Bound a local directory scan; only direct entries are considered.
        let mut matches = Vec::<(usize,PathBuf)>::new();
        let Ok(entries) = fs::read_dir(folder) else { continue; };
        for entry in entries.take(32_768).flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|value|value.to_str()) else { continue; };
            let Some(rank) = preferred.iter().position(|candidate|candidate.eq_ignore_ascii_case(name)) else { continue; };
            if !crate::tui::browse_artwork::is_supported_image(&path) { continue; }
            if fs::metadata(&path).is_ok_and(|metadata|metadata.is_file()) {
                matches.push((rank,path));
            }
        }
        matches.sort_by(|a,b|a.0.cmp(&b.0).then_with(||a.1.cmp(&b.1)));
        if let Some((_,path)) = matches.into_iter().next() { return Some(path); }
    }
    None
}

/// Carrier for a byte-exact Matroska audio-stream copy. An unrecognized or
/// unsupported elementary stream remains in MKA, which can carry its chapters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MatroskaCopyFormat {
    pub extension: &'static str,
    pub muxer: &'static str,
    pub chapters: bool,
    pub text_tags: bool,
    pub artwork: bool,
}

pub(crate) fn matroska_copy_format(codec: &str) -> MatroskaCopyFormat {
    let (extension, muxer, chapters, text_tags, artwork) = match codec {
        "flac" => ("flac", "flac", false, true, true),
        "mp3" => ("mp3", "mp3", false, true, true),
        "aac" => ("aac", "adts", false, false, false),
        "ac3" => ("ac3", "ac3", false, false, false),
        "eac3" => ("eac3", "eac3", false, false, false),
        "dts" => ("dts", "dts", false, false, false),
        "truehd" => ("thd", "truehd", false, false, false),
        "mp2" => ("mp2", "mp2", false, false, false),
        // Ogg repackaging of Matroska Opus/Vorbis changes the retained
        // sample extent (Opus pre-skip/tail or Vorbis end trimming) even with
        // -c copy. Keep MKA rather than publish altered presentation audio.
        "opus" | "vorbis" => ("mka", "matroska", true, true, true),
        "wavpack" => ("wv", "wv", false, true, false),
        "tta" => ("tta", "tta", false, true, false),
        // Matroska ALAC packets often lack a final packet duration. Even
        // with MP4 edit lists disabled, an M4A copy advertises a truncated
        // timeline despite retaining the decoded samples. Keep the MKA
        // presentation clock and authored chapters rather than claim a
        // valid native M4A copy.
        "alac" => ("mka", "matroska", true, true, true),
        codec if matches!(codec,
            "pcm_u8" | "pcm_s16le" | "pcm_s24le" | "pcm_s32le" |
            "pcm_s64le" | "pcm_f32le" | "pcm_f64le") =>
            ("wav", "wav", false, true, false),
        _ => ("mka", "matroska", true, true, true),
    };
    MatroskaCopyFormat { extension, muxer, chapters, text_tags, artwork }
}

/// Preserve Matroska's native container mapping unchanged. Only transport
/// LPCM requires lossless packet unpacking into ordinary WAV PCM; neither
/// transport-specific codec can be packet-copied to the WAV muxer.
pub(crate) fn file_stream_copy_format(format: DiscFormat, codec: &str) -> MatroskaCopyFormat {
    if format == DiscFormat::TransportFile && native_pcm_unpack_required(codec) {
        MatroskaCopyFormat { extension: "wav", muxer: "wav",
            chapters: false, text_tags: true, artwork: false }
    } else { matroska_copy_format(codec) }
}

fn native_pcm_unpack_required(codec: &str) -> bool {
    matches!(codec, "pcm_dvd" | "pcm_bluray")
}

pub(crate) fn matroska_cue_for_program(job: &NativeExtractionJob,
    program: &NativeProgramChoice) -> bool {
    job.format == DiscFormat::Matroska && job.file_per_program &&
        matches!(program.id, PresentationId::MatroskaStream { chapter_count, .. } if chapter_count > 0) &&
        !matroska_copy_format(&program.source_codec).chapters
}

fn extraction_extension(job: &NativeExtractionJob, program: &NativeProgramChoice) -> &'static str {
    if is_file_stream_format(job.format) {
        if job.format == DiscFormat::TransportFile || job.file_per_program || matches!(program.id,
            PresentationId::MatroskaStream { chapter_count: 0, .. }) {
            file_stream_copy_format(job.format, &program.source_codec).extension
        } else { "wav" }
    } else if job.format == DiscFormat::Sacd {
        if job.sacd_dff || program.keep_dst { "dff" } else { "dsf" }
    } else if job.wav_container == NativeWavContainer::W64 { "w64" } else { "wav" }
}

fn name_value(token: &str, program: &NativeProgramChoice, track: &NativeTrackChoice) -> String {
    let id = program.id;
    match token {
        "track" | "chapter" => format!("{:02}", track.number),
        "title" => track.tags.title.clone().unwrap_or_else(|| track.name.clone()),
        "stream" => program.label.clone(),
        "area" => if matches!(id, PresentationId::SacdArea(_)) { program.label.clone() } else { String::new() },
        "group" => if let PresentationId::DvdAudioGroup(n) = id { format!("{n}") } else { String::new() },
        "playlist" => id.blu_ray_parts().map(|(n,_,_,_)| format!("{n:05}")).unwrap_or_default(),
        "album" => program.tags.album.clone().unwrap_or_default(),
        "artist" => track.tags.artist.as_ref().or(program.tags.artist.as_ref()).cloned().unwrap_or_default(),
        "albumartist" => program.tags.artist.clone().unwrap_or_default(),
        "performer" => track.tags.performer.clone().unwrap_or_default(),
        "genre" => track.tags.genre.as_ref().or(program.tags.genre.as_ref()).cloned().unwrap_or_default(),
        "year" | "date" => track.tags.year.as_ref().or(program.tags.year.as_ref()).cloned().unwrap_or_default(),
        "composer" => track.tags.composer.clone().unwrap_or_default(),
        "disc" => String::new(),
        _ => String::new(),
    }
}

/// Serialized pre-round-3 jobs can still be running or resuming. Keep their
/// brace templates on the old resolver; newly entered templates use Convert's
/// percent-token spelling and treat braces as ordinary filename characters.
fn legacy_naming_template(template: &str) -> bool {
    // A literal percentage in an old saved pattern ("100% - {track}") is
    // not a new-style token. Only a delimited token switches the resolver.
    let has_percent_token = template.split('%').skip(1).step_by(2).any(|part|
        !part.is_empty() && part.chars().all(|c|c.is_ascii_alphanumeric() || c=='_'));
    !has_percent_token && ["track", "chapter", "title", "stream", "area", "group",
        "playlist", "album", "artist", "albumartist", "performer", "genre", "year",
        "date", "composer", "disc"].iter().any(|token| template.contains(&format!("{{{token}}}")))
}

fn render_legacy_naming_template(template: &str, program: &NativeProgramChoice, track: &NativeTrackChoice) -> String {
    let mut result = String::new();
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '{' {
            let mut token = String::new();
            while let Some(&next) = chars.peek() {
                chars.next();
                if next == '}' { break; }
                if token.len() < 48 { token.push(next); }
            }
            result.push_str(&name_value(&token.to_ascii_lowercase(), program, track));
        } else { result.push(c); }
    }
    sanitize_component(&result)
}

/// Convert's filename tokens plus native disc identities. Missing evidence
/// yields an empty value, not an invented tag. The suffix is an explicit token
/// only when the user requests it; otherwise the publisher appends it once.
fn native_naming_value(token: &str, program: &NativeProgramChoice,
    track: &NativeTrackChoice, extension: &str, identity: &NativeDiscIdentity,
    album: &str, title_extra: &str) -> Option<String> {
    let id = program.id;
    let number = track.number;
    let value = match token {
        "TRACKNUMBER" | "TRACKNN" | "NN" => format!("{number:02}"),
        "TRACKN" | "TRACK" | "N" => number.to_string(),
        "NNN" => format!("{number:03}"),
        "TITLE" => name_value("title",program,track),
        "ARTIST" => track.tags.artist.clone().or_else(||program.tags.artist.clone())
            .or_else(||identity.artist.clone()).unwrap_or_default(),
        "ALBUM_ARTIST" => program.tags.artist.clone().or_else(||identity.artist.clone()).unwrap_or_default(),
        "ALBUM" => album.to_owned(),
        "COMPOSER" => name_value("composer",program,track),
        "PERFORMER" => name_value("performer",program,track),
        "YEAR" | "DATE" => name_value("year",program,track),
        "GENRE" => name_value("genre",program,track),
        "AREA" => name_value("area",program,track),
        "GROUP" => name_value("group",program,track),
        "DVDTITLE" => id.dvd_video_parts().map(|(_,title,_)|title.to_string()).unwrap_or_default(),
        "PLAYLIST" => name_value("playlist",program,track),
        "STREAM" => match id {
            PresentationId::DvdVideoTitle {audio_stream_index,..} => (u16::from(audio_stream_index)+1).to_string(),
            PresentationId::BluRayTitle {audio_stream_index,..} => (u16::from(audio_stream_index)+1).to_string(),
            PresentationId::MatroskaStream { audio_ordinal, .. } |
            PresentationId::TransportStream { audio_ordinal, .. } => audio_ordinal.to_string(),
            _ => program.label.clone(),
        },
        "ANGLE" => program.angle.to_string(),
        "EXTENSION" | "EXT" => extension.to_string(),
        "FORMAT" => extension.to_ascii_uppercase(),
        "SAMPLERATE" => program.source_rate.map(|rate|rate.to_string()).unwrap_or_default(),
        "BITDEPTH" => program.source_depth.map(|depth|depth.to_string()).unwrap_or_default(),
        "CATALOG" => identity.catalog.clone().unwrap_or_default(),
        "TITLE_EXTRA" => title_extra.to_owned(),
        // Convert's back-compatible %DISC% defaults to disc 1 for an ordinary
        // single-disc album. Native tags carry no separate disc-number field.
        "DISC" => "1".into(),
        "ISRC" | "LABEL" | "COUNTRY" | "PRESSING"
            | "DISCNUMBER" | "TRACKTOTAL" => String::new(),
        _ => return None,
    };
    Some(value)
}

/// The same vocabulary drives the amber invalid-token indicator in the form.
/// Do not make a second allowlist in the renderer or its event handler.
pub(crate) fn unknown_native_naming_token(template: &str) -> Option<String> {
    if legacy_naming_template(template) { return None; }
    let mut rest = template;
    while let Some(start) = rest.find('%') {
        rest = &rest[start+1..];
        let Some(end) = rest.find('%') else { return Some("unclosed % token".into()); };
        let token = &rest[..end];
        if !is_native_naming_token(token) {return Some(format!("%{token}%"));}
        rest = &rest[end+1..];
    }
    None
}

fn is_native_naming_token(token: &str) -> bool {
    matches!(token, "TRACKNUMBER" | "TRACKNN" | "TRACKN" | "TRACK" | "N" | "NN" | "NNN"
        | "TITLE" | "ARTIST" | "ALBUM_ARTIST" | "ALBUM" | "COMPOSER" | "PERFORMER"
        | "YEAR" | "DATE" | "GENRE" | "AREA" | "GROUP" | "DVDTITLE" | "PLAYLIST"
        | "STREAM" | "ANGLE" | "EXTENSION" | "EXT" | "FORMAT" | "SAMPLERATE"
        | "BITDEPTH" | "TITLE_EXTRA" | "ISRC" | "LABEL" | "COUNTRY" | "PRESSING"
        | "CATALOG" | "DISC" | "DISCNUMBER" | "TRACKTOTAL")
}

/// Split only *literal* template slashes into directories. Slashes inside tag
/// values are sanitized by component, so a malicious disc title cannot create
/// another level or escape the selected destination. Recovery and preview both
/// call the same output-path functions below.
fn render_naming_template_for_extension(template: &str, program: &NativeProgramChoice,
    track: &NativeTrackChoice, extension: &str, identity: &NativeDiscIdentity) -> String {
    if legacy_naming_template(template) {
        return render_legacy_naming_template(template,program,track);
    }

    // Convert derives TITLE_EXTRA from a release qualifier in ALBUM and
    // removes that qualifier from ALBUM only if the template references it.
    let raw_album = track.tags.album.as_deref().or(program.tags.album.as_deref())
        .or(identity.title.as_deref()).unwrap_or_default();
    let (album, title_extra) = if template.contains("%TITLE_EXTRA%") {
        match super::stages::extract_title_extra(raw_album) {
            Some((clean, extra)) => {
                let strip_resolution = template.contains("%BITDEPTH%")
                    || template.contains("%SAMPLERATE%");
                (clean, super::stages::normalize_title_extra_value(&extra, true, strip_resolution))
            }
            None => (raw_album.to_owned(), String::new()),
        }
    } else { (raw_album.to_owned(), String::new()) };

    // Run Convert's existing conditional-block pass on the *template*, not
    // on substituted metadata: literal braces inside a track/album title are
    // data, never control syntax. The renderer still sanitizes each value
    // before it is joined to the user's chosen destination.
    let mut tokens = BTreeMap::new();
    for token in template.split('%').skip(1).step_by(2) {
        if token.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            let value = native_naming_value(token,program,track,extension,identity,&album,&title_extra)
                .unwrap_or_default();
            // Keep Convert's metadata naming rules for shared presets. Empty
            // values stay empty so the existing conditional pass can drop
            // missing qualifiers rather than rendering "untitled".
            tokens.insert(token.to_owned(), if value.trim().is_empty() {
                String::new()
            } else if token == "TITLE_EXTRA" {
                super::stages::sanitize_title_extra_component(&value)
            } else {
                super::stages::sanitize_component(&value)
            });
        }
    }
    let conditional = super::stages::render_conditional_template_blocks(
        template, &tokens, None, &BTreeMap::new());
    conditional.split('/').filter(|part|!part.is_empty()).map(|part| {
        let mut output = String::new();
        let mut rest = part;
        while let Some(start) = rest.find('%') {
            output.push_str(&rest[..start]);
            rest = &rest[start+1..];
            let Some(end) = rest.find('%') else {
                output.push('%'); output.push_str(rest); rest=""; break;
            };
            // Use the same normalized value as the conditional pass; only
            // literal template separators may create subfolders.
            if let Some(value) = tokens.get(&rest[..end]) {
                output.push_str(value);
            }
            rest = &rest[end+1..];
        }
        output.push_str(rest);
        sanitize_component(&output)
    }).collect::<Vec<_>>().join("/")
}

fn with_native_extension(stem: String, extension: &str) -> String {
    if stem.to_ascii_lowercase().ends_with(&format!(".{extension}")) {stem}
    else {format!("{stem}.{extension}")}
}

fn prefix_flat_native_name(program_label: &str, stem: String) -> String {
    let prefix = sanitize_component(program_label);
    // Prefix the actual filename, not the first template subdirectory. This
    // retains the per-program distinction without changing the requested
    // %ARTIST%/… directory hierarchy when implicit program folders are off.
    match stem.rsplit_once('/') {
        Some((folder, filename)) => format!("{folder}/{prefix} - {filename}"),
        None => format!("{prefix} - {stem}"),
    }
}

#[cfg(test)]
fn render_naming_template(template: &str, program: &NativeProgramChoice, track: &NativeTrackChoice) -> String {
    render_naming_template_for_extension(template,program,track,"dsf",&NativeDiscIdentity::default())
}

pub(crate) fn proposed_destination(job: &NativeExtractionJob, program: &NativeProgramChoice, track: &NativeTrackChoice) -> PathBuf {
    if job.format == DiscFormat::Archive {
        return archive::proposed_archive_destination(job,program,track);
    }
    let extension = if job.format == DiscFormat::CueImage {
        job.cue_segments.iter().find(|s| s.presentation == program.id && s.number == track.number)
            .map(|s| s.extension.as_str()).unwrap_or("invalid")
    } else { extraction_extension(job, program) };
    let legacy = legacy_naming_template(&job.naming_template);
    let mut stem = render_naming_template_for_extension(&job.naming_template, program, track,
        extension, &job.disc_identity);
    if prefix_implicit_program_name(job) {
        // The default can be reused across unrelated authored streams. Prefix
        // the stream identity when flattening, without guessing unique names.
        stem = if legacy {sanitize_component(&format!("{} - {stem}",program.label))}
            else {prefix_flat_native_name(&program.label,stem)};
    }
    let file_name = if legacy {format!("{stem}.{extension}")}
        else {with_native_extension(stem, extension)};
    let parent = if job.program_subfolders { job.destination.join(sanitize_component(&program.label)) }
        else { job.destination.clone() };
    parent.join(file_name)
}

/// A skipped audio file must never leave its companion to be overwritten or
/// silently omitted. Reject companion-only collisions before processing audio.
fn inspect_companion_target(job:&NativeExtractionJob,planned:&mut HashSet<String>,
    audio:&Path,companion:PathBuf) -> Result<(),String> {
    if job.overwrite_policy==NativeOverwritePolicy::Skip &&
        fs::symlink_metadata(&companion).is_ok() &&
        fs::symlink_metadata(audio).is_err() {
        return Err(format!("Companion already exists: '{}'; choose Replace, disable its option, or change the destination",
            companion.display()));
    }
    inspect_publication_target(job,planned,companion)
}

pub(crate) fn validate_job(job: &NativeExtractionJob) -> Result<(), String> {
    if job.programs.is_empty() || job.programs.iter().any(|p| p.tracks.is_empty()) {
        return Err("Check at least one track or chapter".into());
    }
    if job.source.as_os_str().is_empty() || job.destination.as_os_str().is_empty() {
        return Err("Source and destination must be specified".into());
    }
    if !job.destination.is_absolute() || !job.working_root.is_absolute() {
        return Err("Destination and working storage must be absolute paths".into());
    }
    if !job.source.exists() { return Err("Disc source no longer exists".into()); }
    if job.cue && !job.file_per_program { return Err("CUE belongs to continuous-file mode".into()); }
    if job.format == DiscFormat::CueImage { cue::validate_cue_job(job)?; }
    if job.format == DiscFormat::Archive { archive::validate_archive_job(job)?; }
    if is_file_stream_format(job.format) {
        if job.format == DiscFormat::TransportFile {
            if !crate::disc::on_disc_transport::is_transport_path(&job.source)
                || crate::disc::on_disc_transport::has_disc_location(&job.source)
                || !job.source.is_file() {
                return Err("Standalone extraction requires an orphan M2TS/VOB file, not an authored disc segment".into());
            }
            if !job.file_per_program || job.cue {
                return Err("Standalone transport files expose only whole audio streams, without chapters or CUE".into());
            }
            for program in &job.programs {
                if !matches!(program.id, PresentationId::TransportStream { .. })
                    || program.tracks.len() != 1 || program.tracks[0].number != 1 {
                    return Err("Standalone transport selection must contain one complete audio stream".into());
                }
            }
        } else if !crate::disc::matroska_utils::is_matroska_path(&job.source) || !job.source.is_file() {
            return Err("Matroska extraction needs an existing MKV/MKA file".into());
        }
        if job.format == DiscFormat::Matroska && job.cue && !job.programs.iter().any(|program|
            matroska_cue_for_program(job, program)) {
            return Err("CUE is available only for chaptered native streams without embedded chapters".into());
        }
        if job.downmix.is_active() || job.assume_decrypted || job.sacd_dff ||
            job.programs.iter().any(|program| program.keep_dst) {
            return Err("Disc-only extraction controls cannot be applied to file streams".into());
        }
        for program in &job.programs {
            if job.format == DiscFormat::TransportFile { continue; }
            let PresentationId::MatroskaStream { chapter_count, .. } = program.id else {
                return Err("Matroska job contains a non-Matroska stream identity".into());
            };
            if job.file_per_program && program.tracks.len() != chapter_count.max(1) as usize {
                return Err("Whole-stream copy requires all chapters to be selected; choose Split chapters".into());
            }
            if chapter_count == 0 && (program.tracks.len() != 1 || program.tracks[0].number != 1) {
                return Err("Chapterless Matroska stream requires its one whole-stream selection".into());
            }
            if program.tracks.iter().any(|track| track.number > chapter_count.max(1)) {
                return Err("Matroska chapter selection is outside the probed chapter table".into());
            }
        }
    }
    if job.cue && job.format == DiscFormat::Sacd {
        return Err("CUE FILE WAVE indexes do not describe DSF/DFF containers; disable CUE to extract continuous native DSD".into());
    }
    // Copy protection remains the responsibility of the format-specific
    // materializer; a UI assumption does not disable its sector checks.
    let mut planned = HashSet::new();
    for program in &job.programs {
        let mut nums = BTreeSet::new();
        for track in &program.tracks {
            if track.number == 0 || !nums.insert(track.number) {
                return Err("Track selection contains duplicate or zero track numbers".into());
            }
            if !job.file_per_program {
                let media=proposed_destination(job,program,track);
                inspect_publication_target(job,&mut planned,media.clone())?;
                if native_program_cue_applies(job,program) &&
                    (job.format==DiscFormat::Sacd || is_file_stream_format(job.format)) {
                    inspect_companion_target(job,&mut planned,&media,media.with_extension("cue"))?;
                }
            }
        }
        if let Some(hidden) = native_cue_hidden_target(job,program) {
            inspect_publication_target(job,&mut planned,hidden)?;
        }
        if job.file_per_program {
            let media = proposed_destination_for_program(job, program);
            inspect_publication_target(job, &mut planned, media.clone())?;
            if native_program_cue_applies(job,program) {
                inspect_companion_target(job,&mut planned,&media,media.with_extension("cue"))?;
            }
            if native_program_chapters_apply(job,program) {
                inspect_companion_target(job,&mut planned,&media,native_chapters_target(job,&media))?;
            }
        }
        if (program.keep_dst || job.sacd_dff) && job.format != DiscFormat::Sacd {
            return Err("DST passthrough is available only for SACD".into());
        }
    }
    if job.extraction_log {
        if !job.program_log_paths.is_empty() && job.program_log_paths.len() != job.programs.len() {
            return Err("Extraction log plan does not match the selected programs".into());
        }
        for (index, path) in native_log_targets(job).iter().enumerate() {
            // Each new log must belong to its program's actual output root.
            if !job.program_log_paths.is_empty() {
                let expected = native_program_output_root(job, &job.programs[index]);
                if path.parent() != Some(expected.as_path()) ||
                    !path.file_name().and_then(|name|name.to_str()).is_some_and(|name|
                        name == EXTRACTION_LOG_FILE ||
                        (name.starts_with("extraction-") && name.ends_with(".log") &&
                         name[11..name.len()-4].parse::<u32>().is_ok_and(|n|n>=2))) {
                    return Err(format!("Invalid extraction log destination: '{}'",path.display()));
                }
            }
            if !planned.insert(native_destination_collision_key(path)) {
                return Err("Extraction log conflicts with another selected output".into());
            }
            match fs::symlink_metadata(path) {
                Ok(_) if job.overwrite_policy != NativeOverwritePolicy::Replace =>
                    return Err(format!("Extraction log was created after preflight: '{}'",path.display())),
                Ok(_) => {},
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
                Err(error) => return Err(format!("Cannot inspect extraction log '{}': {error}",path.display())),
            }
        }
    }
    Ok(())
}

/// A conservative key for all planned native publications. Case-insensitive
/// filesystems treat differently capitalized components as one file, even when
/// the host's default filesystem is case-sensitive. Keep the display/published
/// PathBuf unchanged; only the reserved namespace uses this key.
pub(crate) fn native_destination_collision_key(path: &Path) -> String {
    path.to_string_lossy().to_lowercase()
}

fn inspect_publication_target(job: &NativeExtractionJob, planned: &mut HashSet<String>, dest: PathBuf) -> Result<(), String> {
    // Any file-backed input is itself a possible output pathname, including
    // through a symlink. Even explicit Replace must not overwrite the source.
    if (is_file_stream_format(job.format) || job.format == DiscFormat::Archive) &&
        fs::canonicalize(&job.source).ok().is_some_and(|source|
            fs::canonicalize(&dest).ok().as_ref() == Some(&source)) {
        return Err(format!("File-stream output would replace its own input: {}", dest.display()));
    }
    if !planned.insert(native_destination_collision_key(&dest)) {
        return Err(format!("Two selected artifacts would publish to '{}'; choose different names",dest.display()));
    }
    match fs::symlink_metadata(&dest) {
        Ok(_) if job.overwrite_policy == NativeOverwritePolicy::Fail =>
            Err(format!("Destination item already exists: '{}'",dest.display())),
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("Cannot inspect destination item '{}': {e}",dest.display())),
    }
}

/// Native extraction has no DSP contract; this conversion-options projection
/// supplies a typed compatibility request for existing materializer/queue
/// interfaces, not a silently invented PipelineSettings default.
pub(crate) fn native_compatibility_settings() -> Result<tonepoet_pipeline::PipelineSettings, String> {
    let options = crate::convert::formats::ConversionOptions::default();
    super::unified_request::pipeline_settings_from_legacy_options(&options)
        .map_err(|error| format!("Native compatibility settings: {error}"))
}

fn materializer_request(job: &NativeExtractionJob, program: &NativeProgramChoice, index: usize) -> Result<PipelineRequest, String> {
    let mut source = SourceOptions {
        archive_password: None, sacd_area: None, dvda_group: None,
        dvda_group_selection: DvdaGroupSelection::Default,
        dvda_assume_decrypted: job.assume_decrypted,
        dvda_downmix_policy: job.downmix,
        dvdv_vts: None, dvdv_title: None, dvdv_audio_stream: None, dvdv_angle: None,
        bluray_playlist: None, bluray_audio_pid: None, bluray_audio_stream: None, bluray_angle: None,
        sidecar_cue_track_metadata: None, cue_sidecar: CueSidecarPolicy::IgnoreCue,
        track_selection: TrackSelection::All,
    };
    crate::tui::disc_browser::apply_presentation_to_source_options(&mut source, &program.id);
    if program.angle != 0 {
        match program.id {
            PresentationId::DvdVideoTitle { .. } => source.dvdv_angle = Some(program.angle),
            PresentationId::BluRayTitle { .. } => source.bluray_angle = Some(program.angle),
            _ => {},
        }
    }
    Ok(PipelineRequest {
        job_id: format!("native-extract-{}", std::process::id()), item_id: format!("program-{index}"),
        submission_id: None, submission_size: None,
        container: job.source.clone(), source,
        settings: native_compatibility_settings()?,
        registered_effects: Vec::new(),
        deemphasis_choice_origin: Default::default(), deemphasis_evidence_origin: Default::default(),
        worker_count: Some(1), scratch_staging: None, merge: false,
        output_root: job.destination.clone(),
        naming: NamingPolicy { template: "%NN% - %TITLE%".into(), folder_template: None,
            per_album_subdir: false, collision_policy: NamingCollisionPolicy::Fail, windows_portable: false },
        publish: PublishPolicy { overwrite: OverwritePolicy::FailIfExists,
            same_filesystem_required: false, write_manifest: false },
        log: LogPolicy { root: job.working_root.clone(), write_for_blocked: false,
            write_json_log: false, write_conversion_log: false },
        stages: StagePolicy { metadata: StageRequirement::Disabled,
            replaygain: StageRequirement::Disabled, features: StageRequirement::Disabled, generate_cue: false },
        failure_policy: FailurePolicy::FailAlbumOnAnyTrackFailure,
        album_batch: None, album_batch_track: None, pre_extracted_staging: None,
        archive_metadata_overrides: Vec::new(), metadata_overrides: Default::default(),
        batch_resolved_identity: None, suppress_incremental_conversion_log_append: false,
        expected_album_track_count: None, container_extension: None, container_ffmpeg_flags: Vec::new(),
        companion: CompanionCopyPolicy::default(), actions: ActionPipeline::default(),
    })
}

fn extract_sacd_native(src: &TrackSourceRef, output: &Path, dff: bool, keep_dst: bool,
    verify_after: bool, cancel: &CancellationToken, item_cancel: &CancellationToken,
    controls: &NativeExtractionControl) -> Result<(), String> {
    let TrackSourceRef::SacdTrack { iso, track_index, area } = src else { return Err("Not a SACD track".into()); };
    let metadata = crate::tui::sacd::parse_sacd_iso(iso).map_err(|e| e.to_string())?;
    let area_info = match area {
        SacdArea::Stereo => metadata.stereo.as_ref(),
        SacdArea::MultiChannel => metadata.multi_channel.as_ref(),
    }.ok_or("Selected SACD area disappeared")?;
    if !matches!(area_info.header.channel_count, 2 | 5 | 6) { return Err("Unsupported SACD channel count".into()); }
    let format = if keep_dst { sacd_rs::extract::OutputFormat::DffDst }
        else if dff { sacd_rs::extract::OutputFormat::Dff }
        else { sacd_rs::extract::OutputFormat::Dsf };
    let options = area_info.track_extract_options(*track_index as usize, format)
        .map_err(|e| e.to_string())?;
    let mut reader = sacd_rs::iso_reader::IsoReader::open(iso).map_err(|e| e.to_string())?;
    let mut file = OpenOptions::new().write(true).create_new(true).open(output).map_err(|e| e.to_string())?;
    let mut hash=verify_after.then(Sha256::new);
    let report = if verify_after {
        let mut tap=|bytes:&[u8]| {
            if let Some(digest)=hash.as_mut() {digest.update(bytes);}
        };
        sacd_rs::extract::extract_track_with_area_frame_format_and_cancel_tapped(
            &mut reader, &mut file, options, area_info.extraction_frame_format(),
            sacd_rs::extract::ExtractIntegrityOptions::strict(),
            ||controls.wait_blocking(cancel,item_cancel), &mut tap,
        )
    } else {
        sacd_rs::extract::extract_track_with_area_frame_format_and_cancel(
            &mut reader, &mut file, options, area_info.extraction_frame_format(),
            sacd_rs::extract::ExtractIntegrityOptions::strict(),
            ||controls.wait_blocking(cancel,item_cancel),
        )
    }.map_err(|e|format!("SACD extraction: {e}"))?;
    if report.integrity_loss_detected() { return Err("SACD extractor reported loss of integrity".into()); }
    file.sync_all().map_err(|e| e.to_string())?;
    if let Some(hash)=hash {controls.record_dsd_fingerprint(output,hash.finalize().into());}
    Ok(())
}

/// Join completed native SACD tracks, without passing DSD through PCM or
/// concatenating DSF/DFF container bytes. The source readers normalize DSF's
/// block ordering; the writer re-blocks the uninterrupted DSD bitstream once.
/// DSDIFF/DST uses the original encoded frames, re-emitting valid FRTE/DSTI
/// container structures and DSTC checksums for the combined frame sequence.
#[cfg_attr(not(test), allow(dead_code))] // Compatibility entrypoint exercised by focused tests.
fn concatenate_sacd_tracks(
    parts: &[PathBuf], output: &Path, keep_dst: bool, cancel: &CancellationToken,
) -> Result<(Vec<u64>, u32), String> {
    concatenate_sacd_tracks_checked(parts, output, keep_dst, keep_dst, cancel, || Ok(()))
}

fn concatenate_sacd_tracks_checked(
    parts: &[PathBuf], output: &Path, dff: bool, keep_dst: bool, cancel: &CancellationToken,
    checkpoint: impl FnMut() -> Result<(), String>,
) -> Result<(Vec<u64>, u32), String> {
    concatenate_sacd_tracks_checked_tapped(parts,output,dff,keep_dst,cancel,checkpoint,None)
}

fn concatenate_sacd_tracks_checked_tapped(
    parts: &[PathBuf], output: &Path, dff: bool, keep_dst: bool, cancel: &CancellationToken,
    mut checkpoint: impl FnMut() -> Result<(), String>,
    mut on_audio: Option<&mut dyn FnMut(&[u8])>,
) -> Result<(Vec<u64>, u32), String> {
    use sacd_rs::dsd_file::{DsdFrameReader, DsfStreamReader, DsdDsdiffStreamReader, DstFrameReader, DstDsdiffStreamReader};
    if parts.is_empty() { return Err("Continuous SACD selection is empty".into()); }
    let mut samples = Vec::with_capacity(parts.len());
    if keep_dst {
        let first = DstDsdiffStreamReader::new(File::open(&parts[0]).map_err(|e|e.to_string())?)
            .map_err(|e| format!("Read DST track: {e}"))?;
        let channels = u8::try_from(first.info().channel_count)
            .map_err(|_| "Invalid SACD channel count")?;
        let rate = first.info().sample_rate;
        let dst_rate = sacd_rs::dst::DstRate::from_sample_rate(rate).map_err(|e| e.to_string())?;
        let file = OpenOptions::new().create_new(true).write(true).read(true).open(output)
            .map_err(|e|format!("Create private continuous DFF: {e}"))?;
        let mut writer = sacd_rs::dff_dst_writer::DffDstWriter::new(file, channels, rate)
            .map_err(|e|format!("Create continuous DFF writer: {e}"))?;
        for (index, part) in parts.iter().enumerate() {
            checkpoint()?;
            if cancel.is_cancelled() { return Err("SACD extraction cancelled".into()); }
            let mut reader = first_part_dst_reader(part)?;
            if reader.info().channel_count != u16::from(channels) || reader.info().sample_rate != rate {
                return Err(format!("SACD track {} has a different channel count or DSD clock", index + 1));
            }
            let mut track_samples = 0u64;
            while let Some(frame) = reader.next_dst_frame().map_err(|e|format!("Read DST frame: {e}"))? {
                checkpoint()?;
            if cancel.is_cancelled() { return Err("SACD extraction cancelled".into()); }
                let decoded = sacd_rs::dst::decode_frame_with_rate(&frame.encoded, channels, dst_rate)
                    .map_err(|e|format!("Verify source DST frame: {e}"))?;
                if frame.dstc.is_some_and(|crc| crc != sacd_rs::dff_dst_writer::dst_frame_crc(&decoded)) {
                    return Err(format!("SACD track {} has a DST checksum mismatch", index + 1));
                }
                writer.write_passthrough_frame(&frame.encoded, &decoded)
                    .map_err(|e|format!("Join source DST frame: {e}"))?;
                if let Some(tap)=on_audio.as_mut() { (*tap)(&decoded); }
                track_samples = track_samples.checked_add((decoded.len() / usize::from(channels)) as u64 * 8)
                    .ok_or("DST sample-count overflow")?;
            }
            samples.push(track_samples);
        }
        writer.finish().map_err(|e|format!("Finish continuous DFF: {e}"))?;
        File::open(output).and_then(|f| f.sync_all()).map_err(|e|e.to_string())?;
        Ok((samples, rate))
    } else if dff {
        // Plain DSDIFF/DSD, including decoded DST. Reopen each completed
        // track, preserve the channel-interleaved DSD payload and emit one
        // valid continuous DFF header; never concatenate container headers.
        let first = DsdDsdiffStreamReader::new(File::open(&parts[0]).map_err(|e|e.to_string())?)
            .map_err(|e|format!("Read DFF track: {e}"))?;
        let channels = u8::try_from(first.info().channel_count)
            .map_err(|_| "Invalid SACD channel count")?;
        let rate = first.info().sample_rate;
        let file = OpenOptions::new().create_new(true).write(true).read(true).open(output)
            .map_err(|e|format!("Create private continuous DFF: {e}"))?;
        let mut writer = sacd_rs::dff_writer::DffWriter::new(file, channels, rate)
            .map_err(|e|format!("Create continuous DFF writer: {e}"))?;
        for (index, part) in parts.iter().enumerate() {
            checkpoint()?;
            if cancel.is_cancelled() { return Err("SACD extraction cancelled".into()); }
            let mut reader = DsdDsdiffStreamReader::new(File::open(part).map_err(|e|e.to_string())?)
                .map_err(|e|format!("Read DFF track: {e}"))?;
            if reader.info().channel_count != u16::from(channels) || reader.info().sample_rate != rate {
                return Err(format!("SACD track {} has a different channel count or DSD clock",index+1));
            }
            let mut track_samples = 0u64;
            while let Some(frame) = reader.next_dsd_frame().map_err(|e|format!("Read DSDIFF data: {e}"))? {
                checkpoint()?;
                if cancel.is_cancelled() { return Err("SACD extraction cancelled".into()); }
                writer.write_frame(&frame.data).map_err(|e|format!("Join DFF data: {e}"))?;
                if let Some(tap)=on_audio.as_mut() { (*tap)(&frame.data); }
                track_samples = track_samples.checked_add((frame.data.len() / usize::from(channels)) as u64 * 8)
                    .ok_or("DSD sample-count overflow")?;
            }
            samples.push(track_samples);
        }
        writer.finish().map_err(|e|format!("Finish continuous DFF: {e}"))?;
        File::open(output).and_then(|f| f.sync_all()).map_err(|e|e.to_string())?;
        Ok((samples, rate))
    } else {
        let first = DsfStreamReader::new(File::open(&parts[0]).map_err(|e|e.to_string())?)
            .map_err(|e|format!("Read DSF track: {e}"))?;
        let channels = u8::try_from(first.info().channel_count)
            .map_err(|_| "Invalid SACD channel count")?;
        let rate = first.info().sample_rate;
        let file = OpenOptions::new().create_new(true).write(true).read(true).open(output)
            .map_err(|e|format!("Create private continuous DSF: {e}"))?;
        let mut writer = sacd_rs::dsf_writer::DsfWriter::new(file, channels, rate)
            .map_err(|e|format!("Create continuous DSF writer: {e}"))?;
        for (index, part) in parts.iter().enumerate() {
            checkpoint()?;
            if cancel.is_cancelled() { return Err("SACD extraction cancelled".into()); }
            let mut reader = DsfStreamReader::new(File::open(part).map_err(|e|e.to_string())?)
                .map_err(|e|format!("Read DSF track: {e}"))?;
            if reader.info().channel_count != u16::from(channels) || reader.info().sample_rate != rate {
                return Err(format!("SACD track {} has a different channel count or DSD clock",index+1));
            }
            let mut track_samples = 0u64;
            while let Some(frame) = reader.next_dsd_frame().map_err(|e|format!("Read DSD data: {e}"))? {
                checkpoint()?;
            if cancel.is_cancelled() { return Err("SACD extraction cancelled".into()); }
                writer.write_interleaved(&frame.data).map_err(|e|format!("Join DSD data: {e}"))?;
                if let Some(tap)=on_audio.as_mut() { (*tap)(&frame.data); }
                track_samples = track_samples.checked_add((frame.data.len() / usize::from(channels)) as u64 * 8)
                    .ok_or("DSD sample-count overflow")?;
            }
            samples.push(track_samples);
        }
        writer.finish().map_err(|e|format!("Finish continuous DSF: {e}"))?;
        File::open(output).and_then(|f| f.sync_all()).map_err(|e|e.to_string())?;
        Ok((samples,rate))
    }
}

/// Reopen each completed track independently so no DST reader (and its frame
/// index) outlives the one track it validates.
fn first_part_dst_reader(path: &Path) -> Result<sacd_rs::dsd_file::DstDsdiffStreamReader<File>, String> {
    sacd_rs::dsd_file::DstDsdiffStreamReader::new(File::open(path).map_err(|e|e.to_string())?)
        .map_err(|e|format!("Read completed DFF track: {e}"))
}

/// Map only explicit, recognized speaker layouts to WAVEFORMATEXTENSIBLE
/// masks. A zero source mask is not evidence of any particular 5.1/7.1
/// variant. Count-derived labels must never silently invent speaker order.
fn verified_layout_mask(source: &str, channels: u16) -> Option<u32> {
    let (count, mask) = match source.trim().to_ascii_lowercase().as_str() {
        "mono" | "fc" => (1, 0x4),
        "stereo" | "fl+fr" => (2, 0x3),
        "5.1(back)" | "fl+fr+fc+lfe+bl+br" => (6, 0x3f),
        "5.1(side)" | "fl+fr+fc+lfe+sl+sr" => (6, 0x60f),
        "7.1(side)" | "fl+fr+fc+lfe+bl+br+sl+sr" => (8, 0x63f),
        "7.1(wide)" | "fl+fr+fc+lfe+bl+br+flc+frc" => (8, 0xff),
        _ => return None,
    };
    (channels == count).then_some(mask)
}

/// Decode a realized PCM carrier to raw integer samples, without changing its
/// sample clock or channel count. The private raw file is then wrapped by the
/// bounded native writer, which preserves 20 valid bits in 24 and supports
/// RIFF/RF64/W64 without a size-limited intermediate final container.
async fn encode_pcm_container(
    runner: &RealToolRunner, carrier: &Path, output: &Path,
    output_depth: u32, valid_bits: u32, container: NativeWavContainer,
    declared_rate: Option<u32>, declared_channels: Option<u8>,
    source_layout: Option<&str>, cancel: &CancellationToken,
    job_cancel: &CancellationToken, controls: Arc<NativeExtractionControl>,
    estimated_bytes: Option<u64>, verify_after: bool,
) -> Result<(), String> {
    if valid_bits == 0 || valid_bits > output_depth { return Err("Invalid PCM valid-bit depth".into()); }
    let observed = inspect_carrier(carrier)?;
    if declared_rate.is_some_and(|expected| expected != observed.rate) {
        return Err(format!("PCM carrier sample rate {} differs from source rate {:?}; refusing resampled extraction",
            observed.rate, declared_rate));
    }
    if declared_channels.is_some_and(|expected| u16::from(expected) != observed.channels) {
        return Err("PCM carrier channel count differs from source; refusing implicit downmix".into());
    }
    let format = NativePcmFormat {
        rate: observed.rate, channels: observed.channels,
        container_bits: output_depth as u16, valid_bits: valid_bits as u16,
        channel_mask: observed.mask.filter(|value| *value != 0)
            .or_else(|| source_layout.and_then(|layout| verified_layout_mask(layout, observed.channels)))
            .unwrap_or(match observed.channels { 1 => 4, 2 => 3, _ => 0 }),
    };
    format.block_align()?;
    let raw = output.with_extension("raw-pcm-in-working-storage");
    let sample = if output_depth == 16 { "s16le" } else { "s24le" };
    let codec = if output_depth == 16 { "pcm_s16le" } else { "pcm_s24le" };
    let args = ["-hide_banner", "-nostdin", "-loglevel", "error", "-xerror", "-bitexact", "-y",
        "-i", carrier.to_str().ok_or("Non-UTF8 carrier path")?,
        "-map", "0:a:0", "-vn", "-sn", "-dn", "-c:a", codec,
        "-dither_method", "none", "-f", sample,
        raw.to_str().ok_or("Non-UTF8 working file path")?]
        .into_iter().map(str::to_string).collect::<Vec<_>>();
    let command = ToolCommand {
        binary: ToolBinary::Ffmpeg, args, secret_args: Vec::new(), cwd: None,
        environment_policy: tonepoet_pipeline::CommandEnvironmentPolicy::InheritAndSet,
        env: Vec::new(), timeout: Duration::from_secs(6 * 60 * 60),
    };
    controls.watch_file_range(raw.clone(), "Decoding native PCM".to_owned(), estimated_bytes, 30, 65);
    runner.run(command, cancel).await.map_err(|e| format!("Native PCM realization: {e}"))?;
    controls.wait_ready(job_cancel, cancel).await?;
    controls.watch_file_range(output.to_path_buf(), "Packaging native PCM".to_owned(), estimated_bytes, 65, 85);
    let raw_copy = raw.clone();
    let out_copy = output.to_path_buf();
    let cancel_copy = cancel.clone();
    let job_copy = job_cancel.clone();
    tokio::task::spawn_blocking(move || {
        let mut digest=verify_after.then(Sha256::new);
        let mut tap=|bytes:&[u8]| {
            if let Some(hash)=digest.as_mut() {hash.update(bytes);}
        };
        super::native_wav::package_raw_pcm_checked_with_tap(
            &raw_copy, &out_copy, format, container, &cancel_copy,
            || if controls.wait_blocking(&job_copy, &cancel_copy) {
                Err("PCM packaging stopped".to_owned())
            } else { Ok(()) },
            if verify_after {Some(&mut tap)}else{None},
        )?;
        if let Some(hash)=digest {
            let codec=if output_depth==16 {"pcm_s16le"}else{"pcm_s24le"};
            controls.record_audio_fingerprint(&out_copy,hash.finalize().into(),codec);
        }
        Ok::<(),String>(())
    })
        .await.map_err(|e| format!("PCM packaging worker: {e}"))??;
    fs::remove_file(&raw).map_err(|e| format!("Cannot remove private PCM staging: {e}"))?;
    Ok(())
}

/// A component is never a path: normalize separators, dots, control chars,
/// drive syntax and parent markers before combining it with the selected root.
fn sanitize_component(value: &str) -> String {
    let safe: String = value.chars().map(|c|
        if c.is_control() || matches!(c, '/' | '\\' | ':' | '<' | '>' | '|' | '"' | '?' | '*') { '_' } else { c }
    ).collect();
    let safe = safe.trim().trim_matches('.').trim();
    if safe.is_empty() || safe == ".." { "Untitled".into() }
    else { safe.chars().take(160).collect() }
}

/// Canonicalize the existing prefix of a not-yet-created destination. Any
/// uncreated suffix must consist of ordinary directory/file names; this
/// prevents `..` from bypassing the staging-outside-destination check.
fn resolved_future_path(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() { return Err("Destination must be an absolute path".into()); }
    let ancestor = nearest_existing_ancestor(path)?;
    let mut result = fs::canonicalize(ancestor)
        .map_err(|e| format!("Cannot resolve destination parent: {e}"))?;
    for component in path.strip_prefix(ancestor).map_err(|e|e.to_string())?.components() {
        match component {
            std::path::Component::Normal(name) => result.push(name),
            _ => return Err("Destination path includes a non-normalized component".into()),
        }
    }
    Ok(result)
}

/// The default is a dedicated TonePoet cache directory, never an implicit
/// destination sibling. The system temporary directory is only a last resort
/// on platforms that expose neither an application cache nor a data directory.
pub(crate) fn default_native_working_root() -> PathBuf {
    dirs::cache_dir().or_else(dirs::data_local_dir)
        .unwrap_or_else(std::env::temp_dir)
        .join("tonepoet").join("native-extraction")
}

/// Validate a job's working root before creating its per-job TempDir. Historical
/// R5 queued jobs used this implicit sibling path: migrate those to the cache
/// instead of perpetuating that old default on resume.
pub(crate) fn native_working_storage(configured: &Path, destination: &Path) -> Result<PathBuf, String> {
    let dest_path = resolved_future_path(destination)?;
    let obsolete_default = configured.file_name().is_some_and(|name| name == ".tonepoet-native-extraction-work")
        && configured.parent().is_some_and(|parent|
            resolved_future_path(parent).is_ok_and(|parent| dest_path.starts_with(parent)));
    let configured = if obsolete_default { default_native_working_root() }
        else { configured.to_path_buf() };
    let configured_path = resolved_future_path(&configured)?;
    if configured_path.starts_with(&dest_path) {
        return Err("Working storage is within the destination. Choose working storage outside the destination before extracting.".into());
    }
    Ok(configured)
}

fn nearest_existing_ancestor(path: &Path) -> Result<&Path, String> {
    let mut current = path;
    loop {
        if current.exists() { return Ok(current); }
        current = current.parent().ok_or("Destination has no existing ancestor")?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn fixture_job(destination: &Path, scratch: &Path) -> NativeExtractionJob {
        NativeExtractionJob { cue_input_paths: Vec::new(), cue_segments: Vec::new(), archive_members:Vec::new(), archive_length:0, archive_modified:None, archive_password:None, disc_identity: Default::default(),
            source: std::env::current_exe().unwrap(),
            format: DiscFormat::Sacd,
            programs: vec![NativeProgramChoice {
                id: PresentationId::SacdArea(crate::disc::SacdAreaId::Stereo),
                angle: 1, label: "Stereo".into(),
                tracks: vec![NativeTrackChoice { duration_secs: None, number: 1, name: "one".into(),
                    tags: NativeTagValues::default(), estimated_bytes: None }],
                keep_dst: false, source_depth: Some(1), source_codec: "unknown".into(), source_channels: Some(2),
                source_rate: Some(2_822_400), source_layout: None, is_lossless: true,
                tags: NativeTagValues::default(), authored_tags: None,
            }],
            destination: destination.to_path_buf(), working_root: scratch.to_path_buf(),
            wav_container: NativeWavContainer::Automatic, assume_decrypted: false,
            downmix: DvdaDownmixPolicy::None, naming_template: default_naming_template(),
            program_subfolders: true, overwrite_policy: NativeOverwritePolicy::Fail,
            extraction_log: false, options: NativeExtractSettings {create_cue:false, write_chapters:false, ..Default::default()}, program_log_paths: Vec::new(), round4_layout: false, file_per_program: false, cue: false,
            transfer_tags: false, sacd_dff: false, tag_source: default_tag_source(),
            tag_fields: default_tag_fields(), tag_artwork: false,
        }
    }

    #[test]
    fn r2_cue_gap_choices_preserve_or_discard_exact_sample_intervals() {
        let album=PresentationId::CueAlbum(1);
        let image=PathBuf::from("image.flac");
        let marks=[(1u32,100u64,None,500u64),
            (2,500,Some(450),900),(3,900,Some(880),1200)];
        let all=marks.into_iter().map(|(number,index01,index00,end)|NativeCueSegment {
            presentation:album,number,cue_path:PathBuf::from("album.cue"),
            image:image.clone(),start_sample:index01,end_sample:end,
            index00_sample:index00,index01_sample:index01,pre_emphasis:false,
            hidden_track_published:false,appended_gap:None,total_samples:1200,sample_rate:48000,extension:"flac".into(),
            encoder:"flac".into(),muxer:"flac".into(),sample_bits:Some(16),
            channels:2,source_length:0,source_modified:None,cue_length:0,
            cue_modified:None,source_bit_rate:None,packet_cut_samples:Vec::new(),
            packet_part_index:0,
        }).collect::<Vec<_>>();
        let mut selected=all.clone();
        let default=NativeExtractSettings::default();
        configure_cue_gap_segments(&all,&mut selected,&default).unwrap();
        assert_eq!(selected.iter().map(|s|(s.start_sample,s.end_sample))
            .collect::<Vec<_>>(),vec![(100,500),(500,900),(900,1200)]);
        assert_eq!(selected[0].index01_sample,100); // track 00 owns 0..100
        let mut prepend=default.clone();
        prepend.first_gap=NativeCueFirstGap::PrependFirst;
        prepend.between_gap=NativeCueBetweenGap::PrependNext;
        configure_cue_gap_segments(&all,&mut selected,&prepend).unwrap();
        assert_eq!(selected.iter().map(|s|(s.start_sample,s.end_sample))
            .collect::<Vec<_>>(),vec![(0,450),(450,880),(880,1200)]);
        let mut drop=default.clone();
        drop.first_gap=NativeCueFirstGap::Drop;
        drop.between_gap=NativeCueBetweenGap::Drop;
        configure_cue_gap_segments(&all,&mut selected,&drop).unwrap();
        assert_eq!(selected.iter().map(|s|(s.start_sample,s.end_sample))
            .collect::<Vec<_>>(),vec![(100,450),(500,880),(900,1200)]);
        let mut lossy=all.clone();
        for s in &mut lossy {s.encoder="libmp3lame".into();}
        let mut out=lossy.clone();
        configure_cue_gap_segments(&lossy,&mut out,&drop).unwrap();
        assert_eq!(out[0].packet_cut_samples,vec![100,450,500,880,900]);
        assert_eq!(out.iter().map(|s|s.packet_part_index).collect::<Vec<_>>(),
            vec![1,3,5]);
    }

    /// A new selection must not inherit the older mandatory area-folder rule.
    #[test]
    fn r34_one_program_goes_straight_to_folder_with_its_log() {
        let temp=tempfile::tempdir().unwrap();
        let mut job=fixture_job(&temp.path().join("output"),temp.path());
        job.program_subfolders=false;
        job.round4_layout=true;
        job.extraction_log=true;
        plan_native_program_logs(&mut job).unwrap();
        let media=proposed_destination(&job,&job.programs[0],&job.programs[0].tracks[0]);
        assert_eq!(media.parent(),Some(job.destination.as_path()));
        assert_eq!(native_log_targets(&job),vec![job.destination.join(EXTRACTION_LOG_FILE)]);
        validate_job(&job).unwrap();
        let encoded=serde_json::to_string(&job).unwrap();
        let resumed:NativeExtractionJob=serde_json::from_str(&encoded).unwrap();
        assert_eq!(resumed.program_log_paths,job.program_log_paths);
        assert_eq!(proposed_destination(&resumed,&resumed.programs[0],&resumed.programs[0].tracks[0]),media);
    }

    #[test]
    fn r34_two_programs_get_separate_folders_and_independent_logs() {
        let temp=tempfile::tempdir().unwrap();
        let mut job=fixture_job(&temp.path().join("output"),temp.path());
        let mut second=job.programs[0].clone();
        second.id=PresentationId::SacdArea(crate::disc::SacdAreaId::MultiChannel);
        second.label="Multichannel".into();
        job.programs.push(second);
        job.program_subfolders=true;
        job.round4_layout=true;
        job.extraction_log=true;
        plan_native_program_logs(&mut job).unwrap();
        let p0=proposed_destination(&job,&job.programs[0],&job.programs[0].tracks[0]);
        let p1=proposed_destination(&job,&job.programs[1],&job.programs[1].tracks[0]);
        assert_eq!(p0.parent(),Some(job.destination.join("Stereo").as_path()));
        assert_eq!(p1.parent(),Some(job.destination.join("Multichannel").as_path()));
        assert_eq!(native_log_targets(&job),vec![job.destination.join("Stereo/extraction.log"),
            job.destination.join("Multichannel/extraction.log")]);
        validate_job(&job).unwrap();
        let receipt0=native_log_content_for_index(&job,0,&[p0.clone(),p1.clone()],&[],&[],
            NativeLogOutcome::Complete,None);
        let receipt1=native_log_content_for_index(&job,1,&[p0.clone(),p1.clone()],&[],&[],
            NativeLogOutcome::Complete,None);
        assert!(receipt0.contains(&format!("File: {:?}",p0.to_string_lossy())));
        assert!(!receipt0.contains(&format!("File: {:?}",p1.to_string_lossy())));
        assert!(receipt1.contains(&format!("File: {:?}",p1.to_string_lossy())));
        assert!(!receipt1.contains(&format!("File: {:?}",p0.to_string_lossy())));
    }

    #[test]
    fn r34_explicit_program_token_avoids_implicit_folders_and_preserves_deepest_log_root() {
        let temp=tempfile::tempdir().unwrap();
        let mut job=fixture_job(&temp.path().join("output"),temp.path());
        let mut second=job.programs[0].clone();
        second.id=PresentationId::SacdArea(crate::disc::SacdAreaId::MultiChannel);
        second.label="Multichannel".into();
        job.programs.push(second);
        job.round4_layout=true;
        job.program_subfolders=false;
        job.extraction_log=true;
        job.naming_template="%AREA%/%TRACKNUMBER% - %TITLE%".into();
        assert!(native_template_has_program_token(&job.naming_template));
        plan_native_program_logs(&mut job).unwrap();
        let roots=job.programs.iter().map(|p|native_program_output_root(&job,p)).collect::<Vec<_>>();
        assert_eq!(roots,vec![job.destination.join("Stereo"),job.destination.join("Multichannel")]);
        assert_eq!(job.program_log_paths,roots.iter().map(|root|root.join(EXTRACTION_LOG_FILE))
            .collect::<Vec<_>>());
        for (program,root) in job.programs.iter().zip(roots) {
            let media=proposed_destination(&job,program,&program.tracks[0]);
            assert_eq!(media.parent(),Some(root.as_path()));
            assert!(!media.file_name().unwrap().to_string_lossy().starts_with("Stereo - "));
        }
        validate_job(&job).unwrap();
    }

    #[test]
    fn r34_log_suffixes_protect_existing_receipts_and_repeat_without_side_effects() {
        let temp=tempfile::tempdir().unwrap();
        let mut job=fixture_job(&temp.path().join("output"),temp.path());
        job.round4_layout=true;
        job.program_subfolders=false;
        job.extraction_log=true;
        fs::create_dir_all(&job.destination).unwrap();
        fs::write(job.destination.join("extraction.log"),b"existing user receipt").unwrap();
        fs::write(job.destination.join("extraction-2.log"),b"another receipt").unwrap();
        for policy in [NativeOverwritePolicy::Fail,NativeOverwritePolicy::Skip] {
            job.overwrite_policy=policy;
            plan_native_program_logs(&mut job).unwrap();
            assert_eq!(native_log_targets(&job),vec![job.destination.join("extraction-3.log")]);
            validate_job(&job).unwrap();
            plan_native_program_logs(&mut job).unwrap();
            assert_eq!(native_log_targets(&job),vec![job.destination.join("extraction-3.log")]);
        }
        assert_eq!(fs::read(job.destination.join("extraction.log")).unwrap(),
            b"existing user receipt");
        job.overwrite_policy=NativeOverwritePolicy::Replace;
        plan_native_program_logs(&mut job).unwrap();
        assert_eq!(native_log_targets(&job),vec![job.destination.join("extraction.log")]);
        validate_job(&job).unwrap();
    }

    #[test]
    fn r34_multiple_logs_share_a_folder_with_disjoint_names() {
        let temp=tempfile::tempdir().unwrap();
        let mut job=fixture_job(&temp.path().join("output"),temp.path());
        job.round4_layout=true;
        job.program_subfolders=false;
        job.extraction_log=true;
        job.naming_template="%AREA% - %TRACKNUMBER% - %TITLE%".into();
        let mut second=job.programs[0].clone();
        second.id=PresentationId::SacdArea(crate::disc::SacdAreaId::MultiChannel);
        second.label="Multichannel".into();
        job.programs.push(second);
        plan_native_program_logs(&mut job).unwrap();
        assert_eq!(job.program_log_paths,
            vec![job.destination.join("extraction.log"),job.destination.join("extraction-2.log")]);
        validate_job(&job).unwrap();
        let old=job.destination.clone();
        let admitted=temp.path().join("physical-output");
        admit_native_destination(&mut job,admitted.clone()).unwrap();
        assert_eq!(job.destination,admitted);
        assert_eq!(job.program_log_paths,
            vec![admitted.join("extraction.log"),admitted.join("extraction-2.log")]);
        assert!(!job.program_log_paths.iter().any(|path|path.starts_with(&old)));
        assert_eq!(native_program_output_root(&job,&job.programs[0]),admitted);
    }

    #[test]
    fn extraction_log_reports_measured_outputs_confirmed_skips_and_source_identity() {
        let temp = tempfile::tempdir().unwrap();
        let mut job = fixture_job(&temp.path().join("output"), temp.path());
        job.extraction_log = true;
        job.disc_identity = NativeDiscIdentity {
            title: Some("The Album".into()), artist: Some("The Artist".into()),
            catalog: Some("ABC-123".into()),
        };
        job.programs[0].tracks[0].duration_secs = Some(121.0);
        job.programs[0].tracks[0].estimated_bytes = Some(9_000_000);
        job.programs[0].tracks.push(NativeTrackChoice {
            number: 2, name: "two".into(), duration_secs: Some(60.0),
            estimated_bytes: Some(9_000_000), tags: NativeTagValues::default(),
        });
        let written_path = proposed_destination(&job, &job.programs[0], &job.programs[0].tracks[0]);
        std::fs::create_dir_all(written_path.parent().unwrap()).unwrap();
        std::fs::write(&written_path, b"abcde").unwrap();
        let skipped_path = proposed_destination(&job, &job.programs[0], &job.programs[0].tracks[1]);
        let skipped = NativeSkippedSelection::track(&job.programs[0], 2)
            .with_reason(NativeSkipReason::ExistingFile);
        let body = render_native_extraction_log(&job,
            &[written_path.clone(), job.destination.join(EXTRACTION_LOG_FILE)], &[skipped]);
        assert!(body.contains("TonePoet "));
        assert!(body.contains("Generated: "));
        assert!(body.contains("Disc title: \"The Album\""));
        assert!(body.contains("Disc artist: \"The Artist\""));
        assert!(body.contains("Catalog: \"ABC-123\""));
        assert!(body.contains("Duration: 2:01"));
        assert!(body.contains("Duration: 1:00"));
        assert!(body.contains(&format!("File: {:?}", written_path.to_string_lossy())));
        assert!(body.contains(&format!("File: {:?}", skipped_path.to_string_lossy())));
        assert!(body.contains("Size: 5 bytes"), "measured size, not preflight estimate");
        assert!(body.contains("Skipped: file already existed"));
        assert!(body.contains("Written files: 1"));
        assert!(body.contains("Skipped items: 1"));
        assert!(body.contains("Outcome: finished with skipped items"));
        assert!(!body.contains(&format!("File: {:?}",job.destination.join(EXTRACTION_LOG_FILE).to_string_lossy())),
            "log cannot inventory itself");
    }

    #[test]
    fn earlier_native_extraction_journals_deserialize_without_new_receipt_fields() {
        let temp = tempfile::tempdir().unwrap();
        let job = fixture_job(temp.path(), temp.path());
        let mut value = serde_json::to_value(&job).unwrap();
        value.as_object_mut().unwrap().remove("disc_identity");
        value["programs"][0]["tracks"][0].as_object_mut().unwrap().remove("duration_secs");
        let recovered: NativeExtractionJob = serde_json::from_value(value).unwrap();
        assert!(recovered.disc_identity.title.is_none());
        assert!(recovered.programs[0].tracks[0].duration_secs.is_none());
        let skipped = NativeSkippedSelection::track(&job.programs[0], 1);
        let mut value = serde_json::to_value(&skipped).unwrap();
        value.as_object_mut().unwrap().remove("reason");
        let restored: NativeSkippedSelection = serde_json::from_value(value).unwrap();
        assert!(restored.same_selection(&skipped));
        assert_eq!(restored.reason, None);
    }

    #[test]
    fn matroska_job_rejects_missing_source_codec_instead_of_legacy_mka_fallback() {
        let temp = tempfile::tempdir().unwrap();
        let mut job = fixture_job(temp.path(), temp.path());
        job.format = DiscFormat::Matroska;
        job.programs[0].source_codec = "flac".into();
        let mut record = serde_json::to_value(&job).unwrap();
        record["programs"][0].as_object_mut().unwrap().remove("source_codec");
        assert!(serde_json::from_value::<NativeExtractionJob>(record).is_err());
        // Unknown codecs remain eligible for MKA for codec support, not for
        // recovery of an older job without its required codec identity.
        assert_eq!(matroska_copy_format("unrecognized_codec").extension, "mka");
    }

    #[test]
    fn continuous_log_counts_all_tracks_when_a_program_is_skipped() {
        let temp = tempfile::tempdir().unwrap();
        let mut job = fixture_job(temp.path(), temp.path());
        job.file_per_program = true;
        job.programs[0].tracks.push(NativeTrackChoice {
            number: 2, name: "two".into(), duration_secs: Some(60.0),
            estimated_bytes: None, tags: NativeTagValues::default(),
        });
        let skipped = NativeSkippedSelection::program(&job.programs[0])
            .with_reason(NativeSkipReason::ExistingFile);
        let body=render_native_extraction_log(&job,&[],&[skipped]);
        assert!(body.contains("Skipped items: 2"));
        assert!(body.contains("Outcome: finished with skipped items"));
        assert!(!body.contains("Outcome: failed"));
    }

    #[test]
    fn continuous_log_handles_individual_program_and_cue_skips_without_double_counts() {
        let temp = tempfile::tempdir().unwrap();
        let mut job = fixture_job(&temp.path().join("output"), temp.path());
        job.file_per_program = true;
        job.cue = true;
        job.format = DiscFormat::DvdAudio;
        job.programs[0].id = PresentationId::DvdAudioGroup(1);
        job.programs[0].tracks = (1..=3).map(|number| NativeTrackChoice {
            number, name: format!("Track {number}"), duration_secs: Some(10.0),
            estimated_bytes: Some(1000), tags: NativeTagValues::default(),
        }).collect();
        let program = &job.programs[0];
        let skip = |number| NativeSkippedSelection::track(program, number)
            .with_reason(NativeSkipReason::RequestedByUser);
        let all_individual = vec![skip(1),skip(2),skip(3)];
        let body = render_native_extraction_log(&job, &[], &all_individual);
        assert!(body.contains("Skipped items: 3"), "{body}");
        assert!(body.contains("Failed outputs: 0"), "{body}");
        assert!(body.contains("every selected track was individually skipped"), "{body}");
        assert!(body.contains("CUE:") && body.contains("skipped (no continuous audio)"), "{body}");

        let whole = NativeSkippedSelection::program(program)
            .with_reason(NativeSkipReason::RequestedByUser);
        let body = render_native_extraction_log(&job, &[], &[skip(1), whole]);
        assert!(body.contains("Skipped items: 3"), "{body}");
        assert!(!body.contains("Skipped items: 4"), "{body}");
        assert!(body.contains("Failed outputs: 0"), "{body}");
        let whole_existing = NativeSkippedSelection::program(program)
            .with_reason(NativeSkipReason::ExistingFile);
        let body = render_native_extraction_log(&job, &[], &[whole_existing]);
        assert!(body.contains("Skipped items: 3"), "{body}");
        assert!(body.contains("Failed outputs: 0"), "{body}");
        assert!(body.contains("Skipped: file already existed"), "{body}");

        let output = proposed_destination_for_program(&job, program);
        std::fs::create_dir_all(output.parent().unwrap()).unwrap();
        std::fs::write(&output, b"joined audio").unwrap();
        let body = render_native_extraction_log_with_outcome(&job,
            std::slice::from_ref(&output), &[skip(1)], &[], NativeLogOutcome::Complete, None);
        assert!(body.contains("Written files: 1"), "{body}");
        assert!(body.contains("Skipped items: 1"), "{body}");
        // A complete CUE-enabled program with a missing expected sheet is an
        // actual missing publication, not a skipped source track.
        assert!(body.contains("Failed outputs: 1"), "{body}");
        // With no CUE requested, an individually skipped track plus a
        // successfully published combined file is a completed, not failed,
        // extraction.
        job.cue = false;
        let body = render_native_extraction_log(&job, &[output.clone()], &[skip(1)]);
        assert!(body.contains("Written files: 1"), "{body}");
        assert!(body.contains("Skipped items: 1"), "{body}");
        assert!(body.contains("Failed outputs: 0"), "{body}");
        job.cue = true;

        let issue = NativeUnfinishedOutput {
            destination: output.clone(), reason: "join failed: source read error".into(),
            cancelled: false,
        };
        let body = render_native_extraction_log_with_outcome(&job, &[], &[skip(1)],
            &[issue], NativeLogOutcome::Failed, Some("source read error"));
        assert!(body.contains("Failed outputs: 1"), "{body}");
        assert!(body.contains("join failed: source read error"), "{body}");
        assert!(body.contains("Skipped items: 1"), "{body}");
        assert!(body.contains("CUE:") && body.contains("not reached"), "{body}");
    }

    #[test]
    fn terminal_log_distinguishes_failed_cancelled_and_unreached_outputs() {
        let temp = tempfile::tempdir().unwrap();
        let mut job = fixture_job(&temp.path().join("output"), temp.path());
        job.programs[0].tracks = (1..=3).map(|number| NativeTrackChoice {
            number, name: format!("Track {number}"), duration_secs: Some(90.0),
            estimated_bytes: Some(1024), tags: NativeTagValues::default(),
        }).collect();
        let program = &job.programs[0];
        let first = proposed_destination(&job, program, &program.tracks[0]);
        let second = proposed_destination(&job, program, &program.tracks[1]);
        let third = proposed_destination(&job, program, &program.tracks[2]);
        std::fs::create_dir_all(first.parent().unwrap()).unwrap();
        std::fs::write(&first, b"previously published audio").unwrap();
        let failed = NativeUnfinishedOutput { destination: second.clone(),
            reason: "read error on track 2".into(), cancelled: false };
        let body = render_native_extraction_log_with_outcome(&job, std::slice::from_ref(&first),
            &[], &[failed.clone()], NativeLogOutcome::Failed, Some("track 2 failed"));
        assert!(body.contains("Written files: 1"), "{body}");
        assert!(body.contains("Failed outputs: 1"), "{body}");
        assert!(body.contains("read error on track 2"), "{body}");
        let pending = body.find(&format!("File: {:?}",third.to_string_lossy())).unwrap();
        assert!(body[pending..].contains("Outcome: not reached"), "{body}");
        assert!(!body[pending..].contains("Outcome: failed"), "{body}");
        let cancelled = NativeUnfinishedOutput { cancelled: true, ..failed };
        let body = render_native_extraction_log_with_outcome(&job, &[first],
            &[], &[cancelled], NativeLogOutcome::Cancelled, Some("user cancelled"));
        assert!(body.contains("Outcome: cancelled"), "{body}");
        assert!(body.contains("Failed outputs: 0"), "{body}");
        assert!(body.contains("Outcome: not reached"), "{body}");
    }

    #[test]
    fn preexisting_log_under_skip_or_fail_gets_unique_name_before_media_work() {
        let temp = tempfile::tempdir().unwrap();
        let mut job = fixture_job(&temp.path().join("output"), temp.path());
        job.round4_layout = true;
        job.program_subfolders = false;
        job.extraction_log = true;
        fs::create_dir_all(&job.destination).unwrap();
        let log = job.destination.join(EXTRACTION_LOG_FILE);
        fs::write(&log, b"existing user receipt").unwrap();
        let media = proposed_destination(&job, &job.programs[0], &job.programs[0].tracks[0]);
        let before = path_inventory(&job.destination);
        assert_eq!(before, BTreeSet::from([EXTRACTION_LOG_FILE.to_string()]));

        // Both Stop and Skip reserve a different log before the first media
        // operation. Replanning is stable and does not publish anything.
        for policy in [NativeOverwritePolicy::Skip, NativeOverwritePolicy::Fail] {
            job.overwrite_policy = policy;
            plan_native_program_logs(&mut job).unwrap();
            let expected = job.destination.join("extraction-2.log");
            assert_eq!(native_log_targets(&job), vec![expected.clone()]);
            validate_job(&job).unwrap();
            plan_native_program_logs(&mut job).unwrap();
            assert_eq!(native_log_targets(&job), vec![expected]);
            assert_eq!(fs::read(&log).unwrap(), b"existing user receipt");
            assert_eq!(path_inventory(&job.destination), before);
            assert!(!media.exists(), "preflight must not publish media");
        }
        // Replace explicitly reuses the base name, but still does not write
        // until the extraction job actually publishes its receipt.
        job.overwrite_policy = NativeOverwritePolicy::Replace;
        plan_native_program_logs(&mut job).unwrap();
        assert_eq!(native_log_targets(&job), vec![log.clone()]);
        validate_job(&job).unwrap();
        assert_eq!(fs::read(&log).unwrap(), b"existing user receipt");
        assert_eq!(path_inventory(&job.destination), before);
    }

    #[tokio::test]
    async fn preexisting_cue_with_skip_is_rejected_before_continuous_audio_work() {
        let temp = tempfile::tempdir().unwrap();
        let formats = [
            (DiscFormat::DvdAudio, PresentationId::DvdAudioGroup(1)),
            (DiscFormat::DvdVideo, PresentationId::dvd_video(1, 1, 0)),
            (DiscFormat::BluRay, PresentationId::try_blu_ray_title(1, 0x1100, 0, 1).unwrap()),
        ];
        for (format, presentation) in formats {
            let mut job = fixture_job(&temp.path().join(format.name()), temp.path());
            job.format = format;
            job.programs[0].id = presentation;
            job.file_per_program = true;
            job.cue = true;
            job.overwrite_policy = NativeOverwritePolicy::Skip;
            let media = proposed_destination_for_program(&job, &job.programs[0]);
            let cue = media.with_extension("cue");
            fs::create_dir_all(cue.parent().unwrap()).unwrap();
            fs::write(&cue, b"original CUE sheet").unwrap();
            let before = path_inventory(&job.destination);

            // Direct callers and the UI share this validation; execute must
            // refuse without materializing or publishing any audio.
            let error = execute(job.clone(), CancellationToken::new(), |_| {}).await.unwrap_err();
            assert!(error.contains("CUE sheet already exists"), "{format:?}: {error}");
            assert!(error.contains("choose Replace, disable CUE output, or change the destination"),
                "{format:?}: {error}");
            assert!(!media.exists(), "{format:?}: unexpected continuous audio");
            assert_eq!(fs::read(&cue).unwrap(), b"original CUE sheet");
            assert_eq!(path_inventory(&job.destination), before);

            job.cue = false;
            assert!(validate_job(&job).is_ok(), "{format:?}: disabling CUE resolves collision");
            job.cue = true;
            job.overwrite_policy = NativeOverwritePolicy::Fail;
            assert!(validate_job(&job).unwrap_err().contains("Destination item already exists"));
            job.overwrite_policy = NativeOverwritePolicy::Replace;
            assert!(validate_job(&job).is_ok(), "{format:?}: Replace must admit CUE");
            fs::remove_file(&cue).unwrap();
            job.overwrite_policy = NativeOverwritePolicy::Skip;
            assert!(validate_job(&job).is_ok(), "{format:?}: no CUE collision");

            // Skip must continue accepting an existing whole-program audio
            // output when there is no CUE collision.
            fs::write(&media, b"previously joined audio").unwrap();
            assert!(validate_job(&job).is_ok(), "{format:?}: whole-program media skip");
            assert_eq!(fs::read(&media).unwrap(), b"previously joined audio");
        }
    }

    #[tokio::test]
    async fn existing_continuous_audio_and_cue_allow_skip_and_other_programs() {
        let temp = tempfile::tempdir().unwrap();
        let formats = [
            (DiscFormat::DvdAudio, PresentationId::DvdAudioGroup(1), PresentationId::DvdAudioGroup(2)),
            (DiscFormat::DvdVideo, PresentationId::dvd_video(1, 1, 0), PresentationId::dvd_video(1, 2, 0)),
            (DiscFormat::BluRay,
                PresentationId::try_blu_ray_title(1, 0x1100, 0, 1).unwrap(),
                PresentationId::try_blu_ray_title(2, 0x1100, 0, 1).unwrap()),
        ];
        for (format, first_id, second_id) in formats {
            let mut job = fixture_job(&temp.path().join(format.name()), temp.path());
            job.format = format;
            job.programs[0].id = first_id;
            job.file_per_program = true;
            job.cue = true;
            job.overwrite_policy = NativeOverwritePolicy::Skip;
            let media = proposed_destination_for_program(&job, &job.programs[0]);
            let cue = media.with_extension("cue");
            fs::create_dir_all(media.parent().unwrap()).unwrap();
            fs::write(&media, b"previous continuous audio").unwrap();
            fs::write(&cue, b"previous CUE sheet").unwrap();
            let before = path_inventory(&job.destination);

            // The engine skips this entire program without attempting CUE
            // publication, so an existing CUE must not block validation.
            assert!(validate_job(&job).is_ok(), "{format:?}: existing audio and CUE");
            let report = execute(job.clone(), CancellationToken::new(), |_| {}).await.unwrap();
            assert!(report.published.is_empty(), "{format:?}: no output should be republished");
            assert_eq!(report.skipped, job.programs[0].tracks.len(), "{format:?}");
            assert_eq!(fs::read(&media).unwrap(), b"previous continuous audio");
            assert_eq!(fs::read(&cue).unwrap(), b"previous CUE sheet");
            assert_eq!(path_inventory(&job.destination), before);

            // A completed program must not prevent a different selected
            // program from entering the extraction queue under Skip.
            let mut second = job.programs[0].clone();
            second.id = second_id;
            second.label = "Another program".into();
            job.programs.push(second);
            assert!(!proposed_destination_for_program(&job, &job.programs[1]).exists());
            assert!(validate_job(&job).is_ok(), "{format:?}: second program should proceed");
        }
    }

    #[test]
    fn continuous_log_uses_source_duration_for_whole_skips_and_output_duration_for_joins() {
        let temp = tempfile::tempdir().unwrap();
        let mut job = fixture_job(&temp.path().join("output"), temp.path());
        job.format = DiscFormat::DvdAudio;
        job.programs[0].id = PresentationId::DvdAudioGroup(1);
        job.file_per_program = true;
        job.programs[0].tracks = [20.0, 25.0].into_iter().enumerate().map(|(index, minutes)| {
            NativeTrackChoice {
                number: index as u32 + 1,
                name: format!("Track {}", index + 1), duration_secs: Some(minutes * 60.0),
                estimated_bytes: None, tags: NativeTagValues::default(),
            }
        }).collect();
        let program = &job.programs[0];
        let output = proposed_destination_for_program(&job, program);
        let first_skip = NativeSkippedSelection::track(program, 1)
            .with_reason(NativeSkipReason::RequestedByUser);
        let second_skip = NativeSkippedSelection::track(program, 2)
            .with_reason(NativeSkipReason::RequestedByUser);
        let whole_skip = NativeSkippedSelection::program(program)
            .with_reason(NativeSkipReason::ExistingFile);

        let whole = render_native_extraction_log(&job, &[], &[whole_skip]);
        assert!(whole.contains("Duration: 45:00 (selected source; output skipped)"), "{whole}");
        assert!(whole.contains("Skipped: file already existed"), "{whole}");
        assert!(whole.contains("Skipped items: 2"), "{whole}");
        assert!(whole.contains("Failed outputs: 0"), "{whole}");

        let none_written = render_native_extraction_log(&job, &[], &[first_skip.clone(), second_skip]);
        assert!(none_written.contains("Duration: unavailable (no continuous output)"), "{none_written}");
        assert!(!none_written.contains("Duration: 0:00"), "{none_written}");
        assert!(none_written.contains("every selected track was individually skipped"), "{none_written}");
        assert!(none_written.contains("Skipped items: 2"), "{none_written}");
        assert!(none_written.contains("Failed outputs: 0"), "{none_written}");

        fs::create_dir_all(output.parent().unwrap()).unwrap();
        fs::write(&output, b"joined second track").unwrap();
        let joined = render_native_extraction_log(&job, &[output], &[first_skip]);
        assert!(joined.contains("Duration: 25:00"), "{joined}");
        assert!(!joined.contains("Duration: 45:00"), "{joined}");
        assert!(joined.contains("Outcome: written"), "{joined}");
        assert!(joined.contains("Skipped items: 1"), "{joined}");
        assert!(joined.contains("Failed outputs: 0"), "{joined}");
    }

    #[test]
    fn partial_continuous_skips_count_only_published_track_estimates() {
        const TRACK: u64 = 100 * 1024 * 1024;
        let mut job = fixture_job(Path::new("/unused-destination"), Path::new("/unused-scratch"));
        let program = &mut job.programs[0];
        program.tracks = (1..=3).map(|number| NativeTrackChoice { duration_secs: None,
            number, name: format!("Track {number}"), tags: NativeTagValues::default(),
            estimated_bytes: Some(TRACK),
        }).collect();

        // Two selected tracks, first skipped: only track 2 reaches the join.
        let mut two_tracks = program.clone();
        two_tracks.tracks.pop();
        let staged = vec![(2, PathBuf::from("private-2.wav"), "Track 2".into())];
        let published = estimated_bytes_for_staged_continuous_tracks(&two_tracks, &staged);
        let skipped = two_tracks.tracks[0].estimated_bytes.unwrap();
        let total = two_tracks.tracks.iter().filter_map(|t| t.estimated_bytes).sum::<u64>();
        assert_eq!((published, skipped, total), (TRACK, TRACK, 2 * TRACK));
        assert_eq!(published + skipped, total);

        // Three selected tracks, middle skipped: 1 and 3 reach the join.
        let staged = vec![
            (1, PathBuf::from("private-1.wav"), "Track 1".into()),
            (3, PathBuf::from("private-3.wav"), "Track 3".into()),
        ];
        let published = estimated_bytes_for_staged_continuous_tracks(program, &staged);
        let skipped = program.tracks[1].estimated_bytes.unwrap();
        let total = program.tracks.iter().filter_map(|t| t.estimated_bytes).sum::<u64>();
        assert_eq!((published, skipped, total), (2 * TRACK, TRACK, 3 * TRACK));
        assert_eq!(published + skipped, total);
    }

    /// Include hidden entries and every nested directory. An inventory that
    /// looks only at final filenames would miss the old sibling work folder.
    fn path_inventory(root: &Path) -> BTreeSet<String> {
        fn walk(root: &Path, dir: &Path, out: &mut BTreeSet<String>) {
            for entry in fs::read_dir(dir).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                let name = path.strip_prefix(root).unwrap().to_string_lossy()
                    .replace(std::path::MAIN_SEPARATOR, "/");
                if entry.file_type().unwrap().is_dir() {
                    out.insert(format!("{name}/"));
                    walk(root, &path, out);
                } else {
                    out.insert(name);
                }
            }
        }
        let mut inventory = BTreeSet::new();
        walk(root, root, &mut inventory);
        inventory
    }

    #[test]
    fn native_materializer_settings_are_built_by_the_typed_legacy_bridge() {
        let settings = native_compatibility_settings().unwrap();
        settings.validate().unwrap();
    }

    #[test]
    fn default_native_staging_and_persisted_r5_job_use_cache_not_music_tree() {
        let temp = tempfile::tempdir().unwrap();
        let destination = temp.path().join("music/album");
        let old_root = temp.path().join(".tonepoet-native-extraction-work");
        let fallback = default_native_working_root();
        assert!(fallback.ends_with(Path::new("tonepoet/native-extraction")));
        assert_eq!(native_working_storage(&old_root, &destination).unwrap(), fallback);
        let custom = temp.path().join("custom-scratch");
        assert_eq!(native_working_storage(&custom, &destination).unwrap(), custom);
        assert!(path_inventory(temp.path()).is_empty(), "path preflight must be side-effect free");
    }

    #[test]
    fn completed_publish_has_exact_destination_and_parent_inventory() {
        let temp = tempfile::tempdir().unwrap();
        let scratch = temp.path().join("scratch");
        fs::create_dir(&scratch).unwrap();
        let destination = temp.path().join("music");
        let job = fixture_job(&destination, &scratch);
        {
            let staging = tempfile::tempdir_in(&scratch).unwrap();
            let complete = staging.path().join("complete.dsf");
            fs::write(&complete, b"validated native payload").unwrap();
            publish_finished_file(&job, &complete, &destination.join("track.dsf")).unwrap();
            assert_eq!(fs::read(destination.join("track.dsf")).unwrap(), b"validated native payload");
            assert!(!complete.exists());
        }
        assert_eq!(path_inventory(temp.path()), BTreeSet::from([
            "scratch/".to_string(), "music/".to_string(), "music/track.dsf".to_string(),
        ]));
    }

    #[test]
    fn failed_publish_removes_staging_and_does_not_change_destination_inventory() {
        let temp = tempfile::tempdir().unwrap();
        let scratch = temp.path().join("scratch");
        let destination = temp.path().join("music");
        let existing_directory = destination.join("reserved");
        fs::create_dir_all(&scratch).unwrap();
        fs::create_dir_all(&existing_directory).unwrap();
        let before = path_inventory(temp.path());
        let mut job = fixture_job(&destination, &scratch);
        job.overwrite_policy = NativeOverwritePolicy::Replace;
        {
            let staging = tempfile::tempdir_in(&scratch).unwrap();
            let complete = staging.path().join("complete.dsf");
            fs::write(&complete, b"validated native payload").unwrap();
            assert!(publish_finished_file(&job, &complete, &existing_directory).is_err());
        }
        assert_eq!(path_inventory(temp.path()), before);
    }

    #[tokio::test]
    async fn cancelled_and_failed_extractions_leave_destination_and_parent_unchanged() {
        let temp = tempfile::tempdir().unwrap();
        let scratch = temp.path().join("scratch");
        fs::create_dir(&scratch).unwrap();
        let destination = temp.path().join("music");
        let job = fixture_job(&destination, &scratch);
        let before = path_inventory(temp.path());

        let cancelled = CancellationToken::new();
        cancelled.cancel();
        let err = execute(job.clone(), cancelled, |_| {}).await.unwrap_err();
        assert!(err.contains("cancelled"), "{err}");
        assert_eq!(path_inventory(temp.path()), before);

        // A real media failure occurs after the worker owns a TempDir, not
        // just in validate_job(). Our executable is not a valid SACD ISO.
        assert!(execute(job, CancellationToken::new(), |_| {}).await.is_err());
        assert_eq!(path_inventory(temp.path()), before);
    }

    #[cfg(unix)]
    #[test]
    fn cross_volume_publication_cleans_success_and_failed_copy_temp_files() {
        use std::os::unix::fs::MetadataExt;
        let destination_root = tempfile::tempdir().unwrap();
        let Ok(scratch) = tempfile::Builder::new()
            .prefix("tonepoet-extract-cross-device-").tempdir_in("/dev/shm") else {
            return; // Not all Unix hosts mount a writable, separate /dev/shm.
        };
        if fs::metadata(destination_root.path()).unwrap().dev()
            == fs::metadata(scratch.path()).unwrap().dev() { return; }
        let destination = destination_root.path().join("music");
        let mut job = fixture_job(&destination, scratch.path());
        {
            let staging = tempfile::tempdir_in(scratch.path()).unwrap();
            let complete = staging.path().join("complete.dsf");
            fs::write(&complete, b"validated native payload").unwrap();
            publish_finished_file(&job, &complete, &destination.join("track.dsf")).unwrap();
            assert_eq!(fs::read(destination.join("track.dsf")).unwrap(), b"validated native payload");
        }
        assert_eq!(path_inventory(destination_root.path()), BTreeSet::from([
            "music/".to_string(), "music/track.dsf".to_string(),
        ]));
        assert!(path_inventory(scratch.path()).is_empty());

        // EXDEV copying succeeds, but the atomic rename cannot replace a
        // directory. Its private destination-side copy must be removed.
        let reserved = destination.join("reserved");
        fs::create_dir(&reserved).unwrap();
        let before = path_inventory(destination_root.path());
        job.overwrite_policy = NativeOverwritePolicy::Replace;
        {
            let staging = tempfile::tempdir_in(scratch.path()).unwrap();
            let complete = staging.path().join("complete.dsf");
            fs::write(&complete, b"validated native payload").unwrap();
            assert!(publish_finished_file(&job, &complete, &reserved).is_err());
        }
        assert_eq!(path_inventory(destination_root.path()), before);
        assert!(path_inventory(scratch.path()).is_empty());
    }

    #[test]
    fn large_dsf_cover_then_text_stays_in_place_and_preserves_audio() {
        use std::io::{Seek, SeekFrom};
        use id3::TagLike;
        let _coordination = crate::concurrency::scoped_test_coordination_root();
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("track.dsf");
        crate::dsf_tags::write_test_dsf_fixture(&target, None).unwrap();
        let before = std::fs::read(&target).unwrap();
        #[cfg(unix)]
        let original_inode = {
            use std::os::unix::fs::MetadataExt;
            std::fs::metadata(&target).unwrap().ino()
        };

        // JPEG APP2 marker payloads are legal metadata; they make a valid
        // image exceed DSF's 1 MiB reserve without needing a huge bitmap.
        let image = tmp.path().join("cover.jpg");
        image::DynamicImage::new_rgb8(16, 16).save(&image).unwrap();
        let small = std::fs::read(&image).unwrap();
        assert!(small.starts_with(&[0xff, 0xd8]));
        let mut large = vec![0xff, 0xd8];
        for _ in 0..34 {
            large.extend_from_slice(&[0xff, 0xe2, 0xff, 0xff]);
            large.extend(std::iter::repeat_n(0x5au8, 65_533));
        }
        large.extend_from_slice(&small[2..]);
        assert!(large.len() > 2 * 1024 * 1024);
        std::fs::write(&image, &large).unwrap();
        image::open(&image).expect("valid large JPEG cover");

        write_completed_native_metadata(&target,
            &[(lofty::tag::ItemKey::TrackTitle, Some("Original audio".into()))],
            Some(&image)).unwrap();
        let after = std::fs::read(&target).unwrap();
        assert_eq!(&before[28..], &after[28..before.len()]);
        #[cfg(unix)] {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(std::fs::metadata(&target).unwrap().ino(), original_inode,
                "a whole-file copy/replacement would allocate a new inode");
        }
        let mut stream = File::open(&target).unwrap();
        stream.seek(SeekFrom::Start(before.len() as u64)).unwrap();
        let tag = id3::Tag::read_from2(&mut stream).unwrap();
        assert_eq!(tag.title(), Some("Original audio"));
        assert_eq!(tag.pictures().next().unwrap().data.as_slice(), large);
    }
    #[test]
    fn tag_fields_and_naming_use_available_evidence_only() {
        let program = NativeProgramChoice {
            id: PresentationId::SacdArea(crate::disc::SacdAreaId::Stereo),
            angle: 1, label: "Stereo".into(), tracks: Vec::new(), keep_dst: false,
            source_depth: Some(1), source_codec: "unknown".into(), source_channels: Some(2), source_rate: Some(2_822_400),
            source_layout: Some("stereo".into()), is_lossless: true,
            tags: NativeTagValues { album: Some("An Album".into()),
                artist: Some("A Performer".into()), ..Default::default() }, authored_tags: None,
        };
        let track = NativeTrackChoice { duration_secs: None, number: 3, name: "A Song".into(),
            tags: NativeTagValues { title: Some("A Song".into()),
                composer: Some("A Composer".into()), ..Default::default() }, estimated_bytes: None };
        assert_eq!(render_naming_template("{track} - {artist} - {title}", &program, &track),
            "03 - A Performer - A Song");
        assert_eq!(name_value("genre", &program, &track), "");
        assert_eq!(render_naming_template("{composer}/{album}", &program, &track),
            "A Composer_An Album");
        let values = NativeTagValues { title: Some("A Song".into()),
            composer: Some("A Composer".into()), genre: None, ..Default::default() };
        let chosen = selected_tag_items(&values, 1 << 5);
        assert_eq!(chosen.len(), 1);
        assert_eq!(chosen[0].0, lofty::tag::ItemKey::Composer);
        assert!(selected_tag_items(&values, 1 << 3).is_empty());
    }

    #[test]
    fn r27_convert_tokens_resolve_subfolders_and_exactly_one_extension() {
        let temp = tempfile::tempdir().unwrap();
        let mut job = fixture_job(&temp.path().join("output"), temp.path());
        job.programs[0].tracks[0].number = 3;
        job.programs[0].tracks[0].name = "A Song".into();
        job.programs[0].tags.artist = Some("A Performer".into());
        job.naming_template = "%ARTIST%/%TRACKNUMBER% - %TITLE%.%EXTENSION%".into();
        assert_eq!(
            proposed_destination(&job, &job.programs[0], &job.programs[0].tracks[0]),
            job.destination.join("Stereo/A Performer/03 - A Song.dsf")
        );
        assert_eq!(unknown_native_naming_token(&job.naming_template), None);

        job.naming_template = "%TRACKNN% - %TITLE%".into();
        assert_eq!(
            proposed_destination(&job, &job.programs[0], &job.programs[0].tracks[0]),
            job.destination.join("Stereo/03 - A Song.dsf"),
            "Convert's numbering tokens must use the same spelling in Extract"
        );
        assert_eq!(unknown_native_naming_token("%UNRECOGNIZED%"),
            Some("%UNRECOGNIZED%".into()));
        assert_eq!(unknown_native_naming_token("%TITLE"),
            Some("unclosed % token".into()));

        job.disc_identity.catalog = Some("SACD-123".into());
        job.disc_identity.title = Some("Recorded Album".into());
        job.naming_template = "%CATALOG%/%ALBUM%/%TRACKNUMBER% - %TITLE%".into();
        assert_eq!(proposed_destination(&job,&job.programs[0],&job.programs[0].tracks[0]),
            job.destination.join("Stereo/SACD-123/Recorded Album/03 - A Song.dsf"));
    }

    #[test]
    fn r28_convert_preset_disc_number_and_conditional_title_extra_match_convert() {
        let temp = tempfile::tempdir().unwrap();
        let mut job = fixture_job(&temp.path().join("output"), temp.path());
        let track = job.programs[0].tracks.first_mut().unwrap();
        track.name = "First Song".into();
        track.tags.album = Some("Plain Album".into());
        job.naming_template = "%DISC%/%TRACKNN% - %TITLE%".into();
        assert_eq!(proposed_destination(&job,&job.programs[0],&job.programs[0].tracks[0]),
            job.destination.join("Stereo/1/01 - First Song.dsf"),
            "Convert's %DISC% defaults to the first disc when no disc tag is available");

        // Convert omits an entire unresolved conditional block, including its
        // brace punctuation. A missing TITLE_EXTRA must not become Untitled.
        job.naming_template = "%ALBUM% {%TITLE_EXTRA%}/%TRACKNN% - %TITLE%".into();
        assert_eq!(proposed_destination(&job,&job.programs[0],&job.programs[0].tracks[0]),
            job.destination.join("Stereo/Plain Album/01 - First Song.dsf"));

        // The same Convert qualifier extractor supplies a populated block;
        // ordinary metadata blocks retain their literal braces when present.
        job.programs[0].tracks[0].tags.album = Some("Plain Album (Japan SHM-CD)".into());
        assert_eq!(proposed_destination(&job,&job.programs[0],&job.programs[0].tracks[0]),
            job.destination.join("Stereo/Plain Album {Japan SHM-CD}/01 - First Song.dsf"));

        // Braces originating *inside* metadata are literal data, never
        // interpreted as a conditional, even when the template has one.
        job.programs[0].tracks[0].tags.album = Some("Plain {Live} Album".into());
        job.naming_template = "%ALBUM% {%TITLE_EXTRA%}/%TRACKNN% - %TITLE%".into();
        assert_eq!(proposed_destination(&job,&job.programs[0],&job.programs[0].tracks[0]),
            job.destination.join("Stereo/Plain {Live} Album/01 - First Song.dsf"));

        job.naming_template = "%DISC%/%ALBUM%/%TRACKNN% - %TITLE%.%EXTENSION%".into();
        assert_eq!(proposed_destination(&job,&job.programs[0],&job.programs[0].tracks[0]),
            job.destination.join("Stereo/1/Plain {Live} Album/01 - First Song.dsf"),
            "literal subfolders and the explicit extension must resolve exactly once");
        // A metadata path separator is never promoted into a template slash.
        job.programs[0].tracks[0].tags.album = Some("../AC/DC".into());
        let path = proposed_destination(&job,&job.programs[0],&job.programs[0].tracks[0]);
        assert!(path.starts_with(&job.destination));
        assert_eq!(path, job.destination.join("Stereo/1/AC DC/01 - First Song.dsf"));
    }

    #[test]
    fn r29_d05_convert_saved_preset_uses_convert_metadata_normalization() {
        let temp = tempfile::tempdir().unwrap();
        let mut job = fixture_job(&temp.path().join("output"), temp.path());
        let template = "%ARTIST%/%ALBUM% {%TITLE_EXTRA%}/%TRACKNN% - %TITLE%.%EXTENSION%";
        job.naming_template = template.into();
        job.programs[0].tags.artist = Some("AC/DC".into());
        let track = &mut job.programs[0].tracks[0];
        track.tags.album = Some("Aja (Japan / SHM SACD ISO)".into());
        track.tags.title = Some("First Song".into());

        let path = proposed_destination(&job, &job.programs[0], &job.programs[0].tracks[0]);
        let expected_relative = Path::new("AC DC/Aja {Japan  SHM SACD}/01 - First Song.dsf");
        assert_eq!(path, job.destination.join("Stereo").join(expected_relative));
        assert!(path.starts_with(&job.destination), "metadata must stay below the output root");
        assert!(!path.to_string_lossy().ends_with(".dsf.dsf"));

        // Compare the same saved template through Convert's renderer, with
        // Convert's token values. The production Convert track renderer uses
        // these very sanitizers for artist, album, title and TITLE_EXTRA.
        let (album, qualifier) = super::super::stages::extract_title_extra(
            "Aja (Japan / SHM SACD ISO)").unwrap();
        let qualifier = super::super::stages::normalize_title_extra_value(&qualifier, true, false);
        let convert_tokens = BTreeMap::from([
            ("ARTIST".to_owned(), super::super::stages::sanitize_component("AC/DC")),
            ("ALBUM".to_owned(), super::super::stages::sanitize_component(&album)),
            ("TITLE_EXTRA".to_owned(), super::super::stages::sanitize_title_extra_component(&qualifier)),
            ("TRACKNN".to_owned(), "01".into()),
            ("TITLE".to_owned(), super::super::stages::sanitize_component("First Song")),
            ("EXTENSION".to_owned(), "dsf".into()),
        ]);
        let convert_rendered = super::super::stages::conversion_action_render_template(
            template, &convert_tokens).unwrap();
        assert_eq!(expected_relative, Path::new(&convert_rendered));
        assert_eq!(path.strip_prefix(job.destination.join("Stereo")).unwrap(),
            Path::new(&convert_rendered));
    }

    #[test]
    fn r29_d05_metadata_slashes_do_not_create_folders_or_change_legacy_jobs() {
        let temp = tempfile::tempdir().unwrap();
        let mut job = fixture_job(&temp.path().join("output"), temp.path());
        job.programs[0].tags.artist = Some("AC/DC".into());
        job.programs[0].tracks[0].tags.title = Some("Song/Part".into());
        job.programs[0].tracks[0].tags.album = Some("Aja / Japan".into());
        job.naming_template = "%ARTIST%/%ALBUM%/%TRACKNN% - %TITLE%.%EXTENSION%".into();
        assert_eq!(proposed_destination(&job, &job.programs[0], &job.programs[0].tracks[0]),
            job.destination.join("Stereo/AC DC/Aja Japan/01 - Song Part.dsf"),
            "only literal slashes in the template may introduce subfolders");
        assert!(validate_job(&job).is_ok());

        // Names that differ before normalization but resolve to one path
        // must still fail the existing preflight, regardless of case policy.
        job.naming_template = "%TITLE%.%EXTENSION%".into();
        let mut second = job.programs[0].tracks[0].clone();
        second.number = 2;
        second.tags.title = Some("Song Part".into());
        job.programs[0].tracks.push(second);
        for policy in [NativeOverwritePolicy::Fail, NativeOverwritePolicy::Skip,
            NativeOverwritePolicy::Replace] {
            job.overwrite_policy = policy;
            assert!(validate_job(&job).unwrap_err().contains("Two selected artifacts"),
                "normalized collision must be rejected under {policy:?}");
        }

        // Existing recoverable {track} patterns retain native underscores.
        job.programs[0].tracks.truncate(1);
        job.programs[0].tracks[0].tags.title = Some("First Song".into());
        job.naming_template = "{artist} - {track} - {title}".into();
        assert_eq!(proposed_destination(&job, &job.programs[0], &job.programs[0].tracks[0]),
            job.destination.join("Stereo/AC_DC - 01 - First Song.dsf"));
    }

    #[test]
    fn r28_case_insensitive_collision_preflight_rejects_all_policies_and_preserves_unique_outputs() {
        let temp = tempfile::tempdir().unwrap();
        let mut job = fixture_job(&temp.path().join("output"), temp.path());
        job.naming_template = "%TITLE%.%EXTENSION%".into();
        job.programs[0].tracks[0].tags.title = Some("Song".into());
        let mut second = job.programs[0].tracks[0].clone();
        second.number = 2;
        second.tags.title = Some("song".into());
        job.programs[0].tracks.push(second);
        let a = proposed_destination(&job,&job.programs[0],&job.programs[0].tracks[0]);
        let b = proposed_destination(&job,&job.programs[0],&job.programs[0].tracks[1]);
        assert_ne!(a,b,"display names retain their original spelling");
        assert_eq!(native_destination_collision_key(&a),native_destination_collision_key(&b));
        for policy in [NativeOverwritePolicy::Fail, NativeOverwritePolicy::Skip, NativeOverwritePolicy::Replace] {
            job.overwrite_policy = policy;
            assert!(validate_job(&job).unwrap_err().contains("Two selected artifacts"),
                "case-only collision under {policy:?} must be rejected before work");
        }
        job.naming_template = "%TRACKNUMBER% - %TITLE%.%EXTENSION%".into();
        assert!(validate_job(&job).is_ok(),"numbering distinguishes the two outputs");
        job.naming_template = "%TITLE%.%EXTENSION%".into();
        job.programs[0].tracks[1].tags.title = Some("Different Song".into());
        assert!(validate_job(&job).is_ok(),"genuinely distinct titles must be admitted");

        job.programs[0].tracks.truncate(1);
        let mut other = job.programs[0].clone();
        other.label = "Surround".into();
        other.tracks[0].tags.title = Some("Song".into());
        job.programs.push(other);
        assert!(validate_job(&job).is_ok(),"distinct program subfolders distinguish outputs");
        job.programs[1].label = "stereo".into();
        assert!(validate_job(&job).is_err(),"case-only program-folder names collide");

        // Continuous files and their optional CUE artifacts go through the
        // same planned-publication namespace, not the track-only guard.
        job.file_per_program = true;
        job.naming_template = "%TITLE%.%EXTENSION%".into();
        for policy in [NativeOverwritePolicy::Fail, NativeOverwritePolicy::Skip, NativeOverwritePolicy::Replace] {
            job.overwrite_policy = policy;
            assert!(validate_job(&job).unwrap_err().contains("Two selected artifacts"),
                "case-only continuous targets under {policy:?} must be rejected");
        }
    }

    #[test]
    fn r27_disc_identity_tokens_use_authored_ids_not_guessed_tags() {
        let temp = tempfile::tempdir().unwrap();
        let mut job = fixture_job(&temp.path().join("output"), temp.path());
        job.format = DiscFormat::DvdVideo;
        job.programs[0].id = PresentationId::dvd_video(1, 9, 2);
        job.programs[0].angle = 3;
        job.naming_template = "%DVDTITLE%-%STREAM%-%ANGLE%-%TRACKNUMBER%".into();
        let track = &job.programs[0].tracks[0];
        assert_eq!(proposed_destination(&job, &job.programs[0], track),
            job.destination.join("Stereo/9-3-3-01.wav"));
        job.file_per_program = true;
        job.naming_template = "%DVDTITLE%-%STREAM%-%ANGLE%".into();
        assert_eq!(proposed_destination_for_program(&job, &job.programs[0]),
            job.destination.join("Stereo/9-3-3.wav"));
    }

    #[test]
    fn r27_old_continuous_default_keeps_its_recoverable_filename() {
        let temp = tempfile::tempdir().unwrap();
        let mut job = fixture_job(&temp.path().join("output"), temp.path());
        job.file_per_program = true;
        let expected = job.destination.join("Stereo/Stereo.dsf");
        assert_eq!(proposed_destination_for_program(&job, &job.programs[0]), expected);
        job.naming_template = "{track} - {title}".into();
        assert_eq!(proposed_destination_for_program(&job, &job.programs[0]), expected,
            "an older durable extraction must not change its publication target");
        job.naming_template = "{title}.dsf".into();
        assert_eq!(proposed_destination_for_program(&job,&job.programs[0]),
            job.destination.join("Stereo/Stereo.dsf.dsf"),
            "a pre-round-3 explicit extension stays intact in a durable job");
        job.file_per_program = false;
        job.program_subfolders = false;
        assert_eq!(proposed_destination(&job,&job.programs[0],&job.programs[0].tracks[0]),
            job.destination.join("Stereo - one.dsf.dsf"),
            "legacy flattened names and extension handling remain unchanged");
    }

    #[test]
    fn r27_untrusted_tag_values_and_literal_parent_segments_stay_under_folder() {
        let temp = tempfile::tempdir().unwrap();
        let mut job = fixture_job(&temp.path().join("output"), temp.path());
        job.naming_template = "../../%ARTIST%/%TRACKNUMBER% - %TITLE%".into();
        job.programs[0].tags.artist = Some("../outside\\folder".into());
        job.programs[0].tracks[0].name = "../../escape".into();
        let dest = proposed_destination(&job, &job.programs[0], &job.programs[0].tracks[0]);
        assert!(dest.starts_with(&job.destination), "{dest:?}");
        assert!(!dest.components().any(|component|
            matches!(component, std::path::Component::ParentDir)), "{dest:?}");
        assert_eq!(dest.extension().and_then(|ext| ext.to_str()), Some("dsf"));
        // Old recoverable jobs keep their original brace-template semantics.
        job.naming_template = "{track} - {title}".into();
        assert_eq!(proposed_destination(&job, &job.programs[0], &job.programs[0].tracks[0]),
            job.destination.join("Stereo/01 - .._.._escape.dsf"));
        job.naming_template = "100% - {track} - {title}".into();
        assert_eq!(proposed_destination(&job, &job.programs[0], &job.programs[0].tracks[0]),
            job.destination.join("Stereo/100% - 01 - .._.._escape.dsf"));
        job.naming_template = "%ARTIST%/%TRACKNUMBER% - %TITLE%".into();
        job.program_subfolders = false;
        assert_eq!(proposed_destination(&job, &job.programs[0], &job.programs[0].tracks[0]),
            job.destination.join("outside folder/Stereo - 01 - .. .. escape.dsf"));
    }

    #[test]
    fn extracted_sacd_filename_uses_the_resolved_tag_authority() {
        use crate::disc::model::{
            AudioPresentationFormat, CopyProtectionSummary, DiscContents, DiscExtractTagFields,
            DiscPresentation, DiscTrack, FormatProvenance, PresentationExtractInfo, SacdAreaId,
        };
        use crate::tui::disc_browser::DiscBrowserState;

        // The navigation title remains sidecar-first, independently of the
        // user's extraction-time source priority.
        let contents = DiscContents {
            format: DiscFormat::Sacd,
            label: "Disc".into(),
            source_path: "disc.iso".into(),
            presentations: vec![DiscPresentation {
                extract: PresentationExtractInfo {
                    authored_album_tags: DiscExtractTagFields {
                        year: Some("2024".into()), ..Default::default()
                    },
                    sidecar_album_tags: DiscExtractTagFields {
                        year: Some("2026".into()), ..Default::default()
                    },
                    authored_track_tags: vec![DiscExtractTagFields {
                        title: Some("Original Title".into()), year: Some("2021".into()),
                        ..Default::default()
                    }],
                    sidecar_track_tags: vec![DiscExtractTagFields {
                        title: Some("Alternate Title".into()), year: Some("2025".into()),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                id: PresentationId::SacdArea(SacdAreaId::Stereo),
                label: "DSD Stereo".into(),
                format: AudioPresentationFormat {
                    codec: Some("DSD".into()), sample_rate: Some(2_822_400),
                    bit_depth: Some(1), channels: Some(2),
                    channel_layout: Some("stereo".into()), lossless: true,
                    provenance: FormatProvenance::TocHeader,
                },
                tracks: vec![DiscTrack {
                    number: 1, title: Some("Alternate Title".into()),
                    performer: None, duration_secs: Some(60.0), format_note: None,
                }],
                total_duration_secs: 60.0,
                album_title: None, album_artist: None, genre: None, year: None,
            }],
            suppressed: Vec::new(),
            copy_protection: CopyProtectionSummary { description: String::new() },
            diagnostics: Vec::new(),
            album_title: None, album_artist: None, genre: None, year: None,
        };
        let mut state = DiscBrowserState::new(contents, "disc.iso".into());
        state.tags = true;
        state.naming_template = "{track} - {title} - {year}".into();
        assert_eq!(state.tag_source, NativeTagSource::DiscAndSidecar);

        for (priority, title, year) in [
            (NativeTagSource::AuthoredTrack, "Original Title", "2021"),
            (NativeTagSource::DiscAndSidecar, "Alternate Title", "2025"),
        ] {
            state.tag_source = priority;
            let job = state.native_job("/tmp".into());
            assert!(job.transfer_tags);
            let program = &job.programs[0];
            let track = &program.tracks[0];
            assert_eq!(track.tags.title.as_deref(), Some(title));
            assert_eq!(track.tags.year.as_deref(), Some(year));
            assert_eq!(proposed_destination(&job, program, track).file_name().unwrap()
                .to_string_lossy().into_owned(), format!("01 - {title} - {year}.dsf"));
            let entries = selected_tag_items(&track.tags, job.tag_fields);
            assert!(entries.contains(&(lofty::tag::ItemKey::TrackTitle, Some(title.into()))));
            assert!(entries.contains(&(lofty::tag::ItemKey::Year, Some(year.into()))));
        }

        // Track metadata falls back to its authority's album year. When both
        // authorities lack a title, naming falls back to the navigation title.
        state.contents.presentations[0].extract.authored_track_tags[0].year = None;
        state.contents.presentations[0].extract.authored_track_tags[0].title = None;
        state.contents.presentations[0].extract.sidecar_track_tags[0].title = None;
        state.tag_source = NativeTagSource::AuthoredTrack;
        let job = state.native_job("/tmp".into());
        let program = &job.programs[0];
        let track = &program.tracks[0];
        assert_eq!(proposed_destination(&job, program, track).file_name().unwrap()
            .to_string_lossy().into_owned(), "01 - Alternate Title - 2024.dsf");
        assert!(track.tags.title.is_none());
        assert_eq!(track.tags.year.as_deref(), Some("2024"));

        // Non-SACD navigation fallback and program-only dates stay supported.
        let mut non_sacd = program.clone();
        non_sacd.id = PresentationId::dvd_video(1, 1, 0);
        non_sacd.tags.year = Some("2001".into());
        let mut chapter = track.clone();
        chapter.name = "Chapter 1".into();
        chapter.tags = NativeTagValues::default();
        assert_eq!(render_naming_template("{title} - {year}", &non_sacd, &chapter),
            "Chapter 1 - 2001");
        non_sacd.tags.year = None;
        assert_eq!(name_value("year", &non_sacd, &chapter), "");
    }

    #[test]
    fn cue_positions_round_only_indexes_not_audio() {
        let aligned = cue_for_continuous("Album.wav", &["One".into(), "Three".into()],
            &[640, 1280], 48_000).unwrap();
        assert!(aligned.contains("INDEX 01 00:00:01"));
        let nonaligned = cue_for_continuous("Album.wav", &["One".into(), "Three".into()],
            &[48_001, 1280], 48_000).unwrap();
        assert!(nonaligned.contains("INDEX 01 00:01:00"));
        assert!(nonaligned.contains("audio samples are unchanged"));
        assert_eq!(cue_for_continuous("a.wav", &["one".into()], &[44_100], 44_100)
            .unwrap().matches("INDEX 01").count(), 1);
    }

    #[test]
    fn sanitized_track_names_cannot_escape_destination() {
        for name in ["../../foo", "..", "c:\\folder\\evil", "/etc/passwd", "", "a/b"] {
            let component = sanitize_component(name);
            assert!(!component.contains('/'));
            assert!(!component.contains('\\'));
            assert_ne!(component, "..");
        }
    }
    #[tokio::test]
    async fn invalid_working_storage_never_creates_any_destination_entry() {
        let temp = tempfile::tempdir().unwrap();
        let destination = temp.path().join("requested-output");
        let scratch = destination.join("scratch-should-not-exist");
        let job = NativeExtractionJob { cue_input_paths: Vec::new(), cue_segments: Vec::new(), archive_members:Vec::new(), archive_length:0, archive_modified:None, archive_password:None, disc_identity: Default::default(),
            source: std::env::current_exe().unwrap(),
            format: DiscFormat::Sacd,
            programs: vec![NativeProgramChoice {
                id: PresentationId::SacdArea(crate::disc::SacdAreaId::Stereo),
                label: "Stereo".into(),
                tracks: vec![NativeTrackChoice { duration_secs: None, number: 1, name: "one".into(), tags: NativeTagValues::default(), estimated_bytes: None }],
                keep_dst: false,
                source_depth: Some(1), source_codec: "unknown".into(),
                source_channels: Some(2),
                source_rate: Some(2_822_400),
                angle: 1, source_layout: None, is_lossless: true, tags: NativeTagValues::default(), authored_tags: None,
            }],
            destination: destination.clone(),
            working_root: scratch,
            wav_container: NativeWavContainer::Automatic,
            assume_decrypted: false,
            downmix: DvdaDownmixPolicy::None,
            naming_template:default_naming_template(),program_subfolders:true,
            overwrite_policy:NativeOverwritePolicy::Fail,extraction_log:false, options: NativeExtractSettings {create_cue:false, write_chapters:false, ..Default::default()}, program_log_paths: Vec::new(), round4_layout: false,
            file_per_program:false,cue:false,
            transfer_tags:false,sacd_dff: false, tag_source:default_tag_source(),tag_fields:default_tag_fields(),tag_artwork:false,
        };
        let error = execute(job, CancellationToken::new(), |_| {}).await.unwrap_err();
        assert!(error.contains("within the destination"), "{error}");
        assert!(!destination.exists());
    }

    #[test]
    fn native_artwork_prefers_existing_source_key_then_direct_cover() {
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path();
        let source = folder.join("disc.iso");
        fs::write(&source, b"iso").unwrap();
        let lower_rank = folder.join("folder.jpg");
        let preferred = folder.join("cover.png");
        fs::write(&lower_rank, b"placeholder image").unwrap();
        fs::write(&preferred, b"placeholder image").unwrap();
        assert_eq!(native_artwork_path(&AlbumMetadata::default(), &source), Some(preferred.clone()));
        let mut album = AlbumMetadata::default();
        album.extra.insert(STAGED_SOURCE_ARTWORK_PATH_EXTRA_KEY.into(),
            lower_rank.to_string_lossy().into_owned());
        assert_eq!(native_artwork_path(&album, &source), Some(lower_rank));
        assert_eq!(native_artwork_path(&AlbumMetadata::default(), &folder.join("absent.iso")), Some(preferred));
    }

    #[test]
    fn speaker_mask_never_assumes_seven_point_one_from_channel_count() {
        assert_eq!(verified_layout_mask("7.1", 8), None);
        assert_eq!(verified_layout_mask("8ch", 8), None);
        assert_eq!(verified_layout_mask("7.1(side)", 8), Some(0x63f));
        assert_eq!(verified_layout_mask("7.1(wide)", 8), Some(0xff));
        assert_eq!(verified_layout_mask("7.1(side)", 6), None);
        assert_eq!(verified_layout_mask("stereo", 2), Some(0x3));
    }

    #[test]
    fn sacd_continuous_dsf_preserves_every_dsd_byte_and_track_boundary() {
        use sacd_rs::dsd_file::{DsdFrameReader, DsfStreamReader};
        for channels in [2u8, 5, 6] {
            let temp = tempfile::tempdir().unwrap();
            let mut pieces = Vec::new();
            let mut expected = Vec::new();
            let mut lengths = Vec::new();
            for i in [0usize, 2] { // nonadjacent authored track choices
                let path = temp.path().join(format!("part-{i}.dsf"));
                let mut data = Vec::new();
                for n in 0..(1024 * (i + 1) * usize::from(channels)) {
                    data.push(((n * 37 + i * 19) % 256) as u8);
                }
                let mut writer = sacd_rs::dsf_writer::DsfWriter::new(
                    File::create(&path).unwrap(), channels, 2_822_400).unwrap();
                writer.write_interleaved(&data).unwrap();
                writer.finish().unwrap();
                lengths.push((data.len() / usize::from(channels) * 8) as u64);
                expected.extend(data);
                pieces.push(path);
            }
            let output = temp.path().join("joined.dsf");
            let (counts, rate) = concatenate_sacd_tracks(&pieces, &output, false,
                &CancellationToken::new()).unwrap();
            assert_eq!(counts, lengths);
            assert_eq!(rate, 2_822_400);
            let mut joined = DsfStreamReader::new(File::open(&output).unwrap()).unwrap();
            let mut actual = Vec::new();
            while let Some(frame) = joined.next_dsd_frame().unwrap() {
                actual.extend(frame.data);
            }
            assert_eq!(actual, expected, "DSD content changed for {channels} channels");
        }
    }

    #[test]
    fn r30_plain_dff_continuous_join_preserves_interleaved_bytes_and_sample_counts() {
        use sacd_rs::dsd_file::{DsdFrameReader, DsdDsdiffStreamReader};
        for channels in [2u8, 5, 6] {
            let temp = tempfile::tempdir().unwrap();
            let mut parts = Vec::new();
            let mut expected = Vec::new();
            let mut sample_counts = Vec::new();
            // Nonadjacent authored tracks with different lengths exercise the
            // exact same sequential join route as an area-level extraction.
            for index in [0usize, 2] {
                let path = temp.path().join(format!("part-{index}.dff"));
                let data: Vec<u8> = (0..4704 * (index + 1) * usize::from(channels))
                    .map(|n| ((n * 37 + index * 19) % 251) as u8).collect();
                let mut writer = sacd_rs::dff_writer::DffWriter::new(
                    File::create(&path).unwrap(), channels, 2_822_400).unwrap();
                writer.write_frame(&data).unwrap();
                writer.finish().unwrap();
                sample_counts.push((data.len() / usize::from(channels) * 8) as u64);
                expected.extend_from_slice(&data);
                parts.push(path);
            }
            let output = temp.path().join("continuous.dff");
            let (counts, rate) = concatenate_sacd_tracks_checked(
                &parts, &output, true, false, &CancellationToken::new(), || Ok(()),
            ).unwrap();
            assert_eq!(counts, sample_counts, "{channels} channels");
            assert_eq!(rate, 2_822_400);
            let mut reader = DsdDsdiffStreamReader::new(File::open(&output).unwrap()).unwrap();
            assert_eq!(reader.info().channel_count, u16::from(channels));
            assert_eq!(reader.info().sample_rate, rate);
            let mut actual = Vec::new();
            while let Some(frame) = reader.next_dsd_frame().unwrap() {
                actual.extend_from_slice(&frame.data);
            }
            assert_eq!(actual, expected, "DFF altered DSD audio for {channels} channels");
            assert!(reader.next_dsd_frame().unwrap().is_none());
        }
    }

    #[test]
    fn r30_plain_dff_join_rejects_incompatible_clocks_before_publication() {
        let temp = tempfile::tempdir().unwrap();
        let mut parts = Vec::new();
        for (i, rate) in [2_822_400, 5_644_800].into_iter().enumerate() {
            let path = temp.path().join(format!("part-{i}.dff"));
            let mut writer = sacd_rs::dff_writer::DffWriter::new(
                File::create(&path).unwrap(), 2, rate).unwrap();
            writer.write_frame(&[0xa5; 9408]).unwrap();
            writer.finish().unwrap();
            parts.push(path);
        }
        let error = concatenate_sacd_tracks_checked(
            &parts, &temp.path().join("private.dff"), true, false,
            &CancellationToken::new(), || Ok(()),
        ).unwrap_err();
        assert!(error.contains("different channel count or DSD clock"), "{error}");
    }

    #[test]
    fn r30_plain_dff_extension_tags_and_legacy_recovery_defaults() {
        let temp = tempfile::tempdir().unwrap();
        let mut job = fixture_job(&temp.path().join("out"), temp.path());
        let program = &job.programs[0];
        let track = &program.tracks[0];
        assert_eq!(extraction_extension(&job, program), "dsf");
        assert_eq!(proposed_destination(&job, program, track).extension().and_then(|ext| ext.to_str()), Some("dsf"));
        let mut old = serde_json::to_value(&job).unwrap();
        old.as_object_mut().unwrap().remove("sacd_dff");
        let legacy: NativeExtractionJob = serde_json::from_value(old).unwrap();
        assert!(!legacy.sacd_dff, "older queued jobs retain their DSF selection");
        job.sacd_dff = true;
        assert_eq!(extraction_extension(&job, &job.programs[0]), "dff");
        assert_eq!(proposed_destination(&job, &job.programs[0], &job.programs[0].tracks[0])
            .extension().and_then(|ext| ext.to_str()), Some("dff"));
        let values = NativeTagValues {
            catalog: Some("CAT-123".into()), publisher: Some("A label".into()),
            ..Default::default()
        };
        let tagged = selected_tag_items(&values, (1 << 7) | (1 << 8));
        assert_eq!(tagged, vec![
            (lofty::tag::ItemKey::CatalogNumber, Some("CAT-123".into())),
            (lofty::tag::ItemKey::Publisher, Some("A label".into())),
        ]);
        assert!(selected_tag_items(&values, 0x7f).is_empty(),
            "existing seven-field presets must not silently add new tags");
    }

    #[test]
    fn r30d05_non_sacd_source_precedence_controls_individual_and_grouped_tags() {
        use lofty::tag::ItemKey;

        for format in [DiscFormat::DvdAudio, DiscFormat::DvdVideo, DiscFormat::BluRay] {
            let temp = tempfile::tempdir().unwrap();
            let mut job = fixture_job(&temp.path().join("out"), temp.path());
            job.format = format;
            job.tag_fields = 0x1ff;
            let program = &mut job.programs[0];
            program.tags = NativeTagValues {
                album: Some("Navigation album (already merged)".into()),
                artist: Some("Navigation artist".into()),
                ..Default::default()
            };
            program.tracks[0].tags = NativeTagValues {
                title: Some("Navigation track (already merged)".into()),
                ..Default::default()
            };
            // Independent authored facts, not guessed from navigation fields.
            program.authored_tags = Some(NativeAuthoredTags {
                album: NativeTagValues {
                    album: Some("Authored album".into()),
                    artist: Some("Authored artist".into()),
                    year: Some("1997".into()),
                    ..Default::default()
                },
                tracks: BTreeMap::from([(1, NativeTagValues {
                    title: Some("Authored track".into()),
                    artist: Some("Authored performer".into()),
                    composer: Some("  ".into()),
                    ..Default::default()
                })]),
            });
            let track = program.tracks[0].clone();
            let saved_track = NativeTagValues {
                title: Some("MusicBrainz track".into()),
                artist: Some("MusicBrainz artist".into()),
                album: Some("MusicBrainz album".into()),
                composer: Some("Sidecar composer".into()),
                catalog: Some("CAT-88".into()),
                ..Default::default()
            };
            let saved_album = AlbumMetadata {
                album: Some("MusicBrainz album".into()),
                album_artist: Some("MusicBrainz artist".into()).into(),
                genre: Some("Jazz".into()).into(),
                date: Some("2026".into()),
                ..Default::default()
            };

            let authored = native_non_sacd_track_tags(format,
                NativeTagSource::AuthoredTrack, program, &track, saved_track.clone());
            assert_eq!(authored.title.as_deref(), Some("Authored track"), "{format:?}");
            assert_eq!(authored.artist.as_deref(), Some("Authored performer"), "{format:?}");
            assert_eq!(authored.album.as_deref(), Some("Authored album"), "{format:?}");
            assert_eq!(authored.year.as_deref(), Some("1997"), "{format:?}");
            assert_eq!(authored.composer.as_deref(), Some("Sidecar composer"), "{format:?}");
            assert_eq!(authored.catalog.as_deref(), Some("CAT-88"), "{format:?}");
            let chosen = selected_tag_items(&authored, job.tag_fields);
            assert!(chosen.contains(&(ItemKey::TrackTitle, Some("Authored track".into()))));
            assert!(chosen.contains(&(ItemKey::Composer, Some("Sidecar composer".into()))));

            let matched = native_non_sacd_track_tags(format,
                NativeTagSource::MusicBrainz, program, &track, saved_track.clone());
            assert_eq!(matched.title.as_deref(), Some("MusicBrainz track"), "{format:?}");
            assert_eq!(matched.album.as_deref(), Some("MusicBrainz album"), "{format:?}");
            assert_eq!(matched.artist.as_deref(), Some("MusicBrainz artist"), "{format:?}");
            assert!(selected_tag_items(&matched, job.tag_fields).contains(&
                (ItemKey::TrackTitle, Some("MusicBrainz track".into()))));
            assert_ne!(selected_tag_items(&authored, job.tag_fields),
                selected_tag_items(&matched, job.tag_fields), "{format:?}");

            let default = native_non_sacd_track_tags(format,
                NativeTagSource::DiscAndSidecar, program, &track, saved_track);
            assert_eq!(default.title.as_deref(), Some("Navigation track (already merged)"),
                "preserve the earlier navigation-based default for {format:?}");

            // Old recovery jobs and sources without independently observed
            // authored tags must not mistake sidecar-merged navigation for
            // authored metadata.
            let separate = program.authored_tags.take();
            let fallback = native_non_sacd_track_tags(format,
                NativeTagSource::AuthoredTrack, program, &track, matched.clone());
            assert_eq!(fallback.title.as_deref(), Some("MusicBrainz track"), "{format:?}");
            program.authored_tags = separate;

            let authored_group = native_non_sacd_album_tags(
                NativeTagSource::AuthoredTrack, program, &saved_album);
            let matched_group = native_non_sacd_album_tags(
                NativeTagSource::MusicBrainz, program, &saved_album);
            assert_eq!(authored_group.album.as_deref(), Some("Authored album"), "{format:?}");
            assert_eq!(authored_group.artist.as_deref(), Some("Authored artist"), "{format:?}");
            assert_eq!(authored_group.genre.as_deref(), Some("Jazz"), "{format:?}");
            assert_eq!(matched_group.album.as_deref(), Some("MusicBrainz album"), "{format:?}");
            assert_eq!(matched_group.year.as_deref(), Some("2026"), "{format:?}");
            assert!(selected_tag_items(&authored_group, job.tag_fields).contains(&
                (ItemKey::AlbumTitle, Some("Authored album".into()))));
            assert!(selected_tag_items(&matched_group, job.tag_fields).contains(&
                (ItemKey::AlbumTitle, Some("MusicBrainz album".into()))));
            assert_ne!(selected_tag_items(&authored_group, job.tag_fields),
                selected_tag_items(&matched_group, job.tag_fields), "{format:?}");
            assert!(authored_group.title.is_none() && matched_group.title.is_none(),
                "grouped output must not inherit an individual Title");
        }
    }

    #[test]
    fn r30d05_sacd_metadata_selection_is_unchanged() {
        let temp = tempfile::tempdir().unwrap();
        let mut job = fixture_job(&temp.path().join("out"), temp.path());
        job.format = DiscFormat::Sacd;
        let program = &mut job.programs[0];
        program.tags.album = Some("SACD album".into());
        program.tracks[0].tags.title = Some("SACD track".into());
        for source in [NativeTagSource::DiscAndSidecar,
            NativeTagSource::AuthoredTrack, NativeTagSource::MusicBrainz] {
            // SACD jobs already have their selected authority resolved in
            // track/program tags by DiscBrowserState::native_program_choice.
            assert_eq!(native_selected_track_tags(DiscFormat::Sacd, source,
                program, &program.tracks[0]).unwrap().title.as_deref(), Some("SACD track"));
        }
    }

    #[test]
    fn r30d05_legacy_job_without_independent_authored_snapshot_recovers() {
        let temp = tempfile::tempdir().unwrap();
        let mut job = fixture_job(&temp.path().join("out"), temp.path());
        job.format = DiscFormat::DvdVideo;
        job.tag_source = NativeTagSource::AuthoredTrack;
        job.programs[0].authored_tags = Some(NativeAuthoredTags {
            album: NativeTagValues { album: Some("Disc album".into()), ..Default::default() },
            ..Default::default()
        });
        let mut legacy = serde_json::to_value(&job).unwrap();
        assert!(legacy["programs"][0].as_object_mut().unwrap()
            .remove("authored_tags").is_some());
        let recovered: NativeExtractionJob = serde_json::from_value(legacy).unwrap();
        assert!(recovered.programs[0].authored_tags.is_none());
        assert_eq!(recovered.tag_source, NativeTagSource::AuthoredTrack);
        let album = AlbumMetadata { album: Some("Saved album".into()), ..Default::default() };
        assert_eq!(native_non_sacd_album_tags(recovered.tag_source,
            &recovered.programs[0], &album).album.as_deref(), Some("Saved album"));
    }

    #[test]
    fn sacd_continuous_dst_preserves_encoded_frames_and_valid_container() {
        use sacd_rs::dsd_file::{DstDsdiffStreamReader, DstFrameReader};
        for channels in [2u8, 5, 6] {
            let temp = tempfile::tempdir().unwrap();
            let mut pieces = Vec::new();
            let mut originals = Vec::new();
            for i in 0..2usize {
                let path = temp.path().join(format!("part-{i}.dff"));
                let data = vec![(i as u8).wrapping_mul(13); 4704 * usize::from(channels)];
                let encoded = sacd_rs::dst::encode_uncompressed_frame_interleaved(&data, channels).unwrap();
                let mut writer = sacd_rs::dff_dst_writer::DffDstWriter::new(
                    File::create(&path).unwrap(), channels, 2_822_400).unwrap();
                writer.write_passthrough_frame(&encoded, &data).unwrap();
                writer.finish().unwrap();
                pieces.push(path);
                originals.push(encoded);
            }
            let output = temp.path().join("joined.dff");
            let (samples, rate) = concatenate_sacd_tracks(&pieces, &output, true,
                &CancellationToken::new()).unwrap();
            assert_eq!(rate, 2_822_400);
            assert_eq!(samples, vec![37_632, 37_632]);
            let mut reader = DstDsdiffStreamReader::new(File::open(&output).unwrap()).unwrap();
            for encoded in originals {
                assert_eq!(reader.next_dst_frame().unwrap().unwrap().encoded, encoded);
            }
            assert!(reader.next_dst_frame().unwrap().is_none());
        }
    }

    #[test]
    fn continuous_join_cancellation_never_produces_a_complete_output() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("part.dsf");
        let mut writer = sacd_rs::dsf_writer::DsfWriter::new(
            File::create(&source).unwrap(), 2, 2_822_400).unwrap();
        writer.write_interleaved(&[0x96; 512]).unwrap();
        writer.finish().unwrap();
        let result = concatenate_sacd_tracks(&[source], &temp.path().join("joined.dsf"), false, &cancel);
        assert!(result.unwrap_err().contains("cancelled"));
    }

    #[test]
    fn reject_duplicate_track_selection_before_io() {
        let job = NativeExtractionJob { cue_input_paths: Vec::new(), cue_segments: Vec::new(), archive_members:Vec::new(), archive_length:0, archive_modified:None, archive_password:None, disc_identity: Default::default(), source: std::env::current_exe().unwrap(), format: DiscFormat::Sacd,
            programs: vec![NativeProgramChoice { id: PresentationId::SacdArea(crate::disc::SacdAreaId::Stereo),
                label: "Stereo".into(), tracks: vec![NativeTrackChoice{ duration_secs: None,number:1,name:"a".into(),tags:NativeTagValues::default(),estimated_bytes:None},
                NativeTrackChoice{ duration_secs: None,number:1,name:"b".into(),tags:NativeTagValues::default(),estimated_bytes:None}], keep_dst:false,source_depth:Some(1), source_codec: "unknown".into(),
                source_channels:Some(2), source_rate:Some(2822400), angle:1,source_layout:None, is_lossless:true, tags:NativeTagValues::default(), authored_tags: None }],
            destination:"/tmp/dest".into(),working_root:"/tmp".into(),
            wav_container:NativeWavContainer::Automatic,assume_decrypted:false,downmix:DvdaDownmixPolicy::None,
            naming_template:default_naming_template(),program_subfolders:true,
            overwrite_policy:NativeOverwritePolicy::Fail,extraction_log:false, options: NativeExtractSettings {create_cue:false, write_chapters:false, ..Default::default()}, program_log_paths: Vec::new(), round4_layout: false,file_per_program:false,cue:false,
            transfer_tags:false,sacd_dff: false, tag_source:default_tag_source(),tag_fields:default_tag_fields(),tag_artwork:false };
        assert!(validate_job(&job).unwrap_err().contains("duplicate"));
    }
}

#[cfg(test)]
mod native_file_task_controls_tests {
    use super::*;

    #[test]
    fn skip_current_only_cancels_an_active_item() {
        let controls = NativeExtractionControl::default();
        let job = CancellationToken::new();
        controls.skip_current(); // No active item: must not skip the next.
        let first = controls.begin_item(&job);
        assert!(!first.is_cancelled());
        controls.skip_current();
        assert!(first.is_cancelled());
        assert!(!job.is_cancelled());
        assert!(controls.finish_item());
        let next = controls.begin_item(&job);
        assert!(!next.is_cancelled());
        assert!(!controls.finish_item());
    }

    #[tokio::test]
    async fn pause_blocks_a_stage_until_resume_without_cancelling_job() {
        let controls = Arc::new(NativeExtractionControl::default());
        let job = CancellationToken::new();
        let item = controls.begin_item(&job);
        controls.pause();
        let task_controls = Arc::clone(&controls);
        let task_job = job.clone();
        let task_item = item.clone();
        let wait = tokio::spawn(async move {
            task_controls.wait_ready(&task_job, &task_item).await
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!wait.is_finished(), "paused stage must not advance");
        controls.resume();
        assert!(tokio::time::timeout(Duration::from_secs(2), wait)
            .await.expect("stage should resume").expect("task join").is_ok());
        assert!(!job.is_cancelled());
        controls.finish_item();
    }

    #[test]
    fn skipped_sacd_frame_poll_does_not_cancel_the_extraction_job() {
        let controls = Arc::new(NativeExtractionControl::default());
        let job = CancellationToken::new();
        let item = controls.begin_item(&job);
        controls.pause();
        let worker_controls = Arc::clone(&controls);
        let worker_job = job.clone();
        let worker_item = item.clone();
        let worker = std::thread::spawn(move || {
            worker_controls.wait_blocking(&worker_job, &worker_item)
        });
        std::thread::sleep(Duration::from_millis(90));
        assert!(!worker.is_finished(), "native frame callback must pause");
        controls.skip_current();
        assert!(worker.join().expect("frame callback"));
        assert!(!job.is_cancelled());
        assert!(controls.finish_item());
        controls.resume();
    }

    #[test]
    fn r2d02_audio_verification_requires_producer_receipt_and_detects_tamper() {
        if std::process::Command::new("ffmpeg").arg("-version").output().is_err() {return;}
        let dir=tempfile::tempdir().unwrap();
        let make_wav=|path:&Path,payload:&[u8]| {
            let len=u32::try_from(payload.len()).unwrap();
            let mut wav=b"RIFF".to_vec();
            wav.extend_from_slice(&(36+len).to_le_bytes());
            wav.extend_from_slice(b"WAVEfmt "); wav.extend_from_slice(&16u32.to_le_bytes());
            wav.extend_from_slice(&1u16.to_le_bytes());wav.extend_from_slice(&1u16.to_le_bytes());
            wav.extend_from_slice(&48_000u32.to_le_bytes()); wav.extend_from_slice(&96_000u32.to_le_bytes());
            wav.extend_from_slice(&2u16.to_le_bytes());wav.extend_from_slice(&16u16.to_le_bytes());
            wav.extend_from_slice(b"data");wav.extend_from_slice(&len.to_le_bytes());
            wav.extend_from_slice(payload);fs::write(path,wav).unwrap();
        };
        let original=[0x00u8,0x10,0x00,0x20,0x00,0x30];
        let mutated=[0x00u8,0x10,0x00,0x50,0x00,0x30];
        let complete=dir.path().join("stage.wav");
        let target=dir.path().join("verified.wav");
        let mut job=tests::fixture_job(dir.path(),dir.path());
        job.options.verify_after=true;
        make_wav(&complete,&original);
        let controls=NativeExtractionControl::default();
        let missing=publish_finished_file_checked_observed(&job,&complete,&target,
            || Ok(()),Some(&controls)).unwrap_err();
        assert!(missing.contains("did not capture audio while writing"));
        assert!(!target.exists());
        let mut digest=Sha256::new();digest.update(original);
        controls.record_audio_fingerprint(&complete,digest.finalize().into(),"pcm_s16le");
        // The producer has finished hashing its frames; change staging audio.
        make_wav(&complete,&mutated);
        let err=publish_finished_file_checked_observed(&job,&complete,&target,
            || Ok(()),Some(&controls)).unwrap_err();
        assert!(err.contains("Verification failed"),"{err}");
        assert!(!target.exists(),"a failed check cannot publish a bad final file");
        // Option-off retains the old no-digest path and does not start readback.
        job.options.verify_after=false;
        let plain=dir.path().join("plain.wav");
        make_wav(&complete,&original);
        publish_finished_file_checked_observed(&job,&complete,&plain,
            || Ok(()),Some(&controls)).unwrap();
        assert!(plain.exists());
    }

    #[test]
    fn cd01_verified_replace_keeps_the_old_destination_on_failure() {
        if std::process::Command::new("ffmpeg").arg("-version").output().is_err() {return;}
        let root=tempfile::tempdir().unwrap();
        let stage=root.path().join("private.wav");
        let target=root.path().join("album.wav");
        let mut job=tests::fixture_job(root.path(),root.path());
        job.options.verify_after=true;
        job.overwrite_policy=NativeOverwritePolicy::Replace;
        let wav=|sample:u8| {
            let payload=[sample,0,2,0,3,0,4,0];
            let mut out=b"RIFF".to_vec();
            out.extend_from_slice(&(36u32+payload.len() as u32).to_le_bytes());
            out.extend_from_slice(b"WAVEfmt ");out.extend_from_slice(&16u32.to_le_bytes());
            out.extend_from_slice(&1u16.to_le_bytes());out.extend_from_slice(&1u16.to_le_bytes());
            out.extend_from_slice(&48000u32.to_le_bytes());out.extend_from_slice(&96000u32.to_le_bytes());
            out.extend_from_slice(&2u16.to_le_bytes());out.extend_from_slice(&16u16.to_le_bytes());
            out.extend_from_slice(b"data");out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            out.extend_from_slice(&payload);
            out
        };
        let old=wav(11);
        let desired=wav(22);
        let bad=wav(33);
        let expected={let mut h=Sha256::new();h.update(&desired[44..]);h.finalize().into()};
        fs::write(&target,&old).unwrap();
        fs::write(&stage,&bad).unwrap();
        let controls=NativeExtractionControl::default();
        controls.record_audio_fingerprint(&stage,expected,"pcm_s16le");
        let error=publish_finished_file_checked_observed(&job,&stage,&target,
            ||Ok(()),Some(&controls)).unwrap_err();
        assert!(error.contains("Verification failed"),"{error}");
        assert_eq!(fs::read(&target).unwrap(),old);
        assert!(stage.exists(),"failed verified publication retains private staging");
        // A decoder failure must be just as safe as a nonmatching digest.
        fs::write(&stage,b"not a decodable WAV").unwrap();
        controls.record_audio_fingerprint(&stage,expected,"pcm_s16le");
        assert!(publish_finished_file_checked_observed(&job,&stage,&target,
            ||Ok(()),Some(&controls)).is_err());
        assert_eq!(fs::read(&target).unwrap(),old);
        // When readback passes, Replace replaces the old bytes once.
        fs::write(&stage,&desired).unwrap();
        controls.record_audio_fingerprint(&stage,expected,"pcm_s16le");
        publish_finished_file_checked_observed(&job,&stage,&target,
            ||Ok(()),Some(&controls)).unwrap();
        assert_eq!(fs::read(&target).unwrap(),desired);
        // No destination is published for a failed new-file verification.
        let absent=root.path().join("new.wav");
        fs::write(&stage,&bad).unwrap();
        job.overwrite_policy=NativeOverwritePolicy::Fail;
        controls.record_audio_fingerprint(&stage,expected,"pcm_s16le");
        assert!(publish_finished_file_checked_observed(&job,&stage,&absent,
            ||Ok(()),Some(&controls)).is_err());
        assert!(!absent.exists());
        // Cancellation does not touch an existing Replace destination.
        job.overwrite_policy=NativeOverwritePolicy::Replace;
        controls.record_audio_fingerprint(&stage,expected,"pcm_s16le");
        assert!(publish_finished_file_checked_observed(&job,&stage,&target,
            ||Err(std::io::Error::new(std::io::ErrorKind::Interrupted,"stop")),
            Some(&controls)).is_err());
        assert_eq!(fs::read(&target).unwrap(),desired);
    }

    #[test]
    fn cd02_verified_sacd_dsf_and_dff_joins_use_live_dsd_frames() {
        use sacd_rs::dsd_file::DsdFrameReader;
        let root=tempfile::tempdir().unwrap();
        let source_samples=[0x96u8;1024];
        for dff in [false,true] {
            let source=root.path().join(if dff {"part.dff"}else{"part.dsf"});
            if dff {
                let mut writer=sacd_rs::dff_writer::DffWriter::new(
                    File::create(&source).unwrap(),2,2_822_400).unwrap();
                writer.write_frame(&source_samples).unwrap();
                writer.finish().unwrap();
            } else {
                let mut writer=sacd_rs::dsf_writer::DsfWriter::new(
                    File::create(&source).unwrap(),2,2_822_400).unwrap();
                writer.write_interleaved(&source_samples).unwrap();
                writer.finish().unwrap();
            }
            let joined=root.path().join(if dff {"joined.dff"}else{"joined.dsf"});
            let mut digest=Sha256::new();
            let mut tap=|data:&[u8]|digest.update(data);
            let (counts,rate)=concatenate_sacd_tracks_checked_tapped(
                &[source],&joined,dff,false,&CancellationToken::new(),||Ok(()),Some(&mut tap)).unwrap();
            assert_eq!(rate,2_822_400);
            assert_eq!(counts,vec![4096]);
            let expected: [u8;32]=digest.finalize().into();
            let controls=NativeExtractionControl::default();
            controls.record_dsd_fingerprint(&joined,expected);
            let reference=controls.take_audio_fingerprint(&joined).unwrap();
            assert_eq!(native_readback_fingerprint(&joined,&joined,&reference,&mut ||Ok(())).unwrap(),expected);
            let input=File::open(&joined).unwrap();
            let mut frames=sacd_rs::dsd_file::open_dsd_as_decoded_reader(input).unwrap();
            assert_eq!(frames.next_dsd_frame().unwrap().unwrap().data,source_samples.to_vec());
        }
    }

    #[test]
    fn cd02_archive_identity_receipt_matches_copy_and_detects_mutation() {
        let root=tempfile::tempdir().unwrap();
        let dest=root.path().join("archive_member.mp3");
        let good=b"unchanged archive member audio payload";
        let controls=NativeExtractionControl::default();
        let mut digest=Sha256::new();digest.update(good);
        let expected:[u8;32]=digest.finalize().into();
        controls.record_identity_fingerprint(&dest,expected);
        fs::write(&dest,good).unwrap();
        let receipt=controls.take_audio_fingerprint(&dest).unwrap();
        assert_eq!(native_readback_fingerprint(&dest,&dest,&receipt,&mut ||Ok(())).unwrap(),expected);
        fs::write(&dest,b"changed archive member audio payload").unwrap();
        assert_ne!(native_readback_fingerprint(&dest,&dest,&receipt,&mut ||Ok(())).unwrap(),expected);
    }

    #[test]
    fn r2d01_two_physical_cue_images_assign_the_boundary_to_the_selected_policy() {
        let original=PresentationId::CueAlbum(1);
        let one=PathBuf::from("A.wav");let two=PathBuf::from("B.wav");
        let fixture=|number,image:PathBuf,start,end,total,index00|NativeCueSegment {
            presentation:original,number,cue_path:PathBuf::from("Album.cue"),image,
            start_sample:start,end_sample:end,index01_sample:start,index00_sample:index00,
            pre_emphasis:false,hidden_track_published:false,appended_gap:None,
            total_samples:total,sample_rate:48_000,extension:"wav".into(),
            encoder:"pcm_s16le".into(),muxer:"wav".into(),sample_bits:Some(16),
            channels:1,source_length:0,source_modified:None,cue_length:0,cue_modified:None,
            source_bit_rate:None,packet_cut_samples:Vec::new(),packet_part_index:0,
        };
        let all=vec![fixture(1,one.clone(),0,48_000,48_000,None),
            fixture(2,two.clone(),96_000,144_000,144_000,Some(0))];
        for (choice,begin,extra) in [
            (NativeCueBetweenGap::AppendPrevious,96_000u64,true),
            (NativeCueBetweenGap::PrependNext,0u64,false),
            (NativeCueBetweenGap::Drop,96_000u64,false),
        ] {
            let mut settings=NativeExtractSettings::default();
            settings.between_gap=choice;
            let mut selected=all.clone();
            configure_cue_gap_segments(&all,&mut selected,&settings).unwrap();
            assert_eq!(selected[1].start_sample,begin);
            assert_eq!(selected[0].appended_gap.is_some(),extra);
            assert_eq!(selected[0].appended_gap.as_ref().map(|gap|gap.image.clone()),
                extra.then_some(two.clone()));
            assert_eq!(selected[0].appended_gap.as_ref().map(|gap|gap.end_sample),
                extra.then_some(96_000));
        }
        let mut unsupported=all.clone();
        unsupported[1].sample_rate=44_100;
        let mut settings=NativeExtractSettings::default();
        settings.between_gap=NativeCueBetweenGap::AppendPrevious;
        let mut selected=unsupported.clone();
        assert!(configure_cue_gap_segments(&unsupported,&mut selected,&settings).is_err());
    }

}
