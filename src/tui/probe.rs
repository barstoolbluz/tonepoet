//! Audio file probing via ffmpeg-next (in-process) and metadata reading via lofty

use std::path::Path;
use std::sync::Once;

/// Audio stream information from probing
#[derive(Debug, Clone)]
pub struct SourceInfo {
    pub format_name: String,
    pub codec: String,
    pub bit_depth: Option<u32>,
    pub sample_rate: u32,
    pub channels: u32,
    pub channel_layout: String,
    pub duration_secs: f64,
    pub file_size: u64,
}

/// Metadata tags from the source file
#[derive(Debug, Clone, Default)]
pub struct SourceMetadata {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub genre: Option<String>,
    pub year: Option<String>,

    /// Track number from the tag (e.g. 3 for the third track).
    /// Used by the bulk rename wizard for `%N%` / `%NN%` placeholders.
    pub track_number: Option<u32>,

    /// Catalog number (label/pressing identifier). Read from the
    /// CATALOGNUMBER tag (Vorbis comment) or similar format-specific
    /// field. Used by the bulk rename wizard for `%CATALOG%`.
    pub catalog_number: Option<String>,

    /// Gain/peak values from REPLAYGAIN_* tags. Raw strings as stored in the
    /// file (e.g. `"-6.57 dB"` for gain, `"0.988281"` for peak).
    pub rg_track_gain: Option<String>,
    pub rg_track_peak: Option<String>,
    pub rg_album_gain: Option<String>,
    pub rg_album_peak: Option<String>,

    /// EBU R 128 loudness values from R128_*_GAIN tags (Opus/Vorbis).
    /// Stored as display strings in dB form (e.g. `"-6.50 dB"`).
    /// Computed from the Q7.8 fixed-point integers by dividing by 256.
    pub r128_track_gain: Option<String>,
    pub r128_album_gain: Option<String>,

    /// Pre-emphasis detected via metadata (tags, CUE files, catalog number).
    /// None = not detected. Some(source) = evidence found (e.g. "tag", "CUE file", "catalog (35DP-4)").
    pub preemphasis_metadata: Option<String>,

    /// HDCD detection result from analysis cache. Populated when the file
    /// has been previously analyzed. None = not yet analyzed.
    pub hdcd_detail: Option<String>,

    /// CD ISRC code, when present in the file's tags (typically populated by
    /// EAC reading subchannel data during the rip). Used by CUE generation.
    pub isrc: Option<String>,
}

/// Convert an R128 Q7.8 fixed-point integer (stored as a string) into a
/// human-readable dB string like `"-6.50 dB"`. Returns None on parse failure.
fn r128_raw_to_db(raw: &str) -> Option<String> {
    let parsed: i32 = raw.trim().parse().ok()?;
    let db = parsed as f32 / 256.0;
    Some(format!("{:+.2} dB", db))
}

// Ensure ffmpeg is initialized exactly once
static FFMPEG_INIT: Once = Once::new();

fn ensure_ffmpeg_init() {
    FFMPEG_INIT.call_once(|| {
        ffmpeg_next::init().expect("failed to initialize ffmpeg");
        // Suppress ffmpeg's internal stderr logging (corrupt tags, invalid
        // frames, etc.) which would bleed through and corrupt the TUI.
        ffmpeg_next::util::log::set_level(ffmpeg_next::util::log::Level::Quiet);
    });
}

/// Public accessor for the ffmpeg init guard (used by analyze.rs).
pub fn ensure_ffmpeg_init_pub() {
    ensure_ffmpeg_init();
}

impl SourceInfo {
    /// Format duration as HH:MM:SS
    pub fn duration_display(&self) -> String {
        let total = self.duration_secs as u64;
        let h = total / 3600;
        let m = (total % 3600) / 60;
        let s = total % 60;
        if h > 0 {
            format!("{:02}:{:02}:{:02}", h, m, s)
        } else {
            format!("{:02}:{:02}", m, s)
        }
    }

    /// Format file size for display
    pub fn size_display(&self) -> String {
        let bytes = self.file_size as f64;
        if bytes >= 1_073_741_824.0 {
            format!("{:.1} GB", bytes / 1_073_741_824.0)
        } else if bytes >= 1_048_576.0 {
            format!("{:.1} MB", bytes / 1_048_576.0)
        } else if bytes >= 1024.0 {
            format!("{:.1} KB", bytes / 1024.0)
        } else {
            format!("{} B", self.file_size)
        }
    }

    /// Format sample rate for display
    pub fn sample_rate_display(&self) -> String {
        // DSD rates
        if let Some(dsd) = dsd_rate_name(self.sample_rate) {
            let mhz = self.sample_rate as f64 / 1_000_000.0;
            return format!("{} ({:.1} MHz)", dsd, mhz);
        }

        let khz = self.sample_rate as f64 / 1000.0;
        if khz == khz.floor() {
            format!("{:.0} kHz", khz)
        } else {
            format!("{:.1} kHz", khz)
        }
    }

    /// Format codec and bit depth for display (e.g., "PCM 24-bit")
    pub fn codec_display(&self) -> String {
        // DSD doesn't need "1-bit" suffix — the rate tells the story
        if self.codec == "DSD" {
            return self.codec.clone();
        }
        if let Some(depth) = self.bit_depth {
            format!("{} {}-bit", self.codec, depth)
        } else {
            self.codec.clone()
        }
    }

    /// Channel count as display string
    pub fn channels_display(&self) -> String {
        if !self.channel_layout.is_empty() {
            self.channel_layout.clone()
        } else {
            match self.channels {
                1 => "mono".to_string(),
                2 => "stereo".to_string(),
                n => format!("{} ch", n),
            }
        }
    }
}

/// Map DSD sample rates to friendly names
fn dsd_rate_name(rate: u32) -> Option<&'static str> {
    match rate {
        2_822_400 => Some("DSD64"),
        5_644_800 => Some("DSD128"),
        11_289_600 => Some("DSD256"),
        22_579_200 => Some("DSD512"),
        45_158_400 => Some("DSD1024"),
        _ => None,
    }
}

