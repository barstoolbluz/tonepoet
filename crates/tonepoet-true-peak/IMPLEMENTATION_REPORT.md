# Implementation report - three HQ tiers

Date: 2026-09-09
Scope: `tonepoet-true-peak` crate only

## Delivered surface

The caller-facing certified surface is now exactly `PeakTier::{Reference,
Standard, Fast}`. Every tier is hard-bound to `HQ1024V1`; reconstruction choice
and search-policy internals are no longer caller-selectable. `ReportingPeakMeter`
remains public and explicitly separate because it implements the established
standards-compatible reporting profile rather than the HQ ceiling waveform.

Mapping:

- Reference -> internal Reference9, HQ1024V1.
- Standard -> internal Fast90, HQ1024V1.
- Fast -> internal clock-bounded accelerated search, HQ1024V1.

LegacyHeadroom64 remains internal for independent oracle tests. The old public
64x/16x/8x point ladder, Headroom scan ladder, compatibility ceiling bridge,
and unused unqualified RustFFT executor are removed.

## Fast time policy

The public target is one second of total wall time for each 60 seconds of
programme audio. Internally Fast schedules meter work at a 0.90-second-per-
programme-minute slope with a bounded 50 ms startup allowance. The slope is
derived from nominal frame count and sample rate rather than from an album-size
budget. The deliberate margin leaves room for construction, validation, I/O or
decoding, dispatch and result handling; `bench_ceiling_f64le` retains the total
timer and is the binding user-visible wall gate.

Deadline handling is fail-closed and local:

1. constructor plus successful push/finalize processing counts against the
   cumulative internal allowance;
2. before an expensive qualified-prefix FFT block, Fast checks the clock; once
   exhausted, that block emits exact integer knots plus `0 +/- L1*peak` for its
   half knots, where the complete binary64 FIR L1 is derived outward from the
   frozen coefficients; overlap history still advances, so later exact FFT
   blocks can resume without state drift;
3. candidate/summarization work is not started after an observed deadline;
4. heap refinement retains every still-live root upper when the clock stops it;
5. a tile skipped before candidate construction retains the HQ reconstruction
   L-infinity bound over that tile's exact raw support, not a whole-track sample
   peak; if some channel roots were already constructed, those completed
   channels keep their tighter root uppers and only unfinished channels use the
   raw-support fallback.

Thus a deadline can widen only the affected prefix blocks/tiles and cannot make
the certificate optimistic. `time_bounded_prefix_blocks_skipped` and
`time_limited_tiles` make both forms of deadline intervention observable.

Fast can report `Complete` if all competitive regions are genuinely resolved
before the deadline; otherwise it reports `TimeLimited`. Standard retains its
deterministic work-credit `WorkLimited` status.


Standard is not collapsed into Fast in this round. The supplied baseline puts
Standard materially between Reference and the new Fast target, and the policies
have distinct semantics: Standard is deterministic bounded work; Fast is
clock-bounded and may widen under hostile content or host contention. There is
no commissioning evidence yet that this distinction is redundant.

The requested Fast accuracy is deliberately not invented from source inspection.
This environment cannot execute the shipping Rust benchmark, so the achieved
ordinary/hostile interval at one second per programme minute remains an operator
measurement. The implementation reports that interval and the deadline
diagnostics needed to make the measurement meaningful.

## Reference chunk-invariance correction

The previous three-tier delivery was wrong to say that splitting the old
`reference9_dense_and_rescore_paths_are_observable_and_chunk_invariant` test
fully preserved its obligations. The focused dense/rescore tests were an
improvement, but the known constant-carrier chunk-invariance case had been
removed without a runtime correction. Those focused tests remain unchanged.

The corrective change is local to tie-sensitive Reference search state:

- `TileEvaluationFrontier` previously retained equal-upper competitors by
  arrival order once its bounded capacity was full. Equal-authority candidates
  are now ranked by their absolute knot `(cell, phase)`, and replacement uses
  that same total priority. Reordering arrivals therefore cannot change the
  retained frontier.
- `CandidateNode` previously had an incomplete `Eq`/`Ord` relation: distinct
  dense spans, and distinct dyadic work sharing the same phase interval, could
  compare equal even though the heap would execute different work. Its heap
  order now includes the complete work identity and is consistent with `Eq`.

Two cheap state regressions lock those properties directly. In particular,
`rescore_frontier_equal_upper_retention_is_canonical` inserts more than the
frontier capacity in ascending and descending absolute order. The pre-fix
algorithm necessarily retains different tied knots for those two orders; the
corrected algorithm retains the same canonical set.

The end-to-end constant regression is structurally minimized from the known
193-frame, constant-0.75, 1-versus-37 case to 38 frames: 38 is the smallest
carrier that still contains one full 37-frame caller push plus a remainder. It
compares the complete certificate for whole-buffer, one-frame, 37-frame, and
irregular delivery under both edge policies and both internal reconstructions.
A public `PeakTier::Reference` counterpart covers both edge policies through the
caller-facing HQ-only API. The non-degenerate chunk regression and the focused
dense/rescore forcing tests also remain.

