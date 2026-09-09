# tonepoet-true-peak

`tonepoet-true-peak` is an application-independent streaming library for peak
measurement over decoded interleaved `f64` PCM. This round intentionally breaks
the crate surface: the older Headroom ladder is retired and the certified HQ
surface now has exactly three user-selectable tiers.

## Public measurement surface

There are two different measurement jobs. Do not conflate them.

### Certified HQ1024V1 ceiling measurement

`CertifiedPeakMeter` accepts one `PeakTier`:

| Tier | Search | Reconstruction | Contract |
| --- | --- | --- | --- |
| `PeakTier::Reference` | Reference9 | HQ1024V1 | Exhaustive search; no live unresolved region may remain |
| `PeakTier::Standard` | Fast90 | HQ1024V1 | Deterministic bounded-work search; unresolved regions remain in the returned upper |
| `PeakTier::Fast` | time-bounded accelerated search | HQ1024V1 | Target one second of total wall time per minute of programme audio; report the tightest truthful interval obtained |

Changing tier never changes the reconstructed waveform. A returned
`PeakCertificate` always identifies `CertifiedReconstruction::Hq1024V1` and
contains the achieved `[lower, upper]` interval. For hard-ceiling policy, use
the interval upper, not the diagnostic point estimate.

```rust
use tonepoet_true_peak::{CertifiedPeakMeter, EdgePolicy, PeakTier};

let mut meter = CertifiedPeakMeter::new(
    48_000,
    2,
    EdgePolicy::RepeatEndpoints,
    PeakTier::Standard,
)?;
meter.push_interleaved(&decoded_frames_a)?;
meter.push_interleaved(&decoded_frames_b)?;
let certificate = meter.finalize()?;
let safe_upper = certificate.finite_interval.upper_linear;
```

Reference and Standard are deterministic for a fixed backend and are invariant
to caller push size. Reference is not permitted to return a partial certificate:
if its exhaustive-search invariant is ever violated and a live unresolved upper
survives finalization, the meter returns `TruePeakError::ReferenceSearchIncomplete`
in release as well as debug builds. Fast is deliberately clock-bounded, so
scheduler timing may change how much optional refinement fits. Timing never
changes the waveform definition and never licenses dropping an unresolved upper.

Standard is intentionally retained rather than collapsed into Fast. It is the
middle tier with a deterministic work policy: on the supplied baseline it was
materially faster than Reference while remaining independent of host clock
contention. Fast answers a different question -- the tightest truthful
certificate the one-second-per-minute wall target buys on this run -- and may
therefore return a substantially wider interval on hostile material or a slow
host. Commissioning may later show that one of these choices is not worth the
product surface, but this crate round does not have evidence for that collapse.

If Fast reaches its allowance during candidate/refinement work, it returns
`SearchStatus::TimeLimited`. Deadline handling is local and fail-closed. A
qualified-prefix FFT block that would start after the deadline is replaced by
a source-derived FIR L1 enclosure for that block while exact integer knots and
overlap history continue normally. If a tile cannot spend selective search
work, the retained fallback is the frozen HQ1024V1 induced L-infinity bound
times the exact sample peak over that tile's raw reconstruction support. When
a deadline arrives between channels during candidate construction, channels
whose root bounds are already built keep those tighter roots; only unfinished
channels fall back to the raw-support bound. A deadline therefore widens only
the work it actually prevented; it does not poison the whole track or fabricate
a narrow interval.

The public Fast constant is
`FAST_WALL_SECONDS_PER_PROGRAMME_MINUTE == 1`. Internally the scheduler uses a
0.90-second-per-programme-minute slope plus a bounded 50 ms startup allowance.
That leaves headroom for construction, chunk validation, file I/O/decoding,
dispatch and final reporting while avoiding a fixed album budget. The meter
clock covers construction and successful push/finalize work; it cannot control
scheduling pauses or host speed, so the included full-duration benchmark is
the binding user-visible wall-time gate and fails if total Fast wall time
exceeds one second per minute of programme audio.

### Standards-compatible reporting

`ReportingPeakMeter` remains separately exposed. It implements the established
libebur128-compatible 4x/2x/1x reporting profile used when interoperability of
a reported peak value matters. It is not a low-accuracy HQ ceiling tier.

