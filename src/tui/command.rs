//! Vi-style command mode: parsing and execution

use std::path::PathBuf;
use tokio::sync::mpsc;

use crate::convert::formats::AudioFormat;
use crate::convert::simple_wizard::DitherType;
use super::app::*;
use super::message::AppMessage;

/// Parsed command from the command line
#[derive(Debug)]
pub enum Command {
    Quit,
    Write,
    WriteQuit,
    Edit(String),
    Output(String),
    Cd(String),
    Convert,
    /// Add to queue. The bool is "switch after" — true for `:queue!` variant.
    Queue(bool),
    Batch(String),
    Preset(String),
    SaveAs(String),
    Presets,
    Set(String, String),
    Fx(Vec<String>),
    Info,
    Tools,
    Help,
    /// Sort command for the browse screen. Args: (field?, dir?)
    Sort(Option<String>, Option<String>),
    /// Toggle sort direction
    SortDir,
    /// Filter command for the browse screen. Arg: format name or empty (cycle)
    Filter(Option<String>),
    /// Rename the current browse selection to the given name.
    /// Empty arg opens the rename overlay seeded with the current name.
    Rename(String),
    /// Switch to the browse screen. On the convert screen, sets
    /// the return target so a selected file loads back into the source pane.
    Browse,
    Unknown(String),
}

/// Parse a command string (without the leading ':')
pub fn parse_command(input: &str) -> Command {
    let input = input.trim();
    let mut parts = input.splitn(2, char::is_whitespace);
    let cmd = parts.next().unwrap_or("");
    let args = parts.next().unwrap_or("").trim();

    match cmd {
        "q" | "quit" | "exit" => Command::Quit,
        "w" | "write" | "save" => {
            if args.is_empty() {
                Command::Write
            } else {
                Command::SaveAs(args.to_string())
            }
        }
        "wq" => Command::WriteQuit,
        "e" | "edit" => Command::Edit(args.to_string()),
        "o" | "output" => Command::Output(args.to_string()),
        "cd" => Command::Cd(args.to_string()),
        "c" | "convert" => Command::Convert,
        "qa" | "queue" => Command::Queue(false),
        "qa!" | "queue!" => Command::Queue(true),
        "batch" => Command::Batch(args.to_string()),
        "preset" => Command::Preset(args.to_string()),
        "saveas" => Command::SaveAs(args.to_string()),
        "presets" => Command::Presets,
        "set" => {
            let mut set_parts = args.splitn(2, char::is_whitespace);
            let key = set_parts.next().unwrap_or("").to_string();
            let value = set_parts.next().unwrap_or("").trim().to_string();
            Command::Set(key, value)
        }
        "fx" | "effects" => {
            let fx_args: Vec<String> = args.split_whitespace().map(|s| s.to_string()).collect();
            Command::Fx(fx_args)
        }
        "info" => Command::Info,
        "tools" => Command::Tools,
        "h" | "help" => Command::Help,
        "sort" => {
            let mut sort_parts = args.split_whitespace();
            let field = sort_parts.next().map(|s| s.to_string());
            let dir = sort_parts.next().map(|s| s.to_string());
            Command::Sort(field, dir)
        }
        "sortdir" => Command::SortDir,
        "filter" => {
            let arg = if args.is_empty() { None } else { Some(args.to_string()) };
            Command::Filter(arg)
        }
        "rename" | "mv" => Command::Rename(args.to_string()),
        "browse" | "b" => Command::Browse,
        _ => Command::Unknown(input.to_string()),
    }
}

