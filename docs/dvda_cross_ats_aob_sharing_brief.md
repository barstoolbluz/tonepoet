# DVD-Audio Cross-ATS Extraction — Verified Root Cause and Fix

## Status

Four reasoning-model attempts have failed because every one assumed
the wrong disc-absolute sector base AND the wrong codec. This brief
provides empirically verified disc forensics from hex-level ISO
inspection.

## Test case: Dire Straits — Brothers in Arms DVD-Audio ISO

### File layout on disc (from isoinfo + hex inspection)

```
Sector 1,904:      AUDIO_TS.IFO (AMG)
Sector 27,835:     ATS_01_0.IFO
Sector 27,837:     ATS_01_1.AOB  (1 GB)      ┐
Sector 552,109:    ATS_01_2.AOB  (1 GB)       │ ATS 1: MLP sub-stream 0xA1
Sector 1,076,381:  ATS_01_3.AOB  (1 GB)       │ 5.1 multichannel, 96 kHz / 24-bit
Sector 1,600,653:  ATS_01_4.AOB  (164 MB)     ┘ 1,657,117 sectors = 3.2 GB
Sector 1,684,955:  ATS_01_0.BUP
Sector 1,684,957:  ATS_02_0.IFO  (19 sectors total — NO AOBs)
Sector 1,684,974:  ATS_02_0.BUP
Sectors ~1,685,000..1,703,444: DVDAUDIO.MKB + other non-audio data
Sector 1,703,445:  ┐ LPCM sub-stream 0xA0, DVD-Video VOB format
  ...              │ 2ch stereo, 48 kHz / 24-bit
Sector 2,186,446:  ┘ 483,002 sectors = 976 MB
Sector 2,285,442:  End of ISO
```

### CRITICAL: ISO 9660 and UDF disagree on AUDIO_TS.IFO content

This disc has both ISO 9660 and UDF filesystems. Our code uses UDF
(`IsoUdfDvdaVolume`). The UDF AUDIO_TS.IFO has DIFFERENT AOTT data:

**UDF AUDIO_TS.IFO AOTT (what our code reads):**
```
AOTT[0] ordinal=1 pb=0x81 is_audio=true  ts=1 tn=1 (multichannel)
AOTT[1] ordinal=2 pb=0x81 is_audio=true  ts=1 tn=2 (placeholder)
AOTT[2] ordinal=3 pb=0x82 is_audio=true  ts=2 tn=1 atsi_mat=1683053
AOTT[3] ordinal=4 pb=0x00 is_audio=false ts=0 tn=0
```

### Groups created by parser

```
Group 1: AOTT[0] → title_ref(ts=1, t=1) → ATS 1 title 1 (9 ch, 5.1 MLP)
Group 2: AOTT[1] → title_ref(ts=1, t=2) → ATS 1 title 2 (1 ch, 2s placeholder)
          + 9 SAMG tracks (group_nr=2) merged in
Group 3: AOTT[2] → title_ref(ts=2, t=1) → ATS 2 title 1 (9 ch, stereo LPCM)
```

Group 2 is suppressed as placeholder (title_ref resolves to 1-track,
2-second ATS 1 title 2). Group 3 is what the user selects.

### ATS 2 IFO data

```
ats_last_sector:  18      (ATS 2 is only 19 sectors on disc — NO audio here)
atstt_vobs:       77
ats_pgcit:        1 title, 9 chapters, sector ranges 0..483001
```

### SAMG data (AUDIO_PP.IFO)

```
9 tracks, all group_nr=2, zone=VoB
Track 1: abs_first=1703445  abs_last=1748725
Track 2: abs_first=1748726  abs_last=1822597
Track 3: abs_first=1822598  abs_last=1859375
Track 4: abs_first=1859376  abs_last=1916793
Track 5: abs_first=1916794  abs_last=1991297
Track 6: abs_first=1991298  abs_last=2052212
Track 7: abs_first=2052213  abs_last=2093087
Track 8: abs_first=2093088  abs_last=2125169
Track 9: abs_first=2125170  abs_last=2186446
```

### VERIFIED: ATS 2 sector ranges + 1,703,445 = SAMG absolute sectors

Every track, every sector count — perfect match with constant offset:

```
Track | ATS 2 (IFO)       | SAMG absolute           | Offset
1     | 0..45280           | 1703445..1748725        | 1,703,445
2     | 45281..119152      | 1748726..1822597        | 1,703,445
3     | 119153..155930     | 1822598..1859375        | 1,703,445
4     | 155931..213348     | 1859376..1916793        | 1,703,445
5     | 213349..287852     | 1916794..1991297        | 1,703,445
6     | 287853..348767     | 1991298..2052212        | 1,703,445
7     | 348768..389642     | 2052213..2093087        | 1,703,445
8     | 389643..421724     | 2093088..2125169        | 1,703,445
9     | 421725..483001     | 2125170..2186446        | 1,703,445
```

