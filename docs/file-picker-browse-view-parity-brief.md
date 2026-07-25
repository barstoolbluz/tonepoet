# Implementation Brief — File Picker / Browse View UX Parity

Inputs: `docs/file-picker-browse-view-interaction-specification.md` (the spec) and
`docs/file-picker-browse-view-gap-analysis.md` (verified current-state analysis,
including §0a decisions and §5 verification results). Read both before starting.
Line numbers below are anchors from the analysis pass, not contracts — re-locate before
editing.

## 1. Governing decisions (override the spec where they conflict)

1. **Option C consolidation.** The Browse view stays its own implementation. Do NOT
   rebase Browse's panes on `FilePickerState` or otherwise rewrite working Browse UI for
   architecture's sake. Consolidate by extracting small, dependency-free engines into
   `tui-file-picker` (or a lower-level shared module) and re-pointing both surfaces at
   them — but only where extraction is cheap and low-risk. Where extraction is not
   cheap, matching UX behavior via parallel implementation is acceptable.
2. **The bar is UX parity, not code identity.** The spec sentence "The matching
   algorithm, case behavior, timeout behavior, and cycling behavior must be identical"
   (§5) and similar "identical" phrasing elsewhere was agent-added language, not a user
   requirement. Required: the user cannot tell the two surfaces apart for equivalent
   interactions. Not required: byte-identical algorithms or a single shared code path.
3. **Picker search is filesystem-only.** The picker gets recursive *filename* search over
   the filesystem. It must NOT get tag-aware search (Tags/Both modes) or the tag
   database. Tag search remains Browse-only; document this as an intentional difference
   per spec §1/§7.

## 2. What exists today (baselines to preserve — do not regress)

These are verified-compliant behaviors. Spec §16 requires regression coverage for them.

- **Browse inline creation** is already inline in the list, not modal
  (`src/tui/draw_browse.rs:1762-1787`, commit path `src/tui/keybindings.rs:29261-29318`).
- **Browse §9.3 targeting is correct**: `ContextAction::NewFile/NewFolder` always create
  in `app.browse.current_dir` (`keybindings.rs:29243-29259`), never inside a
  right-clicked folder item.
- **Directory rename works** via context menu (File operations → Rename,
  `src/tui/context_menu.rs:708-729, 370-382`) and inline rename (only archive
  directories blocked, `keybindings.rs:1825-1828`).
- **Delayed-click rename** in Browse: 500ms double-click window; a second click on the
  same path after the window triggers inline rename
  (`keybindings.rs:31835-31860`, `src/tui/app.rs:10364-10370`,
  `src/tui/event_loop.rs:609-635`).
- **Right-click re-targets before opening the menu** (`keybindings.rs:31100-31102`) —
  context actions never act on a stale cursor.
- **Browse search subsystem** (`src/tui/browse.rs:1698-1754` SearchState, async recursive
  workers `browse.rs:5707-5941`, Esc cancel `browse.rs:5029-5043`).
- **Bookmarks** with add/rename/delete/navigate and TOML+SQLite persistence
  (`src/tui/bookmarks.rs`, `src/tui/bookmarks_overlay.rs`). The overlay opens via the
  `:bookmarks`/`:bm` command (`src/tui/command.rs:7374-7383`) or the context menu
  (`ContextAction::OpenBookmarks`, `context_menu.rs:1623-1624`) — there is NO Ctrl+B
  binding. In-overlay keys: `handle_bookmarks_overlay_key` (`keybindings.rs:29479+`;
  delete 29535, rename 29541, navigate 29545). Navigation to a dead bookmark already
  shows "bookmark: path no longer exists" (`keybindings.rs:29557-29559`).
- **Path bar**: any click on the breadcrumb opens the editor with the full path
  pre-selected (`TextInputState::new_selected`, `browse.rs:4976`). This satisfies the
  spec's double-click-select-all intent; keep it.
- **Type-ahead baseline** (Browse): case-insensitive, prefix-first then substring, search
  from index 0, no cycling/wrap, failed match leaves cursor unchanged, buffer clears on
  1500ms timeout (`browse.rs:12, 4227-4253`). This is the behavior both surfaces should
  exhibit (with the directory-priority fix below).
