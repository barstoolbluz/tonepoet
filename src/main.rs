use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use tokio::sync::broadcast;

use tonepoet::config::TonepoetConfig;
use tonepoet::convert::pipeline::DvdaDownmixPolicy;
use tonepoet::convert::{
    formats::{
        AacProfile, AudioFormat, ConversionOptions, FileFormat, FormatDetector, Mp3BitrateMode,
        QualitySettings,
    },
    ConversionConfig, ConversionItem, ConversionManager, ConversionProcessor, ConversionQueue, ConversionStatus, ProcessorConfig,
    ProgressUpdate,
};

#[derive(Parser, Debug)]
#[command(name = "tonepoet", about = "Audio conversion toolkit")]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Enable verbose logging
    #[arg(short, long, global = true)]
    verbose: bool,
}

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
enum DsfRecoveryAction {
    Status,
    RecoverTail,
    RestoreBackup,
    KeepCurrent,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Internal process-tree supervisor for conversion action scripts.
    #[command(name = "__action-script-supervisor", hide = true)]
    InternalActionScriptSupervisor {
        #[arg(long)]
        runtime_fd: i32,
        #[arg(long)]
        control_fd: i32,
        #[arg(long)]
        event_fd: i32,
        #[arg(long)]
        script_fd: i32,
        #[arg(long)]
        working_directory_fd: i32,
    },
    /// Internal exec-gated launcher used by the script supervisor.
    #[command(name = "__action-script-launcher", hide = true)]
    InternalActionScriptLauncher {
        #[arg(long)]
        launch_fd: i32,
        #[arg(long)]
        cgroup_fd: Option<i32>,
        #[arg(long)]
        script_fd: i32,
    },
    /// Convert audio files, directories, or archives
    Convert {
        /// Input files, directories, or archives
        #[arg(required = true)]
        paths: Vec<PathBuf>,

        /// Output format (flac, wav, aiff, wavpack, mp3, aac, opus)
        #[arg(short, long)]
        format: Option<String>,

        /// Target PCM bit depth (16, 24, 32, 32f, 64f, or source). With no
        /// flag, DSD/lossy sources use the target format's documented default.
        #[arg(long = "bit-depth", value_name = "16|24|32|32f|64f|source", value_parser = parse_cli_bit_depth)]
        bit_depth: Option<u32>,

        /// Output directory
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Number of concurrent workers
        #[arg(short, long)]
        workers: Option<usize>,

        /// ReplayGain mode (track, album, both, *-if-missing, off)
        #[arg(long)]
        replaygain: Option<String>,

        /// Archive password for encrypted 7z files
        #[arg(long)]
        archive_password: Option<String>,

        /// Use named preset
        #[arg(long)]
        preset: Option<String>,

        /// FLAC compression level (0-8)
        #[arg(long)]
        compression_level: Option<u8>,

        /// MP3/AAC/Opus bitrate in kbps
        #[arg(long)]
        bitrate: Option<u32>,

        /// Force re-encode FLAC files instead of copying
        #[arg(long)]
        reencode_flac: bool,

        /// Merge all tracks into a single file
        #[arg(long)]
        merge: bool,

        /// Preferred backend (ffmpeg or sox)
        #[arg(long)]
        backend: Option<String>,

        /// Append Lineage.txt content to COMMENT tag
        #[arg(long)]
        append_lineage: bool,

        /// Write conversion log file
        #[arg(long)]
        write_log: bool,

        /// Create per-disc subfolders (disc 01/, disc 02/, ...) for
        /// multi-disc sets, with batch-scope album identity resolution
        #[arg(long)]
        disc_subfolders: bool,

        /// Generate CUE files
        #[arg(long)]
        generate_cue: bool,

        // ---- Pipeline flags (PR 10) ----
        /// Select single track by number (1-based)
        #[arg(long)]
        track: Option<u32>,

        /// Select track range (e.g. "3-7", 1-based inclusive)
        #[arg(long)]
        track_range: Option<String>,

        /// SACD area selection (stereo or multichannel)
        #[arg(long)]
        area: Option<String>,

        /// Ignore CUE sheets, treat as single file
        #[arg(long)]
        no_cue: bool,

        /// Allow partial album output on track failures
        #[arg(long)]
        partial: bool,

        /// Overwrite existing output (with backup)
        #[arg(long, name = "overwrite")]
        overwrite_output: bool,

        /// Output naming template (e.g. "%NN% - %TITLE%"; disc tokens include %DISC_FOLDER%, %DISCNUMBER%, %NNDISCNUMBER%, %NNNDISCNUMBER%, %DISCTOTAL%)
        #[arg(long)]
        naming: Option<String>,
        /// Output album/folder naming template (e.g. "%ARTIST%/%ALBUM% (%YEAR%)"; disc-number tokens are available for proven multi-disc items)
        #[arg(long = "folder-naming")]
        folder_naming: Option<String>,

        /// Disable metadata tagging stage
        #[arg(long)]
        no_metadata: bool,

        /// Disable feature generation (log/CUE sidecars)
        #[arg(long)]
        no_features: bool,

        // ---- DVD-Audio flags ----
        /// DVD-Audio group/stream selection (1-9, all, stereo, multichannel, hires)
        #[arg(long = "dvda-group")]
        dvda_group: Option<String>,

        /// Treat DVD-Audio disc as already decrypted (skip CPPM probe)
        #[arg(long = "dvda-assume-decrypted")]
        dvda_assume_decrypted: bool,

        /// DVD-Audio downmix policy (auto, none, foo-compat, ffmpeg)
        #[arg(
            long = "dvda-downmix",
            value_name = "auto|none|foo-compat|ffmpeg",
            value_parser = parse_dvda_downmix_policy
        )]
        dvda_downmix: Option<DvdaDownmixPolicy>,
    },

    /// Launch full interactive TUI
    Tui {
        /// Optional audio files to load into the Convert screen on launch.
        /// A single file opens in Single mode; multiple files open as a
        /// Batch for review. If no files are given, the TUI starts on the
        /// configured default screen (Browse by default).
        ///
        /// Directory arguments are not supported in TUI mode; use
        /// `tonepoet convert <DIR>` for batch directory conversion, or
        /// launch the TUI and navigate via `:cd` on the Browse screen.
        #[arg(required = false)]
        paths: Vec<PathBuf>,
    },

    /// Launch interactive TUI wizard
    Wizard,

    /// Check availability of external audio tools
    CheckTools,

    /// Show or edit configuration
    Config {
        /// Show current config
        #[arg(long)]
        show: bool,

        /// Reset to defaults
        #[arg(long)]
        reset: bool,

        /// Show config file path
        #[arg(long)]
        path: bool,

        /// Retire a validated pending secret journal without contacting the secret backend.
        #[arg(long)]
        retire_secret_journal: bool,
    },

    /// Inspect or resolve DSF tail journals and legacy rollback markers.
    /// This command does not load the ordinary tonepoet configuration, so it
    /// remains available during configuration or secret-store recovery.
    DsfRecover {
        #[arg(value_enum)]
        action: DsfRecoveryAction,

        #[arg(required = true)]
        path: PathBuf,
    },

    /// Probe a DVD-Audio ISO or directory and print disc structure
    DvdaInfo {
        /// Path to a DVD-Audio ISO image or a directory containing AUDIO_TS/
        #[arg(required = true)]
        path: PathBuf,
    },

    /// Probe an optical disc ISO and print structure (auto-detects DVD-Audio/SACD)
    DiscInfo {
        /// Path to a disc ISO image or directory
        #[arg(required = true)]
        path: PathBuf,

        /// Show suppressed presentations (placeholders)
        #[arg(long)]
        raw: bool,

        /// Show diagnostic details
        #[arg(long)]
        verbose: bool,
    },

    /// Tag audio files, an SACD ISO, or a DVD-Video source from MusicBrainz, headless.
    ///
    /// Computes a CD-equivalent TOC from the supplied paths, looks up
    /// matching MusicBrainz releases, populates per-track tags, and
    /// writes them in place. On multi-match the command refuses with
    /// the candidate list unless `--auto` or `--release-id` is given.
    /// Mirrors the in-editor `:tags-mb` flow with no interactive UI.
    TagsMb {
        /// Audio files (FLAC, WAV, etc.) — same album, same directory.
        /// Or one SACD ISO file, DVD-Video ISO, or DVD-Video directory.
        #[arg(required = true)]
        paths: Vec<PathBuf>,

        /// Catalog number (matches MB's `catno:` Lucene clause).
        /// Quote values that contain spaces: `--catno "SRGS 4520"`.
        #[arg(long, conflicts_with = "release_id")]
        catno: Option<String>,

        /// Release year (matches MB's `date:` Lucene clause).
        #[arg(long, conflicts_with = "release_id")]
        year: Option<String>,

        /// Free-form text query (matches MB's `release:` field).
        /// When any of --catno/--year/--query is supplied, the TOC
        /// lookup is skipped in favor of a direct text search.
        #[arg(long, conflicts_with = "release_id")]
        query: Option<String>,

        /// Skip lookup entirely and fetch this MBID directly. Useful
        /// for scripted workflows that already know the release id.
        /// Mutually exclusive with --catno, --year, --query, and --auto.
        #[arg(long, conflicts_with = "auto")]
        release_id: Option<String>,

        /// On multi-match, take the highest-scoring release instead
        /// of refusing. Mutually exclusive with --release-id (which
        /// already picks a specific release).
        #[arg(long)]
        auto: bool,

        /// Show planned tag writes; write nothing. Exits 0 on success
        /// (lookup + populate succeeded), regardless of write count.
        #[arg(long)]
        dry_run: bool,

        /// Only set exit code; no stdout/stderr output beyond errors.
        #[arg(long, conflicts_with = "verbose")]
        quiet: bool,

        /// Per-file before/after diff of each tag change.
        #[arg(long)]
        verbose: bool,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Keep the containment supervisor and launcher out of Tokio entirely.
    // They deliberately own their complete child process topology; starting a
    // multithreaded runtime first would create unrelated threads and process
    // state before subreaper/cgroup/kqueue supervision is armed.
    if let Commands::InternalActionScriptSupervisor {
        runtime_fd,
        control_fd,
        event_fd,
        script_fd,
        working_directory_fd,
    } = &cli.command
    {
        tonepoet::convert::script_supervisor::run_internal_supervisor(
            *runtime_fd,
            *control_fd,
            *event_fd,
            *script_fd,
            *working_directory_fd,
        )?;
        return Ok(());
    }
    if let Commands::InternalActionScriptLauncher {
        launch_fd,
        cgroup_fd,
        script_fd,
    } = &cli.command
    {
        // The launcher inherits the SCRIPT's output descriptors; anything it
        // prints would be captured as script output. Pre-exec failures are
        // already reported through the launch readiness channel — exit
        // nonzero silently instead of letting anyhow print to stderr.
        if let Err(error) = tonepoet::convert::script_supervisor::run_internal_launcher(
            *launch_fd,
            *cgroup_fd,
            *script_fd,
        ) {
            let _ = error; // reason travels via the readiness channel
            std::process::exit(70);
        }
        return Ok(());
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async_main(cli))
}

fn require_startup_config(
    loaded: anyhow::Result<TonepoetConfig>,
) -> anyhow::Result<TonepoetConfig> {
    loaded.map_err(|error| {
        anyhow::anyhow!(
            "failed to load tonepoet configuration; archive-password secret migration or secret-store access may require user action: {error}"
        )
    })
}

fn run_dsf_recovery(action: DsfRecoveryAction, path: &std::path::Path) -> anyhow::Result<()> {
    match action {
        DsfRecoveryAction::Status => {
            let tail = tonepoet::dsf_tags::inspect_tail_journal(path)
                .map_err(anyhow::Error::msg)?;
            let legacy = tonepoet::dsf_tags::inspect_legacy_backup_if_present(path)
                .map_err(anyhow::Error::msg)?;
            if tail.is_none() && legacy.is_none() {
                return Err(anyhow::anyhow!(
                    "no DSF tail journal or legacy rollback marker exists for '{}'",
                    path.display()
                ));
            }
            if let Some(inspection) = tail {
                println!("tail target: {}", inspection.target.display());
                println!("tail journal: {}", inspection.journal.display());
                println!("tail state: {}", inspection.state);
                println!("tail operation: {}", inspection.operation);
                println!("tail original bytes: {}", inspection.original_file_size);
                println!("tail committed bytes: {}", inspection.committed_file_size);
            }
            if let Some(inspection) = legacy {
                println!("legacy target: {}", inspection.target.display());
                println!("legacy marker: {}", inspection.marker.display());
                println!("legacy target bytes: {}", inspection.target_bytes);
                println!("legacy marker bytes: {}", inspection.marker_bytes);
                println!("legacy byte-identical: {}", inspection.byte_identical);
            }
        }
        DsfRecoveryAction::RecoverTail => {
            let inspection = tonepoet::dsf_tags::recover_tail_journal_for_target(path)
                .map_err(anyhow::Error::msg)?
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "no DSF tail journal exists for '{}'",
                        path.display()
                    )
                })?;
            println!(
                "resolved {} {} DSF tail journal '{}' for '{}'",
                inspection.state,
                inspection.operation,
                inspection.journal.display(),
                inspection.target.display()
            );
        }
        DsfRecoveryAction::RestoreBackup => {
            let inspection = tonepoet::dsf_tags::resolve_legacy_backup(
                path,
                tonepoet::dsf_tags::DsfLegacyBackupResolution::RestoreBackup,
            )
            .map_err(anyhow::Error::msg)?;
            println!(
                "restored '{}' from legacy rollback marker '{}' ({} bytes)",
                inspection.target.display(),
                inspection.marker.display(),
                inspection.marker_bytes
            );
        }
        DsfRecoveryAction::KeepCurrent => {
            let inspection = tonepoet::dsf_tags::resolve_legacy_backup(
                path,
                tonepoet::dsf_tags::DsfLegacyBackupResolution::KeepCurrent,
            )
            .map_err(anyhow::Error::msg)?;
            println!(
                "kept current DSF '{}' and retired legacy rollback marker '{}'",
                inspection.target.display(),
                inspection.marker.display()
            );
        }
    }
    Ok(())
}