/// Execute a parsed command against app state
pub fn execute_command(
    app: &mut AppState,
    cmd: Command,
    tx: &mpsc::Sender<AppMessage>,
) {
    match cmd {
        Command::Quit => {
            app.should_quit = true;
        }
        Command::Write => {
            if let Some(name) = &app.preset.active_preset.clone() {
                let preset = super::presets::TuiPreset::from_pill_state(
                    name, &app.convert.format, &app.convert.output_options,
                );
                match super::presets::save_preset(&preset) {
                    Ok(_) => {
                        app.preset.modified = false;
                        app.set_status(format!("Saved preset: {}", name));
                    }
                    Err(e) => app.set_status(format!("Save failed: {}", e)),
                }
            } else {
                app.set_status("No active preset. Use :saveas <name>");
            }
        }
        Command::WriteQuit => {
            if let Some(name) = &app.preset.active_preset.clone() {
                let preset = super::presets::TuiPreset::from_pill_state(
                    name, &app.convert.format, &app.convert.output_options,
                );
                super::presets::save_preset(&preset).ok();
            }
            app.should_quit = true;
        }
        Command::Edit(path) => {
            if path.is_empty() {
                app.set_status("Usage: :e <path>");
                return;
            }
            let expanded = expand_path(&path);
            let p = PathBuf::from(&expanded);
            if !p.exists() {
                app.set_status(format!("Path not found: {}", expanded));
                return;
            }
            // Probe the file first
            match crate::tui::probe::probe_audio(&p) {
                Ok(info) => {
                    app.convert.source.file_path = Some(p.clone());
                    app.convert.source.info = Some(info);
                    app.set_status(format!("Loaded: {}", p.file_name().unwrap_or_default().to_string_lossy()));
                }
                Err(e) => {
                    app.convert.source.file_path = None;
                    app.convert.source.info = None;
                    app.set_status(format!("Probe error: {}", e));
                    return;
                }
            }
            // Read metadata (best-effort)
            if let Ok(meta) = crate::tui::probe::read_metadata(&p) {
                app.convert.metadata.title = meta.title.clone();
                app.convert.metadata.artist = meta.artist.clone();
                app.convert.metadata.album = meta.album.clone();
                app.convert.metadata.genre = meta.genre.clone();
                app.convert.metadata.year = meta.year.clone();
                app.convert.source.metadata = meta;
            }
            app.current_screen = AppScreen::Convert;
        }
        Command::Output(path) => {
            if path.is_empty() {
                app.set_status("Usage: :o <path>");
                return;
            }
            let expanded = expand_path(&path);
            app.convert.output_options.dest_path = Some(PathBuf::from(&expanded));
            app.set_status(format!("Output destination: {}", expanded));
        }
        Command::Cd(path) => {
            if app.current_screen != AppScreen::Browse {
                app.set_status(":cd only works on the browse screen");
                return;
            }
            if path.is_empty() {
                app.set_status("Usage: :cd <path>");
                return;
            }
            match app.browse.navigate_to_str(&path) {
                Ok(()) => {
                    let p = app.browse.current_dir.display().to_string();
                    app.set_status(format!("cd: {}", p));
                    app.browse.probe_current();
                }
                Err(e) => app.set_status(format!("cd: {}", e)),
            }
        }
        Command::Convert => {
            super::convert_actions::convert_or_queue(app, tx, true);
        }
        Command::Queue(switch_after) => {
            execute_queue(app, tx, switch_after);
        }
        Command::Batch(dir) => {
            if dir.is_empty() {
                app.set_status("Usage: :batch <directory>");
            } else {
                app.set_status(format!("Batch scan not yet implemented: {}", dir));
            }
        }
        Command::Preset(name) => {
            if name.is_empty() {
                app.set_status("Usage: :preset <name>");
            } else {
                match super::presets::load_preset(&name) {
                    Ok(preset) => {
                        preset.apply_to_pills(
                            &mut app.convert.format,
                            &mut app.convert.output_options,
                        );
                        app.preset.active_preset = Some(name.clone());
                        app.preset.modified = false;
                        app.set_status(format!("Loaded preset: {}", name));
                    }
                    Err(e) => app.set_status(format!("Load failed: {}", e)),
                }
            }
        }
        Command::SaveAs(name) => {
            if name.is_empty() {
                app.set_status("Usage: :saveas <name>");
            } else {
                let preset = super::presets::TuiPreset::from_pill_state(
                    &name, &app.convert.format, &app.convert.output_options,
                );
                match super::presets::save_preset(&preset) {
                    Ok(_) => {
                        app.preset.active_preset = Some(name.clone());
                        app.preset.modified = false;
                        app.set_status(format!("Saved preset: {}", name));
                    }
                    Err(e) => app.set_status(format!("Save failed: {}", e)),
                }
            }
        }
        Command::Presets => {
            let names = super::presets::list_presets();
            if names.is_empty() {
                app.set_status("No presets saved. Use :saveas <name>");
            } else {
                app.set_status(format!("Presets: {}", names.join(", ")));
            }
        }
        Command::Set(key, value) => {
            execute_set(app, &key, &value);
        }
        Command::Fx(args) => {
            if args.is_empty() || (args.len() == 1 && args[0] == "list") {
                app.set_status("Effects chains not yet implemented");
            } else {
                app.set_status("Effects: not yet implemented");
            }
        }
        Command::Info => {
            if let Some(info) = &app.convert.source.info {
                app.set_status(format!(
                    "{} | {} | {} | {}",
                    info.format_name,
                    info.codec_display(),
                    info.sample_rate_display(),
                    info.channels_display(),
                ));
            } else {
                app.set_status("No source file loaded. Use :e <path>");
            }
        }
        Command::Tools => {
            app.current_screen = AppScreen::Config;
            app.set_status("Showing config/tools");
        }
        Command::Help => {
            app.set_status(":q :e :o :cd :browse :rename :queue :queue! :convert :preset :saveas :set :sort :filter :help");
        }
        Command::Sort(field, dir) => {
            execute_sort(app, field.as_deref(), dir.as_deref());
        }
        Command::SortDir => {
            if app.current_screen != AppScreen::Browse {
                app.set_status(":sortdir only works on the browse screen");
                return;
            }
            app.browse.toggle_sort_dir();
            let msg = format!(
                "Sort: {} {}",
                app.browse.sort_by.label(),
                app.browse.sort_dir.label()
            );
            app.set_status(msg);
        }
        Command::Filter(arg) => {
            execute_filter(app, arg.as_deref());
        }
        Command::Rename(new_name) => {
            execute_rename(app, &new_name);
        }
        Command::Browse => {
            // If invoked from the convert screen, set return_target so the
            // selected file loads back into the source pane. From any other
            // screen, just switch to browse without a return target.
            if app.current_screen == AppScreen::Convert {
                app.browse.return_target = super::browse::BrowseReturnTarget::ConvertSource;
            }
            app.current_screen = AppScreen::Browse;
            app.browse.probe_current();
        }
        Command::Unknown(input) => {
            app.set_status(format!("Unknown command: {}", input));
        }
    }
}

