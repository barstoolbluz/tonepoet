use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TonepoetConfig {
    pub conversion: ConversionSettings,
    #[serde(default)]
    pub ui: UiConfig,
    #[serde(default)]
    pub performance: PerformanceConfig,
    #[serde(default)]
    pub browsing: BrowsingConfig,
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


/// Browse-screen view and interaction preferences.
///
/// This table is intentionally separate from `[performance.browsing]`, which
/// owns operational concerns such as archive-listing policy and timeouts.
/// Deserialization accepts missing fields for backwards compatibility; callers
/// should use [`BrowsingConfig::normalized`] before applying user-edited values.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BrowsingConfig {
    #[serde(default)]
    pub show_hidden: bool,
    #[serde(default = "default_browse_columns")]
    pub columns: Vec<String>,
    #[serde(default = "default_browse_sort")]
    pub default_sort: String,
    #[serde(default = "default_browse_sort_dir")]
    pub default_sort_dir: String,
    #[serde(default = "default_browse_filter")]
    pub default_filter: String,
    /// Whether the Explore pane is present in Browse layout at all.
    /// This is independent from `layout_explore`, which preserves the
    /// enabled pane's collapsed/open state.
    #[serde(default = "default_true")]
    pub layout_explore_enabled: bool,
    /// Whether the Info pane is present in Browse layout at all.
    /// This is independent from `layout_info`, which preserves the
    /// enabled pane's collapsed/open state.
    #[serde(default = "default_true")]
    pub layout_info_enabled: bool,
    #[serde(default = "default_browse_layout_open")]
    pub layout_explore: String,
    #[serde(default = "default_browse_layout_open")]
    pub layout_info: String,
    /// Maximum number of recursive search results retained after global sorting.
    /// The worker scores every match first; this cap is applied only after sort
    /// so late high-quality matches are not discarded by walk order.
    #[serde(default = "default_browse_search_result_cap")]
    pub search_result_cap: usize,
}

impl Default for BrowsingConfig {
    fn default() -> Self {
        Self {
            show_hidden: false,
            columns: default_browse_columns(),
            default_sort: default_browse_sort(),
            default_sort_dir: default_browse_sort_dir(),
            default_filter: default_browse_filter(),
            layout_explore_enabled: true,
            layout_info_enabled: true,
            layout_explore: default_browse_layout_open(),
            layout_info: default_browse_layout_open(),
            search_result_cap: default_browse_search_result_cap(),
        }
    }
}

impl BrowsingConfig {
    pub fn normalized(&self) -> Self {
        let mut columns = Vec::new();
        for raw in &self.columns {
            let column = normalize_browse_token(raw);
            if is_supported_browse_column(&column) && !columns.iter().any(|c| c == &column) {
                columns.push(column);
            }
        }
        if columns.is_empty() {
            columns = default_browse_columns();
        } else if !columns.iter().any(|c| c == "name") {
            columns.insert(0, "name".to_string());
        }

        let default_sort = normalize_browse_token(&self.default_sort);
        let default_sort = if is_supported_browse_sort(&default_sort) {
            default_sort
        } else {
            default_browse_sort()
        };

        let default_sort_dir = normalize_browse_token(&self.default_sort_dir);
        let default_sort_dir = match default_sort_dir.as_str() {
            "asc" | "ascending" => "asc".to_string(),
            "desc" | "descending" => "desc".to_string(),
            _ => default_browse_sort_dir(),
        };

        let default_filter = normalize_browse_token(&self.default_filter);
        let default_filter = if is_supported_browse_filter(&default_filter) {
            default_filter
        } else {
            default_browse_filter()
        };

        let layout_explore = normalize_layout_state(&self.layout_explore);
        let layout_info = normalize_layout_state(&self.layout_info);
        let search_result_cap = normalize_search_result_cap(self.search_result_cap);

        Self {
            show_hidden: self.show_hidden,
            columns,
            default_sort,
            default_sort_dir,
            default_filter,
            layout_explore_enabled: self.layout_explore_enabled,
            layout_info_enabled: self.layout_info_enabled,
            layout_explore,
            layout_info,
            search_result_cap,
        }
    }
}

fn default_browse_columns() -> Vec<String> {
    ["name", "size", "date", "type"]
        .into_iter()
        .map(str::to_string)
        .collect()
}

fn default_browse_sort() -> String { "name".to_string() }
fn default_browse_sort_dir() -> String { "asc".to_string() }
fn default_browse_filter() -> String { "all".to_string() }
fn default_true() -> bool { true }
fn default_browse_layout_open() -> String { "open".to_string() }
fn default_browse_search_result_cap() -> usize { 2000 }

fn normalize_search_result_cap(value: usize) -> usize {
    value.clamp(1, 100_000)
}

fn normalize_browse_token(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace(' ', "_").replace('-', "_")
}

