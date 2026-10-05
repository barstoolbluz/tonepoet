# Brief R29 — the conversion log omits the command that filters the audio

Date: 2026-10-05
Base: the supplied `tonepoet-src.tar.gz` is `main` with R28 applied and
Reference requalified. Gate 7346 passed / 0 failed.

R28 is accepted. The dither line, the dither stage, the glyph and the removed
line are all correct in the field.

## What the log shows

A de-emphasis conversion of an 8-track album records 8 commands, one per
track:

```
Commands:
  1. sox -S -t raw -e floating-point -b 64 -L -r 44100 -c 2 <carrier>.f64le \
       -b 16 -C 8 <out>.flac dither
```

Each reads a `registered-effects/…effect-001.f64le` carrier. The log names
that carrier under `Source ref` and `Realized input`. It does not record the
command that produced it.

That unrecorded command is the one that applies the de-emphasis filter.

## The outcome we want

Every command that reads or writes audio appears in the log, in the order it
ran.

## Open question

The effect pass is lowered to either SoX or FFmpeg. In this conversion the log
reports `Tool versions: sox 14.8.0.1`, so both the effect pass and the terminal
pass were SoX.

Whether the effect and the terminal need to be separate invocations, rather
than one command carrying both the filter and the terminal dither, is not
established here.

## Build capability

`cargo test -p tonepoet-pipeline` builds in seconds. The root `tonepoet` crate
peaks at 6.24 GB in one `rustc` and will OOM under a 4 GB ceiling; write those
changes uncompiled and say so. Some files are Reference-source-locked
(`reference_source_lock.rs` lists them); if your change touches any, say so and
we requalify here.
