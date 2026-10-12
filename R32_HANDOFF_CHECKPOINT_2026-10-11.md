# TonePoet R32 — true-peak target help, handoff checkpoint

**Date:** 2026-10-11  
**State:** implemented and statically verified; **compiled/build-host acceptance is NOT claimed**.  
**Input:** `TonePoet_LOGGING_R32_true_peak_help_text_2026-10-11.tar.gz` plus `BRIEF_LOGGING_R32_true_peak_help_text_2026-10-11.md`.

## What changed

This is a UI-only, four-source-file change. No DSP, certificate, settings, preset, command parser, or output code was edited.

- `src/tui/draw_output.rs`: PCM and DSD true-peak target rows use one shared visible `ⓘ` help span next to `TP target`. The existing pre-emphasis two-cell glyph/guard styling and palette-dependent contrast are followed. The numeric control is aligned with the other Format-pane controls. Remove `0.25 dB steps; :set ...` from the ordinary row and remove the `:set ...` suffix from the lossy cap warning. Keep the cap warning, inline-edit instructions, full-precision target rendering, and the `custom` pill. Its hit offset is recomputed to match the icon and extra spacing.
- `src/tui/convert_screen.rs`: both active target rows register the same two-cell info hitbox *after* their whole-row and custom-pill targets, so the info click wins. No help target is registered for a target row that is not part of the active row set; the draw-size guard is respected.
- `src/tui/button_map.rs`: one `TuiButton::TruePeakInfo` variant.
- `src/tui/keybindings.rs`: one `ActiveOverlay::Notice` handler with title `True peak` and the six-paragraph message, reproduced *exactly* from the R32 brief. No markup engine or overlay redesign.

New `r32_` Rust regressions check both PCM/DSD rendered help icons and mouse routing, exact hitbox coordinates including guard cell, no target mutation, hidden-row behavior, help glyph contrast on all shipped themes, and removal of command hints while retaining the lossy cap/editor hints. Preserved R30 tests cover the custom pill offset and editor.

## Checks and evidence

**Passed in the available environment:**

- Six source-level static QA assertions, with output in `R32_STATIC_QA.txt`.
- The six help paragraphs match the exact text of the R32 brief including spacing and punctuation; no unsupported HTML/Markdown formatting was introduced.
- Both existing TUI `:set` keys still map to the same `parse_true_peak_target`/settings handler; `src/tui/command.rs` is unchanged.
- The portable `R32_CODE_CHANGES.patch` applies with `patch --fuzz=0 -p1` to the four original files and yields four byte-identical outputs.
- `bash -n R32_ACCEPTANCE.sh` succeeded.
- The refreshed `R32_SOURCE_SHA256SUMS.txt` is an inventory of the delivered regular files (it intentionally excludes itself).

**Not run / not passed:** no Rust toolchain, Cargo, Nix, configured external audio dependencies, or SACD ISO are available here. `R32_ACCEPTANCE.sh` stops cleanly with code 2 when Cargo is absent. Do not report compiled tests, GUI execution, full workspace acceptance, or Reference requalification as passed.

**Input integrity exception:** Of 929 paths in the supplied `R31_SOURCE_SHA256SUMS.txt`, the only pre-R32 mismatch was `docs/OUTSTANDING_ISSUES.md`; the supplied archive contains a later issue #73 entry, exactly as the R32 brief says. R32 does not alter that ledger or rewrite the historical R31 checksum file. The new R32 manifest describes the *actual* delivered tree.

## Technical interpretation of the brief

The requested claim about a certified bound is consistent with the checked production path: `docs/dsd_album_gain_true_peak_authority.md` distinguishes a reported peak from the interval certificate's `upper_level()` and documents conservative terminal-error propagation; `tonepoet-pipeline/src/dsd_album_gain.rs::resolve_true_peak_gain_constraints` applies the hard-ceiling solver. Its `allow_boost` flag is false for Guard and true for Normalize. This distinction is why the supplied help copy does not promise that a quiet track is boosted in all modes. The help copy also retains the brief's explicit caveat about decoded lossy output. No gain arithmetic was changed to accommodate the wording.

## Continue / accept on the qualified build host

From the unpacked repository root, with a real SACD ISO available:

```bash
# Optional source-format check before running the remaining gates:
R32_CHECK_FORMAT=1 bash R32_ACCEPTANCE.sh /absolute/path/to/real-SACD.iso
# Or, when checking formatting separately:
cargo fmt --all -- --check
cargo test --locked --lib r32_
bash R32_ACCEPTANCE.sh /absolute/path/to/real-SACD.iso
```

`R32_ACCEPTANCE.sh` first executes new R32 tests, then chains the **unchanged** `R31_ACCEPTANCE.sh`, `R30_ACCEPTANCE.sh`, and `scripts/requalify_reference_r20.sh`. The latter installs four coherent Reference sidecars only after a fresh qualified real-tool pass, reruns freshness/full-workspace checks, and performs smoke tests. It prints an additional negative-runtime-closure smoke instruction; complete that documented check independently before a release. Do not use `TONEPOET_DSD_REFERENCE_UNQUALIFIED_OVERRIDE=1`.

The R32 brief reported **7437 pass / 2 fail** for R31 on its configured host: a known lease-contention test flake (passed 3/3 alone) and the locked-source freshness test (stale after R30). Those are **input claims**, not measurements repeated in this environment. The R30 locked-source freshness obligation remains outstanding until requalification, not an R32 UI code regression.

## Bundle/reproducibility

- Complete source tree: `TonePoet_LOGGING_R32_TRUE_PEAK_HELP_2026-10-11.tar.gz` (the parent directory is `tonepoet/`).
- Portable minimal patch: `R32_CODE_CHANGES.patch` (`patch -p1`, applied at the repository root).
- `R32_CHANGED_SHA256SUMS.txt`: changed Rust files plus handoff runner.
- `R32_SOURCE_SHA256SUMS.txt`: full package file inventory, excluding itself.
- `R32_STATIC_QA.txt`: captured source-level test result.

If work is interrupted, begin with this checkpoint and the R32 manifest, not earlier handoff claims. Keep the scope to the R32 brief; changes to message emphasis/list rendering belong to separate issue #73.