- **Picker filesystem clipboard**: cut/copy/paste with modes and validation
  (`crates/tui-file-picker/src/state.rs:72-82,582`, `input.rs:539-551`). This is the
  model Browse should adopt (possibly generalized to multiple paths for multi-select).
- **Text input widget**: `src/tui/text_input.rs` — selection anchor, ranges, internal
  clipboard, Ctrl+Shift+←/→ word extension, Ctrl+X/C/V, inverse-video selection
  rendering via `src/tui/inline_edit.rs`. This is the editing engine to share.

## 3. Defects in existing behavior (spec violations to fix)

1. **Picker modal naming popup.** All name editing — new file, new folder, rename, AND
   duplicate — goes through the same centered popup
   (`crates/tui-file-picker/src/render.rs:190-191, 971-994`; title chosen from
   `pending_name_action`). Spec §9.1/§13: naming must be inline in the pane; a modal
   must not be used. Browse's inline create line is the reference behavior. The
   migration covers all four name-editing flows, not just create.
2. **Type-ahead lacks directory priority** (Browse). `browse.rs:4240-4248` scans entries
   uniformly. Spec §5: first matching directory wins; a file only when no directory
   matches. Fix wherever the (shared) matcher ends up.
3. **Ctrl+A in text editors is readline-Home, not select-all**
   (`src/tui/text_input.rs`, deliberate, commented). Spec §12/§13.2 require Ctrl+A =
   select-all. DECIDED (user, 2026-07-24): **switch Ctrl+A to select-all** in the shared
   text-editing engine (path bar, inline name editors, and any other consumers of the
   shared widget). Remove or update the code comment defending the readline binding.
   Home/End remain available for cursor movement.

## 4. Gaps to close

Grouped by surface. See the gap analysis for full evidence tables.

### 4.1 Picker crate — missing infrastructure

The crate (`crates/tui-file-picker/src/`) currently has: no right-click handling of any
kind, single selection only (`state.rs:569`), a cursor-only text editor
(`input.rs:587-631 edit_text()`), no type-to-select, no search, no bookmarks, no
maximize/restore. Needed:

- **Right-click context-menu layer** — tree rows, file rows, browse-pane background,
  and (per §10.1) the address bar. Menu content per spec §9/§10: New ▸ File/Folder with
  correct targeting (tree: the right-clicked folder; browse pane: the displayed
  directory), Cut/Copy/Paste (clipboard already exists), Rename, Delete, Duplicate,
  Selection submenu on background. Omit "Open in New Tab" (no tabs exist; §15.1).
  "Open/Edit with System Default": files only (§15.2) — see §4.4 note.
- **Multi-selection model** in the files pane only (not the tree), with Select All /
  Invert / Deselect All (menu + Ctrl+A/Esc per §11.3), the §11.4 right-click rules
  (selected item → bulk across selection; unselected item → collapse selection to it),
  and single-item actions (Rename, Open with) suppressed under multi-select.
- **Real text editing** for address bar and name editors — selection, clipboard,
  Ctrl+Shift+←/→, select-all-on-open where Browse does it. The natural move is sharing
  `TextInputState` (see §5), replacing `edit_text()` and the plain String name buffer.
- **Inline naming** replacing the popup (defect #1), for both create and rename, with
  the existing validation/collision handling (`state.rs:1654-1674`) retained.
- **Delayed-click rename** matching Browse's 500ms pattern (double-click infrastructure
  already exists: `input.rs:373-395`, `state.rs:651`).
- **Type-to-select** matching the Browse baseline behavior (§2 above) plus directory
  priority, guarded off while any text editor has focus.
