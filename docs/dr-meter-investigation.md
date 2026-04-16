# DR Meter Implementation — Investigation Notes

## Current Status

DR values are ±1-2 of foobar2000's foo_dr_meter. For an album where foobar reports DR14 for all tracks, we get 15, 16, 14 across three tracks.

## Root Cause: Two Interacting Problems

### Problem 1: `decoded.plane()` truncates packed format data

ffmpeg-next's `Audio::plane::<T>(index)` returns a slice of length `nb_samples` (per-channel count). For **planar** formats this is correct — each plane IS one channel's data. For **packed** formats, the single interleaved buffer has `nb_samples × channels` values, but `plane()` only returns the first `nb_samples`. We lose half the audio for stereo packed.

**Evidence** (from debug diagnostics on 88.2kHz stereo FLAC files):
```
sum of decoded.samples() across all frames:  34,261,560
sum of ch_floats[0].len() across all frames: 17,130,780  (exactly half)
channels: 2
```

**Fix attempted**: Use `decoded.data(0)` (raw byte buffer) and reinterpret with correct length `nb_samples × channels`. This correctly recovers all per-channel samples.

### Problem 2: The ×2 AES-17 RMS factor

The TT DR spec (as implemented in dr14_t.meter) uses `sqrt(2 × Σs² / N)` instead of standard `sqrt(Σs² / N)`. This is the AES-17 convention where a full-scale sine reads 0 dBFS.

**The interaction**: Before we fixed Problem 1, each packed-format block accumulated only `N/2` real samples but divided by `N`. The formula was:
```
sqrt(2 × sum_of_N/2_samples / N) = sqrt(sum_of_N/2_samples / (N/2)) = standard_RMS
```
The ×2 and the half-data **canceled**, accidentally giving standard RMS. This matched foobar at ±1-2 DR.

After fixing Problem 1 (full data), the ×2 no longer cancels:
```
sqrt(2 × sum_of_N_samples / N) = sqrt(2) × standard_RMS  (+3 dB)
```
DR dropped by ~3 dB ("much worse").

Removing ×2 after the packed fix returns to standard RMS with correct data — but gives the **same ±1-2 variance** as the original accidentally-correct code, because the frame-level block overflow issue remains.

## What We Don't Know

1. **Does foobar2000's foo_dr_meter use the ×2 factor?** dr14_t.meter clearly does, but foobar is closed-source. Our empirical results suggest foobar uses standard RMS (no ×2), or that something else compensates.

2. **Is the FLAC decoder actually outputting packed or planar format?** We inferred packed from the 2:1 ratio between `decoded.samples()` and `plane().len()`, but never confirmed the actual `Sample` variant at runtime. Adding `eprintln!("{:?}", sample_fmt)` would settle this instantly but hasn't been done (TUI captures stderr).

3. **Does dr14_t.meter's resampling to 44.1kHz affect results?** dr14_t.meter runs `ffmpeg -ar 44100` on all input before analysis. At 44.1kHz the decoder may output planar format, avoiding the packed truncation entirely. This could explain why dr14_t.meter's ×2 is correct for their pipeline but wrong for ours.

4. **Block boundary alignment**: The remaining ±1-2 variance comes from frame-level block overflow. When a decoded frame crosses a 3-second block boundary, all its energy goes to one block. The split-accumulate approach (exact boundaries) was attempted but failed due to the packed data truncation making block counts wrong. With the packed fix in place, split-accumulate should be re-attempted.

## What We've Tried

### Algorithm changes (all kept)
- ✅ Per-channel 2nd-highest block peak (not overall file peak)
- ✅ Quadratic mean (RMS-of-RMS) of top 20% in linear domain
- ✅ Second-highest (min for stereo) per-channel DR selection
- ✅ `floor(n × 0.2)` for top-20% block count
- ✅ 44.1kHz block size quirk (3 × 44160 = 132480)
- ✅ EOF decoder flush frames processed
- ✅ Last partial block flushed with actual sample count
- ✅ ffmpeg log level set to Quiet (prevents TUI corruption)
- ✅ Analysis cache with algo_version for invalidation

### Algorithm changes (reverted)
- ❌ AES-17 ×2 RMS factor — empirically gives +3 dB error when combined with correct packed data
- ❌ Split-accumulate at exact block boundaries — failed because block counting used `decoded.samples()` which didn't match actual per-channel data length for packed formats
- ❌ Truncation instead of rounding — inconsistent across tracks

### Packed format fix (implemented but effectively neutralized)
- The `data(0)` fix for packed formats IS in the code and IS correct
- But without ×2, the result is the same as the old code with ×2 + half-data
- The fix is important infrastructure for the split-accumulate re-attempt

## Recommended Next Steps

1. **Confirm the sample format at runtime.** Write `sample_fmt` to a log file for the test tracks. If it's planar, the packed fix is irrelevant and the problem is elsewhere.

2. **Re-attempt split-accumulate with the packed fix in place.** With correct per-channel data, `ch_floats[0].len()` = `nb_samples` for both planar and packed. Use this as the canonical frame length for block boundary splitting.

3. **Test against 44.1kHz files.** All testing so far has been on 88.2kHz DSD rips. Test against standard CD-rip FLACs where the decoder almost certainly outputs S32P (planar) and there's no packed truncation issue.

4. **Consider resampling to 44.1kHz before DR analysis**, matching dr14_t.meter's approach. This would sidestep all packed format issues and make results directly comparable.

5. **Compare per-block RMS values against dr14_t.meter output** on the same file to identify exactly where values diverge.

## File References

- `src/tui/analyze.rs` — DR analysis implementation
- `src/db.rs` — Analysis cache with `ANALYSIS_ALGO_VERSION` (currently 13)
- `src/tui/probe.rs` — ffmpeg init with log level suppression
