# R27C source and acceptance checkpoint — October 10, 2026

## Restore point

Complete tree: `TonePoet_LOGGING_R27C_reference_humanization_2026-10-10.tar.gz`; root `tonepoet/`. Baseline: R27 archive SHA-256 `14a0497b652014c3cc7cd1ab5c593ecdb0e56657fcf8d5dcc2209a7419aebe4a`. Source-only delta: `R27C_CODE_CHANGES_FROM_R27.patch`. Authority: user-supplied independent R27 audit text plus `R27_AUTHORITY_BRIEF_2026-10-10.md`.

**Only `src/convert/pipeline/stages.rs` differs from the original R27 code.** The baseline R27 patch and R27 manifests are historical; use the new `R27C_CHANGED_SHA256SUMS.txt` for this tree. No existing code outside that file was edited. No newly introduced compiler dependencies.

## What has been done

- Confirmed Off and Track auto lack `DsdReferenceAutoGainCarrier`, so R27's humanization was wrongly restricted to album auto.
- Added optional `reference_gain_is_off` display-only coordination-fragment hint from the typed `ReferenceExecutionEvidence.plan.gain_policy`, with backward-compatible serde default. It is **not** an execution receipt.
- Recognized delivered qualified Reference operations independently of album-auto facts; kept `album_uniform_reference_gain` unchanged and strict.
- Gain Off fixed restoration is omitted only from cloned human processing; Track auto shows completed music scalar minus 12 dB, including legitimate 0 dB; a legacy fragment with absent mode hint suppresses a fixed-only gain claim.
- Promoted the already-existing typed delivered Reference PCM contract regardless of gain mode. Suppressed unsanitized generic album `apply_gain` promotion when completed Reference operations exist.
- Added two focused Rust test functions for Off, Track auto, singleton, legacy fragment and structured-evidence preservation.

## Verification state

- PASS: only one existing source file changed relative to R27.
- PASS: source patch applies cleanly to **the uploaded R27 archive** and reconstructs the delivered edited file byte-for-byte.
- PASS: git diff whitespace check and Rust lexical/delimiter scan (NOT compilation).
- **NOT RUN**: cargo, rustc, rustfmt, Nix, compiled regressions, real-SACD Reference qualification, real-SACD Browse → Convert → preset UX test. Those must be done on the qualification host.

## Next session — do only what is required

1. Verify `R27C_CHANGED_SHA256SUMS.txt` and read `R27C_DELIVERY_NOTES_2026-10-10.md` for exact behavior and command list.
2. On a build host: `cargo fmt --all -- --check`; `cargo test --lib r27c_`; `cargo test --lib r27_`; `cargo test --lib album_scope_`. Investigate any compiler or test failure with the smallest coherent correction to `stages.rs`.
3. Run the full workspace tests, builds, existing CLI exit-code and album-publication smoke tests. Check both explicit `--dsd-gain off` and Track auto human logs against their structured receipts on actual completed conversions where fixtures are available.
4. Run `./scripts/requalify_reference_r20.sh /absolute/path/to/real-SACD.iso` **once** after all source changes are final, without an unqualified override, then repeat the full freshness gate.
5. Perform the separately outstanding real-SACD Browse → Convert → preset exercise (#43). This is not replaced by automated CLI smokes.
6. Regenerate the patch, delivery notes, hashes, and tarball if a code or test edit was necessary; do not report a compiled pass if only the source was inspected.

## Preserve scope

Do not broaden into Reference DSP, encoder, CLI precision changes, fragment recovery protocols or provenance infrastructure without a new real defect. Keep original `TrackExecutionEvidence` untouched; keep R27 album-auto corroboration and mixed-source exclusion disclosures intact. The correct engineering boundary is transient human-log presentation, not signal processing.
