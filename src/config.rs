use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TonepoetConfig {
    pub conversion: ConversionSettings,
    #[serde(default)]
    pub ui: UiConfig,
    #[serde(default)]
    pub performance: PerformanceConfig,
}


#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerformanceConfig {
    #[serde(default)]
    pub browsing: BrowsingPerformanceConfig,
}

impl Default for PerformanceConfig {
    fn default() -> Self {
        Self {
            browsing: BrowsingPerformanceConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowsingPerformanceConfig {
    /// Controls automatic archive content listing in Browse.
    /// Valid values: "auto", "always", "never". Unknown values behave as "auto".
    #[serde(default = "default_archive_listing_mode")]
    pub archive_listing: String,
    /// Archive listing timeout in seconds. 0 disables the timeout.
    #[serde(default = "default_archive_listing_timeout")]
    pub archive_listing_timeout: u64,
}

impl Default for BrowsingPerformanceConfig {
    fn default() -> Self {
        Self {
            archive_listing: default_archive_listing_mode(),
            archive_listing_timeout: default_archive_listing_timeout(),
        }
    }
}

fn default_archive_listing_mode() -> String {
    "auto".to_string()
}

fn default_archive_listing_timeout() -> u64 {
    30
}

/// UI-related configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiConfig {
    /// Screen shown when the TUI starts. One of: browse, library, convert, queue, config.
    /// Case-insensitive; unknown values fall back to "browse".
    #[serde(default = "default_initial_screen")]
    pub default_screen: String,
    /// Default action when clicking a preset or "Last used" in the context
    /// menu. "start" = enqueue + start processing (default). "enqueue" =
    /// enqueue only. Holding Shift inverts whichever is set.
    #[serde(default = "default_convert_action")]
    pub convert_default_action: String,
    /// Whether to keep the bit-compare reference after a comparison completes.
    /// false (default) = auto-clear; true = persist until manually cleared.
    #[serde(default)]
    pub compare_keep_reference: bool,
    /// Runtime-selectable TUI theme slug. Unknown values fall back to Tokyo Night.
    #[serde(default = "crate::tui::theme::default_theme_name")]
    pub theme: String,
}

fn default_initial_screen() -> String {
    "browse".to_string()
}

fn default_convert_action() -> String {
    "start".to_string()
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            default_screen: default_initial_screen(),
            convert_default_action: default_convert_action(),
            compare_keep_reference: false,
            theme: crate::tui::theme::default_theme_name(),
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

fn default_worker_count() -> usize {
    std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1)
        .saturating_div(2)
        .max(1)
}

impl Default for ConversionSettings {
    fn default() -> Self {
        Self {
            preferred_backend: "ffmpeg".to_string(),
            worker_count: default_worker_count(),
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
            performance: PerformanceConfig::default(),
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

#[cfg(test)]
mod theme_config_tests {
    use super::*;

    #[test]
    fn ui_theme_defaults_to_tokyo_night_when_missing_from_toml() {
        let config: TonepoetConfig = toml::from_str(
            r#"
[conversion]
preferred_backend = "ffmpeg"
worker_count = 2
process_priority = 0
calculate_replaygain = true
generate_cue_files = false
cue_generation_mode = "IfMerging"
write_log_file = false
persist_queue = true
append_lineage_to_comment = false

[ui]
default_screen = "browse"
convert_default_action = "start"
compare_keep_reference = false
"#,
        )
        .expect("config parses without theme");

        assert_eq!(config.ui.theme, crate::tui::theme::default_theme_slug());
    }


    #[test]
    fn performance_browsing_defaults_when_missing_from_toml() {
        let config: TonepoetConfig = toml::from_str(
            r#"
[conversion]
preferred_backend = "ffmpeg"
worker_count = 2
process_priority = 0
calculate_replaygain = true
generate_cue_files = false
cue_generation_mode = "IfMerging"
write_log_file = false
persist_queue = true
append_lineage_to_comment = false

[ui]
default_screen = "browse"
convert_default_action = "start"
compare_keep_reference = false
"#,
        )
        .expect("config parses without performance");

        assert_eq!(config.performance.browsing.archive_listing, "auto");
        assert_eq!(config.performance.browsing.archive_listing_timeout, 30);
    }

    #[test]
    fn performance_browsing_round_trips_through_toml() {
        let mut config = TonepoetConfig::default();
        config.performance.browsing.archive_listing = "always".to_string();
        config.performance.browsing.archive_listing_timeout = 45;

        let encoded = toml::to_string_pretty(&config).expect("encode config");
        assert!(encoded.contains("[performance.browsing]"));
        assert!(encoded.contains("archive_listing = \"always\""));
        assert!(encoded.contains("archive_listing_timeout = 45"));

        let decoded: TonepoetConfig = toml::from_str(&encoded).expect("decode config");
        assert_eq!(decoded.performance.browsing.archive_listing, "always");
        assert_eq!(decoded.performance.browsing.archive_listing_timeout, 45);
    }

    #[test]
    fn ui_theme_round_trips_through_toml() {
        for palette in crate::tui::theme::palettes() {
            let mut config = TonepoetConfig::default();
            config.ui.theme = palette.slug.to_string();

            let encoded = toml::to_string_pretty(&config).expect("encode config");
            assert!(
                encoded.contains(&format!("theme = \"{}\"", palette.slug)),
                "serialized config must contain theme slug {}",
                palette.slug
            );

            let decoded: TonepoetConfig = toml::from_str(&encoded).expect("decode config");
            assert_eq!(decoded.ui.theme, palette.slug);
        }
    }

}
