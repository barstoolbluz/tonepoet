# Brief R22 — the De-emphasis option never appears for a FLAC source

Date: 2026-10-03
Base: the supplied `tonepoet-src.tar.gz` is `main` at gate 7295 / 0,
`tonepoet-true-peak` 160 / 0, Reference qualification current.

## What the user sees

No pre-emphasis or de-emphasis option in the Format pane for
`~/library/b/Boston - Don't Look Back (1978) [FLAC] {Japan Epic-Sony 35•8P-7}`.

That album is 8 FLAC tracks at 44.1 kHz / 16-bit stereo, all 8 tagged
`PRE_EMPHASIS=1`, with `COMMENT=Matrix: 35 8P-7-6  1A1 (Sony)`, and the disc is
listed in `assets/reference/cds-with-preemphasis-shf.csv`. The TUI was
restarted on a current binary.

## What was measured

`probe_audio()` on that album's first track, and on a track of an unrelated
96 kHz / 24-bit FLAC album:

```
rate=44100  depth=Some(16)  float=Some(false)  lossless=None
rate=96000  depth=Some(24)  float=Some(false)  lossless=None
```

`convert_cd_deemphasis_eligible()` returns `false` for the first.

No existing test probes a file and checks eligibility; the de-emphasis tests
construct source facts directly.

## The outcome we want

A CD-domain source carrying pre-emphasis evidence offers the de-emphasis
option, whatever lossless container it arrives in, and a test proves it from a
file on disk rather than from constructed facts.

Where a source is not eligible, that is because of something true about the
source.

## Build capability

`cargo test -p tonepoet-pipeline` builds in seconds. The root `tonepoet` crate
peaks at 6.24 GB in one `rustc` and will OOM under a 4 GB ceiling; write those
changes uncompiled and say so. Some files are Reference-source-locked
(`reference_source_lock.rs` lists them); if your change touches any, say so and
we requalify here.
