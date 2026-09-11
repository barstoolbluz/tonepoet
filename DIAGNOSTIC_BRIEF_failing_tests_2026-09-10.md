# Diagnostic brief — Fast066V2 native-input-screen: four failing Rust tests

Date: 2026-09-10
Author of this brief: integration/commissioning operator (not the implementing model)
Crate: `crates/tonepoet-true-peak`
Branch context: `feat/true-peak-reference9-fast90`, with your
`tonepoet-fast-input-screen-implementation-commissioning-corrected-2026-09-10`
bundle applied on top.

## Why you are getting this

Your `IMPLEMENTATION_REPORT.md` correctly states that your authoring
environment had no `cargo`/`rustc`/`rustfmt`, so the Rust suite, the release
build, and target-machine wall-time commissioning were left as mandatory
handoff gates. We have now run those gates on real hardware. The Rust build
compiles clean (warnings only). **Four tests fail.** This brief hands you
everything needed to resolve them.

Note on the offline Python qualification: your `IMPLEMENTATION_REPORT.md` says
it passes in your environment. We could **not** re-run it here — our integration
shell has no `numpy`, and `generate_hq1024.py` (and four other qualification
scripts) import it — so we have **not** independently confirmed a passing
baseline. Treat it as a gate you still own, not as something we verified.

This is a **declarative** brief: it describes each failure with measured
evidence and states the outcome we want. It does **not** prescribe an
implementation. You are far better placed than we are to decide, per failure,
whether the correct fix is to the **test** (because it asserts something
stronger than the design actually guarantees) or to the **code** (because it is
a genuine defect). Make that call yourself, failure by failure, and justify it.

## Outcome we want (acceptance criteria)

1. Both of these commands are fully green — every `test result:` line shows
   `0 failed`, across all targets (lib, integration, doctests):

   ```
   RUSTFLAGS='-C llvm-args=--vectorize-loops=false' \
     CARGO_TARGET_DIR=target/fast-shipping \
     cargo test --release -p tonepoet-true-peak --no-fail-fast

   RUSTFLAGS='-C llvm-args=--vectorize-loops=false' \
     CARGO_TARGET_DIR=target/fast-timing \
     cargo test --release -p tonepoet-true-peak --features fast-stage-timing --no-fail-fast
   ```

2. The offline qualification still passes **in an environment that has `numpy`**:
   `python3 -B crates/tonepoet-true-peak/qualification/run_offline_qualification.py`.
   (We could not run it in our shell — no `numpy` — so we are not asserting a
   confirmed baseline; keep it green in yours.)