async fn async_main(cli: Cli) -> anyhow::Result<()> {
    let log_level = if cli.verbose { "debug" } else { "info" };
    init_logging(log_level, matches!(&cli.command, Commands::Tui { .. }));

    if let Commands::DsfRecover { action, path } = &cli.command {
        run_dsf_recovery(*action, path)?;
        return Ok(());
    }
    if let Commands::Config {
        retire_secret_journal: true,
        ..
    } = &cli.command
    {
        let config_path = TonepoetConfig::config_path();
        let at_risk = tonepoet::secret_store::retire_pending_publication_journal_headless(&config_path)
            .map_err(anyhow::Error::msg)?;
        println!(
            "Retired pending secret journal '{}'; {} referenced secret {} may remain orphaned.",
            tonepoet::secret_store::pending_publication_path(&config_path).display(),
            at_risk,
            if at_risk == 1 { "entry" } else { "entries" }
        );
        return Ok(());
    }

    let config = require_startup_config(TonepoetConfig::load())?;

    match cli.command {
        Commands::InternalActionScriptSupervisor { .. }
        | Commands::InternalActionScriptLauncher { .. } => unreachable!(),
        Commands::Tui { paths } => {
            run_tui(config, paths).await?;
        }
        Commands::Convert {
            paths,
            format,
            bit_depth,
            output,
            workers,
            replaygain,
            archive_password,
            preset,
            compression_level,
            bitrate,
            reencode_flac,
            merge,
            backend,
            append_lineage,
            write_log,
            disc_subfolders,
            generate_cue,
            track,
            track_range,
            area,
            no_cue,
            partial,
            overwrite_output,
            naming,
            folder_naming,
            no_metadata,
            no_features,
            dvda_group,
            dvda_assume_decrypted,
            dvda_downmix,
        } => {
            run_convert(
                paths,
                format,
                bit_depth,
                output,
                workers,
                replaygain,
                archive_password,
                preset,
                compression_level,
                bitrate,
                reencode_flac,
                merge,
                backend,
                append_lineage,
                write_log,
                generate_cue,
                track,
                track_range,
                area,
                no_cue,
                disc_subfolders,
                partial,
                overwrite_output,
                naming,
                folder_naming,
                no_metadata,
                no_features,
                dvda_group,
                dvda_assume_decrypted,
                dvda_downmix,
                &config,
            )
            .await?;
        }
        Commands::Wizard => {
            run_wizard(&config).await?;
        }
        Commands::CheckTools => {
            run_check_tools();
        }
        Commands::Config { show, reset, path, retire_secret_journal: _ } => {
            run_config(show, reset, path, &config)?;
        }
        Commands::DsfRecover { .. } => unreachable!("handled before config load"),
        Commands::DvdaInfo { path } => {
            run_dvda_info(&path)?;
        }
        Commands::DiscInfo { path, raw, verbose } => {
            run_disc_info(&path, raw, verbose)?;
        }
        Commands::TagsMb {
            paths,
            catno,
            year,
            query,
            release_id,
            auto,
            dry_run,
            quiet,
            verbose,
        } => {
            let exit_code = run_tags_mb(
                paths, catno, year, query, release_id, auto, dry_run, quiet, verbose,
            )
            .await;
            std::process::exit(exit_code);
        }
    }

    Ok(())
}

fn install_terminal_restore_panic_hook() {
    static INSTALL: std::sync::Once = std::sync::Once::new();

    INSTALL.call_once(|| {
        let original_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if terminal_session_is_owned_by_current_thread() {
                let _ = restore_terminal_state_after_tui();
                original_hook(info);
            } else if terminal_session_is_active() {
                // A panic on a non-owner thread while the TUI owns the terminal
                // is not safe to treat as an ordinary, recoverable JoinError:
                // the UI state may now be inconsistent, and printing a panic
                // inside the alternate screen can corrupt the display. Restore
                // the terminal once, report the panic through the original hook,
                // and fail fast instead of letting the foreground TUI continue
                // after a background task has unwound unexpectedly.
                let _ = restore_terminal_state_after_tui();
                original_hook(info);
                std::process::abort();
            } else {
                original_hook(info);
            }
        }));
    });
}

fn terminal_session_owner() -> &'static Mutex<Option<std::thread::ThreadId>> {
    static OWNER: OnceLock<Mutex<Option<std::thread::ThreadId>>> = OnceLock::new();
    OWNER.get_or_init(|| Mutex::new(None))
}

fn terminal_session_is_active() -> bool {
    terminal_session_owner()
        .lock()
        .map(|owner| owner.is_some())
        .unwrap_or(false)
}

fn terminal_session_is_owned_by_current_thread() -> bool {
    let current = std::thread::current().id();
    terminal_session_owner()
        .lock()
        .map(|owner| owner.as_ref().map(|id| *id == current).unwrap_or(false))
        .unwrap_or(false)
}

fn register_terminal_session_owner(owner: std::thread::ThreadId) {
    if let Ok(mut slot) = terminal_session_owner().lock() {
        *slot = Some(owner);
    }
}

fn clear_terminal_session_owner(owner: std::thread::ThreadId) {
    if let Ok(mut slot) = terminal_session_owner().lock() {
        if slot.as_ref().map(|id| *id == owner).unwrap_or(false) {
            *slot = None;
        }
    }
}

fn restore_terminal_state_after_tui() -> std::io::Result<()> {
    use crossterm::{
        cursor::Show,
        event::{DisableBracketedPaste, DisableMouseCapture},
        execute,
        terminal::{disable_raw_mode, LeaveAlternateScreen},
    };

    let raw_mode_result = disable_raw_mode();
    let mut stdout = std::io::stdout();
    let terminal_result = execute!(
        stdout,
        LeaveAlternateScreen,
        DisableMouseCapture,
        DisableBracketedPaste,
        Show
    );

    raw_mode_result?;
    terminal_result
}

struct TerminalRestoreGuard {
    armed: bool,
    owner_thread: std::thread::ThreadId,
}

impl TerminalRestoreGuard {
    fn armed() -> Self {
        let owner_thread = std::thread::current().id();
        register_terminal_session_owner(owner_thread);
        Self {
            armed: true,
            owner_thread,
        }
    }

    fn disarm(&mut self) {
        if self.armed {
            clear_terminal_session_owner(self.owner_thread);
            self.armed = false;
        }
    }
}

impl Drop for TerminalRestoreGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = restore_terminal_state_after_tui();
            clear_terminal_session_owner(self.owner_thread);
            self.armed = false;
        }
    }
}

/// Initialize env_logger. In TUI mode, logs are redirected to a file at
/// `~/.cache/tonepoet/tonepoet.log` so they don't corrupt the ratatui display.
/// In CLI mode, logs go to stderr as usual.
fn init_logging(level: &str, is_tui: bool) {
    let mut builder =
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(level));
    builder.format_timestamp_secs();

    if is_tui {
        let log_path = dirs::cache_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join("tonepoet")
            .join("tonepoet.log");
        if let Some(parent) = log_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
        {
            builder.target(env_logger::Target::Pipe(Box::new(file)));
        }
        // If the file can't be opened, fall back to stderr (still ugly in TUI
        // but no worse than before). try_init prevents a panic if init was
        // already called somehow; we ignore the result.
    }

    let _ = builder.try_init();
}

fn parse_format(s: &str) -> anyhow::Result<AudioFormat> {
    match s.to_lowercase().as_str() {
        "flac" => Ok(AudioFormat::Flac),
        "wav" => Ok(AudioFormat::Wav),
        "aiff" | "aif" => Ok(AudioFormat::Aiff),
        "wavpack" | "wv" => Ok(AudioFormat::WavPack),
        "mp3" => Ok(AudioFormat::Mp3),
        "aac" | "m4a" => Ok(AudioFormat::Aac),
        "opus" => Ok(AudioFormat::Opus),
        "alac" => Ok(AudioFormat::Alac),
        _ => anyhow::bail!(
            "Unknown format: {}. Supported: flac, wav, aiff, wavpack, mp3, aac, opus, alac",
            s
        ),
    }
}

fn parse_cli_bit_depth(value: &str) -> Result<u32, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "source" => Ok(0),
        "16" => Ok(16),
        "24" => Ok(24),
        "32" => Ok(32),
        "32f" | "f32" | "float32" => Ok(320),
        "64f" | "f64" | "float64" => Ok(640),
        other => Err(format!(
            "invalid bit depth '{other}'; expected 16, 24, 32, 32f, 64f, or source"
        )),
    }
}


#[cfg(test)]
mod startup_config_tests {
    use clap::Parser;

    #[test]
    fn startup_config_failure_is_returned_instead_of_defaulting() {
        let error = super::require_startup_config(Err(anyhow::anyhow!(
            "keyring backend unavailable: Secret Service is locked"
        )))
        .expect_err("startup must fail closed");

        assert_eq!(
            error.to_string(),
            "failed to load tonepoet configuration; archive-password secret migration or secret-store access may require user action: keyring backend unavailable: Secret Service is locked"
        );
    }

    #[test]
    fn config_retire_secret_journal_flag_parses_as_headless_recovery() {
        let cli = super::Cli::try_parse_from([
            "tonepoet",
            "config",
            "--retire-secret-journal",
        ])
        .expect("parse headless journal retirement");

        let super::Commands::Config {
            show,
            reset,
            path,
            retire_secret_journal,
        } = cli.command
        else {
            panic!("expected config command");
        };
        assert!(!show);
        assert!(!reset);
        assert!(!path);
        assert!(retire_secret_journal);
    }
}

#[cfg(test)]
mod bit_depth_cli_tests {
    use super::parse_cli_bit_depth;

    #[test]
    fn accepts_documented_bit_depth_values() {
        assert_eq!(parse_cli_bit_depth("source"), Ok(0));
        assert_eq!(parse_cli_bit_depth("16"), Ok(16));
        assert_eq!(parse_cli_bit_depth("24"), Ok(24));
        assert_eq!(parse_cli_bit_depth("32"), Ok(32));
        assert_eq!(parse_cli_bit_depth("32f"), Ok(320));
        assert_eq!(parse_cli_bit_depth("64f"), Ok(640));
    }

    #[test]
    fn no_depth_flag_reaches_dsd_planner_as_source_and_uses_documented_default() {
        use clap::Parser;
        use std::path::PathBuf;
        use tonepoet_pipeline::{
            AudioCodec, BitDepthTarget, DsdRate, PcmBitDepth, PlanOperation, PlanRequest,
            SampleKind, SourceInfo, SourceRepresentationKind, TopologyPlan,
        };

        let cli = super::Cli::try_parse_from([
            "tonepoet",
            "convert",
            "album.dsf",
            "--format",
            "flac",
        ])
        .expect("parse default-depth CLI");
        let super::Commands::Convert {
            format,
            bit_depth,
            ..
        } = cli.command
        else {
            panic!("expected convert command");
        };
        assert_eq!(bit_depth, None);

        let mut options = tonepoet::convert::ConversionOptions::default();
        options.output_format = super::parse_format(format.as_deref().expect("format"))
            .expect("parse FLAC format");
        options.target_bit_depth = bit_depth;
        let settings = tonepoet::convert::pipeline::pipeline_settings_from_legacy_options(&options)
            .expect("project default CLI settings");
        assert_eq!(settings.target_bit_depth, BitDepthTarget::Source);

        let request = PlanRequest {
            input_path: PathBuf::from("album.dsf"),
            output_path: PathBuf::from("album.flac"),
            source: SourceInfo {
                format: tonepoet_pipeline::AudioFormat::Dsf,
                codec: AudioCodec::Dsd,
                sample_rate_hz: Some(DsdRate::Dsd64.hz()),
                bit_depth: None,
                true_source_depth: None,
                source_representation: SourceRepresentationKind::Dsd,
                sample_kind: Some(SampleKind::Dsd),
                channels: Some(2),
                duration: None,
                audio_md5: None,
            },
            settings,
            intermediate_dir: Some(PathBuf::from("work")),
            container_ffmpeg_flags: Vec::new(),
        };
        let topology = tonepoet_pipeline::plan_topology(&request)
            .expect("no-depth CLI DSD conversion must plan");
        let TopologyPlan::Execute { steps, .. } = topology else {
            panic!("DSD to FLAC must execute");
        };
        assert!(steps.iter().any(|step| matches!(
            step.operation,
            PlanOperation::DsdToPcm {
                target_bit_depth: PcmBitDepth::Int24,
                ..
            }
        )));
    }

    #[test]
    fn rejects_undocumented_or_ambiguous_bit_depth_values() {
        for value in ["8", "20", "33", "640", "float"] {
            assert!(
                parse_cli_bit_depth(value).is_err(),
                "{value} must not be accepted as a CLI bit-depth value"
            );
        }
    }
}

fn parse_replaygain_mode(
    s: &str,
) -> Result<(
    Option<tonepoet::convert::simple_wizard::ReplayGainMode>,
    tonepoet_pipeline::ReplayGainExistingTagPolicy,
), String> {
    use tonepoet::convert::simple_wizard::ReplayGainMode;
    use tonepoet_pipeline::ReplayGainExistingTagPolicy;
    let normalized = s.trim().to_ascii_lowercase().replace('_', "-");
    let parsed = match normalized.as_str() {
        "track" => (Some(ReplayGainMode::Track), ReplayGainExistingTagPolicy::Rescan),
        "album" => (Some(ReplayGainMode::Album), ReplayGainExistingTagPolicy::Rescan),
        "both" => (Some(ReplayGainMode::Both), ReplayGainExistingTagPolicy::Rescan),
        "track-if-missing" => (Some(ReplayGainMode::Track), ReplayGainExistingTagPolicy::SkipIfComplete),
        "album-if-missing" => (Some(ReplayGainMode::Album), ReplayGainExistingTagPolicy::SkipIfComplete),
        "both-if-missing" => (Some(ReplayGainMode::Both), ReplayGainExistingTagPolicy::SkipIfComplete),
        "off" | "none" => (None, ReplayGainExistingTagPolicy::Rescan),
        _ => return Err(format!(
            "invalid ReplayGain mode '{s}'; expected track, album, both, track-if-missing, album-if-missing, both-if-missing, or off"
        )),
    };
    Ok(parsed)
}

fn parse_dvda_group(s: &str) -> tonepoet::convert::pipeline::DvdaGroupSelection {
    use tonepoet::convert::pipeline::DvdaGroupSelection;
    match s.to_lowercase().as_str() {
        "all" => DvdaGroupSelection::All,
        "stereo" => DvdaGroupSelection::PreferStereo,
        "multichannel" | "multi" | "mc" => DvdaGroupSelection::PreferMultichannel,
        "hires" | "highres" => DvdaGroupSelection::PreferHighestResolution,
        other => match other.parse::<u8>() {
            Ok(n) if n >= 1 => DvdaGroupSelection::Group(n),
            _ => {
                eprintln!(
                    "Warning: unknown --dvda-group value '{}', using default",
                    s
                );
                DvdaGroupSelection::Default
            }
        },
    }
}

