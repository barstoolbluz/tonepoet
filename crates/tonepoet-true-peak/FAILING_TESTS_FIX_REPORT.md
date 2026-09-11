# Fast066V2 failing-tests remediation report

Date: 2026-09-10

## Scope and disposition

This complete source bundle addresses the four Rust failures described in
`DIAGNOSTIC_BRIEF_failing_tests_2026-09-10.md` against the supplied repository
bundle. It contains the full repository tree with the fixes applied, not only an
overlay.

Input identities:

```text
a050bf5035279218e5650f592283cbe111a0feb73d28072c6fb812c2b16b0f40  tonepoet-true-peak-failing-tests-bundle-2026-09-10.tar.gz
64ad57a3752fc44d85393bd9c7d411cfea3e132812a71884e2236d5a591ab2d2  DIAGNOSTIC_BRIEF_failing_tests_2026-09-10.md (revised brief supplied 2026-09-10)
```

The fix intentionally does not change the public API, `FAST_ALGORITHM_REVISION`,
`FAST_WALL_NANOS_PER_PROGRAMME_MINUTE`, `PeakTier::Fast.interval_objective_db()`,
the frozen HQ1024 coefficient source, or the 0.01 dB per-channel certificate
width requirement.

## Failure 1 — channel processing independence

**Determination: test defect, not production-code defect.**

The failed assertion required the stereo and mono point estimates for the quiet
channel to agree within a fixed `2.0e-10` linear tolerance. That property is
stronger than the Fast066V2 design. Sparse/direct and packed-dense arithmetic
graphs are allowed to produce different floating-point point estimates; their
authoritative obligation is truthful containment and the 0.01 dB certificate
width floor. The reported discrepancy was already materially smaller than the
certified interval width.

The regression now compares the independently produced stereo and mono
certificates for overlap and applies the existing 0.01 dB width check to the
stereo result and both mono results. It still requires the fixture to exercise
packed dense work and direct rescoring of the quiet channel. No production
arithmetic was changed for this failure.

## Failure 2 — retired 104-evaluation cap

**Determination: test defect, not production-code defect.**

The production resolver contains no 104-evaluation termination condition. The
old public fixture was simply not a valid witness: the native screen discarded
most of its coverage, leaving only 12 mandatory refinements. In addition,
`fast_fine_knots_evaluated` is intentionally retired and remains zero; mandatory
work is accounted in the generic counters.

The replacement has two parts:

1. A private resolver regression constructs competitive uncertain coverage that
   cannot retire by dominance or tolerance and therefore forces complete dyadic
   subdivision. With the frozen 512-phase tail hierarchy and the selected span,
   the current tree geometry implies 1020 tail evaluations, so the test directly
   requires `refined_cells > 104` without depending on screening behavior.
2. The public integration fixture verifies that ordinary Fast execution records
   mandatory refinement in the live generic counters while the retired
   `fast_fine_knots_evaluated` counter remains zero, and still checks the 0.01 dB
   accuracy floor.

The offline source audit was updated to require both regressions. No resolver
policy or work limit was added or changed.

## Failure 3 — qualified-prefix DAZ/FTZ collapse

**Determination: genuine code defect.**

This failure was already present and failing as of baseline 987a355; the revised brief deliberately does not claim that commit introduced it. It nevertheless violates an authority invariant
that the true-peak subsystem already claims: source support proven nonzero from
IEEE-754 magnitude bits must never acquire an exact-zero numerical enclosure
merely because the host enables DAZ/FTZ.

The qualified half-delay executor now carries the already-computed integer
`component_peak_bits` into `block_error_upper`; exact-zero classification is made
before reconstructing the floating peak. For nonzero source support, the final
authoritative error upper is normalized to at least `f64::MIN_POSITIVE` if the
computed bound is zero or subnormal. The same normal-valued fail-closed boundary
is applied to the coefficient-L1 enclosure used when an FFT block is skipped.

This does not perturb ordinary audio: normal error bounds pass through unchanged,
and the new work is one inexpensive magnitude-bit classification per authority
bound, not per FIR tap or FFT operation.

## Failure 4 — raw-screen/direct-enclosure DAZ/FTZ collapse

**Determination: genuine code defect.**

The raw screen already correctly fails open on subnormal support. The remaining
defect was downstream: the direct first-stage authority could still expose an
exact-zero enclosure for source support known to be nonzero under DAZ/FTZ.

The direct-range executor now reduces the support maximum entirely in IEEE
magnitude encodings and reconstructs the floating maximum only after exact-zero
classification. After the existing rigorous rounding/underflow error derivation,
a nonzero source support receives a normal-valued minimum authority floor if the
computed error is zero or subnormal. This preserves fail-closed containment even
if the numerical FIR value itself flushes to zero.

There is no added per-tap work. The integer maximum reduction also avoids the
previous per-sample `u64 -> f64 -> u64` accumulator round trip.

## Qualification and handoff gates

Executed successfully in this environment:

```text
python3 -B qualification/run_offline_qualification.py
```

Result: **PASS**. This includes coefficient regeneration/comparison, qualified
prefix verification, certified-search verification, raw-screen exact tests,
selective Fast source audit, and commissioning-driver tests.

The authoring container does not contain `cargo`, `rustc`, `rustfmt`, or `nix`,
and no usable Rust toolchain could be installed in the environment. Therefore
the two mandatory release Rust commands could not be executed here and remain
handoff gates:

```text
RUSTFLAGS='-C llvm-args=--vectorize-loops=false' \
  CARGO_TARGET_DIR=target/fast-shipping \
  cargo test --release -p tonepoet-true-peak --no-fail-fast

RUSTFLAGS='-C llvm-args=--vectorize-loops=false' \
  CARGO_TARGET_DIR=target/fast-timing \
  cargo test --release -p tonepoet-true-peak --features fast-stage-timing --no-fail-fast
```

Additional local checks completed before packaging:

- modified Python qualification scripts executed successfully;
- modified Rust files passed a lexical delimiter audit;
- changed-file whitespace checks are clean;
- no public/API or frozen coefficient file was modified.

## Files changed from the supplied source bundle

```text
DIAGNOSTIC_BRIEF_failing_tests_2026-09-10.md
crates/tonepoet-true-peak/src/fast_scan.rs
crates/tonepoet-true-peak/src/qualified_half_delay_fft.rs
crates/tonepoet-true-peak/tests/fast_scan.rs
crates/tonepoet-true-peak/qualification/verify_fast_scan.py
crates/tonepoet-true-peak/qualification/verify_qualified_prefix.py
crates/tonepoet-true-peak/FAILING_TESTS_FIX_REPORT.md
```
