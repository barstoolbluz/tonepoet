# DVD-Audio Cross-ATS Stereo: Use Group 1 Chapter Boundaries

## Problem

On the Bowie David Live DVD-A, our stereo extraction uses Group 2's
(ATS 2) chapter boundaries, producing 22 tracks totaling 83.2
minutes. foobar2000's foo_input_dvda produces 21 tracks totaling
~102 minutes. The tracks are different lengths and start at
different points.

Our track 1 is 163.1s. Foobar's track 1 ("1984") is 202.3s.
The cumulative offset grows from 39s at track 2 to 18+ minutes
by track 21.

## Root cause

ATS 2 (Group 2, "LPCM Stereo") has its own PGCIT with chapter
boundaries that differ from ATS 1 (Group 1, MLP 5.1). ATS 2 has
22 chapters; ATS 1 has 21 (matching the 21 songs on the album).
The chapter boundaries don't align — ATS 2's chapters are shorter
and cover a different portion of the MLP bitstream. (Note: the
exact number of tracks in ATS 1 needs verification from the IFO;
foobar's CUE shows 21, and earlier briefs said "10 tracks" for
Group 1, which may have been from a different title or group
selection. The reasoning model should verify ATS 1's chapter
structure directly.)

We use ATS 2's chapter boundaries because Group 2 is the group
the materializer selected for stereo extraction. This is wrong.

## How foobar2000 gets it right

Analyzed the foo_input_dvda source at:
`/tmp/foo_input_dvda/src/foo_input_dvda/`

### The mechanism (audio_track.cpp lines 51-107)

foobar2000's track enumeration works in two passes:

```
Pass 1: track_list.init(zone, downmix=false, ...)
  - Iterates ALL titlesets (ATS 1, ATS 2, ...)
  - For each track, calls get_audio_stream_info() which reads
    actual audio blocks from the AOB files
  - ATS 2 tracks are SILENTLY SKIPPED because ATS 2 has no AOB
    files — get_audio_stream_info() fails and the track is not
    added to the track list

Pass 2: track_list.init(zone, downmix=true, ...)
  - Same iteration over all titlesets
  - ATS 1's MLP tracks have can_downmix=true (because MLP has
    num_substreams > 1, i.e., stereo substream 0)
  - Creates DOWNMIX VERSIONS of ATS 1's tracks using ATS 1's
    chapter boundaries
  - These are the stereo tracks the user sees
```

### Key code (audio_track.cpp line 68)

```cpp
if (!(audio_track.duration < threshold_time) &&
    get_audio_stream_info(dvda_zone, ts, audio_track.block_first,
                          audio_track.audio_stream_info))
```

`get_audio_stream_info` reads from AOB blocks. For ATS 2 (no
AOBs), `dvda_titleset_t::get_blocks()` returns
`DVDAERR_AOB_BLOCK_NOT_FOUND` (dvda_zone.cpp line 271). The
function returns false, and the track is never added.

### Key code (mlp_audio_stream.cpp line 332)

```cpp
si.can_downmix = ctx->mh.num_substreams > 1;
```

MLP streams with multiple substreams support downmix. The Bowie
disc's MLP stream has stereo substream 0 + multichannel extension,
so `can_downmix = true`.

### The result

foobar's stereo presentation uses:
- **ATS 1's chapter boundaries** (21 tracks, correct timing)
- **ATS 1's sector ranges** (the actual AOB data)
- **MLP substream 0 extraction** for stereo (the native stereo
  substream embedded in the MLP bitstream)
- NOT ATS 2's chapter table at all

## What our code should do

For cross-ATS stereo presentations where ATS 2 has no AOBs:

**Do not use ATS 2's chapter boundaries for track splitting.**

Instead, use ATS 1's (the backing group's) chapter boundaries
and apply stereo downmix. This means:

1. When a group is identified as cross-ATS with no AOBs (the
   current identity sector translation case), use the BACKING
   GROUP's track/chapter structure, not the cross-ATS group's.

2. The sector ranges should come from Group 1's PGCIT, not
   Group 2's. Group 1 has 21 tracks with correct chapter points
   that align with the album's song boundaries.

3. The stereo downmix (foo_input_dvda_compatible pan filter or
   MLP substream 0 extraction) is applied during decode, same
   as now.

4. The PTS timing and expected sample counts come from Group 1's
   chapters, which are valid for MLP because Group 1 IS MLP.
   This eliminates the PTS mismatch that required `len_in_pts=0`.