/// Map ffmpeg format names to friendly display names
fn friendly_format_name(name: &str) -> String {
    // ffmpeg format names can be comma-separated (e.g., "mov,mp4,m4a,3gp,3g2,mj2")
    let primary = name.split(',').next().unwrap_or(name);
    match primary {
        "flac" => "FLAC".to_string(),
        "wav" => "WAV".to_string(),
        "aiff" => "AIFF".to_string(),
        "wv" => "WavPack".to_string(),
        "mp3" => "MP3".to_string(),
        "mov" | "mp4" | "m4a" => "M4A".to_string(),
        "ogg" => "OGG".to_string(),
        "opus" => "Opus".to_string(),
        "dsf" => "DSF".to_string(),
        "iff" | "dff" => "DFF".to_string(),
        "matroska" | "webm" => {
            if name.contains("webm") { "WebM".to_string() }
            else { "MKA".to_string() }
        }
        "w64" => "W64".to_string(),
        "rf64" => "RF64".to_string(),
        "ac3" => "AC3".to_string(),
        "dts" => "DTS".to_string(),
        "ape" => "APE".to_string(),
        "shn" => "SHN".to_string(),
        _ => primary.to_uppercase(),
    }
}

/// Map ffmpeg codec names to friendly display names
fn friendly_codec_name(name: &str) -> String {
    match name {
        "flac" => "FLAC".to_string(),
        "pcm_s16le" | "pcm_s16be" => "PCM".to_string(),
        "pcm_s24le" | "pcm_s24be" => "PCM".to_string(),
        "pcm_s32le" | "pcm_s32be" => "PCM".to_string(),
        "pcm_f32le" | "pcm_f32be" => "PCM Float".to_string(),
        "pcm_f64le" | "pcm_f64be" => "PCM Float".to_string(),
        "alac" => "ALAC".to_string(),
        "aac" => "AAC".to_string(),
        "mp3" | "mp3float" => "MP3".to_string(),
        "vorbis" => "Vorbis".to_string(),
        "opus" => "Opus".to_string(),
        "wavpack" => "WavPack".to_string(),
        "dsd_lsbf" | "dsd_lsbf_planar" => "DSD".to_string(),
        "dsd_msbf" | "dsd_msbf_planar" => "DSD".to_string(),
        "ac3" => "AC3".to_string(),
        "dts" | "dca" => "DTS".to_string(),
        "ape" => "APE".to_string(),
        _ => name.to_uppercase(),
    }
}

/// Probe an audio file using ffmpeg-next (in-process, no subprocess)
pub fn probe_audio(path: &Path) -> Result<SourceInfo, String> {
    ensure_ffmpeg_init();

    let file_size = std::fs::metadata(path)
        .map(|m| m.len())
        .unwrap_or(0);

    let ctx = ffmpeg_next::format::input(&path)
        .map_err(|e| format!("Failed to open '{}': {}", path.display(), e))?;

    // Find the best audio stream
    let stream = ctx
        .streams()
        .best(ffmpeg_next::media::Type::Audio)
        .ok_or_else(|| format!("No audio stream found in '{}'", path.display()))?;

    let time_base = stream.time_base();
    let stream_duration = stream.duration();

    // Get codec parameters
    let codec_params = stream.parameters();
    let codec_ctx = ffmpeg_next::codec::context::Context::from_parameters(codec_params)
        .map_err(|e| format!("Failed to read codec parameters: {}", e))?;

    let audio = codec_ctx.decoder().audio()
        .map_err(|e| format!("Failed to create audio decoder context: {}", e))?;

    let codec_name = audio
        .codec()
        .map(|c| c.name().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let sample_rate = audio.rate();
    let channels = audio.channels() as u32;

    // Bit depth: try bits_per_raw_sample from stream parameters, then sample format
    let bit_depth = {
        // bits_per_raw_sample from codec parameters (most reliable for PCM/FLAC)
        let raw_bits = unsafe {
            let params = stream.parameters().as_ptr();
            (*params).bits_per_raw_sample
        };
        if raw_bits > 0 {
            Some(raw_bits as u32)
        } else {
            // Fall back to sample format byte size
            let fmt = audio.format();
            let bytes = fmt.bytes();
            if bytes > 0 {
                Some((bytes as u32) * 8)
            } else {
                None
            }
        }
    };

    // Channel layout description
    let channel_layout = match channels {
        1 => "mono".to_string(),
        2 => "stereo".to_string(),
        6 => "5.1".to_string(),
        8 => "7.1".to_string(),
        n => format!("{} ch", n),
    };

    // Duration: prefer stream duration, fall back to format duration
    let duration_secs = if stream_duration > 0 {
        stream_duration as f64 * time_base.numerator() as f64 / time_base.denominator() as f64
    } else {
        // Format-level duration (in AV_TIME_BASE units)
        let fmt_dur = ctx.duration();
        if fmt_dur > 0 {
            fmt_dur as f64 / ffmpeg_next::ffi::AV_TIME_BASE as f64
        } else {
            0.0
        }
    };

    // Format name
    let format_name_raw = ctx.format().name().to_string();
    let format_name = friendly_format_name(&format_name_raw);
    let codec = friendly_codec_name(&codec_name);

    // For DSD, bit depth is always 1
    let bit_depth = if codec_name.starts_with("dsd_") {
        Some(1)
    } else {
        bit_depth
    };

    Ok(SourceInfo {
        format_name,
        codec,
        bit_depth,
        sample_rate,
        channels,
        channel_layout,
        duration_secs,
        file_size,
    })
}

/// Read metadata tags from an audio file using lofty
pub fn read_metadata(path: &Path) -> Result<SourceMetadata, String> {
    use lofty::file::TaggedFileExt;
    use lofty::tag::{Accessor, ItemKey};

    let tagged_file = lofty::read_from_path(path)
        .map_err(|e| format!("Failed to read tags from '{}': {}", path.display(), e))?;

    // Try each tag in the file, take the first one that has data
    let tags = tagged_file.tags();

    // R128 ItemKeys (Vorbis comment style; not a dedicated lofty variant).
    let r128_track_key = ItemKey::Unknown("R128_TRACK_GAIN".to_string());
    let r128_album_key = ItemKey::Unknown("R128_ALBUM_GAIN".to_string());

    let mut meta = SourceMetadata::default();
    for tag in tags {
        if meta.title.is_none() {
            meta.title = tag.title().map(|s| s.to_string());
        }
        if meta.artist.is_none() {
            meta.artist = tag.artist().map(|s| s.to_string());
        }
        if meta.album.is_none() {
            meta.album = tag.album().map(|s| s.to_string());
        }
        if meta.genre.is_none() {
            meta.genre = tag.genre().map(|s| s.to_string());
        }
        if meta.year.is_none() {
            meta.year = tag.year().map(|y| y.to_string());
        }
        if meta.track_number.is_none() {
            meta.track_number = tag.track();
        }
        if meta.isrc.is_none() {
            meta.isrc = tag
                .get_string(&ItemKey::Isrc)
                .map(|s| s.to_string());
        }
        if meta.catalog_number.is_none() {
            // CATALOGNUMBER is a Vorbis comment convention; also used in
            // some ID3v2 TXXX frames. Try the standard ItemKey first,
            // then fall back to a freeform lookup.
            meta.catalog_number = tag
                .get_string(&ItemKey::CatalogNumber)
                .map(|s| s.to_string())
                .or_else(|| {
                    tag.get_string(&ItemKey::Unknown("CATALOGNUMBER".to_string()))
                        .map(|s| s.to_string())
                });
        }

        // ReplayGain (raw strings, format-preserving)
        if meta.rg_track_gain.is_none() {
            meta.rg_track_gain = tag
                .get_string(&ItemKey::ReplayGainTrackGain)
                .map(|s| s.to_string());
        }
        if meta.rg_track_peak.is_none() {
            meta.rg_track_peak = tag
                .get_string(&ItemKey::ReplayGainTrackPeak)
                .map(|s| s.to_string());
        }
        if meta.rg_album_gain.is_none() {
            meta.rg_album_gain = tag
                .get_string(&ItemKey::ReplayGainAlbumGain)
                .map(|s| s.to_string());
        }
        if meta.rg_album_peak.is_none() {
            meta.rg_album_peak = tag
                .get_string(&ItemKey::ReplayGainAlbumPeak)
                .map(|s| s.to_string());
        }

        // R128 (Q7.8 fixed-point integer converted to dB on read)
        if meta.r128_track_gain.is_none() {
            meta.r128_track_gain = tag
                .get_string(&r128_track_key)
                .and_then(r128_raw_to_db);
        }
        if meta.r128_album_gain.is_none() {
            meta.r128_album_gain = tag
                .get_string(&r128_album_key)
                .and_then(r128_raw_to_db);
        }
    }

    // Quick pre-emphasis metadata check (tags + CUE/log + catalog).
    meta.preemphasis_metadata = preemphasis_metadata_check(path);

    Ok(meta)
}

/// Public wrapper for PE metadata check (used by browse DB cache path).
pub fn preemphasis_metadata_check_pub(path: &Path) -> Option<String> {
    preemphasis_metadata_check(path)
}

/// Lightweight pre-emphasis check using metadata evidence only (no
/// spectral analysis). Checks tags, CUE/log files in the same directory,
/// and catalog number against the known PE disc database.
fn preemphasis_metadata_check(path: &Path) -> Option<String> {
    use super::preemphasis::metadata::{check_tag_evidence, check_file_evidence};
    use super::preemphasis::catalog::check_catalog_evidence;

    // Tags (fastest).
    if let Some(ev) = check_tag_evidence(path) {
        return Some(ev.label().to_string());
    }
    // CUE and log files in the same directory.
    if let Some(ev) = check_file_evidence(path) {
        return Some(ev.label().to_string());
    }
    // Catalog number matching.
    if let Some(cm) = check_catalog_evidence(path) {
        return Some(format!("catalog ({})", cm.catalog_number));
    }
    None
}

// ── Metadata writing ────────────────────────────────────────────────

/// Which metadata field to edit. Used by TextEditTarget::BrowseMetadata
/// and the context menu's "Edit metadata" submenu.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MetadataField {
    Title,
    Artist,
    Album,
    Genre,
    Year,
}

