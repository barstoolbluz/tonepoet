# Qualification portability corrective report

Date: 2026-09-10

Scope: offline qualification only. No Rust source, public API, algorithm revision,
frozen coefficient source, or checked-in qualification JSON was changed.

## Outcome

`qualification/run_offline_qualification.py` no longer requires a fresh
NumPy/SciPy HQ1024 regeneration to reproduce the mint machine's exact binary64
bits. It still rejects algorithm/geometry drift and keeps deterministic
qualification artifacts exact.

## Changed steps

### `generate_hq1024.py` regeneration cross-check

The generator itself is unchanged. The runner now compares its generated Rust
semantically:

- all non-floating structure and data remain exact;
- coefficient array shapes remain exact;
- exact-zero support must remain exact-zero;
- coefficient signs must not change;
- every nonzero coefficient must be within `2.0e-15` absolute of the frozen
  value;
- generated and frozen FNV1a64 values are each checked against their own array
  bits, rather than requiring cross-machine FNV equality;
- `HQ1024_MEASURED_OPERATOR_LINF` is treated as a derived diagnostic and may
  differ by at most `2.0e-12` absolute.

The `2.0e-15` coefficient envelope is about 4.4x the measured cross-machine
maximum (`4.542e-16`) from the diagnostic brief. It is intentionally tiny, but
leaves room for another conforming BLAS/LAPACK implementation. A coefficient
change larger than that still fails even if the regenerated hash is internally
self-consistent.

The generated `hq1024_candidate_coefficients.json` is compared as follows:

- `construction`, `geometry`, and `lagrange_exact` remain exact;
- `measured` values may differ by at most `2.0e-12` absolute;
- FNV fields must match the corresponding generated/frozen Rust file's own
  coefficient bits;
- `full_sha256` remains required and well-formed, but is not compared across
  machines because it hashes a floating-derived full response.

### `verify_certified_search.py`

The independent HQ construction check now compares regenerated first-stage and
tail coefficients to the frozen source with the same `2.0e-15` absolute
coefficient envelope. The old regenerated-tail-vs-frozen-FNV equality was
removed because that was exactly the machine-specific bit-identity defect.

The frozen FNV still has authority over the frozen source: the verifier now
checks the checked-in tail bits against the checked-in FNV directly. Exact
outward dyadic-node validation and deterministic bound stress are evaluated on
the frozen tail bank, which is the runtime authority, rather than on a
last-bit-different regeneration.

The runner compares the generated `certified_search_report.json` with exact
structure/non-floating values and tight numeric diagnostic tolerances of
`2.0e-12` absolute plus `2.0e-13` relative. This report comparison is secondary:
`verify_certified_search.py` must still satisfy all of its existing hard
algorithmic predicates, and its regenerated HQ coefficient check is constrained
at `2.0e-15`.

### `generate_fast_metadata.py`

Audited, not changed. Although it imports NumPy and imports the HQ generator
module, it does not regenerate the SciPy filter design. It reads the frozen Rust
bank, uses exact `Fraction` arithmetic for dyadic checks, and scalar
`math.nextafter` accumulation for L1 metadata. Its checked-in JSON therefore
remains byte-exact in the runner.

### `design_headroom64_filter.py`

This tool is not called by `run_offline_qualification.py`, but it contains the
same class of fresh-SciPy-vs-frozen coefficient check. Its previous `5e-16`
absolute threshold is replaced by the shared `2.0e-15` portability envelope.
The rest of its authority/accuracy checks are unchanged.

### `screening_probe.py`

Audited, not changed. It uses NumPy to analyze a supplied carrier but does not
regenerate and byte/FNV-compare a frozen floating-point design artifact, so the
reported portability defect does not apply to it.

## Regression protection

Added `qualification/test_portable_compare.py`. It proves that the comparison
layer:

- accepts representative last-bit drift;
- accepts different FNV values when each file's hashes are self-consistent and
  coefficients remain within tolerance;
- rejects a coefficient delta beyond `2.0e-15`;
- rejects exact-zero support changes even below that tolerance;
- rejects coefficient sign changes even below that tolerance; and
- rejects non-floating geometry changes.

## Validation performed here

Environment:

- Python 3.13.5
- NumPy 2.3.5
- SciPy 1.17.0
- mpmath 1.3.0
- x86_64 Linux

Results:

- `python3 -B crates/tonepoet-true-peak/qualification/run_offline_qualification.py`
  passed twice consecutively.
- Portable-comparison regression suite: 6/6 passed on each orchestrated run.
- Raw-screen Python tests: 4/4 passed.
- Commissioning helper tests: 9/9 passed.
- `design_headroom64_filter.py` completed successfully.
- All frozen coefficient sources and checked-in JSON artifacts were byte-compared
  against the input bundle and are unchanged.
- The entire `crates/tonepoet-true-peak/src` tree is unchanged.

The two Rust release suites could not be rerun in this execution environment
because `cargo` is not installed. No Rust source was changed by this corrective
round.
