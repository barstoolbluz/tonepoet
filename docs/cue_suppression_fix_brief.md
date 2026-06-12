# CUE Suppression Heuristic — Investigation + Fix Brief

## Problem

Right-clicking a folder of FLAC files in the Browse tab and choosing
"Tagging > Get tags from MusicBrainz" produces "no audio files in
selection" for some directories but not others. The CLI `tags-mb`
command works fine on the same files.

## Primary hypothesis

The CUE suppression heuristic in `expand_paths_to_audio` is the
suspected cause. When a directory contains individual FLAC files plus
a per-track CUE sheet that references WAV files with matching stems,
the suppression logic may be removing all the FLACs.

**The reasoning model should first confirm or refute this hypothesis**
by tracing the actual code path for the failing case (Frostbite)
and the working case (no-CUE directories). If the hypothesis is
wrong, investigate the actual cause in the `TagsFromMb` handler
(`src/tui/command.rs` lines 2238-2360).

## Evidence

| Directory | CUE? | CUE references | Result |
|-----------|------|---------------|--------|
| Frostbite (1980) [FLAC] | Yes, per-track, same-dir WAV stems | Stem-matched to FLACs | FAILS |
| Frostbite (1980) [FLAC] {Japan 24-bit} | Yes, same pattern | Stem-matched | FAILS |
| Ice Pickin' (1978) [FLAC] {Japan 24-bit} | Yes, same pattern | Stem-matched | FAILS |
| Don't Lose Your Cool (1983) [FLAC] | Yes, same pattern | Stem-matched | FAILS |
| Cold Snap (1986) [FLAC] {MFSL} | Yes (.CUE uppercase), same pattern | Stem-matched | FAILS |
| Frozen Alive (1981) [FLAC] | Yes, refs subdirectory WAVs | Stems don't resolve (wrong path) | WORKS |
| Love Can Be Found (1969) [FLAC] | Yes, per-track, same-dir stems | Unknown — may fail parse | WORKS |
| Ice Pickin' (1978) [FLAC] {LP 24-96} | No CUE | N/A | WORKS |
| Barrelhouse Live (1979) [FLAC] | No CUE | N/A | WORKS |
| Complete Imperial Recordings (1991) [FLAC] | Has CUE | Unknown structure | WORKS |

The pattern: directories FAIL when they have a per-track CUE that
references same-directory WAV files whose stems match existing FLACs.
Directories WORK when there's no CUE, or the CUE's FILE references
don't resolve to existing audio files.

The "Love Can Be Found" exception needs investigation — it has a
per-track CUE with matching stems but still works. The CUE has a
non-standard pregap layout (TRACK N+1 appears under FILE N with
INDEX 00, then INDEX 01 under the next FILE block). This may cause
`materializable_cue_referenced_audio_paths_for_queue` to return `Err`,
skipping suppression.

### Example: Albert Collins — Frostbite

```
Directory:
  01 - If You Love Me Like You Say.flac
  02 - Blue Monday Hangover.flac
  ...
  Frostbite.cue

CUE contents:
  FILE "01 - If You Love Me Like You Say.wav" WAVE
    TRACK 01 AUDIO
      INDEX 01 00:00:00
    TRACK 02 AUDIO
      INDEX 00 04:06:70
  FILE "02 - Blue Monday Hangover.wav" WAVE
      INDEX 01 00:00:00
  ...
```

The CUE references per-track WAVs. The WAVs don't exist, but FLACs
with matching stems do. The stem-match fallback in
`resolve_cue_file_reference_for_queue` resolves each reference to the
corresponding FLAC. All 8 FLACs end up in the suppression list. The
CUE is kept but isn't classified as `AudioFile`, so the final filter
removes it too. Result: empty path list, "no audio files in selection."

### Why some per-track CUEs work

