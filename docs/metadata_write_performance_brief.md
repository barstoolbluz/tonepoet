# Brief: metadata tag writes are orders of magnitude slower than they should be

Date: 2026-07-09

## The complaint

Saving tag edits from the metadata-editing overlay is slow — much slower
than foobar2000 doing the identical edit on the identical files. The user
edits albums via MusicBrainz lookup + `:fix-caps` and saves; the save
visibly drags. This brief asks for an architectural redesign of the write
path, not a micro-optimization.

## The environment fact that multiplies everything

The user's library lives on **network filesystems**: `~/livetorrents` and
`~/library` are `fuse.sshfs` mounts (verified with `df -T`). Every byte
the write path touches crosses SSH. Any design that does full-file I/O for
a tag edit pays for the whole file over the network — twice if it both
reads and writes. foobar2000 is fast on the same files because it updates
tags in place through the format's padding, touching kilobytes.

## Where the time goes (receipts)

The overlay's save path: `metadata_editor_save` (src/tui/keybindings.rs:5751;
the spawn is near line 5964) →
one `spawn_blocking` for the whole batch →
`apply_audio_tag_changes_with_save_blocks` (src/tui/probe.rs:2120) →
sequential per-file loop → `write_all_tags` (probe.rs:2171).

Per file, `write_all_tags` does:

1. **`std::fs::copy(path, backup)` — a full copy of the audio file**
   (probe.rs:2180, backup path is a `.tonepoet-bak` sibling,
   src/db.rs:2285). For a split-track album: ~30–60 MB per track, every
   track, every save. For the user's single-image CUE albums (the CCR
   DCC rips this regression was noticed on): the image FLAC is
   300–700 MB. Over sshfs this copy means downloading AND uploading the
   entire file. This happens even when the edit changed one TITLE field.
   The backup is deleted moments later on success (probe.rs:2222) — the
   happy path pays the full cost for nothing.
