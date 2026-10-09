# TonePoet R22C — single-pass human-evidence regression test correction

Date: 2026-10-09. Base: the delivered `TonePoet_R22_complete_source_2026-10-09.tar.gz`, not the original R21 archive. This R22C correction is **test-only**; it does not change the audio execution, evidence projection, log publication, or Browse preset implementation.

## Defect and smallest correction

`src/convert/pipeline/execution_evidence.rs` test
`r22_direct_encode_preserves_sample_facts_without_debug_or_duplicate_fields`
previously called `apply_pcm_terminal_realization` twice on the same mutable
`OperationRecord` and asserted that both projections matched. Its first call
consumes the generic `Audio processing: none` planner parameter. The second
call cannot recover that hint and reports an unnecessary 24-bit-to-24-bit
sample conversion, so the equality assertion was deterministically wrong.

The test now **calls projection only once**, matching the production call
pattern established in the review. It still checks 24-bit PCM input, exact
integer sample values, 96 kHz output, FLAC 24-bit output, samples passed
through unchanged, FFmpeg sample/quantization ownership, no dither, no dither
owner, absence of Rust-internal enum terms, and absence of duplicate dither
and sample-conversion fields.

Only this test was changed in executable source (8 diff lines: five additions,
three deletions). No production-path code, dependency, build script,
Reference qualification sidecar, or other regression test changed.

## Offline verification and limits

The companion `R22C_OFFLINE_QA_2026-10-09.txt` reports the checks performed
against the exact supplied R22 source archive, including one-file source delta,
test-local change boundaries, single invocation, patched-copy byte identity,
unchanged locked sources and sidecars, and updated SHA-256 inventory.
This is **not** a Rust test run; `cargo`, `rustc`, `rustfmt`, and Nix are not
installed here.

## Build-host acceptance gates (still mandatory)

Run from the corrected repository root with the supported Rust/toolchain:

```bash
cargo fmt --all
cargo fmt --all -- --check
cargo test -p tonepoet --lib r22_direct_encode_preserves_sample_facts_without_debug_or_duplicate_fields
cargo test -p tonepoet --lib terminal_realization_evidence_preserves_physical_quantization_truth
cargo test -p tonepoet --lib r22_
cargo test --workspace
cargo build --release
./scripts/smoke_r20_album_publication.sh ./target/release/tonepoet
```

Review the formatter delta before proceeding; do not introduce unrelated
formatting changes. As in R22, perform an actual Browse-folder SACD ISO
preset conversion and Reference requalification with a real SACD ISO:

```bash
./scripts/requalify_reference_r20.sh /absolute/path/to/REAL_SACD.iso
```

Follow its R20 handoff for the negative runtime-mismatch check. The `stages.rs`
change from R22 remains unbound to the installed Reference qualification. Do
not rewrite source-lock sidecars manually or claim qualification until the
configured build host completes the normal gates.

## Applying the correction

The standalone `TonePoet_R22C_test_only_fix_2026-10-09.patch` is relative to
**R22**, not R21. To apply to an unmodified R22 repository:

```bash
git apply --check /path/to/TonePoet_R22C_test_only_fix_2026-10-09.patch
git apply /path/to/TonePoet_R22C_test_only_fix_2026-10-09.patch
```

Alternatively use the complete R22C source archive. Run
`sha256sum -c R22_SOURCE_TREE_SHA256SUMS.txt` from its `tonepoet/` root.
`R22_RESUME_CHECKPOINT_2026-10-09.md` contains the running state and
uncompleted release gates.

Stop after the standard gates pass: the reported defect does not justify any
additional projection machinery or production refactor.
