# Brief: Folder Content Summary with Classification Gate

## Overview

When a user highlights a folder in the Browse view, the info pane should show a useful summary of its audio content — format, channels, bit-depth, sample rate. This already works for Blu-ray, SACD, DVD-Audio, and DVD-Video disc sources. This brief extends it to ALL folders containing music files, with a classification gate that prevents expensive recursive probing on large collection folders.

## Part 1: Classification Gate

### Problem

A user highlighting `~/library/genesis` (50+ album subdirectories) must NOT trigger recursive probing of every album. A user highlighting `~/library/genesis/Genesis - Selling England by the Pound (1973) [FLAC] {UK First-Press LP}` (one album, 8 tracks) SHOULD see a detailed summary.

The gate answers one question: "Is this folder one thing (album/disc) or many things (collection)?" The answer determines the depth of probing.

### Classification outcomes

| Classification | Meaning | Info pane behavior |
|---|---|---|
| `Album` | Folder contains direct audio files | Summarize format/bitrate/channels for files in this folder |
| `Disc` | Folder contains a disc marker (BDMV/VIDEO_TS/AUDIO_TS/SACD) | Use existing disc probe summary |
| `MultiDisc` | Folder contains 2+ units that share a common naming pattern (e.g., "disc 01", "disc 02") | Treat as one logical album, summarize all units together |
| `Collection` | Folder contains 2+ unrelated units, or exceeds fan-out/I/O thresholds | Show only top-level counts ("12 albums, ~180 tracks"), no per-file probing |
| `Unknown` | I/O budget exhausted or no audio content found | Show "folder" or nothing |

### Heuristics (execution order matters)

#### Check 1: Direct audio (cheapest — one `read_dir`)

Scan immediate children of the highlighted folder. If any entry has an audio file extension → classification is `Album`. Summarize this folder's audio files. Do NOT descend into subdirectories.

Cost: 1 `read_dir` syscall.

#### Check 2: Disc marker at root

