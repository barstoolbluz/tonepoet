# TonePoet R27 — Album Processing human-log corrective

## Intake and boundaries

Input: `TonePoet_LOGGING_R27_album_processing_section_2026-10-10.tar.gz` and `BRIEF_LOGGING_R27_album_processing_section_2026-10-10.md` (October 10, 2026). The brief reports that upstream R26 was committed and Reference requalified at `24ce2cd`, with its runner exiting successfully and the real-SACD smoke green. Those upstream results are **reported, not reproduced in this environment**. The provided archive contains no Git metadata.

**Existing source changed:** `src/convert/pipeline/stages.rs` only. No changes to DSP, planner, source extraction, encoder execution, signed Reference authority, existing transcript/portable execution schema, locking, publication, dependencies, or performance-sensitive audio loops. The R27 changes affect conversion-log fragment human presentation and its focused unit tests. New R27 files are delivery artifacts.

## Result

1. **Reference gain:** Qualified successful uniform Reference albums show one `Album gain — <actual tool>` group only when each track's *completed* `apply_gain` operation matches the bound terminal scalar and the linked automatic-policy decision. The applied **music-level** gain is the terminal scalar less exactly the 12.000 dB Reference protection/restore contribution. The general DSD export compensation is not subtracted. Other or divergent routes retain per-track evidence; no incomplete decision is elevated to performed gain.
2. **Precision:** `DbNano` measurements (gain, true peak) retain the nine decimal places already recorded by the analyzer/authority. A configured ceiling displays **at least** two decimal places and only additional significant accepted digits. This intentionally does **not** reject existing higher-precision CLI, TUI or preset values, which would be a cross-component breaking change. An analyzer finite-near-zero `i64::MIN` reporting sentinel is described as `below reporting resolution`, never presented as an actual dBTP value; verified silence is distinct.
3. **Album layout:** In the qualified uniform Reference route: extraction (including DST status and track count), blank line, gain tool/value/peak/ceiling/automatic reason, blank line, delivered PCM rate/bit depth/channels and dither. The protected 64-bit intermediate, terminal scalar and fixed restoration, generic `Sample contract` and automatic-policy jargon are omitted from the **human** album summary. Output rate and contract are derived from the completed typed operations; unfamiliar/mismatched terminal facts fall back to the established generic presentation rather than being silently omitted. Redundant `Uniform across ...`/pure-DSD scope headers are removed in this route, but disclosure of excluded non-DSD tracks is preserved.
4. **Per-track fallback:** For the new qualified Reference fragments, the temporary human evidence clone also replaces the internal terminal scalar with the actually completed music-level gain and hides the fixed restore and recognized internal working precision. The original portable execution evidence is untouched, still containing the terminal scalar, intermediate format, observations, physical invocation linkage and all other forensic data.

The `reference_album_gain` addition to the coordination track fragment is **optional** and has a serde default, allowing old fragments to deserialize. It is presentation data, not proof of execution; proof remains the linked completed operation and automatic policy decision. No portable-evidence schema was revised.

## Verification in this environment

- **PASS:** compared the uploaded source archive against the extracted tree; the only existing file changed is `src/convert/pipeline/stages.rs`.
- **PASS:** `git diff --check` on the one-file source patch.
- **PASS:** `git apply --check` and apply on a fresh copy of that file from the uploaded archive; resulting SHA-256 is byte-identical to the delivered source.
- **PASS:** RustLexer static tokenization of the 83k-line source: zero lexical errors and balanced punctuation/brace nesting. This is a lexical check, **not** compilation.
- **ADDED, NOT EXECUTED:** focused Rust unit regressions covering exact Reference album text; unchanged structured scalar/intermediate evidence; missing/mismatched gain completion; unfamiliar terminal facts; mixed-source exclusions; sentinel measurement; configurable ceiling precision, and the existing R24/R23 assertions updated to their changed presentation contracts.
- **NOT EXECUTED:** `cargo`, `rustc`, `rustfmt`, `nix`, compiled tests, Reference source requalification and live SACD/TUI workflows are unavailable here. Do not interpret these static checks as build-host acceptance.

## Build-host acceptance

From the extracted `tonepoet/` directory on the production qualification host:

```bash
cargo fmt --all -- --check
cargo test --lib r27_
cargo test --lib album_scope_
cargo test --lib r24_applied_gain_appears_once_when_uniform_and_on_each_track_when_divergent
cargo test --lib nonempty_unowned_workspace_cannot_be_claimed
cargo test --workspace
cargo build --workspace
cargo build --release
./scripts/smoke_cli_convert_exit_codes.sh ./target/release/tonepoet
./scripts/smoke_r20_album_publication.sh ./target/release/tonepoet
```

`src/convert/pipeline/stages.rs` is part of `REFERENCE_COMMON_SOURCE_PATHS`. Its R27 edit invalidates the prior Reference freshness identity even though DSP is unchanged. First resolve any unexpected failures, then run Reference requalification **once on the final accepted tree**, with a real SACD ISO:

```bash
unset TONEPOET_DSD_REFERENCE_UNQUALIFIED_OVERRIDE
./scripts/requalify_reference_r20.sh /absolute/path/to/real-SACD.iso
```

Rerun the full gate after requalification as required by the host's existing process. Do not mutate the Reference lock to suppress freshness, claim a successful gate here, or treat a freshness-only failure *before* requalification as a code regression. Manually verify the still-outstanding real-SACD **Browse → Convert → preset** gesture (#43); the present archive cannot complete that UI acceptance on the actual host.

## Delivery files

- `R27_CODE_CHANGES.patch`: source-only unified diff, based on the exact supplied archive.
- `R27_CHANGED_SHA256SUMS.txt`: current-source and R27-artifact hashes (superseding the historical R26 source hash).
- `R27_STATIC_QA.txt`: repeatable offline gates/results.
- `R27_RESUME_CHECKPOINT_2026-10-10.md`: interruption-safe resumption checklist and remaining qualification work.
