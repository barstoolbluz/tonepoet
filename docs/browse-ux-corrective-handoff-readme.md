# Handoff README — Browse UX Corrective Round 2 Bundle

You are the implementing model. No compiler, no repo beyond this bundle; the applying
side compiles, fixes mechanical errors, runs gates, and audits.

## Read in this order

1. `docs/browse-ux-corrective-brief.md` — THE task. Twelve items: two P0 filesystem
   defects (P0-1 move guard, P0-2 no-clobber fallback — note the governing principle:
   pragmatic degraded modes for non-ext4 filesystems), one downgraded-to-tests item
   (P0-3 bookmark migration pinning), one instrumented mystery (item 4 dead Ctrl+V —
   "figure out what consumes it" is the assignment, with a passing probe test
   included), and eight UX corrections. The brief was audited in two adversarial
   rounds against THIS exact snapshot; audit refinements are inline (e.g., existing
   Unsupported fallbacks to imitate, the retry-skip hypothesis for P0-1).
2. `docs/browse-ux-hardening-brief.md` — the prior round's brief, for context on what
   v5 built (do not regress it).
3. `CLAUDE.md` — project conventions.

## Hard constraints

- NO function-key bindings (byobu). NO emojis or decorative unicode (see brief item 5
  ruling — plain words; functional state glyphs only). Ctrl+Q stays quit.
- `src/db.rs` IS in this bundle this round — no patch indirection needed; deliver it
  as a complete file if you change it.
- If you need a file not present, request it in your report; for anything you patch
  blind, record preimage hashes FROM THIS BUNDLE.

## What is in the bundle

The two briefs + this readme, `CLAUDE.md`, workspace `Cargo.toml`, `src/db.rs`, the
complete `tui-file-picker` crate, and the in-scope app files:
`src/tui/{browse, draw_browse, draw, draw_overlays, draw_output, context_menu,
keybindings, command, button_map, bookmarks, bookmarks_overlay, bookmark_workers,
theme_builder, theme, text_input, inline_edit, app, event_loop, message,
display_width, mod}.rs`. `draw_output.rs` and `theme_builder.rs` are style
references. `SHA256SUMS` verifies every file.

## Deliverable format

Overlay `.tar.gz` at repo-relative paths + `MANIFEST.md` (per-file one-liners) +
`ENGINEERING_REPORT.md` (per-item resolution incl. the item-4 root cause you found,
the non-ext4 capability policy you chose, test coverage, unverifiable-without-
compiler list) + `SHA256SUMS`. Applying-side gate: `cargo test --workspace`, zero
failures, untruncated.