fn parse_dvda_downmix_policy(s: &str) -> Result<DvdaDownmixPolicy, String> {
    let normalized = s.to_lowercase().replace('_', "-");
    match normalized.as_str() {
        "auto" => Ok(DvdaDownmixPolicy::Auto),
        "none" | "native" | "raw" | "off" => Ok(DvdaDownmixPolicy::None),
        "foo" | "foo-compat" | "foo-input-dvda" | "foo-input-dvda-compatible" => {
            Ok(DvdaDownmixPolicy::FooInputDvdaCompatible)
        }
        "ffmpeg" | "ffmpeg-default" | "ac2" | "ac-2" => Ok(DvdaDownmixPolicy::FfmpegDefault),
        _ => Err(format!(
            "invalid DVD-Audio downmix policy '{s}'; expected one of: auto, none, foo-compat, ffmpeg"
        )),
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_convert(
    paths: Vec<PathBuf>,
    format: Option<String>,
    bit_depth: Option<u32>,
    output: Option<PathBuf>,
    workers: Option<usize>,
    replaygain: Option<String>,
    archive_password: Option<String>,
    preset: Option<String>,
    compression_level: Option<u8>,
    bitrate: Option<u32>,
    reencode_flac: bool,
    merge: bool,
    backend: Option<String>,
    append_lineage: bool,
    write_log: bool,
    generate_cue: bool,
    track: Option<u32>,
    track_range: Option<String>,
    area: Option<String>,
    no_cue: bool,
    disc_subfolders: bool,
    partial: bool,
    overwrite_output: bool,
    naming: Option<String>,
    folder_naming: Option<String>,
    no_metadata: bool,
    no_features: bool,
    dvda_group: Option<String>,
    dvda_assume_decrypted: bool,
    dvda_downmix: Option<DvdaDownmixPolicy>,
    config: &TonepoetConfig,
) -> anyhow::Result<()> {
    // Load preset if specified
    let preset_options: Option<ConversionOptions> = if let Some(preset_name) = &preset {
        let preset_mgr = tonepoet_wizard::PresetManager::new()
            .map_err(|e| anyhow::anyhow!("Failed to initialize preset manager: {}", e))?;
        let preset = preset_mgr
            .load_preset(preset_name)
            .map_err(|e| anyhow::anyhow!("Failed to load preset '{}': {}", preset_name, e))?;
        Some(preset_to_options(&preset))
    } else {
        None
    };

    // Determine output format
    let output_format = if let Some(fmt_str) = &format {
        parse_format(fmt_str)?
    } else if let Some(ref opts) = preset_options {
        opts.output_format
    } else {
        AudioFormat::Flac
    };

    // Build conversion options
    let mut options = preset_options.unwrap_or_else(|| ConversionOptions {
        output_format,
        quality: output_format.default_quality(),
        ..ConversionOptions::default()
    });

    // Apply CLI overrides
    options.output_format = output_format;
    if let Some(depth) = bit_depth {
        options.target_bit_depth = Some(depth);
    }
    if let Some(dir) = &output {
        options.output_dir = Some(dir.clone());
    } else if let Some(ref dir) = config.conversion.default_destination {
        options.output_dir = Some(dir.clone());
    }
    // The config's default action pipeline applies when nothing more
    // specific set one (same rule the TUI uses for its Output Options seed).
    if options.actions.is_empty() {
        options.actions = config.conversion.actions.clone();
    }

    if let Some(rg) = &replaygain {
        let (mode, existing_tags) = parse_replaygain_mode(rg).map_err(anyhow::Error::msg)?;
        options.replaygain_mode = mode.clone();
        options.calculate_replaygain = options.replaygain_mode.is_some();
        if options.pipeline_settings.is_none() {
            options.pipeline_settings = Some(
                tonepoet::convert::pipeline::pipeline_settings_from_legacy_options(&options)
                    .map_err(anyhow::Error::msg)?,
            );
        }
        let settings = options.pipeline_settings.get_or_insert_with(Default::default);
        settings.replay_gain.mode = mode.map(|mode| match mode {
            tonepoet::convert::simple_wizard::ReplayGainMode::Track => tonepoet_pipeline::ReplayGainMode::Track,
            tonepoet::convert::simple_wizard::ReplayGainMode::Album => tonepoet_pipeline::ReplayGainMode::Album,
            tonepoet::convert::simple_wizard::ReplayGainMode::Both => tonepoet_pipeline::ReplayGainMode::Both,
        });
        settings.replay_gain.existing_tags = existing_tags;
    } else if config.conversion.calculate_replaygain {
        options.calculate_replaygain = true;
        if options.replaygain_mode.is_none() {
            options.replaygain_mode = Some(tonepoet::convert::simple_wizard::ReplayGainMode::Album);
        }
    }

    if let Some(cl) = compression_level {
        if matches!(output_format, AudioFormat::Flac) {
            options.quality = QualitySettings::Flac {
                compression_level: cl,
            };
        }
    }

    if let Some(br) = bitrate {
        match output_format {
            AudioFormat::Mp3 => {
                options.quality = QualitySettings::Mp3 {
                    bitrate_mode: Mp3BitrateMode::Cbr { bitrate: br },
                    quality: 2,
                };
            }
            AudioFormat::Aac => {
                options.quality = QualitySettings::Aac {
                    bitrate: br,
                    profile: AacProfile::Lc,
                };
            }
            AudioFormat::Opus => {
                options.quality = QualitySettings::Opus {
                    bitrate: br,
                    complexity: 10,
                };
            }
            _ => {}
        }
    }

    if reencode_flac {
        options.reencode_flac = true;
    }
    if merge {
        options.merge_to_single = true;
    }
    if let Some(ref be) = backend {
        use tonepoet_backend::Backend;
        options.preferred_backend = Some(match be.to_lowercase().as_str() {
            "sox" => Backend::Sox,
            _ => Backend::FFmpeg,
        });
    }
    options.append_lineage_to_comment =
        append_lineage || config.conversion.append_lineage_to_comment;
    options.write_log_file = write_log || config.conversion.write_log_file;
    options.create_disc_subfolders = disc_subfolders;
    options.generate_cue_files = generate_cue || config.conversion.generate_cue_files;
    if let Some(template) = &naming {
        options.naming_template = Some(template.clone());
    }
    if let Some(template) = &folder_naming {
        options.folder_template = Some(template.clone());
    }

    // Materialize the CLI's complete planner settings through the checked
    // compatibility bridge. This is where `--bit-depth source` remains Source,
    // while malformed numeric requests are rejected rather than substituted.
    let cli_pipeline_settings =
        tonepoet::convert::pipeline::pipeline_settings_from_legacy_options(&options)
            .map_err(|error| anyhow::anyhow!("invalid conversion settings: {error}"))?;
    options.pipeline_settings = Some(cli_pipeline_settings);

    // Build pipeline request template from CLI flags (PR 10).
    // If any pipeline-specific flags are set, we construct a PipelineRequest
    // and attach it to each ConversionItem so the processor uses it directly.
    let pipeline_request_template = build_pipeline_request_template(
        &output,
        &options,
        output_format,
        merge,
        &archive_password,
        &replaygain,
        track,
        track_range.as_deref(),
        area.as_deref(),
        no_cue,
        partial,
        overwrite_output,
        naming.as_deref(),
        folder_naming.as_deref(),
        no_metadata,
        no_features,
        dvda_group.as_deref(),
        dvda_assume_decrypted,
        dvda_downmix,
    );

    // Build processor
    let worker_count = workers.unwrap_or(config.conversion.worker_count);
    let processor_config = ProcessorConfig {
        worker_count,
        tool_paths: std::collections::HashMap::new(),
        default_destination_directory: options
            .output_dir
            .clone()
            .or_else(|| config.conversion.default_destination.clone()),
        scratch_directory: config.conversion.scratch_directory.clone(),
        scratch_memory_limit_percent: config.conversion.scratch_memory_limit_percent,
    };

    let mut manager_config = ConversionConfig::default();
    manager_config.default_format = output_format.clone();
    manager_config.default_options = options.clone();
    manager_config.worker_count = worker_count;
    let manager = ConversionManager::new(manager_config);
    let mut processor = ConversionProcessor::new(processor_config);

    // Set up progress channel
    let (progress_tx, progress_rx) = broadcast::channel::<ProgressUpdate>(256);
    processor.set_progress_channel(progress_tx);

    // Build queue
    let queue = manager.queue.clone();

    // Add files to queue through the same expansion heuristics the TUI uses:
    // directories expand to their queueable contents with CUE suppression
    // (split-track folders never queue their describing CUE; unsplit images
    // queue the CUE and suppress the image), deterministic ordering, and
    // deduplication across overlapping arguments. Explicitly named files keep
    // explicit semantics — naming a CUE on the command line queues it.
    // Note: expansion does not follow symlinks inside directories (matching
    // Browse); symlinked layouts need explicit file arguments.
    {
        let mut q = queue.write().await;
        let planned = plan_cli_convert_queue(&paths);
        if !planned.errors.is_empty() {
            for err in &planned.errors {
                eprintln!("Error: {err}");
            }
            tonepoet::convert::queue_expansion::cleanup_synthetic_cue_artifacts(
                &planned.synthetic_cue_artifacts,
            );
            anyhow::bail!(planned.errors.join("; "));
        }
        let planned_synthetic_cue_artifacts = planned.synthetic_cue_artifacts.clone();
        for warning in &planned.warnings {
            eprintln!("Warning: {warning}");
        }
        let needs_archive_password = planned
            .items
            .iter()
            .any(|(path, _, _)| tonepoet::is_encrypted_archive_ext(path));
        let resolved_archive_password = match resolve_cli_archive_password(
            needs_archive_password,
            &archive_password,
            config,
            // Per-entry tolerant MRU load: surface skipped references on
            // stderr (CLI equivalent of the keychain pane warning) and
            // hand the resolver only the usable passwords.
            || {
                tonepoet::tui::keychain::load_keychain_with_warnings().map(|loaded| {
                    for warning in &loaded.warnings {
                        eprintln!("Warning: {warning}");
                    }
                    loaded.passwords
                })
            },
        ) {
            Ok(password) => password,
            Err(error) => {
                tonepoet::convert::queue_expansion::cleanup_synthetic_cue_artifacts(
                    &planned_synthetic_cue_artifacts,
                );
                return Err(anyhow::anyhow!(error));
            }
        };
        for (path, format, cue_sidecar_override) in planned.items {
            add_item_to_queue(
                &mut q,
                path,
                format,
                cue_sidecar_override,
                &options,
                &resolved_archive_password,
            );
        }

        // Attach pipeline request template to each item if pipeline flags were set.
        // Otherwise, ensure each item has PipelineSettings from legacy options
        // so the scheduler's validate_full_settings_handoff() passes.
        if let Some(ref template) = pipeline_request_template {
            for item in q.all_items_mut() {
                let mut req = template.clone();
                // Per-item overrides: container path and item_id.
                req.container = item.input_path.clone();
                req.item_id = item.id.clone();
                req.job_id = format!("job-{}", item.id);
                // Carry the item's archive password if set.
                if let Some(ref pw) = item.archive_password {
                    req.source.archive_password =
                        Some(tonepoet::convert::pipeline::SecretString::new(pw.clone()));
                }
                item.pipeline_request = Some(req);
            }
        } else {
            // No pipeline-specific flags — attach PipelineSettings from legacy
            // ConversionOptions so the processor can build a PipelineRequest.
            let settings = options
                .pipeline_settings
                .clone()
                .expect("run_convert installs checked CLI pipeline settings");
            for item in q.all_items_mut() {
                item.options.pipeline_settings = Some(settings.clone());
            }
        }

        // Mark all items as queued now that settings/requests are attached.
        for item in q.all_items_mut() {
            item.status = ConversionStatus::Queued;
        }

        let total = q.all_items().len();
        if total == 0 {
            tonepoet::convert::queue_expansion::cleanup_synthetic_cue_artifacts(
                &planned_synthetic_cue_artifacts,
            );
            anyhow::bail!("No supported files found in the provided paths");
        }
        println!(
            "Queued {} item(s) for conversion to {}",
            total,
            output_format.name()
        );

        // Release the write guard before registering artifacts through the
        // manager's queue-snapshot path.  Synthetic album CUE files must be
        // owned by queue item ids, not by a free-standing CLI cleanup set, so
        // panic/unwind and future early returns still run through
        // ConversionManager's drop/removal lifecycle.
        drop(q);
        let claimed = manager
            .register_synthetic_cue_artifacts_for_current_queue_await(&planned_synthetic_cue_artifacts)
            .await
            .map_err(|error| anyhow::anyhow!(
                "synthetic CUE artifact ownership registration failed: {error}"
            ))?;
        for artifact in planned_synthetic_cue_artifacts.difference(&claimed) {
            tonepoet::convert::queue_expansion::cleanup_synthetic_cue_artifact(artifact);
        }
    }

    // Spawn progress display task
    let mut progress_rx_owned = progress_rx;
    let progress_handle = tokio::spawn(async move {
        let pb = indicatif::ProgressBar::new(100);
        pb.set_style(
            indicatif::ProgressStyle::default_bar()
                .template("{spinner:.green} [{bar:40.cyan/blue}] {pos}% {msg}")
                .unwrap()
                .progress_chars("=>-"),
        );

        while let Ok(update) = progress_rx_owned.recv().await {
            let pct = update.progress.min(100.0).max(0.0) as u64;
            pb.set_position(pct);
            match &update.status {
                ConversionStatus::Processing { message, phase, .. } => {
                    let phase_name = phase
                        .as_ref()
                        .map(|p| p.short_name())
                        .unwrap_or("Processing");
                    let msg = message.as_deref().unwrap_or("");
                    pb.set_message(format!("{}: {}", phase_name, msg));
                }
                ConversionStatus::Completed { output_path, .. } => {
                    pb.println(format!("  Completed: {}", output_path.display()));
                }
                ConversionStatus::CompletedWithActionErrors {
                    output_path, errors, ..
                } => {
                    pb.println(format!(
                        "  Completed with action errors: {}",
                        output_path.display()
                    ));
                    for error in errors {
                        pb.println(format!("    - {error}"));
                    }
                }
                ConversionStatus::Partial {
                    output_path,
                    successful,
                    failed,
                    ..
                } => {
                    pb.println(format!(
                        "  Partial ({}/{} ok): {}",
                        successful,
                        successful + failed,
                        output_path.display()
                    ));
                }
                ConversionStatus::Failed { error, .. } => {
                    pb.println(format!("  Failed: {}", error));
                }
                _ => {}
            }
        }
        pb.finish_and_clear();
    });

    // Run the conversion
    let result = processor
        .process_queue_with_progress(queue.clone(), None)
        .await;

    // Drop the processor (and its progress_tx sender) so the progress display
    // task's recv() loop terminates instead of blocking forever.  The manager
    // remains alive through summary printing and then drops, cleaning any
    // registered synthetic CUE artifacts that were not explicitly removed.
    drop(processor);

    // Wait for progress display to finish
    let _ = progress_handle.await;

    // Print summary
    {
        let q = queue.read().await;
        let completed = q.completed_items();
        let failed = q.failed_items();
        let total = q.total_items();
        println!(
            "\nConversion complete: {}/{} succeeded, {} failed",
            completed, total, failed
        );
        // A bare count is undiagnosable; name each failure once.
        for item in q.all_items() {
            if let tonepoet::convert::ConversionStatus::Failed { error, .. } = &item.status {
                eprintln!("  failed: {} — {}", item.input_path.display(), error);
            }
        }
    }

    result.map_err(|e| anyhow::anyhow!("{}", e))
}

/// Everything the CLI convert scan decided to queue, plus user-facing
/// warnings. Separated from queue mutation so the decision logic is testable.
struct PlannedCliQueue {
    items: Vec<(
        PathBuf,
        FileFormat,
        Option<tonepoet::convert::pipeline::CueSidecarPolicy>,
    )>,
    warnings: Vec<String>,
    errors: Vec<String>,
    synthetic_cue_artifacts: std::collections::HashSet<PathBuf>,
}

/// Expand CLI convert arguments through the same queue-expansion heuristics
/// the TUI uses: directories expand to their queueable contents with CUE
/// suppression (split-track folders never queue their describing CUE; unsplit
/// images queue the CUE and suppress the image), deterministic ordering, and
/// deduplication across overlapping arguments. Explicitly named files keep
/// explicit semantics — naming a CUE on the command line queues it. Expansion
/// does not follow symlinks inside directories (matching Browse); symlinked
/// layouts need explicit file arguments.
fn plan_cli_convert_queue(paths: &[PathBuf]) -> PlannedCliQueue {
    let mut warnings = Vec::new();
    let mut expansion_inputs = Vec::new();
    for path in paths {
        if !path.exists() {
            warnings.push(format!("path does not exist: {}", path.display()));
            continue;
        }
        expansion_inputs.push(path.clone());
    }

    let expansion =
        tonepoet::convert::queue_expansion::expand_paths_to_audio_with_metadata(&expansion_inputs);
    let mut errors = Vec::new();
    if !expansion.expansion_errors.is_empty() {
        if expansion.paths.is_empty() {
            errors.extend(expansion.expansion_errors.iter().cloned());
        } else {
            warnings.extend(expansion.expansion_errors.iter().cloned());
        }
    }

    // Explicit file arguments the expansion filtered out (unsupported formats)
    // still deserve the historical warning.
    for path in &expansion_inputs {
        if path.is_file()
            && !expansion.paths.iter().any(|queued| queued == path)
            && FormatDetector::detect(path).is_err()
        {
            warnings.push(format!("unsupported file format: {}", path.display()));
        }
    }

    let mut items = Vec::new();
    for path in &expansion.paths {
        if let Ok(format) = FormatDetector::detect(path) {
            let cue_sidecar_override =
                tonepoet::convert::queue_expansion::cue_sidecar_override_for_commit_path(
                    path,
                    &expansion.cue_artifact_audio,
                );
            items.push((path.clone(), format, cue_sidecar_override));
        }
    }

    PlannedCliQueue {
        items,
        warnings,
        errors,
        synthetic_cue_artifacts: expansion.synthetic_cue_artifacts,
    }
}

fn resolve_cli_archive_password<F>(
    needs_archive_password: bool,
    cli_password: &Option<String>,
    config: &TonepoetConfig,
    load_mru: F,
) -> Result<Option<String>, String>
where
    F: FnOnce() -> Result<Vec<String>, String>,
{
    if !needs_archive_password {
        return Ok(None);
    }
    if let Some(password) = cli_password
        .clone()
        .or_else(|| config.conversion.archive_password.clone())
    {
        return Ok(Some(password));
    }
    if let Some(reference) = config.conversion.archive_password_ref.as_deref() {
        return tonepoet::secret_store::get(reference)
            .map(Some)
            .map_err(|error| {
                format!(
                    "cannot resolve configured archive password before queue admission: {error}"
                )
            });
    }
    load_mru()
        .map(|passwords| passwords.into_iter().next())
        .map_err(|error| {
            format!(
                "cannot resolve stored archive passwords before queue admission: {error}"
            )
        })
}

fn add_item_to_queue(
    queue: &mut ConversionQueue,
    path: PathBuf,
    format: FileFormat,
    cue_sidecar_override: Option<tonepoet::convert::pipeline::CueSidecarPolicy>,
    options: &ConversionOptions,
    resolved_archive_password: &Option<String>,
) {
    let mut item = ConversionItem::new(path.clone(), format, options.clone());
    item.cue_sidecar_override = cue_sidecar_override;
    if tonepoet::is_encrypted_archive_ext(&path) {
        item.set_archive_password(resolved_archive_password.clone(), None);
    }
    queue.add_item_direct(item);
}

/// Build a `PipelineRequest` template from CLI flags. Returns `None` if no
/// pipeline-specific flags were set (items use the legacy routing defaults).
#[allow(clippy::too_many_arguments)]
fn build_pipeline_request_template(
    output: &Option<PathBuf>,
    options: &ConversionOptions,
    _output_format: AudioFormat,
    merge: bool,
    archive_password: &Option<String>,
    replaygain: &Option<String>,
    track: Option<u32>,
    track_range: Option<&str>,
    area: Option<&str>,
    no_cue: bool,
    partial: bool,
    overwrite_output: bool,
    naming: Option<&str>,
    folder_naming: Option<&str>,
    no_metadata: bool,
    no_features: bool,
    dvda_group: Option<&str>,
    dvda_assume_decrypted: bool,
    dvda_downmix: Option<DvdaDownmixPolicy>,
) -> Option<tonepoet::convert::pipeline::PipelineRequest> {
    use std::collections::BTreeSet;
    use tonepoet::convert::pipeline::*;

    // Only build a PipelineRequest if pipeline-specific flags are set.
    let has_pipeline_flags = track.is_some()
        || track_range.is_some()
        || area.is_some()
        || no_cue
        || partial
        || overwrite_output
        || naming.is_some()
        || folder_naming.is_some()
        || no_metadata
        || no_features
        || dvda_group.is_some()
        || dvda_assume_decrypted
        || dvda_downmix.is_some();

    if !has_pipeline_flags {
        return None;
    }

    let track_selection = if let Some(n) = track {
        let mut set = BTreeSet::new();
        set.insert(n);
        TrackSelection::Set(set)
    } else if let Some(range_str) = track_range {
        if let Some((a, b)) = range_str.split_once('-') {
            let start = a.trim().parse::<u32>().unwrap_or(1);
            let end = b.trim().parse::<u32>().unwrap_or(start);
            TrackSelection::Range { start, end }
        } else {
            TrackSelection::All
        }
    } else {
        TrackSelection::All
    };

    let sacd_area = area.map(|a| match a.to_lowercase().as_str() {
        "multichannel" | "multi" | "mc" => SacdArea::MultiChannel,
        _ => SacdArea::Stereo,
    });

    let cue_policy = if no_cue {
        CueSidecarPolicy::IgnoreCue
    } else {
        CueSidecarPolicy::PreferSidecar
    };

    let output_root = output.clone().unwrap_or_else(|| PathBuf::from("."));

    let parsed_rg = replaygain
        .as_deref()
        .and_then(|value| parse_replaygain_mode(value).ok());
    let rg_enabled = parsed_rg
        .as_ref()
        .map(|(mode, _)| mode.is_some())
        .unwrap_or(options.calculate_replaygain);

    let mut request = PipelineRequest {
        actions: tonepoet::convert::pipeline::ActionPipeline::default(),
        worker_count: None,
        scratch_staging: None,
        job_id: String::new(),     // filled per-item
        item_id: String::new(),    // filled per-item
        container: PathBuf::new(), // filled per-item
        source: SourceOptions {
            archive_password: archive_password
                .as_ref()
                .map(|p| SecretString::new(p.clone())),
            sacd_area,
            dvda_group: None,
            dvda_group_selection: dvda_group
                .map(parse_dvda_group)
                .unwrap_or(DvdaGroupSelection::Default),
            dvda_assume_decrypted,
            dvda_downmix_policy: dvda_downmix.unwrap_or(DvdaDownmixPolicy::Auto),
            dvdv_vts: None,
            dvdv_title: None,
            dvdv_audio_stream: None,
            dvdv_angle: None,
            bluray_playlist: None,
            bluray_audio_pid: None,
            bluray_audio_stream: None,
            bluray_angle: None,
            cue_sidecar: cue_policy,
            track_selection,
        },
        settings: options.pipeline_settings.clone().unwrap_or_else(|| {
            tonepoet::convert::pipeline::pipeline_settings_from_legacy_options(options)
                .expect("CLI template tests supply valid legacy conversion options")
        }),
        merge,
        output_root: output_root.clone(),
        naming: NamingPolicy {
            template: naming.unwrap_or("%NN% - %TITLE%").to_string(),
            folder_template: folder_naming.map(str::to_string),
            per_album_subdir: true,
            collision_policy: NamingCollisionPolicy::Fail,
        },
        publish: PublishPolicy {
            overwrite: if overwrite_output {
                OverwritePolicy::ReplaceWithBackup
            } else {
                OverwritePolicy::FailIfExists
            },
            same_filesystem_required: false,
            write_manifest: false,
        },
        log: LogPolicy {
            root: output_root.join(".tonepoet-logs"),
            write_for_blocked: true,
            write_json_log: false,
            write_conversion_log: options.write_log_file,
        },
        stages: StagePolicy {
            metadata: if no_metadata {
                StageRequirement::Disabled
            } else {
                StageRequirement::Enabled
            },
            replaygain: if rg_enabled {
                StageRequirement::Enabled
            } else {
                StageRequirement::Disabled
            },
            features: if no_features {
                StageRequirement::Disabled
            } else {
                StageRequirement::Enabled
            },
            generate_cue: false,
        },
        failure_policy: if partial {
            FailurePolicy::AllowPartialAlbum
        } else {
            FailurePolicy::FailAlbumOnAnyTrackFailure
        },
        container_extension: None,
        container_ffmpeg_flags: Vec::new(),
        album_batch: None,
        album_batch_track: None,
        companion: tonepoet::convert::pipeline::CompanionCopyPolicy {
            extensions: options.effective_companion_extensions(),
            folders: options.effective_companion_folders(),
            exclude_files: options.effective_companion_exclude_files(),
        },
        pre_extracted_staging: None,
        archive_metadata_overrides: Vec::new(),
        metadata_overrides: Default::default(),
        batch_resolved_identity: None,
        expected_album_track_count: None,
        suppress_incremental_conversion_log_append: false,
    };
    if let Some((mode, existing_tags)) = parsed_rg {
        request.settings.replay_gain.mode = mode.map(|mode| match mode {
            tonepoet::convert::simple_wizard::ReplayGainMode::Track => tonepoet_pipeline::ReplayGainMode::Track,
            tonepoet::convert::simple_wizard::ReplayGainMode::Album => tonepoet_pipeline::ReplayGainMode::Album,
            tonepoet::convert::simple_wizard::ReplayGainMode::Both => tonepoet_pipeline::ReplayGainMode::Both,
        });
        request.settings.replay_gain.existing_tags = existing_tags;
    }
    Some(request)
}

/// Convert a wizard ConversionPreset to ConversionOptions
fn preset_to_options(preset: &tonepoet_wizard::ConversionPreset) -> ConversionOptions {
    use tonepoet_wizard::AudioFormat as WizFormat;

    let format = match preset.selected_format {
        WizFormat::Flac => AudioFormat::Flac,
        WizFormat::Wav => AudioFormat::Wav,
        WizFormat::Aiff => AudioFormat::Aiff,
        WizFormat::Mp3 => AudioFormat::Mp3,
        WizFormat::Aac => AudioFormat::Aac,
        WizFormat::Opus => AudioFormat::Opus,
        WizFormat::WavPack => AudioFormat::WavPack,
    };

    let quality = format.default_quality();

    let replaygain_mode = preset.replaygain_mode.as_ref().map(|mode| {
        use tonepoet::convert::simple_wizard::ReplayGainMode;
        use tonepoet_wizard::ReplayGainMode as WizRG;
        match mode {
            WizRG::Track => ReplayGainMode::Track,
            WizRG::Album => ReplayGainMode::Album,
            WizRG::Both => ReplayGainMode::Both,
            WizRG::Off => return ReplayGainMode::Album, // shouldn't reach here
        }
    });

    ConversionOptions {
        output_format: format,
        quality,
        calculate_replaygain: preset
            .replaygain_mode
            .as_ref()
            .map(|m| !matches!(m, tonepoet_wizard::ReplayGainMode::Off))
            .unwrap_or(false),
        replaygain_mode,
        merge_to_single: preset.merge_to_single.unwrap_or(false),
        reencode_flac: preset.reencode_flac.unwrap_or(false),
        ..ConversionOptions::default()
    }
}

async fn run_wizard(_config: &TonepoetConfig) -> anyhow::Result<()> {
    use crossterm::{
        event::{self, EnableMouseCapture, Event, KeyCode, KeyModifiers},
        execute,
        terminal::{enable_raw_mode, EnterAlternateScreen},
    };
    use ratatui::prelude::*;

    install_terminal_restore_panic_hook();

    enable_raw_mode()?;
    let mut terminal_restore = TerminalRestoreGuard::armed();
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut wizard = tonepoet_wizard::SimpleWizard::new();

    let mut mouse_areas = tonepoet_wizard::MouseAreas::new();

    let result: Option<ConversionOptions> = loop {
        // Draw and collect mouse areas
        terminal.draw(|f| {
            mouse_areas = tonepoet_wizard::draw_wizard(f, &wizard);
        })?;

        // Handle input
        if event::poll(std::time::Duration::from_millis(50))? {
            match event::read()? {
                Event::Key(key) => {
                    if key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        break None;
                    }
                    wizard.handle_key(key);
                }
                Event::Mouse(mouse) => {
                    let button_id = mouse_areas.get_button_at(mouse.column, mouse.row);
                    wizard.handle_mouse(mouse, button_id);
                }
                _ => {}
            }
        }

        if wizard.should_exit {
            break None;
        }
        if wizard.should_start_conversion {
            let settings = tonepoet::convert::wizard_integration::extract_wizard_settings(&wizard);
            break Some(settings.1);
        }
    };

    // Restore terminal
    restore_terminal_state_after_tui()?;
    terminal.show_cursor()?;
    terminal_restore.disarm();

    if let Some(options) = result {
        println!(
            "Wizard completed. Format: {}, ReplayGain: {}",
            options.output_format.name(),
            if options.calculate_replaygain {
                "enabled"
            } else {
                "disabled"
            }
        );
        println!("Run conversion with: tonepoet convert <PATH> --preset <name>");
    } else {
        println!("Wizard cancelled.");
    }

    Ok(())
}

fn run_dvda_info(path: &std::path::Path) -> anyhow::Result<()> {
    use tonepoet::tui::dvda::*;

    if !path.exists() {
        anyhow::bail!("Path does not exist: {}", path.display());
    }

    // Open volume: directory or ISO (try UDF first, then ISO9660)
    let volume: Box<dyn DvdaVolume> = if path.is_dir() {
        // Check for AUDIO_TS.IFO in common locations
        let candidates = [
            path.join("AUDIO_TS").join("AUDIO_TS.IFO"),
            path.join("audio_ts").join("audio_ts.ifo"),
            path.join("AUDIO_TS.IFO"),
            path.join("audio_ts.ifo"),
        ];
        let has_amg = candidates.iter().any(|c| c.exists());
        if !has_amg {
            anyhow::bail!(
                "Not a DVD-Audio directory: no AUDIO_TS.IFO found in {}",
                path.display()
            );
        }
        Box::new(DirectoryDvdaVolume::new(path.to_path_buf()))
    } else {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        if !ext.eq_ignore_ascii_case("iso") {
            anyhow::bail!(
                "Expected a .iso file or directory, got: {}",
                path.display()
            );
        }
        if let Ok(vol) = IsoUdfDvdaVolume::open(path) {
            Box::new(vol)
        } else if let Ok(vol) = Iso9660DvdaVolume::open(path) {
            Box::new(vol)
        } else {
            anyhow::bail!(
                "Could not open {} as a DVD-Audio ISO (neither UDF nor ISO9660 readable)",
                path.display()
            );
        }
    };

    // Parse disc structure
    let mut disc = parse_dvda_volume(volume.as_ref())
        .map_err(|e| anyhow::anyhow!("DVD-Audio parse failed: {}", e))?;

    // Refine copy protection with AOB probe
    let _ = refine_copy_protection_from_aob_probe(volume.as_ref(), &mut disc, false);

    // Display header
    let filename = path
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or_else(|| path.to_str().unwrap_or("?"));

    let total_groups = disc.groups.len();
    let total_tracks: usize = disc.groups.iter().map(|g| tonepoet::disc::dvda_utils::group_track_count(&disc, g)).sum();

    println!("{}", filename);
    println!(
        "DVD-Audio · {} group{} · {} track{}",
        total_groups,
        if total_groups == 1 { "" } else { "s" },
        total_tracks,
        if total_tracks == 1 { "" } else { "s" },
    );

    // Copy protection
    let cp = &disc.copy_protection;
    let cp_status = match &cp.source {
        CopyProtectionSource::MkbPresence => {
            if cp.mkb_present {
                "MKB present (no AOB probe)"
            } else {
                "None"
            }
        }
        CopyProtectionSource::MkbPresentAobProbeReadable => "MKB present, AOBs readable",
        CopyProtectionSource::AobProbeNoMpegPs => "MKB present, AOBs NOT readable (CPPM encrypted)",
        CopyProtectionSource::AssumeDecryptedOverride => "MKB present, assumed decrypted (override)",
        CopyProtectionSource::NotDetected => "None",
    };
    println!("Copy protection: {}", cp_status);
    println!();

    // Display each group
    for group in &disc.groups {
        let probe = tonepoet::disc::dvda_utils::probe_group_aob_format_with_path(
            volume.as_ref(), &disc, group, Some(path),
        );
        let tracks = tonepoet::disc::dvda_utils::group_track_count(&disc, group);
        let duration_secs = tonepoet::disc::dvda_utils::group_duration_secs(&disc, group);
        let duration_str = tonepoet::disc::format_duration(duration_secs);

        let (codec_prefix, rate_str, depth_str, ch_str) = if let Some(ref p) = probe {
            let ch_label = p.channel_label.clone();
            (
                format!("{} ", p.codec),
                tonepoet::disc::format_rate(p.sample_rate),
                format!("{}-bit", p.bit_depth),
                ch_label,
            )
        } else {
            let resolved = tonepoet::disc::dvda_utils::resolve_group_format(&disc, group);
            let rate = resolved.sample_rate
                .map(tonepoet::disc::format_rate)
                .unwrap_or_else(|| "Unknown".to_string());
            let depth = resolved.bit_depth
                .map(|d| format!("{}-bit", d))
                .unwrap_or_else(|| "Unknown".to_string());
            let ch = resolved.channel_layout
                .unwrap_or_else(|| "Unknown".to_string());
            (String::new(), rate, depth, ch)
        };

        println!(
            "  Group {}: {}{}/{} {} ({} track{}, {})",
            group.group_nr,
            codec_prefix,
            rate_str,
            depth_str,
            ch_str,
            tracks,
            if tracks == 1 { "" } else { "s" },
            duration_str,
        );
    }

    Ok(())
}

fn run_disc_info(path: &std::path::Path, raw: bool, verbose: bool) -> anyhow::Result<()> {
    use std::collections::BTreeMap;
    use tonepoet::disc;

    if !path.exists() {
        anyhow::bail!("Path does not exist: {}", path.display());
    }

    // Auto-detect format: try SACD first (cheap), then DVD-Audio
    let contents = if tonepoet::tui::sacd::is_sacd_iso(path) {
        // SACD path
        let metadata = tonepoet::tui::sacd::parse_sacd_iso(path)
            .map_err(|e| anyhow::anyhow!("SACD parse failed: {}", e))?;
        let sidecar = tonepoet::tui::sacd_sidecar::find_sidecar_for_iso(path)
            .and_then(|p| tonepoet::tui::sacd_sidecar::parse_sidecar(&p).ok());
        disc::sacd_mapper::map_sacd_disc(&metadata, sidecar.as_ref(), path)
    } else if tonepoet::disc::dvda_utils::is_dvda_source(path) {
        // Try DVD-Audio
        use tonepoet::tui::dvda::*;

        let volume: Box<dyn DvdaVolume> = if path.is_dir() {
            let candidates = [
                path.join("AUDIO_TS").join("AUDIO_TS.IFO"),
                path.join("audio_ts").join("audio_ts.ifo"),
                path.join("AUDIO_TS.IFO"),
                path.join("audio_ts.ifo"),
            ];
            if !candidates.iter().any(|c| c.exists()) {
                anyhow::bail!(
                    "Unrecognized disc format: no AUDIO_TS.IFO found in {}",
                    path.display()
                );
            }
            Box::new(DirectoryDvdaVolume::new(path.to_path_buf()))
        } else {
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if !ext.eq_ignore_ascii_case("iso") {
                anyhow::bail!(
                    "Unrecognized disc format: {}",
                    path.display()
                );
            }
            if let Ok(vol) = IsoUdfDvdaVolume::open(path) {
                Box::new(vol)
            } else if let Ok(vol) = Iso9660DvdaVolume::open(path) {
                Box::new(vol)
            } else {
                anyhow::bail!(
                    "Could not open {} as a disc ISO (not SACD, not DVD-Audio)",
                    path.display()
                );
            }
        };

        let mut dvda_disc = parse_dvda_volume(volume.as_ref())
            .map_err(|e| anyhow::anyhow!("DVD-Audio parse failed: {}", e))?;
        let _ = refine_copy_protection_from_aob_probe(volume.as_ref(), &mut dvda_disc, false);

        // Probe AOBs for each group
        let mut probes = BTreeMap::new();
        for group in &dvda_disc.groups {
            if let Some(probe) = disc::dvda_utils::probe_group_aob_format_with_path(
                volume.as_ref(),
                &dvda_disc,
                group,
                Some(path),
            ) {
                probes.insert(group.group_nr, probe);
            }
        }

        disc::dvda_mapper::map_dvda_disc(&dvda_disc, &probes, path)
    } else if tonepoet::disc::dvdv_utils::is_dvdv_source(path) {
        tonepoet::disc::dvdv_utils::map_dvdv_source(path)
            .map_err(|e| anyhow::anyhow!("DVD-Video parse failed: {}", e))?
    } else {
        anyhow::bail!("Unrecognized disc format: {}", path.display());
    };

    // Display
    println!("{}", contents.label);
    println!(
        "{} · {} presentation{} · {} track{}",
        contents.format.name(),
        contents.presentations.len(),
        if contents.presentations.len() == 1 { "" } else { "s" },
        contents.presentations.iter().map(|p| p.tracks.len()).sum::<usize>(),
        if contents.presentations.iter().map(|p| p.tracks.len()).sum::<usize>() == 1 { "" } else { "s" },
    );
    println!("Copy protection: {}", contents.copy_protection.description);
    println!();

    for pres in &contents.presentations {
        let duration = disc::format_duration(pres.total_duration_secs);
        println!(
            "  {}: {} ({} track{}, {})",
            pres.id.compact_label(),
            pres.label,
            pres.tracks.len(),
            if pres.tracks.len() == 1 { "" } else { "s" },
            duration,
        );
    }

    if raw && !contents.suppressed.is_empty() {
        println!();
        println!("Suppressed:");
        for sup in &contents.suppressed {
            let id_label = sup.id.compact_label();
            println!(
                "  {}: {} track{}, {} — {}",
                id_label,
                sup.track_count,
                if sup.track_count == 1 { "" } else { "s" },
                disc::format_duration(sup.duration_secs),
                sup.reason,
            );
        }
    }

    if verbose && !contents.diagnostics.is_empty() {
        println!();
        println!("Diagnostics:");
        for diag in &contents.diagnostics {
            let severity = match diag.severity {
                disc::DiagnosticSeverity::Info => "info",
                disc::DiagnosticSeverity::Warning => "warn",
                disc::DiagnosticSeverity::Error => "error",
            };
            println!("  [{}] {}", severity, diag.message);
        }
    }

    Ok(())
}

fn run_check_tools() {
    let backend = tonepoet_backend::ConversionBackend::new(tonepoet_backend::Backend::FFmpeg);
    match backend.check_tool_availability() {
        Ok(availability) => {
            println!("Audio Conversion Tool Availability");
            println!("{}", "=".repeat(45));

            let mut tools: Vec<_> = availability.available_tools.iter().collect();
            tools.sort_by_key(|(name, _)| (*name).clone());

            for (tool, available) in &tools {
                let status = if **available { "OK" } else { "MISSING" };
                let icon = if **available { "+" } else { "-" };
                println!("  [{}] {:<15} {}", icon, tool, status);
            }

            let extra_tools: Vec<(&str, &str, Vec<&str>)> = vec![
                ("ffprobe", "Audio stream analysis", vec!["-version"]),
                ("7zz", "Archive extraction (native 7-Zip)", vec!["--help"]),
                ("7z", "Archive extraction (p7zip fallback)", vec![]),
                ("opustags", "Opus metadata editing", vec!["--help"]),
                ("wvtag", "WavPack metadata editing", vec!["--version"]),
                (
                    "AtomicParsley",
                    "M4A/AAC metadata editing",
                    vec!["--version"],
                ),
                ("ssrc", "High-quality resampling", vec![]),
            ];

            println!("\nAdditional tools:");
            for (tool, description, args) in &extra_tools {
                // For tools that always exit non-zero (ssrc), just check if the binary is found
                let available = std::process::Command::new(tool)
                    .args(args.as_slice())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .stdin(std::process::Stdio::null())
                    .status()
                    .is_ok();
                let icon = if available { "+" } else { "-" };
                let status = if available { "OK" } else { "MISSING" };
                println!("  [{}] {:<15} {} ({})", icon, tool, status, description);
            }

            println!();
            if availability.backend_functional {
                println!("Backend: FUNCTIONAL");
            } else {
                println!(
                    "Backend: NOT FUNCTIONAL - missing: {}",
                    availability.missing_critical_tools.join(", ")
                );
            }
        }
        Err(e) => {
            eprintln!("Failed to check tool availability: {}", e);
        }
    }
}

fn run_config(show: bool, reset: bool, path: bool, config: &TonepoetConfig) -> anyhow::Result<()> {
    if path {
        println!("{}", TonepoetConfig::config_path().display());
        return Ok(());
    }

    if reset {
        let mut default_config = TonepoetConfig::default();
        default_config.clear_archive_password();
        let outcome = default_config.save_with_outcome()?;
        println!("Configuration reset to defaults.");
        if let Some(warning) = outcome.warning() {
            eprintln!("Warning: {warning}");
        }
        println!("Saved to: {}", TonepoetConfig::config_path().display());
        return Ok(());
    }

    // Default: show
    if show || (!reset && !path) {
        let toml_str = toml::to_string_pretty(config)?;
        println!("# tonepoet configuration");
        println!("# {}", TonepoetConfig::config_path().display());
        println!();
        println!("{}", toml_str);
    }

    Ok(())
}

async fn run_tui(config: TonepoetConfig, cli_paths: Vec<PathBuf>) -> anyhow::Result<()> {
    use crossterm::{
        event::{EnableBracketedPaste, EnableMouseCapture},
        execute,
        terminal::{enable_raw_mode, EnterAlternateScreen},
    };
    use ratatui::prelude::*;
    use tonepoet::tui::app::AppState;
    use tonepoet::tui::event_loop::run_app;

    install_terminal_restore_panic_hook();
    tonepoet::tui::external_editor::scavenge_stale_embedded_cuesheet_edit_dirs();

    // Set up terminal
    enable_raw_mode()?;
    let mut terminal_restore = TerminalRestoreGuard::armed();
    let mut stdout = std::io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create app state
    let mut app = AppState::new(config);

    // Crash recovery: check for interrupted metadata writes from a previous session.
    let recovered = app.db.recover_stale_metadata_writes();
    if !recovered.is_empty() {
        for msg in &recovered {
            log::warn!("Metadata recovery: {}", msg);
        }
        app.set_status(&format!(
            "Recovered {} file(s) from interrupted metadata writes",
            recovered.len()
        ));
    }

    // Phase 6f: if the user launched with file args (`tonepoet tui foo.flac
    // bar.flac`), seed the Convert screen with those files and land on
    // Convert instead of the configured default screen. Invalid paths
    // (missing, directories) are logged and skipped. Routes through
    // Convert like any other enqueue source — no back door to the queue.
    if !cli_paths.is_empty() {
        app.seed_from_cli_paths(cli_paths);
    }

    // Create message channel
    let (tx, rx) = tokio::sync::mpsc::channel(256);

    // Run the event loop
    let result = run_app(&mut terminal, &mut app, tx, rx).await;

    // Restore terminal
    restore_terminal_state_after_tui()?;
    terminal.show_cursor()?;
    terminal_restore.disarm();

    result.map_err(|e| anyhow::anyhow!("{}", e))
}

/// Headless `:tags-mb`. Mirrors the in-editor flow:
/// 1. Classify paths (audio files vs single SACD ISO; reject mixes).
/// 2. Build an editor state from the paths (using the same helpers
///    the TUI editor-open path uses, minus the AppState wiring).
/// 3. Run lookup — TOC primary, or text search when --query/--catno/
///    --year are present, or direct fetch when --release-id is given.
/// 4. Disambiguate multi-match. Default: refuse + list (exit 2);
///    --auto takes the top score.
/// 5. Populate the state with MB values (`populate_editor_from_mb`).
/// 6. Regenerate CUESHEET for single-image rips, then save via the
///    shared audio/SACD helpers or a DVD-Video JSON metadata sidecar.
///
/// Returns the process exit code: 0 ok, 1 no match, 2 ambiguous,
/// 3 argument/IO error, 4 MB transport/parse error.
#[allow(clippy::too_many_arguments)]
async fn run_tags_mb(
    paths: Vec<PathBuf>,
    catno: Option<String>,
    year: Option<String>,
    query: Option<String>,
    release_id: Option<String>,
    auto: bool,
    dry_run: bool,
    quiet: bool,
    verbose: bool,
) -> i32 {
    use tonepoet::tui::{musicbrainz, probe};

    macro_rules! say { ($($arg:tt)*) => { if !quiet { println!($($arg)*); } } }
    macro_rules! err { ($($arg:tt)*) => { eprintln!($($arg)*); } }

    // ── classify paths ──────────────────────────────────────────────
    let kind = match classify_tags_mb_paths(&paths) {
        Ok(k) => k,
        Err(e) => {
            err!("tags-mb: {}", e);
            return 3;
        }
    };

    // ── build state ────────────────────────────────────────────────
    let mut state = match kind {
        PathKind::SacdIso(ref iso) => match build_sacd_state_for_cli(iso) {
            Ok(s) => s,
            Err(e) => {
                err!("tags-mb: {}", e);
                return 3;
            }
        },
        PathKind::DvdVideoSource(ref source) => match build_dvdv_state_for_cli(source) {
            Ok(s) => s,
            Err(e) => {
                err!("tags-mb: {}", e);
                return 3;
            }
        },
        PathKind::Audio(ref audio_paths) => match build_audio_state_for_cli(audio_paths) {
            Ok(s) => s,
            Err(e) => {
                err!("tags-mb: {}", e);
                return 3;
            }
        },
    };

    // ── read-only refusal (SACD where parent dir isn't writable) ──
    if state.read_only && !dry_run {
        err!("tags-mb: destination is read-only (sidecar dir not writable)");
        return 3;
    }

    // ── lookup ─────────────────────────────────────────────────────
    let n_tracks = match kind {
        PathKind::SacdIso(_) | PathKind::DvdVideoSource(_) => state.active_surface().paths.len(), // disc source replicated × n_tracks
        PathKind::Audio(ref a) => a.len(),
    };

    let db = match tonepoet::db::Database::open() {
        Ok(d) => d,
        Err(e) => {
            err!("tags-mb: open DB: {}", e);
            return 3;
        }
    };

    let release = match resolve_release(
        &db,
        &state,
        &kind,
        n_tracks,
        query.as_deref(),
        catno.as_deref(),
        year.as_deref(),
        release_id.as_deref(),
        auto,
        quiet,
    )
    .await
    {
        Ok(Some(r)) => r,
        Ok(None) => return 1, // no-match status was already printed
        Err(rc) => return rc, // ambiguous (2), MB err (4), etc.
    };

    say!(
        "Matched: {} — {} ({})",
        release.artist,
        release.title,
        release.year.as_deref().unwrap_or("?"),
    );

    // ── populate + save ────────────────────────────────────────────
    // Phase C item 3: surface track-count divergence as a non-fatal
    // stderr warning before populate. Emit to stderr (not stdout) so
    // `--quiet` doesn't hide it — a partial-tag write is the kind of
    // thing scripts should still see.
    if let Some(warn) = musicbrainz::track_count_mismatch_message(&state, &release) {
        err!("tags-mb: {}", warn);
    }
    let mb_mutation_report = musicbrainz::populate_editor_from_mb(&mut state, &release);
    if mb_mutation_report.collapsed_carrier_count() > 0 {
        let mut warning = "tags-mb".to_string();
        mb_mutation_report.append_collapse_warning(&mut warning);
        err!("{}", warning);
    }

    if matches!(kind, PathKind::Audio(_)) {
        if let Err(e) = tonepoet::tui::keybindings::regenerate_cuesheet_for_save(&mut state) {
            err!("tags-mb: cuesheet regen: {}", e);
            return 3;
        }
    }

    if dry_run {
        let count = state
            .active_surface()
            .entries
            .iter()
            .filter(|e| {
                e.value != e.original
                    || e.per_file_values
                        .iter()
                        .zip(e.per_file_originals.iter())
                        .any(|(v, o)| v != o)
            })
            .count();
        say!(
            "Dry run: {} editor entries differ from on-file values; no writes.",
            count
        );
        if verbose {
            for e in &state.active_surface().entries {
                let dirty = e.value != e.original
                    || e.per_file_values
                        .iter()
                        .zip(e.per_file_originals.iter())
                        .any(|(v, o)| v != o);
                if dirty {
                    say!("  {}: \"{}\" → \"{}\"", e.display_key, e.original, e.value);
                }
            }
        }
        return 0;
    }

    match kind {
        PathKind::SacdIso(_) => {
            let sidecar_path = match state.sacd_sidecar_path.clone() {
                Some(p) => p,
                None => {
                    err!("tags-mb: SACD editor has no sidecar target");
                    return 3;
                }
            };
            match tonepoet::tui::keybindings::save_sacd_sidecar(&state, &sidecar_path) {
                Ok(outcome) => {
                    let verb = match outcome.kind {
                        tonepoet::tui::keybindings::SacdSaveKind::Created => "created",
                        tonepoet::tui::keybindings::SacdSaveKind::Updated => "updated",
                    };
                    // Phase D mirror outcome: surface if both areas
                    // got touched, mirror coverage on divergence.
                    let mirror_note = if !outcome.mirror.sibling_present {
                        String::new()
                    } else if outcome.mirror.mirrored_count == outcome.mirror.sibling_total {
                        " (stereo + MCH areas linked)".to_string()
                    } else {
                        format!(
                            " (sibling area: {}/{} tracks mirrored)",
                            outcome.mirror.mirrored_count, outcome.mirror.sibling_total,
                        )
                    };
                    say!(
                        "SACD sidecar {}: {}{}",
                        verb,
                        sidecar_path.display(),
                        mirror_note
                    );
                    0
                }
                Err(e) => {
                    err!("tags-mb: sidecar save failed: {}", e);
                    3
                }
            }
        }
        PathKind::DvdVideoSource(ref source) => {
            match tonepoet::tui::command::save_dvdv_metadata_sidecar(source, &state) {
                Ok(sidecar_path) => {
                    say!("DVD-Video sidecar written: {}", sidecar_path.path.display());
                    0
                }
                Err(e) => {
                    err!("tags-mb: DVD-Video sidecar save failed: {}", e);
                    3
                }
            }
        }
        PathKind::Audio(ref audio_paths) => {
            let entries_snap: Vec<(lofty::tag::ItemKey, Vec<String>, Vec<String>)> = state
                .active_surface()
                .entries
                .iter()
                .map(|e| {
                    (
                        e.item_key.clone(),
                        e.per_file_values.clone(),
                        e.per_file_originals.clone(),
                    )
                })
                .collect();
            let deleted: Vec<usize> = state.active_surface().deleted.clone();
            let paths_owned = audio_paths.clone();
            let results = match tokio::task::spawn_blocking(move || {
                probe::apply_audio_tag_changes(&paths_owned, &entries_snap, &deleted)
            })
            .await
            {
                Ok(r) => r,
                Err(e) => {
                    err!("tags-mb: save task panic: {}", e);
                    return 3;
                }
            };
            let mut wrote = 0usize;
            let mut failed = 0usize;
            for (path, r) in &results {
                match r {
                    Ok(()) => {
                        wrote += 1;
                        if verbose {
                            say!("  wrote: {}", path.display());
                        }
                    }
                    Err(e) => {
                        failed += 1;
                        err!("  failed: {}: {}", path.display(), e);
                    }
                }
            }
            if failed > 0 {
                say!("{} file(s) written, {} failed.", wrote, failed);
                3
            } else {
                say!("{} file(s) written.", wrote);
                0
            }
        }
    }
}

#[derive(Debug)]
enum PathKind {
    Audio(Vec<PathBuf>),
    SacdIso(PathBuf),
    DvdVideoSource(PathBuf),
}

fn classify_tags_mb_paths(paths: &[PathBuf]) -> Result<PathKind, String> {
    if paths.is_empty() {
        return Err("no paths supplied".to_string());
    }

    if paths.len() == 1 {
        let path = paths[0].clone();
        if !path.exists() {
            return Err(format!("path not found: {}", path.display()));
        }
        if path.is_dir() && tonepoet::disc::dvdv_utils::is_dvdv_directory(&path) {
            return Ok(PathKind::DvdVideoSource(path));
        }
        if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("iso")) {
            if tonepoet::disc::dvdv_utils::is_dvdv_iso(&path) {
                return Ok(PathKind::DvdVideoSource(path));
            }
            // DVD-Audio ISOs would otherwise fall through to the SACD parser
            // and die on a misleading "not a valid SACD ISO (no Master TOC
            // magic)" error.
            if tonepoet::disc::dvda_utils::is_dvda_iso(&path) {
                return Err(format!(
                    "{} is a DVD-Audio ISO; CLI tags-mb does not support DVD-Audio yet — use the TUI (:tags-mb from the DVD-Audio editor)",
                    path.display()
                ));
            }
            return Ok(PathKind::SacdIso(path));
        }
    } else if paths.iter().any(|p| {
        p.extension().is_some_and(|e| e.eq_ignore_ascii_case("iso"))
            || (p.is_dir() && tonepoet::disc::dvdv_utils::is_dvdv_directory(p))
    }) {
        return Err("disc sources must be passed alone (no mixed paths)".to_string());
    }

    let mut dirs: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    for p in paths {
        if !p.exists() {
            return Err(format!("file not found: {}", p.display()));
        }
        if let Some(d) = p.parent() {
            dirs.insert(d.to_path_buf());
        }
    }
    if dirs.len() > 1 {
        return Err("audio file paths must share a single directory".to_string());
    }
    Ok(PathKind::Audio(paths.to_vec()))
}

