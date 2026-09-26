# Delivery notes: five field defects from the 0.5.3 week

Date: 2026-09-25  
Bundled source head: `09e6778`  
Prior implementation checkpoint: `f9425ca`
Corrected delivery revision: `r1`

## Summary

This delivery addresses all five findings in `BRIEF_five_field_defects_2026-09-25.md` without changing the `tonepoet-true-peak` public API, the SSRC Binary64 registry/evidence, or the queue database schema.

1. **Per-track album ReplayGain now reduces across the complete independent-file batch.** The queue dispatcher recognizes album-scoped ReplayGain batches, holds successful encoded members at a post-encode barrier, measures the complete ordered set once, and applies the resulting album gain/peak to every member while preserving each member's own track gain/peak. CUE/multi-track single-item behavior remains on its existing source-scan path. A real queued two-track conversion regression exercises the end-to-end path.

2. **Embedded artwork is normalized to a target-safe representation instead of stream-copied blindly.** FFmpeg encode and metadata-transfer command builders transcode preserved embedded artwork to PNG and mark it as attached artwork for formats that support embedded cover art. The command description discloses the normalization. Queue/history failure mapping now prefers the failed track's actionable sentence over the generic album-blocked summary. The FFmpeg command shape is covered by planner tests; an external smoke check in this environment successfully converted a GIF picture stream to PNG while muxing FLAC.

3. **Context-menu presets are applied against the source being converted.** Browse conversion now binds a pending preset continuation to the async source-probe generation/path and applies the preset only after those source facts resolve. DSD fields that are supplied but cannot apply are reported as refused rather than silently skipped. Ordinary saved PCM presets no longer manufacture irrelevant DSD fields. The Format pane constrains Int32 dither choices to terminals the planner can actually admit.

4. **Dead-session queue recovery is repeatable and preserves recovery authority until user action.** A scope observed as live-owned at the initial availability check is logged and skipped for that pass; a scope that races back to live-owned during recovery acquisition is likewise logged and skipped. Neither condition terminates recovery of later scopes. The TUI re-observes dead scopes periodically, merges newly adopted rows into the live queue, and surfaces them as `Interrupted` with Retry/removal guidance. Recovered QueueExecution, ExecutionClaim and ExecutionStaging descriptors remain RecoveryReserved while the row is Interrupted; Retry or Remove retires the complete execution lifecycle. Recovery-reservation conflict status identifies the queue item and directs the operator to Queue. The focused source regression checks both live-owned branches for a diagnostic, `continue`, and absence of an early return; the reservation regression proves Retry and Remove release recovered lifecycle descriptors only at those explicit user boundaries.

5. **Package-version-only changes no longer invalidate Reference qualification.** The Reference common-source hash now canonicalizes only the root `[package].version` value in `Cargo.toml` and the corresponding source-less root `tonepoet` package version in `Cargo.lock`. `Cargo.lock` remains in the source lock; dependency/content changes still alter the closure. Formatting/comments on the version line remain bound. The TUI now exposes the specific Reference qualification refusal rather than collapsing it to generic P0-015 toolchain text.

## Reference qualification

**Requalification is required for this delivery.** `build.rs` changed and is itself inside the Reference source lock. This delivery also changes other source-locked Reference behavior, including `reference_source_lock.rs`, `tonepoet-pipeline/src/plugins.rs`, `src/convert/pipeline/stages.rs`, `src/convert/pipeline/track_executor.rs`, and `src/convert/replaygain.rs`.

The installed Reference report, certification and evidence from bundled head `09e6778` therefore must be regenerated and reinstalled on the operator's machine before Reference conversions can pass closed qualification on this implementation. This bundle does **not** edit those execution-evidence artifacts by hand.

After requalification of this implementation, a later change that only bumps the root package version in `Cargo.toml` and its matching root entry in `Cargo.lock` will retain the same Reference common-source hash.

## Validation performed here

- `git diff --check`: clean.
- FFmpeg artwork smoke test: a GIF picture stream was decoded to PNG and muxed successfully into FLAC with `attached_pic` disposition.
- Source review confirmed the five acceptance paths and their targeted regressions are present.
- Corrected-delivery regression inspection confirms both live-owned recovery branches log their skip reason, continue the same pass, and contain no early `return Ok`.

The prescribed Rust workspace gate was **not run in this container** because it has no `cargo`, `rustc`, `nix`, or `flox`. Per the brief, the gate is run on the operator side. The expected acceptance criterion is no test failures on that operator-side gate.

## Operator gate / handoff

Run the repository's prescribed Nix development environment and workspace gate from `CLAUDE.md`, then perform the required Reference requalification/reinstall for this changed source lock. If the gate exposes a compile or test failure, use `CHANGED_FILES_five_field_defects_2026-09-25.txt` to constrain follow-up to this delivery's delta.