### VERIFIED: `atsi_mat_sector + atstt_vobs` gives the WRONG offset

```
UDF AOTT[2] atsi_mat_sector: 1,683,053
ATS 2 atstt_vobs: 77
Current disc_absolute_base: 1,683,053 + 77 = 1,683,130  ← WRONG

Correct disc-absolute base: 1,703,445
Error: 20,315 sectors off
```

The `atsi_mat_sector` field is relative to the AMG (AUDIO_TS.IFO)
disc sector, NOT disc-absolute:
```
ATS 1: atsi_mat=25,931 + AMG(1,904) = 27,835 = ATS_01_0.IFO disc sector ✓
ATS 2: atsi_mat=1,683,053 + AMG(1,904) = 1,684,957 = ATS_02_0.IFO disc sector ✓
```
But `atstt_vobs=77` for an AOB-less ATS does NOT point to audio data.

### VERIFIED: Actual content at each location

**Probe reads sector 1,683,130** (the wrong base, inside ATS 1 AOBs):
- Contains MPEG-PS with Private Stream 1, sub-stream 0xA1 = **MLP**
- Probe incorrectly reports: "MLP 96kHz/24-bit"

**SAMG absolute sectors 1,703,445..2,186,446** (the actual stereo data):
- Contains DVD-Video VOB format:
  - Stream ID 0xBB: MPEG System Headers
  - Stream ID 0xE0: MPEG Video stream
  - Stream ID 0xBE: Padding
  - Stream ID 0xBF: Navigation (PCI/DSI)
  - Private Stream 1 sub-stream 0xA0: **LPCM** audio
- LPCM header (DVD-Video format, `extra_header_length=4`):
  - `quantization: 24-bit`
  - `sample_rate: 48 kHz`
  - `channels: 2ch (stereo)`
- This is NOT DVD-Audio LPCM format. DVD-Video LPCM uses
  `extra_header_length=4` (our `PCM_EXTRA_HEADER_LENGTH=9` is
  DVD-Audio format). When `extra_header_length < 9`, the current
  code at line 431-435 skips PCM header parsing entirely — returning
  `(None, None)` for `(cci, pcm)` — so the LPCM format metadata
  is silently lost.

**ATS 1 AOBs** (the multichannel data):
- Contains MPEG-PS with Private Stream 1, sub-stream 0xA1 = **MLP**
- Pure DVD-Audio AOB format (no video, no nav packs)

### VERIFIED: Size ratio confirms separate encoding

```
ATS 1 multichannel: 1,657,117 sectors = 3.2 GB (6ch MLP 96/24)
SAMG stereo:          483,002 sectors = 976 MB (2ch LPCM 48/24)
Ratio: 3.43x (consistent with 6ch vs 2ch + lossless-vs-PCM overhead)
```

The stereo data is independently encoded LPCM at a lower sample rate
(48 kHz vs 96 kHz). Not an MLP substream. Not a downmix instruction.
Not a copy of ATS 1.

## Root cause (two bugs)

### Bug 1: Wrong disc-absolute base

`title_disc_absolute_sector_base()` computes `atsi_mat_sector +
atstt_vobs = 1,683,130`. This is wrong by 20,315 sectors. The correct
base is 1,703,445, derivable from SAMG evidence:

```
correct_base = SAMG_track1.abs_first_sector - ATS2_chapter1.first_sector
             = 1,703,445 - 0
             = 1,703,445
```

### Bug 2: Wrong codec identification

The probe reads from the wrong offset (1,683,130 → ATS 1 MLP) and
reports "MLP 96kHz/24-bit". The actual content is LPCM 48kHz/24-bit
in DVD-Video VOB format with a 4-byte extra header (not the 9-byte
DVD-Audio LPCM header that our demuxer expects).

The demuxer currently:
- Checks `extra_header_length >= PCM_EXTRA_HEADER_LENGTH` (9) before
  parsing the DVD-Audio LPCM sub-header
- DVD-Video LPCM packets have `extra_header_length = 5`
- These would be rejected or mishandled by the current parser

Additionally, the DVD-Video VOB sectors contain non-audio PES packets
(video 0xE0, nav 0xBF, system 0xBB, padding 0xBE) that the DVD-Audio
demuxer does not expect in AOB data.

## What needs to change

### 1. Fix disc-absolute base computation

In `materializer_dvda.rs`, `title_disc_absolute_sector_base()` must
use SAMG evidence when available. The materializer has access to
`disc.samg` and all groups. For an AOB-less ATS where correlated SAMG
tracks exist, compute:

```
base = SAMG_first_track.abs_first_sector - ATS_first_chapter.first_sector
```

