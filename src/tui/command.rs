//! Vi-style command mode: parsing and execution

use std::path::PathBuf;
use tokio::sync::mpsc;

use crate::convert::formats::AudioFormat;
use crate::convert::simple_wizard::DitherType;
use super::app::*;
use super::message::AppMessage;

/// Full list of command-mode tokens (including aliases) recognised by
/// `parse_command`. Used by the tab-completion machinery.
///
/// Ordered so more-typed commands come first — matters for UX because
/// the first match is what Tab shows initially before the user cycles.
pub const COMMAND_NAMES: &[&str] = &[
    "q", "quit", "exit",
    "w", "write", "save",
    "wq",
    "e", "edit",
    "o", "output",
    "cd",
    "queue", "queue!", "qa", "qa!",
    "c", "convert",
    "commit", "Commit",
    "go", "start",
    "expand", "x",
    "batch",
    "preset", "presets", "saveas",
    "set",
    "fx", "effects",
    "info",
    "tools",
    "h", "help",
    "sort", "sortdir",
    "filter",
    "rename",
    "del", "delete", "trash",
    "cp", "cp!", "copy", "copy!",
    "mv", "mv!", "move", "move!",
    "browse", "b",
    "recent", "recents",
    "bookmarks", "bm",
];

/// Commands that take a preset name as their argument. Used by the
/// completion machinery to decide whether the word after the command
/// should be completed against preset file names.
pub const PRESET_TAKING_COMMANDS: &[&str] = &[
    "queue", "queue!", "qa", "qa!", "c", "convert", "preset",
];

/// Compute tab-completion candidates from a CommandInput's current text
/// and cursor position. Returns `None` if no completion is applicable
/// (no matching candidates, or we're in a context that doesn't complete).
///
/// Completion kinds:
/// - **Command name** — prefix is the first word of the input and the
///   cursor is within it. Candidates come from `COMMAND_NAMES`.
/// - **Preset name** — first word is in `PRESET_TAKING_COMMANDS` and the
///   cursor is in the second word. Candidates come from the user's
///   preset directory via `presets::list_presets()`. Case-insensitive
///   prefix match.
///
/// Returns a `CompletionState` with `cursor = 0` (first candidate).
pub fn compute_completion(
    text: &str,
    cursor: usize,
) -> Option<CompletionState> {
    let before_cursor = &text[..cursor.min(text.len())];

    // Start of the word being completed: just after the last whitespace,
    // or 0 if there is none. Uses char_indices for multibyte safety.
    let prefix_start = before_cursor
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_whitespace())
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0);
    let prefix = &before_cursor[prefix_start..];

    // Command name completion: nothing before the prefix (prefix_start == 0).
    if prefix_start == 0 {
        let candidates: Vec<String> = COMMAND_NAMES
            .iter()
            .filter(|n| n.starts_with(prefix))
            .map(|n| n.to_string())
            .collect();
        if candidates.is_empty() {
            return None;
        }
        return Some(CompletionState {
            candidates,
            cursor: 0,
            prefix_start,
        });
    }

    // Preset-argument completion: first word is a preset-taking command.
    let first_word_end = before_cursor
        .char_indices()
        .find(|(_, c)| c.is_whitespace())
        .map(|(i, _)| i)
        .unwrap_or(before_cursor.len());
    let first_word = &before_cursor[..first_word_end];

    if !PRESET_TAKING_COMMANDS.contains(&first_word) {
        return None;
    }

    // Case-insensitive prefix match against preset names on disk.
    let presets = super::presets::list_presets();
    let prefix_lower = prefix.to_lowercase();
    let candidates: Vec<String> = presets
        .into_iter()
        .filter(|p| p.to_lowercase().starts_with(&prefix_lower))
        .collect();

    if candidates.is_empty() {
        return None;
    }

    Some(CompletionState {
        candidates,
        cursor: 0,
        prefix_start,
    })
}

/// Advance the completion cycle by `direction` (+1 forward, -1 backward)
/// and apply the new selection to the input. Called on every Tab /
/// Shift+Tab press while a completion is active.
pub fn cycle_completion(
    input: &mut super::text_input::TextInputState,
    state: &mut CompletionState,
    direction: i32,
) {
    let len = state.candidates.len();
    if len == 0 {
        return;
    }
    state.cursor = if direction >= 0 {
        (state.cursor + 1) % len
    } else {
        (state.cursor + len - 1) % len
    };
    apply_completion_to_input(input, state);
}

