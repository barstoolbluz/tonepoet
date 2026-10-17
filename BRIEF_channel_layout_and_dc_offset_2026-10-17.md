# BRIEF — output channel layout, and DC offset

The tree is clean: gate **7529 passed / 0 failed**, zero aborted test binaries,
Reference qualification binding, `main` at the merged `v0.5.5` plus the issue
#59 claim forensics.

Two additions to the Convert screen. Both effects already exist in sox and in
the pinned FFmpeg, so the planner should choose between them as it does
elsewhere rather than being told which to use.

## 1. The output channel layout is a conversion setting

TonePoet carries channel count from source to output untouched. There is no
control for it in `FormatState` or in the pipeline settings, and the only
channel logic I found rejects a mismatch rather than resolving one.

Multichannel reaches TonePoet from at least: a SACD ISO's multichannel area,
a DVD-Audio ISO, a Blu-ray ISO, DVD-Video, a multichannel FLAC or WavPack, and
MKA or MKV. So a 5.1 source has no path to a stereo file, whatever carried it.

**What we want:** the user chooses the output channel layout, and that choice
is independent of how the source was delivered. It belongs where bit depth and
sample rate are decided, not inside any one source family's reader. The
coefficients used for a fold are stated rather than implicit, and the choice
participates in the typed plan, the fingerprint and the conversion log like any
other effect.

One constraint. A fold sums contributing channels — five of them for 5.1 — so
a default that simply adds them will clip. TonePoet already owns a certified
true-peak mechanism, and I believe that is what should govern the result rather
than a fixed attenuation guess, but the right composition is yours to
determine.

sox `remix` and FFmpeg `pan` both do this.

## 2. DC offset: removable, and not alarming when it is tiny

Two halves of one problem.

There is no way to remove a DC offset during conversion.

And the Browse analysis classifies DC bias against a fixed absolute threshold,
so the verdict changes with level. Issue #5 establishes that the measurement is
correct and the label is not: the DC-to-signal ratio is identical across the
quiet DSD transfer, the same transfer after auto-gain, and the redbook CD —
but only the gained one reads `significant!`, because absolute DC scales with
the gain applied. Any boost trips a fixed line.

**What we want:** a DC-offset removal the user can apply during conversion, and
a classification whose verdict does not change merely because the content got
louder. A reading of 0.003 on loud material should not be called significant
when the same master reads negligible when quieter.

sox `dcshift` and FFmpeg `dcshift` both do this; a high-pass is the other
common approach and TonePoet already has registered high-pass effects.

## Placement and ordering

Both rows go **below the fold** — visible when the Format pane is maximized,
like the other advanced controls. Neither is promoted.

Ordering is the planner's job, against every supported effect and not just
these two. A channel fold changes how many signals exist; a DC shift changes
sample values; both interact with gain, resampling, de-emphasis, HDCD decoding
and the terminal. We are not supplying a sequence for you to encode.

More effects are coming. Whatever resolves the order here should hold for the
next one without being rewritten, and without a new scheduler or DSP framework
being invented for it — the existing typed plan, registered-effect ordering and
terminal ownership are the mechanisms.

## Two things already in the tree

`SoxHighPass`, `SoxLowPass`, `FfmpegHighPass`, `FfmpegLowPass` and
`SoxSamplePeakNormalize` are registered effects with no Convert row. The
pattern for surfacing a registered effect as a row therefore already exists and
should be reused rather than reinvented. Surfacing those rows is not asked for
here.

Dither belongs at the final precision-reducing quantization boundary. A remix
or a DC shift is a sample-domain operation and sits before it.