/// Execute a :sort command for the browse screen
fn execute_sort(app: &mut AppState, field: Option<&str>, dir: Option<&str>) {
    use super::browse::{SortBy, SortDir};

    if app.current_screen != AppScreen::Browse {
        app.set_status(":sort only works on the browse screen");
        return;
    }

    // No args → cycle to next field
    if field.is_none() {
        app.browse.cycle_sort_by();
        let msg = format!(
            "Sort: {} {}",
            app.browse.sort_by.label(),
            app.browse.sort_dir.label()
        );
        app.set_status(msg);
        return;
    }

    // Parse explicit field
    let new_field = match field.unwrap().to_lowercase().as_str() {
        "name" | "n" => SortBy::Name,
        "date" | "d" | "modified" | "m" => SortBy::Date,
        "type" | "t" | "kind" => SortBy::Type,
        "size" | "s" => SortBy::Size,
        other => {
            app.set_status(format!(
                "Unknown sort field: {}. Try: name, date, type, size",
                other
            ));
            return;
        }
    };

    // Parse optional direction
    let new_dir = match dir {
        None => app.browse.sort_dir, // preserve current
        Some(d) => match d.to_lowercase().as_str() {
            "asc" | "a" | "ascending" => SortDir::Asc,
            "desc" | "d" | "descending" => SortDir::Desc,
            other => {
                app.set_status(format!("Unknown sort direction: {}. Try: asc, desc", other));
                return;
            }
        },
    };

    app.browse.set_sort(new_field, new_dir);
    let msg = format!(
        "Sort: {} {}",
        app.browse.sort_by.label(),
        app.browse.sort_dir.label()
    );
    app.set_status(msg);
}

/// Execute a :filter command for the browse screen
fn execute_filter(app: &mut AppState, arg: Option<&str>) {
    use super::browse::FormatFilter;
    use crate::convert::formats::AudioFormat;

    if app.current_screen != AppScreen::Browse {
        app.set_status(":filter only works on the browse screen");
        return;
    }

    // No arg → cycle to next filter
    if arg.is_none() {
        app.browse.cycle_format_filter();
        let msg = format!("Filter: {}", app.browse.format_filter.label());
        app.set_status(msg);
        return;
    }

    // Parse explicit filter value
    let new_filter = match arg.unwrap().to_lowercase().as_str() {
        "off" | "none" | "all" => FormatFilter::Off,
        "audio" => FormatFilter::AudioOnly,
        "flac" => FormatFilter::Only(AudioFormat::Flac),
        "opus" => FormatFilter::Only(AudioFormat::Opus),
        "aac" => FormatFilter::Only(AudioFormat::Aac),
        "mp3" => FormatFilter::Only(AudioFormat::Mp3),
        "alac" => FormatFilter::Only(AudioFormat::Alac),
        "wav" => FormatFilter::Only(AudioFormat::Wav),
        "wavpack" | "wv" => FormatFilter::Only(AudioFormat::WavPack),
        "aiff" => FormatFilter::Only(AudioFormat::Aiff),
        other => {
            app.set_status(format!(
                "Unknown filter: {}. Try: off, audio, flac, opus, aac, mp3, alac, wav, wavpack, aiff",
                other
            ));
            return;
        }
    };

    app.browse.set_format_filter(new_filter);
    let msg = format!("Filter: {}", app.browse.format_filter.label());
    app.set_status(msg);
}

