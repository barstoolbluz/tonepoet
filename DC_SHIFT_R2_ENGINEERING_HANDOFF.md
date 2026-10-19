# TonePoet DC Shift R2 - engineering handoff (2026-10-18)

## Status and qualification

**IMPLEMENTED AS SOURCE; NOT BUILD-QUALIFIED. DO NOT MERGE OR SHIP UNTIL THE PINNED BUILD GATE PASSES.**

This tree derives directly from the user-supplied `TonePoet_SNAPSHOT_2026-10-18.tar.gz`, SHA-256 `ba4e5c6d7b73fd387886e9cde818fc630c019e89f778041952e7b8c2006120a0`. No older source patch was applied. The fresh tree's `CLAUDE.md` was inspected.

The working host does not have `nix`, `rustc`, `cargo`, or `rustfmt`, and network/package installation is unavailable. Consequently **no Rust code has been compiled, no Rust tests have been run, and the 7,383-test workspace baseline/regression gate has not been run**. The full implementation is a concrete code bundle, not a proof of production correctness. Source-level review and one distinct FFmpeg transport probe were performed as described below. Claims about absent test results are intentionally not made.

## Source integration points

- `src/tui/app.rs`: four-mode `DcCorrectionMode` pill, Off default, signed fixed nanounits (1e-9 of normalized PCM full scale), strict Manual editor parser, extra Manual value row only in Manual mode. `RemoveDcOffset` remains the existing focused-row identity.
- `src/tui/draw_output.rs`, `convert_screen.rs`, `button_map.rs`, `keybindings.rs`: four rendered clickable pills, width-adaptive labels, literal U+24D8 `ⓘ` with the existing two-cell contrasting foreground treatment; independent two-cell hitbox; existing scrollable Notice and keyboard route; exact layperson message from the brief. Tests added to TUI code.
- `src/tui/presets.rs`: new `dc_correction_mode` and `dc_manual_nanounits` keys; absent modern key uses legacy `remove_dc_offset` boolean, with `true` interpreted as 2 Hz High-pass; new round-trip tests.
- `src/tui/command.rs`: conversion request assembly replaces the old output-processing intent idempotently with one registered effect: old 2 Hz `RemoveDcOffset`, new `DcShiftAuto`, or `DcShiftManual { offset_nanounits }`, arranged after the optional stereo fold. Off inserts nothing.
- `tonepoet-pipeline/src/semantic_plan.rs`: typed `DcBaseline` observation on the exact `ApplyEffect.input` PCM signal, `DcCorrection` decision with a scoped observation dependency, and explicit `dc_decision` on its effect node. Manual has fixed parameter and no observation. Native lowering rejects incompatible bounds and encodes policy version.
- `tonepoet-pipeline/src/fingerprint.rs`: DC intent, manual parameter, observation, dependency, lowering, and policy revision bound into effect/plan identities. Legacy non-DC effects keep their old fingerprints.
- `src/convert/pipeline/dc_shift.rs`: bounded, sequential Float64 interleaved sample iteration, compensated mean, Welford AC-RMS, 16 temporal windows, conservatively classified per-channel correction, fixed double-precision subtraction, same-write output statistics and SHA-256 check against measured input. No integer DSP, sample clamping, limiter, dither, second decoder, or independent scheduler.
- `src/convert/pipeline/stages.rs`: registered-effect chain realizes native DC through the existing Float64 carrier plus FFmpeg `anull` container transportation where necessary. Raw correction itself is a native blocking worker with cancellation. DSD and ordinary/certified true-peak effect-carrier routes use the same registered-effect realization; active effects cannot be documented as executed without a native receipt. Terminal overload is refused where neither user-selected gain/true-peak protection nor floating PCM is available.
- `src/convert/pipeline/execution_evidence.rs`: original projection API preserved; extended projector emits actual per-channel observation/decision/native correction records, frame/rate/sha/sign, and changed-vs-unchanged status. Fails rather than inventing successful DC execution when receipt/dependency is absent.

## Conservative automatic decision policy

Each channel uses all frames and 16 proportional-duration temporal windows. It requires at least `max(2 * sample_rate, 32768)` frames, finite samples, a minimum non-silent peak and AC-RMS (`1e-8`), an observed |mean| >= `1e-9` and `|mean| / AC-RMS >= 0.015`, and a maximum 16-window mean deviation not exceeding `max(0.20*|mean|, 0.02*AC-RMS)`. Decisions are `Apply`, `Negligible`, `Unstable`, `Indeterminate`, or `ContinuousScopeUnavailable`. Such rules support an operational estimate, **not proof of a recording defect**. If no channel is `Apply`, Auto does not subtract anything, reports why, and returns an unchanged carrier. Manual positive `+0.003` **subtracts** 0.003, on all channels, without premeasurement.

