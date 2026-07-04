# Brief: Browse Screen Performance Audit

## Objective

Audit the Browse screen's performance-critical code paths with a fine-toothed comb. Identify and implement improvements that make browsing, navigation, probing, and search feel snappy and responsive. Focus on reducing latency, eliminating redundant work, improving cache hit rates, and hardening robustness.

This is NOT a feature brief — no new UI elements or user-facing behavior changes. The user experience should be identical but faster and more reliable.

## Architecture Overview

The Browse screen uses an async message-passing architecture:

1. **Event loop** (`src/tui/event_loop.rs`): 100ms poll interval, renders UI, drains async messages
2. **Async workers**: Directory scans, audio probes, disc probes, search — all dispatched via `tokio::task::spawn_blocking()`, results returned via `AppMessage` channel
3. **Multi-level caching**: In-memory HashMap (fast) → SQLite persistent cache (survives restarts)
4. **Deduplication guards**: `probe_pending` / `disc_probe_pending` HashSets prevent duplicate in-flight work

### Current flow on cursor move

```
Cursor move
  → probe_current_with_db(tx, db)
    → Check in-memory probe_cache (HashMap)
    → Check SQLite probe_cache (mtime + size match)
    → HIT: spawn_cached_audio_probe_metadata_completion() [fast: preemphasis enrichment only]
    → MISS: spawn_audio_probe() [slow: ffmpeg + lofty on spawn_blocking]
  → probe_selected_disc_after_cursor_move()
    → Check disc_probe_cache (fingerprint match)
    → HIT: use cached DiscContents
    → MISS: spawn_disc_probe() [slow: full disc parse on spawn_blocking]
  → (result arrives via AudioProbeComplete / DiscProbeComplete message on next frame)
```

## Areas to Audit

### 1. Browse audio probe debouncing

**Current state**: NO debouncing on browse cursor movement. Every Up/Down arrow immediately spawns `probe_current_with_db()`. The `probe_pending` set prevents duplicate in-flight probes for the SAME path, but rapid scrolling through 20 files spawns 20 separate probes.

**Compare**: The Convert screen's batch probe IS debounced (200ms via `check_batch_probe_debounce()` at `event_loop.rs:288`). The Browse screen has no equivalent.

**Location**: `src/tui/browse.rs:4054` (`probe_current()`), called from `src/tui/keybindings.rs` after every cursor move.

**Question**: Should browse probes be debounced? The user scrolling through 50 files doesn't need all 50 probed — only the one they stop on. A 150-200ms debounce would eliminate most wasted probes while remaining imperceptible at rest. But it would also delay the info pane update by 150-200ms, which might feel sluggish. Consider: debounce only for files NOT already in the in-memory cache (cached probes can populate the info pane instantly, so no debounce needed for cache hits).

### 2. Directory stats redundancy

**Current state**: When the cursor lands on a directory, `spawn_dir_stats()` (`browse.rs:4514`) walks the entire directory to count files and compute total size. This is a separate walk from the directory scan that already happened.

**Location**: `src/tui/browse.rs:4514` (`spawn_dir_stats()`), called from `probe_current()`.

**Question**: Can the scan results (`all_dirs` / `all_files`) be reused to provide file counts and sizes for immediate child directories without a second walk? The scan already reads metadata for every entry. For subdirectories of the current directory, the data is already available — it just needs to be aggregated.

### 3. Tree expansion synchronous filesystem walks

**Current state**: Tree expansion (`sync_tree_to_current_dir()` at `browse.rs:1663`) calls `tui_file_picker::expand_tree_to_path()` which synchronously walks the filesystem to build tree nodes. No caching — re-walks on every expand. Tree expansion calls `child_directories()` which calls `fs::read_dir()` + `has_child_directories()` (another `fs::read_dir()` per child).

**Location**: `src/tui/browse.rs:1663` (`sync_tree_to_current_dir()`), `crates/tui-file-picker/src/tree.rs:90` (`child_directories()`), `tree.rs:119` (`has_child_directories()`).

