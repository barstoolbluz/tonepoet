# Tonepoet: Session Continuation Prompt

## What is tonepoet?

Tonepoet is a Rust CLI+TUI audio conversion toolkit. The main interface is a ratatui-based TUI with 5 tabs: Browse (1, default), Library (2, placeholder), Convert (3), Queue (4), Config (5). The user navigates their filesystem in Browse, selects files, configures conversion settings, and processes them through a multi-worker pipeline.

Read `CLAUDE.md` in the project root for build instructions, workspace structure, key types, and coding conventions. Everything builds inside `nix develop`.

## What has been built (in rough chronological order)

### Core TUI infrastructure
- Tokyo Night themed ratatui interface with vi-style keybindings
- Two-pass rendering: draw (immutable state), then register mouse buttons (mutable ButtonRenderMap)
- Vi command mode (`:` opens command input): `:q`, `:e`, `:set`, `:preset`, `:cd`, `:sort`, `:filter`, `:rename`, `:cp`, `:mv`, `:del`, etc.
- PillState<T> generic pill selector widget for format options
- All overlay footers are clickable pill buttons with mouse support
- Context menus (right-click / `:context`) with side-by-side submenu rendering

### Browse screen (primary focus of recent work)
- Full file browser with sortable columns (Name, Size, Date, Type)
- Async directory scanning and audio probing (ffmpeg-next + lofty)
- Info pane showing technical details, ReplayGain/R128, metadata
- Inline search panel with fuzzy matching, recursive search, tag search, multiple sort modes
- SQLite tag cache with LRU eviction for search
- Multi-select: click, Ctrl+click toggle, Ctrl+double-click range, Ctrl+V visual mode
- Enter/double-click = select (not load into Convert)
- Context menu with submenus:
  - **Convert >** (Custom, Last Used, presets)
  - Select/Select All/Select Inverse/Deselect
  - Edit metadata / Analyze
  - **Utilities >** (Verify, CUE sheet multi-file, CUE sheet single-image, Bit compare mark/compare/clear)
  - **File operations >** (Rename, Bulk Rename, Copy to..., Move to..., Move to Trash)
  - Copy path
- Type-to-navigate: bare letter keys accumulate into a prefix buffer and jump to the first matching entry; resets after 1.5s timeout; Esc clears
- Bookmarks overlay
- Recent files overlay
- Bulk rename wizard with template engine and CUE import (`:rename-all`)
- Archive support: 7z listing, password keychain, multi-format

### Analysis features
- DR meter (TT algorithm, matching foobar2000)
- Peak, RMS, clipping count, DC bias, actual bit depth
- LUFS + true peak via loudgain subprocess
- Pre-emphasis detection: metadata evidence (tags, CUE FLAGS PRE, log files, catalog number lookup) + spectral analysis (full M0/M1/M2 model comparison with corpus training — developed in a separate session, lives in `src/tui/preemphasis/`)
- SQLite analysis cache with algorithm versioning

### Metadata editing
- Full tag editor overlay: all tags enumerated, multi-file merge with `<multiple values>`
- Per-file detail overlay for mixed fields
- Auto-populate title/track from filenames
- Disc/track sorting
- Batch write with backup/restore

### Utilities (under the Utilities submenu)
- **Verify**: FLAC `--test --silent`, WavPack `-vq`, ffmpeg decode-to-null for others
- **CUE sheet generation**: multi-file (one FILE per track) and single-image (cumulative timestamps)
- **Bit compare**: two-phase mark-then-compare workflow. Decodes both files to s32le via ffmpeg pipes, compares chunk-by-chunk with fill_buf for correctness. Also `:compare path1 path2` command mode.

### Convert screen
- 4-pane layout: Source, Metadata, Format (pills), Output Options
- Tab reorder, `:queue`/`:commit`/`:go` gate
- SourceMode enum (Empty, Single, Batch)
- Batch render + expand overlay
- Tab completion for commands
- CLI file args
- Async probe

### Queue screen
- Processing with progress tracking
- Pause/resume, retry failed, clear completed

### Database (SQLite, WAL mode)
- Schema versioning via PRAGMA user_version (currently v10+, may be higher from the PE session)
- Tables: presets, bookmarks, recent_files, analysis_cache, conversion_queue, search_tag_cache, pe_corpus (from the PE session)

## Architecture patterns

- **Async**: tokio runtime, mpsc channels for TUI messages, Arc<AtomicBool> cancel flags
- **Event loop**: crossterm events + AppMessage channel, merged in select!
- **Overlays**: ActiveOverlay enum with per-overlay scroll/state, key handlers in keybindings.rs
- **Mouse**: ButtonRenderMap records rects during draw, keybindings.rs dispatches clicks via find_button_at
- **Commands**: Command enum parsed in command.rs, dispatched in execute_command
- **Context menu**: ContextMenuEntry::Item / Separator / Submenu, two-level side-by-side rendering

## What's next

### Browse screen polish (nearly done)
- The browse screen is mature. Most features are implemented and tested.
- Minor deferred items in memory files: preset bar clicks, hover highlighting, text field click-to-edit

### Library screen (next major milestone)
- Placeholder tab exists, no implementation yet
- Design direction: metadata-indexed persistent catalog of curated paths
- Search results return individual tracks grouped under album headers
- Full metadata index in SQLite, exposable via `:sql` for ad hoc queries
- Reuses browse infrastructure (search, metadata editing, analysis) mutatis mutandis
- "Open in Library" context menu action from Browse
- See `project_library_todo.md` and `project_search_architecture.md` in memory

### Other deferred items
- Metadata editor: CUE sheet import pill, clipboard paste pill
- Archive descent (browse into archives)
- freedb/MusicBrainz query facility (discussed, not planned)

## User preferences

- Plans before implementation, validation before execution, audits after implementation
- "Whenever we fix bugs we change code, whenever we change code we re-audit"
- Prefers concise communication, no unnecessary summaries
- Commit and push when asked, not proactively
- Uses the TUI extensively for testing — mouse mode means can't copy/paste from terminal
- Has a large music library at ~/library/ organized as `Artist - Album (Year) [Format] {Pressing info}`
- **Do NOT modify any files in ~/library/**

## Key files for orientation

| File | Purpose |
|------|---------|
| `CLAUDE.md` | Full project docs, build, structure, conventions |
| `src/tui/app.rs` | AppState, all overlay/state enums |
| `src/tui/keybindings.rs` | All key + mouse event dispatch (~5000+ LOC) |
| `src/tui/command.rs` | Command enum, parser, execute_command |
| `src/tui/context_menu.rs` | Menu builders and action dispatch |
| `src/tui/browse.rs` | BrowseState, search, directory operations |
| `src/tui/draw_browse.rs` | Browse screen rendering + button registration |
| `src/tui/draw_overlays.rs` | All overlay rendering |
| `src/tui/event_loop.rs` | Async event loop, message handlers |
| `src/tui/message.rs` | AppMessage enum |
| `src/tui/probe.rs` | Audio probing (ffmpeg-next) + metadata (lofty) |
| `src/tui/analyze.rs` | DR meter, peak/RMS/clipping analysis |
| `src/tui/preemphasis/` | Pre-emphasis detection (multi-file module) |
| `src/db.rs` | SQLite database, migrations, all cache operations |
| `src/config.rs` | TonepoetConfig, UiConfig |
