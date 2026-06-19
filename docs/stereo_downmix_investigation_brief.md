# DVD-Audio Stereo Downmix — Investigation Brief

## Problem

Some DVD-Audio discs have a "stereo" presentation group that shares the
multichannel group's MLP bitstream. The disc browser and `disc-info`
correctly identify the codec (MLP 96kHz/24-bit) but show the wrong
channel layout ("5.1") because the MLP major sync's `channel_arrangement`
describes the full multichannel stream.

### Test case: Dire Straits — Brothers in Arms DVD-Audio

```
ATS 1: ATS_01_1.AOB through ATS_01_4.AOB (5.1 MLP)
ATS 2: ATS_02_0.IFO only, NO AOB files

AOTT[0]: group 1 → ATS 1, title 1 (9 tracks, 55:08) — 5.1
AOTT[1]: group 2 → ATS 1, title 2 (1 track, 0:02) — placeholder
AOTT[2]: group 3 → ATS 2, title 1 (9 tracks, 55:14) — "stereo"

Current disc-info output:
  Group 1: MLP 96kHz/24-bit 5.1 (9 tracks, 55:08)
  Group 3: MLP 96kHz/24-bit 5.1 (9 tracks, 55:14)

Expected:
  Group 1: MLP 96kHz/24-bit 5.1 (9 tracks, 55:08)
  Group 3: MLP 96kHz/24-bit Stereo (9 tracks, 55:14)
```

---

## What we investigated

### 1. MLP substream hypothesis — DISPROVED

Initial assumption: `MlpMajorSyncInfo.num_substreams > 1` would indicate
a stereo substream embedded in the multichannel MLP stream (substream 0
= stereo, substream 1 = multichannel extension).

**Finding:** `num_substreams == 1` on Brothers in Arms for ALL groups.
The MLP stream has only one substream. The stereo presentation is NOT
produced via MLP substream extraction.

### 2. IFO downmix matrices — ALL ZEROED

Investigated foo_input_dvda's fallback path (`set_downmix_coef()` when
`can_downmix == false`). This uses the IFO's `DownmixMatrix` entries.

**Finding:** All 14 matrices in ATS 1 and ATS 2 have every coefficient
at `raw: 0`. The `attenuation_db()` for raw=0 is `0.0 dB`. Every source
channel is sent to both L and R at unity gain with no phase inversion.
No chapters reference any downmix matrix (`downmix_matrix: None` on all
chapters).

This would produce a sum-of-all-channels result, not a standard stereo
downmix.

### 3. foo_input_dvda's actual behavior

From `mlp_audio_stream.cpp`:
```cpp
si.can_downmix = ctx->mh.num_substreams > 1;  // line 332 — false for BiA

// In init():
do_downmix = downmix;  // line 384
if (downmix) {
    if (info.can_downmix) {
        av_channel_layout_default(&...downmix_layout, 2);  // NOT taken
    } else {
        set_downmix_coef();  // TAKEN — uses IFO matrix
    }
}
```

From `audio_stream.cpp`:
```cpp
void audio_stream_t::set_downmix_coef() {
    // NO-ARGUMENT overload: uses HARDCODED standard downmix coefficients
    // (Lf=0.5, Rf=0.5, C=0.354, LFE=0.177, Ls/Rs=0.25)
    // Does NOT use IFO matrices or get_downmix_coef().
}

void audio_stream_t::downmix_channels(uint8_t* data, int* data_size) {
    // Called in decode() when do_downmix && !info.can_downmix
    // Applies the coefficient matrix to produce 2-channel output
}
```

**Key finding from the audit:** The no-argument `set_downmix_coef()`
uses hardcoded standard downmix coefficients, NOT the IFO matrices.
The IFO matrix downmix path is PCM-only — gated on
`stream_id == PCM_STREAM_ID` in `audio_track.cpp:76`. For MLP with
`num_substreams == 1`, foo_input_dvda always applies the hardcoded
coefficients to produce a standard stereo downmix from 5.1.

The zeroed IFO downmix matrices are irrelevant for MLP streams.

### 4. What's unclear

- The hardcoded downmix coefficients in `set_downmix_coef()` produce
  a standard stereo downmix. But is this the same quality as what a
  DVD-Audio hardware player would produce? Hardware players may have
  their own downmix algorithms.