**Question**: Tree operations are infrequent enough that this may not matter. But for deeply nested paths (e.g., `/home/user/library/artist/album/disc1`), the initial tree expansion walks every ancestor. Consider: cache `has_child_directories` results to avoid the N+1 query pattern (one `read_dir` per directory + one `read_dir` per child to check if children have children).

### 4. Post-scan classification on main thread

**Current state**: After an async directory scan completes, `reapply_after_directory_scan_complete()` runs `classify_dvda_directory_entries()` and `upgrade_iso_kinds()` synchronously on the event loop thread. These functions perform filesystem I/O (magic-byte reads for ISOs, AUDIO_TS.IFO existence checks for DVD-Audio directories).

**Location**: `src/tui/browse.rs:1719-1721` (post-scan processing), `browse.rs:2155` (`upgrade_iso_kinds()`), `browse.rs:2250` (`classify_dvda_directory_entries()`).

**Current mitigations**: Classification results are cached in per-type HashMaps (keyed by path + mtime/size fingerprint). Cache hits are microsecond-level. Only cache misses trigger I/O.

**Question**: For directories with many ISOs (e.g., a collection folder with 50+ ISOs), the cumulative I/O on cache misses could be noticeable. Consider: move classification to a background task, populate entries with "unknown" kind initially, upgrade them as classification results arrive. Or: are the in-memory caches effective enough that this is a non-issue after the first visit?

### 5. Probe cache warm-up and prefetch

**Current state**: Probes are reactive — triggered only when the cursor lands on a file. The info pane shows nothing until the probe completes.

**Question**: Consider speculative prefetch: when the user enters a directory, spawn low-priority probes for the first N visible entries (or all entries if the directory is small). The SQLite cache makes repeated visits instant, but the FIRST visit to a directory with 20 FLAC files requires 20 sequential cursor-move-triggered probes. Prefetch would warm the cache during the natural pause after navigation.

**Constraints**: Prefetch must be cancellable (user may navigate away), low-priority (don't starve the cursor-focused probe), and bounded (don't probe 10,000 files in a flat directory).

### 6. SQLite cache architecture and query patterns

**Database architecture**:
- Single SQLite file at `~/.cache/tonepoet/tonepoet.db`
- WAL journal mode for concurrent read/write safety
- `rusqlite::Connection` is NOT shared across threads — each async worker opens its own connection (`crate::db::Database::open()` at `browse.rs:3766`)
- Main thread holds `app.db: Database` passed to browse via `probe_current_with_db(tx, Some(&app.db))`

**Key cache tables** (all in `src/db.rs`):

`probe_cache` (line 190): Full probe results — format, codec, sample rate, channels, duration, all metadata fields. `file_path TEXT PRIMARY KEY`, validated by `file_mtime` + `file_size`. No secondary indexes. Query: `SELECT * FROM probe_cache WHERE file_path = ?1 AND file_mtime = ?2 AND file_size = ?3` (line 1838).

`search_tag_cache` (line 498): Lightweight tag strings for search. Same key/validation pattern. Includes `last_accessed` for LRU eviction.

`analysis_cache` (line 437): HDCD/pre-emphasis analysis results. Validated by path + mtime + size + algorithm version.

**Current query pattern**: One query per file, triggered reactively on cursor move. When entering a directory with 100 files, this means up to 100 individual SQLite queries as the user scrolls through entries.

**Batch loading opportunity**: No batch query functions exist. When a directory scan completes, the scan result provides all file paths + mtimes + sizes. A single query like `SELECT * FROM probe_cache WHERE file_path IN (?, ?, ...)` could pre-load all cached probes for the directory into the in-memory `probe_cache` HashMap. This eliminates per-cursor-move DB queries for files that are already cached.

Consider also: a `get_cached_probes_for_directory(dir_prefix)` function using `WHERE file_path LIKE ?1 || '%'` — but this requires careful handling since `LIKE` with a leading wildcard won't use the PRIMARY KEY index. A prefix query `WHERE file_path >= ?1 AND file_path < ?2` (where ?2 is the prefix with the last char incremented) WOULD use the B-tree index.