fn build_audio_state_for_cli(
    paths: &[PathBuf],
) -> Result<tonepoet::tui::app::MetadataEditorState, String> {
    use tonepoet::tui::{app, keybindings, probe};
    let mut paths = paths.to_vec();
    let mut entries =
        probe::read_all_tags_merged(&paths).map_err(|e| format!("read tags: {}", e))?;
    if paths.len() == 1 {
        keybindings::inject_sidecar_cuesheet_if_present(&mut entries, &paths[0]);
        keybindings::apply_embedded_cuesheet_per_track(&mut entries);
    }
    probe::sort_paths_and_entries_by_track(&mut paths, &mut entries);
    let n = paths.len();
    let file_labels: Vec<String> = (1..=n).map(|i| format!("{:>02}", i)).collect();
    Ok(app::MetadataEditorState::for_files(
        paths,
        entries,
        file_labels,
        app::MetadataTechnicalDetails::default(),
    ))
}

fn build_sacd_state_for_cli(
    iso: &std::path::Path,
) -> Result<tonepoet::tui::app::MetadataEditorState, String> {
    use tonepoet::tui::{keybindings, sacd, sacd_sidecar};
    let md = sacd::parse_sacd_iso(iso).map_err(|e| format!("parse SACD ISO: {}", e))?;
    let sidecar_path = sacd_sidecar::find_sidecar_for_iso(iso);
    let sidecar = sidecar_path
        .as_ref()
        .and_then(|p| sacd_sidecar::parse_sidecar(p).ok());
    let (state, _label, _n) = keybindings::build_sacd_editor_state(iso, &md, sidecar.as_ref())
        .map_err(|e| format!("build SACD editor state: {}", e))?;
    Ok(state)
}

