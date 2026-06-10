use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};

use tonepoet::config::TonepoetConfig;
use tonepoet::convert::{
    formats::{
        AacProfile, AudioFormat, ConversionOptions, FileFormat, FormatDetector, Mp3BitrateMode,
        QualitySettings,
    },
    ConversionItem, ConversionProcessor, ConversionQueue, ConversionStatus, ProcessorConfig,
    ProgressUpdate,
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

        /// Output naming template (e.g. "%NN% - %TITLE%")
        #[arg(long)]
        naming: Option<String>,
        /// Output album/folder naming template (e.g. "%ARTIST%/%ALBUM% (%YEAR%)")
        #[arg(long = "folder-naming")]
        folder_naming: Option<String>,

        /// Disable metadata tagging stage
        #[arg(long)]
        no_metadata: bool,

        /// Disable feature generation (log/CUE sidecars)
        #[arg(long)]
        no_features: bool,
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

    /// Tag audio files or an SACD ISO from MusicBrainz, headless.
    ///
    /// Computes a CD-equivalent TOC from the supplied paths, looks up
    /// matching MusicBrainz releases, populates per-track tags, and
    /// writes them in place. On multi-match the command refuses with
    /// the candidate list unless `--auto` or `--release-id` is given.
    /// Mirrors the in-editor `:tags-mb` flow with no interactive UI.
    TagsMb {
        /// Audio files (FLAC, WAV, etc.) — same album, same directory.
        /// Or one SACD ISO file (writes the sidecar XML).
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
            paths,
            format,
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
            partial,
            overwrite_output,
            naming,
            folder_naming,
            no_metadata,
            no_features,
        } => {
            run_convert(
                paths,
                format,
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
                partial,
                overwrite_output,
                naming,
                folder_naming,
                no_metadata,
                no_features,
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
        Commands::Config { show, reset, path } => {
            run_config(show, reset, path, &config)?;
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
    track: Option<u32>,
    track_range: Option<String>,
    area: Option<String>,
    no_cue: bool,
    partial: bool,
    overwrite_output: bool,
    naming: Option<String>,
    folder_naming: Option<String>,
    no_metadata: bool,
    no_features: bool,
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
    options.generate_cue_files = generate_cue || config.conversion.generate_cue_files;
    if let Some(template) = &naming {
        options.naming_template = Some(template.clone());
    }
    if let Some(template) = &folder_naming {
        options.folder_template = Some(template.clone());
    }

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
                            add_item_to_queue(
                                &mut q,
                                p.to_path_buf(),
                                format,
                                &options,
                                &archive_password,
                                config,
                            );
                        }
                    }
                }
            } else if path.is_file() {
                if let Ok(format) = FormatDetector::detect(path) {
                    add_item_to_queue(
                        &mut q,
                        path.clone(),
                        format,
                        &options,
                        &archive_password,
                        config,
                    );
                } else {
                    eprintln!("Warning: unsupported file format: {}", path.display());
                }
            }
        }

        // Attach pipeline request template to each item if pipeline flags were set.
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
        }

        // Mark all items as queued now that settings/requests are attached.
        for item in q.all_items_mut() {
            item.status = ConversionStatus::Queued;
        }

        let total = q.all_items().len();
        if total == 0 {
            anyhow::bail!("No supported files found in the provided paths");
        }
        println!(
            "Queued {} item(s) for conversion to {}",
            total,
            output_format.name()
        );
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
    let mut item = ConversionItem::new(path.clone(), format, options.clone());
    if tonepoet::is_encrypted_archive_ext(&path) {
        // Password priority: CLI flag → config → keychain MRU → None.
        item.archive_password = archive_password
            .clone()
            .or_else(|| config.conversion.archive_password.clone())
            .or_else(|| tonepoet::tui::keychain::load_keychain().into_iter().next());
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
        || no_features;

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

    let rg_enabled = match replaygain.as_deref() {
        Some("off") | Some("none") => false,
        _ => options.calculate_replaygain,
    };

    Some(PipelineRequest {
        worker_count: None,
        job_id: String::new(),     // filled per-item
        item_id: String::new(),    // filled per-item
        container: PathBuf::new(), // filled per-item
        source: SourceOptions {
            archive_password: archive_password
                .as_ref()
                .map(|p| SecretString::new(p.clone())),
            sacd_area,
            dvda_group: None,
            dvda_group_selection: DvdaGroupSelection::Default,
            cue_sidecar: cue_policy,
            track_selection,
        },
        settings: options
            .pipeline_settings
            .clone()
            .unwrap_or_else(|| {
                tonepoet::convert::pipeline::pipeline_settings_from_legacy_options(&options)
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
    })
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
        event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers},
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
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

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
        event::{
            DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        },
        execute,
        terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    };
    use ratatui::prelude::*;
    use tonepoet::tui::app::AppState;
    use tonepoet::tui::event_loop::run_app;

    // Set up terminal
    enable_raw_mode()?;
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
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        DisableBracketedPaste
    )?;
    terminal.show_cursor()?;

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
///    shared `apply_audio_tag_changes` / `save_sacd_sidecar` helpers.
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
        PathKind::SacdIso(_) => state.paths.len(), // ISO replicated × n_tracks
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
    musicbrainz::populate_editor_from_mb(&mut state, &release);

    if matches!(kind, PathKind::Audio(_)) {
        if let Err(e) = tonepoet::tui::keybindings::regenerate_cuesheet_for_save(&mut state) {
            err!("tags-mb: cuesheet regen: {}", e);
            return 3;
        }
    }

    if dry_run {
        let count = state
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
            for e in &state.entries {
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
        PathKind::Audio(ref audio_paths) => {
            let entries_snap: Vec<(lofty::tag::ItemKey, Vec<String>, Vec<String>)> = state
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
            let deleted: Vec<usize> = state.deleted.clone();
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
}

fn classify_tags_mb_paths(paths: &[PathBuf]) -> Result<PathKind, String> {
    if paths.is_empty() {
        return Err("no paths supplied".to_string());
    }
    let isos: Vec<&PathBuf> = paths
        .iter()
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("iso")))
        .collect();
    if !isos.is_empty() {
        if paths.len() != 1 {
            return Err("SACD ISO must be passed alone (no mixed paths)".to_string());
        }
        let iso = isos[0].clone();
        if !iso.exists() {
            return Err(format!("ISO not found: {}", iso.display()));
        }
        return Ok(PathKind::SacdIso(iso));
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
    Ok(app::MetadataEditorState {
        paths,
        entries,
        cursor: 0,
        scroll: 0,
        last_click: None,
        edit_input: None,
        add_key_input: None,
        phase: app::MetadataEditorPhase::Editing,
        dirty: false,
        deleted: Vec::new(),
        file_labels,
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
    })
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
            let durations = match state.sacd_area_kind {
                Some(tonepoet::tui::sacd::AreaKind::Stereo) => {
                    state.sacd_stereo_durations.as_deref()
                }
                Some(tonepoet::tui::sacd::AreaKind::MultiChannel) => {
                    state.sacd_multi_channel_durations.as_deref()
                }
                None => None,
            };
            let durations = durations.ok_or_else(|| {
                eprintln!("tags-mb: SACD has no per-track durations (TRL sectors malformed?)");
                3
            })?;
            tonepoet::tui::command::sacd_durations_to_sectors(durations)
        }
    };
    let toc_string = musicbrainz::build_mb_toc(&sectors).ok_or_else(|| {
        eprintln!("tags-mb: TOC too short");
        3
    })?;
    say!("TOC lookup ({} tracks)...", n_tracks);
    let cached = db.get_cached_mb_response(&toc_string);
    let outcome = musicbrainz::lookup_release_by_toc(&sectors, cached)
        .await
        .map_err(|e| {
            eprintln!("tags-mb: TOC lookup: {}", e);
            4
        })?;
    if let Some(body) = outcome.cache_response {
        let _ = db.store_mb_response(&toc_string, &body);
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
            false,
            false,
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
        )
        .unwrap();
        assert_eq!(
            req.stages.replaygain,
            tonepoet::convert::pipeline::StageRequirement::Disabled
        );
    }
}
