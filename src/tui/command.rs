//! Vi-style command mode: parsing and execution

use std::path::PathBuf;
use tokio::sync::mpsc;

use super::app::*;
use super::message::AppMessage;
use crate::convert::formats::AudioFormat;
use crate::convert::simple_wizard::DitherType;

/// Full list of command-mode tokens (including aliases) recognised by
/// `parse_command`. Used by the tab-completion machinery.
///
/// Ordered so more-typed commands come first — matters for UX because
/// the first match is what Tab shows initially before the user cycles.
pub const COMMAND_NAMES: &[&str] = &[
    "q",
    "quit",
    "exit",
    "w",
    "write",
    "save",
    "wq",
    "e",
    "edit",
    "o",
    "output",
    "cd",
    "queue",
    "queue!",
    "qa",
    "qa!",
    "c",
    "convert",
    "commit",
    "Commit",
    "go",
    "start",
    "expand",
    "x",
    "batch",
    "preset",
    "presets",
    "saveas",
    "set",
    "fx",
    "effects",
    "info",
    "tools",
    "h",
    "help",
    "sort",
    "sortdir",
    "filter",
    "rename",
    "del",
    "delete",
    "trash",
    "cp",
    "cp!",
    "copy",
    "copy!",
    "mv",
    "mv!",
    "move",
    "move!",
    "browse",
    "b",
    "recent",
    "recents",
    "bookmarks",
    "bm",
    "rename-all",
    "renameall",
    "bulk-rename",
    "password",
    "pw",
    "analyze",
    "analyze!",
    "analysis",
    "dr",
    "write-dr",
    "writedr",
    "write-rg-track",
    "write-rg-album",
    "import-cue",
    "cue",
    "cue!",
    "cue-mb",
    "cue-mb!",
    "cue-fill",
    "cue-enrich",
    "cue-view",
    "tags-mb",
    "mb-tags",
    "musicbrainz-tags",
    "revert",
    "restore",
    "g",
    "G",
    "top",
    "bot",
    "bottom",
    "fix-caps",
    "fixcaps",
    "search",
    "s",
    "rs",
    "rsearch",
    "context",
    "menu",
    "ar",
    "ar!",
    "accuraterip",
    "accuraterip!",
    "ar-fix",
    "ar-batch",
    "ctdb",
    "ctdb-repair",
    "cuetools",
    "cuetools-repair",
    "view",
    "cat",
    "edit-file",
    "ef",
];

/// Commands that take a preset name as their argument. Used by the
/// completion machinery to decide whether the word after the command
/// should be completed against preset file names.
pub const PRESET_TAKING_COMMANDS: &[&str] =
    &["queue", "queue!", "qa", "qa!", "c", "convert", "preset"];

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
pub fn compute_completion(text: &str, cursor: usize) -> Option<CompletionState> {
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
    input
        .text
        .replace_range(state.prefix_start..prefix_end, candidate);
    input.cursor = state.prefix_start + candidate.len();
}

/// Argument to `:area` for SACD area switching in the metadata
/// editor. `Toggle` flips between stereo and multi-channel when
/// both are present; an explicit kind is a no-op if that area
/// isn't on the disc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SacdAreaTarget {
    Stereo,
    MultiChannel,
    Toggle,
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
    Queue {
        preset: Option<String>,
    },
    /// Commit the currently-reviewed file/batch from the Convert screen to
    /// the queue. `:commit` (lowercase) enqueues only; `:Commit` (capital)
    /// enqueues AND starts processing, jumping to the Queue screen.
    Commit {
        start: bool,
    },
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
    Copy {
        dest: String,
        force: bool,
    },
    /// Move selected file(s) to a destination. Empty arg opens picker.
    /// `:mv!` replaces existing. Falls back to copy+delete across filesystems.
    Move {
        dest: String,
        force: bool,
    },
    /// Switch to the browse screen. On the convert screen, sets
    /// the return target so a selected file loads back into the source pane.
    Browse,
    /// Open the recent-files overlay.
    Recent,
    /// Open the bookmarks overlay (browse-only). With no args, opens in
    /// browsing mode. With "add [name]", quick-adds the current browse
    /// directory as a bookmark without opening the overlay.
    Bookmarks(String),
    /// Open the bulk rename wizard for the current selection.
    BulkRename,
    /// Analyze selected audio file(s) — DR, peak, clipping, etc.
    /// `force`: if true, skip cache and re-analyze.
    Analyze {
        force: bool,
    },
    /// Write DR analysis report to each album directory.
    WriteDr,
    /// Write ReplayGain track tags via loudgain.
    WriteRgTrack,
    /// Write ReplayGain album + track tags via loudgain.
    WriteRgAlbum,
    /// Import metadata from a CUE sheet via external editor + review.
    ImportCue,
    /// Apply capitalization rules to TITLE, ARTIST, ALBUM fields.
    FixCaps,
    /// Verify integrity of selected audio file(s).
    Verify,
    /// Generate a CUE sheet from the selected audio files.
    /// `single_image`: false = multi-file, true = single image with cumulative timestamps.
    GenerateCue {
        single_image: bool,
    },
    /// Generate a CUE sheet driven by a MusicBrainz disc-TOC lookup. Title,
    /// performer, ISRC, catalog, barcode all come from MB; durations and
    /// pregaps come from local probe + EAC log.
    GenerateCueMb {
        single_image: bool,
    },
    /// Read a colocated CUE, fill empty/absent fields from a MusicBrainz
    /// disc-TOC lookup, write back. Preserves the existing CUE form
    /// (single-image vs multi-file) and user-typed values.
    CueFill,
    /// View the embedded CUE sheet on the metadata-editor cursor row
    /// (synthetic-preview entry, e.g. CUESHEET) in a read-only
    /// CuePreview overlay. Parks the metadata editor; Esc restores.
    CueView,
    /// Scroll the parked CUE preview overlay to the top.
    CueScrollTop,
    /// Scroll the parked CUE preview overlay to the bottom.
    CueScrollBottom,
    /// Begin editing the 1-based `line` of the parked CUE preview.
    CueEditLine(usize),
    /// Open the metadata editor pre-populated with track-level + album-level
    /// values from a MusicBrainz disc-TOC lookup.
    ///
    /// All fields `None` (the bare `:tags-mb` form) keeps today's
    /// behavior: TOC-primary with seed-from-editor text fallback.
    /// Any field `Some` switches to **direct text search** (TOC is
    /// skipped) using the supplied values as the seed. Free-form text
    /// goes to the album field; `--catno` and `--year` flags fill
    /// `catalog` and `year` directly.
    TagsFromMb {
        query: Option<String>,
        catno: Option<String>,
        year: Option<String>,
    },
    /// Toggle the cursor row of the metadata editor between its
    /// MB-proposed value and the file's original (pre-MB) value.
    /// No-op when the row wasn't touched by MB or has been manually
    /// edited away from both endpoints.
    MbRevert,
    MbRestore,
    /// Metadata editor: open the "add new field" prompt. Colon-form
    /// of the `a` bare-char key.
    MetaAdd,
    /// Metadata editor: mark the cursor row for deletion (visible as
    /// strikethrough until save). Colon-form of `d`.
    MetaDelete,
    /// Metadata editor: un-delete the cursor row. Colon-form of `u`.
    MetaUndelete,
    /// Metadata editor: force-open the per-file detail overlay (even
    /// for non-mixed entries with multi-track dim). Colon-form of `D`.
    MetaDetail,
    /// Metadata editor: re-load per-track TITLE / ARTIST / ISRC from
    /// the sidecar `.cue` file alongside the audio. Useful when the
    /// user has edited the sidecar externally OR when the file has
    /// both an embedded CUESHEET and a sidecar and the user wants
    /// the sidecar's values to win.
    TagsCueSidecar,
    /// Return from the metadata editor to the MbSelect picker
    /// (cached release list — no MB requery). Confirmation if the
    /// editor is dirty (any edits / proposed-from-MB values not yet
    /// reverted). No-op when the editor wasn't reached through MB.
    MbBack,
    /// Return from the metadata editor to the GnudbReview surface
    /// (cached review state — no gnudb requery). Mirror of MbBack
    /// for the gnudb flow. No-op when the editor wasn't reached
    /// through gnudb.
    GnudbBack,
    /// Switch the SACD metadata editor to a specific area. The
    /// argument distinguishes stereo / mch / toggle (which flips
    /// to the area not currently shown). No-op when the editor
    /// isn't open on a SACD ISO, or when the requested area isn't
    /// present on the disc. Triggers a full editor rebuild from
    /// the parsed SacdMetadata + sidecar; unsaved edits are
    /// preserved on a best-effort basis (per-track values for
    /// fields the new area also has, by track index).
    SacdSwitchArea(SacdAreaTarget),
    /// Mark the current browse selection as the bit-compare reference.
    MarkCompareRef,
    /// Run bit comparison: current selection vs stored reference.
    BitCompare,
    /// Clear the stored bit-compare reference.
    ClearCompareRef,
    /// Direct comparison of two explicit paths (`:compare path1 path2`).
    ComparePaths {
        path1: String,
        path2: String,
    },
    /// Detect CD pre-emphasis on selected audio file(s).
    DetectPreemphasis,
    /// Train the pre-emphasis corpus model from a directory of non-PE audio.
    TrainPreemphCorpus {
        path: String,
    },
    /// Calibrate the pre-emphasis LDA classifier from labeled PE and non-PE directories.
    CalibratePreemphasis {
        pe_dir: String,
        non_pe_dir: String,
    },
    /// Open the search panel.
    Search {
        recursive: bool,
    },
    /// Set an archive password for the selected archive in Browse.
    Password,
    /// Open the context menu at the current selection.
    ContextMenu,
    /// AccurateRip verification on selected audio files/folder.
    /// `force`: if true, full offset scan (-1200 to +1200).
    AccurateRip {
        force: bool,
    },
    /// Apply AccurateRip offset correction.
    ArFix,
    /// Batch AccurateRip verification of current browse directory.
    ArBatch,
    /// CUETools DB verification.
    Ctdb,
    /// CUETools DB Reed-Solomon repair.
    CtdbRepair,
    /// View a text file in read-only mode.
    ViewFile(std::path::PathBuf),
    /// Edit a text file (not .log files).
    EditFile(std::path::PathBuf),
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
        // Metadata-editor actions. Route via execute_command's
        // with_editor_state helper to the corresponding helper in
        // keybindings.rs. Bare-char a/d/D/u/w keys these commands
        // replaced have been removed (no-bare-char-keys rule);
        // KeyCode::Delete remains as a convenience shortcut for :d.
        // Editor `:w` save lives in Command::Write (overlay-aware).
        // `delete` (without short alias) is taken by browse trash;
        // editor delete is `:d` only. `add` overlaps with bookmarks
        // sub-args but bookmarks parses its own `add` inside the
        // execute_bookmarks helper (top-level `:add` here is safe).
        "a" | "add" => Command::MetaAdd,
        "d" => Command::MetaDelete,
        "u" | "undelete" => Command::MetaUndelete,
        "D" | "detail" => Command::MetaDetail,
        "tags-cue-sidecar" | "tags-cue" => Command::TagsCueSidecar,
        "mb-back" | "tags-mb-back" => Command::MbBack,
        "gnudb-back" | "tags-gnudb-back" => Command::GnudbBack,
        "area" | "sacd-area" => {
            let target = match args.trim().to_ascii_lowercase().as_str() {
                "stereo" | "2ch" | "two-channel" | "2.0" => SacdAreaTarget::Stereo,
                "mch" | "mc" | "multi-channel" | "multichannel" | "5.1" => {
                    SacdAreaTarget::MultiChannel
                }
                "" | "toggle" => SacdAreaTarget::Toggle,
                other => {
                    return Command::Unknown(format!(":area unknown target '{}'", other));
                }
            };
            Command::SacdSwitchArea(target)
        }
        "e" | "edit" => {
            // `:e <N>` (positive integer) targets a line in the parked
            // CUE preview overlay. Anything else falls through to the
            // existing path-or-field handler.
            if let Ok(line) = args.trim().parse::<usize>() {
                if line >= 1 {
                    return Command::CueEditLine(line);
                }
            }
            Command::Edit(args.to_string())
        }
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
            let arg = if args.is_empty() {
                None
            } else {
                Some(args.to_string())
            };
            Command::Filter(arg)
        }
        "del" | "delete" | "trash" => Command::Delete,
        "rename" => Command::Rename(args.to_string()),
        "cp" | "copy" => Command::Copy {
            dest: args.to_string(),
            force: false,
        },
        "cp!" | "copy!" => Command::Copy {
            dest: args.to_string(),
            force: true,
        },
        "mv" | "move" => Command::Move {
            dest: args.to_string(),
            force: false,
        },
        "mv!" | "move!" => Command::Move {
            dest: args.to_string(),
            force: true,
        },
        "browse" | "b" => Command::Browse,
        "recent" | "recents" => Command::Recent,
        "bookmarks" | "bm" => Command::Bookmarks(args.to_string()),
        "rename-all" | "renameall" | "bulk-rename" => Command::BulkRename,
        "analyze" | "analysis" | "dr" => Command::Analyze { force: false },
        "analyze!" => Command::Analyze { force: true },
        "verify" | "test" => Command::Verify,
        "cue" => Command::GenerateCue {
            single_image: false,
        },
        "cue!" => Command::GenerateCue { single_image: true },
        "cue-mb" => Command::GenerateCueMb {
            single_image: false,
        },
        "cue-mb!" => Command::GenerateCueMb { single_image: true },
        "cue-fill" | "cue-enrich" => Command::CueFill,
        "cue-view" => Command::CueView,
        "tags-mb" | "mb-tags" | "musicbrainz-tags" => parse_tags_mb_args(args),
        "revert" => Command::MbRevert,
        "restore" => Command::MbRestore,
        "g" | "top" => Command::CueScrollTop,
        "G" | "bot" | "bottom" => Command::CueScrollBottom,
        "preemphasis" | "preemph" | "pe" => Command::DetectPreemphasis,
        "preemph-calibrate" | "pe-calibrate" => {
            let mut parts = args.splitn(2, char::is_whitespace);
            let pe_dir = parts.next().unwrap_or("").trim().to_string();
            let non_pe_dir = parts.next().unwrap_or("").trim().to_string();
            if pe_dir.is_empty() || non_pe_dir.is_empty() {
                Command::Unknown("usage: :preemph-calibrate <pe_dir> <non_pe_dir>".into())
            } else {
                Command::CalibratePreemphasis { pe_dir, non_pe_dir }
            }
        }
        "preemph-train" | "pe-train" | "train-corpus" => {
            let path = if args.is_empty() {
                dirs::home_dir()
                    .map(|h| h.join("preemph-dev").to_string_lossy().to_string())
                    .unwrap_or_else(|| "~/preemph-dev".to_string())
            } else {
                args.to_string()
            };
            Command::TrainPreemphCorpus { path }
        }
        "mark" => Command::MarkCompareRef,
        "unmark" | "clearref" => Command::ClearCompareRef,
        "compare" | "cmp" => {
            // :compare path1 path2
            let mut parts = args.splitn(2, char::is_whitespace);
            let p1 = parts.next().unwrap_or("").trim().to_string();
            let p2 = parts.next().unwrap_or("").trim().to_string();
            if p1.is_empty() {
                // No args: compare selection vs reference.
                Command::BitCompare
            } else if p2.is_empty() {
                Command::Unknown(format!("compare: need two paths (got one: {})", p1))
            } else {
                Command::ComparePaths {
                    path1: p1,
                    path2: p2,
                }
            }
        }
        "search" | "s" => Command::Search { recursive: false },
        "rs" | "rsearch" => Command::Search { recursive: true },
        "write-dr" | "writedr" => Command::WriteDr,
        "write-rg-track" => Command::WriteRgTrack,
        "write-rg-album" => Command::WriteRgAlbum,
        "import-cue" => Command::ImportCue,
        "fix-caps" | "fixcaps" => Command::FixCaps,
        "password" | "pw" => Command::Password,
        "context" | "menu" => Command::ContextMenu,
        "ar" | "accuraterip" => Command::AccurateRip { force: false },
        "ar!" | "accuraterip!" => Command::AccurateRip { force: true },
        "ar-fix" => Command::ArFix,
        "ar-batch" => Command::ArBatch,
        "ctdb" | "cuetools" => Command::Ctdb,
        "ctdb-repair" | "cuetools-repair" => Command::CtdbRepair,
        "view" | "cat" => Command::ViewFile(std::path::PathBuf::from(args)),
        "edit-file" | "ef" => Command::EditFile(std::path::PathBuf::from(args)),
        _ => Command::Unknown(input.to_string()),
    }
}

const TAGS_MB_USAGE: &str = "usage: :tags-mb [--catno VALUE] [--year YYYY] [text query]";

/// Tokenize a `:tags-mb` arg string into whitespace-separated tokens,
/// respecting double-quoted strings (no escapes). Catalog numbers
/// commonly contain spaces (`SRGS 4520`, `ESGA 509`) so `--catno
/// "SRGS 4520"` must be a single token.
fn tokenize_tags_mb_args(input: &str) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_quote = false;
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                if in_quote {
                    out.push(std::mem::take(&mut cur));
                    in_quote = false;
                } else {
                    in_quote = true;
                }
            }
            c if c.is_whitespace() && !in_quote => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if in_quote {
        return Err("unterminated quoted string".to_string());
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    Ok(out)
}

