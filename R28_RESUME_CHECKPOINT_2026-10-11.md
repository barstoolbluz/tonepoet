# R28 resume checkpoint - 2026-10-11

## Restore point

The complete source bundle has root `tonepoet/`. Input archive SHA-256: `35752ad292912150cc17b81642be2d00c1698a065e7af47f0ff1c1a164e153b1`.

Only one existing file differs from that input: `src/convert/pipeline/execution_evidence.rs`. The source-only patch `R28_CODE_CHANGES_FROM_INPUT.patch` applies to the supplied R28 input, not to R27 or any unrelated tree. It has been verified by applying it to a clean copy and comparing resulting bytes. The R28 brief, patch, QA, delivery notes, checkpoint and checksum list accompany the edited source inside this archive.

## Engineering decision

Backend identity remains forensic data. Map exactly the three native labels `TonePoet Float64 scalar pump`, `TonePoet Float64 scalar materializer`, and `TonePoet DSD realization` to `TonePoet` in `ExecutionBackend::human_label()`. This path covers album and per-track human logs, so there is no reason to modify the producers, the portable JSON or source-locked Reference DSP files. Everything else retains its prior rendering. A new focused `r28_` regression covers both presentation and unchanged structured evidence.

## Proven verification

Static-only: one-file source scope; pristine patch replay; zero lexer errors / balanced delimiters; diff whitespace check; Reference source-lock comparison. No Rust compiler or Cargo was available; no compiled test or runtime conversion was executed here. Read `R28_STATIC_QA.txt` and `R28_DELIVERY_NOTES_2026-10-11.md` for details.

The source lock already mismatched `src/convert/pipeline/stages.rs` at input from R27C. This R28 patch does *not* modify that file, nor locked `track_executor.rs`. Reference certification remains pending after the R27C change. Do not bypass the qualification freshness gate or treat it as passed.

## Next steps on a real build/qualification host

1. Verify `R28_CHANGED_SHA256SUMS.txt` and run the focused `r28_`, `r27c_`, `r27_`, and `album_scope_` tests; investigate only actual failures.
2. Run workspace fmt, full tests, debug/release builds, and both existing smoke scripts as listed in delivery notes. Confirm conversion.log displays `Album gain - TonePoet` (em dash in actual log) and old backend strings remain in structured evidence.
3. If changes were needed to pass a real gate, regenerate patch, hashes, QA, notes, checkpoint, and bundle. Do not open up extra processing components without a demonstrated defect.
4. Once the tree is final, run `scripts/requalify_reference_r20.sh` with a real SACD ISO and all required tools, then run final Reference freshness/full regression checks. Complete real ISO Browse -> Convert -> preset test (#43) separately.
5. The prior potential two-decimal true-peak ceiling mismatch is intentionally unmodified and unverified; do not add a clamp without a separate validated requirement.

## Scope guard

Prefer the smallest coherent correction. Preserve R27C's completed-Reference human projection, gain Off/Track-auto truth, and audio performance. Do not replace backend identities in the provenance record or declare build-host acceptance from static inspection.