impl MetadataField {
    /// Human-readable label for the field.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Title => "title",
            Self::Artist => "artist",
            Self::Album => "album",
            Self::Genre => "genre",
            Self::Year => "year",
        }
    }

    /// All fields in display order.
    pub fn all() -> &'static [MetadataField] {
        &[Self::Title, Self::Artist, Self::Album, Self::Genre, Self::Year]
    }
}

/// Write a single metadata field to an audio file's tags via lofty.
///
/// Re-reads the file, modifies the primary tag (creating one if needed),
/// and saves with default `WriteOptions` (preserves padding and other tags).
///
/// Year values must be valid u32; non-numeric input returns an error.
/// Empty strings clear the field (set to None).
pub fn write_metadata_field(
    path: &Path,
    field: MetadataField,
    value: &str,
) -> Result<(), String> {
    use lofty::config::WriteOptions;
    use lofty::file::{AudioFile, TaggedFileExt};
    use lofty::tag::Accessor;

    let mut tagged = lofty::read_from_path(path)
        .map_err(|e| format!("failed to read '{}': {}", path.display(), e))?;

    // Get or create the primary tag for this format.
    let tag = tagged
        .primary_tag_mut()
        .ok_or_else(|| format!("no writable tag for '{}'", path.display()))?;

    let trimmed = value.trim();

    match field {
        MetadataField::Title => {
            if trimmed.is_empty() {
                tag.remove_title();
            } else {
                tag.set_title(trimmed.to_string());
            }
        }
        MetadataField::Artist => {
            if trimmed.is_empty() {
                tag.remove_artist();
            } else {
                tag.set_artist(trimmed.to_string());
            }
        }
        MetadataField::Album => {
            if trimmed.is_empty() {
                tag.remove_album();
            } else {
                tag.set_album(trimmed.to_string());
            }
        }
        MetadataField::Genre => {
            if trimmed.is_empty() {
                tag.remove_genre();
            } else {
                tag.set_genre(trimmed.to_string());
            }
        }
        MetadataField::Year => {
            if trimmed.is_empty() {
                tag.remove_year();
            } else {
                let y: u32 = trimmed
                    .parse()
                    .map_err(|_| format!("year must be a number, got '{}'", trimmed))?;
                tag.set_year(y);
            }
        }
    }

    tagged
        .save_to_path(path, WriteOptions::default())
        .map_err(|e| format!("failed to save tags to '{}': {}", path.display(), e))?;

    Ok(())
}

// ── Full tag enumeration + batch write (metadata editor) ────────────

/// A single tag entry read from an audio file (or merged across files).
#[derive(Debug, Clone)]
pub struct TagEntry {
    /// Display name (e.g. "ARTIST", "TRACKNUMBER", "CUSTOM_FIELD").
    pub display_key: String,
    /// Lofty item key for read/write mapping.
    pub item_key: lofty::tag::ItemKey,
    /// Displayed value: shared value or "<multiple values>" when files disagree.
    pub value: String,
    /// Original displayed value at read/merge time, for display-level dirty.
    pub original: String,
    /// True if the value is binary (non-editable).
    pub is_binary: bool,
    /// True if files have different values for this key.
    pub is_mixed: bool,
    /// Per-file current values (indexed by paths order). Length = 1 for
    /// single-file editing, N for multi-file.
    pub per_file_values: Vec<String>,
    /// Per-file original values at read time (for per-file write diff).
    pub per_file_originals: Vec<String>,
    /// MB-proposed displayed value, when the entry was touched by a
    /// MusicBrainz populate. Lets the editor show a `[revert]` /
    /// `[use MB]` toggle pill that swaps `value` between the file's
    /// pre-populate `original` and the MB suggestion.
    pub mb_proposed_value: Option<String>,
    /// Per-file MB-proposed values, paired with `mb_proposed_value`.
    pub mb_proposed_per_file: Option<Vec<String>>,
}

