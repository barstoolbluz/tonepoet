# Brief: Browse Screen Refactoring

## Overview

Major refactoring of the Browse screen into a three-pane layout with integrated tree navigation, toolbar with buttons, collapsible panes, persistent view settings, and inline editing improvements. The existing search infrastructure should be hardened and improved. All view preferences persist to config.

Visual reference: `docs/browse_refactoring_mockup.md` contains ASCII mockups of all layout states.

## Part 1: Three-Pane Layout

### Current layout

The browse screen is a two-pane horizontal split:
- File list (left, 66% width)
- Info pane (right, 34% width)

With a fixed header (7 lines), breadcrumb/path bar (3 lines), and footer (2 lines).

### New layout

Three horizontal panes, all collapsible:

```
┌▾ explore ───────┬─▾ browse ──────────────────────────────────┬─▾ info ──────────────┐
│ (tree nav)      │ (file list with sortable columns)          │ (metadata/analysis)   │
└─────────────────┴────────────────────────────────────────────┴───────────────────────┘
```

**Default split ratios:** Explore 20% · Browse 50% · Info 30%

### Explore pane (tree navigation)

A collapsible directory tree, similar to the file picker's tree navigation (`crates/tui-file-picker/src/tree.rs`). Shows the filesystem hierarchy with expandable/collapsible folder nodes (`▾`/`▸`). Clicking a folder in the tree navigates the browse pane to that directory.

- Root defaults to the user's home directory
- Expanded/collapsed node state persists during the session
- Syncs with the browse pane: navigating in browse highlights the corresponding tree node
- Title bar: `▾ explore` (click `▾` to collapse)

### Collapsible pane behavior

Each pane has a `▾`/`▸` toggle in its title bar.

**Collapsed state:** Pane shrinks to a 3-column-wide vertical title bar with the pane name rendered vertically:

```
┌─┐
│▸│
│ │
│e│
│x│
│p│
│l│
│o│
│r│
│e│
│ │
└─┘
```

Clicking the `▸` or anywhere on the vertical title bar expands the pane back to its default size.

**Layout constraints by collapse state:**

| State | Explore | Browse | Info |
|-------|---------|--------|------|
| All open | 20% | 50% (Min 40) | 30% |
| Explore collapsed | 3 cols | ~55% | ~45% |
| Info collapsed | 20% | ~77% | 3 cols |
| Both collapsed | 3 cols | ~94% | 3 cols |

Browse never fully collapses — it always occupies at least 50% of available width.

**Double-click** on the browse pane's title bar collapses both explore and info (maximize browse). Double-click again restores all panes to defaults.

### Reusing file picker tree code

The file picker crate's `TreeNode` struct and tree rendering logic (`crates/tui-file-picker/src/tree.rs`) should be reused or adapted for the explore pane. The browse screen maintains its own `Vec<TreeNode>` synced with the filesystem.

## Part 2: Toolbar

Replace the current breadcrumb-only header with a toolbar row + path bar:

```
 ‹ Back  › Fwd  ↑ Up  Refresh  Options ▾  Search              Show hidden: ○
 path: ~/kairos/pbthal/downloads                                        [Go]
```

### Toolbar buttons

All styled as real buttons with `theme.button` background, matching file picker toolbar styling.

