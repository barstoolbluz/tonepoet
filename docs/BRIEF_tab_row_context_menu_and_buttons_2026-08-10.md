# tonepoet — Tab-row right-click context menu + labelled buttons + clickable close — 2026-08-10

A focused UX round on the **tab strip** for both surfaces — the TUI Browse view
(`src/tui/draw_browse.rs`) and the tui-file-picker crate (`crates/tui-file-picker/`).
The tabbed-browsing feature itself already shipped and is green; this adds the tab-row
**right-click context menu**, reliable **click targets**, and **labelled buttons**.

Outcomes + guardrails; diagnosis is *evidence*, not prescription — **you are the arbiter of HOW**.

## Context (already done — do not redo)
Tabbed browsing is committed to `hardening` @ `6de3505` and green ×2 (5762/0): per-tab state,
`BrowseTabId` async routing, keybindings, the tab strip, the picker's `Vec<PickerTab>`. The strip
already renders per-tab cells + trailing glyph controls (`↶` reopen, `⧉` duplicate, `+` new) and a
per-tab `×`, and registers hit regions (`TuiButton::BrowseDirTab*` in draw_browse.rs;
`FilePickerHitAction::Tab*` in the picker). Keyboard tab-switch (`+` next / `Ctrl+7` prev, plus
Ctrl+T/Ctrl+W, Alt+U/Alt+D, Alt+1–9, Alt+,/Alt+.) works and is byobu-verified — **keep all of it**.

## Ground rules
- Base = `hardening` @ `6de3505` (the bundle is that tree). Version **0.4.6 — do not bump.** No merge.
- Gate: `cargo test --workspace --no-fail-fast` green **×2** (the applier runs it; you have no cargo).
- **Byobu/tmux (HARD):** no F-keys; no plain-letter bindings in the Browse pane. Mouse actions and
  the existing chords only.
- **UX parity, not shared code** — Browse and the picker stay separate implementations (Option C);
  build the same behavior on each, don't rebase one on the other.

---

# Outcomes

**O1 — Tab-row right-click context menu (both surfaces).** Right-clicking **anywhere on the tab row
— including on a tab** — opens a context menu:
- **New Tab** (always)
- **Duplicate** — duplicates the **tab that was right-clicked** (same directory, adjacent), when the
  click landed on a specific tab
- **Close** — closes the **tab that was right-clicked**, when the click landed on a specific tab
- **Reopen Closed Tab** — available **from anywhere on the row** (empty space or on a tab); enabled
  only when there is a closed tab to restore (grey/disabled otherwise)

When the right-click lands on **empty strip space** (not on a tab), Duplicate/Close have no target,
so the menu shows just **New Tab + Reopen Closed Tab**. (Sensible default — refine if you see better,
but never present Duplicate/Close with no target.)

**O1 mechanics (these are load-bearing — the naive path builds the wrong menu):**
- **Intercept the right-click in the strip block before the generic handler.** A right-`Down` over a
  `BrowseDirTab*` hit region (or empty strip row) must be handled in the strip-ownership block
  (`keybindings.rs:~52443`, alongside the existing middle/left arms) and **return**, opening the tab
  menu there. Otherwise it falls through to the generic Browse right-click handler, which calls
  `open_context_menu_with_tx` (`keybindings.rs:10549`) and **defaults a strip hit to the non-tab
  context menu** — the *entry* menu if a file happens to be selected (`build_browse_entry_menu`,
  `:10597`), else the *empty* menu (`build_browse_empty_menu`, `:10601`). Either way a naive
  implementation silently opens the *wrong* (non-tab) menu over the strip (and would pass a shallow
  "a menu appears" test).
- **Target the right-clicked tab by INDEX, carried on the action itself.** Tabs have no cursor to
  move, so store the clicked index **on the tab `ContextAction` variants** (e.g.
  `BrowseTabDuplicate(index)`, `BrowseTabClose(index)`) — mirror the existing `OpenEntryInNewTab(PathBuf)`
  pattern of carrying its target — so a later reorder/focus change can't redirect the action.
