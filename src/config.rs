use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TonepoetConfig {
    pub conversion: ConversionSettings,
    #[serde(default)]
    pub ui: UiConfig,
}

/// UI-related configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiConfig {
    /// Screen shown when the TUI starts. One of: browse, library, convert, queue, config.
    /// Case-insensitive; unknown values fall back to "browse".
    #[serde(default = "default_initial_screen")]
    pub default_screen: String,
}

fn default_initial_screen() -> String {
    "browse".to_string()
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            default_screen: default_initial_screen(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversionSettings {
    /// Preferred backend: "ffmpeg" or "sox"
    pub preferred_backend: String,
    /// Number of parallel worker threads
    pub worker_count: usize,
    /// Process priority (-20 to 19)
    pub process_priority: i8,
    /// Calculate ReplayGain after conversion
    pub calculate_replaygain: bool,
    /// Generate CUE files
    pub generate_cue_files: bool,
    /// CUE generation mode: "Always" or "IfMerging"
    pub cue_generation_mode: String,
    /// Write a conversion log file
    pub write_log_file: bool,
    /// Persist queue to disk between sessions
    pub persist_queue: bool,
    /// Default output directory
    pub default_destination: Option<PathBuf>,
    /// Scratch/temp directory for extraction
    pub scratch_directory: Option<PathBuf>,
    /// Default archive password
    pub archive_password: Option<String>,
    /// Append content from Lineage.txt to COMMENT tag
    pub append_lineage_to_comment: bool,
}

impl Default for ConversionSettings {
    fn default() -> Self {
        Self {
            preferred_backend: "ffmpeg".to_string(),
            worker_count: num_cpus::get().saturating_sub(1).max(1),
            process_priority: 0,
            calculate_replaygain: true,
            generate_cue_files: false,
            cue_generation_mode: "IfMerging".to_string(),
            write_log_file: false,
            persist_queue: true,
            default_destination: None,
            scratch_directory: None,
            archive_password: None,
            append_lineage_to_comment: false,
        }
    }
}

impl Default for TonepoetConfig {
    fn default() -> Self {
        Self {
            conversion: ConversionSettings::default(),
            ui: UiConfig::default(),
        }
    }
}

impl TonepoetConfig {
    /// Load config from the default path (~/.config/tonepoet/config.toml)
    pub fn load() -> anyhow::Result<Self> {
        let config_path = Self::config_path();
        if config_path.exists() {
            let content = std::fs::read_to_string(&config_path)?;
            let config: TonepoetConfig = toml::from_str(&content)?;
            Ok(config)
        } else {
            Ok(Self::default())
        }
    }

    /// Save config to the default path
    pub fn save(&self) -> anyhow::Result<()> {
        let config_path = Self::config_path();
        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = toml::to_string_pretty(self)?;
        std::fs::write(&config_path, content)?;
        Ok(())
    }

    /// Get the config file path
    pub fn config_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("tonepoet")
            .join("config.toml")
    }
}
