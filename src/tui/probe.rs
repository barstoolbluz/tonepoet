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

/// Embedded artwork metadata read from tags. The editor deliberately keeps
/// only compact metadata here instead of storing raw image bytes; this avoids
/// retaining large artwork payloads in TUI state while preserving the typed
/// picture kind reported by Lofty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtworkInfo {
    pub picture_type: lofty::picture::PictureType,
    pub mime_type: String,
    pub data_size: usize,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

impl Default for ArtworkInfo {
    fn default() -> Self {
        Self {
            picture_type: lofty::picture::PictureType::Other,
            mime_type: String::new(),
            data_size: 0,
            width: None,
            height: None,
        }
    }
}

/// Metadata tags from the source file
#[derive(Debug, Clone, Default)]
pub struct SourceMetadata {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub genre: Option<String>,
    pub year: Option<String>,

    /// Encoder/tool/vendor string from source tags or format metadata when available.
    /// Keep this in the cached metadata model so Details does not need to infer
    /// Tool from already-rendered editor rows.
    pub tool: Option<String>,

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

    /// Embedded artwork blocks, without retaining the raw image bytes.
    pub artwork: Vec<ArtworkInfo>,
}


fn picture_dimensions(data: &[u8]) -> (Option<u32>, Option<u32>) {
    parse_png_dimensions(data)
        .or_else(|| parse_jpeg_dimensions(data))
        .unwrap_or((None, None))
}

fn parse_png_dimensions(data: &[u8]) -> Option<(Option<u32>, Option<u32>)> {
    if data.len() < 24 || &data[..8] != b"\x89PNG\r\n\x1a\n" || &data[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes([data[16], data[17], data[18], data[19]]);
    let height = u32::from_be_bytes([data[20], data[21], data[22], data[23]]);
    Some((Some(width), Some(height)))
}

fn parse_jpeg_dimensions(data: &[u8]) -> Option<(Option<u32>, Option<u32>)> {
    if data.len() < 4 || data[0] != 0xff || data[1] != 0xd8 {
        return None;
    }
    let mut i = 2usize;
    while i + 9 < data.len() {
        while i < data.len() && data[i] != 0xff {
            i += 1;
        }
        while i < data.len() && data[i] == 0xff {
            i += 1;
        }
        if i >= data.len() {
            return None;
        }
        let marker = data[i];
        i += 1;
        if marker == 0xd9 || marker == 0xda {
            return None;
        }
        if i + 2 > data.len() {
            return None;
        }
        let len = u16::from_be_bytes([data[i], data[i + 1]]) as usize;
        if len < 2 || i + len > data.len() {
            return None;
        }
        let is_sof = matches!(
            marker,
            0xc0 | 0xc1 | 0xc2 | 0xc3 | 0xc5 | 0xc6 | 0xc7 | 0xc9 | 0xca | 0xcb | 0xcd | 0xce | 0xcf
        );
        if is_sof && len >= 7 {
            let height = u16::from_be_bytes([data[i + 3], data[i + 4]]) as u32;
            let width = u16::from_be_bytes([data[i + 5], data[i + 6]]) as u32;
            return Some((Some(width), Some(height)));
        }
        i += len;
    }
    None
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

fn probe_bluray_disc(path: &Path) -> Result<SourceInfo, String> {
    let contents = crate::disc::bluray_utils::map_bluray_source(path)?;
    let presentation = contents
        .presentations
        .first()
        .ok_or_else(|| format!("Blu-ray disc has no audio streams: {}", path.display()))?;
    Ok(crate::tui::disc_browser::source_info_for_presentation(
        &contents,
        presentation,
    ))
}

pub fn probe_audio(path: &Path) -> Result<SourceInfo, String> {
    flac_metadata_writer::recover_before_read(path)?;
    if crate::disc::dvda_utils::is_dvda_source(path) {
        return probe_dvda_disc(path);
    }
    if crate::disc::dvdv_utils::is_dvdv_source(path) {
        return probe_dvdv_disc(path);
    }
    let mut bluray_probe_error = None;
    if crate::disc::bluray_utils::is_bluray_source(path) {
        match probe_bluray_disc(path) {
            Ok(info) => return Ok(info),
            Err(err) => {
                log::debug!(
                    "Blu-ray probe failed for '{}'; continuing with remaining source probes: {}",
                    path.display(),
                    err
                );
                bluray_probe_error = Some(err);
            }
        }
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

    let ctx = ffmpeg_next::format::input(&path).map_err(|e| {
        let ffmpeg_error = format!("Failed to open '{}': {}", path.display(), e);
        match bluray_probe_error.as_deref() {
            Some(bluray_error) => {
                format!("{ffmpeg_error}; Blu-ray probe also failed: {bluray_error}")
            }
            None => ffmpeg_error,
        }
    })?;

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

    let probed_sample_rate = audio.rate();
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

    // ffmpeg-next reports DSF/DFF dsd_u8 rates in bytes/second. Normalize
    // once at the TUI probe boundary so display and DSD-to-PCM cascades use
    // the same true-rate facts as the conversion pipeline. Reuse the pipeline
    // classifier so losslessly compressed DFF/DST is treated as DSD too.
    let (sample_rate, bit_depth) = normalize_tui_dsd_probe_facts(
        &codec_name,
        probed_sample_rate,
        bit_depth,
    );

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

fn normalize_tui_dsd_probe_facts(
    codec_name: &str,
    probed_sample_rate: u32,
    probed_bit_depth: Option<u32>,
) -> (u32, Option<u32>) {
    let (coding, _) = crate::convert::pipeline::classify_source_audio_probe(
        Some(codec_name),
        None,
        probed_bit_depth,
    );
    let (sample_rate, _) = crate::convert::pipeline::normalize_dsd_probe_rate(
        coding,
        probed_sample_rate,
        None,
    );
    let bit_depth = if coding == crate::convert::pipeline::SourceAudioCoding::Dsd {
        Some(1)
    } else {
        probed_bit_depth
    };
    (sample_rate, bit_depth)
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

    // SACD ISOs aren't tagged files in lofty's sense — pull the
    // album-level fields out of the ScarletBook Master TOC + SACDText
    // sector instead. Per-track text (titles per track) lives on the
    // editor's per-track populate path (C5), not the source-level
    // SourceMetadata.
    if super::sacd::is_sacd_iso(path) {
        return read_metadata_sacd(path);
    }
    if crate::dsf_tags::is_dsf(path) {
        return crate::dsf_tags::read(path).map(|snapshot| source_metadata_from_dsf(&snapshot));
    }

    flac_metadata_writer::recover_before_read(path)?;

    let tagged_file = lofty::read_from_path(path)
        .map_err(|e| format!("Failed to read tags from '{}': {}", path.display(), e))?;

    Ok(source_metadata_from_tags(path, tagged_file.tags(), true))
}

/// Lazily read raw embedded artwork bytes for one picture type.
///
/// The metadata editor stores only lightweight artwork facts in normal read
/// state. Preview rendering calls this for the currently selected artwork row,
/// so large images are not eagerly retained for every file/frame.
pub fn read_embedded_picture_bytes(
    path: &Path,
    picture_type: lofty::picture::PictureType,
) -> Result<Vec<u8>, String> {
    use lofty::file::TaggedFileExt;

    flac_metadata_writer::recover_before_read(path)?;

    let tagged_file = lofty::read_from_path(path)
        .map_err(|e| format!("Failed to read artwork from '{}': {}", path.display(), e))?;

    for tag in tagged_file.tags() {
        if let Some(picture) = tag
            .pictures()
            .iter()
            .find(|picture| picture.pic_type() == picture_type)
        {
            return Ok(picture.data().to_vec());
        }
    }

    Err(format!(
        "No embedded {:?} artwork found in '{}'",
        picture_type,
        path.display()
    ))
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
    meta.tool = from_sidecar("ENCODER")
        .or_else(|| from_sidecar("ENCODED_BY"))
        .or_else(|| from_sidecar("ENCODED BY"))
        .or_else(|| from_sidecar("VENDOR"))
        .or_else(|| from_sidecar("SOFTWARE"));
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

/// Blocking PE metadata check (tags + CUE sidecars + catalog evidence).
///
/// This can perform file/tag I/O and must not run from TUI reducers or other
/// event-loop code. Call it from a worker, `spawn_blocking`, or an already
/// blocking metadata/probe path only.
pub fn preemphasis_metadata_check_blocking(path: &Path) -> Option<String> {
    preemphasis_metadata_check(path)
}

/// Backward-compatible wrapper for existing worker-side callers. New code
/// should use `preemphasis_metadata_check_blocking()` so the blocking boundary
/// is visible at the call site.
#[allow(dead_code)]
pub fn preemphasis_metadata_check_pub(path: &Path) -> Option<String> {
    preemphasis_metadata_check_blocking(path)
}

/// Lightweight Phase 2 pre-emphasis check using PRE flags and catalog evidence
/// only. It never runs spectral analysis and deliberately excludes log-file
/// heuristics.
fn preemphasis_metadata_check(path: &Path) -> Option<String> {
    use super::preemphasis::catalog::check_catalog_evidence;
    use super::preemphasis::metadata::{check_cue_evidence, check_pre_flag_tag_evidence};

    // Tags (fastest).
    if let Some(ev) = check_pre_flag_tag_evidence(path) {
        return Some(ev.label().to_string());
    }
    // CUE FLAGS PRE sidecars.
    if let Some(ev) = check_cue_evidence(path) {
        return Some(ev.label().to_string());
    }
    // Catalog number matching.
    if let Some(cm) = check_catalog_evidence(path) {
        return Some(format!("catalog ({})", cm.catalog_number));
    }
    None
}


mod flac_metadata_writer {
    use std::io::{Read, Seek, SeekFrom, Write};
    use std::path::{Path, PathBuf};

    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;

    const FLAC_MAGIC: &[u8; 4] = b"fLaC";
    const JOURNAL_MAGIC_V2: &[u8] = b"TPFLACMJ2\0\0";
    const JOURNAL_MAGIC_V3: &[u8] = b"TPFLACMJ3\0\0";
    const JOURNAL_MAGIC_V4: &[u8] = b"TPFLACMJ4\0\0";
    const JOURNAL_MAGIC: &[u8] = b"TPFLACMJ5\0\0";
    const BLOCK_STREAMINFO: u8 = 0;
    const BLOCK_PADDING: u8 = 1;
    const BLOCK_VORBIS_COMMENT: u8 = 4;
    const BLOCK_PICTURE: u8 = 6;
    const BLOCK_HEADER_LEN: usize = 4;
    const MAX_BLOCK_BODY_LEN: usize = 0x00ff_ffff;
    const REWRITE_PADDING_BYTES: usize = 1024 * 1024;
    const STREAM_COPY_BUF: usize = 1024 * 1024;
    const ARTWORK_ROLLBACK_MAGIC_V1: &[u8] = b"TPFLACAJ1\0\0";
    const ARTWORK_ROLLBACK_MAGIC_V2: &[u8] = b"TPFLACAJ2\0\0";
    const ARTWORK_ROLLBACK_MAGIC_V3: &[u8] = b"TPFLACAJ3\0\0";
    const ARTWORK_ROLLBACK_MAGIC: &[u8] = b"TPFLACAJ4\0\0";
    const WRITE_LOCK_MAGIC_V1: &[u8] = b"TPFLACWL1\0\0";
    const WRITE_LOCK_MAGIC: &[u8] = b"TPFLACWL2\0\0";

    static OVERFLOW_REWRITE_LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    static COMMON_WRITE_LOCKS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<PathBuf>>> = std::sync::OnceLock::new();

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct ThreadWriteClaim {
        canonical_path: PathBuf,
        claim_token: u64,
    }

    thread_local! {
        static THREAD_WRITE_LOCKS: std::cell::RefCell<Vec<ThreadWriteClaim>> = std::cell::RefCell::new(Vec::new());
    }

    #[cfg(test)]
    type BackupAbsenceHook = dyn Fn(&Path) + Send + Sync + 'static;

    #[cfg(test)]
    type MetadataWriteLenHook = dyn Fn(&Path, usize) + Send + Sync + 'static;

    #[cfg(test)]
    type StreamRewriteBeforeRenameHook = dyn Fn(&Path, &Path) -> Result<(), String> + Send + Sync + 'static;

    #[cfg(test)]
    type StreamCopyChunkHook = dyn Fn(&Path, u64) + Send + Sync + 'static;

    #[cfg(test)]
    type StreamRewritePermitHook = dyn Fn(&Path) + Send + Sync + 'static;

    #[cfg(test)]
    type ParentDirSyncHook = dyn Fn(&Path, &str) -> Option<Result<(), String>> + Send + Sync + 'static;

    #[cfg(test)]
    type MetadataJournalRemoveHook = dyn Fn(&Path) -> Option<Result<(), String>> + Send + Sync + 'static;

    #[cfg(test)]
    type MetadataSnapshotRestoreHook = dyn Fn(&Path) -> Option<Result<(), String>> + Send + Sync + 'static;

    #[cfg(test)]
    type XattrCaptureHook = dyn Fn(&Path) -> Option<Result<(), String>> + Send + Sync + 'static;

    #[cfg(test)]
    type XattrRestoreHook = dyn Fn(&Path, &std::ffi::OsString) -> Option<Result<(), String>> + Send + Sync + 'static;

    #[cfg(test)]
    type AclCaptureHook = dyn Fn(&Path) -> Option<Result<AclSnapshot, String>> + Send + Sync + 'static;

    #[cfg(test)]
    type AclRestoreHook = dyn Fn(&Path, &AclSnapshot) -> Option<Result<(), String>> + Send + Sync + 'static;

    // Test hooks are process-global; two rules make them safe under the
    // parallel test runner. (1) Every installer scopes the hook to the
    // installing test's fixture directory, so a hook can never fire for a
    // bystander test's files. (2) Every installer holds one shared
    // serialization lock for the duration of its body, so two hooked tests
    // can never clobber each other's slot (TestHookGuard clears ALL slots on
    // drop). The lock is reentrant per thread because some tests nest
    // installers.
    #[cfg(test)]
    static HOOK_TEST_SERIALIZATION: std::sync::OnceLock<std::sync::Mutex<()>> =
        std::sync::OnceLock::new();

    #[cfg(test)]
    thread_local! {
        static HOOK_TEST_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    #[cfg(test)]
    pub(super) struct HookSerializationGuard {
        held: Option<std::sync::MutexGuard<'static, ()>>,
    }

    #[cfg(test)]
    impl Drop for HookSerializationGuard {
        fn drop(&mut self) {
            let _ = &self.held;
            HOOK_TEST_DEPTH.with(|depth| depth.set(depth.get() - 1));
        }
    }

    #[cfg(test)]
    pub(super) fn acquire_hook_test_serialization() -> HookSerializationGuard {
        let prior = HOOK_TEST_DEPTH.with(|depth| {
            let value = depth.get();
            depth.set(value + 1);
            value
        });
        let held = (prior == 0).then(|| {
            match HOOK_TEST_SERIALIZATION
                .get_or_init(|| std::sync::Mutex::new(()))
                .lock()
            {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            }
        });
        HookSerializationGuard { held }
    }

    #[cfg(test)]
    static TEST_BACKUP_ABSENCE_HOOK: std::sync::OnceLock<
        std::sync::Mutex<Option<std::sync::Arc<BackupAbsenceHook>>>,
    > = std::sync::OnceLock::new();

    #[cfg(test)]
    static TEST_METADATA_WRITE_LEN_HOOK: std::sync::OnceLock<
        std::sync::Mutex<Option<std::sync::Arc<MetadataWriteLenHook>>>,
    > = std::sync::OnceLock::new();

    #[cfg(test)]
    static TEST_STREAM_REWRITE_BEFORE_RENAME_HOOK: std::sync::OnceLock<
        std::sync::Mutex<Option<std::sync::Arc<StreamRewriteBeforeRenameHook>>>,
    > = std::sync::OnceLock::new();

    #[cfg(test)]
    static TEST_STREAM_COPY_CHUNK_HOOK: std::sync::OnceLock<
        std::sync::Mutex<Option<std::sync::Arc<StreamCopyChunkHook>>>,
    > = std::sync::OnceLock::new();

    #[cfg(test)]
    static TEST_STREAM_REWRITE_PERMIT_HOOK: std::sync::OnceLock<
        std::sync::Mutex<Option<std::sync::Arc<StreamRewritePermitHook>>>,
    > = std::sync::OnceLock::new();

    #[cfg(test)]
    static TEST_PARENT_DIR_SYNC_HOOK: std::sync::OnceLock<
        std::sync::Mutex<Option<std::sync::Arc<ParentDirSyncHook>>>,
    > = std::sync::OnceLock::new();

    #[cfg(test)]
    static TEST_METADATA_JOURNAL_REMOVE_HOOK: std::sync::OnceLock<
        std::sync::Mutex<Option<std::sync::Arc<MetadataJournalRemoveHook>>>,
    > = std::sync::OnceLock::new();

    #[cfg(test)]
    static TEST_METADATA_SNAPSHOT_RESTORE_HOOK: std::sync::OnceLock<
        std::sync::Mutex<Option<std::sync::Arc<MetadataSnapshotRestoreHook>>>,
    > = std::sync::OnceLock::new();

    #[cfg(test)]
    static TEST_XATTR_CAPTURE_HOOK: std::sync::OnceLock<
        std::sync::Mutex<Option<std::sync::Arc<XattrCaptureHook>>>,
    > = std::sync::OnceLock::new();

    #[cfg(test)]
    static TEST_XATTR_RESTORE_HOOK: std::sync::OnceLock<
        std::sync::Mutex<Option<std::sync::Arc<XattrRestoreHook>>>,
    > = std::sync::OnceLock::new();

    #[cfg(test)]
    static TEST_ACL_CAPTURE_HOOK: std::sync::OnceLock<
        std::sync::Mutex<Option<std::sync::Arc<AclCaptureHook>>>,
    > = std::sync::OnceLock::new();

    #[cfg(test)]
    static TEST_ACL_RESTORE_HOOK: std::sync::OnceLock<
        std::sync::Mutex<Option<std::sync::Arc<AclRestoreHook>>>,
    > = std::sync::OnceLock::new();

    #[derive(Debug, Clone)]
    struct FlacBlock {
        block_type: u8,
        data: Vec<u8>,
    }

    #[derive(Debug, Clone)]
    struct FlacMetadata {
        blocks: Vec<FlacBlock>,
        audio_start: u64,
        raw_metadata_region: Vec<u8>,
    }

    #[derive(Debug, Clone)]
    pub(super) struct FlacMetadataSnapshot {
        pub(super) audio_start: u64,
        pub(super) raw_metadata_region: Vec<u8>,
    }


    #[derive(Debug, Clone)]
    enum VorbisComment {
        Parsed { name: String, value: String },
        Raw(Vec<u8>),
    }

    #[derive(Debug, Clone)]
    struct VorbisComments {
        vendor: String,
        comments: Vec<VorbisComment>,
    }

    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub(super) struct FlacWriteReport {
        pub durability_warnings: Vec<String>,
    }

    impl FlacWriteReport {
        fn clean() -> Self {
            Self { durability_warnings: Vec::new() }
        }

        fn with_warning(mut self, warning: Option<String>) -> Self {
            if let Some(warning) = warning.filter(|warning| !warning.trim().is_empty()) {
                self.durability_warnings.push(warning);
            }
            self
        }

        fn extend_warnings<I>(&mut self, warnings: I)
        where
            I: IntoIterator<Item = String>,
        {
            self.durability_warnings
                .extend(warnings.into_iter().filter(|warning| !warning.trim().is_empty()));
        }
    }

    #[derive(Debug)]
    pub(super) struct FlacWriteClaim {
        lock_path: PathBuf,
        canonical_path: PathBuf,
        claim_token: u64,
        active: bool,
        reentrant: bool,
    }

    impl FlacWriteClaim {
        fn reentrant(lock_path: PathBuf, canonical_path: PathBuf, claim_token: u64) -> Self {
            Self { lock_path, canonical_path, claim_token, active: true, reentrant: true }
        }

        fn acquired(lock_path: PathBuf, canonical_path: PathBuf, claim_token: u64) -> Self {
            Self { lock_path, canonical_path, claim_token, active: true, reentrant: false }
        }

        fn claim_token(&self) -> u64 {
            self.claim_token
        }

        fn release_process_claim(&self) {
            THREAD_WRITE_LOCKS.with(|locks| {
                let mut locks = locks.borrow_mut();
                if let Some(pos) = locks.iter().rposition(|held| {
                    held.canonical_path == self.canonical_path && held.claim_token == self.claim_token
                }) {
                    locks.remove(pos);
                }
            });
            if let Some(lock_set) = COMMON_WRITE_LOCKS.get() {
                if let Ok(mut lock_set) = lock_set.lock() {
                    lock_set.remove(&self.canonical_path);
                }
            }
        }

        pub(super) fn release_with_warning(&mut self, context: &str) -> Option<String> {
            if !self.active {
                return None;
            }
            self.active = false;
            if self.reentrant {
                return None;
            }
            self.release_process_claim();
            match std::fs::remove_file(&self.lock_path) {
                Ok(()) => post_commit_parent_sync_warning(&self.lock_path, context),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => Some(format!(
                    "FLAC native write for '{}' committed, but common write lock '{}' was already absent during cleanup. The file mutation is complete; this may indicate an external cleanup race, so later recovery/read guards should be allowed to verify the file before further writes.",
                    self.canonical_path.display(),
                    self.lock_path.display()
                )),
                Err(err) => Some(format!(
                    "FLAC native write for '{}' committed, but cleanup of common write lock '{}' failed: {err}. The lock remains beside the media file and may block reads/writes until it is removed or recovered after the owner exits.",
                    self.canonical_path.display(),
                    self.lock_path.display()
                )),
            }
        }

        fn release_best_effort(&mut self) {
            if !self.active {
                return;
            }
            self.active = false;
            if self.reentrant {
                return;
            }
            self.release_process_claim();
            match std::fs::remove_file(&self.lock_path) {
                Ok(()) => {
                    let _ = sync_parent_dir(&self.lock_path, "FLAC common write lock removal");
                }
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => {}
            }
        }
    }

    impl Drop for FlacWriteClaim {
        fn drop(&mut self) {
            self.release_best_effort();
        }
    }

    #[derive(Debug)]
    pub(super) struct ArtworkRollbackJournalClaim {
        pub(super) path: PathBuf,
        pub(super) _write_claim: FlacWriteClaim,
    }

    impl std::ops::Deref for ArtworkRollbackJournalClaim {
        type Target = PathBuf;

        fn deref(&self) -> &Self::Target {
            &self.path
        }
    }

    impl AsRef<Path> for ArtworkRollbackJournalClaim {
        fn as_ref(&self) -> &Path {
            self.path.as_path()
        }
    }

    impl PartialEq for ArtworkRollbackJournalClaim {
        fn eq(&self, other: &Self) -> bool {
            self.path == other.path
        }
    }

    impl Eq for ArtworkRollbackJournalClaim {}

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct StreamRewriteCommit {
        durability_warning: Option<String>,
    }

    pub(super) fn is_probably_flac(path: &Path) -> bool {
        if path.extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.eq_ignore_ascii_case("flac"))
            .unwrap_or(false)
        {
            return true;
        }
        let mut magic = [0u8; 4];
        std::fs::File::open(path)
            .and_then(|mut file| file.read_exact(&mut magic))
            .map(|()| &magic == FLAC_MAGIC)
            .unwrap_or(false)
    }

    pub(super) fn write_vorbis_comment_changes(
        path: &Path,
        changes: &[(lofty::tag::ItemKey, Option<String>)],
        cancel: Option<&super::MetadataWriteCancelFlag>,
    ) -> Result<FlacWriteReport, String> {
        if changes.is_empty() {
            return Ok(FlacWriteReport::clean());
        }
        reject_symlink_native_write(path, "tag write")?;
        let mut write_claim = acquire_common_write_claim(path, "tag write")?;
        recover_metadata_journal(path)?;
        recover_artwork_rollback_journal_before_native_write(path, "tag write")?;
        reject_hardlinked_native_write(path, "tag write")?;
        let metadata = read_flac_metadata(path)?;
        let replacement = build_vorbis_comment_replacement(&metadata, changes)?;
        let mut report = write_replacement_blocks(path, &metadata, replacement, cancel)?;
        if let Some(warning) = write_claim.release_with_warning("FLAC common write lock removal after tag write") {
            report.durability_warnings.push(warning);
        }
        Ok(report)
    }

    pub(super) fn recover_before_read(path: &Path) -> Result<(), String> {
        if is_probably_flac(path) {
            recover_common_write_lock_for_read_path(path)?;
            recover_metadata_journal_for_read_path(path)?;
            recover_artwork_rollback_journal_for_read_path(path)?;
        }
        Ok(())
    }

    pub(super) fn recover_metadata_journals_in_directory(dir: &Path) -> Vec<String> {
        let mut messages = Vec::new();
        let Ok(entries) = std::fs::read_dir(dir) else {
            return messages;
        };
        for entry in entries.flatten() {
            let journal = entry.path();
            let Some(name) = journal.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if is_rewrite_tmp_file_name(name) {
                if rewrite_tmp_owner_appears_active(name) {
                    continue;
                }
                match std::fs::remove_file(&journal) {
                    Ok(()) => messages.push(format!(
                        "Removed stale FLAC rewrite temp {}",
                        journal.display()
                    )),
                    Err(err) => messages.push(format!(
                        "FLAC rewrite temp cleanup failed for {}: {err}",
                        journal.display()
                    )),
                }
                continue;
            }
            if let Some(original_name) = name.strip_suffix(".tonepoet-write-lock") {
                let original = journal.with_file_name(original_name);
                match recover_common_write_lock(&original) {
                    Ok(MetadataJournalRecovery::RecoveredOrCleaned) => messages.push(format!(
                        "Recovered stale FLAC common write lock for {}",
                        original.display()
                    )),
                    Ok(MetadataJournalRecovery::NoJournal | MetadataJournalRecovery::ActiveOwner) => {},
                    Err(err) => messages.push(format!(
                        "FLAC common write lock recovery failed for {}: {err}",
                        original.display()
                    )),
                }
                continue;
            }
            if let Some(original_name) = name.strip_suffix(".tonepoet-meta-journal") {
                let original = journal.with_file_name(original_name);
                match recover_metadata_journal_for_startup(&original) {
                    Ok(MetadataJournalRecovery::RecoveredOrCleaned) => messages.push(format!(
                        "Recovered FLAC metadata journal for {}",
                        original.display()
                    )),
                    Ok(MetadataJournalRecovery::NoJournal | MetadataJournalRecovery::ActiveOwner) => {},
                    Err(err) => messages.push(format!(
                        "FLAC metadata journal recovery failed for {}: {err}",
                        original.display()
                    )),
                }
                continue;
            }
            if let Some(original_name) = name.strip_suffix(".tonepoet-artwork-rollback") {
                let original = journal.with_file_name(original_name);
                match recover_artwork_rollback_journal(&original) {
                    Ok(()) => messages.push(format!(
                        "Recovered FLAC artwork rollback journal for {}",
                        original.display()
                    )),
                    Err(err) => messages.push(format!(
                        "FLAC artwork rollback journal recovery failed for {}: {err}",
                        original.display()
                    )),
                }
                continue;
            }
        }
        messages
    }

    pub(super) fn restore_metadata_snapshot(
        path: &Path,
        snapshot: &FlacMetadataSnapshot,
    ) -> Result<(), String> {
        #[cfg(test)]
        if let Some(result) = run_test_metadata_snapshot_restore_hook(path) {
            return result;
        }
        let _write_claim = acquire_common_write_claim(path, "metadata snapshot restore")?;
        match read_flac_metadata(path) {
            Ok(current) if current.audio_start == snapshot.audio_start => {
                overwrite_metadata_region(path, &snapshot.raw_metadata_region)
            }
            Ok(current) => {
                let blocks = decode_metadata_region(&snapshot.raw_metadata_region)?;
                stream_rewrite(path, current.audio_start, &blocks, None).map(|_| ())
            }
            Err(parse_err) => {
                let file_len = std::fs::metadata(path)
                    .map_err(|err| format!("stat FLAC for metadata restore '{}': {err}", path.display()))?
                    .len();
                if file_len < 4 + snapshot.raw_metadata_region.len() as u64 {
                    return Err(format!(
                        "cannot restore FLAC metadata for '{}': file is shorter than the saved metadata region after parse failure ({parse_err})",
                        path.display()
                    ));
                }
                overwrite_metadata_region(path, &snapshot.raw_metadata_region)
            }
        }
    }

    fn restore_metadata_snapshot_from_audio_start(
        path: &Path,
        snapshot: &FlacMetadataSnapshot,
        current_audio_start: u64,
    ) -> Result<(), String> {
        if current_audio_start == snapshot.audio_start {
            let file_len = std::fs::metadata(path)
                .map_err(|err| format!("stat FLAC for metadata restore '{}': {err}", path.display()))?
                .len();
            if file_len < 4 + snapshot.raw_metadata_region.len() as u64 {
                return Err(format!(
                    "cannot restore FLAC metadata for '{}': file is shorter than the saved metadata region",
                    path.display()
                ));
            }
            overwrite_metadata_region(path, &snapshot.raw_metadata_region)
        } else {
            let blocks = decode_metadata_region(&snapshot.raw_metadata_region)?;
            stream_rewrite(path, current_audio_start, &blocks, None).map(|_| ())
        }
    }

    pub(super) fn preview_picture_write(
        path: &Path,
        picture_type: lofty::picture::PictureType,
        mime_type: &str,
        image_bytes: &[u8],
    ) -> Result<(FlacMetadataSnapshot, Vec<u8>), String> {
        reject_symlink_native_write(path, "artwork write")?;
        let _write_claim = acquire_common_write_claim(path, "artwork write")?;
        recover_metadata_journal(path)?;
        recover_artwork_rollback_journal_before_native_write(path, "artwork write")?;
        reject_hardlinked_native_write(path, "artwork write")?;
        let metadata = read_flac_metadata(path)?;
        let replacement = build_picture_replacement(
            &metadata,
            Some((picture_type, mime_type, image_bytes)),
        )?;
        let intended_region = encode_replacement_region_for_identity(&metadata, replacement)?;
        Ok((
            FlacMetadataSnapshot {
                audio_start: metadata.audio_start,
                raw_metadata_region: metadata.raw_metadata_region,
            },
            intended_region,
        ))
    }

    pub(super) fn preview_picture_removal(
        path: &Path,
        picture_type: lofty::picture::PictureType,
    ) -> Result<Option<(FlacMetadataSnapshot, Vec<u8>)>, String> {
        reject_symlink_native_write(path, "artwork removal")?;
        let _write_claim = acquire_common_write_claim(path, "artwork removal")?;
        recover_metadata_journal(path)?;
        recover_artwork_rollback_journal_before_native_write(path, "artwork removal")?;
        reject_hardlinked_native_write(path, "artwork removal")?;
        let metadata = read_flac_metadata(path)?;
        let target_type = picture_type.as_u8() as u32;
        let has_target = metadata.blocks.iter().any(|block| {
            block.block_type == BLOCK_PICTURE
                && parse_picture_type_code(&block.data).ok() == Some(target_type)
        });
        if !has_target {
            return Ok(None);
        }
        let replacement = build_picture_replacement(&metadata, Some((picture_type, "", &[])))?;
        let intended_region = encode_replacement_region_for_identity(&metadata, replacement)?;
        Ok(Some((
            FlacMetadataSnapshot {
                audio_start: metadata.audio_start,
                raw_metadata_region: metadata.raw_metadata_region,
            },
            intended_region,
        )))
    }

    pub(super) fn write_picture_block(
        path: &Path,
        picture_type: lofty::picture::PictureType,
        mime_type: &str,
        image_bytes: &[u8],
        cancel: Option<&super::MetadataWriteCancelFlag>,
    ) -> Result<FlacWriteReport, String> {
        reject_symlink_native_write(path, "artwork write")?;
        let mut write_claim = acquire_common_write_claim(path, "artwork write")?;
        recover_metadata_journal(path)?;
        recover_artwork_rollback_journal_before_native_write(path, "artwork write")?;
        reject_hardlinked_native_write(path, "artwork write")?;
        let metadata = read_flac_metadata(path)?;
        let replacement = build_picture_replacement(
            &metadata,
            Some((picture_type, mime_type, image_bytes)),
        )?;
        let mut report = write_replacement_blocks(path, &metadata, replacement, cancel)?;
        if let Some(warning) = write_claim.release_with_warning("FLAC common write lock removal after artwork write") {
            report.durability_warnings.push(warning);
        }
        Ok(report)
    }

    pub(super) fn remove_picture_block(
        path: &Path,
        picture_type: lofty::picture::PictureType,
        cancel: Option<&super::MetadataWriteCancelFlag>,
    ) -> Result<FlacWriteReport, String> {
        reject_symlink_native_write(path, "artwork removal")?;
        let mut write_claim = acquire_common_write_claim(path, "artwork removal")?;
        recover_metadata_journal(path)?;
        recover_artwork_rollback_journal_before_native_write(path, "artwork removal")?;
        reject_hardlinked_native_write(path, "artwork removal")?;
        let metadata = read_flac_metadata(path)?;
        let target_type = picture_type.as_u8() as u32;
        let has_target = metadata.blocks.iter().any(|block| {
            block.block_type == BLOCK_PICTURE
                && parse_picture_type_code(&block.data).ok() == Some(target_type)
        });
        if !has_target {
            let mut report = FlacWriteReport::clean();
            if let Some(warning) = write_claim.release_with_warning("FLAC common write lock removal after no-op artwork removal") {
                report.durability_warnings.push(warning);
            }
            return Ok(report);
        }
        let replacement = build_picture_replacement(&metadata, Some((picture_type, "", &[])))?;
        let mut report = write_replacement_blocks(path, &metadata, replacement, cancel)?;
        if let Some(warning) = write_claim.release_with_warning("FLAC common write lock removal after artwork removal") {
            report.durability_warnings.push(warning);
        }
        Ok(report)
    }

    fn read_flac_metadata(path: &Path) -> Result<FlacMetadata, String> {
        let mut file = std::fs::File::open(path)
            .map_err(|err| format!("open FLAC '{}': {err}", path.display()))?;
        let mut magic = [0u8; 4];
        file.read_exact(&mut magic)
            .map_err(|err| format!("read FLAC magic '{}': {err}", path.display()))?;
        if &magic != FLAC_MAGIC {
            return Err(format!("'{}' is not a FLAC stream", path.display()));
        }

        let mut blocks = Vec::new();
        let mut raw_metadata_region = Vec::new();
        for block_index in 0..1024usize {
            let mut header = [0u8; BLOCK_HEADER_LEN];
            file.read_exact(&mut header)
                .map_err(|err| format!("read FLAC metadata block header '{}': {err}", path.display()))?;
            let is_last = header[0] & 0x80 != 0;
            let block_type = header[0] & 0x7f;
            let body_len = ((header[1] as usize) << 16) | ((header[2] as usize) << 8) | header[3] as usize;
            let mut data = vec![0u8; body_len];
            file.read_exact(&mut data)
                .map_err(|err| format!("read FLAC metadata block body '{}': {err}", path.display()))?;
            raw_metadata_region.extend_from_slice(&header);
            raw_metadata_region.extend_from_slice(&data);
            blocks.push(FlacBlock { block_type, data });
            if block_index == 0 {
                let first = blocks.first().expect("just pushed");
                if first.block_type != BLOCK_STREAMINFO || first.data.len() != 34 {
                    return Err(format!(
                        "invalid FLAC '{}': first metadata block is not 34-byte STREAMINFO",
                        path.display()
                    ));
                }
            }
            if is_last {
                let audio_start = file
                    .stream_position()
                    .map_err(|err| format!("read FLAC metadata offset '{}': {err}", path.display()))?;
                return Ok(FlacMetadata {
                    blocks,
                    audio_start,
                    raw_metadata_region,
                });
            }
        }
        Err(format!(
            "invalid FLAC '{}': metadata block chain did not terminate",
            path.display()
        ))
    }

    fn build_vorbis_comment_replacement(
        metadata: &FlacMetadata,
        changes: &[(lofty::tag::ItemKey, Option<String>)],
    ) -> Result<Vec<FlacBlock>, String> {
        let old_vorbis = metadata
            .blocks
            .iter()
            .find(|block| block.block_type == BLOCK_VORBIS_COMMENT)
            .map(|block| parse_vorbis_comments(&block.data))
            .transpose()?
            .unwrap_or_else(|| VorbisComments {
                vendor: "tonepoet".to_string(),
                comments: Vec::new(),
            });
        let mut vorbis = old_vorbis;
        apply_comment_changes(&mut vorbis, changes)?;
        let maybe_vorbis_block = if vorbis.comments.is_empty() {
            None
        } else {
            Some(FlacBlock {
                block_type: BLOCK_VORBIS_COMMENT,
                data: serialize_vorbis_comments(&vorbis)?,
            })
        };

        let mut replacement = Vec::with_capacity(metadata.blocks.len() + 1);
        let streaminfo = metadata
            .blocks
            .first()
            .ok_or_else(|| "FLAC metadata is empty".to_string())?;
        replacement.push(streaminfo.clone());

        let mut inserted_vorbis = false;
        for block in metadata.blocks.iter().skip(1) {
            match block.block_type {
                BLOCK_VORBIS_COMMENT => {
                    if !inserted_vorbis {
                        if let Some(vorbis_block) = maybe_vorbis_block.clone() {
                            replacement.push(vorbis_block);
                        }
                        inserted_vorbis = true;
                    }
                }
                BLOCK_PADDING => {}
                _ => replacement.push(block.clone()),
            }
        }

        if !inserted_vorbis {
            if let Some(vorbis_block) = maybe_vorbis_block {
                replacement.insert(1, vorbis_block);
            }
        }

        Ok(replacement)
    }

    fn build_picture_replacement(
        metadata: &FlacMetadata,
        replacement_picture: Option<(lofty::picture::PictureType, &str, &[u8])>,
    ) -> Result<Vec<FlacBlock>, String> {
        let target_type = replacement_picture.as_ref().map(|(picture_type, _, _)| picture_type.as_u8() as u32);
        let new_picture = match replacement_picture {
            Some((_picture_type, mime_type, data)) if !data.is_empty() => Some(FlacBlock {
                block_type: BLOCK_PICTURE,
                data: serialize_picture_block(target_type.expect("target type"), mime_type, data)?,
            }),
            _ => None,
        };

        let mut replacement = Vec::with_capacity(metadata.blocks.len() + usize::from(new_picture.is_some()));
        let streaminfo = metadata
            .blocks
            .first()
            .ok_or_else(|| "FLAC metadata is empty".to_string())?;
        replacement.push(streaminfo.clone());

        let mut inserted_picture = false;
        for block in metadata.blocks.iter().skip(1) {
            match block.block_type {
                BLOCK_PADDING => {}
                BLOCK_PICTURE if target_type.is_some_and(|target| {
                    parse_picture_type_code(&block.data).ok() == Some(target)
                }) => {
                    if !inserted_picture {
                        if let Some(picture) = new_picture.clone() {
                            replacement.push(picture);
                        }
                        inserted_picture = true;
                    }
                }
                BLOCK_VORBIS_COMMENT => {
                    replacement.push(block.clone());
                    if !inserted_picture {
                        if let Some(picture) = new_picture.clone() {
                            replacement.push(picture);
                            inserted_picture = true;
                        }
                    }
                }
                _ => replacement.push(block.clone()),
            }
        }

        if !inserted_picture {
            if let Some(picture) = new_picture {
                replacement.insert(1, picture);
            }
        }

        Ok(replacement)
    }

    fn parse_picture_type_code(data: &[u8]) -> Result<u32, String> {
        if data.len() < 4 {
            return Err("truncated FLAC PICTURE block".to_string());
        }
        Ok(u32::from_be_bytes([data[0], data[1], data[2], data[3]]))
    }

    fn serialize_picture_block(
        picture_type_code: u32,
        mime_type: &str,
        image_bytes: &[u8],
    ) -> Result<Vec<u8>, String> {
        let (width, height) = super::picture_dimensions(image_bytes);
        let mut out = Vec::new();
        push_be_u32(&mut out, picture_type_code);
        push_be_len_prefixed(&mut out, mime_type.as_bytes(), "picture MIME type")?;
        push_be_len_prefixed(&mut out, b"", "picture description")?;
        push_be_u32(&mut out, width.unwrap_or(0));
        push_be_u32(&mut out, height.unwrap_or(0));
        push_be_u32(&mut out, 0);
        push_be_u32(&mut out, 0);
        push_be_len_prefixed(&mut out, image_bytes, "picture data")?;
        if out.len() > MAX_BLOCK_BODY_LEN {
            return Err(format!("FLAC PICTURE block is too large: {} bytes", out.len()));
        }
        Ok(out)
    }

    fn decode_metadata_region(raw: &[u8]) -> Result<Vec<FlacBlock>, String> {
        let mut pos = 0usize;
        let mut blocks = Vec::new();
        for block_index in 0..1024usize {
            if raw.len().saturating_sub(pos) < BLOCK_HEADER_LEN {
                return Err("truncated FLAC metadata region".to_string());
            }
            let header = &raw[pos..pos + BLOCK_HEADER_LEN];
            pos += BLOCK_HEADER_LEN;
            let is_last = header[0] & 0x80 != 0;
            let block_type = header[0] & 0x7f;
            let body_len = ((header[1] as usize) << 16) | ((header[2] as usize) << 8) | header[3] as usize;
            let end = pos
                .checked_add(body_len)
                .ok_or_else(|| "FLAC metadata region length overflow".to_string())?;
            if end > raw.len() {
                return Err("truncated FLAC metadata block in saved region".to_string());
            }
            let data = raw[pos..end].to_vec();
            pos = end;
            blocks.push(FlacBlock { block_type, data });
            if block_index == 0 {
                let first = blocks.first().expect("just pushed");
                if first.block_type != BLOCK_STREAMINFO || first.data.len() != 34 {
                    return Err("saved FLAC metadata region does not start with STREAMINFO".to_string());
                }
            }
            if is_last {
                if pos != raw.len() {
                    return Err("saved FLAC metadata region has trailing bytes after last block".to_string());
                }
                return Ok(blocks);
            }
        }
        Err("saved FLAC metadata region did not terminate".to_string())
    }

    /// Alias groups whose spellings all refer to the SAME logical field.
    /// Editing any of them must remove every alias, or the write leaves a
    /// stale duplicate under the other spelling (a FLAC tagged TOTALTRACKS
    /// reads as TrackTotal, and the edit would write TRACKTOTAL alongside).
    fn vorbis_key_aliases(comment_key: &str) -> &'static [&'static str] {
        match comment_key {
            "TRACKTOTAL" | "TOTALTRACKS" => &["TRACKTOTAL", "TOTALTRACKS"],
            "DISCTOTAL" | "TOTALDISCS" => &["DISCTOTAL", "TOTALDISCS"],
            "COMMENT" | "DESCRIPTION" => &["COMMENT", "DESCRIPTION"],
            _ => &[],
        }
    }

    fn apply_comment_changes(
        vorbis: &mut VorbisComments,
        changes: &[(lofty::tag::ItemKey, Option<String>)],
    ) -> Result<(), String> {
        use std::collections::BTreeMap;

        // Resolve logical alias groups before mutating the comment list. The
        // metadata editor emits one canonical row per group, but callers may
        // still submit legacy + canonical keys together. Equal requests
        // coalesce; conflicting requests fail closed instead of making the
        // result depend on slice order.
        let mut resolved = BTreeMap::<String, Option<String>>::new();
        for (key, new_value) in changes {
            let raw_key = vorbis_comment_key(key)?;
            let aliases = vorbis_key_aliases(&raw_key);
            let comment_key = aliases.first().copied().unwrap_or(raw_key.as_str()).to_string();
            let normalized_value = new_value
                .as_ref()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty());
            if let Some(previous) = resolved.get(&comment_key) {
                if previous != &normalized_value {
                    return Err(format!(
                        "conflicting metadata changes target the same Vorbis alias group {comment_key}: {previous:?} versus {normalized_value:?}"
                    ));
                }
                continue;
            }
            resolved.insert(comment_key, normalized_value);
        }

        for (comment_key, new_value) in resolved {
            let aliases = vorbis_key_aliases(&comment_key);
            vorbis.comments.retain(|comment| match comment {
                VorbisComment::Parsed { name, .. } => {
                    !name.eq_ignore_ascii_case(&comment_key)
                        && !aliases.iter().any(|alias| name.eq_ignore_ascii_case(alias))
                }
                VorbisComment::Raw(_) => true,
            });
            if let Some(value) = new_value {
                validate_vorbis_field_name(&comment_key)?;
                vorbis.comments.push(VorbisComment::Parsed {
                    name: comment_key,
                    value,
                });
            }
        }
        Ok(())
    }

    fn vorbis_comment_key(key: &lofty::tag::ItemKey) -> Result<String, String> {
        let mapped = match key {
            lofty::tag::ItemKey::Unknown(name) => Some(name.as_str()),
            _ => key.map_key(lofty::tag::TagType::VorbisComments, true),
        }
        .ok_or_else(|| format!("cannot map {:?} to a FLAC Vorbis comment key", key))?;
        let mapped = mapped.trim();
        if mapped.is_empty() {
            return Err(format!("cannot map {:?} to a non-empty FLAC Vorbis comment key", key));
        }
        Ok(mapped.to_ascii_uppercase())
    }

    fn validate_vorbis_field_name(name: &str) -> Result<(), String> {
        if name.bytes().all(|b| (0x20..=0x7d).contains(&b) && b != b'=') {
            Ok(())
        } else {
            Err(format!("invalid FLAC Vorbis comment field name: {name:?}"))
        }
    }

    fn parse_vorbis_comments(data: &[u8]) -> Result<VorbisComments, String> {
        let mut pos = 0usize;
        let vendor_len = read_le_u32(data, &mut pos)? as usize;
        let vendor_bytes = read_bytes(data, &mut pos, vendor_len)?;
        let vendor = String::from_utf8(vendor_bytes.to_vec())
            .map_err(|_| "FLAC Vorbis vendor string is not valid UTF-8".to_string())?;
        let count = read_le_u32(data, &mut pos)? as usize;
        let mut comments = Vec::with_capacity(count);
        for _ in 0..count {
            let len = read_le_u32(data, &mut pos)? as usize;
            let raw = read_bytes(data, &mut pos, len)?.to_vec();
            match String::from_utf8(raw.clone()) {
                Ok(text) => {
                    if let Some((name, value)) = text.split_once('=') {
                        comments.push(VorbisComment::Parsed {
                            name: name.to_string(),
                            value: value.to_string(),
                        });
                    } else {
                        comments.push(VorbisComment::Raw(raw));
                    }
                }
                Err(_) => comments.push(VorbisComment::Raw(raw)),
            }
        }
        Ok(VorbisComments { vendor, comments })
    }

    fn serialize_vorbis_comments(vorbis: &VorbisComments) -> Result<Vec<u8>, String> {
        let mut out = Vec::new();
        push_le_u32(&mut out, checked_u32_len(vorbis.vendor.len(), "vendor")?);
        out.extend_from_slice(vorbis.vendor.as_bytes());
        push_le_u32(&mut out, checked_u32_len(vorbis.comments.len(), "comment count")?);
        for comment in &vorbis.comments {
            match comment {
                VorbisComment::Parsed { name, value } => {
                    validate_vorbis_field_name(name)?;
                    let len = name.len().saturating_add(1).saturating_add(value.len());
                    push_le_u32(&mut out, checked_u32_len(len, "comment")?);
                    out.extend_from_slice(name.as_bytes());
                    out.push(b'=');
                    out.extend_from_slice(value.as_bytes());
                }
                VorbisComment::Raw(raw) => {
                    push_le_u32(&mut out, checked_u32_len(raw.len(), "raw comment")?);
                    out.extend_from_slice(raw);
                }
            }
        }
        if out.len() > MAX_BLOCK_BODY_LEN {
            return Err(format!(
                "FLAC Vorbis comment block is too large: {} bytes",
                out.len()
            ));
        }
        Ok(out)
    }

    fn write_replacement_blocks(
        path: &Path,
        metadata: &FlacMetadata,
        mut replacement: Vec<FlacBlock>,
        cancel: Option<&super::MetadataWriteCancelFlag>,
    ) -> Result<FlacWriteReport, String> {
        let old_metadata_len = metadata.raw_metadata_region.len();
        let new_len_without_padding = encoded_blocks_len(&replacement)?;
        if new_len_without_padding <= old_metadata_len {
            let slack = old_metadata_len - new_len_without_padding;
            if slack == 0 || slack >= BLOCK_HEADER_LEN {
                if slack >= BLOCK_HEADER_LEN {
                    replacement.push(FlacBlock {
                        block_type: BLOCK_PADDING,
                        data: vec![0u8; slack - BLOCK_HEADER_LEN],
                    });
                }
                let encoded = encode_blocks(&replacement)?;
                debug_assert_eq!(encoded.len(), old_metadata_len);
                super::check_metadata_write_cancel(cancel, "before in-place FLAC metadata write")?;
                let mut durability_warnings = Vec::new();
                if let Some(warning) = write_metadata_journal(path, metadata, Some(&encoded))? {
                    durability_warnings.push(warning);
                }
                #[cfg(test)]
                run_test_backup_absence_hook(path);
                let write_result = overwrite_metadata_region(path, &encoded);
                if let Err(err) = write_result {
                    let recover_result = recover_owned_metadata_journal(path);
                    return Err(match recover_result {
                        Ok(()) => format!("FLAC metadata write failed and was restored: {err}"),
                        Err(recover_err) => format!(
                            "FLAC metadata write failed ({err}) and recovery failed: {recover_err}"
                        ),
                    });
                }
                if let Some(warning) = remove_metadata_journal_after_committed_write(path)? {
                    durability_warnings.push(warning);
                }
                let mut report = FlacWriteReport::clean();
                report.extend_warnings(durability_warnings);
                return Ok(report);
            }
        }

        append_padding(&mut replacement, REWRITE_PADDING_BYTES)?;
        let commit = stream_rewrite(path, metadata.audio_start, &replacement, cancel)?;
        Ok(FlacWriteReport::clean()
            .with_warning(commit.durability_warning))
    }

    fn encode_replacement_region_for_identity(
        metadata: &FlacMetadata,
        mut replacement: Vec<FlacBlock>,
    ) -> Result<Vec<u8>, String> {
        let old_metadata_len = metadata.raw_metadata_region.len();
        let new_len_without_padding = encoded_blocks_len(&replacement)?;
        if new_len_without_padding <= old_metadata_len {
            let slack = old_metadata_len - new_len_without_padding;
            if slack == 0 || slack >= BLOCK_HEADER_LEN {
                if slack >= BLOCK_HEADER_LEN {
                    replacement.push(FlacBlock {
                        block_type: BLOCK_PADDING,
                        data: vec![0u8; slack - BLOCK_HEADER_LEN],
                    });
                }
                let encoded = encode_blocks(&replacement)?;
                debug_assert_eq!(encoded.len(), old_metadata_len);
                return Ok(encoded);
            }
        }
        append_padding(&mut replacement, REWRITE_PADDING_BYTES)?;
        encode_blocks(&replacement)
    }

    fn append_padding(blocks: &mut Vec<FlacBlock>, padding_bytes: usize) -> Result<(), String> {
        if padding_bytes > MAX_BLOCK_BODY_LEN {
            return Err(format!("requested FLAC padding is too large: {padding_bytes} bytes"));
        }
        blocks.push(FlacBlock {
            block_type: BLOCK_PADDING,
            data: vec![0u8; padding_bytes],
        });
        Ok(())
    }

    fn encoded_blocks_len(blocks: &[FlacBlock]) -> Result<usize, String> {
        blocks.iter().try_fold(0usize, |acc, block| {
            if block.data.len() > MAX_BLOCK_BODY_LEN {
                return Err(format!(
                    "FLAC metadata block {} is too large: {} bytes",
                    block.block_type,
                    block.data.len()
                ));
            }
            acc.checked_add(BLOCK_HEADER_LEN + block.data.len())
                .ok_or_else(|| "FLAC metadata block length overflow".to_string())
        })
    }

    fn encode_blocks(blocks: &[FlacBlock]) -> Result<Vec<u8>, String> {
        if blocks.is_empty() {
            return Err("cannot encode empty FLAC metadata block list".to_string());
        }
        let mut out = Vec::with_capacity(encoded_blocks_len(blocks)?);
        for (idx, block) in blocks.iter().enumerate() {
            if block.block_type > 0x7f {
                return Err(format!("invalid FLAC metadata block type: {}", block.block_type));
            }
            if block.data.len() > MAX_BLOCK_BODY_LEN {
                return Err(format!(
                    "FLAC metadata block {} is too large: {} bytes",
                    block.block_type,
                    block.data.len()
                ));
            }
            let last = idx + 1 == blocks.len();
            out.push((if last { 0x80 } else { 0 }) | (block.block_type & 0x7f));
            let len = block.data.len() as u32;
            out.extend_from_slice(&len.to_be_bytes()[1..]);
            out.extend_from_slice(&block.data);
        }
        Ok(out)
    }

    fn reject_symlink_native_write(path: &Path, operation: &str) -> Result<(), String> {
        let meta = std::fs::symlink_metadata(path)
            .map_err(|err| format!("stat FLAC path before native {operation} '{}': {err}", path.display()))?;
        if meta.file_type().is_symlink() {
            return Err(format!(
                "refusing native FLAC metadata-region {operation} for '{}': the path is a symlink. The native writer uses path-local recovery journals, so mutating the symlink target while storing recovery state beside the symlink would not provide reliable crash recovery. Rewrite the canonical target path instead.",
                path.display()
            ));
        }
        Ok(())
    }

    #[cfg(unix)]
    fn reject_hardlinked_native_write(path: &Path, operation: &str) -> Result<(), String> {
        use std::os::unix::fs::MetadataExt;

        let meta = std::fs::metadata(path)
            .map_err(|err| format!("stat FLAC path before native {operation} '{}': {err}", path.display()))?;
        if meta.nlink() > 1 {
            return Err(format!(
                "refusing native FLAC metadata-region {operation} for '{}': the file has {} hardlinks. In-place writes preserve the shared inode, but the recovery journal is path-local; a crash could leave hardlink aliases unable to find the journal. Remove hardlinks or rewrite a de-hardlinked copy before editing metadata.",
                path.display(),
                meta.nlink()
            ));
        }
        Ok(())
    }

    #[cfg(not(unix))]
    fn reject_hardlinked_native_write(_path: &Path, _operation: &str) -> Result<(), String> {
        Ok(())
    }

    fn overwrite_metadata_region(path: &Path, encoded_metadata_region: &[u8]) -> Result<(), String> {
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|err| format!("open FLAC for metadata write '{}': {err}", path.display()))?;
        let mut magic = [0u8; 4];
        file.read_exact(&mut magic)
            .map_err(|err| format!("verify FLAC magic before write '{}': {err}", path.display()))?;
        if &magic != FLAC_MAGIC {
            return Err(format!("refusing to write '{}': FLAC magic changed", path.display()));
        }
        file.seek(SeekFrom::Start(4))
            .map_err(|err| format!("seek FLAC metadata '{}': {err}", path.display()))?;
        #[cfg(test)]
        run_test_metadata_write_len_hook(path, encoded_metadata_region.len());
        file.write_all(encoded_metadata_region)
            .map_err(|err| format!("write FLAC metadata '{}': {err}", path.display()))?;
        file.flush()
            .map_err(|err| format!("flush FLAC metadata '{}': {err}", path.display()))?;
        file.sync_data()
            .map_err(|err| format!("sync FLAC metadata '{}': {err}", path.display()))?;
        Ok(())
    }

    fn stream_rewrite(
        path: &Path,
        old_audio_start: u64,
        blocks: &[FlacBlock],
        cancel: Option<&super::MetadataWriteCancelFlag>,
    ) -> Result<StreamRewriteCommit, String> {
        super::check_metadata_write_cancel(cancel, "before starting FLAC overflow rewrite")?;
        reject_symlink_overflow_rewrite(path)?;
        let _overflow_rewrite_permit = acquire_overflow_rewrite_permit(path, cancel)?;
        let tmp_path = rewrite_tmp_path(path);
        let cleanup = CleanupPath::new(tmp_path.clone());
        let mut input = std::fs::File::open(path)
            .map_err(|err| format!("open FLAC for streaming rewrite '{}': {err}", path.display()))?;
        let source_metadata = input
            .metadata()
            .map_err(|err| format!("stat FLAC before rewrite '{}': {err}", path.display()))?;
        reject_hardlinked_overflow_rewrite(path, &source_metadata)?;
        let source_identity = SourceFileIdentity::capture(path, &source_metadata)?;
        let preservation = OriginalFileMetadata::capture(path, &source_metadata)?;
        let mut output = create_restrictive_rewrite_temp(&tmp_path)
            .map_err(|err| format!("create FLAC rewrite temp '{}': {err}", tmp_path.display()))?;
        output
            .write_all(FLAC_MAGIC)
            .map_err(|err| format!("write FLAC magic '{}': {err}", tmp_path.display()))?;
        let encoded = encode_blocks(blocks)?;
        output
            .write_all(&encoded)
            .map_err(|err| format!("write FLAC metadata '{}': {err}", tmp_path.display()))?;
        input
            .seek(SeekFrom::Start(old_audio_start))
            .map_err(|err| format!("seek FLAC audio '{}': {err}", path.display()))?;
        copy_stream_bounded(path, &mut input, &mut output, cancel)
            .map_err(|err| format!("stream FLAC audio rewrite '{}': {err}", path.display()))?;
        output
            .flush()
            .map_err(|err| format!("flush FLAC rewrite '{}': {err}", tmp_path.display()))?;
        output
            .sync_all()
            .map_err(|err| format!("sync FLAC rewrite '{}': {err}", tmp_path.display()))?;
        drop(output);
        preservation.apply_to_replacement(&tmp_path)?;
        std::fs::File::open(&tmp_path)
            .and_then(|file| file.sync_all())
            .map_err(|err| format!("sync preserved FLAC rewrite metadata '{}': {err}", tmp_path.display()))?;
        #[cfg(test)]
        run_test_stream_rewrite_before_rename_hook(path, &tmp_path)?;
        super::check_metadata_write_cancel(cancel, "before committing FLAC overflow rewrite")?;
        source_identity.validate_unchanged_before_commit(path)?;
        std::fs::rename(&tmp_path, path)
            .map_err(|err| format!("commit FLAC rewrite '{}': {err}", path.display()))?;
        cleanup.disarm();
        let durability_warning = post_commit_parent_sync_warning(path, "FLAC overflow rewrite commit");
        Ok(StreamRewriteCommit { durability_warning })
    }

    // `path` only feeds the cfg(test) permit hook.
    #[cfg_attr(not(test), allow(unused_variables))]
    fn acquire_overflow_rewrite_permit(
        path: &Path,
        cancel: Option<&super::MetadataWriteCancelFlag>,
    ) -> Result<std::sync::MutexGuard<'static, ()>, String> {
        let lock = OVERFLOW_REWRITE_LOCK.get_or_init(|| std::sync::Mutex::new(()));
        loop {
            match lock.try_lock() {
                Ok(guard) => {
                    #[cfg(test)]
                    run_test_stream_rewrite_permit_hook(path);
                    return Ok(guard);
                }
                Err(std::sync::TryLockError::Poisoned(poisoned)) => {
                    #[cfg(test)]
                    run_test_stream_rewrite_permit_hook(path);
                    return Ok(poisoned.into_inner());
                }
                Err(std::sync::TryLockError::WouldBlock) => {
                    super::check_metadata_write_cancel(cancel, "waiting for another FLAC overflow rewrite to finish")?;
                    std::thread::sleep(std::time::Duration::from_millis(25));
                }
            }
        }
    }

    #[cfg(unix)]
    fn reject_symlink_overflow_rewrite(path: &Path) -> Result<(), String> {
        let meta = std::fs::symlink_metadata(path)
            .map_err(|err| format!("stat FLAC path before overflow rewrite '{}': {err}", path.display()))?;
        if meta.file_type().is_symlink() {
            return Err(format!(
                "refusing FLAC metadata overflow rewrite for '{}': the path is a symlink; replacing it would overwrite the symlink itself rather than safely preserving symlink identity. Rewrite the canonical target path or add enough FLAC padding for an in-place edit.",
                path.display()
            ));
        }
        Ok(())
    }

    #[cfg(not(unix))]
    fn reject_symlink_overflow_rewrite(_path: &Path) -> Result<(), String> {
        Ok(())
    }

    #[cfg(unix)]
    fn reject_hardlinked_overflow_rewrite(path: &Path, metadata: &std::fs::Metadata) -> Result<(), String> {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() > 1 {
            return Err(format!(
                "refusing FLAC metadata overflow rewrite for '{}': metadata growth requires replacing the file and would break {} hardlinks; remove hardlinks or add FLAC padding first",
                path.display(),
                metadata.nlink()
            ));
        }
        Ok(())
    }

    #[cfg(not(unix))]
    fn reject_hardlinked_overflow_rewrite(_path: &Path, _metadata: &std::fs::Metadata) -> Result<(), String> {
        Ok(())
    }

    #[cfg(unix)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct SourceFileIdentity {
        dev: u64,
        ino: u64,
        len: u64,
        nlink: u64,
        mode: u32,
        uid: u32,
        gid: u32,
        mtime_sec: i64,
        mtime_nsec: i64,
        ctime_sec: i64,
        ctime_nsec: i64,
    }

    #[cfg(unix)]
    impl SourceFileIdentity {
        fn capture(path: &Path, metadata: &std::fs::Metadata) -> Result<Self, String> {
            let link_meta = std::fs::symlink_metadata(path)
                .map_err(|err| format!("stat FLAC path before overflow rewrite '{}': {err}", path.display()))?;
            if link_meta.file_type().is_symlink() {
                return Err(format!(
                    "refusing FLAC metadata overflow rewrite for '{}': the path is a symlink; replacing it would overwrite the symlink itself rather than safely preserving symlink identity. Rewrite the canonical target path or add enough FLAC padding for an in-place edit.",
                    path.display()
                ));
            }
            use std::os::unix::fs::MetadataExt;
            Ok(Self {
                dev: metadata.dev(),
                ino: metadata.ino(),
                len: metadata.len(),
                nlink: metadata.nlink(),
                mode: metadata.mode() & 0o7777,
                uid: metadata.uid(),
                gid: metadata.gid(),
                mtime_sec: metadata.mtime(),
                mtime_nsec: metadata.mtime_nsec(),
                ctime_sec: metadata.ctime(),
                ctime_nsec: metadata.ctime_nsec(),
            })
        }

        fn validate_unchanged_before_commit(&self, path: &Path) -> Result<(), String> {
            let link_meta = std::fs::symlink_metadata(path)
                .map_err(|err| format!("revalidate FLAC before overflow rewrite commit '{}': {err}", path.display()))?;
            if link_meta.file_type().is_symlink() {
                return Err(format!(
                    "refusing to commit FLAC overflow rewrite for '{}': path became a symlink while the rewrite was in progress",
                    path.display()
                ));
            }
            let current = std::fs::metadata(path)
                .map_err(|err| format!("revalidate FLAC before overflow rewrite commit '{}': {err}", path.display()))?;
            use std::os::unix::fs::MetadataExt;
            let current_identity = Self {
                dev: current.dev(),
                ino: current.ino(),
                len: current.len(),
                nlink: current.nlink(),
                mode: current.mode() & 0o7777,
                uid: current.uid(),
                gid: current.gid(),
                mtime_sec: current.mtime(),
                mtime_nsec: current.mtime_nsec(),
                ctime_sec: current.ctime(),
                ctime_nsec: current.ctime_nsec(),
            };
            if current_identity == *self {
                return Ok(());
            }
            let mut changed = Vec::new();
            if current_identity.dev != self.dev || current_identity.ino != self.ino {
                changed.push("filesystem identity");
            }
            if current_identity.len != self.len {
                changed.push("file length");
            }
            if current_identity.nlink != self.nlink {
                changed.push("hardlink count");
            }
            if current_identity.mode != self.mode {
                changed.push("mode");
            }
            if current_identity.uid != self.uid || current_identity.gid != self.gid {
                changed.push("owner/group");
            }
            if current_identity.mtime_sec != self.mtime_sec || current_identity.mtime_nsec != self.mtime_nsec {
                changed.push("mtime");
            }
            if current_identity.ctime_sec != self.ctime_sec || current_identity.ctime_nsec != self.ctime_nsec {
                changed.push("ctime");
            }
            if changed.is_empty() {
                changed.push("metadata");
            }
            Err(format!(
                "refusing to commit FLAC overflow rewrite for '{}': source changed during rewrite ({})",
                path.display(),
                changed.join(", ")
            ))
        }
    }

    #[cfg(not(unix))]
    #[derive(Debug, Clone)]
    struct SourceFileIdentity {
        len: u64,
        modified: Option<std::time::SystemTime>,
        readonly: bool,
    }

    #[cfg(not(unix))]
    impl SourceFileIdentity {
        fn capture(_path: &Path, metadata: &std::fs::Metadata) -> Result<Self, String> {
            Ok(Self {
                len: metadata.len(),
                modified: metadata.modified().ok(),
                readonly: metadata.permissions().readonly(),
            })
        }

        fn validate_unchanged_before_commit(&self, path: &Path) -> Result<(), String> {
            let current = std::fs::metadata(path)
                .map_err(|err| format!("revalidate FLAC before overflow rewrite commit '{}': {err}", path.display()))?;
            let current_identity = Self {
                len: current.len(),
                modified: current.modified().ok(),
                readonly: current.permissions().readonly(),
            };
            if current_identity.len == self.len
                && current_identity.modified == self.modified
                && current_identity.readonly == self.readonly
            {
                return Ok(());
            }
            Err(format!(
                "refusing to commit FLAC overflow rewrite for '{}': source changed during rewrite",
                path.display()
            ))
        }
    }

    #[cfg(unix)]
    fn create_restrictive_rewrite_temp(path: &Path) -> std::io::Result<std::fs::File> {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
    }

    #[cfg(not(unix))]
    fn create_restrictive_rewrite_temp(path: &Path) -> std::io::Result<std::fs::File> {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
    }

    #[cfg(unix)]
    struct OriginalFileMetadata {
        mode: u32,
        uid: u32,
        gid: u32,
        atime_sec: i64,
        atime_nsec: i64,
        mtime_sec: i64,
        mtime_nsec: i64,
        xattrs: Vec<(std::ffi::OsString, Vec<u8>)>,
        acl_snapshot: AclSnapshot,
    }

    #[allow(dead_code)]
    #[derive(Debug, Clone)]
    pub(super) enum AclSnapshot {
        /// ACL tools or ACL support are absent. This is an explicit capability
        /// downgrade, not a failed preservation attempt.
        Unsupported(String),
        /// getfacl/setfacl are unavailable and no existing POSIX ACL xattr was
        /// detected. Mode bits are still preserved.
        ToolsUnavailable(String),
        /// getfacl succeeded and reported only base mode-equivalent ACL entries.
        /// Preserving mode bits is sufficient.
        NoExtended(Vec<u8>),
        /// getfacl succeeded and reported extended ACL semantics that must be
        /// restored before the temp-file replacement may commit.
        Captured(Vec<u8>),
    }

    #[cfg(unix)]
    impl OriginalFileMetadata {
        fn capture(path: &Path, metadata: &std::fs::Metadata) -> Result<Self, String> {
            use std::os::unix::fs::MetadataExt;
            Ok(Self {
                mode: metadata.mode() & 0o7777,
                uid: metadata.uid(),
                gid: metadata.gid(),
                atime_sec: metadata.atime(),
                atime_nsec: metadata.atime_nsec(),
                mtime_sec: metadata.mtime(),
                mtime_nsec: metadata.mtime_nsec(),
                xattrs: linux_xattrs(path)?,
                acl_snapshot: capture_acl_text(path)?,
            })
        }

        fn apply_to_replacement(&self, tmp_path: &Path) -> Result<(), String> {
            let tmp_c = c_path(tmp_path)?;
            // Ownership and mode are security-relevant. The temp is created 0600;
            // if the original has a different owner/group and the platform refuses
            // to preserve it, abort before rename so the original remains intact.
            let chown_result = unsafe { libc::chown(tmp_c.as_ptr(), self.uid, self.gid) };
            if chown_result != 0 {
                let err = std::io::Error::last_os_error();
                let current = std::fs::metadata(tmp_path)
                    .map_err(|meta_err| format!("stat FLAC rewrite temp '{}': {meta_err}", tmp_path.display()))?;
                use std::os::unix::fs::MetadataExt;
                if current.uid() != self.uid || current.gid() != self.gid {
                    return Err(format!(
                        "preserve FLAC owner/group on rewrite temp '{}': {err}",
                        tmp_path.display()
                    ));
                }
            }
            if unsafe { libc::chmod(tmp_c.as_ptr(), self.mode as libc::mode_t) } != 0 {
                return Err(format!(
                    "preserve FLAC permissions on rewrite temp '{}': {}",
                    tmp_path.display(),
                    std::io::Error::last_os_error()
                ));
            }
            apply_linux_xattrs(tmp_path, &self.xattrs)?;
            apply_acl_text(tmp_path, &self.acl_snapshot)?;
            preserve_timestamps(tmp_path, self.atime_sec, self.atime_nsec, self.mtime_sec, self.mtime_nsec);
            Ok(())
        }
    }

    #[cfg(not(unix))]
    struct OriginalFileMetadata {
        permissions: std::fs::Permissions,
    }

    #[cfg(not(unix))]
    impl OriginalFileMetadata {
        fn capture(_path: &Path, metadata: &std::fs::Metadata) -> Result<Self, String> {
            Ok(Self { permissions: metadata.permissions() })
        }

        fn apply_to_replacement(&self, tmp_path: &Path) -> Result<(), String> {
            std::fs::set_permissions(tmp_path, self.permissions.clone())
                .map_err(|err| format!("preserve FLAC permissions on rewrite temp '{}': {err}", tmp_path.display()))
        }
    }

    #[cfg(unix)]
    fn c_path(path: &Path) -> Result<std::ffi::CString, String> {
        use std::os::unix::ffi::OsStrExt;
        std::ffi::CString::new(path.as_os_str().as_bytes())
            .map_err(|_| format!("path contains NUL byte: '{}'", path.display()))
    }

    #[cfg(all(target_os = "linux", unix))]
    fn linux_xattrs(path: &Path) -> Result<Vec<(std::ffi::OsString, Vec<u8>)>, String> {
        use std::os::unix::ffi::OsStringExt;
        #[cfg(test)]
        if let Some(result) = run_test_xattr_capture_hook(path) {
            result?;
        }
        let path_c = c_path(path)?;
        let len = unsafe { libc::listxattr(path_c.as_ptr(), std::ptr::null_mut(), 0) };
        if len < 0 {
            let err = std::io::Error::last_os_error();
            if unsupported_metadata_errno(&err) {
                return Ok(Vec::new());
            }
            return Err(format!("capture FLAC xattrs for '{}': listxattr failed: {err}", path.display()));
        }
        if len == 0 {
            return Ok(Vec::new());
        }
        let mut names = vec![0u8; len as usize];
        let got = unsafe { libc::listxattr(path_c.as_ptr(), names.as_mut_ptr() as *mut libc::c_char, names.len()) };
        if got < 0 {
            let err = std::io::Error::last_os_error();
            if unsupported_metadata_errno(&err) {
                return Ok(Vec::new());
            }
            return Err(format!("capture FLAC xattrs for '{}': listxattr failed: {err}", path.display()));
        }
        names.truncate(got as usize);
        let mut out = Vec::new();
        for raw_name in names.split(|b| *b == 0).filter(|name| !name.is_empty()) {
            if is_posix_acl_xattr(raw_name) {
                continue;
            }
            let name_c = match std::ffi::CString::new(raw_name) {
                Ok(name) => name,
                Err(_) => continue,
            };
            let value_len = unsafe { libc::getxattr(path_c.as_ptr(), name_c.as_ptr(), std::ptr::null_mut(), 0) };
            if value_len < 0 {
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() == Some(libc::ENODATA) {
                    // The attribute disappeared between list and get, likely due
                    // to an external writer. Do not invent an attribute value.
                    continue;
                }
                if unsupported_metadata_errno(&err) {
                    return Err(format!("capture FLAC xattr '{}' on '{}': existing attribute cannot be read on this filesystem/namespace: {err}", String::from_utf8_lossy(raw_name), path.display()));
                }
                return Err(format!("capture FLAC xattr '{}' on '{}': {err}", String::from_utf8_lossy(raw_name), path.display()));
            }
            let mut value = vec![0u8; value_len as usize];
            let got_value = unsafe { libc::getxattr(path_c.as_ptr(), name_c.as_ptr(), value.as_mut_ptr() as *mut libc::c_void, value.len()) };
            if got_value < 0 {
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() == Some(libc::ENODATA) {
                    continue;
                }
                if unsupported_metadata_errno(&err) {
                    return Err(format!("capture FLAC xattr '{}' on '{}': existing attribute cannot be read on this filesystem/namespace: {err}", String::from_utf8_lossy(raw_name), path.display()));
                }
                return Err(format!("capture FLAC xattr '{}' on '{}': {err}", String::from_utf8_lossy(raw_name), path.display()));
            }
            value.truncate(got_value as usize);
            out.push((std::ffi::OsString::from_vec(raw_name.to_vec()), value));
        }
        Ok(out)
    }

    #[cfg(not(all(target_os = "linux", unix)))]
    fn linux_xattrs(_path: &Path) -> Result<Vec<(std::ffi::OsString, Vec<u8>)>, String> {
        Ok(Vec::new())
    }

    #[cfg(all(target_os = "linux", unix))]
    fn apply_linux_xattrs(path: &Path, xattrs: &[(std::ffi::OsString, Vec<u8>)]) -> Result<(), String> {
        use std::os::unix::ffi::OsStrExt;
        let path_c = c_path(path)?;
        for (name, value) in xattrs {
            #[cfg(test)]
            if let Some(result) = run_test_xattr_restore_hook(path, name) {
                result?;
            }
            let name_c = match std::ffi::CString::new(name.as_os_str().as_bytes()) {
                Ok(name) => name,
                Err(_) => continue,
            };
            if unsafe {
                libc::setxattr(
                    path_c.as_ptr(),
                    name_c.as_ptr(),
                    value.as_ptr() as *const libc::c_void,
                    value.len(),
                    0,
                )
            } != 0
            {
                let err = std::io::Error::last_os_error();
                return Err(format!("preserve FLAC xattr '{}' on rewrite temp '{}': {err}", name.to_string_lossy(), path.display()));
            }
            verify_linux_xattr(path, name, value)?;
        }
        Ok(())
    }

    #[cfg(not(all(target_os = "linux", unix)))]
    fn apply_linux_xattrs(_path: &Path, _xattrs: &[(std::ffi::OsString, Vec<u8>)]) -> Result<(), String> {
        Ok(())
    }

    #[cfg(all(target_os = "linux", unix))]
    fn verify_linux_xattr(path: &Path, name: &std::ffi::OsString, expected: &[u8]) -> Result<(), String> {
        use std::os::unix::ffi::OsStrExt;
        let path_c = c_path(path)?;
        let name_c = std::ffi::CString::new(name.as_os_str().as_bytes())
            .map_err(|_| format!("FLAC xattr name contains NUL byte: '{}'", name.to_string_lossy()))?;
        let len = unsafe { libc::getxattr(path_c.as_ptr(), name_c.as_ptr(), std::ptr::null_mut(), 0) };
        if len < 0 {
            return Err(format!(
                "verify preserved FLAC xattr '{}' on rewrite temp '{}': {}",
                name.to_string_lossy(),
                path.display(),
                std::io::Error::last_os_error()
            ));
        }
        let mut actual = vec![0u8; len as usize];
        let got = unsafe {
            libc::getxattr(
                path_c.as_ptr(),
                name_c.as_ptr(),
                actual.as_mut_ptr() as *mut libc::c_void,
                actual.len(),
            )
        };
        if got < 0 {
            return Err(format!(
                "verify preserved FLAC xattr '{}' on rewrite temp '{}': {}",
                name.to_string_lossy(),
                path.display(),
                std::io::Error::last_os_error()
            ));
        }
        actual.truncate(got as usize);
        if actual != expected {
            return Err(format!(
                "verify preserved FLAC xattr '{}' on rewrite temp '{}': value mismatch",
                name.to_string_lossy(),
                path.display()
            ));
        }
        Ok(())
    }

    #[cfg(unix)]
    fn unsupported_metadata_errno(err: &std::io::Error) -> bool {
        // ENOTSUP == EOPNOTSUPP on Linux; both are listed for platforms
        // where they differ, so compare with || instead of match arms.
        let Some(code) = err.raw_os_error() else { return false };
        code == libc::ENOTSUP || code == libc::EOPNOTSUPP || code == libc::ENOSYS
    }

    #[cfg(all(target_os = "linux", unix))]
    fn is_posix_acl_xattr(name: &[u8]) -> bool {
        name == b"system.posix_acl_access" || name == b"system.posix_acl_default"
    }

    #[cfg(all(target_os = "linux", unix))]
    fn linux_has_posix_acl_xattr(path: &Path) -> Result<Option<bool>, String> {
        let path_c = c_path(path)?;
        let len = unsafe { libc::listxattr(path_c.as_ptr(), std::ptr::null_mut(), 0) };
        if len < 0 {
            let err = std::io::Error::last_os_error();
            if unsupported_metadata_errno(&err) {
                return Ok(None);
            }
            return Err(format!("detect FLAC POSIX ACL xattrs for '{}': {err}", path.display()));
        }
        if len == 0 {
            return Ok(Some(false));
        }
        let mut names = vec![0u8; len as usize];
        let got = unsafe { libc::listxattr(path_c.as_ptr(), names.as_mut_ptr() as *mut libc::c_char, names.len()) };
        if got < 0 {
            let err = std::io::Error::last_os_error();
            if unsupported_metadata_errno(&err) {
                return Ok(None);
            }
            return Err(format!("detect FLAC POSIX ACL xattrs for '{}': {err}", path.display()));
        }
        names.truncate(got as usize);
        Ok(Some(names.split(|b| *b == 0).any(is_posix_acl_xattr)))
    }

    #[cfg(unix)]
    fn capture_acl_text(path: &Path) -> Result<AclSnapshot, String> {
        #[cfg(test)]
        if let Some(result) = run_test_acl_capture_hook(path) {
            return result;
        }
        if !command_exists_for_metadata("getfacl") {
            #[cfg(all(target_os = "linux", unix))]
            if linux_has_posix_acl_xattr(path)? == Some(true) {
                return Err(format!(
                    "cannot preserve existing FLAC POSIX ACLs for '{}': getfacl is unavailable",
                    path.display()
                ));
            }
            return Ok(AclSnapshot::ToolsUnavailable("getfacl unavailable".to_string()));
        }
        let output = std::process::Command::new("getfacl")
            .args(["--access", "--omit-header", "--"])
            .arg(path)
            .output()
            .map_err(|err| format!("capture FLAC ACLs for '{}': run getfacl: {err}", path.display()))?;
        if output.status.success() {
            return Ok(if acl_text_has_extended_entries(&output.stdout) {
                AclSnapshot::Captured(output.stdout)
            } else {
                AclSnapshot::NoExtended(output.stdout)
            });
        }
        if acl_output_reports_unsupported(&output) {
            return Ok(AclSnapshot::Unsupported(String::from_utf8_lossy(&output.stderr).trim().to_string()));
        }
        Err(format!(
            "capture FLAC ACLs for '{}': getfacl failed: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }

    #[cfg(unix)]
    fn apply_acl_text(path: &Path, acl_snapshot: &AclSnapshot) -> Result<(), String> {
        #[cfg(test)]
        if let Some(result) = run_test_acl_restore_hook(path, acl_snapshot) {
            return result;
        }
        let acl_text = match acl_snapshot {
            AclSnapshot::Captured(acl_text) => acl_text,
            AclSnapshot::Unsupported(reason) | AclSnapshot::ToolsUnavailable(reason) => {
                let _ = reason.as_str();
                return Ok(());
            }
            AclSnapshot::NoExtended(base_acl_text) => {
                let _ = base_acl_text.len();
                return Ok(());
            }
        };
        if !command_exists_for_metadata("setfacl") {
            return Err(format!(
                "cannot preserve existing FLAC ACLs on rewrite temp '{}': setfacl is unavailable",
                path.display()
            ));
        }
        let mut child = std::process::Command::new("setfacl")
            .args(["--set-file=-", "--"])
            .arg(path)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|err| format!("preserve FLAC ACLs on rewrite temp '{}': run setfacl: {err}", path.display()))?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(acl_text)
                .map_err(|err| format!("preserve FLAC ACLs on rewrite temp '{}': feed setfacl: {err}", path.display()))?;
        }
        let output = child
            .wait_with_output()
            .map_err(|err| format!("preserve FLAC ACLs on rewrite temp '{}': wait for setfacl: {err}", path.display()))?;
        if !output.status.success() {
            return Err(format!(
                "preserve FLAC ACLs on rewrite temp '{}': setfacl failed: {}",
                path.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(())
    }

    #[cfg(unix)]
    fn acl_text_has_extended_entries(text: &[u8]) -> bool {
        String::from_utf8_lossy(text).lines().any(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return false;
            }
            line.starts_with("default:")
                || (line.starts_with("user:") && !line.starts_with("user::"))
                || (line.starts_with("group:") && !line.starts_with("group::"))
                || line.starts_with("mask::")
        })
    }

    #[cfg(unix)]
    fn acl_output_reports_unsupported(output: &std::process::Output) -> bool {
        let stderr = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
        stderr.contains("operation not supported")
            || stderr.contains("not supported")
            || stderr.contains("no such attribute")
            || stderr.contains("acl not supported")
    }

    #[cfg(unix)]
    fn command_exists_for_metadata(name: &str) -> bool {
        std::process::Command::new(name)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .stdin(std::process::Stdio::null())
            .status()
            .map(|_| true)
            .unwrap_or(false)
    }

    #[cfg(unix)]
    fn preserve_timestamps(path: &Path, atime_sec: i64, atime_nsec: i64, mtime_sec: i64, mtime_nsec: i64) {
        let Ok(path_c) = c_path(path) else {
            return;
        };
        let times = [
            libc::timespec { tv_sec: atime_sec as libc::time_t, tv_nsec: atime_nsec as libc::c_long },
            libc::timespec { tv_sec: mtime_sec as libc::time_t, tv_nsec: mtime_nsec as libc::c_long },
        ];
        let _ = unsafe { libc::utimensat(libc::AT_FDCWD, path_c.as_ptr(), times.as_ptr(), 0) };
    }

    fn copy_stream_bounded(
        target_path: &Path,
        input: &mut std::fs::File,
        output: &mut std::fs::File,
        cancel: Option<&super::MetadataWriteCancelFlag>,
    ) -> std::io::Result<u64> {
        let _ = &target_path;
        let mut buf = vec![0u8; STREAM_COPY_BUF];
        let mut copied = 0u64;
        loop {
            if let Some(cancel) = cancel {
                if cancel.is_cancelled() {
                    cancel.record_observation();
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Interrupted,
                        "metadata save cancelled during FLAC overflow stream copy",
                    ));
                }
            }
            let n = input.read(&mut buf)?;
            if n == 0 {
                return Ok(copied);
            }
            output.write_all(&buf[..n])?;
            copied += n as u64;
            #[cfg(test)]
            run_test_stream_copy_chunk_hook(target_path, copied);
        }
    }

    pub(super) fn acquire_native_write_claim(path: &Path, operation: &str) -> Result<FlacWriteClaim, String> {
        acquire_common_write_claim(path, operation)
    }

    fn write_lock_path(path: &Path) -> PathBuf {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "unknown.flac".to_string());
        path.with_file_name(format!("{name}.tonepoet-write-lock"))
    }

    fn current_common_write_claim_token(path: &Path) -> Option<u64> {
        let canonical_path = canonical_journal_path(path);
        current_common_write_claim_token_for_canonical(&canonical_path)
    }

    fn current_common_write_claim_token_for_canonical(canonical_path: &Path) -> Option<u64> {
        THREAD_WRITE_LOCKS.with(|locks| {
            locks
                .borrow()
                .iter()
                .rev()
                .find(|held| held.canonical_path == canonical_path)
                .map(|held| held.claim_token)
        })
    }

    fn acquire_common_write_claim(path: &Path, operation: &str) -> Result<FlacWriteClaim, String> {
        let canonical_path = canonical_journal_path(path);
        let lock_path = write_lock_path(path);
        if let Some(claim_token) = current_common_write_claim_token_for_canonical(&canonical_path) {
            return Ok(FlacWriteClaim::reentrant(lock_path, canonical_path, claim_token));
        }
        let lock_set = COMMON_WRITE_LOCKS.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()));
        {
            let mut lock_set = lock_set
                .lock()
                .map_err(|_| format!("acquire FLAC common write lock for '{}': process-local lock table is poisoned", path.display()))?;
            if lock_set.contains(&canonical_path) {
                return Err(format!(
                    "cannot start native FLAC {operation} for '{}': another metadata/artwork mutation for the same FLAC is already in progress in this process",
                    path.display()
                ));
            }
            lock_set.insert(canonical_path.clone());
        }

        let result = acquire_common_write_claim_on_disk(path, &lock_path, operation, &canonical_path);
        match result {
            Ok(claim) => Ok(claim),
            Err(err) => {
                if let Ok(mut lock_set) = lock_set.lock() {
                    lock_set.remove(&canonical_path);
                }
                Err(err)
            }
        }
    }

    fn acquire_common_write_claim_on_disk(
        path: &Path,
        lock_path: &Path,
        operation: &str,
        canonical_path: &Path,
    ) -> Result<FlacWriteClaim, String> {
        let mut retried_after_stale_recovery = false;
        loop {
            let claim_token = new_claim_token();
            match create_common_write_lock_file(path, lock_path, operation, claim_token) {
                Ok(()) => {
                    THREAD_WRITE_LOCKS.with(|locks| locks.borrow_mut().push(ThreadWriteClaim {
                        canonical_path: canonical_path.to_path_buf(),
                        claim_token,
                    }));
                    return Ok(FlacWriteClaim::acquired(lock_path.to_path_buf(), canonical_path.to_path_buf(), claim_token));
                }
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                    if retried_after_stale_recovery {
                        return Err(format!(
                            "cannot start native FLAC {operation} for '{}': common write lock '{}' still exists after stale recovery",
                            path.display(),
                            lock_path.display()
                        ));
                    }
                    retried_after_stale_recovery = true;
                    match recover_common_write_lock(path)? {
                        MetadataJournalRecovery::NoJournal | MetadataJournalRecovery::RecoveredOrCleaned => continue,
                        MetadataJournalRecovery::ActiveOwner => {
                            return Err(format!(
                                "cannot start native FLAC {operation} for '{}': metadata/artwork write lock '{}' is owned by a live writer",
                                path.display(),
                                lock_path.display()
                            ));
                        }
                    }
                }
                Err(err) => {
                    return Err(format!(
                        "create native FLAC common write lock '{}' for {operation} on '{}': {err}",
                        lock_path.display(),
                        path.display()
                    ));
                }
            }
        }
    }

    fn create_common_write_lock_file(path: &Path, lock_path: &Path, operation: &str, claim_token: u64) -> std::io::Result<()> {
        let owner = OwnerProcessIdentity::current();
        let canonical = canonical_journal_path(path).to_string_lossy().as_bytes().to_vec();
        let mut body = Vec::new();
        body.extend_from_slice(WRITE_LOCK_MAGIC);
        push_owner_identity(&mut body, owner);
        push_le_u64(&mut body, claim_token);
        push_le_u64(&mut body, canonical.len() as u64);
        body.extend_from_slice(&canonical);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(lock_path)?;
        let cleanup = CleanupPath::new(lock_path.to_path_buf());
        file.write_all(&body)?;
        file.flush()?;
        file.sync_all()?;
        drop(file);
        match sync_parent_dir(path, &format!("FLAC common write lock acquisition for {operation}")) {
            Ok(_) => {
                cleanup.disarm();
                Ok(())
            }
            Err(err) => Err(std::io::Error::new(std::io::ErrorKind::Other, err)),
        }
    }

    fn parse_common_write_lock(path: &Path, lock_path: &Path) -> Result<(OwnerProcessIdentity, u64), String> {
        let mut data = Vec::new();
        std::fs::File::open(lock_path)
            .and_then(|mut file| file.read_to_end(&mut data))
            .map_err(|err| format!("read FLAC common write lock '{}': {err}", lock_path.display()))?;
        let is_v2 = data.len() >= WRITE_LOCK_MAGIC.len() && &data[..WRITE_LOCK_MAGIC.len()] == WRITE_LOCK_MAGIC;
        let is_v1 = data.len() >= WRITE_LOCK_MAGIC_V1.len() && &data[..WRITE_LOCK_MAGIC_V1.len()] == WRITE_LOCK_MAGIC_V1;
        if !is_v2 && !is_v1 {
            return Err(format!("invalid FLAC common write lock '{}': bad magic", lock_path.display()));
        }
        let mut pos = if is_v2 { WRITE_LOCK_MAGIC.len() } else { WRITE_LOCK_MAGIC_V1.len() };
        let owner = OwnerProcessIdentity {
            pid: read_le_u64(&data, &mut pos)?,
            start_ticks: read_le_u64(&data, &mut pos)?,
            boot_id_hash: read_le_u64(&data, &mut pos)?,
            process_token: read_le_u64(&data, &mut pos)?,
        };
        let claim_token = if is_v2 { read_le_u64(&data, &mut pos)? } else { 0 };
        let path_len = read_le_u64(&data, &mut pos)? as usize;
        let canonical = read_bytes(&data, &mut pos, path_len)?.to_vec();
        let current = canonical_journal_path(path).to_string_lossy().as_bytes().to_vec();
        if !canonical.is_empty() && canonical != current {
            return Err(format!(
                "invalid FLAC common write lock '{}': lock belongs to a different target",
                lock_path.display()
            ));
        }
        if pos != data.len() {
            return Err(format!("invalid FLAC common write lock '{}': trailing bytes", lock_path.display()));
        }
        Ok((owner, claim_token))
    }

    fn recover_common_write_lock(path: &Path) -> Result<MetadataJournalRecovery, String> {
        let lock_path = write_lock_path(path);
        if !lock_path.exists() {
            return Ok(MetadataJournalRecovery::NoJournal);
        }
        match parse_common_write_lock(path, &lock_path) {
            Ok((owner, _claim_token)) if owner.appears_active() => return Ok(MetadataJournalRecovery::ActiveOwner),
            Ok((_owner, _claim_token)) => {},
            Err(err) => {
                if common_write_lock_appears_recent(&lock_path) {
                    return Ok(MetadataJournalRecovery::ActiveOwner);
                }
                if !journal_path(path).exists() && !artwork_rollback_journal_path(path).exists() {
                    match std::fs::remove_file(&lock_path) {
                        Ok(()) => {
                            sync_parent_dir(path, "FLAC malformed common write lock removal")?;
                            return Ok(MetadataJournalRecovery::RecoveredOrCleaned);
                        }
                        Err(remove_err) if remove_err.kind() == std::io::ErrorKind::NotFound => {
                            return Ok(MetadataJournalRecovery::RecoveredOrCleaned);
                        }
                        Err(remove_err) => {
                            return Err(format!(
                                "remove malformed stale FLAC common write lock '{}' for '{}': {remove_err}",
                                lock_path.display(),
                                path.display()
                            ));
                        }
                    }
                }
                return Err(format!(
                    "cannot recover FLAC common write lock for '{}': {err}. Recovery journals exist, but the lock is not parseable enough to prove the owner is stale, so it will not be removed automatically.",
                    path.display()
                ));
            }
        }
        match recover_metadata_journal_for_startup(path)? {
            MetadataJournalRecovery::ActiveOwner => return Ok(MetadataJournalRecovery::ActiveOwner),
            MetadataJournalRecovery::NoJournal | MetadataJournalRecovery::RecoveredOrCleaned => {}
        }
        match recover_artwork_rollback_journal_for_claim(path)? {
            MetadataJournalRecovery::ActiveOwner => return Ok(MetadataJournalRecovery::ActiveOwner),
            MetadataJournalRecovery::NoJournal | MetadataJournalRecovery::RecoveredOrCleaned => {}
        }
        match std::fs::remove_file(&lock_path) {
            Ok(()) => {
                sync_parent_dir(path, "FLAC common write lock stale removal")?;
                Ok(MetadataJournalRecovery::RecoveredOrCleaned)
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(MetadataJournalRecovery::RecoveredOrCleaned),
            Err(err) => Err(format!("remove stale FLAC common write lock '{}': {err}", lock_path.display())),
        }
    }

    fn common_write_lock_appears_recent(lock_path: &Path) -> bool {
        let Ok(meta) = std::fs::metadata(lock_path) else {
            return false;
        };
        let Ok(modified) = meta.modified() else {
            return false;
        };
        modified
            .elapsed()
            .map(|age| age < std::time::Duration::from_secs(2))
            .unwrap_or(true)
    }

    fn recover_common_write_lock_for_read_path(path: &Path) -> Result<(), String> {
        match recover_common_write_lock(path)? {
            MetadataJournalRecovery::NoJournal | MetadataJournalRecovery::RecoveredOrCleaned => {}
            MetadataJournalRecovery::ActiveOwner => {
                return Err(format!(
                    "FLAC metadata/artwork write appears to be in progress for '{}'; common write lock is owned by a live writer and the file will not be parsed until the writer commits or becomes stale",
                    path.display()
                ));
            }
        }
        if let Some(target) = canonical_target_for_symlink_read(path) {
            match recover_common_write_lock(&target)? {
                MetadataJournalRecovery::NoJournal | MetadataJournalRecovery::RecoveredOrCleaned => {}
                MetadataJournalRecovery::ActiveOwner => {
                    return Err(format!(
                        "FLAC metadata/artwork write appears to be in progress for '{}'; target-local common write lock is owned by a live writer and the file will not be parsed until the writer commits or becomes stale",
                        target.display()
                    ));
                }
            }
        }
        Ok(())
    }

    fn journal_path(path: &Path) -> PathBuf {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "unknown.flac".to_string());
        path.with_file_name(format!("{name}.tonepoet-meta-journal"))
    }

    fn journal_tmp_path(path: &Path) -> PathBuf {
        let journal = journal_path(path);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        journal.with_extension(format!("journal.tmp.{}.{nanos}", std::process::id()))
    }

    fn claim_journal_no_replace<F>(
        path: &Path,
        tmp: &Path,
        journal: &Path,
        journal_label: &str,
        parent_sync_context: &str,
        mut recover_existing: F,
    ) -> Result<Option<String>, String>
    where
        F: FnMut(&Path) -> Result<MetadataJournalRecovery, String>,
    {
        let mut retried_after_stale_recovery = false;
        loop {
            if journal.exists() {
                if retried_after_stale_recovery {
                    return Err(format!(
                        "cannot acquire {journal_label} for '{}': recovery journal '{}' still exists after stale-journal recovery; another metadata operation may have claimed it",
                        path.display(),
                        journal.display()
                    ));
                }
                retried_after_stale_recovery = true;
                match recover_existing(path)? {
                    MetadataJournalRecovery::NoJournal | MetadataJournalRecovery::RecoveredOrCleaned => continue,
                    MetadataJournalRecovery::ActiveOwner => {
                        return Err(format!(
                            "cannot acquire {journal_label} for '{}': recovery journal '{}' is owned by a live writer",
                            path.display(),
                            journal.display()
                        ));
                    }
                }
            }
            match std::fs::rename(tmp, journal) {
                Ok(()) => {
                    let sync_status = match sync_parent_dir(path, parent_sync_context) {
                        Ok(status) => status,
                        Err(err) => {
                            let cleanup_note = match std::fs::remove_file(journal) {
                                Ok(()) => {
                                    let _ = sync_parent_dir(path, &format!("{parent_sync_context} rollback"));
                                    "removed the uncommitted claim".to_string()
                                }
                                Err(remove_err) => format!(
                                    "could not remove the uncommitted claim '{}': {remove_err}",
                                    journal.display()
                                ),
                            };
                            return Err(format!(
                                "commit {journal_label} '{}' failed before metadata mutation because parent-directory sync failed: {err}; {cleanup_note}",
                                journal.display()
                            ));
                        }
                    };
                    return Ok(parent_sync_durability_warning(sync_status, path, parent_sync_context));
                }
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(err) => {
                    return Err(format!(
                        "commit {journal_label} '{}' from temp '{}': {err}",
                        journal.display(),
                        tmp.display()
                    ));
                }
            }
        }
    }

    fn recover_artwork_rollback_journal_for_claim(path: &Path) -> Result<MetadataJournalRecovery, String> {
        recover_artwork_rollback_journal_for_claim_with_token(path).map(|(status, _claim_token)| status)
    }

    fn recover_artwork_rollback_journal_for_claim_with_token(path: &Path) -> Result<(MetadataJournalRecovery, Option<u64>), String> {
        let journal = artwork_rollback_journal_path(path);
        if !journal.exists() {
            return Ok((MetadataJournalRecovery::NoJournal, None));
        }
        let mut data = Vec::new();
        std::fs::File::open(&journal)
            .and_then(|mut file| file.read_to_end(&mut data))
            .map_err(|err| format!("read FLAC artwork rollback journal '{}': {err}", journal.display()))?;
        let record = parse_artwork_rollback_journal(&journal, &data)?;
        validate_artwork_rollback_target_path(path, &record)?;
        if record.owner.appears_active() {
            return Ok((MetadataJournalRecovery::ActiveOwner, (record.claim_token != 0).then_some(record.claim_token)));
        }
        recover_artwork_rollback_journal(path)?;
        Ok((MetadataJournalRecovery::RecoveredOrCleaned, None))
    }

    fn recover_artwork_rollback_journal_before_native_write(path: &Path, operation: &str) -> Result<(), String> {
        match recover_artwork_rollback_journal_for_claim_with_token(path)? {
            (MetadataJournalRecovery::NoJournal | MetadataJournalRecovery::RecoveredOrCleaned, _) => Ok(()),
            (MetadataJournalRecovery::ActiveOwner, Some(journal_claim_token))
                if current_common_write_claim_token(path) == Some(journal_claim_token) => Ok(()),
            (MetadataJournalRecovery::ActiveOwner, _) => Err(format!(
                "cannot start native FLAC {operation} for '{}': artwork rollback journal is owned by a live writer or by a different common write claim; another artwork mutation has not committed or rolled back yet",
                path.display()
            )),
        }
    }

    fn write_metadata_journal(
        path: &Path,
        metadata: &FlacMetadata,
        intended_metadata_region: Option<&[u8]>,
    ) -> Result<Option<String>, String> {
        write_metadata_journal_with_owner(
            path,
            metadata,
            intended_metadata_region,
            OwnerProcessIdentity::current(),
        )
    }

    fn write_metadata_journal_with_owner(
        path: &Path,
        metadata: &FlacMetadata,
        intended_metadata_region: Option<&[u8]>,
        owner: OwnerProcessIdentity,
    ) -> Result<Option<String>, String> {
        reject_symlink_native_write(path, "journal creation")?;
        let _write_claim = acquire_common_write_claim(path, "metadata journal creation")?;
        reject_hardlinked_native_write(path, "journal creation")?;
        let journal = journal_path(path);
        let tmp = journal_tmp_path(path);
        let cleanup = CleanupPath::new(tmp.clone());
        let file_meta = std::fs::metadata(path)
            .map_err(|err| format!("stat FLAC before metadata journal '{}': {err}", path.display()))?;
        let (dev, ino) = file_identity(&file_meta);
        let path_bytes = canonical_journal_path(path)
            .to_string_lossy()
            .as_bytes()
            .to_vec();
        let raw_metadata_region = &metadata.raw_metadata_region;
        let intended_len = intended_metadata_region
            .map(|region| region.len() as u64)
            .unwrap_or(0);
        let intended_checksum = intended_metadata_region
            .map(|region| checksum64(region))
            .unwrap_or(0);
        let mut body = Vec::new();
        let claim_token = current_common_write_claim_token(path).unwrap_or_else(new_claim_token);
        body.extend_from_slice(JOURNAL_MAGIC);
        push_le_u64(&mut body, file_meta.len());
        push_le_u64(&mut body, metadata.audio_start);
        push_le_u64(&mut body, raw_metadata_region.len() as u64);
        push_le_u64(&mut body, checksum64(raw_metadata_region));
        push_le_u64(&mut body, intended_len);
        push_le_u64(&mut body, intended_checksum);
        push_owner_identity(&mut body, owner);
        push_le_u64(&mut body, claim_token);
        push_le_u64(&mut body, dev);
        push_le_u64(&mut body, ino);
        push_le_u64(&mut body, path_bytes.len() as u64);
        body.extend_from_slice(&path_bytes);
        body.extend_from_slice(raw_metadata_region);

        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .map_err(|err| format!("create FLAC metadata journal '{}': {err}", tmp.display()))?;
        file.write_all(&body)
            .map_err(|err| format!("write FLAC metadata journal '{}': {err}", tmp.display()))?;
        file.flush()
            .map_err(|err| format!("flush FLAC metadata journal '{}': {err}", tmp.display()))?;
        file.sync_all()
            .map_err(|err| format!("sync FLAC metadata journal '{}': {err}", tmp.display()))?;
        drop(file);
        let durability_warning = claim_journal_no_replace(
            path,
            &tmp,
            &journal,
            "FLAC metadata journal",
            "FLAC metadata journal commit",
            recover_metadata_journal_for_startup,
        )?;
        drop(cleanup);
        Ok(durability_warning)
    }

    fn remove_metadata_journal(path: &Path) -> Result<(), String> {
        let journal = journal_path(path);
        match std::fs::remove_file(&journal) {
            Ok(()) => {
                sync_parent_dir(path, "FLAC metadata journal removal")?;
                Ok(())
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(format!(
                "remove FLAC metadata journal '{}': {err}",
                journal.display()
            )),
        }
    }

    fn remove_metadata_journal_after_committed_write(path: &Path) -> Result<Option<String>, String> {
        let journal = journal_path(path);
        #[cfg(test)]
        if let Some(result) = run_test_metadata_journal_remove_hook(&journal) {
            return match result {
                Ok(()) => Ok(post_commit_parent_sync_warning(path, "FLAC metadata journal removal")),
                Err(err) => Ok(Some(format!(
                    "FLAC metadata write for '{}' committed, but cleanup of recovery journal '{}' failed: {err}. The audio metadata is updated; remove the stale journal after verifying the file, or allow startup/read recovery to retry cleanup.",
                    path.display(),
                    journal.display()
                ))),
            };
        }
        match std::fs::remove_file(&journal) {
            Ok(()) => Ok(post_commit_parent_sync_warning(path, "FLAC metadata journal removal")),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Ok(Some(format!(
                "FLAC metadata write for '{}' committed, but cleanup of recovery journal '{}' failed: {err}. The audio metadata is updated; remove the stale journal after verifying the file, or allow startup/read recovery to retry cleanup.",
                path.display(),
                journal.display()
            ))),
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct OwnerProcessIdentity {
        pid: u64,
        start_ticks: u64,
        boot_id_hash: u64,
        process_token: u64,
    }

    fn push_owner_identity(out: &mut Vec<u8>, owner: OwnerProcessIdentity) {
        push_le_u64(out, owner.pid);
        push_le_u64(out, owner.start_ticks);
        push_le_u64(out, owner.boot_id_hash);
        push_le_u64(out, owner.process_token);
    }

    impl OwnerProcessIdentity {
        fn current() -> Self {
            let pid = std::process::id();
            Self {
                pid: pid as u64,
                start_ticks: process_start_ticks(pid).unwrap_or(0),
                boot_id_hash: boot_id_hash().unwrap_or(0),
                process_token: process_instance_token(),
            }
        }

        fn appears_active(self) -> bool {
            if self.pid == 0 {
                return false;
            }
            let Ok(pid) = u32::try_from(self.pid) else {
                return false;
            };
            if pid == std::process::id()
                && self.process_token != 0
                && self.process_token == process_instance_token()
            {
                return true;
            }
            if self.start_ticks == 0 || self.boot_id_hash == 0 {
                return false;
            }
            let Some(current_boot) = boot_id_hash() else {
                return false;
            };
            if current_boot != self.boot_id_hash {
                return false;
            }
            process_start_ticks(pid) == Some(self.start_ticks)
        }
    }

    fn new_claim_token() -> u64 {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let seq = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let pid = std::process::id() as u128;
        let raw = format!("{pid}:{now}:{seq}:{}", process_instance_token());
        checksum64(raw.as_bytes()).max(1)
    }

    fn process_instance_token() -> u64 {
        static TOKEN: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
        *TOKEN.get_or_init(|| {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0);
            let pid = std::process::id() as u128;
            let stack_marker = 0u8;
            let addr = (&stack_marker as *const u8 as usize) as u128;
            let raw = format!("{pid}:{now}:{addr}");
            checksum64(raw.as_bytes()).max(1)
        })
    }

    #[cfg(unix)]
    fn process_start_ticks(pid: u32) -> Option<u64> {
        let stat = std::fs::read_to_string(std::path::Path::new("/proc").join(pid.to_string()).join("stat")).ok()?;
        let (_, after_comm) = stat.rsplit_once(") ")?;
        let fields: Vec<&str> = after_comm.split_whitespace().collect();
        // /proc/<pid>/stat fields are 1-indexed. After the comm field, fields[0]
        // is state (field 3), so starttime (field 22) is index 19.
        fields.get(19)?.parse::<u64>().ok()
    }

    #[cfg(not(unix))]
    fn process_start_ticks(_pid: u32) -> Option<u64> {
        None
    }

    #[cfg(unix)]
    fn boot_id_hash() -> Option<u64> {
        std::fs::read(std::path::Path::new("/proc/sys/kernel/random/boot_id"))
            .ok()
            .map(|bytes| checksum64(&bytes))
    }

    #[cfg(not(unix))]
    fn boot_id_hash() -> Option<u64> {
        None
    }

    #[cfg(test)]
    fn stale_test_owner_identity() -> OwnerProcessIdentity {
        OwnerProcessIdentity {
            pid: u32::MAX as u64,
            start_ticks: 1,
            boot_id_hash: boot_id_hash().unwrap_or(1),
            process_token: 0,
        }
    }


    pub(super) fn begin_artwork_rollback_journal_with_intended(
        path: &Path,
        snapshot: &FlacMetadataSnapshot,
        intended_metadata_region: &[u8],
    ) -> Result<ArtworkRollbackJournalClaim, String> {
        write_artwork_rollback_journal(
            path,
            snapshot,
            Some(intended_metadata_region),
            OwnerProcessIdentity::current(),
        )
    }

    fn write_artwork_rollback_journal(
        path: &Path,
        snapshot: &FlacMetadataSnapshot,
        intended_metadata_region: Option<&[u8]>,
        owner: OwnerProcessIdentity,
    ) -> Result<ArtworkRollbackJournalClaim, String> {
        reject_symlink_native_write(path, "artwork rollback journal creation")?;
        let write_claim = acquire_common_write_claim(path, "artwork rollback journal creation")?;
        let claim_token = write_claim.claim_token();
        reject_hardlinked_native_write(path, "artwork rollback journal creation")?;
        let journal = artwork_rollback_journal_path(path);
        let tmp = artwork_rollback_journal_tmp_path(path);
        let cleanup = CleanupPath::new(tmp.clone());
        let file_meta = std::fs::metadata(path)
            .map_err(|err| format!("stat FLAC before artwork rollback journal '{}': {err}", path.display()))?;
        let (dev, ino) = file_identity(&file_meta);
        let (mtime_sec, mtime_nsec, ctime_sec, ctime_nsec) = file_change_times(&file_meta);
        let path_bytes = canonical_journal_path(path)
            .to_string_lossy()
            .as_bytes()
            .to_vec();
        let original_len = snapshot.raw_metadata_region.len() as u64;
        let original_checksum = checksum64(&snapshot.raw_metadata_region);
        let intended_metadata_len = intended_metadata_region
            .map(|region| region.len() as u64)
            .unwrap_or(0);
        let intended_metadata_checksum = intended_metadata_region
            .map(checksum64)
            .unwrap_or(0);
        let intended_file_len = intended_metadata_region
            .map(|region| {
                file_meta
                    .len()
                    .saturating_sub(snapshot.audio_start)
                    .saturating_add(4)
                    .saturating_add(region.len() as u64)
            })
            .unwrap_or(0);
        let mut body = Vec::new();
        body.extend_from_slice(ARTWORK_ROLLBACK_MAGIC);
        push_le_u64(&mut body, owner.pid);
        push_le_u64(&mut body, owner.start_ticks);
        push_le_u64(&mut body, owner.boot_id_hash);
        push_le_u64(&mut body, owner.process_token);
        push_le_u64(&mut body, claim_token);
        push_le_u64(&mut body, file_meta.len());
        push_le_u64(&mut body, snapshot.audio_start);
        push_le_u64(&mut body, original_len);
        push_le_u64(&mut body, original_checksum);
        push_le_u64(&mut body, intended_metadata_len);
        push_le_u64(&mut body, intended_metadata_checksum);
        push_le_u64(&mut body, intended_file_len);
        push_le_u64(&mut body, dev);
        push_le_u64(&mut body, ino);
        push_le_i64(&mut body, mtime_sec);
        push_le_i64(&mut body, mtime_nsec);
        push_le_i64(&mut body, ctime_sec);
        push_le_i64(&mut body, ctime_nsec);
        push_le_u64(&mut body, path_bytes.len() as u64);
        body.extend_from_slice(&path_bytes);
        body.extend_from_slice(&snapshot.raw_metadata_region);

        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .map_err(|err| format!("create FLAC artwork rollback journal '{}': {err}", tmp.display()))?;
        file.write_all(&body)
            .map_err(|err| format!("write FLAC artwork rollback journal '{}': {err}", tmp.display()))?;
        file.flush()
            .map_err(|err| format!("flush FLAC artwork rollback journal '{}': {err}", tmp.display()))?;
        file.sync_all()
            .map_err(|err| format!("sync FLAC artwork rollback journal '{}': {err}", tmp.display()))?;
        drop(file);
        let _durability_warning = claim_journal_no_replace(
            path,
            &tmp,
            &journal,
            "FLAC artwork rollback journal",
            "FLAC artwork rollback journal commit",
            recover_artwork_rollback_journal_for_claim,
        )?;
        drop(cleanup);
        Ok(ArtworkRollbackJournalClaim { path: journal, _write_claim: write_claim })
    }

    #[cfg(test)]
    pub(super) fn test_mark_artwork_rollback_journal_stale(path: &Path) -> Result<(), String> {
        let journal = artwork_rollback_journal_path(path);
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&journal)
            .map_err(|err| format!("open FLAC artwork rollback journal '{}': {err}", journal.display()))?;
        file.seek(SeekFrom::Start(ARTWORK_ROLLBACK_MAGIC.len() as u64))
            .map_err(|err| format!("seek FLAC artwork rollback journal '{}': {err}", journal.display()))?;
        file.write_all(&0u64.to_le_bytes())
            .map_err(|err| format!("mark FLAC artwork rollback journal stale '{}': {err}", journal.display()))?;
        file.sync_all()
            .map_err(|err| format!("sync stale FLAC artwork rollback journal '{}': {err}", journal.display()))?;
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn test_rewrite_artwork_rollback_owner_identity(
        path: &Path,
        pid: u64,
        start_ticks: u64,
        boot_id_hash: u64,
        process_token: u64,
    ) -> Result<(), String> {
        let journal = artwork_rollback_journal_path(path);
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&journal)
            .map_err(|err| format!("open FLAC artwork rollback journal '{}': {err}", journal.display()))?;
        file.seek(SeekFrom::Start(ARTWORK_ROLLBACK_MAGIC.len() as u64))
            .map_err(|err| format!("seek FLAC artwork rollback journal '{}': {err}", journal.display()))?;
        file.write_all(&pid.to_le_bytes())
            .and_then(|()| file.write_all(&start_ticks.to_le_bytes()))
            .and_then(|()| file.write_all(&boot_id_hash.to_le_bytes()))
            .and_then(|()| file.write_all(&process_token.to_le_bytes()))
            .map_err(|err| format!("rewrite FLAC artwork rollback journal owner '{}': {err}", journal.display()))?;
        file.sync_all()
            .map_err(|err| format!("sync FLAC artwork rollback journal owner '{}': {err}", journal.display()))?;
        Ok(())
    }

    pub(super) fn remove_artwork_rollback_journal(path: &Path) -> Result<(), String> {
        let journal = artwork_rollback_journal_path(path);
        match std::fs::remove_file(&journal) {
            Ok(()) => {
                sync_parent_dir(path, "FLAC artwork rollback journal removal")?;
                Ok(())
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(format!(
                "remove FLAC artwork rollback journal '{}': {err}",
                journal.display()
            )),
        }
    }

    pub(super) fn remove_artwork_rollback_journal_after_committed_batch(
        path: &Path,
    ) -> Result<Option<String>, String> {
        let journal = artwork_rollback_journal_path(path);
        match std::fs::remove_file(&journal) {
            Ok(()) => Ok(post_commit_parent_sync_warning(
                path,
                "FLAC artwork rollback journal removal",
            )),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(format!(
                "remove FLAC artwork rollback journal '{}': {err}",
                journal.display()
            )),
        }
    }

    pub(super) fn remove_artwork_rollback_journal_after_successful_restore(
        path: &Path,
    ) -> Result<Option<String>, String> {
        let journal = artwork_rollback_journal_path(path);
        match std::fs::remove_file(&journal) {
            Ok(()) => Ok(post_commit_parent_sync_warning(
                path,
                "FLAC artwork rollback journal removal after restore",
            )),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(format!(
                "remove FLAC artwork rollback journal '{}': {err}",
                journal.display()
            )),
        }
    }

    fn recover_artwork_rollback_journal(path: &Path) -> Result<(), String> {
        let journal = artwork_rollback_journal_path(path);
        if !journal.exists() {
            return Ok(());
        }
        let mut data = Vec::new();
        std::fs::File::open(&journal)
            .and_then(|mut file| file.read_to_end(&mut data))
            .map_err(|err| format!("read FLAC artwork rollback journal '{}': {err}", journal.display()))?;
        let record = parse_artwork_rollback_journal(&journal, &data)?;
        validate_artwork_rollback_target_path(path, &record)?;
        if record.owner.appears_active() {
            return Ok(());
        }

        let current_file_meta = std::fs::metadata(path)
            .map_err(|err| format!("stat FLAC before artwork rollback recovery '{}': {err}", path.display()))?;
        let current = match read_flac_metadata(path) {
            Ok(current) => current,
            Err(parse_err) => {
                let Some(audio_start) = record.recovery_audio_start_for_unparseable_current(&current_file_meta) else {
                    return Err(format!(
                        "refusing FLAC artwork rollback recovery for '{}': current metadata cannot be parsed and the file no longer matches a journaled recoverable identity: {parse_err}",
                        path.display()
                    ));
                };
                let snapshot = FlacMetadataSnapshot {
                    audio_start: record.audio_start,
                    raw_metadata_region: record.raw_metadata_region.clone(),
                };
                restore_metadata_snapshot_from_audio_start(path, &snapshot, audio_start)?;
                remove_artwork_rollback_journal(path)?;
                return Ok(());
            }
        };
        let current_len = current.raw_metadata_region.len() as u64;
        let current_checksum = checksum64(&current.raw_metadata_region);

        if current_len == record.metadata_len
            && current_checksum == record.metadata_checksum
            && current.raw_metadata_region == record.raw_metadata_region
        {
            // The rollback has already reached the original metadata state.
            // This includes the idempotent case where a previous recovery
            // committed an overflow-style restore by rename, then crashed
            // before removing the rollback journal; that can legitimately
            // change inode/ctime relative to the pre-artwork snapshot.
            remove_artwork_rollback_journal(path)?;
            return Ok(());
        }

        if !record.matches_intended_current(&current_file_meta, current_len, current_checksum) {
            return Err(format!(
                "refusing FLAC artwork rollback recovery for '{}': current file no longer matches either the journaled original metadata or the intended artwork mutation",
                path.display()
            ));
        }

        let snapshot = FlacMetadataSnapshot {
            audio_start: record.audio_start,
            raw_metadata_region: record.raw_metadata_region,
        };
        restore_metadata_snapshot(path, &snapshot)?;
        remove_artwork_rollback_journal(path)?;
        Ok(())
    }

    struct ParsedArtworkRollbackJournal {
        owner: OwnerProcessIdentity,
        claim_token: u64,
        file_len: u64,
        audio_start: u64,
        metadata_len: u64,
        metadata_checksum: u64,
        intended_metadata_len: u64,
        intended_metadata_checksum: u64,
        intended_file_len: u64,
        dev: u64,
        ino: u64,
        // Timestamps are part of the on-disk journal format and are parsed
        // for completeness/forward diagnostics; recovery decisions consult
        // dev/ino, lengths, and checksums instead (timestamps are too easily
        // perturbed by backup tools to gate recovery on).
        #[allow(dead_code)]
        mtime_sec: i64,
        #[allow(dead_code)]
        mtime_nsec: i64,
        #[allow(dead_code)]
        ctime_sec: i64,
        #[allow(dead_code)]
        ctime_nsec: i64,
        canonical_path: Vec<u8>,
        raw_metadata_region: Vec<u8>,
    }

    impl ParsedArtworkRollbackJournal {
        fn has_intended_identity(&self) -> bool {
            self.intended_metadata_len != 0
                || self.intended_metadata_checksum != 0
                || self.intended_file_len != 0
        }

        fn matches_intended_current(
            &self,
            current_file_meta: &std::fs::Metadata,
            current_metadata_len: u64,
            current_metadata_checksum: u64,
        ) -> bool {
            if !(self.has_intended_identity()
                && current_metadata_len == self.intended_metadata_len
                && current_metadata_checksum == self.intended_metadata_checksum
                && current_file_meta.len() == self.intended_file_len)
            {
                return false;
            }
            // In-place artwork mutations keep the original file length and
            // inode. If the intended state has the original length but the
            // filesystem identity changed, this is an external replacement
            // that happens to carry matching metadata, not the file we
            // snapshotted. Overflow rewrites can legitimately replace the
            // inode, so for those the intended metadata checksum plus final
            // file length are the commit identity.
            if self.intended_file_len == self.file_len && self.dev != 0 && self.ino != 0 {
                let (dev, ino) = file_identity(current_file_meta);
                return dev == self.dev && ino == self.ino;
            }
            true
        }

        fn recovery_audio_start_for_unparseable_current(
            &self,
            current_file_meta: &std::fs::Metadata,
        ) -> Option<u64> {
            // A crash during rollback recovery can leave the metadata chain
            // unparsable while the rollback journal is still present. We may
            // recover only when the current file still has a journaled identity.
            // For a torn in-place restore, the original inode/length remain and
            // the saved original audio offset is the correct source position.
            if self.matches_original_recovery_identity(current_file_meta) {
                return Some(self.audio_start);
            }
            // For an interrupted recovery from an overflow artwork mutation,
            // the source state may still be the intended post-artwork file. We
            // do not have to parse the current metadata to know where audio
            // should begin: the artwork rollback journal records the intended metadata length
            // used to build that committed artwork mutation.
            if self.matches_intended_recovery_identity(current_file_meta) {
                return Some(4 + self.intended_metadata_len);
            }
            None
        }

        fn matches_original_recovery_identity(&self, current_file_meta: &std::fs::Metadata) -> bool {
            if self.file_len != 0 && current_file_meta.len() != self.file_len {
                return false;
            }
            if self.dev != 0 && self.ino != 0 {
                let (dev, ino) = file_identity(current_file_meta);
                if dev != self.dev || ino != self.ino {
                    return false;
                }
            }
            true
        }

        fn matches_intended_recovery_identity(&self, current_file_meta: &std::fs::Metadata) -> bool {
            if !self.has_intended_identity()
                || self.intended_metadata_len == 0
                || self.intended_file_len == 0
                || current_file_meta.len() != self.intended_file_len
            {
                return false;
            }
            if self.intended_file_len == self.file_len && self.dev != 0 && self.ino != 0 {
                let (dev, ino) = file_identity(current_file_meta);
                return dev == self.dev && ino == self.ino;
            }
            true
        }
    }

    fn parse_artwork_rollback_journal(
        journal: &Path,
        data: &[u8],
    ) -> Result<ParsedArtworkRollbackJournal, String> {
        let is_v4 = data.len() >= ARTWORK_ROLLBACK_MAGIC.len()
            && &data[..ARTWORK_ROLLBACK_MAGIC.len()] == ARTWORK_ROLLBACK_MAGIC;
        let is_v3 = data.len() >= ARTWORK_ROLLBACK_MAGIC_V3.len()
            && &data[..ARTWORK_ROLLBACK_MAGIC_V3.len()] == ARTWORK_ROLLBACK_MAGIC_V3;
        let is_v2 = data.len() >= ARTWORK_ROLLBACK_MAGIC_V2.len()
            && &data[..ARTWORK_ROLLBACK_MAGIC_V2.len()] == ARTWORK_ROLLBACK_MAGIC_V2;
        let is_v1 = data.len() >= ARTWORK_ROLLBACK_MAGIC_V1.len()
            && &data[..ARTWORK_ROLLBACK_MAGIC_V1.len()] == ARTWORK_ROLLBACK_MAGIC_V1;
        if !is_v4 && !is_v3 && !is_v2 && !is_v1 {
            return Err(format!("invalid FLAC artwork rollback journal '{}': bad magic", journal.display()));
        }
        let mut pos = if is_v4 {
            ARTWORK_ROLLBACK_MAGIC.len()
        } else if is_v3 {
            ARTWORK_ROLLBACK_MAGIC_V3.len()
        } else if is_v2 {
            ARTWORK_ROLLBACK_MAGIC_V2.len()
        } else {
            ARTWORK_ROLLBACK_MAGIC_V1.len()
        };
        let owner_pid = read_le_u64(data, &mut pos)?;
        let (owner_start_ticks, owner_boot_id_hash, owner_process_token) = if is_v4 || is_v3 {
            (
                read_le_u64(data, &mut pos)?,
                read_le_u64(data, &mut pos)?,
                read_le_u64(data, &mut pos)?,
            )
        } else {
            (0, 0, 0)
        };
        let owner = OwnerProcessIdentity {
            pid: owner_pid,
            start_ticks: owner_start_ticks,
            boot_id_hash: owner_boot_id_hash,
            process_token: owner_process_token,
        };
        let claim_token = if is_v4 { read_le_u64(data, &mut pos)? } else { 0 };
        let (
            file_len,
            audio_start,
            metadata_len,
            metadata_checksum,
            intended_metadata_len,
            intended_metadata_checksum,
            intended_file_len,
            dev,
            ino,
            mtime_sec,
            mtime_nsec,
            ctime_sec,
            ctime_nsec,
        ) = if is_v4 || is_v3 || is_v2 {
            let file_len = read_le_u64(data, &mut pos)?;
            let audio_start = read_le_u64(data, &mut pos)?;
            let metadata_len = read_le_u64(data, &mut pos)?;
            let metadata_checksum = read_le_u64(data, &mut pos)?;
            let intended_metadata_len = read_le_u64(data, &mut pos)?;
            let intended_metadata_checksum = read_le_u64(data, &mut pos)?;
            let intended_file_len = read_le_u64(data, &mut pos)?;
            let dev = read_le_u64(data, &mut pos)?;
            let ino = read_le_u64(data, &mut pos)?;
            let mtime_sec = read_le_i64(data, &mut pos)?;
            let mtime_nsec = read_le_i64(data, &mut pos)?;
            let ctime_sec = read_le_i64(data, &mut pos)?;
            let ctime_nsec = read_le_i64(data, &mut pos)?;
            (
                file_len,
                audio_start,
                metadata_len,
                metadata_checksum,
                intended_metadata_len,
                intended_metadata_checksum,
                intended_file_len,
                dev,
                ino,
                mtime_sec,
                mtime_nsec,
                ctime_sec,
                ctime_nsec,
            )
        } else {
            let audio_start = read_le_u64(data, &mut pos)?;
            let metadata_len = read_le_u64(data, &mut pos)?;
            let metadata_checksum = read_le_u64(data, &mut pos)?;
            (
                0,
                audio_start,
                metadata_len,
                metadata_checksum,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
            )
        };
        let path_len = read_le_u64(data, &mut pos)? as usize;
        let canonical_path = read_bytes(data, &mut pos, path_len)?.to_vec();
        let raw_metadata_region = read_bytes(data, &mut pos, metadata_len as usize)?.to_vec();
        if pos != data.len() {
            return Err(format!("invalid FLAC artwork rollback journal '{}': trailing bytes", journal.display()));
        }
        if checksum64(&raw_metadata_region) != metadata_checksum {
            return Err(format!("invalid FLAC artwork rollback journal '{}': metadata checksum mismatch", journal.display()));
        }
        Ok(ParsedArtworkRollbackJournal {
            owner,
            claim_token,
            file_len,
            audio_start,
            metadata_len,
            metadata_checksum,
            intended_metadata_len,
            intended_metadata_checksum,
            intended_file_len,
            dev,
            ino,
            mtime_sec,
            mtime_nsec,
            ctime_sec,
            ctime_nsec,
            canonical_path,
            raw_metadata_region,
        })
    }

    fn validate_artwork_rollback_target_path(
        path: &Path,
        record: &ParsedArtworkRollbackJournal,
    ) -> Result<(), String> {
        let current_path = canonical_journal_path(path).to_string_lossy().as_bytes().to_vec();
        if !record.canonical_path.is_empty() && record.canonical_path != current_path {
            return Err(format!(
                "refusing FLAC artwork rollback recovery for '{}': journal belongs to a different path",
                path.display()
            ));
        }
        if record.audio_start != 4 + record.metadata_len {
            return Err(format!(
                "invalid FLAC artwork rollback journal for '{}': saved audio offset {} does not match metadata length {}",
                path.display(),
                record.audio_start,
                record.metadata_len
            ));
        }
        Ok(())
    }

    fn artwork_rollback_journal_path(path: &Path) -> PathBuf {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "unknown.flac".to_string());
        path.with_file_name(format!("{name}.tonepoet-artwork-rollback"))
    }

    fn artwork_rollback_journal_tmp_path(path: &Path) -> PathBuf {
        let journal = artwork_rollback_journal_path(path);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        journal.with_extension(format!("artwork-rollback.tmp.{}.{nanos}", std::process::id()))
    }

    fn recover_metadata_journal_for_read_path(path: &Path) -> Result<(), String> {
        recover_metadata_journal(path)?;
        if let Some(target) = canonical_target_for_symlink_read(path) {
            recover_metadata_journal(&target)?;
        }
        Ok(())
    }

    fn recover_artwork_rollback_journal_for_read_path(path: &Path) -> Result<(), String> {
        recover_artwork_rollback_journal(path)?;
        if let Some(target) = canonical_target_for_symlink_read(path) {
            recover_artwork_rollback_journal(&target)?;
        }
        Ok(())
    }

    fn canonical_target_for_symlink_read(path: &Path) -> Option<PathBuf> {
        let meta = std::fs::symlink_metadata(path).ok()?;
        if !meta.file_type().is_symlink() {
            return None;
        }
        let target = std::fs::canonicalize(path).ok()?;
        if target == path {
            None
        } else {
            Some(target)
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum MetadataJournalRecovery {
        NoJournal,
        RecoveredOrCleaned,
        ActiveOwner,
    }

    pub(super) fn recover_metadata_journal(path: &Path) -> Result<(), String> {
        match recover_metadata_journal_impl(path, false)? {
            MetadataJournalRecovery::NoJournal | MetadataJournalRecovery::RecoveredOrCleaned => Ok(()),
            MetadataJournalRecovery::ActiveOwner => Err(format!(
                "FLAC metadata write appears to be in progress for '{}'; recovery journal is owned by a live writer and will not be consumed before reading or competing writes",
                path.display()
            )),
        }
    }

    fn recover_owned_metadata_journal(path: &Path) -> Result<(), String> {
        match recover_metadata_journal_impl(path, true)? {
            MetadataJournalRecovery::NoJournal | MetadataJournalRecovery::RecoveredOrCleaned => Ok(()),
            MetadataJournalRecovery::ActiveOwner => Err(format!(
                "FLAC metadata journal for '{}' is owned by another live write claim and cannot be recovered by this writer",
                path.display()
            )),
        }
    }

    fn recover_metadata_journal_for_startup(path: &Path) -> Result<MetadataJournalRecovery, String> {
        recover_metadata_journal_impl(path, false)
    }

    fn recover_metadata_journal_impl(
        path: &Path,
        allow_active_owner_recovery: bool,
    ) -> Result<MetadataJournalRecovery, String> {
        let journal = journal_path(path);
        if !journal.exists() {
            return Ok(MetadataJournalRecovery::NoJournal);
        }
        let mut data = Vec::new();
        std::fs::File::open(&journal)
            .and_then(|mut file| file.read_to_end(&mut data))
            .map_err(|err| format!("read FLAC metadata journal '{}': {err}", journal.display()))?;
        let record = parse_metadata_journal(&journal, &data)?;
        validate_journal_target(path, &record)?;

        if let Some(owner) = record.owner {
            if owner.appears_active() {
                let token_matches_current_claim = record.claim_token != 0
                    && current_common_write_claim_token(path) == Some(record.claim_token);
                if !allow_active_owner_recovery || !token_matches_current_claim {
                    return Ok(MetadataJournalRecovery::ActiveOwner);
                }
            }
        }

        match read_flac_metadata(path) {
            Ok(current) => {
                let current_len = current.raw_metadata_region.len() as u64;
                let current_checksum = checksum64(&current.raw_metadata_region);
                if current_len == record.metadata_len && current_checksum == record.metadata_checksum {
                    remove_metadata_journal(path)?;
                    return Ok(MetadataJournalRecovery::RecoveredOrCleaned);
                }
                if let Some((intended_len, intended_checksum)) = record.intended_metadata_identity() {
                    if current_len == intended_len && current_checksum == intended_checksum {
                        remove_metadata_journal(path)?;
                        return Ok(MetadataJournalRecovery::RecoveredOrCleaned);
                    }
                }
                // A torn write can still leave a syntactically parseable FLAC
                // block chain with the wrong last-block flag or block length,
                // yielding a different computed audio_start. That is not a
                // reason to refuse recovery once the journal owner is stale or
                // explicitly recovering its own failed write: the journal was
                // written before the fixed-size metadata-region overwrite,
                // file identity/length still match, and
                // overwrite_metadata_region() verifies FLAC magic before
                // restoring the saved original region.
            }
            Err(_) => {
                // The exact failure mode this journal is for: an interrupted
                // in-place metadata-region overwrite can make the block chain
                // unparsable. We can still restore the saved region after the
                // target identity/size checks above and the FLAC magic check in
                // overwrite_metadata_region(), but only if the owner is stale or
                // the writer is explicitly recovering its own failed write.
            }
        }

        overwrite_metadata_region(path, &record.raw_metadata_region)?;
        remove_metadata_journal(path)?;
        Ok(MetadataJournalRecovery::RecoveredOrCleaned)
    }

    struct ParsedMetadataJournal {
        file_len: u64,
        audio_start: u64,
        metadata_len: u64,
        metadata_checksum: u64,
        intended_metadata_len: u64,
        intended_metadata_checksum: u64,
        owner: Option<OwnerProcessIdentity>,
        claim_token: u64,
        dev: u64,
        ino: u64,
        canonical_path: Vec<u8>,
        raw_metadata_region: Vec<u8>,
    }

    impl ParsedMetadataJournal {
        fn intended_metadata_identity(&self) -> Option<(u64, u64)> {
            (self.intended_metadata_len != 0 || self.intended_metadata_checksum != 0)
                .then_some((self.intended_metadata_len, self.intended_metadata_checksum))
        }
    }

    fn parse_metadata_journal(journal: &Path, data: &[u8]) -> Result<ParsedMetadataJournal, String> {
        let is_v5 = data.len() >= JOURNAL_MAGIC.len() && &data[..JOURNAL_MAGIC.len()] == JOURNAL_MAGIC;
        let is_v4 = data.len() >= JOURNAL_MAGIC_V4.len() && &data[..JOURNAL_MAGIC_V4.len()] == JOURNAL_MAGIC_V4;
        let is_v3 = data.len() >= JOURNAL_MAGIC_V3.len() && &data[..JOURNAL_MAGIC_V3.len()] == JOURNAL_MAGIC_V3;
        let is_v2 = data.len() >= JOURNAL_MAGIC_V2.len() && &data[..JOURNAL_MAGIC_V2.len()] == JOURNAL_MAGIC_V2;
        if !is_v5 && !is_v4 && !is_v3 && !is_v2 {
            return Err(format!("invalid FLAC metadata journal '{}': bad magic", journal.display()));
        }
        let mut pos = if is_v5 {
            JOURNAL_MAGIC.len()
        } else if is_v4 {
            JOURNAL_MAGIC_V4.len()
        } else if is_v3 {
            JOURNAL_MAGIC_V3.len()
        } else {
            JOURNAL_MAGIC_V2.len()
        };
        let file_len = read_le_u64(data, &mut pos)?;
        let audio_start = read_le_u64(data, &mut pos)?;
        let metadata_len = read_le_u64(data, &mut pos)?;
        let metadata_checksum = read_le_u64(data, &mut pos)?;
        let (intended_metadata_len, intended_metadata_checksum) = if is_v5 || is_v4 || is_v3 {
            (read_le_u64(data, &mut pos)?, read_le_u64(data, &mut pos)?)
        } else {
            (0, 0)
        };
        let owner = if is_v5 || is_v4 {
            Some(OwnerProcessIdentity {
                pid: read_le_u64(data, &mut pos)?,
                start_ticks: read_le_u64(data, &mut pos)?,
                boot_id_hash: read_le_u64(data, &mut pos)?,
                process_token: read_le_u64(data, &mut pos)?,
            })
        } else {
            None
        };
        let claim_token = if is_v5 { read_le_u64(data, &mut pos)? } else { 0 };
        let dev = read_le_u64(data, &mut pos)?;
        let ino = read_le_u64(data, &mut pos)?;
        let path_len = read_le_u64(data, &mut pos)? as usize;
        let canonical_path = read_bytes(data, &mut pos, path_len)?.to_vec();
        let raw_metadata_region = read_bytes(data, &mut pos, metadata_len as usize)?.to_vec();
        if pos != data.len() {
            return Err(format!("invalid FLAC metadata journal '{}': trailing bytes", journal.display()));
        }
        if checksum64(&raw_metadata_region) != metadata_checksum {
            return Err(format!("invalid FLAC metadata journal '{}': metadata checksum mismatch", journal.display()));
        }
        Ok(ParsedMetadataJournal {
            file_len,
            audio_start,
            metadata_len,
            metadata_checksum,
            intended_metadata_len,
            intended_metadata_checksum,
            owner,
            claim_token,
            dev,
            ino,
            canonical_path,
            raw_metadata_region,
        })
    }

    fn validate_journal_target(path: &Path, record: &ParsedMetadataJournal) -> Result<(), String> {
        let meta = std::fs::metadata(path)
            .map_err(|err| format!("stat FLAC before journal recovery '{}': {err}", path.display()))?;
        if meta.len() != record.file_len {
            return Err(format!(
                "refusing FLAC journal recovery for '{}': file length changed from {} to {}",
                path.display(),
                record.file_len,
                meta.len()
            ));
        }
        let (dev, ino) = file_identity(&meta);
        if record.dev != 0 && record.ino != 0 && (record.dev != dev || record.ino != ino) {
            return Err(format!(
                "refusing FLAC journal recovery for '{}': filesystem identity changed",
                path.display()
            ));
        }
        let current_path = canonical_journal_path(path).to_string_lossy().as_bytes().to_vec();
        if !record.canonical_path.is_empty() && record.canonical_path != current_path {
            return Err(format!(
                "refusing FLAC journal recovery for '{}': journal belongs to a different path",
                path.display()
            ));
        }
        if record.audio_start != 4 + record.metadata_len {
            return Err(format!(
                "invalid FLAC metadata journal for '{}': saved audio offset {} does not match metadata length {}",
                path.display(),
                record.audio_start,
                record.metadata_len
            ));
        }
        Ok(())
    }

    fn canonical_journal_path(path: &Path) -> PathBuf {
        std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
    }

    #[cfg(unix)]
    fn file_identity(meta: &std::fs::Metadata) -> (u64, u64) {
        use std::os::unix::fs::MetadataExt;
        (meta.dev(), meta.ino())
    }

    #[cfg(not(unix))]
    fn file_identity(_meta: &std::fs::Metadata) -> (u64, u64) {
        (0, 0)
    }

    #[cfg(unix)]
    fn file_change_times(meta: &std::fs::Metadata) -> (i64, i64, i64, i64) {
        use std::os::unix::fs::MetadataExt;
        (meta.mtime(), meta.mtime_nsec(), meta.ctime(), meta.ctime_nsec())
    }

    #[cfg(not(unix))]
    fn file_change_times(_meta: &std::fs::Metadata) -> (i64, i64, i64, i64) {
        (0, 0, 0, 0)
    }

    fn checksum64(data: &[u8]) -> u64 {
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for byte in data {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash
    }

    fn rewrite_tmp_path(path: &Path) -> PathBuf {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "unknown.flac".to_string());
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        path.with_file_name(format!(
            ".{name}.tonepoet-flac-rewrite-{}-{nanos}.tmp",
            std::process::id()
        ))
    }

    fn is_rewrite_tmp_file_name(name: &str) -> bool {
        name.starts_with('.')
            && name.contains(".tonepoet-flac-rewrite-")
            && name.ends_with(".tmp")
    }

    fn rewrite_tmp_owner_appears_active(name: &str) -> bool {
        let Some(pid) = rewrite_tmp_owner_pid(name) else {
            return false;
        };
        if pid == std::process::id() {
            return true;
        }
        rewrite_tmp_pid_exists(pid)
    }

    fn rewrite_tmp_owner_pid(name: &str) -> Option<u32> {
        let marker = ".tonepoet-flac-rewrite-";
        let after_marker = name.split_once(marker)?.1;
        let pid_text = after_marker.split_once('-')?.0;
        pid_text.parse::<u32>().ok()
    }

    #[cfg(unix)]
    fn rewrite_tmp_pid_exists(pid: u32) -> bool {
        std::path::Path::new("/proc").join(pid.to_string()).exists()
    }

    #[cfg(not(unix))]
    fn rewrite_tmp_pid_exists(_pid: u32) -> bool {
        false
    }

    struct CleanupPath {
        path: PathBuf,
        armed: std::sync::atomic::AtomicBool,
    }

    impl CleanupPath {
        fn new(path: PathBuf) -> Self {
            Self {
                path,
                armed: std::sync::atomic::AtomicBool::new(true),
            }
        }

        fn disarm(&self) {
            self.armed.store(false, std::sync::atomic::Ordering::Relaxed);
        }
    }

    impl Drop for CleanupPath {
        fn drop(&mut self) {
            if self.armed.load(std::sync::atomic::Ordering::Relaxed) {
                let _ = std::fs::remove_file(&self.path);
            }
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum ParentDirSyncStatus {
        Synced,
        Unsupported,
        NoParent,
    }

    fn sync_parent_dir(path: &Path, context: &str) -> Result<ParentDirSyncStatus, String> {
        let Some(parent) = path.parent() else {
            return Ok(ParentDirSyncStatus::NoParent);
        };
        sync_directory(parent, context)
    }

    fn parent_sync_durability_warning(
        status: ParentDirSyncStatus,
        path: &Path,
        context: &str,
    ) -> Option<String> {
        match status {
            ParentDirSyncStatus::Synced | ParentDirSyncStatus::NoParent => None,
            ParentDirSyncStatus::Unsupported => Some(format!(
                "durability warning after {context} for '{}': parent-directory fsync is unsupported on this filesystem/platform; file contents were synced, but directory-entry durability cannot be fully confirmed",
                path.display()
            )),
        }
    }

    fn post_commit_parent_sync_warning(path: &Path, context: &str) -> Option<String> {
        match sync_parent_dir(path, context) {
            Ok(status) => parent_sync_durability_warning(status, path, context),
            Err(err) => Some(format!(
                "durability warning after {context} for '{}': the file mutation already committed, but parent-directory fsync failed: {err}",
                path.display()
            )),
        }
    }


    #[cfg(unix)]
    fn sync_directory(parent: &Path, context: &str) -> Result<ParentDirSyncStatus, String> {
        #[cfg(test)]
        if let Some(result) = run_test_parent_dir_sync_hook(parent, context) {
            return result;
        }
        let dir = match std::fs::File::open(parent) {
            Ok(dir) => dir,
            Err(err) if unsupported_parent_dir_sync_error(&err) => {
                return Ok(ParentDirSyncStatus::Unsupported);
            }
            Err(err) => {
                return Err(format!(
                    "sync parent directory after {context} for '{}': open '{}': {err}",
                    parent.display(),
                    parent.display()
                ));
            }
        };
        match dir.sync_all() {
            Ok(()) => Ok(ParentDirSyncStatus::Synced),
            Err(err) if unsupported_parent_dir_sync_error(&err) => Ok(ParentDirSyncStatus::Unsupported),
            Err(err) => Err(format!(
                "sync parent directory after {context} for '{}': {err}",
                parent.display()
            )),
        }
    }

    #[cfg(not(unix))]
    fn sync_directory(_parent: &Path, _context: &str) -> Result<ParentDirSyncStatus, String> {
        Ok(ParentDirSyncStatus::Unsupported)
    }

    #[cfg(unix)]
    fn unsupported_parent_dir_sync_error(err: &std::io::Error) -> bool {
        // ENOTSUP == EOPNOTSUPP on Linux; both are listed for platforms
        // where they differ, so compare with || instead of match arms.
        let Some(code) = err.raw_os_error() else { return false };
        code == libc::EINVAL
            || code == libc::ENOTSUP
            || code == libc::EOPNOTSUPP
            || code == libc::ENOSYS
            || code == libc::EPERM
    }

    fn read_le_u32(data: &[u8], pos: &mut usize) -> Result<u32, String> {
        let bytes = read_bytes(data, pos, 4)?;
        let mut arr = [0u8; 4];
        arr.copy_from_slice(bytes);
        Ok(u32::from_le_bytes(arr))
    }

    fn read_le_u64(data: &[u8], pos: &mut usize) -> Result<u64, String> {
        let bytes = read_bytes(data, pos, 8)?;
        let mut arr = [0u8; 8];
        arr.copy_from_slice(bytes);
        Ok(u64::from_le_bytes(arr))
    }

    fn read_le_i64(data: &[u8], pos: &mut usize) -> Result<i64, String> {
        let bytes = read_bytes(data, pos, 8)?;
        let mut arr = [0u8; 8];
        arr.copy_from_slice(bytes);
        Ok(i64::from_le_bytes(arr))
    }

    fn read_bytes<'a>(data: &'a [u8], pos: &mut usize, len: usize) -> Result<&'a [u8], String> {
        let end = pos
            .checked_add(len)
            .ok_or_else(|| "Vorbis comment length overflow".to_string())?;
        if end > data.len() {
            return Err("truncated FLAC Vorbis comment block".to_string());
        }
        let slice = &data[*pos..end];
        *pos = end;
        Ok(slice)
    }

    fn push_le_u32(out: &mut Vec<u8>, value: u32) {
        out.extend_from_slice(&value.to_le_bytes());
    }

    fn push_le_u64(out: &mut Vec<u8>, value: u64) {
        out.extend_from_slice(&value.to_le_bytes());
    }

    fn push_le_i64(out: &mut Vec<u8>, value: i64) {
        out.extend_from_slice(&value.to_le_bytes());
    }

    fn push_be_u32(out: &mut Vec<u8>, value: u32) {
        out.extend_from_slice(&value.to_be_bytes());
    }

    fn push_be_len_prefixed(out: &mut Vec<u8>, bytes: &[u8], what: &str) -> Result<(), String> {
        let len = checked_u32_len(bytes.len(), what)?;
        push_be_u32(out, len);
        out.extend_from_slice(bytes);
        Ok(())
    }

    fn checked_u32_len(len: usize, what: &str) -> Result<u32, String> {
        u32::try_from(len).map_err(|_| format!("FLAC Vorbis {what} is too large: {len}"))
    }

    #[cfg(test)]
    fn run_test_backup_absence_hook(path: &Path) {
        let Some(slot) = TEST_BACKUP_ABSENCE_HOOK.get() else {
            return;
        };
        let hook = slot.lock().expect("backup absence hook poisoned").clone();
        if let Some(hook) = hook {
            hook(path);
        }
    }

    #[cfg(test)]
    fn run_test_metadata_write_len_hook(path: &Path, len: usize) {
        let Some(slot) = TEST_METADATA_WRITE_LEN_HOOK.get() else {
            return;
        };
        let hook = slot.lock().expect("metadata write length hook poisoned").clone();
        if let Some(hook) = hook {
            hook(path, len);
        }
    }

    #[cfg(test)]
    fn run_test_stream_rewrite_before_rename_hook(path: &Path, tmp_path: &Path) -> Result<(), String> {
        let Some(slot) = TEST_STREAM_REWRITE_BEFORE_RENAME_HOOK.get() else {
            return Ok(());
        };
        let hook = slot.lock().expect("stream rewrite hook poisoned").clone();
        if let Some(hook) = hook {
            hook(path, tmp_path)?;
        }
        Ok(())
    }

    #[cfg(test)]
    fn run_test_stream_copy_chunk_hook(path: &Path, copied: u64) {
        let Some(slot) = TEST_STREAM_COPY_CHUNK_HOOK.get() else {
            return;
        };
        let hook = slot.lock().expect("stream copy chunk hook poisoned").clone();
        if let Some(hook) = hook {
            hook(path, copied);
        }
    }

    #[cfg(test)]
    fn run_test_stream_rewrite_permit_hook(path: &Path) {
        let Some(slot) = TEST_STREAM_REWRITE_PERMIT_HOOK.get() else {
            return;
        };
        let hook = slot.lock().expect("stream rewrite permit hook poisoned").clone();
        if let Some(hook) = hook {
            hook(path);
        }
    }

    #[cfg(test)]
    fn run_test_parent_dir_sync_hook(
        parent: &Path,
        context: &str,
    ) -> Option<Result<ParentDirSyncStatus, String>> {
        let slot = TEST_PARENT_DIR_SYNC_HOOK.get()?;
        let hook = slot.lock().expect("parent dir sync hook poisoned").clone();
        hook.and_then(|hook| hook(parent, context).map(|result| result.map(|()| ParentDirSyncStatus::Synced)))
    }

    #[cfg(test)]
    fn run_test_metadata_journal_remove_hook(path: &Path) -> Option<Result<(), String>> {
        let slot = TEST_METADATA_JOURNAL_REMOVE_HOOK.get()?;
        let hook = slot.lock().expect("metadata journal remove hook poisoned").clone();
        hook.and_then(|hook| hook(path))
    }

    #[cfg(test)]
    fn run_test_metadata_snapshot_restore_hook(path: &Path) -> Option<Result<(), String>> {
        let slot = TEST_METADATA_SNAPSHOT_RESTORE_HOOK.get()?;
        let hook = slot.lock().expect("metadata snapshot restore hook poisoned").clone();
        hook.and_then(|hook| hook(path))
    }

    #[cfg(test)]
    fn run_test_xattr_capture_hook(path: &Path) -> Option<Result<(), String>> {
        let slot = TEST_XATTR_CAPTURE_HOOK.get()?;
        let hook = slot.lock().expect("xattr capture hook poisoned").clone();
        hook.and_then(|hook| hook(path))
    }

    #[cfg(test)]
    fn run_test_xattr_restore_hook(path: &Path, name: &std::ffi::OsString) -> Option<Result<(), String>> {
        let slot = TEST_XATTR_RESTORE_HOOK.get()?;
        let hook = slot.lock().expect("xattr restore hook poisoned").clone();
        hook.and_then(|hook| hook(path, name))
    }

    #[cfg(test)]
    fn run_test_acl_capture_hook(path: &Path) -> Option<Result<AclSnapshot, String>> {
        let slot = TEST_ACL_CAPTURE_HOOK.get()?;
        let hook = slot.lock().expect("ACL capture hook poisoned").clone();
        hook.and_then(|hook| hook(path))
    }

    #[cfg(test)]
    fn run_test_acl_restore_hook(path: &Path, snapshot: &AclSnapshot) -> Option<Result<(), String>> {
        let slot = TEST_ACL_RESTORE_HOOK.get()?;
        let hook = slot.lock().expect("ACL restore hook poisoned").clone();
        hook.and_then(|hook| hook(path, snapshot))
    }

    #[cfg(test)]
    struct TestHookGuard;

    #[cfg(test)]
    impl Drop for TestHookGuard {
        fn drop(&mut self) {
            if let Some(slot) = TEST_BACKUP_ABSENCE_HOOK.get() {
                *slot.lock().expect("backup absence hook poisoned") = None;
            }
            if let Some(slot) = TEST_METADATA_WRITE_LEN_HOOK.get() {
                *slot.lock().expect("metadata write length hook poisoned") = None;
            }
            if let Some(slot) = TEST_STREAM_REWRITE_BEFORE_RENAME_HOOK.get() {
                *slot.lock().expect("stream rewrite hook poisoned") = None;
            }
            if let Some(slot) = TEST_STREAM_COPY_CHUNK_HOOK.get() {
                *slot.lock().expect("stream copy chunk hook poisoned") = None;
            }
            if let Some(slot) = TEST_STREAM_REWRITE_PERMIT_HOOK.get() {
                *slot.lock().expect("stream rewrite permit hook poisoned") = None;
            }
            if let Some(slot) = TEST_PARENT_DIR_SYNC_HOOK.get() {
                *slot.lock().expect("parent dir sync hook poisoned") = None;
            }
            if let Some(slot) = TEST_METADATA_JOURNAL_REMOVE_HOOK.get() {
                *slot.lock().expect("metadata journal remove hook poisoned") = None;
            }
            if let Some(slot) = TEST_METADATA_SNAPSHOT_RESTORE_HOOK.get() {
                *slot.lock().expect("metadata snapshot restore hook poisoned") = None;
            }
            if let Some(slot) = TEST_XATTR_CAPTURE_HOOK.get() {
                *slot.lock().expect("xattr capture hook poisoned") = None;
            }
            if let Some(slot) = TEST_XATTR_RESTORE_HOOK.get() {
                *slot.lock().expect("xattr restore hook poisoned") = None;
            }
            if let Some(slot) = TEST_ACL_CAPTURE_HOOK.get() {
                *slot.lock().expect("ACL capture hook poisoned") = None;
            }
            if let Some(slot) = TEST_ACL_RESTORE_HOOK.get() {
                *slot.lock().expect("ACL restore hook poisoned") = None;
            }
        }
    }

    #[cfg(test)]
    pub(super) fn test_with_fast_path_hooks<F, R>(
        scope: &Path,
        backup_absence_hook: impl Fn(&Path) + Send + Sync + 'static,
        metadata_write_len_hook: impl Fn(&Path, usize) + Send + Sync + 'static,
        body: F,
    ) -> R
    where
        F: FnOnce() -> R,
    {
        let _serial = acquire_hook_test_serialization();
        let backup_scope = scope.to_path_buf();
        let len_scope = scope.to_path_buf();
        *TEST_BACKUP_ABSENCE_HOOK
            .get_or_init(|| std::sync::Mutex::new(None))
            .lock()
            .expect("backup absence hook poisoned") = Some(std::sync::Arc::new(move |path: &Path| {
                if path.starts_with(&backup_scope) {
                    backup_absence_hook(path);
                }
            }));
        *TEST_METADATA_WRITE_LEN_HOOK
            .get_or_init(|| std::sync::Mutex::new(None))
            .lock()
            .expect("metadata write length hook poisoned") = Some(std::sync::Arc::new(move |path: &Path, len: usize| {
                if path.starts_with(&len_scope) {
                    metadata_write_len_hook(path, len);
                }
            }));
        let _guard = TestHookGuard;
        body()
    }

    #[cfg(test)]
    pub(super) fn test_with_stream_rewrite_before_rename_hook<F, R>(
        scope: &Path,
        hook: impl Fn(&Path, &Path) -> Result<(), String> + Send + Sync + 'static,
        body: F,
    ) -> R
    where
        F: FnOnce() -> R,
    {
        let _serial = acquire_hook_test_serialization();
        let scope = scope.to_path_buf();
        *TEST_STREAM_REWRITE_BEFORE_RENAME_HOOK
            .get_or_init(|| std::sync::Mutex::new(None))
            .lock()
            .expect("stream rewrite hook poisoned") = Some(std::sync::Arc::new(move |path: &Path, tmp: &Path| {
                if !path.starts_with(&scope) {
                    return Ok(());
                }
                hook(path, tmp)
            }));
        let _guard = TestHookGuard;
        body()
    }

    #[cfg(test)]
    pub(super) fn test_with_stream_copy_chunk_hook<F, R>(
        scope: &Path,
        hook: impl Fn(u64) + Send + Sync + 'static,
        body: F,
    ) -> R
    where
        F: FnOnce() -> R,
    {
        let _serial = acquire_hook_test_serialization();
        let scope = scope.to_path_buf();
        *TEST_STREAM_COPY_CHUNK_HOOK
            .get_or_init(|| std::sync::Mutex::new(None))
            .lock()
            .expect("stream copy chunk hook poisoned") = Some(std::sync::Arc::new(move |path: &Path, copied: u64| {
                if path.starts_with(&scope) {
                    hook(copied);
                }
            }));
        let _guard = TestHookGuard;
        body()
    }

    #[cfg(test)]
    pub(super) fn test_with_stream_rewrite_permit_hook<F, R>(
        scope: &Path,
        hook: impl Fn(&Path) + Send + Sync + 'static,
        body: F,
    ) -> R
    where
        F: FnOnce() -> R,
    {
        let _serial = acquire_hook_test_serialization();
        let scope = scope.to_path_buf();
        *TEST_STREAM_REWRITE_PERMIT_HOOK
            .get_or_init(|| std::sync::Mutex::new(None))
            .lock()
            .expect("stream rewrite permit hook poisoned") = Some(std::sync::Arc::new(move |path: &Path| {
                if path.starts_with(&scope) {
                    hook(path);
                }
            }));
        let _guard = TestHookGuard;
        body()
    }

    #[cfg(test)]
    pub(super) fn test_with_parent_dir_sync_hook<F, R>(
        scope: &Path,
        hook: impl Fn(&Path, &str) -> Option<Result<(), String>> + Send + Sync + 'static,
        body: F,
    ) -> R
    where
        F: FnOnce() -> R,
    {
        let _serial = acquire_hook_test_serialization();
        let scope = scope.to_path_buf();
        *TEST_PARENT_DIR_SYNC_HOOK
            .get_or_init(|| std::sync::Mutex::new(None))
            .lock()
            .expect("parent dir sync hook poisoned") = Some(std::sync::Arc::new(move |parent: &Path, context: &str| {
                if !parent.starts_with(&scope) {
                    return None;
                }
                hook(parent, context)
            }));
        let _guard = TestHookGuard;
        body()
    }

    #[cfg(test)]
    pub(super) fn test_with_metadata_journal_remove_hook<F, R>(
        scope: &Path,
        hook: impl Fn(&Path) -> Option<Result<(), String>> + Send + Sync + 'static,
        body: F,
    ) -> R
    where
        F: FnOnce() -> R,
    {
        let _serial = acquire_hook_test_serialization();
        let scope = scope.to_path_buf();
        *TEST_METADATA_JOURNAL_REMOVE_HOOK
            .get_or_init(|| std::sync::Mutex::new(None))
            .lock()
            .expect("metadata journal remove hook poisoned") = Some(std::sync::Arc::new(move |path: &Path| {
                if !path.starts_with(&scope) {
                    return None;
                }
                hook(path)
            }));
        let _guard = TestHookGuard;
        body()
    }

    #[cfg(test)]
    pub(super) fn test_with_metadata_snapshot_restore_hook<F, R>(
        scope: &Path,
        hook: impl Fn(&Path) -> Option<Result<(), String>> + Send + Sync + 'static,
        body: F,
    ) -> R
    where
        F: FnOnce() -> R,
    {
        let _serial = acquire_hook_test_serialization();
        let scope = scope.to_path_buf();
        *TEST_METADATA_SNAPSHOT_RESTORE_HOOK
            .get_or_init(|| std::sync::Mutex::new(None))
            .lock()
            .expect("metadata snapshot restore hook poisoned") = Some(std::sync::Arc::new(move |path: &Path| {
                if !path.starts_with(&scope) {
                    return None;
                }
                hook(path)
            }));
        let _guard = TestHookGuard;
        body()
    }

    #[cfg(test)]
    pub(super) fn test_with_xattr_capture_hook<F, R>(
        scope: &Path,
        hook: impl Fn(&Path) -> Option<Result<(), String>> + Send + Sync + 'static,
        body: F,
    ) -> R
    where
        F: FnOnce() -> R,
    {
        let _serial = acquire_hook_test_serialization();
        let scope = scope.to_path_buf();
        *TEST_XATTR_CAPTURE_HOOK
            .get_or_init(|| std::sync::Mutex::new(None))
            .lock()
            .expect("xattr capture hook poisoned") = Some(std::sync::Arc::new(move |path: &Path| {
                if !path.starts_with(&scope) {
                    return None;
                }
                hook(path)
            }));
        let _guard = TestHookGuard;
        body()
    }

    #[cfg(test)]
    pub(super) fn test_with_xattr_restore_hook<F, R>(
        scope: &Path,
        hook: impl Fn(&Path, &std::ffi::OsString) -> Option<Result<(), String>> + Send + Sync + 'static,
        body: F,
    ) -> R
    where
        F: FnOnce() -> R,
    {
        let _serial = acquire_hook_test_serialization();
        let scope = scope.to_path_buf();
        *TEST_XATTR_RESTORE_HOOK
            .get_or_init(|| std::sync::Mutex::new(None))
            .lock()
            .expect("xattr restore hook poisoned") = Some(std::sync::Arc::new(move |path: &Path, name: &std::ffi::OsString| {
                if !path.starts_with(&scope) {
                    return None;
                }
                hook(path, name)
            }));
        let _guard = TestHookGuard;
        body()
    }

    #[cfg(test)]
    pub(super) fn test_with_acl_capture_hook<F, R>(
        scope: &Path,
        hook: impl Fn(&Path) -> Option<Result<AclSnapshot, String>> + Send + Sync + 'static,
        body: F,
    ) -> R
    where
        F: FnOnce() -> R,
    {
        let _serial = acquire_hook_test_serialization();
        let scope = scope.to_path_buf();
        *TEST_ACL_CAPTURE_HOOK
            .get_or_init(|| std::sync::Mutex::new(None))
            .lock()
            .expect("ACL capture hook poisoned") = Some(std::sync::Arc::new(move |path: &Path| {
                if !path.starts_with(&scope) {
                    return None;
                }
                hook(path)
            }));
        let _guard = TestHookGuard;
        body()
    }

    #[cfg(test)]
    pub(super) fn test_with_acl_restore_hook<F, R>(
        scope: &Path,
        hook: impl Fn(&Path, &AclSnapshot) -> Option<Result<(), String>> + Send + Sync + 'static,
        body: F,
    ) -> R
    where
        F: FnOnce() -> R,
    {
        let _serial = acquire_hook_test_serialization();
        let scope = scope.to_path_buf();
        *TEST_ACL_RESTORE_HOOK
            .get_or_init(|| std::sync::Mutex::new(None))
            .lock()
            .expect("ACL restore hook poisoned") = Some(std::sync::Arc::new(move |path: &Path, snapshot: &AclSnapshot| {
                if !path.starts_with(&scope) {
                    return None;
                }
                hook(path, snapshot)
            }));
        let _guard = TestHookGuard;
        body()
    }

    #[cfg(test)]
    pub(super) fn test_acl_snapshot_captured(text: Vec<u8>) -> AclSnapshot {
        AclSnapshot::Captured(text)
    }

    #[cfg(test)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(super) enum TestInPlaceKillPoint {
        AfterJournalCreate,
        DuringPartialMetadataOverwrite,
        AfterSyncedOverwriteBeforeJournalRemoval,
    }

    #[cfg(test)]
    pub(super) fn test_simulate_in_place_kill_point(
        path: &Path,
        changes: &[(lofty::tag::ItemKey, Option<String>)],
        point: TestInPlaceKillPoint,
    ) -> Result<(), String> {
        recover_metadata_journal(path)?;
        recover_artwork_rollback_journal(path)?;
        let metadata = read_flac_metadata(path)?;
        let replacement = build_vorbis_comment_replacement(&metadata, changes)?;
        let mut padded_replacement = replacement;
        let old_metadata_len = metadata.raw_metadata_region.len();
        let new_len_without_padding = encoded_blocks_len(&padded_replacement)?;
        if new_len_without_padding > old_metadata_len {
            return Err("test kill-point replacement does not fit existing metadata region".to_string());
        }
        let slack = old_metadata_len - new_len_without_padding;
        if slack > 0 && slack < BLOCK_HEADER_LEN {
            return Err("test kill-point replacement leaves unencodable FLAC slack".to_string());
        }
        if slack >= BLOCK_HEADER_LEN {
            padded_replacement.push(FlacBlock {
                block_type: BLOCK_PADDING,
                data: vec![0u8; slack - BLOCK_HEADER_LEN],
            });
        }
        let encoded = encode_blocks(&padded_replacement)?;
        write_metadata_journal_with_owner(path, &metadata, Some(&encoded), stale_test_owner_identity()).map(|_| ())?;

        match point {
            TestInPlaceKillPoint::AfterJournalCreate => Ok(()),
            TestInPlaceKillPoint::DuringPartialMetadataOverwrite => {
                let mut file = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(path)
                    .map_err(|err| format!("open FLAC kill-point fixture '{}': {err}", path.display()))?;
                file.seek(SeekFrom::Start(4))
                    .map_err(|err| format!("seek FLAC kill-point fixture '{}': {err}", path.display()))?;
                file.write_all(&[0x7f, 0xff, 0xff, 0xff])
                    .map_err(|err| format!("write FLAC partial kill-point fixture '{}': {err}", path.display()))?;
                file.sync_data()
                    .map_err(|err| format!("sync FLAC partial kill-point fixture '{}': {err}", path.display()))?;
                Ok(())
            }
            TestInPlaceKillPoint::AfterSyncedOverwriteBeforeJournalRemoval => {
                overwrite_metadata_region(path, &encoded)
            }
        }
    }

    #[cfg(test)]
    pub(super) fn test_simulate_parseable_wrong_audio_offset_with_journal(
        path: &Path,
        changes: &[(lofty::tag::ItemKey, Option<String>)],
    ) -> Result<(), String> {
        recover_metadata_journal(path)?;
        let metadata = read_flac_metadata(path)?;
        let mut replacement = build_vorbis_comment_replacement(&metadata, changes)?;
        let old_metadata_len = metadata.raw_metadata_region.len();
        let new_len_without_padding = encoded_blocks_len(&replacement)?;
        if new_len_without_padding > old_metadata_len {
            return Err("test wrong-offset replacement does not fit existing metadata region".to_string());
        }
        let slack = old_metadata_len - new_len_without_padding;
        if slack > 0 && slack < BLOCK_HEADER_LEN {
            return Err("test wrong-offset replacement leaves unencodable FLAC slack".to_string());
        }
        if slack >= BLOCK_HEADER_LEN {
            replacement.push(FlacBlock {
                block_type: BLOCK_PADDING,
                data: vec![0u8; slack - BLOCK_HEADER_LEN],
            });
        }
        let encoded = encode_blocks(&replacement)?;
        write_metadata_journal_with_owner(path, &metadata, Some(&encoded), stale_test_owner_identity()).map(|_| ())?;

        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|err| format!("open FLAC wrong-offset fixture '{}': {err}", path.display()))?;
        file.seek(SeekFrom::Start(4))
            .map_err(|err| format!("seek FLAC wrong-offset fixture '{}': {err}", path.display()))?;
        file.write_all(&[0x80 | BLOCK_STREAMINFO, 0x00, 0x00, 0x22])
            .map_err(|err| format!("write FLAC wrong-offset fixture '{}': {err}", path.display()))?;
        file.sync_data()
            .map_err(|err| format!("sync FLAC wrong-offset fixture '{}': {err}", path.display()))?;
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn test_create_stale_stream_rewrite_tmp(path: &Path) -> Result<PathBuf, String> {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "unknown.flac".to_string());
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let tmp_path = path.with_file_name(format!(
            ".{name}.tonepoet-flac-rewrite-0-{nanos}.tmp"
        ));
        let mut tmp = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)
            .map_err(|err| format!("create stale FLAC rewrite temp '{}': {err}", tmp_path.display()))?;
        tmp.write_all(FLAC_MAGIC)
            .map_err(|err| format!("write stale FLAC rewrite temp '{}': {err}", tmp_path.display()))?;
        tmp.write_all(b"partial stream rewrite")
            .map_err(|err| format!("write stale FLAC rewrite temp '{}': {err}", tmp_path.display()))?;
        tmp.sync_all()
            .map_err(|err| format!("sync stale FLAC rewrite temp '{}': {err}", tmp_path.display()))?;
        Ok(tmp_path)
    }

    #[cfg(test)]
    pub(super) fn test_force_stream_rewrite_commit(
        path: &Path,
        changes: &[(lofty::tag::ItemKey, Option<String>)],
    ) -> Result<(), String> {
        recover_metadata_journal(path)?;
        let metadata = read_flac_metadata(path)?;
        let mut replacement = build_vorbis_comment_replacement(&metadata, changes)?;
        append_padding(&mut replacement, REWRITE_PADDING_BYTES)?;
        stream_rewrite(path, metadata.audio_start, &replacement, None).map(|_| ())
    }

    #[cfg(test)]
    pub(super) fn test_block_payloads(path: &Path) -> Result<Vec<(u8, Vec<u8>)>, String> {
        let metadata = read_flac_metadata(path)?;
        Ok(metadata
            .blocks
            .into_iter()
            .map(|block| (block.block_type, block.data))
            .collect())
    }

    #[cfg(test)]
    pub(super) fn test_vorbis_field_values(path: &Path, key: &str) -> Result<Vec<String>, String> {
        let metadata = read_flac_metadata(path)?;
        let Some(block) = metadata
            .blocks
            .iter()
            .find(|block| block.block_type == BLOCK_VORBIS_COMMENT)
        else {
            return Ok(Vec::new());
        };
        let comments = parse_vorbis_comments(&block.data)?;
        Ok(comments
            .comments
            .into_iter()
            .filter_map(|comment| match comment {
                VorbisComment::Parsed { name, value } if name.eq_ignore_ascii_case(key) => Some(value),
                _ => None,
            })
            .collect())
    }

    #[cfg(test)]
    pub(super) fn test_journal_path(path: &Path) -> PathBuf {
        journal_path(path)
    }

    #[cfg(test)]
    pub(super) fn test_write_lock_path(path: &Path) -> PathBuf {
        write_lock_path(path)
    }

    #[cfg(test)]
    pub(super) fn test_write_stale_common_write_lock(path: &Path) -> Result<(), String> {
        let lock = write_lock_path(path);
        let _ = std::fs::remove_file(&lock);
        let owner = stale_test_owner_identity();
        let canonical = canonical_journal_path(path).to_string_lossy().as_bytes().to_vec();
        let mut body = Vec::new();
        body.extend_from_slice(WRITE_LOCK_MAGIC);
        push_owner_identity(&mut body, owner);
        push_le_u64(&mut body, new_claim_token());
        push_le_u64(&mut body, canonical.len() as u64);
        body.extend_from_slice(&canonical);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options
            .open(&lock)
            .map_err(|err| format!("create stale FLAC common write lock '{}': {err}", lock.display()))?;
        file.write_all(&body)
            .map_err(|err| format!("write stale FLAC common write lock '{}': {err}", lock.display()))?;
        file.sync_all()
            .map_err(|err| format!("sync stale FLAC common write lock '{}': {err}", lock.display()))?;
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn test_read_audio_start(path: &Path) -> Result<u64, String> {
        read_flac_metadata(path).map(|metadata| metadata.audio_start)
    }

    #[cfg(test)]
    pub(super) fn test_write_current_metadata_journal(path: &Path) -> Result<(), String> {
        let metadata = read_flac_metadata(path)?;
        write_metadata_journal_with_owner(
            path,
            &metadata,
            Some(&metadata.raw_metadata_region),
            stale_test_owner_identity(),
        )
        .map(|_| ())
    }

    #[cfg(test)]
    pub(super) fn test_write_active_metadata_journal(path: &Path) -> Result<(), String> {
        let metadata = read_flac_metadata(path)?;
        write_metadata_journal(path, &metadata, Some(&metadata.raw_metadata_region)).map(|_| ())
    }

    #[cfg(test)]
    pub(super) fn test_write_metadata_journal_with_pid_reuse_owner(path: &Path) -> Result<(), String> {
        let metadata = read_flac_metadata(path)?;
        let pid = std::process::id();
        let current_start = process_start_ticks(pid).unwrap_or(0);
        let mismatched_start = current_start.wrapping_add(1).max(1);
        let owner = OwnerProcessIdentity {
            pid: pid as u64,
            start_ticks: mismatched_start,
            boot_id_hash: boot_id_hash().unwrap_or(1),
            process_token: process_instance_token().wrapping_add(1).max(1),
        };
        write_metadata_journal_with_owner(path, &metadata, Some(&metadata.raw_metadata_region), owner)
            .map(|_| ())
    }

    #[cfg(test)]
    pub(super) fn test_picture_block_type_count(path: &Path, picture_type: lofty::picture::PictureType) -> Result<usize, String> {
        let metadata = read_flac_metadata(path)?;
        let target = picture_type.as_u8() as u32;
        Ok(metadata
            .blocks
            .iter()
            .filter(|block| block.block_type == BLOCK_PICTURE)
            .filter(|block| parse_picture_type_code(&block.data).ok() == Some(target))
            .count())
    }
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

/// Write a single metadata field to an audio file's tags.
///
/// FLAC writes use the same padding-aware metadata writer as the full editor
/// save path, so inline browse edits no longer create a full-file backup or
/// perform the slow Lofty full-file FLAC rewrite. Other formats deliberately
/// retain the conservative generic writer.
///
/// Year values must be valid u32; non-numeric input returns an error.
/// Empty strings clear the field (set to None).
pub fn write_metadata_field(path: &Path, field: MetadataField, value: &str) -> Result<(), String> {
    let change = metadata_field_change(field, value)?;
    write_all_tags(path, &[change])
}

/// Execute an inline metadata edit under one crash-recovery authority.
///
/// Native FLAC and DSF writes already own format-specific recovery journals and
/// therefore use the ordinary writer directly. Every other format is enclosed
/// by the database-backed full-file transaction; the inner writer is
/// deliberately backup-free so it cannot retire or replace the transaction's
/// rollback marker independently.
pub fn write_metadata_field_transactional(
    path: &Path,
    field: MetadataField,
    value: &str,
) -> Result<(), String> {
    write_metadata_field_transactional_with_control(path, field, value, None, None).map(|_| ())
}

/// Inline metadata write with the same operation-scoped cancellation,
/// byte-progress, and durability-warning report used by the full metadata
/// editor. DSF and native-FLAC paths observe cancellation inside their bounded
/// copy loops. Generic formats retain the database transaction and are checked
/// before mutation because their third-party writer has no cancellable seam.
pub fn write_metadata_field_transactional_with_control(
    path: &Path,
    field: MetadataField,
    value: &str,
    cancel: Option<&MetadataWriteCancelFlag>,
    byte_progress: Option<
        &(dyn Fn(&std::path::Path, crate::dsf_tags::DsfWriteProgress) + Send + Sync),
    >,
) -> Result<MetadataWriteCommitReport, String> {
    let change = metadata_field_change(field, value)
        .map_err(|error| format!("write failed before mutation: {error}"))?;
    reject_unsupported_dff_metadata_write(path, "writing")?;
    if uses_native_flac_metadata_journal(path) || crate::dsf_tags::is_dsf(path) {
        return write_all_tags_with_cancel_report_classified(
            path,
            &[change],
            cancel,
            byte_progress,
        )
        .map_err(MetadataWriteFailure::into_message);
    }

    check_metadata_write_cancel(cancel, "before starting inline metadata transaction")?;

    let db = crate::db::Database::open()
        .map_err(|error| format!("write failed before mutation: metadata journal unavailable: {error}"))?;
    write_metadata_field_with_database(&db, path, change)?;
    Ok(MetadataWriteCommitReport::clean())
}

fn write_metadata_field_with_database(
    db: &crate::db::Database,
    path: &Path,
    change: (lofty::tag::ItemKey, Option<String>),
) -> Result<(), String> {
    reject_unsupported_dff_metadata_write(path, "writing")?;
    if crate::dsf_tags::is_dsf(path) {
        return write_all_tags(path, std::slice::from_ref(&change));
    }
    db.atomic_metadata_write(path, || {
        write_all_tags_without_full_file_backup(path, std::slice::from_ref(&change))
    })
}

fn metadata_field_change(
    field: MetadataField,
    value: &str,
) -> Result<(lofty::tag::ItemKey, Option<String>), String> {
    let trimmed = value.trim();
    if matches!(field, MetadataField::Year) && !trimmed.is_empty() {
        trimmed
            .parse::<u32>()
            .map_err(|_| format!("year must be a number, got '{}'", trimmed))?;
    }
    Ok((
        metadata_field_item_key(field),
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        },
    ))
}

fn metadata_field_item_key(field: MetadataField) -> lofty::tag::ItemKey {
    match field {
        MetadataField::Title => lofty::tag::ItemKey::TrackTitle,
        MetadataField::Artist => lofty::tag::ItemKey::TrackArtist,
        MetadataField::Album => lofty::tag::ItemKey::AlbumTitle,
        MetadataField::Genre => lofty::tag::ItemKey::Genre,
        MetadataField::Year => lofty::tag::ItemKey::Year,
    }
}

// ── Full tag enumeration + batch write (metadata editor) ────────────

/// Positional dimension represented by a metadata row. File rows align with
/// `PresentationTab.paths`; track rows align with the CUE/medium track model.
/// The distinction is explicit because the two dimensions can have the same
/// length (for example, two tracks across two member images).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RowScope {
    #[default]
    File,
    Track,
}

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
    /// Compatibility summary: true when at least one source carrier stored
    /// more than one item for this logical key. Per-slot decisions must use
    /// `per_file_stored_value_counts`; this aggregate cannot identify which
    /// file would lose cardinality.
    pub has_multiple_stored_values: bool,
    /// The semantic dimension of the positional value vectors.
    pub row_scope: RowScope,
    /// Stored item/frame cardinality for each position in `per_file_values`.
    /// A value greater than one means replacing that scalar display value
    /// collapses multiple source items into one. Empty is allowed for
    /// synthetic/derived rows that have no source-carrier provenance.
    pub per_file_stored_value_counts: Vec<usize>,
    /// Current values in the row's declared positional dimension.
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

impl TagEntry {
    /// Resolve row scope with a compatibility fallback for older construction
    /// sites and persisted/test fixtures. New synthesized rows must set
    /// `row_scope` explicitly; a vector that cannot be file-aligned is still
    /// treated as track-scoped so legacy behavior remains fail-safe.
    pub fn effective_row_scope(&self, file_count: usize) -> RowScope {
        if self.row_scope == RowScope::Track || self.per_file_values.len() != file_count {
            RowScope::Track
        } else {
            RowScope::File
        }
    }

    pub fn is_track_scoped(&self, file_count: usize) -> bool {
        self.effective_row_scope(file_count) == RowScope::Track
    }

    /// Return the original stored-item cardinality for one positional slot.
    /// Legacy/synthetic rows may not carry a vector. The aggregate flag is a
    /// safe fallback only for a single-slot row; using it for a mixed row
    /// would warn against the wrong file.
    pub fn stored_value_count_for_slot(&self, slot: usize) -> usize {
        self.per_file_stored_value_counts
            .get(slot)
            .copied()
            .unwrap_or_else(|| {
                if self.per_file_values.len() == 1 && self.has_multiple_stored_values {
                    2
                } else if self.per_file_values.get(slot).is_some() {
                    1
                } else {
                    0
                }
            })
    }

    pub fn slot_has_multiple_stored_values(&self, slot: usize) -> bool {
        self.stored_value_count_for_slot(slot) > 1
    }

    /// Return positional carriers that would lose stored-item cardinality if
    /// the supplied scalar replacements were applied.
    ///
    /// A carrier is reported only when all three conditions hold:
    /// - the source stored more than one item/frame for the logical key;
    /// - the replacement differs from the current scalar projection, so the
    ///   mutation actually changes text; and
    /// - the replacement differs from the original scalar projection, so a
    ///   revert does not produce a false warning.
    ///
    /// All interactive scalar-edit paths must use this detector before
    /// replacing `per_file_values`; cardinality provenance is positional and
    /// cannot be inferred from the row-wide compatibility summary.
    pub fn stored_value_collapse_slots<'a, I>(&self, replacements: I) -> Vec<usize>
    where
        I: IntoIterator<Item = (usize, &'a str)>,
    {
        let mut slots = replacements
            .into_iter()
            .filter_map(|(slot, replacement)| {
                let current = self.per_file_values.get(slot)?;
                let original = self.per_file_originals.get(slot)?;
                (self.slot_has_multiple_stored_values(slot)
                    && current.as_str() != replacement
                    && original.as_str() != replacement)
                    .then_some(slot)
            })
            .collect::<Vec<_>>();
        slots.sort_unstable();
        slots.dedup();
        slots
    }

    /// Return source carriers whose current scalar projection already differs
    /// from the original multi-item representation. This is the snapshot-free
    /// form of `stored_value_collapse_slots`, used only when provider
    /// population happened while constructing an editor before the event-loop
    /// reducer could retain a pre-mutation snapshot.
    pub fn current_stored_value_collapse_slots(&self) -> Vec<usize> {
        let mut slots = self
            .per_file_values
            .iter()
            .zip(self.per_file_originals.iter())
            .enumerate()
            .filter_map(|(slot, (current, original))| {
                (self.slot_has_multiple_stored_values(slot) && current != original)
                    .then_some(slot)
            })
            .collect::<Vec<_>>();
        slots.sort_unstable();
        slots.dedup();
        slots
    }

    /// Discard source-carrier cardinality when a file-scoped row is repurposed
    /// as a synthetic or track-scoped row. Both representations must be reset
    /// together so the single-slot compatibility fallback cannot emit a stale
    /// warning.
    pub fn clear_stored_value_provenance(&mut self) {
        self.per_file_stored_value_counts.clear();
        self.has_multiple_stored_values = false;
    }
}

/// One logical metadata field whose scalar replacement would collapse one or
/// more source carriers that originally stored multiple values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetadataStoredValueCollapse {
    pub display_key: String,
    pub slots: Vec<usize>,
}

/// Structured result for multi-field metadata mutations that replace scalar
/// projections. Provider population and MB revert/restore controls use this
/// report while sharing the same positional detector used by typed edits,
/// paste, and capitalization.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MetadataMutationReport {
    pub changed_fields: usize,
    pub collapsed_fields: Vec<MetadataStoredValueCollapse>,
}

impl MetadataMutationReport {
    /// Compare complete entry snapshots before and after a mutation. Entries
    /// are matched by their case-insensitive display key because the editor
    /// maintains one row per logical key but may re-sort rows after provider
    /// population.
    pub fn between(before: &[TagEntry], after: &[TagEntry]) -> Self {
        let mut report = Self::default();

        for after_entry in after {
            let before_entry = before.iter().find(|entry| {
                entry
                    .display_key
                    .eq_ignore_ascii_case(&after_entry.display_key)
            });

            let changed = match before_entry {
                Some(entry) => entry.per_file_values != after_entry.per_file_values,
                None => after_entry.per_file_values.iter().any(|value| !value.is_empty()),
            };
            if !changed {
                continue;
            }
            report.changed_fields += 1;

            let Some(before_entry) = before_entry else {
                continue;
            };
            let mut slots = before_entry.stored_value_collapse_slots(
                after_entry
                    .per_file_values
                    .iter()
                    .enumerate()
                    .map(|(slot, replacement)| (slot, replacement.as_str())),
            );

            // A provider may intentionally repurpose a file-scoped row as a
            // track-scoped projection. In that case the post-mutation entry
            // clears source-carrier provenance; do not attribute the old file
            // cardinality to unrelated track positions.
            slots.retain(|&slot| after_entry.slot_has_multiple_stored_values(slot));
            if !slots.is_empty() {
                report.collapsed_fields.push(MetadataStoredValueCollapse {
                    display_key: after_entry.display_key.clone(),
                    slots,
                });
            }
        }

        report
    }

    pub fn for_entry(before: &TagEntry, after: &TagEntry) -> Self {
        Self::between(std::slice::from_ref(before), std::slice::from_ref(after))
    }

    /// Derive the current MusicBrainz population delta from durable editor
    /// provenance. This is used when a provider-populated editor was built
    /// before the completion reducer obtained a pre-mutation snapshot (for
    /// example, split-CUE editor construction).
    pub fn from_musicbrainz_entries(entries: &[TagEntry]) -> Self {
        let mut report = Self::default();

        for entry in entries {
            if entry.mb_proposed_value.is_none() && entry.mb_proposed_per_file.is_none() {
                continue;
            }
            if entry.per_file_values == entry.per_file_originals {
                continue;
            }

            report.changed_fields += 1;
            let slots = entry.current_stored_value_collapse_slots();
            if !slots.is_empty() {
                report.collapsed_fields.push(MetadataStoredValueCollapse {
                    display_key: entry.display_key.clone(),
                    slots,
                });
            }
        }

        report
    }

    pub fn merge(&mut self, other: Self) {
        self.changed_fields = self.changed_fields.saturating_add(other.changed_fields);
        self.collapsed_fields.extend(other.collapsed_fields);
    }

    pub fn collapsed_carrier_count(&self) -> usize {
        self.collapsed_fields
            .iter()
            .map(|field| field.slots.len())
            .sum()
    }

    pub fn append_collapse_warning(&self, status: &mut String) {
        let carriers = self.collapsed_carrier_count();
        if carriers == 0 {
            return;
        }
        status.push_str(&format!(
            "; warning: {} carrier{} across {} field{} collapsed multiple stored values \
             into one value",
            carriers,
            if carriers == 1 { "" } else { "s" },
            self.collapsed_fields.len(),
            if self.collapsed_fields.len() == 1 { "" } else { "s" },
        ));
    }

    pub fn append_provider_summary(&self, provider: &str, status: &mut String) {
        status.push_str(&format!(
            "; {} populated {} field{}",
            provider,
            self.changed_fields,
            if self.changed_fields == 1 { "" } else { "s" },
        ));
        self.append_collapse_warning(status);
    }
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
/// Grow `entry.per_file_values` and `per_file_originals` to `target_dim`,
/// padding with the existing first-element value. This deliberately never
/// shrinks: a guarded per-track populate path must not destroy existing
/// row-dimension values or their revert originals.
///
/// Used by both MB and gnudb populate paths to grow tag entries to
/// per-track dimension on single-image rips.
pub fn ensure_dim_replicate(entry: &mut TagEntry, target_dim: usize) {
    if entry.per_file_values.len() >= target_dim {
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
    // This helper converts a single-carrier file row into a track-scoped
    // proposal vector. Source-file cardinality no longer maps to those track
    // positions, so retaining or broadcasting it would warn against the wrong
    // detail slot.
    entry.clear_stored_value_provenance();
}

pub fn metadata_editor_has_changes(state: &super::app::MetadataEditorState) -> bool {
    if state.active_surface().refresh_failed
        || state.active_surface().pending_embedded_cuesheet_delete
    {
        return true;
    }

    let writable_indices: Vec<usize> = state.active_surface()
        .technical_details
        .files
        .iter()
        .enumerate()
        .filter_map(|(idx, file)| file.file_facts.write_eligibility.is_writable().then_some(idx))
        .collect();
    let has_file_access_model = state.active_surface().technical_details.files.len() == state.active_surface().paths.len();

    let deletion_is_dirty = !state.active_surface().deleted.is_empty()
        && (!has_file_access_model || !writable_indices.is_empty());

    deletion_is_dirty
        || state.active_surface().entries.iter().any(|e| {
            if has_file_access_model
                && !e.is_track_scoped(state.active_surface().paths.len())
                && e.per_file_values.len() == state.active_surface().paths.len()
            {
                return writable_indices.iter().any(|&idx| {
                    e.per_file_values.get(idx) != e.per_file_originals.get(idx)
                });
            }
            e.value != e.original || e.per_file_values != e.per_file_originals
        })
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
pub fn toggle_mb_revert(entry: &mut TagEntry) -> MetadataMutationReport {
    let before = entry.clone();
    let proposed = match &entry.mb_proposed_value {
        Some(p) => p.clone(),
        None => return MetadataMutationReport::default(),
    };
    let proposed_per_file = match &entry.mb_proposed_per_file {
        Some(p) => p.clone(),
        None => return MetadataMutationReport::default(),
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
        return MetadataMutationReport::default();
    }

    let n = entry.per_file_values.len();
    let all_same = entry.per_file_values.windows(2).all(|w| w[0] == w[1]);
    entry.is_mixed = !all_same && n > 1;
    if entry.is_mixed {
        entry.value = "<multiple values>".to_string();
    }
    MetadataMutationReport::for_entry(&before, entry)
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
pub fn toggle_mb_revert_field(entry: &mut TagEntry) -> MetadataMutationReport {
    let before = entry.clone();
    let Some(ref proposed) = entry.mb_proposed_value else {
        return MetadataMutationReport::default();
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
        return MetadataMutationReport::default();
    }

    recompute_aggregate_value(entry);
    MetadataMutationReport::for_entry(&before, entry)
}

/// Restore action for the detail overlay: discard any per-file user
/// edits and snap `per_file_values` back to the as-retrieved MB
/// proposal. Broadcasts `mb_proposed_value` when `mb_proposed_per_file`
/// is None. No-op when MB never touched the field.
pub fn restore_mb_proposed(entry: &mut TagEntry) -> MetadataMutationReport {
    let before = entry.clone();
    let Some(ref proposed) = entry.mb_proposed_value else {
        return MetadataMutationReport::default();
    };
    let proposed_per_file: Vec<String> = match &entry.mb_proposed_per_file {
        Some(v) => v.clone(),
        None => vec![proposed.clone(); entry.per_file_values.len()],
    };
    entry.per_file_values = proposed_per_file;
    recompute_aggregate_value(entry);
    MetadataMutationReport::for_entry(&before, entry)
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
/// Reads disc/track tags from already-merged editor entries, falling back to
/// directory/filename patterns. Entry-aware variant of `sort_paths_by_track`.
///
/// Sorts `paths` by (disc, track, filename) AND permutes each entry's
/// `per_file_values` + `per_file_originals` in lockstep so the per-file
/// vectors stay aligned with the new path order.
pub fn sort_paths_and_entries_by_track(
    paths: &mut Vec<std::path::PathBuf>,
    entries: &mut Vec<TagEntry>,
) {
    let perm = sort_permutation_for_paths_and_entries(paths, entries);
    apply_paths_entries_permutation(paths, entries, &perm);
}

/// Sort paths, editor entries, and cached source metadata together.
///
/// Use this when the caller has already read compact `SourceMetadata` from
/// the same tag pass as `entries`; otherwise artwork/ReplayGain caches would
/// remain in pre-sort order while the UI rows use the sorted path order.
pub fn sort_paths_entries_and_metadata_by_track(
    paths: &mut Vec<std::path::PathBuf>,
    entries: &mut Vec<TagEntry>,
    metadata: &mut Vec<SourceMetadata>,
) {
    let perm = sort_permutation_for_paths_and_entries(paths, entries);
    apply_paths_entries_permutation(paths, entries, &perm);
    apply_same_len_permutation(metadata, &perm);
}

/// Sort paths, editor entries, cached source metadata, and per-file read
/// errors together. Use this for metadata-editor open paths so partial
/// tag-read failures remain attached to the same file after track sorting.
pub fn sort_paths_entries_metadata_and_errors_by_track(
    paths: &mut Vec<std::path::PathBuf>,
    entries: &mut Vec<TagEntry>,
    metadata: &mut Vec<SourceMetadata>,
    metadata_errors: &mut Vec<Option<MetadataReadIssue>>,
) {
    let perm = sort_permutation_for_paths_and_entries(paths, entries);
    apply_paths_entries_permutation(paths, entries, &perm);
    apply_same_len_permutation(metadata, &perm);
    apply_same_len_permutation(metadata_errors, &perm);
}

fn sort_permutation_for_paths_and_entries(
    paths: &[std::path::PathBuf],
    entries: &[TagEntry],
) -> Vec<usize> {
    let n = paths.len();
    if n <= 1 {
        return (0..n).collect();
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
    perm
}

fn apply_paths_entries_permutation(
    paths: &mut Vec<std::path::PathBuf>,
    entries: &mut Vec<TagEntry>,
    perm: &[usize],
) {
    let n = paths.len();
    if perm.len() != n || n <= 1 {
        return;
    }

    let sorted_paths: Vec<_> = perm.iter().map(|&i| paths[i].clone()).collect();
    *paths = sorted_paths;

    for entry in entries.iter_mut() {
        if !entry.is_track_scoped(n)
            && entry.per_file_values.len() == n
            && entry.per_file_originals.len() == n
        {
            let sv: Vec<_> = perm
                .iter()
                .map(|&i| entry.per_file_values[i].clone())
                .collect();
            let so: Vec<_> = perm
                .iter()
                .map(|&i| entry.per_file_originals[i].clone())
                .collect();
            let sc = (entry.per_file_stored_value_counts.len() == n).then(|| {
                perm.iter()
                    .map(|&i| entry.per_file_stored_value_counts[i])
                    .collect::<Vec<_>>()
            });
            entry.per_file_values = sv;
            entry.per_file_originals = so;
            if let Some(sc) = sc {
                entry.per_file_stored_value_counts = sc;
            } else if entry.has_multiple_stored_values
                || !entry.per_file_stored_value_counts.is_empty()
            {
                // A non-aligned aggregate cannot identify a carrier after the
                // path permutation. Retire it rather than risk warning against
                // the wrong file. Production readers always provide an aligned
                // vector; this is a fail-safe for legacy/synthetic rows.
                entry.clear_stored_value_provenance();
            }
        }
        // Per-track entries (len != n, single-image rips with embedded
        // CUESHEET) are indexed by MB-track position, not file position,
        // so the path permutation doesn't apply.
    }
}

fn apply_same_len_permutation<T: Clone>(items: &mut Vec<T>, perm: &[usize]) {
    if items.len() != perm.len() || items.len() <= 1 {
        return;
    }
    let sorted: Vec<T> = perm.iter().map(|&i| items[i].clone()).collect();
    *items = sorted;
}

#[cfg(test)]
type SortAfterRecoverBeforeLoftyHook = dyn Fn(&std::path::Path) + Send + Sync + 'static;

#[cfg(test)]
static TEST_SORT_AFTER_RECOVER_BEFORE_LOFTY_HOOK: std::sync::OnceLock<
    std::sync::Mutex<Option<std::sync::Arc<SortAfterRecoverBeforeLoftyHook>>>,
> = std::sync::OnceLock::new();

#[cfg(test)]
fn run_test_sort_after_recover_before_lofty_hook(path: &std::path::Path) {
    let Some(slot) = TEST_SORT_AFTER_RECOVER_BEFORE_LOFTY_HOOK.get() else {
        return;
    };
    let hook = slot
        .lock()
        .expect("sort recovery hook poisoned")
        .clone();
    if let Some(hook) = hook {
        hook(path);
    }
}

#[cfg(test)]
struct SortAfterRecoverBeforeLoftyHookGuard;

#[cfg(test)]
impl Drop for SortAfterRecoverBeforeLoftyHookGuard {
    fn drop(&mut self) {
        if let Some(slot) = TEST_SORT_AFTER_RECOVER_BEFORE_LOFTY_HOOK.get() {
            *slot.lock().expect("sort recovery hook poisoned") = None;
        }
    }
}

#[cfg(test)]
fn with_sort_after_recover_before_lofty_hook<F, R>(
    scope: &std::path::Path,
    hook: impl Fn(&std::path::Path) + Send + Sync + 'static,
    body: F,
) -> R
where
    F: FnOnce() -> R,
{
    let _serial = flac_metadata_writer::acquire_hook_test_serialization();
    let scope = scope.to_path_buf();
    *TEST_SORT_AFTER_RECOVER_BEFORE_LOFTY_HOOK
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .expect("sort recovery hook poisoned") = Some(std::sync::Arc::new(move |path: &std::path::Path| {
            if path.starts_with(&scope) {
                hook(path);
            }
        }));
    let _guard = SortAfterRecoverBeforeLoftyHookGuard;
    body()
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
            // Native FLAC writes recover from .tonepoet-meta-journal before any
            // tag/probe reader is allowed to parse the metadata block chain. If
            // recovery itself refuses the target (for example, because the file
            // identity changed), do not hand a possibly torn metadata region to
            // Lofty; fall back to path-derived ordering for this entry.
            let (tag_disc, tag_track) = match flac_metadata_writer::recover_before_read(p) {
                Ok(()) => {
                    #[cfg(test)]
                    run_test_sort_after_recover_before_lofty_hook(p);
                    lofty::read_from_path(p)
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
                        .unwrap_or((None, None))
                }
                Err(_) => (None, None),
            };

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

/// Canonical display-key identity used by metadata-editor surfaces when they
/// need to merge format-specific aliases before applying the standard ordering.
/// The returned key is a logical editor key, not necessarily the raw tag name.
pub fn canonical_metadata_display_key(display_key: &str) -> String {
    // Alias LOOKUP uses the squashed form, but keys with no known alias
    // must keep their separators: returning the squashed fallback rewrote
    // e.g. REPLAYGAIN_ALBUM_GAIN to REPLAYGAINALBUMGAIN — and the AddKey
    // flow then wrote that separator-less tag name to disk.
    let normalized: String = display_key
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .map(|ch| ch.to_ascii_uppercase())
        .collect();
    match normalized.as_str() {
        "YEAR" => "DATE".to_string(),
        "ALBUMARTIST" | "ALBUMARTISTS" | "ALBUMARTISTCREDIT" => "ALBUMARTIST".to_string(),
        // foobar2000/flac convention is canonical; legacy spellings and
        // lofty's old DESCRIPTION comment alias merge into it on read.
        "TOTALTRACKS" => "TRACKTOTAL".to_string(),
        "TOTALDISCS" => "DISCTOTAL".to_string(),
        "DESCRIPTION" => "COMMENT".to_string(),
        "MUSICBRAINZALBUMID" => "MUSICBRAINZ_ALBUMID".to_string(),
        "MUSICBRAINZALBUMARTISTID" => "MUSICBRAINZ_ALBUMARTISTID".to_string(),
        "MUSICBRAINZRELEASEGROUPID" => "MUSICBRAINZ_RELEASEGROUPID".to_string(),
        "MUSICBRAINZTRACKID" | "MUSICBRAINZRECORDINGID" => "MUSICBRAINZ_TRACKID".to_string(),
        "MUSICBRAINZRELEASETRACKID" => "MUSICBRAINZ_RELEASETRACKID".to_string(),
        "MUSICBRAINZARTISTID" => "MUSICBRAINZ_ARTISTID".to_string(),
        _ => display_key.trim().to_ascii_uppercase(),
    }
}


#[derive(Debug, Clone)]
struct CanonicalEditorTagField {
    display_key: String,
    item_key: lofty::tag::ItemKey,
    value: String,
    is_binary: bool,
    stored_value_count: usize,
}

fn canonical_editor_item_key(
    canonical_display_key: &str,
    fallback: &lofty::tag::ItemKey,
) -> lofty::tag::ItemKey {
    match canonical_display_key {
        "TRACKTOTAL" => lofty::tag::ItemKey::Unknown("TRACKTOTAL".to_string()),
        "DISCTOTAL" => lofty::tag::ItemKey::Unknown("DISCTOTAL".to_string()),
        "COMMENT" => lofty::tag::ItemKey::Comment,
        _ => fallback.clone(),
    }
}

fn append_distinct_editor_value(current: &mut String, next: &str) {
    if next.is_empty() {
        return;
    }
    if current.is_empty() {
        current.push_str(next);
        return;
    }
    if current.split("; ").any(|value| value == next) {
        return;
    }
    current.push_str("; ");
    current.push_str(next);
}

/// Collect one logical row per canonical editor key. Vorbis/ID3 aliases are
/// collapsed before placeholder synthesis, so COMMENT/DESCRIPTION and total
/// spellings cannot produce competing rows or order-dependent writes.
///
/// The editor is intentionally scalar: when a key has multiple stored values,
/// it renders a joined summary. This is lossy only if that row is explicitly
/// edited. The save planner emits changes solely for rows whose value changed,
/// so unrelated edits leave the carrier's original multi-value frames/items
/// untouched and in their original cardinality.
fn canonical_editor_fields_from_tag(tag: &lofty::tag::Tag) -> Vec<CanonicalEditorTagField> {
    use lofty::tag::ItemValue;
    use std::collections::HashMap;

    let mut fields = Vec::new();
    let mut indexes = HashMap::<String, usize>::new();
    for item in tag.items() {
        let raw_display = item_key_display(item.key(), tag.tag_type());
        let display_key = canonical_metadata_display_key(&raw_display);
        let (value, is_binary) = match item.value() {
            ItemValue::Text(value) => (value.clone(), false),
            ItemValue::Locator(value) => (value.clone(), false),
            ItemValue::Binary(value) => (format!("<binary, {} bytes>", value.len()), true),
        };
        if let Some(index) = indexes.get(&display_key).copied() {
            let field: &mut CanonicalEditorTagField = &mut fields[index];
            // A second stored carrier is cardinality information even when
            // its text duplicates the first value. Keep display de-duplicated,
            // but retain the fact that an explicit scalar edit will collapse
            // more than one stored item.
            field.stored_value_count += 1;
            append_distinct_editor_value(&mut field.value, &value);
            field.is_binary |= is_binary;
            continue;
        }
        indexes.insert(display_key.clone(), fields.len());
        fields.push(CanonicalEditorTagField {
            item_key: canonical_editor_item_key(&display_key, item.key()),
            display_key,
            value,
            is_binary,
            stored_value_count: 1,
        });
    }
    fields
}

/// DSF uses the same scalar presentation policy as other carriers. Joined
/// values are display-only until the row itself is edited; an unrelated edit
/// emits no change for this key, so the ID3 backend preserves all original
/// frames rather than collapsing them into the joined display string.
fn canonical_editor_fields_from_dsf(
    snapshot: &crate::dsf_tags::DsfTagSnapshot,
) -> Vec<CanonicalEditorTagField> {
    snapshot
        .fields
        .iter()
        .map(|(display_key, values)| {
            let display_key = canonical_metadata_display_key(display_key);
            let stored_value_count = snapshot.stored_value_count(&display_key);
            CanonicalEditorTagField {
                item_key: canonical_editor_item_key(
                    &display_key,
                    &lofty::tag::ItemKey::Unknown(display_key.clone()),
                ),
                display_key,
                value: values.join("; "),
                is_binary: false,
                stored_value_count,
            }
        })
        .collect()
}

fn source_metadata_from_dsf(snapshot: &crate::dsf_tags::DsfTagSnapshot) -> SourceMetadata {
    SourceMetadata {
        title: snapshot.first("TITLE").map(ToOwned::to_owned),
        artist: snapshot.first("ARTIST").map(ToOwned::to_owned),
        album: snapshot.first("ALBUM").map(ToOwned::to_owned),
        genre: snapshot.first("GENRE").map(ToOwned::to_owned),
        year: snapshot.first("DATE").map(ToOwned::to_owned),
        tool: snapshot
            .first("ENCODER")
            .or_else(|| snapshot.first("ENCODINGTOOL"))
            .map(ToOwned::to_owned),
        track_number: snapshot.parsed_u32("TRACKNUMBER"),
        catalog_number: snapshot.first("CATALOGNUMBER").map(ToOwned::to_owned),
        rg_track_gain: snapshot.first("REPLAYGAIN_TRACK_GAIN").map(ToOwned::to_owned),
        rg_track_peak: snapshot.first("REPLAYGAIN_TRACK_PEAK").map(ToOwned::to_owned),
        rg_album_gain: snapshot.first("REPLAYGAIN_ALBUM_GAIN").map(ToOwned::to_owned),
        rg_album_peak: snapshot.first("REPLAYGAIN_ALBUM_PEAK").map(ToOwned::to_owned),
        r128_track_gain: snapshot.first("R128_TRACK_GAIN").and_then(r128_raw_to_db),
        r128_album_gain: snapshot.first("R128_ALBUM_GAIN").and_then(r128_raw_to_db),
        isrc: snapshot.first("ISRC").map(ToOwned::to_owned),
        ..SourceMetadata::default()
    }
}

fn tag_entries_from_dsf_snapshot(snapshot: &crate::dsf_tags::DsfTagSnapshot) -> Vec<TagEntry> {
    let mut entries = canonical_editor_fields_from_dsf(snapshot)
        .into_iter()
        .map(|field| TagEntry {
            row_scope: RowScope::File,
            display_key: field.display_key,
            item_key: field.item_key,
            value: field.value.clone(),
            original: field.value.clone(),
            is_binary: false,
            is_mixed: false,
            has_multiple_stored_values: field.stored_value_count > 1,
            per_file_stored_value_counts: vec![field.stored_value_count],
            per_file_values: vec![field.value.clone()],
            per_file_originals: vec![field.value],
            mb_proposed_value: None,
            mb_proposed_per_file: None,
        })
        .collect::<Vec<_>>();
    sort_entries_standard_first(&mut entries);
    entries
}

/// Priority order for standard fields (displayed first, in this order).
pub(super) const STANDARD_KEY_ORDER: &[&str] = &[
    "TITLE",
    "ARTIST",
    "ALBUM",
    "DATE",
    "GENRE",
    "COMPOSER",
    "PERFORMER",
    "ALBUMARTIST",
    "ORIGINALDATE",
    "TRACKNUMBER",
    "TRACKTOTAL",
    "DISCNUMBER",
    "DISCTOTAL",
    "COMMENT",
    "CATALOGNUMBER",
    "RELEASECOUNTRY",
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

/// Core fields that should always appear in the metadata editor,
/// even when empty. Matches foobar2000's default field set.
const CORE_EDITOR_FIELDS: &[&str] = &[
    "ARTIST",
    "TITLE",
    "ALBUM",
    "DATE",
    "GENRE",
    "COMPOSER",
    "PERFORMER",
    "ALBUMARTIST",
    "TRACKNUMBER",
    "TRACKTOTAL",
    "DISCNUMBER",
    "DISCTOTAL",
    "COMMENT",
];

/// Ensure the core editor fields are present in the entry list.
/// Missing fields are added as empty entries so the editor always
/// shows them and saved sidecars always contain them.
pub fn ensure_standard_fields_present(entries: &mut Vec<TagEntry>, n_files: usize) {
    for &field in CORE_EDITOR_FIELDS {
        let exists = entries
            .iter()
            .any(|entry| canonical_metadata_display_key(&entry.display_key) == field);
        if !exists {
            entries.push(TagEntry {
                row_scope: crate::tui::probe::RowScope::File,
                display_key: field.to_string(),
                item_key: lofty::tag::ItemKey::Unknown(field.to_string()),
                value: String::new(),
                original: String::new(),
                is_binary: false,
                is_mixed: false,
                has_multiple_stored_values: false,
                per_file_stored_value_counts: vec![0; n_files],
                per_file_values: vec![String::new(); n_files],
                per_file_originals: vec![String::new(); n_files],
                mb_proposed_value: None,
                mb_proposed_per_file: None,
            });
        }
    }
}

fn sort_entries_by_standard_order(entries: &mut Vec<TagEntry>) {
    entries.sort_by(|a, b| {
        let a_upper = canonical_metadata_display_key(&a.display_key);
        let b_upper = canonical_metadata_display_key(&b.display_key);
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

/// Sort `entries` so STANDARD_KEY_ORDER fields lead in their listed
/// order, with the remainder sorted alphabetically by display key.
/// Also ensures core editor fields are present (empty if not already
/// populated). Used by `read_all_tags_merged` and the main MusicBrainz /
/// GNUDB populate paths so post-populate entries fall into their
/// logical positions instead of trailing.
pub fn sort_entries_standard_first(entries: &mut Vec<TagEntry>) {
    let n_files = entries.first().map(|e| e.per_file_values.len()).unwrap_or(1);
    ensure_standard_fields_present(entries, n_files);
    sort_entries_by_standard_order(entries);
}

/// Sort existing entries without synthesizing missing core editor fields.
/// Supplemental MusicBrainz metadata uses this so enrichment cannot create
/// empty TITLE/ALBUM rows or inflate an album-level field into per-track shape.
pub fn sort_entries_standard_first_existing_only(entries: &mut Vec<TagEntry>) {
    sort_entries_by_standard_order(entries);
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

fn normalize_metadata_tool_key(key: &str) -> String {
    key.chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .map(|ch| ch.to_ascii_uppercase())
        .collect()
}

fn is_metadata_tool_key(key: &str) -> bool {
    matches!(
        normalize_metadata_tool_key(key).as_str(),
        "ENCODER"
            | "ENCODEDBY"
            | "ENCODINGTOOL"
            | "ENCODERSETTINGS"
            | "VENDOR"
            | "TOOL"
            | "SOFTWARE"
            | "WRITINGAPPLICATION"
            | "ITUNESENCODER"
    )
}

fn non_empty_tool_value(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn source_metadata_tool_from_tag(tag: &lofty::tag::Tag) -> Option<String> {
    use lofty::tag::{ItemKey, ItemValue};

    // Try common textual keys first. These cover Vorbis comments, MP4 freeform
    // tags, ID3 TXXX-style mappings, and similar format-specific fields without
    // depending on every Lofty `ItemKey` variant name.
    for key in [
        "ENCODER",
        "ENCODED_BY",
        "ENCODED BY",
        "ENCODING_TOOL",
        "ENCODERSETTINGS",
        "VENDOR",
        "TOOL",
        "SOFTWARE",
        "WRITING_APPLICATION",
        "ITUNESENCODER",
    ] {
        if let Some(value) = tag.get_string(&ItemKey::Unknown(key.to_string())).and_then(non_empty_tool_value) {
            return Some(value);
        }
    }

    // Then inspect all items using the format-specific display key. This keeps
    // the value in `SourceMetadata` even when the editable row later gets
    // renamed, hidden, or merged with another tag representation.
    for item in tag.items() {
        let display_key = item_key_display(item.key(), tag.tag_type());
        if !is_metadata_tool_key(&display_key) {
            continue;
        }
        match item.value() {
            ItemValue::Text(value) | ItemValue::Locator(value) => {
                if let Some(value) = non_empty_tool_value(value) {
                    return Some(value);
                }
            }
            ItemValue::Binary(_) => {}
        }
    }

    None
}

fn source_metadata_from_tags(
    path: &Path,
    tags: &[lofty::tag::Tag],
    include_external_preemphasis_checks: bool,
) -> SourceMetadata {
    use lofty::tag::{Accessor, ItemKey};

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
            meta.catalog_number = tag
                .get_string(&ItemKey::CatalogNumber)
                .map(|s| s.to_string())
                .or_else(|| {
                    tag.get_string(&ItemKey::Unknown("CATALOGNUMBER".to_string()))
                        .map(|s| s.to_string())
                });
        }
        if meta.tool.is_none() {
            meta.tool = source_metadata_tool_from_tag(tag);
        }

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

        if meta.r128_track_gain.is_none() {
            meta.r128_track_gain = tag.get_string(&r128_track_key).and_then(r128_raw_to_db);
        }
        if meta.r128_album_gain.is_none() {
            meta.r128_album_gain = tag.get_string(&r128_album_key).and_then(r128_raw_to_db);
        }

        for picture in tag.pictures() {
            let data = picture.data();
            let (width, height) = picture_dimensions(data);
            meta.artwork.push(ArtworkInfo {
                picture_type: picture.pic_type(),
                mime_type: picture
                    .mime_type()
                    .map(|mime| mime.to_string())
                    .unwrap_or_else(|| "application/octet-stream".to_string()),
                data_size: data.len(),
                width,
                height,
            });
        }
    }

    meta.artwork.sort_by(|a, b| {
        a.picture_type
            .as_u8()
            .cmp(&b.picture_type.as_u8())
            .then_with(|| a.mime_type.cmp(&b.mime_type))
            .then_with(|| a.data_size.cmp(&b.data_size))
            .then_with(|| a.width.cmp(&b.width))
            .then_with(|| a.height.cmp(&b.height))
    });

    // Optional pre-emphasis metadata check (tags + CUE/log + catalog).
    // The metadata-editor open path disables this because it would otherwise
    // perform additional immediate file/tag I/O after the already-open Lofty
    // pass. Dedicated browse/metadata reads keep the historical behavior.
    if include_external_preemphasis_checks {
        meta.preemphasis_metadata = preemphasis_metadata_check(path);
    }

    meta
}

fn read_all_tags_from_tagged_file(tagged: &lofty::file::TaggedFile) -> Vec<TagEntry> {
    use lofty::file::TaggedFileExt;

    let tag = match tagged.primary_tag().or_else(|| tagged.first_tag()) {
        Some(tag) => tag,
        None => return Vec::new(),
    };

    let mut entries = canonical_editor_fields_from_tag(tag)
        .into_iter()
        .map(|field| TagEntry {
            row_scope: crate::tui::probe::RowScope::File,
            display_key: field.display_key,
            item_key: field.item_key,
            value: field.value.clone(),
            original: field.value.clone(),
            is_binary: field.is_binary,
            is_mixed: false,
            has_multiple_stored_values: field.stored_value_count > 1,
            per_file_stored_value_counts: vec![field.stored_value_count],
            per_file_values: vec![field.value.clone()],
            per_file_originals: vec![field.value],
            mb_proposed_value: None,
            mb_proposed_per_file: None,
        })
        .collect::<Vec<_>>();

    for entry in &mut entries {
        if is_synthetic_preview(entry) {
            entry.is_binary = true;
        }
    }

    sort_entries_standard_first(&mut entries);
    entries
}

/// Read all tags from an audio file's primary tag.
/// Returns entries sorted: standard fields first, then alphabetical.
pub fn read_all_tags(path: &std::path::Path) -> Result<Vec<TagEntry>, String> {
    if crate::dsf_tags::is_dsf(path) {
        let outcome = crate::dsf_tags::read_with_warnings(path)?;
        for warning in &outcome.warnings {
            log::warn!("DSF metadata read warning for '{}': {}", path.display(), warning);
        }
        return Ok(tag_entries_from_dsf_snapshot(&outcome.snapshot));
    }
    flac_metadata_writer::recover_before_read(path)?;
    let tagged = lofty::read_from_path(path)
        .map_err(|e| format!("failed to read '{}': {}", path.display(), e))?;
    Ok(read_all_tags_from_tagged_file(&tagged))
}

/// Tags plus compact source metadata read from the same Lofty pass.
///
/// `metadata[i]` is aligned with the input `paths[i]` until the caller
/// applies any later path permutation. The TUI metadata editor uses this to
/// avoid immediately re-reading tags/artwork after `read_all_tags_merged`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataReadIssueKind {
    /// The file could not be opened or read from the filesystem.
    FilesystemRead,
    /// The filesystem denied access to the file.
    PermissionDenied,
    /// Lofty could not identify or support the container/tag format.
    UnsupportedFormat,
    /// Lofty recognized the file class but failed while decoding tag data.
    TagRead,
    /// Audio remains readable, but noncanonical container metadata means the
    /// editor must remain read-only until the file is repaired or rewritten.
    ContainerQuirk,
}

/// Typed per-file metadata read issue produced at the tag I/O boundary.
///
/// Classification happens after attempting the real Lofty read and inspecting
/// `LoftyError::kind()`. We deliberately do not reject files by extension: an
/// uncommon extension may still contain a supported container, while a familiar
/// extension may contain corrupt or unsupported data. UI/model code formats
/// this value; it does not infer unsupported state from error strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetadataReadIssue {
    pub kind: MetadataReadIssueKind,
    pub reason: String,
}


fn dsf_container_quirk_issue(
    path: &std::path::Path,
    warnings: &[String],
) -> Option<MetadataReadIssue> {
    if warnings.is_empty() {
        return None;
    }
    let reason = format!(
        "read noncanonical DSF metadata from '{}': {}; metadata writes are blocked until the container is repaired",
        path.display(),
        warnings.join("; ")
    );
    Some(MetadataReadIssue {
        kind: MetadataReadIssueKind::ContainerQuirk,
        reason,
    })
}
impl MetadataReadIssue {
    fn filesystem(path: &std::path::Path, reason: String) -> Self {
        Self {
            kind: MetadataReadIssueKind::FilesystemRead,
            reason: format!("failed to recover metadata journal before reading '{}': {}", path.display(), reason),
        }
    }

    fn from_lofty_read_error(path: &std::path::Path, err: lofty::error::LoftyError) -> Self {
        use lofty::error::ErrorKind;
        let kind = match err.kind() {
            ErrorKind::UnknownFormat | ErrorKind::UnsupportedTag => {
                MetadataReadIssueKind::UnsupportedFormat
            }
            ErrorKind::Io(io_err) if io_err.kind() == std::io::ErrorKind::PermissionDenied => {
                MetadataReadIssueKind::PermissionDenied
            }
            ErrorKind::Io(_) => MetadataReadIssueKind::FilesystemRead,
            _ => MetadataReadIssueKind::TagRead,
        };
        Self {
            kind,
            reason: format!("failed to read '{}': {}", path.display(), err),
        }
    }
}

#[derive(Debug, Clone)]
pub struct MergedTagsAndMetadata {
    pub entries: Vec<TagEntry>,
    pub metadata: Vec<SourceMetadata>,
    /// One entry per input path. A populated slot means that file failed tag
    /// reading, but other files may still have produced editable entries.
    pub metadata_errors: Vec<Option<MetadataReadIssue>>,
}

/// Read and merge tags from multiple audio files.
///
/// For each `ItemKey` present in any file, collects per-file values.
/// If all files agree → shared value. If they differ → `<mixed>`.
/// Duplicate keys within a single file are joined with "; ".
pub fn read_all_tags_merged(paths: &[std::path::PathBuf]) -> Result<Vec<TagEntry>, String> {
    use lofty::file::TaggedFileExt;
    use std::collections::HashMap;

    if paths.is_empty() {
        return Ok(Vec::new());
    }
    if paths.len() == 1 {
        return read_all_tags(&paths[0]);
    }

    struct KeyData {
        item_key: lofty::tag::ItemKey,
        is_binary: bool,
        stored_value_counts: Vec<usize>,
        values: Vec<String>,
    }

    let n = paths.len();
    let mut key_order = Vec::<String>::new();
    let mut key_map = HashMap::<String, KeyData>::new();

    for (file_idx, path) in paths.iter().enumerate() {
        if crate::dsf_tags::is_dsf(path) {
            let outcome = crate::dsf_tags::read_with_warnings(path)?;
            for warning in &outcome.warnings {
                log::warn!("DSF metadata read warning for '{}': {}", path.display(), warning);
            }
            for field in canonical_editor_fields_from_dsf(&outcome.snapshot) {
                let key = field.display_key.clone();
                if !key_map.contains_key(&key) {
                    key_order.push(key.clone());
                    key_map.insert(
                        key.clone(),
                        KeyData {
                            item_key: field.item_key.clone(),
                            is_binary: false,
                            stored_value_counts: vec![0; n],
                            values: vec![String::new(); n],
                        },
                    );
                }
                if let Some(data) = key_map.get_mut(&key) {
                    data.values[file_idx] = field.value;
                    data.stored_value_counts[file_idx] = field.stored_value_count;
                }
            }
            continue;
        }
        flac_metadata_writer::recover_before_read(path)?;
        let tagged = lofty::read_from_path(path)
            .map_err(|err| format!("failed to read '{}': {}", path.display(), err))?;
        let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) else {
            continue;
        };

        for field in canonical_editor_fields_from_tag(tag) {
            let key = field.display_key.clone();
            if !key_map.contains_key(&key) {
                key_order.push(key.clone());
                key_map.insert(
                    key.clone(),
                    KeyData {
                        item_key: field.item_key.clone(),
                        is_binary: field.is_binary,
                        stored_value_counts: vec![0; n],
                        values: vec![String::new(); n],
                    },
                );
            }
            if let Some(data) = key_map.get_mut(&key) {
                data.values[file_idx] = field.value;
                data.is_binary |= field.is_binary;
                data.stored_value_counts[file_idx] = field.stored_value_count;
            }
        }
    }

    let mut entries = Vec::new();
    for key in key_order {
        let data = key_map.remove(&key).ok_or_else(|| {
            format!("metadata merge lost canonical key {key} while preserving row order")
        })?;
        let all_same = data.values.windows(2).all(|values| values[0] == values[1]);
        let is_mixed = !all_same;
        let display_value = if is_mixed {
            "<multiple values>".to_string()
        } else {
            data.values.first().cloned().unwrap_or_default()
        };
        entries.push(TagEntry {
            row_scope: crate::tui::probe::RowScope::File,
            display_key: key,
            item_key: data.item_key,
            value: display_value.clone(),
            original: display_value,
            is_binary: data.is_binary,
            is_mixed,
            has_multiple_stored_values: data
                .stored_value_counts
                .iter()
                .any(|count| *count > 1),
            per_file_stored_value_counts: data.stored_value_counts,
            per_file_values: data.values.clone(),
            per_file_originals: data.values,
            mb_proposed_value: None,
            mb_proposed_per_file: None,
        });
    }

    for entry in &mut entries {
        if is_synthetic_preview(entry) {
            entry.is_binary = true;
        }
    }
    sort_entries_standard_first(&mut entries);
    Ok(entries)
}

/// Read merged editor entries and per-file `SourceMetadata` in one Lofty pass.
///
/// This is the preferred path for opening the metadata editor: entries,
/// ReplayGain fields, and compact artwork metadata all come from the same
/// tag read, so Details/Artwork can be cached without duplicate tag I/O.
pub fn read_all_tags_merged_with_metadata(
    paths: &[std::path::PathBuf],
) -> Result<MergedTagsAndMetadata, String> {
    use lofty::file::TaggedFileExt;
    use std::collections::HashMap;

    let n = paths.len();
    if paths.is_empty() {
        return Ok(MergedTagsAndMetadata {
            entries: Vec::new(),
            metadata: Vec::new(),
            metadata_errors: Vec::new(),
        });
    }

    if paths.len() == 1 {
        let path = &paths[0];
        if crate::dsf_tags::is_dsf(path) {
            return match crate::dsf_tags::read_with_warnings(path) {
                Ok(outcome) => {
                    for warning in &outcome.warnings {
                        log::warn!("DSF metadata read warning for '{}': {}", path.display(), warning);
                    }
                    let issue = dsf_container_quirk_issue(path, &outcome.warnings);
                    Ok(MergedTagsAndMetadata {
                        entries: tag_entries_from_dsf_snapshot(&outcome.snapshot),
                        metadata: vec![source_metadata_from_dsf(&outcome.snapshot)],
                        metadata_errors: vec![issue],
                    })
                }
                Err(reason) => Ok(MergedTagsAndMetadata {
                    entries: Vec::new(),
                    metadata: vec![SourceMetadata::default()],
                    metadata_errors: vec![Some(MetadataReadIssue {
                        kind: MetadataReadIssueKind::TagRead,
                        reason,
                    })],
                }),
            };
        }
        if let Err(err) = flac_metadata_writer::recover_before_read(path) {
            return Ok(MergedTagsAndMetadata {
                entries: Vec::new(),
                metadata: vec![SourceMetadata::default()],
                metadata_errors: vec![Some(MetadataReadIssue::filesystem(path, err))],
            });
        }
        let tagged = match lofty::read_from_path(path) {
            Ok(tagged) => tagged,
            Err(err) => {
                return Ok(MergedTagsAndMetadata {
                    entries: Vec::new(),
                    metadata: vec![SourceMetadata::default()],
                    metadata_errors: vec![Some(MetadataReadIssue::from_lofty_read_error(path, err))],
                });
            }
        };
        let entries = read_all_tags_from_tagged_file(&tagged);
        let metadata = source_metadata_from_tags(path, tagged.tags(), false);
        return Ok(MergedTagsAndMetadata {
            entries,
            metadata: vec![metadata],
            metadata_errors: vec![None],
        });
    }

    struct KeyData {
        item_key: lofty::tag::ItemKey,
        is_binary: bool,
        stored_value_counts: Vec<usize>,
        values: Vec<String>,
    }

    let mut metadata = vec![SourceMetadata::default(); n];
    let mut metadata_errors = vec![None; n];
    let mut key_order = Vec::<String>::new();
    let mut key_map = HashMap::<String, KeyData>::new();

    for (file_idx, path) in paths.iter().enumerate() {
        if crate::dsf_tags::is_dsf(path) {
            match crate::dsf_tags::read_with_warnings(path) {
                Ok(outcome) => {
                    for warning in &outcome.warnings {
                        log::warn!("DSF metadata read warning for '{}': {}", path.display(), warning);
                    }
                    metadata_errors[file_idx] =
                        dsf_container_quirk_issue(path, &outcome.warnings);
                    metadata[file_idx] = source_metadata_from_dsf(&outcome.snapshot);
                    for field in canonical_editor_fields_from_dsf(&outcome.snapshot) {
                        let key = field.display_key.clone();
                        if !key_map.contains_key(&key) {
                            key_order.push(key.clone());
                            key_map.insert(
                                key.clone(),
                                KeyData {
                                    item_key: field.item_key.clone(),
                                    is_binary: false,
                                    stored_value_counts: vec![0; n],
                                    values: vec![String::new(); n],
                                },
                            );
                        }
                        if let Some(data) = key_map.get_mut(&key) {
                            data.values[file_idx] = field.value;
                            data.stored_value_counts[file_idx] = field.stored_value_count;
                        }
                    }
                }
                Err(reason) => {
                    metadata_errors[file_idx] = Some(MetadataReadIssue {
                        kind: MetadataReadIssueKind::TagRead,
                        reason,
                    });
                }
            }
            continue;
        }
        if let Err(err) = flac_metadata_writer::recover_before_read(path) {
            metadata_errors[file_idx] = Some(MetadataReadIssue::filesystem(path, err));
            continue;
        }
        let tagged = match lofty::read_from_path(path) {
            Ok(tagged) => tagged,
            Err(err) => {
                metadata_errors[file_idx] = Some(MetadataReadIssue::from_lofty_read_error(path, err));
                continue;
            }
        };
        metadata[file_idx] = source_metadata_from_tags(path, tagged.tags(), false);
        let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) else {
            continue;
        };
        for field in canonical_editor_fields_from_tag(tag) {
            let key = field.display_key.clone();
            if !key_map.contains_key(&key) {
                key_order.push(key.clone());
                key_map.insert(
                    key.clone(),
                    KeyData {
                        item_key: field.item_key.clone(),
                        is_binary: field.is_binary,
                        stored_value_counts: vec![0; n],
                        values: vec![String::new(); n],
                    },
                );
            }
            if let Some(data) = key_map.get_mut(&key) {
                data.values[file_idx] = field.value;
                data.is_binary |= field.is_binary;
                data.stored_value_counts[file_idx] = field.stored_value_count;
            }
        }
    }

    let mut entries = Vec::new();
    for key in key_order {
        let data = key_map.remove(&key).ok_or_else(|| {
            format!("metadata merge lost canonical key {key} while preserving row order")
        })?;
        let all_same = data.values.windows(2).all(|values| values[0] == values[1]);
        let is_mixed = !all_same;
        let display_value = if is_mixed {
            "<multiple values>".to_string()
        } else {
            data.values.first().cloned().unwrap_or_default()
        };
        entries.push(TagEntry {
            row_scope: crate::tui::probe::RowScope::File,
            display_key: key,
            item_key: data.item_key,
            value: display_value.clone(),
            original: display_value,
            is_binary: data.is_binary,
            is_mixed,
            has_multiple_stored_values: data
                .stored_value_counts
                .iter()
                .any(|count| *count > 1),
            per_file_stored_value_counts: data.stored_value_counts,
            per_file_values: data.values.clone(),
            per_file_originals: data.values,
            mb_proposed_value: None,
            mb_proposed_per_file: None,
        });
    }

    for entry in &mut entries {
        if is_synthetic_preview(entry) {
            entry.is_binary = true;
        }
    }
    sort_entries_standard_first(&mut entries);

    Ok(MergedTagsAndMetadata {
        entries,
        metadata,
        metadata_errors,
    })
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
/// `state.active_surface().deleted` (indices of entries the user removed).
pub fn apply_audio_tag_changes(
    paths: &[std::path::PathBuf],
    entries_snap: &[(lofty::tag::ItemKey, Vec<String>, Vec<String>)],
    deleted: &[usize],
) -> Vec<(std::path::PathBuf, Result<(), String>)> {
    apply_audio_tag_changes_with_save_blocks(paths, entries_snap, deleted, &[])
        .into_iter()
        .map(crate::tui::app::MetadataEditorWriteResult::into_legacy_result)
        .collect()
}

/// Apply audio tag changes while respecting per-file save blocks captured by
/// the metadata editor at open time. A file that failed the initial Lofty read,
/// is read-only, or whose write eligibility could not be verified must not be
/// treated as an ordinary writable empty-tag file. If such a file has pending
/// changes, return an explicit skipped result instead of attempting a write.
pub type MetadataWriteProgressCallback = std::sync::Arc<dyn Fn(usize, usize, &std::path::Path, &crate::tui::app::MetadataEditorWriteResult) + Send + Sync>;
pub type MetadataWriteByteProgressCallback = std::sync::Arc<
    dyn Fn(usize, usize, &std::path::Path, crate::dsf_tags::DsfWriteProgress) + Send + Sync,
>;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MetadataWriteCommitReport {
    pub durability_warnings: Vec<String>,
}

impl MetadataWriteCommitReport {
    fn clean() -> Self {
        Self { durability_warnings: Vec::new() }
    }

    fn from_warnings(warnings: Vec<String>) -> Self {
        Self {
            durability_warnings: warnings
                .into_iter()
                .filter(|warning| !warning.trim().is_empty())
                .collect(),
        }
    }
}

/// Cooperative cancellation handle for metadata writes.
///
/// Cancellation is intentionally observed only at safe points: before a file
/// starts, before a full-file fallback starts, before a FLAC overflow rewrite
/// commits its temp file, and between bounded stream-copy chunks. Once a
/// metadata-region overwrite or rename commit has begun, the writer finishes
/// the local crash-safe sequence instead of leaving recovery artifacts half
/// maintained.
#[derive(Clone)]
pub struct MetadataWriteCancelFlag {
    cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    observations: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl MetadataWriteCancelFlag {
    pub fn new() -> Self {
        Self {
            cancelled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            observations: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn check(&self, context: &str) -> Result<(), String> {
        if self.is_cancelled() {
            self.record_observation();
            Err(format!("metadata save cancelled {context}"))
        } else {
            Ok(())
        }
    }

    fn operation_scope(&self) -> Self {
        Self {
            cancelled: std::sync::Arc::clone(&self.cancelled),
            observations: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }

    fn record_observation(&self) {
        self.observations
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    fn observation_count(&self) -> u64 {
        self.observations.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Default for MetadataWriteCancelFlag {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for MetadataWriteCancelFlag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MetadataWriteCancelFlag")
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

fn check_metadata_write_cancel(
    cancel: Option<&MetadataWriteCancelFlag>,
    context: &str,
) -> Result<(), String> {
    if let Some(cancel) = cancel {
        cancel.check(context)?;
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum MetadataWriteFailure {
    Cancelled(String),
    Failed(String),
}

impl MetadataWriteFailure {
    fn into_message(self) -> String {
        match self {
            Self::Cancelled(message) | Self::Failed(message) => message,
        }
    }
}

pub fn apply_audio_tag_changes_with_save_blocks(
    paths: &[std::path::PathBuf],
    entries_snap: &[(lofty::tag::ItemKey, Vec<String>, Vec<String>)],
    deleted: &[usize],
    save_block_reasons: &[Option<String>],
) -> Vec<crate::tui::app::MetadataEditorWriteResult> {
    apply_audio_tag_changes_with_save_blocks_and_progress(
        paths,
        entries_snap,
        deleted,
        save_block_reasons,
        None,
        None,
    )
}

/// Apply whole-file audio-tag changes with bounded per-file parallelism.
///
/// This is the legacy File-scope API boundary used by CLI/non-editor callers.
/// Every value/original vector must be path-aligned. Track-scoped rows must use
/// `apply_audio_tag_changes_with_save_blocks_progress_and_forced_deletes`, which
/// carries an explicit `RowScope`; this wrapper never infers scope by length.
/// Planning remains deterministic and single-threaded; only independent file
/// writes are piped. Results are returned in path order so save-result reduction
/// and sidecar gating remain stable even when workers complete out of order.
pub fn apply_audio_tag_changes_with_save_blocks_and_progress(
    paths: &[std::path::PathBuf],
    entries_snap: &[(lofty::tag::ItemKey, Vec<String>, Vec<String>)],
    deleted: &[usize],
    save_block_reasons: &[Option<String>],
    progress: Option<MetadataWriteProgressCallback>,
    cancel: Option<MetadataWriteCancelFlag>,
) -> Vec<crate::tui::app::MetadataEditorWriteResult> {
    // Rows whose value vectors don't align to the path count cannot be
    // whole-file writes. Route them as Track scope — which the underlying
    // writer SKIPS (track-dimension data round-trips through the regenerated
    // CUESHEET, never through per-file tags) — and keep writing the aligned
    // rows. Refusing the entire save here broke CLI tags-mb on single-image
    // sources, whose CUESHEET expansion always carries track-dimension rows
    // alongside perfectly aligned album/CUESHEET rows.
    let mut misaligned_rows = Vec::new();
    let scoped_entries: Vec<_> = entries_snap
        .iter()
        .enumerate()
        .map(|(idx, (key, values, originals))| {
            let scope = if values.len() != paths.len() || originals.len() != paths.len() {
                misaligned_rows.push(idx);
                RowScope::Track
            } else {
                RowScope::File
            };
            (key.clone(), scope, values.clone(), originals.clone())
        })
        .collect();
    if !misaligned_rows.is_empty() {
        log::warn!(
            "legacy metadata write: {} row(s) not aligned to {} path(s) routed track-scoped and skipped for whole-file writes: rows {:?}",
            misaligned_rows.len(),
            paths.len(),
            misaligned_rows
        );
    }
    apply_audio_tag_changes_with_save_blocks_progress_and_forced_deletes(
        paths,
        &scoped_entries,
        deleted,
        save_block_reasons,
        progress,
        None,
        cancel,
        &[],
    )
}

/// Apply audio tag changes with additional file-indexed tombstones that do not
/// have to correspond to a currently visible editor row. This is used for
/// embedded-CUESHEET deletion after the UI has reshaped itself from a sidecar
/// synthetic row: the save still deletes the on-disk tag through the normal
/// async/progress/cancellation path, but the visible CUESHEET row is no longer
/// treated as the tag to delete.
pub fn apply_audio_tag_changes_with_save_blocks_progress_and_forced_deletes(
    paths: &[std::path::PathBuf],
    entries_snap: &[(lofty::tag::ItemKey, RowScope, Vec<String>, Vec<String>)],
    deleted: &[usize],
    save_block_reasons: &[Option<String>],
    progress: Option<MetadataWriteProgressCallback>,
    byte_progress: Option<MetadataWriteByteProgressCallback>,
    cancel: Option<MetadataWriteCancelFlag>,
    forced_deletes: &[(usize, lofty::tag::ItemKey)],
) -> Vec<crate::tui::app::MetadataEditorWriteResult> {
    #[derive(Debug)]
    struct PlannedWrite {
        original_index: usize,
        write_ordinal: usize,
        path: std::path::PathBuf,
        changes: Vec<(lofty::tag::ItemKey, Option<String>)>,
    }

    let mut planned = Vec::new();
    let mut immediate_results: Vec<(usize, crate::tui::app::MetadataEditorWriteResult)> = Vec::new();

    for (file_idx, path) in paths.iter().enumerate() {
        let mut changes: Vec<(lofty::tag::ItemKey, Option<String>)> = Vec::new();
        for (entry_idx, (key, row_scope, vals, origs)) in entries_snap.iter().enumerate() {
            // Track-scoped rows round-trip through the regenerated CUESHEET
            // model instead of through whole-file tag writes. The explicit
            // marker is required when track and file dimensions are equal.
            if *row_scope == RowScope::Track {
                continue;
            }
            if deleted.contains(&entry_idx) {
                changes.push((key.clone(), None));
            } else if file_idx < vals.len() && file_idx < origs.len() && vals[file_idx] != origs[file_idx] {
                changes.push((key.clone(), Some(vals[file_idx].clone())));
            }
        }
        for (target_idx, key) in forced_deletes {
            if *target_idx == file_idx {
                changes.push((key.clone(), None));
            }
        }

        if changes.is_empty() {
            continue;
        }

        if let Some(reason) = save_block_reasons
            .get(file_idx)
            .and_then(|reason| reason.as_ref())
            .filter(|reason| !reason.trim().is_empty())
        {
            immediate_results.push((
                file_idx,
                crate::tui::app::MetadataEditorWriteResult::skipped(
                    path.clone(),
                    format!("file is not writable in this editor session: {}", reason.trim()),
                ),
            ));
            continue;
        }

        planned.push(PlannedWrite {
            original_index: file_idx,
            write_ordinal: 0,
            path: path.clone(),
            changes,
        });
    }

    if planned.is_empty() {
        immediate_results.sort_by_key(|(idx, _)| *idx);
        return immediate_results.into_iter().map(|(_, result)| result).collect();
    }

    let total = planned.len();
    for (idx, write) in planned.iter_mut().enumerate() {
        write.write_ordinal = idx + 1;
    }
    let requires_serialization = metadata_write_targets_require_serialization(
        planned.iter().map(|write| write.path.as_path()),
    );
    let worker_count = metadata_write_worker_count(total, requires_serialization);
    let planned = std::sync::Arc::new(std::sync::Mutex::new(std::collections::VecDeque::from(planned)));
    let completed = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let results = std::sync::Arc::new(std::sync::Mutex::new(Vec::<(usize, crate::tui::app::MetadataEditorWriteResult)>::with_capacity(total)));
    let cancel = std::sync::Arc::new(cancel);

    std::thread::scope(|scope| {
        for _ in 0..worker_count {
            let planned = std::sync::Arc::clone(&planned);
            let completed = std::sync::Arc::clone(&completed);
            let results = std::sync::Arc::clone(&results);
            let progress = progress.clone();
            let byte_progress = byte_progress.clone();
            let cancel = std::sync::Arc::clone(&cancel);
            scope.spawn(move || loop {
                if cancel.as_ref().as_ref().is_some_and(|flag| flag.is_cancelled()) {
                    break;
                }
                let Some(write) = planned.lock().expect("metadata write queue poisoned").pop_front() else {
                    break;
                };
                let result = if cancel.as_ref().as_ref().is_some_and(|flag| flag.is_cancelled()) {
                    crate::tui::app::MetadataEditorWriteResult::skipped(
                        write.path.clone(),
                        "metadata save cancelled before starting this file".to_string(),
                    )
                } else {
                    let report_byte_progress =
                        |path: &std::path::Path, update: crate::dsf_tags::DsfWriteProgress| {
                            if let Some(progress) = byte_progress.as_deref() {
                                progress(write.write_ordinal, total, path, update);
                            }
                        };
                    match write_all_tags_with_cancel_report_classified(
                        &write.path,
                        &write.changes,
                        cancel.as_ref().as_ref(),
                        Some(&report_byte_progress),
                    ) {
                        Ok(report) => crate::tui::app::MetadataEditorWriteResult::saved_with_warnings(
                            write.path.clone(),
                            report.durability_warnings,
                        ),
                        Err(MetadataWriteFailure::Cancelled(reason)) => {
                            crate::tui::app::MetadataEditorWriteResult::skipped(write.path.clone(), reason)
                        }
                        Err(MetadataWriteFailure::Failed(reason)) => {
                            crate::tui::app::MetadataEditorWriteResult::failed(write.path.clone(), reason)
                        }
                    }
                };
                let done = completed.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                if let Some(progress) = progress.as_ref() {
                    progress(done, total, &write.path, &result);
                }
                results
                    .lock()
                    .expect("metadata write result set poisoned")
                    .push((write.original_index, result));
            });
        }
    });

    while let Some(write) = planned.lock().expect("metadata write queue poisoned").pop_front() {
        let result = crate::tui::app::MetadataEditorWriteResult::skipped(
            write.path.clone(),
            "metadata save cancelled before starting this file".to_string(),
        );
        let done = completed.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        if let Some(progress) = progress.as_ref() {
            progress(done, total, &write.path, &result);
        }
        results
            .lock()
            .expect("metadata write result set poisoned")
            .push((write.original_index, result));
    }

    let mut out = std::sync::Arc::try_unwrap(results)
        .map_err(|_| ())
        .ok()
        .and_then(|mutex| mutex.into_inner().ok())
        .unwrap_or_default();
    out.extend(immediate_results);
    out.sort_by_key(|(idx, _)| *idx);
    out.into_iter().map(|(_, result)| result).collect()
}

fn metadata_write_parallelism(write_count: usize) -> usize {
    let cpus = num_cpus::get().max(1);
    write_count.min(cpus).min(4).max(1)
}

fn metadata_write_targets_require_serialization<'a>(
    paths: impl IntoIterator<Item = &'a std::path::Path>,
) -> bool {
    let mut pathnames = std::collections::BTreeSet::new();
    let mut authority_paths = std::collections::BTreeSet::new();
    let mut identities = Vec::<same_file::Handle>::new();
    for path in paths {
        if !pathnames.insert(path.to_path_buf()) {
            return true;
        }
        if crate::dsf_tags::is_dsf(path) {
            let identity = match same_file::Handle::from_path(path) {
                Ok(identity) => identity,
                // DSF writes are destructive. If file identity cannot be
                // established, fail closed to the serial scheduler rather than
                // assuming two pathnames cannot name the same inode/file object.
                Err(_) => return true,
            };
            if identities.iter().any(|existing| existing == &identity) {
                return true;
            }
            identities.push(identity);
            let authorities = match crate::dsf_tags::write_authority_paths(path) {
                Ok(authorities) => authorities,
                Err(_) => return true,
            };
            if authorities
                .into_iter()
                .any(|authority| !authority_paths.insert(authority))
            {
                return true;
            }
        }
    }
    false
}

fn metadata_write_worker_count(write_count: usize, requires_serialization: bool) -> usize {
    // Distinct DSF targets may run concurrently only when their pathnames,
    // file identities, hashed journals, legacy journals, and store-lock
    // authority paths are all disjoint. Any derivation failure serializes.
    if requires_serialization {
        1
    } else {
        metadata_write_parallelism(write_count)
    }
}

fn is_dff_path(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("dff"))
}

fn reject_unsupported_dff_metadata_write(
    path: &std::path::Path,
    operation: &str,
) -> Result<(), String> {
    if is_dff_path(path) {
        Err(format!(
            "DFF metadata {operation} is not supported for '{}'; refusing before allocating a full-file rollback backup",
            path.display()
        ))
    } else {
        Ok(())
    }
}

fn reject_unsupported_dff_metadata_batch(
    paths: &[std::path::PathBuf],
    operation: &str,
) -> Result<(), String> {
    for path in paths {
        reject_unsupported_dff_metadata_write(path, operation)?;
    }
    Ok(())
}

/// Write a batch of tag changes to an audio file.
/// Each entry in `changes` is (ItemKey, Option<new_value>).
/// `None` means delete the tag. Empty string also deletes.
pub fn write_all_tags(
    path: &std::path::Path,
    changes: &[(lofty::tag::ItemKey, Option<String>)],
) -> Result<(), String> {
    write_all_tags_with_cancel(path, changes, None)
}

pub fn write_all_tags_with_cancel(
    path: &std::path::Path,
    changes: &[(lofty::tag::ItemKey, Option<String>)],
    cancel: Option<&MetadataWriteCancelFlag>,
) -> Result<(), String> {
    write_all_tags_with_cancel_report_classified(path, changes, cancel, None)
        .map(|_| ())
        .map_err(MetadataWriteFailure::into_message)
}

fn write_all_tags_with_cancel_report_classified(
    path: &std::path::Path,
    changes: &[(lofty::tag::ItemKey, Option<String>)],
    cancel: Option<&MetadataWriteCancelFlag>,
    byte_progress: Option<
        &(dyn Fn(&std::path::Path, crate::dsf_tags::DsfWriteProgress) + Send + Sync),
    >,
) -> Result<MetadataWriteCommitReport, MetadataWriteFailure> {
    let operation_cancel = cancel.map(MetadataWriteCancelFlag::operation_scope);
    write_all_tags_with_cancel_report(path, changes, operation_cancel.as_ref(), byte_progress)
        .map_err(|message| {
            if operation_cancel
                .as_ref()
                .is_some_and(|flag| flag.observation_count() > 0)
            {
                MetadataWriteFailure::Cancelled(message)
            } else {
                MetadataWriteFailure::Failed(message)
            }
        })
}

fn write_all_tags_with_cancel_report(
    path: &std::path::Path,
    changes: &[(lofty::tag::ItemKey, Option<String>)],
    cancel: Option<&MetadataWriteCancelFlag>,
    byte_progress: Option<
        &(dyn Fn(&std::path::Path, crate::dsf_tags::DsfWriteProgress) + Send + Sync),
    >,
) -> Result<MetadataWriteCommitReport, String> {
    if changes.is_empty() {
        return Ok(MetadataWriteCommitReport::clean());
    }

    check_metadata_write_cancel(cancel, "before starting file")?;

    if crate::dsf_tags::is_dsf(path) {
        let dsf_changes = changes
            .iter()
            .map(|(key, value)| {
                let canonical_key = match key {
                    lofty::tag::ItemKey::Unknown(value) => Some(value.as_str()),
                    _ => key.map_key(lofty::tag::TagType::VorbisComments, true),
                }
                .ok_or_else(|| format!("cannot map {:?} to the DSF editor tag canon", key))?;
                Ok(crate::dsf_tags::DsfTagChange {
                    canonical_key: canonical_metadata_display_key(canonical_key),
                    value: value.clone(),
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let is_cancelled = || {
            cancel.is_some_and(|flag| {
                let cancelled = flag.is_cancelled();
                if cancelled {
                    flag.record_observation();
                }
                cancelled
            })
        };
        let report_progress = |update| {
            if let Some(progress) = byte_progress {
                progress(path, update);
            }
        };
        let warning = crate::dsf_tags::write_with_control(
            path,
            &dsf_changes,
            &is_cancelled,
            &report_progress,
        )?;
        return Ok(MetadataWriteCommitReport::from_warnings(warning.into_iter().collect()));
    }

    if flac_metadata_writer::is_probably_flac(path) {
        let observation_before = cancel.map_or(0, MetadataWriteCancelFlag::observation_count);
        match flac_metadata_writer::write_vorbis_comment_changes(path, changes, cancel) {
            Ok(report) => {
                return Ok(MetadataWriteCommitReport::from_warnings(report.durability_warnings));
            }
            Err(native_err)
                if cancel.is_some_and(|flag| flag.observation_count() > observation_before) =>
            {
                return Err(native_err);
            }
            Err(native_err) => {
                return Err(native_flac_write_refused_error(path, "tag write", &native_err));
            }
        }
    }

    reject_unsupported_dff_metadata_write(path, "writing")?;
    check_metadata_write_cancel(cancel, "before starting full-file fallback rewrite")?;
    let cleanup_warning = write_all_tags_lofty_with_backup(path, changes)?;
    Ok(MetadataWriteCommitReport::from_warnings(cleanup_warning.into_iter().collect()))
}

fn write_all_tags_without_full_file_backup(
    path: &std::path::Path,
    changes: &[(lofty::tag::ItemKey, Option<String>)],
) -> Result<(), String> {
    if changes.is_empty() {
        return Ok(());
    }

    if crate::dsf_tags::is_dsf(path) {
        return Err(format!(
            "internal transaction error: DSF path '{}' reached the legacy full-file transaction even though DSF owns a native recovery journal",
            path.display()
        ));
    }

    if flac_metadata_writer::is_probably_flac(path) {
        return Err(format!(
            "internal transaction error: native FLAC path '{}' reached the full-file writer",
            path.display()
        ));
    }

    reject_unsupported_dff_metadata_write(path, "writing")?;
    write_all_tags_lofty_in_place(path, changes)
}

fn native_flac_write_refused_error(path: &std::path::Path, operation: &str, native_err: &str) -> String {
    format!(
        "native FLAC metadata-region {operation} refused for '{}': {native_err}. Refusing automatic Lofty fallback because it would create a full-file .tonepoet-bak copy and rewrite the whole FLAC. Repair the FLAC, add sufficient FLAC padding, or use an explicit full-rewrite repair path if that cost is acceptable.",
        path.display()
    )
}

#[cfg(test)]
type LoftyFallbackHook = dyn Fn(&std::path::Path) + Send + Sync + 'static;

#[cfg(test)]
static TEST_LOFTY_FALLBACK_HOOK: std::sync::OnceLock<
    std::sync::Mutex<Option<std::sync::Arc<LoftyFallbackHook>>>,
> = std::sync::OnceLock::new();

#[cfg(test)]
fn run_test_lofty_fallback_hook(path: &std::path::Path) {
    let Some(slot) = TEST_LOFTY_FALLBACK_HOOK.get() else {
        return;
    };
    let hook = slot
        .lock()
        .expect("Lofty fallback hook poisoned")
        .clone();
    if let Some(hook) = hook {
        hook(path);
    }
}

fn lofty_vorbis_comment_key(key: &lofty::tag::ItemKey) -> Option<String> {
    let mapped = match key {
        lofty::tag::ItemKey::Unknown(name) => Some(name.as_str()),
        _ => key.map_key(lofty::tag::TagType::VorbisComments, true),
    }?;
    let mapped = mapped.trim();
    (!mapped.is_empty()).then(|| mapped.to_ascii_uppercase())
}

fn canonical_vorbis_alias_group<'a>(
    key: &'a str,
) -> (&'a str, &'static [&'static str]) {
    match key {
        "TRACKTOTAL" | "TOTALTRACKS" => ("TRACKTOTAL", &["TRACKTOTAL", "TOTALTRACKS"]),
        "DISCTOTAL" | "TOTALDISCS" => ("DISCTOTAL", &["DISCTOTAL", "TOTALDISCS"]),
        "COMMENT" | "DESCRIPTION" => ("COMMENT", &["COMMENT", "DESCRIPTION"]),
        _ => (key, &[]),
    }
}

fn normalized_vorbis_lofty_changes(
    changes: &[(lofty::tag::ItemKey, Option<String>)],
) -> Result<
    Vec<(
        String,
        lofty::tag::ItemKey,
        Option<String>,
        Vec<lofty::tag::ItemKey>,
    )>,
    String,
> {
    use std::collections::BTreeMap;

    let mut resolved = BTreeMap::<String, Option<String>>::new();
    for (key, value) in changes {
        let raw = lofty_vorbis_comment_key(key)
            .ok_or_else(|| format!("cannot map {:?} to a Vorbis comment key", key))?;
        let (canonical, _) = canonical_vorbis_alias_group(&raw);
        let normalized = value
            .as_ref()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        if let Some(previous) = resolved.get(canonical) {
            if previous != &normalized {
                return Err(format!(
                    "conflicting metadata changes target the same Vorbis alias group {canonical}: {previous:?} versus {normalized:?}"
                ));
            }
        } else {
            resolved.insert(canonical.to_string(), normalized);
        }
    }

    Ok(resolved
        .into_iter()
        .map(|(canonical, value)| {
            let (_, aliases) = canonical_vorbis_alias_group(&canonical);
            let removal_keys = aliases
                .iter()
                .map(|alias| lofty::tag::ItemKey::Unknown((*alias).to_string()))
                .chain(std::iter::once(lofty::tag::ItemKey::Unknown(canonical.clone())))
                .collect::<Vec<_>>();
            let insert_key = match canonical.as_str() {
                "COMMENT" => lofty::tag::ItemKey::Comment,
                _ => lofty::tag::ItemKey::Unknown(canonical.clone()),
            };
            (canonical, insert_key, value, removal_keys)
        })
        .collect())
}


fn apply_vorbis_lofty_changes(
    tag: &mut lofty::tag::Tag,
    changes: &[(lofty::tag::ItemKey, Option<String>)],
) -> Result<(), String> {
    use lofty::tag::{ItemValue, TagItem};

    for (canonical, insert_key, new_value, removal_keys) in
        normalized_vorbis_lofty_changes(changes)?
    {
        // Remove the actual ItemKey instances Lofty parsed, not only guessed
        // Unknown aliases. Some Vorbis spellings map to typed ItemKey variants,
        // while others remain Unknown; canonicalizing each existing item before
        // collecting its key covers both.
        let parsed_alias_keys = tag
            .items()
            .filter_map(|item| {
                let raw = lofty_vorbis_comment_key(item.key())?;
                let (existing_canonical, _) = canonical_vorbis_alias_group(&raw);
                (existing_canonical == canonical).then(|| item.key().clone())
            })
            .collect::<Vec<_>>();
        for removal_key in parsed_alias_keys.iter().chain(removal_keys.iter()) {
            tag.remove_key(removal_key);
        }
        tag.remove_key(&insert_key);
        if let Some(value) = new_value {
            tag.insert_unchecked(TagItem::new(insert_key, ItemValue::Text(value)));
        }
    }
    Ok(())
}

fn write_all_tags_lofty_in_place(
    path: &std::path::Path,
    changes: &[(lofty::tag::ItemKey, Option<String>)],
) -> Result<(), String> {
    use lofty::config::WriteOptions;
    use lofty::file::{AudioFile, TaggedFileExt};
    use lofty::tag::{ItemValue, TagItem};

    let mut tagged = lofty::read_from_path(path)
        .map_err(|error| format!("failed to read '{}': {error}", path.display()))?;

    if tagged.primary_tag().is_none() {
        let tag_type = tagged.primary_tag_type();
        tagged.insert_tag(lofty::tag::Tag::new(tag_type));
    }
    let tag = tagged
        .primary_tag_mut()
        .ok_or_else(|| "failed to create primary tag".to_string())?;

    if tag.tag_type() == lofty::tag::TagType::VorbisComments {
        apply_vorbis_lofty_changes(tag, changes)?;
    } else {
        for (key, new_value) in changes {
            match new_value {
                Some(value) if !value.trim().is_empty() => {
                    tag.remove_key(key);
                    tag.insert_unchecked(TagItem::new(
                        key.clone(),
                        ItemValue::Text(value.trim().to_string()),
                    ));
                }
                _ => {
                    tag.remove_key(key);
                }
            }
        }
    }

    tagged
        .save_to_path(path, WriteOptions::default())
        .map_err(|error| format!("failed to save '{}': {error}", path.display()))
}

fn write_all_tags_lofty_with_backup(
    path: &std::path::Path,
    changes: &[(lofty::tag::ItemKey, Option<String>)],
) -> Result<Option<String>, String> {
    reject_unsupported_dff_metadata_write(path, "writing")?;
    // Non-FLAC formats keep the existing file-scope rollback path until they
    // receive native metadata-region writers. FLACs must not silently enter
    // this path: a native FLAC refusal is returned to the caller as an
    // explicit, user-visible error instead of creating a full-file backup.
    #[cfg(test)]
    run_test_lofty_fallback_hook(path);
    let backup = crate::db::Database::backup_path_for(path);
    crate::db::Database::create_backup_for(path, &backup)
        .map_err(|error| format!("backup failed for '{}': {error}", path.display()))?;

    match write_all_tags_lofty_in_place(path, changes) {
        Ok(()) => match std::fs::remove_file(&backup) {
            Ok(()) => Ok(None),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Some(format!(
                "metadata write for '{}' committed, but full-file rollback marker '{}' was already absent during cleanup",
                path.display(),
                backup.display()
            ))),
            Err(error) => Ok(Some(format!(
                "metadata write for '{}' committed, but cleanup of full-file rollback marker '{}' failed: {error}. Verify the committed file, then remove the marker explicitly; future full-file writes will refuse to overwrite it.",
                path.display(),
                backup.display()
            ))),
        },
        Err(error) => match crate::db::Database::restore_backup_for(path, &backup) {
            Ok(()) => Err(error),
            Err(restore_error) => Err(format!(
                "{error}; rollback could not be completed for '{}' from rollback marker '{}': {restore_error}",
                path.display(),
                backup.display()
            )),
        },
    }
}

/// Embed or replace one artwork picture type in each path and return updated compact metadata.
#[derive(Debug, Clone, Default)]
pub struct ArtworkWriteBatchResult {
    pub metadata: Vec<SourceMetadata>,
    pub durability_warnings: Vec<String>,
}

pub fn write_artwork_to_files(
    paths: &[std::path::PathBuf],
    image_path: &std::path::Path,
    picture_type: lofty::picture::PictureType,
) -> Result<ArtworkWriteBatchResult, String> {
    write_artwork_to_files_with_cancel(paths, image_path, picture_type, None)
}

pub fn write_artwork_to_files_with_cancel(
    paths: &[std::path::PathBuf],
    image_path: &std::path::Path,
    picture_type: lofty::picture::PictureType,
    cancel: Option<&MetadataWriteCancelFlag>,
) -> Result<ArtworkWriteBatchResult, String> {
    check_metadata_write_cancel(cancel, "before reading artwork image")?;
    reject_unsupported_dff_metadata_batch(paths, "artwork writing")?;
    let image_bytes = std::fs::read(image_path)
        .map_err(|e| format!("read artwork '{}': {}", image_path.display(), e))?;
    let mime_type = image_mime_type(image_path, &image_bytes)?;
    let mime_label = mime_type_to_string(&mime_type);
    let mut metadata_cache = read_artwork_metadata_cache(paths)?;
    let artwork_info = artwork_info_from_image(picture_type, &mime_label, &image_bytes);
    let commit = apply_artwork_batch(paths, cancel, |path| {
        write_artwork_one_file(path, &image_bytes, &mime_type, &mime_label, picture_type, cancel)
    })?;
    Ok(ArtworkWriteBatchResult {
        metadata: project_artwork_metadata_from_cache(
            paths,
            &mut metadata_cache,
            ArtworkProjection::Replace(artwork_info),
        ),
        durability_warnings: commit.committed_warnings,
    })
}

/// Remove one artwork picture type from each path and return updated compact metadata.
pub fn remove_artwork_from_files(
    paths: &[std::path::PathBuf],
    picture_type: lofty::picture::PictureType,
) -> Result<ArtworkWriteBatchResult, String> {
    remove_artwork_from_files_with_cancel(paths, picture_type, None)
}

pub fn remove_artwork_from_files_with_cancel(
    paths: &[std::path::PathBuf],
    picture_type: lofty::picture::PictureType,
    cancel: Option<&MetadataWriteCancelFlag>,
) -> Result<ArtworkWriteBatchResult, String> {
    check_metadata_write_cancel(cancel, "before reading artwork metadata")?;
    reject_unsupported_dff_metadata_batch(paths, "artwork removal")?;
    let mut metadata_cache = read_artwork_metadata_cache(paths)?;
    let commit = apply_artwork_batch(paths, cancel, |path| remove_artwork_one_file(path, picture_type, cancel))?;
    Ok(ArtworkWriteBatchResult {
        metadata: project_artwork_metadata_from_cache(
            paths,
            &mut metadata_cache,
            ArtworkProjection::Remove(picture_type),
        ),
        durability_warnings: commit.committed_warnings,
    })
}

#[derive(Debug)]
enum ArtworkRollbackToken {
    Flac {
        path: std::path::PathBuf,
        snapshot: flac_metadata_writer::FlacMetadataSnapshot,
        journal: std::path::PathBuf,
        _write_claim: flac_metadata_writer::FlacWriteClaim,
    },
    Dsf {
        path: std::path::PathBuf,
        snapshot: crate::dsf_tags::DsfArtworkSnapshot,
        commit_warning: Option<String>,
    },
    FullFileBackup {
        path: std::path::PathBuf,
        backup: std::path::PathBuf,
    },
}

fn apply_artwork_batch<F>(
    paths: &[std::path::PathBuf],
    cancel: Option<&MetadataWriteCancelFlag>,
    mut apply: F,
) -> Result<ArtworkCleanupReport, String>
where
    F: FnMut(&std::path::Path) -> Result<Option<ArtworkRollbackToken>, String>,
{
    let unique_paths = unique_artwork_paths(paths);
    let mut rollbacks: Vec<ArtworkRollbackToken> = Vec::with_capacity(unique_paths.len());

    for path in &unique_paths {
        if let Err(err) = check_metadata_write_cancel(cancel, "before starting artwork file") {
            let rollback_issues = rollback_artwork_tokens(&mut rollbacks);
            if rollback_issues.is_empty() {
                return Err(err);
            }
            return Err(format!("{err}; rollback issues: {}", rollback_issues.join("; ")));
        }
        match apply(path) {
            Ok(Some(token)) => rollbacks.push(token),
            Ok(None) => {}
            Err(err) => {
                let rollback_issues = rollback_artwork_tokens(&mut rollbacks);
                if rollback_issues.is_empty() {
                    return Err(err);
                }
                return Err(format!("{err}; rollback issues: {}", rollback_issues.join("; ")));
            }
        }
    }

    let cleanup = cleanup_artwork_tokens(&mut rollbacks);
    if cleanup.blocking_issues.is_empty() {
        Ok(cleanup)
    } else {
        Err(format!(
            "artwork batch completed, but one or more rollback artifacts remain armed; refusing to report a clean commit: {}",
            cleanup.blocking_issues.join("; ")
        ))
    }
}

fn write_artwork_one_file(
    path: &std::path::Path,
    image_bytes: &[u8],
    lofty_mime_type: &lofty::picture::MimeType,
    flac_mime_type: &str,
    picture_type: lofty::picture::PictureType,
    cancel: Option<&MetadataWriteCancelFlag>,
) -> Result<Option<ArtworkRollbackToken>, String> {
    if flac_metadata_writer::is_probably_flac(path) {
        // Once a file is identified as FLAC, every native-path failure must
        // carry the no-Lofty-fallback explanation, including precursor steps
        // (claim, parse/preview, journal) — not just the mutation itself.
        let refuse = |native_err: String| {
            if native_err.contains("metadata save cancelled") {
                native_err
            } else {
                native_flac_write_refused_error(path, "artwork write", &native_err)
            }
        };
        let write_claim = flac_metadata_writer::acquire_native_write_claim(path, "artwork write")
            .map_err(refuse)?;
        let (snapshot, intended_metadata_region) = flac_metadata_writer::preview_picture_write(
            path,
            picture_type,
            flac_mime_type,
            image_bytes,
        )
        .map_err(refuse)?;
        let rollback_journal = flac_metadata_writer::begin_artwork_rollback_journal_with_intended(
            path,
            &snapshot,
            &intended_metadata_region,
        )
        .map_err(refuse)?;
        let journal = rollback_journal.path.clone();
        match flac_metadata_writer::write_picture_block(path, picture_type, flac_mime_type, image_bytes, cancel) {
            Ok(_outcome) => {
                drop(rollback_journal);
                return Ok(Some(ArtworkRollbackToken::Flac {
                    path: path.to_path_buf(),
                    snapshot,
                    journal,
                    _write_claim: write_claim,
                }));
            }
            Err(native_err) => {
                match flac_metadata_writer::restore_metadata_snapshot(path, &snapshot) {
                    Ok(()) => {
                        if let Err(cleanup_err) = flac_metadata_writer::remove_artwork_rollback_journal_after_successful_restore(path) {
                            return Err(format!(
                                "FLAC artwork write for '{}' failed: {native_err}; rollback restored the original metadata, but cleanup of recovery journal '{}' failed: {cleanup_err}. The rollback journal remains armed and will be retried by recovery.",
                                path.display(),
                                journal.display()
                            ));
                        }
                    }
                    Err(rollback_err) => {
                        return Err(format!(
                            "FLAC artwork write for '{}' failed: {native_err}; rollback restore failed: {rollback_err}. Recovery journal '{}' remains armed and must not be removed until recovery succeeds.",
                            path.display(),
                            journal.display()
                        ));
                    }
                }
                if native_err.contains("metadata save cancelled") {
                    return Err(native_err);
                }
                return Err(native_flac_write_refused_error(path, "artwork write", &native_err));
            }
        }
    }

    if crate::dsf_tags::is_dsf(path) {
        let cancelled = || cancel.is_some_and(MetadataWriteCancelFlag::is_cancelled);
        let (snapshot, commit_warning) = crate::dsf_tags::write_artwork_with_control(
            path,
            picture_type.as_u8(),
            flac_mime_type,
            image_bytes,
            &cancelled,
            &|_| {},
        )?;
        return Ok(Some(ArtworkRollbackToken::Dsf {
            path: path.to_path_buf(),
            snapshot,
            commit_warning,
        }));
    }

    reject_unsupported_dff_metadata_write(path, "artwork writing")?;
    check_metadata_write_cancel(cancel, "before starting artwork full-file fallback rewrite")?;
    write_artwork_lofty_with_backup(path, image_bytes, lofty_mime_type, picture_type)
        .map(|backup| Some(ArtworkRollbackToken::FullFileBackup { path: path.to_path_buf(), backup }))
}

fn remove_artwork_one_file(
    path: &std::path::Path,
    picture_type: lofty::picture::PictureType,
    cancel: Option<&MetadataWriteCancelFlag>,
) -> Result<Option<ArtworkRollbackToken>, String> {
    if flac_metadata_writer::is_probably_flac(path) {
        // Same rule as artwork writes: precursor failures on a FLAC target
        // carry the no-Lofty-fallback explanation.
        let refuse = |native_err: String| {
            if native_err.contains("metadata save cancelled") {
                native_err
            } else {
                native_flac_write_refused_error(path, "artwork removal", &native_err)
            }
        };
        let write_claim = flac_metadata_writer::acquire_native_write_claim(path, "artwork removal")
            .map_err(refuse)?;
        let Some((snapshot, intended_metadata_region)) =
            flac_metadata_writer::preview_picture_removal(path, picture_type).map_err(refuse)?
        else {
            return Ok(None);
        };
        let rollback_journal = flac_metadata_writer::begin_artwork_rollback_journal_with_intended(
            path,
            &snapshot,
            &intended_metadata_region,
        )
        .map_err(refuse)?;
        let journal = rollback_journal.path.clone();
        match flac_metadata_writer::remove_picture_block(path, picture_type, cancel) {
            Ok(_outcome) => {
                drop(rollback_journal);
                return Ok(Some(ArtworkRollbackToken::Flac {
                    path: path.to_path_buf(),
                    snapshot,
                    journal,
                    _write_claim: write_claim,
                }));
            }
            Err(native_err) => {
                match flac_metadata_writer::restore_metadata_snapshot(path, &snapshot) {
                    Ok(()) => {
                        if let Err(cleanup_err) = flac_metadata_writer::remove_artwork_rollback_journal_after_successful_restore(path) {
                            return Err(format!(
                                "FLAC artwork removal for '{}' failed: {native_err}; rollback restored the original metadata, but cleanup of recovery journal '{}' failed: {cleanup_err}. The rollback journal remains armed and will be retried by recovery.",
                                path.display(),
                                journal.display()
                            ));
                        }
                    }
                    Err(rollback_err) => {
                        return Err(format!(
                            "FLAC artwork removal for '{}' failed: {native_err}; rollback restore failed: {rollback_err}. Recovery journal '{}' remains armed and must not be removed until recovery succeeds.",
                            path.display(),
                            journal.display()
                        ));
                    }
                }
                if native_err.contains("metadata save cancelled") {
                    return Err(native_err);
                }
                return Err(native_flac_write_refused_error(path, "artwork removal", &native_err));
            }
        }
    }

    if crate::dsf_tags::is_dsf(path) {
        let cancelled = || cancel.is_some_and(MetadataWriteCancelFlag::is_cancelled);
        let Some((snapshot, commit_warning)) = crate::dsf_tags::remove_artwork_with_control(
            path,
            picture_type.as_u8(),
            &cancelled,
            &|_| {},
        )? else {
            return Ok(None);
        };
        return Ok(Some(ArtworkRollbackToken::Dsf {
            path: path.to_path_buf(),
            snapshot,
            commit_warning,
        }));
    }

    reject_unsupported_dff_metadata_write(path, "artwork removal")?;
    check_metadata_write_cancel(cancel, "before starting artwork full-file fallback rewrite")?;
    remove_artwork_lofty_with_backup(path, picture_type)
        .map(|backup| Some(ArtworkRollbackToken::FullFileBackup { path: path.to_path_buf(), backup }))
}

fn write_artwork_lofty_with_backup(
    path: &std::path::Path,
    image_bytes: &[u8],
    mime_type: &lofty::picture::MimeType,
    picture_type: lofty::picture::PictureType,
) -> Result<std::path::PathBuf, String> {
    use lofty::config::WriteOptions;
    use lofty::file::{AudioFile, TaggedFileExt};
    use lofty::picture::Picture;

    let backup = crate::db::Database::backup_path_for(path);
    crate::db::Database::create_backup_for(path, &backup)
        .map_err(|error| format!("backup failed for '{}': {error}", path.display()))?;

    let result = (|| -> Result<(), String> {
        let mut tagged = lofty::read_from_path(path)
            .map_err(|e| format!("failed to read '{}': {}", path.display(), e))?;
        if tagged.primary_tag().is_none() {
            let tt = tagged.primary_tag_type();
            tagged.insert_tag(lofty::tag::Tag::new(tt));
        }
        let tag = tagged
            .primary_tag_mut()
            .ok_or_else(|| format!("no writable tag for '{}'", path.display()))?;
        tag.remove_picture_type(picture_type);
        tag.push_picture(Picture::new_unchecked(
            picture_type,
            Some(mime_type.clone()),
            None,
            image_bytes.to_vec(),
        ));
        tagged
            .save_to_path(path, WriteOptions::default())
            .map_err(|e| format!("failed to save '{}': {}", path.display(), e))?;
        Ok(())
    })();

    if let Err(err) = result {
        return match crate::db::Database::restore_backup_for(path, &backup) {
            Ok(()) => Err(err),
            Err(restore_err) => Err(format!(
                "{err}; rollback could not be completed for '{}' from artwork rollback marker '{}': {restore_err}",
                path.display(),
                backup.display()
            )),
        };
    }
    Ok(backup)
}

fn remove_artwork_lofty_with_backup(
    path: &std::path::Path,
    picture_type: lofty::picture::PictureType,
) -> Result<std::path::PathBuf, String> {
    use lofty::config::WriteOptions;
    use lofty::file::{AudioFile, TaggedFileExt};

    let backup = crate::db::Database::backup_path_for(path);
    crate::db::Database::create_backup_for(path, &backup)
        .map_err(|error| format!("backup failed for '{}': {error}", path.display()))?;

    let result = (|| -> Result<(), String> {
        let mut tagged = lofty::read_from_path(path)
            .map_err(|e| format!("failed to read '{}': {}", path.display(), e))?;
        let Some(tag) = tagged.primary_tag_mut() else {
            return Ok(());
        };
        tag.remove_picture_type(picture_type);
        tagged
            .save_to_path(path, WriteOptions::default())
            .map_err(|e| format!("failed to save '{}': {}", path.display(), e))?;
        Ok(())
    })();

    if let Err(err) = result {
        return match crate::db::Database::restore_backup_for(path, &backup) {
            Ok(()) => Err(err),
            Err(restore_err) => Err(format!(
                "{err}; rollback could not be completed for '{}' from artwork rollback marker '{}': {restore_err}",
                path.display(),
                backup.display()
            )),
        };
    }
    Ok(backup)
}

fn rollback_artwork_tokens(tokens: &mut [ArtworkRollbackToken]) -> Vec<String> {
    let mut issues = Vec::new();
    for token in tokens.iter_mut().rev() {
        match token {
            ArtworkRollbackToken::Flac { path, snapshot, journal, _write_claim } => {
                match flac_metadata_writer::restore_metadata_snapshot(path, snapshot) {
                    Ok(()) => {
                        if let Err(err) = flac_metadata_writer::remove_artwork_rollback_journal_after_successful_restore(path) {
                            issues.push(format!(
                                "remove FLAC artwork rollback journal '{}' failed after successful rollback restore: {err}; journal remains armed for later recovery",
                                journal.display()
                            ));
                        }
                    }
                    Err(err) => {
                        issues.push(format!(
                            "restore FLAC metadata '{}' failed: {err}; keeping rollback journal '{}' armed for later recovery",
                            path.display(),
                            journal.display()
                        ));
                    }
                }
                if let Some(warning) = _write_claim.release_with_warning("FLAC common write lock removal after artwork rollback") {
                    issues.push(warning);
                }
            }
            ArtworkRollbackToken::Dsf { path, snapshot, .. } => {
                match crate::dsf_tags::restore_artwork_snapshot(path, snapshot) {
                    Ok(Some(warning)) => issues.push(warning),
                    Ok(None) => {}
                    Err(error) => issues.push(format!(
                        "restore DSF artwork metadata '{}' failed: {error}; any retained DSF journal remains authoritative for startup recovery",
                        path.display()
                    )),
                }
            }
            ArtworkRollbackToken::FullFileBackup { path, backup } => {
                if backup.exists() {
                    if let Err(error) = crate::db::Database::restore_backup_for(path, backup) {
                        issues.push(format!(
                            "restore '{}' from '{}' could not be completed: {error}",
                            path.display(),
                            backup.display()
                        ));
                    }
                } else {
                    issues.push(format!(
                        "restore '{}' failed: backup '{}' missing",
                        path.display(),
                        backup.display()
                    ));
                }
            }
        }
    }
    issues
}

#[derive(Debug, Default)]
struct ArtworkCleanupReport {
    blocking_issues: Vec<String>,
    committed_warnings: Vec<String>,
}

fn cleanup_artwork_tokens(tokens: &mut [ArtworkRollbackToken]) -> ArtworkCleanupReport {
    let mut report = ArtworkCleanupReport::default();
    for token in tokens {
        match token {
            ArtworkRollbackToken::Flac { path, journal, _write_claim, .. } => {
                match flac_metadata_writer::remove_artwork_rollback_journal_after_committed_batch(path) {
                    Ok(Some(warning)) => report.committed_warnings.push(warning),
                    Ok(None) => {}
                    Err(err) => report.blocking_issues.push(format!(
                        "remove FLAC artwork rollback journal '{}' failed and the journal remains armed: {err}",
                        journal.display()
                    )),
                }
                if let Some(warning) = _write_claim.release_with_warning("FLAC common write lock removal after committed artwork batch") {
                    report.committed_warnings.push(warning);
                }
            }
            ArtworkRollbackToken::Dsf { commit_warning, .. } => {
                if let Some(warning) = commit_warning.take() {
                    report.committed_warnings.push(warning);
                }
            }
            ArtworkRollbackToken::FullFileBackup { backup, .. } => {
                match std::fs::remove_file(&*backup) {
                    Ok(()) => {}
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                    Err(err) => report.blocking_issues.push(format!(
                        "remove artwork backup '{}' failed: {err}",
                        backup.display()
                    )),
                }
            }
        }
    }
    report
}

enum ArtworkProjection {
    Replace(ArtworkInfo),
    Remove(lofty::picture::PictureType),
}

fn read_artwork_metadata_for_projection(path: &std::path::Path) -> Result<SourceMetadata, String> {
    use lofty::file::TaggedFileExt;

    if super::sacd::is_sacd_iso(path) {
        return read_metadata_sacd(path);
    }

    flac_metadata_writer::recover_before_read(path)?;
    let tagged = lofty::read_from_path(path)
        .map_err(|e| format!("failed to read metadata before artwork write '{}': {}", path.display(), e))?;
    Ok(source_metadata_from_tags(path, tagged.tags(), false))
}

fn read_artwork_metadata_cache(
    paths: &[std::path::PathBuf],
) -> Result<std::collections::HashMap<std::path::PathBuf, SourceMetadata>, String> {
    let mut cache = std::collections::HashMap::new();
    for path in unique_artwork_paths(paths) {
        cache.insert(path.clone(), read_artwork_metadata_for_projection(&path)?);
    }
    Ok(cache)
}

fn project_artwork_metadata_from_cache(
    paths: &[std::path::PathBuf],
    cache: &mut std::collections::HashMap<std::path::PathBuf, SourceMetadata>,
    projection: ArtworkProjection,
) -> Vec<SourceMetadata> {
    for metadata in cache.values_mut() {
        match &projection {
            ArtworkProjection::Replace(replacement) => {
                metadata.artwork.retain(|art| art.picture_type != replacement.picture_type);
                metadata.artwork.push(replacement.clone());
            }
            ArtworkProjection::Remove(picture_type) => {
                metadata.artwork.retain(|art| art.picture_type != *picture_type);
            }
        }
        sort_artwork_infos(&mut metadata.artwork);
    }
    paths
        .iter()
        .map(|path| cache.get(path).cloned().unwrap_or_default())
        .collect()
}

fn artwork_info_from_image(
    picture_type: lofty::picture::PictureType,
    mime_type: &str,
    image_bytes: &[u8],
) -> ArtworkInfo {
    let (width, height) = picture_dimensions(image_bytes);
    ArtworkInfo {
        picture_type,
        mime_type: mime_type.to_string(),
        data_size: image_bytes.len(),
        width,
        height,
    }
}

fn sort_artwork_infos(artwork: &mut Vec<ArtworkInfo>) {
    artwork.sort_by(|a, b| {
        a.picture_type
            .as_u8()
            .cmp(&b.picture_type.as_u8())
            .then_with(|| a.mime_type.cmp(&b.mime_type))
            .then_with(|| a.data_size.cmp(&b.data_size))
            .then_with(|| a.width.cmp(&b.width))
            .then_with(|| a.height.cmp(&b.height))
    });
}

fn unique_artwork_paths(paths: &[std::path::PathBuf]) -> Vec<std::path::PathBuf> {
    let mut seen = std::collections::BTreeSet::new();
    let mut unique = Vec::new();
    for path in paths {
        if seen.insert(path.clone()) {
            unique.push(path.clone());
        }
    }
    unique
}

fn mime_type_to_string(mime_type: &lofty::picture::MimeType) -> String {
    mime_type.to_string()
}

fn image_mime_type(
    path: &std::path::Path,
    data: &[u8],
) -> Result<lofty::picture::MimeType, String> {
    use lofty::picture::MimeType;
    if data.starts_with(b"\xff\xd8\xff") {
        return Ok(MimeType::Jpeg);
    }
    if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Ok(MimeType::Png);
    }
    if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        return Ok(MimeType::Gif);
    }
    if data.starts_with(b"BM") {
        return Ok(MimeType::Bmp);
    }
    if data.starts_with(b"RIFF") && data.get(8..12) == Some(b"WEBP") {
        return Ok(MimeType::Unknown("image/webp".to_string()));
    }
    match path.extension().and_then(|ext| ext.to_str()).map(|ext| ext.to_ascii_lowercase()) {
        Some(ext) if ext == "jpg" || ext == "jpeg" => Ok(MimeType::Jpeg),
        Some(ext) if ext == "png" => Ok(MimeType::Png),
        Some(ext) if ext == "gif" => Ok(MimeType::Gif),
        Some(ext) if ext == "bmp" => Ok(MimeType::Bmp),
        Some(ext) if ext == "webp" => Ok(MimeType::Unknown("image/webp".to_string())),
        _ => Err(format!(
            "unsupported artwork image type for '{}' (supported: JPEG, PNG, GIF, BMP, WebP)",
            path.display()
        )),
    }
}

/// Return true when metadata writes for this path are owned by the native FLAC
/// metadata-region writer and its sidecar `.tonepoet-meta-journal`. Callers
/// must not also create a DB metadata-journal entry pointing at a fake
/// `.tonepoet-bak`; for FLAC, the sidecar journal is the sole recovery
/// artifact. Non-FLAC formats continue to use the legacy full-file backup
/// writer until they receive native metadata-region writers.
pub fn uses_native_flac_metadata_journal(path: &std::path::Path) -> bool {
    flac_metadata_writer::is_probably_flac(path)
}

/// Recover native FLAC recovery journals before any direct Lofty read outside
/// this module. For symlinked read paths, this also checks the canonical target
/// path because native writes are refused through symlinks but a stale journal
/// may legitimately live beside the real target. Callers that cannot surface the
/// error should skip the tag read rather than parse a possibly torn metadata
/// block chain.
pub fn recover_flac_metadata_before_read(path: &std::path::Path) -> Result<(), String> {
    flac_metadata_writer::recover_before_read(path)
}

/// Recover stale native FLAC metadata journals in one directory. This is used
/// during startup and before browse-visible probes so a process crash during an
/// in-place metadata write is repaired before Lofty/ffmpeg attempt to parse
/// a possibly half-written metadata block chain.
pub fn recover_stale_flac_metadata_journals_in_dir(dir: &std::path::Path) -> Vec<String> {
    let mut messages = flac_metadata_writer::recover_metadata_journals_in_directory(dir);
    messages.extend(crate::dsf_tags::recover_stale_writes_in_directory(dir));
    messages
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_write_worker_policy_parallelizes_distinct_dsf_targets() {
        assert_eq!(metadata_write_worker_count(8, true), 1);
        assert_eq!(metadata_write_worker_count(1, false), 1);
        assert_eq!(
            metadata_write_worker_count(8, false),
            metadata_write_parallelism(8)
        );
    }

    #[cfg(unix)]
    #[test]
    fn metadata_write_worker_policy_serializes_hard_link_aliases() {
        let temp = tempfile::tempdir().expect("tempdir");
        let target = temp.path().join("target.dsf");
        let alias = temp.path().join("alias.dsf");
        std::fs::write(&target, b"fixture").expect("write target");
        std::fs::hard_link(&target, &alias).expect("create hard-link alias");

        assert!(metadata_write_targets_require_serialization([
            target.as_path(),
            alias.as_path(),
        ]));
    }

    #[test]
    fn metadata_write_worker_policy_parallelizes_distinct_file_identities() {
        let temp = tempfile::tempdir().expect("tempdir");
        let first = temp.path().join("first.dsf");
        let second = temp.path().join("second.dsf");
        std::fs::write(&first, b"first").expect("write first target");
        std::fs::write(&second, b"second").expect("write second target");

        assert!(!metadata_write_targets_require_serialization([
            first.as_path(),
            second.as_path(),
        ]));
    }


    #[cfg(unix)]
    #[test]
    fn metadata_write_worker_policy_parallelizes_distinct_non_utf8_authorities() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let first = temp.path().join(OsString::from_vec(b"one-\xff.dsf".to_vec()));
        let second = temp.path().join(OsString::from_vec(b"two-\xfe.dsf".to_vec()));
        crate::dsf_tags::write_test_dsf_fixture(&first, None).expect("write first DSF");
        crate::dsf_tags::write_test_dsf_fixture(&second, None).expect("write second DSF");

        assert!(!metadata_write_targets_require_serialization([
            first.as_path(),
            second.as_path(),
        ]));
        assert_eq!(
            metadata_write_worker_count(2, false),
            metadata_write_parallelism(2),
        );
    }

    #[cfg(unix)]
    #[test]
    fn metadata_write_worker_policy_parallelizes_non_utf8_and_literal_audio_dsf() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let non_utf = temp.path().join(OsString::from_vec(b"track-\xff.dsf".to_vec()));
        let literal = temp.path().join("audio.dsf");
        crate::dsf_tags::write_test_dsf_fixture(&non_utf, None).expect("write non-UTF DSF");
        crate::dsf_tags::write_test_dsf_fixture(&literal, None).expect("write literal DSF");

        assert!(!metadata_write_targets_require_serialization([
            non_utf.as_path(),
            literal.as_path(),
        ]));
    }

    #[cfg(unix)]
    #[test]
    fn metadata_write_worker_policy_serializes_shared_legacy_fallback_authority() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let non_utf = temp.path().join(OsString::from_vec(b"track-\xff.dsf".to_vec()));
        let literal = temp.path().join("audio.dsf");
        crate::dsf_tags::write_test_dsf_fixture(&non_utf, None).expect("write non-UTF DSF");
        crate::dsf_tags::write_test_dsf_fixture(&literal, None).expect("write literal DSF");
        std::fs::write(
            temp.path().join(".audio.dsf.tonepoet-dsf-tail.journal"),
            b"legacy authority fixture",
        )
        .expect("write legacy authority");

        assert!(metadata_write_targets_require_serialization([
            non_utf.as_path(),
            literal.as_path(),
        ]));
    }

    #[test]
    fn dst_probe_is_normalized_as_one_bit_dsd() {
        assert_eq!(normalize_tui_dsd_probe_facts("dst", 352_800, Some(8)), (2_822_400, Some(1)));
        assert_eq!(normalize_tui_dsd_probe_facts("pcm_s24le", 96_000, Some(24)), (96_000, Some(24)));
    }

    #[test]
    fn dff_tag_write_is_rejected_before_backup_or_fallback_writer() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("unsupported.dff");
        let original = b"synthetic DFF bytes";
        std::fs::write(&path, original).expect("write fixture");
        let fallback_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let calls_for_hook = std::sync::Arc::clone(&fallback_calls);

        let error = with_lofty_fallback_hook(
            temp.path(),
            move |_| {
                calls_for_hook.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            },
            || {
                write_all_tags(
                    &path,
                    &[(lofty::tag::ItemKey::TrackTitle, Some("new title".to_string()))],
                )
            },
        )
        .expect_err("DFF tag writing must be rejected before fallback");

        assert!(error.contains("DFF metadata writing is not supported"));
        assert!(error.contains("refusing before allocating a full-file rollback backup"));
        assert_eq!(fallback_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(std::fs::read(&path).expect("read unchanged DFF"), original);
        assert!(!crate::db::Database::backup_path_for(&path).exists());

        let db = crate::db::Database::open_memory().expect("memory database");
        let transactional_error = write_metadata_field_with_database(
            &db,
            &path,
            (lofty::tag::ItemKey::TrackTitle, Some("transactional title".to_string())),
        )
        .expect_err("DFF inline transaction must fail before backup allocation");
        assert!(transactional_error.contains("DFF metadata writing is not supported"));
        assert_eq!(std::fs::read(&path).expect("read unchanged DFF"), original);
        assert!(!crate::db::Database::backup_path_for(&path).exists());
        assert!(db.stale_metadata_writes().expect("read metadata journal").is_empty());
    }

    #[test]
    fn dff_artwork_mutations_are_rejected_before_full_file_backup() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("unsupported-artwork.dff");
        let original = b"synthetic DFF bytes";
        std::fs::write(&path, original).expect("write fixture");

        let write_error = write_artwork_one_file(
            &path,
            b"not-decoded-because-preflight-runs-first",
            &lofty::picture::MimeType::Png,
            "image/png",
            lofty::picture::PictureType::CoverFront,
            None,
        )
        .expect_err("DFF artwork write must be rejected before backup");
        assert!(write_error.contains("DFF metadata artwork writing is not supported"));
        assert!(!crate::db::Database::backup_path_for(&path).exists());

        let remove_error = remove_artwork_one_file(
            &path,
            lofty::picture::PictureType::CoverFront,
            None,
        )
        .expect_err("DFF artwork removal must be rejected before backup");
        assert!(remove_error.contains("DFF metadata artwork removal is not supported"));
        assert_eq!(std::fs::read(&path).expect("read unchanged DFF"), original);
        assert!(!crate::db::Database::backup_path_for(&path).exists());

        let missing_image = temp.path().join("missing.png");
        let batch_write_error = write_artwork_to_files(
            std::slice::from_ref(&path),
            &missing_image,
            lofty::picture::PictureType::CoverFront,
        )
        .expect_err("DFF artwork batch must reject before reading the image");
        assert!(batch_write_error.contains("DFF metadata artwork writing is not supported"));
        assert!(!batch_write_error.contains("read artwork"));

        let batch_remove_error = remove_artwork_from_files(
            std::slice::from_ref(&path),
            lofty::picture::PictureType::CoverFront,
        )
        .expect_err("DFF artwork removal batch must reject before metadata reads");
        assert!(batch_remove_error.contains("DFF metadata artwork removal is not supported"));
        assert_eq!(std::fs::read(&path).expect("read unchanged DFF"), original);
        assert!(!crate::db::Database::backup_path_for(&path).exists());
    }

    #[test]
    fn inline_non_flac_writer_uses_one_database_transaction_and_restores_exact_bytes() {
        let db = crate::db::Database::open_memory().expect("memory database");
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("corrupt.opus");
        let original = b"not a parseable Opus file";
        std::fs::write(&path, original).expect("write fixture");
        let change = metadata_field_change(MetadataField::Title, "New title")
            .expect("valid title change");

        let error = write_metadata_field_with_database(&db, &path, change)
            .expect_err("unreadable carrier must fail and roll back");

        assert!(error.starts_with("write failed (rolled back): failed to read"));
        assert_eq!(std::fs::read(&path).expect("read restored fixture"), original);
        assert!(!crate::db::Database::backup_path_for(&path).exists());
        assert!(db.stale_metadata_writes().expect("read journal").is_empty());
    }

    #[test]
    fn inline_dsf_writer_bypasses_the_legacy_database_transaction() {
        let db = crate::db::Database::open_memory().expect("memory database");
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("corrupt.dsf");
        let original = b"not a parseable DSF file";
        std::fs::write(&path, original).expect("write fixture");
        let change = metadata_field_change(MetadataField::Title, "New title")
            .expect("valid title change");

        let error = write_metadata_field_with_database(&db, &path, change)
            .expect_err("invalid DSF must fail through the native DSF writer");

        assert!(error.starts_with(&format!(
            "failed to save DSF ID3 tags to '{}':",
            path.display()
        )));
        assert_eq!(std::fs::read(&path).expect("read unchanged fixture"), original);
        assert!(db.stale_metadata_writes().expect("read journal").is_empty());
        assert!(std::fs::read_dir(temp.path())
            .expect("read tempdir")
            .flatten()
            .all(|entry| !entry.file_name().to_string_lossy().contains(".tonepoet-bak.txn-")));
    }

    #[test]
    fn editing_totals_removes_legacy_alias_spellings() {
        // A FLAC tagged with legacy TOTALTRACKS reads as ItemKey::TrackTotal;
        // an edit must not leave the stale spelling beside the new
        // TRACKTOTAL — alias-complete deletion.
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("legacy.flac");
        let status = std::process::Command::new("ffmpeg")
            .args([
                "-y", "-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i",
                "sine=frequency=440:sample_rate=44100:duration=0.2", "-c:a", "flac",
            ])
            .arg(&path)
            .stdin(std::process::Stdio::null())
            .status();
        let Ok(status) = status else {
            eprintln!("skipping: ffmpeg unavailable");
            return;
        };
        assert!(status.success());
        // Seed the legacy spellings raw.
        write_all_tags(
            &path,
            &[
                (
                    lofty::tag::ItemKey::Unknown("TOTALTRACKS".to_string()),
                    Some("10".to_string()),
                ),
                (
                    lofty::tag::ItemKey::Unknown("DESCRIPTION".to_string()),
                    Some("old comment".to_string()),
                ),
            ],
        )
        .expect("seed legacy tags");

        // Edit through the mapped keys (what the editor emits).
        write_all_tags(
            &path,
            &[
                (lofty::tag::ItemKey::TrackTotal, Some("12".to_string())),
                (lofty::tag::ItemKey::Comment, Some("new comment".to_string())),
            ],
        )
        .expect("edit totals");

        let reread = read_all_tags_merged(&[path]).expect("re-read");
        let totals: Vec<_> = reread
            .iter()
            .filter(|entry| {
                entry.display_key.eq_ignore_ascii_case("TRACKTOTAL")
                    || entry.display_key.eq_ignore_ascii_case("TOTALTRACKS")
            })
            .filter(|entry| entry.per_file_values.iter().any(|v| !v.is_empty()))
            .collect();
        assert_eq!(totals.len(), 1, "exactly one totals spelling: {totals:?}");
        assert!(totals[0].per_file_values.iter().any(|v| v == "12"));
        let comments: Vec<_> = reread
            .iter()
            .filter(|entry| {
                entry.display_key.eq_ignore_ascii_case("COMMENT")
                    || entry.display_key.eq_ignore_ascii_case("DESCRIPTION")
            })
            .filter(|entry| entry.per_file_values.iter().any(|v| !v.is_empty()))
            .collect();
        assert_eq!(comments.len(), 1, "exactly one comment spelling: {comments:?}");
        assert!(comments[0].per_file_values.iter().any(|v| v == "new comment"));
    }

    #[test]
    fn legacy_wrapper_writes_aligned_rows_and_skips_misaligned_per_row() {
        // CLI tags-mb shape: one image, an aligned album row plus a
        // track-dimension row expanded to n_tracks values. The save must
        // write the aligned row and skip ONLY the misaligned one — refusing
        // the whole save broke single-image CLI saves.
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("single.flac");
        let status = std::process::Command::new("ffmpeg")
            .args([
                "-y", "-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i",
                "sine=frequency=440:sample_rate=44100:duration=0.2", "-c:a", "flac",
            ])
            .arg(&path)
            .stdin(std::process::Stdio::null())
            .status();
        let Ok(status) = status else {
            eprintln!("skipping: ffmpeg unavailable");
            return;
        };
        assert!(status.success());

        let entries = vec![
            (
                lofty::tag::ItemKey::AlbumTitle,
                vec!["Aligned Album".to_string()],
                vec![String::new()],
            ),
            (
                lofty::tag::ItemKey::TrackTitle,
                vec!["Track One".to_string(), "Track Two".to_string()],
                vec![String::new(), String::new()],
            ),
        ];
        let results = apply_audio_tag_changes_with_save_blocks_and_progress(
            &[path.clone()],
            &entries,
            &[],
            &[None],
            None,
            None,
        );
        assert!(
            matches!(
                &results[0].outcome,
                crate::tui::app::MetadataEditorWriteOutcome::Saved
                    | crate::tui::app::MetadataEditorWriteOutcome::SavedWithWarnings { .. }
            ),
            "aligned rows must still save: {results:?}"
        );
        let reread = read_all_tags_merged(&[path]).expect("re-read");
        assert!(
            reread.iter().any(|entry| {
                entry.display_key.eq_ignore_ascii_case("ALBUM")
                    && entry.per_file_values.iter().any(|v| v == "Aligned Album")
            }),
            "aligned album row must be written"
        );
        assert!(
            !reread.iter().any(|entry| {
                entry.display_key.eq_ignore_ascii_case("TITLE")
                    && entry.per_file_values.iter().any(|v| !v.is_empty())
            }),
            "misaligned track row must be skipped, not sprayed"
        );
    }


    struct LoftyFallbackHookGuard;

    impl Drop for LoftyFallbackHookGuard {
        fn drop(&mut self) {
            if let Some(slot) = TEST_LOFTY_FALLBACK_HOOK.get() {
                *slot.lock().expect("Lofty fallback hook poisoned") = None;
            }
        }
    }

    fn with_lofty_fallback_hook<F, R>(
        scope: &std::path::Path,
        hook: impl Fn(&std::path::Path) + Send + Sync + 'static,
        body: F,
    ) -> R
    where
        F: FnOnce() -> R,
    {
        let _serial = flac_metadata_writer::acquire_hook_test_serialization();
        let scope = scope.to_path_buf();
        *TEST_LOFTY_FALLBACK_HOOK
            .get_or_init(|| std::sync::Mutex::new(None))
            .lock()
            .expect("Lofty fallback hook poisoned") = Some(std::sync::Arc::new(move |path: &std::path::Path| {
                if path.starts_with(&scope) {
                    hook(path);
                }
            }));
        let _guard = LoftyFallbackHookGuard;
        body()
    }

    #[test]
    fn canonical_metadata_display_key_collapses_editor_aliases() {
        assert_eq!(canonical_metadata_display_key("Year"), "DATE");
        assert_eq!(canonical_metadata_display_key("Album Artist"), "ALBUMARTIST");
        assert_eq!(canonical_metadata_display_key("MUSICBRAINZ_ALBUMID"), "MUSICBRAINZ_ALBUMID");
        assert_eq!(canonical_metadata_display_key("MusicBrainz Release Track Id"), "MUSICBRAINZ_RELEASETRACKID");
    }

    #[test]
    fn placeholder_synthesis_recognizes_legacy_alias_rows_as_canonical_fields() {
        let mut entries = vec![TagEntry {
            row_scope: RowScope::File,
            display_key: "DESCRIPTION".to_string(),
            item_key: lofty::tag::ItemKey::Unknown("DESCRIPTION".to_string()),
            value: "legacy comment".to_string(),
            original: "legacy comment".to_string(),
            is_binary: false,
            is_mixed: false,
            has_multiple_stored_values: false,
            per_file_stored_value_counts: Vec::new(),
            per_file_values: vec!["legacy comment".to_string()],
            per_file_originals: vec!["legacy comment".to_string()],
            mb_proposed_value: None,
            mb_proposed_per_file: None,
        }];

        ensure_standard_fields_present(&mut entries, 1);

        let logical_comments = entries
            .iter()
            .filter(|entry| canonical_metadata_display_key(&entry.display_key) == "COMMENT")
            .collect::<Vec<_>>();
        assert_eq!(logical_comments.len(), 1);
        assert_eq!(logical_comments[0].value, "legacy comment");
    }

    #[test]
    fn canonical_editor_fields_mark_distinct_multi_value_carriers() {
        use lofty::tag::{ItemKey, ItemValue, Tag, TagItem, TagType};

        let mut tag = Tag::new(TagType::VorbisComments);
        // push() APPENDS duplicate-key items; insert_unchecked() REPLACES
        // same-key items and would leave only the last value.
        tag.push(TagItem::new(
            ItemKey::TrackArtist,
            ItemValue::Text("Alpha".to_string()),
        ));
        tag.push(TagItem::new(
            ItemKey::TrackArtist,
            ItemValue::Text("Alpha".to_string()),
        ));
        tag.push(TagItem::new(
            ItemKey::TrackArtist,
            ItemValue::Text("Beta".to_string()),
        ));

        let field = canonical_editor_fields_from_tag(&tag)
            .into_iter()
            .find(|field| field.display_key == "ARTIST")
            .expect("canonical artist field");
        assert_eq!(field.value, "Alpha; Beta");
        assert_eq!(field.stored_value_count, 3);
    }

    #[test]
    fn merged_dsf_rows_preserve_per_file_stored_value_counts() {
        use id3::frame::Comment;
        use id3::TagLike;

        let temp = tempfile::tempdir().expect("tempdir");
        let multi_path = temp.path().join("multi.dsf");
        let scalar_path = temp.path().join("scalar.dsf");

        let mut multi_tag = id3::Tag::new();
        multi_tag.add_frame(Comment {
            lang: "eng".to_string(),
            description: "first".to_string(),
            text: "Alpha".to_string(),
        });
        multi_tag.add_frame(Comment {
            lang: "eng".to_string(),
            description: "duplicate".to_string(),
            text: "Alpha".to_string(),
        });
        multi_tag.add_frame(Comment {
            lang: "eng".to_string(),
            description: "second".to_string(),
            text: "Beta".to_string(),
        });
        let mut multi_metadata = Vec::new();
        multi_tag
            .write_to(&mut multi_metadata, id3::Version::Id3v24)
            .expect("serialize multi-value ID3 fixture");
        crate::dsf_tags::write_test_dsf_fixture(&multi_path, Some(&multi_metadata))
            .expect("write multi-value DSF fixture");

        let mut scalar_tag = id3::Tag::new();
        scalar_tag.add_frame(Comment {
            lang: "eng".to_string(),
            description: "only".to_string(),
            text: "Gamma".to_string(),
        });
        let mut scalar_metadata = Vec::new();
        scalar_tag
            .write_to(&mut scalar_metadata, id3::Version::Id3v24)
            .expect("serialize scalar ID3 fixture");
        crate::dsf_tags::write_test_dsf_fixture(&scalar_path, Some(&scalar_metadata))
            .expect("write scalar DSF fixture");

        let paths = vec![multi_path, scalar_path];
        let merged = read_all_tags_merged(&paths).expect("merge DSF tags");
        let comment = merged
            .iter()
            .find(|entry| entry.display_key == "COMMENT")
            .expect("merged COMMENT row");
        assert_eq!(
            comment.per_file_values,
            vec!["Alpha; Beta".to_string(), "Gamma".to_string()]
        );
        assert_eq!(comment.per_file_stored_value_counts, vec![3, 1]);
        assert!(comment.has_multiple_stored_values);

        let album = read_all_tags_merged_with_metadata(&paths)
            .expect("merge DSF tags with album metadata");
        let album_comment = album
            .entries
            .iter()
            .find(|entry| entry.display_key == "COMMENT")
            .expect("album COMMENT row");
        assert_eq!(album_comment.per_file_stored_value_counts, vec![3, 1]);
    }

    #[test]
    fn lofty_vorbis_alias_write_collapses_typed_and_legacy_spellings() {
        use lofty::tag::{ItemKey, ItemValue, Tag, TagItem, TagType};

        let mut tag = Tag::new(TagType::VorbisComments);
        tag.insert_unchecked(TagItem::new(
            ItemKey::Comment,
            ItemValue::Text("old canonical comment".to_string()),
        ));
        tag.insert_unchecked(TagItem::new(
            ItemKey::Unknown("DESCRIPTION".to_string()),
            ItemValue::Text("old legacy comment".to_string()),
        ));
        tag.insert_unchecked(TagItem::new(
            ItemKey::Unknown("TRACKTOTAL".to_string()),
            ItemValue::Text("10".to_string()),
        ));
        tag.insert_unchecked(TagItem::new(
            ItemKey::Unknown("TOTALTRACKS".to_string()),
            ItemValue::Text("9".to_string()),
        ));

        apply_vorbis_lofty_changes(
            &mut tag,
            &[
                (ItemKey::Comment, Some("new comment".to_string())),
                (
                    ItemKey::Unknown("TOTALTRACKS".to_string()),
                    Some("12".to_string()),
                ),
            ],
        )
        .expect("alias-complete write");

        let fields = canonical_editor_fields_from_tag(&tag);
        let comments = fields
            .iter()
            .filter(|field| field.display_key == "COMMENT")
            .map(|field| field.value.as_str())
            .collect::<Vec<_>>();
        let totals = fields
            .iter()
            .filter(|field| field.display_key == "TRACKTOTAL")
            .map(|field| field.value.as_str())
            .collect::<Vec<_>>();
        assert_eq!(comments, vec!["new comment"]);
        assert_eq!(totals, vec!["12"]);
    }

    #[test]
    fn ensure_dim_replicate_never_shrinks_existing_row_values() {
        let mut entry = TagEntry {
            row_scope: crate::tui::probe::RowScope::File,
            display_key: "TITLE".to_string(),
            item_key: lofty::tag::ItemKey::TrackTitle,
            value: "A".to_string(),
            original: "A".to_string(),
            is_binary: false,
            is_mixed: true,
            has_multiple_stored_values: false,
            per_file_stored_value_counts: Vec::new(),
            per_file_values: vec!["A".to_string(), "B".to_string(), "C".to_string()],
            per_file_originals: vec!["A".to_string(), "B".to_string(), "C".to_string()],
            mb_proposed_value: None,
            mb_proposed_per_file: None,
        };
        ensure_dim_replicate(&mut entry, 1);
        assert_eq!(entry.per_file_values, vec!["A".to_string(), "B".to_string(), "C".to_string()]);
        assert_eq!(entry.per_file_originals, vec!["A".to_string(), "B".to_string(), "C".to_string()]);
    }

    #[test]
    fn merged_with_metadata_records_single_file_read_error_without_failing() {
        let path = std::env::temp_dir().join(format!(
            "tonepoet-missing-{}-{}.flac",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));

        let merged = read_all_tags_merged_with_metadata(&[path.clone()])
            .expect("partial metadata merge should not fail for one unreadable file");

        assert!(merged.entries.is_empty());
        assert_eq!(merged.metadata.len(), 1);
        assert_eq!(merged.metadata_errors.len(), 1);
        let error = merged.metadata_errors[0]
            .as_ref()
            .expect("missing file should record a per-file metadata error");
        assert!(matches!(
            error.kind,
            MetadataReadIssueKind::FilesystemRead
                | MetadataReadIssueKind::PermissionDenied
                | MetadataReadIssueKind::TagRead
        ));
        assert!(error.reason.contains(&path.display().to_string()));
    }

    #[test]
    fn merged_with_metadata_does_not_reject_by_extension_before_lofty_read() {
        let path = std::env::temp_dir().join(format!(
            "tonepoet-unknown-container-{}-{}.txt",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, b"not an audio container").expect("write fixture");

        let merged = read_all_tags_merged_with_metadata(&[path.clone()])
            .expect("metadata merge should return a partial result");

        let issue = merged.metadata_errors[0]
            .as_ref()
            .expect("Lofty read failure should produce a typed issue");
        assert!(matches!(
            issue.kind,
            MetadataReadIssueKind::UnsupportedFormat | MetadataReadIssueKind::TagRead
        ));
        assert!(issue.reason.contains(&path.display().to_string()));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn native_flac_tag_refusal_does_not_fall_back_to_lofty_full_rewrite() {
        use lofty::tag::ItemKey;
        use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("malformed.flac");
        std::fs::write(&path, b"not a flac stream").expect("write malformed fixture");
        let fallback_called = Arc::new(AtomicBool::new(false));
        let seen = fallback_called.clone();

        let err = with_lofty_fallback_hook(
            temp.path(),
            move |_path| {
                seen.store(true, Ordering::SeqCst);
            },
            || write_all_tags(&path, &[(ItemKey::TrackTitle, Some("Title".to_string()))])
                .expect_err("native FLAC refusal must be surfaced"),
        );

        assert!(
            err.contains("native FLAC metadata-region tag write refused"),
            "error must identify the native FLAC refusal: {err}"
        );
        assert!(
            err.contains("Refusing automatic Lofty fallback"),
            "error must explain that full-file fallback was refused: {err}"
        );
        assert!(
            !fallback_called.load(Ordering::SeqCst),
            "FLAC native refusal must not call the full-file Lofty backup path"
        );
        assert!(
            !crate::db::Database::backup_path_for(&path).exists(),
            "FLAC native refusal must not create a .tonepoet-bak file"
        );
    }

    #[test]
    fn native_flac_artwork_refusal_does_not_fall_back_to_lofty_full_rewrite() {
        use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("malformed-artwork.flac");
        std::fs::write(&path, b"not a flac stream").expect("write malformed fixture");
        let fallback_called = Arc::new(AtomicBool::new(false));
        let seen = fallback_called.clone();

        let err = with_lofty_fallback_hook(
            temp.path(),
            move |_path| {
                seen.store(true, Ordering::SeqCst);
            },
            || write_artwork_one_file(
                &path,
                b"fake image payload",
                &lofty::picture::MimeType::Png,
                "image/png",
                lofty::picture::PictureType::CoverFront,
                None,
            )
            .expect_err("native FLAC artwork refusal must be surfaced"),
        );

        assert!(
            err.contains("native FLAC metadata-region artwork write refused"),
            "error must identify the native FLAC artwork refusal: {err}"
        );
        assert!(
            err.contains("Refusing automatic Lofty fallback"),
            "error must explain that full-file fallback was refused: {err}"
        );
        assert!(
            !fallback_called.load(Ordering::SeqCst),
            "FLAC artwork refusal must not call the full-file Lofty backup path"
        );
        assert!(
            !crate::db::Database::backup_path_for(&path).exists(),
            "FLAC artwork refusal must not create a .tonepoet-bak file"
        );
    }

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


    fn flac_block(block_type: u8, last: bool, data: &[u8]) -> Vec<u8> {
        assert!(data.len() <= 0x00ff_ffff);
        let mut out = Vec::with_capacity(4 + data.len());
        out.push((if last { 0x80 } else { 0 }) | block_type);
        let len = data.len() as u32;
        out.extend_from_slice(&len.to_be_bytes()[1..]);
        out.extend_from_slice(data);
        out
    }

    fn vorbis_block_body(vendor: &str, comments: &[(&str, &str)]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
        out.extend_from_slice(vendor.as_bytes());
        out.extend_from_slice(&(comments.len() as u32).to_le_bytes());
        for (name, value) in comments {
            let comment = format!("{name}={value}");
            out.extend_from_slice(&(comment.len() as u32).to_le_bytes());
            out.extend_from_slice(comment.as_bytes());
        }
        out
    }

    fn write_synthetic_flac(path: &std::path::Path, comments: &[(&str, &str)], padding: usize, audio_len: usize) -> Vec<u8> {
        let blocks = vec![
            (0, vec![0u8; 34]),
            (4, vorbis_block_body("tonepoet-test", comments)),
            (1, vec![0u8; padding]),
        ];
        write_synthetic_flac_with_blocks(path, &blocks, audio_len)
    }

    fn corrupt_synthetic_flac_metadata_header(path: &std::path::Path) {
        use std::io::{Seek, SeekFrom, Write};
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .expect("open FLAC for metadata corruption");
        file.seek(SeekFrom::Start(4)).expect("seek metadata header");
        file.write_all(&[0x7f, 0xff, 0xff, 0xff])
            .expect("write corrupt metadata header");
        file.sync_data().expect("sync corrupt metadata header");
    }

    fn write_synthetic_flac_with_blocks(
        path: &std::path::Path,
        blocks: &[(u8, Vec<u8>)],
        audio_len: usize,
    ) -> Vec<u8> {
        assert!(!blocks.is_empty(), "synthetic FLAC must have metadata blocks");
        assert_eq!(blocks[0].0, 0, "first synthetic FLAC block must be STREAMINFO");
        assert_eq!(blocks[0].1.len(), 34, "synthetic STREAMINFO must be 34 bytes");
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"fLaC");
        for (idx, (block_type, body)) in blocks.iter().enumerate() {
            bytes.extend_from_slice(&flac_block(*block_type, idx + 1 == blocks.len(), body));
        }
        let audio: Vec<u8> = (0..audio_len).map(|idx| (idx % 251) as u8).collect();
        bytes.extend_from_slice(&audio);
        std::fs::write(path, &bytes).expect("write synthetic flac");
        audio
    }

    fn synthetic_picture_block(picture_type: lofty::picture::PictureType, data: &[u8]) -> Vec<u8> {
        let mime = b"image/png";
        let mut out = Vec::new();
        out.extend_from_slice(&(picture_type.as_u8() as u32).to_be_bytes());
        out.extend_from_slice(&(mime.len() as u32).to_be_bytes());
        out.extend_from_slice(mime);
        out.extend_from_slice(&0u32.to_be_bytes()); // empty description
        out.extend_from_slice(&1u32.to_be_bytes()); // width
        out.extend_from_slice(&1u32.to_be_bytes()); // height
        out.extend_from_slice(&24u32.to_be_bytes()); // depth
        out.extend_from_slice(&0u32.to_be_bytes()); // indexed colors
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(data);
        out
    }

    fn command_available(name: &str) -> bool {
        ["-version", "--version"].iter().any(|flag| {
            std::process::Command::new(name)
                .arg(flag)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .stdin(std::process::Stdio::null())
                .status()
                .map(|status| status.success())
                .unwrap_or(false)
        })
    }

    fn generate_small_real_flac_with_ffmpeg(path: &std::path::Path) -> Result<(), String> {
        let status = std::process::Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=997:duration=1:sample_rate=44100",
                "-c:a",
                "flac",
            ])
            .arg(path)
            .stdin(std::process::Stdio::null())
            .status()
            .map_err(|err| format!("spawn ffmpeg for real FLAC fixture: {err}"))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("ffmpeg failed creating real FLAC fixture: {status}"))
        }
    }

    fn generate_large_real_flac_with_ffmpeg(path: &std::path::Path, min_bytes: u64) -> Result<(), String> {
        let mut duration_secs = std::env::var("TONEPOET_LARGE_FLAC_DURATION_SECS")
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(180);
        for _ in 0..4 {
            let input = format!("anoisesrc=d={duration_secs}:r=192000:color=white:amplitude=0.95");
            let status = std::process::Command::new("ffmpeg")
                .args([
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-y",
                    "-f",
                    "lavfi",
                    "-i",
                    &input,
                    "-sample_fmt",
                    "s32",
                    "-bits_per_raw_sample",
                    "24",
                    "-compression_level",
                    "0",
                    "-c:a",
                    "flac",
                ])
                .arg(path)
                .stdin(std::process::Stdio::null())
                .status()
                .map_err(|err| format!("spawn ffmpeg for large FLAC fixture: {err}"))?;
            if !status.success() {
                return Err(format!("ffmpeg failed creating large FLAC fixture: {status}"));
            }
            let size = std::fs::metadata(path)
                .map_err(|err| format!("stat large FLAC fixture '{}': {err}", path.display()))?
                .len();
            if size >= min_bytes {
                return Ok(());
            }
            duration_secs = duration_secs.saturating_mul(2);
        }
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        Err(format!(
            "ffmpeg large FLAC fixture remained below {min_bytes} bytes after retries: {size} bytes"
        ))
    }

    fn lofty_title(path: &std::path::Path) -> Option<String> {
        use lofty::file::TaggedFileExt;
        let tagged = lofty::read_from_path(path).ok()?;
        let tag = tagged.primary_tag()?;
        tag.get_string(&lofty::tag::ItemKey::TrackTitle)
            .map(|value| value.to_string())
    }

    fn assert_semantic_title_readback(path: &std::path::Path, expected: &str) {
        assert_eq!(
            flac_metadata_writer::test_vorbis_field_values(path, "TITLE")
                .expect("native Vorbis readback"),
            vec![expected.to_string()],
            "the native FLAC parser must read back exactly the value it wrote"
        );
        assert_eq!(
            lofty_title(path).as_deref(),
            Some(expected),
            "Lofty must read the native-written FLAC title"
        );
        if command_available("metaflac") {
            let output = std::process::Command::new("metaflac")
                .arg("--show-tag=TITLE")
                .arg(path)
                .output()
                .expect("run metaflac readback");
            assert!(output.status.success(), "metaflac readback failed");
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                stdout.lines().any(|line| line == format!("TITLE={expected}")),
                "metaflac must read the native-written FLAC title; stdout was {stdout:?}"
            );
        }
    }

    fn file_region_checksum(path: &std::path::Path, offset: u64) -> Result<(u64, u64), String> {
        use std::io::{Read, Seek, SeekFrom};
        let mut file = std::fs::File::open(path)
            .map_err(|err| format!("open checksum fixture '{}': {err}", path.display()))?;
        file.seek(SeekFrom::Start(offset))
            .map_err(|err| format!("seek checksum fixture '{}': {err}", path.display()))?;
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        let mut len = 0u64;
        let mut buf = vec![0u8; 1024 * 1024];
        loop {
            let n = file
                .read(&mut buf)
                .map_err(|err| format!("read checksum fixture '{}': {err}", path.display()))?;
            if n == 0 {
                return Ok((len, hash));
            }
            len += n as u64;
            for byte in &buf[..n] {
                hash ^= *byte as u64;
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
    }

    fn payloads_for_type(path: &std::path::Path, block_type: u8) -> Vec<Vec<u8>> {
        flac_metadata_writer::test_block_payloads(path)
            .expect("read FLAC blocks")
            .into_iter()
            .filter_map(|(ty, data)| (ty == block_type).then_some(data))
            .collect()
    }

    #[test]
    fn flac_native_tag_write_preserves_real_world_non_target_blocks_byte_identical() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("layout-matrix.flac");
        let huge_comment = "x".repeat(96 * 1024);
        let application = b"tpstapplication-state".to_vec();
        let seektable = vec![0x11u8; 18 * 2];
        let cuesheet = vec![0x22u8; 396];
        let picture = synthetic_picture_block(lofty::picture::PictureType::CoverFront, b"front-picture");
        let unknown = b"unknown-block-payload".to_vec();
        let blocks = vec![
            (0, vec![0u8; 34]),
            (1, vec![0u8; 13]),
            (2, application.clone()),
            (3, seektable.clone()),
            (4, vorbis_block_body("tonepoet-test", &[
                ("TITLE", "Old Title"),
                ("ALBUM", "Old Album"),
                ("COMMENT", huge_comment.as_str()),
            ])),
            (6, picture.clone()),
            (5, cuesheet.clone()),
            (127, unknown.clone()),
            (1, vec![0u8; 128 * 1024]),
        ];
        let audio = write_synthetic_flac_with_blocks(&path, &blocks, 512 * 1024);
        let audio_start_before = flac_metadata_writer::test_read_audio_start(&path).expect("audio start before");
        let preserved_before: Vec<(u8, Vec<Vec<u8>>)> = [2u8, 3, 5, 6, 127]
            .into_iter()
            .map(|ty| (ty, payloads_for_type(&path, ty)))
            .collect();

        write_all_tags(
            &path,
            &[
                (lofty::tag::ItemKey::TrackTitle, Some("New Title".to_string())),
                (lofty::tag::ItemKey::Genre, Some("Rock".to_string())),
            ],
        )
        .expect("native FLAC layout-matrix tag write");

        for (block_type, before) in preserved_before {
            assert_eq!(
                payloads_for_type(&path, block_type),
                before,
                "non-target FLAC metadata block type {block_type} must survive byte-identical"
            );
        }
        assert_eq!(
            flac_metadata_writer::test_vorbis_field_values(&path, "TITLE").expect("title values"),
            vec!["New Title".to_string()],
        );
        assert_eq!(
            flac_metadata_writer::test_vorbis_field_values(&path, "COMMENT").expect("comment values"),
            vec![huge_comment],
            "huge existing comments must survive unrelated tag updates",
        );
        assert_eq!(
            flac_metadata_writer::test_vorbis_field_values(&path, "GENRE").expect("genre values"),
            vec!["Rock".to_string()],
        );
        let audio_start_after = flac_metadata_writer::test_read_audio_start(&path).expect("audio start after");
        assert_eq!(audio_start_before, audio_start_after, "padded layout write must not move audio");
        let bytes = std::fs::read(&path).expect("read layout-matrix FLAC");
        assert_eq!(&bytes[audio_start_after as usize..], audio.as_slice());
        assert!(!crate::db::Database::backup_path_for(&path).exists());
        assert!(!flac_metadata_writer::test_journal_path(&path).exists());
    }

    #[test]
    fn unrelated_flac_edit_preserves_distinct_multi_value_items() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("multi-value.flac");
        let blocks = vec![
            (0, vec![0u8; 34]),
            (4, vorbis_block_body(
                "tonepoet-test",
                &[
                    ("TITLE", "Old title"),
                    ("COMMENT", "first comment"),
                    ("COMMENT", "second comment"),
                ],
            )),
            (1, vec![0u8; 4096]),
        ];
        write_synthetic_flac_with_blocks(&path, &blocks, 16 * 1024);

        write_all_tags(
            &path,
            &[(lofty::tag::ItemKey::TrackTitle, Some("New title".to_string()))],
        )
        .expect("unrelated title edit");

        assert_eq!(
            flac_metadata_writer::test_vorbis_field_values(&path, "COMMENT")
                .expect("comment values"),
            vec!["first comment".to_string(), "second comment".to_string()],
            "an unedited multi-value row must retain distinct stored items",
        );
    }

    #[test]
    fn flac_native_tag_write_inserts_vorbis_comment_when_missing() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("missing-vorbis.flac");
        let application = b"tpstno-vorbis-application".to_vec();
        let blocks = vec![
            (0, vec![0u8; 34]),
            (2, application.clone()),
            (1, vec![0u8; 64 * 1024]),
        ];
        let audio = write_synthetic_flac_with_blocks(&path, &blocks, 128 * 1024);
        let audio_start_before = flac_metadata_writer::test_read_audio_start(&path).expect("audio start before");

        write_all_tags(
            &path,
            &[(lofty::tag::ItemKey::TrackTitle, Some("Created Vorbis".to_string()))],
        )
        .expect("native FLAC tag write should create a Vorbis comment block");

        assert_eq!(payloads_for_type(&path, 2), vec![application]);
        assert_eq!(
            flac_metadata_writer::test_vorbis_field_values(&path, "TITLE").expect("title values"),
            vec!["Created Vorbis".to_string()],
        );
        let audio_start_after = flac_metadata_writer::test_read_audio_start(&path).expect("audio start after");
        assert_eq!(audio_start_before, audio_start_after, "creating Vorbis in padding must not move audio");
        let bytes = std::fs::read(&path).expect("read missing-vorbis FLAC");
        assert_eq!(&bytes[audio_start_after as usize..], audio.as_slice());
        assert!(!crate::db::Database::backup_path_for(&path).exists());
    }

    #[test]
    fn flac_native_artwork_replace_preserves_other_blocks_and_other_pictures() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("artwork-layout.flac");
        let application = b"tpstartwork-application".to_vec();
        let cuesheet = vec![0x33u8; 396];
        let old_front = synthetic_picture_block(lofty::picture::PictureType::CoverFront, b"old-front");
        let back = synthetic_picture_block(lofty::picture::PictureType::CoverBack, b"back-cover");
        let blocks = vec![
            (0, vec![0u8; 34]),
            (4, vorbis_block_body("tonepoet-test", &[("TITLE", "Old")])),
            (2, application.clone()),
            (6, old_front),
            (5, cuesheet.clone()),
            (6, back.clone()),
            (1, vec![0u8; 64 * 1024]),
        ];
        let audio = write_synthetic_flac_with_blocks(&path, &blocks, 128 * 1024);
        let audio_start_before = flac_metadata_writer::test_read_audio_start(&path).expect("audio start before");

        let rollback = write_artwork_one_file(
            &path,
            b"new-front-image",
            &lofty::picture::MimeType::Png,
            "image/png",
            lofty::picture::PictureType::CoverFront,
            None,
        )
        .expect("native artwork replacement")
        .expect("rollback token");
        let mut rollback_tokens = [rollback];
        cleanup_artwork_tokens(&mut rollback_tokens);

        assert_eq!(payloads_for_type(&path, 2), vec![application]);
        assert_eq!(payloads_for_type(&path, 5), vec![cuesheet]);
        let pictures = payloads_for_type(&path, 6);
        assert_eq!(pictures.len(), 2, "front replacement must not duplicate PICTURE blocks");
        assert!(pictures.contains(&back), "non-target back-cover PICTURE block must survive byte-identical");
        assert_eq!(
            flac_metadata_writer::test_picture_block_type_count(&path, lofty::picture::PictureType::CoverFront)
                .expect("front picture count"),
            1,
        );
        let audio_start_after = flac_metadata_writer::test_read_audio_start(&path).expect("audio start after");
        assert_eq!(audio_start_before, audio_start_after);
        let bytes = std::fs::read(&path).expect("read artwork-layout FLAC");
        assert_eq!(&bytes[audio_start_after as usize..], audio.as_slice());
        assert!(!crate::db::Database::backup_path_for(&path).exists());
    }

    #[test]
    fn flac_tag_only_write_uses_padding_without_full_backup_and_preserves_audio() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("padded.flac");
        let audio = write_synthetic_flac(&path, &[("TITLE", "Old"), ("ARTIST", "The Band")], 4096, 256 * 1024);
        let audio_start_before = flac_metadata_writer::test_read_audio_start(&path).expect("audio start before");
        let backup_checks = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let write_lens = std::sync::Arc::new(std::sync::Mutex::new(Vec::<usize>::new()));
        let backup_checks_hook = std::sync::Arc::clone(&backup_checks);
        let write_lens_hook = std::sync::Arc::clone(&write_lens);
        let expected_path_for_backup = path.clone();
        let expected_path_for_len = path.clone();

        flac_metadata_writer::test_with_fast_path_hooks(
            temp.path(),
            move |write_path| {
                assert_eq!(write_path, expected_path_for_backup.as_path());
                assert!(
                    !crate::db::Database::backup_path_for(write_path).exists(),
                    "FLAC fast path must not create a full-file backup during the write"
                );
                backup_checks_hook.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            },
            move |write_path, len| {
                assert_eq!(write_path, expected_path_for_len.as_path());
                write_lens_hook.lock().expect("write lens poisoned").push(len);
            },
            || {
                write_all_tags(
                    &path,
                    &[(lofty::tag::ItemKey::TrackTitle, Some("A Better Title".to_string()))],
                )
            },
        )
        .expect("padding-aware FLAC write");

        let backup = crate::db::Database::backup_path_for(&path);
        assert!(!backup.exists(), "FLAC fast path must not create a full-file backup");
        assert!(backup_checks.load(std::sync::atomic::Ordering::SeqCst) > 0);
        let write_lens = write_lens.lock().expect("write lens poisoned");
        assert_eq!(write_lens.len(), 1);
        assert!(
            write_lens[0] <= audio_start_before as usize - 4,
            "in-place write must be bounded to the FLAC metadata region"
        );
        assert!(!flac_metadata_writer::test_journal_path(&path).exists());
        let audio_start_after = flac_metadata_writer::test_read_audio_start(&path).expect("audio start after");
        assert_eq!(audio_start_before, audio_start_after, "padded update must not move audio");
        let bytes = std::fs::read(&path).expect("read rewritten flac");
        assert_eq!(&bytes[audio_start_after as usize..], audio.as_slice());
    }

    #[test]
    fn flac_overflow_rewrite_streams_audio_and_grows_padding_for_next_save() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("overflow.flac");
        let audio = write_synthetic_flac(&path, &[("TITLE", "Old")], 0, 2 * 1024 * 1024);
        let long_title = "x".repeat(128 * 1024);

        write_all_tags(
            &path,
            &[(lofty::tag::ItemKey::TrackTitle, Some(long_title))],
        )
        .expect("streaming FLAC rewrite");

        let backup = crate::db::Database::backup_path_for(&path);
        assert!(!backup.exists(), "FLAC overflow rewrite must not create a .tonepoet-bak backup");
        let audio_start_after = flac_metadata_writer::test_read_audio_start(&path).expect("audio start after");
        let bytes = std::fs::read(&path).expect("read rewritten flac");
        assert_eq!(&bytes[audio_start_after as usize..], audio.as_slice());

        let audio_start_before_second_save = audio_start_after;
        write_all_tags(
            &path,
            &[(lofty::tag::ItemKey::AlbumTitle, Some("Next save stays in place".to_string()))],
        )
        .expect("second save should consume grown padding");
        let audio_start_after_second_save = flac_metadata_writer::test_read_audio_start(&path).expect("second audio start");
        assert_eq!(audio_start_before_second_save, audio_start_after_second_save);
    }

    #[cfg(unix)]
    fn unix_inode(path: &std::path::Path) -> u64 {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(path).expect("stat fixture").ino()
    }

    #[cfg(unix)]
    fn unix_mode(path: &std::path::Path) -> u32 {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(path).expect("stat fixture").mode() & 0o7777
    }

    #[cfg(unix)]
    fn unix_mtime(path: &std::path::Path) -> (i64, i64) {
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::metadata(path).expect("stat fixture");
        (meta.mtime(), meta.mtime_nsec())
    }

    #[cfg(all(target_os = "linux", unix))]
    fn set_user_xattr_if_supported(path: &std::path::Path, name: &str, value: &[u8]) -> bool {
        use std::os::unix::ffi::OsStrExt;
        let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).expect("path cstring");
        let c_name = std::ffi::CString::new(name).expect("name cstring");
        unsafe {
            libc::setxattr(
                c_path.as_ptr(),
                c_name.as_ptr(),
                value.as_ptr() as *const libc::c_void,
                value.len(),
                0,
            ) == 0
        }
    }

    #[cfg(all(target_os = "linux", unix))]
    fn get_user_xattr(path: &std::path::Path, name: &str) -> Option<Vec<u8>> {
        use std::os::unix::ffi::OsStrExt;
        let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
        let c_name = std::ffi::CString::new(name).ok()?;
        let len = unsafe { libc::getxattr(c_path.as_ptr(), c_name.as_ptr(), std::ptr::null_mut(), 0) };
        if len < 0 {
            return None;
        }
        let mut value = vec![0u8; len as usize];
        let got = unsafe {
            libc::getxattr(
                c_path.as_ptr(),
                c_name.as_ptr(),
                value.as_mut_ptr() as *mut libc::c_void,
                value.len(),
            )
        };
        if got < 0 {
            return None;
        }
        value.truncate(got as usize);
        Some(value)
    }

    #[cfg(unix)]
    fn set_mtime_to_past(path: &std::path::Path) {
        use std::os::unix::ffi::OsStrExt;
        let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).expect("path cstring");
        let times = [
            libc::timespec { tv_sec: 1_700_000_000 as libc::time_t, tv_nsec: 123_000_000 },
            libc::timespec { tv_sec: 1_700_000_000 as libc::time_t, tv_nsec: 456_000_000 },
        ];
        let rc = unsafe { libc::utimensat(libc::AT_FDCWD, c_path.as_ptr(), times.as_ptr(), 0) };
        assert_eq!(rc, 0, "fixture should be able to set mtime: {}", std::io::Error::last_os_error());
    }

    #[cfg(unix)]

    #[test]
    fn flac_overflow_rewrites_are_serialized_across_parallel_workers() {
        use std::sync::{Arc, Barrier};
        use std::sync::atomic::{AtomicUsize, Ordering};

        let temp = tempfile::tempdir().expect("tempdir");
        let one = temp.path().join("overflow-one.flac");
        let two = temp.path().join("overflow-two.flac");
        let _ = write_synthetic_flac(&one, &[("TITLE", "one")], 0, 256 * 1024);
        let _ = write_synthetic_flac(&two, &[("TITLE", "two")], 0, 256 * 1024);

        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let active_for_hook = Arc::clone(&active);
        let max_for_hook = Arc::clone(&max_active);

        flac_metadata_writer::test_with_stream_rewrite_permit_hook(
            temp.path(),
            move |_path| {
                let now = active_for_hook.fetch_add(1, Ordering::SeqCst) + 1;
                loop {
                    let current = max_for_hook.load(Ordering::SeqCst);
                    if now <= current
                        || max_for_hook
                            .compare_exchange(current, now, Ordering::SeqCst, Ordering::SeqCst)
                            .is_ok()
                    {
                        break;
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(75));
                active_for_hook.fetch_sub(1, Ordering::SeqCst);
            },
            || {
                let barrier = Arc::new(Barrier::new(2));
                let one_for_thread = one.clone();
                let two_for_thread = two.clone();
                let b1 = Arc::clone(&barrier);
                let b2 = Arc::clone(&barrier);
                let h1 = std::thread::spawn(move || {
                    b1.wait();
                    write_all_tags(
                        &one_for_thread,
                        &[(lofty::tag::ItemKey::TrackTitle, Some("x".repeat(128 * 1024)))],
                    )
                });
                let h2 = std::thread::spawn(move || {
                    b2.wait();
                    write_all_tags(
                        &two_for_thread,
                        &[(lofty::tag::ItemKey::TrackTitle, Some("y".repeat(128 * 1024)))],
                    )
                });
                h1.join().expect("thread one").expect("write one");
                h2.join().expect("thread two").expect("write two");
            },
        );

        assert_eq!(
            max_active.load(Ordering::SeqCst),
            1,
            "FLAC overflow stream rewrites must be serialized even when callers run in parallel"
        );
        assert_eq!(
            flac_metadata_writer::test_vorbis_field_values(&one, "TITLE").expect("one title"),
            vec!["x".repeat(128 * 1024)]
        );
        assert_eq!(
            flac_metadata_writer::test_vorbis_field_values(&two, "TITLE").expect("two title"),
            vec!["y".repeat(128 * 1024)]
        );
    }

    #[test]
    fn flac_overflow_rewrite_preserves_mode_and_timestamps() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("overflow-metadata.flac");
        let audio = write_synthetic_flac(&path, &[("TITLE", "Old")], 0, 512 * 1024);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).expect("set mode");
        set_mtime_to_past(&path);
        let mode_before = unix_mode(&path);
        let mtime_before = unix_mtime(&path);

        write_all_tags(
            &path,
            &[(lofty::tag::ItemKey::TrackTitle, Some("x".repeat(128 * 1024)))],
        )
        .expect("overflow rewrite");

        assert_eq!(unix_mode(&path), mode_before, "overflow replacement must preserve ordinary mode bits");
        assert_eq!(unix_mtime(&path), mtime_before, "overflow replacement intentionally preserves source timestamps");
        let audio_start = flac_metadata_writer::test_read_audio_start(&path).expect("audio start");
        let bytes = std::fs::read(&path).expect("read rewritten file");
        assert_eq!(&bytes[audio_start as usize..], audio.as_slice());
    }

    #[cfg(all(target_os = "linux", unix))]
    #[test]
    fn flac_overflow_rewrite_preserves_user_xattrs_when_supported() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("overflow-xattr.flac");
        let _ = write_synthetic_flac(&path, &[("TITLE", "Old")], 0, 64 * 1024);
        let xattr_name = "user.tonepoet_overflow_test";
        let xattr_value = b"metadata-preserved";
        if !set_user_xattr_if_supported(&path, xattr_name, xattr_value) {
            eprintln!("skipping xattr preservation assertion: filesystem does not support writable user xattrs");
            return;
        }

        write_all_tags(
            &path,
            &[(lofty::tag::ItemKey::TrackTitle, Some("x".repeat(128 * 1024)))],
        )
        .expect("overflow rewrite");

        assert_eq!(
            get_user_xattr(&path, xattr_name).as_deref(),
            Some(xattr_value.as_slice()),
            "Linux user xattrs must survive FLAC overflow replacement when the filesystem supports them"
        );
    }

    #[cfg(unix)]
    #[test]
    fn flac_overflow_rewrite_acl_preservation_is_exercised_when_tools_exist() {
        if !command_available("getfacl") || !command_available("setfacl") {
            eprintln!("skipping ACL preservation assertion: getfacl/setfacl not available");
            return;
        }
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("overflow-acl.flac");
        let _ = write_synthetic_flac(&path, &[("TITLE", "Old")], 0, 64 * 1024);
        let uid = unsafe { libc::geteuid() };
        let named_user_entry = format!("u:{uid}:r--");
        let status = std::process::Command::new("setfacl")
            .args(["-m", "u::rw-,g::r--,o::---"])
            .arg("-m")
            .arg(named_user_entry)
            .arg("--")
            .arg(&path)
            .stdin(std::process::Stdio::null())
            .status()
            .expect("run setfacl");
        if !status.success() {
            eprintln!("skipping ACL preservation assertion: filesystem rejected ACL setup");
            return;
        }
        let before = std::process::Command::new("getfacl")
            .args(["--access", "--omit-header", "--"])
            .arg(&path)
            .output()
            .expect("run getfacl before");
        assert!(before.status.success());

        write_all_tags(
            &path,
            &[(lofty::tag::ItemKey::TrackTitle, Some("x".repeat(128 * 1024)))],
        )
        .expect("overflow rewrite");

        let after = std::process::Command::new("getfacl")
            .args(["--access", "--omit-header", "--"])
            .arg(&path)
            .output()
            .expect("run getfacl after");
        assert!(after.status.success());
        assert_eq!(after.stdout, before.stdout, "ACL text should survive overflow replacement when ACL tools/filesystem support it");
    }

    #[cfg(all(target_os = "linux", unix))]
    #[test]
    fn flac_overflow_rewrite_aborts_on_xattr_capture_failure_without_replacing_original() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("overflow-xattr-capture-fail.flac");
        let _ = write_synthetic_flac(&path, &[("TITLE", "Old")], 0, 64 * 1024);
        let before = std::fs::read(&path).expect("read before");
        let temp_dir = temp.path().to_path_buf();

        let result = flac_metadata_writer::test_with_xattr_capture_hook(
            temp.path(),
            |_path| Some(Err("injected xattr capture failure".to_string())),
            || write_all_tags(
                &path,
                &[(lofty::tag::ItemKey::TrackTitle, Some("x".repeat(128 * 1024)))],
            ),
        );

        let err = result.expect_err("xattr capture failure must abort overflow rewrite");
        assert!(err.contains("injected xattr capture failure"));
        assert_eq!(std::fs::read(&path).expect("read after failed rewrite"), before);
        assert!(
            std::fs::read_dir(&temp_dir)
                .expect("read tempdir")
                .flatten()
                .all(|entry| !entry.file_name().to_string_lossy().contains(".tonepoet-flac-rewrite-")),
            "xattr capture failure must not leave a rewrite temp file"
        );
    }

    #[cfg(all(target_os = "linux", unix))]
    #[test]
    fn flac_overflow_rewrite_aborts_on_xattr_restore_failure_without_replacing_original() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("overflow-xattr-restore-fail.flac");
        let _ = write_synthetic_flac(&path, &[("TITLE", "Old")], 0, 64 * 1024);
        let xattr_name = "user.tonepoet_overflow_restore_fail";
        if !set_user_xattr_if_supported(&path, xattr_name, b"must-preserve") {
            eprintln!("skipping xattr restore-failure assertion: filesystem does not support writable user xattrs");
            return;
        }
        let before = std::fs::read(&path).expect("read before");
        let temp_dir = temp.path().to_path_buf();

        let result = flac_metadata_writer::test_with_xattr_restore_hook(
            temp.path(),
            |_tmp_path, name| {
                (name.to_string_lossy() == "user.tonepoet_overflow_restore_fail")
                    .then(|| Err("injected xattr restore failure".to_string()))
            },
            || write_all_tags(
                &path,
                &[(lofty::tag::ItemKey::TrackTitle, Some("x".repeat(128 * 1024)))],
            ),
        );

        let err = result.expect_err("xattr restore failure must abort overflow rewrite before rename");
        assert!(err.contains("injected xattr restore failure"));
        assert_eq!(std::fs::read(&path).expect("read after failed rewrite"), before);
        assert_eq!(
            get_user_xattr(&path, xattr_name).as_deref(),
            Some(b"must-preserve".as_slice()),
            "failed xattr restore must leave the original xattr on the original file"
        );
        assert!(
            std::fs::read_dir(&temp_dir)
                .expect("read tempdir")
                .flatten()
                .all(|entry| !entry.file_name().to_string_lossy().contains(".tonepoet-flac-rewrite-")),
            "xattr restore failure must clean the rewrite temp file"
        );
    }

    #[cfg(unix)]
    #[test]
    fn flac_overflow_rewrite_aborts_on_acl_capture_failure_without_replacing_original() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("overflow-acl-capture-fail.flac");
        let _ = write_synthetic_flac(&path, &[("TITLE", "Old")], 0, 64 * 1024);
        let before = std::fs::read(&path).expect("read before");
        let temp_dir = temp.path().to_path_buf();

        let result = flac_metadata_writer::test_with_acl_capture_hook(
            temp.path(),
            |_path| Some(Err("injected ACL capture failure".to_string())),
            || write_all_tags(
                &path,
                &[(lofty::tag::ItemKey::TrackTitle, Some("x".repeat(128 * 1024)))],
            ),
        );

        let err = result.expect_err("ACL capture failure must abort overflow rewrite");
        assert!(err.contains("injected ACL capture failure"));
        assert_eq!(std::fs::read(&path).expect("read after failed rewrite"), before);
        assert!(
            std::fs::read_dir(&temp_dir)
                .expect("read tempdir")
                .flatten()
                .all(|entry| !entry.file_name().to_string_lossy().contains(".tonepoet-flac-rewrite-")),
            "ACL capture failure must not leave a rewrite temp file"
        );
    }

    #[cfg(unix)]
    #[test]
    fn flac_overflow_rewrite_aborts_on_acl_restore_failure_without_replacing_original() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("overflow-acl-restore-fail.flac");
        let _ = write_synthetic_flac(&path, &[("TITLE", "Old")], 0, 64 * 1024);
        let before = std::sync::Arc::new(std::fs::read(&path).expect("read before"));
        let before_for_hook = std::sync::Arc::clone(&before);
        let original_for_hook = path.clone();
        let temp_dir = temp.path().to_path_buf();
        let acl = b"user::rw-\nuser:12345:r--\ngroup::r--\nmask::r--\nother::---\n".to_vec();

        let result = flac_metadata_writer::test_with_acl_capture_hook(
            temp.path(),
            move |_path| Some(Ok(flac_metadata_writer::test_acl_snapshot_captured(acl.clone()))),
            || flac_metadata_writer::test_with_acl_restore_hook(
                temp.path(),
                move |_restore_target, _snapshot| {
                    // ACL restore runs against the rewrite TEMP before rename;
                    // the invariant under test is that the ORIGINAL is still
                    // byte-identical at that moment.
                    assert_eq!(
                        std::fs::read(&original_for_hook).expect("read original in ACL hook"),
                        before_for_hook.as_slice()
                    );
                    Some(Err("injected ACL restore failure".to_string()))
                },
                || write_all_tags(
                    &path,
                    &[(lofty::tag::ItemKey::TrackTitle, Some("x".repeat(128 * 1024)))],
                ),
            ),
        );

        let err = result.expect_err("ACL restore failure must abort overflow rewrite before rename");
        assert!(err.contains("injected ACL restore failure"));
        assert_eq!(std::fs::read(&path).expect("read after failed rewrite"), before.as_slice());
        assert!(
            std::fs::read_dir(&temp_dir)
                .expect("read tempdir")
                .flatten()
                .all(|entry| !entry.file_name().to_string_lossy().contains(".tonepoet-flac-rewrite-")),
            "ACL restore failure must clean the rewrite temp file"
        );
    }

    #[cfg(unix)]
    #[test]
    fn flac_overflow_rewrite_refuses_to_break_hardlinks() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("hardlinked.flac");
        let link = temp.path().join("hardlinked-copy.flac");
        let before_audio = write_synthetic_flac(&path, &[("TITLE", "Old")], 0, 128 * 1024);
        std::fs::hard_link(&path, &link).expect("create hardlink");
        let inode_before = unix_inode(&path);
        let before = std::fs::read(&path).expect("read before");

        let err = write_all_tags(
            &path,
            &[(lofty::tag::ItemKey::TrackTitle, Some("x".repeat(128 * 1024)))],
        )
        .expect_err("overflow rewrite must refuse hardlinked target");

        assert!(err.contains("hardlink") || err.contains("hardlinks"), "error should be actionable: {err}");
        assert_eq!(unix_inode(&path), inode_before, "refused overflow rewrite must preserve inode");
        assert_eq!(unix_inode(&link), inode_before, "hardlink identity must remain intact");
        assert_eq!(std::fs::read(&path).expect("read after"), before);
        let audio_start = flac_metadata_writer::test_read_audio_start(&path).expect("audio start after refused rewrite");
        assert_eq!(&std::fs::read(&path).expect("read bytes")[audio_start as usize..], before_audio.as_slice());
        assert!(
            std::fs::read_dir(temp.path())
                .expect("read tempdir")
                .flatten()
                .all(|entry| !entry.file_name().to_string_lossy().contains(".tonepoet-flac-rewrite-")),
            "hardlink refusal must happen before creating a rewrite temp"
        );
    }

    #[cfg(unix)]
    #[test]
    fn flac_overflow_rewrite_refuses_symlink_path_without_replacing_link() {
        use std::os::unix::fs as unix_fs;

        let temp = tempfile::tempdir().expect("tempdir");
        let target = temp.path().join("target.flac");
        let link = temp.path().join("linked.flac");
        let _ = write_synthetic_flac(&target, &[("TITLE", "Old")], 0, 128 * 1024);
        let target_before = std::fs::read(&target).expect("read target before");
        unix_fs::symlink(&target, &link).expect("create symlink");

        let err = write_all_tags(
            &link,
            &[(lofty::tag::ItemKey::TrackTitle, Some("x".repeat(128 * 1024)))],
        )
        .expect_err("overflow rewrite through symlink must be refused");

        assert!(err.contains("symlink"), "error should explain symlink refusal: {err}");
        assert!(
            std::fs::symlink_metadata(&link)
                .expect("symlink metadata")
                .file_type()
                .is_symlink(),
            "overflow refusal must not replace the symlink with a regular file"
        );
        assert_eq!(
            std::fs::read(&target).expect("read target after"),
            target_before,
            "refused symlink overflow rewrite must leave the target byte-identical"
        );
        assert!(
            std::fs::read_dir(temp.path())
                .expect("read tempdir")
                .flatten()
                .all(|entry| !entry.file_name().to_string_lossy().contains(".tonepoet-flac-rewrite-")),
            "symlink refusal must happen before creating a rewrite temp"
        );
    }

    #[cfg(unix)]
    #[test]
    fn flac_padded_fast_path_refuses_symlink_path_without_journal_or_target_mutation() {
        use std::os::unix::fs as unix_fs;

        let temp = tempfile::tempdir().expect("tempdir");
        let target = temp.path().join("fast-target.flac");
        let link = temp.path().join("fast-link.flac");
        let _ = write_synthetic_flac(&target, &[("TITLE", "Old")], 4096, 64 * 1024);
        let target_before = std::fs::read(&target).expect("read target before");
        unix_fs::symlink(&target, &link).expect("create symlink");

        let err = write_all_tags(
            &link,
            &[(lofty::tag::ItemKey::TrackTitle, Some("Fits in padding".to_string()))],
        )
        .expect_err("native padded FLAC write through symlink must be refused");

        assert!(err.contains("symlink"), "error should explain symlink refusal: {err}");
        assert!(
            std::fs::symlink_metadata(&link)
                .expect("symlink metadata")
                .file_type()
                .is_symlink(),
            "padded fast-path refusal must not replace the symlink"
        );
        assert_eq!(
            std::fs::read(&target).expect("read target after"),
            target_before,
            "refused symlink fast-path write must leave the target byte-identical"
        );
        assert!(
            !flac_metadata_writer::test_journal_path(&link).exists(),
            "refused symlink fast-path write must not create a symlink-local metadata journal"
        );
        assert!(
            !flac_metadata_writer::test_journal_path(&target).exists(),
            "refused symlink fast-path write must not create a target-local metadata journal"
        );
    }

    #[cfg(unix)]
    #[test]
    fn flac_recover_before_read_through_symlink_recovers_target_local_journal() {
        use flac_metadata_writer::TestInPlaceKillPoint;
        use std::os::unix::fs as unix_fs;

        let temp = tempfile::tempdir().expect("tempdir");
        let target = temp.path().join("real-target.flac");
        let link = temp.path().join("linked-target.flac");
        let _ = write_synthetic_flac(&target, &[("TITLE", "Original")], 4096, 64 * 1024);
        let before = std::fs::read(&target).expect("read target before");
        let journal = flac_metadata_writer::test_journal_path(&target);

        flac_metadata_writer::test_simulate_in_place_kill_point(
            &target,
            &[(lofty::tag::ItemKey::TrackTitle, Some("Interrupted".to_string()))],
            TestInPlaceKillPoint::DuringPartialMetadataOverwrite,
        )
        .expect("construct target-local stale journal");
        assert!(journal.exists(), "fixture must leave target-local recovery journal");
        unix_fs::symlink(&target, &link).expect("create symlink after interrupted write");

        recover_flac_metadata_before_read(&link)
            .expect("read guard through symlink must recover canonical target-local journal");

        assert!(!journal.exists(), "target-local journal must be removed after symlink read recovery");
        assert_eq!(
            std::fs::read(&target).expect("read target after recovery"),
            before,
            "symlink read guard must restore the mutated canonical target before parsing"
        );
        assert!(
            std::fs::symlink_metadata(&link)
                .expect("symlink metadata")
                .file_type()
                .is_symlink(),
            "read recovery through a symlink must not replace the symlink"
        );
    }

    #[cfg(unix)]
    #[test]
    fn flac_recover_before_read_through_symlink_recovers_target_local_artwork_rollback() {
        use std::os::unix::fs as unix_fs;

        let temp = tempfile::tempdir().expect("tempdir");
        let target = temp.path().join("art-real-target.flac");
        let link = temp.path().join("art-linked-target.flac");
        let _ = write_synthetic_flac(&target, &[("TITLE", "Original")], 4096, 64 * 1024);
        let before = std::fs::read(&target).expect("read target before");
        let png = tiny_png();
        let (snapshot, intended_metadata_region) = flac_metadata_writer::preview_picture_write(
            &target,
            lofty::picture::PictureType::CoverFront,
            "image/png",
            &png,
        )
        .expect("preview artwork write");
        let rollback_journal = flac_metadata_writer::begin_artwork_rollback_journal_with_intended(
            &target,
            &snapshot,
            &intended_metadata_region,
        )
        .expect("begin target-local rollback journal");
        flac_metadata_writer::write_picture_block(
            &target,
            lofty::picture::PictureType::CoverFront,
            "image/png",
            &png,
            None,
        )
        .expect("mutate target artwork after rollback journal");
        flac_metadata_writer::test_mark_artwork_rollback_journal_stale(&target)
            .expect("mark rollback journal stale");
        let rollback_path = rollback_journal.path.clone();
        drop(rollback_journal);
        assert!(rollback_path.exists(), "fixture must leave target-local artwork rollback journal");
        unix_fs::symlink(&target, &link).expect("create symlink after artwork mutation");

        recover_flac_metadata_before_read(&link)
            .expect("read guard through symlink must recover canonical target-local artwork rollback journal");

        assert!(!rollback_path.exists(), "target-local artwork rollback journal must be removed after symlink read recovery");
        assert_eq!(
            std::fs::read(&target).expect("read target after recovery"),
            before,
            "symlink read guard must restore target metadata from artwork rollback journal before parsing"
        );
        assert!(
            std::fs::symlink_metadata(&link)
                .expect("symlink metadata")
                .file_type()
                .is_symlink(),
            "artwork rollback recovery through a symlink must not replace the symlink"
        );
    }

    #[cfg(unix)]
    #[test]
    fn flac_native_artwork_write_refuses_symlink_path_without_rollback_journal() {
        use std::os::unix::fs as unix_fs;

        let temp = tempfile::tempdir().expect("tempdir");
        let target = temp.path().join("art-target.flac");
        let link = temp.path().join("art-link.flac");
        let image = temp.path().join("cover.png");
        let _ = write_synthetic_flac(&target, &[("TITLE", "Old")], 4096, 64 * 1024);
        std::fs::write(&image, tiny_png()).expect("write png fixture");
        let target_before = std::fs::read(&target).expect("read target before");
        unix_fs::symlink(&target, &link).expect("create symlink");

        let err = write_artwork_to_files(&[link.clone()], &image, lofty::picture::PictureType::CoverFront)
            .expect_err("native FLAC artwork write through symlink must be refused");

        assert!(err.contains("symlink"), "error should explain symlink refusal: {err}");
        assert!(
            std::fs::symlink_metadata(&link)
                .expect("symlink metadata")
                .file_type()
                .is_symlink(),
            "artwork refusal must not replace the symlink"
        );
        assert_eq!(
            std::fs::read(&target).expect("read target after"),
            target_before,
            "refused symlink artwork write must leave the target byte-identical"
        );
        assert!(
            !flac_metadata_writer::test_journal_path(&link).exists(),
            "refused symlink artwork write must not create a symlink-local metadata journal"
        );
        assert!(
            !link.with_file_name("art-link.flac.tonepoet-artwork-rollback").exists(),
            "refused symlink artwork write must not create a symlink-local artwork rollback journal"
        );
    }

    #[cfg(unix)]
    #[test]
    fn flac_padded_fast_path_refuses_hardlinked_file_without_journal_or_mutation() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("hardlinked-fast.flac");
        let link = temp.path().join("hardlinked-fast-copy.flac");
        let _ = write_synthetic_flac(&path, &[("TITLE", "Old")], 4096, 64 * 1024);
        std::fs::hard_link(&path, &link).expect("create hardlink");
        let inode_before = unix_inode(&path);
        let before = std::fs::read(&path).expect("read before");
        let audio_start_before = flac_metadata_writer::test_read_audio_start(&path).expect("audio start before");

        let err = write_all_tags(
            &path,
            &[(lofty::tag::ItemKey::TrackTitle, Some("Fits in padding".to_string()))],
        )
        .expect_err("native padded FLAC write on hardlinked file must be refused");

        assert!(err.contains("hardlink") || err.contains("hardlinks"), "error should explain hardlink recovery-locality refusal: {err}");
        assert_eq!(unix_inode(&path), inode_before);
        assert_eq!(unix_inode(&link), inode_before);
        assert_eq!(std::fs::read(&path).expect("read after"), before);
        assert_eq!(
            flac_metadata_writer::test_read_audio_start(&path).expect("audio start after"),
            audio_start_before,
            "refused hardlinked fast-path write must leave metadata layout unchanged"
        );
        assert!(
            !flac_metadata_writer::test_journal_path(&path).exists(),
            "refused hardlinked fast-path write must not create a path-local metadata journal"
        );
        assert!(
            !flac_metadata_writer::test_journal_path(&link).exists(),
            "refused hardlinked fast-path write must not create a hardlink-alias metadata journal"
        );
    }

    #[cfg(unix)]
    #[test]
    fn flac_native_artwork_write_refuses_hardlinked_file_without_rollback_journal() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("hardlinked-art.flac");
        let link = temp.path().join("hardlinked-art-copy.flac");
        let image = temp.path().join("cover.png");
        let _ = write_synthetic_flac(&path, &[("TITLE", "Old")], 4096, 64 * 1024);
        std::fs::hard_link(&path, &link).expect("create hardlink");
        std::fs::write(&image, tiny_png()).expect("write png fixture");
        let inode_before = unix_inode(&path);
        let before = std::fs::read(&path).expect("read before");

        let err = write_artwork_to_files(&[path.clone()], &image, lofty::picture::PictureType::CoverFront)
            .expect_err("native FLAC artwork write on hardlinked file must be refused");

        assert!(err.contains("hardlink") || err.contains("hardlinks"), "error should explain hardlink recovery-locality refusal: {err}");
        assert_eq!(unix_inode(&path), inode_before);
        assert_eq!(unix_inode(&link), inode_before);
        assert_eq!(std::fs::read(&path).expect("read after"), before);
        assert!(
            !flac_metadata_writer::test_journal_path(&path).exists(),
            "refused hardlinked artwork write must not create a path-local metadata journal"
        );
        assert!(
            !path.with_file_name("hardlinked-art.flac.tonepoet-artwork-rollback").exists(),
            "refused hardlinked artwork write must not create a path-local artwork rollback journal"
        );
    }

    #[test]
    fn flac_overflow_preservation_failure_keeps_original_and_cleans_temp() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("overflow-fail.flac");
        let before_audio = write_synthetic_flac(&path, &[("TITLE", "Old")], 0, 64 * 1024);
        let before = std::sync::Arc::new(std::fs::read(&path).expect("read before"));
        let before_for_hook = std::sync::Arc::clone(&before);
        let temp_dir = temp.path().to_path_buf();

        let result = flac_metadata_writer::test_with_stream_rewrite_before_rename_hook(
            temp.path(),
            move |original_path, tmp_path| {
                assert!(tmp_path.exists(), "test hook runs after temp rewrite and preservation");
                assert_eq!(std::fs::read(original_path).expect("read original in hook"), before_for_hook.as_slice());
                Err("injected preservation/commit failure".to_string())
            },
            || write_all_tags(
                &path,
                &[(lofty::tag::ItemKey::TrackTitle, Some("x".repeat(128 * 1024)))],
            ),
        );

        let err = result.expect_err("injected hook should fail rewrite before rename");
        assert!(err.contains("injected preservation/commit failure"));
        assert_eq!(std::fs::read(&path).expect("read after failed rewrite"), before.as_slice());
        let audio_start = flac_metadata_writer::test_read_audio_start(&path).expect("audio start after failed rewrite");
        assert_eq!(&std::fs::read(&path).expect("read bytes")[audio_start as usize..], before_audio.as_slice());
        assert!(
            std::fs::read_dir(&temp_dir)
                .expect("read tempdir")
                .flatten()
                .all(|entry| !entry.file_name().to_string_lossy().contains(".tonepoet-flac-rewrite-")),
            "failed overflow rewrite must clean its temp file"
        );
    }

    #[cfg(unix)]
    #[test]
    fn flac_overflow_rewrite_revalidates_source_before_commit() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("overflow-concurrent-change.flac");
        let _ = write_synthetic_flac(&path, &[("TITLE", "Old")], 0, 64 * 1024);
        let temp_dir = temp.path().to_path_buf();
        let externally_written = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
        let externally_written_for_hook = std::sync::Arc::clone(&externally_written);

        let result = flac_metadata_writer::test_with_stream_rewrite_before_rename_hook(
            temp.path(),
            move |original_path, tmp_path| {
                assert!(tmp_path.exists(), "test hook runs after temp rewrite is complete");
                std::fs::remove_file(original_path).expect("replace original path before commit");
                let _ = write_synthetic_flac(original_path, &[("TITLE", "External change")], 0, 32 * 1024);
                let bytes = std::fs::read(original_path).expect("read externally replaced source");
                *externally_written_for_hook.lock().expect("external bytes poisoned") = bytes;
                Ok(())
            },
            || write_all_tags(
                &path,
                &[(lofty::tag::ItemKey::TrackTitle, Some("x".repeat(128 * 1024)))],
            ),
        );

        let err = result.expect_err("overflow rewrite must refuse to overwrite concurrent external changes");
        assert!(
            err.contains("source changed during rewrite") || err.contains("revalidate FLAC"),
            "error should explain pre-commit source revalidation: {err}"
        );
        let expected = externally_written.lock().expect("external bytes poisoned").clone();
        assert!(!expected.is_empty(), "hook should have installed an external replacement");
        assert_eq!(
            std::fs::read(&path).expect("read after refused commit"),
            expected,
            "refused overflow commit must not overwrite the concurrently changed source"
        );
        assert!(
            std::fs::read_dir(&temp_dir)
                .expect("read tempdir")
                .flatten()
                .all(|entry| !entry.file_name().to_string_lossy().contains(".tonepoet-flac-rewrite-")),
            "pre-commit revalidation failure must clean the rewrite temp file"
        );
    }

    #[test]
    fn flac_metadata_journal_recovers_before_later_read_or_write() {
        use std::io::{Seek, SeekFrom, Write};

        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("recover.flac");
        let _audio = write_synthetic_flac(&path, &[("TITLE", "Original")], 4096, 4096);
        let before = std::fs::read(&path).expect("read original");
        let audio_start = flac_metadata_writer::test_read_audio_start(&path).expect("audio start") as usize;
        let journal = flac_metadata_writer::test_journal_path(&path);
        flac_metadata_writer::test_write_current_metadata_journal(&path).expect("write journal");

        {
            let mut file = std::fs::OpenOptions::new().write(true).open(&path).expect("open for corrupt");
            file.seek(SeekFrom::Start(4)).expect("seek metadata");
            file.write_all(&[0x7f, 0xff, 0xff, 0xff]).expect("corrupt metadata header");
            file.sync_all().expect("sync corrupt fixture");
        }

        let messages = recover_stale_flac_metadata_journals_in_dir(temp.path());
        assert_eq!(messages.len(), 1);
        assert!(!journal.exists());
        let after_recovery = std::fs::read(&path).expect("read after recovery");
        assert_eq!(&after_recovery[4..audio_start], &before[4..audio_start]);
        assert_eq!(&after_recovery[audio_start..], &before[audio_start..]);

        write_all_tags(
            &path,
            &[(lofty::tag::ItemKey::TrackTitle, Some("Recovered then updated".to_string()))],
        )
        .expect("write after startup-style recovery");
        assert!(!journal.exists());
    }


    #[test]
    fn active_metadata_journal_is_not_consumed_when_current_metadata_is_original() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("active-original.flac");
        let _ = write_synthetic_flac(&path, &[("TITLE", "Original")], 4096, 64 * 1024);
        let journal = flac_metadata_writer::test_journal_path(&path);

        flac_metadata_writer::test_write_active_metadata_journal(&path)
            .expect("write active metadata journal");
        assert!(journal.exists(), "active metadata journal must exist");

        let err = recover_flac_metadata_before_read(&path)
            .expect_err("read guard must not consume a live writer's journal");
        assert!(
            err.contains("write appears to be in progress"),
            "active-journal error should be transient and actionable: {err}"
        );
        assert!(journal.exists(), "read recovery must not remove an active journal");

        let write_err = write_all_tags(
            &path,
            &[(lofty::tag::ItemKey::TrackTitle, Some("Competing write".to_string()))],
        )
        .expect_err("competing native write must not consume a live writer's journal");
        assert!(
            write_err.contains("write appears to be in progress"),
            "competing write should report the active metadata journal: {write_err}"
        );
        assert!(journal.exists(), "competing write must leave the active journal armed");
    }

    #[test]
    fn metadata_journal_claim_does_not_overwrite_active_owner() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("claim-active.flac");
        let _ = write_synthetic_flac(&path, &[("TITLE", "Original")], 4096, 64 * 1024);
        let journal = flac_metadata_writer::test_journal_path(&path);

        flac_metadata_writer::test_write_active_metadata_journal(&path)
            .expect("first writer claims metadata journal");
        let first_journal = std::fs::read(&journal).expect("read first journal claim");

        let err = flac_metadata_writer::test_write_active_metadata_journal(&path)
            .expect_err("second writer must not overwrite an active metadata journal claim");
        assert!(
            err.contains("owned by a live writer") || err.contains("write appears to be in progress"),
            "second claim should fail as an active-writer conflict: {err}"
        );
        assert_eq!(
            std::fs::read(&journal).expect("read journal after rejected claim"),
            first_journal,
            "no-clobber journal acquisition must leave the first writer's journal bytes intact"
        );
    }

    #[test]
    fn active_common_write_lock_blocks_reads_and_competing_native_writes() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("common-active.flac");
        let _ = write_synthetic_flac(&path, &[("TITLE", "Original")], 4096, 64 * 1024);
        let lock_path = flac_metadata_writer::test_write_lock_path(&path);
        let claim = flac_metadata_writer::acquire_native_write_claim(&path, "test active claim")
            .expect("acquire common write claim");
        assert!(lock_path.exists(), "common write lock should exist while claim is held");

        let read_err = recover_flac_metadata_before_read(&path)
            .expect_err("read guard must not parse while a common write claim is active");
        assert!(
            read_err.contains("write appears to be in progress") || read_err.contains("write lock"),
            "read guard should report the active common lock: {read_err}"
        );

        let path_for_thread = path.clone();
        let competing = std::thread::spawn(move || {
            write_all_tags(
                &path_for_thread,
                &[(lofty::tag::ItemKey::TrackTitle, Some("Competing".to_string()))],
            )
        })
        .join()
        .expect("competing writer thread should not panic");
        let write_err = competing.expect_err("competing writer must not enter native FLAC writer");
        assert!(
            write_err.contains("already in progress") || write_err.contains("write lock"),
            "competing writer should report common write-lock contention: {write_err}"
        );
        drop(claim);
        assert!(!lock_path.exists(), "common write lock should be removed when claim drops");
    }

    #[test]
    fn stale_common_write_lock_is_recovered_before_native_write() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("common-stale.flac");
        let _ = write_synthetic_flac(&path, &[("TITLE", "Original")], 4096, 64 * 1024);
        let lock_path = flac_metadata_writer::test_write_lock_path(&path);
        flac_metadata_writer::test_write_stale_common_write_lock(&path)
            .expect("write stale common lock");
        assert!(lock_path.exists(), "stale common lock should exist before retry");

        write_all_tags(
            &path,
            &[(lofty::tag::ItemKey::TrackTitle, Some("Recovered".to_string()))],
        )
        .expect("native write should recover stale common lock and proceed");
        assert!(!lock_path.exists(), "successful writer should remove common write lock");
        assert_eq!(
            flac_metadata_writer::test_vorbis_field_values(&path, "TITLE").expect("title values"),
            vec!["Recovered".to_string()]
        );
    }


    #[test]
    fn common_write_lock_parent_sync_failure_is_reported_after_committed_tag_write() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("common-cleanup-warning.flac");
        let _ = write_synthetic_flac(&path, &[("TITLE", "Original")], 4096, 64 * 1024);
        let lock_path = flac_metadata_writer::test_write_lock_path(&path);

        let report = flac_metadata_writer::test_with_parent_dir_sync_hook(
            temp.path(),
            |_parent, context| {
                (context == "FLAC common write lock removal after tag write")
                    .then(|| Err("injected directory fsync failure after common lock removal".to_string()))
            },
            || {
                write_all_tags_with_cancel_report(
                    &path,
                    &[(lofty::tag::ItemKey::TrackTitle, Some("Committed".to_string()))],
                    None,
                    None,
                )
            },
        )
        .expect("tag write should be committed with a cleanup durability warning");

        assert!(
            report
                .durability_warnings
                .iter()
                .any(|warning| warning.contains("common write lock") || warning.contains("directory fsync failed")),
            "common lock cleanup durability warning should be surfaced: {:?}",
            report.durability_warnings
        );
        assert!(!lock_path.exists(), "common lock file should be removed before the parent-sync warning is reported");
        assert_eq!(
            flac_metadata_writer::test_vorbis_field_values(&path, "TITLE").expect("title values"),
            vec!["Committed".to_string()]
        );
    }

    #[test]
    fn metadata_journal_claim_recovers_stale_owner_then_retries_once() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("claim-stale.flac");
        let _ = write_synthetic_flac(&path, &[("TITLE", "Original")], 4096, 64 * 1024);
        let journal = flac_metadata_writer::test_journal_path(&path);

        flac_metadata_writer::test_write_current_metadata_journal(&path)
            .expect("write stale metadata journal");
        let stale_journal = std::fs::read(&journal).expect("read stale journal");

        flac_metadata_writer::test_write_active_metadata_journal(&path)
            .expect("claim should recover stale journal and retry once");
        assert!(journal.exists(), "new active journal should be claimed after stale cleanup");
        assert_ne!(
            std::fs::read(&journal).expect("read retried journal"),
            stale_journal,
            "retried claim should install the new writer's journal, not overwrite blindly"
        );
    }

    #[test]
    fn active_metadata_journal_is_not_restored_over_parseable_torn_metadata() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("active-torn.flac");
        let _ = write_synthetic_flac(&path, &[("TITLE", "Original")], 4096, 64 * 1024);
        let original_audio_start = flac_metadata_writer::test_read_audio_start(&path)
            .expect("original audio start");
        let journal = flac_metadata_writer::test_journal_path(&path);

        flac_metadata_writer::test_write_active_metadata_journal(&path)
            .expect("write active metadata journal");
        {
            use std::io::{Seek, SeekFrom, Write};
            let mut file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .expect("open active torn fixture");
            file.seek(SeekFrom::Start(4)).expect("seek metadata");
            file.write_all(&[0x80, 0x00, 0x00, 0x22])
                .expect("write parseable wrong-offset metadata header");
            file.sync_data().expect("sync torn fixture");
        }
        let torn_audio_start = flac_metadata_writer::test_read_audio_start(&path)
            .expect("active torn metadata should remain parseable");
        assert_ne!(torn_audio_start, original_audio_start, "fixture must be parseable-but-torn");

        let err = recover_flac_metadata_before_read(&path)
            .expect_err("read guard must not restore concurrently with a live writer");
        assert!(err.contains("write appears to be in progress"));
        assert!(journal.exists(), "active journal must remain armed");
        assert_eq!(
            flac_metadata_writer::test_read_audio_start(&path)
                .expect("torn metadata should still parse after skipped active recovery"),
            torn_audio_start,
            "active-owner recovery must not restore concurrently"
        );
    }

    #[test]
    fn metadata_journal_pid_reuse_owner_mismatch_recovers_as_stale() {
        use std::io::{Seek, SeekFrom, Write};

        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("pid-reuse-stale.flac");
        let _audio = write_synthetic_flac(&path, &[("TITLE", "Original")], 4096, 64 * 1024);
        let before = std::fs::read(&path).expect("read original");
        let journal = flac_metadata_writer::test_journal_path(&path);

        flac_metadata_writer::test_write_metadata_journal_with_pid_reuse_owner(&path)
            .expect("write metadata journal with PID-reuse-like owner mismatch");
        {
            let mut file = std::fs::OpenOptions::new().write(true).open(&path).expect("open corrupt fixture");
            file.seek(SeekFrom::Start(4)).expect("seek metadata");
            file.write_all(&[0x7f, 0xff, 0xff, 0xff]).expect("corrupt metadata header");
            file.sync_all().expect("sync corrupt fixture");
        }

        recover_flac_metadata_before_read(&path)
            .expect("mismatched owner identity must not suppress stale metadata recovery");
        assert!(!journal.exists(), "stale journal should be consumed after recovery");
        assert_eq!(
            std::fs::read(&path).expect("read recovered fixture"),
            before,
            "PID-reuse-style stale recovery must restore original bytes"
        );
    }

    #[test]
    fn flac_in_place_kill_points_recover_original_bytes_before_reads_or_writes() {
        use flac_metadata_writer::TestInPlaceKillPoint;

        for point in [
            TestInPlaceKillPoint::AfterJournalCreate,
            TestInPlaceKillPoint::DuringPartialMetadataOverwrite,
            TestInPlaceKillPoint::AfterSyncedOverwriteBeforeJournalRemoval,
        ] {
            let temp = tempfile::tempdir().expect("tempdir");
            let path = temp.path().join(format!("kill-{point:?}.flac"));
            let _audio = write_synthetic_flac(&path, &[("TITLE", "Original")], 4096, 64 * 1024);
            let before = std::fs::read(&path).expect("read original fixture");
            let journal = flac_metadata_writer::test_journal_path(&path);

            flac_metadata_writer::test_simulate_in_place_kill_point(
                &path,
                &[(lofty::tag::ItemKey::TrackTitle, Some("Interrupted".to_string()))],
                point,
            )
            .expect("construct in-place kill point");
            assert!(journal.exists(), "kill point must leave a recovery journal");

            let messages = recover_stale_flac_metadata_journals_in_dir(temp.path());
            assert_eq!(messages.len(), 1);
            assert!(!journal.exists(), "recovery must remove the journal after restore/commit resolution");
            let after_recovery = std::fs::read(&path).expect("read recovered fixture");
            match point {
                TestInPlaceKillPoint::AfterSyncedOverwriteBeforeJournalRemoval => {
                    assert_ne!(after_recovery, before, "synced metadata overwrite is a committed state");
                    assert_eq!(
                        flac_metadata_writer::test_vorbis_field_values(&path, "TITLE")
                            .expect("title after committed kill point"),
                        vec!["Interrupted".to_string()]
                    );
                }
                _ => {
                    assert_eq!(after_recovery, before, "{point:?} recovery must restore the pre-write FLAC bytes");
                }
            }

            write_all_tags(
                &path,
                &[(lofty::tag::ItemKey::TrackTitle, Some("Post-recovery write".to_string()))],
            )
            .expect("write after kill-point recovery");
        }
    }

    #[test]
    fn flac_metadata_journal_recovers_parseable_wrong_audio_offset_torn_write() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("wrong-offset.flac");
        let _audio = write_synthetic_flac(&path, &[("TITLE", "Original")], 4096, 64 * 1024);
        let before = std::fs::read(&path).expect("read original fixture");
        let audio_start_before = flac_metadata_writer::test_read_audio_start(&path).expect("audio start before");
        let journal = flac_metadata_writer::test_journal_path(&path);

        flac_metadata_writer::test_simulate_parseable_wrong_audio_offset_with_journal(
            &path,
            &[(lofty::tag::ItemKey::TrackTitle, Some("Interrupted".to_string()))],
        )
        .expect("construct parseable wrong-offset crash state");
        assert!(journal.exists(), "wrong-offset crash state must leave a recovery journal");
        let wrong_audio_start = flac_metadata_writer::test_read_audio_start(&path)
            .expect("wrong-offset metadata should remain syntactically parseable");
        assert_ne!(wrong_audio_start, audio_start_before, "fixture must parse with the wrong audio offset");

        let messages = recover_stale_flac_metadata_journals_in_dir(temp.path());
        assert_eq!(messages.len(), 1);
        assert!(!journal.exists(), "wrong-offset recovery must remove the journal");
        let after_recovery = std::fs::read(&path).expect("read recovered fixture");
        assert_eq!(after_recovery, before, "parseable wrong-offset recovery must restore original FLAC bytes");

        write_all_tags(
            &path,
            &[(lofty::tag::ItemKey::TrackTitle, Some("Post wrong-offset recovery".to_string()))],
        )
        .expect("write after wrong-offset recovery");
    }

    #[test]
    fn flac_real_journal_recovers_parseable_wrong_audio_offset_before_lofty_read() {
        if !command_available("ffmpeg") {
            eprintln!("skipping real wrong-offset recovery test because ffmpeg is unavailable");
            return;
        }
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("real-wrong-offset.flac");
        generate_small_real_flac_with_ffmpeg(&path).expect("generate real FLAC fixture");
        write_all_tags(
            &path,
            &[(lofty::tag::ItemKey::TrackTitle, Some("Original after padding".to_string()))],
        )
        .expect("seed real FLAC padding");
        assert_semantic_title_readback(&path, "Original after padding");
        let before = std::fs::read(&path).expect("read real original fixture");
        let audio_start_before = flac_metadata_writer::test_read_audio_start(&path).expect("real audio start before");
        let journal = flac_metadata_writer::test_journal_path(&path);

        flac_metadata_writer::test_simulate_parseable_wrong_audio_offset_with_journal(
            &path,
            &[(lofty::tag::ItemKey::TrackTitle, Some("Interrupted real".to_string()))],
        )
        .expect("construct real parseable wrong-offset crash state");
        let wrong_audio_start = flac_metadata_writer::test_read_audio_start(&path)
            .expect("real wrong-offset metadata should parse");
        assert_ne!(wrong_audio_start, audio_start_before);

        let messages = recover_stale_flac_metadata_journals_in_dir(temp.path());
        assert_eq!(messages.len(), 1);
        assert!(!journal.exists());
        let after_recovery = std::fs::read(&path).expect("read recovered real fixture");
        assert_eq!(after_recovery, before, "real wrong-offset recovery must restore original bytes before semantic reads");
        assert_semantic_title_readback(&path, "Original after padding");
    }

    #[test]
    fn sort_paths_by_track_recovers_stale_flac_journal_before_lofty_attempt() {
        let temp = tempfile::tempdir().expect("tempdir");
        let stale_path = temp.path().join("02-stale.flac");
        let clean_path = temp.path().join("01-clean.flac");
        let _ = write_synthetic_flac(&stale_path, &[("TITLE", "Original")], 4096, 64 * 1024);
        let _ = write_synthetic_flac(&clean_path, &[("TITLE", "Clean")], 4096, 64 * 1024);
        let stale_before = std::sync::Arc::new(std::fs::read(&stale_path).expect("read stale original"));
        let journal = flac_metadata_writer::test_journal_path(&stale_path);

        flac_metadata_writer::test_simulate_parseable_wrong_audio_offset_with_journal(
            &stale_path,
            &[(lofty::tag::ItemKey::TrackTitle, Some("Interrupted".to_string()))],
        )
        .expect("construct stale journal before sort");
        assert!(journal.exists(), "fixture must leave a FLAC metadata journal");

        let seen_stale_before_lofty = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let hook_seen = std::sync::Arc::clone(&seen_stale_before_lofty);
        let hook_path = stale_path.clone();
        let hook_journal = journal.clone();
        let hook_before = std::sync::Arc::clone(&stale_before);

        let mut paths = vec![stale_path.clone(), clean_path];
        with_sort_after_recover_before_lofty_hook(
            temp.path(),
            move |path| {
                if path == hook_path.as_path() {
                    assert!(
                        !hook_journal.exists(),
                        "sort_paths_by_track must remove the stale journal before Lofty is attempted"
                    );
                    let bytes = std::fs::read(path).expect("read recovered sort fixture in hook");
                    assert_eq!(
                        bytes.as_slice(),
                        hook_before.as_slice(),
                        "sort_paths_by_track must restore original FLAC bytes before Lofty is attempted"
                    );
                    hook_seen.store(true, std::sync::atomic::Ordering::SeqCst);
                }
            },
            || sort_paths_by_track(&mut paths),
        );

        assert!(
            seen_stale_before_lofty.load(std::sync::atomic::Ordering::SeqCst),
            "test hook must observe the stale path between recovery and Lofty read"
        );
        assert!(!journal.exists());
        let stale_after = std::fs::read(&stale_path).expect("read recovered stale fixture");
        assert_eq!(stale_after.as_slice(), stale_before.as_slice());
    }

    #[test]
    fn sort_paths_by_track_recovers_real_flac_journal_before_tag_ordering() {
        if !command_available("ffmpeg") {
            eprintln!("skipping real sort recovery test because ffmpeg is unavailable");
            return;
        }
        let temp = tempfile::tempdir().expect("tempdir");
        let track_one = temp.path().join("zz-track-one.flac");
        let track_two = temp.path().join("aa-track-two.flac");
        generate_small_real_flac_with_ffmpeg(&track_one).expect("generate track one real FLAC");
        generate_small_real_flac_with_ffmpeg(&track_two).expect("generate track two real FLAC");

        write_all_tags(
            &track_one,
            &[
                (lofty::tag::ItemKey::TrackTitle, Some("Track One".to_string())),
                (lofty::tag::ItemKey::TrackNumber, Some("1".to_string())),
            ],
        )
        .expect("seed track one tags and padding");
        write_all_tags(
            &track_two,
            &[
                (lofty::tag::ItemKey::TrackTitle, Some("Track Two".to_string())),
                (lofty::tag::ItemKey::TrackNumber, Some("2".to_string())),
            ],
        )
        .expect("seed track two tags and padding");

        flac_metadata_writer::test_simulate_parseable_wrong_audio_offset_with_journal(
            &track_one,
            &[(lofty::tag::ItemKey::TrackTitle, Some("Interrupted Track One".to_string()))],
        )
        .expect("construct stale real-FLAC journal before sort");
        let journal = flac_metadata_writer::test_journal_path(&track_one);
        assert!(journal.exists());

        let mut paths = vec![track_two.clone(), track_one.clone()];
        sort_paths_by_track(&mut paths);

        assert!(!journal.exists(), "sort must recover stale FLAC journals before tag reads");
        assert_eq!(
            paths,
            vec![track_one.clone(), track_two.clone()],
            "after recovery, Lofty tag ordering must win over fallback filename ordering"
        );
        assert_semantic_title_readback(&track_one, "Track One");
    }

    #[test]
    fn flac_parent_dir_sync_failure_after_overflow_commit_is_success_with_warning() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("parent-sync-failure.flac");
        let _ = write_synthetic_flac(&path, &[("TITLE", "Old")], 0, 256 * 1024);

        let report = flac_metadata_writer::test_with_parent_dir_sync_hook(
            temp.path(),
            |_parent, context| {
                if context.contains("overflow rewrite commit") {
                    Some(Err("simulated durable directory sync failure".to_string()))
                } else {
                    Some(Ok(()))
                }
            },
            || write_all_tags_with_cancel_report(
                &path,
                &[(lofty::tag::ItemKey::TrackTitle, Some("x".repeat(128 * 1024)))],
                None,
                None,
            )
            .expect("post-rename parent-directory fsync failure must not reclassify committed audio mutation as failed"),
        );

        assert!(
            report.durability_warnings.iter().any(|warning|
                warning.contains("FLAC overflow rewrite commit")
                    && warning.contains("parent-directory fsync failed")
            ),
            "committed overflow rewrite must surface a durability warning: {:?}",
            report.durability_warnings
        );
        assert!(
            std::fs::read_dir(temp.path())
                .expect("read tempdir")
                .flatten()
                .all(|entry| !entry.file_name().to_string_lossy().contains(".tonepoet-flac-rewrite-")),
            "same-process parent-sync warning must not leave a stale rewrite temp"
        );
        assert_semantic_title_readback(&path, &"x".repeat(128 * 1024));
    }

    #[test]
    fn flac_parent_dir_sync_failure_after_journal_removal_is_success_with_warning() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("journal-removal-sync-failure.flac");
        let _ = write_synthetic_flac(&path, &[("TITLE", "Old")], 4096, 64 * 1024);

        let report = flac_metadata_writer::test_with_parent_dir_sync_hook(
            temp.path(),
            |_parent, context| {
                if context.contains("metadata journal removal") {
                    Some(Err("simulated journal-removal directory sync failure".to_string()))
                } else {
                    Some(Ok(()))
                }
            },
            || write_all_tags_with_cancel_report(
                &path,
                &[(lofty::tag::ItemKey::TrackTitle, Some("Committed in-place".to_string()))],
                None,
                None,
            )
            .expect("post-commit journal-removal parent fsync failure must be a warning, not failed save"),
        );

        assert!(
            report.durability_warnings.iter().any(|warning|
                warning.contains("FLAC metadata journal removal")
                    && warning.contains("parent-directory fsync failed")
            ),
            "in-place commit must surface a journal-removal durability warning: {:?}",
            report.durability_warnings
        );
        assert!(!flac_metadata_writer::test_journal_path(&path).exists());
        assert_semantic_title_readback(&path, "Committed in-place");
    }

    #[test]
    fn flac_metadata_journal_cleanup_failure_after_commit_is_success_with_warning() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("journal-cleanup-failure.flac");
        let _ = write_synthetic_flac(&path, &[("TITLE", "Old")], 4096, 64 * 1024);

        let report = flac_metadata_writer::test_with_metadata_journal_remove_hook(
            temp.path(),
            |_journal| Some(Err("simulated journal cleanup failure".to_string())),
            || write_all_tags_with_cancel_report(
                &path,
                &[(lofty::tag::ItemKey::TrackTitle, Some("Committed despite cleanup warning".to_string()))],
                None,
                None,
            )
            .expect("post-commit metadata-journal cleanup failure must be a warning, not failed save"),
        );

        assert!(
            report.durability_warnings.iter().any(|warning|
                warning.contains("cleanup of recovery journal")
                    && warning.contains("simulated journal cleanup failure")
            ),
            "committed in-place write must surface cleanup failure as a warning: {:?}",
            report.durability_warnings
        );
        assert_semantic_title_readback(&path, "Committed despite cleanup warning");
        assert!(
            flac_metadata_writer::test_journal_path(&path).exists(),
            "injected cleanup failure intentionally leaves the recovery journal for later/remedial cleanup"
        );
    }

    #[test]
    fn metadata_write_cancel_operation_scopes_share_cancellation_but_isolate_observations() {
        let request = MetadataWriteCancelFlag::new();
        let first = request.operation_scope();
        let second = request.operation_scope();

        request.cancel();
        assert!(first.check("inside first operation").is_err());

        assert!(request.is_cancelled());
        assert!(second.is_cancelled());
        assert_eq!(first.observation_count(), 1);
        assert_eq!(second.observation_count(), 0);
        assert_eq!(request.observation_count(), 0);

        assert!(second.check("inside second operation").is_err());
        assert_eq!(first.observation_count(), 1);
        assert_eq!(second.observation_count(), 1);
        assert_eq!(request.observation_count(), 0);
    }

    #[test]
    fn flac_overflow_rewrite_cancellation_before_commit_preserves_original_and_cleans_temp() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("cancel-before-commit.flac");
        let _ = write_synthetic_flac(&path, &[("TITLE", "Old")], 0, 512 * 1024);
        let before = std::fs::read(&path).expect("read before");
        let cancel = MetadataWriteCancelFlag::new();
        let cancel_for_hook = cancel.clone();

        let err = flac_metadata_writer::test_with_stream_rewrite_before_rename_hook(
            temp.path(),
            move |_path, _tmp_path| {
                cancel_for_hook.cancel();
                Ok(())
            },
            || write_all_tags_with_cancel(
                &path,
                &[(lofty::tag::ItemKey::TrackTitle, Some("x".repeat(128 * 1024)))],
                Some(&cancel),
            )
            .expect_err("cancellation before commit must stop the overflow rewrite"),
        );

        assert!(err.contains("metadata save cancelled before committing FLAC overflow rewrite"), "unexpected error: {err}");
        assert_eq!(std::fs::read(&path).expect("read after"), before, "cancel before rename must leave original bytes intact");
        assert!(
            std::fs::read_dir(temp.path())
                .expect("read tempdir")
                .flatten()
                .all(|entry| !entry.file_name().to_string_lossy().contains(".tonepoet-flac-rewrite-")),
            "cancel before rename must clean the rewrite temp in the same process"
        );
    }

    #[test]
    fn flac_overflow_rewrite_cancellation_between_stream_chunks_preserves_original() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("cancel-between-chunks.flac");
        let _ = write_synthetic_flac(&path, &[("TITLE", "Old")], 0, 3 * 1024 * 1024);
        let before = std::fs::read(&path).expect("read before");
        let cancel = MetadataWriteCancelFlag::new();
        let cancel_for_hook = cancel.clone();
        let fired = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let fired_hook = std::sync::Arc::clone(&fired);

        let err = flac_metadata_writer::test_with_stream_copy_chunk_hook(
            temp.path(),
            move |_copied| {
                if !fired_hook.swap(true, std::sync::atomic::Ordering::SeqCst) {
                    cancel_for_hook.cancel();
                }
            },
            || write_all_tags_with_cancel(
                &path,
                &[(lofty::tag::ItemKey::TrackTitle, Some("x".repeat(128 * 1024)))],
                Some(&cancel),
            )
            .expect_err("cancellation between stream-copy chunks must stop rewrite before commit"),
        );

        assert!(err.contains("metadata save cancelled during FLAC overflow stream copy"), "unexpected error: {err}");
        assert_eq!(std::fs::read(&path).expect("read after"), before, "chunk-level cancellation must leave original bytes intact");
        assert!(
            std::fs::read_dir(temp.path())
                .expect("read tempdir")
                .flatten()
                .all(|entry| !entry.file_name().to_string_lossy().contains(".tonepoet-flac-rewrite-")),
            "chunk-level cancellation must clean the rewrite temp in the same process"
        );
    }

    #[test]
    fn non_flac_cancellation_before_fallback_does_not_create_backup() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("cancel-before-fallback.mp3");
        std::fs::write(&path, b"not really an mp3; cancellation must fire before Lofty").expect("write fixture");
        let cancel = MetadataWriteCancelFlag::new();
        cancel.cancel();

        let err = write_all_tags_with_cancel(
            &path,
            &[(lofty::tag::ItemKey::TrackTitle, Some("New".to_string()))],
            Some(&cancel),
        )
        .expect_err("pre-cancelled generic write must not start fallback");

        assert!(err.contains("metadata save cancelled before starting file"), "unexpected error: {err}");
        assert!(!crate::db::Database::backup_path_for(&path).exists(), "cancel before fallback must not create .tonepoet-bak");
    }

    #[test]
    fn flac_stream_rewrite_kill_point_cleans_temp_without_touching_original() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("stream-kill.flac");
        let _audio = write_synthetic_flac(&path, &[("TITLE", "Original")], 0, 512 * 1024);
        let before = std::fs::read(&path).expect("read original fixture");
        let tmp = flac_metadata_writer::test_create_stale_stream_rewrite_tmp(&path)
            .expect("construct stale stream rewrite temp");
        assert!(tmp.exists());

        let messages = recover_stale_flac_metadata_journals_in_dir(temp.path());
        assert_eq!(messages.len(), 1);
        assert!(!tmp.exists(), "startup recovery must clean interrupted stream rewrite temps");
        let after = std::fs::read(&path).expect("read after cleanup");
        assert_eq!(after, before, "interrupted stream rewrite before rename must leave original untouched");
    }

    #[test]
    fn flac_stream_rewrite_commit_point_leaves_valid_file_and_preserves_audio() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("stream-commit.flac");
        let audio = write_synthetic_flac(&path, &[("TITLE", "Original")], 0, 512 * 1024);

        flac_metadata_writer::test_force_stream_rewrite_commit(
            &path,
            &[(lofty::tag::ItemKey::TrackTitle, Some("Committed rewrite".to_string()))],
        )
        .expect("force stream rewrite commit");

        let messages = recover_stale_flac_metadata_journals_in_dir(temp.path());
        assert!(messages.is_empty(), "rename-committed rewrite should not need journal recovery");
        assert!(!flac_metadata_writer::test_journal_path(&path).exists());
        let audio_start_after = flac_metadata_writer::test_read_audio_start(&path).expect("audio start after");
        let bytes = std::fs::read(&path).expect("read committed rewrite");
        assert_eq!(&bytes[audio_start_after as usize..], audio.as_slice());
    }

    #[test]
    fn flac_real_fixture_fast_path_is_semantically_readable_and_bounded() {
        if !command_available("ffmpeg") {
            eprintln!("skipping real FLAC acceptance test because ffmpeg is unavailable");
            return;
        }
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("real-acceptance.flac");
        generate_small_real_flac_with_ffmpeg(&path).expect("generate real FLAC fixture");

        let padding_seed = "x".repeat(128 * 1024);
        write_all_tags(
            &path,
            &[(lofty::tag::ItemKey::TrackTitle, Some(padding_seed))],
        )
        .expect("seed padding through streaming rewrite");

        let final_title = "Native padded fast path readback";
        let audio_start_before = flac_metadata_writer::test_read_audio_start(&path).expect("audio start before");
        let audio_before = file_region_checksum(&path, audio_start_before).expect("audio checksum before");
        let backup_checks = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let write_lens = std::sync::Arc::new(std::sync::Mutex::new(Vec::<usize>::new()));
        let backup_checks_hook = std::sync::Arc::clone(&backup_checks);
        let write_lens_hook = std::sync::Arc::clone(&write_lens);

        flac_metadata_writer::test_with_fast_path_hooks(
            temp.path(),
            move |write_path| {
                assert!(
                    !crate::db::Database::backup_path_for(write_path).exists(),
                    "real FLAC fast path must not create a .tonepoet-bak during write"
                );
                backup_checks_hook.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            },
            move |_write_path, len| {
                write_lens_hook.lock().expect("write lens poisoned").push(len);
            },
            || {
                write_all_tags(
                    &path,
                    &[(lofty::tag::ItemKey::TrackTitle, Some(final_title.to_string()))],
                )
            },
        )
        .expect("real FLAC padded fast path write");

        assert!(backup_checks.load(std::sync::atomic::Ordering::SeqCst) > 0);
        let write_lens = write_lens.lock().expect("write lens poisoned");
        assert_eq!(write_lens.len(), 1);
        assert!(
            write_lens[0] <= 2 * 1024 * 1024,
            "real FLAC padded fast path wrote an unexpectedly large metadata region: {} bytes",
            write_lens[0]
        );
        let audio_start_after = flac_metadata_writer::test_read_audio_start(&path).expect("audio start after");
        assert_eq!(audio_start_before, audio_start_after);
        let audio_after = file_region_checksum(&path, audio_start_after).expect("audio checksum after");
        assert_eq!(audio_before, audio_after, "audio region must remain byte-identical");
        assert!(!crate::db::Database::backup_path_for(&path).exists());
        assert_semantic_title_readback(&path, final_title);
    }

    /// Manual benchmark for real (typically network) filesystems. Copies
    /// nothing and mutates only the file you point it at — use a scratch
    /// copy. Two writes are timed: the first may be an overflow rewrite if
    /// the file lacks padding (which then seeds 1 MiB of padding), the
    /// second exercises the in-place fast path.
    #[test]
    #[ignore = "manual benchmark: set TONEPOET_BENCH_FLAC to a scratch FLAC copy"]
    fn flac_tag_write_manual_benchmark_at_env_path() {
        let Some(path) = std::env::var_os("TONEPOET_BENCH_FLAC") else {
            eprintln!("TONEPOET_BENCH_FLAC not set; nothing to benchmark");
            return;
        };
        let path = std::path::PathBuf::from(path);
        for round in 1..=2 {
            let started = std::time::Instant::now();
            write_all_tags(
                &path,
                &[(
                    lofty::tag::ItemKey::Comment,
                    Some(format!("tonepoet-bench round {round} pid {}", std::process::id())),
                )],
            )
            .expect("benchmark tag write");
            eprintln!(
                "BENCH round {round}: tag write on {} took {:?}",
                path.display(),
                started.elapsed()
            );
        }
    }

    #[test]
    #[ignore = "requires ffmpeg and creates a >=100 MB FLAC fixture"]
    fn flac_large_real_fixture_acceptance_uses_padding_without_backup_and_reads_back() {
        if !command_available("ffmpeg") {
            eprintln!("skipping large FLAC acceptance test because ffmpeg is unavailable");
            return;
        }
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("large-real-acceptance.flac");
        generate_large_real_flac_with_ffmpeg(&path, 100 * 1024 * 1024)
            .expect("generate >=100 MB real FLAC fixture");

        let padding_seed = "x".repeat(128 * 1024);
        write_all_tags(
            &path,
            &[(lofty::tag::ItemKey::TrackTitle, Some(padding_seed))],
        )
        .expect("seed large fixture padding through streaming rewrite");

        let final_title = "Large native padded fast path readback";
        let audio_start_before = flac_metadata_writer::test_read_audio_start(&path).expect("audio start before");
        let audio_before = file_region_checksum(&path, audio_start_before).expect("large audio checksum before");
        let backup_checks = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let write_lens = std::sync::Arc::new(std::sync::Mutex::new(Vec::<usize>::new()));
        let backup_checks_hook = std::sync::Arc::clone(&backup_checks);
        let write_lens_hook = std::sync::Arc::clone(&write_lens);

        flac_metadata_writer::test_with_fast_path_hooks(
            temp.path(),
            move |write_path| {
                assert!(
                    !crate::db::Database::backup_path_for(write_path).exists(),
                    "large FLAC fast path must not create a .tonepoet-bak during write"
                );
                backup_checks_hook.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            },
            move |_write_path, len| {
                write_lens_hook.lock().expect("write lens poisoned").push(len);
            },
            || {
                write_all_tags(
                    &path,
                    &[(lofty::tag::ItemKey::TrackTitle, Some(final_title.to_string()))],
                )
            },
        )
        .expect("large FLAC padded fast path write");

        assert!(backup_checks.load(std::sync::atomic::Ordering::SeqCst) > 0);
        let write_lens = write_lens.lock().expect("write lens poisoned");
        assert_eq!(write_lens.len(), 1);
        assert!(
            write_lens[0] <= 2 * 1024 * 1024,
            "large FLAC fast path wrote an unexpectedly large metadata region: {} bytes",
            write_lens[0]
        );
        let audio_start_after = flac_metadata_writer::test_read_audio_start(&path).expect("audio start after");
        assert_eq!(audio_start_before, audio_start_after);
        let audio_after = file_region_checksum(&path, audio_start_after).expect("large audio checksum after");
        assert_eq!(audio_before, audio_after, "large audio region must remain byte-identical");
        assert!(!crate::db::Database::backup_path_for(&path).exists());
        assert_semantic_title_readback(&path, final_title);
    }

    fn tiny_png() -> Vec<u8> {
        vec![
            0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n',
            0, 0, 0, 13, b'I', b'H', b'D', b'R',
            0, 0, 0, 1, 0, 0, 0, 1,
            8, 2, 0, 0, 0,
        ]
    }

    #[test]
    fn flac_artwork_write_uses_padding_without_full_backup_and_preserves_audio() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("artwork-padded.flac");
        let audio = write_synthetic_flac(&path, &[("TITLE", "Old")], 4096, 128 * 1024);
        let audio_start_before = flac_metadata_writer::test_read_audio_start(&path).expect("audio start before");
        let png = tiny_png();

        let rollback = write_artwork_one_file(
            &path,
            &png,
            &lofty::picture::MimeType::Png,
            "image/png",
            lofty::picture::PictureType::CoverFront,
            None,
        )
        .expect("native artwork write")
        .expect("rollback token");
        let mut rollback_tokens = [rollback];
        cleanup_artwork_tokens(&mut rollback_tokens);

        assert!(!crate::db::Database::backup_path_for(&path).exists());
        assert!(!flac_metadata_writer::test_journal_path(&path).exists());
        assert_eq!(
            flac_metadata_writer::test_picture_block_type_count(&path, lofty::picture::PictureType::CoverFront)
                .expect("picture count"),
            1
        );
        let audio_start_after = flac_metadata_writer::test_read_audio_start(&path).expect("audio start after");
        assert_eq!(audio_start_before, audio_start_after, "padded artwork update must not move audio");
        let bytes = std::fs::read(&path).expect("read rewritten flac");
        assert_eq!(&bytes[audio_start_after as usize..], audio.as_slice());
    }

    #[test]
    fn flac_artwork_remove_uses_metadata_region_and_preserves_audio() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("artwork-remove.flac");
        let audio = write_synthetic_flac(&path, &[("TITLE", "Old")], 4096, 128 * 1024);
        let png = tiny_png();
        let rollback = write_artwork_one_file(
            &path,
            &png,
            &lofty::picture::MimeType::Png,
            "image/png",
            lofty::picture::PictureType::CoverFront,
            None,
        )
        .expect("native artwork write")
        .expect("rollback token");
        let mut rollback_tokens = [rollback];
        cleanup_artwork_tokens(&mut rollback_tokens);
        let audio_start_before = flac_metadata_writer::test_read_audio_start(&path).expect("audio start before");

        let rollback = remove_artwork_one_file(&path, lofty::picture::PictureType::CoverFront, None)
            .expect("native artwork remove")
            .expect("rollback token");
        let mut rollback_tokens = [rollback];
        cleanup_artwork_tokens(&mut rollback_tokens);

        assert!(!crate::db::Database::backup_path_for(&path).exists());
        assert_eq!(
            flac_metadata_writer::test_picture_block_type_count(&path, lofty::picture::PictureType::CoverFront)
                .expect("picture count"),
            0
        );
        let audio_start_after = flac_metadata_writer::test_read_audio_start(&path).expect("audio start after");
        assert_eq!(audio_start_before, audio_start_after);
        let bytes = std::fs::read(&path).expect("read rewritten flac");
        assert_eq!(&bytes[audio_start_after as usize..], audio.as_slice());
    }

    #[test]
    fn sort_paths_entries_and_metadata_keeps_cached_metadata_aligned() {
        let mut paths = vec![
            std::path::PathBuf::from("02 - Second.flac"),
            std::path::PathBuf::from("01 - First.flac"),
        ];
        let mut entries = vec![TagEntry {
            row_scope: crate::tui::probe::RowScope::File,
            display_key: "TRACKNUMBER".to_string(),
            item_key: lofty::tag::ItemKey::TrackNumber,
            value: "<multiple values>".to_string(),
            original: "<multiple values>".to_string(),
            is_binary: false,
            is_mixed: true,
            has_multiple_stored_values: true,
            per_file_stored_value_counts: vec![2, 1],
            per_file_values: vec!["2".to_string(), "1".to_string()],
            per_file_originals: vec!["2".to_string(), "1".to_string()],
            mb_proposed_value: None,
            mb_proposed_per_file: None,
        }];
        let mut metadata = vec![
            SourceMetadata {
                title: Some("Second".to_string()),
                ..Default::default()
            },
            SourceMetadata {
                title: Some("First".to_string()),
                ..Default::default()
            },
        ];

        sort_paths_entries_and_metadata_by_track(&mut paths, &mut entries, &mut metadata);

        assert_eq!(paths[0].file_name().and_then(|s| s.to_str()), Some("01 - First.flac"));
        assert_eq!(entries[0].per_file_values, vec!["1".to_string(), "2".to_string()]);
        assert_eq!(entries[0].per_file_stored_value_counts, vec![1, 2]);
        assert!(entries[0].has_multiple_stored_values);
        assert_eq!(metadata[0].title.as_deref(), Some("First"));
        assert_eq!(metadata[1].title.as_deref(), Some("Second"));
    }

    fn entry_with_mb_proposed(original: &str, proposed: &str, per_file_count: usize) -> TagEntry {
        TagEntry {
            row_scope: crate::tui::probe::RowScope::File,
            display_key: "TITLE".to_string(),
            item_key: lofty::tag::ItemKey::TrackTitle,
            value: proposed.to_string(),
            original: original.to_string(),
            is_binary: false,
            is_mixed: false,
            has_multiple_stored_values: false,
            per_file_stored_value_counts: Vec::new(),
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
            row_scope: crate::tui::probe::RowScope::File,
            display_key: "TITLE".into(),
            item_key: lofty::tag::ItemKey::TrackTitle,
            value: "x".into(),
            original: "x".into(),
            is_binary: false,
            is_mixed: false,
            has_multiple_stored_values: false,
            per_file_stored_value_counts: Vec::new(),
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

    fn multi_value_mb_entry(current: &[&str], proposed: &[&str]) -> TagEntry {
        let current_values = current.iter().map(|value| (*value).to_string()).collect::<Vec<_>>();
        let proposed_values = proposed.iter().map(|value| (*value).to_string()).collect::<Vec<_>>();
        let mixed = current_values.windows(2).any(|pair| pair[0] != pair[1]);
        TagEntry {
            row_scope: RowScope::File,
            display_key: "ARTIST".to_string(),
            item_key: lofty::tag::ItemKey::TrackArtist,
            value: if mixed {
                "<multiple values>".to_string()
            } else {
                current_values.first().cloned().unwrap_or_default()
            },
            original: if mixed {
                "<multiple values>".to_string()
            } else {
                current_values.first().cloned().unwrap_or_default()
            },
            is_binary: false,
            is_mixed: mixed,
            has_multiple_stored_values: true,
            per_file_stored_value_counts: vec![2, 1],
            per_file_values: current_values.clone(),
            per_file_originals: current_values,
            mb_proposed_value: Some("<multiple values>".to_string()),
            mb_proposed_per_file: Some(proposed_values),
        }
    }

    #[test]
    fn musicbrainz_snapshot_report_recovers_prepopulated_editor_loss() {
        let mut entry = multi_value_mb_entry(
            &["New Artist", "New Scalar"],
            &["New Artist", "New Scalar"],
        );
        entry.per_file_originals = vec!["Alpha; Beta".to_string(), "Gamma".to_string()];

        let report = MetadataMutationReport::from_musicbrainz_entries(&[entry]);

        assert_eq!(report.changed_fields, 1);
        assert_eq!(report.collapsed_carrier_count(), 1);
        assert_eq!(report.collapsed_fields[0].display_key, "ARTIST");
        assert_eq!(report.collapsed_fields[0].slots, vec![0]);
    }

    #[test]
    fn mutation_report_merge_aggregates_provider_tabs() {
        let mut first = MetadataMutationReport {
            changed_fields: 2,
            collapsed_fields: vec![MetadataStoredValueCollapse {
                display_key: "ARTIST".to_string(),
                slots: vec![0],
            }],
        };
        first.merge(MetadataMutationReport {
            changed_fields: 3,
            collapsed_fields: vec![MetadataStoredValueCollapse {
                display_key: "COMPOSER".to_string(),
                slots: vec![1, 2],
            }],
        });

        assert_eq!(first.changed_fields, 5);
        assert_eq!(first.collapsed_carrier_count(), 3);
        assert_eq!(first.collapsed_fields.len(), 2);
    }

    #[test]
    fn provider_summary_composes_population_and_cardinality_loss() {
        let report = MetadataMutationReport {
            changed_fields: 8,
            collapsed_fields: vec![MetadataStoredValueCollapse {
                display_key: "ARTIST".to_string(),
                slots: vec![0, 2],
            }],
        };
        let mut status = "MusicBrainz complete".to_string();

        report.append_provider_summary("MusicBrainz", &mut status);

        assert_eq!(
            status,
            "MusicBrainz complete; MusicBrainz populated 8 fields; warning: 2 carriers across 1 field collapsed multiple stored values into one value"
        );
    }

    #[test]
    fn provider_completion_paths_preserve_structured_cardinality_reports() {
        let event_loop = include_str!("event_loop.rs");
        let keybindings = include_str!("keybindings.rs");
        let main = include_str!("../main.rs");

        assert!(event_loop.contains(
            "mb_mutation_report.append_provider_summary(\"MusicBrainz\", &mut msg)"
        ));
        assert!(keybindings.contains(
            "mutation_report.append_provider_summary(\"GNUDB\", &mut status)"
        ));
        assert!(keybindings.contains(
            "apply_active_musicbrainz_values_to_matching_presentations"
        ));
        assert!(keybindings.contains("apply_result.changed_presentations"));
        assert!(main.contains(
            "mb_mutation_report.append_collapse_warning(&mut warning)"
        ));
    }

    #[test]
    fn use_mb_values_reports_only_the_multi_value_carrier() {
        let mut entry = multi_value_mb_entry(
            &["Alpha; Beta", "Gamma"],
            &["New Artist", "New Scalar"],
        );

        let report = toggle_mb_revert_field(&mut entry);

        assert_eq!(entry.per_file_values, vec!["New Artist", "New Scalar"]);
        assert_eq!(report.changed_fields, 1);
        assert_eq!(report.collapsed_carrier_count(), 1);
        assert_eq!(report.collapsed_fields[0].display_key, "ARTIST");
        assert_eq!(report.collapsed_fields[0].slots, vec![0]);
    }

    #[test]
    fn reverting_mb_values_to_original_reports_no_cardinality_loss() {
        let mut entry = multi_value_mb_entry(
            &["Alpha; Beta", "Gamma"],
            &["New Artist", "New Scalar"],
        );
        entry.per_file_values = vec!["New Artist".to_string(), "New Scalar".to_string()];
        recompute_aggregate_value(&mut entry);

        let report = toggle_mb_revert_field(&mut entry);

        assert_eq!(entry.per_file_values, vec!["Alpha; Beta", "Gamma"]);
        assert_eq!(report.changed_fields, 1);
        assert_eq!(report.collapsed_carrier_count(), 0);
    }

    #[test]
    fn restoring_mb_values_after_manual_edit_reports_cardinality_loss() {
        let mut entry = multi_value_mb_entry(
            &["Alpha; Beta", "Gamma"],
            &["New Artist", "New Scalar"],
        );
        entry.per_file_values = vec!["Manual Artist".to_string(), "Gamma".to_string()];
        recompute_aggregate_value(&mut entry);

        let report = restore_mb_proposed(&mut entry);

        assert_eq!(entry.per_file_values, vec!["New Artist", "New Scalar"]);
        assert_eq!(report.collapsed_carrier_count(), 1);
        assert_eq!(report.collapsed_fields[0].slots, vec![0]);
    }

    #[test]
    fn metadata_editor_has_changes_true_with_pending_deletion() {
        use crate::tui::app::MetadataEditorState;
        // No value changes, but one entry marked for deletion → dirty.
        let mut state = MetadataEditorState::for_files(
            vec![std::path::PathBuf::from("/tmp/01.flac")],
            vec![TagEntry {
                row_scope: crate::tui::probe::RowScope::File,
                display_key: "TITLE".into(),
                item_key: lofty::tag::ItemKey::TrackTitle,
                value: "x".into(),
                original: "x".into(),
                is_binary: false,
                is_mixed: false,
                has_multiple_stored_values: false,
                per_file_stored_value_counts: Vec::new(),
                per_file_values: vec!["x".into()],
                per_file_originals: vec!["x".into()],
                mb_proposed_value: None,
                mb_proposed_per_file: None,
            }],
            vec!["01".into()],
            crate::tui::app::MetadataTechnicalDetails::default(),
        );
        state.active_surface_mut().deleted = vec![0];
        assert!(metadata_editor_has_changes(&state));
    }

    #[test]
    fn metadata_editor_has_changes_false_after_full_revert() {
        use crate::tui::app::MetadataEditorState;
        let mut state = MetadataEditorState::for_files(
            vec![std::path::PathBuf::from("/tmp/01.flac")],
            vec![
                entry_with_mb_proposed("File", "MB", 1),
                entry_with_mb_proposed("File2", "MB2", 1),
            ],
            vec!["01".into()],
            crate::tui::app::MetadataTechnicalDetails::default(),
        );
        state.active_surface_mut().dirty = true;
        // Both entries currently show the MB value → has changes.
        assert!(metadata_editor_has_changes(&state));

        // Revert both.
        toggle_mb_revert(&mut state.active_surface_mut().entries[0]);
        toggle_mb_revert(&mut state.active_surface_mut().entries[1]);
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
            row_scope: crate::tui::probe::RowScope::File,
            display_key: "TITLE".into(),
            item_key: lofty::tag::ItemKey::TrackTitle,
            value: "x".into(),
            original: "y".into(),
            is_binary: false,
            is_mixed: false,
            has_multiple_stored_values: false,
            per_file_stored_value_counts: Vec::new(),
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
        let build_area = |magic: &[u8; 8], channels: u8, lou: u8| {
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

    #[test]
    fn artwork_batch_rolls_back_successful_flac_metadata_when_later_target_fails() {
        let td = tempfile::tempdir().expect("tempdir");
        let one = td.path().join("one.flac");
        let two = td.path().join("two.flac");
        let audio_one = write_synthetic_flac(&one, &[("TITLE", "one")], 4096, 4096);
        let audio_two = write_synthetic_flac(&two, &[("TITLE", "two")], 4096, 4096);
        let png = tiny_png();
        let paths = vec![one.clone(), two.clone()];

        let result = apply_artwork_batch(&paths, None, |path| {
            if path == one.as_path() {
                write_artwork_one_file(
                    path,
                    &png,
                    &lofty::picture::MimeType::Png,
                    "image/png",
                    lofty::picture::PictureType::CoverFront,
                    None,
                )
            } else {
                Err("simulated second-file failure".to_string())
            }
        });

        assert!(result.is_err());
        assert_eq!(
            flac_metadata_writer::test_picture_block_type_count(&one, lofty::picture::PictureType::CoverFront)
                .expect("picture count"),
            0
        );
        let one_audio_start = flac_metadata_writer::test_read_audio_start(&one).expect("one audio start") as usize;
        let two_audio_start = flac_metadata_writer::test_read_audio_start(&two).expect("two audio start") as usize;
        assert_eq!(&std::fs::read(&one).expect("read one")[one_audio_start..], audio_one.as_slice());
        assert_eq!(&std::fs::read(&two).expect("read two")[two_audio_start..], audio_two.as_slice());
        assert!(!crate::db::Database::backup_path_for(&one).exists());
        assert!(!crate::db::Database::backup_path_for(&two).exists());
    }

    #[test]
    fn flac_artwork_rollback_recovery_is_idempotent_after_torn_in_place_restore() {
        let td = tempfile::tempdir().expect("tempdir");
        let path = td.path().join("artwork-torn-restore.flac");
        let _audio = write_synthetic_flac(&path, &[("TITLE", "original")], 4096, 4096);
        let original = std::fs::read(&path).expect("read original");
        let png = tiny_png();
        let (snapshot, intended_metadata_region) = flac_metadata_writer::preview_picture_write(
            &path,
            lofty::picture::PictureType::CoverFront,
            "image/png",
            &png,
        )
        .expect("preview artwork write");
        let rollback_journal = flac_metadata_writer::begin_artwork_rollback_journal_with_intended(
            &path,
            &snapshot,
            &intended_metadata_region,
        )
        .expect("write artwork rollback journal");

        flac_metadata_writer::write_picture_block(
            &path,
            lofty::picture::PictureType::CoverFront,
            "image/png",
            &png,
            None,
        )
        .expect("simulate committed artwork mutation before crash");
        flac_metadata_writer::test_mark_artwork_rollback_journal_stale(&path)
            .expect("simulate owner process death");
        let rollback_path = rollback_journal.path.clone();
        drop(rollback_journal);

        // Simulate a second crash during rollback recovery itself: the saved
        // original metadata region is being restored in place, but only a torn
        // metadata header reached disk. The rollback journal is still present,
        // so the next recovery must use the journal bytes rather than refusing
        // because the current FLAC metadata is unparsable.
        corrupt_synthetic_flac_metadata_header(&path);
        assert!(
            flac_metadata_writer::test_read_audio_start(&path).is_err(),
            "fixture should be unparsable before idempotent rollback recovery"
        );
        assert!(rollback_path.exists(), "rollback journal must remain after torn restore");

        recover_flac_metadata_before_read(&path)
            .expect("read guard must recover a torn rollback-restore attempt");
        assert!(!rollback_path.exists(), "idempotent recovery removes the rollback journal");
        assert_eq!(
            std::fs::read(&path).expect("read recovered"),
            original,
            "torn rollback recovery must restore the exact pre-artwork FLAC bytes"
        );
        assert_eq!(
            flac_metadata_writer::test_picture_block_type_count(&path, lofty::picture::PictureType::CoverFront)
                .expect("picture count"),
            0
        );
    }

    #[test]
    fn flac_artwork_rollback_cleanup_is_idempotent_after_overflow_restore_commit() {
        let td = tempfile::tempdir().expect("tempdir");
        let path = td.path().join("artwork-overflow-restore-committed.flac");
        let _audio = write_synthetic_flac(&path, &[("TITLE", "original")], 0, 4096);
        let original = std::fs::read(&path).expect("read original");
        let large_artwork = vec![0x5au8; 8192];
        let (snapshot, intended_metadata_region) = flac_metadata_writer::preview_picture_write(
            &path,
            lofty::picture::PictureType::CoverFront,
            "image/png",
            &large_artwork,
        )
        .expect("preview overflow artwork write");
        assert_ne!(
            snapshot.raw_metadata_region.len(),
            intended_metadata_region.len(),
            "fixture should force an overflow artwork metadata region"
        );
        let rollback_journal = flac_metadata_writer::begin_artwork_rollback_journal_with_intended(
            &path,
            &snapshot,
            &intended_metadata_region,
        )
        .expect("write artwork rollback journal");
        flac_metadata_writer::write_picture_block(
            &path,
            lofty::picture::PictureType::CoverFront,
            "image/png",
            &large_artwork,
            None,
        )
        .expect("simulate committed overflow artwork mutation before crash");
        flac_metadata_writer::test_mark_artwork_rollback_journal_stale(&path)
            .expect("simulate owner process death");

        // Simulate recovery committing the rollback by overflow-style rename,
        // then crashing before rollback-journal removal. The file now contains
        // the original metadata but may have a different inode/ctime than the
        // pre-artwork snapshot. Recovery must consume this journal as already
        // rolled back, not reject it as an externally replaced target.
        flac_metadata_writer::restore_metadata_snapshot(&path, &snapshot)
            .expect("commit rollback restore but leave journal behind");
        let rollback_path = rollback_journal.path.clone();
        drop(rollback_journal);
        assert!(rollback_path.exists(), "crash before cleanup leaves rollback journal");
        assert_eq!(
            std::fs::read(&path).expect("read after restore commit"),
            original,
            "fixture should already be rolled back before cleanup retry"
        );

        recover_flac_metadata_before_read(&path)
            .expect("recovery must treat original metadata with stale journal as an idempotent completed rollback");
        assert!(!rollback_path.exists(), "idempotent cleanup removes rollback journal");
        assert_eq!(
            std::fs::read(&path).expect("read after idempotent cleanup"),
            original,
            "cleanup retry must not mutate already restored bytes"
        );
    }

    #[test]
    fn flac_artwork_stale_rollback_journal_restores_after_process_death() {
        let td = tempfile::tempdir().expect("tempdir");
        let path = td.path().join("artwork-crash-rollback.flac");
        let audio = write_synthetic_flac(&path, &[("TITLE", "original")], 4096, 4096);
        let png = tiny_png();
        let (snapshot, intended_metadata_region) = flac_metadata_writer::preview_picture_write(
            &path,
            lofty::picture::PictureType::CoverFront,
            "image/png",
            &png,
        )
        .expect("preview artwork write");
        let rollback_journal = flac_metadata_writer::begin_artwork_rollback_journal_with_intended(
            &path,
            &snapshot,
            &intended_metadata_region,
        )
        .expect("write artwork rollback journal");

        flac_metadata_writer::write_picture_block(
            &path,
            lofty::picture::PictureType::CoverFront,
            "image/png",
            &png,
            None,
        )
        .expect("simulate successful first artwork write before crash");
        flac_metadata_writer::test_mark_artwork_rollback_journal_stale(&path)
            .expect("simulate owner process death");
        let rollback_path = rollback_journal.path.clone();
        drop(rollback_journal);
        assert!(rollback_path.exists(), "crash leaves durable FLAC artwork rollback journal");
        assert_eq!(
            flac_metadata_writer::test_picture_block_type_count(&path, lofty::picture::PictureType::CoverFront)
                .expect("picture count before recovery"),
            1
        );

        let messages = recover_stale_flac_metadata_journals_in_dir(td.path());
        assert!(
            messages.iter().any(|message| message.contains("artwork rollback journal")),
            "startup directory recovery should report the stale artwork rollback journal: {messages:?}"
        );
        assert!(!rollback_path.exists(), "recovery removes durable FLAC artwork rollback journal");
        assert_eq!(
            flac_metadata_writer::test_picture_block_type_count(&path, lofty::picture::PictureType::CoverFront)
                .expect("picture count after recovery"),
            0,
            "stale artwork rollback journal must restore pre-batch FLAC metadata"
        );
        let audio_start = flac_metadata_writer::test_read_audio_start(&path).expect("audio start") as usize;
        assert_eq!(&std::fs::read(&path).expect("read after recovery")[audio_start..], audio.as_slice());
    }

    #[test]
    fn flac_artwork_stale_rollback_refuses_external_replacement() {
        let td = tempfile::tempdir().expect("tempdir");
        let path = td.path().join("artwork-external-replace.flac");
        let _ = write_synthetic_flac(&path, &[("TITLE", "original")], 4096, 4096);
        let png = tiny_png();
        let (snapshot, intended_metadata_region) = flac_metadata_writer::preview_picture_write(
            &path,
            lofty::picture::PictureType::CoverFront,
            "image/png",
            &png,
        )
        .expect("preview artwork write");
        let rollback_journal = flac_metadata_writer::begin_artwork_rollback_journal_with_intended(
            &path,
            &snapshot,
            &intended_metadata_region,
        )
        .expect("write artwork rollback journal");
        flac_metadata_writer::write_picture_block(
            &path,
            lofty::picture::PictureType::CoverFront,
            "image/png",
            &png,
            None,
        )
        .expect("simulate successful artwork write before crash");
        flac_metadata_writer::test_mark_artwork_rollback_journal_stale(&path)
            .expect("simulate owner process death");
        let rollback_path = rollback_journal.path.clone();
        drop(rollback_journal);

        std::fs::remove_file(&path).expect("replace target after crash");
        let _ = write_synthetic_flac(&path, &[("TITLE", "external")], 4096, 4096);

        let messages = recover_stale_flac_metadata_journals_in_dir(td.path());
        assert!(
            messages.iter().any(|message| {
                message.contains("artwork rollback journal recovery failed")
                    && message.contains("no longer matches")
            }),
            "recovery must refuse to restore stale artwork metadata into an externally replaced file: {messages:?}"
        );
        assert!(
            rollback_path.exists(),
            "refused recovery keeps the rollback journal for explicit operator inspection"
        );
        assert_eq!(
            flac_metadata_writer::test_picture_block_type_count(&path, lofty::picture::PictureType::CoverFront)
                .expect("picture count"),
            0,
            "refused recovery must not mutate the externally replaced target"
        );
    }

    #[test]
    fn flac_artwork_cleanup_parent_sync_failure_is_committed_warning_not_blocking() {
        let td = tempfile::tempdir().expect("tempdir");
        let path = td.path().join("artwork-cleanup-warning.flac");
        let _ = write_synthetic_flac(&path, &[("TITLE", "original")], 4096, 4096);
        let png = tiny_png();
        let rollback = write_artwork_one_file(
            &path,
            &png,
            &lofty::picture::MimeType::Png,
            "image/png",
            lofty::picture::PictureType::CoverFront,
            None,
        )
        .expect("native artwork write")
        .expect("rollback token");

        let cleanup = flac_metadata_writer::test_with_parent_dir_sync_hook(
            td.path(),
            |_parent, context| {
                (context == "FLAC artwork rollback journal removal")
                    .then(|| Err("injected directory fsync failure after journal removal".to_string()))
            },
            || {
                let mut rollback_tokens = [rollback];
                cleanup_artwork_tokens(&mut rollback_tokens)
            },
        );

        assert!(
            cleanup.blocking_issues.is_empty(),
            "post-removal parent fsync failure is a committed durability warning, not an armed rollback failure: {:?}",
            cleanup.blocking_issues
        );
        assert!(
            cleanup.committed_warnings.iter().any(|warning| warning.contains("already committed")),
            "cleanup should surface a committed durability warning: {:?}",
            cleanup.committed_warnings
        );
        assert_eq!(
            flac_metadata_writer::test_picture_block_type_count(&path, lofty::picture::PictureType::CoverFront)
                .expect("picture count"),
            1,
            "artwork mutation remains committed after rollback-journal removal warning"
        );
        assert!(
            !path.with_file_name("artwork-cleanup-warning.flac.tonepoet-artwork-rollback").exists(),
            "journal removal succeeded before the parent fsync warning"
        );
    }

    #[test]
    fn native_artwork_write_failure_keeps_rollback_journal_when_restore_fails() {
        let td = tempfile::tempdir().expect("tempdir");
        let path = td.path().join("artwork-write-restore-fails.flac");
        let _ = write_synthetic_flac(&path, &[("TITLE", "original")], 4096, 4096);
        let png = tiny_png();
        let cancel = MetadataWriteCancelFlag::new();
        cancel.cancel();
        let hook_path = path.clone();

        let err = flac_metadata_writer::test_with_metadata_snapshot_restore_hook(
            td.path(),
            move |p| (p == hook_path.as_path()).then(|| Err("injected rollback restore failure".to_string())),
            || {
                write_artwork_one_file(
                    &path,
                    &png,
                    &lofty::picture::MimeType::Png,
                    "image/png",
                    lofty::picture::PictureType::CoverFront,
                    Some(&cancel),
                )
                .expect_err("cancelled native artwork write with failed restore should fail")
            },
        );

        assert!(
            err.contains("rollback restore failed") && err.contains("remains armed"),
            "error should report that the recovery journal remains armed: {err}"
        );
        assert!(
            path.with_file_name("artwork-write-restore-fails.flac.tonepoet-artwork-rollback").exists(),
            "failed rollback restore must not remove the only durable recovery journal"
        );
        assert_eq!(
            flac_metadata_writer::test_picture_block_type_count(&path, lofty::picture::PictureType::CoverFront)
                .expect("picture count"),
            0,
            "cancel happened before mutation; the important invariant is that the journal remains for recovery"
        );
    }

    #[test]
    fn batch_artwork_rollback_failure_keeps_flac_rollback_journal() {
        let td = tempfile::tempdir().expect("tempdir");
        let path = td.path().join("artwork-batch-restore-fails.flac");
        let _ = write_synthetic_flac(&path, &[("TITLE", "original")], 4096, 4096);
        let png = tiny_png();
        let rollback = write_artwork_one_file(
            &path,
            &png,
            &lofty::picture::MimeType::Png,
            "image/png",
            lofty::picture::PictureType::CoverFront,
            None,
        )
        .expect("native artwork write")
        .expect("rollback token");
        let journal = match &rollback {
            ArtworkRollbackToken::Flac { journal, .. } => journal.clone(),
            ArtworkRollbackToken::FullFileBackup { .. }
            | ArtworkRollbackToken::Dsf { .. } => {
                panic!("expected native FLAC rollback token")
            }
        };
        let hook_path = path.clone();

        let issues = flac_metadata_writer::test_with_metadata_snapshot_restore_hook(
            td.path(),
            move |p| (p == hook_path.as_path()).then(|| Err("injected rollback restore failure".to_string())),
            || {
                let mut rollback_tokens = [rollback];
                rollback_artwork_tokens(&mut rollback_tokens)
            },
        );

        assert!(
            issues.iter().any(|issue| issue.contains("keeping rollback journal") && issue.contains("armed")),
            "rollback failure should explicitly keep the journal armed: {issues:?}"
        );
        assert!(journal.exists(), "rollback journal must remain after restore failure");
        assert_eq!(
            flac_metadata_writer::test_picture_block_type_count(&path, lofty::picture::PictureType::CoverFront)
                .expect("picture count"),
            1,
            "failed same-process rollback leaves the mutation visible but recoverable by the retained journal"
        );
    }

    #[test]
    fn artwork_rollback_pid_reuse_identity_mismatch_does_not_suppress_recovery() {
        let td = tempfile::tempdir().expect("tempdir");
        let path = td.path().join("artwork-pid-reuse.flac");
        let _ = write_synthetic_flac(&path, &[("TITLE", "original")], 4096, 4096);
        let png = tiny_png();
        let (snapshot, intended_metadata_region) = flac_metadata_writer::preview_picture_write(
            &path,
            lofty::picture::PictureType::CoverFront,
            "image/png",
            &png,
        )
        .expect("preview artwork write");
        let rollback_journal = flac_metadata_writer::begin_artwork_rollback_journal_with_intended(
            &path,
            &snapshot,
            &intended_metadata_region,
        )
        .expect("write rollback journal");
        flac_metadata_writer::write_picture_block(
            &path,
            lofty::picture::PictureType::CoverFront,
            "image/png",
            &png,
            None,
        )
        .expect("simulate committed artwork mutation before crash");
        flac_metadata_writer::test_rewrite_artwork_rollback_owner_identity(
            &path,
            std::process::id() as u64,
            u64::MAX,
            u64::MAX,
            u64::MAX,
        )
        .expect("simulate PID reuse with mismatched process identity");
        let rollback_path = rollback_journal.path.clone();
        drop(rollback_journal);

        recover_flac_metadata_before_read(&path)
            .expect("mismatched owner identity must be treated as stale and recovered");
        assert!(!rollback_path.exists(), "stale rollback journal should be consumed");
        assert_eq!(
            flac_metadata_writer::test_picture_block_type_count(&path, lofty::picture::PictureType::CoverFront)
                .expect("picture count"),
            0,
            "PID-only liveness must not suppress stale rollback recovery after PID reuse"
        );
    }

    #[test]
    fn artwork_rollback_journal_claim_does_not_overwrite_active_owner() {
        let td = tempfile::tempdir().expect("tempdir");
        let path = td.path().join("artwork-claim-active.flac");
        let _ = write_synthetic_flac(&path, &[("TITLE", "original")], 4096, 4096);
        let png = tiny_png();
        let (snapshot, intended_metadata_region) = flac_metadata_writer::preview_picture_write(
            &path,
            lofty::picture::PictureType::CoverFront,
            "image/png",
            &png,
        )
        .expect("preview artwork write");

        let journal = flac_metadata_writer::begin_artwork_rollback_journal_with_intended(
            &path,
            &snapshot,
            &intended_metadata_region,
        )
        .expect("first writer claims artwork rollback journal");
        let first_journal = std::fs::read(&journal).expect("read first artwork rollback journal");

        let err = flac_metadata_writer::begin_artwork_rollback_journal_with_intended(
            &path,
            &snapshot,
            &intended_metadata_region,
        )
        .expect_err("second writer must not overwrite active artwork rollback journal");
        assert!(
            err.contains("owned by a live writer"),
            "second artwork rollback claim should fail as an active-writer conflict: {err}"
        );
        assert_eq!(
            std::fs::read(&journal).expect("read artwork rollback journal after rejected claim"),
            first_journal,
            "no-clobber acquisition must leave the first artwork rollback journal intact"
        );
        flac_metadata_writer::remove_artwork_rollback_journal(&path).expect("cleanup active artwork journal");
    }

    #[test]
    fn artwork_rollback_journal_claim_recovers_stale_owner_then_retries_once() {
        let td = tempfile::tempdir().expect("tempdir");
        let path = td.path().join("artwork-claim-stale.flac");
        let _ = write_synthetic_flac(&path, &[("TITLE", "original")], 4096, 4096);
        let png = tiny_png();
        let (snapshot, intended_metadata_region) = flac_metadata_writer::preview_picture_write(
            &path,
            lofty::picture::PictureType::CoverFront,
            "image/png",
            &png,
        )
        .expect("preview artwork write");

        let journal = flac_metadata_writer::begin_artwork_rollback_journal_with_intended(
            &path,
            &snapshot,
            &intended_metadata_region,
        )
        .expect("write first rollback journal");
        flac_metadata_writer::test_mark_artwork_rollback_journal_stale(&path)
            .expect("mark rollback journal stale");
        let journal_path = journal.path.clone();
        let stale_journal = std::fs::read(&journal_path).expect("read stale rollback journal");
        drop(journal);

        let retried_journal = flac_metadata_writer::begin_artwork_rollback_journal_with_intended(
            &path,
            &snapshot,
            &intended_metadata_region,
        )
        .expect("claim should recover stale rollback journal and retry once");
        assert_eq!(retried_journal.path.as_path(), journal_path.as_path());
        assert_ne!(
            std::fs::read(&journal_path).expect("read retried rollback journal"),
            stale_journal,
            "retried artwork claim should install the new writer's journal only after stale cleanup"
        );
        flac_metadata_writer::remove_artwork_rollback_journal(&path).expect("cleanup retried artwork journal");
    }

    #[test]
    fn same_process_old_artwork_rollback_journal_does_not_authorize_new_claim() {
        let td = tempfile::tempdir().expect("tempdir");
        let path = td.path().join("artwork-old-claim-token.flac");
        let _ = write_synthetic_flac(&path, &[("TITLE", "original")], 4096, 4096);
        let png = tiny_png();
        let (snapshot, intended_metadata_region) = flac_metadata_writer::preview_picture_write(
            &path,
            lofty::picture::PictureType::CoverFront,
            "image/png",
            &png,
        )
        .expect("preview artwork write");

        let rollback_journal = flac_metadata_writer::begin_artwork_rollback_journal_with_intended(
            &path,
            &snapshot,
            &intended_metadata_region,
        )
        .expect("create rollback journal under first common claim");
        let rollback_path = rollback_journal.path.clone();
        drop(rollback_journal);
        assert!(rollback_path.exists(), "rollback journal intentionally remains armed");
        assert!(
            !flac_metadata_writer::test_write_lock_path(&path).exists(),
            "dropping the first claim should release the common lock while leaving the journal"
        );

        let err = write_all_tags(
            &path,
            &[(lofty::tag::ItemKey::TrackTitle, Some("later unrelated tag write".to_string()))],
        )
        .expect_err("a later same-process claim must not inherit an old rollback journal");
        assert!(
            err.contains("different common write claim") || err.contains("owned by a live writer"),
            "error should explain that the rollback journal is not bound to the current claim: {err}"
        );
        assert!(rollback_path.exists(), "old rollback journal must remain armed for explicit cleanup/recovery");
        flac_metadata_writer::remove_artwork_rollback_journal(&path).expect("cleanup rollback journal");
    }

    #[test]
    fn active_artwork_common_claim_blocks_tag_write_from_another_thread() {
        let td = tempfile::tempdir().expect("tempdir");
        let path = td.path().join("artwork-common-blocks-tags.flac");
        let _ = write_synthetic_flac(&path, &[("TITLE", "original")], 4096, 4096);
        let png = tiny_png();
        let (snapshot, intended_metadata_region) = flac_metadata_writer::preview_picture_write(
            &path,
            lofty::picture::PictureType::CoverFront,
            "image/png",
            &png,
        )
        .expect("preview artwork write");
        let rollback_journal = flac_metadata_writer::begin_artwork_rollback_journal_with_intended(
            &path,
            &snapshot,
            &intended_metadata_region,
        )
        .expect("active artwork rollback claim");
        let path_for_thread = path.clone();
        let competing = std::thread::spawn(move || {
            write_all_tags(
                &path_for_thread,
                &[(lofty::tag::ItemKey::TrackTitle, Some("Competing".to_string()))],
            )
        })
        .join()
        .expect("competing writer thread should not panic");
        let err = competing.expect_err("tag writer must not run while artwork claim is active");
        assert!(
            err.contains("already in progress") || err.contains("write lock"),
            "tag writer should be blocked by the common FLAC write claim: {err}"
        );
        flac_metadata_writer::remove_artwork_rollback_journal(&path).expect("cleanup active rollback journal");
        drop(rollback_journal);
    }

    #[test]
    fn active_flac_artwork_rollback_journal_is_not_restored_inside_live_process() {
        let td = tempfile::tempdir().expect("tempdir");
        let path = td.path().join("artwork-active-rollback.flac");
        let _ = write_synthetic_flac(&path, &[("TITLE", "original")], 4096, 4096);
        let png = tiny_png();
        let (snapshot, intended_metadata_region) = flac_metadata_writer::preview_picture_write(
            &path,
            lofty::picture::PictureType::CoverFront,
            "image/png",
            &png,
        )
        .expect("preview artwork write");
        let rollback_journal = flac_metadata_writer::begin_artwork_rollback_journal_with_intended(
            &path,
            &snapshot,
            &intended_metadata_region,
        )
        .expect("write active artwork rollback journal");

        flac_metadata_writer::write_picture_block(
            &path,
            lofty::picture::PictureType::CoverFront,
            "image/png",
            &png,
            None,
        )
        .expect("write artwork while current process owns rollback journal");

        let read_err = recover_flac_metadata_before_read(&path)
            .expect_err("read guard must block while the live common artwork lock is held");
        assert!(
            read_err.contains("write appears to be in progress") || read_err.contains("common write lock"),
            "active common lock should be reported as a transient in-progress condition: {read_err}"
        );
        assert_eq!(
            flac_metadata_writer::test_picture_block_type_count(&path, lofty::picture::PictureType::CoverFront)
                .expect("picture count"),
            1,
            "active rollback journals are owned by the running batch and must not be restored by incidental reads"
        );
        flac_metadata_writer::remove_artwork_rollback_journal(&path).expect("cleanup active journal");
        let rollback_path = rollback_journal.path.clone();
        drop(rollback_journal);
        assert!(!rollback_path.exists());
    }

}

#[cfg(test)]
mod tolerant_dsf_editor_read_tests {
    use super::*;
    use id3::TagLike;

    #[test]
    fn noncanonical_dsf_remains_visible_with_typed_write_block_issue() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("quirky.dsf");
        let mut tag = id3::Tag::new();
        tag.add_frame(id3::Frame::text("TIT2", "Visible title"));
        let mut metadata = Vec::new();
        tag.write_to(&mut metadata, id3::Version::Id3v24)
            .expect("serialize ID3 fixture");
        crate::dsf_tags::write_test_dsf_fixture(&path, Some(&metadata))
            .expect("write DSF fixture");
        let mut bytes = std::fs::read(&path).expect("read DSF fixture");
        let actual_size = bytes.len() as u64;
        bytes[12..20].copy_from_slice(&(actual_size + 1).to_le_bytes());
        std::fs::write(&path, bytes).expect("publish DSF size quirk");

        let merged = read_all_tags_merged_with_metadata(std::slice::from_ref(&path))
            .expect("quirky DSF should remain readable");

        assert!(merged.entries.iter().any(|entry| {
            entry.display_key == "TITLE" && entry.value == "Visible title"
        }));
        assert_eq!(merged.metadata_errors.len(), 1);
        let issue = merged.metadata_errors[0]
            .as_ref()
            .expect("quirky DSF should carry a typed issue");
        assert_eq!(issue.kind, MetadataReadIssueKind::ContainerQuirk);
        assert!(issue.reason.contains("declared file size"));
        assert!(issue.reason.contains("metadata writes are blocked"));
    }
}
