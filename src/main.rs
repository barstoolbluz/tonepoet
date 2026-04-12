use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};

use tonepoet::config::TonepoetConfig;
use tonepoet::convert::{
    ConversionQueue, ConversionItem, ConversionStatus,
    ConversionProcessor, ProcessorConfig, ProgressUpdate,
    formats::{AudioFormat, FileFormat, FormatDetector, ConversionOptions, QualitySettings,
              Mp3BitrateMode, AacProfile},
};

#[derive(Parser)]
#[command(name = "tonepoet", about = "Audio conversion toolkit")]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Enable verbose logging
    #[arg(short, long, global = true)]
    verbose: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Convert audio files, directories, or archives
    Convert {
        /// Input files, directories, or archives
        #[arg(required = true)]
        paths: Vec<PathBuf>,

        /// Output format (flac, wav, aiff, wavpack, mp3, aac, opus)
        #[arg(short, long)]
        format: Option<String>,

        /// Output directory
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Number of concurrent workers
        #[arg(short, long)]
        workers: Option<usize>,

        /// ReplayGain mode (track, album, both, off)
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

        /// Generate CUE files
        #[arg(long)]
        generate_cue: bool,
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
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let log_level = if cli.verbose { "debug" } else { "info" };
    init_logging(log_level, matches!(cli.command, Commands::Tui { .. }));

    let config = TonepoetConfig::load().unwrap_or_default();

    match cli.command {
        Commands::Tui { paths } => {
            run_tui(config, paths).await?;
        }
        Commands::Convert {
            paths, format, output, workers, replaygain,
            archive_password, preset, compression_level, bitrate,
            reencode_flac, merge, backend, append_lineage,
            write_log, generate_cue,
        } => {
            run_convert(
                paths, format, output, workers, replaygain,
                archive_password, preset, compression_level, bitrate,
                reencode_flac, merge, backend, append_lineage,
                write_log, generate_cue, &config,
            ).await?;
        }
        Commands::Wizard => {
            run_wizard(&config).await?;
        }
        Commands::CheckTools => {
            run_check_tools();
        }
        Commands::Config { show, reset, path } => {
            run_config(show, reset, path, &config)?;
        }
    }

    Ok(())
}

/// Initialize env_logger. In TUI mode, logs are redirected to a file at
/// `~/.cache/tonepoet/tonepoet.log` so they don't corrupt the ratatui display.
/// In CLI mode, logs go to stderr as usual.
fn init_logging(level: &str, is_tui: bool) {
    let mut builder = env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or(level),
    );
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
        _ => anyhow::bail!("Unknown format: {}. Supported: flac, wav, aiff, wavpack, mp3, aac, opus, alac", s),
    }
}

