# tonepoet-true-peak

`tonepoet-true-peak` is an application-independent streaming crate for peak
measurement over decoded interleaved `f64` PCM. The certified surface has
exactly three user-selectable HQ1024V1 tiers; standards-compatible reporting is
separate.

## Public measurement surface

### Certified HQ1024V1 ceiling measurement

`CertifiedPeakMeter` accepts one `PeakTier`:

| Tier | Search | Reconstruction | Contract |
| --- | --- | --- | --- |
| `Reference` | Reference9 | HQ1024V1 | Exhaustive search; no live unresolved region may remain |
| `Standard` | Fast90 | HQ1024V1 | Deterministic bounded-work search; unresolved regions remain in the returned upper |
| `Fast` | Fast066V2 | HQ1024V1 | Complete HQ4 survey plus bounded selective HQ1024 probing; commissioning target 0.66 s/min |

Changing tier never changes the reconstructed waveform. A returned
`PeakCertificate` identifies `CertifiedReconstruction::Hq1024V1` and contains
an achieved `[lower, upper]` interval. For a hard-ceiling decision, use the
interval upper rather than the diagnostic point estimate.

```rust
use tonepoet_true_peak::{CertifiedPeakMeter, EdgePolicy, PeakTier};

let mut meter = CertifiedPeakMeter::new(
    48_000,
    2,
    EdgePolicy::RepeatEndpoints,
    PeakTier::Fast,
)?;
meter.push_interleaved(&decoded_frames_a)?;
meter.push_interleaved(&decoded_frames_b)?;
let certificate = meter.finalize()?;
let safe_upper = certificate.finite_interval.upper_linear;
```

All three tiers are deterministic for a fixed backend and invariant to caller
push size. Reference fails closed with
`TruePeakError::ReferenceSearchIncomplete` if an exhaustive-search invariant is
ever violated. Fast does not consult a clock while measuring.

### Fast066V2 execution policy

`FAST_ALGORITHM_REVISION == "Fast066V2"` identifies the shipping Fast policy.
It preserves the qualified 2x prefix, complete HQ4 survey, 4096-frame tiles,
256-frame certificate groups, frozen HQ1024 reconstruction, and existing
outward numerical arithmetic.

For each channel/tile, Fast066V2:

1. builds the complete HQ4 survey and every flat group upper bound;
2. retains those valid bounds and rejects optional work only against that
   channel's accumulated downward lower witness;
3. nominates at most 64 HQ4 local maxima using deterministic spatial-plus-global
   selection;
4. evaluates or reuses one fitted HQ1024 proposal for each surviving nominee;
5. ranks proposals by measured magnitude and admits at most eight finishers;
6. gives each finisher at most two `q0 +/- 32` probes and three `q1-1..q1+1`
   final knots, with no widening fallback.

The structural ceiling is therefore 104 newly evaluated non-HQ4 fine knots per
channel/tile: `64 + 8 * (2 + 3)`. Survey-knot and straightforward same-finisher
reuse can reduce the actual count. Candidate quotas affect the point estimate,
not certificate validity: every finite interval retains its independently
valid flat upper bound even when optional work is skipped.

The optional-work rejection domain is the full conservative neighborhood
`[256*i - 256, 256*i + 256]` clipped to the finite target. Boundary candidates
use a separately retained left-halo quarter-cell bound, preventing a
center-group-only rejection from overlooking a higher neighboring region.
Comparison is always per-channel; future samples visible only because of caller
chunking never become scheduling thresholds.

Fine-tail numerical uncertainty is cached once per channel/tile from the union
support maximum, maximum qualified-prefix error, and outward maximum frozen
phase L1 norm. Exact/coarse and HQ4 survey cases keep their tighter existing
handling. If the common envelope itself is unrepresentable for exceptional
finite data, Fast falls back only to the prior bounded per-knot numerical
calculation; it does not reopen V1 search work.

Fast normally returns `SearchStatus::WorkLimited` because unresolved flat-bound
uncertainty can remain after its deterministic work budget. Fully dominated
cases, including digital silence, may return `Complete`. The public Fast backend
never returns `TimeLimited`.

