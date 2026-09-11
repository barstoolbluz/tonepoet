# Diagnostic brief — offline qualification is not portable across hardware/BLAS

Date: 2026-09-10
Author of this brief: integration/commissioning operator (not the implementing model)
Crate: `crates/tonepoet-true-peak`
Branch context: `feat/true-peak-reference9-fast90`, with the native-input-screen
bundle and the four-failing-tests fix already applied and green.

## Standing status (so you know this is not a regression report)

Both Rust suites are fully green on real hardware, in both build configs:

```
RUSTFLAGS='-C llvm-args=--vectorize-loops=false' CARGO_TARGET_DIR=target/fast-shipping \
  cargo test --release -p tonepoet-true-peak --no-fail-fast          # 49+12+16+0, 0 failed
RUSTFLAGS='-C llvm-args=--vectorize-loops=false' CARGO_TARGET_DIR=target/fast-timing \
  cargo test --release -p tonepoet-true-peak --features fast-stage-timing --no-fail-fast   # 49+12+16+0, 0 failed
```

This brief is about a *different* gate: the offline Python qualification. Your
prior report recorded it as PASS in your environment. We could not run it before
(no numpy); we have now added numpy/scipy/mpmath to the flake dev shell and run
it on real hardware for the first time. **It fails here — and the failure is a
portability defect in the qualification harness, not in any shipping artifact.**

## What we want (outcome, declarative)

Make the offline qualification's *regeneration cross-checks* pass on any
conforming machine, while still catching genuine algorithmic drift. Concretely:
`python3 -B crates/tonepoet-true-peak/qualification/run_offline_qualification.py`
should pass on hardware other than the one that originally minted the frozen
coefficients.

This is your subsystem and your methodology call. We are **not** prescribing the
mechanism. We describe the defect precisely; you choose the fix.

**Hard constraints — do not violate:**
- Do **not** regenerate or modify the frozen coefficient sources
  (`src/hq1024_coefficients.rs`, `src/qualified_prefix_coefficients.rs`,
  `src/headroom64_coefficients.rs`, `src/raw_screen_metadata.rs`, and the
  checked-in `*.json`). They are the embedded source of truth, validated by the
  Rust suite's frozen-checksum integrity tests, which pass. Changing them over a
  2-ULP hardware artifact is the wrong fix.
- Do **not** change the public Rust API, `FAST_ALGORITHM_REVISION`, the frozen
  constants, or any Rust source. The Rust gates already pass; leave them alone.
- The relaxed check must still **fail** on a genuinely wrong regeneration
  (wrong algorithm, wrong geometry, a coefficient off by more than rounding).
  Do not simply delete the comparison — re-scope it so it tolerates cross-BLAS
  ULP noise but nothing larger.

## The defect, with measured evidence

`generate_hq1024.py` (and the other regenerate-and-compare steps) recompute the
coefficients with `numpy`/`scipy.signal` and assert the generated Rust file is
**byte-identical** to the checked-in file, including FNV1a64 hash constants
computed over the exact coefficient bits. On this machine the regenerated file
differs from the checked-in file, so `require_identical(...)` raises at step 1
and the run aborts.

We proved this is **hardware/BLAS floating-point drift, not a version problem**:

- Recorded provenance (in `qualification/fast066_numerical_probe_report.json`):
  `python_version 3.13.5`, `numpy_version 2.3.5`. **scipy version is not
  recorded anywhere.**
- We reproduced with uv on **python 3.13.5 + numpy 2.3.5** (the exact recorded
  numpy) and swept **scipy 1.16.0, 1.16.1, 1.16.2, 1.17.0, 1.18.1**, with
  `OPENBLAS_NUM_THREADS=OMP_NUM_THREADS=MKL_NUM_THREADS=1` to remove threading.
