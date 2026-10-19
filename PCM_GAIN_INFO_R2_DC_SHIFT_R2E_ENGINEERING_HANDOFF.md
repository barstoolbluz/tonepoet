# TonePoet PCM Gain Info R2 / DC Shift R2E — Corrective Engineering Handoff

**Date:** 2026-10-18  
**Starting authority:** `TonePoet_PCM_Gain_Info_R1_FULL_SOURCE_2026-10-18.tar.gz`, SHA-256 `4aad4eb54f200894658f46ad3ba4bdf85805265ad57aef811a2237798fd44677`  
**Review addressed:** Independent PCM Gain Info R1 / DC Shift R2D source review, Findings A and B  
**Status:** Corrective source candidate. Rust build, runtime and pinned-toolchain qualification **not performed**; do not call production-qualified.

## Corrective changes

### A. Custom DSD true-peak + multichannel native DC

`tonepoet-pipeline/src/semantic_plan.rs` previously required `!request.source.is_dsd()` before attaching the verified-multichannel-layout obligation to the **initial** certified PCM terminal candidate. This excluded the Custom DSD-to-PCM split prepass, despite that prepass producing a raw Float64 multichannel terminal carrier with verified speaker roles.

The initial terminal admission now uses the actual normalized true-peak gain policy, the actual typed post-effect channel count (>2), and the registered native DC effect. It also excludes Reference delivery. The candidate's `verified_multichannel_dc_raw_input_layout` requirement must be satisfied by an admitted FFmpeg direct PCM (or eligible lossy) terminal **before** `charged_terminal` and its proof are frozen. Explicit incompatible SoX and missing-FFmpeg-proof cases refuse at admission. Neither a late backend switch nor inferred speaker roles are allowed.

The unchanged final `src/convert/pipeline/plan_bridge.rs` mask check continues to bind the actual verified `channel_layout` to raw FFmpeg input before `-i`. A new bridge regression tests the DSD carrier `0x3f`/six-channel binding and frozen-terminal identity. Typed planner tests cover both Track and Album Guard, Auto and Manual DC, and candidate refusal.

### B. SSRC resampler preference is not a PCM terminal choice

The old guard refused any `PreferredTool::Ssrc` in multichannel certified DC, including when the already-selected `ResamplePcm` step was a qualified SSRC resampler and a distinct FFmpeg terminal could be selected.

The early guard now permits this preference **only when an existing typed `ResamplePcm` node has selected SSRC**. It does not waive the same FFmpeg-only terminal layout contract, change the selected SSRC profile/attestation, or allow an unproved terminal. No rate change means no such SSRC exception. The additional lookup is short-circuited for all requests without multichannel certified native DC, leaving Off/ordinary routing unaffected.

An independently discovered prerequisite also needed a narrow correction: the new native DC effect was registered as an unspecified (`plain_transform`) effect, so the existing strong-Binary64 SSRC gate rejected it as `protected_post_resample_effect_unqualified` even when the resampler and terminal were qualified. The actual production native worker already reads finite interleaved Float64, performs `sample - resolved_offset` in Float64, refuses nonfinite output, and **never** clamps, applies a limiter, converts to integer, or dithers. Only the native `DcShiftAuto` and `DcShiftManual` effect contracts now declare this genuine Binary64/finite/no-clamp behavior; their typed output states report the same. Other effects remain unqualified for that protected chain. **No DC worker implementation or true-peak certification code was changed.**

Focused Rust tests inject an exact, scope-matching *test-only* Established Binary64 SSRC authority to exercise the positive planner branch independently of build-host commissioning. They also refuse a noncommissioned SSRC profile, no-rate-change pseudo-resampler, and an unproved FFmpeg terminal. An injected test authority does **not** grant production SSRC evidence; production admission continues to require its existing commissioned evidence/attestation and protected RIFF capacity.

## Source inventory

Compared against the starting R1 archive, exactly **two existing Rust source files differ**:

1. `tonepoet-pipeline/src/semantic_plan.rs` — early terminal admission predicates, native DC processing contract/output-state precision, and four focused Rust regression tests.
2. `src/convert/pipeline/plan_bridge.rs` — one additional DSD raw-carrier terminal/mask regression test. The production bridge code is unchanged.

All four PCM Gain Info R1 TUI Rust files and their existing UI tests, help text, glyph, hitbox and Notice behavior are **byte-identical** to R1. Also unchanged: the native DC subtraction/assessment code, DSD reconstruction, SSRC execution, true-peak measurement and error bound, channel layout/stereo fold code, dither and output publication.

New supporting material: `validation/dc_shift_r2e/` source audit, six-channel FFmpeg transport reproducer, executed logs; this handoff, and the R2E resume checkpoint.