/// Execute a `:queue` / `:queue!` command. Context-sensitive:
/// - On the browse screen: adds the multi-selected files (or the current
///   entry, if nothing is multi-selected) to the conversion queue with
///   default options. Stays on browse unless `switch_after` is true.
/// - On the convert screen: adds the currently loaded source file to the
///   queue (via the existing convert_or_queue path). Stays on convert
///   unless `switch_after` is true.
/// - Anywhere else: status error.
fn execute_queue(app: &mut AppState, tx: &mpsc::Sender<AppMessage>, switch_after: bool) {
    match app.current_screen {
        AppScreen::Browse => {
            // Collect paths: multi-selected if any, else the current entry
            // (provided it's an audio file or archive).
            let mut paths: Vec<PathBuf> = app.browse.multi_selected.clone();
            if paths.is_empty() {
                if let Some(entry) = app.browse.selected_entry() {
                    use super::browse::EntryKind;
                    match &entry.kind {
                        EntryKind::AudioFile(_) | EntryKind::Archive => {
                            paths.push(entry.path.clone());
                        }
                        _ => {
                            app.set_status("queue: selection is not an audio file");
                            return;
                        }
                    }
                } else {
                    app.set_status("queue: no selection");
                    return;
                }
            }

            let options = crate::convert::ConversionOptions::default();
            let mut count = 0;
            for p in paths {
                if app.manager.add_file_ready_for_processing(p, options.clone()).is_ok() {
                    count += 1;
                }
            }
            app.browse.clear_multi_selection();
            app.set_status(format!("Queued {} files", count));
            if switch_after {
                app.current_screen = AppScreen::Queue;
            }
        }
        AppScreen::Convert => {
            // Use the existing convert-to-queue path (`false` = queue, don't start).
            let ok = super::convert_actions::convert_or_queue(app, tx, false);
            if ok && switch_after {
                app.current_screen = AppScreen::Queue;
            }
        }
        _ => {
            app.set_status(":queue only works on the browse or convert screen");
        }
    }
}

/// Execute a `:rename` command for the browse screen.
/// - With no argument: opens a TextEdit overlay seeded with the current name
///   so the user can edit and press Enter to commit.
/// - With an argument: commits the rename directly.
fn execute_rename(app: &mut AppState, new_name: &str) {
    if app.current_screen != AppScreen::Browse {
        app.set_status(":rename only works on the browse screen");
        return;
    }

    // Capture the currently selected entry's path (must be a real entry,
    // not the ".." parent pseudo-entry).
    let (original_path, current_name) = match app.browse.selected_entry() {
        Some(e) if !matches!(e.kind, super::browse::EntryKind::ParentDir) => {
            (e.path.clone(), e.name.clone())
        }
        Some(_) => {
            app.set_status("rename: cannot rename parent directory (..)");
            return;
        }
        None => {
            app.set_status("rename: no selection");
            return;
        }
    };

    let trimmed = new_name.trim();
    if trimmed.is_empty() {
        // No arg → open the overlay seeded with the current name.
        app.active_overlay = ActiveOverlay::TextEdit {
            input: super::text_input::TextInputState::new(current_name),
            target: TextEditTarget::BrowseRename(original_path),
            label: "rename".to_string(),
        };
    } else {
        // Arg provided → commit directly.
        super::keybindings::commit_browse_rename(app, original_path, trimmed);
    }
}