fn parse_replaygain_mode(s: &str) -> Option<tonepoet::convert::simple_wizard::ReplayGainMode> {
    use tonepoet::convert::simple_wizard::ReplayGainMode;
    match s.to_lowercase().as_str() {
        "track" => Some(ReplayGainMode::Track),
        "album" => Some(ReplayGainMode::Album),
        "both" => Some(ReplayGainMode::Both),
        "off" | "none" => None,
        _ => Some(ReplayGainMode::Album),
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_convert(
    paths: Vec<PathBuf>,
    format: Option<String>,
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
    config: &TonepoetConfig,
) -> anyhow::Result<()> {
    // Load preset if specified
    let preset_options: Option<ConversionOptions> = if let Some(preset_name) = &preset {
        let preset_mgr = tonepoet_wizard::PresetManager::new()
            .map_err(|e| anyhow::anyhow!("Failed to initialize preset manager: {}", e))?;
        let preset = preset_mgr.load_preset(preset_name)
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
    let mut options = preset_options.unwrap_or_else(|| {
        ConversionOptions {
            output_format,
            quality: output_format.default_quality(),
            ..ConversionOptions::default()
        }
    });

    // Apply CLI overrides
    options.output_format = output_format;
    if let Some(dir) = &output {
        options.output_dir = Some(dir.clone());
    } else if let Some(ref dir) = config.conversion.default_destination {
        options.output_dir = Some(dir.clone());
    }

    if let Some(rg) = &replaygain {
        options.replaygain_mode = parse_replaygain_mode(rg);
        options.calculate_replaygain = options.replaygain_mode.is_some();
    } else if config.conversion.calculate_replaygain {
        options.calculate_replaygain = true;
        if options.replaygain_mode.is_none() {
            options.replaygain_mode = Some(tonepoet::convert::simple_wizard::ReplayGainMode::Album);
        }
    }

    if let Some(cl) = compression_level {
        if matches!(output_format, AudioFormat::Flac) {
            options.quality = QualitySettings::Flac { compression_level: cl };
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
    options.append_lineage_to_comment = append_lineage || config.conversion.append_lineage_to_comment;
    options.write_log_file = write_log || config.conversion.write_log_file;
    options.generate_cue_files = generate_cue || config.conversion.generate_cue_files;

    // Build processor
    let worker_count = workers.unwrap_or(config.conversion.worker_count);
    let processor_config = ProcessorConfig {
        worker_count,
        tool_paths: std::collections::HashMap::new(),
        default_destination_directory: options.output_dir.clone()
            .or_else(|| config.conversion.default_destination.clone()),
        scratch_directory: config.conversion.scratch_directory.clone(),
    };

    let mut processor = ConversionProcessor::new(processor_config);

    // Set up progress channel
    let (progress_tx, progress_rx) = broadcast::channel::<ProgressUpdate>(256);
    processor.set_progress_channel(progress_tx);

    // Build queue
    let queue = Arc::new(RwLock::new(ConversionQueue::new()));

    // Add files to queue
    {
        let mut q = queue.write().await;
        for path in &paths {
            if !path.exists() {
                eprintln!("Warning: path does not exist: {}", path.display());
                continue;
            }

            if path.is_dir() {
                for entry in walkdir::WalkDir::new(path)
                    .follow_links(true)
                    .into_iter()
                    .filter_map(|e| e.ok())
                {
                    let p = entry.path();
                    if p.is_file() {
                        if let Ok(format) = FormatDetector::detect(p) {
                            add_item_to_queue(&mut q, p.to_path_buf(), format, &options, &archive_password, config);
                        }
                    }
                }
            } else if path.is_file() {
                if let Ok(format) = FormatDetector::detect(path) {
                    add_item_to_queue(&mut q, path.clone(), format, &options, &archive_password, config);
                } else {
                    eprintln!("Warning: unsupported file format: {}", path.display());
                }
            }
        }

        let total = q.all_items().len();
        if total == 0 {
            anyhow::bail!("No supported files found in the provided paths");
        }
        println!("Queued {} item(s) for conversion to {}", total, output_format.name());
    }

    // Spawn progress display task
    let mut progress_rx_owned = progress_rx;
    let progress_handle = tokio::spawn(async move {
        let pb = indicatif::ProgressBar::new(100);
        pb.set_style(
            indicatif::ProgressStyle::default_bar()
                .template("{spinner:.green} [{bar:40.cyan/blue}] {pos}% {msg}")
                .unwrap()
                .progress_chars("=>-")
        );

        while let Ok(update) = progress_rx_owned.recv().await {
            let pct = update.progress.min(100.0).max(0.0) as u64;
            pb.set_position(pct);
            match &update.status {
                ConversionStatus::Processing { message, phase, .. } => {
                    let phase_name = phase.as_ref().map(|p| p.short_name()).unwrap_or("Processing");
                    let msg = message.as_deref().unwrap_or("");
                    pb.set_message(format!("{}: {}", phase_name, msg));
                }
                ConversionStatus::Completed { output_path } => {
                    pb.println(format!("  Completed: {}", output_path.display()));
                }
                ConversionStatus::Failed { error } => {
                    pb.println(format!("  Failed: {}", error));
                }
                _ => {}
            }
        }
        pb.finish_and_clear();
    });

    // Run the conversion
    let result = processor.process_queue_with_progress(queue.clone(), None).await;

    // Wait for progress display to finish
    let _ = progress_handle.await;

    // Print summary
    {
        let q = queue.read().await;
        let completed = q.completed_items();
        let failed = q.failed_items();
        let total = q.total_items();
        println!("\nConversion complete: {}/{} succeeded, {} failed", completed, total, failed);
    }

    result.map_err(|e| anyhow::anyhow!("{}", e))
}

fn add_item_to_queue(
    queue: &mut ConversionQueue,
    path: PathBuf,
    format: FileFormat,
    options: &ConversionOptions,
    archive_password: &Option<String>,
    config: &TonepoetConfig,
) {
    let mut item = ConversionItem::new(path, format, options.clone());
    if matches!(format, FileFormat::SevenZip) {
        item.archive_password = archive_password.clone()
            .or_else(|| config.conversion.archive_password.clone());
    }
    item.status = ConversionStatus::Queued;
    queue.add_item_direct(item);
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
        use tonepoet_wizard::ReplayGainMode as WizRG;
        use tonepoet::convert::simple_wizard::ReplayGainMode;
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
        calculate_replaygain: preset.replaygain_mode.as_ref()
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
        event::{self, Event, KeyCode, KeyModifiers, EnableMouseCapture, DisableMouseCapture},
        execute,
        terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    };
    use ratatui::prelude::*;

    enable_raw_mode()?;
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
                    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
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
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
    terminal.show_cursor()?;

    if let Some(options) = result {
        println!("Wizard completed. Format: {}, ReplayGain: {}",
            options.output_format.name(),
            if options.calculate_replaygain { "enabled" } else { "disabled" });
        println!("Run conversion with: tonepoet convert <PATH> --preset <name>");
    } else {
        println!("Wizard cancelled.");
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
                ("7z", "Archive extraction", vec![]),
                ("opustags", "Opus metadata editing", vec!["--help"]),
                ("wvtag", "WavPack metadata editing", vec!["--version"]),
                ("AtomicParsley", "M4A/AAC metadata editing", vec!["--version"]),
                ("ssrc", "High-quality resampling", vec![]),
            ];

            println!("\nAdditional tools:");
            for (tool, description, args) in &extra_tools {
                // For tools that always exit non-zero (ssrc), just check if the binary is found
                let available = std::process::Command::new(tool)
                    .args(args.as_slice())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
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
                println!("Backend: NOT FUNCTIONAL - missing: {}",
                    availability.missing_critical_tools.join(", "));
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
        let default_config = TonepoetConfig::default();
        default_config.save()?;
        println!("Configuration reset to defaults.");
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
        event::{EnableMouseCapture, DisableMouseCapture},
        execute,
        terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    };
    use ratatui::prelude::*;
    use tonepoet::tui::app::AppState;
    use tonepoet::tui::event_loop::run_app;

    // Set up terminal
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create app state
    let mut app = AppState::new(config);

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
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
    terminal.show_cursor()?;

    result.map_err(|e| anyhow::anyhow!("{}", e))
}
