# Brief: Browse Selection Bindings & Bulk Operation Guards

## Overview

Two interconnected changes:

1. **Selection bindings redesign** — Implement keyboard and mouse+keyboard bindings for multi-select and range-select that survive terminal multiplexers, emulators, and legacy key encoding
2. **Bulk operation guards** — Add confirmation prompts before expensive operations on large selections or directories containing many audio files

## Part 1: Selection Bindings

### Design principles

Keyboard and mouse are coeval: every operation has a form on each device that means the same thing. Every binding has a baseline form that works in any terminal under any multiplexer. Enhanced bindings (Kitty keyboard protocol) degrade gracefully to baseline equivalents.

### Reliability tiers

- **Baseline**: Works in any terminal, under any multiplexer, with no protocol negotiation.
- **Enhanced keys only**: Works only when the app has negotiated the Kitty keyboard protocol at startup. Degrades to a baseline key when the protocol is absent.
- **Emulator dependent**: May be intercepted by the terminal emulator before the app sees it. Always paired with a baseline twin, never the only path to its operation.

### Keyboard bindings

| Key | Action | Reliability |
|---|---|---|
| `↑` `↓` `PgUp` `PgDn` | move cursor | Baseline |
| `Home` | move to beginning, top of list | Baseline |
| `End` | move to end, bottom of list | Baseline |
| `Space` | toggle mark on cursor, advance one, stop at end; set anchor to the toggled row | Baseline |
| `v` | enter modal range; movement previews; `Enter` or `Space` commits; `Esc` cancels | Baseline |
| `Shift`+`↑` `↓` `PgUp` `PgDn` `Home` `End` | enter range and extend by that motion | Baseline (arrows carry shift in legacy encoding) |
| `Shift+Space` | extend: mark from anchor to cursor in one press, then move anchor to cursor | Enhanced keys only. Degrades to plain `Space` without them |
| `Alt+a` | select all: toggle-all across the visible set | Baseline (see macOS note) |
| `Alt+i` | invert marks across the visible set | Baseline (see macOS note) |
| `Esc` | cancel a live range, else clear marks | Baseline |

**macOS note.** `Alt` is the Option key. Terminal.app and iTerm2 default to composing special characters with Option rather than sending it as Meta, so `Alt`+letter reaches the app only when "Use Option as Meta key" is enabled. On Linux and Windows terminals `Alt` encodes cleanly as an escape prefix.

`*` remains available as an alternate invert binding for Midnight Commander muscle memory — it is unconditionally safe.

### Mouse + keyboard bindings

| Gesture | Action | Reliability |
|---|---|---|
| click on row body | move cursor, preserve marks | Baseline |
| click on selection gutter | toggle mark, cursor unchanged | Baseline (mouse twin of `Space`) |
| drag | range preview, commit on release, auto-scroll at edges | Baseline (mouse twin of `v`) |
| wheel | scroll viewport only | Baseline |
| double-click | activate: descend or open | Baseline |
| `Shift`+click | extend from anchor to clicked row | Emulator dependent (mouse twin of `Shift+Space`) |
| `Ctrl`+click | toggle mark on clicked row | Emulator dependent (mouse twin of gutter click) |

### Device pairing

| Operation | Keyboard | Mouse |
|---|---|---|
| toggle one item | `Space` | gutter click |
| modal range | `v` | drag |
| one-shot extend from anchor | `Shift+Space` | `Shift`+click |
| toggle one without moving cursor | `Space` on a re-visited row | `Ctrl`+click or gutter click |

### Forbidden chords

| Chord | Reason |
|---|---|
| `Ctrl-a` | screen and byobu prefix |
| `Ctrl-b` | tmux prefix |
| `F1`–`F12` | byobu window and pane control |
| `Ctrl`+arrows | byobu pane navigation |
| `Ctrl-Space` | encodes as NUL, often lost |
| `Ctrl+Shift`+letter | indistinguishable from `Ctrl`+letter in legacy encoding |
| `Ctrl-v` | paste in most terminals and editors |

### Current state to replace

