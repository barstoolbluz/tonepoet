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
            if name.contains("webm") {
                "WebM".to_string()
            } else {
                "MKA".to_string()
            }
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

fn probe_dvda_disc(path: &Path) -> Result<SourceInfo, String> {
    let contents = crate::disc::dvda_utils::map_dvda_source(path)?;
    let presentation = contents
        .presentations
        .first()
        .ok_or_else(|| format!("DVD-Audio disc has no audio streams: {}", path.display()))?;
    Ok(crate::tui::disc_browser::source_info_for_presentation(&contents, presentation))
}

fn probe_dvdv_disc(path: &Path) -> Result<SourceInfo, String> {
    let contents = crate::disc::dvdv_utils::map_dvdv_source(path)?;
    let presentation = contents
        .presentations
        .first()
        .ok_or_else(|| format!("DVD-Video disc has no audio streams: {}", path.display()))?;
    Ok(crate::tui::disc_browser::source_info_for_presentation(&contents, presentation))
}

pub fn probe_audio(path: &Path) -> Result<SourceInfo, String> {
    if crate::disc::dvda_utils::is_dvda_source(path) {
        return probe_dvda_disc(path);
    }
    if crate::disc::dvdv_utils::is_dvdv_source(path) {
        return probe_dvdv_disc(path);
    }
    // SACD ISOs are ScarletBook-format DSD streams that ffmpeg can't open
    // (it'll either error out on the unrecognised container or mis-detect
    // the leading bytes as ISO9660). Branch up-front: if magic bytes are
    // present, synthesize SourceInfo from the parsed Master TOC + Area TOC
    // so the source pane shows DSD64 / channels / duration without the
    // broken ffmpeg fallback.
    if super::sacd::is_sacd_iso(path) {
        return probe_sacd(path);
    }

    ensure_ffmpeg_init();

    let file_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);

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

    let audio = codec_ctx
        .decoder()
        .audio()
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

/// Synthesize a `SourceInfo` for a SACD ISO. Defaults to surfacing
/// the **stereo** area when both stereo and multi-channel are
/// present; falls back to multi-channel if only that exists. The
/// area can be overridden later (C6) once the source pane gains a
/// stereo/MCH toggle pill.
///
/// Sample rate is fixed at SACD's canonical 64×44.1 kHz (DSD64);
/// bit depth = 1 (DSD); duration is taken from the area's
/// `total_playtime` (m/s/f@75 → seconds). Format name surfaces
/// "SACD ISO (DST)" or "SACD ISO" depending on whether DST
/// compression is in use, since the user usually wants to know
/// before attempting any export.
fn probe_sacd(path: &Path) -> Result<SourceInfo, String> {
    let md = super::sacd::parse_sacd_iso(path)
        .map_err(|e| format!("SACD parse failed for '{}': {}", path.display(), e))?;

    let file_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);

    // Pick the area to surface. Stereo wins if present.
    let (area, area_label) = if let Some(stereo) = md.stereo.as_ref() {
        (stereo, "stereo")
    } else if let Some(mch) = md.multi_channel.as_ref() {
        (mch, "MCH")
    } else {
        // Triggered when the master TOC pointed at one or both areas
        // but every area parse failed (parse_sacd_iso runs in
        // best-effort mode by default — per-area errors yield None).
        // The all-areas-zero case is rejected earlier by
        // parse_master_toc, so this Err means "areas declared but
        // unreadable", not "no areas declared".
        return Err(format!(
            "SACD '{}': declared areas all failed to parse",
            path.display(),
        ));
    };

    let channels = area.header.channel_count as u32;
    // SACD multi-channel discs are essentially always 5.1 (loudspeaker
    // config 5 in the spec), and 5-channel-only is "5.0". We don't try
    // to enumerate every loudspeaker_config nibble — those uncommon
    // configs render as "N ch" via the catch-all.
    let channel_layout = match channels {
        2 => "stereo".to_string(),
        5 => "5.0".to_string(),
        6 => "5.1".to_string(),
        n => format!("{} ch", n),
    };

    let duration_secs = area.header.total_playtime.total_seconds();

    let format_name = if area.header.frame_format.is_dst_encoded() {
        format!("SACD ISO (DST, {})", area_label)
    } else {
        format!("SACD ISO ({})", area_label)
    };

    // Codec label tracks the DSD rate (always DSD64 for SACD discs
    // per the spec — no other sample_frequency value has shipped).
    let codec = "DSD64".to_string();

    Ok(SourceInfo {
        format_name,
        codec,
        bit_depth: Some(1),
        sample_rate: super::sacd::SACD_SAMPLE_RATE_HZ,
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

    // SACD ISOs aren't tagged files in lofty's sense — pull the
    // album-level fields out of the ScarletBook Master TOC + SACDText
    // sector instead. Per-track text (titles per track) lives on the
    // editor's per-track populate path (C5+), not the source-level
    // SourceMetadata.
    if super::sacd::is_sacd_iso(path) {
        return read_metadata_sacd(path);
    }

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
            meta.isrc = tag.get_string(&ItemKey::Isrc).map(|s| s.to_string());
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
            meta.r128_track_gain = tag.get_string(&r128_track_key).and_then(r128_raw_to_db);
        }
        if meta.r128_album_gain.is_none() {
            meta.r128_album_gain = tag.get_string(&r128_album_key).and_then(r128_raw_to_db);
        }
    }

    // Quick pre-emphasis metadata check (tags + CUE/log + catalog).
    meta.preemphasis_metadata = preemphasis_metadata_check(path);

    Ok(meta)
}