This environment still cannot execute Rust, so 38 frames is a structural
minimization, not an empirically proven minimal reproducer. The required
operator gate is therefore explicit: run the new focused regression first, then
`cargo test` and `cargo test --release`. Do not call this correction commissioned
until those gates pass on Rust 1.93.x.

## Independent Legacy oracle correction

LegacyHeadroom64 remains crate-private and is now exercised again as an actual
independent oracle. The focused Rust regression
`hq_reference_is_cross_checked_against_legacy_direct_oracle_on_analytical_intersample_carrier`
calls the direct Legacy64 cascade and HQ1024V1 Reference in the same test on a
deterministic half-sample analytical carrier inside the qualified frequency
domain. The stored-sample maximum is deliberately below the known continuous
peak, so the fixture cannot collapse into a sample-aligned check.

The comparison envelope is inherited from the existing Legacy qualification:
0.050 dB absolute point error in the <=0.495 Fs qualified domain. HQ is required
to remain inside that established analytical envelope, and the Legacy/HQ
cross-difference is bounded by the corresponding triangle bound. This is a
regression oracle for gross/self-consistent HQ coefficient or ordering errors,
not a new production authority or a second shipping scan.

`qualification/verify_certified_search.py` no longer accepts the mere presence
of the `legacy_direct_oracle_peak` helper as proof that the oracle is exercised.
It requires the focused analytical regression and its actual Legacy call. The
existing per-reconstruction exhaustive test remains because it checks a
different property: search enclosure of each selected finite reconstruction.

## MSRV/dependency cleanup

- MSRV raised from Rust 1.82 to Rust 1.93.
- The const `transmute` workaround was replaced with const `f64::to_bits()`.
- The retired unqualified RustFFT executor was deleted.
- The crate now depends directly on `num-complex` rather than depending on all
  of RustFFT solely to obtain `Complex64`.
- The existing release `codegen-units = 1` setting is retained to preserve the
  codegen shape used for the supplied performance baseline. It is explicitly no
  longer described as the LLVM workaround; the known-good workaround remains
  disabling loop vectorization in the operator environment.

No version bump was made.

## Verification performed here

This environment has Python/NumPy/SciPy but no `rustc`, `cargo`, `nix`, or
`rustup`. Therefore no Rust compile/test result is claimed.

The independent source-only qualification passed after the changes. The final
source audit also verifies that Reference cannot degrade to a release-only
partial certificate: a live unresolved Reference upper fails closed with
`ReferenceSearchIncomplete` instead of relying on `debug_assert!`.

- HQ1024 coefficient regeneration is byte-identical.
- Qualified-prefix coefficient regeneration is byte-identical.
- Frozen twiddle and packed-prefix error bounds re-derive outward.
- HQ sampled response and dyadic bounds pass.
- Legacy oracle composition/dense-pairing bounds pass.
- Source audit confirms the direct Legacy oracle is actually called by the HQ
  analytical cross-check regression.
- Source audit confirms the tie-canonical frontier/order regressions and the
  reduced constant-carrier chunk regression remain present.
- DAZ/FTZ bitwise classification/logarithmic source audit passes.
- Three-tier public-surface audit passes.
- Fast rate-derived deadline and fail-closed fallback source audit passes.

The checked-in qualification reports are regenerated from the final source.

## Required operator gates

Before hand-off into the wider application, run:

1. `cargo test reference9_constant_carrier_preserves_certificate_across_former_1_vs_37_boundary -- --nocapture`
2. `cargo test public_reference_constant_carrier_is_chunk_invariant_across_edges -- --nocapture`
3. `cargo test hq_reference_is_cross_checked_against_legacy_direct_oracle_on_analytical_intersample_carrier -- --nocapture`
4. `cargo test`
5. `cargo test --release`
6. full-duration Reference and Standard benchmark on the operator carrier
7. full-duration Fast benchmark on ordinary material
8. full-duration Fast benchmark on hostile material (dense intersample peaks,
   upper-band energy, and difficult phase relationships)
9. first use the known-good release setting with loop vectorization disabled;
   then retry default release codegen to determine whether deleting the retired
   executor/dependency eliminated the LLVM crash

For Fast, record total wall seconds, programme seconds, `TimeLimited` vs
`Complete`, achieved interval width, `time_bounded_prefix_blocks_skipped`,
`time_limited_tiles`, and unresolved upper.
The benchmark itself fails the one-second-per-minute wall gate.

## Integration warning

Do not map persisted wider-application strings by name. In particular, old
`fast` meant the retired 16x-prefix mode while new `PeakTier::Fast` means the
clock-bounded HQ1024V1 tier. The application migration is intentionally deferred
to the later integration round.
