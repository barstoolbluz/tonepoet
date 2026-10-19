# TonePoet DC Shift R2C — corrective engineering handoff

**Date:** 2026-10-18  
**Base:** `TonePoet_DC_Shift_R2_FULL_SOURCE_2026-10-18.tar.gz` (the earlier *unqualified* implementation).  
**State:** Three reviewer-reported defects addressed in source; **Rust compilation, workspace tests, and integrated audio qualification remain unexecuted**. This is a corrective candidate, **not** an approved production build.

## Exact corrections

1. **Exhaustive Rust decisions** — `src/convert/pipeline/execution_evidence.rs` now names and summarizes `DecisionKind::DcCorrection` in both `decision_kind_name()` and `decision_summary()`. Those are real exhaustive-match arms, not catch-all fallbacks. No other decision meanings were changed.

2. **Verified multichannel speaker identity** — The speaker mask is captured from the *source before its Float64 intermediate can erase channel roles*, using the established DVD-Audio/stereo-fold verification policy (`verified_stereo_fold_source_layout`). `TrackSourceRef::{RegisteredEffectCarrier,PcmTruePeakCarrier,DsdTruePeakCarrier}` can retain that mask, and the final `tonepoet-pipeline::PlanRequest.raw_input_channel_layout` carries it to the FFmpeg raw-Float64 input command. The terminal uses `-channel_layout <verified mask>` as an **input-side option before `-i`**, never a count-only guess. Container ingress/egress and intermediate FFmpeg effects also bind known roles. After stereo folding, the resulting two-channel layout is `0x3`, rather than the original multichannel mask. Unsupported/unverified multichannel routes **refuse conversion** instead of silently relabeling channels; compatible Auto backend selection may use FFmpeg when required. Paths without DC correction do not add speaker probes or raw masks.

   An explicit limitation is that a *non-raw* multichannel DC carrier without a bound input mask is conservatively refused by the terminal planner; routes dependent on a SoX-only or WavPack-hybrid terminal are not claimed to be automatically repaired. In particular, this correction does not rewrite DVD-Audio reorder/fold logic.

3. **Decision before execution** — The typed planner's `DcBaseline` observation and `DcCorrection` decision are read into `DcEffectDependency` while freezing the registered-effect execution contract, before any samples are touched. The dependency's scoped identity, subject, read contract, and policy revision are validated and aligned by effect position. The executor first calls `dc_shift::resolve` to complete measurement (Auto only) and select **immutable per-channel fixed subtraction values**. It then calls `dc_shift::apply_resolved` with those values; the writer cannot recalculate the policy. Input SHA-256 is verified before publishing correction; an automatic *no-change* decision also verifies input identity. The receipt carries the original typed dependency, and evidence projection refuses a missing/mismatched binding instead of manufacturing one post hoc. Manual uses fixed values without measurement. The original numerical estimator, four-mode TUI, preset migration, and high-pass logic are retained.

## Regression coverage added

- `dc_shift.rs`: Auto resolves each of three channel offsets without writing output; fixed correction matches sample arithmetic; stale input refuses both corrected output and automatic skipped output.
- `plan_bridge.rs`: Auto's registered-effect execution contract contains an exact scoped observation/decision binding; Manual has no Auto dependency.
- `tonepoet-pipeline/src/plugins.rs`: terminal command binds 3.0 (`0x7`), quad (`0x33`), and the DVD-Audio assignment-18-style five-speaker mask (`0x3b`) **before raw input**; invalid/mismatched masks are refused.
- `validation/dc_shift_r2c/static_regression_audit.py`: 13 source consistency assertions, including explicit planner-request initializer coverage and all touched carrier patterns.
- `validation/dc_shift_r2c/ffmpeg_speaker_reproducer.py`: standalone actual FFmpeg transport/decoded-sample regression with repeatable 3.0, quad, and five-speaker fixtures. `ffmpeg_dc_layout_results.json` contains the measured output layouts and bitwise-sample comparisons.

## Checks genuinely executed here

- `python3 validation/dc_shift_r2c/static_regression_audit.py` — **13 / 13 PASS** (source structure only, not a Rust parser/typechecker).
- `python3 validation/dc_shift_r2c/ffmpeg_speaker_reproducer.py` — **3 / 3 PASS**: output 3.0, quad, and five-speaker mask `0x3b`, with decoded binary64 frames **bit-identical** to the post-subtraction raw inputs. Executed using this host's **FFmpeg 7.1.5**, not the pinned FFmpeg 8 build.
- Source diff inventoried against the previous R2 archive. Source archive SHA-256 and file manifest are generated at delivery. Original repository and non-DC source files are preserved.

**Not performed:** `nix develop`, `cargo fmt`, `cargo check`, `cargo clippy`, any Rust test including the new ones, the normal workspace gate, or end-to-end TonePoet decoding/encoding. This host has **no `nix`, `cargo`, `rustc`, or `rustfmt`**. The independent reviewer reported 33/33 pre-existing DSP tests against R2; that is the reviewer's result, **not a test rerun here**. No performance or release qualification is claimed.

## Build-host qualification required

From a clean extraction, first verify the bundle's `DC_SHIFT_R2C_SHA256SUMS.txt` in the repository root (`sha256sum -c DC_SHIFT_R2C_SHA256SUMS.txt`). Follow `CLAUDE.md` for the pinned Rust/FFmpeg environment; do **not** use a generic old system Rust compiler.

```sh
nix develop --extra-experimental-features 'nix-command flakes'
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p tonepoet-pipeline
cargo test --workspace --no-fail-fast --exclude tonepoet-true-peak
cargo test -p tonepoet-true-peak
cargo build --release
python3 validation/dc_shift_r2c/static_regression_audit.py
python3 validation/dc_shift_r2c/ffmpeg_speaker_reproducer.py
```

Then run *actual TonePoet* conversions for 3.0/quad and DVD-Audio assignment 18 through Manual and Auto (verify decoded speaker identity and arithmetic), after-fold stereo, before/after resampling, normal and certified true-peak, DSD multichannel, HDCD, and PCM terminal dither. Exercise the FFmpeg 8 pinned build and the selected terminal, plus cancellation/failed observation and the exact evidence records. Confirm Off and legacy high-pass retain previous routes/performance. As the original R2 brief requested, profile source-decode counts, carrier passes, memory and short/long wall time for all four modes. Fix any actual compile or integration errors and repeat the complete gate **before** approval.

## Known R2 limitations intentionally not disguised

- Continuous CUE programme members have no album-wide automatic common DC estimator. R2's conservative `ContinuousScopeUnavailable` behavior still measures and skips corrections rather than introduce per-track boundary steps.
- The automatic stability policy is conservative evidence, not diagnosis of hardware DC. No new speculative frequency classifier was introduced.
- FFmpeg 7.1.5 transport verification is not proof of the pinned build, Rust compilation, or source-specific conversion routes.

## Minimality and resume

This corrective set only changes the earlier R2 implementation and the constructors needed for new source-mask fields; it does not replace the scheduler, true-peak engine, stereo fold, or any codec. See `DC_SHIFT_R2C_SOURCE_CHANGE_INVENTORY.txt` for the current-R2 delta and `DC_SHIFT_R2C_RESUME_CHECKPOINT.md` for restart instructions. The R2 handoff and old `DC_SHIFT_R2_SHA256SUMS.txt` remain in the tree as historical evidence; **the old R2 manifest is expected to fail** against this corrected source. Use only `DC_SHIFT_R2C_SHA256SUMS.txt` for R2C verification. This document supersedes R2 for the three reported defects. Do not replay throwaway construction scripts on an already corrected checkout.