- **Every one of those combinations produces byte-identical output on this box**
  (we `cmp`-verified the scipy sweep), and all of them differ from the checked-in
  file by the same amount. nixpkgs' numpy 2.4.2 / scipy 1.17.0 shows the same
  1588-line, ≤4.5e-16 difference (same diff count, same max magnitude; we did not
  byte-compare it against the uv builds). So the package version is
  not the differentiator — the machine is.
- Magnitude of the difference (regenerated vs checked-in
  `hq1024_coefficients.rs`): of **9,179** coefficient floats, **6,420 differ**,
  every one by **≤ 4.542e-16 absolute** (~2 ULP at magnitude 1). The FNV1a64
  hash constants differ completely because they hash the exact coefficient bits,
  so a 2-ULP wobble anywhere flips every hash and forces byte-inequality.

Mechanism: `scipy.signal`'s filter design is LAPACK-backed; numpy/scipy PyPI
wheels bundle OpenBLAS, which dispatches CPU-microarchitecture-specific SIMD
kernels. The coefficients were minted on different silicon (or a different BLAS
build) than this host, so the last ULP lands differently. This is not fixable by
pinning package versions; only the identical CPU + BLAS build would reproduce it.

Conclusion: byte/FNV-exact regeneration is a "reproduce on the mint machine only"
check. It cannot serve as a portable gate. The portable validator is the Rust
frozen-checksum integrity test (`hq1024_coefficients::integrity_tests::
frozen_descriptor_tables_match_their_checksums`), which passes.

## Scope — likely more than one step

The orchestrator aborts at the first regen step (`generate_hq1024.py`), so we
only *directly* observed HQ1024. Please audit every regenerate-and-compare step
for the same non-portability and fix them together. We verified the import map:
the orchestrated steps that use `numpy`/`scipy` floating-point regeneration —
and therefore carry the same non-portability — are `generate_hq1024.py`
(numpy+scipy), `verify_certified_search.py` (numpy+scipy), and
`generate_fast_metadata.py` (numpy). Two more numpy-based minters exist but are
**not** invoked by `run_offline_qualification.py` today —
`design_headroom64_filter.py` (numpy+scipy) and `screening_probe.py` (numpy) —
and carry the same caveat if ever re-run.

Everything else in the orchestrated run is already hardware-independent and
should **stay exact**: the qualified-prefix steps (`generate_qualified_prefix.py`,
`verify_qualified_prefix.py`) use `mpmath`/stdlib (arbitrary-precision, not
BLAS-backed), and the raw-screen steps (`generate_raw_screen_metadata.py`,
`test_raw_screen.py`, the `verify_fast_scan.py` source audit) are integer/exact.
Do not loosen those.

## Candidate approaches (your choice — not prescriptive)

- Compare regenerated vs checked-in coefficients within a **tight absolute
  tolerance** (e.g. a small multiple of the ~4.5e-16 observed drift, comfortably
  below any real-error scale) instead of byte/FNV-exact; drop or tolerance-gate
  the hash-equality assertions for FP-derived data.
- Or verify the checked-in coefficients **satisfy their defining mathematical
  property** to high precision (interpolation/half-delay conditions, response
  mask, L1/curvature bounds) rather than byte-reproducing one FP computation —
  arguably stronger and inherently portable. `mpmath` is available and is
  hardware-independent if you want a high-precision oracle.
- Keep exact byte comparison only for artifacts that are genuinely
  deterministic across platforms (integer/rational/hash-of-integer data).

## Environment / how to run

`numpy`, `scipy`, `mpmath` are now in the flake dev shell:

```
nix develop --extra-experimental-features 'nix-command flakes'
python3 -B crates/tonepoet-true-peak/qualification/run_offline_qualification.py
```

This host: x86_64-linux, not the Xeon E5-1680 v2 target, not the mint machine.

## What to return

A new bundle we overlay on the tree (same convention as before): updated
qualification scripts that make `run_offline_qualification.py` pass on conforming
hardware without touching frozen coefficients or any Rust source, plus a short
report stating, per changed step, the comparison you adopted and the tolerance
(or property) you chose and why it still catches a real regression.