- **Recursive filename search** (filesystem-only per decision #3): invocation, query
  editing, result list navigation, activation, Esc cancel/clear. Async so the UI stays
  responsive on large trees; Browse's worker pattern (`browse.rs:5707-5941`) is
  reference material, minus tag modes.
- **Bookmarks**: add/remove/rename/navigate with persistence. Strongly prefer reusing
  the app's bookmark store (`~/.config/tonepoet/bookmarks.toml` + SQLite sync) so both
  surfaces see one bookmark set — if the crate can't own that dependency, expose a host
  hook and let tonepoet supply the store. Reordering is not currently supported in
  Browse and is not required (spec conditions it on user-controlled ordering).
- **Maximize/restore** for the picker overlay: state in the picker or its host,
  disclosure control in the title bar (matching the app's ▾/▸ pane-title idiom,
  `draw_browse.rs:363-372`), full-terminal maximize, restore to
  `file_picker_overlay_area()` dimensions (`src/tui/draw_overlays.rs:5243-5262`). No
  minimize. Title bar styling should match the app panes' solid bars.

### 4.2 Browse view — missing pieces

- **Filesystem clipboard**: adopt the picker's cut/copy/paste model (generalize to
  Vec<PathBuf> for multi-select). Add Cut/Copy to entry menus and Paste to the
  background menu (`context_menu.rs:770-786`) enabled when the clipboard has content.
  Today Browse only has "Copy to…/Move to…" pickers (`context_menu.rs:371-381`) — keep
  those; they are complementary.
- **Duplicate** action: Browse lacks it entirely. The picker already has it
  (toolbar-only, files-only, name-prompt flow —
  `crates/tui-file-picker/src/state.rs:1584-1650`, `input.rs:483-485`,
  `render.rs:220`); it still needs right-click exposure there. Add Duplicate to Browse
  (entry menu + bulk per §11.4); whether directories are duplicable is the
  implementer's call — the picker currently restricts to files
  (`WrongSelectionMode("Duplicate supports files only")`) — keep the two surfaces
  consistent whichever way.
- **Keyboard selection**: Browse ALREADY has select-all on **Alt+A**
  (`keybindings.rs:3778`) and Esc already clears multi-selection as part of the Esc
  cascade (`keybindings.rs:3807-3835`, clears at 3823; cascade order: options menu →
  archive listing → type-ahead → search → range mode → metadata focus →
  multi-selection → filter → archive). Remaining work: add Ctrl+A per spec §11.3
  (keeping Alt+A), with text-focus precedence (§11.5) — Ctrl+A must not reach the
  browse pane while `path_input`, search input, or an inline editor is active (this
  interacts with the Ctrl+A=select-all decision in §3.3: text editors consume it when
  focused). Give the picker equivalent bindings.
- **§11.4 right-click/selection rules**: partially present. Bulk file operations
  already act on the multi-selection when it is non-empty —
  `action_selection_in_current_directory` (`src/tui/command.rs:8954`) feeds
  CopyTo/MoveTo via `collect_selection_for_file_ops_scoped`
  (`context_menu.rs:1566-1595`). Missing: the §11.4 selected-vs-unselected rules
  (right-click re-targets `selected_index` at `keybindings.rs:31100-31101`, but its
  interaction with `multi_selected` — bulk when clicked item is selected, collapse when
  not — is not implemented as a rule), suppression of single-item actions (Rename,
  Open with) under multi-select, and extending bulk semantics to the new
  Cut/Copy/Paste/Duplicate actions.
