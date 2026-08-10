# tonepoet — World-class tabbed browsing in the Browse view AND the file picker — 2026-08-09

A large, long-promised feature: **browser-style tabs** — each tab an independent directory
context — in **two** surfaces: the TUI **Browse** screen (`src/tui/browse.rs`) and the
**tui-file-picker** crate (`crates/tui-file-picker/`).

This brief is **outcomes + guardrails**. The diagnosis and maps below are *evidence*, not a
prescription — **you are the arbiter of HOW**. If you disagree with any structural suggestion, do
what's correct; what binds you is the outcomes (§A), the invariants (§C), and the input rules
(§D). Design the mechanism yourself.

You may deliver in **phases** (e.g. one surface at a time, or engine-then-UI) as long as each
landed phase is independently compiling, gate-green, and non-regressing. Both surfaces must reach
the outcome by the end.

## Ground rules
- Base = `main`/`hardening` @ `2d854e9`, version **0.4.6 — do not bump.** No merge, no unrelated
  refactors.
- Gate: `cargo test --workspace --no-fail-fast` green **×2** (the applier runs it; you have no
  cargo — say so honestly).
- This is greenfield: no tab scaffolding exists (see §E). The metadata-editor tab strip
  (`active_tab: usize` + `Vec<PresentationTab>` + `Tab`/`BackTab` cycling +
  `content_tab_slots_*` renderer, `src/tui/draw_overlays.rs:4305-4505`) is a **working in-repo
  structural model to mirror** — but it is overlay-scoped and cannot be lifted directly.

---

# A. Outcomes (the contract)

**O1 — Browse view tabs.** The Browse screen supports multiple tabs, each an **independent
directory context**: its own `current_dir`, entry list, cursor/scroll, multi-selection, inline
search, filter, ad-hoc sort, type-ahead buffer, back/forward nav history, and in-archive state.
Switching tabs instantly shows that tab's context, untouched. **Ephemeral**: every launch opens a
single default tab at the usual home directory; **no tab state persists across restarts** (do NOT
build session-restore/durable-tab machinery).

**O2 — Background tabs stay live and correct.** A tab loading a large/slow directory in the
background must **keep loading into its own context** and show the completed result when you return
to it — never bleeding a stale/async result into the wrong tab, and never silently dropping a
background tab's load. (This is the load-bearing engineering outcome — see the INV-SCAN hazard in
§C and the evidence in §F.2. A "world-class" result lives or dies here.)

**O3 — File picker tabs.** The tui-file-picker gains the same tab model, **independent per picker
session** (each time the picker opens, it starts with one tab; picker tabs are unrelated to Browse
tabs). Tabs sit *below* the picker's 12 "purposes" (§F.4) so all of them — Select-File,
Select-Directory/Destination, Save-as, artwork, tag-transfer, etc. — inherit tabs transparently.
The result contract (`FilePickerAction` + `session_id`) and every selection mode
(Files / Directories / FilesOrDirectories / Save-as) are unchanged.

**O4 — UX parity across the two surfaces.** Same tab affordances, same key feel, same visual
language on both surfaces. This is **UX parity, not code identity** — per the recorded Option C
decision (§F.5) the two remain separate implementations; do NOT rebase one on the other.