- Should tonepoet replicate foo_input_dvda's hardcoded coefficients,
  or use ffmpeg's built-in downmix (`-ac 2`) which may use different
  coefficients?

- Are there other DVD-Audio discs with this same pattern (AOB-less
  ATS 2 + single-substream MLP)?

---

## What the reasoning model should investigate

### Question 1: foo_input_dvda's hardcoded downmix coefficients

Read the full `set_downmix_coef()` (no-argument overload) and
`downmix_channels()` functions in `audio_stream.cpp`. Document the
exact hardcoded coefficients. How do they compare to ATSC A/52
standard downmix? How do they compare to ffmpeg's `-ac 2` behavior?

### Question 2: IFO matrix vs hardcoded — when is each used?

The no-argument `set_downmix_coef()` (hardcoded) is used for MLP
when `can_downmix == false`. The two-argument overload (IFO matrix)
is loaded in `audio_track.cpp` but only for PCM streams
(`stream_id == PCM_STREAM_ID`). Confirm this understanding by
tracing the full code path. Are there any edge cases where IFO
matrices ARE applied to MLP streams?

### Question 3: Display label strategy

Given the findings, what should the display show for a group in an
AOB-less ATS? Options:

a. "5.1" — codec truth from the MLP major sync
b. "Stereo" — inferred from the separate-ATS authoring pattern
c. "5.1 → Stereo" — both, indicating downmix
d. "5.1 (downmix available)" — notes the authoring intent

### Question 4: Extraction strategy

When the user selects group 3 for conversion, what should tonepoet do?

a. Extract all 6 channels as 5.1 (what the codec produces)
b. Apply a standard ATSC A/52 downmix to produce stereo
c. Apply the IFO downmix matrix (even if zeroed)
d. Let ffmpeg handle it with `-ac 2`

### Question 5: Detection heuristic

What is the correct heuristic for identifying a "stereo presentation"
group? The `num_substreams > 1` test failed. Alternatives:

a. AOB-less ATS (cross_ats) alone — assumes any AOB-less group is stereo
b. AOB-less ATS + channel_arrangement > 2 channels — the group's MLP
   shows multichannel but it's in a separate ATS with no AOBs
c. Check whether the group's AOTT entry has different `ats_category`
   or other metadata distinguishing it
d. Check the SAMG tracks for the placeholder group (group 2 had 9
   SAMG tracks with their own channel format info)

---

## Reference code to read

```
foo_input_dvda (at /tmp/foo_input_dvda/src/foo_input_dvda/):
  audio_stream.cpp    — set_downmix_coef(), downmix_channels()
  audio_stream.h      — do_downmix flag, downmix coefficients
  mlp_audio_stream.cpp — can_downmix, init(), decode()
  dvda_zone.cpp       — get_downmix_coef()
  dvda_zone.h         — dvda_downmix_matrix_t
  dvda_core.cpp       — how downmix is triggered at playback level

tonepoet:
  src/disc/dvda_utils.rs        — AobProbeResult with channel_label, cross_ats detection
  src/disc/dvda_mapper.rs       — probe.channel_label consumption
  src/convert/pipeline/dvda_mlp.rs — MlpMajorSyncInfo.num_substreams
  crates/dvda-demuxer/src/tui/dvda/model.rs — DownmixMatrix, DownmixCoefficient
```

---

## Brothers in Arms debug data

```
AOTT entries:
  AOTT[0]: ordinal=1, ts_nr=1, title_nr=1, tracks=9, duration=3308.1s
  AOTT[1]: ordinal=2, ts_nr=1, title_nr=2, tracks=1, duration=2.0s
  AOTT[2]: ordinal=3, ts_nr=2, title_nr=1, tracks=9, duration=3313.7s

Title sets:
  ATS 1: 2 titles, 1 present format, aobs=["ATS_01_1..4.AOB"]
  ATS 2: 1 title, 0 present formats, aobs=[]

MLP major sync (all groups): num_substreams=1, channel_arrangement=12 (5.1)

Downmix matrices: all 14 × 8ch zeroed (raw=0, 0.0 dB, no phase inversion)
Chapter downmix refs: all None
```
