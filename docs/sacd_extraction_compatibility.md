# SACD extraction compatibility: early Japanese pressings and TOC quirks

## Background

Some early Japanese SACD pressings (SME JSACD SRGS series, circa 2000-2004)
have TOC data that doesn't cleanly align with the actual audio content on
the disc. The reference C extractor (`sacd_extract` from Sound-Linux-More,
version 0.3.9.3) handles these discs without errors. Our Rust implementation
needed several fixes to match that behavior.

This document records what we found, how we fixed it, and what to look for
if a new problem disc surfaces.

## Problem discs used for validation

| Disc | Catalog | Tracks | Issue |
|------|---------|--------|-------|
| Herbie Hancock — An Evening with Herbie Hancock & Chick Corea in Concert | SME JSACD SRGS 4522 | 6 | Final-track TRL2 duration overshoot, garbage sectors past audio |
| Miles Davis — E.S.P. | SME JSACD SRGS 4561 | 7 | Packet overflow, non-zero reserved bits in uncompressed DST headers |
| Miles Davis — Nefertiti | SME JSACD SRGS 4521 | 6 | Trailing incomplete frame at end of sector range |

## Issue 1: Final-track sector range extends past audio

**Symptom:** The last track on the stereo area reads into non-audio sectors
containing garbage data (impossible timecodes, wrong data types, flipped
DST bits).

**Root cause:** The area TOC's `track_end` field and the TRL1 `length_lsn`
for the final track declare more sectors than contain valid audio. On the
Herbie Hancock disc, `track_end = 1927142` but the last valid audio sector
is ~LSN 1737215 — a gap of ~190,000 garbage sectors.

**How sacd_extract handles it:** It applies a timecode-based time filter
derived from TRL2 start/duration values. Frames are only emitted when their
timecode falls within the track's declared time window. Frames in the
garbage zone have impossible timecodes that fall outside the window.

**Our fix:** Wire the time filter through `track_extract_options()` in
`sacd.rs`, which attaches `TimeFilter::new(start_time, duration)` to the
extraction options. The frame reader's `timecode_selected()` method filters
frames at output time.

## Issue 2: Final-track TRL2 duration overshoot

**Symptom:** Even with the time filter, the declared TRL2 duration for the
final track can overshoot the actual audio. On the Herbie Hancock disc,
TRL2 says track 6 has 99,155 frames, but sacd_extract emits only 58,456
frames. The area `total_playtime` (412,839 frames) matches `start + duration`
exactly — both overshoot.

**Key data point:** For tracks 1-5, TRL2 duration exactly matches
sacd_extract's emitted frame count. Only the final track overshoots.

**Our fix:** Dynamic tail trimming. The frame reader tracks how many frames
have been emitted. When an incomplete frame arrives whose timecode is at or
past `filter_start + frames_emitted`, it's treated as the dynamic tail
boundary. This fires exactly where sacd_extract stops: the last emitted
frame is timecode 372,139, and the first non-emitted frame is 372,140.

## Issue 3: Non-monotonic garbage timecodes after the tail

**Symptom:** After the dynamic tail fires, subsequent garbage sectors have
random (non-monotonic) timecodes. Some garbage timecodes are numerically
lower than the last valid frame, causing them to appear "in-window" if
checked against the time filter.

**Our fix:** Sticky `past_time_filter_end` flag. Once the dynamic tail trim
fires, ALL subsequent frames are filtered unconditionally. The flag is
checked in `timecode_selected()`, `handle_incomplete_frame()`, and the
various `can_skip_*` methods. This prevents non-monotonic garbage from
re-entering the emission window.

## Issue 4: Trailing incomplete frame at end of sector range

**Symptom:** The sector range for a track rarely aligns to DSD frame
boundaries. The last frame often has fewer bytes than expected (e.g., 8064
of 9408 bytes, or 672 of 9408 bytes). This is normal — the frame was being
assembled when the sector range ended.

**How sacd_extract handles it:** Drops the trailing partial frame silently.
The emitted frame count and byte count are clean multiples of the frame size.

**Our fix:** `flush()` in `frame.rs` silently drops incomplete pending
frames at end-of-range without recording them as integrity loss. Mid-stream
incomplete frames (from garbage sectors or malformed data) still go through
`handle_incomplete_frame()` where they can trigger errors or be filtered
by the dynamic tail.

## Issue 5: Area TOC frame_format routing

