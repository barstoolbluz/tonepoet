# R19 HDCD R4D — resumable hand-off checkpoint

## Inputs and authority

- Baseline full source: `TonePoet_R19_HDCD_R4C_THREE_DEFECT_CORRECTIVE_2026-10-14.tar.gz`.
- Baseline archive SHA-256: `542bb8d35d1fbe46fe6407af93ce8c47e865b454f49535190605d811fa4e7d4f`.
- Authority: `BRIEF_R19_HDCD_R4_defaults_depth_dither_lossy_2026-10-14.md`; user R4C hand-off audit.

## Current work

Fixed the remaining user-default conflict: effectual HDCD on a known-lossless 16/44.1 source selected to MP3/AAC/Opus no longer also enables **automatic** certified TruePeak Guard. Late scan and manual HDCD toggle recompute the automatic PCM gain choice; explicit gain stays authoritative and the existing commit refusal remains.

Changed exactly two Rust files: `src/tui/app.rs` (policy and three new focused tests) and `src/tui/convert_actions.rs` (corrected AAC test probe facts + typed-policy assertion). The two-file R4C->R4D delta is `R19_HDCD_R4D_SOURCE_DELTA.patch`. All other original source files, including the R4C `plan_bridge.rs` correction, are intact.

## Verification status

PASS: input archive identity; original R4C inventory excluding two intentionally modified files; patch cleanly applies and reproduces both new source files; four HDCD compile-free SQLite/FFmpeg probes and Python syntax.

NOT RUN: Rust compile/test/full gate (toolchain absent), pinned libfdk AAC execution, actual end-to-end UI/commit, Reference requalification.

## Resume sequence

1. Extract the full R4D bundle. Verify its adjacent archive `.sha256` and the in-package `R19_HDCD_R4D_PACKAGE_SHA256SUMS.txt`.
2. Read `DELIVERY_REPORT_R19_HDCD_R4D_AUTO_GAIN_CORRECTIVE_2026-10-15.md` before altering source. The existing `R19_HDCD_R4D_SOURCE_DELTA.patch` is relative to R4C and tested for exact reproduction.
3. On the pinned Nix build host run `cargo check --workspace --all-targets`; then focused `r4d_`, `r4c_aac_` and `hdcd_` tests, then the normal complete workspace gate. Only fix demonstrated failures, preserving R4C's prior three corrections.
4. Qualify real HDCD FLAC -> MP3, AAC, Opus commits for both event orders, confirm explicit incompatible gain refuses, and execute pinned AAC terminal-dither variants.
5. Satisfy the pre-existing Reference source-lock requalification with the repository's established script and fixtures. No hand-edited evidence.
6. If any code or docs change, regenerate the R4D inventory and archive identity; preserve a new checkpoint and do not claim gates not actually executed.

## Acceptance state

Focused corrective candidate complete; **NOT build-host qualified**.
