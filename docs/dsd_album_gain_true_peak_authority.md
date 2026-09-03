# DSD album-gain true-peak measurement

## Scope

This note documents the Tonepoet-side use of the standalone `tonepoet-true-peak` crate for DSD album auto-gain. It does not change the separately certified DSD Reference measurement contract or its policy versions.

## Production path

Album-scoped DSD analysis retains the existing reconstruction flow: each selected DSD track is decoded once to a headerless little-endian Float64 PCM carrier at the final requested PCM rate, the carrier length is validated for complete frames, and the retained carrier is scanned sequentially on the blocking worker with bounded memory and cancellation checks. The `Headroom64x` meter receives the known sample rate and channel count and reports the maximum across channels.

Tonepoet maps the meter result directly into the existing `AlbumPeakMeasurement` boundary:

- true digital silence remains `Silence`;
- a finite Headroom64x dBTP point remains that finite dBTP value (rounded only to the existing `DbNano` representation);
- values above full scale are not clamped.

There is no DSD-album-gain commissioning stamp, executable hash check, profile commissioning set, runtime `-30 dBTP` floor, chain reserve, runtime commissioning error, or replacement warning. A normal build can use DSD album auto-gain without a prior commissioning run.

## Why the DSD caller does not use `headroom64x_authority()`

The standalone crate publishes a `0.030 dB` Headroom64x reserve only when a caller can state that spectral support is at or below `0.495 * Fs`. SoX-ng `rate -u` does not establish that hard-support property: its passband description does not prove the reconstructed carrier contains no transition-band content above that limit.

The DSD album-gain path therefore does not manufacture a maximum-frequency declaration merely to promote the point through `headroom64x_authority()`. It consumes the Headroom64x point estimate directly. The standalone band-qualified authority API remains available unchanged for callers that genuinely know their signal support.

## Correctness and regression protection

Correctness is a normal Rust-test property. `cargo test -p tonepoet-true-peak` and `cargo test --workspace` cover the published Headroom64x analytical cases and bounds, libebur128-compatible Reporting4x reference values, a checked-in real-program-material regression, streaming/chunking equivalence, finite-stream edges, silence, multichannel behavior, and above-full-scale input. The checked-in Headroom64x coefficient table also has a std-only count/`f64::to_bits()` integrity checksum test.

The frozen Reporting4x values in the Rust tests were established independently with libebur128 1.2.6. The real-program fixture additionally freezes a Reporting4x libebur128 result and a 256x libsoxr observation for Headroom64x; it is a regression cross-check rather than proof of the `0.030 dB` authority reserve. Analytical Headroom64x cases use their continuous-time truth directly. Ordinary tests consume only checked-in PCM bytes and require no SoX, FFmpeg, libsoxr, libebur128, Python, Nix, network access, decoder, or real DSD corpus.

## Offline filter design

`crates/tonepoet-true-peak/qualification/design_headroom64_filter.py` is retained only as optional offline developer tooling for regenerating and auditing the 384-tap Headroom64x first-stage filter. It requires NumPy/SciPy when a developer intentionally runs it. Cargo, `build.rs`, the Rust tests, Tonepoet runtime, and `flake.nix` do not invoke it. It is not a release, commissioning, build, test, or runtime gate.
