# DVD-Audio Cross-ATS Extraction — Stream Hint Probe Failure

## Problem

The previous fix added `infer_disc_absolute_stream_hint()` to detect
MLP vs LPCM before processing disc-absolute sectors. This works for
track 1 but fails for later tracks, because the hint probe scans
each track's own sector range independently.

## Evidence

Diagnostic logging added to `extract_track_audio_payload()`:

```
group 3 track 1: sector_address_space=DiscAbsolute { title_set_nr: 2 }, stream_kind_hint=Some(Mlp)
group 3 track 3: sector_address_space=DiscAbsolute { title_set_nr: 2 }, stream_kind_hint=None
group 3 track 6: sector_address_space=DiscAbsolute { title_set_nr: 2 }, stream_kind_hint=None
group 3 track 7: sector_address_space=DiscAbsolute { title_set_nr: 2 }, stream_kind_hint=None
```

Track 1 finds the MLP major sync within 512 sectors. Tracks 3, 6, 7
get `None` — no major sync within their first 512 sectors.

## Why this happens

MLP access units are a continuous stream across all tracks. The major
sync appears periodically — typically near the start of the disc, not
at the beginning of each track's sector range. Later tracks start
mid-stream where there may be no major sync for hundreds or thousands
of sectors.

When `stream_kind_hint` is `None`, the packet filter at line 1188 is
bypassed. Foreign/malformed packets (sub-stream ID 0xA0 with invalid
channel-assignment codes 85, 137, 223, 242) reach the LPCM decoder
and fail.

## Hypothesis

The hint should be determined once (from sectors known to contain
the major sync, such as track 1's range) and shared across all tracks
in the same group. The reasoning model should evaluate this approach
and propose the best fix.

## Constraints

- `extract_track_audio_payload()` is called per-track by the pipeline
  scheduler, potentially in parallel across worker threads
- The hint result could be:
  a. Computed at the materializer level and stored on the track source ref
  b. Computed from track 1's range and applied to all tracks in the group
  c. Determined from the materializer's knowledge of the codec (IFO or
     AOB probe already identified this as MLP during materialization)
  d. Some other approach

- For non-cross-ATS tracks (normal ATS-relative addressing), the hint
  probe returns `None` and the existing strict validation works. The
  fix should not change behavior for normal tracks.

## Code to read

```
src/convert/pipeline/dvda_realize.rs:
  1040  infer_disc_absolute_stream_hint()  — per-track probe (the bug)
  1016  packets_matching_stream_hint()     — packet filter
  1134  stream_kind_hint usage in extract_track_audio_payload()
  1188  filter dispatch

src/convert/pipeline/materializer_dvda.rs:
  1105  title_set_has_existing_aobs detection
  1117  DvdaSectorAddressSpace::DiscAbsolute set on track

src/convert/pipeline/types.rs:
  TrackSourceRef::DvdaTrack fields — could carry a hint
```

## What the reasoning model should produce

1. Root cause confirmation or alternative diagnosis
2. A fix that ensures all tracks in a disc-absolute group get the
   correct stream hint (MLP for Brothers in Arms)
3. No behavior change for normal ATS-relative tracks