CUEs where `materializable_cue_referenced_audio_paths_for_queue`
returns `Err` skip suppression entirely. This happens when:
- A track has no INDEX 01 (common in CUEs with split pregap layout)
- A FILE reference resolves to nothing (no file, no stem match)
- The CUE parse itself fails

So the bug is intermittent — it only triggers when the CUE is
well-formed enough to fully parse AND the stem resolution succeeds
for all tracks.

---

## Root Cause

`into_queue_paths()` unconditionally suppresses every audio file that
any CUE references, without distinguishing:

- **Single-image CUE**: 1 FILE, N TRACKs — the audio file is an image
  to be split by the CUE materializer. Suppression is correct.
- **Per-track CUE**: N FILEs, ~1 TRACK each — the audio files ARE the
  individual tracks. Suppression removes the actual content.

---

## The Fix

### Detection

After `materializable_cue_referenced_audio_paths_for_queue` resolves
all FILE references, compare the number of unique resolved files
to the number of tracks:

- `unique_files < track_count` → single/multi-image CUE → suppress
  (correct existing behavior)
- `unique_files == track_count` → per-track CUE → do NOT suppress

### Implementation options

**Option A (simplest):** At the end of
`materializable_cue_referenced_audio_paths_for_queue`, after building
`referenced` (unique resolved paths) and `resolved_tracks`, check:

```rust
if referenced.len() == sheet.tracks.len() {
    // Per-track CUE: each track has its own file.
    // Don't suppress — these files are the tracks, not images.
    return Err("per-track CUE: audio files are tracks, not images".to_string());
}
```

Returning `Err` causes `cue_referenced_audio_paths` to skip this
CUE's references for suppression purposes (with a log warning).

**Option B (more precise):** Instead of returning `Err`, return only
the paths that are shared by multiple tracks (the actual image files):

```rust
let shared_images: Vec<PathBuf> = referenced
    .into_iter()
    .filter(|path| {
        resolved_tracks.iter().filter(|(_, p, _)| same_path_for_queue(p, path)).count() > 1
    })
    .collect();
Ok(shared_images)
```

This handles mixed-layout CUEs (some tracks share an image, some have
their own file) but is more complex.

**Recommendation:** Option A for now. Per-track CUEs are common (EAC
default for per-track rips). Mixed-layout CUEs are extremely rare.

### Test coverage needed

Add tests for:
1. Per-track CUE with stem-matched FLACs — FLACs should NOT be
   suppressed
2. Single-image CUE — FLACs SHOULD be suppressed (existing behavior
   preserved)
3. Per-track CUE where some FILE references fail — `Err` path, no
   suppression (existing behavior preserved)

---

## Code to Read

```
src/tui/browse.rs:
  2471  expand_paths_to_audio()         — entry point
  2487  QueueExpansionPlan              — struct with cue_sheets + queueable_non_cue
  2501  into_queue_paths()              — suppression logic
  2565  cue_referenced_audio_paths()    — iterates CUEs, calls materializable_*
  2586  materializable_cue_referenced_audio_paths_for_queue()  — resolves + validates
  2643  validate_queue_cue_index_order() — INDEX ordering check
  2670  resolve_cue_file_reference_for_queue()  — stem-match fallback
  2722  collect_audio_reference_candidates()    — directory scan for matches
  2740  unique_queue_reference_candidate()      — 0/1/N candidate handling
  2768  same_path_for_queue()           — canonicalized path comparison
  3094  Tests: 6 existing tests (all single-image scenarios)
```

---

## What the reasoning model should produce

1. The fix in `materializable_cue_referenced_audio_paths_for_queue`
   (Option A or B)
2. At least 2 new tests:
   - Per-track CUE with stem-matched FLACs: verify FLACs not suppressed
   - Single-image CUE with stem-matched FLAC: verify FLAC IS suppressed
     (regression guard)
3. Any necessary changes to the warning log message in
   `cue_referenced_audio_paths` (line 2576) to distinguish per-track
   skip from actual parse errors