## Second difference: stereo extraction method

foo_input_dvda uses a different stereo extraction path than we do
for MLP with multiple substreams (`can_downmix = true`).

### foo_input_dvda (mlp_audio_stream.cpp line 387)

When `can_downmix` is true, it sets ffmpeg's internal MLP decoder
`downmix_layout` to stereo:

```cpp
av_channel_layout_default(
    &((mlp_dc_t*)ctx->codecCtx->priv_data)->downmix_layout, 2);
```

This tells ffmpeg's MLP decoder to extract **substream 0 only**
— the native authored stereo mix embedded in the MLP bitstream.
The disc author created this stereo mix during mastering; it is
NOT a mechanical fold-down of the 5.1 channels.

### Our code

We decode all MLP substreams to 5.1 multichannel PCM, then apply
a pan filter to fold down to stereo:

```
pan=stereo|FL=0.500*FL+0.354*FC+0.177*LFE+0.250*BL|
           FR=0.500*FR+0.354*FC+0.177*LFE+0.250*BR
```

This produces a synthetic stereo downmix from the surround
channels, not the authored stereo mix.

### foo_input_dvda's fallback (audio_stream.cpp lines 90-108)

When `can_downmix` is false (PCM tracks or MLP without multiple
substreams), foo_input_dvda falls back to a coefficient-based
downmix using exactly the same coefficients as our pan filter:

```
L = 0.500*Lf + 0.354*C + 0.177*LFE + 0.250*Ls
R = 0.500*Rf + 0.354*C + 0.177*LFE + 0.250*Rs
```

Or it uses IFO-authored downmix matrices from the ATSI block if
present (audio_track.cpp lines 77-84).

### Impact on this disc

The Bowie disc's MLP stream has `num_substreams > 1`, so
`can_downmix = true`. foobar extracts substream 0 (authored
stereo). We decode 5.1 and fold down. The audio content will
differ — potentially significantly, depending on how the disc
was mastered.

The correct behavior for foo_input_dvda compatibility is to
extract MLP substream 0 when available, not to pan-filter the
5.1 decode. The pan filter should be the fallback for streams
without a native stereo substream.

## Implications

This is a fundamental change to how cross-ATS stereo groups are
materialized. The current approach — "use Group 2's IFO but read
Group 1's AOB data" — is half right. It correctly reads from
Group 1's AOBs but uses the wrong chapter boundaries.

The correct approach is: "use Group 1's chapter boundaries AND
Group 1's AOB data, with stereo downmix applied during decode."
Group 2's IFO contributes only the DETECTION that a stereo
downmix is desired (its codec claim of LPCM stereo signals that
the authored intent is a 2-channel presentation).

## Comparison data

```
Track  Foobar Title                      Foobar Dur   Our Dur   Delta
  1    1984                                202.3s     163.1s   -39.2s
  2    Rebel Rebel                         162.1s     127.6s   -34.5s
  3    Moonage Daydream                    308.2s     244.3s   -63.9s
  4    Sweet Thing/Candidate/Reprise       515.0s     426.1s   -88.9s
  5    Changes                             215.7s     178.5s   -37.2s
  ...  (all tracks shorter, growing delta)
 21    Rock 'N' Roll Suicide                  ?       241.3s      ?
 22    (none in foobar)                       -         0.4s      -
```

Total: foobar ~102 min, ours 83.2 min, difference ~19 min.
We have 22 tracks; foobar has 21.

## Code locations

```
src/convert/pipeline/materializer_dvda.rs
  Cross-ATS AOB resolution and group selection
  sector_ranges_for_translation()
  This is where the chapter boundaries are applied

src/convert/pipeline/dvda_realize.rs
  extract_track_audio_payload() — reads sectors per track
  realize_dvda_track() — orchestrates decode

foo_input_dvda reference:
  /tmp/foo_input_dvda/src/foo_input_dvda/audio_track.cpp
    Lines 51-107: track_list.init() — two-pass enumeration
    Line 68: get_audio_stream_info() gate (skips no-AOB tracks)
  /tmp/foo_input_dvda/src/foo_input_dvda/dvda_zone.cpp
    Lines 134-150: AOB file opening (null for missing AOBs)
    Line 271: DVDAERR_AOB_BLOCK_NOT_FOUND for missing AOBs
  /tmp/foo_input_dvda/src/foo_input_dvda/mlp_audio_stream.cpp
    Line 332: can_downmix = num_substreams > 1
