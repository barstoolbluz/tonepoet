# DVD-Video LPCM Sub-Header Follow-Up — Verified Error Evidence

## Status

The previous fix (SAMG-derived disc-absolute base + DVD-Video LPCM
parser) correctly resolved Bug 1 (wrong sector base) and partially
resolved Bug 2 (codec identification). The probe now correctly reads
from sector 1,703,445 and identifies LPCM 48kHz/24-bit/2ch.

However, extraction still fails because the DVD-Video LPCM sub-header
layout differs from DVD-Audio at a more fundamental level than the
previous fix addressed. The previous fix added `parse_dvd_video_pcm_sub_header()`
inside `parse_pcm_sub_header()`, but the problem is upstream: `parse_sub_header()`
itself misinterprets bytes[3] for DVD-Video packets.

## Error from latest run

```
DVD-Audio ATS 2 group 3 title 1 uses SAMG-derived disc-absolute base 1703445
  for AOB-less VOB sharing   ← CORRECT, base is now right

track realization failed: DVD-Audio LPCM sector-local commit failed at
  logical sector 1703503: DVD-Audio Private Stream 1 packet handler failed:
  DVD-Audio LPCM packet/header mismatch: LPCM packet format changed from
  (1, 48000, None, 2, 0, 24, None) to (2, 44100, None, 2, 1, 20, None)
```

The first format tuple `(1, 48000, None, 2, 0, 24, None)` is correct
(ch_assign=1/stereo, 48kHz, 2ch, 24-bit). The second `(2, 44100, None,
2, 1, 20, None)` is garbage from a misparsed sub-header.

## Root cause: bytes[3] is NOT `extra_header_length` in DVD-Video

### DVD-Audio Private Stream 1 sub-header layout

```
bytes[0] = sub_stream_id (0xA0 for LPCM, 0xA1 for MLP)
bytes[1] = cyclic counter
bytes[2] = (padding/reserved)
bytes[3] = extra_header_length (tells how many more header bytes follow)
bytes[4 .. 4+extra_header_length] = format-specific header
bytes[4+extra_header_length ..] = audio payload
```

`extra_header_length` is 9 for DVD-Audio LPCM, 6 for MLP. The demuxer
uses it to compute `total_header_length = 4 + extra_header_length` and
slices the payload accordingly.

### DVD-Video Private Stream 1 sub-header layout (VERIFIED)

```
bytes[0] = sub_stream_id (0xA0 for LPCM)
bytes[1] = number_of_frame_headers
bytes[2..3] = first_access_unit_pointer (u16 BE) ← bytes[3] is FAU low byte!
bytes[4] = emphasis(1) | mute(1) | reserved(1) | audio_frame_number(5)
bytes[5] = quantization(2) | sample_rate(2) | reserved(1) | channels(3)
bytes[6] = dynamic_range_control
bytes[7+] = LPCM audio samples (payload starts here, always)
```

**bytes[3] is the low byte of the first_access_unit_pointer**, NOT an
extra header length. It varies per sector because the FAU pointer
changes with audio frame alignment.

### VERIFIED: bytes[3] varies wildly across sectors

Sampled sectors from the Brothers in Arms stereo region:

```
Sector 1703463: bytes[3]=0x04  FAU=4     (demuxer thinks hdr=8 bytes)
Sector 1703464: bytes[3]=0x90  FAU=400   (demuxer thinks hdr=148 bytes!)
Sector 1703500: bytes[3]=0x54  FAU=340   (demuxer thinks hdr=88 bytes)
Sector 1703501: bytes[3]=0x00  FAU=256   (demuxer thinks hdr=4 bytes)
Sector 1703503: bytes[3]=0x58  FAU=88    (demuxer thinks hdr=92 bytes)
```

Full distribution across 155 sectors: values range from 0x00 to 0xF4
(0 to 244). None of these are header lengths — they're FAU pointers.

### VERIFIED: bytes[5] is ALWAYS 0x81 (format is stable)

Every sampled sector has bytes[5]=0x81:
- quantization = 10 (binary) = 24-bit
- sample_rate = 00 (binary) = 48 kHz
- channels = 001 (binary) = 2ch (stereo)

The format never changes. The "format mismatch" errors are caused by
the demuxer reading format fields from the wrong byte offsets after
miscomputing the header length.

### How the misparsing happens (sector 1703503)