fn normalize_layout_state(value: &str) -> String {
    match normalize_browse_token(value).as_str() {
        "collapsed" | "closed" | "collapse" => "collapsed".to_string(),
        _ => "open".to_string(),
    }
}

fn is_supported_browse_column(value: &str) -> bool {
    matches!(
        value,
        "name"
            | "size"
            | "date"
            | "type"
            | "format"
            | "codec"
            | "sample_rate"
            | "channels"
            | "duration"
            | "artist"
            | "album"
    )
}

fn is_supported_browse_sort(value: &str) -> bool {
    matches!(
        value,
        "name"
            | "size"
            | "date"
            | "type"
            | "format"
            | "codec"
            | "sample_rate"
            | "channels"
            | "duration"
            | "artist"
            | "album"
    )
}

fn is_supported_browse_filter(value: &str) -> bool {
    matches!(
        value,
        "all" | "off" | "audio" | "audio_only" | "flac" | "opus" | "aac" | "mp3"
            | "alac" | "wav" | "wavpack" | "aiff" | "dsf" | "dff" | "dts" | "ac3"
            | "ape" | "lpcm"
    )
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
    /// Maximum percentage of total RAM that scratch/tmpfs staging may reserve (0-90).
    #[serde(default = "default_scratch_memory_limit_percent")]
    pub scratch_memory_limit_percent: u8,
    /// Default archive password
    pub archive_password: Option<String>,
    /// Default ordered pre/post conversion action pipeline.
    #[serde(default, skip_serializing_if = "crate::convert::pipeline::ActionPipeline::is_empty")]
    pub actions: crate::convert::pipeline::ActionPipeline,
    /// Append content from Lineage.txt to COMMENT tag
    pub append_lineage_to_comment: bool,
}

pub const DEFAULT_SCRATCH_MEMORY_LIMIT_PERCENT: u8 = 50;

fn default_scratch_memory_limit_percent() -> u8 {
    DEFAULT_SCRATCH_MEMORY_LIMIT_PERCENT
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
            scratch_memory_limit_percent: default_scratch_memory_limit_percent(),
            archive_password: None,
            actions: crate::convert::pipeline::ActionPipeline::default(),
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
            browsing: BrowsingConfig::default(),
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
    fn conversion_scratch_memory_limit_defaults_when_missing_from_toml() {
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
"#,
        )
        .expect("config parses without scratch memory limit");

        assert_eq!(
            config.conversion.scratch_memory_limit_percent,
            DEFAULT_SCRATCH_MEMORY_LIMIT_PERCENT
        );
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


    #[test]
    fn browsing_defaults_when_missing_from_toml() {
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
"#,
        )
        .expect("config parses without browsing");

        assert_eq!(config.browsing, BrowsingConfig::default());
    }

    #[test]
    fn browsing_config_normalizes_user_values() {
        let config = BrowsingConfig {
            show_hidden: true,
            columns: vec!["Size".into(), "sample-rate".into(), "size".into()],
            default_sort: "nonsense".into(),
            default_sort_dir: "descending".into(),
            default_filter: "audio only".into(),
            layout_explore_enabled: false,
            layout_info_enabled: true,
            layout_explore: "closed".into(),
            layout_info: "OPEN".into(),
            search_result_cap: 0,
        }
        .normalized();

        assert_eq!(config.columns, vec!["name", "size", "sample_rate"]);
        assert_eq!(config.default_sort, "name");
        assert_eq!(config.default_sort_dir, "desc");
        assert_eq!(config.default_filter, "audio_only");
        assert!(!config.layout_explore_enabled);
        assert!(config.layout_info_enabled);
        assert_eq!(config.layout_explore, "collapsed");
        assert_eq!(config.layout_info, "open");
        assert_eq!(config.search_result_cap, 1);
        assert!(config.show_hidden);
    }

    #[test]
    fn browsing_config_round_trips_through_toml() {
        let mut config = TonepoetConfig::default();
        config.browsing.show_hidden = true;
        config.browsing.columns = vec!["name".into(), "codec".into()];
        config.browsing.default_sort = "date".into();
        config.browsing.default_sort_dir = "desc".into();
        config.browsing.default_filter = "flac".into();
        config.browsing.layout_explore_enabled = true;
        config.browsing.layout_info_enabled = false;
        config.browsing.layout_explore = "collapsed".into();
        config.browsing.search_result_cap = 4096;

        let encoded = toml::to_string_pretty(&config).expect("encode config");
        assert!(encoded.contains("[browsing]"));
        assert!(encoded.contains("show_hidden = true"));
        assert!(encoded.contains("default_sort = \"date\""));
        assert!(encoded.contains("layout_info_enabled = false"));
        assert!(encoded.contains("search_result_cap = 4096"));

        let decoded: TonepoetConfig = toml::from_str(&encoded).expect("decode config");
        assert_eq!(decoded.browsing.normalized(), config.browsing.normalized());
    }

}
