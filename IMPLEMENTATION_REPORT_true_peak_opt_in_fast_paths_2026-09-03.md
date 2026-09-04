# Implementation report - opt-in faster true-peak scanning paths

Date: 2026-09-03
Requested base: `main` @ `143e672`
Scope: `crates/tonepoet-true-peak`, production album-scan selection, CLI/TUI/preset plumbing, and settings/fingerprint compatibility.

## Delivery status

Implemented the requested three-rung headroom ladder while keeping `Headroom64x` as the default/reference path and keeping `Reporting4x` outside the ladder.

The implementation container has no Rust or Nix toolchain. This delivery is therefore source-audited and independently DSP-qualified here, but it is not claimed to be Cargo/Nix compile-certified or performance-measured. The operator release gate at the end of this report is mandatory.

## Final modes

| User rung | Point meter | Declared one-sided under-read bound | Calibration | Default |
|---|---|---:|---:|---|
| Reference | Headroom64x | 0.030 dB | existing | yes |
| Fast | Headroom16x | 0.044 dB | +0.007 dB | no |
| Fastest | Headroom8x | 0.084 dB | +0.088 dB | no |

All three headroom point authorities are qualified only through 0.495 * Fs. The opt-in modes reuse prefixes of the frozen Headroom64 interpolation cascade. `Reporting4x` is unchanged and is not exposed as a speed tier.

`HeadroomScanMode::default()` is `Reference`. A non-album auto-gain scope canonicalizes the scan mode back to `Reference`, including deserialization of stale or hand-authored settings. Presets omit the hidden fast selection when the selected gain regime/scope cannot use it.

## Point qualification

`qualification/verify_fast_headroom_paths.py` reconstructs the runtime filters independently and checks response, grid, deterministic exact-peak cases, bridge constants, and the static operation model.

Final checked-in results:

| Rung | Declared bound | Derived component budget | Margin | Worst deterministic searched under-read |
|---|---:|---:|---:|---:|
| Fast / 16x | 0.044 dB | 0.041246306703683004 dB | 0.0027536932963169933 dB | 0.031650445887747666 dB |
| Fastest / 8x | 0.084 dB | 0.08220159870397907 dB | 0.001798401296020935 dB | 0.04926380716120213 dB |

The response sweep and deterministic search are engineering qualification evidence over the stated band, consistent with the existing Headroom64 methodology. They are not represented as a theorem for arbitrary critical-Nyquist content.

Ordinary Rust regressions freeze the declared constants and strongest deterministic cases so implementation drift is not dependent on rerunning Python during normal development.

## Hard-ceiling preservation

The album hard-ceiling contract still governs the same uncalibrated finite Headroom64x reconstruction. The fast choices do not substitute a different ceiling waveform and do not use the point-estimate dB reserve to calculate hard gain.

- Reference evaluates the full 64x reconstruction directly.
- Fast evaluates the 16x prefix and uses a four-point cubic bridge to enclose the unchanged 64x reconstruction.
- Fastest evaluates the 8x prefix and uses the same bridge construction.
- The runtime envelope evaluates the two interior Bernstein controls. The interval endpoints are already represented by coarse knots.
- RepeatEndpoints and ZeroExtend remain covered because the exact induced-L-infinity coefficient error acts on an extended sequence whose magnitude remains bounded by the original sample peak.
- Incomplete/non-finite bridge state fails closed.
- The production album gain consumes the returned reconstruction upper bound (`signal_upper_linear`), not the faster point estimate.

Independently recomputed bridge norms:

| Bridge | Exact measured phase L1 maximum | Declared enclosure | Margin |
|---|---:|---:|---:|
| 16x -> unchanged 64x | 0.002850095510818164 | 0.0030 | 0.00014990448918183585 |
| 8x -> unchanged 64x | 0.0029326006842504177 | 0.0030 | 0.0000673993157495824 |

The implementation also includes an explicit binary64 numerical allowance. The pre-existing `qualification/verify_ceiling_contract.py` passes unchanged against the final tree.

## Reference-path invariance

The reference mode was deliberately isolated from fast-only execution machinery.

The following input implementation sections compare byte-for-byte identical against the supplied bundle:

- `HeadroomHalfSampleStage::process_frame` - the original first-stage Reference hot path and summation order.
- `HeadroomTwoXStage` - fields, constructor, and hot loop.
- `HeadroomEngine` - fields, constructor, finite-stream state, and full 64x execution loop.

Fast-only first-stage accumulation and later symmetric-pair execution live behind separate methods/wrappers. The Reference object does not acquire fast-only coefficient storage or fast-only hot-loop work.

The qualified frozen artifacts are byte-identical to the supplied bundle:

- `src/headroom64_coefficients.rs`: `4bfaabaf6c9688724e47d187c8c7aa267cae8b58db24a906362c7a81eabfa071`
- `tests/fixtures/real_reference_48k_stereo.f64le`: `b6ba8b041ebd87543f04f92267487937128acc9905fc743323567682ef77fd20`

## Performance design

The qualification tool derives the operation counts from the actual stage shapes and fails if its modeled topology stops matching the implementation.

For the exact production hard-ceiling scan:

| Rung | FIR coefficient products | Cubic bridge multiplies | Modeled multiplies / input frame / channel | Fraction of Reference |
|---|---:|---:|---:|---:|
| Reference | 576 | 0 | 576 | 1.000000 |
| Fast | 272 | 32 | 304 | 0.5277778 |
| Fastest | 240 | 16 | 256 | 0.4444444 |

