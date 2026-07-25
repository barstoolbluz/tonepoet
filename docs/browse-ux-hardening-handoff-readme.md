# Handoff README — Browse UX Hardening Bundle

You are the implementing model. No compiler, no repo beyond this bundle; the applying
side fixes mechanical compile errors, runs gates, and audits.

## Read in this order

1. `docs/browse-ux-hardening-brief.md` — THE task. Seven items; items 1–2 implement a
   user-approved UI design whose ASCII mockups in the brief are the design contract.
   §8 constraints are hard — notably **no function-key bindings anywhere** (byobu
   intercepts F-keys) and quit stays Ctrl+Q.
2. `CLAUDE.md` — project conventions (edition 2021, two-pass mouse registration,
   testing rules).

The brief has been through two mechanical audit rounds (127 claims verified in round
2); its file:line references were re-verified against this exact snapshot (branch
`hardening` @ a781563 + the two 0.4.4 release commits). Anchors may still drift a few
lines — re-locate, don't trust blindly.

## What is in the bundle

- The two docs above, workspace `Cargo.toml`, `CLAUDE.md`.
- Complete `tui-file-picker` crate (the shared-engine home: text input, bookmarks
  store + `BookmarkMutation` model, filesystem clipboard; a shared scrollbar widget
  may reasonably land here — if it does, the crate must keep compiling standalone).
- App-side TUI files in scope: `src/tui/{browse, draw_browse, draw, draw_overlays,
  context_menu, keybindings, command, button_map, bookmarks, bookmarks_overlay,
  theme_builder, theme, text_input, inline_edit, app, event_loop, message,
  display_width, mod}.rs`. `theme_builder.rs` is included as the DESIGN REFERENCE for
  item 1 (imitate its anatomy), not as a file expected to change.
- `BUNDLE_SHA256.txt` — verify with `sha256sum -c BUNDLE_SHA256.txt`.

Files not present exist in the repo — name them in your report rather than guessing
their contents.

## Deliverable format

`.tar.gz` changed-file overlay at repo-relative paths, plus:
- `MANIFEST.md`: every delivered file with a one-line purpose/change summary;
- `ENGINEERING_REPORT.md`: design decisions where the brief leaves choice (add/rename
  input placement in the manager, scrollbar widget location, dropdown sizing),
  per-item test coverage, anything unverifiable without a compiler, files you needed
  but lacked;
- a SHA-256 manifest of delivered files.

Include regression tests per brief §8. Applying-side gate: `cargo test --workspace`,
zero failures, untruncated results.