**O5 — The four requested capabilities (all must-have).**
- **Open in New Tab** from the context menu (Alt+M / right-click a folder → opens it in a new
  background tab). This fulfills the placeholder the prior deliverable already reserved
  (`docs/file-picker-browse-view-interaction-specification.md` §10.2 / §15.1 — "Open in New Tab,
  only when tabbed browsing is implemented"). Shown only where tabs exist.
- **Reopen closed tab** — an in-session undo-close that restores the last-closed tab (its
  directory and, ideally, its view state) from an in-memory stack. *This is within-session only;
  it does NOT contradict the ephemeral rule (no cross-restart persistence needed).*
- **Duplicate tab** — clone the current tab (same directory) into a new adjacent tab.
- **Reorder tabs** — move a tab left/right in the strip.

**O6 — Keyboard + mouse coeval, byobu-safe (see §D for the hard rules and the keyspace).** Desired
key feel (browser muscle-memory where the terminal delivers it):
- `Ctrl+T` new tab · `Ctrl+W` close tab · `Alt+←` prev · `Alt+→` next · `Alt+1..9` jump to tab N.
- Every tab op ALSO has a **mouse form**: a clickable tab strip with `[+]` (new) and per-tab `[x]`
  (close), click-to-activate, middle-click-to-close, drag-to-reorder.
- Reorder's keyboard form and any other new chords are yours to choose **within** §D; keep them
  byobu-safe and give each a mouse coeval.
- `Ctrl+Tab`/`Ctrl+Shift+Tab`/`Ctrl+1..9` are **NOT reliably delivered** in this app's terminal
  mode (§D.2) — do not use them as a sole path. If you want them as muscle-memory aliases, they'd
  require negotiating keyboard-enhancement flags the app does not push today — treat as out of
  scope unless trivial.

**O7 — Visual.** A 1-line tab strip renders above the file list on both surfaces (§F.4/§F.6 give
the exact low-risk insertion points and confirm there's vertical room with no cursor/scroll math
changes). It must degrade gracefully at small terminal sizes and with many tabs (condense/scroll
the strip; bump min-size guards rather than corrupt layout). See **Appendix I** for an illustrative
target (non-binding — it fixes the *layout and the visible states*, not the glyphs).

**O8 — Tab UX behaviors (decide each deliberately; a world-class result NAMES these — do not
improvise or omit).** These are the product decisions that separate great tabs from a toy. Each is
*yours to decide* within the invariants — pick a behavior, make it consistent across both surfaces,
and state your choice + rationale. A model that leaves these implicit will produce something merely
good.
- **Last-tab-close.** Closing the only tab must NOT close the Browse screen or the picker (and must
  never make the picker return `Cancelled` as a side-effect). Decide: no-op, or collapse to a fresh
  single home tab.
- **Focus after close / after switch.** Which tab becomes active when you close the focused one
  (left neighbor / right / most-recently-used)? The newly-focused tab shows its own cursor+scroll
  untouched.
- **New-tab placement + start directory.** Where does a new tab land (adjacent to current vs end)
  and what dir does it start in? Bare `Ctrl+T` (empty/home), Open-in-New-Tab (target folder,
  background), Duplicate (current dir, adjacent), reopen-closed (restored dir/index) may each differ
  — state each.
- **Per-tab visual state.** Each tab shows its label + an **active** indicator, a **loading**
  indicator while its async scan is in flight (crucial — O2 makes background loading first-class, so
  the user must *see* which background tab is still working), and ideally a marker when a tab holds a
  pending multi-selection.
- **Tab label derivation + truncation.** Label = current dir basename (or the archive name when
  in-archive); middle-truncate long names; no attempt at cross-tab uniqueness beyond basename.
- **Reopen-closed semantics.** LIFO stack (depth ≥1, your call); restores directory and (ideally)
  view state; where the restored tab reopens (end vs original index) and whether it takes focus.
- **Reorder disambiguation.** A drag threshold distinguishes reorder-drag from click-activate;
  dropping onto/over a `[x]` cell does not close; keyboard reorder form is byobu-safe (§D) with the
  drag as its mouse coeval.

---

# B. What a "tab" holds vs. what stays shared (decomposition — evidence, decide the shape yourself)

The single biggest design task is deciding, field by field, what becomes **per-tab** vs stays
**shared**. Both surfaces have the same shape of problem. §F.1 and §F.3 give the full field-by-field
maps with `file:line` anchors; the summary:

**Per-tab (the navigation context):** current directory, entry lists (raw + filtered), cursor,
scroll, multi-selection + anchor + range/drag state, inline search state, filter text, ad-hoc
sort, type-ahead buffer, nav back/forward history, per-view async-scan slot + generation, and
in-archive browsing context. In the picker: also address-bar edit buffer, per-tab tree expansion,
`file_table_state`, free-space readout.

**Shared / app-global (must NOT become per-tab):**
- **Clipboards** — filesystem clipboard and tag clipboard should be **shared** so copy-in-tab-A /
  paste-in-tab-B works (today `filesystem_clipboard`/`tag_clipboard` live on `BrowseState`,
  `browse.rs:2483`/`2493` — hoist them). Paste destination resolves from the **focused tab** at
  action time.
- **Single-worker singletons** — the `tag_clipboard_copy_*` and `tag_transfer_*` guards
  (`browse.rs:2496-2520`) assert "at most one blocking worker per AppState." They MUST stay
  app-global singletons; moving them per-tab would spawn N concurrent workers (INV-SINGLETON).
- **Single overlays** — context menu, options menu, bookmarks dropdown are one-at-a-time overlays
  (`app.rs` / `browse.rs:2805`). They may stay global **but must bind to the tab they opened
  over** (INV-OVERLAY) so switching tabs mid-menu can't act on the wrong directory.
- **Transfer queue** — already snapshots `destination_dir` at enqueue (`app.rs:6202`), so in-flight
  jobs are tab-safe; just read the correct (focused) tab's `current_dir` at enqueue.
- **Config-derived defaults, theme, result contract** — shared.

**Genuinely-your-call design decisions (pick and justify):**
- The picker's `focus` field (`state.rs:1066`, 13 variants) **conflates navigation focus
  (Tree/Files/Address — per-tab) with modal focus (Menu/Save/DeleteConfirm — global).** It likely
  needs splitting. Browse has the analogous `navigation_pane` (per-tab) vs the global overlays.
- The **sidebar tree** (Browse `tree_*` `browse.rs:2780`; picker `tree_*` `state.rs:1021`): per-tab
  tree that mirrors each tab's `current_dir` (consistent, heavier) vs one shared tree the active
  tab drives (lighter). Either is acceptable if it's coherent and doesn't misroute async tree scans.
- The **probe/stats/classification caches** (Browse `browse.rs:2571-2737`) are keyed by absolute
  path+identity, so they're *safe to share* across tabs as an optimization — but the debounce/focus
  fields (`last_focus_*`, `probe_debounce`, `deferred_work`) are genuinely per-view. Decide the
  split; don't regress probing.
- Pane enable/collapse + column set (`browse.rs:2796-2804`) read like **global screen chrome**
  (stable across tabs); confirm and keep consistent.

---

# C. Invariants / guardrails (hard)

- **INV-SCAN (the crux).** Every async completion that today matches only on generation + a
  path-like field must be routed to the **owning tab** and must never bleed into or be dropped by
  another tab. Affected messages (all in `src/tui/message.rs`), and note the matching field is
  **not** a uniform `path`: `DirScanBatch`/`DirScanComplete` (:514/:522, `generation`+`path`),
  `BrowseTreeChildrenComplete` (:532, `generation`+`path`), `PathValidationComplete` (:506,
  `generation`+`origin_dir`+`input`), `SearchComplete` (:555, `generation`+`root`+ a full launch
  identity: query/mode/filters/sort/cap/`archive_path`), plus the bookmark async completions
  `BookmarkActivationResolved` (:293) / `BookmarkDetailLoaded` (:305), plus any per-tab
  probe/stats/classification completions you keep per-tab. Per-tab generation counters **collide**
  (each `BrowseState`/tab starts at 0), so generation alone cannot disambiguate tabs — a tab/view
  identity must **join** each message's existing identity (note `SearchComplete` already validates a
  launch identity a tab-id must be added to). Route end-to-end (message + scan handle + spawn fn +
  reducer-by-tab-lookup, *before* the existing `is_current_dir_scan` generation check at
  `browse.rs:7274`). The picker's scans (if async — see §F.3) and bookmark activation (which must
  navigate/act on the **focused** tab) need the same treatment. **Acceptance:** open a slow dir in
  tab A, switch to tab B, keep working; A finishes and shows correct contents when selected; B is
  never disturbed. (See §F.2 for the exact call sites.)
