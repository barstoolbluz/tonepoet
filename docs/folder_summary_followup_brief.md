# Brief: Folder Summary Display Fixes & Scroll Redraw Investigation

## Overview

Five issues with the folder content summary implementation:

1. **"probing..." never resolves** for non-disc folders — the album summary line always says "probing..." because probe cache data for files INSIDE the highlighted folder is never populated on highlight
2. **Directory stats regression** — the old view showed file count + size for directories; now it shows "collection · many albums" which is less useful
3. **"0 stereo" / "0 multichannel"** displayed when count is zero — should be suppressed
4. **Copy protection line** displayed for all discs — may not be useful
5. **Screen redraw stutter** during rapid scrolling (e.g., holding PgDn)

## Fix 1: "probing..." never resolves

### Root cause

`folder_probe_rollup()` at `src/tui/browse.rs:7491` looks up each file path from the folder's `FolderAudioSummary.file_paths` in the browse probe cache. But the probe cache only contains entries for files the user has previously selected/probed. On a first visit to a folder, no files have been probed, so the rollup is empty and the display permanently shows "probing...".

The classification walk correctly avoids calling `probe_audio()`. But nothing triggers probe cache population for the folder's files after classification completes.

### Fix

The "probing..." label should only appear temporarily while probes are actually in flight. When no probes are in flight and the probe cache is empty for these files, the display should simply show the track count and format (from file extensions, which the classification already collected) without the bitrate/sample-rate detail line. The detail line should appear later if the user enters the folder and files get probed, or if SQLite warm-up loads cached probes.

Specifically in the summary line builder at `src/tui/draw_browse.rs:2385`:

```rust
} else if include_probe_status && audio.track_count > 0 {
    parts.push("probing...".to_string());
}
```

Change this to only show "probing..." when there are actually pending probes or folder classification warm-up in flight for this folder. If no probes are active, just omit the bitrate/sample-rate portion entirely — the track count and format from the extension are still useful:

```
8 tracks · FLAC
```

is much better than:

```
8 tracks · FLAC · probing...
```

that never resolves.

## Fix 2: Restore directory stats for Collection folders

### Problem

For folders classified as `Collection` (e.g., an artist directory with 20+ album subfolders), the info pane now shows:

```
kind: collection · many albums
```

The old view showed:

```
files: 247 audio (12.4 GB) / 312 total (14.1 GB)
```

The old view was more useful — it gave concrete numbers. The classification label "collection · many albums" adds no actionable information.

### Fix

For `Collection` and `Unknown` classifications, restore the existing directory stats display (file counts + sizes from `spawn_dir_stats()`). The classification label can remain as a secondary line, but the primary display should be the file/size stats that the user relied on.

The dir stats infrastructure (`spawn_dir_stats()`, `DirStats`, `dir_stats_cache`) is still fully functional. The classification just needs to not REPLACE the stats display — it should supplement it.

In `entry_info_lines()` in `src/tui/draw_browse.rs`, when classification is Collection or Unknown:
- Show dir stats (file count, audio file count, sizes) as the primary content — same as before the classification feature
- Optionally show the classification label beneath it

## Fix 3: Suppress zero channel counts

### Problem

`disc_content_summary_lines()` at `src/tui/disc_browser.rs:470` always shows both multichannel and stereo counts:

```
content: 2 audio streams · 12 tracks
         0 multichannel · 2 stereo
```

When either count is zero, it should be suppressed:

```
content: 2 audio streams · 12 tracks
         2 stereo
```

### Fix

In `disc_content_summary_lines()` at `src/tui/disc_browser.rs:487`:

Build the channel line conditionally:

```rust
let mut channel_parts = Vec::new();
if multichannel_count > 0 {
    channel_parts.push(format!("{multichannel_count} multichannel"));
}
if stereo_count > 0 {
    channel_parts.push(format!("{stereo_count} stereo"));
}
if !channel_parts.is_empty() {
    lines.push(format!("         {}", channel_parts.join(" · ")));
}
```

