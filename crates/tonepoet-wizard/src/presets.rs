use std::fs;
use std::io;
use std::path::PathBuf;

use crate::types::{
    AudioFormat, DitherType, NyquistTransition, ReplayGainMode,
};

/// In-memory projection of the canonical Tonepoet TUI preset into the smaller
/// legacy wizard surface. This is deliberately not serializable: the only
/// persisted preset schema is `src/tui/presets.rs::TuiPreset`.
#[derive(Debug, Clone)]
pub struct TuiPresetProjection {
    pub selected_format: AudioFormat,
    pub selected_quality: Option<String>,
    pub bit_depth: Option<u32>,
    pub sample_rate: Option<u32>,
    pub compression_level: Option<u8>,
    pub resample_quality: Option<u8>,
    pub dither_type: Option<DitherType>,
    pub nyquist_transition: Option<NyquistTransition>,
    pub verify_encoding: Option<bool>,
    pub store_md5: Option<bool>,
    pub opus_content_type: Option<crate::types::OpusContentType>,
    pub aac_profile: Option<crate::types::AacProfile>,
    pub replaygain_mode: Option<ReplayGainMode>,
    pub copy_files_enabled: bool,
    pub copy_files_extensions: String,
    pub copy_subdirectories_enabled: bool,
    pub copy_subdirectories: String,
    pub merge_to_single: Option<bool>,
    pub reencode_flac: Option<bool>,
}

pub struct PresetManager {
    presets_dir: PathBuf,
}

impl PresetManager {
    pub fn new() -> io::Result<Self> {
        let presets_dir = Self::get_presets_directory()?;
        Ok(Self { presets_dir })
    }

    fn get_presets_directory() -> io::Result<PathBuf> {
        if let Ok(xdg_config) = std::env::var("XDG_CONFIG_HOME") {
            Ok(PathBuf::from(xdg_config).join("tonepoet").join("presets"))
        } else if let Ok(home) = std::env::var("HOME") {
            Ok(PathBuf::from(home)
                .join(".config")
                .join("tonepoet")
                .join("presets"))
        } else {
            Ok(PathBuf::from("./presets"))
        }
    }

    /// Read the canonical TUI preset schema and project only the subset that
    /// the legacy wizard can represent. Versionless wizard-schema TOML is not
    /// accepted and there is intentionally no wizard-schema writer.
    pub fn load_preset(&self, name: &str) -> io::Result<TuiPresetProjection> {
        let file_path = self.presets_dir.join(format!("{name}.toml"));
        let toml_str = fs::read_to_string(&file_path)?;
        let root: toml::Value = toml::from_str(&toml_str)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let table = root.as_table().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "preset root must be a TOML table")
        })?;

        let version = required_integer(table, "version")?;
        if version != 5 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unsupported TUI preset version {version}; current presets require version 5"),
            ));
        }
        let selected_format = match required_string(table, "format")?.to_ascii_lowercase().as_str() {
            "flac" => AudioFormat::Flac,
            "wav" => AudioFormat::Wav,
            "aiff" | "aif" => AudioFormat::Aiff,
            "wavpack" | "wv" => AudioFormat::WavPack,
            "mp3" => AudioFormat::Mp3,
            "aac" | "m4a" => AudioFormat::Aac,
            "opus" => AudioFormat::Opus,
            other => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("TUI preset format '{other}' is not representable by the legacy wizard"),
                ))
            }
        };

        let sample_rate = u32::try_from(required_integer(table, "sample_rate")?)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "sample_rate is outside u32"))?;
        let sample_rate = (sample_rate != 0).then_some(sample_rate);
        let bit_depth = match required_string(table, "bit_depth")? {
            "source" => None,
            "16" => Some(16),
            "24" => Some(24),
            "32" => Some(32),
            other => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("TUI preset bit depth '{other}' is not representable by the legacy wizard"),
                ))
            }
        };
        let dither_type = match required_string(table, "dither")? {
            "none" => Some(DitherType::None),
            "tpdf" => Some(DitherType::Tpdf),
            "shaped" | "shibata" => Some(DitherType::Shibata),
            "low-shibata" | "low_shibata" => Some(DitherType::LowShibata),
            "high-shibata" | "high_shibata" => Some(DitherType::HighShibata),
            "gesemann" => Some(DitherType::Gesemann),
            other => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("TUI preset dither '{other}' is not representable by the legacy wizard"),
                ))
            }
        };
        let replaygain_mode = match required_string(table, "replaygain")? {
            "off" => Some(ReplayGainMode::Off),
            "track" => Some(ReplayGainMode::Track),
            "album" => Some(ReplayGainMode::Album),
            "both" => Some(ReplayGainMode::Both),
            other => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("TUI preset ReplayGain mode '{other}' is not representable by the legacy wizard"),
                ))
            }
        };
        let nyquist_transition = match table.get("resampler").and_then(toml::Value::as_str) {
            Some("ssrc") => Some(NyquistTransition::BrickWall),
            Some("sox" | "soxr" | "none" | "off") | None => None,
            Some(other) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unknown TUI preset resampler '{other}'"),
                ))
            }
        };

        let companion_extensions = optional_string(table, "companion_extensions").unwrap_or_default();
        let companion_folders = optional_string(table, "companion_folders").unwrap_or_default();
        let merge_to_single = match required_string(table, "merge")? {
            "multi-file" => Some(false),
            "single-image" => Some(true),
            other => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unknown TUI preset merge mode '{other}'"),
                ))
            }
        };
        let force_encode = table
            .get("force_encode")
            .and_then(toml::Value::as_bool)
            .unwrap_or(false);

        Ok(TuiPresetProjection {
            selected_format,
            selected_quality: None,
            bit_depth,
            sample_rate,
            compression_level: None,
            resample_quality: None,
            dither_type,
            nyquist_transition,
            verify_encoding: None,
            store_md5: None,
            opus_content_type: None,
            aac_profile: None,
            replaygain_mode,
            copy_files_enabled: !companion_extensions.trim().is_empty(),
            copy_files_extensions: companion_extensions,
            copy_subdirectories_enabled: !companion_folders.trim().is_empty(),
            copy_subdirectories: companion_folders,
            merge_to_single,
            reencode_flac: Some(force_encode),
        })
    }

    pub fn list_presets(&self) -> io::Result<Vec<String>> {
        let mut presets = Vec::new();
        if let Ok(entries) = fs::read_dir(&self.presets_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("toml") {
                    if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                        presets.push(stem.to_string());
                    }
                }
            }
        }
        presets.sort();
        Ok(presets)
    }
}

fn required_string<'a>(table: &'a toml::Table, key: &str) -> io::Result<&'a str> {
    table.get(key).and_then(toml::Value::as_str).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, format!("TUI preset field '{key}' must be a string"))
    })
}

fn optional_string(table: &toml::Table, key: &str) -> Option<String> {
    table.get(key).and_then(toml::Value::as_str).map(str::to_string)
}

fn required_integer(table: &toml::Table, key: &str) -> io::Result<i64> {
    table.get(key).and_then(toml::Value::as_integer).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, format!("TUI preset field '{key}' must be an integer"))
    })
}
