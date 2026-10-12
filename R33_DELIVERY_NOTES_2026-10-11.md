# TonePoet R33 — engineering handoff (2026-10-11)

## Input and scope

Source: `TonePoet_LOGGING_R33_metadata_authority_sacd_tags_true_peak_2026-10-11.tar.gz`. Brief: `BRIEF_LOGGING_R33_metadata_authority_sacd_tags_true_peak_precision_2026-10-11.md`. Preserve R32. Issues #76 (album composer/metadata authority), #75 (SACD tag transfer) and #74 (true-peak reporting/input granularity).

## Implementation candidate

* **#76:** The existing aggregate metadata resolver chooses authoritative carriers by configured priority. For CUE metadata serialization, **only a globally one-track-per-file CUE** permits file-scoped performer/composer/lyricist/arranger credit values to be projected into their respective track-specific TonePoet CUE extension blocks. It no longer incorrectly requires all FLAC COMPOSER values to be equal. Physical multi-track images retain the ordinary stricter album/track model. No general-purpose metadata authority resolver was replaced, and the existing per-side write transaction was preserved.
* **#75:** Tag transfer recognizes an SACD ISO explicitly or as the only logical audio carrier in a folder together with its same-stem XML sidecar. Reads reuse the existing ScarletBook/sidecar source projection. Only same-stem XML is writable; the ISO is never mutated. The classification-time XML snapshot is checked again before read/write; selected area's track IDs come from ISO TOC counts, not metabase TOTALTRACKS guesses. Reject incomplete or duplicate mappings and lossy metadata representations; preserve unknown metabase fields and stored list values, and avoid no-op rewrites. Multichannel-only SACDs have a verified mapping path.
* **#74:** Album Reference gain human-log renders the exact nanodecibel residual `ceiling − (reported measured peak + applied music gain)` when nonzero. This gives a numeric explanation for the discrepancy without weakening the qualified hard-ceiling inequality or modifying DSP. TUI custom input, settings updates, presets, and CLI target flags now accept only 0.0001 dBTP grid values; the internal DbNano representation, old valid presets, and existing qualified hard-ceiling solver retain nanodecibel precision.

**Precision caveat:** Four decimals is a user-entry granularity limit, **not** proof that delivered peaks exactly equal the displayed target. Actual terminal realization bounds vary by PCM depth, dither, lossy encoder-input path, and measurement tier. The mathematical hard-ceiling promise is an upper bound, not exact equality. This deliberately declines the brief's stronger proposed universal guarantee.

**Authority caveat:** Existing explicit CUE override actions and the established `DISCNUMBER` dual-carrier exception for an album's sidecar-authoritative *ordinary save* are unchanged. The latter is an existing documented test contract, not silently removed in this change; a literal universal ban on all nonselected writes would require changing that policy and its load/save tests. Do **not** claim that the entire global metadata-authority policy has been formally proved from these R33 fixes alone. The R33 code specifically eliminates the observed per-file COMPOSER refusal and preserves the existing resolver/transaction invariants.

## Validation evidence, not claims

* Diff relative to the supplied source is confined to six Rust files, plus new R33 delivery/acceptance/checkpoint artifacts.
* Added focused `r33_` regressions for composers in extended CUE metadata, SACD field preservation, write idempotency, ordered-list metadata, stale XML snapshot rejection, duplicate/unmapped SACD track IDs, true-peak grid, and exact log residual.
* No `cargo`, `rustc`, `rustfmt` or Nix in this execution environment, and no real SACD ISO fixture. **Compilation, cargo fmt, compiled regressions, full workspace gate, and real-SACD Reference qualification are NOT verified here.** Prior R32 gate of 7442 pass / 1 source-lock failure is a statement in the brief, not a new test run.
* R30's source-locked Reference evidence has necessarily become stale because R33 edits `src/convert/pipeline/stages.rs`. Build host must perform complete fresh requalification and bind current locked source hashes. No unqualified override allowed.

## Build-host handoff

1. From the repository root, run `R33_CHECK_FORMAT=1 bash ./R33_ACCEPTANCE.sh /absolute/path/to/real.sacd.iso` with Rust 1.93+, sound audio dependencies, and pinned Reference binary/fixtures. Run `cargo fmt --all` first if needed and re-run requalification **after** every source/formatting edit to locked files.
2. Exercise the field scenarios with actual individually tagged FLAC album + CUE (different per-track COMPOSER), and one SACD ISO with a same-stem XML sidecar as both tag-transfer source and target. Check all data round trips and byte/mtime preservation of non-authoritative carriers.
3. Record the complete compiler output, workspace gates, Reference requalification status, and post-integration SHA-256 manifests. A source-only static audit is not acceptance.

The resume checkpoint (`R33_RESUME_CHECKPOINT_2026-10-11.md`) and full tree hash manifest allow interrupted review and rebuild to resume without reconstructing assumptions.