This is a deliberately basic policy: it cannot identify every legitimate low-frequency/asymmetric recording. Verify false positives on real content before production use.

## CUE continuity limitation

Continuous CUE-image segments have no finished album-wide DC estimator in this change. When the existing source reference identifies a continuous CUE segment, Auto still measures, but an otherwise-applicable *per-member* subtraction is conservatively skipped with `ContinuousScopeUnavailable`; it does **not** impose a potentially audible discontinuity at adjoining track boundaries. Independent tracks can use Auto individually; Manual uses one user-specified value on every member. This deliberately does not claim the brief's preferred album-wide correction capability; determine whether existing album coordination can safely provide a shared correction estimate during build-host qualification. No new album service was introduced.

## Carrier and peak-policy limitations

Native arithmetic works on little-endian interleaved Float64 raw staging data, not FFmpeg 8 `dcshift` (S32P) or SoX integer-core DSP. The existing FFmpeg command planner moves Float64 WAV to/from raw using `anull` and `pcm_f64le`. On **host FFmpeg 7.1.5**, an independent 480,000-frame stereo round trip (7,680,000 bytes) was bit-identical, including samples with amplitudes > 1; raw->WAV 0.0973 s, WAV->raw 0.0843 s, no source decode. This is a **transport smoke test only**, not a pinned FFmpeg 8 or TonePoet conversion test. See `DC_SHIFT_R2_TRANSPORT_DIAGNOSTIC.txt` and `tools/dc_shift_transport_probe.py`.

A corrected carrier with peak above full scale rejects an unprotected integer/lossy terminal. Actual full-route true-peak, HDCD, DSD, ReplayGain, quantization, dither, lossy and lossless rendering need build-host audio fixtures to verify ordering and avoidance of hidden peak changes. No proof of these route contracts is implied by the standalone transport probe.

## Verification actually executed

1. Baseline source archive unpacked and its SHA-256 recorded: `ba4e5c6d7b73fd387886e9cde818fc630c019e89f778041952e7b8c2006120a0`.
2. Verified code modification set against source archive by per-file SHA-256: 12 existing files modified, one Rust module added, none removed. See `DC_SHIFT_R2_SOURCE_CHANGE_INVENTORY.txt`.
3. Inspected pinned project instructions and route source, plus final new code sections; no compiler-based verification possible.
4. Independent FFmpeg 7.1.5 Float64 transportation test: **PASS, bit-identical**, 480,000 frames x 2 channels including > full-scale values; details above.
5. Rust compile, format, clippy and tests: **NOT EXECUTED / NOT QUALIFIED** (no Nix/Rust toolchain in host).
6. Four-mode full pipeline throughput/peak memory/number-of-source-decodes: **NOT MEASURED**. Isolated transportation-only subprocess timings are not substituted for pipeline benchmarks.

## Required next gates on pinned build host

Run at the repository root, using the exact `CLAUDE.md` toolchain recipe:

```sh
nix develop --extra-experimental-features 'nix-command flakes'
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p tonepoet-pipeline
cargo test --workspace --no-fail-fast --exclude tonepoet-true-peak
cargo test -p tonepoet-true-peak
cargo build --release
```

The `CLAUDE.md` notes the workspace gate can exceed normal foreground execution windows; run it with persistent session/logs on that host. Fix any compilation, pre-existing gate divergences, or new test failures on the **fresh** repo; don't change unrelated behavior to force qualification. Then run decoded-sample fixtures exercising the planner's physical route, including a positive and negative per-channel bias, signed Manual input, >1 Float64 intermediate, nonrepresentable integer terminal, CUE direct-stream admission, certified true peak, DSD materialization, HDCD, de-emphasis, stereo fold with explicit speaker roles, terminal dither, and cancellation. Benchmark Off/High-pass/Auto/Manual, short and long tracks, under the pinned toolchain and provide process/copy and carrier I/O counts. Audit concise `conversion.log` and structured execution record against the real failed/skipped/successful jobs.

## Resumption / interruption

This archive is **a complete source tree**. Extract it to a clean directory. Read `DC_SHIFT_R2_CHECKPOINT.md`, this handoff, `DC_SHIFT_R2_SOURCE_CHANGE_INVENTORY.txt`, and the original prompt at `DC_SHIFT_R2_IMPLEMENTATION_BRIEF.md`. Verify `DC_SHIFT_R2_SHA256SUMS.txt` for content integrity. The former in-container patch scripts were *construction tools*, not required to resume; **do not replay them over this already modified tree**. Continue qualification and focused corrections from this snapshot.
