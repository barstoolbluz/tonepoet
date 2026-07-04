# Brief: Fix navigation into disc source directories

## Problem

DVD-Video, DVD-Audio, and Blu-ray directories cannot be navigated into from the Browse screen. Pressing Enter, Right arrow, or double-clicking these directories toggles selection instead of descending into them.

Example: `~/library/zeppelin/Led Zeppelin - Led Zeppelin DVD (2003) [IFO] {DVD-9}` contains subdirectories `artwork/`, `disc 01/`, `disc 02/`. The user cannot browse into any of them from the Browse screen.

## Root cause

When a directory is classified as a disc source (during `classify_dvda_directory_entries()` or `upgrade_iso_kinds()`), its `EntryKind` changes from `Directory` to `DvdVideoDir`, `DvdAudioDir`, or `BlurayDir`. All navigation code checks `entry.is_dir()` or matches on `EntryKind::Directory | EntryKind::ParentDir`, which excludes disc directory kinds.

There are FOUR code paths that need fixing:

### 1. `BrowseEntry::is_dir()` — `src/tui/browse.rs:1043`

```rust
pub fn is_dir(&self) -> bool {
    matches!(self.kind, EntryKind::Directory | EntryKind::ParentDir)
}
```

Returns `false` for `DvdVideoDir`, `DvdAudioDir`, `BlurayDir`.

### 2. `enter_selected()` — `src/tui/browse.rs:2897`
(line number confirmed correct after performance audit)

```rust
pub fn enter_selected(&mut self) -> bool {
    if let Some(entry) = self.entries.get(self.selected_index) {
        if entry.is_dir() {  // ← fails for disc dirs
```

This is the function that actually performs directory navigation. It calls `is_dir()` internally, so even if callers are fixed, this guard blocks entry.

### 3. Enter/Right key handler — `src/tui/keybindings.rs:2817-2845`

The Enter key handler dispatches on `entry.kind`:

```rust
match &entry.kind {
    EntryKind::Directory | EntryKind::ParentDir if app.browse.is_in_archive() => { ... }
    EntryKind::Directory | EntryKind::ParentDir => {
        app.browse.enter_selected();  // ← disc dirs never reach here
    }
    ...
    EntryKind::DvdAudioDir | EntryKind::DvdVideoDir | EntryKind::BlurayDir => {
        app.browse.toggle_selection();  // ← disc dirs land here instead
    }
}
```

The Right arrow handler at line 2790 uses `entry.is_dir()`:

```rust
if entry.is_dir() {  // ← fails for disc dirs
    app.browse.enter_selected();
}
```

### 4. Mouse double-click handler — `src/tui/keybindings.rs:23970`

```rust
EntryKind::Directory | EntryKind::ParentDir => {
    app.browse.enter_selected();  // ← disc dirs fall to _ catch-all
}
_ => {
    app.browse.toggle_selection();
}
```

## Previous fix attempt (failed)

A previous attempt added `is_navigable_dir()` to `BrowseEntry` and updated the keybinding dispatch and `enter_selected()`. This compiled successfully but the user reported navigation still didn't work. The changes were reverted. The reasoning model should investigate why these surface-level fixes were insufficient — there may be additional guards, state checks, or post-navigation behavior (e.g., `refresh()` reclassifying the directory, or `sync_tree_to_current_dir()` rejecting non-`Directory` entries) that prevent the navigation from completing.

## Design intent

Disc directory classification is an INFO OVERLAY — it tells the info pane and convert routing that this directory contains a disc source. It should NOT be a NAVIGATION BARRIER. These are real filesystem directories with browsable contents (artwork, disc subdirectories, VIDEO_TS folders, BDMV folders). Users must be able to navigate into them.

The disc classification should be preserved so the info pane shows disc metadata and the convert flow routes correctly. But Enter, Right arrow, and double-click should navigate into these directories exactly as they do for regular directories.

## Secondary issue: directory stats

`probe_current()` at `src/tui/browse.rs:4913` also uses `is_dir()` to decide whether to probe directory stats. Disc directories won't get file counts or size stats computed. This should be fixed alongside the navigation issue.

## Fix approach

Add a method `is_navigable_dir()` (or equivalent) that returns `true` for `Directory | ParentDir | DvdAudioDir | DvdVideoDir | BlurayDir`. Use it in:

1. `enter_selected()` — the actual navigation function
2. Enter key handler — both archive and non-archive branches
3. Right arrow handler
4. Mouse double-click handler
5. `probe_current()` dir stats branch

BUT: also investigate why the previous attempt to do exactly this didn't work. Trace the full code path from Enter key → `enter_selected()` → `refresh()` → scan → classification to verify nothing downstream re-blocks navigation. Check:

- Does `refresh()` or `begin_async_scan()` have any special behavior when `current_dir` is a disc source?
- Does `sync_tree_to_current_dir()` reject disc directories?
- Does `classify_dvda_directory_entries()` or `upgrade_iso_kinds()` do anything that interferes with browsing INSIDE a disc source directory?
- Are there any `is_dir()` checks in `apply_view()`, `sort_entries()`, or the parent-entry construction that would break?

## Code locations

- `BrowseEntry::is_dir()`: `src/tui/browse.rs:1043`
- `enter_selected()`: `src/tui/browse.rs:2897`
- Enter key handler: `src/tui/keybindings.rs:2817` (match on entry.kind)
- Right arrow handler: `src/tui/keybindings.rs:2790` (entry.is_dir())
- Mouse double-click: `src/tui/keybindings.rs:23970` (match on EntryKind)
- Dir stats probe: `src/tui/browse.rs:4913` (entry.is_dir())
- `refresh()`: `src/tui/browse.rs:2092`
- `begin_async_scan()`: `src/tui/browse.rs:2123`
- `sync_tree_to_current_dir()`: `src/tui/browse.rs:2059`
- `classify_dvda_directory_entries()`: `src/tui/browse.rs:2663`
- `upgrade_iso_kinds()`: `src/tui/browse.rs:2560`
- `apply_view()`: `src/tui/browse.rs:2706`
- `apply_view()`: `src/tui/browse.rs` (search `fn apply_view`)

## Files to modify

1. **`src/tui/browse.rs`** — Add `is_navigable_dir()`, update `enter_selected()`, update dir stats probe
2. **`src/tui/keybindings.rs`** — Update Enter, Right arrow, and double-click handlers

## Exit criteria

- User can Enter/Right-arrow/double-click into DvdVideoDir, DvdAudioDir, BlurayDir entries
- Contents of disc directories display normally (subdirectories, audio files, etc.)
- Disc classification preserved in info pane and convert routing
- Directory stats computed for disc directories
- No regression in regular directory or archive navigation
- `cargo check` — zero errors, zero warnings
- `cargo test --no-run` — zero errors, zero warnings