`FAST_WALL_NANOS_PER_PROGRAMME_MINUTE == 660_000_000` is an end-to-end release
benchmark target, not a runtime deadline and not an achieved-performance claim.

### Standards-compatible reporting

`ReportingPeakMeter` remains separately exposed. It implements the established
libebur128-compatible 4x/2x/1x reporting profile and is not a lower-accuracy HQ
ceiling tier. At rates below 96 kHz it uses 4x, at 96-191.999 kHz it uses 2x,
and at 192 kHz and above it uses 1x.

## Persistence collision warning

The names `reference`, `standard`, and especially `fast` existed in the retired
Headroom ladder and may occur in application settings. This crate does not
perform persistence migration. An old stored `fast` value meant the retired
16x-prefix job; current `PeakTier::Fast` means Fast066V2 over HQ1024V1. Wider
application code must make that migration explicitly.

## Finite-stream and numerical semantics

For certified HQ measurement:

- non-finite samples are rejected transactionally before streaming state mutates;
- `RepeatEndpoints` and `ZeroExtend` define finite-input extension;
- exact input-sample maxima remain independent lower/report floors;
- channels remain independent and the overall interval encloses their maximum;
- exact digital silence remains an exact zero interval;
- positive subnormals are classified and converted to dBTP without depending on
  DAZ/FTZ treatment of a subnormal libm operand;
- HQ1024V1 retains its frozen induced L-infinity upper of `4.68`;
- memory remains bounded by fixed prefix state, reconstruction halo, tile
  scratch, and bounded candidate work rather than programme duration.

## MSRV and release compiler note

The crate MSRV is Rust 1.93. The supplied baseline reported an LLVM 21.1
loop-vectorizer SIGSEGV under default optimized compilation and successful
optimized builds with loop vectorization disabled. The known-good release gate
should therefore be run with the operator's established vectorizer-disabled
setting first, followed by a separate default-codegen retry. Fast066V2 does not
claim to cure that compiler defect.

## Qualification and commissioning

The source-round qualifier is idempotent:

```text
python3 -B qualification/run_offline_qualification.py
```

It regenerates and byte-compares the frozen artifacts, independently re-derives
the numerical bounds, exercises exact-rational flat-envelope cases, and audits
the Fast066V2 graph and its behavioral test hooks. It is not a Rust compiler or
throughput substitute.

With the shipping Rust toolchain, run at minimum:

```text
cargo test
cargo test --release
cargo test --features fast-stage-timing
```

The timing feature exposes only the V2 B/C/D cuts inside the crate; it does not
add a public tier or preserve V1 in shipping code. Full A/B/C/D commissioning is
performed by the external driver `qualification/commission_fast066_v2.py`. The
driver requires the unchanged V1 baseline archive with SHA-256
`604060fce4a9159383e7200cd35af7325a7ddce8f854bb13782bee93ff3e61d0`, runs all
configurations against one carrier, and writes one provenance-rich JSON record.

```text
python3 -B qualification/commission_fast066_v2.py \
  --v1-archive /path/to/tonepoet-fast066-v1-implementation-corrected-2026-09-09.tar.gz \
  --carrier /path/to/carrier.f64le \
  --sample-rate 192000 \
  --channels 2 \
  --expected-point-dbtp <revalidated-dense-HQ-point> \
  --rustflags '<operator release flags, including the vectorizer setting>' \
  --output /path/to/fast066-commissioning.json
```

The driver measures A from the separately verified V1 source, B as V2 survey +
bounds, C as V2 B + nomination, and D as complete V2. If C is at or above 0.66
seconds per programme minute, commissioning stops before D for performance
purposes so the mandatory path can be profiled. Otherwise it runs an
instrumented nonbinding D followed by the uninstrumented binding D wall gate.
Carrier/source hashes and compiler metadata are collected outside the measured
wall interval; V1 and V2 use separate build directories. The manifest records
carrier/source identities, edge policy, algorithm revisions, compiler/codegen
inputs, active SIMD backend, timings, certificates, errors, and V2 work counters.

Use the full-duration carrier and agreed hostile/accuracy corpus. Release
requires both the uninstrumented 0.66 s/min wall target and the numerical and
certificate criteria. Until those operator runs are complete, throughput is
unverified.