- **Duplicate needs a NEW by-index path.** `request_duplicate_browse_tab` (`keybindings.rs:6681`) and
  `browse.duplicate_tab()` (`browse.rs:3691`) duplicate the **active** tab only; there is **no**
  by-index duplicate on either surface (picker `duplicate_tab` `state.rs:1539` is also active-only). Add
  a by-index duplicate (or switch-to-target-then-duplicate, the way `request_close_browse_tab` already
  switches to the target index). **Close is fine** — `request_close_browse_tab(index)` / picker
  `close_tab(index)` already take an index.
- **Menu dismissal + focus.** The menu closes on **Esc** and **click-away** like other context menus
  (reuse the overlay machinery); on close, focus returns to Browse with no stray file-entry selection
  side effect from having opened it.
- **Mouse-only by design.** Opening the tab menu is a right-click action; do **not** add a plain-letter
  Browse binding for it (Alt+M stays the *entry* menu). Discoverability is covered by O5.
- **Right-click must not start a drag.** Opening the menu must not call `begin_tab_drag`, and any
  in-flight `tab_drag_active` state must not survive a right-click (clear it if set).

**O2 — Per-tab close is reliably LEFT-clickable (Browse only).** Field-verified symptom: left-clicking
a tab's `×` does **not** close it (middle-click and Ctrl+W do work). **The mechanism is NOT confirmed —
do not assume it; reproduce and diagnose.** A static trace is ambiguous: the strip's left-`Down` arm
(`keybindings.rs:52459`) handles only `BrowseDirTab` and **falls through** for `BrowseDirTabClose`
(only the middle-`Down` arm at `:52449` closes), while the downstream generic button dispatch *does*
map `BrowseDirTabClose → request_close_browse_tab` (`:53009`). So whether the left-click is dropped on
`Down`, pre-empted on `Up` by the range-select handler (`:52489`), or fails for another reason (e.g. a
narrow-cell `close_w == 0` at `draw_browse.rs:235` where a cell under 5 cols draws no `×` and registers
no close region) must be **reproduced and diagnosed**, not guessed. The robust fix is to handle
left-click on the close region **explicitly in the strip block** (mirror the middle-`Down` arm at
`:52449`, with a `return`), so closing is deterministic regardless of the downstream path. Keep
middle-click-close, left-click on the tab **body** (switch/activate), and drag-to-reorder unchanged.
**Picker note:** the picker already handles left-click `TabClose → close_tab(index)`
(`crates/tui-file-picker/src/input.rs:388`) — it is NOT broken; do not touch it.

**O3 — Labelled buttons + separators; drop the standalone Duplicate button.** Replace the glyph
controls with clear **text/labelled buttons** with visible separators/padding:
- Trailing inline controls become **`New Tab`** (labelled button). **Remove the standalone
  `Duplicate` button** — duplication now lives in the right-click menu (O1). Reopen may stay as a
  labelled button *or* live only in the menu — your call, but it must be reachable (menu covers it).
  **Invariant:** dropping the Reopen *button* under width pressure (width/overflow rule below) must
  NEVER remove Reopen from the right-click *menu* — the menu is its guaranteed home.
- Each tab's close becomes an obvious click target (e.g. `[×]`) whose **hit region exactly matches
  what's drawn**. (Note: a matching close region already exists at `draw_browse.rs:241`; O2 is a
  *handler* gap, not a draw/geometry mismatch — if you widen the glyph to `[×]`, keep the region
  inside `close_w`.)
- Use real separators (e.g. `│` / padding) between controls so they read as distinct buttons, not a
  glyph run.