```
bytes[0..8]: a0 04 00 58 03 81 80 01

Demuxer reads:
  stream_id = bytes[0] = 0xA0 (PCM) ✓
  cyclic = bytes[1] = 0x04
  extra_header_length = bytes[3] = 0x58 = 88
  total_header_length = 4 + 88 = 92
  → payload starts at byte 92 (WRONG — should be byte 7)
  → 85 bytes of audio data are consumed as "header"

Then parse_pcm_sub_header tries DVD-Audio layout:
  bytes[7] = 0x01 → group1_bits_code = 1 (20-bit)  ← GARBAGE
  bytes[8] = 0x98 → group1_sample_rate_code = 8 (44.1kHz)  ← GARBAGE
  bytes[10] = 0x02 → channel_assignment = 2  ← GARBAGE

These garbage values PASS the plausibility check because they're all
within valid DVD-Audio ranges (20-bit, 44.1kHz, ch_assign=2 are all
legal DVD-Audio values). The check cannot distinguish real DVD-Audio
format fields from arbitrary audio payload bytes that happen to look
plausible.
```

## What the previous fix got right

1. SAMG-derived disc-absolute base (1,703,445) — CORRECT
2. Probe reading from correct offset — CORRECT
3. Probe identifying LPCM — CORRECT
4. `elementary_stream_kind_hint` set to LPCM for VoB tracks — CORRECT
5. Empty-packet-list skip for non-audio VOB sectors — CORRECT
6. Removal of physical-sector-map code — CORRECT
7. DVD-Video LPCM format field parsing (bytes[5] layout) — CORRECT

## What the previous fix got wrong

The fix added `parse_dvd_video_pcm_sub_header()` inside
`parse_pcm_sub_header()`, but `parse_sub_header()` had already
misinterpreted bytes[3] as `extra_header_length` and computed a
wrong `total_header_length`. By the time `parse_pcm_sub_header()` runs:

- `total_header_length` is wrong (e.g., 92 instead of 7)
- The payload slice is wrong (starts 85 bytes too late)
- Format bytes[7..12] point into audio data, not header fields
- The plausibility check passes on random audio bytes

## What needs to change

### The sub-header parser needs a format mode

`parse_sub_header()` must know whether it's parsing a DVD-Audio or
DVD-Video Private Stream 1 packet BEFORE it reads bytes[3].

For DVD-Video LPCM:
- `total_header_length` is always 7 (fixed)
- Format fields are at bytes[4..7] (fixed positions)
- bytes[2..3] is first_access_unit_pointer (u16 BE)
- bytes[3] must NOT be interpreted as extra_header_length

### How to signal the format mode

The signal is already available in the pipeline:

1. **At materialization**: `SamgZone::Vob` on correlated SAMG tracks
   tells us the content is DVD-Video VOB format.

2. **At realization**: The `elementary_stream_kind_hint` on the track
   source ref already carries `Some(Lpcm)` for these tracks (set by
   the previous fix's `SamgSectorCorrelation::elementary_stream_kind_hint()`).

3. **Proposed**: Add a new field or extend the existing hint to carry
   a `DvdVideoVob` flag that tells the demuxer to use DVD-Video
   sub-header layout. This could be:
   a) A new variant in `DvdaElementaryStreamKind` (e.g., `DvdVideoLpcm`)
   b) A separate boolean field on `TrackSourceRef::DvdaTrack`
   c) A parameter to `parse_private_stream_1_packets()` or a separate
      DVD-Video variant of that function

### Option (a) is the cleanest

Add `DvdVideoLpcm` to `DvdaElementaryStreamKind`. When the hint is
`DvdVideoLpcm`, the demuxer uses a DVD-Video sub-header parser with
fixed 7-byte header length. When it's `Lpcm` or `Mlp`, the existing
DVD-Audio parser is used.

The materializer already sets the hint from SAMG correlation:
```rust
fn elementary_stream_kind_hint(&self) -> Option<DvdaElementaryStreamKind> {
    if self.tracks.iter().all(|track| matches!(track.zone, SamgZone::Vob)) {
        Some(DvdaElementaryStreamKind::DvdVideoLpcm)  // was: Lpcm
    } else {
        None
    }
}
```

### Changes required

**types.rs:**
- Add `DvdVideoLpcm` variant to `DvdaElementaryStreamKind`

**dvda_demux.rs:**
- Add `DvdaSubHeaderMode` enum: `DvdAudio` (default) vs `DvdVideo`
- Add `parse_private_stream_1_packets_with_mode(sector, mode)` or
  add a `mode` parameter to the existing function
- In `parse_sub_header()`, when mode is `DvdVideo`:
  - Set `total_header_length = 7` (fixed)
  - Parse bytes[4..7] as DVD-Video format fields
  - Ignore bytes[3] as a length field
- When mode is `DvdAudio`: existing behavior unchanged
- The existing `parse_private_stream_1_packets()` should default to
  `DvdAudio` mode for backwards compatibility

**dvda_realize.rs:**
- When `stream_kind_hint` is `DvdVideoLpcm`, pass `DvdVideo` mode
  to the demuxer
- The LPCM packet handler should accept `DvdVideoLpcm` hints the
  same way it accepts `Lpcm` hints

