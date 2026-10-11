# R30C resume checkpoint — 2026-10-11

## Current state

- Source basis: verified complete `TonePoet_LOGGING_R30_CORRECTIVE_2026-10-11.tar.gz`, SHA-256 `4bedce957bdf6cb1b31272c5231b9d0ea39ffcc2bc998adf05ba7e8bbbab4ddb`.
- Correction: only two obsolete expected strings changed in `src/convert/pipeline/stages.rs`; production formatter remains the R30 exact, minimum-two-decimal implementation.
- Corrected `src/convert/pipeline/stages.rs` SHA-256: `deb8826d4643c97d65e5a612dce5b61e34f523da2800221905e26650a7396a8f`.
- Incremental patch: `R30C_FROM_R30_TEST_ONLY.patch`, SHA-256 `325fe2780cbfce250bcf9455176e5a619c407bde587f472854e87c9386eed2eb`.
- The complete R30C archive, patch, checksum inventory and this checkpoint are the durable handoff. `R30_FROM_R29C.patch` is the historical first-stage patch and must be followed by R30C's test-only patch if starting from R29C.

## Static evidence

- The incremental patch independently replayed byte-for-byte onto the original R30 file.
- Expected edits only: R29 album-ceiling provenance expectation and R27 album-log golden expectation.
- No changes to successful production behavior, Reference qualification sidecars or requalification machinery.
- The archive/source manifest was regenerated and checked; retain it with this source snapshot.

## Remaining hard gates

1. Run the four focused `cargo test --locked --lib` commands in `R30C_DELIVERY_NOTES_2026-10-11.md`.
2. Run the ordinary full build-host gates using the existing `R30_ACCEPTANCE.sh` with a real SACD ISO. This reuses `scripts/requalify_reference_r20.sh` unchanged and requires fresh source-locked Reference evidence.
3. Complete the documented negative closure-mismatch smoke and visually inspect true-peak log presentation.
4. Record actual test results and qualification sidecars; do not report the candidate as qualified in their absence.

**Unrun here:** Cargo/Rust tests, rustfmt, Nix, real-audio conversions, full workspace gate, freshness and runtime-closure qualification.