fn build_dvdv_state_for_cli(
    source: &std::path::Path,
) -> Result<tonepoet::tui::app::MetadataEditorState, String> {
    use tonepoet::tui::app;

    let sectors = tonepoet::tui::command::dvdv_source_to_cd_sectors(source)?;
    let n_tracks = sectors
        .len()
        .checked_sub(1)
        .ok_or_else(|| "DVD-Video MusicBrainz TOC has no tracks".to_string())?;

    let sidecar_writable = tonepoet::tui::command::dvdv_metadata_sidecar_target_is_writable(source);
    let sidecar_path = tonepoet::tui::command::dvdv_metadata_sidecar_path_for_source(source).ok();

    let mut state = app::MetadataEditorState::for_files(
        std::iter::repeat(source.to_path_buf()).take(n_tracks).collect(),
        Vec::new(),
        (1..=n_tracks).map(|i| format!("{:>02}", i)).collect(),
        app::MetadataTechnicalDetails::default(),
    );
    state.read_only = !sidecar_writable;
    state.sacd_sidecar_path = sidecar_path;
    Ok(state)
}

/// Run the right MB lookup mode for the CLI args, disambiguate, and
/// return the chosen release. `Ok(None)` means "no match" (caller
/// returns 1). `Err(code)` is a direct process exit code.
#[allow(clippy::too_many_arguments)]
async fn resolve_release(
    db: &tonepoet::db::Database,
    state: &tonepoet::tui::app::MetadataEditorState,
    kind: &PathKind,
    n_tracks: usize,
    query: Option<&str>,
    catno: Option<&str>,
    year: Option<&str>,
    release_id: Option<&str>,
    auto: bool,
    quiet: bool,
) -> Result<Option<tonepoet::tui::musicbrainz::MbRelease>, i32> {
    use tonepoet::tui::musicbrainz;

    macro_rules! say { ($($arg:tt)*) => { if !quiet { println!($($arg)*); } } }

    if let Some(mbid) = release_id {
        let cached = db.get_cached_mb_search(&musicbrainz::detail_cache_key(mbid));
        let outcome = musicbrainz::fetch_release_detail(mbid, n_tracks, cached)
            .await
            .map_err(|e| {
                eprintln!("tags-mb: fetch release: {}", e);
                4
            })?;
        if let Some((k, v)) = outcome.cache_write {
            let _ = db.store_mb_search(&k, &v);
        }
        return Ok(outcome.release);
    }

    let explicit = query.is_some() || catno.is_some() || year.is_some();
    if explicit {
        let mut cached: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        let key = musicbrainz::search_cache_key("", query.unwrap_or(""), catno, year);
        if let Some(b) = db.get_cached_mb_search(&key) {
            cached.insert(key, b);
        }
        let outcome = musicbrainz::search_releases_by_query(
            "",
            query.unwrap_or(""),
            catno,
            year,
            n_tracks,
            cached,
        )
        .await
        .map_err(|e| {
            eprintln!("tags-mb: search: {}", e);
            4
        })?;
        for (k, v) in &outcome.cache_writes {
            let _ = db.store_mb_search(k, v);
        }
        return disambiguate(outcome.releases, auto, quiet, "search");
    }

    let synthesized_toc = !matches!(kind, PathKind::Audio(_));
    let sectors: Vec<u32> = match kind {
        PathKind::Audio(paths) => {
            let dir = paths[0]
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."));
            match tonepoet::tui::accuraterip::find_toc_offsets(dir) {
                Some(s) => s,
                None => {
                    let (sample_counts, sample_rate) =
                        tonepoet::tui::accuraterip::collect_sample_counts(paths).map_err(|e| {
                            eprintln!("tags-mb: {}", e);
                            3
                        })?;
                    let samples_per_frame = (sample_rate / 75) as u64;
                    let mut s = Vec::with_capacity(sample_counts.len() + 1);
                    let mut frame: u64 = 150;
                    for &c in &sample_counts {
                        s.push(frame as u32);
                        frame += c / samples_per_frame;
                    }
                    s.push(frame as u32);
                    s
                }
            }
        }
        PathKind::SacdIso(_) => {
            let surface = state.active_surface();
            let durations = match surface.sacd_area_kind {
                Some(tonepoet::tui::sacd::AreaKind::Stereo) => {
                    surface.sacd_stereo_durations.as_deref()
                }
                Some(tonepoet::tui::sacd::AreaKind::MultiChannel) => {
                    surface.sacd_multi_channel_durations.as_deref()
                }
                None => None,
            };
            let durations = durations.ok_or_else(|| {
                eprintln!("tags-mb: SACD has no per-track durations (TRL sectors malformed?)");
                3
            })?;
            tonepoet::tui::command::sacd_durations_to_sectors(durations)
        }
        PathKind::DvdVideoSource(path) => {
            tonepoet::tui::command::dvdv_source_to_cd_sectors(path).map_err(|e| {
                eprintln!("tags-mb: DVD-Video TOC: {}", e);
                3
            })?
        }
    };
    if musicbrainz::build_mb_toc(&sectors).is_none() {
        eprintln!("tags-mb: TOC too short");
        return Err(3);
    }
    // Synthesized TOCs (SACD/DVD-A/DVD-V durations) get the stub-drop
    // cascade; real rip geometry stays a single exact candidate.
    let candidates = if synthesized_toc {
        musicbrainz::toc_candidates_from_sectors(&sectors)
    } else {
        vec![musicbrainz::TocCandidate::exact(sectors)]
    };
    if candidates.len() > 1 {
        say!(
            "TOC lookup ({} tracks, {} stub-drop stages)...",
            n_tracks,
            candidates.len()
        );
    } else {
        say!("TOC lookup ({} tracks)...", n_tracks);
    }
    let cached: Vec<Option<String>> = candidates
        .iter()
        .map(|candidate| {
            musicbrainz::build_mb_toc(&candidate.sectors)
                .and_then(|toc| db.get_cached_mb_response(&toc))
        })
        .collect();
    let outcome = musicbrainz::lookup_release_by_toc_cascading(&candidates, cached)
        .await
        .map_err(|e| {
            eprintln!("tags-mb: TOC lookup: {}", e);
            4
        })?;
    for (toc, body) in &outcome.cache_writes {
        let _ = db.store_mb_response(toc, body);
    }
    if !outcome.dropped_source_indices.is_empty() {
        let ordinals: Vec<String> = outcome
            .dropped_source_indices
            .iter()
            .map(|i| format!("#{}", i + 1))
            .collect();
        say!(
            "matched after excluding sub-4s stub track(s) {}",
            ordinals.join(", ")
        );
    }
    disambiguate(outcome.releases, auto, quiet, "TOC")
}

