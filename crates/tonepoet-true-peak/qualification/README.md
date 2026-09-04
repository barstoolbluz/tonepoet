# Headroom64x offline filter-design tooling

Correctness gates for `tonepoet-true-peak` live in ordinary Rust tests. Nothing in this directory is required to build, test, install, or run the crate or Tonepoet.

`design_headroom64_filter.py` is optional offline tooling for intentional redesign/audit of the checked-in 384-tap Type-II equiripple half-sample filter. `verify_ceiling_contract.py` is separate optional qualification tooling for the finite hard-ceiling reconstruction. `verify_fast_headroom_paths.py` independently rebuilds the exact 16x and 8x fast execution paths, derives their point-authority component budgets, runs deterministic exact-peak searches, and recomputes the induced-norm bridges back to the unchanged full-64x finite reconstruction. The tools use NumPy/SciPy only when invoked manually.

It is not invoked by Cargo, `build.rs`, Rust tests, runtime code, or `flake.nix`, and it is not a release or commissioning gate. The checked-in coefficients are protected during normal development by a std-only Rust integrity test over the coefficient count and exact `f64::to_bits()` values.

For an intentional offline design audit, a developer may run:

```sh
python3 crates/tonepoet-true-peak/qualification/design_headroom64_filter.py \
  --source crates/tonepoet-true-peak \
  --report /tmp/headroom64-design.json
```

The normal regression suite remains:

```sh
cargo test -p tonepoet-true-peak
cargo test --workspace
```

Those Rust tests carry the lasting correctness evidence: analytical three-tone and upper-band five-tone truths, the explicit out-of-domain R3 near-Nyquist diagnostic, deterministic upper-band and multitone families, frozen adversarial cases, the formula-derived 64x grid bound, Reporting4x values frozen from libebur128 1.2.6, streaming equivalence, finite-stream boundaries, silence, multichannel maxima, and above-full-scale behavior. They also include `tests/fixtures/real_reference_48k_stereo.f64le`, a small redistributable excerpt of genuine recorded saxophone material. Its test freezes a libebur128 1.2.6 Reporting4x result and a 256x libsoxr Headroom64x cross-check; the reference tools are not executed by Cargo. See `tests/fixtures/README.md` for provenance, licensing, exact hashes, and reference-generation details.

## Hard-ceiling reconstruction audit

`verify_ceiling_contract.py` is a second optional offline audit for the album
NormalizePeak ceiling work. It independently rebuilds the complete six-stage
reconstruction from the checked-in coefficient values, derives the complete
64-phase induced L-infinity norm, evaluates both finite-stream edge policies,
and freezes analytical/real-material reconstruction values used by Rust tests.
It also records high-precision dB-to-linear references for the directed fixed-
point conversion tests.

Run it manually with:

```sh
python3 crates/tonepoet-true-peak/qualification/verify_ceiling_contract.py \
  --source crates/tonepoet-true-peak \
  --report /tmp/headroom64-ceiling-contract.json
```

Like the filter-design program, it is **not** a commissioning mechanism or a
build/runtime gate. In particular, it makes no `<= 0.495 * Fs` claim for the
production DSD carrier. Its report intentionally separates the exact finite
reconstruction ceiling from comparison against the historical ideal analytical
tone truths, so interpolation-model deviation cannot be mistaken for the tiny
numeric enclosure used by the hard-ceiling proof.

## Fast headroom path audit

Run the independent fast-path qualification with:

```sh
python3 crates/tonepoet-true-peak/qualification/verify_fast_headroom_paths.py \
  --source crates/tonepoet-true-peak \
  --report /tmp/fast-headroom-paths.json
```

The audit checks the exact checked-in first-stage coefficients, independently reconstructed symmetrized Blackman prefixes, cascade delays, operation-count model, qualified-band response extrema, analytic grid miss, required analytical tones, deterministic aligned-peak cases, and the 16x-cubic / 8x-cubic bridge norms. The point-response budget is an engineering qualification over the stated band, matching the existing Headroom64x methodology; it is not presented as a theorem for arbitrary critical-Nyquist content. The finite-reconstruction bridge constants are independently recomputed induced-L-infinity enclosures. Ordinary Rust tests separately compare the actual symmetric-stage runtime impulse response against that independent definition and freeze the strongest deterministic under-read cases found by qualification. They fail if the implementation drifts outside the published `0.044 dB` and `0.084 dB` one-sided point bounds or if either bridge exceeds its public enclosure. The checked-in `fast_headroom_paths_report.json` records the current audit result for review; runtime does not read it.
