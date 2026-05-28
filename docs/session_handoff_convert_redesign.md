# Session Handoff: Convert Screen Redesign Brief

**Date:** 2026-05-28
**Repo state:** Commit `4fa6e80` on `main`
**Your task:** Research the current convert screen architecture, then write a reasoning model brief for a collapsible/scrollable pane redesign. Produce the brief as `docs/convert_screen_redesign_brief.md`. Do NOT make code changes — only produce the brief document.

## User's Vision

The convert screen currently has 4 fixed panes (Source, Metadata, Format, Output Options). The user wants:

1. **Each pane collapsible/expandable** — click or key to collapse a pane to a 1-line summary bar. Collapsed panes give their vertical space to the remaining expanded panes.
2. **Scroll support within panes** — when content exceeds the pane's visible height (e.g., 100 enqueued files in the source pane), the pane becomes scrollable.
3. **Metadata pane as a scrollable file list** — show all enqueued files, click one to edit its metadata. This transforms the metadata pane from a static display into a mini file browser with per-file editing.
4. **Source pane summary** — first 5-6 lines summarize the batch (format, total files, total size), expandable to see individual files.

## What to Research

### Current Convert Screen Architecture

Read these files thoroughly:

- `src/tui/convert_screen.rs` — Main layout: how the 4 panes are sized and positioned. This is the critical file.
- `src/tui/draw_source.rs` — Source pane rendering (amber border)
- `src/tui/draw_metadata.rs` — Metadata pane rendering (purple border)  
- `src/tui/draw_output.rs` — Format pane rendering (green border) — format/rate/depth/dither/RG pills
- `src/tui/draw_output_options.rs` — Output options pane rendering (cyan border) — dest/templates/merge
- `src/tui/app.rs` — `ConvertState`, `SourceState`, `MetadataState`, `FormatState`, `OutputOptionsState` — the state model for each pane. Search for these struct definitions.
- `src/tui/keybindings.rs` — `handle_convert_key` function — how keyboard navigation works across panes. Also check mouse handling for convert screen buttons.
- `src/tui/pill.rs` — `PillState<T>` generic pill selector widget used in the format pane
- `src/tui/button_map.rs` — `TuiButton` variants for convert screen elements

### Design Patterns to Follow

Read the memory files for coding conventions:

- `~/.claude/projects/-home-daedalus-dev-tonepoet/memory/feedback_no_bare_char_actions.md` — colon commands for overlay actions
- `~/.claude/projects/-home-daedalus-dev-tonepoet/memory/feedback_keyboard_mouse_coeval.md` — every action needs keyboard, mouse, AND context menu paths
- `~/.claude/projects/-home-daedalus-dev-tonepoet/memory/feedback_overlay_button_dispatch.md` — overlay button dispatch pattern

### Reference Implementation

The collapsible queue sub-lines (just committed at `4fa6e80`) are a simpler version of the same pattern — `tracks_collapsed: bool` on queue items, conditional rendering, `▼`/`▶` indicators, Tab toggle + mouse click + context menu. The pane collapse system should follow the same UX vocabulary but at the pane level.

## Design Questions to Address in the Brief

1. **Layout redistribution:** When a pane collapses, how do remaining panes split the freed space? Equal distribution? Or does one "primary" pane (like Source or Format) absorb it?

2. **Collapse indicator:** Where does the expand/collapse affordance live? In the pane's border/title bar? A clickable `▼`/`▶` in the border?

3. **Collapsed summary:** What does each pane show when collapsed to 1 line?
   - Source: `"12 files · 4.2 GB · FLAC"`
   - Metadata: `"Artist: Various · Album: ..."`
   - Format: `"FLAC · 44.1 kHz · 16-bit · TPDF"`
   - Output: `"~/music/converted/ · %NN% - %TITLE%"`

4. **Scroll model:** Per-pane scroll offset + visible height. What widget handles this? Ratatui has `ScrollbarState` and `List` with scrolling. Does the existing pill navigation in the format pane conflict with pane-level scrolling?

5. **Metadata file list:** Is this a new `List` widget inside the metadata pane, or a repurposed version of the browse file list? How does click-to-edit work — inline editing, or opens the metadata editor overlay?

6. **Keyboard navigation:** Currently Tab cycles between panes. With collapsible panes, does Tab skip collapsed panes? How does the user expand a collapsed pane — navigate to it then press a key?

7. **State persistence:** Should collapse state persist across screen switches (Convert → Queue → Convert)?

## Brief Format

Follow the same format as `docs/per_track_progress_robustness_brief.md`:
- Context section explaining the motivation
- Non-negotiable constraints
- Feature-by-feature specification with file-level change tables
- Line numbers from the current codebase
- What NOT to change
- Test/verification requirements

The brief should be detailed enough that a reasoning model can produce complete, compilable replacement files for all affected source files.

## Important Constraints

- The convert screen code is render-only (immutable state refs). Mouse buttons are registered in a second pass via `ButtonRenderMap`. Follow this two-pass pattern.
- `PillState<T>` is a generic widget used across the format pane. The redesign must not break pill navigation or rendering.
- The format pane has constraint cascading (`apply_format_constraints`) that disables/enables pills based on the selected format. This must continue to work.
- Do NOT touch the queue screen, progress pipeline, or any conversion logic. This is purely a TUI layout change.
