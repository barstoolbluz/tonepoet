# DC Shift R2C — persisted resumption checkpoint (2026-10-18)

**Code state:** Three reported defects corrected and saved in the full repository tree. Source audit and standalone FFmpeg multichannel sample/layout reproduction pass. **No Rust build or workspace test has been run.** Do not report this bundle qualified.

## Reopen these exact locations

1. `src/convert/pipeline/execution_evidence.rs`: `decision_kind_name`, `decision_summary`, and `completed_registered_effects_with_dc` typed receipt validation.
2. `src/convert/pipeline/plan_bridge.rs`: `DcEffectDependency`, `segmented_registered_effect_execution_from_typed`, source-role mask in `plan_request_for_track_impl` and regression test.
3. `src/convert/pipeline/dc_shift.rs`: `resolve`, `apply_resolved`, SHA-256 validation, new regression tests.
4. `src/convert/pipeline/stages.rs`: `prepare_registered_effect_carrier_for_track`, `verified_stereo_fold_source_layout`, `realize_registered_effect_chain_impl`, `native_dc_container_transport`, PCM/DSD true-peak paths.
5. `src/convert/pipeline/types.rs`: newly retained `channel_layout` on 3 carrier variants; `tonepoet-pipeline/src/plan.rs`, `plugins.rs`, `fingerprint.rs` for raw-Float64 input layout.

## Verified this session

- 13 source-consistency checks passed via `validation/dc_shift_r2c/static_regression_audit.py`.
- FFmpeg 7.1.5 reproducer passed 3.0, quad, 5-channel `0x3b` with decoded PCM binary64 sample bitwise identity; results in `validation/dc_shift_r2c/ffmpeg_dc_layout_results.json`.
- All explicit PlanRequest literals and three changed TrackSourceRef carrier variants inspected by script.

## Unverified, next steps

On the pinned Nix toolchain, run exactly the gate commands in `DC_SHIFT_R2C_ENGINEERING_HANDOFF.md`. First fix concrete compiler findings; then verify new Rust tests and actual multichannel conversions and effects through TonePoet. Test unsupported layout refusal, DSD, true-peak, resampling, manual/Auto/no-op, evidence, and legacy Off/high-pass. Do not broaden architecture without a specific demonstrated failure. Refresh this file with command outcomes and rearchive if any code changes occur.

## Limitations / confidence boundary

The source was not compiled or linked. No Rust tests, full conversion fixture, or performance benchmarks were run here; pending gates remain blockers to production handoff. Old R2 album-wide CUE Auto limitation remains (skip to preserve continuity). Standalone FFmpeg tests used v7.1.5, not the pinned v8 build.

## Archive integrity

`DC_SHIFT_R2C_SHA256SUMS.txt` holds per-file hashes of this full tree (excluding itself), `DC_SHIFT_R2C_SOURCE_CHANGE_INVENTORY.txt` identifies the changed files against previous R2, and external `<archive>.sha256` hashes the compressed archive. Verify both; a fresh process needs only this full-source tarball and those documents.