Fastest is 0.8421053 of Fast in this multiply model.

Using only the supplied 12.7 CPU-minute Reference baseline as a proportional design extrapolation gives:

- Fast: 6.7027777777777775 CPU-minutes for a 40-minute 176.4 kHz stereo carrier.
- Fastest: 5.644444444444444 CPU-minutes.

These are designed-for figures, not measurements. The model intentionally does not pretend that additions, comparisons, absolute values, memory traffic, branch behavior, compiler vectorization, or I/O scale identically with FIR products.

`examples/bench_ceiling_f64le.rs` exercises the exact production ceiling meter for `reference`, `fast`, and `fastest`. It reports wall timing/realtime, not falsely labeled CPU time. Release acceptance must use process CPU time with the shipping Nix/Rust codegen.

## Settings, compatibility, and UX

- Existing/default settings retain `Reference` without silently inheriting a faster path.
- Historical Reference serialization is preserved by omitting the compatibility-default field.
- Historical Reference album fingerprints are preserved; only explicit Fast/Fastest choices contribute new fingerprint bytes.
- The CLI exposes `reference`, `fast`, and `fastest` and states the 0.030/0.044/0.084 dB one-sided under-read bounds.
- The TUI shows exactly the same three bounds as the user-facing distinction, not opaque interpolation-factor names.
- The scan selector is visible/active only for album-scoped automatic DSD gain.
- Malformed preset scope cannot leave a fast scan active through stale UI state.
- No F-key or plain-letter shortcut was added.

## Crate and implementation constraints

- `crates/tonepoet-true-peak/Cargo.toml` still has an empty `[dependencies]` section.
- The crate remains application-independent: no Tonepoet settings, DSD, album-gain, or pipeline type is imported into it.
- Streaming `new` / `push_interleaved` / `finalize` behavior remains; no whole-album buffer was introduced.
- No `unsafe`, `static mut`, `std::env::set_var`, or `std::env::remove_var` was added by this change.
- Added source/user-visible text is ASCII; no decorative Unicode was introduced.
- No process-global-state mutation was added to tests.

## Verification performed in this container

PASS:

1. `python3 -m py_compile` on both true-peak qualification programs.
2. `verify_fast_headroom_paths.py` with 1,000 deterministic exact-peak cases.
3. A second final qualification run produced a byte-for-byte identical report to `fast_headroom_paths_report.json`.
4. The pre-existing `verify_ceiling_contract.py` passes unchanged.
5. Frozen coefficient and real-material fixture byte identity and SHA-256 checks.
6. Byte-identity checks for the original Reference first-stage process function, `HeadroomTwoXStage`, and `HeadroomEngine` implementation sections.
7. Source diff whitespace audit.
8. Added-line audits for non-ASCII text, unsafe/process-global-state mutation, and literal F-key tokens.
9. Stale-prototype audit for discarded 32x fast-path identifiers.
10. Source-level delimiter/balance audit across every changed Rust file.

Not executable here and therefore NOT claimed:

- `cargo check`, `cargo build`, `cargo fmt`, `cargo clippy`, or any Rust test.
- Nix development-shell or workspace certification.
- Audio-tool integration runs.
- Measured process-CPU performance of the three production modes.

## Mandatory operator release gate

Run from the repository root using the project's Nix environment:

```sh
nix develop --extra-experimental-features 'nix-command flakes'

cargo fmt -- --check
cargo check --workspace
cargo test -p tonepoet-true-peak
cargo test -p tonepoet-pipeline
cargo test --workspace --no-fail-fast
cargo clippy --workspace --all-targets --all-features -- -D warnings

python3 crates/tonepoet-true-peak/qualification/verify_fast_headroom_paths.py \
  --source crates/tonepoet-true-peak \
  --report /tmp/fast_headroom_paths_report.json \
  --random-cases 1000
cmp /tmp/fast_headroom_paths_report.json \
  crates/tonepoet-true-peak/qualification/fast_headroom_paths_report.json

python3 crates/tonepoet-true-peak/qualification/verify_ceiling_contract.py \
  --source crates/tonepoet-true-peak \
  --report /tmp/ceiling_contract_report.json
```

For performance acceptance, use the same retained 176.4 kHz stereo Float64 carrier and a release build for all three modes. Warm the file cache once, then collect at least five clean process-CPU measurements per mode (user time + system time), on the same otherwise-idle machine, and compare medians. One suitable harness is:

```sh
cargo build --release -p tonepoet-true-peak --example bench_ceiling_f64le

/usr/bin/time -v target/release/examples/bench_ceiling_f64le \
  /path/to/carrier.f64le 176400 2 reference
/usr/bin/time -v target/release/examples/bench_ceiling_f64le \
  /path/to/carrier.f64le 176400 2 fast
/usr/bin/time -v target/release/examples/bench_ceiling_f64le \
  /path/to/carrier.f64le 176400 2 fastest
```

Acceptance criteria from the work order:

1. `Reference` retains existing results/contract.
2. `Fast` and `Fastest` pass the published bound regressions and hard-ceiling tests.
3. The slower fast rung is well under 7.7 CPU-minutes when normalized to a 40-minute 176.4 kHz stereo scan.
4. `Fastest` is meaningfully faster than `Fast` in actual process CPU time. If it is only marginally faster, do not ship two fast options merely because the static model predicts a separation.
5. The full workspace is green with no new warnings.

The performance gate is intentionally empirical. The implementation has been shaped to create a large static-work reduction, but release acceptance should follow measured CPU behavior, not the model.
