# Gap Analysis — File Picker / Browse View Interaction Specification

Companion to `file-picker-browse-view-interaction-specification.md`. Analyzed 2026-07-24
against the `working` branch. Line numbers are approximate anchors, not contracts.

## 0a. Decisions (user, 2026-07-24) — these govern the implementation brief

1. **Consolidation strategy: Option C.** Browse stays its own implementation; do NOT
   rebase Browse on the picker crate. Extract only small leaf engines into shared code
   (text input, type-to-select matcher, click timing, name validator, filesystem
   clipboard). Heavy app-entangled subsystems (Browse search, bookmarks, context-menu
   content) stay app-side.
2. **Parity means UX parity, not code identity.** The spec's "matching algorithm,
   timeout, and cycling behavior must be identical" language was agent-added, not a user
   requirement. Equivalent user-facing behavior is the bar; shared code is a means where
   cheap, not an end.
3. **Picker search is filesystem-only.** The file picker needs recursive *filename*
   search over the filesystem — no tag-aware/tag-database search. Tag search remains a
   Browse-only capability (documented intentional difference per spec §1/§7).
4. **Ctrl+A becomes select-all** in the shared text-editing engine (path bar and inline
   editors), replacing the deliberate readline-Home binding in `text_input.rs`.
5. **No function-key bindings anywhere** (byobu/multiplexers intercept F-keys). Audit
   result: the app (`src/tui/`) has zero F-key bindings; the picker crate has exactly
   one — F5→refresh (`input.rs:191`), with no non-F-key alternative. That binding must
   gain a non-F-key equivalent; no new F-key bindings may be introduced.

## 0. Executive summary

The two surfaces are far apart, and the gaps are asymmetric:

- **The Browse view is the strong surface** for search, bookmarks, context menus,
  multi-selection, inline naming/rename, delayed-click rename, and text editing.
  All of that is app-side code the picker crate cannot see.
- **The file-picker crate is the strong surface for exactly one thing**: it has a real
  filesystem clipboard (cut/copy/paste of files with modes and validation) — which the
  Browse view lacks entirely.
- **Almost nothing behavioral is shared.** The crate currently exports tree helpers
  (`TreeNode`, `child_directories`, `expand_tree_to_path`, `refresh_tree_children`),
  `display_width`, theme, and the file-task progress/conflict machinery. Every
  interaction behavior in scope (type-to-select, context menus, selection, inline edit,
  search, bookmarks, text editing) is either app-only, crate-only, or duplicated.
- **Two hard spec violations exist today**: the picker names new files via a modal popup
  (§9.1 forbids), and Browse type-to-select does not prioritize directories (§5).
- **Cross-cutting missing infrastructure** (needed by many requirements): right-click
  support in the picker crate (it has none at all), a filesystem clipboard in Browse,
  multi-selection in the picker, a shared text-input widget with selection, and
  maximize/restore state for the picker overlay.

## 1. Architecture snapshot (what is shared today)

| Layer | Shared? | Evidence |
|---|---|---|
| Tree node model + directory scanning | ✓ shared | `browse.rs:2141` (`BrowseTreeNode = tui_file_picker::TreeNode`), `browse.rs:3082,10851-10870` |
| `display_width` | ✓ shared | `src/tui/display_width.rs:3` re-exports from crate |
| File-task progress / conflict resolution | ✓ shared | `app.rs:5910-5988`, `message.rs:473-477` |
| Text input widget | ✗ duplicated | app: `src/tui/text_input.rs` (selection, clipboard, word-jump); crate: `input.rs:587-631 edit_text()` (cursor-only) |
| Context menus | ✗ app-only | `src/tui/context_menu.rs` (~3K lines); crate has zero right-click handling |
| Search | ✗ app-only | `browse.rs:1698+` SearchState; crate has only extension filtering (`filter.rs`) |
| Bookmarks | ✗ app-only | `src/tui/bookmarks.rs`, `bookmarks_overlay.rs`; crate: nothing |
| Filesystem clipboard | ✗ crate-only | `state.rs:72-82,582`, `input.rs:539-551`; Browse: nothing |
| Multi-selection | ✗ app-only | `browse.rs:2170 multi_selected`; crate: single `selected: Option<PathBuf>` |
| Inline naming / rename | ✗ app-only (inline); crate uses modal | `draw_browse.rs:1762-1787`, `inline_edit.rs`; crate `render.rs:971-994` popup |

