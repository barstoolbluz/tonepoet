# TonePoet R19 HDCD R4D — automatic HDCD/true-peak conflict corrective

Date: 2026-10-15 (America/New_York).

## Decision and authority

- **Engineering fix delivered; build-host acceptance outstanding.** Do not call this release-qualified on compile-free evidence.
- Input authority: `TonePoet_R19_HDCD_R4C_THREE_DEFECT_CORRECTIVE_2026-10-14.tar.gz`, SHA-256 `542bb8d35d1fbe46fe6407af93ce8c47e865b454f49535190605d811fa4e7d4f`.
- Governing assignment: `BRIEF_R19_HDCD_R4_defaults_depth_dither_lossy_2026-10-14.md`, and the user's R4C hand-off validation describing the one remaining automatic HDCD/TruePeak Guard conflict.
- **Only two Rust files changed relative to R4C:** `src/tui/app.rs` and `src/tui/convert_actions.rs` (the latter only makes the pre-existing AAC Rust test representative and strengthens its assertion).
- All three previous R4C corrections remain untouched. No runtime dependencies, scanners, terminal algorithms, new policy state, source locks, or qualification machinery have been changed.

## Root cause

For known-lossless 16-bit/44.1-kHz effectual HDCD FLAC, lossy targets automatically selected HDCD Decode **On** and certified PCM **TruePeak Guard**. `src/tui/command.rs` correctly refuses that unqualified combination at commit, even though neither conflicting default was a user choice. Completing the HDCD scan also did not cause the automatic gain default to be recomputed if a lossy target had already been selected.

## Minimal correction

1. `FormatState::apply_auto_gain_defaults()` declines the incompatible *automatic* certified TruePeak Guard when the target is lossy, HDCD Decode is enabled, and the current scan summary is actionable. It keeps the ordinary float-to-integer and non-HDCD lossless-to-lossy safety defaults unchanged.
2. `FormatState::recompute_auto_hdcd()` refreshes automatic gain for PCM-source lossy targets. This is the existing recomputation path for scan completion, target switches, and manual HDCD changes, including late scan results. The extra refresh is deliberately restricted to PCM-source lossy targets to avoid changing DSD or lossless-target gain behavior.
3. `pcm_gain_overridden` remains authoritative. Explicit Guard or Normalize stays selected and is **still refused** by the unchanged commit-time qualification guard. Explicit Off and FixedGain choices are unaffected.
4. The existing R4C AAC Rust regression now sets known-lossless 16/44.1 source facts and verifies both the Format gain mode and the projected typed gain policy are Off; its terminal dither assertions are retained.

## Regression coverage added (Rust tests written, not run here)

- `r4d_effectual_hdcd_and_lossy_target_auto_defaults_are_compatible_in_both_orders`: effectual HDCD scan **before and after** target selection, with MP3/AAC/Opus, checking that the ordinary Guard appears before a late scan, then gives way to HDCD Decode and gain Off; repeated recomputation is stable.
- `r4d_manual_hdcd_toggle_reconciles_automatic_gain_but_explicit_gain_stays_authoritative`: manual Decode Off restores automatic Guard; On suppresses automatic Guard; user-selected Guard/Normalize remain selected (and subject to the unchanged commit refusal); user-selected Off survives a format switch.
- `r4d_passive_or_absent_hdcd_preserves_ordinary_lossless_to_lossy_gain_default`: no HDCD, passive signaling, and TF-only signaling continue to default to Guard for lossless-to-lossy conversions.
- The pre-existing `r4c_aac_preserves_manual_dither_off_and_terminal_ffmpeg_lowering` now covers real lossless source facts and requires a nonconflicting typed gain policy in addition to its original dither cases and AAC command planning.

## Verification executed

- **PASS**: delivered R4C archive SHA-256 matches the audit input.
- **PASS**: old R4C 993-entry SHA-256 inventory was checked; exactly the **two expected modified source files** differ, with the other 991 entries unchanged.
- **PASS**: `R19_HDCD_R4D_SOURCE_DELTA.patch` applies cleanly with `git apply --check --whitespace=error` to original R4C copies; patch application reproduces both revised Rust files byte-for-byte.
- **PASS**: `python3 tests/verify_hdcd_analysis_cache_sql.py`.
- **PASS**: `python3 tests/verify_hdcd_v28_migration_sql.py` (14 modeled migration cases).
- **PASS**: `python3 tests/hdcd_source_first_ffmpeg_smoke.py` (FLAC/CUE source-first decode behavior).
- **PASS**: `python3 tests/hdcd_lossy_precision_ffmpeg_smoke.py` (Opus/MP3 high-precision FFmpeg paths; Int16 dither boundary).
- **PASS**: `python3 -m py_compile` for the four compile-free probes.
- **NOT RUN**: Rust compiler, `cargo check`, focused Rust tests, full workspace suite, pinned libfdk AAC, end-to-end commit on a real HDCD FLAC, Reference requalification. `cargo`, `rustc`, `rustfmt`, and `nix` are unavailable in this environment. See `R19_HDCD_R4D_COMPILE_FREE_TEST_LOG.txt`.

The source-level correction and test design are supported by direct inspection and passing compile-free probes; the new Rust assertions remain **unexecuted**. Do not infer compiled or runtime acceptance from the above.

## Required build-host hand-off gates

1. In the pinned Nix environment, run `cargo check --workspace --all-targets`.
2. Run `cargo test r4d_ -- --nocapture`, `cargo test r4c_aac_preserves_manual_dither_off_and_terminal_ffmpeg_lowering -- --nocapture`, and `cargo test hdcd_ -- --nocapture`; include related Format-state/PCM gain tests.
3. Execute the normal complete gate: `cargo test --workspace --no-fail-fast --exclude tonepoet-true-peak` and the separate `tonepoet-true-peak` test gate on a suitable host. Build release as required by the project.
4. Perform a real effectual 16/44.1 lossless HDCD FLAC -> MP3, AAC (with **pinned libfdk**), and Opus commit with untouched defaults. Repeat scan-before-format and format-before-scan orderings. Check source-first decoding, codec rate/precision and successful queue admission. Confirm explicit Guard/Normalize still refuses, and no-HDCD lossless-to-lossy still defaults Guard.
5. Re-run pinned AAC automatic and explicit dither cases carried forward from R4C.
6. Fulfill the **pre-existing** Reference source-lock/requalification obligation on an authorized build host using `scripts/requalify_reference_r20.sh` with required real fixtures; do not edit lock data manually or claim qualification from this archive.

## Integration/performance proportionality

This is a small, UI-time state-policy correction with no added I/O, rescans, audio operations, or new per-sample work. The existing committed true-peak admission restriction is intentionally unchanged. No further infrastructure or architectural work is recommended based on these findings alone.

## Deliverable identity

This bundle contains the complete R4D source tree, `R19_HDCD_R4D_SOURCE_DELTA.patch` against R4C, this report, an execution log, a resume checkpoint, and `R19_HDCD_R4D_PACKAGE_SHA256SUMS.txt` for current integrity. The historical R4/R4C manifests remain unmodified and describe earlier snapshots; **use the R4D inventory for this bundle**.