| Button | Action |
|--------|--------|
| `‹ Back` | Navigate to previous directory in history |
| `› Fwd` | Navigate forward in history |
| `↑ Up` | Navigate to parent directory |
| `Refresh` | Reload current directory contents (also `:refresh` command) |
| `Options ▾` | Open options dropdown menu |
| `Search` | Open the existing search panel (`\` key behavior) |
| `Show hidden: ○/●` | Quick toggle for hidden files (most-used option, always visible) |

Disabled buttons (e.g., Back with no history) use `theme.button_disabled`.

### Options dropdown menu

All options are **persistent** — changing any option writes immediately to `config.toml`.

```
┌─ Options ─────────────────┐
│ ● Show hidden files       │
│ Columns               ▸   │
│ Default sort           ▸   │
│ Filter                 ▸   │
│ Archive listing mode   ▸   │
│───────────────────────────│
│ Save layout as default    │
│ Restore defaults          │
└───────────────────────────┘
```

**Show hidden files** — toggle, persists to `[browsing] show_hidden = true/false`

**Columns ▸** — submenu with checkboxes for each available column:
```
┌─ Columns ─────────┐
│ ☑ Name             │
│ ☑ Size             │
│ ☑ Date             │
│ ☑ Type             │
│ ☐ Format           │
│ ☐ Codec            │
│ ☐ Sample rate      │
│ ☐ Channels         │
│ ☐ Duration         │
│ ☐ Artist           │
│ ☐ Album            │
└────────────────────┘
```
Name is always shown (can't be unchecked). Audio-specific columns (Format, Codec, etc.) require probing — they show `—` for non-audio files. Persists to `[browsing] columns = ["name", "size", "date", "type"]`.

**Default sort ▸** — submenu to set the default sort column and direction. This is the sort applied when entering a new directory. The user can still click column headers to sort ad-hoc within a directory. Persists to `[browsing] default_sort = "name"` and `[browsing] default_sort_dir = "asc"`.

**Filter ▸** — submenu: All files, Audio only, or by specific format (FLAC, Opus, AAC, etc.). Persists to `[browsing] default_filter = "all"`.

**Archive listing mode ▸** — Auto (skip remote) / Always / Never. Same config field as `[performance.browsing] archive_listing`. This is a convenience shortcut — changing it here writes to the same config field.

**Save layout as default** — captures the current pane collapse/expand state and saves to config: `[browsing] layout_explore = "open"` / `"collapsed"`, `layout_info = "open"` / `"collapsed"`.

**Restore defaults** — resets ALL browse settings (layout, columns, sort, filter, hidden files) back to factory defaults and saves.

### Path bar

Below the toolbar. Shows current path with `~` expansion. Clickable — clicking opens inline path editing (same as current breadcrumb). `[Go]` button confirms manual path entry.

### Refresh

`:refresh` command and toolbar Refresh button both reload the current directory listing. This re-reads the filesystem, updates file sizes/dates, discovers new/deleted files, and re-applies the current sort and filter.

## Part 3: Inline Editing Improvements

### Selection and clipboard

Add selection support and clipboard to `TextInputState`:

```rust
pub struct TextInputState {
    pub text: String,
    pub cursor: usize,
    pub select_all: bool,
    // NEW:
    pub selection_anchor: Option<usize>,  // byte offset where selection started
    pub clipboard: String,                // internal clipboard
}
```

Selection is a range from `selection_anchor` to `cursor`. When selection is active, typed characters replace the selection.

| Key | Action |
|-----|--------|
| Double-click | Select entire field text |
| Ctrl+A | Select all text in field |
| Ctrl+C | Copy selection to clipboard |
| Ctrl+X | Cut selection to clipboard |
| Ctrl+V | Paste from clipboard |
| Ctrl+Left | Skip word left |
| Ctrl+Right | Skip word right |
| Ctrl+Home | Jump to beginning of text |
| Ctrl+End | Jump to end of text |
| Home | Jump to beginning of text |
| End | Jump to end of text |
| Shift+Left/Right | Extend selection by one character |
| Shift+Ctrl+Left/Right | Extend selection by one word |

### Tab behavior while inline editing in browse view

| Key | Action |
|-----|--------|
| Tab | Commit current edit, move cursor to next file/folder, start editing it |
| Shift+Tab | Commit current edit, move cursor to previous file/folder, start editing it |

This allows rapid sequential renaming — the user can Tab through files, editing each name without leaving inline edit mode.

### Path field

Same inline editing improvements apply to the path field. Tab in the path field triggers filesystem tab completion (already exists).

## Part 4: Search Button

The Search button on the toolbar opens the existing search panel (same as pressing `\`). The search infrastructure already supports:

- Non-recursive (current directory) and recursive search
- Fuzzy matching via SkimMatcherV2
- Tag search (artist, album, title, etc.) via `search_tag_cache` SQLite table
- Search modes: Filename / Tags / Both
- Sort results by score, name, date, size, extension, artist, album, year, title
- Audio-only filter
- Async background search with cancellation
- 500 result cap

The reasoning model should harden and improve this infrastructure:
- Review the search panel UI for consistency with the new toolbar/pane design
- Ensure search results integrate cleanly with the new three-pane layout
- Review the tag cache for staleness/invalidation issues
- Consider raising the 500 result cap or making it configurable
- Ensure search works correctly when the explore pane is open (tree should reflect search results context)

## Part 5: Config Persistence

### New config fields

Add to `config.toml`:

```toml
[browsing]
show_hidden = false
columns = ["name", "size", "date", "type"]
default_sort = "name"
default_sort_dir = "asc"
default_filter = "all"
layout_explore = "open"       # "open" or "collapsed"
layout_info = "open"          # "open" or "collapsed"
```

The existing `[performance.browsing]` section remains for archive listing mode and timeout.

### Config struct

Add `BrowsingConfig` to `src/config.rs`:

```rust
pub struct BrowsingConfig {
    pub show_hidden: bool,
    pub columns: Vec<String>,
    pub default_sort: String,
    pub default_sort_dir: String,
    pub default_filter: String,
    pub layout_explore: String,
    pub layout_info: String,
}
```

Add `browsing: BrowsingConfig` to `TonepoetConfig`. All fields have `#[serde(default)]` for backward compatibility.

### Restore defaults

"Restore defaults" in the Options menu resets `BrowsingConfig` to its `Default` implementation and saves immediately. This covers layout, columns, sort, filter, and hidden files — everything in the `[browsing]` section.

## Current code locations

- Browse screen draw: `src/tui/draw_browse.rs:32` (`draw_browse_screen`)
- Browse state: `src/tui/browse.rs` (`BrowseState`)
- Browse keybindings: `src/tui/keybindings.rs` (search `AppScreen::Browse`)
- File picker tree: `crates/tui-file-picker/src/tree.rs`
- File picker toolbar: `crates/tui-file-picker/src/render.rs:207`
- File picker split pane: `crates/tui-file-picker/src/render.rs:308`
- Convert screen collapse pattern: `src/tui/convert_screen.rs:22`
- TextInputState: `src/tui/text_input.rs:13`
- Search state: `src/tui/browse.rs:358` (`SearchState`)
- Search panel draw: `src/tui/draw_browse.rs:122`
- Search commands: `src/tui/command.rs` (`:search`, `:rsearch`)
- Config: `src/config.rs`
- Type-ahead: `src/tui/browse.rs:1723`

## Files to modify

### Layout and rendering
- `src/tui/draw_browse.rs` — Three-pane layout, toolbar, collapsed pane rendering, search panel integration
- `src/tui/browse.rs` — Add tree state, pane collapse state, new BrowseState fields

### Toolbar and buttons
- `src/tui/button_map.rs` — New TuiButton variants for toolbar buttons and options menu items
- `src/tui/keybindings.rs` — Handle toolbar button clicks, options menu, refresh command, pane collapse toggles

### Inline editing
- `src/tui/text_input.rs` — Add selection_anchor, clipboard, Ctrl+C/X/V, word skip, selection extension
- `src/tui/inline_edit.rs` — Render selection highlighting

### Config
- `src/config.rs` — Add BrowsingConfig struct
- `src/tui/draw.rs` — Potentially update Config screen to show browse settings

### Tree navigation
- Reuse or adapt `crates/tui-file-picker/src/tree.rs` for the explore pane

### Search
- `src/tui/browse.rs` — Harden search infrastructure
- `src/tui/draw_browse.rs` — Search panel consistency with new layout

### Commands
- `src/tui/command.rs` — Add `:refresh` command

## Exit criteria

- Three-pane layout with explore (tree nav), browse (file list), info (metadata)
- All three panes collapsible to vertical title bar via ▾/▸ toggle
- Double-click browse title maximizes (collapses both side panes)
- Toolbar with Back/Fwd/Up/Refresh/Options/Search/Show-hidden buttons
- Options dropdown with persistent settings (hidden files, columns, default sort, filter, archive listing, save layout, restore defaults)
- Columns submenu lets user add/remove columns
- Explore pane tree syncs with browse navigation
- Refresh button and `:refresh` command reload directory
- Inline editing: double-click select all, Ctrl+C/X/V clipboard, Ctrl+arrows word skip, Shift+arrows selection
- Tab/Shift+Tab in inline edit moves to next/prev file
- Search button opens existing search panel
- Search infrastructure hardened and improved
- All browse settings persist to `[browsing]` section in config.toml
- `cargo check` — zero errors, zero warnings
- `cargo test --no-run` — zero errors, zero warnings