## 2. Per-section findings

### §4 Title bar / maximize–restore

| Requirement | Picker crate | Browse view | Gap |
|---|---|---|---|
| Solid title bar consistent with app panes | Partial — plain ratatui `Block` border+title (`render.rs:117-122`) | ✓ custom solid bar with ▾/▸ glyph (`draw_browse.rs:363-372, 1639-1664`) | Picker bar doesn't match app pane style |
| Dynamic dimensions from terminal | ✓ host computes 90%/86% (`draw_overlays.rs:5243-5262`) | ✓ | — |
| No minimize | ✓ compliant | ✓ | — |
| Maximize to full terminal + restore | ✗ absent — no state field, no control | ✓ for explore/info panes (`browse.rs:2413+`, `keybindings.rs:31970`; Browse pane title double-click) | Picker overlay needs maximize state + `file_picker_overlay_area()` change |
| Click disclosure toggles maximize/restore | ✗ absent | Partial (pane collapse toggle exists; semantics are collapse, not maximize) | New hit region + state in picker; reconcile "collapse" vs "maximize" semantics |

### §5 Type-to-select

| Requirement | Picker crate | Browse view | Gap |
|---|---|---|---|
| Incremental type-to-select in active pane | ✗ entirely absent (bare chars fall through, `input.rs:158,238`) | ✓ `type_ahead_push/pop` (`browse.rs:4227-4278`), 1500ms timeout (`browse.rs:12`), case-insensitive, prefix-then-substring | Implement in crate; ideally extract Browse's engine into shared code |
| Directories matched before files | n/a | ✗ **spec violation** — `browse.rs:4240-4248` scans entries uniformly, no `is_dir()` priority | Fix in the (shared) matcher |
| Identical algorithm/timeout/cycling in both | ✗ | — | Only achievable via shared implementation; cycling behavior currently undefined in Browse |
| Must not intercept while text editor focused | n/a | ✓ guarded (`keybindings.rs:3610-3623, 3857-3860`) | Preserve guard in shared version |

### §6 Tree expansion/collapse

| Requirement | Picker crate | Browse view | Gap |
|---|---|---|---|
| Mouse on disclosure control toggles | ✗ whole row is one hit region (`render.rs:474-477`) | ✗ whole row is one button (`draw_browse.rs:391`) | Neither has a discrete disclosure hit target; need e.g. `TreeDisclosure(index)` hit action in both |
| Double-click expanded dir collapses / collapsed expands | ✗ double-click special-cased only for file rows (`input.rs:373-394`) | ✗ no tree-row double-click logic (`keybindings.rs:31952-31966` is single-click toggle) | Missing in both |
| Cursor-key expand/collapse preserved | ✓ `tree_right/tree_left` (`state.rs:1373-1409`) | ✓ (`keybindings.rs:3655-3681`) | Preserve; independently implemented |
| Glyph reflects expansion state | ✓ ▾/▸ (`render.rs:456-460`) | ✓ ▾/▸ (`draw_browse.rs:430`) | — |

### §7 Search — **parity ~0%**

Browse: full subsystem — `/` invocation + toolbar button (`keybindings.rs:3797-3804`,
`draw_browse.rs:303`), `SearchState` with `TextInputState` query (`browse.rs:1698-1754`),
recursive async workers (`browse.rs:5707-5941`), modes Filename/Tags/Both, 7 sort keys,
multi-focus navigation (`SearchFocus`, `browse.rs:1617-1629`), Esc cancel with async task
cancellation (`browse.rs:5029-5043`), dedicated results view (`draw_browse.rs:1692+`).