## Actually executed verification

The following ran against the delivered working tree, with `PYTHONDONTWRITEBYTECODE=1`. Logs are in `validation/dc_shift_r2e/`.

| Command or check | Result | What it does **not** prove |
|---|---|---|
| `python3 validation/dc_shift_r2e/static_audit.py` | **16/16 PASS** | Static invariants only, no Rust compilation |
| `python3 validation/pcm_gain_info_r1/static_audit.py` | **15/15 PASS** | No real ratatui execution |
| `python3 validation/dc_shift_r2c/static_regression_audit.py` | **13/13 PASS** | No Rust type checking |
| `python3 validation/channel_dc_smoke.py` | **13 PASS** | Existing Python/FFmpeg fixtures only |
| `python3 validation/channel_layout_binding_corrective.py` | **14 PASS** | Existing FFmpeg/layout fixtures only |
| `python3 validation/dvda_stereo_fold_corrective.py` | **6 PASS** | Existing decoded DVD-A fixtures only |
| `python3 validation/dc_shift_r2c/ffmpeg_speaker_reproducer.py` | **3/3 PASS** | No TonePoet conversion |
| `python3 validation/dc_shift_r2d/ffmpeg_flac_multichannel_terminal.py` | **2/2 PASS** | No TonePoet true-peak or DC executor |
| `python3 validation/dc_shift_r2e/ffmpeg_dsd_5_1_terminal.py` | **1/1 PASS** | Synthetic *post-reconstruction* raw input only; not SACD/DSD decoding or certified true peak |
| Original-source SHA comparison with R1 archive | **Exactly 2 existing Rust files changed** | No compiler or external integration |

The new 5.1 transport fixture used the host's **FFmpeg 7.1.5**, at 176,400 Hz, raw Float64 6-channel layout `0x3f` -> FLAC Int24 -> decoded Float64. The center-only signal stayed center-only; all other decoded channels stayed silent, with maximum sample error ~1.165e-7 (expected Int24 quantization). It verifies the encoder speaker binding and a simulated manually subtracted input, not the original DSD decoder, Rust worker, or TonePoet full route.

**Unavailable/unexecuted:** `nix`, `cargo`, `rustc`/`rustfmt`; all Rust compilation/tests/clippy/release gates, the new Rust regression cases, full TonePoet DSD and SSRC conversions, the pinned FFmpeg 8 build, and performance benchmarks. This delivery **does not** claim those gates passed or that an actual SACD/SSRC file has been converted with R2E.

## Build-host acceptance commands

On the project's pinned Nix build host, in the extracted repository:

```bash
nix develop --extra-experimental-features 'nix-command flakes'
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo test -p tonepoet-pipeline custom_dsd_multichannel_dc -- --nocapture
cargo test -p tonepoet-pipeline native_float64_dc_effect_explicitly_preserves -- --nocapture
cargo test -p tonepoet-pipeline forced_qualified_ssrc_resampler_with_multichannel_dc -- --nocapture
cargo test -p tonepoet custom_dsd_multichannel_dc_charged_terminal -- --nocapture
cargo test -p tonepoet pcm_gain_info_regressions -- --nocapture
cargo test -p tonepoet pcm_gain_info_glyph_tests -- --nocapture
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --no-fail-fast --exclude tonepoet-true-peak
cargo test -p tonepoet-true-peak
cargo build --release
```

Require real supported input/output trials: Custom multichannel SACD/DSD -> FLAC Int24, Track and Album Guard with Manual and eligible Auto shift, original speaker roles verified; an independently commissioned protected Binary64 SSRC rate-change with 3.0/quad, post-resample native DC and Guard, with unchanged resampler identity and FFmpeg charged terminal; uncommissioned SSRC and incompatible/no-proof terminal cases must refuse. Verify measured/decided/applied DC and true-peak ceiling/evidence against actual decoded audio; do not substitute a Python model for TonePoet's executor. Recheck Off/stereo-fold/High-pass and PCM gain info Notice on the build host. Only after those gates pass can this be called qualified.

## Integrity and resume

The **new** `PCM_GAIN_INFO_R2_DC_SHIFT_R2E_SHA256SUMS.txt` is the authoritative manifest for the full delivered source tree (excluding itself). Historical manifests from earlier archived versions are included for provenance but naturally contain hashes for **older** versions of the two corrected files; do not use an old manifest to validate the R2E tree.

This report and `PCM_GAIN_INFO_R2_RESUME_CHECKPOINT.md` are included in the full-source archive. Archive SHA-256 is furnished in an adjacent `.tar.gz.sha256` file, calculated after packaging. An archive is not production-qualified merely because it passes its SHA-256 manifest.
