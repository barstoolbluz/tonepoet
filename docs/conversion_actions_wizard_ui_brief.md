# Brief: conversion-actions wizard UI — two-dialog reshape + Output Options surfacing

Date: 2026-07-13. Scope: TUI-ONLY round. The action engine, safety perimeter,
journals, election, and CLI behavior are out of scope and must not change.
Baseline: suite 3134/0, zero cold-build warnings, branch `working` at e0c04b8.

## What already exists (do not rebuild)

- `src/tui/conversion_actions_ui.rs` (~1,600 lines): a working wizard behind
  the `:actions` vi command — currently ONE modal with PRE/POST phase tabs and
  three columns (Available 24% / Pipeline 33% / Config+Preview 43%), keys:
  Tab focus, Enter add/edit, `[`/`]` reorder, `d` delete, `s` commit, Esc.
  `WizardKeyResult::Commit(draft)` returns the edited `ActionPipeline` to
  `app.convert.output_options.actions` (session state).
- Live dry-run preview machinery: `refresh_wizard_preview_for_app(state, app)`
  builds a simulated-album preview context from current app metadata; the
  `:actions-run` flow has its own plan/preview/apply overlay (untouched here).
- Persistence: presets capture the pipeline (`src/tui/presets.rs`:
  `actions: ActionPipeline` saved/loaded with output options);
  `config.toml [conversion.actions]` seeds the session at startup
  (`app.rs` ~9103) and is also the CLI default.
- TUI conventions (must follow): two-pass rendering — draw functions take
  immutable state; mouse targets are registered in a second pass via
  `ButtonRenderMap` (`button_map.rs`); Tokyo Night theme (`theme.rs`);
  `draw_output_options.rs` exposes row-offset constants
  (`OUTPUT_OPTIONS_*_ROW`) shared with mouse hit registration so rendered rows
  and clickable rows cannot drift.

## Deliverable 1 — Output Options pane surfacing

Add an `Actions` section to the Output Options pane (cyan), after the existing
`Conversion` section, with one focusable row summarizing the pipeline:

```
│   Conversion                                               │
│   force encode   [ ]                                       │
│   disc subfolders[x]                                       │
│   write log      [x]                                       │
│                                                            │
│   Actions                                                  │
│   pipeline    ▸ 1 pre · 4 post          Enter/click edit   │
```

Empty pipeline renders dim: `pipeline    ▸ none`.

- New `OutputOptionsField::Actions` variant participating in the pane's
  existing field-focus cycle (arrow keys / Tab within the pane).
- Enter while focused, or mouse click anywhere on the row, opens the wizard
  (same code path as `:actions`).
- Follow the row-offset-constant pattern for the mouse hit registration.
- The summary must live-update after the wizard commits.

## Deliverable 2 — reshape the wizard to the two-dialog flow

The mockup (`conversion_actions_wizard.html`, directional not pixel-exact) is
authoritative for STRUCTURE; two panes fit small terminals better than three.

### Dialog A — the pipeline view

```
┌─ Conversion actions ───────────────────────────────────────────────────────┐
│ Runs before & after each conversion.        Adding to   ● Post   ○ Pre     │
├────────────────────────────┬───────────────────────────────────────────────┤
│ Available                  │ Pipeline                                      │
│ ▸ Rename files             │ Pre-conversion                                │
│   Copy files               │   (none yet)                                  │
│   Move files               │                                               │
│   Delete files             │ Post-conversion                               │
│   Create folder            │ 1 Rename   *.log *.cue      template          │
│   Run script               │ 2 Fixcaps  *.txt                              │
│                            │ 3 Copy     cover.jpg → dest                   │
│ Enter → add to pipeline    │ 4 Run      finalize.sh                        │
│                            │                                               │
│                            │ space configure · ↑↓ reorder · del remove     │
├────────────────────────────┴───────────────────────────────────────────────┤
│  Enter Add    space Configure    ↑↓ Reorder    s Save    Esc Done          │
└─────────────────────────────────────────────────────────────────────────────┘
```

- TWO panes: Available (left), Pipeline (right). The pipeline pane shows the
  Pre-conversion and Post-conversion lists TOGETHER, ordered, always visible —
  replacing the current PRE/POST tab pair.
- The `Adding to ● Post ○ Pre` radio (header right) selects where a newly
  added action lands. Selection in the pipeline list is phase-aware (moving
  the cursor across the Pre/Post boundary is fine; reorder stays within a
  phase; moving an action BETWEEN phases is allowed via a dedicated key —
  pick one, document it in the footer or the pipeline hint line).
- Adding an action opens Dialog B immediately (configure-on-add), preselected
  with defaults; Esc from Dialog B on a fresh add removes the placeholder.
- `space` (or Enter) on a pipeline entry opens Dialog B for it.
- Keep `s` = commit draft to session (existing semantics). ADD a
  "save as default" affordance (e.g. `S`) that commits AND writes the pipeline
  to `config.toml [conversion.actions]` via `TonepoetConfig` (confirm
  overwrite in the status line, no extra modal). Esc = cancel (existing
  confirm-on-dirty behavior if present; otherwise discard silently as today).

