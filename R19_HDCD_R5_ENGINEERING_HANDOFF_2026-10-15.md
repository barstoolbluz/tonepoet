# TonePoet R19 HDCD R5 — engineering handoff

**Date:** October 15, 2026. **Input:** `TonePoet_LOGGING_R19_HDCD_R5_2026-10-15.tar.gz`, SHA-256 `71e912e5d53a68a02a97bb41b23f6a73bd608f6f320d26451ff5142a67fbdc1b`. **Authority:** `BRIEF_R19_HDCD_R5_ui_and_eight_failures_2026-10-15.md` (already present in source tree). This is a **corrective candidate**, not a claimed successful Rust/build-host acceptance or Reference requalification.

## Scope and intended corrections

All changes are limited to ten source files; the complete original repository is retained.

| Brief item | Correction / evidence to obtain on build host |
| --- | --- |
| Two AAC fixture `output_path` failures | Set `request.output_path` to `output.m4a` for both tests and the Int16 AAC fast-path case in `tonepoet-pipeline/src/plugins.rs`. Run both targeted tests. |
| Certified DSD carrier gained an unintended `aresample`/dither step | The AAC terminal Int16 filter now explicitly excludes original DSD source representation, even when the decoded carrier is PCM. Run the qualified album-gain test; do not change its expected certificate. |
| Direct FFmpeg AAC CUE stream rejected | Permit **only** the exact SoXR AAC Int16 terminal-quantization filter in the existing direct-stream admission check. Reject extra filters/processing or different source semantics. The existing four-format direct-stream regression asserts the AAC filter too. |
| HDCD float policy test's disabled depth pill | Reapply WAV constraints after selecting WAV and before trying Float32/Float64. No production policy is changed. |
| v23 startup test expects obsolete v27 | Update expected SQLite user version to 28, matching the current v28 migration. The SQLite matrix remains unchanged. |
| Deferred CUE warning lost after commit | Store expansion warnings on the exact pending continuation and append them *after* the terminal queue-commit status, without duplicating a note. |
| Context-menu Last Used commits too early | Start and await both de-emphasis and HDCD source scans. Either accepted worker completion can resume the same single-consumer continuation once both authoritative results exist; generation and full source-path equality guard stale events. Existing tests now drive both completion types. |
| UI A | Remove the warning symbol `⚠`; retain the textual HDCD-off warning and clickable information glyph. |
| UI B | Hide HDCD row for passive/Transient-Filter-only and nonactionable evidence, including maximized layout. Preserve safety of nonactionable selection. |
| UI C | Replace HDCD track-count suffix with a short, count-free presence message. |
| UI D | Move the true-peak information glyph after the value/custom controls; drawing and button registration use the same value/edit-width-based offset on PCM and DSD rows. Preserve existing value and custom-pill alignment. |
| UI E | Replace the HDCD help text with the four exact paragraphs from the brief, including plain-text `On —` and `Off —`. |

## Verification performed here

- Input R4D 999-entry SHA-256 inventory passed *before modification*.
- The 10-file patch applies cleanly, with strict whitespace checking, to an independently extracted input tree. Reproduction is byte-for-byte on all ten modified files; the patch also reverse-checks cleanly on the delivered tree.
- Four compile-free probes passed: HDCD analysis cache SQL; v28 migration SQL (14 model cases); HDCD source-first FFmpeg/CUE smoke; and lossy precision FFmpeg smoke for available codecs. See `R19_HDCD_R5_COMPILE_FREE_LOG.txt`.
- A separate **19-check static source audit** passed: source scope, exact help text, UI layout/hiding rules, AAC/DSD route guards, schema assertion, deferred commit and warning retention. See `R19_HDCD_R5_STATIC_AUDIT_LOG.txt`. Static analysis is *not* Rust execution.
- The `libfdk_aac` encoder is not installed locally. No Rust toolchain or configured Nix build host is present. Rust compilation, the 8 targeted tests, the 7,490+/9 full gate, pinned libfdk AAC execution, and the Reference qualification/freshness gate **have not been run**.

## Required build-host acceptance (not performed here)

From repository root, on the project's pinned, equipped Rust/Nix/codec host:

1. Verify `R19_HDCD_R5_PACKAGE_SHA256SUMS.txt` with `sha256sum -c R19_HDCD_R5_PACKAGE_SHA256SUMS.txt`.
2. Run the eight exact tests from the brief individually or via project-standard filters, and confirm they pass:
   - `plugins::tests::libfdk_aac_quantizes_a_wide_carrier_once_at_its_terminal_boundary`
   - `plugins::tests::certified_true_peak_lossy_aac_does_not_gain_an_unqualified_dither_boundary`
   - `plugins::tests::ffmpeg_lossy_album_gain_pins_measured_encoder_input_rate`
   - `convert::pipeline::stages::tests::cue_stream_auto_alac_and_lossy_targets_remain_direct_ffmpeg`
   - `tui::app::hdcd_format_policy_tests::hdcd_defaults_follow_resolved_precision_and_lossy_codec`
   - `tui::app::app_startup_options_tests::quiescent_v23_startup_activates_v24_protocol_then_current_schema_without_fallback`
   - `tui::command::execute_queue_state_consistency_tests::async_browse_queue_review_commit_preserves_cue_warning_after_final_commit_status`
   - `tui::context_menu::tests::context_menu_convert_last_used_audio_directory_commits_after_async_expansion`
3. Run `cargo test --workspace --locked` (or the project's canonical Nix gate); examine *all* regressions, including the R5 UI test. Do not mark the known source-lock freshness test as passing until fresh qualification evidence is installed.
4. Requalify the current final source with `scripts/requalify_reference_r20.sh` in the configured real-tool host. This is owed because `plan_bridge.rs`, `plugins.rs`, and `semantic_plan.rs` moved across R4D/R5. The existing qualification sidecars in this candidate are **historical/stale** and intentionally unmodified. Do not use an unqualified override, fabricate evidence, or weaken the certificate/freshness check.
5. Re-run the full gate with new qualification evidence. Verify the audio route outcomes against real qualified fixtures and libfdk AAC. Accept only after all gates pass, or carry forward a narrowly evidenced build-host corrective defect.

## Package contents and integrity

- `R19_HDCD_R5_SOURCE_DELTA.patch`: exact minimal 10-file source delta; dry-run with `git apply --check`, apply with `git apply`.
- `R19_HDCD_R5_COMPILE_FREE_LOG.txt`: executed, non-Rust tests.
- `R19_HDCD_R5_STATIC_AUDIT_LOG.txt`: scope/text/structural checks (not execution).
- `R19_HDCD_R5_RESUME_CHECKPOINT_2026-10-15.md`: source of truth for continuation after an interrupted session.
- `R19_HDCD_R5_PACKAGE_SHA256SUMS.txt`: canonical inventory for this candidate, generated after all other files; the older `R19_HDCD_R4D_PACKAGE_SHA256SUMS.txt` is **historical** and will not validate changed R5 files.

Keep this candidate and its checkpoint until build-host validation finishes; do not conflate compile-free pass with acceptance.
