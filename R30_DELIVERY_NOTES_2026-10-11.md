# TonePoet R30 — focused corrective implementation (2026-10-11)

**Delivery status: source candidate, NOT a qualified Reference release.** This
container has no Cargo, rustc, rustfmt or Nix. No compiled test, real audio
conversion, Reference qualification or freshness gate was run here. The original
installed v18 certification sidecars are deliberately untouched and stale for
this source; they must not be presented as certification of R30.

## Original inputs

- `TonePoet_LOGGING_R30_ceiling_two_decimals_and_custom_pill_2026-10-11.tar.gz`
  SHA-256 `50192aadb4b39479cf8356aa63b48ae9934c9a32d38e56c49f9b0a18b38ee330`.
- `BRIEF_LOGGING_R30_ceiling_two_decimals_and_custom_pill_2026-10-11.md`
  SHA-256 `d3d36c7ae90e8d6a75fa2af65cc112e905380340bb25688f5aafb7b56d768ca8`.
- The brief reports R29C accepted into the downstream tree at `572063d`,
  with 7,427 test passes and two failures (R27 ceiling presentation and expected
  qualification freshness). These are **user-supplied prior build-host results**,
  not independently rerun in this session.

## Implementation

1. **Human-log ceiling**: `human_log_ceiling` again displays at least two
   fractional digits, so `-0.1` becomes `-0.10` and integer `-12` becomes
   `-12.00`. Significant digits through the nanodecibel place remain exact:
   `-0.0001`, `-0.000000001`. Zero displays as `0.00`. This is presentation
   only; the encoded target, DSP, processing and forensic evidence are unchanged.
2. **Actionable `custom` pill**: PCM and DSD true-peak target rows show a
   distinct clickable pill. Clicking it or pressing Enter on the focused row
   opens an editor in the row. Its input supports existing text-selection
   semantics, nine-place decimal precision, Enter to validate/commit, Esc to
   cancel, and Tab to commit and advance. Invalid Enter keeps the editor
   available for repair; invalid blur/hidden-field transitions discard the
   draft and retain the prior setting. The row and pill map use the same
   computed geometry. The `:set` command keys remain available.
3. **Unsigned magnitude semantics**: bare positive `0.0001` represents
   **headroom**, committing ceiling `-0.0001 dBTP`, in both inline and `:set`
   paths. An explicit negative `-0.0001` means the same ceiling. Explicitly
   positive `+0.0001` is rejected with an explanatory error. The existing
   inclusive range `-12..0 dBTP`, allowance of `0`, and nine-decimal exactness
   remain. State/provenance change only after valid admission.
4. **`missing_docs`**: added comments to `TruePeakTargetOrigin`, its members
   and methods, and the DSD provenance field in `PipelineSettings`. No schema,
   serialization or runtime behavior changed.
5. **Targeted regressions**: numeric two-decimal + nanodecimal examples;
   visible button geometry in rendered ratatui buffers; both PCM/DSD mouse
   activation, inline value commit, invalid Enter, cancellation, blur behavior;
   `:set` positive-magnitude conversion and positive-sign refusal.

Changed sources: `src/convert/pipeline/stages.rs`, `tonepoet-pipeline/src/settings.rs`,
`src/tui/app.rs`, `src/tui/button_map.rs`, `src/tui/command.rs`,
`src/tui/convert_screen.rs`, `src/tui/draw_output.rs`,
`src/tui/format_interactions.rs`, `src/tui/keybindings.rs`.

No dependency upgrades, audio-processing changes, new persistence schema,
qualification-sidecar changes, or unrelated redesign were made.

## Reproducibility and verification

- `R30_FROM_R29C.patch`: source-level diff relative to the supplied R30
  bundle's complete baseline tree (label R29C); independently replayed onto
  the original baseline tree and checked byte-for-byte for all edited paths.
- `R30_CHANGED_SHA256SUMS.txt`: hashes for the nine edited Rust sources.
- `R30_SOURCE_SHA256SUMS.txt`: full delivered-source inventory (excluding
  this manifest itself), with regular-file hashes and symlink target records.
- `R30_STATIC_QA.txt`: exact static checks performed and limitations.
- `R30_RESUME_CHECKPOINT_2026-10-11.md`: recovery and outstanding gates.

## Required acceptance on a configured build host

1. Review any necessary Rust formatting and finalize sources **before**
   Reference requalification. Do not blanket-format unrelated files merely
   because the large inherited workspace has differing style.
2. Run `./R30_ACCEPTANCE.sh /absolute/path/to/real-SACD.iso` to run focused
   R30/R29/R29C tests, `settings_sentinel`, Reference source-lock
   requalification, workspace tests, release builds, CLI/album smokes, and a
   positive qualified Reference conversion. The existing transactional
   `scripts/requalify_reference_r20.sh` is invoked unchanged.
3. Perform the repository's documented negative runtime-closure mismatch smoke
   and inspect the human log for ordinary/fine-grained `Ceiling` presentation.
4. Never waive an unexpected compilation failure or genuine test failure.
   Investigate the known staging lease-ownership contention flake if observed,
   but do not treat it as a general allowance to ignore red gates.
5. Repack only from the source with **fresh coherent four-piece qualification
   evidence** and attach logs/hashes proving acceptance; never copy stale
   qualification claims onto this candidate.

**Release decision: HOLD until these build-host gates pass.**