Picker crate: **nothing** — only extension-based type filtering (`filter.rs:4-60`). No
query input, no results view, no recursion, no cancellation.

Gap: the entire search subsystem must become available to the picker, per spec §7 by
reusing the Browse implementation (likely extracting a shared search engine or having the
picker host embed the app search component). Building a second search system would
itself violate §7.

### §8 Bookmarks — **parity ~0%**

Browse: complete — overlay opened via `:bookmarks`/`:bm` command
(`command.rs:7374-7383`) or context menu (`ContextAction::OpenBookmarks`,
`context_menu.rs:1623-1624`); NO Ctrl+B binding exists (earlier report was wrong).
In-overlay keys via `handle_bookmarks_overlay_key` (`keybindings.rs:29479+`: delete
29535, rename 29541, navigate 29545; `bookmarks.rs:136-298`), dual persistence
(`~/.config/tonepoet/bookmarks.toml` + SQLite sync, `bookmarks.rs:54-155`).
**Missing even in Browse: reordering** (no move up/down — spec §8 requires it only "if
ordering is user-controlled"; currently it is not, so this is a documented-decision item).
Missing/inaccessible-target representation: not verified — check how navigation to a
deleted bookmark path behaves.

Picker crate: **nothing** (grep for "bookmark" in crate returns zero hits).

### §9 Creating files and folders

| Requirement | Picker crate | Browse view | Gap |
|---|---|---|---|
| Toolbar **New ▸ File/Folder** | ✓ menu exists (Alt+O; `render.rs:689,747`) | ✓ | — |
| Naming is inline, never modal | ✗ **spec violation** — centered popup `render_create_name_popup` (`render.rs:190-191, 971-994`) | ✓ inline create line in list (`draw_browse.rs:1762-1787`) | Picker must migrate popup → inline row |
| Tree right-click folder → New (creates inside it) | ✗ no right-click at all | ⚠ unverified — tree-node context menu targeting needs checking | Implement (crate); verify + regression-cover (Browse) |
| No New submenu on tree-pane files | ✗ n/a | ⚠ unverified | Verify |
| Browse-pane background → New (creates in displayed dir) | ✗ no right-click | ✓ `build_browse_empty_menu` (`context_menu.rs:770-786`) | Crate gap |
| Right-click folder item in browse pane must NOT create inside it | ✗ n/a | ⚠ unverified — entry menu (`context_menu.rs:708`) targeting needs checking | Verify + test |
| Name validation / collision / error reporting | ✓ (`state.rs:1654-1674`) | ✓ (`keybindings.rs:29261-29318`) | Both correct but **duplicated** — extract shared validator |

### §10 Context menus

| Requirement | Picker crate | Browse view | Gap |
|---|---|---|---|
| Any right-click context menu | ✗ **none anywhere** — menu is keyboard-only (Alt+O) | ✓ (`keybindings.rs:6240, 6511-6561, 31084-31116`) | Picker needs a context-menu layer; ideally the shared one |
| §10.1 Path bar Cut/Copy/Paste menu | ✗ | ✗ not found | Missing in **both** |
| §10.2 file/folder menu: Cut, Copy | ✓ (menu, not right-click) | ✗ — only "Copy to…/Move to…" pickers; no clipboard staging (`context_menu.rs:371-381`) | Browse needs Cut/Copy backed by a filesystem clipboard |
| §10.2 Rename | ✓ toolbar button only — NOT in Alt+O menu, no F2 binding (`state.rs:1567-1582`, `input.rs:479`) | ✓ files AND directories (`file_ops_submenu`; see §5 item 3) | Picker: expose via context menu when it exists |
| §10.2 Delete | ✓ | ✓ (`DeletePermanently`) | — |
| §10.2 Duplicate | ✓ toolbar, files-only, name-prompt popup (`state.rs:1584-1650`, `render.rs:220`) | ✗ absent | Browse gap; picker needs right-click exposure |
| §10.2 Open in New Tab | correctly omitted (no tabs) | correctly omitted | Compliant via §15.1 — document |
| §10.2 Open/Edit with System Default (files only) | ✗ | Partial — View/Edit for text files (`context_menu.rs:747-749`); `external_editor.rs:99-167` exists but not exposed as general "system default" action | Wire up for all files; files-only guard |
| Actions apply to right-click target, not stale cursor | n/a | ✓ right-click re-targets before menu (`keybindings.rs:31100-31102`) | Preserve; replicate in picker |
| §10.3 background menu: New / Selection / Paste | ✗ | Partial — New ✓, Selection ✓, **Paste ✗** (no clipboard) | Paste blocked on Browse filesystem clipboard |

### §11 Selection and bulk operations

| Requirement | Picker crate | Browse view | Gap |
|---|---|---|---|
| Multi-selection in browse pane | ✗ single selection only (`state.rs:569`) | ✓ `multi_selected: Vec<PathBuf>` (`browse.rs:2170`), non-recursive, ParentDir excluded (`context_menu.rs:1085-1090`) | Crate needs a selection model |
| No multi-selection in tree pane | ✓ (vacuously) | ✓ | — |
| Selection submenu (Select All / Invert / Deselect) | ✗ | ✓ in entry + empty menus (`context_menu.rs:615-618, 776-778`) | Crate gap; Browse is the parity baseline (spec §11.2 says verify + regression-cover, don't reimplement) |
| Ctrl+A select-all / Esc deselect in browse pane | ✗ | Partial — select-all exists on **Alt+A** (`keybindings.rs:3778`); Esc clears multi-selection within the Esc cascade (`keybindings.rs:3807-3835`, at 3823) | Add Ctrl+A per spec (keep Alt+A); picker needs both |
| §11.4 right-click on selected item → bulk; on unselected → collapse selection | ✗ n/a | Partial — bulk file ops already use `multi_selected` when non-empty (`command.rs:8954` `action_selection_in_current_directory`; CopyTo/MoveTo via `context_menu.rs:1566-1595`); the selected-vs-unselected right-click rule and single-item suppression are not implemented | Implement the rule; extend bulk to Cut/Copy/Paste/Duplicate |
| Single-item actions disabled under multi-select | ✗ n/a | ✗ not implemented | Missing |
| §11.5 text-focus precedence for Ctrl+A etc. | n/a | Partially moot (no browse Ctrl+A yet); guards exist for type-ahead | Must be designed in when Ctrl+A lands |

### §12 Path bar text editing

Browse path bar: `path_input: Option<TextInputState>` (`browse.rs:2186, 4961-4993`),
opened fully-selected via `TextInputState::new_selected`. The shared widget
(`text_input.rs`) has a real selection model (anchor, ranges), internal clipboard,
`Ctrl+Shift+Left/Right` word extension, Ctrl+X/C/V.

Picker address bar: minimal `edit_text()` (`input.rs:587-631`) — cursor only. No
selection, no clipboard, no word/segment ops, no double-click select, no selection
rendering (`render.rs:270-306`).

| Requirement | Picker | Browse | Note |
|---|---|---|---|
| Ctrl+A selects entire path | ✗ | ✗ **deliberate deviation** — `text_input.rs:~580` binds Ctrl+A to readline Home, with a code comment explaining the choice | Spec conflict to resolve explicitly (see §5 open questions) |
| Double-click selects path | ✗ | ⚠ open-time select-all exists (`new_selected`); double-click-on-bar not verified | Verify |
| Ctrl+Shift+←/→ segment selection | ✗ | Partial — word-based (whitespace), not path-segment (`/`) boundaries (`text_input.rs:294-356`) | Boundary semantics need path-segment mode |
| Ctrl+X/C/V | ✗ | ✓ (internal clipboard) | — |
| Clipboard backing | none | internal `static Mutex<String>` (`text_input.rs:7-11`); OSC52 used only for "Copy Path" (`context_menu.rs:1171-1179`) | No system-clipboard read; paste from other apps impossible — spec expects "copied from elsewhere" to work |

### §13 Inline naming and renaming

| Requirement | Picker crate | Browse view | Gap |
|---|---|---|---|
| Inline editing in tree AND browse panes | ✗ modal popup for create; rename similar | ✓ browse pane inline (`draw_browse.rs:1781`, `inline_edit.rs`); ⚠ tree-pane inline rename not verified | Crate: modal→inline; Browse: verify tree-pane coverage |
| §13.1 delayed-click rename | ✗ no timing infrastructure for rename (double-click tracking exists: `input.rs:373-395`, 500ms `state.rs:651`) | ✓ implemented — 500ms window, second-click-after-window renames (`keybindings.rs:31835-31860`, `app.rs:10364-10370`, `event_loop.rs:609-635`) | Port Browse pattern into crate |
| §13.2 editor keys (Ctrl+A, Ctrl+Shift+←/→, X/C/V) | ✗ plain String buffer | ✓ via `TextInputState` (`text_input.rs:549-653`), selection rendered inverse-video (`inline_edit.rs:46`) | Crate should adopt shared `TextInputState` |
| Enter commits w/ validation; Esc cancels | ✓ (`input.rs:332-334`, `state.rs:1654-1674`) | ✓ (`keybindings.rs:29261-29318`) | Validation logic duplicated — consolidate |
| Click-outside commit/cancel policy | ✗ undefined for popup | Implicit (inline row; outside click selects other entry) | Define policy once, in shared layer |

### §14 Platform conventions

- **No macOS Command-key handling anywhere.** Both surfaces hardcode
  `KeyModifiers::CONTROL`; zero `cfg(target_os = "macos")` in key dispatch.
- **Double-click window**: hardcoded 500ms in both (crate `state.rs:651`, Browse
  keybindings) — consistent, but private/not configurable, not platform-derived.
- Terminal reality: crossterm delivers no native double-click events; all timing is
  hand-rolled — a shared click-timing helper is the natural consolidation point.

### §15 Conditional capabilities

- **Tabs**: not implemented in either surface. Both correctly omit "Open in New Tab"
  (compliant — just needs documenting as an intentional absence).
- **System default editor**: `external_editor.rs:99-167` implements EDITOR/VISUAL-based
  open/view but is not exposed in any context menu, and has no files-only guard at the
  menu layer. Note: spec means the *platform* default (xdg-open/`open`) — current code is
  $EDITOR-based; decide which satisfies "system default" in a TUI context.

## 3. Hard spec violations (existing behavior contradicting the spec)

1. **Picker modal naming popup** (`render.rs:971-994`) — §9.1 forbids modal naming.
2. **Browse type-to-select ignores directory priority** (`browse.rs:4240-4248`) — §5.
3. **Ctrl+A = readline Home in shared text widget** (`text_input.rs`, deliberate) — §12/§13.2
   require select-all. Needs an explicit product decision (spec change or code change).

## 4. Missing cross-cutting infrastructure (blocks many requirements)

| Infrastructure | Blocks | Exists today? |
|---|---|---|
| Right-click/context-menu layer in picker crate | §9.2, §9.3, §10.*, §11.2, §11.4 | No (crate has zero right-click) |
| Filesystem clipboard in Browse | §10.2 Cut/Copy, §10.3 Paste, §11.4 | Only in crate (`FilePickerClipboard`) — inverse gap |
| Multi-selection model in picker | §11.* | No |
| Shared text-input widget (selection+clipboard) used by picker | §12, §13.2, §10.1 | App-only (`text_input.rs`); crate has cursor-only `edit_text()` |
| Maximize/restore state for picker overlay | §4 | No |
| Type-to-select engine (dir-first) shared | §5 | Browse-only, non-compliant |
| Search subsystem reachable from picker | §7 | Browse-only |
| Bookmarks subsystem reachable from picker | §8 | Browse-only |
| System clipboard bridge (read side) | §10.1, §12 paste-from-elsewhere | No (write-only OSC52 for Copy Path) |
| Duplicate action (file copy-in-place) | §10.2, §11.4 | Neither surface |
| Shared name validator | §9, §13 | Duplicated (crate `state.rs:1654`, app `keybindings.rs:29261+`) |

## 5. Verification results (resolved 2026-07-24)

1. **Tree-pane right-click on a file: moot, but a new gap found.** The Browse tree pane
   is directories-only (`browse.rs:3082` uses `child_directories`), so the §9.2
   file-suppression rule is vacuously satisfied. However, the tree pane has **no context
   menu at all** — `open_context_menu` only fires for `BrowseEntry`/`BrowseList` buttons
   (`keybindings.rs:6519-6526`); tree-node clicks only navigate (`keybindings.rs:31952`).
   §9.2's tree-pane New submenu is therefore MISSING in Browse too, not just the picker.
2. **Browse folder-item right-click: COMPLIANT.** `ContextAction::NewFile/NewFolder`
   always create in `app.browse.current_dir` (`keybindings.rs:29243-29259`), never inside
   the right-clicked folder. §9.3 baseline is correct today; needs regression cover only.
3. **Directory rename: PRESENT.** Directory entry menu includes File operations → Rename
   (`context_menu.rs:708-729, 370-382`); inline rename allows directories (only archive
   directories are blocked, `keybindings.rs:1825-1828`). Compliant baseline.
4. **Tree-pane inline rename: ABSENT.** Rename operates on `browse.selected_entry()`
   (browse pane) only; no rename path exists for tree nodes. Gap vs §13 in BOTH surfaces.
5. **Missing bookmark target: partial.** Navigation validates `path.is_dir()` and shows
   "bookmark: path no longer exists" (`keybindings.rs:29545-29568`) — good. The overlay
   does NOT visually mark dead bookmarks (`bookmarks_overlay.rs:76-121`) — §8
   "representing missing or inaccessible bookmark targets" is half-met.
6. **Path bar double-click: effectively satisfied.** Any click on the breadcrumb opens
   the editor with the full path pre-selected (`TextInputState::new_selected`,
   `browse.rs:4976`). No distinct double-click handler exists or is needed; document as
   the intended equivalent behavior.
7. **Type-ahead cycling: none.** Repeat keys extend the buffer ("a","a" → query "aa");
   no cycling, no wrap-around; search always from index 0, prefix-first then substring;
   failed match leaves cursor unchanged; buffer clears only on 1500ms timeout
   (`browse.rs:12, 4227-4253`). This is the baseline the shared engine should replicate
   (per decision #2, UX parity — cycling is not required).

## 6. Structural note for the brief

The spec's consolidation mandate collides with the current dependency direction: the
sophisticated implementations (search, bookmarks, context menus, selection, text input,
inline edit) live in the app, and the crate cannot depend on the app. Consolidation
therefore means *extraction*: either (a) move shared engines (text input, type-to-select,
click timing, name validation, selection model, filesystem clipboard, context-menu
scaffolding) down into `tui-file-picker` or a new lower-level crate, and re-point the app
at them; or (b) keep heavy subsystems (search, bookmarks) app-side and give the picker
host-integration hooks so the app supplies them to the picker surface. The spec permits
either ("the reusable crate **or a lower-level shared component** wherever practical"),
but the brief must choose per subsystem — this is the single biggest design decision the
implementing model faces.

Rough magnitude by area (crate-side work dominates): context-menu + right-click layer in
the picker, multi-selection + clipboard unification, text-input unification, inline-edit
migration (modal→inline), type-to-select extraction + dir-priority fix, maximize/restore,
then search/bookmarks parity (largest, likely via host hooks), plus test coverage per
§16's regression requirements.