**Current selection model** (`src/tui/browse.rs`):
- `multi_selected: Vec<PathBuf>` (line 1367) — vector of selected paths
- `multi_select_anchor: Option<PathBuf>` (line 1371) — anchor for range selection
- `visual_mode: bool` (line 1375) — cursor movement extends selection
- `toggle_selection()` (line 3243) — adds/removes current entry from multi_selected
- `update_visual_selection()` (line 3288) — range from anchor to cursor
- `clear_multi_selection()` (line 3263) — clears all

**Current keybindings** (`src/tui/keybindings.rs`):
- Space (line 2956) — toggles selection on current entry and moves down
- Ctrl+V was the visual mode toggle — REMOVED in prior commit
- No Shift+arrow range select
- No drag select
- No gutter click
- No Shift+click or Ctrl+click

**Current rendering** (`src/tui/draw_browse.rs`):
- `render_entry_line()` (line 1911) — renders each row
- Checkbox indicator: `●` (cyan) or space — 1 char gutter
- Cursor indicator: `▸ ` or `  ` — 2 chars
- Full row prefix: `│ ▸ ● ` (border + cursor + checkbox + space = 5 chars before columns)

**Kitty keyboard protocol**: NOT implemented. No protocol negotiation exists. Image protocol (Kitty graphics) is implemented but keyboard enhancement is not. The `Shift+Space` binding should be gated behind enhanced key detection if/when it's added; until then it degrades to plain Space.

### Implementation

#### Selection state changes (`src/tui/browse.rs`)

The `visual_mode` bool needs to become a richer state:

```rust
pub enum SelectionMode {
    /// Normal mode — cursor moves independently of selection.
    Normal,
    /// Modal range mode (v key or drag). Movement previews the range.
    /// Enter/Space commits, Esc cancels. Stores the anchor index and
    /// the selection snapshot from before range mode was entered.
    Range {
        anchor_index: usize,
        pre_range_selection: Vec<PathBuf>,
    },
}
```

The anchor should be index-based within the current view (not path-based) for range computation, but paths should be resolved at commit time so the selection survives resort/refilter.

#### Space key behavior

