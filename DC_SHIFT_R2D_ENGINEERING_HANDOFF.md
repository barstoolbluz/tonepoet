# TonePoet DC Shift R2D — initial certified terminal-admission corrective

**Date:** 2026-10-18  
**Input archive:** `TonePoet_DC_Shift_R2C_FULL_SOURCE_2026-10-18.tar.gz`  
**Input SHA-256:** `3cb75b870a5dce6f149f7cab3bf85e367df300a2eae21d49408c90307746ce70`  
**Status:** Narrow source-level correction implemented. **Not Rust-build-qualified**; no TonePoet executable was run. Do not approve without the build-host gate below.

## Defect and correction

R2C correctly retains the verified multichannel speaker mask through the native Float64 DC carrier and binds it to FFmpeg's raw input. But for an undithered FLAC Int24 terminal, the **initial** certified true-peak typed plan can select SoX under Auto (higher support rank). The terminal proof/realization is then frozen. On the **final** raw carrier, `plan_request_for_track_impl()` rejects that frozen SoX choice because the verified speaker roles cannot be attached to SoX's raw input. The original user preference was Auto; the late refusal should not occur when a qualified FFmpeg direct terminal exists.

**Change:** `tonepoet-pipeline/src/semantic_plan.rs` now applies an explicit, candidate-level raw-layout capability requirement **before** `selected_candidate_index()` and the certified terminal proof selection, but only when all of these are true:

- The initial plan is a certified PCM true-peak plan (not DSD).
- The typed terminal-input signal remains multichannel (>2 channels); a prior stereo fold therefore does *not* trigger it.
- The registered effect chain includes Auto or Manual DC subtraction.

The required contract is admitted only for direct FFmpeg PCM terminals without WavPack hybrid, and already-supported direct FFmpeg lossy encoder-input terminals. It is *not* granted to SoX, a SoX preterminal plus FFmpeg package, or hybrid routes. Auto may now select a terminal with a valid FFmpeg proof at the initial certification boundary. An incompatible explicit backend is refused there; absent a qualified layout-capable terminal, normal candidate admission refuses rather than substituting SoX. This does **not** modify the selected true-peak proof, reset a frozen tool at final lowering, or attempt a new source/layout inference.

**Unchanged:** `src/convert/pipeline/plan_bridge.rs` production terminal pinning and final-phase mask assertion; `tonepoet-pipeline/src/plugins.rs` FFmpeg input-side `-channel_layout`; native DC measurement, decision, subtraction and evidence; stereo fold; high-pass; encoder/dither algorithms; and Off fast paths.

## Regression tests added (Rust — written, NOT EXECUTED)

1. `tonepoet-pipeline/src/semantic_plan.rs`: 3.0 and quad, Auto and Manual DC, FLAC Int24, Auto backend, Track Guard. Assertions: initially unresolved raw layout; selected FFmpeg direct terminal; selected candidate owns a true-peak proof and the new layout capability.
2. Same module: explicit SoX is refused at initial admission; removal of the FFmpeg terminal proof via the existing test-only candidate policy makes initial admission refuse with `no_admitted_candidate`.
3. Same module: no DC, legacy 2 Hz high-pass, and stereo fold plus DC retain their previously selected SoX terminal and do not acquire the multichannel raw-input requirement.
4. `src/convert/pipeline/plan_bridge.rs`: 3.0/quad, Auto/Manual DC; bind typed initial source and certified execution, carry the selected FFmpeg terminal and verified mask into a synthetic prepared true-peak carrier, assert the final command is FFmpeg with `-channel_layout` **before `-i`**. This is a planner/lowering fixture, not an actual decoded-audio conversion.

## Actual checks executed in this environment

- R2C existing source/static regression checks: **13/13 PASS**.
- Existing stand-alone FFmpeg raw Float64 -> Float64 WAV speaker mapping/sample-identity checks: **3/3 PASS** (3.0, quad, `0x3b`).
- Existing `validation/channel_dc_smoke.py`: **13 PASS**.
- Existing `validation/channel_layout_binding_corrective.py`: **14 decoded-audio fixture cases PASS**.
- Existing `validation/dvda_stereo_fold_corrective.py`: **6 DVD-A fixture cases PASS**.
- New `validation/dc_shift_r2d/ffmpeg_flac_multichannel_terminal.py`: **2/2 PASS**. Real FFmpeg raw Float64 -> FLAC Int24 -> Float64 decode preserves 3.0/quad layouts, center-only/rear-left-only channel occupancy, and 24-bit quantization-level sample accuracy; output reports `bits_per_raw_sample=24`. **Host FFmpeg, not TonePoet**. The sample is already below the peak ceiling, so this does *not* verify the certified TonePoet gain calculation.
- R2C manifest comparison (before new documentation): the only source files modified are the two Rust files listed above. Other source and unrelated audio code preserved; generated Python bytecode restored to R2C for reproducible archive integrity.

**Unavailable and NOT run:** `nix`, `cargo`, `rustc`, `rustfmt`, `cargo check`, `cargo fmt`, `cargo clippy`, Rust unit and workspace tests, pinned FFmpeg build, end-to-end TonePoet FLAC conversion, certified true-peak ceiling calculation, cancellation and performance gates. Network cannot resolve `static.rust-lang.org`, so obtaining the pinned toolchain here was not feasible. The new Rust tests remain unexecuted. **Do not construe the Python/FFmpeg tests as compiling this patch.**

## Build-host acceptance and gate

Use the repository's `CLAUDE.md` and pinned Nix shell. From a clean unpack, run:

```sh
sha256sum -c DC_SHIFT_R2D_SHA256SUMS.txt
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
python3 validation/dc_shift_r2d/ffmpeg_flac_multichannel_terminal.py
python3 validation/channel_dc_smoke.py
python3 validation/channel_layout_binding_corrective.py
python3 validation/dvda_stereo_fold_corrective.py
```

Then run a **real** TonePoet 3.0 FLAC (center-only track) -> FLAC Int24 with Auto backend, Manual and eligible Auto shift and Track Guard, and the analogous quad case. Prove, from actual initial and final planner evidence and decoded output, that both charged/final terminals are FFmpeg, layout masks remain `0x7`/`0x33`, center/rear-only samples are in their original channels, effective subtraction and measured/resolved evidence are exact, and the certified output ceiling holds. Exercise a negative cell without a qualified layout-capable terminal (refuse at **initial** admission), and Off, high-pass and stereo-fold controls. Investigate/fix only actual compiler or integration failures, preserving previously qualified paths. `CLAUDE.md` has instructions for the long-running workspace test gate.

## Historical records

The archive includes older R2/R2C SHA256SUMS and handoffs **as historical documents**. Their file hashes are expected to disagree with subsequently corrected source; **only `DC_SHIFT_R2D_SHA256SUMS.txt` certifies this R2D archive**. The R2C source remains unchanged except for the two explicitly identified Rust files. `DC_SHIFT_R2D_RESUME_CHECKPOINT.md` records precise continuation instructions.