**Additional SQLite considerations**:
- `PRAGMA cache_size`: Default is 2000 pages (≈8MB). For large libraries, increasing this could improve cache hit rates.
- `PRAGMA mmap_size`: Memory-mapped I/O could reduce syscall overhead for frequent small queries.
- Connection pool: Currently each worker opens/closes a connection. A shared connection pool (or keeping connections open longer) would avoid repeated `open()` overhead.
- WAL checkpoint: Automatic, but large bursts of writes (e.g., probing 100 files and caching results) could cause WAL growth. Consider explicit checkpoint after batch operations.

**Location**: `src/db.rs:1830` (`get_cached_probe()`), `src/db.rs:1870` (`store_probe()`), `src/db.rs:1924` (`invalidate_probe()`), `src/tui/browse.rs:4061` (`probe_current_with_db()`).

### 7. Search performance on large directories

**Current state**: Recursive search (`spawn_search_async()` at `browse.rs:3730`) walks the entire directory tree with `WalkDir`, fuzzy-matching every file. For tag search, it reads tags from every audio file (with DB cache).

**Location**: `src/tui/browse.rs:3730` (`spawn_search_async()`).

**Observations**:
- The search is already async and cancellable (atomic flag checked every entry)
- Tag cache in SQLite prevents redundant lofty reads
- Results capped by `search_result_cap` (default 2000)

**Question**: For very large libraries (100K+ files), the walk itself is the bottleneck. Consider: early termination when `result_cap` high-scoring matches have been found (the current implementation collects ALL matches, sorts, then truncates). A priority queue with a score floor that rises as results accumulate could skip weak matches earlier.

### 8. Archive entry probing

**Current state**: Probing an audio file inside an archive requires extracting it to a temp directory, probing the extracted file, then cleaning up. This is `spawn_archive_entry_audio_probe()` at `browse.rs:4315`.

**Location**: `src/tui/browse.rs:4315` (`spawn_archive_entry_audio_probe()`).

**Question**: If the archive is already staged (deferred save mode), the file is already on disk in the staging directory — extraction is unnecessary. Verify this optimization is in place. Also: is the temp directory cleanup happening even on probe failure/cancellation?

### 9. Rendering efficiency

**Current state**: `draw_browse_list()` renders only visible entries (viewport clipping via `scroll_offset` + `visible_height`). Column layout is recomputed every frame via `browse_column_layout()`.

**Location**: `src/tui/draw_browse.rs:1610` (`draw_browse_list()`), `draw_browse.rs:66` (`browse_column_layout()`).

**Question**: Column layout is pure arithmetic based on `inner_w` and `columns` — cheap enough to run per frame. But `render_entry_row()` builds styled spans per entry per frame. For 50 visible entries × 4-11 columns × multiple spans each, this is 200-500 span allocations per frame at 10 FPS. Consider: are there any allocations that could be avoided or reused across frames?

### 10. Event loop message drain

**Current state**: The event loop drains ALL pending messages per frame (`event_loop.rs:122-124`). If 50 probe results arrive simultaneously, all 50 are processed in one frame, potentially causing a frame drop.

**Location**: `src/tui/event_loop.rs:122` (message drain loop).

**Question**: Consider limiting message processing to N messages per frame (e.g., 10), deferring the rest to the next frame. This prevents burst-induced frame drops at the cost of slightly delayed state updates.

### 11. Memory management for large directories

**Current state**: `all_dirs` and `all_files` hold `BrowseEntry` structs for every entry in the current directory. Each `BrowseEntry` contains a `PathBuf`, `String` (name), `String` (name_lower), and metadata. For directories with 10K+ entries, this could be significant.

**Location**: `src/tui/browse.rs` — `all_dirs`, `all_files`, `entries` vectors.

**Question**: Profile memory usage for large directories. Are there fields on `BrowseEntry` that could be computed lazily or stored more compactly? Is `name_lower` (lowercase copy of name) worth the memory for case-insensitive operations?

## Code Locations Summary