3. **Hard constraints — do not violate to make a test pass:**
   - Do not weaken the accuracy guarantee. The per-channel certified interval
     width must remain ≤ 0.01 dB (the `assert_fast_accuracy` floor and the
     bench's binding accuracy gate). It is currently met on all inputs we
     tested, including these fixtures.
   - Do not change the frozen public API, `FAST_ALGORITHM_REVISION`
     (`Fast066V2`), `FAST_WALL_NANOS_PER_PROGRAMME_MINUTE` (`660_000_000`), or
     `PeakTier::Fast.interval_objective_db()` (`None`).
   - No band-aids: if a failure is a real defect, fix the defect; do not merely
     retune a threshold or swap a fixture to hide it. If a failure is a
     legitimately over-strict test, fixing the test is the correct engineering
     — but say so explicitly and explain why the design does not owe the
     stronger property.

4. Return a new bundle (same layout as your previous one — a directory tree we
   overlay on top of the crate). Include a short report of what you changed for
   each of the four failures and, for each, your test-vs-code determination and
   its justification.

## Environment where the failures reproduce

- `x86_64-linux`, release build, `RUSTFLAGS='-C llvm-args=--vectorize-loops=false'`.
- This is **not** the target Xeon E5-1680 v2; do not treat any wall-time number
  here as commissioning. Wall time is irrelevant to these four failures.
- The CPU honors DAZ/FTZ (MXCSR DAZ bit 6 / FTZ bit 15) — relevant to failures
  3 and 4, which deliberately set those bits.

Provenance matters here and is not uniform across the four — we verified it with
`git`:

| # | Test | Test provenance | Fails on pre-patch baseline (987a355)? |
|---|------|-----------------|----------------------------------------|
| 1 | `channel_processing_is_independent_...` | **added by your bundle** (`tests/fast_scan.rs`) | n/a — test does not exist there |
| 2 | `mandatory_resolution_..._104_..._budget` | **added by your bundle** (`tests/fast_scan.rs`) | n/a — test does not exist there |
| 3 | `daz_ftz_subnormal_input_cannot_become_exact_silence` | **pre-existing**, present and failing as of 987a355; `src/certified_scan.rs` is **unchanged** by your bundle | **YES — fails identically at line 4649**, with the patch stashed |
| 4 | `raw_screen_..._fail_closed_under_daz_ftz` | **added by your bundle** (`src/fast_scan.rs`) | n/a — test does not exist there |

So failures 1, 2 and 4 are your own tests against your own code. **Failure 3 is
different: it is a pre-existing test that already failed on the branch before
your bundle was applied** — we confirmed it fails the same way with your changes
stashed. It exercises the qualified-prefix path; your bundle did modify
`qualified_half_delay_fft.rs`, but that modification is not what makes it fail
(baseline fails too). We include it here because our acceptance gate is a green
suite and you own that subsystem — but if you judge it out of scope for *this*
bundle, say so and we will route it separately rather than have you carry a
pre-existing defect.

Accuracy is not the issue in failures 1 and 2: we measured both certified
intervals at ~0.0092–0.0094 dB, inside the 0.01 dB floor (numbers below).
Failures 3 and 4 are not about interval width at all — they are DAZ/FTZ
fail-closed (subnormal-flush) invariants. What fails across the set are
auxiliary invariants: point-estimate reproducibility, a work-counter magnitude,
and DAZ/FTZ fail-closed behavior — not the peak value on ordinary audio.

---

## Failure 1 — `channel_processing_is_independent_within_certificate_error`

- File: `crates/tonepoet-true-peak/tests/fast_scan.rs`, panics at the second
  assertion (the quiet-channel comparison, ~line 246).
- Fixture: 1200 frames. Left channel = `deterministic_noise`. Right channel =
  a 31-frame-shifted copy scaled by `1.0e-6` (so channel 1 peak ≈ 1.5e-6).
  Runs the same right-channel data both as stereo channel 1 and as a standalone
  mono scan and asserts the two point estimates agree.

Measured (this build):

```
loud channel:  |stereo.ch0 - mono_left.ch0|  = 2.220e-16   (<= 2e-10 ? yes)
quiet channel: |stereo.ch1 - mono_right.ch0| = 2.658e-10   (<= 2e-10 ? NO — 1.33x over)
  stereo.ch1 point ≈ 1.50100227519e-6
  mono_right point ≈ 1.50073651486e-6
  stereo.dense_regions              = 1     (assert >0: ok)
  stereo.direct_rescore_evaluations = 1807  (assert >0: ok)
  channel interval widths: ch0 = 0.009316 dB, ch1 = 0.009369 dB
```

Facts for your determination:

- The absolute tolerance is a fixed `2.0e-10` linear. The observed discrepancy
  `2.658e-10` corresponds to ~1.77e-4 relative on a 1.5e-6 peak, i.e. ~0.0015
  dB — well **inside** each channel's own certified interval width (~0.0094 dB).
- So the *certificate* is channel-independent to spec; it is the *point
  estimate* that differs by slightly more than `2e-10`.
- The plausible mechanism is the packed dense first-stage FFT (two channels
  packed into one complex transform; note `dense_regions=1`,
  `direct_rescore_evaluations=1807`). The question you must answer: is ~2.66e-10
  of quiet-channel perturbation the *expected* floor of the packed-FFT +
  direct-rescore path (in which case the test's fixed `2e-10` is stronger than
  the design guarantees, and the honest fix is to assert independence *within
  certificate error* rather than against an absolute linear constant), or is it
  avoidable cross-channel coupling you intend to eliminate at the source?

## Failure 2 — `mandatory_resolution_can_exceed_the_retired_104_fine_knot_budget`

- File: `crates/tonepoet-true-peak/tests/fast_scan.rs`, panics at ~line 260.
- Fixture: `deterministic_noise(257, 1)` scanned at 192 kHz mono, `ZeroExtend`.
- Assertion: `certificate.diagnostics.refined_cells > 104`, intended to prove
  the production resolver is not capped by the retired 104-fine-knot policy.

Measured (this build):

```
refined_cells             = 12     (assert > 104 ? NO)
fast_fine_knots_evaluated = 0
candidate_cells           = 128
dense_regions             = 1
dense_phase_evaluations   = 1811
strict_coarse_evaluations = 0
channel interval width ch0 = 0.009240 dB   (accuracy floor: ok)
```

Facts for your determination:

- Your `lib.rs` rewrite retired the V2 nomination/proposal/finishing counters
  (they are documented as always zero now) and states "mandatory dyadic work
  uses generic counters." `fast_fine_knots_evaluated` is 0.
- So `refined_cells` may no longer be the counter that expresses "fine knots
  the mandatory resolver evaluated," and/or this 257-sample fixture simply does
  not force >104 mandatory refinements under the selective scanner (the screen
  rejects most of it: `candidate_cells=128`, `refined_cells=12`).
- You must decide: is the *intent* (prove the >104 cap is truly gone) better
  served by (a) asserting on a different, still-live counter, (b) a fixture that
  genuinely provokes >104 mandatory refinements, or (c) some invariant that
  demonstrates uncapped behavior directly? Whatever you choose, the test must
  end up actually exercising and proving the property it names.

## Failure 3 — `daz_ftz_subnormal_input_cannot_become_exact_silence`

- **Pre-existing** (see provenance table): this test and its failure predate
  your bundle. It fails identically on baseline 987a355 with your changes
  stashed. `certified_scan.rs` is unchanged by your bundle.
- File: `crates/tonepoet-true-peak/src/certified_scan.rs` (inline test).
- Panics at **line 4649**: the assertion
  `"{reconstruction:?} qualified prefix misclassified a nonzero subnormal block
  as silence"` (`saw_nonzero_half_error` is false).
- The test spawns a thread, sets MXCSR DAZ+FTZ, feeds an all-subnormal block
  (`f64::from_bits(1|3|7|11)`), and requires the `QualifiedHalfDelayFft`
  qualified prefix to emit a **nonzero** half-phase error enclosure.

Determination signal: under hardware DAZ/FTZ, some floating-point operation in
the qualified-prefix half-error path operates on subnormal operands and the CPU
flushes the result to exact zero, so `half_error[0]` reads as magnitude-zero and
the block is misclassified as silence. The test asserts the design is DAZ/FTZ-
immune ("fail closed, never collapse a nonzero subnormal support to exact
zero"). This reads as a **genuine code gap** (the invariant is one the design
intends to hold on any conforming host), not an over-strict test — but confirm
that yourself and fix at the source (e.g. compute/inspect the enclosure via
integer/bit operations that are immune to DAZ/FTZ, consistent with the
`magnitude_bits` discipline used elsewhere in the same test).

## Failure 4 — `raw_screen_and_direct_enclosure_fail_closed_under_daz_ftz`

- File: `crates/tonepoet-true-peak/src/fast_scan.rs` (inline test).
- Panics at **line 2290**: `"direct numerical envelope must not collapse a
  nonzero subnormal support to exact zero"`.
- Same setup family: MXCSR DAZ+FTZ set, a single subnormal sample
  (`f64::from_bits(7)`) in the raw support; after `execute_direct_ranges`, no
  cached half within the range has `upper() > 0.0`.

This test is **new in your bundle** and exercises the native-screen/direct-
enclosure path. It is almost certainly the **same class of root cause** as the
pre-existing failure 3, surfacing in the `fast_scan` direct-enclosure path
rather than the qualified prefix: a subnormal intermediate flushed to zero by
the hardware. The likely single discipline that fixes both is making the
enclosure/half-error arithmetic DAZ/FTZ-immune (bit/integer inspection rather
than DAZ-sensitive float comparisons, consistent with `magnitude_bits`). If you
address failure 3, treat failure 4 as the same fix verified in a second path.
The raw screen must continue to *fail closed* (stay unqualified, keep an
infinite/again-nonzero `root_d_upper`) exactly as the test demands.

---

## How to build and test (nix dev shell)

```
nix develop --extra-experimental-features 'nix-command flakes'
# then, inside the shell, the two commands under "acceptance criteria" above.
```

The crate needs `libclang`/ffmpeg headers only for the workspace's other crates;
`tonepoet-true-peak` itself is pure Rust and builds/tests standalone via
`-p tonepoet-true-peak`.

## What is in the bundle we are sending you

The full repository tree (workspace root + all crates), excluding `target/` and
`.git/`. The files you will most likely touch:

- `crates/tonepoet-true-peak/src/fast_scan.rs`
- `crates/tonepoet-true-peak/src/certified_scan.rs`
- `crates/tonepoet-true-peak/src/qualified_half_delay_fft.rs`
- `crates/tonepoet-true-peak/tests/fast_scan.rs`

Return a bundle we overlay on top of the current tree (same convention as your
prior deliveries), plus a short per-failure report with your test-vs-code
determinations.
