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
    });
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

    Ok(meta)
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
}