At input rates below 96 kHz it reports on the 4x profile; at 96-191.999 kHz it
uses 2x; at 192 kHz and above it uses 1x, matching the existing reporting
contract.

## What disappeared

The following caller-selectable surface is intentionally gone:

- `TruePeakMode::Headroom64x`
- `TruePeakMode::HeadroomStandard16x`
- `TruePeakMode::Headroom16x`
- `TruePeakMode::Headroom8x`
- `HeadroomScanMode::{Reference, Standard, Fast, Fastest}`
- `HeadroomCeilingMeter` and the prefix-ceiling bridge
- public reconstruction selection between LegacyHeadroom64 and HQ1024V1
- public `SearchPolicy::{Reference9, Fast90}` selection independent of tier
- the unused unqualified RustFFT prefix executor

`LegacyHeadroom64` remains internal as the independent oracle used by tests to
cross-check the new HQ reconstruction/search machinery. Reporting4x remains
public because it answers a different standards/interoperability question.

## Persistence collision warning

The names `reference`, `standard`, and especially `fast` existed in the retired
ladder and may also exist in wider-application saved settings. This crate does
not implement persistence or migration. Integration code must not deserialize
an old stored `fast` value directly into `PeakTier::Fast`: the old value meant
the retired 16x-prefix job, while the new value means a clock-bounded HQ1024V1
search. Make that migration decision explicitly in the later application round.

## Finite-stream and numerical semantics

For certified HQ measurement:

- input is finite decoded PCM; non-finite samples are rejected before mutation;
- `RepeatEndpoints` and `ZeroExtend` extend the original finite input;
- exact input sample maxima remain independent lower/report floors;
- channels remain independent and the overall interval encloses their maximum;
- silence is represented as an exact zero linear interval;
- positive subnormal binary64 values are classified and converted to dBTP from
  their bit representation so DAZ/FTZ cannot silently turn nonzero into silence;
- the HQ1024V1 reconstruction has a frozen induced L-infinity upper of `4.68`;
- memory is bounded by fixed prefix state, reconstruction halo, canonical tile
  summaries, and candidate work rather than programme duration.

The Fast deadline is a search/work policy, not a numerical assumption. When
work is skipped, its uncertainty stays in the upper bound.

## MSRV and release compiler note

The crate MSRV is now Rust 1.93. The obsolete Rust-1.82 const-transmute
workaround has been removed and `f64::to_bits()` is used directly. The existing
release `codegen-units = 1` setting is retained to preserve the codegen shape of
the supplied performance baseline; it is not claimed as the LLVM workaround.

The source bundle supplied for this round reported an LLVM 21.1 loop-vectorizer
SIGSEGV under default optimized compilation and successful optimized builds
with loop vectorization disabled. This environment has no Rust toolchain, so
this delivery does not claim that removing the retired executor/dependency also
removes that compiler crash. The operator must run the release gate with the
same known-good loop-vectorizer-disabled setting first, then separately retry
default release codegen to determine whether the surface/executor reduction
made the compiler workaround unnecessary.

## Qualification and operator gates

The source-only numerical qualifiers are idempotent:

```text
python3 qualification/run_offline_qualification.py
```

They regenerate and byte-compare the HQ1024 and qualified-prefix artifacts,
independently re-derive the prefix enclosure and certified-search constants,
and source-audit the three-tier/fail-closed policy. They do not substitute for
Rust codegen or timed commissioning.

With the operator's shipping toolchain:

```text
cargo test
cargo test --release
cargo run --release --example bench_ceiling_f64le -- <carrier.f64le> <rate> <channels> reference
cargo run --release --example bench_ceiling_f64le -- <carrier.f64le> <rate> <channels> standard
cargo run --release --example bench_ceiling_f64le -- <carrier.f64le> <rate> <channels> fast
```

Run the full-duration Fast benchmark on ordinary and hostile material. The
benchmark reports the achieved interval and returns nonzero if total Fast wall
time exceeds the one-second-per-minute target. Do not replace that gate with a
short-run extrapolation.


This environment cannot answer the brief's empirical question "what accuracy
does one second per minute buy?" without executing the shipping Rust build on
the operator's material. Do not substitute a source-derived accuracy promise.
Record the achieved interval width from those runs; the implementation keeps
that result truthful even when the deadline forces a wider enclosure.
