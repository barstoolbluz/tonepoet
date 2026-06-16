# Post-Encode Sample Validation Rejects Resampled Output

## Problem

Converting audio with a different target sample rate fails with:
```
post-encode sample drift for lossless output: expected 21455826,
got 19712540, allowed 0
```

The ratio is consistent across all tracks: `19712540/21455826 ≈ 0.9188`
which is exactly `88200/96000`. The source is 96kHz, the target is
88.2kHz. The validation computes expected samples from the SOURCE
sample rate but the output has the TARGET sample rate's sample count.

This prevents any resampling of lossless formats. A user who sets
88.2kHz on the sample rate pill cannot convert 96kHz FLACs.

## Root cause

`validate_encoded_output_with_tool_limits()` at stages.rs:885 receives
`expected_samples` computed from the source file's sample count. When
the target sample rate differs from the source, the output will have
a different number of samples. The validation rejects this as "drift"
with `allowed 0` (from `encoded_output_sample_tolerance` at line 942
when `probe.exact` is true).

The validation assumes lossless output = same sample count as input.
This is only true for passthrough or same-rate encoding. Resampling
is a valid lossless operation (96kHz FLAC → 88.2kHz FLAC is still
lossless FLAC) but produces a different sample count.

## Code to read

```
src/convert/pipeline/stages.rs
  885   validate_encoded_output_with_tool_limits() — the validation
  922   delta comparison with allowed tolerance
  942   encoded_output_sample_tolerance() — returns 0 for exact probes
  935   requires_lossless_post_encode_sample_validation() — gate
```

## What the reasoning model should produce

1. When the target sample rate differs from the source sample rate,
   compute the expected output sample count from the target rate:
   `expected_output = source_samples * target_rate / source_rate`
   and validate against that with a reasonable tolerance (resampling
   can produce ±1 sample due to rounding).

2. Passthrough (same format, same rate) should keep the strict
   `allowed 0` validation — that's correct for bit-exact copies.

3. The fix should be minimal — just the validation logic, not the
   entire encode pipeline.

## How DSD→PCM avoids this today

SACD/DSD tracks set `expected_samples: None` in the materializer
(materializer_sacd.rs:104). At stages.rs:893, `None` causes the
validation to return `Ok(None)` — skipping it entirely. So DSD→PCM
conversions already work because there's no expected sample count
to validate against.

The bug affects ALL PCM source types when the target sample rate
differs from the source:

- **Single-file (FLAC/WAV/etc):** materializer sets `expected_samples`
  from source file sample count. Validation compares against this.
- **DVD-Audio:** `post_encode_expected_samples_for_track()` (line 2116)
  re-probes the realized WAV and uses the decoded sample count as the
  reference. This is the source-rate sample count. If the encode
  resamples, the output has fewer samples → validation fails.
- **DVD-Video LPCM:** same pattern as DVD-Audio.

None of these have been hit in practice because users typically
extract at source rate. But setting a different rate on the sample
rate pill will fail for any of them.

## Constraints

- Passthrough validation must remain strict (allowed 0)
- Lossy format validation is already skipped (line 897)
- DSD format validation is already skipped (line 936-938)
- DSD→PCM works today via expected_samples=None — don't break it
- The fix should handle any sample rate conversion, not just 96→88.2