Current: toggle + advance. New behavior:
1. Toggle mark on the entry at cursor
2. Set `multi_select_anchor` to the toggled row
3. Advance cursor by one (stop at end, don't wrap)

This is the same as current behavior — preserve it.

#### `v` key — modal range

1. Enter `SelectionMode::Range` with anchor at current cursor
2. Snapshot current `multi_selected` as `pre_range_selection`
3. On cursor movement: compute range from anchor to cursor, show preview (highlighted but not yet committed). The preview REPLACES the pre_range snapshot visually — the user sees the range, not the old selection.
4. `Enter` or `Space`: commit the range (merge into multi_selected), return to Normal mode
5. `Esc`: restore `pre_range_selection`, return to Normal mode

#### Shift+arrow — one-shot range extend

1. If not already in Range mode, enter it with anchor at current cursor
2. Move cursor by the arrow direction
3. Extend selection from anchor to new cursor position
4. Stay in Range mode — subsequent Shift+arrows continue extending
5. Any non-Shift navigation key commits the range and returns to Normal

#### Shift+Space — extend from anchor

1. Mark all entries from `multi_select_anchor` to cursor (inclusive)
2. Move anchor to cursor
3. Stay in Normal mode (not modal)

#### Alt+a — select all

Toggle all visible entries. If all are selected, deselect all. If some or none are selected, select all. Exclude ParentDir.

#### Alt+i — invert

Toggle every visible entry's mark state. Exclude ParentDir.

#### `*` — invert (alternate)

Same as Alt+i. Unconditionally safe binding for Midnight Commander users.

#### Gutter click

The checkbox column (currently 1 char `●` or space) is the gutter. Clicking it should toggle the mark on that entry WITHOUT moving the cursor. This requires the gutter to have its own hit target separate from the row body.

Currently the gutter and row body share one `BrowseEntry(idx)` button target. Split this into:
- `BrowseEntryGutter(idx)` — gutter column click → toggle mark, don't move cursor
- `BrowseEntry(idx)` — row body click → move cursor, preserve marks

#### Drag

1. Mouse down on a row body → record drag anchor
2. Mouse move while held → extend selection preview from anchor to current row (auto-scroll if mouse is at top/bottom edge of viewport)
3. Mouse up → commit the range

This requires tracking drag state in the mouse handler. Currently mouse move events only update `hover_target`. Add drag tracking:

```rust
pub struct BrowseDragState {
    pub anchor_index: usize,
    pub active: bool,
}
```

#### Shift+click

Extend selection from `multi_select_anchor` to clicked row. Same as Shift+Space but mouse-driven.

#### Ctrl+click

Toggle mark on clicked row without moving cursor. Same as gutter click but available on the full row.

#### Rendering changes

The gutter needs to be a distinct clickable region. Currently the checkbox is 1 char wide inside the row prefix. Consider making it 2-3 chars wide for a better click target:

```
│ ▸ [●] filename.flac    1.2MB  2024-01-15  FLAC │
     ^^^
     gutter (3 chars: space + marker + space)
```

During Range mode, the previewed range should be visually distinct from committed selections — use a different highlight color (e.g., `theme.selection_bg` for preview vs `theme.cyan` for committed marks).

## Part 2: Bulk Operation Guards

### Problem

Several operations can be triggered on a single directory that contains hundreds or thousands of audio files. No confirmation prompt is shown — the operation immediately spawns work for every file. This can freeze the UI, saturate I/O, and take minutes.

### Operations that need guards

| Operation | Current guard | Risk |
|---|---|---|
| Edit metadata | None | Opens editor for 1000+ files |
| Analyze | None | Spawns ffmpeg for every file |
| Verify (integrity) | None | Full decode of every file |
| AccurateRip (verify) | None | Full decode + network lookup |
| AccurateRip (batch) | None | Same, recursive |
| AccurateRip (full scan) | None | All offsets × all files |
| AccurateRip (fix offset) | None | Rewrites files |
| CueToolsDB (verify) | None | Network + decode |
| CueToolsDB (repair) | ConfirmAction::CtdbRepair | Already guarded |
| MusicBrainz tagging | None | Network lookup, opens picker |
| GNUDB tagging | None | Network lookup |
| Pre-emphasis detection | None | Spectral analysis per file |

### Implementation

Add a file-count check before launching bulk operations. When the operation would affect more than a threshold number of audio files, show a confirmation dialog.

#### Threshold

Suggested threshold: **50 audio files** (total individual audio files that would be processed, NOT folders). The count includes audio files found recursively inside subdirectories when the operation would recurse. Examples:
- A single album directory with 12 FLAC tracks → 12 audio files → no prompt
- A box set directory with 5 discs × 15 tracks = 75 audio files → prompt
- A multi-selected set of 3 albums totalling 40 tracks → no prompt
- An artist folder containing 20 albums → hundreds of audio files → always prompts

Use the same expansion logic as `collect_selection_for_file_ops()` / `expand_paths_to_audio()` to count, but with a fast bail-out (stop counting at threshold + 1, don't enumerate all 10,000 files).

#### Confirmation dialog

```
This will analyze 247 audio files across 12 folders.
Continue? [Enter = yes, Esc = cancel]
```

Use the existing `ConfirmAction` enum in `src/tui/app.rs` (search for `ConfirmAction`). Add variants for each guarded operation. The confirmation overlay is already rendered by `draw_overlays.rs`.

#### Where to add guards

The guard should be inserted at the command dispatch level in `src/tui/command.rs` and `src/tui/context_menu.rs`, before the operation spawns async work. The flow:

1. Collect selection paths (existing `collect_selection_for_file_ops()`)
2. Count audio files (new: `count_audio_files_bounded(paths, threshold)`)
3. If count > threshold: set `app.active_overlay = ActiveOverlay::Confirm { action: ConfirmAction::Analyze { paths, count }, ... }`
4. If count <= threshold: proceed directly

The `count_audio_files_bounded()` function should:
- Walk directories recursively
- Count files with audio extensions (use `classify_file()`)
- Stop counting at `threshold + 1` (we only need to know "over threshold", not the exact count for 100K files)
- Return the count (capped at threshold + 1)

For the confirmation message, if count > threshold, show the capped count: "This will analyze at least 50 audio files. Continue?"
For exact counts up to a reasonable limit (e.g., 500), show the exact count.

### Current code locations for guards

- `collect_selection_for_file_ops()`: `src/tui/command.rs:4904`
- `ConfirmAction` enum: `src/tui/app.rs` (search `ConfirmAction`)
- Confirm overlay rendering: `src/tui/draw_overlays.rs` (search `ConfirmAction`)
- Confirm dispatch: `src/tui/keybindings.rs` (search `ConfirmAction`)
- Existing CtdbRepair guard: `src/tui/keybindings.rs` (search `CtdbRepair`)
- `expand_paths_to_audio()`: `src/tui/browse.rs` or `src/tui/command.rs` (search `expand_paths_to_audio`)
- `classify_file()`: `src/tui/browse.rs` (search `fn classify_file`)

## Code locations for selection

- Selection state: `src/tui/browse.rs:1364-1375` (`multi_selected`, `visual_mode`, `multi_select_anchor`)
- `toggle_selection()`: `src/tui/browse.rs:3243`
- `update_visual_selection()`: `src/tui/browse.rs:3288`
- Space key: `src/tui/keybindings.rs:2956`
- Row rendering: `src/tui/draw_browse.rs:1911` (`render_entry_line`)
- Gutter/checkbox rendering: `src/tui/draw_browse.rs:1931-1943`
- Button registration: `src/tui/draw_browse.rs` (search `BrowseEntry(i)`)
- Mouse click handler: `src/tui/keybindings.rs:24062`
- Esc handler: `src/tui/keybindings.rs` (search `clear_multi_selection` in Esc cascade)
- Context menu select/deselect: `src/tui/context_menu.rs` (search `ContextAction::Select`)

## Files to modify

1. **`src/tui/browse.rs`** — Selection state (replace visual_mode with SelectionMode enum), anchor tracking, range computation, select-all, invert, drag state, `count_audio_files_bounded()`
2. **`src/tui/keybindings.rs`** — All keyboard bindings (Space, v, Shift+arrows, Alt+a, Alt+i, *, Esc), mouse handlers (gutter click, drag, Shift+click, Ctrl+click), confirm dispatch for guarded operations
3. **`src/tui/draw_browse.rs`** — Gutter hit target split, range preview rendering, drag preview
4. **`src/tui/button_map.rs`** — Add `BrowseEntryGutter(usize)` variant
5. **`src/tui/command.rs`** — Insert guards before bulk operations
6. **`src/tui/context_menu.rs`** — Insert guards before bulk operations dispatched from context menu
7. **`src/tui/app.rs`** — Add `ConfirmAction` variants for guarded operations

## Exit criteria

### Selection
- Space toggles mark and advances cursor
- `v` enters modal range; movement previews; Enter/Space commits; Esc cancels
- Shift+arrows enter range and extend
- Alt+a selects/deselects all visible entries
- Alt+i / `*` inverts all visible marks
- Gutter click toggles mark without moving cursor
- Drag previews range and commits on release with auto-scroll at edges
- Shift+click extends from anchor to clicked row
- Ctrl+click toggles mark on clicked row
- Esc cancels live range, then clears marks (two-stage)
- Range preview visually distinct from committed selections
- All selection operations exclude ParentDir entries

### Guards
- Operations on >50 audio files show confirmation prompt
- Prompt shows file count (exact up to 500, "at least 50" above)
- Enter confirms, Esc cancels
- Guards on: edit metadata, analyze, verify, AccurateRip (all), CueToolsDB verify, MusicBrainz, GNUDB, pre-emphasis detection
- CueToolsDB repair retains its existing guard
- No guard on operations that already have their own confirmation flow

### General
- No forbidden chords used (no Ctrl+a/b/v, no F-keys, no Ctrl+arrows)
- `cargo check` — zero errors, zero warnings
- `cargo test --no-run` — zero errors, zero warnings