/// Parse the arg string for the `:tags-mb` family of commands.
/// Bare (empty args) form maps to `Command::TagsFromMb { None, None, None }`,
/// preserving today's TOC-primary-with-seed-fallback behavior.
/// Any non-empty arg returns the struct populated for direct text
/// search (TOC will be skipped at dispatch). Errors land as
/// `Command::Unknown(usage_or_specific_error)`.
fn parse_tags_mb_args(args: &str) -> Command {
    let trimmed = args.trim();
    if trimmed.is_empty() {
        return Command::TagsFromMb {
            query: None,
            catno: None,
            year: None,
        };
    }
    let tokens = match tokenize_tags_mb_args(trimmed) {
        Ok(t) => t,
        Err(e) => return Command::Unknown(format!(":tags-mb: {} — {}", e, TAGS_MB_USAGE)),
    };

    let mut catno: Option<String> = None;
    let mut year: Option<String> = None;
    let mut text_parts: Vec<String> = Vec::new();
    let mut iter = tokens.into_iter();
    while let Some(tok) = iter.next() {
        match tok.as_str() {
            "--catno" => {
                let Some(val) = iter.next() else {
                    return Command::Unknown(format!(
                        ":tags-mb: --catno requires a value — {}",
                        TAGS_MB_USAGE
                    ));
                };
                if val.is_empty() {
                    return Command::Unknown(format!(
                        ":tags-mb: --catno value must be non-empty — {}",
                        TAGS_MB_USAGE
                    ));
                }
                if catno.is_some() {
                    return Command::Unknown(format!(
                        ":tags-mb: --catno specified twice — {}",
                        TAGS_MB_USAGE
                    ));
                }
                catno = Some(val);
            }
            "--year" => {
                let Some(val) = iter.next() else {
                    return Command::Unknown(format!(
                        ":tags-mb: --year requires a value — {}",
                        TAGS_MB_USAGE
                    ));
                };
                if val.is_empty() {
                    return Command::Unknown(format!(
                        ":tags-mb: --year value must be non-empty — {}",
                        TAGS_MB_USAGE
                    ));
                }
                if year.is_some() {
                    return Command::Unknown(format!(
                        ":tags-mb: --year specified twice — {}",
                        TAGS_MB_USAGE
                    ));
                }
                year = Some(val);
            }
            s if s.starts_with("--") => {
                return Command::Unknown(format!(
                    ":tags-mb: unknown flag '{}' — {}",
                    s, TAGS_MB_USAGE
                ));
            }
            _ => text_parts.push(tok),
        }
    }

    let query = if text_parts.is_empty() {
        None
    } else {
        Some(text_parts.join(" "))
    };

    if query.is_none() && catno.is_none() && year.is_none() {
        // Tokenization consumed everything (e.g. just whitespace
        // inside a quoted block). Surface as a usage error rather
        // than dispatching an empty search.
        return Command::Unknown(format!(":tags-mb: empty query — {}", TAGS_MB_USAGE));
    }

    Command::TagsFromMb { query, catno, year }
}

/// Execute a parsed command against app state
/// Run `f` on the active metadata editor state. State may be parked
/// in `pending_metadata_editor` (colon command from the command bar)
/// or live in `active_overlay` (mouse-pill click that restored before
/// dispatching). Restores state to active_overlay after `f` runs,
/// unless `f` (or something it called) set a different overlay.
fn with_editor_state<F>(app: &mut AppState, mut f: F)
where
    F: FnMut(&mut super::app::MetadataEditorState),
{
    let mut state = if let Some(parked) = app.pending_metadata_editor.take() {
        parked
    } else if matches!(
        app.active_overlay,
        super::app::ActiveOverlay::MetadataEditor(_)
    ) {
        let prev = std::mem::replace(&mut app.active_overlay, super::app::ActiveOverlay::None);
        if let super::app::ActiveOverlay::MetadataEditor(s) = prev {
            s
        } else {
            unreachable!()
        }
    } else {
        app.set_status("metadata-editor command requires the editor to be active");
        return;
    };
    f(&mut state);
    if matches!(app.active_overlay, super::app::ActiveOverlay::None) {
        app.active_overlay = super::app::ActiveOverlay::MetadataEditor(state);
    }
}

/// Like `with_editor_state` but also threads `app` and `tx` into the
/// closure. Used by `:w` save which needs both.
fn with_editor_state_and_tx<F>(app: &mut AppState, tx: &mpsc::Sender<AppMessage>, mut f: F)
where
    F: FnMut(&mut AppState, &mut super::app::MetadataEditorState, &mpsc::Sender<AppMessage>),
{
    let mut state = if let Some(parked) = app.pending_metadata_editor.take() {
        parked
    } else if matches!(
        app.active_overlay,
        super::app::ActiveOverlay::MetadataEditor(_)
    ) {
        let prev = std::mem::replace(&mut app.active_overlay, super::app::ActiveOverlay::None);
        if let super::app::ActiveOverlay::MetadataEditor(s) = prev {
            s
        } else {
            unreachable!()
        }
    } else {
        app.set_status("metadata-editor command requires the editor to be active");
        return;
    };
    f(app, &mut state, tx);
    if matches!(app.active_overlay, super::app::ActiveOverlay::None) {
        app.active_overlay = super::app::ActiveOverlay::MetadataEditor(state);
    }
}