- **Tree-pane context menu**: Browse's tree pane currently has NO context menu
  (`keybindings.rs:6519-6526` never fires for `BrowseTreeNode`). Spec §9.2 requires
  right-click New ▸ File/Folder on tree folders (tree is directories-only, so the
  file-suppression rule is moot in Browse; the picker's tree may differ).
- **Tree-pane inline rename** (§13): absent in both surfaces today; rename operates
  only on `browse.selected_entry()`.
- **Tree double-click + discrete disclosure hit target** (§6, both surfaces): tree rows
  are single hit regions (`draw_browse.rs:391`; crate `render.rs:474-477`). Add a
  disclosure-glyph-specific click target and double-click expand/collapse semantics
  without breaking the existing single-click navigate/toggle behavior.
- **Path bar context menu** (§10.1, both surfaces): Cut/Copy/Paste over the text
  selection.
- **Dead-bookmark representation** (§8): the overlay renders missing targets
  identically to live ones (`bookmarks_overlay.rs:76-121`); mark them visually.
- **Type-ahead directory priority** (defect #2).

### 4.3 Shared-extraction candidates (do where cheap — decision #1)

Judged low-risk because they are leaf code with no app-state dependencies:

- `src/tui/text_input.rs` → into the crate (or a shared module) essentially verbatim;
  the app re-exports, the picker adopts. Precedent: `src/tui/display_width.rs:3` already
  re-exports from the crate. Add a path-segment word-boundary mode ('/' as boundary) for
  the path bar per §12 — current word ops are whitespace-based
  (`text_input.rs:294-356`).
- Type-to-select matcher (pure function over (name, is_dir) + query) with the
  dir-priority rule, used by both surfaces.
- Click-timing helper (double-click vs delayed-click discrimination, one 500ms constant).
- Name validation (currently duplicated: crate `state.rs:1654-1674` vs app
  `keybindings.rs:29261+`).
- Filesystem clipboard type (already in crate; Browse consumes it).

Heavier subsystems (search UI, bookmarks store/overlay, context-menu content) are NOT
required to be shared — parallel implementations with matching UX are acceptable, and
host hooks are a fine middle ground where the crate needs app-owned data (bookmark
store). Choose per subsystem and say what you chose and why.

### 4.4 Notes and clarifications

- **"Open/Edit with System Default"**: `src/tui/external_editor.rs:99-167` opens via
  $EDITOR/$VISUAL, which is arguably the right "system default" for a TUI. Exposing that
  through the context menus (files only) satisfies the spirit of §15.2; xdg-open is an
  alternative. Either is acceptable — state the choice. Currently this capability is not
  exposed in any context menu.
- **Text clipboard**: both surfaces use an internal `static Mutex<String>`
  (`text_input.rs:7-11`); OSC52 is write-only, used for "Copy Path"
  (`context_menu.rs:1171-1179`). Terminals cannot generally read the system clipboard
  without terminal-specific protocols; internal clipboard + OSC52-write is the accepted
  baseline. Paste-from-other-apps typically arrives as bracketed paste, which the event
  loop already receives as key events — verify inline editors and the path bar accept
  pasted text that way, and don't build a system-clipboard read path.
- **Platform conventions (§14)**: this codebase currently targets Linux terminals; no
  macOS Command-key mapping exists anywhere. Do not build a modifier-abstraction layer
  now; keep the 500ms double-click constant in one shared place so it can become
  configurable later.
- **Tabs**: none exist. Omit "Open in New Tab" everywhere (§15.1) — no disabled
  placeholder entries.

## 5. Constraints

- **No function-key bindings, anywhere.** Terminal multiplexers the user runs (e.g.
  byobu) intercept F-keys, so F1–F12 must never be load-bearing. This bans introducing
  any new F-key binding (no F2-rename, no F5-refresh conventions from desktop file
  managers) AND requires fixing the one existing violation: the picker's F5→refresh
  (`crates/tui-file-picker/src/input.rs:191`) must gain a non-F-key binding (e.g.
  Ctrl+R or `r`-family — implementer's choice, consistent across both surfaces); drop
  or keep F5 as a non-documented extra, but nothing may be reachable ONLY via an F-key.
  Browse currently exposes Refresh via the context menu only, so give both surfaces the
  same keyboard refresh while you're there.
- Rust edition 2021; ratatui 0.26 + crossterm 0.27. The crate must keep compiling
  standalone (it is a workspace member with no dependency on the app; do not invert
  that).
- Two-pass rendering rule in the app: draw with immutable state, then register mouse
  hit regions (`ButtonRenderMap` app-side, `FilePickerHitAction` crate-side).
- Subprocesses (if any) must use `Stdio::null()` stdin (project convention; see
  CLAUDE.md).
- File deletion is permanent in this app (no trash) with safety guards — the Delete and
  bulk-Delete paths must reuse the existing guarded deletion, not reimplement it.
- Do not regress any §2 baseline. Spec §16 requires regression tests for preserved
  behavior, not just new tests for new behavior.
- All tests must pass: `cargo test --workspace` (never plain `cargo test`), zero
  failures, full untruncated `test result:` lines.

## 6. Deliverable

Source bundle (`.tar.gz`) with changed/new files at repo-relative paths, plus a manifest
and a written summary of: per-subsystem sharing choice (extracted / parallel / host
hook), intentional surface differences (running list per spec §1 — at minimum: tag
search Browse-only; tabs absent; anything else you add). Flag anything you could not
verify without a compiler; the
applying side handles compile fixes and runs the gates.
