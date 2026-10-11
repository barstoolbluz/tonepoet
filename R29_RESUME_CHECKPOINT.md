# R29 crash-safe resume checkpoint — 2026-10-11

## Inputs / authority

- Original user brief: `BRIEF_LOGGING_R29_ceiling_control_and_log_lines_2026-10-11.md`.
- Original complete source bundle: `TonePoet_LOGGING_R29_ceiling_control_and_log_lines_2026-10-11.tar.gz`.
- Input source `Cargo.toml` names `tonepoet 0.5.4`, `rust-version=1.93`. Offline extracted baseline was committed in an ephemeral Git worktree before any changes. The acceptance run must use a configured Rust/Nix host with required real audio executables and a real SACD ISO.
- Scratch Git baseline commit: `edd716b` (`Archive baseline R28`); implementation commit: `fe3bc13` (`R29: true-peak ceiling provenance, controls, PCM CLI and audio MD5 logs`).
- Focused patch `R29_CHANGES.patch` SHA-256 `e8999af58605859214fda3ddd45795b0884b16c6abd218701431ac6b6e45ba56`; 11 files, 911 insertions/52 deletions. The repository has a clean worktree at the implementation commit.

## Completed implementation

11 files changed/added:
1. `tonepoet-pipeline/src/settings.rs` — typed ceiling source, common grid constant, serde compatibility.
2. `src/main.rs` — PCM CLI controls, DSD ceiling-origin binding, regression unit tests.
3. `src/tui/app.rs` — unified .25dB ceiling grid and user-origin tracking, tests.
4. `src/tui/command.rs` — explicit PCM/DSD custom ceiling input.
5. `src/tui/convert_actions.rs` — propagate origin through runtime settings.
6. `src/tui/presets.rs` — preset origin and DSD merged-projection correction.
7. `src/tui/draw_output.rs` — common selector display, source-specific custom command hint.
8. `src/convert/pipeline/execution_evidence.rs` — human-only ancillary artifact projection.
9. `src/convert/pipeline/mod.rs` — register optional stored-checksum reader.
10. `src/convert/pipeline/stored_audio_checksum.rs` — robust FLAC/WavPack native-MD5 scanner and tests.
11. `src/convert/pipeline/stages.rs` — log fragment provenance/MD5, compact output presentation, tests.

### Deliberate semantics

- The 0.01 dB `POST_FINAL_ACCEPTANCE_RESERVE` is an engineering bound, not demonstrated analyzer reporting quantum; the model uses `DbNano` and 9-decimal bounded input. No DSP/measurement behavior changed.
- Zero dBTP remains valid, consistent with existing `MAX_NORMALIZE_TARGET`.
- PCM true-peak CLI opts are refused if policy is off/fixed and no mode is chosen, preventing accidental processing changes.
- WavPack MD5 is optional. No MD5 is invented if absent, and no hybrid file is labelled lossless.
- Source-locked files changed intentionally. **The included installed v18 qualification sidecars are stale for this source**, regardless of their previous success. Requalify once AFTER all final source changes/formatting.
- Format descriptors and writer/encoder evidence must not be conflated: Reference PCM terminal does not prove FLAC package encoding.
- Settings fingerprint uses a deliberate field inventory and does not include provenance-only fields, preserving audio-settings identity for choices with identical audio outputs.

## Verification recorded in this environment

- PASS — original archive inspected/extracted; complete workspace preserved.
- PASS — `git diff --check` (no conflict markers/whitespace errors).
- PASS — bracket/delimiter lexical scan with Python `pygments` RustLexer across all 11 source files; this is NOT a Rust parse/type check.
- PASS — source trace: fragment assembled in Features stage ahead of Publish, hence staged audio exists when native checksum is read.
- PASS — independent patch replay: applying `R29_CHANGES.patch` to a clean extraction of the actual uploaded source archive recreated every one of the 11 changed/new files byte-for-byte; reverse patch check also passed.
- PASS — integration link scan: both TUI roots, CLI mode/target/scope/scan routing, forensic operation preservation and album fragment projection.
- PASS — packaged archive content hashes audited against embedded `SOURCE_SHA256SUMS` and the single retained source symlink preserved.
- **NOT RUN** — `cargo fmt`, `cargo check`, `cargo test` (Rust toolchain not available).
- **NOT RUN** — full workspace release gate or performance/real-file smokes.
- **NOT RUN** — real-tool Reference requalification + freshness gate; source-lock changes require this before production.

## Required next action on qualifying host

1. Restore/unpack the full source. Review/format the changed Rust hunks. If formatting rewrites locked source, do this BEFORE requalification. Do not blanket-reformat unrelated files.
2. Run targeted R29 tests, and the strict repository-specific `scripts/requalify_reference_r20.sh` (acceptance script wraps it and prerequisites). The R20 runner transactionally installs all four coherent generated sidecars, runs `reference_qualification_freshness`, targeted smoke tests, workspace test suite and release build.
3. Supply a real SACD ISO to test the positive Reference conversion. Run the documented negative runtime-mismatch smoke separately; this cannot safely be performed by modifying a production tree.
4. The brief documents a lease-ownership failure `convert::pipeline::memory_budget::tests::execution_staging_live_and_recovery_reserved_block_stale_cleanup_until_retired` which passes 3/3 in isolation. If it occurs, validate the exact isolated test and retry the gate; never mask unrelated failures as this flake.
5. Inspect a real output album log to verify `Ceiling:` independently of `Gain ...`, one writer/target line, and native `Stored audio MD5` when present; compare against `metaflac` or `wvunpack -s`. Confirm retagging preserves the audio digest and that it is not claimed to prove decoded audio correctness.
6. Rebuild/repackage from qualified source only after passing all hard gates; recompute SHA256SUMS and retain all generated sidecars and test reports with the release artifact.

Resume is self-contained: `tonepoet/` has the complete modified sources; `R29_CHANGES.patch` is an independent minimal diff for review/reapply. SHA256SUMS authenticates delivered handoff files, **not** the upstream Reference executable certification.
