# Implementation Brief — Browse UX Hardening (bookmarks, clipboard keys, menus, scrollbars)

Seven user-requested Browse-pane improvements. Items 1–2 implement a UI design that has
already been reviewed and approved by the user (ASCII mockups below are the design
contract for layout/affordances; exact column math may flex). Items 3–7 are behavioral.
Line numbers are anchors from analysis on branch `hardening` @ a781563 — re-locate
before editing.

## 1. Bookmark Manager (replaces the Browse bookmarks overlay)

**What's wrong:** the current overlay (`src/tui/bookmarks_overlay.rs`) is a flat
64-wide list — no filter, no reordering, no scroll indicator, cramped 33% name column,
and no detail about the target beyond a `(missing)` tag. The picker's bookmark panel is
already nicer; the app-level one should be the flagship.

**Design (approved):** adapt the theme builder's anatomy — two-column body (list left,
detail card right), header status zone, chip footer with a status/feedback line
(`src/tui/theme_builder.rs:1636+` is the reference implementation; its left
slots-list + right bordered card + footer chips are the pattern to imitate).

```
┌▾ bookmarks ─────────────────────────────────────────────────────────┐   <- solid title bar,
│ 7 bookmarks · 1 missing                          in: ~/livetorrents │      Browse-pane idiom,
│                                                                     │      green identity
│  / filter…                  ┌─ Uriah Heep ──────────────────────┐   │
│                             │                                   │   │
│ ▸ Uriah Heep              █ │ path    ~/livetorrents/Uriah Heep │   │   <- selection = full-row
│   Music                   █ │         - Look At Yourself (1971) │   │      selection_bg bar + ▸
│   livetorrents            █ │ status  ● reachable               │   │
│   DSD masters             ░ │ target  directory · 14 items      │   │   <- █/░ = new scrollbar
│ ! Old NAS rips            ░ │                                   │   │      widget (item 5)
│   Downloads               ░ │ contents                          │   │
│   temp                    ░ │  ▸ artwork/                       │   │   <- live peek at target:
│                             │  01 - Look At Yourself.flac       │   │      first N entries,
│                             │  02 - I Wanna Be Free.flac        │   │      lazy + cached,
│                             │  03 - July Morning.flac           │   │      non-recursive
│                             │  … 9 more                         │   │
│                             └───────────────────────────────────┘   │
│                                                                     │
│ [a add][e rename][d delete][J/K move][Enter go][Esc close]          │   <- footer_pill chips
│ moved "Uriah Heep" up                                               │   <- status line
└─────────────────────────────────────────────────────────────────────┘
```

Behavior contract:
- Header: bookmark count, missing tally (destructive color), `in:` = current Browse dir
  (the target of `a add`).
- List rows: name only (path lives in the card). Missing targets: `!` marker +
  `theme.destructive` dim — visible immediately, not on activation. `/` enters a filter
  row (substring, case-insensitive, same TextInput machinery); filtered-out rows hidden,
  Esc clears filter before closing the manager.
