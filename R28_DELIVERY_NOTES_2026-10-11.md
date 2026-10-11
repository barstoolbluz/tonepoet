# TonePoet R28 - human-readable backend names

Date: 2026-10-11

## Source and scope

- Input: `TonePoet_LOGGING_R28_backend_names_2026-10-11.tar.gz`, SHA-256 `35752ad292912150cc17b81642be2d00c1698a065e7af47f0ff1c1a164e153b1`.
- Authority: `BRIEF_LOGGING_R28_backend_names_2026-10-11.md`, SHA-256 `0cc6d135ac9f0bd6f7070bd411988f783b26b06524d217cea1c35afd4d2b0a22`.
- One existing source file changed: `src/convert/pipeline/execution_evidence.rs`. One focused `r28_` unit regression was added in that file.
- Exact source-only delta: `R28_CODE_CHANGES_FROM_INPUT.patch`, applicable to the supplied archive's source tree. Original R27/R27C artifacts are historical and remain unmodified.

## Change

`ExecutionBackend::human_label()` is the one established presentation entry point shared by generic per-track operation rendering and the album-level uniform-operation summaries. Its three known native implementation labels now render as their responsible author, `TonePoet`:

| Underlying native implementation identity (forensic record) | Human-log backend |
| --- | --- |
| `TonePoet Float64 scalar pump` | `TonePoet` |
| `TonePoet Float64 scalar materializer` | `TonePoet` |
| `TonePoet DSD realization` | `TonePoet` |

Thus `Album gain - TonePoet` (the log uses an em dash) identifies the operator without exposing a carrier's precision or internal mechanism. The operation itself already tells readers *what* happened; the suffix says *who performed it*. Exact matching limits the new presentation rule to the three legacy labels. External tools (`sox`, `ffmpeg`, `ssrc`, `sacd-rs`), `TonePoet file copy`, `TonePoet qualified Reference path`, and future native backends retain their existing labels.

The serialization model is not changed. Persisted `ExecutionBackend::Native { implementation }`, operation kind/status, references, invocation associations, gain parameters, and all structured forensic evidence remain unmodified. Both failed/discarded attempt renderers and successful track renderers reuse the same generic presentation entry point. No new DSP or CLI/TUI/preset behavior is introduced.

This is intentionally a display-only mapping rather than renaming backend identifiers at the point where execution evidence is recorded in `track_executor.rs` or `stages.rs`. It avoids schema drift and avoids modifying two Reference source-locked files. The small label comparisons happen only during human-log formatting, not per audio sample or per output frame.

## Regression coverage

`r28_native_backend_labels_identify_tonepoet_without_rewriting_evidence` constructs completed typed operations with all three underlying identities. It asserts clean rendered operation lines, absence of the implementation labels from human output, continued presence of those identities in serialized evidence, serialization equality before/after rendering, and exact unchanged labels for existing external tools, named native operations and the future effects stub.

## Verification performed here

- PASS: compared all 893 original regular files to the uploaded archive: only `execution_evidence.rs` differs, no baseline file missing or mode changed.
- PASS: `git diff --check` on the canonical source patch.
- PASS: applied the patch to a separate pristine source copy; resulting source bytes match the delivered source exactly.
- PASS: Pygments Rust tokenization of the edited file reports zero error tokens and a balanced delimiter scan. This is not compilation.
- PASS: none of the 24 Reference-locked source entries changed as a consequence of R28; the preexisting `stages.rs` mismatch remains. (The Cargo root files require the gate's special canonicalization.)
- NOT RUN: Rust format/check, compiled unit tests, workspace build/tests, runtime smokes, real SACD ISO conversion, Reference requalification. `cargo`, `rustc`, `rustfmt`, and `nix` are unavailable in this execution environment.

Full static evidence is in `R28_STATIC_QA.txt`; `R28_CHANGED_SHA256SUMS.txt` lists current R28 files.

## Build-host acceptance

Run from the extracted `tonepoet/` directory on the configured Rust/Nix host:

```bash
sha256sum -c R28_CHANGED_SHA256SUMS.txt
cargo fmt --all -- --check
cargo test --lib r28_native_backend_labels_identify_tonepoet_without_rewriting_evidence
cargo test --lib r27c_
cargo test --lib r27_
cargo test --lib album_scope_
cargo test --lib r24_applied_gain_appears_once_when_uniform_and_on_each_track_when_divergent
cargo test --workspace
cargo build --workspace
cargo build --release
./scripts/smoke_cli_convert_exit_codes.sh ./target/release/tonepoet
./scripts/smoke_r20_album_publication.sh ./target/release/tonepoet
```

Then on the *final* accepted source tree, perform one genuine Reference requalification with actual ISO and required tools:

```bash
./scripts/requalify_reference_r20.sh /absolute/path/to/real-SACD.iso
```

The input already has an out-of-date Reference source lock because the prior R27C change in `stages.rs` was not yet requalified. R28 neither repairs nor worsens that mismatch. Do not claim final qualification based on R28's static checks. The remaining live Browse -> Convert -> preset gesture (#43) requires separate real-SACD acceptance; it is not covered by this label edit.

## Explicit exclusions

No edits to Reference DSP, signal ownership, precision, provenance schema, serialization, tool invocation, execution hot path, gain Off/Track/Album semantics, peak ceiling parser precision, or the test-only `tonepoet::future_dsp` placeholder. The brief's two carried-forward acceptance questions remain open, not R28 blockers introduced by the display correction.