- **INV-SINGLETON.** The single-blocking-worker guards (`tag_clipboard_copy_*`, `tag_transfer_*`)
  stay app-global singletons. No N-workers.
- **INV-CLIPBOARD.** Filesystem + tag clipboards work across tabs; paste destination = the focused
  tab at action time (today hardcoded to `app.browse.current_dir`, `keybindings.rs:7990`).
- **INV-OVERLAY.** Context menu / options menu / bookmarks dropdown bind to the tab they opened
  over; a tab switch while one is open must not act on the wrong tab. Bookmarks selection navigates
  the **focused** tab (not tab 0). The recently-added Alt+M context menu and options-menu keyboard
  focus must keep working per-tab.
- **INV-ARCHIVE.** A Browse tab can be *inside an archive* (`archive: Option<ArchiveBrowseState>`,
  `browse.rs:2746`), which carries a staging/deferred-save lifecycle (`ArchiveBrowseState.staging`;
  `active_archive_staging()` `browse.rs:4352`; deferred repackage/cleanup on leaving the archive).
  **Closing such a tab, reopening it via reopen-closed, or exiting the app must run the same staging
  flush/cleanup that leaving the archive does today** — no orphaned staging dirs, no lost deferred
  saves, no double-repackage. Duplicating an in-archive tab must not alias one staging session
  across two tabs.
- **INV-NO-REGRESS.** With a single tab (the overwhelming common case) every existing Browse and
  picker behavior is byte-for-byte intact — navigation, selection, search, sort, archive browsing,
  transfer queue, tag transfer, all 12 picker purposes, the `FilePickerAction`+`session_id`
  result contract, and the recent Browse polish (async tree, Alt+M, options-menu keyboard focus).
  Full workspace suite green ×2.
