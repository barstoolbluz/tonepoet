# DVD-Audio Post-Encode Sample Drift Issue

## Problem

DVD-Audio MLP extraction works end-to-end, but the post-encode lossless
validator rejects some tracks due to sample count drift between the
PTS-derived expected count and the actual decoded output.

## Observed behavior (HDAD2009.ISO, group 1)

Track 2: expected=82,080,000, actual=82,080,000, drift=0 → **PASS**
Track 1: expected=126,912,000, actual=126,911,360, drift=640 → **DVD-A realizer PASS** (within 192,000 tolerance), **post-encode validator FAIL** (allowed=0)

## Error message

```
post-encode sample drift for lossless output
/tmp/dvda-test/.tonepoet-staging/.../001-01 - Track 01.flac:
expected 126912000, got 126911360, allowed 0
```

## Root cause chain

### 1. Expected samples are PTS-derived (imprecise for MLP)

`expected_samples_from_pts_len()` in `materializer_dvda.rs:1286`:

```rust
fn expected_samples_from_pts_len(len_in_pts: u32, sample_rate: u32) -> Option<u64> {
    let numerator = u64::from(len_in_pts) * u64::from(sample_rate);
    if numerator % PTS_PER_SECOND == 0 {
        Some(numerator / PTS_PER_SECOND)
    } else {
        None  // returns None if PTS doesn't divide evenly
    }
}
```

Track 1: `59490000 * 192000 / 90000 = 126,912,000` (divides evenly → `Some`)

### 2. MLP decoded output has fewer samples

The MLP codec operates on fixed-size access units. The actual decoded
sample count depends on how the MLP encoder packed frames, not on the
PTS duration from the IFO. A 640-sample drift at 192kHz is ~3.3ms — well
within normal MLP frame boundary rounding.

### 3. DVD-A realizer accepts drift (Phase 3 tolerance = 1 second)

`dvda_realize.rs:2326`: `Phase3Tolerance => sample_rate as u64` = 192,000
samples tolerance. 640 < 192,000 → PASS. Logs a warning.

### 4. Post-encode validator uses zero tolerance for lossless

`stages.rs:922`: `encoded_output_sample_tolerance()` returns 0 when
`probe.exact` is true (which it is for FLAC). The validator gets its
`expected_samples` from `PreparedTrack.expected_samples` — the PTS-derived
value from step 1.

So the DVD-A realizer says "640 drift is fine" but then the downstream
lossless encoder validator says "0 drift allowed" using the same
PTS-derived expected count.

## The conflict

Two validators with different tolerances:
- DVD-A realizer: PTS estimate ± 1 second → PASS
- Post-encode lossless: exact match required → FAIL

The post-encode validator was designed for CUE/SACD/7z sources where
expected sample counts are precise (from CUE sheet indices, SACD TOC,
or ffprobe). For DVD-Audio, the expected count is derived from PTS
timestamps which are imprecise for MLP-encoded content.

## What needs to happen

The post-encode validator needs to know that DVD-Audio tracks have
imprecise expected sample counts so it can apply appropriate tolerance.
Options include:

1. **Use actual decoded samples as the reference**: After the DVD-A
   realizer produces a WAV, update `PreparedTrack.expected_samples`
   (or the equivalent passed to the validator) to the actual decoded
   count instead of the PTS estimate.

2. **Apply source-kind-aware tolerance**: When the source is DVD-Audio,
   allow the same tolerance the realizer uses (1 second) instead of 0.

3. **Mark expected_samples as imprecise**: Add a field like
   `expected_samples_exact: bool` to PreparedTrack. When false, the
   post-encode validator uses a tolerance instead of exact match.

4. **Skip post-encode validation for DVD-Audio**: Since the realizer
   already validates duration, the downstream check is redundant.

## Relevant code locations

- `materializer_dvda.rs:1286` — `expected_samples_from_pts_len()`
- `stages.rs:1995` — passes `track.expected_samples` to post-encode validator
- `stages.rs:870` — `validate_encoded_output_with_tool_limits()`
- `stages.rs:922` — `encoded_output_sample_tolerance()` returns 0 for exact
- `dvda_realize.rs:2326` — Phase3 tolerance = sample_rate (1 second)
- `dvda_realize.rs:2180` — DVD-A WAV sample drift validation (passes)

## Context for the reasoning model

The SACD materializer avoids this problem by setting `expected_samples: None`
(materializer_sacd.rs:104):

```rust
// The encoded artifact will usually be PCM FLAC/MP3/AAC/etc.,
// not DSD64. Leave this unset so merge validation probes the
// real encoded output instead of comparing against 2.8224 MHz
// SACD source-domain sample counts.
expected_samples: None,
```

When `expected_samples` is None, the post-encode validator skips
(stages.rs:876-878):

```rust
let Some(expected) = expected_samples else {
    return Ok(None);
};
```

The DVD-Audio materializer could follow the same pattern — set
`expected_samples: None` for MLP tracks where the count is PTS-derived
and imprecise. Or it could set `expected_samples` to the actual decoded
count after realization, which would make the post-encode check exact
against the real data.
