# DR Meter Implementation — Investigation Notes

## Current Status

DR values are ±1-2 of foobar2000's foo_dr_meter. For an album where foobar reports DR14 for all tracks, we get 15, 16, 14 across three tracks. Acceptable for display but not reference-grade.

## Confirmed Facts

1. **Sample format is I32(Packed)** for 88.2kHz FLAC files (confirmed at runtime). The FLAC decoder outputs packed interleaved S32, not planar S32P.

2. **`decoded.samples()` (= `nb_samples`) is per-channel.** FFmpeg docs confirm this. For stereo packed I32, the interleaved buffer has `nb_samples × 2` i32 values.

3. **`Audio::plane::<T>(0)` truncates packed data.** ffmpeg-next's `plane()` returns a slice of length `nb_samples`, but for packed stereo the buffer actually contains `nb_samples × channels` interleaved values. After de-interleaving with `step_by(channels)`, each channel gets only `nb_samples / channels` samples — **half the audio is lost**.

4. **The ×2 AES-17 RMS factor is used by dr14_t.meter** (`sqrt(2 × Σs² / N)`). This is confirmed in their source code. They also provide a standard RMS function `u_rms()` without ×2, confirming the factor is deliberate.

5. **dr14_t.meter resamples all audio to 44.1kHz 16-bit** before analysis (`ffmpeg -ar 44100`). At 44.1kHz, the FLAC decoder likely outputs S32P (planar), avoiding the packed truncation issue entirely.

## The Interaction

The current code has two compensating behaviors:

| Component | Behavior | Effect on RMS |
|-----------|----------|---------------|
| `plane()` truncation | Each block accumulates `N/2` actual samples but `block_sample_count` advances by `N` | RMS denominator is 2× too large → RMS -3 dB |
| AES-17 ×2 factor | `sqrt(2 × sum / N)` | RMS +3 dB |
| **Net** | **-3 + 3 = 0** | **Standard RMS** |

This accidental cancellation gives results that are ±1-2 of foobar.

### Tested Configurations (tracks 1-3, foobar DR14 for all)

| ×2 factor | Packed fix (`data(0)`) | Results | Notes |
|-----------|----------------------|---------|-------|
| Yes | No (half data) | 15, 16, 14 | ×2 and half-data cancel → standard RMS |
| Yes | Yes (full data) | 12, 13, 11 | Full AES-17 RMS, ~3 dB too low |
| No | Yes (full data) | 15, 16, 14 | Standard RMS with correct data |
| No | No (half data) | 18, ?, ? | No compensation at all, way too high |

**Conclusion**: Standard RMS (no ×2) with correct data gives the same result as ×2 with half data. Both produce ±1-2 of foobar. The remaining variance comes from frame-level block overflow.

## Current Code State

The code uses **×2 with half data** (the accidentally-correct configuration). This is kept because:
- It gives ±1-2 accuracy without unsafe code
- The `data(0)` packed fix requires `unsafe` pointer reinterpretation
- Switching to `data(0)` + no ×2 gives identical results but adds complexity

## Root Cause of ±1-2 Variance

When a decoded audio frame (~4096 samples) crosses a 3-second block boundary (~132K-264K samples), all the frame's energy goes into the current block. The overflow samples' energy is counted in the wrong block:
- Block N: slightly too much energy → RMS slightly high
- Block N+1: missing that energy → RMS slightly low

This shifts per-block RMS values enough to change the top-20% selection and nudge the final DR across integer boundaries differently per track.

A split-accumulate approach (splitting frames at block boundaries) was attempted but couldn't be completed because the packed truncation issue made `ch_floats[0].len()` ≠ `decoded.samples()`, breaking the block counting logic.

## Recommended Next Steps (Priority Order)

1. **Re-attempt split-accumulate with `data(0)` packed fix + no ×2.** With correct per-channel data, `ch_floats[0].len()` = `nb_samples` for both planar and packed. Block counting uses `ch_floats[0].len()`. This eliminates the frame overflow variance.

2. **Test against 44.1kHz CD-rip FLACs.** All testing was on 88.2kHz DSD rips (packed format). Standard 44.1kHz FLACs likely decode as S32P (planar) where `plane()` works correctly. Need to verify both formats give consistent results.

3. **Compare per-block RMS values against dr14_t.meter** on the same file. Install `pip install dr14_t.meter`, run on a test track, extract per-block values, and compare numerically.

4. **Consider resampling to 44.1kHz before DR analysis** (matching dr14_t.meter). This would sidestep packed format issues and make results directly comparable.

## File References

- `src/tui/analyze.rs` — DR analysis implementation
- `src/db.rs` — Analysis cache with `ANALYSIS_ALGO_VERSION` (currently 16)
- `src/tui/probe.rs` — ffmpeg init with log level suppression
