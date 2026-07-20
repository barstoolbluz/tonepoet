# P0 Reference DSD→PCM Policy v3 Amendment

**Date:** 2026-07-19
**Status:** append-only corrective policy amendment
**New immutable policy key:** `sox_ng_14_8_0_1_v3`

This amendment does not alter the historical `sox_ng_14_8_0_1_v1` or
`sox_ng_14_8_0_1_v2` identities. It creates a new policy because the qualified
cell set and terminal/analyzer evidence contract have changed.

## 1. Deterministic rejection precedence

The pure planner rejects in this order:

1. `pathway = manual` with `DSD-REF-P0-001`;
2. any policy other than v3 with `DSD-REF-P0-015`;
3. non-singleton programme scope with `DSD-REF-P0-012` or `DSD-REF-P0-013`;
4. source facts and source-cell admission;
5. target, profile, depth, gain, and packaging details.

Manual therefore wins independently of missing, malformed, or unsupported
source facts. Historical policy identity wins before v3-only cell errors.

## 2. Int16 is unavailable

No conservative worst-case peak bound for the commissioned SoX-ng Shibata
realization is frozen in this policy. Every Int16 target cell rejects before
render with:

```text
DSD-REF-P0-022: Reference policy sox_ng_14_8_0_1_v3 does not enable Int16
because the commissioned SoX-ng Shibata realization has no qualified
conservative worst-case peak bound. Choose Int24, Float32, or Float64, or wait
for a later immutable policy with a derived Shibata bound.
```

The Shibata command remains representable for historical policy decoding and
future evidence work; it is not an enabled v3 terminal cell.

## 3. Source-front-end cell authority

Enabled standalone source cells are:

- uncompressed DSF, DSD64/128/256, mono and stereo;
- uncompressed DSDIFF/DSD, DSD64/128/256, mono and stereo;
- DSDIFF/DST, DSD64 stereo only.

Predictive DSDIFF/DST outside DSD64 stereo rejects with `DSD-REF-P0-021`.
SACD DSD and SACD DST remain represented but reject before extraction with:

```text
DSD-REF-P0-023: Reference policy sox_ng_14_8_0_1_v3 does not enable SACD DSD
or DST extraction because the production extraction/materialization path is
not yet qualified by pinned end-to-end SACD fixtures. Extract to a qualified
DSF/DSDIFF source first or wait for a later immutable policy.
```

Promotion requires the production private-copy/container-classification/DSTC/
canonical-DFF materialization seam to reproduce the pinned oracle bytes and then
execute the exact planner-emitted render step for every enabled source/rate/channel
cell. The durable executed-evidence v2 digest binds the original source kind,
admitted source-content hash, and canonical materialization hash. Decoder-primitive
fixtures alone are not source-front-end authority.

## 4. True-peak analyzer authority

The one-sided `Q + E = 0.110000000 dB` authority is promotion-gated by a
1,200-case systematic corpus covering:

- every target rate from 44.1 through 768 kHz;
- mono and stereo;
- normalized frequencies 0.25 and 0.45 cycles/sample;
- phases 0 and π/4, with an independent stereo phase relationship;
- single-tone and phase-aligned four-tone waveform families, including fractional-sample aligned multitone peaks;
- analytic peaks at −120.003, −12.003, and −0.500 dBFS;
- durations 0.125 and 0.500 seconds;
- early and late peak placement.

Every case uses the planner-emitted loudnorm command and production parser,
requires a finite result for nonzero near-silence, proves the conservative
upper bound does not fall below analytic truth, and contributes to a canonical
evidence digest. This is a systematic empirical qualification contract rather
than a claim of a mathematical bound for an unspecified analyzer build. A policy
artifact without this exact report remains a `qualification_candidate` and
cannot execute.

## 5. Packaging and metadata evidence

The WavPack Int24 `-bits_per_raw_sample 24` correction introduced by v2 is
retained in v3. Package qualification executes planner-emitted commands and
compares decoded samples with QPCM.

A test-only FFmpeg stream-copy metadata rewrite is described only as package
stream-copy metadata evidence. It does not claim qualification of the
production metadata/artwork mutator. Production safety continues to require
post-mutation decode-and-compare before publication.