- **Width/overflow:** text labels are wider than glyphs and the strip is one line (and only drawn at
  2+ tabs). Reserve the action-button width first (as today), give tabs the remainder with the
  existing `‹ … ›+N` overflow, and define graceful degradation — labelled controls may fall back to
  compact forms (or drop Reopen first) below a width threshold. Never render controls that overrun
  `area.right()` or zero out `tabs_w`.

**O4 — Picker key parity.** The picker gains the same switch keys as Browse: **`+` = next**,
**`Ctrl+7` = previous**, with the same guard that a **bare `+` only switches when 2+ tabs exist**
(so a single-tab picker keeps `+` for type-ahead). `Ctrl+7` carries a modifier and never shadows
type-ahead. Keep the picker's existing tab keys/behaviors.

**O5 — Discoverability.** Surface the switch keys where users will see them — preferred: the **footer
context bar** (`src/tui/draw_footer.rs`) when Browse has 2+ tabs (e.g. `+/^7 switch · ^T new ·
^W close`). Don't crowd the one-line strip if the footer is cleaner. **Note:** `hint_groups_for(current:
AppScreen, theme)` (`draw_footer.rs:278`) has no access to `app`/tab-count and the Browse hint set is
static (`:288`); a "when 2+ tabs" hint requires threading tab-count (or `&AppState`) into
`hint_groups_for`/`draw_context_bar`, and the existing width-drop/priority logic (`drop_one_hint`)
must keep working with the added hints.

**O6 — Everything else stays.** All existing keybindings and tab behaviors, the single-tab
layout (the strip and its row appear only at 2+ tabs — do not reintroduce a strip row at one tab),
and the green feature remain intact.

---

# Guardrails / invariants (hard)
- **No regression** to the shipped tabbed feature: per-tab independence, `BrowseTabId` async routing,
  clipboard/singleton sharing, archive-tab close/reopen lifecycle, and the single-tab-no-strip layout
  (Option B) all stay green. Full workspace suite green ×2.
- **Right-click on a tab must target THAT tab** (Duplicate/Close operate on the right-clicked tab,
  not the active tab), and the menu must bind to the tab it opened over even if focus later changes.
- **Left-click on the tab body still switches/reorders**; only the close region closes on left-click;
  middle-click-close stays; middle-click stays scoped to the strip (no stray primary-paste elsewhere).
- **Left-click on empty strip space is inert** — no menu, no selection; it must not leak into the
  file-list range-selection/hover handlers.
- **Byobu-safe**; no new plain-letter Browse bindings.
- Menu contents/positioning reuse each surface's existing context-menu machinery (don't invent a new
  overlay system) — see §Evidence.

---

# Evidence (anchors — verify, then design; base `6de3505`)

**Browse strip + hit regions:** `draw_browse_tab_strip` (`src/tui/draw_browse.rs:174`) renders cells
`{▐ active}{◐ loading}{label}{• selected}{ ×}` + trailing `↶ ⧉ +`, registering
`TuiButton::BrowseDirTab(i)` / `BrowseDirTabClose(i)` / `BrowseDirTabNew` / `BrowseDirTabDuplicate` /
`BrowseDirTabReopenClosed` (draw_browse.rs:~238–270). The strip only draws at `tab_count() > 1`
(Option B) — keep that.

**The O2 close-click gap (mechanism UNCONFIRMED — see O2):** in the strip mouse block
(`src/tui/keybindings.rs:~52426`), **middle**-`Down` on a tab/close closes (`:52449`), but the
**left**-`Down` arm (`:52459`) handles only `BrowseDirTab` and **falls through** for
`BrowseDirTabClose` (no return). The generic button dispatch *does* map
`BrowseDirTabClose → request_close_browse_tab` (`:53009`), so it is unclear whether the left-click is
lost on `Down`, pre-empted on `Up` by the range-select handler (`:52489`), or dropped by a narrow-cell
`close_w == 0` (`draw_browse.rs:235`). Reproduce, then wire an explicit left-click arm for the close
region (mirror the middle-click, with a `return`).

**Browse context-menu machinery to reuse:** `ContextAction` enum (`src/tui/context_menu.rs:98`),
`ContextMenuEntry`/`ContextMenuItem` (`:65`/`:53`), `item()`/`item_enabled()` helpers (`:377`/`:386`),
and `open_context_menu_with_tx(app, x, y, tx: Option<&mpsc::Sender<AppMessage>>)` **at
`src/tui/keybindings.rs:10549`** (opens `ActiveOverlay::ContextMenu`; dispatches by `find_button_at`
and **defaults empty/unknown hits to the file-entry/empty menu** — hence the O1 intercept requirement).
Add tab `ContextAction` variants (carrying the target index) + a builder for the tab-row menu; open it
on right-`Down` over the strip **before** this generic handler runs.

**Browse tab actions to call:** `request_new_browse_tab` (`keybindings.rs:6689`),
`request_reopen_browse_tab` (`:6697`), `request_close_browse_tab(index)` (`:6627`, **already
by-index** — switches to that tab's id then closes), and `request_duplicate_browse_tab` (`:6681`,
**no index** — calls `browse.duplicate_tab()` `browse.rs:3691` which duplicates the *active* tab).
For the menu, Close is ready; **Duplicate needs a new by-index path** (add `duplicate_tab_at(index)`
or switch-to-target-then-duplicate). Picker mirrors this: `close_tab(index)` (`state.rs:1552`) is
by-index; `duplicate_tab` (`:1539`) is active-only and needs the same new path.

**Picker machinery to reuse:** `FilePickerContextMenuKind` (`crates/tui-file-picker/src/state.rs:208`)
+ `context_menu_kind`/`context_menu_target`/`context_menu_anchor` (`:1106–1108`); hit actions
`FilePickerHitAction::TabActivate/TabClose/TabNew/TabDuplicate/TabReopenClosed`; tab actions
`new_tab` (`:1510`), `open_dir_in_new_tab` (`:1526`), `duplicate_tab` (`:1539`), `close_tab` (`:1552`),
`reopen_closed_tab` (`:1608`). Strip render: `render_tab_strip` (`crates/tui-file-picker/src/render.rs`).
Add a `TabStrip` context-menu kind + tab-row right-click; mirror the Browse behavior.

---

# Deliverables
- Patch/changed files; a short WHY per surface; the tests below; honest note you can't run cargo
  (the applier gates ×2).
- **Tests (behavior):** (a) right-click a tab opens the **tab** menu (assert its entries positively:
  New/Duplicate/Close/Reopen), **not** the non-tab entry/empty menu — and Duplicate/Close act on the
  **right-clicked** tab (target by index:
  duplicate/close a non-active tab and assert it, not the active one); (b) right-click empty strip
  space → New + Reopen only; (c) Reopen is available/enabled from anywhere on the row when a closed tab
  exists, disabled otherwise; (d) **left-click** on a tab's close closes that tab **(Browse)**;
  left-click on the body still switches; middle-click-close still works; left-click on empty strip is
  inert; (e) picker `+`/`Ctrl+7` switch (bare `+` only at 2+ tabs); (f) single-tab layout unchanged
  (no strip row at one tab); (g) regression: the existing tab suites stay green (incl. the picker's
  already-working left-click close).

# Bundle manifest
- This brief. Complete compiling `hardening` @ `6de3505` tree:
  - Browse: `src/tui/draw_browse.rs`, `src/tui/keybindings.rs`, `src/tui/context_menu.rs`,
    `src/tui/button_map.rs`, `src/tui/draw_footer.rs`, `src/tui/app.rs`, `src/tui/browse.rs`.
  - Picker: the entire `crates/tui-file-picker/` crate.
  - Full `src/` + `crates/` + `tonepoet-pipeline/` + root `Cargo.toml` + `flake.nix` + `CLAUDE.md`
    so it compiles. NOT `target/`. If anything's missing, say so rather than guessing.