- **INV-INPUT.** All of §D. Every tab operation has both a keyboard form and a mouse form; no
  capability is reachable only through a multiplexer-intercepted chord. **Middle-click-to-close
  must be scoped to tab-strip cell hit regions only** — it must not intercept middle-click
  primary-paste anywhere else.
- **INV-LAYOUT.** The tab strip degrades gracefully at small sizes and high tab counts; bump
  min-size guards (picker `render.rs:112`, currently width<48/height<10) rather than corrupt the
  list. `last_render_area` / `ButtonRenderMap` hit-testing stays correct (only the active tab draws;
  the map is rebuilt each frame — §F.4).

---

# D. Input rules (HARD) + keyspace evidence

## D.1 Hard rules (from CLAUDE.md, `feedback_no_fkey_bindings`, byobu practice, and code comments)
1. **No F-keys, ever** (byobu captures them).
2. **No plain-letter bindings in the Browse pane** — plain letters (and Shift+letter) are the
   type-ahead catch-all (`keybindings.rs:7015`); the user has repeatedly and emphatically rejected
   vi-style `j`/`k`/`l`/`g`. Actions go on **Alt+/Ctrl+ chords, arrows, Enter/Space, or mouse**.
3. **Avoid** `Ctrl+Space`/`Alt+Space` (NUL / IME / multiplexer), `Ctrl+Shift+letter`
   (indistinguishable from `Ctrl+letter` in legacy encoding — the app already swallows
   `Ctrl+Shift+V` at `keybindings.rs:31`).
4. **`Ctrl+M` == `Enter`** in legacy encoding — cannot bind distinctly.
5. **`Tab`/`BackTab` are already taken** for pane-focus cycling in Browse (`keybindings.rs:6639/6643`)
   and search-control cycling — they can NOT be the tab-switch key.
6. **Plain digits `1`–`5` are globally taken** for screen switching (`keybindings.rs:190-206`) — so
   plain-digit tab-jump is out; **`Alt+1..9` is free** and is the jump path.
7. Mouse and keyboard must be **coeval** — every tab op has both forms.

## D.2 Decisive terminal-capability finding
The app runs **crossterm in baseline mode** — there is no `PushKeyboardEnhancement` /
`KeyboardEnhancementFlags` anywhere in `src/` or `crates/`. Therefore under byobu/tmux:
`Ctrl+Tab`, `Ctrl+Shift+Tab`, and `Ctrl+1..9` are **not reliably delivered** (legacy encoding
collapses them). `Ctrl+PageUp/PageDown` is inconsistent. Do not depend on any of these.

