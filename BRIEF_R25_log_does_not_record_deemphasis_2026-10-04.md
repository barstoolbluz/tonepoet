# Brief R25 — the conversion log never mentions de-emphasis

Date: 2026-10-04
Base: the supplied `tonepoet-src.tar.gz` is `main` with R23 applied.

## What the user sees

A de-emphasis conversion of
`~/library/b/Boston - Don't Look Back (1978) [FLAC] {Japan Epic-Sony 35•8P-7}`
produced a log whose Conversion Settings section reads:

```
Target format: FLAC
Resampling: no (source rate preserved)
Bit-depth conversion: no (source depth preserved)
Dither: no (not requested)
Force encode: No
Merge mode: No
FLAC compression: 8
Metadata: Enabled
ReplayGain: Enabled
ReplayGain policy: recomputed: registered sample-domain processing changes the output signal
ReplayGain clipping prevention: Yes
Features: Enabled
```

De-emphasis is not named anywhere in the log.

The output files fail AccurateRip verification; the input files pass.

## What is already there

`req.registered_effects = vec![cd_deemphasis_effect()]`
(`src/convert/pipeline/plan_bridge.rs`). That registration is what produced the
`ReplayGain policy: recomputed` line above. The log records the consequence of
the filter and not the filter.

No string matching `deemphas` or `pre-emphas` appears anywhere in
`crates/tonepoet-features/`.

## The outcome we want

The log records that de-emphasis was applied, what evidence it was applied on,
and whether that was the automatic default or the user's choice.

It also records what the filter means for the output: the audio has been
altered, it is no longer bit-identical to the source, and it will not match
AccurateRip or any other checksum taken from the pre-emphasised disc.

A conversion where de-emphasis was available and left off says so too, so the
log distinguishes "not applicable" from "declined".

## Build capability

`cargo test -p tonepoet-pipeline` builds in seconds. The root `tonepoet` crate
peaks at 6.24 GB in one `rustc` and will OOM under a 4 GB ceiling; write those
changes uncompiled and say so. Some files are Reference-source-locked
(`reference_source_lock.rs` lists them); if your change touches any, say so and
we requalify here.
