# R27 — interruption-safe engineering checkpoint (October 10, 2026)

## Authority and current state

Authority: `BRIEF_LOGGING_R27_album_processing_section_2026-10-10.md` from the same user turn. Intake archive: `TonePoet_LOGGING_R27_album_processing_section_2026-10-10.tar.gz` (SHA-256 `97c433e7e7bd95bc0e73c2c4ad3a69e6517f99201a74a2465e50ca9332b7a623`). This archive contains the R26 tree, without Git metadata. The brief reports R26 Reference requalification at upstream `24ce2cd`; not verified here.

R27 implementation is complete to the limits of source-level static verification. **Only** the existing file `src/convert/pipeline/stages.rs` was changed. `R27_CODE_CHANGES.patch` reproduces that changed file exactly against intake. Delivery notes and this checkpoint are included in the final source bundle.

The user wants world-class *and* small, behavior-preserving engineering: do not add further infrastructure without a realistic demonstrated failure. Retain the new compact three-group human presentation, exact stored measurement resolution, configured-target fidelity, typed completed-operation corroboration, mixed-album scope safety and immutable structured forensic evidence.

## Checks already completed

1. Old-to-new one-file source diff and patch reproduction: pass.
2. `git diff --check`: pass.
3. RustLexer error and delimiter scan: pass (lexical only).
4. New R27 focused Rust unit tests written but **cannot run here** (no Rust/Cargo toolchain). Full Rust test gate, Rust formatting check, production real-SACD smoke, and Reference requalification are **not** validated.

## First steps on resumption

1. Verify `R27_CHANGED_SHA256SUMS.txt` and source-only diff. Treat R26/R25 manifests as historical, not current-source manifests.
2. Run `cargo fmt --all -- --check` and `cargo test --lib r27_` first. If formatting needs updating, limit the resulting diff to the single edited source file and regenerate all R27 checksums/patch/bundle.
3. Resolve genuine test/compiler defects with the smallest localized correction, checking the exact evidence before requesting unrelated hardening. Run `cargo test --workspace`, build and existing CLI/album smokes afterward.
4. `src/convert/pipeline/stages.rs` is in the Reference source closure; once acceptance is otherwise green, run `./scripts/requalify_reference_r20.sh /absolute/path/to/real-SACD.iso` **once** on the final tree, without unqualified override. Rerun the full gate.
5. Perform the outstanding manual real-SACD Browse → Convert → preset workflow (#43). Do not assert it is complete based on the CLI smoke.
6. Regenerate patch, notes, manifest and archive if ANY source is changed. Preserve the zero-compiler caveat until real build-host execution is documented.

## Engineering decisions requiring no new work absent evidence

- Accepted high-precision CLI/TUI/preset target values are **preserved**; the log shows at least two decimals, plus meaningful additional configured digits. Do not enforce a two-decimal parser grid on this presentation-only iteration unless the user explicitly chooses a wider UX compatibility change.
- The observer's nine-place `DbNano` value is a recorded numerical measurement resolution, **not** a claim of metrological accuracy to nine significant decimals. A finite near-zero sentinel never prints a fictitious dBTP magnitude.
- The Reference internal 12 dB restoration remains in the structured operation record; the human projection subtracts exactly it, with no DSP mutation.
- Mixed-source excluded participant disclosures cannot be dropped merely to simplify the album log.

**Completion status:** offline source/handoff ready; build-host acceptance and real-ISO Reference requalification outstanding.