### Dialog B — per-action config box (modal over Dialog A)

```
┌─ Configure · Rename files ──────────────────────────────────────────────────┐
│  Mode   ● Template   ○ Uppercase   ○ Lowercase   ○ Fixcaps                  │
│                                                                             │
│  Target   [ *.log, *.cue ]   matches 2 files                                │
│                                                                             │
│  Template [ %ALBUM% ]   tokens  %ARTIST% %ALBUM% %DISC% %TRACK% %YEAR%      │
│                                                                             │
│  Preview  dry-run · 2 operations                                            │
│   Disc 1.log → Deep Purple – Nobody's Perfect (Japan / SHM) [Disc 1].log    │
│   Disc 1.cue → Deep Purple – Nobody's Perfect (Japan / SHM) [Disc 1].cue    │
│                                                                             │
│  Re-running plans 0 operations when names already match.                    │
├─────────────────────────────────────────────────────────────────────────────┤
│  Apply                                                       Esc Cancel    │
└─────────────────────────────────────────────────────────────────────────────┘
```

- One config box per action KIND, fields per the existing config editors in
  `conversion_actions_ui.rs` (mode radio is rename-only; copy/move get
  destination; runscript gets script path/args/timeout; all get target +
  exclude where the engine supports them). Reuse the existing field
  model/validation — this is a re-layout, not a re-model.
- Full-width live dry-run preview (reuse `refresh_wizard_preview_for_app`
  plumbing), refreshed on every field edit; show the operation count and the
  idempotency note exactly as mocked.
- "matches N files" hint next to Target when the preview context can resolve
  it; omit silently when it cannot.
- Apply = write the action back into the draft pipeline and close;
  Esc = discard field edits (and remove the entry if it was a fresh add).

## Deliverable 3 — rich mouse support (mouse and keyboard are coeval)

Every interactive element in both dialogs and the pane row must be clickable,
registered through the standard second-pass `ButtonRenderMap`:

- Pane row: click opens the wizard.
- Dialog A: click an Available entry to select; DOUBLE-CLICK to add (and open
  Dialog B). Click a pipeline entry to select; double-click to configure.
  Click the `● Post / ○ Pre` radio to switch. Click footer buttons
  (Add / Configure / Reorder is keyboard-only — instead render ▲▼ nudge
  buttons on the selected pipeline row for mouse reorder). Scroll wheel
  scrolls the hovered list.
- Dialog B: click a field to focus it (entering edit mode consistent with the
  pane's inline-edit conventions); click a Mode radio to select; click a token
  chip to INSERT that token at the template cursor; click Apply/Cancel.
  Scroll wheel scrolls the preview when it overflows.
- Double-click detection: if the codebase has no existing double-click helper,
  implement one (interval ~400ms, same cell/target) in `keybindings.rs` or
  `button_map.rs` where mouse events dispatch — reusable, not wizard-local.

## Constraints

- TUI-only: no changes under `src/convert/` except none at all — if the UI
  needs data the engine doesn't expose, surface that in your report instead
  of changing the engine.
- Respect small terminals: both dialogs must degrade (the existing wizard's
  `centered_rect(92, 88, …)` sizing and the pane's `area.height < 5` guards
  show the pattern). Dialog B must fit 80×24.
- Existing tests in `conversion_actions_ui.rs` cover the wizard state machine
  and key handling — update them to the new flow rather than deleting;
  add coverage for: radio phase targeting, configure-on-add cancel removing
  the placeholder, between-phase move, save-as-default write, double-click
  dispatch, and the pane row summary rendering.
- `:actions`, `:actions-run`, `:actions-identity-import` keep working
  unchanged (they share state entry points).
- Suite baseline 3134/0 must hold; zero cold-build warnings; the sandbox
  cannot compile — favor mechanically verifiable changes.

## Files in this bundle (the strict slice)

- this brief
- `src/tui/conversion_actions_ui.rs` — the wizard (reshape target)
- `src/tui/draw_output_options.rs` — pane row (Deliverable 1)
- `src/tui/app.rs` — AppState/OutputOptionsState/OutputOptionsField/overlay enum
- `src/tui/keybindings.rs` — key + mouse dispatch
- `src/tui/button_map.rs` — TuiButton/ButtonRenderMap
- `src/tui/command.rs` — `:actions` entry (context)
- `src/tui/draw_overlays.rs` — overlay dispatch (context)
- `src/tui/presets.rs` — pipeline persistence in presets (context)
- `src/tui/theme.rs` — theme (context)
- `src/tui/inline_edit.rs` — inline field editing conventions (context)
- `src/config.rs` — `[conversion.actions]` for save-as-default
- `src/convert/pipeline/mod.rs` — ActionPipeline re-exports (reference only)
