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
    Queue,
    Batch(String),
    Preset(String),
    SaveAs(String),
    Presets,
    Set(String, String),
    Fx(Vec<String>),
    Info,
    Tools,
    Help,
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
        "qa" | "queue" => Command::Queue,
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
            if path.is_empty() {
                app.set_status("Usage: :cd <path>");
                return;
            }
            let expanded = expand_path(&path);
            match std::env::set_current_dir(&expanded) {
                Ok(_) => app.set_status(format!("Changed directory: {}", expanded)),
                Err(e) => app.set_status(format!("cd failed: {}", e)),
            }
        }
        Command::Convert => {
            super::convert_actions::convert_or_queue(app, tx, true);
        }
        Command::Queue => {
            super::convert_actions::convert_or_queue(app, tx, false);
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
            app.set_status(":q :e <path> :o <path> :set <key> <val> :preset :saveas :convert :help");
        }
        Command::Unknown(input) => {
            app.set_status(format!("Unknown command: {}", input));
        }
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