During the same `read_dir`, check for disc markers: `BDMV/`, `VIDEO_TS/`, `AUDIO_TS/` (directory names only — no file reads). ISO files (`.iso` extension) are recorded as units by extension alone — do NOT read magic bytes during the classification walk (that's I/O reserved for the disc probe on descend). If a disc marker directory is found → classification is `Disc`. Use existing disc probe infrastructure.

Cost: 0 additional syscalls (piggyback on check 1, directory name checks only).

#### Check 3: Fan-out early-out

During the same `read_dir`, count immediate subdirectories. If `subdir_count >= 8` AND no direct audio AND no disc marker → classification is `Collection`. Stop immediately.

Threshold: **8 subdirectories**. Rationale: most box sets are 2-6 discs. An 8-disc set is rare enough that classifying it as a collection is an acceptable trade-off. Artist folders with 20-50 albums are caught instantly.

Cost: 0 additional syscalls.

#### Check 4: Shallow walk (depth ≤ 2, I/O budget ≤ 100)

Walk subdirectories up to 2 layers deep, counting "units" found:

For each subdirectory:
- Has disc marker (BDMV, VIDEO_TS, AUDIO_TS) → record one UNIT, do NOT recurse into it (disc internals can have hundreds of files)
- Has direct audio files → record one UNIT
- Contains an ISO file → record one UNIT
- None of the above → recurse one more level (if depth allows)

Stop conditions:
- `unit_count >= 2` → check the multi-disc heuristic (check 5), then classify as `Collection` or `MultiDisc`
- I/O budget (100 directory reads/stats) exceeded → classify as `Collection/Unknown`
- Depth limit (2) reached → classify based on units found

If `unit_count == 1` → classification is `Album` or `Disc` (single unit, show detailed summary).
If `unit_count == 0` → classification is `Unknown` (no audio content found).

#### Check 5: Multi-disc heuristic

When 2+ units are found, before classifying as `Collection`, check whether they look like discs of the same album:

- All unit folder names match a disc-like pattern: starts with "disc", "cd", "disk", or is a bare number ("01", "02"), case-insensitive
- Examples: "disc 01", "disc 02", "CD1", "CD2", "Disc One", "Disc Two", "01", "02"
- All units are siblings (same parent directory)
- The units are ALL of the subdirectories (no non-disc siblings, ignoring non-audio folders like "artwork", "scans", "covers", "booklet")

If the multi-disc heuristic matches → classification is `MultiDisc`. Treat all units as one logical album. Summarize all units together (total track count, format breakdown across all discs).

If the heuristic does NOT match (e.g., sibling folders named "Selling England by the Pound" and "The Lamb Lies Down on Broadway") → classification is `Collection`.

Ignorable sibling folder names (not counted as non-disc siblings): "artwork", "art", "scans", "scan", "covers", "cover", "booklet", "booklets", "extras", "bonus", "images", "photos", "logo", "liner notes". Case-insensitive.

#### Check 6: Debounce and cache

Classification fires on highlight (cursor move). Scrolling through a directory listing must NOT re-walk the filesystem for every row.

- **Debounce**: Use the existing browse probe debounce infrastructure. Classification should be debounced the same way audio probes are — only fire after the cursor settles.
- **Cache**: Cache classification results per-path with a directory identity fingerprint (mtime). Use the same `ProbeCacheIdentity` pattern as the existing probe cache.
- **Invalidation**: Cache entries invalidate when the directory's mtime changes (a file is added/removed/renamed inside it).

### Reasoning model latitude

The heuristics above are our best design. However, the reasoning model has full latitude to improve them. In particular:

- If a better or cheaper multi-disc detection heuristic exists (beyond the naming patterns in Check 5), implement it instead. The goal is correctly identifying same-album multi-disc sets at minimal or no additional I/O cost beyond what the classification walk already performs.
- If the ordering of checks can be improved for earlier short-circuiting, reorder them.
- If the thresholds (fan-out 8, I/O budget 100, depth 2) should be different based on analysis of the code and data structures, adjust them with a comment explaining why.
- If additional ignorable sibling folder names are obvious, add them.

The constraints that must NOT change: the classification walk must never call `probe_audio()` or `read_metadata()`, must be debounced and cached, and must respect a hard I/O budget.

### Constants

```rust
const FOLDER_CLASSIFY_FAN_OUT_THRESHOLD: usize = 8;
const FOLDER_CLASSIFY_IO_BUDGET: usize = 100;
const FOLDER_CLASSIFY_MAX_DEPTH: usize = 2;
```

## Part 2: Info Pane Summary Display

### Current state

Disc sources (Blu-ray, SACD, DVD-Audio, DVD-Video) show structured summaries in the info pane when highlighted. Regular folders show nothing useful — just the folder name and basic filesystem metadata (size, date).

### New display for all classified folders

#### Album (folder with direct audio)

```
 8 tracks · FLAC · 24-bit/96kHz · stereo
 duration: 42:18 · size: 1.2 GB
```

If mixed formats/rates exist in the folder, show the dominant format with a note:

```
 12 tracks · FLAC · mixed rates
 8× 24-bit/96kHz · 4× 16-bit/44.1kHz
 duration: 58:04 · size: 1.8 GB
```

The format/rate summary should be derived from the probe cache — if files haven't been probed yet, show what's available and note "probing..." for unprobed files.

#### Disc (BDMV, VIDEO_TS, AUDIO_TS, SACD)

Improve the existing disc summary. Instead of printing raw stream details that run off the screen, show:

```
 content: 18 audio streams · 62 tracks
          12 multichannel · 6 stereo

 streams:
   LPCM 24-bit/96kHz 5.1
   LPCM 24-bit/96kHz stereo
   TrueHD 24-bit/96kHz 5.1
   DTS-HD MA 24-bit/96kHz 5.1
   AC3 16-bit/48kHz 5.1
   AC3 16-bit/48kHz stereo
```

Stream display rules:
- Cap at 6 streams
- Sort priority: LPCM first, then by channels (stereo first, multichannel later), then by bit-depth (24-bit first), then by sample rate (higher first)
- If more than 6 streams exist, show "... and N more" below the list
- The "Audio Streams" button/pill remains for users who want the full list

For SACD: always 1-bit/2.8224MHz DSD, so just show:

```
 content: 2 audio streams · 12 tracks
          1 multichannel · 1 stereo

 streams:
   DSD 2.8MHz 5.1
   DSD 2.8MHz stereo
```

#### MultiDisc

```
 3 discs · 36 tracks · FLAC · 16-bit/44.1kHz
 duration: 2:14:30 · size: 2.1 GB
```

Or with mixed formats:

```
 3 discs · 36 tracks · FLAC · mixed rates
 disc 01: 12 tracks · 24-bit/96kHz
 disc 02: 12 tracks · 24-bit/96kHz
 disc 03: 12 tracks · 16-bit/44.1kHz
```

#### Collection

```
 collection · 24 albums
```

No per-file probing. Just the count of units found during classification (or "many albums" if the fan-out early-out fired before counting).

### Probing depth on highlight vs descend

- **On highlight**: classification walk only (check 1-6). No ffmpeg probing, no tag reading. Format/bitrate info comes from the probe cache (if files were previously probed) or from file extensions (if not).
- **On descend** (user enters the folder): full probing of visible entries via the existing browse probe infrastructure. This is when ffmpeg runs.

The classification walk should NEVER call `probe_audio()` or `read_metadata()`. It only uses:
- `read_dir()` for directory listing
- File extension checking via `classify_file()`
- Disc marker existence checks (directory name / file existence)
- Probe cache lookups for format/rate info (if available, not blocking)

## Current code locations

### Disc probe summaries (existing)
- `disc_summary()`: `src/tui/disc_browser.rs` (search `fn disc_summary`)
- `presentation_summary()`: `src/tui/disc_browser.rs` (search `fn presentation_summary`)
- Info pane disc rendering: `src/tui/draw_browse.rs` (search `disc_summary` in `entry_info_lines`)

### Info pane rendering
- `entry_info_lines()`: `src/tui/draw_browse.rs` (search `fn entry_info_lines`)
- `draw_browse_info()`: `src/tui/draw_browse.rs` (search `fn draw_browse_info`)

### Probe infrastructure
- `probe_current_with_db()`: `src/tui/browse.rs` (search `fn probe_current_with_db`)
- Probe cache: `src/tui/browse.rs` (search `probe_cache`)
- `classify_file()`: `src/tui/browse.rs:8704` (free function that classifies a path by extension into EntryKind)

### Disc detection
- `is_dvdv_directory()`: `src/disc/dvdv_utils.rs`
- `is_dvda_directory()`: `src/disc/dvda_utils.rs`
- `is_bluray_source()`: `src/disc/bluray_utils.rs`
- `is_sacd_iso()`: `src/tui/sacd.rs:174` (magic-byte check — NOT for classification walk, only for disc probe on descend)

### Directory stats
- `spawn_dir_stats()`: `src/tui/browse.rs` (search `fn spawn_dir_stats`)

## Async integration

The classification walk should run on `tokio::task::spawn_blocking()`, same as probe and dir stats workers. It needs:

- A new `AppMessage` variant (e.g., `FolderClassifyComplete`) in `src/tui/message.rs` carrying the classification result, path, and identity fingerprint
- A reducer in `src/tui/event_loop.rs` that accepts the result only if the path and identity still match the current selection
- Stale-result rejection using the same generation/identity pattern as probe completions

The classification result should be stored alongside or integrated with the existing dir stats cache, since both fire on directory highlight.

## Album display: duration and size

The Album display shows duration and size. Duration requires probe data (ffmpeg). On first highlight when the probe cache is cold, duration is unknown. The display should:

- Show track count and format info from the classification walk (extension-based, always available)
- Show duration and size from the probe cache or dir stats when available
- Show "duration: —" or omit the line when no probe data exists yet
- NEVER block the classification result waiting for probe data

The existing `spawn_dir_stats()` already computes total size for directories. Duration accumulation can be added to the dir stats result if all files are probed, or shown as partial.

## Files to modify

1. **`src/tui/browse.rs`** — Folder classification engine (new), classification cache, integration with probe_current flow
2. **`src/tui/draw_browse.rs`** — Info pane rendering for Album/MultiDisc/Collection classifications, improved disc stream summary display
3. **`src/tui/disc_browser.rs`** — Refactor `disc_summary()` / `presentation_summary()` for the new stream display format (sorted, capped at 6, prioritized)
4. **`src/tui/message.rs`** — Add `FolderClassifyComplete` message variant
5. **`src/tui/event_loop.rs`** — Add reducer for classification results

## Exit criteria

- Highlighting a single-album folder shows format/bitrate/channel summary
- Highlighting a disc source folder shows improved stream summary (sorted, capped, prioritized)
- Highlighting a multi-disc folder (disc 01/disc 02) treats it as one album
- Highlighting a collection folder (artist directory) shows "collection · N albums" without deep probing
- Fan-out threshold (8 subdirs) prevents probing artist/label folders
- I/O budget (100 reads) prevents runaway walks
- Depth limit (2 layers) respected
- "Yes – Fragile BD 2015/FRAGILE/BDMV" detected at depth 2
- Classification debounced and cached per-path with mtime fingerprint
- Classification NEVER calls probe_audio() or read_metadata()
- Existing disc probe ("Audio Streams" button) still works for full details
- `cargo check` — zero errors, zero warnings
- `cargo test --no-run` — zero errors, zero warnings
