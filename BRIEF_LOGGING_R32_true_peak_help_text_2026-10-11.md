# BRIEF — R31 is accepted; and the true-peak row advertises a command instead of explaining itself

R31 is applied and gated. Both items are fixed and the gate is otherwise clean.

This is how it reads to us from the outside; we misapprehend this codebase
routinely, so where we are wrong, trust your own reading.

## What is verified

Gate: **7437 passed / 2 failed**. Compile clean, no unused or `missing_docs`
warnings. `r31_` 3 of 3, `r30_` 10 of 10.

```
execution_staging_live_and_recovery_reserved_block_stale_cleanup_until_retired
installed_reference_qualification_binds_the_current_locked_sources
```

The first passes 3 of 3 in isolation and is the documented lease-ownership
contention flake. The second is the expected freshness failure; `stages.rs` and
`settings.rs` moved in R30 and requalification has not yet been run.

Your correction was right and ours was wrong: `command.rs` assigns
`result.true_peak_dbtp` from `measure_loudness()` at two sites, so the
user-facing analysis does report true peak. Our grep was truncated and then
read as complete.

## 1. The true-peak row advertises a command instead of explaining itself

Both true-peak target rows, PCM and DSD, end with the same trailing text from
one shared site:

```
  0.25 dB steps; :set dsd-true-peak-target <dBTP>
  0.25 dB steps; :set pcm-true-peak-target <dBTP>
```

**What we want:** that text gone. The step size is evident from using the
control, and the row should not advertise a command. The `:set` keys keep
working exactly as they do now.

In its place a clickable `ⓘ` beside the row label, on both rows, opening a
popup that explains what true peak is. One message serves both; the concept
does not differ. `TuiButton::DeemphasisInfo` is the pattern: a two-cell region
registered in `convert_screen.rs`, opening `ActiveOverlay::Notice` with a title
and message.

The text:

> True peak is the highest level the reconstructed waveform reaches, in
> decibels relative to digital full scale. 0 dBTP is full scale.
>
> It is not the same as the highest sample value. Samples are individual
> points. When audio is reconstructed for playback or for sample-rate
> conversion, the signal between those points can rise higher than the samples
> themselves. These intersample peaks can exceed full scale even when no single
> sample does, and that is what clips.
>
> Target — the highest true peak the output may reach. TonePoet measures the
> audio and derives the gain from an upper bound on that measurement, one that
> cannot read low, so the output's true peak lands at or below the figure you
> set. A faster scan tier costs a little extra headroom, never safety. Encoding
> to a lossy format afterwards can move the peak again, because the decoder
> reconstructs the signal differently.
>
> Scope — which tracks share one gain calculation.
>
> Track — each track is measured on its own and gets its own gain to meet the
> target.
>
> Album — every track in this job is measured together and the same gain is
> applied to all of them, preserving the relative levels between them. Album
> means the tracks grouped into this job, not album metadata or tags.

The popup renders one uniform style with no markup, so this follows the
`Target —` shape the pre-emphasis message already uses. Emphasis and lists are
logged as issue 73 and deliberately not asked for here.

Two things in that text we would like checked rather than trusted. We believe
the gain comes from a certified upper bound that cannot under-read, so
measurement slack is spent as extra headroom and never as risk; the wording
asserts that. And we could not confirm whether a quiet track is raised to the
ceiling under Track scope, so the text says only that each track gets its own
gain.

More of these help messages are coming to the Convert screen, so whatever
convention this one sets is worth settling now rather than retrofitting.

## Requalification

`R31_ACCEPTANCE.sh` chains the `r31_` regressions into the unchanged
`R30_ACCEPTANCE.sh`, which runs `scripts/requalify_reference_r20.sh`. We have
not run it yet. R31 itself edited no locked file; the outstanding freshness
failure is R30's.