| Component | File | Key Functions |
|-----------|------|--------------|
| Directory scan | `src/tui/browse.rs` | `begin_async_scan()` (1727), `spawn_dir_scan()` (4531), `scan_directory_blocking()` (4582) |
| Audio probe | `src/tui/probe.rs` | `probe_audio()` (349), `read_metadata()` (561) |
| Probe dispatch | `src/tui/browse.rs` | `probe_current_with_db()` (4061), `spawn_audio_probe()` (4435) |
| Probe cache (mem) | `src/tui/browse.rs` | `probe_cache` HashMap (1043), `probe_pending` HashSet (1047) |
| Probe cache (DB) | `src/db.rs` | `get_cached_probe()` (1830), `store_probe()` (1870) |
| Dir stats | `src/tui/browse.rs` | `spawn_dir_stats()` (4514) |
| Disc probe | `src/tui/disc_browser.rs` | `spawn_disc_probe()` (362), `probe_disc_contents()` (394) |
| Disc cache | `src/tui/browse.rs` | `disc_probe_cache` HashMap (1092), fingerprint validation |
| Post-scan classify | `src/tui/browse.rs` | `upgrade_iso_kinds()` (2155), `classify_dvda_directory_entries()` (2250) |
| Tree expansion | `src/tui/browse.rs` | `sync_tree_to_current_dir()` (1663) |
| Tree filesystem | `crates/tui-file-picker/src/tree.rs` | `child_directories()` (90), `has_child_directories()` (119) |
| Search (async) | `src/tui/browse.rs` | `spawn_search_async()` (3730) |
| Search (local) | `src/tui/browse.rs` | `execute_search_over_entries()` (3586) |
| Tag cache (DB) | `src/db.rs` | `get_cached_tags()`, `store_cached_tags()` (498) |
| Archive probe | `src/tui/browse.rs` | `spawn_archive_entry_audio_probe()` (4315) |
| Archive listing | `src/tui/archive_listing.rs` | `list_archive_with_options()` (287) |
| Event loop | `src/tui/event_loop.rs` | `run_app()` (18), poll (127), message drain (122), debounce checks (288, 329) |
| Rendering | `src/tui/draw_browse.rs` | `draw_browse_list()` (1610), `browse_column_layout()` (66) |
| Classification caches | `src/tui/browse.rs` | `sacd_classify_cache`, `dvda_iso_classify_cache`, etc. (1070-1083) |

## Files to include in bundle

1. `src/tui/browse.rs` — Scan, probe, cache, search, classification, tree sync
2. `src/tui/draw_browse.rs` — Rendering, column layout, entry rows
3. `src/tui/probe.rs` — Audio probing, metadata reading
4. `src/tui/event_loop.rs` — Event loop, message drain, debounce
5. `src/tui/disc_browser.rs` — Disc probe, fingerprint cache
6. `src/tui/disc_browser_actions.rs` — Disc probe triggers
7. `src/tui/message.rs` — Message types for async results
8. `src/db.rs` — SQLite cache tables, query/store functions
9. `src/tui/archive_listing.rs` — Archive listing, parsing, caching
10. `crates/tui-file-picker/src/tree.rs` — Tree expansion, child directory discovery

## Deliverables

For each area audited, report:

1. **Current cost**: How expensive is this operation? (estimated latency, I/O count)
2. **Current mitigations**: What caching/async/dedup is already in place?
3. **Recommendation**: Concrete improvement with expected impact, or "already optimal — no change needed"
4. **Implementation**: If recommending a change, provide the implementation

Changes should be conservative and targeted. Do not refactor working async patterns or caching infrastructure that is already effective. Focus on the highest-impact improvements:

- Eliminating redundant I/O or computation
- Improving cache hit rates
- Reducing latency on common operations (cursor move, directory enter, search)
- Preventing pathological cases (burst message processing, large directory memory)

## Exit criteria

- All implemented changes pass `cargo check` — zero errors, zero warnings
- All implemented changes pass `cargo test --no-run` — zero errors, zero warnings
- No behavioral changes visible to the user (same UI, same results, just faster)
- Each change accompanied by a brief comment explaining the performance rationale