Apply the same fix to `disc_summary()` at line 460 which has the same issue in the single-line format.

## Fix 4: Copy protection line

### Problem

The copy protection line is shown for all disc sources. For most discs (especially audio-only), this is "none" and wastes a line of info pane real estate.

### Fix

Only show the copy protection line when the value is NOT "none" (case-insensitive). In `src/tui/draw_browse.rs`, find where copy protection is rendered and gate it:

```rust
if !contents.copy_protection.description.eq_ignore_ascii_case("none") {
    // render copy protection line
}
```

## Fix 5: Screen redraw stutter during rapid scrolling

### Problem

When holding PgDn to scroll rapidly, the screen visibly redraws every few seconds. This feels like a stutter/flicker. The user reports it happens both inside and outside a terminal multiplexer.

### Investigation needed

The reasoning model should investigate:

1. **Folder classification firing during scroll** — Is `schedule_cursor_focused_folder_classification()` being called on every cursor move, and are classification completions triggering expensive view rebuilds?

2. **Probe cache warm-up merges** — Are `drain_probe_cache_warm_rows_for_frame()` bursts causing visible state changes (e.g., re-sort, re-filter) that trigger full redraws?

3. **Debounce timing** — Is the cold probe debounce (from the performance audit) firing periodically during scroll, causing a batch of probe state changes?

4. **Dirty flags** — The performance audit added coalesced dirty flags (`take_probe_cache_deferred_work()`). Are these being checked and acted on at a frequency that causes periodic stutters?

5. **Message drain** — Are background messages (warm-up rows, classification results, dir stats) arriving in bursts that cause periodic heavy frames?

The fix should ensure that during rapid continuous scrolling (cursor moving every frame), no expensive recomputation or visible state change occurs until the cursor settles. The debounce should prevent ALL background work (classification, probing, warm-up, stats) from mutating visible state while the cursor is in motion.

## Current code locations

- Album summary line builder: `src/tui/draw_browse.rs:2385` ("probing...")
- `folder_probe_rollup()`: `src/tui/browse.rs:7491`
- `entry_info_lines()`: `src/tui/draw_browse.rs` (search `fn entry_info_lines`)
- Dir stats display: `src/tui/draw_browse.rs` (search `current_dir_stats`)
- `disc_summary()`: `src/tui/disc_browser.rs:450`
- `disc_content_summary_lines()`: `src/tui/disc_browser.rs:470`
- Copy protection rendering: `src/tui/draw_browse.rs:2463`
- `schedule_cursor_focused_folder_classification()`: `src/tui/browse.rs:6674`
- Cold probe debounce: `src/tui/browse.rs` (search `probe_debounce`)
- Warm-up drain: `src/tui/browse.rs` (search `drain_probe_cache_warm_rows`)
- Deferred work flags: `src/tui/browse.rs` (search `take_probe_cache_deferred_work`)
- Event loop message drain: `src/tui/event_loop.rs` (search `try_recv`)

## Files to modify

1. **`src/tui/draw_browse.rs`** — Fix "probing..." display, restore dir stats for Collection, gate copy protection
2. **`src/tui/disc_browser.rs`** — Suppress zero channel counts in disc_summary and disc_content_summary_lines
3. **`src/tui/browse.rs`** — Investigate and fix scroll redraw stutter (debounce, dirty flags, message drain timing)
4. **`src/tui/event_loop.rs`** — Potentially adjust message drain or deferred work timing

## Exit criteria

- Non-disc album folders show "N tracks · FORMAT" without "probing..." when no probes are in flight
- Collection folders show file count + size stats (like before), with optional classification label
- Disc summaries suppress "0 multichannel" or "0 stereo" when count is zero
- Copy protection line only shown when value is not "none"
- No visible screen stutter during rapid PgDn scrolling
- `cargo check` — zero errors, zero warnings
- `cargo test --no-run` — zero errors, zero warnings