/// Synthesize source-level metadata for a SACD ISO from the Master
/// TOC + SACDText. Maps:
///   - master_text.album_title  → meta.album   (album of the ISO)
///   - master_text.album_artist → meta.artist
///   - disc_genres[0].name()    → meta.genre
///   - master_toc.disc_date.year → meta.year
///   - master_toc.album_catalog_number → meta.catalog_number
///
/// Title is intentionally left empty at the source level — SACDs
/// don't have a single "track title"; per-track titles surface
/// through the editor (C5) by pulling area.tracks[i].text.title.
///
/// Pre-emphasis is not applicable to DSD audio (the SACD spec
/// doesn't define pre-emphasis), so that field stays None.
fn read_metadata_sacd(path: &Path) -> Result<SourceMetadata, String> {
    let md = super::sacd::parse_sacd_iso(path)
        .map_err(|e| format!("SACD parse failed for '{}': {}", path.display(), e))?;

    // Sidecar wins on every field it provides; ScarletBook fills the
    // gaps. Same precedence rule the editor uses
    // (build_sacd_editor_state). Reading the sidecar twice (here +
    // editor open) is a minor inefficiency but keeps the surfaces
    // independent — the browse info pane works whether or not the
    // user has opened the editor.
    let sidecar = super::sacd_sidecar::find_sidecar_for_iso(path)
        .and_then(|p| super::sacd_sidecar::parse_sidecar(&p).ok());

    // First non-empty value across all sidecar tracks for a given
    // (already uppercased) meta key. Returns None when the sidecar
    // is absent or the key is missing/empty everywhere.
    let from_sidecar = |key: &str| -> Option<String> {
        sidecar
            .as_ref()?
            .tracks
            .iter()
            .find_map(|t| t.meta.get(key).filter(|s| !s.trim().is_empty()))
            .map(|s| s.trim().to_string())
    };

    let mut meta = SourceMetadata::default();

    meta.album = from_sidecar("ALBUM")
        .or_else(|| md.master_text.as_ref().and_then(|t| t.album_title.clone()));
    meta.artist = from_sidecar("ARTIST")
        .or_else(|| from_sidecar("ALBUMARTIST"))
        .or_else(|| from_sidecar("ALBUM ARTIST"))
        .or_else(|| md.master_text.as_ref().and_then(|t| t.album_artist.clone()));
    meta.year =
        from_sidecar("DATE").or_else(|| md.master_toc.disc_date.map(|d| d.year.to_string()));
    meta.catalog_number = from_sidecar("CATALOGNUMBER")
        .or_else(|| from_sidecar("DISCOGS_CATALOG"))
        .or_else(|| {
            let c = md.master_toc.album_catalog_number.trim().to_string();
            if c.is_empty() {
                None
            } else {
                Some(c)
            }
        });
    meta.genre = from_sidecar("GENRE").or_else(|| {
        md.master_toc
            .disc_genres
            .first()
            .or_else(|| md.master_toc.album_genres.first())
            .map(|g| g.name())
            .filter(|n| *n != "Not used" && *n != "Not defined")
            .map(|s| s.to_string())
    });

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
    use super::preemphasis::catalog::check_catalog_evidence;
    use super::preemphasis::metadata::{check_file_evidence, check_tag_evidence};

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
        &[
            Self::Title,
            Self::Artist,
            Self::Album,
            Self::Genre,
            Self::Year,
        ]
    }
}