pub fn execute_command(app: &mut AppState, cmd: Command, tx: &mpsc::Sender<AppMessage>) {
    match cmd {
        Command::Quit => {
            // If a CUE preview is parked, :q just cancels the preview
            // rather than quitting the app.
            if app.pending_cue_preview.take().is_some() {
                app.set_status("CUE preview cancelled".to_string());
                return;
            }
            app.should_quit = true;
        }
        Command::Write => {
            // If the metadata editor is parked or active, :w saves
            // tags (Phase 4 regen + per-file lofty write) via
            // metadata_editor_save (extracted helper in keybindings.rs).
            if app.pending_metadata_editor.is_some()
                || matches!(
                    app.active_overlay,
                    super::app::ActiveOverlay::MetadataEditor(_)
                )
            {
                with_editor_state_and_tx(app, tx, |app, state, tx| {
                    super::keybindings::metadata_editor_save(app, state, tx)
                });
                return;
            }
            // If a CUE preview is parked, :w writes the previewed CUE.
            if let Some(state) = app.pending_cue_preview.take() {
                if let Err(reason) = super::cue_generate::validate_cue_content(&state.content) {
                    app.pending_cue_preview = Some(state);
                    app.set_status(format!("CUE invalid, not written: {}", reason));
                    return;
                }
                let path = state.write_path.clone();
                match std::fs::write(&path, &state.content) {
                    Ok(()) => {
                        let name = path
                            .file_name()
                            .map(|s| s.to_string_lossy().to_string())
                            .unwrap_or_else(|| path.display().to_string());
                        app.set_status(format!("CUE written: {}", name));
                        if app.current_screen == AppScreen::Browse {
                            app.browse.refresh();
                            app.browse.probe_current_with_db(tx, Some(&app.db));
                        }
                    }
                    Err(e) => {
                        // Re-park so the user can retry.
                        app.pending_cue_preview = Some(state);
                        app.set_status(format!("CUE write failed: {}", e));
                    }
                }
                return;
            }
            if let Some(name) = &app.preset.active_preset.clone() {
                let preset = super::presets::TuiPreset::from_pill_state(
                    name,
                    &app.convert.format,
                    &app.convert.output_options,
                );
                match super::presets::save_preset_with_db(&preset, &app.db) {
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
            // If a CUE preview is parked, :wq writes the CUE then closes
            // the overlay (does not quit the app).
            if let Some(state) = app.pending_cue_preview.take() {
                if let Err(reason) = super::cue_generate::validate_cue_content(&state.content) {
                    app.pending_cue_preview = Some(state);
                    app.set_status(format!("CUE invalid, not written: {}", reason));
                    return;
                }
                let path = state.write_path.clone();
                match std::fs::write(&path, &state.content) {
                    Ok(()) => {
                        let name = path
                            .file_name()
                            .map(|s| s.to_string_lossy().to_string())
                            .unwrap_or_else(|| path.display().to_string());
                        app.set_status(format!("CUE written: {}", name));
                        if app.current_screen == AppScreen::Browse {
                            app.browse.refresh();
                            app.browse.probe_current_with_db(tx, Some(&app.db));
                        }
                    }
                    Err(e) => {
                        app.pending_cue_preview = Some(state);
                        app.set_status(format!("CUE write failed: {}", e));
                    }
                }
                return;
            }
            if let Some(name) = &app.preset.active_preset.clone() {
                let preset = super::presets::TuiPreset::from_pill_state(
                    name,
                    &app.convert.format,
                    &app.convert.output_options,
                );
                super::presets::save_preset_with_db(&preset, &app.db).ok();
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

            app.convert.source.mode = SourceMode::from_single(p.clone(), Some(info), metadata);
            app.set_status(format!(
                "Loaded: {}",
                p.file_name().unwrap_or_default().to_string_lossy()
            ));
            app.current_screen = AppScreen::Convert;
            app.recent.record_use_with_db(&p, &app.db);
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
                    // If async, the status + probe happen in the
                    // PathValidationComplete / DirScanComplete handlers.
                    // If sync fallback, current_dir is already updated.
                    if !app.browse.is_async_enabled() {
                        let p = app.browse.current_dir.display().to_string();
                        app.set_status(format!("cd: {}", p));
                        app.browse.probe_current_with_db(tx, Some(&app.db));
                    } else {
                        app.set_status(format!("Resolving: {}", path));
                    }
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
                    &name,
                    &app.convert.format,
                    &app.convert.output_options,
                );
                match super::presets::save_preset_with_db(&preset, &app.db) {
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
            app.active_overlay = ActiveOverlay::Help {
                screen: app.current_screen,
                scroll: 0,
            };
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
            app.browse.probe_current_with_db(tx, Some(&app.db));
        }
        Command::Recent => {
            app.recent.open_overlay();
        }
        Command::Bookmarks(args) => {
            execute_bookmarks(app, &args);
        }
        Command::Password => {
            if app.current_screen != AppScreen::Browse {
                app.set_status(":password only works on the browse screen");
            } else if let Some(entry) = app.browse.selected_entry() {
                if matches!(entry.kind, super::browse::EntryKind::Archive) {
                    let path = entry.path.clone();
                    app.active_overlay = ActiveOverlay::TextEdit {
                        input: super::text_input::TextInputState::empty(),
                        target: TextEditTarget::ArchivePassword(path),
                        label: "archive password".to_string(),
                    };
                } else {
                    app.set_status("Selected file is not an archive");
                }
            } else {
                app.set_status("No file selected");
            }
        }
        Command::Analyze { force } => {
            // Collect paths to analyze from the current context.
            // On Browse, directories are expanded recursively to find
            // nested audio files (e.g., disc 01/disc 02 folders).
            let mut paths: Vec<std::path::PathBuf> = match app.current_screen {
                AppScreen::Browse => {
                    let sel = collect_selection_for_file_ops(app);
                    super::browse::expand_paths_to_audio(&sel)
                        .into_iter()
                        .filter(|p| {
                            matches!(
                                super::browse::classify_file(p),
                                super::browse::EntryKind::AudioFile(_)
                            )
                        })
                        .collect()
                }
                AppScreen::Convert => app.convert.source.mode.all_paths(),
                _ => Vec::new(),
            };
            // Sort by disc/track for logical result order.
            super::probe::sort_paths_by_track(&mut paths);
            // Check for single-image CUE layout.
            if paths.len() <= 1 {
                let dir = if paths.is_empty() {
                    let sel = collect_selection_for_file_ops(app);
                    sel.first().and_then(|p| {
                        if p.is_dir() {
                            Some(p.clone())
                        } else {
                            p.parent().map(|d| d.to_path_buf())
                        }
                    })
                } else {
                    paths[0].parent().map(|d| d.to_path_buf())
                };
                if let Some(ref dir) = dir {
                    if let Some(info) = super::cue_parser::detect_single_image(dir) {
                        let n = info.track_boundaries.len();
                        let can_seek = super::cue_parser::can_ffmpeg_read(&info.audio_path);
                        app.set_status(format!("Analyzing {} tracks (single image)...", n,));
                        app.analysis_results.clear();
                        app.analysis_pending = n;

                        // Build display names from CUE metadata.
                        let display_paths: Vec<std::path::PathBuf> = info
                            .sheet
                            .tracks
                            .iter()
                            .map(|t| {
                                let name = format!(
                                    "{:02} - {}.flac",
                                    t.number,
                                    t.title.as_deref().unwrap_or("Track"),
                                );
                                info.audio_path
                                    .parent()
                                    .unwrap_or(std::path::Path::new("."))
                                    .join(name)
                            })
                            .collect();

                        if can_seek {
                            // Fast path: seek-based analysis (no temp files).
                            for (i, &(start, count)) in info.track_boundaries.iter().enumerate() {
                                let audio_path = info.audio_path.clone();
                                let display_path = display_paths[i].clone();
                                let original_path = info.audio_path.clone();
                                let tx = tx.clone();
                                tokio::spawn(async move {
                                    let pcm_path = audio_path.clone();

                                    let seek = start as f64 / info.sample_rate as f64;
                                    let dur = count as f64 / info.sample_rate as f64;
                                    let (pcm_result, hdcd_result) = tokio::join!(
                                        tokio::task::spawn_blocking(move || {
                                            super::analyze::analyze_file(
                                                &pcm_path,
                                                Some(start),
                                                Some(count),
                                            )
                                        }),
                                        super::analyze::detect_hdcd(
                                            &audio_path,
                                            Some(seek),
                                            Some(dur)
                                        ),
                                    );
                                    // LUFS: skip for seek-based (loudgain needs a real file per track).

                                    let final_result = match pcm_result {
                                        Ok(Ok(mut result)) => {
                                            result.path = display_path;

                                            let pe_evidence = super::preemphasis::metadata::check_tag_evidence(&original_path)
                                                .or_else(|| super::preemphasis::metadata::check_file_evidence(&original_path));
                                            let catalog_match =
                                                super::preemphasis::catalog::check_catalog_evidence(
                                                    &original_path,
                                                );

                                            if let Some(ev) = pe_evidence {
                                                result.preemphasis = Some(super::preemphasis::PreemphasisConfidence::StrongCandidate);
                                                result.preemphasis_detail = Some(format!(
                                                    "{} indicates source disc used pre-emphasis; verify de-emphasis was not applied during ripping",
                                                    ev.label()
                                                ));
                                            } else if let Some(cm) = catalog_match {
                                                result.preemphasis = Some(super::preemphasis::PreemphasisConfidence::StrongCandidate);
                                                result.preemphasis_detail = Some(format!(
                                                    "{}; verify de-emphasis was not applied during ripping",
                                                    cm.detail
                                                ));
                                            }

                                            if result.declared_bit_depth == Some(16)
                                                || (result.declared_bit_depth.is_none()
                                                    && result.actual_bit_depth <= 16)
                                            {
                                                if let Some(hdcd) = hdcd_result {
                                                    result.hdcd_detected = Some(hdcd.detected);
                                                    if hdcd.detected {
                                                        result.hdcd_detail = Some(hdcd.detail);
                                                    }
                                                }
                                            }

                                            Ok(Box::new(result))
                                        }
                                        Ok(Err(e)) => Err(format!("track {}: {}", i + 1, e)),
                                        Err(e) => Err(format!("task panicked: {}", e)),
                                    };
                                    let _ = tx
                                        .send(AppMessage::AnalysisComplete {
                                            result: final_result,
                                        })
                                        .await;
                                });
                            }
                        } else {
                            // Slow path: extract to temp files (WavPack v4, etc.).
                            let tmp_dir = std::env::temp_dir()
                                .join(format!("tonepoet-analyze-{}", std::process::id()));
                            app.analysis_temp_dir = Some(tmp_dir.clone());
                            let tx = tx.clone();
                            tokio::spawn(async move {
                                if let Err(e) = std::fs::create_dir_all(&tmp_dir) {
                                    for _ in 0..n {
                                        let _ = tx
                                            .send(AppMessage::AnalysisComplete {
                                                result: Err(format!("temp dir failed: {}", e)),
                                            })
                                            .await;
                                    }
                                    return;
                                }
                                let track_paths = match tokio::task::spawn_blocking({
                                    let info = info.clone();
                                    let tmp_dir = tmp_dir.clone();
                                    move || {
                                        super::cue_parser::extract_single_image_tracks(
                                            &info, &tmp_dir,
                                        )
                                    }
                                })
                                .await
                                {
                                    Ok(Ok(paths)) => paths,
                                    result => {
                                        let msg = match result {
                                            Ok(Err(e)) => format!("extraction failed: {}", e),
                                            Err(e) => format!("extraction task failed: {}", e),
                                            _ => unreachable!(),
                                        };
                                        for _ in 0..n {
                                            let _ = tx
                                                .send(AppMessage::AnalysisComplete {
                                                    result: Err(msg.clone()),
                                                })
                                                .await;
                                        }
                                        return;
                                    }
                                };
                                for (i, temp_path) in track_paths.into_iter().enumerate() {
                                    let display_path = display_paths[i].clone();
                                    let original_path = info.audio_path.clone();
                                    let tx = tx.clone();
                                    tokio::spawn(async move {
                                        let pcm_path = temp_path.clone();
                                        let lufs_path = temp_path.clone();
                                        let hdcd_path = temp_path;
                                        let (pcm_result, lufs_result, hdcd_result) = tokio::join!(
                                            tokio::task::spawn_blocking(move || {
                                                super::analyze::analyze_file(&pcm_path, None, None)
                                            }),
                                            super::analyze::measure_loudness(&lufs_path),
                                            super::analyze::detect_hdcd(&hdcd_path, None, None),
                                        );
                                        let final_result = match pcm_result {
                                            Ok(Ok(mut result)) => {
                                                result.path = display_path;
                                                if let Some((lufs, tp)) = lufs_result {
                                                    result.lufs = Some(lufs);
                                                    result.true_peak_dbtp = Some(tp);
                                                }
                                                let pe_evidence = super::preemphasis::metadata::check_tag_evidence(&original_path)
                                                    .or_else(|| super::preemphasis::metadata::check_file_evidence(&original_path));
                                                let catalog_match = super::preemphasis::catalog::check_catalog_evidence(&original_path);
                                                if let Some(ev) = pe_evidence {
                                                    result.preemphasis = Some(super::preemphasis::PreemphasisConfidence::StrongCandidate);
                                                    result.preemphasis_detail = Some(format!(
                                                        "{} indicates source disc used pre-emphasis; verify de-emphasis was not applied during ripping",
                                                        ev.label()
                                                    ));
                                                } else if let Some(cm) = catalog_match {
                                                    result.preemphasis = Some(super::preemphasis::PreemphasisConfidence::StrongCandidate);
                                                    result.preemphasis_detail = Some(format!(
                                                        "{}; verify de-emphasis was not applied during ripping",
                                                        cm.detail
                                                    ));
                                                }
                                                if result.declared_bit_depth == Some(16)
                                                    || (result.declared_bit_depth.is_none()
                                                        && result.actual_bit_depth <= 16)
                                                {
                                                    if let Some(hdcd) = hdcd_result {
                                                        result.hdcd_detected = Some(hdcd.detected);
                                                        if hdcd.detected {
                                                            result.hdcd_detail = Some(hdcd.detail);
                                                        }
                                                    }
                                                }
                                                Ok(Box::new(result))
                                            }
                                            Ok(Err(e)) => Err(format!("track {}: {}", i + 1, e)),
                                            Err(e) => Err(format!("task panicked: {}", e)),
                                        };
                                        let _ = tx
                                            .send(AppMessage::AnalysisComplete {
                                                result: final_result,
                                            })
                                            .await;
                                    });
                                }
                            });
                        }
                        return;
                    }
                }
            }
            if paths.is_empty() {
                app.set_status("No audio files to analyze");
            } else {
                app.analysis_results.clear();

                // Check DB cache for each path; only spawn analysis for misses.
                // :analyze! skips the cache entirely.
                let mut to_analyze = Vec::new();
                if force {
                    to_analyze = paths.clone();
                } else {
                    for path in &paths {
                        let cached = std::fs::metadata(path).ok().and_then(|meta| {
                            let mtime = meta
                                .modified()
                                .map(crate::db::systemtime_to_unix)
                                .unwrap_or(0);
                            app.db.get_cached_analysis(
                                &path.display().to_string(),
                                mtime,
                                meta.len(),
                            )
                        });
                        if let Some(result) = cached {
                            app.analysis_results.push(result);
                        } else {
                            to_analyze.push(path.clone());
                        }
                    }
                }

                if to_analyze.is_empty() {
                    // All results served from cache — show overlay immediately.
                    let count = app.analysis_results.len();
                    let last = &app.analysis_results[count - 1];
                    let name = last
                        .path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    app.set_status(format!(
                        "Analyzed: {} — DR{} ({}) [cached]",
                        name,
                        last.dr_value,
                        super::analyze::dr_label(last.dr_value),
                    ));
                    app.active_overlay = super::app::ActiveOverlay::Analysis { scroll: 0 };
                } else {
                    app.analysis_pending = to_analyze.len();
                    for path in to_analyze {
                        let tx = tx.clone();
                        tokio::spawn(async move {
                            let pcm_path = path.clone();
                            let lufs_path = path.clone();
                            let hdcd_path = path;

                            // Run PCM analysis, loudgain, and HDCD detection in parallel.
                            let (pcm_result, lufs_result, hdcd_result) = tokio::join!(
                                tokio::task::spawn_blocking(move || {
                                    super::analyze::analyze_file(&pcm_path, None, None)
                                }),
                                super::analyze::measure_loudness(&lufs_path),
                                super::analyze::detect_hdcd(&hdcd_path, None, None),
                            );

                            // Merge results and send (always send to decrement pending counter).
                            let final_result = match pcm_result {
                                Ok(Ok(mut result)) => {
                                    if let Some((lufs, tp)) = lufs_result {
                                        result.lufs = Some(lufs);
                                        result.true_peak_dbtp = Some(tp);
                                    }

                                    // Fast pre-emphasis detection (metadata + catalog only, no spectral).
                                    let pe_path = result.path.clone();
                                    let pe_evidence =
                                        super::preemphasis::metadata::check_tag_evidence(&pe_path)
                                            .or_else(|| {
                                                super::preemphasis::metadata::check_file_evidence(
                                                    &pe_path,
                                                )
                                            });
                                    let catalog_match =
                                        super::preemphasis::catalog::check_catalog_evidence(
                                            &pe_path,
                                        );

                                    if let Some(ev) = pe_evidence {
                                        result.preemphasis = Some(super::preemphasis::PreemphasisConfidence::StrongCandidate);
                                        result.preemphasis_detail = Some(format!(
                                            "{} indicates source disc used pre-emphasis; verify de-emphasis was not applied during ripping",
                                            ev.label()
                                        ));
                                    } else if let Some(cm) = catalog_match {
                                        result.preemphasis = Some(super::preemphasis::PreemphasisConfidence::StrongCandidate);
                                        result.preemphasis_detail = Some(format!(
                                            "{}; verify de-emphasis was not applied during ripping",
                                            cm.detail
                                        ));
                                    }

                                    // HDCD detection (only meaningful for 16-bit sources).
                                    if result.declared_bit_depth == Some(16)
                                        || (result.declared_bit_depth.is_none()
                                            && result.actual_bit_depth <= 16)
                                    {
                                        if let Some(hdcd) = hdcd_result {
                                            result.hdcd_detected = Some(hdcd.detected);
                                            if hdcd.detected {
                                                result.hdcd_detail = Some(hdcd.detail);
                                            }
                                        }
                                    }

                                    Ok(Box::new(result))
                                }
                                Ok(Err(e)) => {
                                    let name = lufs_path
                                        .file_name()
                                        .map(|n| n.to_string_lossy().to_string())
                                        .unwrap_or_default();
                                    Err(format!("{}: {}", name, e))
                                }
                                Err(e) => Err(format!("task panicked: {}", e)),
                            };
                            let _ = tx
                                .send(AppMessage::AnalysisComplete {
                                    result: final_result,
                                })
                                .await;
                        });
                    }
                }
            }
        }
        Command::Verify => {
            let paths: Vec<std::path::PathBuf> = match app.current_screen {
                AppScreen::Browse => {
                    let sel = collect_selection_for_file_ops(app);
                    super::browse::expand_paths_to_audio(&sel)
                        .into_iter()
                        .filter(|p| {
                            matches!(
                                super::browse::classify_file(p),
                                super::browse::EntryKind::AudioFile(_)
                            )
                        })
                        .collect()
                }
                AppScreen::Convert => app.convert.source.mode.all_paths(),
                _ => Vec::new(),
            };
            if paths.is_empty() {
                app.set_status("No audio files to verify");
            } else {
                app.verify_results.clear();
                app.verify_pending = paths.len();
                for path in paths {
                    let tx = tx.clone();
                    tokio::spawn(async move {
                        let result = super::verify::verify_file(path).await;
                        let _ = tx.send(AppMessage::VerifyComplete { result }).await;
                    });
                }
                app.set_status(format!("Verifying {} file(s)...", app.verify_pending));
            }
        }
        Command::GenerateCue { single_image } => {
            let mut paths: Vec<std::path::PathBuf> = match app.current_screen {
                AppScreen::Browse => {
                    let sel = collect_selection_for_file_ops(app);
                    super::browse::expand_paths_to_audio(&sel)
                        .into_iter()
                        .filter(|p| {
                            matches!(
                                super::browse::classify_file(p),
                                super::browse::EntryKind::AudioFile(_)
                            )
                        })
                        .collect()
                }
                _ => Vec::new(),
            };
            if paths.is_empty() {
                app.set_status("No audio files for CUE generation");
            } else {
                super::probe::sort_paths_by_track(&mut paths);

                // CUE file goes in the root of the selection. When a
                // directory was selected, use that directory; when individual
                // files were selected, use their parent.
                let output_dir = if app.current_screen == AppScreen::Browse {
                    let sel = collect_selection_for_file_ops(app);
                    if sel.len() == 1 && sel[0].is_dir() {
                        sel[0].clone()
                    } else {
                        paths[0]
                            .parent()
                            .unwrap_or_else(|| std::path::Path::new("."))
                            .to_path_buf()
                    }
                } else {
                    paths[0]
                        .parent()
                        .unwrap_or_else(|| std::path::Path::new("."))
                        .to_path_buf()
                };

                match super::cue_generate::gather_cue_info(&paths, &output_dir) {
                    Ok((album, tracks)) => {
                        let pregap_count =
                            tracks.iter().filter(|t| t.pregap_frames.is_some()).count();
                        let cue_content = if single_image {
                            let image_name =
                                super::cue_generate::derive_image_filename(&album, &paths[0]);
                            let ext = paths[0]
                                .extension()
                                .and_then(|e| e.to_str())
                                .unwrap_or("flac");
                            let fmt = super::cue_generate::cue_format_tag(ext);
                            super::cue_generate::generate_single_image_cue(
                                &album,
                                &tracks,
                                &image_name,
                                fmt,
                            )
                        } else {
                            super::cue_generate::generate_multifile_cue(&album, &tracks)
                        };

                        let cue_filename = super::cue_generate::cue_output_filename(&album);
                        let cue_path = output_dir.join(&cue_filename);

                        match std::fs::write(&cue_path, &cue_content) {
                            Ok(()) => {
                                let mode = if single_image {
                                    "single image"
                                } else {
                                    "multi-file"
                                };
                                let pregap_note = if pregap_count > 0 {
                                    format!(
                                        ", with {} EAC pregap{}",
                                        pregap_count,
                                        if pregap_count == 1 { "" } else { "s" }
                                    )
                                } else {
                                    String::new()
                                };
                                app.set_status(format!(
                                    "CUE sheet ({}{}) written: {}",
                                    mode, pregap_note, cue_filename,
                                ));
                                // Refresh browse to show the new file.
                                if app.current_screen == AppScreen::Browse {
                                    app.browse.refresh();
                                    app.browse.probe_current_with_db(tx, Some(&app.db));
                                }
                            }
                            Err(e) => {
                                app.set_status(format!("CUE write failed: {}", e));
                            }
                        }
                    }
                    Err(e) => {
                        app.set_status(format!("CUE generation failed: {}", e));
                    }
                }
            }
        }
        Command::GenerateCueMb { single_image } => {
            let mut paths: Vec<std::path::PathBuf> = match app.current_screen {
                AppScreen::Browse => {
                    let sel = collect_selection_for_file_ops(app);
                    super::browse::expand_paths_to_audio(&sel)
                        .into_iter()
                        .filter(|p| {
                            matches!(
                                super::browse::classify_file(p),
                                super::browse::EntryKind::AudioFile(_)
                            )
                        })
                        .collect()
                }
                _ => Vec::new(),
            };
            if paths.is_empty() {
                app.set_status("No audio files for MusicBrainz CUE generation");
            } else {
                super::probe::sort_paths_by_track(&mut paths);
                let output_dir = if app.current_screen == AppScreen::Browse {
                    let sel = collect_selection_for_file_ops(app);
                    if sel.len() == 1 && sel[0].is_dir() {
                        sel[0].clone()
                    } else {
                        paths[0]
                            .parent()
                            .unwrap_or_else(|| std::path::Path::new("."))
                            .to_path_buf()
                    }
                } else {
                    paths[0]
                        .parent()
                        .unwrap_or_else(|| std::path::Path::new("."))
                        .to_path_buf()
                };

                // Compute TOC sectors (with 150 leadin). Try a colocated
                // EAC log / single-image CUE first, fall back to deriving
                // from sample counts of the selected tracks.
                let sectors: Vec<u32> = match super::accuraterip::find_toc_offsets(&output_dir) {
                    Some(s) => s,
                    None => match super::accuraterip::collect_sample_counts(&paths) {
                        Ok((sample_counts, sample_rate)) => {
                            let samples_per_frame = (sample_rate / 75) as u64;
                            let mut sectors = Vec::with_capacity(sample_counts.len() + 1);
                            let mut frame: u64 = 150;
                            for &count in &sample_counts {
                                sectors.push(frame as u32);
                                frame += count / samples_per_frame;
                            }
                            sectors.push(frame as u32);
                            sectors
                        }
                        Err(e) => {
                            app.set_status(format!("MusicBrainz CUE: {}", e));
                            return;
                        }
                    },
                };

                let toc_string = match super::musicbrainz::build_mb_toc(&sectors) {
                    Some(s) => s,
                    None => {
                        app.set_status("MusicBrainz CUE: TOC too short".to_string());
                        return;
                    }
                };
                let cached = app.db.get_cached_mb_response(&toc_string);
                let n_cached = if cached.is_some() {
                    "cached"
                } else {
                    "fetching"
                };
                app.set_status(format!(
                    "MusicBrainz CUE: {} disc TOC ({} tracks)…",
                    n_cached,
                    sectors.len() - 1,
                ));

                let tx = tx.clone();
                let toc_for_msg = toc_string.clone();
                let paths_for_msg = paths.clone();
                let output_dir_for_msg = output_dir.clone();
                tokio::spawn(async move {
                    let outcome = super::musicbrainz::lookup_release_by_toc(&sectors, cached).await;
                    let _ = tx
                        .send(AppMessage::CueMbComplete {
                            outcome,
                            paths: paths_for_msg,
                            output_dir: output_dir_for_msg,
                            single_image,
                            toc_string: toc_for_msg,
                        })
                        .await;
                });
            }
        }
        Command::MbRevert => {
            let Some(mut state) = app.pending_metadata_editor.take() else {
                app.set_status(":revert only works in the metadata editor");
                return;
            };
            // In the detail overlay, :revert is a field-level toggle
            // (operates on per_file_values vs the MB-proposed set).
            // Outside, it's the cursor-row value-based toggle.
            let in_detail = state.phase == super::app::MetadataEditorPhase::DetailEdit;
            let target_idx = if in_detail {
                state.detail_field_idx
            } else {
                state.cursor
            };
            if let Some(entry) = state.entries.get_mut(target_idx) {
                if in_detail {
                    let pill_before = super::probe::mb_pill_state_field(entry);
                    if matches!(pill_before, super::probe::MbRevertPill::None) {
                        app.set_status(
                            ":revert: field was not changed by MusicBrainz, or has manual edits",
                        );
                    } else {
                        super::probe::toggle_mb_revert_field(entry);
                        let after = super::probe::mb_pill_state_field(entry);
                        let display_key = entry.display_key.clone();
                        state.dirty = super::probe::metadata_editor_has_changes(&state);
                        let label = match after {
                            super::probe::MbRevertPill::Revert => "applied MB values",
                            super::probe::MbRevertPill::UseMb => "reverted to file values",
                            super::probe::MbRevertPill::None => "no change",
                        };
                        app.set_status(format!(":revert: {} ({})", display_key, label));
                    }
                } else {
                    let pill_before = super::probe::mb_pill_state(entry);
                    if matches!(pill_before, super::probe::MbRevertPill::None) {
                        app.set_status(":revert: cursor row was not changed by MusicBrainz");
                    } else {
                        super::probe::toggle_mb_revert(entry);
                        let after = super::probe::mb_pill_state(entry);
                        let display_key = entry.display_key.clone();
                        state.dirty = super::probe::metadata_editor_has_changes(&state);
                        let label = match after {
                            super::probe::MbRevertPill::Revert => "applied MB value",
                            super::probe::MbRevertPill::UseMb => "reverted to file value",
                            super::probe::MbRevertPill::None => "no change",
                        };
                        app.set_status(format!(":revert: {} ({})", display_key, label));
                    }
                }
            }
            app.active_overlay = super::app::ActiveOverlay::MetadataEditor(state);
        }
        Command::MbRestore => {
            let Some(mut state) = app.pending_metadata_editor.take() else {
                app.set_status(":restore only works in the metadata editor");
                return;
            };
            // Field-target: in the detail overlay we use detail_field_idx;
            // in the main editor we use the cursor row, so the user can
            // restore a non-mixed row that doesn't surface the bulk pill.
            let in_detail = state.phase == super::app::MetadataEditorPhase::DetailEdit;
            let target_idx = if in_detail {
                state.detail_field_idx
            } else {
                state.cursor
            };
            if let Some(entry) = state.entries.get_mut(target_idx) {
                if !super::probe::entry_has_mb_proposed(entry) {
                    app.set_status(":restore: field was not populated from MusicBrainz");
                } else {
                    super::probe::restore_mb_proposed(entry);
                    let display_key = entry.display_key.clone();
                    state.dirty = super::probe::metadata_editor_has_changes(&state);
                    app.set_status(format!(":restore: {} (snapped to MB values)", display_key));
                }
            }
            app.active_overlay = super::app::ActiveOverlay::MetadataEditor(state);
        }
        Command::MetaAdd => {
            with_editor_state(app, |state| {
                super::keybindings::metadata_editor_open_add(state)
            });
        }
        Command::MetaDelete => {
            with_editor_state(app, |state| {
                super::keybindings::metadata_editor_delete_cursor(state)
            });
        }
        Command::MetaUndelete => {
            with_editor_state(app, |state| {
                super::keybindings::metadata_editor_undelete_cursor(state)
            });
        }
        Command::MetaDetail => {
            with_editor_state(app, |state| {
                super::keybindings::metadata_editor_open_detail(state)
            });
        }
        Command::MbBack => {
            // Editor state may be in pending (colon command from
            // command-bar) or active_overlay (mouse-pill click that
            // restored before dispatching). Take from either.
            let state = if let Some(parked) = app.pending_metadata_editor.take() {
                parked
            } else if matches!(
                app.active_overlay,
                super::app::ActiveOverlay::MetadataEditor(_)
            ) {
                let prev =
                    std::mem::replace(&mut app.active_overlay, super::app::ActiveOverlay::None);
                if let super::app::ActiveOverlay::MetadataEditor(s) = prev {
                    s
                } else {
                    unreachable!()
                }
            } else {
                app.set_status(":mb-back only works in the metadata editor");
                return;
            };
            let Some(cache) = state.mb_back.clone() else {
                app.set_status(
                    ":mb-back: no MB lookup to return to (run :tags-mb first)".to_string(),
                );
                app.active_overlay = super::app::ActiveOverlay::MetadataEditor(state);
                return;
            };
            if state.dirty {
                // Park the editor on pending so the user can cancel
                // and come back. ConfirmAction::MbBack carries the
                // cached release list for the post-confirm transition.
                app.pending_metadata_editor = Some(state);
                app.active_overlay = super::app::ActiveOverlay::Confirmation {
                    action: super::app::ConfirmAction::MbBack(cache),
                    message: "Discard editor changes and return to MB picker?".to_string(),
                };
                return;
            }
            // Not dirty — go directly back, no confirmation.
            drop(state);
            let mut mb_state = super::app::MbSelectState::new(cache.releases, cache.paths);
            mb_state.selected = cache.selected;
            app.active_overlay = super::app::ActiveOverlay::MbSelect(Box::new(mb_state));
            app.set_status(":mb-back: pick a different release".to_string());
        }
        Command::GnudbBack => {
            // Mirror of Command::MbBack for the gnudb flow.
            let state = if let Some(parked) = app.pending_metadata_editor.take() {
                parked
            } else if matches!(
                app.active_overlay,
                super::app::ActiveOverlay::MetadataEditor(_)
            ) {
                let prev =
                    std::mem::replace(&mut app.active_overlay, super::app::ActiveOverlay::None);
                if let super::app::ActiveOverlay::MetadataEditor(s) = prev {
                    s
                } else {
                    unreachable!()
                }
            } else {
                app.set_status(":gnudb-back only works in the metadata editor");
                return;
            };
            let Some(review) = state.gnudb_back.clone() else {
                app.set_status(
                    ":gnudb-back: no gnudb review to return to (run :tags-gnudb first)".to_string(),
                );
                app.active_overlay = super::app::ActiveOverlay::MetadataEditor(state);
                return;
            };
            if state.dirty {
                app.pending_metadata_editor = Some(state);
                app.active_overlay = super::app::ActiveOverlay::Confirmation {
                    action: super::app::ConfirmAction::GnudbBack(review),
                    message: "Discard editor changes and return to gnudb review?".to_string(),
                };
                return;
            }
            drop(state);
            app.active_overlay = super::app::ActiveOverlay::GnudbReview(review);
            app.set_status(":gnudb-back: review per-track values".to_string());
        }
        Command::SacdSwitchArea(target) => {
            let mut state = if let Some(parked) = app.pending_metadata_editor.take() {
                parked
            } else if matches!(
                app.active_overlay,
                super::app::ActiveOverlay::MetadataEditor(_)
            ) {
                let prev =
                    std::mem::replace(&mut app.active_overlay, super::app::ActiveOverlay::None);
                if let super::app::ActiveOverlay::MetadataEditor(s) = prev {
                    s
                } else {
                    unreachable!()
                }
            } else {
                app.set_status(":area only works in the metadata editor");
                return;
            };
            // SACD only.
            let Some(_) = state.sacd_area_kind else {
                app.set_status(":area: editor is not on a SACD ISO");
                app.active_overlay = super::app::ActiveOverlay::MetadataEditor(state);
                return;
            };
            let iso_path = match state.paths.first().cloned() {
                Some(p) => p,
                None => {
                    app.set_status(":area: no source path on editor state");
                    app.active_overlay = super::app::ActiveOverlay::MetadataEditor(state);
                    return;
                }
            };
            if state.dirty {
                app.set_status(
                    ":area: editor has unsaved edits — save (:w) or discard (:q!) first",
                );
                app.active_overlay = super::app::ActiveOverlay::MetadataEditor(state);
                return;
            }
            match super::keybindings::switch_sacd_editor_area(&mut state, &iso_path, target) {
                Ok(label) => app.set_status(format!(":area → {}", label)),
                Err(reason) => app.set_status(reason),
            }
            app.active_overlay = super::app::ActiveOverlay::MetadataEditor(state);
        }
        Command::TagsCueSidecar => {
            let Some(mut state) = app.pending_metadata_editor.take() else {
                app.set_status(":tags-cue-sidecar only works in the metadata editor");
                return;
            };
            match super::keybindings::reload_from_sidecar_cue(&mut state) {
                Ok(msg) => app.set_status(msg),
                Err(reason) => app.set_status(reason),
            }
            app.active_overlay = super::app::ActiveOverlay::MetadataEditor(state);
        }
        Command::CueView => {
            let Some(state) = app.pending_metadata_editor.take() else {
                app.set_status(":cue-view only works in the metadata editor");
                return;
            };
            let cursor = state.cursor;
            let entry = match state.entries.get(cursor) {
                Some(e) => e,
                None => {
                    app.set_status(":cue-view: no entry at cursor");
                    app.active_overlay = super::app::ActiveOverlay::MetadataEditor(state);
                    return;
                }
            };
            if !super::probe::is_synthetic_preview(entry) {
                app.set_status(format!(
                    ":cue-view: row '{}' has no embedded CUE sheet",
                    entry.display_key,
                ));
                app.active_overlay = super::app::ActiveOverlay::MetadataEditor(state);
                return;
            }
            let content = entry.value.clone();
            let summary = format!(
                "{} (read-only · {})",
                entry.display_key,
                super::probe::cue_summary_string(&content),
            );
            app.pending_metadata_editor = Some(state);
            app.active_overlay = super::app::ActiveOverlay::CuePreview(Box::new(
                super::app::CuePreviewState::new_readonly(content, summary),
            ));
        }
        Command::TagsFromMb { query, catno, year } => {
            let explicit_args = query.is_some() || catno.is_some() || year.is_some();
            let direct_seed = if explicit_args {
                Some(SacdMbSeed {
                    artist: String::new(),
                    album: query.clone().unwrap_or_default(),
                    catalog: catno.clone(),
                    year: year.clone(),
                })
            } else {
                None
            };

            // C-2d: Browse + single SACD ISO selection + no editor
            // open → auto-open the SACD editor first, then fall
            // through to in-editor dispatch which uses the TOC path.
            // Without this, right-click → "Get tags from MusicBrainz"
            // on an ISO surfaces the editor-first hint instead of
            // doing what the user asked.
            if app.current_screen == AppScreen::Browse
                && !matches!(
                    app.active_overlay,
                    super::app::ActiveOverlay::MetadataEditor(_)
                )
                && app.pending_metadata_editor.is_none()
            {
                let sel = collect_selection_for_file_ops(app);
                let sacd_isos: Vec<std::path::PathBuf> = sel
                    .iter()
                    .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("iso")))
                    .filter(|p| super::sacd::is_sacd_iso(p))
                    .cloned()
                    .collect();
                let has_audio = sel.iter().any(|p| {
                    matches!(
                        super::browse::classify_file(p),
                        super::browse::EntryKind::AudioFile(_)
                    )
                });

                if sacd_isos.len() > 1 {
                    app.set_status(":tags-mb: multiple SACD ISOs selected — select one at a time");
                    return;
                }
                if !sacd_isos.is_empty() && has_audio {
                    app.set_status(
                        ":tags-mb: mixed selection (SACD ISO + audio files) — select one type",
                    );
                    return;
                }
                if let Some(iso) = sacd_isos.into_iter().next() {
                    super::keybindings::open_metadata_editor_for_sacd(app, iso);
                    // If parse failed (or any reason left the editor
                    // unset), the open helper already set a clear
                    // status; surface it instead of letting the
                    // Browse fallthrough overwrite with a less
                    // specific hint.
                    if !matches!(
                        app.active_overlay,
                        super::app::ActiveOverlay::MetadataEditor(_),
                    ) {
                        return;
                    }
                }
            }

            // In-editor dispatch (SACD or regular file). Returns Some
            // when an editor was in scope; the Browse-path fall-through
            // below runs only when no editor is open. `direct_seed` is
            // `Some` when the user supplied explicit args — the
            // in-editor dispatch then skips TOC and goes straight to
            // text search.
            if let Some(dispatched) = try_dispatch_in_editor_tags_mb(app, tx, direct_seed.clone()) {
                if dispatched {
                    return;
                }
            }

            let mut paths: Vec<std::path::PathBuf> = match app.current_screen {
                AppScreen::Browse => {
                    let sel = collect_selection_for_file_ops(app);
                    super::browse::expand_paths_to_audio(&sel)
                        .into_iter()
                        .filter(|p| {
                            matches!(
                                super::browse::classify_file(p),
                                super::browse::EntryKind::AudioFile(_)
                            )
                        })
                        .collect()
                }
                _ => Vec::new(),
            };
            if paths.is_empty() {
                // SACD ISOs are handled by the auto-open block at the
                // top of this arm (C-2d); anything reaching here is
                // genuinely a "no audio files" case (empty selection,
                // data-disc `.iso`, or unrecognized extensions).
                app.set_status(":tags-mb: no audio files in selection");
                return;
            }
            super::probe::sort_paths_by_track(&mut paths);

            // Explicit args from the user override the TOC primary
            // path: fire a direct text search instead.
            if let Some(seed) = direct_seed {
                let ctx = super::message::TagsMbContext {
                    paths,
                    editor_park: false,
                    fallback_seed: None,
                };
                super::event_loop::spawn_tags_mb_text_search(
                    app,
                    tx,
                    seed,
                    ctx,
                    super::event_loop::TextSearchMode::DirectRequest,
                );
                return;
            }

            let dir = paths[0]
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."))
                .to_path_buf();

            let sectors: Vec<u32> = match super::accuraterip::find_toc_offsets(&dir) {
                Some(s) => s,
                None => match super::accuraterip::collect_sample_counts(&paths) {
                    Ok((sample_counts, sample_rate)) => {
                        let samples_per_frame = (sample_rate / 75) as u64;
                        let mut sectors = Vec::with_capacity(sample_counts.len() + 1);
                        let mut frame: u64 = 150;
                        for &count in &sample_counts {
                            sectors.push(frame as u32);
                            frame += count / samples_per_frame;
                        }
                        sectors.push(frame as u32);
                        sectors
                    }
                    Err(e) => {
                        app.set_status(format!(":tags-mb: {}", e));
                        return;
                    }
                },
            };
            let toc_string = match super::musicbrainz::build_mb_toc(&sectors) {
                Some(s) => s,
                None => {
                    app.set_status(":tags-mb: TOC too short".to_string());
                    return;
                }
            };
            spawn_tags_mb_toc_lookup(
                app, tx, sectors, toc_string, paths, /* editor_park */ false,
                /* fallback_seed */ None,
            );
        }
        Command::CueFill => {
            let mut paths: Vec<std::path::PathBuf> = match app.current_screen {
                AppScreen::Browse => {
                    let sel = collect_selection_for_file_ops(app);
                    super::browse::expand_paths_to_audio(&sel)
                        .into_iter()
                        .filter(|p| {
                            matches!(
                                super::browse::classify_file(p),
                                super::browse::EntryKind::AudioFile(_)
                            )
                        })
                        .collect()
                }
                _ => Vec::new(),
            };
            if paths.is_empty() {
                app.set_status(":cue-fill: no audio files in selection");
                return;
            }
            super::probe::sort_paths_by_track(&mut paths);
            let output_dir = if app.current_screen == AppScreen::Browse {
                let sel = collect_selection_for_file_ops(app);
                if sel.len() == 1 && sel[0].is_dir() {
                    sel[0].clone()
                } else {
                    paths[0]
                        .parent()
                        .unwrap_or_else(|| std::path::Path::new("."))
                        .to_path_buf()
                }
            } else {
                paths[0]
                    .parent()
                    .unwrap_or_else(|| std::path::Path::new("."))
                    .to_path_buf()
            };

            let cue_path = match super::accuraterip::find_cue_file(&output_dir) {
                Some(p) => p,
                None => {
                    app.set_status(
                        ":cue-fill: no .cue file in directory (use :cue-mb to generate one)",
                    );
                    return;
                }
            };
            let sheet = match super::cue_parser::parse_cue_file(&cue_path) {
                Ok(s) => s,
                Err(e) => {
                    app.set_status(format!(":cue-fill: parse failed: {}", e));
                    return;
                }
            };
            if sheet.tracks.is_empty() {
                app.set_status(":cue-fill: parsed CUE has no tracks");
                return;
            }

            // Detect layout. Single-image = unique audio files in CUE == 1
            // and multiple tracks. We pass either 1 or N audio paths into
            // the bridge accordingly.
            let unique_files: std::collections::HashSet<&str> = sheet
                .tracks
                .iter()
                .filter_map(|t| t.file.as_deref())
                .collect();
            let single_image = unique_files.len() == 1 && sheet.tracks.len() > 1;
            let bridge_paths: Vec<std::path::PathBuf> = if single_image {
                vec![paths[0].clone()]
            } else {
                if paths.len() != sheet.tracks.len() {
                    app.set_status(format!(
                        ":cue-fill: CUE has {} tracks but {} audio files in selection",
                        sheet.tracks.len(),
                        paths.len(),
                    ));
                    return;
                }
                paths.clone()
            };

            let layout = if single_image {
                let image_filename = bridge_paths[0]
                    .strip_prefix(&output_dir)
                    .unwrap_or(&bridge_paths[0])
                    .to_string_lossy()
                    .to_string();
                let ext = bridge_paths[0]
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("flac");
                super::message::CueFillLayout::SingleImage {
                    image_filename,
                    format_tag: super::cue_generate::cue_format_tag(ext).to_string(),
                }
            } else {
                super::message::CueFillLayout::MultiFile
            };

            let (album, tracks) = match super::cue_generate::cue_sheet_to_track_info(
                &sheet,
                &bridge_paths,
                &output_dir,
            ) {
                Ok(pair) => pair,
                Err(e) => {
                    app.set_status(format!(":cue-fill: {}", e));
                    return;
                }
            };

            // TOC for MB lookup. Reuse find_toc_offsets first; fall back
            // to deriving from sample counts of the audio paths.
            let toc_paths: Vec<std::path::PathBuf> = if single_image {
                vec![bridge_paths[0].clone()]
            } else {
                paths.clone()
            };
            let sectors: Vec<u32> = match super::accuraterip::find_toc_offsets(&output_dir) {
                Some(s) => s,
                None => match super::accuraterip::collect_sample_counts(&toc_paths) {
                    Ok((sample_counts, sample_rate)) => {
                        let samples_per_frame = (sample_rate / 75) as u64;
                        let mut sectors = Vec::with_capacity(sample_counts.len() + 1);
                        let mut frame: u64 = 150;
                        for &count in &sample_counts {
                            sectors.push(frame as u32);
                            frame += count / samples_per_frame;
                        }
                        sectors.push(frame as u32);
                        sectors
                    }
                    Err(e) => {
                        app.set_status(format!(":cue-fill: {}", e));
                        return;
                    }
                },
            };
            let toc_string = match super::musicbrainz::build_mb_toc(&sectors) {
                Some(s) => s,
                None => {
                    app.set_status(":cue-fill: TOC too short".to_string());
                    return;
                }
            };
            let cached = app.db.get_cached_mb_response(&toc_string);
            let n_cached = if cached.is_some() {
                "cached"
            } else {
                "fetching"
            };
            app.set_status(format!(
                ":cue-fill: {} disc TOC ({} tracks)…",
                n_cached,
                sectors.len() - 1,
            ));

            let tx = tx.clone();
            let toc_for_msg = toc_string.clone();
            tokio::spawn(async move {
                let outcome = super::musicbrainz::lookup_release_by_toc(&sectors, cached).await;
                let _ = tx
                    .send(AppMessage::CueFillComplete {
                        outcome,
                        cue_path,
                        album: Box::new(album),
                        tracks,
                        layout,
                        toc_string: toc_for_msg,
                    })
                    .await;
            });
        }
        Command::CueScrollTop => {
            if let Some(mut state) = app.pending_cue_preview.take() {
                state.scroll = 0;
                app.active_overlay = super::app::ActiveOverlay::CuePreview(state);
            }
        }
        Command::CueScrollBottom => {
            if let Some(mut state) = app.pending_cue_preview.take() {
                let last = state.content.lines().count().saturating_sub(1);
                state.scroll = last;
                app.active_overlay = super::app::ActiveOverlay::CuePreview(state);
            }
        }
        Command::CueEditLine(line_1based) => {
            let Some(mut state) = app.pending_cue_preview.take() else {
                app.set_status("no CUE preview to edit");
                return;
            };
            let idx = line_1based.saturating_sub(1);
            if state.begin_edit(idx) {
                // Auto-scroll so the edited line is visible.
                state.scroll = idx.saturating_sub(2);
                app.active_overlay = super::app::ActiveOverlay::CuePreview(state);
            } else {
                let total = state.line_count();
                app.pending_cue_preview = Some(state);
                app.set_status(format!("line {} out of range (1..={})", line_1based, total,));
            }
        }
        Command::MarkCompareRef => {
            if app.current_screen != AppScreen::Browse {
                app.set_status(":mark only works on the browse screen");
            } else {
                let sel = collect_selection_for_file_ops(app);
                let mut paths: Vec<std::path::PathBuf> = super::browse::expand_paths_to_audio(&sel)
                    .into_iter()
                    .filter(|p| {
                        matches!(
                            super::browse::classify_file(p),
                            super::browse::EntryKind::AudioFile(_)
                        )
                    })
                    .collect();
                if paths.is_empty() {
                    app.set_status("No audio files to mark as reference");
                } else {
                    super::probe::sort_paths_by_track(&mut paths);
                    let count = paths.len();
                    app.compare_reference = paths;
                    app.set_status(format!("Marked {} file(s) as compare reference", count,));
                }
            }
        }
        Command::ClearCompareRef => {
            if app.compare_reference.is_empty() {
                app.set_status("No compare reference set");
            } else {
                app.compare_reference.clear();
                app.set_status("Compare reference cleared");
            }
        }
        Command::BitCompare => {
            if app.compare_reference.is_empty() {
                app.set_status("No compare reference set (use :mark first)");
            } else if app.current_screen != AppScreen::Browse {
                app.set_status(":compare only works on the browse screen");
            } else {
                let sel = collect_selection_for_file_ops(app);
                let mut targets: Vec<std::path::PathBuf> =
                    super::browse::expand_paths_to_audio(&sel)
                        .into_iter()
                        .filter(|p| {
                            matches!(
                                super::browse::classify_file(p),
                                super::browse::EntryKind::AudioFile(_)
                            )
                        })
                        .collect();
                if targets.is_empty() {
                    app.set_status("No audio files selected for comparison");
                } else {
                    super::probe::sort_paths_by_track(&mut targets);
                    let refs = &app.compare_reference;
                    if refs.len() != targets.len() {
                        app.set_status(format!(
                            "Reference has {} file(s) but target has {} — counts must match",
                            refs.len(),
                            targets.len(),
                        ));
                    } else {
                        app.compare_results.clear();
                        app.compare_pending = refs.len();
                        for (ref_path, target_path) in refs.iter().zip(targets.iter()) {
                            let tx = tx.clone();
                            let rp = ref_path.clone();
                            let tp = target_path.clone();
                            tokio::spawn(async move {
                                let result = super::bit_compare::compare_files(rp, tp).await;
                                let _ = tx.send(AppMessage::CompareComplete { result }).await;
                            });
                        }
                        app.set_status(format!("Comparing {} pair(s)...", app.compare_pending,));
                    }
                }
            }
        }
        Command::ComparePaths { path1, path2 } => {
            let p1 = std::path::PathBuf::from(&path1);
            let p2 = std::path::PathBuf::from(&path2);
            if !p1.exists() {
                app.set_status(format!("compare: not found: {}", path1));
            } else if !p2.exists() {
                app.set_status(format!("compare: not found: {}", path2));
            } else {
                app.compare_results.clear();
                app.compare_pending = 1;
                let tx = tx.clone();
                tokio::spawn(async move {
                    let result = super::bit_compare::compare_files(p1, p2).await;
                    let _ = tx.send(AppMessage::CompareComplete { result }).await;
                });
                app.set_status("Comparing...");
            }
        }
        Command::DetectPreemphasis => {
            let paths: Vec<std::path::PathBuf> = match app.current_screen {
                AppScreen::Browse => {
                    let sel = collect_selection_for_file_ops(app);
                    super::browse::expand_paths_to_audio(&sel)
                        .into_iter()
                        .filter(|p| {
                            matches!(
                                super::browse::classify_file(p),
                                super::browse::EntryKind::AudioFile(_)
                            )
                        })
                        .collect()
                }
                _ => Vec::new(),
            };
            if paths.is_empty() {
                app.set_status("No audio files for pre-emphasis detection");
            } else {
                app.preemph_results.clear();
                app.preemph_pending = paths.len();
                for path in paths {
                    let tx = tx.clone();
                    tokio::spawn(async move {
                        let result = super::preemphasis::detect_preemphasis(path).await;
                        let _ = tx.send(AppMessage::PreemphasisComplete { result }).await;
                    });
                }
                app.set_status(format!(
                    "Detecting pre-emphasis on {} file(s)...",
                    app.preemph_pending,
                ));
            }
        }
        Command::TrainPreemphCorpus { path } => {
            let scan_path = std::path::PathBuf::from(&path);
            if !scan_path.is_dir() {
                app.set_status(format!("Not a directory: {}", path));
            } else {
                let tx = tx.clone();
                app.set_status(format!("Training corpus from {}...", path));
                tokio::spawn(async move {
                    let result =
                        super::preemphasis::corpus::train_corpus_from_dir(&scan_path).await;
                    let _ = tx
                        .send(super::message::AppMessage::CorpusTrainComplete {
                            result: result.map(|m| (m.n_tracks, m.n_frames)),
                        })
                        .await;
                });
            }
        }
        Command::CalibratePreemphasis { pe_dir, non_pe_dir } => {
            let pe_path = std::path::PathBuf::from(&pe_dir);
            let non_pe_path = std::path::PathBuf::from(&non_pe_dir);
            if !pe_path.is_dir() {
                app.set_status(format!("Not a directory: {}", pe_dir));
            } else if !non_pe_path.is_dir() {
                app.set_status(format!("Not a directory: {}", non_pe_dir));
            } else {
                let tx = tx.clone();
                app.set_status(format!("Calibrating pre-emphasis detector..."));
                tokio::spawn(async move {
                    let result =
                        super::preemphasis::corpus::calibrate(&pe_path, &non_pe_path).await;
                    let _ = tx
                        .send(super::message::AppMessage::CalibrationComplete {
                            result: result.map(|r| {
                                (r.n_pe, r.n_non_pe, r.cv_accuracy, r.cv_fpr, r.threshold)
                            }),
                        })
                        .await;
                });
            }
        }
        Command::Search { recursive } => {
            if app.current_screen != AppScreen::Browse {
                app.set_status(":search only works on the browse screen");
            } else {
                app.browse.open_search();
                app.browse.search.recursive = recursive;
            }
        }
        Command::BulkRename => {
            if app.current_screen != AppScreen::Browse {
                app.set_status(":rename-all only works on the browse screen");
            } else {
                let paths = collect_selection_for_file_ops(app);
                let audio_paths: Vec<PathBuf> = paths
                    .into_iter()
                    .filter(|p| {
                        app.browse.entries.iter().any(|e| {
                            e.path == *p && matches!(e.kind, super::browse::EntryKind::AudioFile(_))
                        })
                    })
                    .collect();
                super::keybindings::open_bulk_rename(app, audio_paths);
            }
        }
        Command::WriteRgTrack | Command::WriteRgAlbum => {
            let album = matches!(cmd, Command::WriteRgAlbum);
            let paths: Vec<std::path::PathBuf> = app
                .analysis_results
                .iter()
                .map(|r| r.path.clone())
                .collect();
            if paths.is_empty() {
                app.set_status("No analysis results — run :analyze first");
            } else {
                let tx = tx.clone();
                let db_paths: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
                app.set_status(format!(
                    "Writing {} ReplayGain tags...",
                    if album { "album + track" } else { "track" },
                ));
                tokio::spawn(async move {
                    let mut args = vec!["-s".to_string(), "i".to_string(), "-k".to_string()];
                    if album {
                        args.push("-a".to_string());
                    } else {
                        args.push("-r".to_string());
                    }
                    for p in &paths {
                        args.push(p.to_string_lossy().to_string());
                    }
                    let output = tokio::process::Command::new("loudgain")
                        .args(&args)
                        .output()
                        .await;
                    let msg = match output {
                        Ok(o) if o.status.success() => {
                            format!(
                                "ReplayGain tags written ({} file{})",
                                db_paths.len(),
                                if db_paths.len() == 1 { "" } else { "s" }
                            )
                        }
                        Ok(o) => {
                            let stderr = String::from_utf8_lossy(&o.stderr);
                            format!(
                                "loudgain failed: {}",
                                stderr.lines().next().unwrap_or("unknown error")
                            )
                        }
                        Err(e) => format!("loudgain not found: {}", e),
                    };
                    let _ = tx
                        .send(super::message::AppMessage::StatusMessage(msg))
                        .await;
                });
                // Invalidate probe cache for the written files.
                for r in &app.analysis_results {
                    app.browse.probe_cache.remove(&r.path);
                    let _ = app.db.invalidate_probe(&r.path.display().to_string());
                }
            }
        }
        Command::WriteDr => {
            if app.analysis_results.is_empty() {
                app.set_status("No analysis results — run :analyze first");
            } else {
                let reports = super::dr_report::format_dr_reports(&app.analysis_results);
                let mut written = Vec::new();
                let mut errors = Vec::new();
                for (dir, text) in &reports {
                    let path = dir.join("dr_analysis.txt");
                    match std::fs::write(&path, text) {
                        Ok(()) => written.push(path),
                        Err(e) => errors.push(format!("{}: {}", dir.display(), e)),
                    }
                }
                if errors.is_empty() {
                    if written.len() == 1 {
                        app.set_status(format!("DR report written to {}", written[0].display(),));
                    } else {
                        app.set_status(format!(
                            "DR reports written to {} directories",
                            written.len(),
                        ));
                    }
                } else {
                    app.set_status(format!("DR report errors: {}", errors.join("; "),));
                }
            }
        }
        Command::ImportCue => {
            // Collect audio paths from current browse selection.
            let mut paths: Vec<std::path::PathBuf> = match app.current_screen {
                AppScreen::Browse => {
                    let sel = collect_selection_for_file_ops(app);
                    super::browse::expand_paths_to_audio(&sel)
                        .into_iter()
                        .filter(|p| {
                            matches!(
                                super::browse::classify_file(p),
                                super::browse::EntryKind::AudioFile(_)
                            )
                        })
                        .collect()
                }
                _ => Vec::new(),
            };
            super::probe::sort_paths_by_track(&mut paths);

            if paths.is_empty() {
                app.set_status("No audio files for CUE import");
                return;
            }

            let groups = super::gnudb::group_by_disc(&paths);

            if groups.len() <= 1 {
                // Single disc.
                let (_, group_paths) = groups.into_iter().next().unwrap();
                let dir = group_paths[0].parent().unwrap_or(std::path::Path::new("."));
                let cue_path = match super::gnudb::find_cue_in_dir(dir) {
                    Some(p) => p,
                    None => {
                        app.set_status("No CUE file found in directory");
                        return;
                    }
                };
                let sheet = match super::cue_parser::parse_cue_file(&cue_path) {
                    Ok(s) => s,
                    Err(e) => {
                        app.set_status(format!("CUE parse error: {}", e));
                        return;
                    }
                };
                let n_tracks = sheet.tracks.len();
                let review = super::gnudb::build_review_state_from_cue(&sheet, group_paths);
                app.set_status(format!(
                    "CUE import: {} tracks from {}",
                    n_tracks,
                    cue_path.file_name().unwrap_or_default().to_string_lossy(),
                ));
                app.active_overlay = ActiveOverlay::GnudbReview(Box::new(review));
            } else {
                // Multi-disc: find CUE in each subdirectory.
                let mut entries = Vec::new();
                for (label, group_paths) in groups {
                    let dir = group_paths[0].parent().unwrap_or(std::path::Path::new("."));
                    if let Some(cue_path) = super::gnudb::find_cue_in_dir(dir) {
                        if let Ok(sheet) = super::cue_parser::parse_cue_file(&cue_path) {
                            entries.push((label, sheet, group_paths));
                        }
                    }
                }
                if entries.is_empty() {
                    app.set_status("No CUE files found in any disc directory");
                    return;
                }
                let n_discs = entries.len();
                let n_tracks: usize = entries.iter().map(|(_, s, _)| s.tracks.len()).sum();
                let review = super::gnudb::build_multi_disc_review_state_from_cue(&entries);
                app.set_status(format!(
                    "CUE import: {} disc{}, {} tracks",
                    n_discs,
                    if n_discs == 1 { "" } else { "s" },
                    n_tracks,
                ));
                app.active_overlay = ActiveOverlay::GnudbReview(Box::new(review));
            }
        }
        Command::FixCaps => {
            // Restore parked metadata editor if coming from command mode.
            if let Some(parked) = app.pending_metadata_editor.take() {
                app.active_overlay = ActiveOverlay::MetadataEditor(parked);
            }
            if let super::app::ActiveOverlay::MetadataEditor(ref mut state) = app.active_overlay {
                use super::app::MetadataEditorPhase;
                // Phase-scoped: main editor skips mixed (user can't
                // see them and the placeholder shouldn't get rewritten);
                // detail overlay capitalizes only the focused entry's
                // per-track values; other phases (InlineEdit / AddingKey
                // / Saving) refuse cleanly.
                let focus = match state.phase {
                    MetadataEditorPhase::Editing => None,
                    MetadataEditorPhase::DetailEdit => Some(state.detail_field_idx),
                    _ => {
                        app.set_status(":fix-caps not available in this phase");
                        return;
                    }
                };
                let result = super::keybindings::fix_caps_for_state(state, focus);
                if result.changed_values > 0 {
                    state.dirty = true;
                }
                let mut msg = format!(
                    "Capitalization applied ({} values changed",
                    result.changed_values
                );
                if result.skipped_deleted > 0 {
                    msg.push_str(&format!(
                        "; {} deleted entries skipped",
                        result.skipped_deleted
                    ));
                }
                msg.push(')');
                app.set_status(msg);
            } else {
                app.set_status(":fix-caps only works in the metadata editor");
            }
        }
        Command::AccurateRip { force } => {
            // Collect audio file paths from the current context.
            let mut paths: Vec<std::path::PathBuf> = match app.current_screen {
                AppScreen::Browse => {
                    let sel = collect_selection_for_file_ops(app);
                    super::browse::expand_paths_to_audio(&sel)
                        .into_iter()
                        .filter(|p| {
                            matches!(
                                super::browse::classify_file(p),
                                super::browse::EntryKind::AudioFile(_)
                            )
                        })
                        .collect()
                }
                AppScreen::Convert => app.convert.source.mode.all_paths(),
                _ => Vec::new(),
            };
            super::probe::sort_paths_by_track(&mut paths);
            // Check for single-image CUE layout before the normal multi-file flow.
            if paths.len() <= 1 {
                let dir = if paths.is_empty() {
                    let sel = collect_selection_for_file_ops(app);
                    sel.first().and_then(|p| {
                        if p.is_dir() {
                            Some(p.clone())
                        } else {
                            p.parent().map(|d| d.to_path_buf())
                        }
                    })
                } else {
                    paths[0].parent().map(|d| d.to_path_buf())
                };
                if let Some(ref dir) = dir {
                    if let Some(info) = super::cue_parser::detect_single_image(dir) {
                        let n = info.track_boundaries.len();
                        let full_scan = force;
                        let tx = tx.clone();
                        app.set_status(format!(
                            "AccurateRip: verifying {} tracks (single image)...",
                            n,
                        ));
                        tokio::spawn(async move {
                            let result =
                                super::accuraterip::verify_single_image(&info, full_scan).await;
                            let _ = tx
                                .send(AppMessage::AccurateRipComplete {
                                    pages: vec![super::app::ArVerifyPage {
                                        label: String::new(),
                                        result,
                                    }],
                                })
                                .await;
                        });
                        return;
                    }
                }
            }
            if paths.is_empty() {
                app.set_status("No audio files for AccurateRip verification");
            } else {
                let groups = super::gnudb::group_by_disc(&paths);
                let n_groups = groups.len();
                let n_tracks: usize = groups.iter().map(|(_, p)| p.len()).sum();
                let full_scan = force;
                let tx = tx.clone();

                if n_groups <= 1 {
                    // Single disc — existing flow.
                    let group_paths = groups.into_iter().next().unwrap().1;
                    let sample_data = super::accuraterip::collect_sample_counts(&group_paths);
                    match sample_data {
                        Err(e) => {
                            app.set_status(format!("AccurateRip: {}", e));
                        }
                        Ok((sample_counts, sample_rate)) => {
                            app.set_status(format!(
                                "AccurateRip: verifying {} tracks...",
                                n_tracks,
                            ));
                            tokio::spawn(async move {
                                let result = super::accuraterip::verify_album(
                                    &group_paths,
                                    &sample_counts,
                                    sample_rate,
                                    full_scan,
                                )
                                .await;
                                let _ = tx
                                    .send(AppMessage::AccurateRipComplete {
                                        pages: vec![super::app::ArVerifyPage {
                                            label: String::new(),
                                            result,
                                        }],
                                    })
                                    .await;
                            });
                        }
                    }
                } else {
                    // Multi-disc — verify each disc sequentially in one task.
                    app.set_status(format!(
                        "AccurateRip: verifying {} discs, {} tracks...",
                        n_groups, n_tracks,
                    ));
                    tokio::spawn(async move {
                        let mut pages = Vec::with_capacity(n_groups);
                        for (label, group_paths) in groups {
                            let sample_data =
                                super::accuraterip::collect_sample_counts(&group_paths);
                            match sample_data {
                                Ok((sample_counts, sample_rate)) => {
                                    let result = super::accuraterip::verify_album(
                                        &group_paths,
                                        &sample_counts,
                                        sample_rate,
                                        full_scan,
                                    )
                                    .await;
                                    pages.push(super::app::ArVerifyPage { label, result });
                                }
                                Err(e) => {
                                    log::warn!("AccurateRip: skipping disc '{}': {}", label, e);
                                }
                            }
                        }
                        if !pages.is_empty() {
                            let _ = tx.send(AppMessage::AccurateRipComplete { pages }).await;
                        }
                    });
                }
            }
        }
        Command::ArFix => {
            // Check that the AR verification overlay is active.
            if let ActiveOverlay::AccurateRipVerify(ref state) = app.active_overlay {
                let page = &state.pages[state.active_page];
                if let Some(offset) = super::accuraterip::detect_uniform_offset(&page.result) {
                    let paths: Vec<std::path::PathBuf> =
                        page.result.tracks.iter().map(|t| t.path.clone()).collect();
                    let n = paths.len();
                    app.active_overlay = ActiveOverlay::Confirmation {
                        message: format!(
                            "Apply offset correction ({:+} samples) to {} tracks?\n\
                             Files will be re-encoded to FLAC and verified at offset +0\n\
                             before replacing originals.",
                            offset, n,
                        ),
                        action: super::app::ConfirmAction::OffsetCorrection { paths, offset },
                    };
                } else {
                    app.set_status(
                        "No uniform non-zero offset detected — correction not applicable",
                    );
                }
            } else {
                // No overlay open — run verification first, then auto-fix.
                app.auto_fix_on_complete = true;
                execute_command(app, Command::AccurateRip { force: false }, tx);
            }
        }
        Command::Ctdb => {
            // Same path collection as :ar.
            let mut paths: Vec<std::path::PathBuf> = match app.current_screen {
                AppScreen::Browse => {
                    let sel = collect_selection_for_file_ops(app);
                    super::browse::expand_paths_to_audio(&sel)
                        .into_iter()
                        .filter(|p| {
                            matches!(
                                super::browse::classify_file(p),
                                super::browse::EntryKind::AudioFile(_)
                            )
                        })
                        .collect()
                }
                _ => Vec::new(),
            };
            super::probe::sort_paths_by_track(&mut paths);
            // Check for single-image CUE layout.
            if paths.len() <= 1 {
                let dir = if paths.is_empty() {
                    let sel = collect_selection_for_file_ops(app);
                    sel.first().and_then(|p| {
                        if p.is_dir() {
                            Some(p.clone())
                        } else {
                            p.parent().map(|d| d.to_path_buf())
                        }
                    })
                } else {
                    paths[0].parent().map(|d| d.to_path_buf())
                };
                if let Some(ref dir) = dir {
                    if let Some(info) = super::cue_parser::detect_single_image(dir) {
                        let n = info.track_boundaries.len();
                        let tx = tx.clone();
                        app.set_status(format!(
                            "CUETools DB: verifying {} tracks (single image)...",
                            n,
                        ));
                        // Cache lookup before spawn (needs &app.db on main thread).
                        let cache_paths = vec![info.audio_path.clone()];
                        let cache_key = super::ctdb::compute_ctdb_parity_cache_key(&cache_paths);
                        let cached_parity = cache_key
                            .as_deref()
                            .and_then(|k| app.db.get_cached_ctdb_parity(k, 16));
                        tokio::spawn(async move {
                            let result = super::ctdb::verify_ctdb_single_image(
                                &info,
                                cache_key,
                                cached_parity,
                            )
                            .await;
                            let _ = tx
                                .send(AppMessage::CtdbComplete {
                                    pages: vec![super::app::CtdbVerifyPage {
                                        label: String::new(),
                                        result,
                                    }],
                                })
                                .await;
                        });
                        return;
                    }
                }
            }
            if paths.is_empty() {
                app.set_status("No audio files for CTDB verification");
            } else {
                let groups = super::gnudb::group_by_disc(&paths);
                let n_groups = groups.len();
                let n_tracks: usize = groups.iter().map(|(_, p)| p.len()).sum();
                let tx = tx.clone();

                if n_groups <= 1 {
                    // Single disc.
                    let group_paths = groups.into_iter().next().unwrap().1;
                    let sample_data = super::accuraterip::collect_sample_counts(&group_paths);
                    match sample_data {
                        Err(e) => {
                            app.set_status(format!("CTDB: {}", e));
                        }
                        Ok((sample_counts, sample_rate)) => {
                            app.set_status(format!(
                                "CUETools DB: verifying {} tracks...",
                                n_tracks,
                            ));
                            let cache_key =
                                super::ctdb::compute_ctdb_parity_cache_key(&group_paths);
                            let cached_parity = cache_key
                                .as_deref()
                                .and_then(|k| app.db.get_cached_ctdb_parity(k, 16));
                            tokio::spawn(async move {
                                let result = super::ctdb::verify_ctdb(
                                    &group_paths,
                                    &sample_counts,
                                    sample_rate,
                                    cache_key,
                                    cached_parity,
                                )
                                .await;
                                let _ = tx
                                    .send(AppMessage::CtdbComplete {
                                        pages: vec![super::app::CtdbVerifyPage {
                                            label: String::new(),
                                            result,
                                        }],
                                    })
                                    .await;
                            });
                        }
                    }
                } else {
                    // Multi-disc — verify each disc sequentially.
                    app.set_status(format!(
                        "CUETools DB: verifying {} discs, {} tracks...",
                        n_groups, n_tracks,
                    ));
                    tokio::spawn(async move {
                        let mut pages = Vec::with_capacity(n_groups);
                        for (idx, (label, mut group_paths)) in groups.into_iter().enumerate() {
                            let disc_name = if label.is_empty() {
                                format!("disc {}", idx + 1)
                            } else {
                                label.clone()
                            };
                            let _ = tx
                                .send(AppMessage::StatusMessage(format!(
                                    "CUETools DB: verifying {}/{}  — {}...",
                                    idx + 1,
                                    n_groups,
                                    disc_name
                                )))
                                .await;
                            super::probe::sort_paths_by_track(&mut group_paths);
                            let dir = group_paths[0]
                                .parent()
                                .unwrap_or(std::path::Path::new("."))
                                .to_path_buf();

                            // Per-disc single-image detection. Cache key is computed
                            // inside the spawn (file-metadata-only, no DB access).
                            // cached_parity is None here — multi-disc spawns populate
                            // the cache from each result's parity_cache_write after
                            // the spawn completes; the cache helps subsequent
                            // single-disc verifies.
                            let result = if let Some(info) =
                                super::cue_parser::detect_single_image(&dir)
                            {
                                let cache_paths = vec![info.audio_path.clone()];
                                let cache_key =
                                    super::ctdb::compute_ctdb_parity_cache_key(&cache_paths);
                                super::ctdb::verify_ctdb_single_image(&info, cache_key, None).await
                            } else {
                                match super::accuraterip::collect_sample_counts(&group_paths) {
                                    Ok((sample_counts, sample_rate)) => {
                                        let cache_key = super::ctdb::compute_ctdb_parity_cache_key(
                                            &group_paths,
                                        );
                                        super::ctdb::verify_ctdb(
                                            &group_paths,
                                            &sample_counts,
                                            sample_rate,
                                            cache_key,
                                            None,
                                        )
                                        .await
                                    }
                                    Err(e) => {
                                        log::warn!("CTDB: skipping disc '{}': {}", label, e);
                                        continue;
                                    }
                                }
                            };

                            pages.push(super::app::CtdbVerifyPage { label, result });
                        }
                        if !pages.is_empty() {
                            let _ = tx.send(AppMessage::CtdbComplete { pages }).await;
                        }
                    });
                }
            }
        }
        Command::CtdbRepair => {
            // If the CTDB overlay is open with parity available, extract
            // repair parameters from it. Otherwise, run CTDB verify first.
            if let ActiveOverlay::CtdbVerify(ref state) = app.active_overlay {
                let page = &state.pages[state.active_page];
                let result = &page.result;

                // Check that parity is available.
                let parity_url = match &result.parity_url {
                    Some(url) => url.clone(),
                    None => {
                        app.set_status("No parity data available for this disc");
                        return;
                    }
                };

                let npar = match result.npar {
                    Some(n) => n as usize,
                    None => {
                        app.set_status("CTDB entry missing npar value");
                        return;
                    }
                };

                // Check if any tracks have mismatches.
                let has_mismatch = result
                    .tracks
                    .iter()
                    .any(|t| t.status == super::ctdb::CtdbTrackStatus::Mismatch);
                if !has_mismatch {
                    app.set_status("No mismatches detected — repair not needed");
                    return;
                }

                let paths: Vec<std::path::PathBuf> =
                    result.tracks.iter().map(|t| t.path.clone()).collect();
                let n = paths.len();

                // Extract expected CRCs from the CTDB entry for post-repair
                // verification. These come from the database, not our computation.
                let expected_crcs: Vec<u32> = result
                    .tracks
                    .iter()
                    .filter_map(|t| t.expected_crc32)
                    .collect();
                if expected_crcs.len() != n {
                    app.set_status("Cannot repair: missing expected CRC for some tracks");
                    return;
                }

                // Detect single-image CUE layout: all tracks point at the
                // same file. Repair flow is different (decode once, repair
                // whole image, re-encode single file).
                let single_image: Option<Box<super::cue_parser::SingleImageInfo>> = if n > 1
                    && paths.iter().all(|p| p == &paths[0])
                {
                    let dir = paths[0].parent().unwrap_or(std::path::Path::new("."));
                    match super::cue_parser::detect_single_image(dir) {
                        Some(info) => Some(Box::new(info)),
                        None => {
                            app.set_status("Single-image CTDB repair: failed to detect CUE layout");
                            return;
                        }
                    }
                } else {
                    None
                };

                // For single-image, the path list seen by AR is the same
                // file repeated N times. We want to query AR cache by the
                // unique audio path, not N times.
                let cache_query_paths: Vec<std::path::PathBuf> = if single_image.is_some() {
                    vec![paths[0].clone()]
                } else {
                    paths.clone()
                };

                // Auto-detect drive read offset from AR cache. `None` means
                // we don't have enough cached data to be sure — run :ar
                // first and resume the confirmation when it completes.
                match detect_ar_offset_from_cache(&app.db, &cache_query_paths) {
                    Some(offset) => {
                        let offset_note = if offset != 0 {
                            format!("offset: {:+} samples (from AR cache)", offset)
                        } else {
                            "offset: +0 (verified by AR)".to_string()
                        };

                        let message = format!(
                            "Apply CTDB Reed-Solomon repair to {} tracks?\n\
                             Parity: {} symbols, {}\n\
                             Files will be re-encoded and verified before replacing originals.",
                            n, npar, offset_note,
                        );

                        let action = match single_image {
                            Some(info) => super::app::ConfirmAction::CtdbRepairSingleImage {
                                info,
                                parity_url,
                                npar,
                                offset,
                                expected_crcs,
                            },
                            None => super::app::ConfirmAction::CtdbRepair {
                                paths,
                                parity_url,
                                npar,
                                offset,
                                expected_crcs,
                            },
                        };

                        app.active_overlay = ActiveOverlay::Confirmation { message, action };
                    }
                    None => {
                        // No usable AR cache — defer the repair until AR
                        // verification completes and gives us an offset.
                        app.pending_ctdb_repair = Some(super::app::PendingCtdbRepair {
                            paths,
                            parity_url,
                            npar,
                            expected_crcs,
                            single_image,
                        });
                        app.set_status(
                            "No AR offset cached — running AccurateRip to detect drive offset...",
                        );
                        execute_command(app, Command::AccurateRip { force: false }, tx);
                    }
                }
            } else {
                // No CTDB overlay open (e.g. invoked from the "CUETools DB
                // repair" context menu, or directly via :ctdb-repair). Run
                // CTDB verify first; the auto-repair flag tells the
                // CtdbComplete handler to re-dispatch :ctdb-repair once
                // the verification overlay is installed.
                app.auto_repair_on_ctdb_complete = true;
                app.set_status("Running CUETools DB verification first to detect mismatches...");
                execute_command(app, Command::Ctdb, tx);
            }
        }
        Command::ArBatch => {
            if app.current_screen != AppScreen::Browse {
                app.set_status(":ar-batch only works on the browse screen");
                return;
            }
            // Use the selected entry if it's a directory, otherwise
            // use the current browse directory.
            let scan_dir = app
                .browse
                .selected_entry()
                .filter(|e| e.path.is_dir())
                .map(|e| e.path.clone())
                .unwrap_or_else(|| app.browse.current_dir.clone());
            app.set_status(format!(
                "AccurateRip batch: scanning {}...",
                scan_dir.display()
            ));

            let tx = tx.clone();
            tokio::spawn(async move {
                let result = super::accuraterip::batch_verify(&scan_dir, tx.clone()).await;
                let _ = tx.send(AppMessage::ArBatchComplete { result }).await;
            });
        }
        Command::ViewFile(path) => {
            // Resolve path: if empty, use current browse selection.
            let target = if path.as_os_str().is_empty() {
                if app.current_screen != AppScreen::Browse {
                    app.set_status(":view only works on the browse screen");
                    return;
                }
                let entry = app.browse.selected_entry();
                match entry {
                    Some(e) if super::browse::is_viewable_text_file(&e.path) => e.path.clone(),
                    Some(_) => {
                        app.set_status("Selected file is not a viewable text file");
                        return;
                    }
                    None => {
                        app.set_status("No file selected");
                        return;
                    }
                }
            } else {
                path
            };
            match super::external_editor::open_in_viewer(&target) {
                Ok(_) => {
                    app.force_redraw = true;
                }
                Err(e) => app.set_status(format!("View error: {}", e)),
            }
        }
        Command::EditFile(path) => {
            let target = if path.as_os_str().is_empty() {
                if app.current_screen != AppScreen::Browse {
                    app.set_status(":edit-file only works on the browse screen");
                    return;
                }
                let entry = app.browse.selected_entry();
                match entry {
                    Some(e) if super::browse::is_editable_text_file(&e.path) => e.path.clone(),
                    Some(e) if super::browse::is_viewable_text_file(&e.path) => {
                        app.set_status("Cannot edit log files — use :view instead");
                        return;
                    }
                    Some(_) => {
                        app.set_status("Selected file is not an editable text file");
                        return;
                    }
                    None => {
                        app.set_status("No file selected");
                        return;
                    }
                }
            } else {
                if !super::browse::is_editable_text_file(&path)
                    && super::browse::is_viewable_text_file(&path)
                {
                    app.set_status("Cannot edit log files — use :view instead");
                    return;
                }
                path
            };
            match super::external_editor::open_in_editor(&target) {
                Ok(_) => {
                    app.force_redraw = true;
                }
                Err(e) => app.set_status(format!("Edit error: {}", e)),
            }
        }
        Command::ContextMenu => {
            let origin = match app.current_screen {
                AppScreen::Browse => app
                    .button_map
                    .find_button_rect(&super::button_map::TuiButton::BrowseEntry(
                        app.browse.selected_index,
                    ))
                    .map(|r| (r.x + 2, r.y)),
                AppScreen::Queue => app
                    .button_map
                    .find_button_rect(&super::button_map::TuiButton::QueueItem(app.selected_index))
                    .map(|r| (r.x + 2, r.y)),
                _ => None,
            }
            .unwrap_or_else(|| {
                crossterm::terminal::size()
                    .map(|(w, h)| (w / 3, h / 3))
                    .unwrap_or((20, 10))
            });
            super::keybindings::open_context_menu(app, origin.0, origin.1);
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
            app.bookmarks.add_with_db(name.clone(), path, &app.db);
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
fn execute_queue(app: &mut AppState, _tx: &mpsc::Sender<AppMessage>, preset: Option<String>) {
    // Helper: load a named preset into the format/output-options pills.
    // Returns Ok(()) on success, Err(status_message) on failure.
    let load_preset_into_pills = |app: &mut AppState, name: &str| -> Result<(), String> {
        match super::presets::load_preset(name) {
            Ok(p) => {
                p.apply_to_pills(&mut app.convert.format, &mut app.convert.output_options);
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
                SourceMode::Single {
                    info: slot,
                    metadata: meta_slot,
                    ..
                } => {
                    *slot = Some(info);
                    *meta_slot = metadata;
                }
                SourceMode::Batch {
                    cursor_info,
                    cursor_metadata,
                    ..
                } => {
                    *cursor_info = Some(info);
                    *cursor_metadata = metadata;
                }
                SourceMode::MultiTrack {
                    info: slot,
                    metadata: meta_slot,
                    ..
                } => {
                    *slot = Some(info);
                    *meta_slot = metadata;
                }
                SourceMode::Empty => {
                    // Unreachable given paths.is_empty() check above.
                }
            }
            app.convert.source.mode = mode;
            app.recent.record_use_with_db(&first, &app.db);

            // Persist batch state for crash recovery.
            let batch_paths = app.convert.source.mode.all_paths();
            let _ = app
                .db
                .save_batch_state(&batch_paths, None, None, None, None, None);

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
fn execute_commit(app: &mut AppState, tx: &mpsc::Sender<AppMessage>, start: bool) {
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

    // Block commit when no destination path is set.
    if app.convert.output_options.dest_path.is_none() {
        app.set_status("no destination path set — enter a path in the output options");
        return;
    }

    // Block commit when all tracks are deselected.
    if let SourceMode::MultiTrack { selected, .. } = &app.convert.source.mode {
        if selected.iter().all(|s| !s) {
            app.set_status("no tracks selected");
            return;
        }
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

    // For MultiTrack sources with deselected tracks, attach a PipelineRequest
    // with TrackSelection::Set to the just-enqueued item.
    if let SourceMode::MultiTrack {
        tracks,
        selected,
        path,
        ..
    } = &app.convert.source.mode
    {
        let has_deselected = selected.iter().any(|s| !s);
        if has_deselected {
            use std::collections::BTreeSet;
            let selected_numbers: BTreeSet<u32> = tracks
                .iter()
                .zip(selected.iter())
                .filter(|(_, &sel)| sel)
                .map(|(t, _)| t.number)
                .collect();

            if !selected_numbers.is_empty() {
                // Build a minimal PipelineRequest with the track selection.
                use crate::convert::pipeline::*;
                let output_root = options.output_dir.clone().unwrap_or_else(|| {
                    path.parent()
                        .unwrap_or(std::path::Path::new("."))
                        .to_path_buf()
                });
                let rg_enabled = options.calculate_replaygain;

                let pipeline_settings = options
                    .pipeline_settings
                    .clone()
                    .unwrap_or_else(|| {
                        let mut s = tonepoet_pipeline::PipelineSettings::default();
                        s.target_format = tonepoet_pipeline::AudioFormat::Flac;
                        s
                    });
                let req = PipelineRequest {
                    worker_count: None,
                    job_id: String::new(),
                    item_id: String::new(),
                    container: path.clone(),
                    source: SourceOptions {
                        archive_password: None,
                        sacd_area: None,
                        cue_sidecar: CueSidecarPolicy::PreferSidecar,
                        track_selection: TrackSelection::Set(selected_numbers),
                    },
                    settings: pipeline_settings,
                    merge: options.merge_to_single,
                    output_root: output_root.clone(),
                    naming: NamingPolicy {
                        template: options
                            .naming_template
                            .clone()
                            .unwrap_or_else(|| "%NN% - %TITLE%".to_string()),
                        folder_template: options.folder_template.clone(),
                        per_album_subdir: true,
                        collision_policy: NamingCollisionPolicy::Fail,
                    },
                    publish: PublishPolicy {
                        overwrite: OverwritePolicy::FailIfExists,
                        same_filesystem_required: false,
                    },
                    log: LogPolicy {
                        root: output_root.join(".tonepoet-logs"),
                        write_for_blocked: true,
                        write_json_log: false,
                    },
                    stages: StagePolicy {
                        metadata: StageRequirement::Enabled,
                        replaygain: if rg_enabled {
                            StageRequirement::Enabled
                        } else {
                            StageRequirement::Disabled
                        },
                        features: StageRequirement::Enabled,
                        generate_cue: false,
                    },
                    failure_policy: FailurePolicy::FailAlbumOnAnyTrackFailure,
                };

                if let Ok(mut q) = app.manager.queue.try_write() {
                    for item in q.all_items_mut() {
                        if item.input_path == *path && item.pipeline_request.is_none() {
                            let mut item_req = req.clone();
                            item_req.item_id = item.id.clone();
                            item_req.job_id = format!("job-{}", item.id);
                            if let Some(ref pw) = item.archive_password {
                                item_req.source.archive_password =
                                    Some(SecretString::new(pw.clone()));
                            }
                            item.pipeline_request = Some(item_req);
                        }
                    }
                }
            }
        }
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
        if outcome.previously_converted > 0 {
            parts.push(format!("{} re-converting", outcome.previously_converted));
        }
        parts.join(", ")
    };

    // Clear source pane so a subsequent `:queue` arrives fresh.
    app.convert.source.mode = SourceMode::Empty;
    app.convert.metadata = MetadataState::default();
    let _ = app.db.clear_batch_state();

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
            app.browse.probe_current_with_db(tx, Some(&app.db));
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
            input: super::text_input::TextInputState::new(if initial.ends_with('/') {
                initial
            } else {
                format!("{}/", initial)
            }),
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
pub(super) fn collect_selection_for_file_ops(app: &AppState) -> Vec<PathBuf> {
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

/// Phase C-2 seed values for `search_releases_by_query` extracted
/// from a SACD metadata editor's current state. Per-track rows
/// with mixed values are skipped — MB's Lucene query has no notion
/// of "or these track-level values", so a divergent ARTIST column
/// can't seed a useful album-level search. `ALBUMARTIST` is the
/// canonical album-level row and is preferred over `ARTIST` when
/// both are present.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SacdMbSeed {
    pub artist: String,
    pub album: String,
    pub catalog: Option<String>,
    pub year: Option<String>,
}

/// Extract a search-query seed from the editor's current entries.
/// Returns `None` when none of ARTIST/ALBUMARTIST/ALBUM/CATALOGNUMBER/
/// DATE yields a usable value (all empty or all mixed). MB's text
/// search can be partial — having only an album title still produces
/// results — so any single non-empty term is enough.
pub(super) fn seed_sacd_mb_query(state: &super::app::MetadataEditorState) -> Option<SacdMbSeed> {
    let entry_value = |k: &str| -> Option<String> {
        state
            .entries
            .iter()
            .find(|e| e.display_key == k)
            .map(|e| e.value.trim().to_string())
            .filter(|s| !s.is_empty() && s != "<multiple values>")
    };
    let artist = entry_value("ALBUMARTIST")
        .or_else(|| entry_value("ARTIST"))
        .unwrap_or_default();
    let album = entry_value("ALBUM").unwrap_or_default();
    let catalog = entry_value("CATALOGNUMBER");
    // DATE on SACDs is a year string like "1959"; MB's `date:`
    // Lucene term accepts that form directly so no further parsing.
    let year = entry_value("DATE");

    if artist.is_empty() && album.is_empty() && catalog.is_none() && year.is_none() {
        return None;
    }
    Some(SacdMbSeed {
        artist,
        album,
        catalog,
        year,
    })
}

/// Convert per-track SACD durations (seconds) to a sector vector
/// in the shape `lookup_release_by_toc` and `build_mb_toc` expect:
/// `[off1, off2, …, offN, leadout]`, where `off1 = 150` (the standard
/// 2-second CD pre-gap) and each subsequent offset is the prior plus
/// the prior track's length in CD frames (75 fps).
///
/// SACD `PlayTime` uses CD-style 75 fps natively, so the conversion
/// `(seconds * 75.0).round() as u32` matches MB's expected geometry
/// directly — no DSD-frame complication. `saturating_add` guards
/// against arithmetic overflow on pathological inputs (a 24-hour
/// compilation is ~6.5M frames, well within u32, so this is purely
/// defensive).
pub fn sacd_durations_to_sectors(durations: &[f64]) -> Vec<u32> {
    let mut sectors = Vec::with_capacity(durations.len() + 1);
    let mut cur: u32 = 150;
    sectors.push(cur);
    for &d in durations {
        let frames = (d * 75.0).round().max(0.0) as u32;
        cur = cur.saturating_add(frames);
        sectors.push(cur);
    }
    sectors
}

/// Common spawn point for the unified `:tags-mb` TOC flow. All three
/// entry points (Browse audio-file selection, SACD editor in-place,
/// regular file editor in-place) compute their own sectors and
/// `toc_string`, then call this to do the cache check, status, and
/// async fire. The result re-enters via `MbOutcome::Toc` and routes
/// through the shared handler.
///
/// `paths` is the audio paths (or ISO replicated per-track for SACD).
/// `editor_park` is true when an editor is sitting in
/// `active_overlay` and should be populated in place. `fallback_seed`
/// is `Some(...)` only for SACD editors where TOC misses are common
/// enough to justify the text-search fallback hop.
pub(super) fn spawn_tags_mb_toc_lookup(
    app: &mut AppState,
    tx: &mpsc::Sender<AppMessage>,
    sectors: Vec<u32>,
    toc_string: String,
    paths: Vec<std::path::PathBuf>,
    editor_park: bool,
    fallback_seed: Option<SacdMbSeed>,
) {
    let cached = app.db.get_cached_mb_response(&toc_string);
    let n_cached = if cached.is_some() {
        "cached"
    } else {
        "fetching"
    };
    let n_tracks = sectors.len().saturating_sub(1);
    app.set_status(format!(
        ":tags-mb: {} disc TOC ({} tracks)…",
        n_cached, n_tracks,
    ));

    let tx = tx.clone();
    let toc_for_msg = toc_string;
    let ctx = super::message::TagsMbContext {
        paths,
        editor_park,
        fallback_seed,
    };

    tokio::spawn(async move {
        let outcome = super::musicbrainz::lookup_release_by_toc(&sectors, cached).await;
        let _ = tx
            .send(AppMessage::TagsFromMbComplete {
                outcome: super::message::MbOutcome::Toc {
                    outcome,
                    toc_string: toc_for_msg,
                },
                ctx,
            })
            .await;
    });
}

/// Dispatch `:tags-mb` when a metadata editor is open. Handles both
/// SACD ISOs (TOC from per-area durations) and regular file editors
/// (TOC from accuraterip helpers on `state.paths`). Returns
/// `Some(true)` when the dispatch fired (caller short-circuits) or
/// `None` when no editor is in scope (caller falls through to the
/// Browse path).
///
/// `direct_seed = Some(...)` means the user supplied explicit args
/// (`:tags-mb --catno X miles davis`); the dispatch then skips TOC
/// entirely and spawns a text search using the supplied seed. The
/// editor still gets parked and populated in place as for any
/// in-editor dispatch.
///
/// **Parking-slot invariant:** the editor must be left in
/// `active_overlay`, never in `pending_metadata_editor`, before this
/// returns. The CommandInput Enter and ContextMenu wrappers
/// auto-restore `pending → active` only when `active == None`, so
/// leaving the editor parked would cause it to be drained before
/// our async result arrives.
fn try_dispatch_in_editor_tags_mb(
    app: &mut AppState,
    tx: &mpsc::Sender<AppMessage>,
    direct_seed: Option<SacdMbSeed>,
) -> Option<bool> {
    use super::app::ActiveOverlay;

    let state_owned: Box<super::app::MetadataEditorState> =
        if let Some(parked) = app.pending_metadata_editor.take() {
            parked
        } else if matches!(app.active_overlay, ActiveOverlay::MetadataEditor(_)) {
            let prev = std::mem::replace(&mut app.active_overlay, ActiveOverlay::None);
            if let ActiveOverlay::MetadataEditor(s) = prev {
                s
            } else {
                unreachable!()
            }
        } else {
            return None;
        };

    // Direct-args path: skip TOC, fire a text search using the
    // user-supplied seed. Editor goes into `active_overlay` so the
    // result handler can populate it in place (same parking-slot
    // invariant as the TOC path).
    if let Some(seed) = direct_seed {
        let paths = state_owned.paths.clone();
        app.active_overlay = ActiveOverlay::MetadataEditor(state_owned);
        let ctx = super::message::TagsMbContext {
            paths,
            editor_park: true,
            fallback_seed: None,
        };
        super::event_loop::spawn_tags_mb_text_search(
            app,
            tx,
            seed,
            ctx,
            super::event_loop::TextSearchMode::DirectRequest,
        );
        return Some(true);
    }

    // Compute sectors + fallback eligibility per editor kind. SACD
    // editors derive geometry from their stashed per-area durations
    // and enable the text-search fallback (TOC misses are common for
    // SACD-only releases). Regular file editors derive geometry from
    // the audio files via the same accuraterip helpers the Browse
    // path uses, and disable the fallback (file-level TOCs are
    // sample-exact; fallback rarely helps).
    let (sectors, fallback_seed) = if let Some(area_kind) = state_owned.sacd_area_kind {
        let durations = match area_kind {
            super::sacd::AreaKind::Stereo => state_owned.sacd_stereo_durations.as_deref(),
            super::sacd::AreaKind::MultiChannel => {
                state_owned.sacd_multi_channel_durations.as_deref()
            }
        };
        let Some(durations) = durations.filter(|d| !d.is_empty()) else {
            app.active_overlay = ActiveOverlay::MetadataEditor(state_owned);
            app.set_status(
                ":tags-mb: no per-track durations for this SACD area — \
                 ISO TRL sectors may be malformed"
                    .to_string(),
            );
            return Some(true);
        };
        let sectors = sacd_durations_to_sectors(durations);
        let seed = seed_sacd_mb_query(&state_owned);
        (sectors, seed)
    } else {
        // File editor: state.paths is the audio file set. Use the
        // same TOC derivation the Browse path uses — first try
        // AccurateRip-style offsets in the parent dir, fall back to
        // per-file sample counts.
        if state_owned.paths.is_empty() {
            app.active_overlay = ActiveOverlay::MetadataEditor(state_owned);
            app.set_status(":tags-mb: editor has no paths".to_string());
            return Some(true);
        }
        let dir = state_owned.paths[0]
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .to_path_buf();
        let sectors = match super::accuraterip::find_toc_offsets(&dir) {
            Some(s) => s,
            None => match super::accuraterip::collect_sample_counts(&state_owned.paths) {
                Ok((sample_counts, sample_rate)) => {
                    let samples_per_frame = (sample_rate / 75) as u64;
                    let mut sectors = Vec::with_capacity(sample_counts.len() + 1);
                    let mut frame: u64 = 150;
                    for &count in &sample_counts {
                        sectors.push(frame as u32);
                        frame += count / samples_per_frame;
                    }
                    sectors.push(frame as u32);
                    sectors
                }
                Err(e) => {
                    app.active_overlay = ActiveOverlay::MetadataEditor(state_owned);
                    app.set_status(format!(":tags-mb: {}", e));
                    return Some(true);
                }
            },
        };
        (sectors, None)
    };

    let Some(toc_string) = super::musicbrainz::build_mb_toc(&sectors) else {
        app.active_overlay = ActiveOverlay::MetadataEditor(state_owned);
        app.set_status(":tags-mb: TOC too short".to_string());
        return Some(true);
    };
    let paths = state_owned.paths.clone();

    // Editor MUST go back into active_overlay (not pending) before
    // the spawn — see the parking-slot invariant in this function's
    // docstring.
    app.active_overlay = ActiveOverlay::MetadataEditor(state_owned);

    spawn_tags_mb_toc_lookup(
        app,
        tx,
        sectors,
        toc_string,
        paths,
        /* editor_park */ true,
        fallback_seed,
    );

    Some(true)
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
                app.set_status(format!(
                    "format = {}",
                    app.convert.format.format.selected_label()
                ));
            }
            "r" | "rate" => {
                app.set_status(format!(
                    "rate = {}",
                    app.convert.format.sample_rate.selected_label()
                ));
            }
            "d" | "depth" => {
                app.set_status(format!(
                    "depth = {}",
                    app.convert.format.bit_depth.selected_label()
                ));
            }
            "dither" => {
                app.set_status(format!(
                    "dither = {}",
                    app.convert.format.dither.selected_label()
                ));
            }
            "rg" | "replaygain" => {
                app.set_status(format!(
                    "replaygain = {}",
                    app.convert.format.replaygain.selected_label()
                ));
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
                app.set_status(format!(
                    "Unknown format: {}. Try: flac, opus, aac, mp3, alac, wav, wavpack, aiff",
                    value
                ));
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
                app.set_status(format!(
                    "Unknown rate: {}. Try: 44.1, 48, 88.2, 96, 176.4, 192, 352.8, 384, 705.6, 768",
                    value
                ));
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
                app.set_status(format!(
                    "Unknown depth: {}. Try: 16, 24, 32, 32f, 64f",
                    value
                ));
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
                app.set_status(format!(
                    "Unknown dither: {}. Try: tpdf, none, shaped",
                    value
                ));
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
                app.set_status(format!(
                    "Unknown rg mode: {}. Try: album, track, both, off",
                    value
                ));
            }
        }
        _ => {
            app.set_status(format!(
                "Unknown setting: {}. Try: format, rate, depth, dither, rg",
                key
            ));
        }
    }
}

/// Detect a uniform AR offset from the SQLite cache for a set of track files.
///
/// Returns:
/// - `Some(n)` if every track has a fresh cache entry, all are verified, and
///   they share a single offset value `n` (which may be 0 — meaning AR
///   confirmed offset 0). The caller can use `n` directly.
/// - `None` if at least one track has no cache entry, results are stale, any
///   track is not verified, or offsets disagree across tracks. The caller
///   should run `:ar` to resolve before proceeding.
fn detect_ar_offset_from_cache(db: &crate::db::Database, paths: &[PathBuf]) -> Option<i32> {
    let mut common_offset: Option<i32> = None;

    for path in paths {
        let meta = std::fs::metadata(path).ok()?;
        let mtime = meta
            .modified()
            .map(crate::db::systemtime_to_unix)
            .unwrap_or(0);
        let size = meta.len();
        let path_str = path.display().to_string();

        let cached = db.get_cached_ar(&path_str, mtime, size)?;

        for t in &cached {
            if t.status != super::accuraterip::ArTrackStatus::Verified {
                return None;
            }
            // `offset = None` means AR did not record an offset for this
            // track — treat as indeterminate and re-run.
            let off = t.offset?;
            match common_offset {
                Some(prev) if prev != off => return None, // mixed offsets
                None => common_offset = Some(off),
                _ => {} // same offset, continue
            }
        }
    }

    common_offset
}

/// Expand ~ to home directory.
fn expand_path(path: &str) -> String {
    if path.starts_with('~') {
        if let Ok(home) = std::env::var("HOME") {
            return path.replacen('~', &home, 1);
        }
    }
    path.to_string()
}

/// Build a list of proposed changes by comparing CUE sheet metadata
/// against the current tags in the metadata editor.
/// CUE fields that can be imported, mapped to their tag display keys.
const CUE_IMPORTABLE_FIELDS: &[&str] = &["TITLE", "ARTIST", "ALBUM", "TRACKNUMBER"];

/// Check if a tag field has corresponding CUE data.
pub fn is_cue_importable(field: &str) -> bool {
    let upper = field.to_ascii_uppercase();
    CUE_IMPORTABLE_FIELDS.iter().any(|&f| f == upper) || upper == "PERFORMER"
}

#[allow(dead_code)] // Legacy CUE diff flow; will be removed in cleanup.
fn build_cue_diff(
    state: &super::app::MetadataEditorState,
    sheet: &super::cue_parser::CueSheet,
    field_filter: Option<&str>,
) -> Vec<super::app::CueImportChange> {
    let mut changes = Vec::new();

    // Map the filter to the CUE field it corresponds to.
    // ARTIST and PERFORMER both map to CUE performer.
    let filter_upper = field_filter.map(|f| f.to_ascii_uppercase());

    for (i, path) in state.paths.iter().enumerate() {
        let stem = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let filename = path
            .file_stem()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| stem.clone());

        let track = sheet
            .tracks
            .iter()
            .find(|t| t.file.as_deref() == Some(stem.as_str()))
            .or_else(|| sheet.tracks.get(i));

        let mut proposed: Vec<(&str, String)> = Vec::new();

        if let Some(t) = track {
            if let Some(ref title) = t.title {
                proposed.push(("TITLE", title.clone()));
            }
            if let Some(ref performer) = t.performer {
                proposed.push(("ARTIST", performer.clone()));
            }
            if t.number > 0 {
                proposed.push(("TRACKNUMBER", t.number.to_string()));
            }
        }
        if let Some(ref album) = sheet.title {
            proposed.push(("ALBUM", album.clone()));
        }

        for (field, new_value) in proposed {
            // Apply field filter if set.
            if let Some(ref filter) = filter_upper {
                let field_upper = field.to_ascii_uppercase();
                // PERFORMER filter matches ARTIST CUE data.
                let matches =
                    field_upper == *filter || (filter == "PERFORMER" && field_upper == "ARTIST");
                if !matches {
                    continue;
                }
            }

            let old_value = state
                .entries
                .iter()
                .find(|e| e.display_key.eq_ignore_ascii_case(field))
                .map(|e| e.per_file_values.get(i).cloned().unwrap_or_default())
                .unwrap_or_default();

            if old_value != new_value {
                changes.push(super::app::CueImportChange {
                    file_index: i,
                    filename: filename.clone(),
                    field: field.to_string(),
                    old_value,
                    new_value,
                });
            }
        }
    }

    changes
}

/// Apply a set of CUE import changes to a metadata editor state.
pub fn apply_cue_changes(
    state: &mut super::app::MetadataEditorState,
    changes: &[super::app::CueImportChange],
) {
    let n = state.paths.len();

    for change in changes {
        // Find or create the entry for this field.
        let idx = match state
            .entries
            .iter()
            .position(|e| e.display_key.eq_ignore_ascii_case(&change.field))
        {
            Some(i) => i,
            None => {
                let item_key = match change.field.to_ascii_uppercase().as_str() {
                    "TITLE" => lofty::tag::ItemKey::TrackTitle,
                    "ARTIST" => lofty::tag::ItemKey::TrackArtist,
                    "ALBUM" => lofty::tag::ItemKey::AlbumTitle,
                    "TRACKNUMBER" => lofty::tag::ItemKey::TrackNumber,
                    _ => lofty::tag::ItemKey::Unknown(change.field.to_ascii_uppercase()),
                };
                state.entries.push(super::probe::TagEntry {
                    display_key: change.field.to_ascii_uppercase(),
                    item_key,
                    value: String::new(),
                    original: String::new(),
                    is_binary: false,
                    is_mixed: false,
                    per_file_values: vec![String::new(); n],
                    per_file_originals: vec![String::new(); n],
                    mb_proposed_value: None,
                    mb_proposed_per_file: None,
                });
                state.entries.len() - 1
            }
        };

        if change.file_index < n {
            state.entries[idx].per_file_values[change.file_index] = change.new_value.clone();
        }
    }

    // Update merged display values and mixed state.
    for entry in &mut state.entries {
        let all_same = entry.per_file_values.windows(2).all(|w| w[0] == w[1]);
        entry.is_mixed = !all_same && n > 1;
        entry.value = if entry.is_mixed {
            "<multiple values>".to_string()
        } else {
            entry.per_file_values.first().cloned().unwrap_or_default()
        };
    }

    state.dirty = true;
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
    fn command_completion_prefix_con_matches_convert_and_context() {
        let got = compute_completion("con", 3).expect("should have candidates");
        assert_eq!(
            got.candidates,
            vec!["convert".to_string(), "context".to_string()]
        );
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

#[cfg(test)]
mod sacd_seed_tests {
    use super::*;
    use crate::tui::app::{MetadataEditorPhase, MetadataEditorState};
    use crate::tui::probe::TagEntry;

    fn editor_with(entries: Vec<(&str, &str, bool)>) -> MetadataEditorState {
        // Each entry: (display_key, value, is_mixed). per_file_values
        // shape doesn't matter for seed extraction — it only reads
        // `value` and `is_mixed`/empty checks via `value`.
        use lofty::tag::ItemKey;
        let entries: Vec<TagEntry> = entries
            .into_iter()
            .map(|(k, v, mixed)| TagEntry {
                display_key: k.to_string(),
                item_key: ItemKey::Unknown(k.to_string()),
                value: v.to_string(),
                original: String::new(),
                is_binary: false,
                is_mixed: mixed,
                per_file_values: vec![v.to_string()],
                per_file_originals: vec![v.to_string()],
                mb_proposed_value: None,
                mb_proposed_per_file: None,
            })
            .collect();
        MetadataEditorState {
            paths: vec![std::path::PathBuf::from("/tmp/x.iso")],
            entries,
            cursor: 0,
            scroll: 0,
            last_click: None,
            edit_input: None,
            add_key_input: None,
            phase: MetadataEditorPhase::Editing,
            dirty: false,
            deleted: Vec::new(),
            file_labels: vec!["01".to_string()],
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
        }
    }

    #[test]
    fn seed_returns_none_when_all_fields_empty() {
        let state = editor_with(vec![]);
        assert!(seed_sacd_mb_query(&state).is_none());
    }

    #[test]
    fn seed_returns_none_when_only_other_keys_present() {
        // TITLE / TRACKNUMBER aren't seed candidates.
        let state = editor_with(vec![
            ("TITLE", "Some Title", false),
            ("TRACKNUMBER", "01", false),
        ]);
        assert!(seed_sacd_mb_query(&state).is_none());
    }

    #[test]
    fn seed_prefers_albumartist_over_artist() {
        let state = editor_with(vec![
            ("ALBUMARTIST", "Miles Davis", false),
            ("ARTIST", "Miles Davis Sextet", false),
            ("ALBUM", "Kind of Blue", false),
        ]);
        let seed = seed_sacd_mb_query(&state).expect("seed");
        assert_eq!(seed.artist, "Miles Davis");
        assert_eq!(seed.album, "Kind of Blue");
        assert_eq!(seed.catalog, None);
        assert_eq!(seed.year, None);
    }

    #[test]
    fn seed_falls_back_to_artist_when_albumartist_missing() {
        let state = editor_with(vec![
            ("ARTIST", "Thelonious Monk", false),
            ("ALBUM", "Solo Monk", false),
        ]);
        let seed = seed_sacd_mb_query(&state).expect("seed");
        assert_eq!(seed.artist, "Thelonious Monk");
    }

    #[test]
    fn seed_skips_mixed_per_track_artist() {
        // ARTIST is per-track and divergent → can't seed an
        // album-level query; ALBUMARTIST missing → artist empty.
        let state = editor_with(vec![
            ("ARTIST", "<multiple values>", true),
            ("ALBUM", "Various Compilation", false),
        ]);
        let seed = seed_sacd_mb_query(&state).expect("seed");
        assert_eq!(seed.artist, "");
        assert_eq!(seed.album, "Various Compilation");
    }

    #[test]
    fn seed_passes_catalog_and_year_when_present() {
        let state = editor_with(vec![
            ("ALBUMARTIST", "Thelonious Monk", false),
            ("ALBUM", "Solo Monk", false),
            ("CATALOGNUMBER", "SRGS 4520", false),
            ("DATE", "1965", false),
        ]);
        let seed = seed_sacd_mb_query(&state).expect("seed");
        assert_eq!(seed.catalog.as_deref(), Some("SRGS 4520"));
        assert_eq!(seed.year.as_deref(), Some("1965"));
    }

    #[test]
    fn seed_trims_whitespace_in_field_values() {
        // ScarletBook strings sometimes carry trailing whitespace
        // from padding in the binary header; the seed should strip
        // those before they reach Lucene escape.
        let state = editor_with(vec![("ALBUM", "  Padded Album  ", false)]);
        let seed = seed_sacd_mb_query(&state).expect("seed");
        assert_eq!(seed.album, "Padded Album");
    }

    #[test]
    fn seed_treats_only_catalog_as_sufficient() {
        // MB search with only `catno:` is valid and useful (some
        // pressings have nothing else reliable).
        let state = editor_with(vec![("CATALOGNUMBER", "SRGS 4520", false)]);
        let seed = seed_sacd_mb_query(&state).expect("seed");
        assert_eq!(seed.artist, "");
        assert_eq!(seed.album, "");
        assert_eq!(seed.catalog.as_deref(), Some("SRGS 4520"));
    }

    #[test]
    fn seed_empty_value_is_treated_as_absent() {
        // An entry that exists but has empty value (e.g., user
        // cleared the row) shouldn't make catalog Some("").
        let state = editor_with(vec![("ALBUM", "X", false), ("CATALOGNUMBER", "", false)]);
        let seed = seed_sacd_mb_query(&state).expect("seed");
        assert_eq!(seed.catalog, None);
    }
}

#[cfg(test)]
mod sacd_toc_tests {
    use super::sacd_durations_to_sectors;

    #[test]
    fn three_track_disc_offsets_and_leadout() {
        // 4:00, 3:30, 5:00 = 240s / 210s / 300s at 75 fps.
        // 240*75 = 18000, 210*75 = 15750, 300*75 = 22500.
        // offsets: 150, 18150, 33900;  leadout: 56400.
        let sectors = sacd_durations_to_sectors(&[240.0, 210.0, 300.0]);
        assert_eq!(sectors, vec![150, 18150, 33900, 56400]);
    }

    #[test]
    fn single_track_disc() {
        let sectors = sacd_durations_to_sectors(&[60.0]);
        // offsets: [150], leadout: 150 + 60*75 = 4650.
        assert_eq!(sectors, vec![150, 4650]);
    }

    #[test]
    fn empty_input_yields_pre_gap_only() {
        // Defensive: zero tracks produces only the pre-gap entry.
        // The dispatch path refuses this case before calling, but
        // we don't want to panic if it slips through.
        let sectors = sacd_durations_to_sectors(&[]);
        assert_eq!(sectors, vec![150]);
    }

    #[test]
    fn fractional_seconds_round_to_nearest_frame() {
        // 4:00.74 frame = 240 + 74/75 sec → 18074 frames exact.
        // Verify rounding handles sub-frame fractions cleanly.
        let sectors = sacd_durations_to_sectors(&[240.0 + 74.0 / 75.0]);
        assert_eq!(sectors, vec![150, 150 + 18074]);
    }

    #[test]
    fn build_mb_toc_consumes_the_sector_form() {
        // Round-trip with the existing `build_mb_toc` to confirm the
        // output shape is the one MB expects: "1 N leadout off1 off2 …"
        // ("+"-joined, since the helper is also used to build URLs).
        let sectors = sacd_durations_to_sectors(&[240.0, 210.0, 300.0]);
        let toc = crate::tui::musicbrainz::build_mb_toc(&sectors).expect("toc");
        assert_eq!(toc, "1+3+56400+150+18150+33900");
    }

    #[test]
    fn long_compilation_does_not_overflow() {
        // 24 hours of audio = ~6.5M frames, well within u32.
        // 100 tracks of 14:24 each = 86400 seconds total.
        let dur = 864.0; // 14:24 each
        let durs = vec![dur; 100];
        let sectors = sacd_durations_to_sectors(&durs);
        assert_eq!(sectors.len(), 101);
        // Final leadout = 150 + 100 * 14:24 in frames.
        let expected_leadout = 150 + 100 * ((dur * 75.0).round() as u32);
        assert_eq!(*sectors.last().unwrap(), expected_leadout);
    }
}

#[cfg(test)]
mod tags_mb_args_tests {
    use super::*;

    fn parse(s: &str) -> Command {
        super::parse_tags_mb_args(s)
    }

    fn tokenize(s: &str) -> Result<Vec<String>, String> {
        super::tokenize_tags_mb_args(s)
    }

    // ── tokenizer ──────────────────────────────────────────────

    #[test]
    fn tokenize_plain_whitespace_split() {
        assert_eq!(tokenize("a b c").unwrap(), vec!["a", "b", "c"]);
    }

    #[test]
    fn tokenize_double_quoted_preserves_internal_spaces() {
        assert_eq!(
            tokenize(r#"--catno "SRGS 4520" miles davis"#).unwrap(),
            vec!["--catno", "SRGS 4520", "miles", "davis"],
        );
    }

    #[test]
    fn tokenize_unterminated_quote_errors() {
        assert!(tokenize(r#"--catno "SRGS 4520"#).is_err());
    }

    #[test]
    fn tokenize_empty_input_is_empty_vec() {
        assert!(tokenize("").unwrap().is_empty());
        assert!(tokenize("   ").unwrap().is_empty());
    }

    #[test]
    fn tokenize_multiple_consecutive_spaces_collapse() {
        assert_eq!(tokenize("a   b").unwrap(), vec!["a", "b"]);
    }

    // ── parser: bare form ─────────────────────────────────────

    #[test]
    fn empty_args_parses_to_all_none() {
        match parse("") {
            Command::TagsFromMb { query, catno, year } => {
                assert_eq!(query, None);
                assert_eq!(catno, None);
                assert_eq!(year, None);
            }
            c => panic!("expected TagsFromMb, got {:?}", c),
        }
    }

    #[test]
    fn whitespace_only_args_parses_to_all_none() {
        match parse("   ") {
            Command::TagsFromMb { query, catno, year } => {
                assert!(query.is_none() && catno.is_none() && year.is_none());
            }
            c => panic!("expected TagsFromMb, got {:?}", c),
        }
    }

    // ── parser: free-form text ─────────────────────────────────

    #[test]
    fn free_form_text_only() {
        match parse("miles davis kind of blue") {
            Command::TagsFromMb { query, catno, year } => {
                assert_eq!(query.as_deref(), Some("miles davis kind of blue"));
                assert_eq!(catno, None);
                assert_eq!(year, None);
            }
            c => panic!("expected TagsFromMb, got {:?}", c),
        }
    }

    // ── parser: --catno ────────────────────────────────────────

    #[test]
    fn catno_only_with_quoted_value() {
        match parse(r#"--catno "SRGS 4520""#) {
            Command::TagsFromMb { query, catno, year } => {
                assert_eq!(catno.as_deref(), Some("SRGS 4520"));
                assert_eq!(query, None);
                assert_eq!(year, None);
            }
            c => panic!("expected TagsFromMb, got {:?}", c),
        }
    }

    #[test]
    fn catno_with_bare_value_no_quotes() {
        match parse("--catno SRGS-4520") {
            Command::TagsFromMb { catno, .. } => {
                assert_eq!(catno.as_deref(), Some("SRGS-4520"));
            }
            c => panic!("expected TagsFromMb, got {:?}", c),
        }
    }

    #[test]
    fn catno_missing_value_errors() {
        assert!(matches!(parse("--catno"), Command::Unknown(_)));
    }

    #[test]
    fn catno_duplicate_errors() {
        assert!(matches!(parse("--catno X --catno Y"), Command::Unknown(_)));
    }

    // ── parser: --year ─────────────────────────────────────────

    #[test]
    fn year_only() {
        match parse("--year 1971") {
            Command::TagsFromMb { year, .. } => {
                assert_eq!(year.as_deref(), Some("1971"));
            }
            c => panic!("expected TagsFromMb, got {:?}", c),
        }
    }

    #[test]
    fn year_missing_value_errors() {
        assert!(matches!(parse("--year"), Command::Unknown(_)));
    }

    // ── parser: combined / interleaved ────────────────────────

    #[test]
    fn all_three_in_canonical_order() {
        match parse(r#"--catno "ESGA 509" --year 1971 carole king tapestry"#) {
            Command::TagsFromMb { query, catno, year } => {
                assert_eq!(catno.as_deref(), Some("ESGA 509"));
                assert_eq!(year.as_deref(), Some("1971"));
                assert_eq!(query.as_deref(), Some("carole king tapestry"));
            }
            c => panic!("expected TagsFromMb, got {:?}", c),
        }
    }

    #[test]
    fn flags_after_free_text() {
        match parse("miles davis --year 1959 --catno CL-1355") {
            Command::TagsFromMb { query, catno, year } => {
                assert_eq!(query.as_deref(), Some("miles davis"));
                assert_eq!(catno.as_deref(), Some("CL-1355"));
                assert_eq!(year.as_deref(), Some("1959"));
            }
            c => panic!("expected TagsFromMb, got {:?}", c),
        }
    }

    // ── parser: error cases ────────────────────────────────────

    #[test]
    fn unknown_flag_errors() {
        assert!(matches!(parse("--frobnicate X"), Command::Unknown(_)));
    }

    #[test]
    fn unterminated_quote_errors() {
        assert!(matches!(
            parse(r#"--catno "SRGS 4520"#),
            Command::Unknown(_)
        ));
    }
}