#[cfg(test)]
mod tags_mb_cli_tests {
    use super::*;

    #[test]
    fn classify_rejects_empty_paths() {
        let r = classify_tags_mb_paths(&[]);
        assert!(r.is_err());
    }

    #[test]
    fn classify_rejects_mixed_iso_and_audio() {
        let td = tempfile::tempdir().unwrap();
        let iso = td.path().join("a.iso");
        let flac = td.path().join("b.flac");
        std::fs::write(&iso, b"").unwrap();
        std::fs::write(&flac, b"").unwrap();
        let r = classify_tags_mb_paths(&[iso, flac]);
        assert!(r.is_err());
    }

    #[test]
    fn classify_rejects_multi_directory_audio() {
        let td = tempfile::tempdir().unwrap();
        let dir_a = td.path().join("a");
        let dir_b = td.path().join("b");
        std::fs::create_dir(&dir_a).unwrap();
        std::fs::create_dir(&dir_b).unwrap();
        let f_a = dir_a.join("01.flac");
        let f_b = dir_b.join("02.flac");
        std::fs::write(&f_a, b"").unwrap();
        std::fs::write(&f_b, b"").unwrap();
        let r = classify_tags_mb_paths(&[f_a, f_b]);
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("single directory"));
    }

    #[test]
    fn classify_accepts_audio_files_in_same_dir() {
        let td = tempfile::tempdir().unwrap();
        let f1 = td.path().join("01.flac");
        let f2 = td.path().join("02.flac");
        std::fs::write(&f1, b"").unwrap();
        std::fs::write(&f2, b"").unwrap();
        match classify_tags_mb_paths(&[f1.clone(), f2.clone()]) {
            Ok(PathKind::Audio(v)) => assert_eq!(v, vec![f1, f2]),
            other => panic!(
                "expected Audio, got {:?}",
                match other {
                    Ok(PathKind::SacdIso(_)) => "SacdIso".to_string(),
                    Ok(PathKind::DvdVideoSource(_)) => "DvdVideoSource".to_string(),
                    Err(e) => format!("Err({})", e),
                    _ => "?".to_string(),
                }
            ),
        }
    }

    #[test]
    fn classify_accepts_single_iso() {
        let td = tempfile::tempdir().unwrap();
        let iso = td.path().join("disc.iso");
        std::fs::write(&iso, b"").unwrap();
        match classify_tags_mb_paths(&[iso.clone()]) {
            Ok(PathKind::SacdIso(p)) => assert_eq!(p, iso),
            _ => panic!("expected SacdIso"),
        }
    }

    #[test]
    fn classify_rejects_nonexistent_paths() {
        let r = classify_tags_mb_paths(&[PathBuf::from("/nonexistent.flac")]);
        assert!(r.is_err());
        let r = classify_tags_mb_paths(&[PathBuf::from("/nonexistent.iso")]);
        assert!(r.is_err());
    }
}

