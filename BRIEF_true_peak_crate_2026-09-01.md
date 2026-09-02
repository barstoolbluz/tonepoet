# BRIEF — A world-class, standalone, path-independent true-peak crate

**Date:** 2026-09-01
**Base:** `main` @ `3714ac1`
**Related:** `OUTSTANDING_ISSUES.md` #19 (preserve-original-peaks and true-peak-informed
headroom), #27 (the conversion manifest)

## A caveat on everything in the "what exists" section

What follows is my reading of the code as it stands, assembled over several analysis passes.
I was wrong more than once while assembling it and corrected myself each time — most notably
about which tool the DSD path uses for true peak, and about where the conversion manifest is
written. Some of what follows may still be wrong. Treat it as a map to verify, not as
established fact, and prefer the code where they disagree. Points I am genuinely unsure about
are marked as such rather than smoothed over.

## What we want

A standalone crate that measures true peak, depends on nothing in tonepoet, and can be
consumed unchanged both by the pipeline as it exists today and by the redesigned pipeline
that follows.

The interface should be: **decoded audio frames in, level out.** Sample rate and channel
count are parameters. The crate should not open files, discover tools, or know what a
"pipeline", "measurement", "policy" or "carrier" is.

Concretely, it must not import `DbNano`, `MeasurementParser`, `PlannedMeasurement`,
`DsdReferencePolicyVersion`, or any other tonepoet type. Today's caller converts the result
into whatever the current contract wants; the redesigned pipeline converts it into whatever
it wants. That single rule is what lets both consume the same crate without modification, and
it is the most important constraint in this brief.

### The quality bar

This must be a **world-class true-peak implementation**. That is the requirement, not an
aspiration, and it is the reason this is a separate crate rather than a helper function.

Concretely, world-class means:

- Its measurements are correct, not merely plausible — demonstrably so, against independent
  references, on real material.
- Its numbers are reproducible by other professional meters, or where they deliberately are
  not, the divergence is intentional, documented, and selectable. See "the conformance
  question" below.
- It is honest at the edges: silence, inputs shorter than the filter, levels above 0 dBFS,
  multichannel, and the warm-up region all behave correctly rather than approximately.
- It is deterministic, so the same input always yields the same answer.
- It would stand on its own if published — something an audio engineer could depend on
  without knowing anything about tonepoet.

A meter whose correctness cannot be demonstrated is not world-class regardless of how good its
filter is, which is why the validation requirement below is not optional.

## What exists today, as best I can tell

### Three different peak measurements, none shared

**1. The DSD reference path** (`tonepoet-pipeline/src/dsd_reference.rs`) has true peak, but
does not compute it. `build_true_peak_measurement` constructs a *planned external command*
which the root crate later runs and text-parses. There are four versioned parser contracts:
three FFmpeg `loudnorm` variants reading `input_tp`, and one SoX contract that oversamples
(`rate -v -L -s`) to a "qualified 16x measurement view" and reads `Pk lev dB` from `stats`.

There are **sixteen** `DsdReferencePolicyVersion` values. At execution,
`validate_reference_measurement_contract` rejects everything except the current pair
(`SoxNg14801V16` + `SoxStatsPkLevDbV1`) with "unknown or historical Reference measurement
parser". The older variants exist to deserialize records, not to run.

The reason for that churn appears, from `assets/dsd_reference/brief_dsd_reference_p0_corrective_analyzer_carrier.md`,
to be carrier-format interop rather than disagreement about the algorithm:

```
sox f64 → W64  → ffmpeg loudnorm:  input_tp 166.64   BROKEN (+2^31 scale)
sox f32 → W64  → ffmpeg loudnorm:  input_tp -20.00   correct
sox f64 → CAF  → ffmpeg loudnorm:  input_tp 166.64   BROKEN
sox f64 → WAV / AIFF:              correct, but 4 GiB-class ceilings
```

I am **not certain** why the design ended at SoX 16x rather than a fixed loudnorm carrier —
whether loudnorm's BS.1770 4x was also judged insufficient, or whether SoX simply won on
carrier robustness. That distinction matters for the conformance question below and is worth
establishing.

**2. The PCM/ReplayGain path** shells out to `loudgain` and parses `True_Peak_dBTP` from
tab-delimited columns by field index (`src/convert/replaygain.rs`, `src/tui/analyze.rs`).
loudgain uses libebur128 internally. As far as I can tell this feeds ReplayGain **tagging
only** — I found no peak-driven gain decision anywhere on the PCM side.

**3. DSD album auto-gain** (`tonepoet-pipeline/src/dsd_album_gain.rs`) measures **sample peak
only** — SoX `stats` → `Pk lev dB`. This is the path that decides whether to attenuate a
batch, and it is blind to inter-sample overs. It is the subject of issue #19.

### The planner/executor split

`tonepoet-pipeline` appears to be a pure planner: I found no `std::process` or
`tokio::process` in it, and its dependencies are only `sha2`, `serde` and `serde_json`. It
builds `PlannedCommand` values; the root crate executes and parses them. The root crate is
GPL-3.0-or-later while `tonepoet-pipeline` is MIT OR Apache-2.0, so in-process measurement
naturally belongs on the root-crate side of that line, where licensing is unconstrained.

### Where a decoded-PCM moment already exists