/// Write a single metadata field to an audio file's tags via lofty.
///
/// Re-reads the file, modifies the primary tag (creating one if needed),
/// and saves with default `WriteOptions` (preserves padding and other tags).
///
/// Year values must be valid u32; non-numeric input returns an error.
/// Empty strings clear the field (set to None).
pub fn write_metadata_field(path: &Path, field: MetadataField, value: &str) -> Result<(), String> {
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
/// Resize `entry.per_file_values` and `per_file_originals` to
/// `target_dim`, padding with the existing first-element value when
/// growing. Replicating preserves revert semantics: pressing revert
/// after a per-track populate restores the editor to whatever was on
/// disk for the original (lower) dimension. Truncation is a plain
/// `Vec::resize` and discards trailing values.
///
/// Used by both MB and gnudb populate paths to grow tag entries to
/// per-track dimension on single-image rips.
pub fn ensure_dim_replicate(entry: &mut TagEntry, target_dim: usize) {
    if entry.per_file_values.len() == target_dim {
        return;
    }
    let pad_v = entry.per_file_values.first().cloned().unwrap_or_default();
    let pad_o = entry
        .per_file_originals
        .first()
        .cloned()
        .unwrap_or_default();
    entry.per_file_values.resize(target_dim, pad_v);
    entry.per_file_originals.resize(target_dim, pad_o);
}

pub fn metadata_editor_has_changes(state: &super::app::MetadataEditorState) -> bool {
    !state.deleted.is_empty()
        || state
            .entries
            .iter()
            .any(|e| e.value != e.original || e.per_file_values != e.per_file_originals)
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
    let Some(ref proposed) = entry.mb_proposed_value else {
        return;
    };
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
    let Some(ref proposed) = entry.mb_proposed_value else {
        return;
    };
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
    let parent_name = path
        .parent()
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
        return (
            None,
            if title.is_empty() {
                None
            } else {
                Some(title.to_string())
            },
        );
    }
    let track: u32 = s[..digit_end].parse().unwrap_or(0);
    let track = if track > 0 { Some(track) } else { None };
    // Strip separator after digits.
    let rest =
        s[digit_end..].trim_start_matches(|c: char| c == ' ' || c == '-' || c == '.' || c == '_');
    let title = rest.trim();
    let title = if title.is_empty() {
        None
    } else {
        Some(title.to_string())
    };
    (track, title)
}