fn disambiguate(
    releases: Vec<tonepoet::tui::musicbrainz::MbRelease>,
    auto: bool,
    quiet: bool,
    source: &str,
) -> Result<Option<tonepoet::tui::musicbrainz::MbRelease>, i32> {
    match releases.len() {
        0 => {
            if !quiet {
                println!("No MusicBrainz release matched the {}.", source);
            }
            Ok(None)
        }
        1 => Ok(Some(releases.into_iter().next().unwrap())),
        _ if auto => Ok(Some(releases.into_iter().next().unwrap())),
        n => {
            eprintln!(
                "tags-mb: {} candidate releases matched. Re-run with --release-id <MBID> or --auto:",
                n,
            );
            for r in releases.iter().take(10) {
                eprintln!(
                    "  [{}]  {} — {} ({})",
                    r.release_id,
                    r.artist,
                    r.title,
                    r.year.as_deref().unwrap_or("?"),
                );
            }
            if releases.len() > 10 {
                eprintln!("  …and {} more.", releases.len() - 10);
            }
            Err(2)
        }
    }
}

#[cfg(test)]
mod pipeline_cli_tests {
    use super::*;

    fn default_options() -> ConversionOptions {
        ConversionOptions {
            output_format: AudioFormat::Flac,
            quality: AudioFormat::Flac.default_quality(),
            ..ConversionOptions::default()
        }
    }

    #[test]
    fn no_pipeline_flags_returns_none() {
        let result = build_pipeline_request_template(
            &None,
            &default_options(),
            AudioFormat::Flac,
            false,
            &None,
            &None,
            None,
            None,
            None,
            false,
            false,
            false,
            None,
            None,
            false,
            false,
            None,
            false,
            None,
        );
        assert!(result.is_none());
    }

    #[test]
    fn track_flag_maps_to_set() {
        let req = build_pipeline_request_template(
            &None,
            &default_options(),
            AudioFormat::Flac,
            false,
            &None,
            &None,
            Some(5),
            None,
            None,
            false,
            false,
            false,
            None,
            None,
            false,
            false,
            None,
            false,
            None,
        )
        .unwrap();
        assert!(matches!(req.source.track_selection,
            tonepoet::convert::pipeline::TrackSelection::Set(ref s) if s.contains(&5) && s.len() == 1
        ));
    }

    #[test]
    fn track_range_flag_maps_to_range() {
        let req = build_pipeline_request_template(
            &None,
            &default_options(),
            AudioFormat::Flac,
            false,
            &None,
            &None,
            None,
            Some("3-7"),
            None,
            false,
            false,
            false,
            None,
            None,
            false,
            false,
            None,
            false,
            None,
        )
        .unwrap();
        assert!(matches!(
            req.source.track_selection,
            tonepoet::convert::pipeline::TrackSelection::Range { start: 3, end: 7 }
        ));
    }

    #[test]
    fn area_flag_maps_to_sacd_area() {
        let req = build_pipeline_request_template(
            &None,
            &default_options(),
            AudioFormat::Flac,
            false,
            &None,
            &None,
            None,
            None,
            Some("multichannel"),
            false,
            false,
            false,
            None,
            None,
            false,
            false,
            None,
            false,
            None,
        )
        .unwrap();
        assert_eq!(
            req.source.sacd_area,
            Some(tonepoet::convert::pipeline::SacdArea::MultiChannel)
        );
    }

    #[test]
    fn no_cue_flag_maps_to_ignore_cue() {
        let req = build_pipeline_request_template(
            &None,
            &default_options(),
            AudioFormat::Flac,
            false,
            &None,
            &None,
            None,
            None,
            None,
            true,
            false,
            false,
            None,
            None,
            false,
            false,
            None,
            false,
            None,
        )
        .unwrap();
        assert_eq!(
            req.source.cue_sidecar,
            tonepoet::convert::pipeline::CueSidecarPolicy::IgnoreCue
        );
    }

    #[test]
    fn partial_flag_maps_to_allow_partial() {
        let req = build_pipeline_request_template(
            &None,
            &default_options(),
            AudioFormat::Flac,
            false,
            &None,
            &None,
            None,
            None,
            None,
            false,
            true,
            false,
            None,
            None,
            false,
            false,
            None,
            false,
            None,
        )
        .unwrap();
        assert_eq!(
            req.failure_policy,
            tonepoet::convert::pipeline::FailurePolicy::AllowPartialAlbum
        );
    }

    #[test]
    fn overwrite_flag_maps_to_replace_with_backup() {
        let req = build_pipeline_request_template(
            &None,
            &default_options(),
            AudioFormat::Flac,
            false,
            &None,
            &None,
            None,
            None,
            None,
            false,
            false,
            true,
            None,
            None,
            false,
            false,
            None,
            false,
            None,
        )
        .unwrap();
        assert_eq!(
            req.publish.overwrite,
            tonepoet::convert::pipeline::OverwritePolicy::ReplaceWithBackup
        );
    }

    #[test]
    fn naming_flag_maps_to_template() {
        let req = build_pipeline_request_template(
            &None,
            &default_options(),
            AudioFormat::Flac,
            false,
            &None,
            &None,
            None,
            None,
            None,
            false,
            false,
            false,
            Some("{nn} - {title}"),
            None,
            false,
            false,
            None,
            false,
            None,
        )
        .unwrap();
        assert_eq!(req.naming.template, "{nn} - {title}");
    }

    #[test]
    fn stage_flags_disable_stages() {
        let req = build_pipeline_request_template(
            &None,
            &default_options(),
            AudioFormat::Flac,
            false,
            &None,
            &None,
            None,
            None,
            None,
            false,
            false,
            false,
            None,
            None,
            true,
            true,
            None,
            false,
            None,
        )
        .unwrap();
        assert_eq!(
            req.stages.metadata,
            tonepoet::convert::pipeline::StageRequirement::Disabled
        );
        assert_eq!(
            req.stages.features,
            tonepoet::convert::pipeline::StageRequirement::Disabled
        );
    }

    #[test]
    fn replaygain_off_disables_rg_stage() {
        let req = build_pipeline_request_template(
            &None,
            &default_options(),
            AudioFormat::Flac,
            false,
            &None,
            &Some("off".to_string()),
            None,
            None,
            None,
            false,
            false,
            false,
            None,
            None,
            false,
            true,
            None,
            false,
            None,
        )
        .unwrap();
        assert_eq!(
            req.stages.replaygain,
            tonepoet::convert::pipeline::StageRequirement::Disabled
        );
    }

    #[test]
    fn dvda_group_flag_triggers_pipeline_request() {
        let req = build_pipeline_request_template(
            &None,
            &default_options(),
            AudioFormat::Flac,
            false,
            &None,
            &None,
            None,
            None,
            None,
            false,
            false,
            false,
            None,
            None,
            false,
            false,
            Some("stereo"),
            false,
            None,
        );
        assert!(req.is_some());
        let req = req.unwrap();
        assert_eq!(
            req.source.dvda_group_selection,
            tonepoet::convert::pipeline::DvdaGroupSelection::PreferStereo
        );
    }

    #[test]
    fn dvda_assume_decrypted_flag_triggers_pipeline_request() {
        let req = build_pipeline_request_template(
            &None,
            &default_options(),
            AudioFormat::Flac,
            false,
            &None,
            &None,
            None,
            None,
            None,
            false,
            false,
            false,
            None,
            None,
            false,
            false,
            None,
            true,
            None,
        );
        assert!(req.is_some());
        assert!(req.unwrap().source.dvda_assume_decrypted);
    }

    #[test]
    fn dvda_downmix_flag_triggers_pipeline_request() {
        let req = build_pipeline_request_template(
            &None,
            &default_options(),
            AudioFormat::Flac,
            false,
            &None,
            &None,
            None,
            None,
            None,
            false,
            false,
            false,
            None,
            None,
            false,
            false,
            None,
            false,
            Some(DvdaDownmixPolicy::FooInputDvdaCompatible),
        );
        assert!(req.is_some());
        assert_eq!(
            req.unwrap().source.dvda_downmix_policy,
            tonepoet::convert::pipeline::DvdaDownmixPolicy::FooInputDvdaCompatible
        );
    }


    #[test]
    fn dvda_downmix_clap_rejects_unknown_values() {
        let err = Cli::try_parse_from([
            "tonepoet",
            "convert",
            "disc.iso",
            "--dvda-downmix",
            "surprise",
        ])
        .expect_err("unknown DVD-Audio downmix values must fail clap parsing");
        assert!(err.to_string().contains("invalid DVD-Audio downmix policy"));
    }

    #[test]
    fn dvda_downmix_clap_accepts_documented_values() {
        let cli = Cli::try_parse_from([
            "tonepoet",
            "convert",
            "disc.iso",
            "--dvda-downmix",
            "ffmpeg",
        ])
        .expect("documented DVD-Audio downmix value should parse");

        let Commands::Convert { dvda_downmix, .. } = cli.command else {
            panic!("expected convert command");
        };
        assert_eq!(dvda_downmix, Some(DvdaDownmixPolicy::FfmpegDefault));
    }

    #[test]
    fn dvda_group_numeric_maps_to_group() {
        let req = build_pipeline_request_template(
            &None,
            &default_options(),
            AudioFormat::Flac,
            false,
            &None,
            &None,
            None,
            None,
            None,
            false,
            false,
            false,
            None,
            None,
            false,
            false,
            Some("2"),
            false,
            None,
        )
        .unwrap();
        assert_eq!(
            req.source.dvda_group_selection,
            tonepoet::convert::pipeline::DvdaGroupSelection::Group(2)
        );
    }

    #[test]
    fn dvda_group_all_maps_correctly() {
        let req = build_pipeline_request_template(
            &None,
            &default_options(),
            AudioFormat::Flac,
            false,
            &None,
            &None,
            None,
            None,
            None,
            false,
            false,
            false,
            None,
            None,
            false,
            false,
            Some("all"),
            false,
            None,
        )
        .unwrap();
        assert_eq!(
            req.source.dvda_group_selection,
            tonepoet::convert::pipeline::DvdaGroupSelection::All
        );
    }
}

#[cfg(test)]
mod dvda_cli_tests {
    use super::*;

    #[test]
    fn parse_dvda_group_stereo() {
        use tonepoet::convert::pipeline::DvdaGroupSelection;
        assert_eq!(parse_dvda_group("stereo"), DvdaGroupSelection::PreferStereo);
    }

    #[test]
    fn parse_dvda_group_multichannel_aliases() {
        use tonepoet::convert::pipeline::DvdaGroupSelection;
        assert_eq!(
            parse_dvda_group("multichannel"),
            DvdaGroupSelection::PreferMultichannel
        );
        assert_eq!(
            parse_dvda_group("multi"),
            DvdaGroupSelection::PreferMultichannel
        );
        assert_eq!(
            parse_dvda_group("mc"),
            DvdaGroupSelection::PreferMultichannel
        );
    }

    #[test]
    fn parse_dvda_group_hires_aliases() {
        use tonepoet::convert::pipeline::DvdaGroupSelection;
        assert_eq!(
            parse_dvda_group("hires"),
            DvdaGroupSelection::PreferHighestResolution
        );
        assert_eq!(
            parse_dvda_group("highres"),
            DvdaGroupSelection::PreferHighestResolution
        );
    }

    #[test]
    fn parse_dvda_group_all() {
        use tonepoet::convert::pipeline::DvdaGroupSelection;
        assert_eq!(parse_dvda_group("all"), DvdaGroupSelection::All);
    }

    #[test]
    fn parse_dvda_group_numeric() {
        use tonepoet::convert::pipeline::DvdaGroupSelection;
        assert_eq!(parse_dvda_group("1"), DvdaGroupSelection::Group(1));
        assert_eq!(parse_dvda_group("9"), DvdaGroupSelection::Group(9));
    }

