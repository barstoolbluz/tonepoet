# RESUME CHECKPOINT — TonePoet R19 HDCD R5, October 15, 2026

**Task:** implement `BRIEF_R19_HDCD_R5_ui_and_eight_failures_2026-10-15.md` against the attached full source archive. The input was the R4D tree with 7,490 passing tests, eight reproducible targeted failures, and one known Reference source-lock freshness failure. This checkpoint is intentionally useful across interruptions or another model session.

## Concrete identities and rules

- Input archive: `TonePoet_LOGGING_R19_HDCD_R5_2026-10-15.tar.gz` — SHA-256 `71e912e5d53a68a02a97bb41b23f6a73bd608f6f320d26451ff5142a67fbdc1b`.
- R5 brief: `BRIEF_R19_HDCD_R5_ui_and_eight_failures_2026-10-15.md` — SHA-256 `fa99e5a2d54be26b01ac89bf7708bf5e0471d7047e9a1309e4983d0bfac83fe3`.
- Source patch: `R19_HDCD_R5_SOURCE_DELTA.patch`. Strict `git apply --check --whitespace=error` passed against original R4D extraction; after applying, every modified source file matches the delivered R5 tree. `git apply --reverse --check` also passed on R5.
- Only these 10 files are modified relative to original (no Cargo or qualification files changed):
  `tonepoet-pipeline/src/plugins.rs`, `src/convert/pipeline/track_executor.rs`, `src/convert/pipeline/stages.rs`, `src/tui/app.rs`, `src/tui/draw_output.rs`, `src/tui/convert_screen.rs`, `src/tui/keybindings.rs`, `src/tui/command.rs`, `src/tui/event_loop.rs`, `src/tui/context_menu.rs`.
- New deliverables are additive, not replacements for the existing source/configuration/qualification evidence.

## Verified work, not yet verified work

All requested R5 corrections are represented in source; the implementation intent is enumerated in `R19_HDCD_R5_ENGINEERING_HANDOFF_2026-10-15.md`. Baseline manifest passed with 999 entries before modification. A 19-check source audit, four Python/SQLite/FFmpeg probes, and patch reproduction passed. The HDCD help text matches the brief byte-for-byte after joining its wrapped paragraph lines.

**Do not claim `cargo test` passed:** `cargo`, `rustc`, `rustfmt`, and configured Nix/Reference qualification were absent in this environment; the libfdk AAC encoder also was unavailable. This is a candidate for **real build-host verification**, not full acceptance. Source-lock qualification for the moved `plan_bridge.rs`, `plugins.rs`, and `semantic_plan.rs` remains owed; retain original sidecars until genuine `scripts/requalify_reference_r20.sh` completes.

## Resume commands

```bash
cd tonepoet
sha256sum -c R19_HDCD_R5_PACKAGE_SHA256SUMS.txt
# On configured pinned build host:
cargo test --workspace --locked
scripts/requalify_reference_r20.sh
cargo test --workspace --locked
```

In the first Rust gate, review the eight exact tests listed in the R5 brief; the Reference freshness test is expected to be stale until real source requalification. Validate libfdk AAC in the pinned host and recheck CUE direct routing and async queue warning preservation. Record exact failing assertions and make the **smallest coherent corrective patch** only if the build gate supplies concrete failures. Avoid unneeded architecture, performance regressions, and changes to qualified DSD paths or certificate sidecars.

If modifying files later, regenerate **new** package hashes and create a new, uniquely named archive/checkpoint rather than mutating this delivered candidate in place.
