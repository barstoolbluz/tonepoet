# Addendum: manifests, module declarations, and files the implementer requested

Date: 2026-07-11. Third and final archive for the conversion-actions work,
supplied on the implementer's request. Read order: brief v2 → supplement →
this.

## What this bundle adds and why

- `Cargo.toml` (workspace root) + `Cargo.lock` — the workspace member list,
  dependency structure, and pinned versions. The capability-layer dependency
  decision (cap-std vs rustix/libc openat-family) is yours to make HERE,
  explicitly: pin the version, state the reason in your implementation
  notes, and preserve the existing supported targets (Linux + macOS — the
  flake builds both; no Linux-only syscall may be the sole mechanism).
- All workspace member manifests (`crates/*/Cargo.toml`,
  `tonepoet-pipeline/Cargo.toml`) — dependency direction and feature flags.
  Reminder from the supplement: tui-file-picker is a standalone widget
  crate; main crate → picker, never the reverse.
- `src/lib.rs` — the crate's module declarations and public re-exports
  (`pub mod convert; pub mod config; pub mod tui;` plus helpers like
  `detect_7z_binary`).
- `src/tui/mod.rs`, `src/convert/pipeline/mod.rs` — module declarations for
  the two trees you are extending (`src/convert/mod.rs` is already in the
  base bundle). New modules must be declared here. Note: CLAUDE.md claims
  pipeline mod.rs carries `#![forbid(unsafe_code)]` — that is STALE; the
  forbid exists only file-level on three `dvda_*` modules. There is no
  pipeline-wide unsafe ban, but keep any unsafe in the descriptor layer
  minimal, encapsulated, and justified per site (house standard; probe.rs's
  documented FFI pattern is the precedent).
- `src/tui/template_builder.rs` — explicitly referenced by SR-6
  (`render_template_preview` :568, the canonical-example preview). Includes
  the token help/gallery machinery the wizard's token hints can reuse.
- `src/convert/cue_parser.rs` — CORRECTION: the supplement originally said
  this file was in the base bundle; it was not. It is here. Relevant for its
  `atomic_replace` (:1685) and as frozen-policy context for sidecar
  writeback (`docs/cue_sidecar_writeback_policy.md`, included in this
  bundle — do not change its call sites' behavior).
- `CLAUDE.md` — module-status guidance the brief relies on (e.g. renaming.rs
  "dormant, pending preset system"). Known-stale entries, do not act on
  them: it lists `docs/naming_template_expansion_brief.md`, which does not
  exist on disk; it claims pipeline mod.rs has `#![forbid(unsafe_code)]`
  (file-level on three dvda_* modules only — see above); line-count/size
  notes drift. Where CLAUDE.md and brief v2 disagree, brief v2 governs.

## Unchanged

Everything in brief v2 (including SR-1..SR-8 and the acceptance list) and
the supplement stands as written, with the single cue_parser correction
noted above. This addendum adds context, not requirements.