    #[test]
    fn parse_dvda_group_zero_falls_back() {
        use tonepoet::convert::pipeline::DvdaGroupSelection;
        assert_eq!(parse_dvda_group("0"), DvdaGroupSelection::Default);
    }

    #[test]
    fn parse_dvda_group_unknown_falls_back() {
        use tonepoet::convert::pipeline::DvdaGroupSelection;
        assert_eq!(parse_dvda_group("nonsense"), DvdaGroupSelection::Default);
    }

    #[test]
    fn parse_dvda_group_case_insensitive() {
        use tonepoet::convert::pipeline::DvdaGroupSelection;
        assert_eq!(
            parse_dvda_group("STEREO"),
            DvdaGroupSelection::PreferStereo
        );
        assert_eq!(parse_dvda_group("ALL"), DvdaGroupSelection::All);
        assert_eq!(
            parse_dvda_group("MultiChannel"),
            DvdaGroupSelection::PreferMultichannel
        );
    }
}

#[cfg(test)]
mod terminal_restore_session_tests {
    use super::{
        terminal_session_is_active, terminal_session_is_owned_by_current_thread,
        TerminalRestoreGuard,
    };

    #[test]
    fn terminal_session_restore_authority_is_limited_to_owner_thread() {
        let mut guard = TerminalRestoreGuard::armed();

        assert!(terminal_session_is_active());
        assert!(terminal_session_is_owned_by_current_thread());

        let handle = std::thread::spawn(|| {
            assert!(terminal_session_is_active());
            assert!(!terminal_session_is_owned_by_current_thread());
        });
        handle.join().expect("worker thread should not panic");

        guard.disarm();
        assert!(!terminal_session_is_active());
    }
}

#[cfg(test)]
mod cli_convert_queue_planning_tests {
    use super::*;

    fn touch(path: &std::path::Path) {
        std::fs::write(path, b"fixture").expect("fixture file");
    }

    fn planned_names(planned: &PlannedCliQueue) -> Vec<String> {
        planned
            .items
            .iter()
            .map(|(path, _, _)| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }


    fn write_mergeable_split_cue_album_fixture(root: &std::path::Path) {
        touch(&root.join("side_a.flac"));
        touch(&root.join("side_b.flac"));
        std::fs::write(
            root.join("side_a.cue"),
            r#"TITLE "Album Side A"
FILE "side_a.flac" WAVE
  TRACK 01 AUDIO
    TITLE "A1"
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    TITLE "A2"
    INDEX 01 03:00:00
"#,
        )
        .expect("side A cue");
        std::fs::write(
            root.join("side_b.cue"),
            r#"TITLE "Album Side B"
FILE "side_b.flac" WAVE
  TRACK 01 AUDIO
    TITLE "B1"
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    TITLE "B2"
    INDEX 01 03:00:00
"#,
        )
        .expect("side B cue");
    }

    fn write_overlong_split_cue_album_fixture(root: &std::path::Path) {
        touch(&root.join("side_a.flac"));
        touch(&root.join("side_b.flac"));
        fn many_track_cue(title: &str, image: &str, first: usize, count: usize) -> String {
            let mut text = format!("TITLE \"{title}\"\nFILE \"{image}\" WAVE\n");
            for n in first..first + count {
                text.push_str(&format!(
                    "  TRACK {:02} AUDIO\n    INDEX 01 {:02}:00:00\n",
                    ((n - first) % 99) + 1,
                    n
                ));
            }
            text
        }
        std::fs::write(root.join("side_a.cue"), many_track_cue("Album Side A", "side_a.flac", 0, 50))
            .expect("side A cue");
        std::fs::write(root.join("side_b.cue"), many_track_cue("Album Side B", "side_b.flac", 50, 50))
            .expect("side B cue");
    }

    /// The Dreams box-set shape: split per-track FLACs plus an uppercase .CUE
    /// describing an image that is not present. The CUE must be suppressed —
    /// it previously queued and failed every folder conversion containing one.
    #[test]
    fn folder_scan_suppresses_cue_beside_split_tracks_case_insensitively() {
        let temp = tempfile::tempdir().expect("tempdir");
        let disc = temp.path().join("disc 03");
        std::fs::create_dir_all(&disc).expect("disc dir");
        touch(&disc.join("01 - One.flac"));
        touch(&disc.join("02 - Two.flac"));
        std::fs::write(
            disc.join("Dreams CD3.CUE"),
            "FILE \"Dreams CD3.WAV\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    INDEX 01 03:00:00\n",
        )
        .expect("cue fixture");

        let planned = plan_cli_convert_queue(&[temp.path().to_path_buf()]);

        let names = planned_names(&planned);
        assert_eq!(names, vec!["01 - One.flac", "02 - Two.flac"]);
        assert!(
            !names.iter().any(|name| name.ends_with(".CUE")),
            "a CUE describing a missing image must never be queued from a folder scan"
        );
    }

    /// An unsplit image with its CUE: the CUE is the queueable source and the
    /// image is suppressed, with no double conversion.
    #[test]
    fn folder_scan_queues_split_source_cue_and_suppresses_its_image() {
        let temp = tempfile::tempdir().expect("tempdir");
        touch(&temp.path().join("album.flac"));
        std::fs::write(
            temp.path().join("album.cue"),
            "FILE \"album.flac\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    INDEX 01 03:00:00\n",
        )
        .expect("cue fixture");

        let planned = plan_cli_convert_queue(&[temp.path().to_path_buf()]);

        assert_eq!(planned_names(&planned), vec!["album.cue"]);
    }

    /// Explicitly naming a CUE on the command line keeps explicit semantics
    /// even when the expansion would classify it as a metadata artifact.
    #[test]
    fn explicit_cue_file_argument_is_still_queued() {
        let temp = tempfile::tempdir().expect("tempdir");
        let audio = temp.path().join("01 - One.flac");
        touch(&audio);
        let cue = temp.path().join("album.cue");
        std::fs::write(
            &cue,
            "FILE \"01 - One.flac\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n",
        )
        .expect("cue fixture");

        let planned = plan_cli_convert_queue(&[cue.clone()]);
        assert_eq!(planned_names(&planned), vec!["album.cue"]);
    }

    /// Overlapping arguments (a directory and a file inside it) queue once.
    #[test]
    fn overlapping_arguments_deduplicate() {
        let temp = tempfile::tempdir().expect("tempdir");
        let audio = temp.path().join("01 - One.flac");
        touch(&audio);

        let planned = plan_cli_convert_queue(&[temp.path().to_path_buf(), audio.clone()]);
        assert_eq!(planned_names(&planned), vec!["01 - One.flac"]);
    }

    /// Suppressed sibling audio from an unresolvable CUE carries the
    /// EmbeddedOnly sidecar override so downstream conversion skips sidecar
    /// CUE discovery, exactly like the TUI commit path.
    #[test]
    fn cue_artifact_audio_gets_embedded_only_override() {
        let temp = tempfile::tempdir().expect("tempdir");
        touch(&temp.path().join("01 - One.flac"));
        std::fs::write(
            temp.path().join("broken.cue"),
            "FILE \"missing-image.wav\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    INDEX 01 03:00:00\n",
        )
        .expect("cue fixture");

        let planned = plan_cli_convert_queue(&[temp.path().to_path_buf()]);
        assert_eq!(planned.items.len(), 1);
        let (path, _, override_policy) = &planned.items[0];
        assert!(path.ends_with("01 - One.flac"));
        assert_eq!(
            *override_policy,
            Some(tonepoet::convert::pipeline::CueSidecarPolicy::EmbeddedOnly),
            "sibling audio of an unresolvable CUE must skip sidecar CUE detection"
        );
    }

    /// Unsupported explicit file arguments warn; missing paths warn; neither
    /// aborts the rest of the queue.
    #[test]
    fn warnings_for_missing_and_unsupported_arguments() {
        let temp = tempfile::tempdir().expect("tempdir");
        let audio = temp.path().join("01 - One.flac");
        touch(&audio);
        let unsupported = temp.path().join("notes.xyz");
        touch(&unsupported);

        let planned = plan_cli_convert_queue(&[
            audio.clone(),
            unsupported.clone(),
            temp.path().join("does-not-exist.flac"),
        ]);

        assert_eq!(planned_names(&planned), vec!["01 - One.flac"]);
        assert_eq!(planned.warnings.len(), 2);
        assert!(planned.warnings.iter().any(|w| w.contains("does not exist")));
        assert!(planned.warnings.iter().any(|w| w.contains("unsupported file format")));
    }

    #[test]
    fn cli_planner_surfaces_fatal_merged_cue_errors_instead_of_silent_empty_queue() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_overlong_split_cue_album_fixture(temp.path());

        let planned = plan_cli_convert_queue(&[temp.path().to_path_buf()]);

        assert!(planned.items.is_empty(), "fatal merged-CUE planning errors must not queue fallback items");
        assert!(planned.synthetic_cue_artifacts.is_empty());
        assert!(
            planned.errors.iter().any(|err| err.contains("at most 99")),
            "planner must return the fatal expansion error to the CLI, got {:?}",
            planned.errors
        );
    }


    #[test]
    fn cli_planner_keeps_partial_queue_when_one_synthetic_group_fails() {
        let temp = tempfile::tempdir().expect("tempdir");
        let bad = temp.path().join("bad-disc");
        let good = temp.path().join("good-disc");
        std::fs::create_dir_all(&bad).expect("bad dir");
        std::fs::create_dir_all(&good).expect("good dir");
        write_overlong_split_cue_album_fixture(&bad);
        write_mergeable_split_cue_album_fixture(&good);
        let standalone = temp.path().join("01 - One.flac");
        touch(&standalone);

        let planned = plan_cli_convert_queue(&[temp.path().to_path_buf()]);
        let names = planned_names(&planned);

        assert!(
            planned.errors.is_empty(),
            "partial expansion errors must be warnings when queueable work remains: {:?}",
            planned.errors
        );
        assert!(
            planned.warnings.iter().any(|warning| warning.contains("at most 99")),
            "failed group must be surfaced as a warning, got {:?}",
            planned.warnings
        );
        assert_eq!(planned.synthetic_cue_artifacts.len(), 1);
        assert!(
            names.iter().any(|name| name == "album.cue"),
            "valid merged group should survive partial expansion, got {names:?}"
        );
        assert!(
            names.iter().any(|name| name == "01 - One.flac"),
            "unrelated ordinary audio should survive partial expansion, got {names:?}"
        );
        assert!(
            !names.iter().any(|name| name == "side_a.cue" || name == "side_b.cue"),
            "fail-closed group side CUEs must not be queued as fallback: {names:?}"
        );
        tonepoet::convert::queue_expansion::cleanup_synthetic_cue_artifacts(
            &planned.synthetic_cue_artifacts,
        );
    }

    #[test]
    fn cli_archive_password_resolution_propagates_mru_backend_failure_before_admission() {
        let calls = std::cell::Cell::new(0usize);
        let error = resolve_cli_archive_password(
            true,
            &None,
            &TonepoetConfig::default(),
            || {
                calls.set(calls.get() + 1);
                Err("keyring backend unavailable: Secret Service is locked".to_string())
            },
        )
        .expect_err("unavailable MRU backend must fail queue admission");

        assert_eq!(calls.get(), 1);
        assert_eq!(
            error,
            "cannot resolve stored archive passwords before queue admission: keyring backend unavailable: Secret Service is locked"
        );
    }

    #[test]
    fn explicit_cli_archive_password_bypasses_unavailable_mru_backend() {
        let calls = std::cell::Cell::new(0usize);
        let password = resolve_cli_archive_password(
            true,
            &Some("ephemeral-cli-secret".to_string()),
            &TonepoetConfig::default(),
            || {
                calls.set(calls.get() + 1);
                Err("must not be called".to_string())
            },
        )
        .expect("explicit CLI password is self-contained");

        assert_eq!(calls.get(), 0);
        assert_eq!(password.as_deref(), Some("ephemeral-cli-secret"));
    }

    #[test]
    fn non_archive_cli_admission_does_not_resolve_config_or_mru_secrets() {
        let calls = std::cell::Cell::new(0usize);
        let mut config = TonepoetConfig::default();
        config.conversion.archive_password = Some("unused-cleartext".to_string());
        config.conversion.archive_password_ref =
            Some("archive-password:unavailable-but-unused".to_string());

        let password = resolve_cli_archive_password(
            false,
            &Some("unused-cli-secret".to_string()),
            &config,
            || {
            calls.set(calls.get() + 1);
            Err("must not be called".to_string())
            },
        )
        .expect("non-archive queue admission must be independent of secret backends");

        assert_eq!(calls.get(), 0);
        assert_eq!(password, None);
    }

    #[test]
    fn cli_synthetic_artifacts_are_manager_owned_and_drop_cleaned() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_mergeable_split_cue_album_fixture(temp.path());

        let planned = plan_cli_convert_queue(&[temp.path().to_path_buf()]);
        assert_eq!(planned.items.len(), 1);
        assert_eq!(planned.synthetic_cue_artifacts.len(), 1);
        let artifact = planned.synthetic_cue_artifacts.iter().next().unwrap().clone();
        assert!(artifact.exists(), "planner-created synthetic CUE must exist before ownership transfer");

        let manager = ConversionManager::new(ConversionConfig::default());
        {
            let mut q = manager.queue.try_write().expect("queue write");
            for (path, format, cue_sidecar_override) in planned.items.clone() {
                add_item_to_queue(
                    &mut q,
                    path,
                    format,
                    cue_sidecar_override,
                    &ConversionOptions::default(),
                    &None,
                );
            }
            for item in q.all_items_mut() {
                item.status = ConversionStatus::Queued;
            }
        }

        let claimed = match manager
            .register_synthetic_cue_artifacts_for_current_queue_nonblocking(&planned.synthetic_cue_artifacts)
        {
            tonepoet::convert::SyntheticCueArtifactRegistration::Registered { claimed } => claimed,
            tonepoet::convert::SyntheticCueArtifactRegistration::Deferred { .. } => {
                panic!("uncontended CLI ownership registration should not defer")
            }
            tonepoet::convert::SyntheticCueArtifactRegistration::Failed { error, .. } => {
                panic!("uncontended CLI ownership registration should not fail: {error}")
            }
        };
        assert!(claimed.contains(&artifact), "artifact must be registered against a queue item id");
        drop(manager);
        assert!(
            !artifact.exists(),
            "ConversionManager Drop must clean registered CLI synthetic CUE artifacts without manual free-standing cleanup"
        );
    }

}

#[cfg(test)]
mod dsf_recovery_cli_tests {
    use super::*;

    #[test]
    fn dsf_recover_subcommand_parses_all_explicit_resolution_actions() {
        for (raw, expected) in [
            ("status", DsfRecoveryAction::Status),
            ("recover-tail", DsfRecoveryAction::RecoverTail),
            ("restore-backup", DsfRecoveryAction::RestoreBackup),
            ("keep-current", DsfRecoveryAction::KeepCurrent),
        ] {
            let cli = Cli::try_parse_from(["tonepoet", "dsf-recover", raw, "album.dsf"])
                .expect("parse DSF recovery action");
            match cli.command {
                Commands::DsfRecover { action, path } => {
                    assert_eq!(action, expected);
                    assert_eq!(path, PathBuf::from("album.dsf"));
                }
                other => panic!("expected dsf-recover command, got {other:?}"),
            }
        }
    }
}
