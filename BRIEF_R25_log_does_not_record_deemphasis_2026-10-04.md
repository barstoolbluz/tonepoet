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

The log records de-emphasis when there is pre-emphasis evidence for the
source, and says nothing about it otherwise. A conversion with no evidence
gets no de-emphasis lines at all.

Where there is evidence, the Conversion Settings section carries these four
lines, in the log's existing one-fact-per-line style.

Applied:

```
De-emphasis: yes (CD pre-emphasis filtered out of the audio)
De-emphasis evidence: PRE_EMPHASIS tag on all 8 source tracks
De-emphasis chosen by: automatic — target is not lossless 16-bit/44.1 kHz
De-emphasis effect: audio altered; output is not bit-identical to the source, and will not match AccurateRip or any checksum taken from the pre-emphasised disc
```

Evidence present, filter not applied:

```
De-emphasis: no (pre-emphasis evidence present; filter not applied)
De-emphasis evidence: CUE FLAGS PRE on all 8 source tracks
De-emphasis chosen by: user
De-emphasis effect: pre-emphasis remains in the audio; the pre-emphasis flag is preserved in the output format
```

The `evidence` value is one of:

```
PRE_EMPHASIS tag on all N source tracks
CUE FLAGS PRE on all N source tracks
exact catalog match in the bundled pre-emphasis reference
```

The `chosen by` value is either `user` or `automatic — <reason>`.

The final line of the not-applied case reads `the pre-emphasis flag cannot be
carried by the output format` where that is so.

Use this wording. Do not paraphrase it.

## Build capability

`cargo test -p tonepoet-pipeline` builds in seconds. The root `tonepoet` crate
peaks at 6.24 GB in one `rustc` and will OOM under a 4 GB ceiling; write those
changes uncompiled and say so. Some files are Reference-source-locked
(`reference_source_lock.rs` lists them); if your change touches any, say so and
we requalify here.