/// Replace the active completion range (`prefix_start..input.cursor`)
/// with the currently-selected candidate, moving the cursor to the end
/// of the inserted text.
pub fn apply_completion_to_input(
    input: &mut super::text_input::TextInputState,
    state: &CompletionState,
) {
    let candidate = &state.candidates[state.cursor];
    let prefix_end = input.cursor.min(input.text.len());
    input.text.replace_range(state.prefix_start..prefix_end, candidate);
    input.cursor = state.prefix_start + candidate.len();
}

/// Parsed command from the command line
#[derive(Debug)]
pub enum Command {
    Quit,
    Write,
    WriteQuit,
    Edit(String),
    Output(String),
    Cd(String),
    /// Open the Convert screen for batch review, inheriting the current
    /// browse selection if any. Triggered by `:queue`, `:queue!`, `:qa`,
    /// `:qa!`, `:convert`, `:c`. Optional preset name is loaded into the
    /// format pills before review.
    Queue { preset: Option<String> },
    /// Commit the currently-reviewed file/batch from the Convert screen to
    /// the queue. `:commit` (lowercase) enqueues only; `:Commit` (capital)
    /// enqueues AND starts processing, jumping to the Queue screen.
    Commit { start: bool },
    /// Start processing whatever's already in the queue. No new batch.
    /// Triggered by `:go` / `:start`.
    Go,
    /// Open the BatchList expand overlay. Only valid when the Convert
    /// source is a multi-file `Batch`. Triggered by `:expand` / `:x`.
    Expand,
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
    /// Move selected browse file(s) to the system trash (XDG Trash on
    /// Linux, Finder Trash on macOS). Shows confirmation first.
    Delete,
    /// Copy selected file(s) to a destination. Empty arg opens a TextEdit
    /// picker. `:cp!` variant replaces existing files.
    Copy { dest: String, force: bool },
    /// Move selected file(s) to a destination. Empty arg opens picker.
    /// `:mv!` replaces existing. Falls back to copy+delete across filesystems.
    Move { dest: String, force: bool },
    /// Switch to the browse screen. On the convert screen, sets
    /// the return target so a selected file loads back into the source pane.
    Browse,
    /// Open the recent-files overlay.
    Recent,
    /// Open the bookmarks overlay (browse-only). With no args, opens in
    /// browsing mode. With "add [name]", quick-adds the current browse
    /// directory as a bookmark without opening the overlay.
    Bookmarks(String),
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
        "c" | "convert" | "qa" | "queue" | "qa!" | "queue!" => {
            let preset = if args.is_empty() {
                None
            } else {
                Some(args.to_string())
            };
            Command::Queue { preset }
        }
        "commit" => Command::Commit { start: false },
        "Commit" => Command::Commit { start: true },
        "go" | "start" => Command::Go,
        "expand" | "x" => Command::Expand,
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
        "del" | "delete" | "trash" => Command::Delete,
        "rename" => Command::Rename(args.to_string()),
        "cp" | "copy" => Command::Copy { dest: args.to_string(), force: false },
        "cp!" | "copy!" => Command::Copy { dest: args.to_string(), force: true },
        "mv" | "move" => Command::Move { dest: args.to_string(), force: false },
        "mv!" | "move!" => Command::Move { dest: args.to_string(), force: true },
        "browse" | "b" => Command::Browse,
        "recent" | "recents" => Command::Recent,
        "bookmarks" | "bm" => Command::Bookmarks(args.to_string()),
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
                app.set_status("Usage: :e <path>  or  :e title|artist|album|genre|year");
                return;
            }

            // Context-sensitive: on Browse with an audio file selected,
            // if the arg is a known metadata field name, open the tag
            // editor for that field instead of loading a source path.
            if app.current_screen == AppScreen::Browse {
                use crate::tui::probe::MetadataField;
                let field = match path.to_lowercase().as_str() {
                    "title" => Some(MetadataField::Title),
                    "artist" => Some(MetadataField::Artist),
                    "album" => Some(MetadataField::Album),
                    "genre" => Some(MetadataField::Genre),
                    "year" => Some(MetadataField::Year),
                    _ => None,
                };
                if let Some(field) = field {
                    execute_edit_metadata(app, field);
                    return;
                }
            }

            let expanded = expand_path(&path);
            let p = PathBuf::from(&expanded);
            if !p.exists() {
                app.set_status(format!("Path not found: {}", expanded));
                return;
            }
            // Probe the file first
            let info = match crate::tui::probe::probe_audio(&p) {
                Ok(i) => i,
                Err(e) => {
                    // Reset to Empty on probe failure — :e abandons any
                    // existing source (single or batch) regardless.
                    app.convert.source.mode = SourceMode::Empty;
                    app.set_status(format!("Probe error: {}", e));
                    return;
                }
            };
            // Read metadata (best-effort).
            let metadata = crate::tui::probe::read_metadata(&p).unwrap_or_default();
            // Populate the editable metadata pane from the source tags.
            app.convert.metadata.title = metadata.title.clone();
            app.convert.metadata.artist = metadata.artist.clone();
            app.convert.metadata.album = metadata.album.clone();
            app.convert.metadata.genre = metadata.genre.clone();
            app.convert.metadata.year = metadata.year.clone();

            app.convert.source.mode = SourceMode::Single {
                path: p.clone(),
                info: Some(info),
                metadata,
            };
            app.set_status(format!(
                "Loaded: {}",
                p.file_name().unwrap_or_default().to_string_lossy()
            ));
            app.current_screen = AppScreen::Convert;
            app.recent.record_use(&p);
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
                    app.browse.probe_current(tx);
                }
                Err(e) => app.set_status(format!("cd: {}", e)),
            }
        }
        Command::Queue { preset } => {
            execute_queue(app, tx, preset);
        }
        Command::Commit { start } => {
            execute_commit(app, tx, start);
        }
        Command::Go => {
            execute_go(app, tx);
        }
        Command::Expand => {
            execute_expand(app);
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
            if let Some(info) = app.convert.source.mode.current_info() {
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
            app.set_status(":q :e :o :cd :browse :rename :del :cp :mv :queue :convert :commit :Commit :go :start :expand :recent :bm :preset :saveas :set :sort :filter :help");
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
        Command::Delete => {
            execute_delete(app);
        }
        Command::Rename(new_name) => {
            execute_rename(app, &new_name, tx);
        }
        Command::Copy { dest, force } => {
            execute_file_op(app, &dest, force, false);
        }
        Command::Move { dest, force } => {
            execute_file_op(app, &dest, force, true);
        }
        Command::Browse => {
            // If invoked from the convert screen, set return_target so the
            // selected file loads back into the source pane. From any other
            // screen, just switch to browse without a return target.
            if app.current_screen == AppScreen::Convert {
                app.browse.return_target = super::browse::BrowseReturnTarget::ConvertSource;
            }
            app.current_screen = AppScreen::Browse;
            app.browse.probe_current(tx);
        }
        Command::Recent => {
            app.recent.open_overlay();
        }
        Command::Bookmarks(args) => {
            execute_bookmarks(app, &args);
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

/// Execute a `:bookmarks` / `:bm` command. Browse-only.
///   `:bm`              → open the bookmarks overlay
///   `:bm add`          → quick-add current browse dir with default name (last
///                        path component), no overlay
///   `:bm add <name>`   → quick-add current browse dir with the given name
fn execute_bookmarks(app: &mut AppState, args: &str) {
    if app.current_screen != AppScreen::Browse {
        app.set_status(":bookmarks only works on the browse screen");
        return;
    }

    let trimmed = args.trim();
    if trimmed.is_empty() {
        app.bookmarks.open_overlay();
        return;
    }

    // Parse subcommand.
    let mut parts = trimmed.splitn(2, char::is_whitespace);
    let sub = parts.next().unwrap_or("").to_lowercase();
    let rest = parts.next().unwrap_or("").trim();

    match sub.as_str() {
        "add" => {
            let path = app.browse.current_dir.clone();
            let name = if rest.is_empty() {
                super::bookmarks::BookmarksState::default_name_for_path(&path)
            } else {
                rest.to_string()
            };
            app.bookmarks.add(name.clone(), path);
            app.set_status(format!("bookmark added: {}", name));
        }
        other => {
            app.set_status(format!("unknown :bm subcommand: {}", other));
        }
    }
}

/// Execute a `:queue` / `:queue!` / `:convert` / `:c` command.
///
/// Opens the Convert screen for batch review with the current Browse
/// selection inherited. Every enqueue operation must pass through this
/// review step — there is no back door to the queue.
///
/// Context-sensitive behavior:
/// - From Browse: collects selection (multi-selected or cursor entry),
///   probes the first file, optionally loads a preset, populates the
///   source pane, and switches to Convert. `previous_screen` is set so
///   the user returns to Browse on `:commit` / Esc.
/// - From Convert: with a preset arg, loads that preset in place; without,
///   reminds the user to pick files in Browse first.
/// - From Library: placeholder — selection inheritance arrives in Phase 6c.
/// - From Queue / elsewhere: with a preset arg, loads the preset without
///   switching; without, shows an error.
///
/// Phase 6b limitation: multi-file selections are rejected with a message
/// directing the user to deselect extras. Real batch support (summary +
/// expand overlay + bulk commit) arrives in Phase 6c/6d.
fn execute_queue(
    app: &mut AppState,
    _tx: &mpsc::Sender<AppMessage>,
    preset: Option<String>,
) {
    // Helper: load a named preset into the format/output-options pills.
    // Returns Ok(()) on success, Err(status_message) on failure.
    let load_preset_into_pills = |app: &mut AppState, name: &str| -> Result<(), String> {
        match super::presets::load_preset(name) {
            Ok(p) => {
                p.apply_to_pills(
                    &mut app.convert.format,
                    &mut app.convert.output_options,
                );
                app.preset.active_preset = Some(name.to_string());
                app.preset.modified = false;
                Ok(())
            }
            Err(e) => Err(format!("preset '{}' failed: {}", name, e)),
        }
    };

    match app.current_screen {
        AppScreen::Browse => {
            let paths = app.browse.collect_selection_for_queue();

            if paths.is_empty() {
                app.set_status("queue: no selection");
                return;
            }

            let first = paths[0].clone();
            let path_count = paths.len();

            // Probe the first file before touching any state so preset
            // application stays atomic with a successful load.
            let info = match crate::tui::probe::probe_audio(&first) {
                Ok(i) => i,
                Err(e) => {
                    app.set_status(format!("probe error: {}", e));
                    return;
                }
            };
            let metadata = crate::tui::probe::read_metadata(&first).unwrap_or_default();

            // Load preset (if any) after a successful probe.
            if let Some(name) = &preset {
                if let Err(msg) = load_preset_into_pills(app, name) {
                    app.set_status(msg);
                    return;
                }
            }

            // Populate the editable metadata pane from the first file's
            // tags (same for single-file and batch modes — batch edits
            // only affect the cursor file when the user drills in).
            app.convert.metadata.title = metadata.title.clone();
            app.convert.metadata.artist = metadata.artist.clone();
            app.convert.metadata.album = metadata.album.clone();
            app.convert.metadata.genre = metadata.genre.clone();
            app.convert.metadata.year = metadata.year.clone();

            // Build the SourceMode (computes batch summary synchronously
            // for multi-file batches).
            let mut mode = SourceMode::from_paths(paths);
            // Populate the first-file probe result into the appropriate
            // variant so the user sees immediate info on landing.
            match &mut mode {
                SourceMode::Single { info: slot, metadata: meta_slot, .. } => {
                    *slot = Some(info);
                    *meta_slot = metadata;
                }
                SourceMode::Batch { cursor_info, cursor_metadata, .. } => {
                    *cursor_info = Some(info);
                    *cursor_metadata = metadata;
                }
                SourceMode::Empty => {
                    // Unreachable given paths.is_empty() check above.
                }
            }
            app.convert.source.mode = mode;
            app.recent.record_use(&first);

            // Remember where we came from, switch to Convert for review.
            app.previous_screen = Some(AppScreen::Browse);
            app.current_screen = AppScreen::Convert;

            if path_count == 1 {
                app.set_status("review settings, then :commit or :Commit");
            } else {
                app.set_status(format!(
                    "batch of {} files — review settings, then :commit or :Commit",
                    path_count
                ));
            }
        }
        AppScreen::Library => {
            // Placeholder screen. Selection inheritance arrives in 6c.
            if let Some(name) = &preset {
                match load_preset_into_pills(app, name) {
                    Ok(()) => app.set_status(format!("preset loaded: {}", name)),
                    Err(msg) => app.set_status(msg),
                }
            } else {
                app.set_status("library → Convert inheritance arrives in Phase 6c");
            }
        }
        AppScreen::Convert => {
            // Already on Convert. A preset arg loads in place; without an
            // arg, remind the user to pick files in Browse first.
            if let Some(name) = &preset {
                match load_preset_into_pills(app, name) {
                    Ok(()) => app.set_status(format!("preset loaded: {}", name)),
                    Err(msg) => app.set_status(msg),
                }
            } else {
                app.set_status("switch to Browse to pick files, then :queue");
            }
        }
        AppScreen::Queue => {
            if let Some(name) = &preset {
                match load_preset_into_pills(app, name) {
                    Ok(()) => app.set_status(format!("preset loaded: {}", name)),
                    Err(msg) => app.set_status(msg),
                }
            } else {
                app.set_status(":queue: switch to Browse to pick files first");
            }
        }
        _ => {
            app.set_status(":queue not supported on this screen");
        }
    }
}

/// Execute a `:commit` / `:Commit` command. Only valid on the Convert
/// screen with a source file or batch loaded. Enqueues the batch (and
/// optionally starts processing), then navigates away:
/// - `:commit` (start=false): enqueue, return to origin (previous_screen)
/// - `:Commit` (start=true): enqueue, start processing, jump to Queue
///
/// Phase 6d: reads `source.mode.all_paths()` which yields 1 path for
/// Single, N for Batch. The pill state (options) is applied uniformly
/// to all files in the commit — the whole point of reviewing once per
/// batch.
fn execute_commit(
    app: &mut AppState,
    tx: &mpsc::Sender<AppMessage>,
    start: bool,
) {
    if app.current_screen != AppScreen::Convert {
        app.set_status(":commit only works on the Convert screen");
        return;
    }

    // Determine what to commit from the current source mode.
    let batch = app.convert.source.mode.all_paths();
    if batch.is_empty() {
        app.set_status("nothing to commit — no source file loaded");
        return;
    }

    // Build options from the current pill state.
    let options = super::convert_actions::pills_to_options(
        &app.convert.format,
        &app.convert.output_options,
        &app.config,
    );
    let format_name = options.output_format.name();

    // Enqueue the whole batch via the shared helper.
    let outcome = super::convert_actions::commit_batch(app, &batch, &options);

    // Nothing enqueued → don't clear state or navigate; user sees error.
    if outcome.enqueued == 0 {
        if outcome.skipped > 0 && outcome.errors == 0 {
            app.set_status(format!(
                "commit: all {} file(s) already queued",
                outcome.skipped
            ));
        } else {
            app.set_status(format!(
                "commit failed: {} errors, {} skipped",
                outcome.errors, outcome.skipped
            ));
        }
        return;
    }

    // Build the success status message.
    let success_status = if batch.len() == 1 {
        let filename = batch[0]
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        format!("Queued: {} → {}", filename, format_name)
    } else {
        let mut parts = vec![format!("Queued {} → {}", outcome.enqueued, format_name)];
        if outcome.skipped > 0 {
            parts.push(format!("{} skipped", outcome.skipped));
        }
        if outcome.errors > 0 {
            parts.push(format!("{} errors", outcome.errors));
        }
        parts.join(", ")
    };

    // Clear source pane so a subsequent `:queue` arrives fresh.
    app.convert.source.mode = SourceMode::Empty;
    app.convert.metadata = MetadataState::default();

    // Remove only the committed paths from browse.multi_selected so the
    // user's unrelated selection state is preserved. This handles:
    // - Multi-file batches sourced from multi_selected (all committed
    //   paths get removed)
    // - Single-file commits where the file happened to be the only
    //   entry in multi_selected (that entry gets removed)
    // - Single-file commits from `:e` / recent-files / Browse Enter for
    //   a file that is NOT in multi_selected (nothing removed)
    app.browse.multi_selected.retain(|p| !batch.contains(p));

    if start {
        // :Commit — start processing if not already active, land on Queue.
        if !app.processing_active {
            super::convert_actions::start_processing(app, tx);
            app.set_status(format!(
                "Processing {} file(s) → {}",
                outcome.enqueued, format_name
            ));
        } else {
            app.set_status(format!("{} (processing active)", success_status));
        }
        app.previous_screen = None;
        app.current_screen = AppScreen::Queue;
    } else {
        // :commit — return to origin, defaulting to Browse.
        app.set_status(success_status);
        let origin = app.previous_screen.take().unwrap_or(AppScreen::Browse);
        app.current_screen = origin;
        if origin == AppScreen::Browse {
            app.browse.probe_current(tx);
        }
    }
}

/// Execute a `:go` / `:start` command — begin processing whatever's in
/// the queue. No new batch involved. No-op if already processing or if
/// nothing is ready. Works from any screen.
fn execute_go(app: &mut AppState, tx: &mpsc::Sender<AppMessage>) {
    if app.processing_active {
        app.set_status("already processing");
        return;
    }
    // start_processing handles the zero-ready-items case itself.
    super::convert_actions::start_processing(app, tx);
}

/// Execute `:del` / `:delete` / `:trash` — show confirmation, then move
/// selected browse entries to the system trash.
fn execute_delete(app: &mut AppState) {
    if app.current_screen != AppScreen::Browse {
        app.set_status(":del only works on the Browse screen");
        return;
    }

    let paths = collect_selection_for_file_ops(app);
    if paths.is_empty() {
        app.set_status("no files selected");
        return;
    }

    let count = paths.len();
    app.active_overlay = ActiveOverlay::Confirmation {
        message: format!("Move {} item(s) to trash?", count),
        action: ConfirmAction::TrashSelection(paths),
    };
}

/// Execute a `:cp` / `:mv` command. Collects selected files on Browse,
/// then either opens a TextEdit picker for the destination (if no arg)
/// or performs the operation directly (if arg provided).
fn execute_file_op(app: &mut AppState, dest: &str, force: bool, is_move: bool) {
    if app.current_screen != AppScreen::Browse {
        let cmd = if is_move { ":mv" } else { ":cp" };
        app.set_status(format!("{} only works on the Browse screen", cmd));
        return;
    }

    // Collect sources: multi-selected or cursor entry. Unlike
    // collect_selection_for_queue, we DON'T expand directories — copy/move
    // operates on the entry itself, not its contents.
    let sources = collect_selection_for_file_ops(app);
    if sources.is_empty() {
        app.set_status("no files selected for copy/move");
        return;
    }

    let target = if is_move {
        TextEditTarget::BrowseMove { sources, force }
    } else {
        TextEditTarget::BrowseCopy { sources, force }
    };
    let label = if is_move { "move to" } else { "copy to" };

    if dest.trim().is_empty() {
        // No destination arg — open picker pre-filled with current dir.
        let initial = app.browse.current_dir.display().to_string();
        app.active_overlay = ActiveOverlay::TextEdit {
            input: super::text_input::TextInputState::new(
                if initial.ends_with('/') { initial } else { format!("{}/", initial) },
            ),
            target,
            label: label.to_string(),
        };
    } else {
        // Destination provided — perform directly via the apply handler.
        let dest_expanded = expand_path(dest.trim());
        super::keybindings::apply_file_op_pub(app, target, &dest_expanded);
    }
}

/// Collect selected entries for file ops (copy/move). Unlike
/// `collect_selection_for_queue`, directories are NOT expanded — the
/// op targets the directory itself.
fn collect_selection_for_file_ops(app: &AppState) -> Vec<PathBuf> {
    use super::browse::EntryKind;
    if !app.browse.multi_selected.is_empty() {
        return app.browse.multi_selected.clone();
    }
    if let Some(entry) = app.browse.selected_entry() {
        if !matches!(entry.kind, EntryKind::ParentDir) {
            return vec![entry.path.clone()];
        }
    }
    Vec::new()
}

/// Public entry point for metadata editing from context_menu.rs.
pub fn execute_edit_metadata_pub(app: &mut AppState, field: crate::tui::probe::MetadataField) {
    execute_edit_metadata(app, field);
}

/// Execute `:e title` / `:edit artist` etc. — open a TextEdit overlay
/// for the selected audio file's metadata tag on the Browse screen.
fn execute_edit_metadata(app: &mut AppState, field: crate::tui::probe::MetadataField) {
    // Validate: must be on Browse with a selected audio file.
    let entry = match app.browse.selected_entry() {
        Some(e) => e,
        None => {
            app.set_status("edit: no file selected");
            return;
        }
    };
    if !entry.is_audio() {
        app.set_status("edit: selected entry is not an audio file");
        return;
    }
    let path = entry.path.clone();

    // Race check: refuse if the file is currently being converted.
    let is_processing = app.items_snapshot.iter().any(|item| {
        item.input_path == path
            && matches!(
                item.status,
                crate::convert::ConversionStatus::Processing { .. }
            )
    });
    if is_processing {
        app.set_status(format!(
            "cannot edit: {} is currently being converted",
            path.file_name().unwrap_or_default().to_string_lossy()
        ));
        return;
    }

    // Pre-fill with the current value from probe cache (if available).
    let current_value = app
        .browse
        .probe_cache
        .get(&path)
        .and_then(|opt| opt.as_ref())
        .map(|cached| {
            let m = &cached.metadata;
            match field {
                crate::tui::probe::MetadataField::Title => m.title.clone().unwrap_or_default(),
                crate::tui::probe::MetadataField::Artist => m.artist.clone().unwrap_or_default(),
                crate::tui::probe::MetadataField::Album => m.album.clone().unwrap_or_default(),
                crate::tui::probe::MetadataField::Genre => m.genre.clone().unwrap_or_default(),
                crate::tui::probe::MetadataField::Year => m.year.clone().unwrap_or_default(),
            }
        })
        .unwrap_or_default();

    app.active_overlay = ActiveOverlay::TextEdit {
        input: super::text_input::TextInputState::new(current_value),
        target: TextEditTarget::BrowseMetadata { path, field },
        label: format!("edit {}", field.label()),
    };
}

/// Execute `:expand` / `:x` — open the BatchList expand overlay.
/// Only valid when the Convert source is a multi-file Batch.
fn execute_expand(app: &mut AppState) {
    if app.current_screen != AppScreen::Convert {
        app.set_status(":expand only works on the Convert screen");
        return;
    }
    if !app.convert.source.mode.is_batch() {
        app.set_status(":expand: no batch loaded (use :queue from Browse)");
        return;
    }
    app.active_overlay = ActiveOverlay::BatchList { scroll: 0 };
}

/// Execute a `:rename` command for the browse screen.
/// - With no argument: opens a TextEdit overlay seeded with the current name
///   so the user can edit and press Enter to commit.
/// - With an argument: commits the rename directly.
fn execute_rename(app: &mut AppState, new_name: &str, tx: &mpsc::Sender<AppMessage>) {
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
        super::keybindings::commit_browse_rename(app, original_path, trimmed, tx);
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

#[cfg(test)]
mod completion_tests {
    use super::*;
    use crate::tui::text_input::TextInputState;

    // ── compute_completion: command-name completion ──

    #[test]
    fn command_completion_prefix_q() {
        // Typing "q" should match multiple q-prefixed commands.
        let got = compute_completion("q", 1).expect("should have candidates");
        assert_eq!(got.prefix_start, 0);
        assert!(got.candidates.contains(&"q".to_string()));
        assert!(got.candidates.contains(&"quit".to_string()));
        assert!(got.candidates.contains(&"queue".to_string()));
        assert!(got.candidates.contains(&"queue!".to_string()));
    }

    #[test]
    fn command_completion_prefix_commit_uppercase() {
        // Case-sensitive: "Comm" matches "Commit" but not "commit".
        let got = compute_completion("Comm", 4).expect("should have candidates");
        assert_eq!(got.candidates, vec!["Commit".to_string()]);
    }

    #[test]
    fn command_completion_prefix_com_lowercase() {
        // "com" matches "commit" but not "Commit" (case-sensitive).
        let got = compute_completion("com", 3).expect("should have candidates");
        assert!(got.candidates.contains(&"commit".to_string()));
        assert!(!got.candidates.contains(&"Commit".to_string()));
    }

    #[test]
    fn command_completion_prefix_con_matches_convert() {
        // "con" matches "convert" (the "c" alias doesn't match since it's shorter).
        let got = compute_completion("con", 3).expect("should have candidates");
        assert_eq!(got.candidates, vec!["convert".to_string()]);
    }

    #[test]
    fn command_completion_empty_prefix() {
        // Empty input matches all commands.
        let got = compute_completion("", 0).expect("should have candidates");
        assert_eq!(got.candidates.len(), COMMAND_NAMES.len());
    }

    #[test]
    fn command_completion_no_match() {
        // Gibberish matches nothing.
        let got = compute_completion("xyzzy", 5);
        assert!(got.is_none());
    }

    #[test]
    fn command_completion_uppercase_q_no_match() {
        // Case-sensitive: "Q" doesn't match "quit" (lowercase).
        let got = compute_completion("Q", 1);
        assert!(got.is_none());
    }

    // ── compute_completion: preset-arg completion ──

    #[test]
    fn preset_completion_only_for_preset_taking_commands() {
        // "set foo" is NOT a preset-taking command, so no completion
        // after the space.
        let got = compute_completion("set foo", 7);
        assert!(got.is_none());
    }

    #[test]
    fn preset_completion_for_non_preset_command() {
        // "presets foo" (plural, which lists presets) is NOT in
        // PRESET_TAKING_COMMANDS, so no arg completion.
        let got = compute_completion("presets foo", 11);
        assert!(got.is_none());
    }

    // Note: preset completion positive tests can't be unit-tested here
    // without setting up a temp preset directory. The parsing/dispatch
    // logic is covered by the structural tests above.

    // ── apply_completion_to_input ──

    #[test]
    fn apply_replaces_prefix_and_updates_cursor() {
        let mut input = TextInputState::new("q".to_string());
        input.cursor = 1;
        let state = CompletionState {
            candidates: vec!["quit".to_string()],
            cursor: 0,
            prefix_start: 0,
        };
        apply_completion_to_input(&mut input, &state);
        assert_eq!(input.text, "quit");
        assert_eq!(input.cursor, 4);
    }

    #[test]
    fn apply_preserves_text_before_prefix() {
        let mut input = TextInputState::new("queue fo".to_string());
        input.cursor = 8;
        let state = CompletionState {
            candidates: vec!["foobar2k".to_string()],
            cursor: 0,
            prefix_start: 6,
        };
        apply_completion_to_input(&mut input, &state);
        assert_eq!(input.text, "queue foobar2k");
        assert_eq!(input.cursor, 14);
    }

    #[test]
    fn apply_then_cycle_replaces_previous_candidate() {
        // Simulate: type "q" → Tab → first candidate → Tab → second candidate.
        // After first apply, input.cursor is at the end of the first candidate;
        // the second apply should replace the previous candidate entirely.
        let mut input = TextInputState::new("q".to_string());
        input.cursor = 1;
        let mut state = CompletionState {
            candidates: vec!["quit".to_string(), "queue".to_string()],
            cursor: 0,
            prefix_start: 0,
        };
        apply_completion_to_input(&mut input, &state);
        assert_eq!(input.text, "quit");
        assert_eq!(input.cursor, 4);

        // Cycle forward
        cycle_completion(&mut input, &mut state, 1);
        assert_eq!(input.text, "queue");
        assert_eq!(input.cursor, 5);

        // Cycle forward again — wraps back to "quit"
        cycle_completion(&mut input, &mut state, 1);
        assert_eq!(input.text, "quit");
        assert_eq!(input.cursor, 4);
    }

    #[test]
    fn cycle_backward_wraps() {
        let mut input = TextInputState::new("".to_string());
        input.cursor = 0;
        let mut state = CompletionState {
            candidates: vec!["a".to_string(), "b".to_string(), "c".to_string()],
            cursor: 0,
            prefix_start: 0,
        };
        apply_completion_to_input(&mut input, &state);
        assert_eq!(input.text, "a");

        cycle_completion(&mut input, &mut state, -1);
        assert_eq!(input.text, "c");

        cycle_completion(&mut input, &mut state, -1);
        assert_eq!(input.text, "b");

        cycle_completion(&mut input, &mut state, -1);
        assert_eq!(input.text, "a");
    }
}