The SAMG tracks live on a different group (group 2 has samg_tracks,
group 3 has the ATS title_ref). To find them, search `disc.samg.tracks`
for tracks whose `group_nr` matches a group that has SAMG tracks with
matching track count and zone=VoB. Or more directly: look for SAMG
tracks whose sector counts match the ATS chapter sector counts.

### 2. Fix the probe to read from correct offset

In `dvda_utils.rs`, `probe_group_aob_format_with_path()` must use the
SAMG-derived base instead of `atsi_mat_sector + atstt_vobs` for the
cross-ATS fallback. This will cause the probe to read LPCM sectors
instead of MLP, which will correctly identify the codec.

### 3. Handle DVD-Video LPCM format in the demuxer

In `dvda_demux.rs`, the demuxer needs to handle DVD-Video LPCM
packets. These have a different sub-header layout from DVD-Audio LPCM.
All byte indices below are relative to the sub-header start (what
`parse_sub_header()` sees in `bytes[]`):

**DVD-Audio LPCM sub-header** (our current `parse_pcm_sub_header`):
```
bytes[0]  = sub_stream_id (0xA0)
bytes[1]  = cyclic counter
bytes[2]  = unknown
bytes[3]  = extra_header_length (9 for DVD-Audio)
bytes[4..5] = first_audio_frame (u16 BE)
bytes[6]  = unknown
bytes[7]  = group2_bits(4) | group1_bits(4)
bytes[8]  = group2_sample_rate(4) | group1_sample_rate(4)
bytes[9]  = unknown
bytes[10] = channel_assignment
bytes[11] = unknown
bytes[12] = cci
```

**DVD-Video LPCM sub-header** (Brothers in Arms stereo, extra_len=4):
```
bytes[0]  = sub_stream_id (0xA0)
bytes[1]  = cyclic counter
bytes[2]  = unknown
bytes[3]  = extra_header_length (4 in this case; 136 on other discs)
bytes[4]  = emphasis(1) | mute(1) | reserved(1) | audio_frame_number(5)
bytes[5]  = quantization(2) | sample_rate(2) | reserved(1) | channels(3)
bytes[6]  = dynamic_range_control
bytes[7]  = (may not exist if extra_header_length is short)
```

The format fields are at completely different offsets. The demuxer
must detect which format is in use and branch accordingly.

Options:
a) Add a `parse_dvd_video_pcm_sub_header()` variant
b) Detect the header format from `extra_header_length` and branch
c) For the SamgAbsolute/DiscAbsolute+VoB path, use a different
   packet handler that understands DVD-Video VOB structure

### 4. Handle audio-less VOB sectors gracefully

All 483,002 sectors in the SAMG range start with valid MPEG-PS pack
headers (verified: 1001/1001 sampled). The existing
`parse_private_stream_1_packets` already skips non-Private-Stream-1
PES packets (video 0xE0, nav 0xBF, system 0xBB, padding 0xBE) — it
only collects `stream_id == 0xBD` packets and ignores others.

However, ~2% of sectors contain ONLY non-audio PES data (no 0xBD
packet at all). `parse_private_stream_1_packets` returns an empty
`Vec` for these. The realization loop must treat empty packet lists
from VOB sectors as normal (skip the sector) rather than as an error.
The existing `disc_absolute_hint_allows_sparse_raw_sector` non-audio
skip logic handles `MissingPackHeader` errors but NOT empty packet
lists from valid sectors — this may need adjustment.

### 5. Remove the physical-sector-map approach

`build_disc_absolute_audio_sector_map()` and
`disc_absolute_sector_contains_hinted_audio()` were built for the
previous (incorrect) hypothesis. With the corrected SAMG base, the
sector ranges directly map to disc-absolute LBAs. No physical-sector
scanning is needed.

### 6. Fix the stream kind hint

The materialized `elementary_stream_kind_hint` is set to MLP (from
the broken probe). With the corrected probe, it should be LPCM. Or
better: derive the hint from the SAMG evidence (SAMG zone=VoB + LPCM
sub-header) rather than from the probe.

## Code to read

