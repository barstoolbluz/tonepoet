# Archive Metadata Edit Hang Investigation

## The Bug (Empirically Observed)

User navigates to `/mnt/fileshare/hodgepodge/flaclab/pbthal/downloads/` in the Browse screen and tries to edit metadata on `10cc - Bloody Tourists (UK)-jun-2023.7z`. The status bar shows "Extracting archive..." and then the TUI hangs. Cursor movement stops working, but mouse wheel scrolling and mouse clicks still work (very slowly).

**This used to work fine before recent changes.**

The filesystem is local — `/dev/sdb1` mounted as fuseblk (NTFS) at `/home/daedalus/fileshare/hodgepodge`. Not a network filesystem.

## What We Checked

All archive extraction and metadata reading code is properly async:
- `extract_archive_to_staging` is async, spawned via `tokio::spawn` (keybindings.rs:9488)
- `collect_all_staged_archive_audio_files` uses `spawn_blocking` (keybindings.rs:9549)
- `read_all_tags_merged_with_metadata` uses `spawn_blocking` (keybindings.rs:9569)
- Progress messages go through the async message channel
- Dir stats computation uses `spawn_blocking` (browse.rs:8401)
- Folder content classification uses `spawn_blocking` (browse.rs:9138)
- Directory scanning uses `spawn_blocking` with 30-second timeout (browse.rs:8438)

**However:** The initial directory listing at browse.rs:3586 (`fs::read_dir`) runs synchronously on the main thread. For each entry it calls `fs::symlink_metadata` and potentially `fs::metadata`. On a large directory on NTFS/fuseblk with many files, this could be slow.

## Likely Candidates

### 1. Concurrent I/O overload on NTFS/fuseblk

When the user navigates to a directory and simultaneously triggers archive extraction, multiple async tasks fire concurrently:
- Folder content classification (for the parent directory)
- Dir stats computation (recursive walk)
- Audio probing (per-file ffmpeg metadata reads)
- Archive extraction (7z subprocess reading from the same filesystem)
- Warm cache indexing

All of these do filesystem I/O on the same NTFS/fuseblk mount. NTFS-3g (the fuseblk FUSE driver) serializes all operations through a single-threaded userspace process. Under concurrent stat/read pressure, every operation queues behind every other operation, effectively serializing the entire I/O workload.

### 2. Browse directory reload during extraction

Something may be triggering a full directory reload or re-probe while extraction is running. If the browse pane re-scans the directory (which on NTFS/fuseblk is slow), cursor movement would block waiting for the scan to complete.

### 3. Recent changes that increased I/O

The folder content summary and classification features (`0e4f874`, `f1608a7`) added:
- `scan_folder_for_classification` — walks subdirectories up to depth 2
- `compute_dir_stats` — recursive walk for file/audio/size counts
- Warm cache indexing — probes entries ahead of cursor

These are all async, but they generate many concurrent stat/readdir calls. On NTFS/fuseblk, this concurrent I/O may overwhelm the FUSE daemon.

## What Needs Investigation

1. Is the directory large (hundreds of entries)? The `pbthal/downloads` directory likely has many large 7z files.

2. Is the hang reproducible on ext4/NVMe (not fuseblk)? If it only happens on fuseblk, the fix is to throttle concurrent I/O on slow filesystems.

3. Does the hang happen without the archive extraction (just navigating to the directory)? If so, the issue is the browse I/O, not the extraction.

4. Are probe/classification/dir-stats tasks cancelled when the user initiates archive metadata editing? If not, they continue competing for I/O.

5. Is there a synchronous filesystem call hiding in the event loop or draw path that we missed?

## Files to Examine

- `src/tui/browse.rs:3586` — synchronous `fs::read_dir` in directory listing
- `src/tui/browse.rs:6454` — `probe_current_with_db` 
- `src/tui/browse.rs:8438` — directory scan task
- `src/tui/browse.rs:9139` — folder classification task  
- `src/tui/keybindings.rs:9488` — archive extraction spawn
- `src/tui/event_loop.rs` — message dispatch, redraw triggers

## Your Task

Diagnose why the TUI hangs when editing archive metadata on a large directory on NTFS/fuseblk. This used to work — something in recent changes (browse performance features, folder content summary, warm cache indexing) is generating enough concurrent I/O to overwhelm the FUSE driver. Find the root cause and fix it — either by cancelling unnecessary I/O during archive operations, throttling concurrent filesystem access, or moving any remaining synchronous calls off the main thread.