/// True when this entry's value is too large or structured to render
/// inline in the metadata editor and should display a synthesized
/// summary string instead (currently: CUESHEET, which can carry
/// kilobytes of multi-line CUE content for embedded-cuesheet single
/// image rips).
pub fn is_synthetic_preview(entry: &TagEntry) -> bool {
    entry.display_key.eq_ignore_ascii_case("CUESHEET")
}

/// Build a one-line summary for a synthetic-preview tag value. Used
/// in place of the raw value on the metadata-editor row so a 1-2KB
/// CUESHEET doesn't fill the editor with noise.
pub fn cue_summary_string(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return "[CUE sheet · empty]".to_string();
    }
    let lines = trimmed.lines().count();
    let bytes = trimmed.len();
    let size_str = if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{} B", bytes)
    };
    format!("[CUE sheet · {} lines · {}]", lines, size_str)
}

/// True when at least one entry has been changed from its on-disk
/// original value, or there are pending deletions queued. Used to
/// refresh the editor's `dirty` flag after a revert toggle so the
/// indicator accurately reflects whether anything would be written
/// on save.
pub fn metadata_editor_has_changes(state: &super::app::MetadataEditorState) -> bool {
    !state.deleted.is_empty()
        || state.entries.iter().any(|e|
            e.value != e.original
            || e.per_file_values != e.per_file_originals
        )
}

/// State of the per-row revert toggle pill in the metadata editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MbRevertPill {
    /// Field wasn't touched by MB or was manually edited; no pill.
    None,
    /// Current value is the MB suggestion; pill says `[revert]` and
    /// flips to the file's original value.
    Revert,
    /// Current value is the file's original; pill says `[use MB]` and
    /// flips to the MB suggestion.
    UseMb,
}

/// Decide which pill (if any) to show on a row of the metadata editor.
///
/// `None` when the entry wasn't populated from MB, or when the user
/// has manually edited away from both the MB suggestion and the
/// file's original (toggle would be ambiguous; user can use undo).
pub fn mb_pill_state(entry: &TagEntry) -> MbRevertPill {
    let Some(ref proposed) = entry.mb_proposed_value else {
        return MbRevertPill::None;
    };
    if entry.value == *proposed {
        MbRevertPill::Revert
    } else if entry.value == entry.original {
        MbRevertPill::UseMb
    } else {
        MbRevertPill::None
    }
}

/// Flip a TagEntry between its MB-proposed value and the pre-populate
/// original. No-op when there is no MB-proposed value, or when the
/// user has manually edited away from both endpoints.
///
/// Touches `value`, `per_file_values`, and `is_mixed`. `original` and
/// `mb_proposed_*` are preserved so the toggle can flip again.
pub fn toggle_mb_revert(entry: &mut TagEntry) {
    let proposed = match &entry.mb_proposed_value {
        Some(p) => p.clone(),
        None => return,
    };
    let proposed_per_file = match &entry.mb_proposed_per_file {
        Some(p) => p.clone(),
        None => return,
    };

    if entry.value == proposed {
        // MB → original
        entry.value = entry.original.clone();
        entry.per_file_values = entry.per_file_originals.clone();
    } else if entry.value == entry.original {
        // original → MB
        entry.value = proposed;
        entry.per_file_values = proposed_per_file;
    } else {
        // Manual edit; toggle is ambiguous. No-op.
        return;
    }

    let n = entry.per_file_values.len();
    let all_same = entry.per_file_values.windows(2).all(|w| w[0] == w[1]);
    entry.is_mixed = !all_same && n > 1;
    if entry.is_mixed {
        entry.value = "<multiple values>".to_string();
    }
}

/// Field-level pill state for the metadata-editor *detail* overlay.
/// Operates on `per_file_values` instead of the displayed `value`, so it
/// remains meaningful when the field shows `<multiple values>` in the
/// main editor (where the value-based [`mb_pill_state`] would return
/// `None`).
///
/// Returns:
/// - `Revert` when current per-file values match the MB-proposed set
///   (broadcasting `mb_proposed_value` if `mb_proposed_per_file` is None)
/// - `UseMb` when current per-file values match the pre-MB originals
/// - `None` when MB never touched this field, or the user has manually
///   edited some files (toggle would be ambiguous)
pub fn mb_pill_state_field(entry: &TagEntry) -> MbRevertPill {
    let Some(ref proposed) = entry.mb_proposed_value else {
        return MbRevertPill::None;
    };
    let proposed_per_file: Vec<String> = match &entry.mb_proposed_per_file {
        Some(v) => v.clone(),
        None => vec![proposed.clone(); entry.per_file_values.len()],
    };
    if entry.per_file_values == proposed_per_file {
        MbRevertPill::Revert
    } else if entry.per_file_values == entry.per_file_originals {
        MbRevertPill::UseMb
    } else {
        MbRevertPill::None
    }
}

/// Field-level revert toggle. Swaps `per_file_values` between the
/// MB-proposed set and the pre-MB originals, recomputing `value` and
/// `is_mixed`. No-op when MB never touched this field, or the user has
/// manually edited (state isn't either endpoint).
pub fn toggle_mb_revert_field(entry: &mut TagEntry) {
    let Some(ref proposed) = entry.mb_proposed_value else { return; };
    let proposed_per_file: Vec<String> = match &entry.mb_proposed_per_file {
        Some(v) => v.clone(),
        None => vec![proposed.clone(); entry.per_file_values.len()],
    };

    if entry.per_file_values == proposed_per_file {
        entry.per_file_values = entry.per_file_originals.clone();
    } else if entry.per_file_values == entry.per_file_originals {
        entry.per_file_values = proposed_per_file;
    } else {
        return;
    }

    recompute_aggregate_value(entry);
}

/// Restore action for the detail overlay: discard any per-file user
/// edits and snap `per_file_values` back to the as-retrieved MB
/// proposal. Broadcasts `mb_proposed_value` when `mb_proposed_per_file`
/// is None. No-op when MB never touched the field.
pub fn restore_mb_proposed(entry: &mut TagEntry) {
    let Some(ref proposed) = entry.mb_proposed_value else { return; };
    let proposed_per_file: Vec<String> = match &entry.mb_proposed_per_file {
        Some(v) => v.clone(),
        None => vec![proposed.clone(); entry.per_file_values.len()],
    };
    entry.per_file_values = proposed_per_file;
    recompute_aggregate_value(entry);
}