/// Execute a :set command
fn execute_set(app: &mut AppState, key: &str, value: &str) {
    if key.is_empty() {
        app.set_status("Usage: :set <key> <value>  (format, rate, depth, dither, rg)");
        return;
    }
    if value.is_empty() {
        // Show current value
        match key {
            "f" | "format" => {
                app.set_status(format!("format = {}", app.convert.format.format.selected_label()));
            }
            "r" | "rate" => {
                app.set_status(format!("rate = {}", app.convert.format.sample_rate.selected_label()));
            }
            "d" | "depth" => {
                app.set_status(format!("depth = {}", app.convert.format.bit_depth.selected_label()));
            }
            "dither" => {
                app.set_status(format!("dither = {}", app.convert.format.dither.selected_label()));
            }
            "rg" | "replaygain" => {
                app.set_status(format!("replaygain = {}", app.convert.format.replaygain.selected_label()));
            }
            _ => {
                app.set_status(format!("Unknown setting: {}", key));
            }
        }
        return;
    }

    match key {
        "f" | "format" => {
            let fmt = match value.to_lowercase().as_str() {
                "flac" => Some(AudioFormat::Flac),
                "opus" => Some(AudioFormat::Opus),
                "aac" => Some(AudioFormat::Aac),
                "mp3" => Some(AudioFormat::Mp3),
                "alac" => Some(AudioFormat::Alac),
                "wav" => Some(AudioFormat::Wav),
                "wavpack" | "wv" => Some(AudioFormat::WavPack),
                "aiff" => Some(AudioFormat::Aiff),
                _ => None,
            };
            if let Some(f) = fmt {
                app.convert.format.format.select_value(&f);
                app.convert.format.apply_format_constraints();
                app.preset.mark_modified();
                app.set_status(format!("format = {}", f.name()));
            } else {
                app.set_status(format!("Unknown format: {}. Try: flac, opus, aac, mp3, alac, wav, wavpack, aiff", value));
            }
        }
        "r" | "rate" => {
            let rate: Option<u32> = match value {
                "44.1" | "44100" => Some(44_100),
                "48" | "48000" => Some(48_000),
                "88.2" | "88200" => Some(88_200),
                "96" | "96000" => Some(96_000),
                "176.4" | "176400" => Some(176_400),
                "192" | "192000" => Some(192_000),
                "352.8" | "352800" => Some(352_800),
                "384" | "384000" => Some(384_000),
                "705.6" | "705600" => Some(705_600),
                "768" | "768000" => Some(768_000),
                _ => None,
            };
            if let Some(r) = rate {
                app.convert.format.sample_rate.select_value(&r);
                app.preset.mark_modified();
                app.set_status(format!("rate = {} kHz", value));
            } else {
                app.set_status(format!("Unknown rate: {}. Try: 44.1, 48, 88.2, 96, 176.4, 192, 352.8, 384, 705.6, 768", value));
            }
        }
        "d" | "depth" => {
            let depth = match value.to_lowercase().as_str() {
                "16" => Some(BitDepthChoice::Int16),
                "24" => Some(BitDepthChoice::Int24),
                "32" => Some(BitDepthChoice::Int32),
                "32f" | "f32" | "float32" => Some(BitDepthChoice::Float32),
                "64f" | "f64" | "float64" => Some(BitDepthChoice::Float64),
                _ => None,
            };
            if let Some(d) = depth {
                app.convert.format.bit_depth.select_value(&d);
                app.preset.mark_modified();
                app.set_status(format!("depth = {}", value));
            } else {
                app.set_status(format!("Unknown depth: {}. Try: 16, 24, 32, 32f, 64f", value));
            }
        }
        "dither" => {
            let dt = match value.to_lowercase().as_str() {
                "tpdf" => Some(DitherType::TPDF),
                "none" | "off" => Some(DitherType::None),
                "shaped" | "shibata" => Some(DitherType::Shibata),
                _ => None,
            };
            if let Some(d) = dt {
                app.convert.format.dither.select_value(&d);
                app.preset.mark_modified();
                app.set_status(format!("dither = {}", value));
            } else {
                app.set_status(format!("Unknown dither: {}. Try: tpdf, none, shaped", value));
            }
        }
        "rg" | "replaygain" => {
            let rg = match value.to_lowercase().as_str() {
                "album" => Some(ReplayGainChoice::Album),
                "track" => Some(ReplayGainChoice::Track),
                "both" => Some(ReplayGainChoice::Both),
                "off" | "none" => Some(ReplayGainChoice::Off),
                _ => None,
            };
            if let Some(r) = rg {
                app.convert.format.replaygain.select_value(&r);
                app.preset.mark_modified();
                app.set_status(format!("replaygain = {}", value));
            } else {
                app.set_status(format!("Unknown rg mode: {}. Try: album, track, both, off", value));
            }
        }
        _ => {
            app.set_status(format!("Unknown setting: {}. Try: format, rate, depth, dither, rg", key));
        }
    }
}

/// Expand ~ to home directory
fn expand_path(path: &str) -> String {
    if path.starts_with('~') {
        if let Ok(home) = std::env::var("HOME") {
            return path.replacen('~', &home, 1);
        }
    }
    path.to_string()
}
