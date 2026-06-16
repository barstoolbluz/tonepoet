# Publish Fix — Independent Track Jobs Sharing an Album Folder

## Problem

Converting a folder of audio files fails with "Publish: destination
already exists" on all but the first track, even when the output
directory is empty. Reproduces on both CLI and TUI.

CLI reproduction:
```
mkdir -p /tmp/empty && tonepoet convert /path/to/6-flacs/ --format flac --output /tmp/empty/
```
Result: 1/6 succeeded, 5 failed.

## Design principle

The TRACK is the unit of work. Not the album. Each file queued for
conversion is an independent job. The pipeline must not group tracks
into album-level jobs, add album-level abstractions, or change queue
item visibility. When a user queues 6 files, they see 6 items in the
queue, each with its own progress, each converting independently.

Multiple independent track jobs that happen to target the same output
folder must coexist. The publish stage must allow this.

## REJECTED approaches (v3-v10)

All previous attempts tried to GROUP single-file items into one
multi-track album job at the processor level. This was rejected
because:
- It changes the user-visible queue model (6 items become 1+5 grouped)
- It introduces `ConversionStatus::Grouped` which leaks backend
  implementation into the UI ("Grouped with 01 - Track Name")
- It adds complexity to the processor, materializer, and queue
- The track should be the unit of work, not the album

## The correct fix

Fix the PUBLISH STAGE to allow independent jobs to add files to an
existing album folder, instead of failing with DestinationExists.

When `publish_album_output()` encounters `plan.album_dir.exists()`
with `OverwritePolicy::FailIfExists`:

- If the plan has a SINGLE audio entry (one track job + sidecars):
  acquire the publish lock, check that the specific audio OUTPUT FILE
  doesn't already exist, then add the track to the existing folder.
- If the plan has MULTIPLE audio entries (multi-track album job like
  SACD/DVD-Audio/CUE): preserve existing behavior (fail if folder
  exists, or replace with backup if overwrite is enabled).

This preserves multi-track album overwrite protection while allowing
independent track jobs to share a folder.

### conversion.log handling

Each independent track job produces a `conversion.log` sidecar.
Multiple jobs targeting the same folder will collide on this filename.

The fix: when adding a track to an existing folder incrementally,
APPEND to the existing `conversion.log` rather than replacing it.
Or: overwrite it (each job's log is self-contained and the last one
wins). Do NOT create per-track log files — the user expects one
`conversion.log` per album folder.

Preferred: append. The final `conversion.log` then contains the
combined record of all track conversions, similar to a multi-track
album log.

### Sidecar collision policy

For audio files: `FailIfExists` means reject if the specific output
file already exists (file-level collision, not folder-level).

For `conversion.log`: always allow overwrite/append (it's diagnostic,
not precious audio data).

For other sidecars (CUE sheets, etc.): allow overwrite if same name.

## Code to read

```
src/convert/pipeline/stages.rs
  7232  publish_album_output() — main publish function
  7309  plan.album_dir.exists() — folder existence check
  7311  OverwritePolicy::FailIfExists — the error return to change
  5216  conversion.log sidecar creation
  10423 acquire_publish_lock() — per-album file lock (keep using this)

src/convert/pipeline/types.rs
  310   PublishPolicy struct
  320   OverwritePolicy enum
  1345  PublishPlan struct
```

## What the reasoning model should produce

1. Modified `publish_album_output()` that allows single-audio-entry
   plans to add their track to an existing album folder under
   `FailIfExists`, while preserving multi-track album folder-level
   collision protection.

2. File-level collision check for audio files within incremental
   publish (reject if the specific .flac/.opus/etc already exists).

3. `conversion.log` append or overwrite — not per-track filenames.

4. The publish lock must still serialize concurrent writes to the
   same album folder.

5. No changes to the queue model, processor, materializer, or
   ConversionStatus enum.

6. No new enum variants, no grouping logic, no queue item merging.

7. Tests:
   - Two sequential single-file jobs to same album folder succeed
   - File-level collision (same track twice) fails under FailIfExists
   - Multi-track album publish still fails on existing folder
   - conversion.log is appended/overwritten, not duplicated

## Constraints

- NO changes to ConversionStatus, ConversionQueue, or processor
- NO track grouping or album-level job merging
- Each queued file remains an independent job with independent progress
- The queue display must show each track as its own item (no "Grouped")
- The publish lock and crash recovery must continue to work
- The --overwrite flag must still work
- Multi-track source types (SACD, DVD-Audio, CUE, 7z) are unaffected

## Future direction (not for this fix)

The 7z materializer currently extracts all tracks and treats them as
one multi-track album job. In future, it should extract tracks and
submit each as an independent track job, same as single-file
conversions. This is NOT part of this fix.
