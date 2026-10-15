# TonePoet R19 HDCD R5C — named-preset CUE warning corrective handoff

**Date:** October 15, 2026. **Base:** `TonePoet_R19_HDCD_R5_EIGHT_FAILURES_UI_CORRECTIVE_2026-10-15.tar.gz`, SHA-256 `ae99786572e2314a3813eb22b91b6e3002cdac7ef6e049bfb8cc760b643ca98e`. **Authority:** the independent R5 validation supplied in this conversation. **Status:** source-level corrective candidate, not yet Rust/build-host acceptance.

## Narrow defect and correction

Named-preset automatic commits pass through a deferred **source-probe preset continuation** before the existing **scan-gated post-load continuation**. R5 saved warnings only on the latter. The preset-loaded status could replace the former visible warnings, leaving the queued result incomplete.

Exactly **two existing source files** changed:

- `src/tui/app.rs`: add `status_notes: Vec<String>` to the existing `PendingBrowseConvertPresetContinuation`, matching the existing post-load continuation's bounded warning payload.
- `src/tui/command.rs`: after successful folder expansion, attach the first ordinary warning and one distinct cumulative multi-FILE CUE warning to whichever matching continuation is present, using the existing duplicate suppression. When the source probe resolves a named preset, transfer these notes to the pending post-load scan continuation or append directly after an immediate final status. Preserve notes after a preset refusal or unresolved ISO as well. The unchanged scan-resume code appends notes **after** the queued result.

Unchanged: selection/path/generation checks; single-consumer ownership; queue transaction; audio execution, qualification and pipeline; scan scheduling; database/schema; output/preset policy; the other 1,009 R5 source/support files. No new warning framework or dependencies.

## Focused regression

`src/tui/command.rs` adds `async_named_preset_commit_preserves_both_cue_warnings_across_probe_and_scans` inside `execute_queue_state_consistency_tests`. It creates a small valid stereo 16-bit/44.1-kHz WAV without an encoder, saves a named v5 preset with an output destination, publishes a folder expansion with an ordinary warning followed by a different cumulative-CUE warning, and verifies:

1. Both notes initially belong to the source-probe preset continuation.
2. Processing the **real asynchronous** `AppMessage::ProbeResult` event loads the preset and transfers both notes to the scan-gated post-load continuation.
3. HDCD scan completion leaves the normal **Queued** result primary with each warning present **once**.

This specific selection leaves **one** valid audio source. `SourceMode::Single` completes the de-emphasis batch preflight synchronously; it does not generate an asynchronous de-emphasis batch completion event. The regression honors that existing behavior instead of manufacturing an event. Existing batch continuation tests retain coverage of both asynchronous scan completions and orders. The original no-preset CUE-warning regression remains byte-for-byte unchanged.

## Verified in this environment

- Clean R5 extraction's **1,011-entry SHA-256 inventory PASS**.
- Two-file patch applies and reverses cleanly; independent apply reproduces both modified source files byte-for-byte; reverse reconstitutes all 1,011 historical hashes: **PASS**.
- **16 structural source-contract checks PASS**; see `R19_HDCD_R5C_STATIC_AUDIT_LOG.txt`.
- **Four Python/SQLite/FFmpeg compile-free probes PASS**; see `R19_HDCD_R5C_COMPILE_FREE_LOG.txt`.
- **NOT RUN**: Rust compilation, new named-preset Rust test, existing no-preset/Last Used Rust tests, full workspace gate, pinned libfdk AAC, and qualified Reference requalification. No Rust/cargo or configured Nix/Reference host here.

## Build-host gates (mandatory before acceptance)

```bash
cd tonepoet
sha256sum -c R19_HDCD_R5C_PACKAGE_SHA256SUMS.txt
cargo test --workspace --locked async_named_preset_commit_preserves_both_cue_warnings_across_probe_and_scans
cargo test --workspace --locked async_browse_queue_review_commit_preserves_cue_warning_after_final_commit_status
cargo test --workspace --locked context_menu_convert_last_used_audio_directory_commits_after_async_expansion
cargo test --workspace --locked
scripts/requalify_reference_r20.sh
cargo test --workspace --locked
```

R5 already owed genuine source-lock requalification because `plan_bridge.rs`, `plugins.rs`, and `semantic_plan.rs` moved earlier. R5C does not alter those files or historical sidecars. Do not claim requalification or use unqualified overrides. Run additional targeted tests as appropriate for the configured build host. The original `R19_HDCD_R5_PACKAGE_SHA256SUMS.txt` is **historical and deliberately stale** for the modified files; use the new R5C manifest.

## Delivered artifacts

Full source tree plus `R19_HDCD_R5C_SOURCE_DELTA.patch`, the two R5C verification logs, `R19_HDCD_R5C_RESUME_CHECKPOINT_2026-10-15.md`, and `R19_HDCD_R5C_PACKAGE_SHA256SUMS.txt`. No original R5 files were removed.