```
src/convert/pipeline/materializer_dvda.rs (3949 lines)
  ~841  title_disc_absolute_sector_base() — WRONG formula (Bug 1)
  ~869  DiscAbsoluteAudioSectorMap — remove or gate
  ~880  build_disc_absolute_audio_sector_map() — remove or gate
  ~1032 sector_ranges_for_address_space() — applies base to ranges
  ~1244 append_tracks_for_group() — title_refs vs samg_tracks dispatch
  ~1300 append_title_tracks() — where DiscAbsolute is set
  ~1506 prepared_track_from_samg_track() — existing SAMG path (reference)

src/convert/pipeline/dvda_demux.rs (1213 lines)
  22    PCM_EXTRA_HEADER_LENGTH = 9 — DVD-Audio format
  25    DvdaSubstreamKind enum
  43    DvdaPcmSubHeader struct
  238   parse_private_stream_1_packets() — entry point
  285   parse_private_stream_1_packet() — individual packet parse
  430   PCM sub-header dispatch
  451   parse_pcm_sub_header() — DVD-Audio layout, needs DVD-Video variant

src/convert/pipeline/dvda_realize.rs (5724 lines)
  ~879  TrackSectorReader enum — DiscAbsoluteIso variant
  ~943  read_blocks_into() — sector read dispatch
  ~960  read_disc_absolute_blocks_from_iso() — raw ISO reads
  ~1059 disc_absolute_hint_allows_sparse_raw_sector() — non-audio skip

src/convert/pipeline/dvda_mlp.rs (991 lines)
  MLP stream validation — may need changes for LPCM path

src/convert/pipeline/types.rs (1535 lines)
  ~601  DvdaSectorAddressSpace enum (AtsAobRelative, DiscAbsolute, SamgAbsolute)
  ~432  TrackSourceRef::DvdaTrack fields

src/disc/dvda_utils.rs (560 lines)
  ~110  probe_group_aob_format_with_path() — cross-ATS probe fallback (Bug 2)
  ~166  disc_lba computation — same wrong formula

crates/dvda-phase1/src/tui/dvda/model.rs (614 lines)
  ~373  SamgInfo, SamgTrack — abs_first_sector field
  ~420  DvdaGroup — title_refs + samg_tracks
  ~445  SamgTrackRef

src/convert/pipeline/mod.rs (187 lines)
  Module re-exports
```

## What the reasoning model should produce

1. **Fixed `title_disc_absolute_sector_base()`** that computes the
   correct disc-absolute base from SAMG evidence when SAMG tracks
   exist for a correlated group with matching track count.

2. **Fixed probe** in `probe_group_aob_format_with_path()` that uses
   the SAMG-derived base for cross-ATS reads, correctly identifying
   LPCM instead of MLP.

3. **DVD-Video LPCM sub-header parsing** in `dvda_demux.rs` — either
   a separate parser function or detection based on
   `extra_header_length` to handle DVD-Video LPCM format variants.

4. **Empty-packet-list handling** in the realization loop for VOB
   sectors that contain only video/nav PES packets (no 0xBD). The
   existing parser already skips non-0xBD PES packets, but ~2% of
   sectors produce an empty packet list that must not be treated as
   an error.

5. **Removal/gating of physical-sector-map code** — no longer needed
   once the correct base is computed.

6. **Correct `elementary_stream_kind_hint`** for cross-ATS LPCM tracks.

7. **No behavior change** for normal ATS-relative tracks (those with
   their own AOB files and MLP content).

## Additional context: DVD-Video LPCM is a general gap

The DVD-Video LPCM format issue is not unique to cross-ATS. A pure
DVD-Video ISO (Neil Young — Live at the Fillmore East, 1970) contains
LPCM 24-bit/96kHz/2ch with `extra_header_length=136` (0x88). This is
LARGER than `PCM_EXTRA_HEADER_LENGTH` (9), so `parse_pcm_sub_header()`
IS called — but it interprets the DVD-Video LPCM layout as DVD-Audio
LPCM layout, reading bytes[7..12] for format fields that are at
completely different offsets in the DVD-Video format. This produces
garbage sample rates, bit depths, and channel assignments.

The Brothers in Arms stereo has `extra_header_length=4`, which causes
the opposite problem: `parse_pcm_sub_header()` is NOT called, so
format metadata is silently lost.

Both are DVD-Video LPCM. The demuxer must distinguish DVD-Audio from
DVD-Video LPCM sub-headers. The format fields (quantization, sample
rate, channels) live at different byte offsets:
- DVD-Audio: bytes[7] = bits, bytes[8] = rates, bytes[10] = ch_assign
- DVD-Video: bytes[5] = quant(2)|sr(2)|reserved(1)|channels(3)

The distinguishing signal could be the SAMG zone (VoB vs Aob), the
sector address space, or the `extra_header_length` value pattern.

## Constraints

- The materializer has access to `disc: &DvdaDisc` which contains
  `disc.samg: Option<SamgInfo>` with all SAMG tracks
- `SamgTrack.abs_first_sector` and `abs_last_sector` are disc-absolute
  LBAs (verified empirically)
- The SAMG tracks for the stereo group are on `group_nr=2`, while the
  ATS title_ref group is `group_nr=3` — they have different group numbers
- `SamgTrack.zone = SamgZone::Vob` for these tracks (DVD-Video VOBs)
- The existing `SamgAbsolute` path in the materializer already handles
  SAMG-only groups correctly — the fix should follow a similar pattern
  or reuse that path when SAMG evidence overrides ATS addressing