**materializer_dvda.rs:**
- `SamgSectorCorrelation::elementary_stream_kind_hint()` should
  return `DvdVideoLpcm` for VoB-zone tracks (not just `Lpcm`)

**dvda_utils.rs (probe):**
- When reading from SAMG-derived disc-absolute sectors, use `DvdVideo`
  mode for demuxing so the probe correctly parses the sub-headers

### Revert the previous `parse_pcm_sub_header` changes

The previous fix's `parse_dvd_video_pcm_sub_header()`,
`dvd_audio_pcm_sub_header_is_plausible()`, and the dispatch logic
inside `parse_pcm_sub_header()` should be reverted or simplified.
With the mode-based approach, `parse_sub_header()` handles the
format distinction, and `parse_pcm_sub_header()` only needs to
handle DVD-Audio LPCM (the DVD-Video case is handled entirely
within the DVD-Video sub-header parser).

## DVD-Video Private Stream 1 sub-header: complete specification

For reference, the complete DVD-Video LPCM sub-header (7 bytes):

```
Offset  Field                                    Bits
0       sub_stream_id                            8    (0xA0..0xA7)
1       number_of_frame_headers                  8
2..3    first_access_unit_pointer                16   (u16 BE)
4       emphasis(1)|mute(1)|reserved(1)|frame(5) 8
5       quant(2)|freq(2)|reserved(1)|channels(3) 8
6       dynamic_range_control                    8
```

Format field decoding (byte 5):
- quantization: 0=16-bit, 1=20-bit, 2=24-bit
- freq: 0=48kHz, 1=96kHz
- channels: 0=mono, 1=stereo, ... (value + 1 = channel count)

This is identical across all verified sectors. The header is always
exactly 7 bytes. Audio payload always starts at byte 7.

## Code to read

All files from the previous bundle are still relevant. The key
changes are in:

```
src/convert/pipeline/dvda_demux.rs
  408   parse_sub_header() — must branch on DVD-Audio vs DVD-Video mode
  239   parse_private_stream_1_packets() — needs mode parameter or variant
  451   parse_pcm_sub_header() — previous fix's dispatch (revert/simplify)
  483   dvd_audio_pcm_sub_header_is_plausible() — previous fix (revert)
  499   parse_dvd_audio_pcm_sub_header() — previous fix (keep, rename back)
  523   parse_dvd_video_pcm_sub_header() — previous fix (move into sub-header parser)
  551   dvd_video_lpcm_channel_assignment_for_count() — previous fix (keep)

src/convert/pipeline/types.rs
  596   DvdaElementaryStreamKind enum — add DvdVideoLpcm variant

src/convert/pipeline/materializer_dvda.rs
  850   SamgSectorCorrelation::elementary_stream_kind_hint() — return DvdVideoLpcm

src/convert/pipeline/dvda_realize.rs
  753   ExpectedElementaryStreamKind enum — needs DvdVideoLpcm variant or mapping
  774   From<DvdaElementaryStreamKind> impl — must handle DvdVideoLpcm
  1021  packet_kind_matches_hint — accept DvdVideoLpcm as LPCM
  1064  parse_track_sector_private_stream_1_packets — pass mode based on hint
  1031  packets_matching_stream_hint — accept DvdVideoLpcm

src/disc/dvda_utils.rs
  237   cross-ATS probe fallback — use DvdVideo mode when reading SAMG-derived sectors
```

## What the reasoning model should produce

1. `DvdVideoLpcm` variant added to `DvdaElementaryStreamKind`

2. DVD-Video mode in `parse_sub_header()` that uses fixed 7-byte
   header length and DVD-Video format field layout

3. Mode parameter threaded from the realization loop through to the
   demuxer, driven by the `elementary_stream_kind_hint`

4. Materializer stamps `DvdVideoLpcm` (not `Lpcm`) for VoB-correlated
   cross-ATS tracks

5. Probe uses DVD-Video mode when reading SAMG-derived sectors

6. Cleanup of the previous fix's plausibility-check approach (no
   longer needed with explicit mode signaling)

7. No behavior change for DVD-Audio LPCM or MLP tracks

## Constraints

- `parse_private_stream_1_packets()` is a public function used by
  both the probe (dvda_utils.rs) and the realizer (dvda_realize.rs).
  Adding a mode parameter changes the public API. A wrapper function
  that defaults to DVD-Audio mode preserves backwards compatibility.
- The `DvdaSubHeader` struct's `extra_header_length` and
  `total_header_length` fields are used throughout the codebase.
  For DVD-Video mode, `extra_header_length` should be set to 3
  (bytes[4..7], the format fields) and `total_header_length` to 7,
  so downstream code that uses `total_header_length` to find the
  payload start still works correctly.
- All existing tests must continue to pass — they use DVD-Audio format.