The album auto-gain path materialises a raw Float64 carrier on disk — `track-NNNN-<hash>.f64le`
in a scratch directory — validates that its length is frame-aligned, runs SoX over it, and
deletes it. Format, sample rate and channel count are all known at that point.

That is the natural first consumer: it is where true peak is **missing**, it is what issue #19
is about, and using it there requires no change to the certified reference contract.

### In-process resampling is already available and unused

- The ffmpeg in this flake is built `--enable-libsoxr`.
- `aresample` exposes `resampler=soxr`, `precision` from 15 to 33 bits, and
  `internal_sample_fmt`.
- `ffmpeg-next` is already a dependency; `software-resampling` is one of its **default**
  features, so it is already compiled in.
- Its `Context::get_with(src_format, src_layout, src_rate, dst_format, dst_layout, dst_rate,
  options: Dictionary)` accepts explicit formats and rates plus an options dictionary — enough
  to select soxr, set precision, and run F64 in and out at an arbitrary oversampling ratio.
- The **already-built binary links `libswresample.so.5` and `libsoxr.so.0`**.
- `software::resampling` has **zero** uses anywhere in tonepoet.

So high-quality oversampling is linked into the process today and entirely unused. Whether to
use it, or to implement the filter directly, is open — see below.

I noticed the built binary loads two `libavutil` versions simultaneously (`.so.59` from
ffmpeg-full 7.1.3 and `.so.60` from ffmpeg-headless 8.0.1). I did not determine why, and it
may be irrelevant, but a component doing more in-process ffmpeg work is where it would start
to matter.

## The conformance question, which we have not decided

ITU-R BS.1770-4 specifies a *particular* 4x oversampling filter. soxr at high precision is
arguably a better reconstruction, but "better" and "conformant" are different properties, and
they can disagree by a fraction of a dB.

Which matters depends on the use:

- For **headroom decisions** — should this album be attenuated — a superior estimate is
  straightforwardly better.
- For **reported dBTP** — anything shown to a user or written into a tag — a number that
  cannot be reproduced by other meters reads as a defect, however good the filter.

A defensible answer is that the crate offers both: a conformant mode that agrees with
libebur128 within tolerance, and a higher-precision mode for internal decisions. Which is the
default, and whether both are needed at all, is the implementer's call.

This is settleable empirically rather than by argument, and the references that settle it are
described in the next section. There is real material to test on in this environment,
including large DSD sources.

## Implementation freedom, and the one thing that is not optional

**How the measurement is produced is entirely your choice.** Write the oversampling filter
from scratch; drive libsoxr in-process through `ffmpeg-next`, which is already linked and
unused; adapt a published implementation with a compatible licence; or something else. There
is no preferred answer here and no obligation to reuse anything that currently exists in this
repository. The existing paths are described above so you know what is there, not because the
new crate should imitate them — they are external-tool text parsing, which is precisely what
this work is meant to stop doing.

**What is not optional is demonstrating correctness.** A true-peak meter that has not been
checked against independent references is a guess with a confident interface. The delivery
should establish agreement — or explain divergence — against references outside its own
implementation.

Note carefully that the two in-tree implementations are **cross-checks, not ground truth**.
The SoX 16x route arrived after sixteen policy revisions driven by carrier interop bugs, and
the loudnorm variants it replaced produced values like `input_tp 166.64` on a carrier that
merely happened to be f64 W64. Agreement with them is evidence; disagreement is not
automatically your error, and finding that one of them is wrong would be a genuinely useful
result. Published conformance vectors, other standards-compliant meters, and synthesised
signals with analytically known inter-sample peaks are all stronger references than anything
currently in this repository.

## Things the crate has to get right

Listed because they are where a naive implementation quietly goes wrong, not as a design.

- Multichannel: true peak is per-channel, and the reported value is the maximum across
  channels.
- Silence and near-silence, including a true `-inf` result, which the existing parsers already
  represent explicitly.
- Filter warm-up and latency, so the first samples are not mismeasured or skipped.
- Determinism: the same input must produce the same output, since results feed gain decisions
  and, at least today, contracts that are validated strictly.
- Very short inputs, shorter than the filter length.
- Values above 0 dBFS, which are the entire point — clamping them defeats the purpose.

## Scope

**In scope:** the crate, and wiring it into the DSD album auto-gain path so #19's attenuation
decision can be made against a true-peak measurement rather than a sample peak.

**Out of scope:** changing the DSD reference measurement contract. It is certified, its
executor rejects unknown parsers, and a pipeline redesign is planned that will revisit it. A
seventeenth policy version spent on machinery about to be replaced is work thrown away.

**Also out of scope:** the "preserve original peaks" gain policy from #19 itself. That is a
product decision about what to do with the measurement, and it should follow once the
measurement exists.

## Working constraints

- The implementation container has no Rust toolchain, no Nix, and none of the audio tools.
  Running the gate is the operator's job; no delivery should assume it has been run.
- `src/convert/pipeline/mod.rs` carries `#![deny(unsafe_code)]`. Files beneath it that need
  `unsafe` use a narrowly scoped `#[allow(unsafe_code)]` with a justifying comment; `tool.rs`
  and `progress/streaming.rs` are the established examples.
- New workspace members are an established pattern; there are currently eight.
- Tests that mutate process-global state have caused repeated flakes in this project.
