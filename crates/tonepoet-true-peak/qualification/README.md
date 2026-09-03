# Headroom64x offline filter-design tooling

Correctness gates for `tonepoet-true-peak` live in ordinary Rust tests. Nothing in this directory is required to build, test, install, or run the crate or Tonepoet.

The only retained Python program is `design_headroom64_filter.py`. It is optional **offline developer tooling** for a future intentional redesign/regeneration of the checked-in 384-tap Type-II equiripple half-sample filter. It uses NumPy/SciPy to reproduce the design, inspect the complete 64-phase cascade response, and run the historical deterministic analytical search that informed the published Headroom64x constants.

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
