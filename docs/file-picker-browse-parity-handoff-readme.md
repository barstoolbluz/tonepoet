# Handoff README — File Picker / Browse View UX Parity Bundle

You are the implementing model. You have no compiler and no repo access beyond this
bundle; the applying side will fix compile errors, run gates, and audit.

## Read in this order

1. `docs/file-picker-browse-view-parity-brief.md` — THE task. Its §1 governing
   decisions override the spec where they conflict. §5 constraints are hard
   (notably: **no function-key bindings anywhere** — byobu intercepts F-keys).
2. `docs/file-picker-browse-view-gap-analysis.md` — verified current-state analysis
   (twice-audited; §0a decisions, §5 verification results).
3. `docs/file-picker-browse-view-interaction-specification.md` — the underlying spec,
   as amended by the brief's decisions.
4. `CLAUDE.md` — project conventions (edition 2021, two-pass rendering, error/test
   conventions).

## What is in the bundle

- All three docs above + `CLAUDE.md` + workspace `Cargo.toml`.
- The complete `tui-file-picker` crate (`crates/tui-file-picker/`).
- The app-side TUI files in scope: `src/tui/{browse, draw_browse, context_menu,
  keybindings, text_input, inline_edit, bookmarks, bookmarks_overlay, app, event_loop,
  draw_overlays, external_editor, command, button_map, draw, message, theme,
  display_width, mod}.rs`.
- `BUNDLE_SHA256.txt` — hashes of every file in the bundle
  (verify: `sha256sum -c BUNDLE_SHA256.txt`).

Files NOT in the bundle exist in the repo (e.g. other `src/tui/*.rs` modules, other
crates). If you need one, say so in your report rather than guessing its contents.
`src/tui/keybindings.rs` is ~41K lines and `browse.rs` ~16.7K — the brief's line
anchors were re-verified against this exact snapshot.

## Deliverable format

Return a `.tar.gz` containing changed/new files at repo-relative paths, plus:

- a manifest listing every file with a one-line change summary;
- a written report covering: per-subsystem sharing choice (extracted / parallel /
  host hook) with rationale; the running list of intentional surface differences
  (spec §1); anything you could not verify without a compiler; and any bundle files
  you needed but did not have.

Tests: include new/updated tests per brief §5 (regression cover for preserved §2
baselines and coverage for new behavior). The applying side runs
`cargo test --workspace` and audits before merge.
