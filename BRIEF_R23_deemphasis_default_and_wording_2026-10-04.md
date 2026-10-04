# Brief R23 — de-emphasis default, and what the UI tells the user

Date: 2026-10-04
Base: the supplied `tonepoet-src.tar.gz` is `main` with R22 applied. Gate 7298
passed / 0 failed; `tonepoet-true-peak` 160 / 0; Reference qualification
current.

R22 works: a pre-emphasised FLAC album now offers the De-emphasis field. Three
things about that field are wrong.

## 1. The warning glyph

The row renders `ⓘ ⚠ pre-emphasis retained; playback signaling suppressed`.

Drop the `⚠`.

## 2. The default does not follow the target

De-emphasis stays Off regardless of what the source is being converted to.

It should default to On whenever the target is lossy, or whenever the target is
anything other than lossless 16-bit/44.1 kHz. It should default to Off when the
target is lossless 16-bit/44.1 kHz.

This must re-evaluate whenever the target changes. Switching from AAC to WAV, or
changing the sample rate or bit depth, re-decides the default. A user who has
set the field by hand keeps their choice.

## 3. The wording

The row and the pop-up describe internal bookkeeping rather than what happens
to the music.

All three evidence states carry the same guidance, differing only in what is
known. When De-emphasis is Off:

```
pre-emphasis detected: turn on to correct it.
pre-emphasis flagged in the CUE: turn on to correct it.
this pressing is known pre-emphasised: turn on to correct it.
```

for a tag on the file, a CUE `FLAGS PRE`, and a catalog match respectively.

When De-emphasis is On, the row does not tell the user to turn on something
already on:

```
pre-emphasis will be corrected.
```

The suffix is currently computed without reference to whether the field is
enabled.

Pop-up:

```
This disc was mastered with pre-emphasis: the treble was boosted on the CD,
and the player is meant to cut it back on playback.

On — tonepoet removes the boost now, so the file plays correctly anywhere.
This is a filter, not a tag change: the audio is altered, and the output will
not be bit-identical to the source even when both sides are lossless at
16-bit/44.1 kHz.

Off — the boost stays in the audio, and tonepoet keeps the pre-emphasis flag
wherever the format can carry it. CD players and some software — foobar2000,
JRiver, iTunes, cmus — read that flag and correct playback. Where the flag
cannot travel, such as a lossy target or anything outside 16-bit/44.1 kHz,
the file will sound bright everywhere.
```

## The outcome we want

The field defaults correctly for the target, follows the target when it
changes, and says in plain terms what each setting does to the audio.

## Build capability

`cargo test -p tonepoet-pipeline` builds in seconds. The root `tonepoet` crate
peaks at 6.24 GB in one `rustc` and will OOM under a 4 GB ceiling; write those
changes uncompiled and say so. Some files are Reference-source-locked
(`reference_source_lock.rs` lists them); if your change touches any, say so and
we requalify here.