**Symptom:** Some garbage sectors have the `dst_encoded` bit set in the
sector header even though the disc is uncompressed DSD. If the frame reader
uses the per-sector bit for DST/DSD routing, it may try to DST-decode plain
DSD data.

**How sacd_extract handles it:** Uses `area_toc->frame_format` for
DST/DSD routing decisions (whether to decode DST or passthrough DSD). The
per-sector `dst_encoded` bit is only used for frame-info entry width
(3 bytes for DSD, 4 bytes for DST).

**Our fix:** `frame_dst_routing()` in `frame.rs` uses the area-level
`expected_frame_format` (passed via `set_expected_frame_format()`) for
routing. The sector header's `dst_encoded` bit controls `frame_info_uses_dst_width`
only. Format mismatches between the area format and sector header are not errors —
the area format is authoritative.

## Issue 6: Non-zero reserved bits in uncompressed DST headers

**Symptom:** Some frames have `DSTCoded=0` (uncompressed) but the 6-bit
reserved/stuffing field after the DSTCoded bit is non-zero. The DST spec
says these should be zero.

**How sacd_extract handles it:** `libdstdec` treats this as
`InvalidStuffingPattern` — an error. But sacd_extract never hits this path
because it routes by area format, not per-sector bit. For a DSD disc,
uncompressed frames are never sent to the DST decoder.

**Our fix:** With area frame_format routing (issue 5), uncompressed DSD
frames on a DSD-format disc are never DST-decoded, so the reserved bits
are never checked. The DST decoder still rejects non-zero stuffing bits
for frames that ARE sent through the decoder (matching libdstdec behavior).

## Issue 7: Raw SACD timecodes with seconds >= 60

**Symptom:** TRL2 timecodes can have seconds values above 59. For example,
track 1 on the Herbie Hancock disc has `start_time = 0:01:74` (74 frames)
and track 2 has `start_time = 12:40:61` (61 seconds).

**How sacd_extract handles it:** Uses `TIME_FRAMECOUNT` math directly:
`minutes * 60 * 75 + seconds * 75 + frames`. Values above 59 seconds are
valid in this frame-count arithmetic.

**Our fix:** `Timecode::as_frame_count()` uses the same direct multiplication
without rejecting non-normalized values. `is_normalized()` only checks the
frames field (< 75) for the sub-second SACD clock. Timecodes with seconds
above 59 are valid for frame-count computation and time filter comparison.

## TRL2 byte layout

Each TRL2 entry is 4 bytes: `minutes, seconds, frames, flags/reserved`.
Bytes 0, 1, 2 are the time fields; byte 3 is flags. This matches
sacd_extract's `area_tracklist_time_t` struct:

```c
typedef struct {
    uint8_t minutes;
    uint8_t seconds;
    uint8_t frames;
    uint8_t flags;
} area_tracklist_time_t;
```

Start times are at TRL2 offset 8, durations at offset 8 + 255*4 = 1028.

## Debugging a new problem disc

If a new SACD ISO fails extraction:

1. **Check sacd_extract first:** Run `sacd_extract -2 -s -i <iso> -o <dir>`.
   If it extracts cleanly, the disc is valid and our code has a bug.

2. **Dump the sector at the error LSN:**
   ```python
   with open("disc.iso", "rb") as f:
       f.seek(lsn * 2048)
       sector = f.read(2048)
       hdr = sector[0]
       dst = hdr & 1
       fi = (hdr >> 2) & 7
       pi = (hdr >> 5) & 7
       print(f"dst={dst} fi={fi} pi={pi}")
   ```

3. **Check if the error is in garbage territory:** Compare the error LSN
   against the last valid audio LSN. Scan backwards from the error for
   sectors with sane timecodes (seconds < 60, frames < 75).

4. **Compare frame counts:** Check sacd_extract's DSF output for the
   failing track: `sample_count` from the DSF header, divided by 8, divided
   by channel count, divided by 4704 = frame count. Compare against our
   `parser_frames_emitted`.

5. **Check the time filter:** The extraction log should show the TRL2-derived
   time window. Verify the failing frame's timecode is inside/outside the
   window.

## Files involved

| File | Role |
|------|------|
| `crates/sacd-rs/src/frame.rs` | Sector parser, frame assembly, time filter, dynamic tail trim |
| `crates/sacd-rs/src/extract.rs` | Extraction orchestrator, integrity reporting |
| `src/tui/sacd.rs` | TOC parser (TRL1/TRL2), `track_extract_options()` |
| `src/convert/pipeline/stages.rs` | Pipeline integration, `realize_sacd_track_blocking()` |