/// True when MB populated this field, so the detail overlay should
/// surface a [restore] pill.
pub fn entry_has_mb_proposed(entry: &TagEntry) -> bool {
    entry.mb_proposed_value.is_some()
}

/// Re-derive `value` and `is_mixed` from `per_file_values`. Used after
/// any field-level mutation that touches the per-file vector.
fn recompute_aggregate_value(entry: &mut TagEntry) {
    let n = entry.per_file_values.len();
    let all_same = entry.per_file_values.windows(2).all(|w| w[0] == w[1]);
    entry.is_mixed = !all_same && n > 1;
    entry.value = if entry.is_mixed {
        "<multiple values>".to_string()
    } else {
        entry.per_file_values.first().cloned().unwrap_or_default()
    };
}

/// Extract a track number from a filename stem by taking leading digits.
/// Returns 0 if no leading digits found.
/// Examples: "01 - Foo" → 1, "Track 03" → 3, "Foo" → 0.
pub fn extract_track_from_filename(stem: &str) -> u32 {
    let s = stem.trim();
    // Try leading digits first.
    let digits: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
    if !digits.is_empty() {
        return digits.parse().unwrap_or(0);
    }
    // Try "Track NN" pattern.
    let lower = s.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix("track") {
        let rest = rest.trim_start();
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        return digits.parse().unwrap_or(0);
    }
    0
}

/// Extract a disc number from a path's parent directory name.
/// Matches patterns like "Disc 01", "CD2", "Disk 1", "d01".
/// Returns 1 if no disc pattern found (default single-disc).
pub fn extract_disc_from_path(path: &std::path::Path) -> u32 {
    let parent_name = path.parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("");
    let lower = parent_name.to_ascii_lowercase();

    // Try prefixes: "disc", "disk", "cd", "d" (in order of specificity).
    for prefix in &["disc", "disk", "cd", "d"] {
        if let Some(rest) = lower.strip_prefix(prefix) {
            let rest = rest.trim_start_matches(|c: char| c == ' ' || c == '_' || c == '-');
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(n) = digits.parse::<u32>() {
                if n > 0 {
                    return n;
                }
            }
        }
    }
    1 // default: single disc
}

/// Parse a tag value like "1", "01", or "1/12" into a u32.
/// Returns 0 if unparseable.
pub fn parse_track_disc_tag(s: &str) -> u32 {
    let s = s.trim();
    // Handle "N/M" format (track/total or disc/total).
    let part = s.split('/').next().unwrap_or(s).trim();
    part.parse().unwrap_or(0)
}

/// Parse a filename stem into (track_number, title).
/// Strips leading "Track " prefix, extracts leading digits as track,
/// strips separator, remainder is title.
///
/// Examples:
/// - "01 - Statesboro Blues" → (Some(1), Some("Statesboro Blues"))
/// - "Track 03 - Foo" → (Some(3), Some("Foo"))
/// - "Statesboro Blues" → (None, Some("Statesboro Blues"))
/// - "01" → (Some(1), None)
pub fn parse_title_from_filename(stem: &str) -> (Option<u32>, Option<String>) {
    let mut s = stem.trim();
    // Strip "Track " prefix (case-insensitive) only if followed by
    // a non-letter (digit, space, separator) to avoid matching "Tracking".
    let lower = s.to_ascii_lowercase();
    if lower.starts_with("track") {
        let after = &s[5..];
        if after.is_empty() || !after.starts_with(|c: char| c.is_ascii_alphabetic()) {
            s = after.trim_start();
        }
    }
    // Extract leading digits.
    let digit_end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    if digit_end == 0 {
        // No digits — entire stem is the title.
        let title = s.trim();
        return (None, if title.is_empty() { None } else { Some(title.to_string()) });
    }
    let track: u32 = s[..digit_end].parse().unwrap_or(0);
    let track = if track > 0 { Some(track) } else { None };
    // Strip separator after digits.
    let rest = s[digit_end..].trim_start_matches(|c: char| {
        c == ' ' || c == '-' || c == '.' || c == '_'
    });
    let title = rest.trim();
    let title = if title.is_empty() { None } else { Some(title.to_string()) };
    (track, title)
}