2. `lofty::read_from_path` + `save_to_path(WriteOptions::default())`
   (probe.rs:2185-2214). **VERIFIED against the vendored lofty 0.21.1
   source** (a reference copy of `src/flac/write.rs` is in this bundle
   as `reference/lofty-0.21.1-flac-write.rs`): the FLAC writer carries
   an explicit `TODO: We need to actually use padding` (upstream
   lofty-rs issue #445). Its write path is: `read_to_end` the ENTIRE
   remainder of the file into memory (audio data included), splice the
   rebuilt VORBIS_COMMENT/PICTURE blocks into that byte vector, then
   truncate the file after STREAMINFO and rewrite everything to EOF.
   Padding is never consumed for in-place updates —
   `WriteOptions::default().preferred_padding == Some(1024)` only makes
   it ADD a trailing PADDING block when one is missing. So a full-file
   read + full-file write happens on EVERY lofty FLAC tag save,
   unconditionally — it is the guaranteed case, not the overflow case.
   It also buffers the whole file in RAM (a 700 MB image costs 700 MB
   of memory per save). The single-image path additionally regenerates
   the embedded CUESHEET tag through this same write
   (`regenerate_cuesheet_for_save`, keybindings.rs:5914).
3. Files are processed **sequentially** in one `spawn_blocking`; no
   per-file progress reaches the UI, no cancellation. On sshfs, where
   per-file latency dominates, N files cost N × (round trips) with zero
   pipelining.

A second write path shares the pattern: the metadata pane's inline field
editor (keybindings.rs:19357) creates the same full-file backup **on the
main thread** — the comment says "Step 1 (main thread, fast)". Over sshfs
on a 350 MB image, that "fast" step freezes the TUI for the duration of a
full network copy.

Artwork writes (`write_artwork_to_files` / `remove_artwork_from_files` /
`transactional_artwork_batch`, probe.rs:2234+) are the same architecture
again — per-file full backup + lofty rewrite — plus a full
`read_all_tags_merged_with_metadata` re-read of the batch afterwards.

Cost model for one save of a corrected single-image album over sshfs
today — all of it now verified, none of it conditional: ~2× file size
for the backup copy PLUS ~2× for lofty's unconditional full-file
rewrite = ~4× file size of network I/O, and file-size RAM. Roughly
1.4 GB of transfer (and 350 MB of memory) to change text tags on one
350 MB image. foobar2000's cost for the same edit: kilobytes.

## What to build

Redesign the tag-write architecture so that the happy path for a tag-only
edit touches **metadata bytes, not audio bytes**. Design latitude is
yours; the directions we consider promising, in priority order:

1. **Kill the unconditional full-file backup.** Crash-safety must be
   preserved, but as a *metadata-scope* mechanism, not a file-scope one:
   e.g. journal the original metadata blocks (they are small) so an
   interrupted write can be logically restored; or write-verify-commit
   against the format's structure. The existing write journal in db.rs
   (`begin_metadata_write` / `complete_metadata_write`, used by the
   inline-edit path via event_loop.rs:3299) is prior art to build on.
   Whatever you choose must be explicit about what happens if the
   process dies mid-write on each supported format.
2. **Padding-aware in-place writes for FLAC** (the user's dominant
   format). Lofty 0.21.1 cannot do this — verified above, no
   measurement needed. Write a small FLAC-specific metadata writer
   (metaflac-style: VORBIS_COMMENT + PICTURE + PADDING block juggling
   over the metadata region only is a well-specified, testable
   problem; the reference copy of lofty's block parsing in this bundle
   shows the block layout). Read tags however you like (lofty reads
   are metadata-only and fine); the WRITE fast path must seek/write
   only the metadata region when the new blocks fit in existing
   blocks + padding. When padding IS exhausted, grow it generously in
   the one unavoidable rewrite (foobar's strategy) so the NEXT save is
   in-place — and stream that rewrite (bounded buffer), never
   read_to_end the audio into RAM. Keep lofty writes for the formats
   where you do not build a native fast path.
3. **Bounded parallelism + progress.** Per-file writes are independent
   (per-file results already exist: `MetadataEditorWriteResult`).
   Pipeline them with bounded concurrency; surface per-file progress to
   the UI (`AppMessage`); keep failure isolation per file. Batch-scope
   consistency (CUESHEET + sidecar writeback ordering,
   `cue_sidecar_writeback_result_after_successful_image_save`,
   keybindings.rs:6034) must survive reordering — the sidecar rewrite
   still runs only after ITS image's save succeeded.
4. **Move the inline-edit path's backup off the main thread** and onto
   the same redesigned write machinery, so there is ONE write
   architecture, not two.
5. Artwork batch writes ride the same machinery (they add PICTURE blocks
   — the padding sizing policy must account for them), and drop the
   full-batch re-read afterwards in favor of updating state from what
   was written, if that can be done without lying to the user about
   on-disk truth.

## Hard constraints

- **Semantics are frozen.** What tags get written, the single-image
  CUESHEET regeneration, the sidecar write-back and its
  encoding/byte-span policy (docs/cue_sidecar_writeback_policy.md), MB
  and `:fix-caps` flows — all behavior-identical. This brief is about
  HOW bytes reach disk, not WHICH.
- Crash-safety may change mechanism but not guarantee: an interrupted
  save must never leave a file unreadable or half-tagged without a
  recovery path. State the guarantee per format and test it (kill-point
  tests: truncate/abort mid-write against fixtures, then recover).
- The three-copies invariant for single-image albums (flat tags,
  embedded CUESHEET, sidecar .cue) must not regress
  (writeback_end_to_end_tests pins it).
- Formats beyond FLAC (MP3/ID3v2, M4A, Opus/Vorbis, WavPack) must keep
  working through whatever generic path remains; it is acceptable for
  them to stay slower, not acceptable for them to regress correctness.
- Network filesystems are the design target, not an afterthought — but
  do NOT special-case "if sshfs then X"; make the fast path fast
  everywhere by doing less I/O.
- Mechanical acceptance for the FLAC fast path: a tag-only save against
  a padded fixture must leave the audio region byte-identical AND must
  not create a `.tonepoet-bak` full copy; assert bytes-touched is
  bounded (e.g. compare the file before/after outside the metadata
  region, and assert no sibling backup existed during the write via a
  filesystem watch or an injected hook). Add a large-file fixture
  (generate ≥100 MB FLAC with ffmpeg in tests, as writeback E2E does)
  so the difference is measurable, and have the applier benchmark
  before/after on a real sshfs album.
- Suite baseline: 2620 lib tests passing, 0 failures, zero warnings
  (cold builds — cargo suppresses warnings for cached crates). The
  tui-file-picker crate is separate (`cargo test -p tui-file-picker`,
  70 passing) and untouched by this work.
- The sandbox cannot compile; the applier fixes compile errors and runs
  the benchmarks. State intended behavior per change in tests.

## Files in this bundle

Editor core and write paths:
- `src/tui/probe.rs` — read_all_tags_merged_with_metadata, write_all_tags,
  apply_audio_tag_changes_with_save_blocks, artwork writes, CUESHEET regen
- `src/tui/keybindings.rs` — editor open/save wiring, inline-edit write
  path (19357), sidecar writeback gating
- `src/tui/app.rs` — MetadataEditorState, MetadataEditorWriteResult
- `src/tui/metadata_editor_actions.rs`, `src/tui/metadata_view_models.rs`
- `src/tui/event_loop.rs` — write-complete handlers (3299, 3844)
- `src/tui/message.rs` — AppMessage variants
- `src/tui/draw_overlays.rs` — editor overlay rendering (for progress UI)
- `src/tui/command.rs` — :tags-mb / :fix-caps entry points
- `src/tui/musicbrainz.rs`, `src/tui/gnudb.rs` — tag sources feeding saves
- `src/tui/cue_parser.rs`, `src/convert/cue_parser.rs` — single-image
  detection + sidecar write-back (context; writeback policy is frozen)
- `src/db.rs` — backup + metadata write journal machinery
- `Cargo.toml` — lofty 0.21 dependency
- `docs/cue_sidecar_writeback_policy.md` — frozen policy (context)
- `reference/lofty-0.21.1-flac-write.rs` — verbatim copy of lofty
  0.21.1's FLAC writer (the verified full-rewrite behavior + block
  parsing layout referenced above)
- `docs/metadata_write_performance_brief.md` — this brief
