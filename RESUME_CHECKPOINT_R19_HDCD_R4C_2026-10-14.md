# R19 HDCD R4C resumable implementation checkpoint

## State

Three audit defects corrected in the authoritative full R4 source tree, with a small three-file patch and focused Rust regression tests. The archive is a complete replacement snapshot. The only outstanding acceptance requirements are Rust compile/test, pinned AAC physical execution, full workspace gates and Reference source-lock requalification on a suitable build host.

## Identity

Original complete R4 corrective source SHA-256: `f934758022830a9cfdd13e4d604cc6cfdbbf30ece2783045e8b157fbdf4da06f`.
Input authority: `BRIEF_R19_HDCD_R4_defaults_depth_dither_lossy_2026-10-14.md` plus user audit (three defects: Int16 auto-HDCD-off, HDCD TPDF mixed-batch leakage, AAC explicit dither Off lost).

Modified code only:
- `src/tui/app.rs`
- `src/tui/convert_actions.rs`
- `src/convert/pipeline/plan_bridge.rs` (Reference source-lock coverage)

Added artifacts: `R19_HDCD_R4C_SOURCE_DELTA.patch`, `R19_HDCD_R4C_PACKAGE_SHA256SUMS.txt`, report and compile-free log. Historical `R19_HDCD_R4_PACKAGE_SHA256SUMS.txt` still describes BASE R4, not current R4C.

## Resume procedure

1. Extract the complete `TonePoet_R19_HDCD_R4C_THREE_DEFECT_CORRECTIVE_2026-10-14.tar.gz`; verify the adjacent `.sha256` and `R19_HDCD_R4C_PACKAGE_SHA256SUMS.txt` inside `tonepoet/`.
2. Read `DELIVERY_REPORT_R19_HDCD_R4C_THREE_DEFECT_CORRECTIVE_2026-10-14.md`; inspect the three-file patch rather than replacing existing source wholesale.
3. Perform focused and full compilation/tests on the pinned Nix build host. Execute the AAC, HDCD CUE and specialized dither variants. Source review and compile-free FFmpeg/SQLite checks have passed but the Rust tests were not executable here.
4. Requalify Reference using the established script with real fixtures because `plan_bridge.rs` changed. Never edit qualification JSON by hand; do not claim source-lock freshness until the gate runs.
5. If a test fails, make the smallest correction to the actual affected code, rerun relevant tests, regenerate the R4C package SHA inventory after any modifications, and record the new results. Preserve existing HDCD/SSRC source/terminal authority and the R4 migration correction.

## Test evidence

See `R19_HDCD_R4C_COMPILE_FREE_TEST_LOG.txt` and the detailed delivery report. PASS: baseline 988 checksum entries, patch application and byte-for-byte reconstruction, four existing compile-free HDCD/FFmpeg/SQLite probes. NOT RUN: Rust check or test binaries, pinned libfdk AAC, Reference requalification.

## Final status

Delivered engineering corrective candidate, NOT a release-qualified build. No speculative hardening or redesign recommended.