/// Sort paths by (disc, track, filename) for logical display order.
/// Reads disc/track tags from each file via lofty (lightweight read),
/// falls back to directory/filename patterns.
pub fn sort_paths_by_track(paths: &mut Vec<std::path::PathBuf>) {
    use lofty::file::TaggedFileExt;
    use lofty::tag::ItemKey;

    if paths.len() <= 1 {
        return;
    }

    let sort_keys: Vec<(u32, u32, String)> = paths.iter().map(|p| {
        // Try reading disc/track from tags.
        let (tag_disc, tag_track) = lofty::read_from_path(p).ok()
            .and_then(|tagged| {
                let tag = tagged.primary_tag().or_else(|| tagged.first_tag())?;
                let disc = tag.get_string(&ItemKey::DiscNumber)
                    .map(|s| parse_track_disc_tag(s));
                let track = tag.get_string(&ItemKey::TrackNumber)
                    .map(|s| parse_track_disc_tag(s));
                Some((disc, track))
            })
            .unwrap_or((None, None));

        let disc = tag_disc.unwrap_or_else(|| extract_disc_from_path(p));
        let track = tag_track.unwrap_or_else(|| {
            let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            extract_track_from_filename(stem)
        });
        let filename = p.file_name()
            .map(|n| n.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        (disc, track, filename)
    }).collect();

    let mut perm: Vec<usize> = (0..paths.len()).collect();
    perm.sort_by(|&a, &b| sort_keys[a].cmp(&sort_keys[b]));

    let sorted: Vec<_> = perm.iter().map(|&i| paths[i].clone()).collect();
    *paths = sorted;
}

/// Priority order for standard fields (displayed first, in this order).
pub(super) const STANDARD_KEY_ORDER: &[&str] = &[
    "TITLE", "ARTIST", "ALBUM", "ALBUMARTIST", "GENRE",
    "DATE", "ORIGINALDATE", "YEAR",
    "TRACKNUMBER", "TRACKTOTAL", "DISCNUMBER", "DISCTOTAL",
    "CATALOGNUMBER", "RELEASECOUNTRY",
    "COMMENT", "COMPOSER", "CONDUCTOR", "LABEL",
    "ISRC", "BARCODE",
    "MUSICBRAINZ_ALBUMID", "MUSICBRAINZ_ALBUMARTISTID",
    "MUSICBRAINZ_RELEASEGROUPID",
    "MUSICBRAINZ_TRACKID", "MUSICBRAINZ_RELEASETRACKID",
    "MUSICBRAINZ_ARTISTID",
];

/// Sort `entries` so STANDARD_KEY_ORDER fields lead in their listed
/// order, with the remainder sorted alphabetically by display key.
/// Used by `read_all_tags_merged` and the MusicBrainz / GNUDB populate
/// paths so post-populate entries fall into their logical positions
/// instead of trailing.
pub fn sort_entries_standard_first(entries: &mut Vec<TagEntry>) {
    entries.sort_by(|a, b| {
        let a_upper = a.display_key.to_ascii_uppercase();
        let b_upper = b.display_key.to_ascii_uppercase();
        let a_idx = STANDARD_KEY_ORDER.iter().position(|&k| k == a_upper);
        let b_idx = STANDARD_KEY_ORDER.iter().position(|&k| k == b_upper);
        match (a_idx, b_idx) {
            (Some(ai), Some(bi)) => ai.cmp(&bi),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a_upper.cmp(&b_upper),
        }
    });
}

/// Map an ItemKey to a human-readable display name using the tag's
/// format-specific key string. Falls back to Debug format for unmapped keys.
fn item_key_display(key: &lofty::tag::ItemKey, tag_type: lofty::tag::TagType) -> String {
    // Try format-specific mapping first (gives "ARTIST" for VorbisComments, etc.)
    if let Some(s) = key.map_key(tag_type, true) {
        return s.to_string();
    }
    // Fallback: Debug format with cleanup
    format!("{:?}", key)
}

/// Read all tags from an audio file's primary tag.
/// Returns entries sorted: standard fields first, then alphabetical.
pub fn read_all_tags(path: &std::path::Path) -> Result<Vec<TagEntry>, String> {
    use lofty::file::TaggedFileExt;
    use lofty::tag::ItemValue;

    let tagged = lofty::read_from_path(path)
        .map_err(|e| format!("failed to read '{}': {}", path.display(), e))?;

    let tag = match tagged.primary_tag().or_else(|| tagged.first_tag()) {
        Some(t) => t,
        None => return Ok(Vec::new()), // tagless file — editor opens empty
    };

    let tag_type = tag.tag_type();
    let mut entries: Vec<TagEntry> = Vec::new();

    for item in tag.items() {
        let key = item.key().clone();
        let display_key = item_key_display(&key, tag_type);
        let (value, is_binary) = match item.value() {
            ItemValue::Text(t) => (t.clone(), false),
            ItemValue::Locator(l) => (l.clone(), false),
            ItemValue::Binary(b) => (format!("<binary, {} bytes>", b.len()), true),
        };
        entries.push(TagEntry {
            display_key,
            item_key: key,
            value: value.clone(),
            original: value.clone(),
            is_binary,
            is_mixed: false,
            per_file_values: vec![value.clone()],
            per_file_originals: vec![value],
            mb_proposed_value: None,
            mb_proposed_per_file: None,
        });
    }

    sort_entries_standard_first(&mut entries);

    Ok(entries)
}

/// Read and merge tags from multiple audio files.
///
/// For each `ItemKey` present in any file, collects per-file values.
/// If all files agree → shared value. If they differ → `<mixed>`.
/// Duplicate keys within a single file are joined with "; ".
pub fn read_all_tags_merged(paths: &[std::path::PathBuf]) -> Result<Vec<TagEntry>, String> {
    use std::collections::HashMap;
    use lofty::file::TaggedFileExt;
    use lofty::tag::{ItemValue, TagType};

    if paths.is_empty() {
        return Ok(Vec::new());
    }
    if paths.len() == 1 {
        return read_all_tags(&paths[0]);
    }

    // Read all files, collecting per-key values.
    // Key: ItemKey → Vec of (file_index, value_string) for ordering.
    // Also track the first tag_type for display name resolution.
    let mut first_tag_type: Option<TagType> = None;
    let n = paths.len();

    // For each ItemKey, store: display_key, is_binary, per_file_value[file_idx]
    struct KeyData {
        display_key: String,
        is_binary: bool,
        values: Vec<String>, // one per file, "" if absent
    }

    // Use Vec<(ItemKey, KeyData)> to preserve insertion order (first-seen).
    let mut key_order: Vec<lofty::tag::ItemKey> = Vec::new();
    let mut key_map: HashMap<lofty::tag::ItemKey, KeyData> = HashMap::new();

    for (file_idx, path) in paths.iter().enumerate() {
        let tagged = lofty::read_from_path(path)
            .map_err(|e| format!("failed to read '{}': {}", path.display(), e))?;

        let tag = match tagged.primary_tag().or_else(|| tagged.first_tag()) {
            Some(t) => t,
            None => continue, // file has no tags — all values stay ""
        };

        if first_tag_type.is_none() {
            first_tag_type = Some(tag.tag_type());
        }
        let tag_type = first_tag_type.unwrap();

        // Collect values per key. Join duplicates within this file with "; ".
        let mut file_values: HashMap<lofty::tag::ItemKey, (String, bool)> = HashMap::new();
        for item in tag.items() {
            let key = item.key().clone();
            let (val, is_bin) = match item.value() {
                ItemValue::Text(t) => (t.clone(), false),
                ItemValue::Locator(l) => (l.clone(), false),
                ItemValue::Binary(b) => (format!("<binary, {} bytes>", b.len()), true),
            };
            let entry = file_values.entry(key.clone()).or_insert_with(|| (String::new(), is_bin));
            if entry.0.is_empty() {
                entry.0 = val;
            } else {
                entry.0 = format!("{}; {}", entry.0, val);
            }
            // Ensure this key is in the order list.
            if !key_map.contains_key(&key) {
                let display = item_key_display(&key, tag_type);
                key_order.push(key.clone());
                key_map.insert(key, KeyData {
                    display_key: display,
                    is_binary: is_bin,
                    values: vec![String::new(); n],
                });
            }
        }

        // Write this file's values into the key_map.
        for (key, (val, is_bin)) in &file_values {
            if let Some(data) = key_map.get_mut(key) {
                data.values[file_idx] = val.clone();
                if *is_bin { data.is_binary = true; }
            }
        }
    }

    // Build merged TagEntry list.
    let mut entries: Vec<TagEntry> = Vec::new();
    for key in &key_order {
        let data = &key_map[key];
        let all_same = data.values.windows(2).all(|w| w[0] == w[1]);
        let is_mixed = !all_same;
        let display_value = if is_mixed {
            "<multiple values>".to_string()
        } else {
            data.values[0].clone()
        };

        entries.push(TagEntry {
            display_key: data.display_key.clone(),
            item_key: key.clone(),
            value: display_value.clone(),
            original: display_value,
            is_binary: data.is_binary,
            is_mixed,
            per_file_values: data.values.clone(),
            per_file_originals: data.values.clone(),
            mb_proposed_value: None,
            mb_proposed_per_file: None,
        });
    }

    // Sort with standard key priority.
    sort_entries_standard_first(&mut entries);

    Ok(entries)
}

/// Write a batch of tag changes to an audio file.
/// Each entry in `changes` is (ItemKey, Option<new_value>).
/// `None` means delete the tag. Empty string also deletes.
pub fn write_all_tags(
    path: &std::path::Path,
    changes: &[(lofty::tag::ItemKey, Option<String>)],
) -> Result<(), String> {
    use lofty::config::WriteOptions;
    use lofty::file::{AudioFile, TaggedFileExt};
    use lofty::tag::{ItemValue, TagItem};

    // Create a backup copy before modifying (crash safety).
    let backup = crate::db::Database::backup_path_for(path);
    std::fs::copy(path, &backup)
        .map_err(|e| format!("backup failed for '{}': {}", path.display(), e))?;

    let result = (|| -> Result<(), String> {
        let mut tagged = lofty::read_from_path(path)
            .map_err(|e| format!("failed to read '{}': {}", path.display(), e))?;

        // Get or create the primary tag.
        if tagged.primary_tag().is_none() {
            let tt = tagged.primary_tag_type();
            tagged.insert_tag(lofty::tag::Tag::new(tt));
        }
        let tag = tagged.primary_tag_mut()
            .ok_or_else(|| "failed to create primary tag".to_string())?;

        for (key, new_value) in changes {
            match new_value {
                Some(val) if !val.trim().is_empty() => {
                    tag.remove_key(key);
                    tag.insert_unchecked(TagItem::new(
                        key.clone(),
                        ItemValue::Text(val.trim().to_string()),
                    ));
                }
                _ => {
                    tag.remove_key(key);
                }
            }
        }

        tagged
            .save_to_path(path, WriteOptions::default())
            .map_err(|e| format!("failed to save '{}': {}", path.display(), e))?;

        Ok(())
    })();

    match &result {
        Ok(()) => {
            // Success — remove backup.
            let _ = std::fs::remove_file(&backup);
        }
        Err(_) => {
            // Failure — restore from backup.
            let _ = std::fs::rename(&backup, path);
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r128_raw_to_db_positive() {
        assert_eq!(r128_raw_to_db("1664").as_deref(), Some("+6.50 dB"));
    }

    #[test]
    fn r128_raw_to_db_negative() {
        assert_eq!(r128_raw_to_db("-1664").as_deref(), Some("-6.50 dB"));
    }

    #[test]
    fn r128_raw_to_db_zero() {
        assert_eq!(r128_raw_to_db("0").as_deref(), Some("+0.00 dB"));
    }

    #[test]
    fn r128_raw_to_db_fractional() {
        // 128 / 256 = 0.5 dB
        assert_eq!(r128_raw_to_db("128").as_deref(), Some("+0.50 dB"));
    }

    #[test]
    fn r128_raw_to_db_invalid_returns_none() {
        assert!(r128_raw_to_db("not a number").is_none());
        assert!(r128_raw_to_db("").is_none());
        assert!(r128_raw_to_db("-6.5").is_none()); // Not an integer
    }

    #[test]
    fn r128_raw_to_db_whitespace_trimmed() {
        assert_eq!(r128_raw_to_db("  -1664  ").as_deref(), Some("-6.50 dB"));
    }

    fn entry_with_mb_proposed(
        original: &str,
        proposed: &str,
        per_file_count: usize,
    ) -> TagEntry {
        TagEntry {
            display_key: "TITLE".to_string(),
            item_key: lofty::tag::ItemKey::TrackTitle,
            value: proposed.to_string(),
            original: original.to_string(),
            is_binary: false,
            is_mixed: false,
            per_file_values: vec![proposed.to_string(); per_file_count],
            per_file_originals: vec![original.to_string(); per_file_count],
            mb_proposed_value: Some(proposed.to_string()),
            mb_proposed_per_file: Some(vec![proposed.to_string(); per_file_count]),
        }
    }

    #[test]
    fn pill_state_revert_when_value_matches_proposed() {
        let e = entry_with_mb_proposed("File Title", "MB Title", 1);
        assert_eq!(mb_pill_state(&e), MbRevertPill::Revert);
    }

    #[test]
    fn pill_state_use_mb_after_revert() {
        let mut e = entry_with_mb_proposed("File Title", "MB Title", 1);
        toggle_mb_revert(&mut e);
        assert_eq!(e.value, "File Title");
        assert_eq!(mb_pill_state(&e), MbRevertPill::UseMb);
    }

    #[test]
    fn pill_state_none_for_manual_edit() {
        let mut e = entry_with_mb_proposed("File Title", "MB Title", 1);
        e.value = "User Hand-Edit".to_string();
        e.per_file_values = vec!["User Hand-Edit".to_string()];
        assert_eq!(mb_pill_state(&e), MbRevertPill::None);
    }

    #[test]
    fn pill_state_none_when_not_from_mb() {
        let e = TagEntry {
            display_key: "TITLE".into(),
            item_key: lofty::tag::ItemKey::TrackTitle,
            value: "x".into(), original: "x".into(),
            is_binary: false, is_mixed: false,
            per_file_values: vec!["x".into()],
            per_file_originals: vec!["x".into()],
            mb_proposed_value: None,
            mb_proposed_per_file: None,
        };
        assert_eq!(mb_pill_state(&e), MbRevertPill::None);
    }

    #[test]
    fn toggle_round_trips_between_mb_and_original() {
        let mut e = entry_with_mb_proposed("File", "MB", 1);
        toggle_mb_revert(&mut e); // MB → File
        assert_eq!(e.value, "File");
        toggle_mb_revert(&mut e); // File → MB
        assert_eq!(e.value, "MB");
        toggle_mb_revert(&mut e); // MB → File
        assert_eq!(e.value, "File");
    }

    #[test]
    fn toggle_no_op_on_manual_edit() {
        let mut e = entry_with_mb_proposed("File", "MB", 1);
        e.value = "Hand-Edit".into();
        e.per_file_values = vec!["Hand-Edit".into()];
        toggle_mb_revert(&mut e);
        assert_eq!(e.value, "Hand-Edit");  // unchanged
    }

    #[test]
    fn toggle_swaps_per_file_values_too() {
        let mut e = entry_with_mb_proposed("File", "MB", 3);
        toggle_mb_revert(&mut e);
        assert_eq!(e.per_file_values, vec!["File", "File", "File"]);
        assert!(!e.is_mixed);
        toggle_mb_revert(&mut e);
        assert_eq!(e.per_file_values, vec!["MB", "MB", "MB"]);
    }

    #[test]
    fn metadata_editor_has_changes_true_with_pending_deletion() {
        use crate::tui::app::{MetadataEditorPhase, MetadataEditorState};
        // No value changes, but one entry marked for deletion → dirty.
        let state = MetadataEditorState {
            paths: vec![std::path::PathBuf::from("/tmp/01.flac")],
            entries: vec![TagEntry {
                display_key: "TITLE".into(),
                item_key: lofty::tag::ItemKey::TrackTitle,
                value: "x".into(), original: "x".into(),
                is_binary: false, is_mixed: false,
                per_file_values: vec!["x".into()],
                per_file_originals: vec!["x".into()],
                mb_proposed_value: None,
                mb_proposed_per_file: None,
            }],
            cursor: 0, scroll: 0, last_click: None,
            edit_input: None, add_key_input: None,
            phase: MetadataEditorPhase::Editing,
            dirty: false, deleted: vec![0],
            file_labels: vec!["01".into()],
            detail_field_idx: 0, detail_cursor: 0, detail_scroll: 0, detail_edit: None,
        };
        assert!(metadata_editor_has_changes(&state));
    }

    #[test]
    fn metadata_editor_has_changes_false_after_full_revert() {
        use crate::tui::app::{MetadataEditorPhase, MetadataEditorState};
        let mut state = MetadataEditorState {
            paths: vec![std::path::PathBuf::from("/tmp/01.flac")],
            entries: vec![
                entry_with_mb_proposed("File", "MB", 1),
                entry_with_mb_proposed("File2", "MB2", 1),
            ],
            cursor: 0, scroll: 0, last_click: None,
            edit_input: None, add_key_input: None,
            phase: MetadataEditorPhase::Editing,
            dirty: true, deleted: Vec::new(),
            file_labels: vec!["01".into()],
            detail_field_idx: 0, detail_cursor: 0, detail_scroll: 0, detail_edit: None,
        };
        // Both entries currently show the MB value → has changes.
        assert!(metadata_editor_has_changes(&state));

        // Revert both.
        toggle_mb_revert(&mut state.entries[0]);
        toggle_mb_revert(&mut state.entries[1]);
        assert!(!metadata_editor_has_changes(&state));
    }

    /// Critical round-trip test for the CUESHEET tag embed plan:
    /// confirm lofty preserves a multi-line Vorbis comment value
    /// through write → read on a real FLAC file. If this test fails,
    /// the embed plan must switch to FLAC's native CUESHEET metadata
    /// block instead of a Vorbis comment.
    ///
    /// The save path uses `val.trim()` (probe.rs:1116-ish), so the
    /// trailing newline gets stripped — we compare against the
    /// trim_end of the producer output, not the raw producer.
    #[test]
    fn lofty_cuesheet_vorbis_comment_round_trips_multiline() {
        use lofty::config::WriteOptions;
        use lofty::file::{AudioFile, TaggedFileExt};
        use lofty::tag::{ItemKey, ItemValue, TagItem};

        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/silence.flac");
        assert!(
            fixture.exists(),
            "missing test fixture: {}",
            fixture.display()
        );
        // Copy to a tmp file so we don't mutate the fixture.
        let tmp = std::env::temp_dir()
            .join(format!("tonepoet-cuesheet-roundtrip-{}.flac", std::process::id()));
        std::fs::copy(&fixture, &tmp).expect("copy fixture");

        let cue_payload = "REM GENRE Rock\n\
            REM DATE 1970\n\
            CATALOG 0044007735428\n\
            TITLE \"Whole Album\"\n\
            PERFORMER \"Album Artist\"\n\
            FILE \"image.flac\" FLAC\n\
              TRACK 01 AUDIO\n\
                TITLE \"First\"\n\
                PERFORMER \"Album Artist\"\n\
                INDEX 01 00:00:00\n\
              TRACK 02 AUDIO\n\
                TITLE \"Second\"\n\
                PERFORMER \"Album Artist\"\n\
                INDEX 01 04:00:00\n";

        // Write CUESHEET via lofty.
        {
            let mut tagged = lofty::read_from_path(&tmp).expect("read fixture");
            if tagged.primary_tag().is_none() {
                let tt = tagged.primary_tag_type();
                tagged.insert_tag(lofty::tag::Tag::new(tt));
            }
            let tag = tagged.primary_tag_mut().expect("primary tag");
            let key = ItemKey::Unknown("CUESHEET".to_string());
            tag.remove_key(&key);
            tag.insert_unchecked(TagItem::new(
                key,
                ItemValue::Text(cue_payload.trim().to_string()),
            ));
            tagged.save_to_path(&tmp, WriteOptions::default()).expect("save");
        }

        // Read CUESHEET back.
        let read_back = {
            let tagged = lofty::read_from_path(&tmp).expect("re-read");
            let tag = tagged.primary_tag().expect("primary tag re-read");
            let key = ItemKey::Unknown("CUESHEET".to_string());
            tag.get_string(&key).map(|s| s.to_string())
        };

        let _ = std::fs::remove_file(&tmp);

        let read_back = read_back.expect("CUESHEET tag should be present after save");
        assert_eq!(
            read_back, cue_payload.trim(),
            "lofty should preserve multi-line Vorbis comment value byte-for-byte (modulo trim)"
        );
        // Sanity: the value still has internal newlines (lofty didn't
        // collapse them).
        assert!(read_back.contains('\n'), "internal newlines must be preserved");
        assert!(
            read_back.matches('\n').count() >= 5,
            "multi-line content (≥5 newlines) must round-trip"
        );
    }

    #[test]
    fn toggle_no_op_when_no_mb_proposed() {
        let mut e = TagEntry {
            display_key: "TITLE".into(),
            item_key: lofty::tag::ItemKey::TrackTitle,
            value: "x".into(), original: "y".into(),
            is_binary: false, is_mixed: false,
            per_file_values: vec!["x".into()],
            per_file_originals: vec!["y".into()],
            mb_proposed_value: None,
            mb_proposed_per_file: None,
        };
        toggle_mb_revert(&mut e);
        assert_eq!(e.value, "x");
    }
}