- Detail card (theme-builder bordered card, bold title = bookmark name): `path`
  (wrapped, `~` abbreviation per existing convention), `status` (`● reachable` green /
  `✕ missing` red), `target` (directory · item count), then a `contents` section
  listing the first handful of child entries (dirs first with `▸`, then files, `… N
  more` tail). Lazy-loaded off the event thread, cached per open, never recursive; on a
  missing target the section collapses to a hint ("target no longer exists — e rename,
  d delete").
- Keys: existing a/e/d/Enter/Esc semantics preserved (`handle_bookmarks_overlay_key`,
  `keybindings.rs:31372+`); NEW: `J`/`K` (shift) move the selected bookmark down/up and
  persist the order; `/` filter. Every mutation echoes to the status line
  (theme-builder style: "moved X up", "deleted Y", "renamed Z").
- Reordering persists as the TOML array order (the authoritative store from the v9
  work, `src/tui/bookmarks.rs` + crate `bookmarks.rs` `BookmarkMutation` model — add a
  move/reorder mutation kind; keep the SQLite mirror reconciliation intact).
- Add/rename stay inline (input row replaces the filter row or appears in the card —
  implementer's choice, but NOT a separate modal; validation and error display via the
  existing error path).
- Sizing: centered, responsive like the theme builder (its min is 92x28; the manager
  should degrade gracefully below that — at minimum fall back to a single-column list
  with the card hidden).
- Entry points: everything that opens the old overlay opens this (`:bookmarks`/`:bm`,
  `command.rs:7374-7383`; `ContextAction::OpenBookmarks`, `context_menu.rs:1923-1926`),
  plus the new path-row dropdown's "Manage…" (item 2). The old overlay rendering is
  retired.

Reference material: theme builder card/chips/status (`theme_builder.rs:1636-1932`),
current overlay (`bookmarks_overlay.rs`), picker bookmark panel — the `name — path` +
proactive `(missing)` treatment worth keeping in spirit (`crates/tui-file-picker/src/
render.rs:1146-1256`), Browse solid title bars (`draw_browse.rs:385-399`), footer
pills (`draw_overlays.rs:20-42`), Tokyo Night tokens (`theme.rs` — `selection_bg`,
`destructive`, `label`, `value`, `text_dim`, `title`, chips green/cyan/amber/purple).

## 2. Bookmarks dropdown on the path row

**Current path row** (`draw_browse.rs:270-334`): `path:` breadcrumb (flexible,
`BrowseBreadcrumb` hit target) + right-aligned 5-wide `Go` button (`BrowsePathGo`,
`keybindings.rs:4420-4450`). No dropdown exists on this row today.

**Required:** a `★ ▾` button AFTER `Go` (user accepts the breadcrumb narrowing ~6
columns). Activating it opens an anchored dropdown:

```
│ path: ~/livetorrents/Uriah Heep - Look At Yourself      [ Go ][ ★ ▾ ] │
                                                       ┌──────────────────┐
                                                       │ Uriah Heep       │  <- Enter/click = navigate
                                                       │ Music            │
                                                       │ livetorrents     │
                                                       │ DSD masters      │
                                                       │ ! Old NAS rips   │  <- missing: dim red,
                                                       │ Downloads        │     activation refused w/
                                                       ├──────────────────┤     existing status msg
                                                       │ ★ Bookmark this  │  <- current add flow
                                                       │ ⚙ Manage…        │  <- opens item-1 manager
                                                       └──────────────────┘
```

- Reuse the `Options ▾` dropdown machinery (`draw_browse.rs:592-709`,
  `options_menu_geometry_for_area`) — anchored, keyboard-navigable (with the item-6
  wrap fix), Esc closes. CAVEAT (audited): that geometry helper hardcodes its anchor
  as a fixed toolbar-left offset (`preferred_x = toolbar_area.x.saturating_add(30)`,
  `draw_browse.rs:676`) — it does NOT take a button position. Generalize it to accept
  an anchor rect (the clamp already exists: `clamp_menu_x`, `draw_browse.rs:858-862`,
  handles the right-edge → clamp-left case) rather than duplicating the machinery.
- List order = persisted bookmark order (item 1). Long lists scroll (cap the panel
  height; scrollbar optional here).
- `Ctrl+B` is currently unbound — bind it to open this dropdown (keyboard parity with
  the mouse affordance). NOT an F-key (hard constraint, §8).
- New `TuiButton` variant for the button; register in the second render pass per the
  two-pass convention.

## 3. Ctrl+C / Ctrl+X / Ctrl+V for files and folders in Browse

**Current state:** filesystem clipboard fully exists — `ContextAction::CutSelection /
CopySelection / PasteSelection` with handlers at `context_menu.rs:1418-1462`,
selection scoping via `collect_selection_for_file_ops_scoped` (`command.rs:8959`,
multi-select aware via `action_selection_in_current_directory`, `browse.rs:4781`),
async paste via `start_filesystem_clipboard_paste`
(`keybindings.rs:26217-26263`). But they are reachable ONLY by right-click. No
keyboard bindings: **quit is Ctrl+Q by deliberate design** (`keybindings.rs:145-149`),
and Ctrl+C/X/V are bound to nothing at global or Browse scope (verified).

**Required:** when the browse pane has focus and NO text editor is active:
- `Ctrl+C` → CopySelection, `Ctrl+X` → CutSelection, `Ctrl+V` → PasteSelection into
  the displayed directory — dispatching into the existing handlers so status messages
  ("Copied 5 items"), archive-entry refusal, and multi-select semantics stay identical
  to the menu path.
- Text-focus precedence is already structurally guaranteed (path input and inline-edit
  handlers consume keys before `handle_browse_key`, `keybindings.rs:91-135`) — do not
  disturb that ordering; add regression tests proving Ctrl+C in the path bar copies
  TEXT while Ctrl+C in the list copies FILES.
- Tree pane: if focus is on the tree, apply to the tree cursor's directory via the
  existing `TreeCut/TreeCopy/TreePaste` actions (`context_menu.rs:133-135, 842-873`) —
  same keys, focus-appropriate target.
- Search-results view and archive listings: refuse with a status message rather than
  silently doing nothing surprising (archives already refuse in the handlers).

## 4. One standard Select submenu

**Current inconsistency:** entry menus carry FOUR loose top-level items — `Select`,
`Select All`, `Select Inverse`, `Deselect` (`context_menu.rs:675-695` AudioFile: right
after Convert's separator; `771-793` Directory: after the Utilities submenu, i.e. the
two menus don't even agree on position) — while the empty-space menu has a
`Selection ▸` submenu with differently-worded children (`Select All / Invert
Selection / Deselect All`, `context_menu.rs:891-926`).

**Required (user-specified):** ONE submenu, same label and wording everywhere:

```
Select ►   This item          <- entry menus only (omit in empty-space menu)
           All
           Invert selection
           Deselect
```

- Reuse the existing ContextActions behind the current items — this is a menu-shape and
  label change, not new behavior.
- Entry menus: the submenu replaces the four loose items (keeps their position after
  Convert's separator in the AudioFile menu; Directory menu likewise consolidates).
- Empty-space menu: `Selection ▸` becomes `Select ▸` with `All / Invert selection /
  Deselect` (no "This item" — nothing is under the pointer).
- Update every stale test pinning the old four-item shape / "Selection" label
  (including the round-9 empty-space traversal test in `keybindings.rs`
  `inline_edit_behavior_tests::browse_empty_space_context_menu_ignores_existing_selection`).

## 5. Clickable proportional scrollbars

**Current state:** NO scrollbars anywhere (verified — zero ratatui `Scrollbar` usage,
zero scrollbar glyph rendering in app or picker). Scroll state exists everywhere:
browse list `scroll_offset` + `visible_height` (`browse.rs:2196-2197`, cursor tracking
`browse.rs:4636-4639`), tree `tree_scroll`/`tree_visible_height` (`browse.rs:2452-2453,
3302-3305`), row viewport math in `draw_browse.rs:1585-1611`. NOTE: search results have
NO separate scroll field — the results view scrolls via the main pane's
`scroll_offset`/`visible_height`, so the list scrollbar covers it automatically.

**Required:** a shared scrollbar widget (crate or app-side shared module — it should
also serve the item-1 manager and, ideally later, the picker):

```
│ 42 - Traffic - The Low Spark….flac   █ │   thumb █ (theme.title): size = visible
│ 43 - Uriah Heep - July Morning.flac  █ │   fraction, position = where you are —
│ 44 - Van Der Graaf….flac             ░ │   the "how much room is left" indicator
│ 45 - Wishbone Ash….flac              ░ │   the user asked for
│ 46 - Yes - Heart Of The Sunrise.flac ░ │   track ░ (theme.text_dim)
```

- Surfaces: browse file list (which also covers search results — shared scroll state),
  tree pane, bookmark manager list.
  Rendered in the column just inside the right border; only when content overflows.
- Mouse: click on track above/below thumb = page up/down; click+drag on thumb = jump
  proportionally (a drag-state pattern exists: `BrowseDragState`). Register hit targets
  per the two-pass ButtonRenderMap convention.
- Keep the thumb ≥1 row; guarantee top/bottom track cells reachable (rounding).
- Mouse wheel already works — `keybindings.rs:32857-32870` routes ScrollUp/ScrollDown
  over the list to `app.browse.scroll_viewport(±3)`. The bar must mirror wheel scrolls
  live; extend equivalent wheel handling to any surface that gets a bar but lacks it.

## 6. Context-menu navigation: wrap-around + no submenu trap

**Current defects** (both verified):
- No wrap: `Up`/`Down` clamp at the ends (`keybindings.rs:6857-6868` — `selected > 0` /
  `selected + 1 < n` guards).
- The trap: `open_context_menu` AUTO-PUSHES the first entry's submenu as a focused
  level at open (`keybindings.rs:6923-6932`, commented "so users see the cascade
  preview"). Since Convert is the first entry in every entry menu, the cursor lands
  INSIDE Convert's children; Up can't leave, and the user must know to press Left.

**Required:**
- Up on the first selectable row wraps to the last selectable row and vice versa, in
  EVERY menu level (skip separators/disabled items as the current movement does).
- Remove the auto-push. The menu opens with the cursor on the first root row. Submenus
  open only on `Right`/`Enter`, click, or hovering the preview panel (the existing
  preview-panel rendering for a highlighted Submenu row — `draw_overlays.rs:587-615`,
  hover promotion `keybindings.rs:24788-24837` — already gives the "cascade preview"
  the auto-push comment wanted, without stealing focus).
- Highlighting a Submenu row must never itself push a level (mouse hover on the row
  highlights only; hover on the preview panel may promote, as today).

## 7. Properties + Tags & Tagging

**Current:** `Edit metadata` (→ `ContextAction::EditMetadataFull`) sits mid-menu in
SIX entry-menu builders: AudioFile (`context_menu.rs:683`), Archive (`:704`), SacdIso
(`:725`), the DVD/Bluray family (`:757`), Directory (`:775`), and OtherFile-when-cue
(`:805`). The `Tagging` submenu (`build_tagging_submenu`, `context_menu.rs:517-547`)
holds the MusicBrainz/CUE actions.

**Required (user-specified):**
- Rename the entry `Edit metadata` → **`Properties`** and move it to the VERY BOTTOM of
  every entry menu (after Copy path, behind a separator — OS convention). Same action
  (`EditMetadataFull`), same dispatch (`context_menu.rs:1399-1410`).
- Rename the `Tagging` submenu → **`Tags & Tagging`** and add **`Edit metadata`** as
  its first child (same `EditMetadataFull` action) followed by a separator, then the
  existing children (MusicBrainz, CUE items).
- Apply consistently across ALL SIX builders enumerated above. Update stale tests
  pinning old labels/positions.

## 8. Constraints

- **No function-key bindings anywhere** (byobu intercepts F-keys; standing project
  rule). Nothing may be reachable only via an F-key.
- Quit stays Ctrl+Q; do not rebind. Ctrl+C/X/V must never reach the browse pane while
  any text editor has focus (existing dispatch order does this — preserve it).
- Preserve all v9 parity behaviors (picker search lifecycle, filesystem clipboard
  semantics, selection rules) — this brief builds on them, it does not reshape them.
- Two-pass rendering: draw immutable, then register mouse targets (ButtonRenderMap).
- Deletion remains permanent with existing guards — the manager's `d delete` deletes a
  BOOKMARK, never the target directory; keep that unambiguous in labels/status.
- Rust 2021, ratatui 0.26/crossterm 0.27. Crate (`tui-file-picker`) must keep building
  standalone if the scrollbar widget lands there.
- Tests: regression cover for each item (menu shapes, wrap behavior, key dispatch
  precedence, scrollbar geometry math, bookmark reorder persistence + SQLite mirror
  reconciliation). Gate: `cargo test --workspace`, zero failures, untruncated results.

## 9. Deliverable

`.tar.gz` overlay with repo-relative paths + manifest (file → one-line change summary)
+ engineering report covering: design decisions taken where this brief leaves choice
(add/rename input placement, scrollbar widget location, dropdown panel sizing),
anything unverifiable without a compiler, and any files you needed but lacked. The
applying side compiles, fixes mechanical errors, runs gates, and audits.