## D.3 Confirmed-free, byobu-safe chords for tabs
`Ctrl+T` (new) and `Ctrl+W` (close) are unbound in Browse and byobu-safe (neither is a multiplexer
prefix). The Browse `Alt+`-taken set is only `Alt+m`, `Alt+p`, `Alt+r`, `Alt+a`, `Alt+i` — so
`Alt+←`, `Alt+→`, and `Alt+1..9` are free. (`Ctrl+W` is delete-word in the picker's text-input
editor — behavior covered by `text_input.rs:1761` (a test); a different surface/focus, so no Browse
conflict, but keep tab-close out of the picker's *text-editing* focus.) The full Browse and picker taken-keyspace tables are in §F.7 —
consult them before binding anything, and verify each new chord against them.

---

# E. What the prior deliverable dropped (so you know the history)
Tabbed browsing was **specified as a future capability and deferred, never built.** The earlier
reasoning-model deliverable carved out a placeholder: the interaction spec
(`docs/file-picker-browse-view-interaction-specification.md` §10.2, §15.1) says the context-menu
**"Open in New Tab"** action must be shown **only when tabbed browsing is implemented**, and the
gap-analysis (`docs/file-picker-browse-view-gap-analysis.md` §15.1) records the current absence as
*intentional pending this feature*. There is **no** `BrowseTab`/`PickerTab` type, no tab vector, no
"Open in New Tab" code, no keybindings — greenfield. Honor the spec's intent: adding tabs is what
"turns on" that reserved action.

---

# F. Context & evidence (maps — read before designing; all `file:line`)

## F.1 Browse state decomposition (Browse)
`BrowseState` is `src/tui/browse.rs:2455-2832`, and — critically — **much Browse-adjacent state
lives on `AppState`, not `BrowseState`**: `bookmarks` (`app.rs:11559`), `active_overlay` incl.
`ContextMenu` (`app.rs:5636`), `browse_context_action_paths` (set `keybindings.rs:10278`),
`file_transfers` (`app.rs:11335`, `QueuedFileTransfer` `app.rs:6195`), `browse_inline_edit`,
`browse_info_focus`, `pending_browse_*`, `last_browse_click`. So "tabify Browse" is *not* "make
`BrowseState` a `Vec`" — it's a field-by-field decision across BOTH structs.
- PER-TAB core: `current_dir` (2456), `parent_entry` (2461), `all_dirs`/`all_files` (2463/2465),
  `entries` (2474), `selected_index` (2475), `scroll_offset` (2476), `visible_height` (2477),
  `multi_selected` (2480), `multi_select_anchor` (2528), `selection_mode` (2531), `drag_state`
  (2534), `search` (2537, struct 1742), `path_input` (2540), `filter_*` (2548-2552), `show_hidden`
  (2553, seeded from config), `sort_by`/`sort_dir` (2558/2559), `default_sort_*` (2564/2565),
  `format_filter` (2568), `archive` (2746), `type_ahead_buffer`/`_last_keystroke` (2772/2774),
  `nav_history`/`nav_history_index` (2818/2819).
- PER-TAB async slot: `scan_pending` (2750), `scan_discovered_count` (2754), `scan_generation`
  (2760), `cursor_restore_target`/`_scroll_offset` (2764/2769), `pending_inline_rename_after_scan`
  (2827).
- SHARED (hoist off BrowseState): `filesystem_clipboard`/`_generation`/`_retry_plan`
  (2483/2488/2524), `tag_clipboard` + `tag_clipboard_copy_*` (2493-2508), `tag_transfer_*`
  (2513-2520) — the last two groups are the app-singleton workers.
- Probe/stats/classification caches (2571-2737): per-tab in behavior, safe-to-share by path key;
  `last_focus_*`/`probe_debounce`/`deferred_work` stay per-view.
- Wiring: `scan_tx` (2831) shared. Layout chrome `explore_enabled`/`info_enabled`/`*_collapsed`/
  `browse_maximized`/`columns` (2796-2804) read as global; `navigation_pane` (2777) per-tab.
- Sidebar tree `tree_*` (2780-2791): design decision (per-tab vs shared), see §B.

## F.2 Async-scan model + the exact hazard sites (Browse)
Nav: `enter_selected` (5015), `go_parent` (5036, sets `cursor_restore_target`), `navigate_to[_str]`
(5198/5220), `go_back`/`go_forward` (3336/3349 → `navigate_without_history` 3373), `push_nav_history`
(3362), `apply_view` (4824)/`apply_view_preserving_cursor` (4868), `refresh[_with_search]`
(3884/3892). Scan spawn: `begin_async_scan` (3933; bumps `scan_generation` at 3977; `spawn_dir_scan`
10073 streams `DirScanBatch`→`DirScanComplete`); tree `spawn_browse_tree_scan` (10023 →
`BrowseTreeChildrenComplete`). **Reducers** (`event_loop.rs`): `DirScanBatch`→
`apply_dir_scan_batch_if_current` (5235); `DirScanComplete`→ guarded by `is_current_dir_scan`
(5279; matcher `browse.rs:7274` = generation==∧current_dir==); `BrowseTreeChildrenComplete`→
`apply_tree_scan_complete` (5259; matcher 3853). **Messages carry only `generation`+`path`, no tab
id** (`message.rs:514/522/532/506/555`). This is INV-SCAN.

## F.3 Picker state decomposition (tui-file-picker)
Single struct `FilePickerState` at `crates/tui-file-picker/src/state.rs:1015`. Natural shape: a
`Vec<PickerTab>` + `active_tab: usize`, where each `PickerTab` owns `current_dir` (1016),
`history_back`/`_forward` (1017/1018), `address_editing`/`address_input` (1019/1020), `tree_*`
(1021-1024), `entries` (1025), `visible_path_indices` (1028), `file_cursor`/`file_scroll`/
`file_table_state` (1029-1031), `multi_selected`+lookup (1043-1049), `range_anchor`/`visual_range`
(1053/1056), `type_ahead` (1097), `search` (1098), `free_space_bytes` (1101), selection-disclosure
counters (1059/1063). `FilePickerState` keeps the GLOBAL fields (theme 1065, `selection_mode` 1068,
`show_preview` 1070, `clipboard` 1077, `paste_task` 1081, all menu/submenu/context-menu/delete/
create/save modal state, `hit_regions` 1089, `last_layout` 1091, click timers 1092-1096, bookmarks
1099, `operation_policy` 1102, `save_*` 1109-1111, `title_case` 1112). The **`focus` field (1066)
needs splitting** (nav vs modal — see §B). Nav primitives: `navigate_to_dir[_with_history]`
(2329/2333), `go_back`/`go_forward` (2368/2381), `go_parent` (2394), `commit_address` (2401),
`refresh` (2179). Result: `FilePickerAction` (113) = None/Selected/SelectedMany/OpenSystemDefault/
Cancelled; `accept_current_selection` (2645), `open_or_select_current` (2629). Handlers:
`handle_key` (`input.rs:107`, dispatch by `focus`), `handle_mouse` (`input.rs:282`), render
`render`/`render_with_image_context` (`render.rs:52/101`), host overlay `draw_file_picker_overlay`
(`draw_overlays.rs:5563`). The picker's async scanning (if any) needs the same INV-SCAN tab-id
routing as Browse.

## F.4 Picker purposes (tabs sit below all of them)
`FilePickerPurpose` (`app.rs:6286`) → 12 entry points (SelectArtwork, SelectFile, SelectDirectory,
SelectDestination, SelectPreset, SavePreset, CopyTo, MoveTo, BrowseTagTransfer, MetadataTagBlocksFile,
MetadataTagTransfer, Generic), collapsing to 3 shapes: single-file pick, directory pick, save-as. All
use one `FilePickerState`, wrapped as `MetadataFilePickerState { session_id, purpose, picker }`
(`app.rs:6346`); host matches `FilePickerAction` at `keybindings.rs:13781-13795` with `session_id`
guarding against stale closes (`app.rs:6636`). Tabs are a navigation-context multiplier below this
layer — the contract is untouched.

## F.5 Two implementations (Option C) — parity, not shared code
Browse and the picker are **fully separate** (gap-analysis `docs/file-picker-browse-view-gap-analysis.md`
§0a, verbatim): *Browse stays its own implementation; extract only small leaf engines; parity = UX
parity, not code identity.* Shared today: tree node model (`BrowseTreeNode = tui_file_picker::TreeNode`,
`browse.rs:2141`), `display_width`, file-task progress/conflict machinery. Everything else (search,
bookmarks, context menus, multi-select, text input) is duplicated/one-sided. **Consequence: tabs must
be implemented twice.** Companion docs in-repo: `file-picker-browse-view-interaction-specification.md`,
`file-picker-browse-view-parity-brief.md`, `file-picker-browse-parity-handoff-readme.md`.

## F.6 Layout / strip insertion (both surfaces)
- Browse: `draw_browse_screen` (`draw_browse.rs:162`) sets `last_render_area` (163) then a vertical
  layout (165-173): `[0]` header `Length(7)`, `[1]` toolbar `Length(5)`, `[2]` content `Min(10)`,
  `[3]` footer `Length(2)` (the 5-**screen** tab bar — do NOT overload it). Insert a directory-tab
  strip as a new `Constraint::Length(1)` between `[1]` and `[2]`; content is `Min(10)` so it yields
  a row. Buttons: `ButtonRenderMap` (`button_map.rs:657`) is rebuilt every frame; add e.g.
  `BrowseTab(usize)`/`BrowseTabClose(usize)` variants and register strip cells like
  `register_browse_buttons` (`draw_browse.rs:1934`). The screen tab bar already uses `TuiButton::Tab(u8)`
  (`button_map.rs:28`) for screens — directory tabs are orthogonal; name them distinctly.
- Picker: `render` (`render.rs:101`); row 0 of `outer_inner` is already a 1-line title bar
  (`render.rs:128`); insert a `Constraint::Length(1)` tab strip below it in all three layout branches
  (save-inline 142, conflict-policy 162, default 180). File-list Rect is derived (`render_split_pane`
  371) and visible rows recomputed each frame (`set_file_visible_rows` 759) → **no cursor/scroll math
  changes.** New hit regions via `FilePickerHitAction` (`state.rs:245`) like the title's
  `record_hit_region` (`render.rs:132`). Bump the min-size guard (`render.rs:112`).

## F.7 Taken keyspace (verify before binding)
- Browse global pre-layer (`handle_key` 21-302): `Ctrl+Q` quit, `Esc` cancel inline write, plain
  `1-5` screen switch (190-206), `:` command, `?` help, `Ctrl+Shift+V` swallowed (31).
- Browse-global block (`handle_browse_key` 6561-6636): `Alt+m` context menu, `Ctrl+b` bookmarks,
  `Ctrl+z`/`Ctrl+y` undo/redo, `Ctrl+/`,`Ctrl+_` clear-sel, `Ctrl+c`/`x`/`v`/`p` fs-clipboard,
  `/`,`Ctrl+f` search, `.` hidden, `Ctrl+r` refresh, `Tab`/`BackTab` pane focus.
- Browse list (6676-7021): Shift+nav range-preview; plain nav keys; `Left`/`Right` dir up/enter;
  `Backspace`; `Delete`; `Enter`; `Space`; `Shift+Space`; `Alt+r`/`Alt+a`/`Ctrl+a`/`Alt+i`/`*`;
  `Esc` escalating cancel; `Ctrl+e`/`Alt+p` editor; **`Char` catch-all → type-ahead (7015)**.
- Picker (`input.rs`): `Esc`,`Tab` focus, `Left/Right` (+Alt=history), nav keys, `Backspace`,
  `Enter`/`Alt+Enter`, `Space`, `v` visual, `Ctrl+A`, `Ctrl+Space` mark, `Ctrl+L` address, `/`/`Ctrl+F`
  search, `Ctrl+R` refresh, `Ctrl+C/X/V/P`, `Ctrl+Shift+V` host-paste, `Delete`, `Alt+O` menu, chars →
  type-ahead. Editor `text_input.rs` uses `Ctrl+W` (delete-word), `Ctrl+A/E/U/K`, etc.
- **Free for tabs** (both): `Ctrl+T`, `Ctrl+W` (Browse), `Alt+←`, `Alt+→`, `Alt+1..9`. Verify each
  against the tables above in the surface you're binding.

---

# G. Deliverables
- Patch or changed files (both surfaces); a short WHY per major decision (especially the tab-id
  routing scheme, the state decomposition, and the `focus` split); the tests below; an honest note
  that you can't run cargo (the applier gates ×2). If you reject any diagnosis, say so and do what's
  correct — the outcomes bind, not the mechanism.
- **Tests (behavior, not implementation):** (a) INV-SCAN — a background tab's dir scan completes into
  its own context and the active tab is undisturbed (drive via the message reducers with two tabs at
  colliding generations); (b) open/close/switch/duplicate/reorder/reopen-closed each mutate the tab
  set correctly and preserve per-tab context, incl. last-tab-close and focus-after-close (O8); (c)
  copy in tab A → paste in tab B lands in B's dir; (c2) a transfer enqueued from tab B targets B's
  dir and a later switch to A does not retarget the in-flight job; (d) a single-tab session is
  behaviorally identical to today (regression) on both surfaces; (e) "Open in New Tab" appears only
  with tabs and opens the folder in a background tab; (f) picker: all selection modes + the
  `FilePickerAction`+`session_id` contract still hold with tabs; (g) input: the byobu-safe chords
  work and no plain-letter/type-ahead regression, and middle-click-close is scoped to strip cells;
  (h) INV-ARCHIVE — closing a tab that is inside an archive with pending staged edits flushes/cleans
  staging exactly as leaving the archive does (no orphaned staging, no lost deferred save); (i)
  bookmark activation navigates the focused tab, not tab 0. State explicitly whether the picker's
  scan path is sync or async: if async, mirror test (a) for the picker; if sync, note that tab-safety
  is trivially satisfied and why.

# H. Bundle manifest
- This brief. Complete compiling `main`@`2d854e9` tree:
  - Browse: `src/tui/browse.rs`, `src/tui/draw_browse.rs`, `src/tui/keybindings.rs`,
    `src/tui/app.rs`, `src/tui/message.rs`, `src/tui/event_loop.rs`, `src/tui/button_map.rs`,
    `src/tui/context_menu.rs`, `src/tui/bookmarks.rs`, `src/tui/draw_footer.rs`,
    `src/tui/draw_overlays.rs` (metadata-editor tab-strip reference).
  - Picker: the entire `crates/tui-file-picker/` crate.
  - Reference docs: `docs/file-picker-browse-view-gap-analysis.md`,
    `docs/file-picker-browse-view-interaction-specification.md`,
    `docs/file-picker-browse-view-parity-brief.md`,
    `docs/file-picker-browse-parity-handoff-readme.md`.
  - Full `src/` + `crates/` + `tonepoet-pipeline/` + root `Cargo.toml` + `flake.nix` + `CLAUDE.md`
    so it compiles. NOT `target/`. If anything's missing, say so rather than guessing.

---

# Appendix I — Illustrative target (NON-BINDING)

This mockup fixes **the layout and the visible states**, not the glyphs. The commitment: a
**single-line tab row** sitting between the path bar and the file list, **one clickable cell per
open directory**, with the active tab, in-flight background loads, and pending selections all
visibly distinguishable. The exact characters (a `▐` bar vs. a highlight/underline for "active", a
`◐` vs. `⋯` for "loading", etc.) are **yours** — pick what renders crisply in the Tokyo Night theme
and reads clearly under byobu.

## Browse screen — tab strip between the path bar and the three-pane content
```
┌────────────────────────────────────────────────────────────────────────────────────────┐
│   T O N E P O E T                                  tonepoet 0.4.6                         │  header  (Length 7)
├────────────────────────────────────────────────────────────────────────────────────────┤
│  ⌂  /home/daedalus/torrents                        [.] hidden   [f] flac   ⇅ name ▲       │  toolbar/path (Length 5)
├────────────────────────────────────────────────────────────────────────────────────────┤
│▐ torrents ✕│ ◐ FLAC rips │  Wish You Were Here •✕ │                                 [ + ] │  ◄── NEW tab strip (Length 1)
├──────────────────┬───────────────────────────────────────────────┬───────────────────────┤
│ ▾ torrents       │  Name                       Size    Kind       │  Wish You Were Here    │
│   ▸ Pink Floyd   │  ▸ Pink Floyd - Animals     —       folder      │  Pink Floyd · 1975     │  content (Min 10):
│   ▸ Mazzy Star   │ ▸▐ Pink Floyd - WYWH        —       folder   ◀  │  FLAC · 24/96 · LP     │  explore │ list │ info
│   ▸ Wilco        │  ▸ Mazzy Star - SDSF        —       folder      │  5 tracks · 43:12      │
│                  │    01 Shine On.flac         184 MB  audio       │  A1  Shine On You…     │
├──────────────────┴───────────────────────────────────────────────┴───────────────────────┤
│ [1]Browse  [2]Library  [3]Convert  [4]Queue  [5]Config                                     │  footer (Length 2):
│ ↑↓ move   → open   ^T new tab   ^W close   ⌥←→ switch   ⌥1-9 jump   Space mark   ⌥M menu   │  5-screen tabs + keys
└────────────────────────────────────────────────────────────────────────────────────────┘
```

## Anatomy of the tab strip (the point of the feature)
```
 ▐ torrents ✕ │ ◐ FLAC rips │   Wish You Were Here •✕ │                         [ + ]
 └────┬─────┘   └────┬────┘     └──────────┬────────┘                          └─┬─┘
   ACTIVE tab      BACKGROUND tab           inactive tab                       new tab
   (its dir is     still LOADING            with a pending                   (click, or ^T)
   shown below)    (◐ — you SEE which       multi-selection
                   tab is working even      (• dirty marker)     ✕ = close (click / middle-click / ^W)
                   off-screen — O2)
```
Markers: `▐`/highlight = active · `◐` = async scan in flight (O2/O8) · `•` = pending selection ·
`✕` = close · `[ + ]` = new tab.

## File picker overlay — same language, independent tabs per picker session
```
        ┌─ ▸ Select destination folder ─────────────────────────────────────────┐
        │▐ ~/library ✕│ ◐ floyd │  staging •✕ │                            [ + ] │  ◄── tab strip (below title)
        │ [New ▾]  [Copy]  [Move]          ⌂ /home/daedalus/library           ⌕   │  toolbar + address
        │ ┌─ Tree ─────┐┌─ Files ───────────────────────┐┌─ Preview ─────────┐  │
        │ │ ▾ library  ││ ▸▐ floyd            —    folder ││  floyd/           │  │  panes (Min 3):
        │ │  ▸ jazz    ││  ▸ jazz             —    folder ││  12 items         │  │  tree │ files │ preview
        │ │  ▸ misc    ││  ▸ misc             —    folder ││                   │  │
        │ └────────────┘└───────────────────────────────┘└───────────────────┘  │
        │ 428 GB free           ^T new   ^W close   ⌥←→ switch      Enter select  │  status
        └───────────────────────────────────────────────────────────────────────┘
```

## "Open in New Tab" — the reserved hook (§E) becomes real, only with tabs present
```
   ▸ Pink Floyd - WYWH ┈┈┈┐
     Open                │   (Enter)
     Open in New Tab  ◄──┤   ← appears only now that tabbed browsing exists
     Copy                │   (^C)
     Cut                 │   (^X)
     Rename              │
   ┈┈┈┈┈┈┈┈┈┈┈┈┈┈┈┈┈┈┈┈┈┘
```

## Overflow — many tabs condense/scroll, never corrupt the row (O7)
```
│ ‹ │▐ torrents ✕│ ◐ FLAC… │ WYWH •✕ │ jazz │ misc │ dl… │ › +4  [ + ] │
  └┬┘                                                        └┬┘
 scroll left                                        hidden-tab count
```
