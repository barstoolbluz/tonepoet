# Brief R9 — rate-change accounting, again, at the post-encode check

Date: 2026-09-30
Base: the supplied `tonepoet-src.tar.gz` is authoritative. It is our local
`main` with R6, R7 and R8 applied and the SSRC true-peak registry commissioned.

The SSRC grid qualified: 4,368 cells, all passing, promoted through the
promotion script. The conversion that motivated it now gets past commissioning
and fails at post-encode validation instead.

---

## The defect

AC/DC, *Back in Black*: one WavPack image plus sidecar CUE, 10 tracks,
192 kHz source, converting to FLAC Int16 TPDF at 44.1 kHz with album
true-peak. Track 7:

```
track validation failed: post-encode sample drift for lossless output
  007-07 - You Shook Me All Night Long.flac: expected 40140800, got 9219840, allowed 0
```

40140800 / 9219840 = 4.35374149659864, exactly 192000/44100. Both counts are
the same 209.067 seconds of audio. The expectation is in source-rate samples,
the actual is in target-rate samples, and they are compared directly with a
tolerance of zero.

The machinery to handle this exists and was not used.
`post_encode_sample_expectation_from_source` rescales via
`resampled_sample_count` when source and target rates differ, sets the target
rate, marks the expectation resampled, and attaches an SSRC filter-tail
allowance. Two signals say that branch was skipped and the `same_rate`
fallback taken instead: the tolerance is 0, and `encoded_output_sample_tolerance`
returns at least 1 for any resampled expectation — the SSRC FIR tail when a
filter length is known, otherwise 1 for endpoint rounding — so 0 can only come
from a non-resampled expectation; and the rate guard above the drift
comparison passed, which the fallback permits because it still records the
target rate while leaving the count unscaled.

Why the rescaling branch was skipped is not established. One candidate is
`source_sample_rate` arriving as `None`, which selects the fallback. There is
a recovery path that re-probes the realized input when the source rate is
missing, and it logs when it runs; it did not log during this conversion.
That is a lead, not a finding.

This is the same family as issue #55: rate-change accounting where one side of
a comparison was never brought into the other's domain. Different site —
post-encode lossless validation rather than the terminal realization.

## Scope not established

Only track 7 reached validation. The other nine were cancelled while in
flight. Whether the drift affects every track or only some is unknown, and we
did not instrument to find out.

## A second, smaller defect

The nine collateral failures read:

```
certified SSRC true-peak terminal execution failed: tool cancelled
```

R4 taught the scheduler to classify `PCM true-peak measurement failed for
track <N>: PCM true-peak scan cancelled` as collateral. This is a different
wording from a different path, so it is not classified, and nine cancellations
are again reported as peer failures with the real cause buried among them.

Recognise this variant too, on the same exact-match basis R4 used. Not a
generic `contains("cancelled")`.

## Required

A lossless conversion whose target rate differs from its source rate validates
against an expectation expressed in the output's own domain, and this album
converts.

The zero tolerance stays. This is about comparing comparable quantities, not
about loosening a lossless check. Where an expectation carries a sample count
it should carry the rate that count is in, so a future domain mismatch fails
loudly instead of silently comparing across rates.

## State

Gate 7206-7208 passed, 0 failed plus up to two contention flakes that pass in
isolation. `tonepoet-true-peak` 160/0. Reference requalified on the Nix
rooting. SSRC true-peak registry commissioned with 4,368 records bound to
report SHA-256 `b5ab9698`. Issues #55 and #57 closed; this is filed as #58.

The contention flake has now appeared on seven different tests across runs,
all in the coordination-lease area, which supports the wandering hypothesis
from the R4 brief rather than any test-specific cause.

## Build capability

`cargo test -p tonepoet-pipeline` and `cargo test -p tonepoet-true-peak` build
in seconds. The root `tonepoet` crate peaks at 6.24 GB in one `rustc` and will
OOM under a 4 GB ceiling; `stages.rs` lives there, so write that change
uncompiled and say so.