/// Sort paths by (disc, track, filename) for logical display order.
/// Reads disc/track tags from each file via lofty (lightweight read),
/// falls back to directory/filename patterns.
/// Entry-aware variant of `sort_paths_by_track`. Sorts `paths` by
/// (disc, track, filename) AND permutes each entry's `per_file_values`
/// + `per_file_originals` in lockstep so the per-file vectors stay
/// aligned with the new path order.
///
/// Pulls disc/track from already-merged `entries` (the canonical
/// source after `read_all_tags_merged`); falls back to path-name
/// extraction when an entry is empty or missing. Treats empty
/// strings as "no tag" — divergence from `sort_paths_by_track`'s
/// `parse_track_disc_tag("")=0` behavior, but matches what
/// `open_metadata_editor` has always done.
///
/// Used by:
/// - TUI's `open_metadata_editor` after `read_all_tags_merged`.
/// - CLI `tonepoet tags-mb` audio-file path before populate.
///
/// Caller's responsibility: don't mix this with `sort_paths_by_track`
/// in the same flow — they may produce slightly different orderings
/// on edge cases (present-but-empty tags), and the latter doesn't
/// touch the entry vectors.
pub fn sort_paths_and_entries_by_track(
    paths: &mut Vec<std::path::PathBuf>,
    entries: &mut Vec<TagEntry>,
) {
    let n = paths.len();
    if n <= 1 {
        return;
    }

    let disc_entry = entries
        .iter()
        .find(|e| e.display_key.to_ascii_uppercase() == "DISCNUMBER");
    let track_entry = entries
        .iter()
        .find(|e| e.display_key.to_ascii_uppercase() == "TRACKNUMBER");

    let sort_keys: Vec<(u32, u32, String)> = (0..n)
        .map(|i| {
            let tag_disc = disc_entry
                .and_then(|e| e.per_file_values.get(i))
                .filter(|v| !v.is_empty())
                .map(|v| parse_track_disc_tag(v));
            let tag_track = track_entry
                .and_then(|e| e.per_file_values.get(i))
                .filter(|v| !v.is_empty())
                .map(|v| parse_track_disc_tag(v));

            let disc = tag_disc.unwrap_or_else(|| extract_disc_from_path(&paths[i]));
            let track = tag_track.unwrap_or_else(|| {
                let stem = paths[i].file_stem().and_then(|s| s.to_str()).unwrap_or("");
                extract_track_from_filename(stem)
            });
            let filename = paths[i]
                .file_name()
                .map(|n| n.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            (disc, track, filename)
        })
        .collect();

    let mut perm: Vec<usize> = (0..n).collect();
    perm.sort_by(|&a, &b| sort_keys[a].cmp(&sort_keys[b]));

    let sorted_paths: Vec<_> = perm.iter().map(|&i| paths[i].clone()).collect();
    *paths = sorted_paths;

    for entry in entries.iter_mut() {
        if entry.per_file_values.len() == n {
            let sv: Vec<_> = perm
                .iter()
                .map(|&i| entry.per_file_values[i].clone())
                .collect();
            let so: Vec<_> = perm
                .iter()
                .map(|&i| entry.per_file_originals[i].clone())
                .collect();
            entry.per_file_values = sv;
            entry.per_file_originals = so;
        }
        // Per-track entries (len != n, single-image rips with embedded
        // CUESHEET) are indexed by MB-track position, not file position,
        // so the path permutation doesn't apply.
    }
}

pub fn sort_paths_by_track(paths: &mut Vec<std::path::PathBuf>) {
    use lofty::file::TaggedFileExt;
    use lofty::tag::ItemKey;

    if paths.len() <= 1 {
        return;
    }

    let sort_keys: Vec<(u32, u32, String)> = paths
        .iter()
        .map(|p| {
            // Try reading disc/track from tags.
            let (tag_disc, tag_track) = lofty::read_from_path(p)
                .ok()
                .and_then(|tagged| {
                    let tag = tagged.primary_tag().or_else(|| tagged.first_tag())?;
                    let disc = tag
                        .get_string(&ItemKey::DiscNumber)
                        .map(|s| parse_track_disc_tag(s));
                    let track = tag
                        .get_string(&ItemKey::TrackNumber)
                        .map(|s| parse_track_disc_tag(s));
                    Some((disc, track))
                })
                .unwrap_or((None, None));

            let disc = tag_disc.unwrap_or_else(|| extract_disc_from_path(p));
            let track = tag_track.unwrap_or_else(|| {
                let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                extract_track_from_filename(stem)
            });
            let filename = p
                .file_name()
                .map(|n| n.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            (disc, track, filename)
        })
        .collect();

    let mut perm: Vec<usize> = (0..paths.len()).collect();
    perm.sort_by(|&a, &b| sort_keys[a].cmp(&sort_keys[b]));

    let sorted: Vec<_> = perm.iter().map(|&i| paths[i].clone()).collect();
    *paths = sorted;
}

/// Priority order for standard fields (displayed first, in this order).
pub(super) const STANDARD_KEY_ORDER: &[&str] = &[
    "TITLE",
    "ARTIST",
    "ALBUM",
    "ALBUMARTIST",
    "GENRE",
    "DATE",
    "ORIGINALDATE",
    "YEAR",
    "TRACKNUMBER",
    "TRACKTOTAL",
    "DISCNUMBER",
    "DISCTOTAL",
    "CATALOGNUMBER",
    "RELEASECOUNTRY",
    "COMMENT",
    "COMPOSER",
    "CONDUCTOR",
    "LABEL",
    "ISRC",
    "BARCODE",
    "MUSICBRAINZ_ALBUMID",
    "MUSICBRAINZ_ALBUMARTISTID",
    "MUSICBRAINZ_RELEASEGROUPID",
    "MUSICBRAINZ_TRACKID",
    "MUSICBRAINZ_RELEASETRACKID",
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

    // Force-binary on synthetic-preview rows (CUESHEET) so inline edit
    // is blocked everywhere — those values can be 1-2KB of multi-line
    // content and a synthetic summary is shown in the editor instead.
    for e in &mut entries {
        if is_synthetic_preview(e) {
            e.is_binary = true;
        }
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
    use lofty::file::TaggedFileExt;
    use lofty::tag::{ItemValue, TagType};
    use std::collections::HashMap;

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
            let entry = file_values
                .entry(key.clone())
                .or_insert_with(|| (String::new(), is_bin));
            if entry.0.is_empty() {
                entry.0 = val;
            } else {
                entry.0 = format!("{}; {}", entry.0, val);
            }
            // Ensure this key is in the order list.
            if !key_map.contains_key(&key) {
                let display = item_key_display(&key, tag_type);
                key_order.push(key.clone());
                key_map.insert(
                    key,
                    KeyData {
                        display_key: display,
                        is_binary: is_bin,
                        values: vec![String::new(); n],
                    },
                );
            }
        }

        // Write this file's values into the key_map.
        for (key, (val, is_bin)) in &file_values {
            if let Some(data) = key_map.get_mut(key) {
                data.values[file_idx] = val.clone();
                if *is_bin {
                    data.is_binary = true;
                }
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

    // Force-binary on synthetic-preview rows (CUESHEET) so inline
    // edit is blocked — see read_all_tags for rationale.
    for e in &mut entries {
        if is_synthetic_preview(e) {
            e.is_binary = true;
        }
    }

    // Sort with standard key priority.
    sort_entries_standard_first(&mut entries);

    Ok(entries)
}

/// Apply a metadata-editor snapshot to a set of audio files, writing
/// every per-file diff via `write_all_tags`. Synchronous and blocking
/// — callers that share an async runtime should wrap this in
/// `tokio::task::spawn_blocking`. Returns one `(path, Result)` per
/// file that had changes (files with no diff are silently skipped).
///
/// Used by:
/// - TUI editor's `:w` (`metadata_editor_save` wraps in spawn_blocking
///   and sends `MetadataEditorWriteComplete` afterwards).
/// - CLI `tonepoet tags-mb` (calls directly via spawn_blocking + await).
///
/// `entries_snap` is the editor's per-entry snapshot
/// `(ItemKey, per_file_values, per_file_originals)`. Per-track entries
/// (where `vals.len() != paths.len()`) are skipped — they round-trip
/// through the CUESHEET tag via `regenerate_cuesheet_for_save`, not
/// through individual file writes. `deleted` is the editor's
/// `state.deleted` (indices of entries the user removed).
pub fn apply_audio_tag_changes(
    paths: &[std::path::PathBuf],
    entries_snap: &[(lofty::tag::ItemKey, Vec<String>, Vec<String>)],
    deleted: &[usize],
) -> Vec<(std::path::PathBuf, Result<(), String>)> {
    let mut results = Vec::new();
    for (file_idx, path) in paths.iter().enumerate() {
        let mut changes: Vec<(lofty::tag::ItemKey, Option<String>)> = Vec::new();
        for (entry_idx, (key, vals, origs)) in entries_snap.iter().enumerate() {
            // Per-track entries (single-image rips with embedded CUESHEET)
            // round-trip through the regenerated CUESHEET tag instead of
            // having a per-file lofty home; skip them here.
            if vals.len() != paths.len() {
                continue;
            }
            if deleted.contains(&entry_idx) {
                changes.push((key.clone(), None));
            } else if file_idx < vals.len() && file_idx < origs.len() {
                if vals[file_idx] != origs[file_idx] {
                    changes.push((key.clone(), Some(vals[file_idx].clone())));
                }
            }
        }
        if !changes.is_empty() {
            let r = write_all_tags(path, &changes);
            results.push((path.clone(), r));
        }
    }
    results
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
        let tag = tagged
            .primary_tag_mut()
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

    fn entry_with_mb_proposed(original: &str, proposed: &str, per_file_count: usize) -> TagEntry {
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
            value: "x".into(),
            original: "x".into(),
            is_binary: false,
            is_mixed: false,
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
        assert_eq!(e.value, "Hand-Edit"); // unchanged
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
                value: "x".into(),
                original: "x".into(),
                is_binary: false,
                is_mixed: false,
                per_file_values: vec!["x".into()],
                per_file_originals: vec!["x".into()],
                mb_proposed_value: None,
                mb_proposed_per_file: None,
            }],
            cursor: 0,
            scroll: 0,
            last_click: None,
            edit_input: None,
            add_key_input: None,
            phase: MetadataEditorPhase::Editing,
            dirty: false,
            deleted: vec![0],
            file_labels: vec!["01".into()],
            detail_field_idx: 0,
            detail_cursor: 0,
            detail_scroll: 0,
            detail_edit: None,
            mb_back: None,
            gnudb_back: None,
            read_only: false,
            sacd_sidecar_path: None,
            sacd_area_kind: None,
            sacd_stereo_durations: None,
            sacd_multi_channel_durations: None,
            dvdv_track_durations: None,
            dvdv_angle_number: None,
            dvdv_title_angle_count: None,
            dvdv_source_chapters: None,
            presentation_tabs: vec![],
            active_tab: 0,
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
            cursor: 0,
            scroll: 0,
            last_click: None,
            edit_input: None,
            add_key_input: None,
            phase: MetadataEditorPhase::Editing,
            dirty: true,
            deleted: Vec::new(),
            file_labels: vec!["01".into()],
            detail_field_idx: 0,
            detail_cursor: 0,
            detail_scroll: 0,
            detail_edit: None,
            mb_back: None,
            gnudb_back: None,
            read_only: false,
            sacd_sidecar_path: None,
            sacd_area_kind: None,
            sacd_stereo_durations: None,
            sacd_multi_channel_durations: None,
            dvdv_track_durations: None,
            dvdv_angle_number: None,
            dvdv_title_angle_count: None,
            dvdv_source_chapters: None,
            presentation_tabs: vec![],
            active_tab: 0,
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

        let fixture =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/silence.flac");
        assert!(
            fixture.exists(),
            "missing test fixture: {}",
            fixture.display()
        );
        // Copy to a tmp file so we don't mutate the fixture.
        let tmp = std::env::temp_dir().join(format!(
            "tonepoet-cuesheet-roundtrip-{}.flac",
            std::process::id()
        ));
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
            tagged
                .save_to_path(&tmp, WriteOptions::default())
                .expect("save");
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
            read_back,
            cue_payload.trim(),
            "lofty should preserve multi-line Vorbis comment value byte-for-byte (modulo trim)"
        );
        // Sanity: the value still has internal newlines (lofty didn't
        // collapse them).
        assert!(
            read_back.contains('\n'),
            "internal newlines must be preserved"
        );
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
            value: "x".into(),
            original: "y".into(),
            is_binary: false,
            is_mixed: false,
            per_file_values: vec!["x".into()],
            per_file_originals: vec!["y".into()],
            mb_proposed_value: None,
            mb_proposed_per_file: None,
        };
        toggle_mb_revert(&mut e);
        assert_eq!(e.value, "x");
    }

    // ---------- SACD probe path (C4) ----------

    /// Build a synthetic SACD ISO at `path` with stereo + (optionally)
    /// multi-channel areas, plus master TOC text. Mirrors the helper
    /// in sacd.rs tests but lives here so we can drive probe_sacd
    /// without a public re-export.
    fn write_sacd_iso(
        path: &std::path::Path,
        stereo: bool,
        multi: bool,
        dst_encoded: bool,
        album_title: Option<&str>,
        album_artist: Option<&str>,
        disc_year: u16,
        catalog: Option<&str>,
        playtime_minutes: u8,
    ) {
        use crate::tui::sacd::*;
        use std::io::{Seek, SeekFrom, Write};

        let total_sectors = 700u64;
        let f = std::fs::File::create(path).unwrap();
        f.set_len(total_sectors * SECTOR_SIZE).unwrap();
        drop(f);
        let mut f = std::fs::File::options().write(true).open(path).unwrap();

        // Master TOC
        let mut mtoc = vec![0u8; 0xa8];
        mtoc[0..8].copy_from_slice(MASTER_TOC_MAGIC);
        mtoc[0x08] = 1;
        mtoc[0x09] = 20;
        mtoc[0x10..0x12].copy_from_slice(&1u16.to_be_bytes());
        mtoc[0x12..0x14].copy_from_slice(&1u16.to_be_bytes());
        if let Some(c) = catalog {
            let bytes = c.as_bytes();
            let n = bytes.len().min(16);
            mtoc[0x18..0x18 + n].copy_from_slice(&bytes[..n]);
        }
        if stereo {
            mtoc[0x40..0x44].copy_from_slice(&540u32.to_be_bytes());
            mtoc[0x54..0x56].copy_from_slice(&3u16.to_be_bytes());
        }
        if multi {
            mtoc[0x48..0x4c].copy_from_slice(&600u32.to_be_bytes());
            mtoc[0x56..0x58].copy_from_slice(&3u16.to_be_bytes());
        }
        if disc_year > 0 {
            mtoc[0x78..0x7a].copy_from_slice(&disc_year.to_be_bytes());
            mtoc[0x7a] = 6;
            mtoc[0x7b] = 15;
        }
        // disc_genre[0] = JAZZ for the test
        mtoc[0x68] = 1;
        mtoc[0x6b] = 14;
        mtoc[0x80] = 1;
        mtoc[0x88] = b'e';
        mtoc[0x89] = b'n';
        mtoc[0x8a] = 2;
        f.seek(SeekFrom::Start(510 * SECTOR_SIZE)).unwrap();
        f.write_all(&mtoc).unwrap();

        // SACDText at LSN 511
        if album_title.is_some() || album_artist.is_some() {
            let mut tbuf = vec![0u8; SECTOR_SIZE as usize];
            tbuf[0..8].copy_from_slice(SACD_TEXT_MAGIC);
            let mut data_pos = 0x100u16;
            if let Some(t) = album_title {
                tbuf[0x10..0x12].copy_from_slice(&data_pos.to_be_bytes());
                let bytes = t.as_bytes();
                tbuf[data_pos as usize..data_pos as usize + bytes.len()].copy_from_slice(bytes);
                data_pos += bytes.len() as u16 + 1;
            }
            if let Some(a) = album_artist {
                tbuf[0x12..0x14].copy_from_slice(&data_pos.to_be_bytes());
                let bytes = a.as_bytes();
                tbuf[data_pos as usize..data_pos as usize + bytes.len()].copy_from_slice(bytes);
            }
            f.seek(SeekFrom::Start(511 * SECTOR_SIZE)).unwrap();
            f.write_all(&tbuf).unwrap();
        }

        // Helper: build an area-TOC sector with the requested format,
        // channel count, and total playtime.
        let mut build_area = |magic: &[u8; 8], channels: u8, lou: u8| {
            let mut a = vec![0u8; SECTOR_SIZE as usize];
            a[0..8].copy_from_slice(magic);
            a[0x08] = 1;
            a[0x09] = 20;
            a[0x0a..0x0c].copy_from_slice(&3u16.to_be_bytes()); // size
            a[0x14] = 0x04;
            a[0x15] = if dst_encoded { 0 } else { 2 };
            a[0x20] = channels;
            a[0x21] = (lou << 3) | 0;
            a[0x22] = channels;
            a[0x40] = playtime_minutes;
            a[0x41] = 30;
            a[0x42] = 0;
            a[0x44] = 0;
            a[0x45] = 1; // 1 track for simplicity
            a[0x48..0x4c].copy_from_slice(&650u32.to_be_bytes());
            a[0x4c..0x50].copy_from_slice(&100_000u32.to_be_bytes());
            a[0x50] = 1;
            a[0x58] = b'e';
            a[0x59] = b'n';
            a[0x5a] = 2;
            a
        };

        if stereo {
            f.seek(SeekFrom::Start(540 * SECTOR_SIZE)).unwrap();
            f.write_all(&build_area(TWOCH_TOC_MAGIC, 2, 0)).unwrap();
        }
        if multi {
            f.seek(SeekFrom::Start(600 * SECTOR_SIZE)).unwrap();
            f.write_all(&build_area(MULCH_TOC_MAGIC, 6, 5)).unwrap();
        }
    }

    #[test]
    fn probe_sacd_returns_dsd64_stereo_when_both_areas_present() {
        let td = tempfile::tempdir().unwrap();
        let path = td.path().join("disc.iso");
        write_sacd_iso(&path, true, true, false, None, None, 2003, None, 50);
        let info = probe_audio(&path).expect("probe");
        assert!(info.format_name.starts_with("SACD ISO"));
        assert!(info.format_name.contains("stereo"));
        assert_eq!(info.codec, "DSD64");
        assert_eq!(info.bit_depth, Some(1));
        assert_eq!(info.sample_rate, 2_822_400);
        assert_eq!(info.channels, 2);
        assert_eq!(info.channel_layout, "stereo");
        assert!((info.duration_secs - 50.0 * 60.0 - 30.0).abs() < 1e-6);
    }

    #[test]
    fn probe_sacd_falls_back_to_multi_channel_when_no_stereo() {
        let td = tempfile::tempdir().unwrap();
        let path = td.path().join("mc_only.iso");
        write_sacd_iso(&path, false, true, false, None, None, 0, None, 30);
        let info = probe_audio(&path).expect("probe");
        assert!(info.format_name.contains("MCH"));
        assert_eq!(info.channels, 6);
        assert_eq!(info.channel_layout, "5.1");
    }

    #[test]
    fn probe_sacd_marks_dst_encoded() {
        let td = tempfile::tempdir().unwrap();
        let path = td.path().join("dst.iso");
        write_sacd_iso(&path, true, false, true, None, None, 0, None, 60);
        let info = probe_audio(&path).expect("probe");
        assert!(info.format_name.contains("DST"), "got {}", info.format_name);
    }

    #[test]
    fn read_metadata_sacd_pulls_text_and_year_and_catalog() {
        let td = tempfile::tempdir().unwrap();
        let path = td.path().join("titled.iso");
        write_sacd_iso(
            &path,
            true,
            false,
            false,
            Some("Kind of Blue"),
            Some("Miles Davis"),
            1959,
            Some("PROC-001"),
            45,
        );
        let m = read_metadata(&path).expect("read_metadata");
        assert_eq!(m.album.as_deref(), Some("Kind of Blue"));
        assert_eq!(m.artist.as_deref(), Some("Miles Davis"));
        assert_eq!(m.year.as_deref(), Some("1959"));
        assert_eq!(m.catalog_number.as_deref(), Some("PROC-001"));
        assert_eq!(m.genre.as_deref(), Some("Jazz"));
        // Source-level title intentionally empty for SACDs.
        assert!(m.title.is_none());
        // Pre-emphasis not applicable to DSD.
        assert!(m.preemphasis_metadata.is_none());
    }

    #[test]
    fn probe_audio_passes_through_non_sacd_iso_with_no_magic() {
        // Build a 2 MB ISO (large enough to clear is_sacd_iso's
        // size threshold of 1,044,488 bytes) but with NO ScarletBook
        // magic anywhere. is_sacd_iso must reject it via the magic
        // check (not the size shortcut), so probe_audio must drop
        // through to ffmpeg.
        let td = tempfile::tempdir().unwrap();
        let path = td.path().join("plain_data.iso");
        let f = std::fs::File::create(&path).unwrap();
        f.set_len(2 * 1024 * 1024).unwrap(); // 2 MB of zeros
        drop(f);

        // Confirm the SACD detector says no.
        assert!(
            !crate::tui::sacd::is_sacd_iso(&path),
            "synthetic ISO should not match SACD magic"
        );

        // probe_audio should error from ffmpeg (zeros aren't a valid
        // audio container) — and the error must not be the SACD
        // parser's fingerprint.
        match probe_audio(&path) {
            Ok(_) => panic!("ffmpeg should not synthesize info from zeros"),
            Err(e) => {
                assert!(
                    !e.contains("SACD parse failed"),
                    "should have reached ffmpeg, not SACD branch: {}",
                    e
                );
            }
        }
    }

    #[test]
    fn probe_sacd_returns_err_on_magic_but_malformed_master_toc() {
        // File has SACDMTOC magic at LSN 510 but the rest of the
        // master_toc_t is all zeros — parse_master_toc rejects this
        // because both area pointers are 0 ("no playable areas").
        // is_sacd_iso passes the cheap magic-byte check; probe_sacd
        // must therefore propagate the parser's Err rather than
        // silently fall through.
        use crate::tui::sacd::*;
        use std::io::{Seek, SeekFrom, Write};

        let td = tempfile::tempdir().unwrap();
        let path = td.path().join("magic_only.iso");
        let total_sectors = 700u64;
        let f = std::fs::File::create(&path).unwrap();
        f.set_len(total_sectors * SECTOR_SIZE).unwrap();
        drop(f);
        let mut f = std::fs::File::options().write(true).open(&path).unwrap();
        f.seek(SeekFrom::Start(MASTER_TOC_LSNS[0] * SECTOR_SIZE))
            .unwrap();
        f.write_all(MASTER_TOC_MAGIC).unwrap();
        // The remaining 160 bytes of the master_toc_t stay zero,
        // which means area_1_toc_1_start = 0 AND area_2_toc_1_start = 0,
        // tripping parse_master_toc's no-playable-areas guard.
        drop(f);

        assert!(is_sacd_iso(&path), "magic should be detected");
        let res = probe_audio(&path);
        match res {
            Err(e) => assert!(
                e.contains("SACD parse failed") && e.contains("no playable areas"),
                "unexpected error message: {}",
                e,
            ),
            Ok(_) => panic!("probe_sacd should reject malformed master TOC"),
        }
    }

    #[test]
    fn probe_sacd_dst_multi_channel_format_name() {
        // High-bit-rate 5.1 SACDs nearly always use DST. The
        // format_name marker should call out both DST encoding
        // and the MCH area selection.
        let td = tempfile::tempdir().unwrap();
        let path = td.path().join("dst_mch.iso");
        // Stereo absent, multi-channel present, DST-encoded.
        write_sacd_iso(&path, false, true, true, None, None, 0, None, 75);
        let info = probe_audio(&path).expect("probe");
        assert!(
            info.format_name.contains("DST"),
            "format_name should mark DST: got {}",
            info.format_name
        );
        assert!(
            info.format_name.contains("MCH"),
            "format_name should mark MCH: got {}",
            info.format_name
        );
        assert_eq!(info.channels, 6);
        assert_eq!(info.channel_layout, "5.1");
    }
}
