# Brief: Options Menu Fixes & Layout Submenu

## Overview

Five fixes to the Browse screen's Options dropdown menu:

1. **Z-order bug**: Menu renders behind browse pane content instead of on top
2. **No hover highlighting**: Mouse hover doesn't show a highlight bar on menu items
3. **Esc doesn't close the menu**: Standard UX expectation violated
4. **Click-outside doesn't close the menu**: Standard UX expectation violated
5. **New Layout submenu**: Add "Show Explore Pane" / "Show Info Pane" toggles for users who don't want side panes or are on small terminals

## Bug 1: Menu renders behind browse pane content

### Root cause

`draw_options_menu()` (`src/tui/draw_browse.rs:440`) renders a `Block` and `Paragraph` into the menu area, but does NOT call `f.render_widget(Clear, area)` first to erase underlying content. The context menu system (`src/tui/draw_overlays.rs:326`) does this correctly — it uses `ratatui::widgets::Clear` before rendering the block.

The options menu's position starts at `toolbar_area.y + 1` and extends downward into `chunks[2]` (the three-pane content area). Without a `Clear`, the browse pane title bar and file list content bleed through any menu cells that aren't explicitly covered by the `Paragraph` text.

### Fix

Add `f.render_widget(Clear, area)` immediately before the `Block` render in `draw_options_menu()`. This is a one-line fix but should be verified for all submenu states (Root, Columns, Sort, Filter, ArchiveListing).

Also add an explicit background style to the `Block` so the interior has a solid fill:

```rust
f.render_widget(Clear, area);
let block = Block::default()
    .borders(Borders::ALL)
    .title(title)
    .border_style(theme.border(theme.cyan))
    .style(Style::default().bg(theme.bg));  // solid background fill
```

## Bug 2: No hover highlighting on menu items

### Root cause

`draw_options_menu()` renders all items with `theme.text_style()` regardless of hover state. It does not receive or check `app.hover_target`. The context menu system uses a `selected` index to highlight the focused row — the options menu has no equivalent.

### Fix

1. Pass `hover: Option<TuiButton>` into `draw_options_menu()`.

2. When rendering each menu row, check if the row's `TuiButton` matches `hover`. If so, apply a highlight style (same pattern as context menu selection: `fg(theme.bg).bg(theme.blue).add_modifier(BOLD)`).

3. Update the call site at `draw_browse_screen()` line 192 to pass `app.hover_target`.

```rust
// In draw_options_menu, when building line styles:
let style = match button {
    Some(btn) if hover == Some(*btn) => {
        Style::default().fg(theme.bg).bg(theme.blue).add_modifier(Modifier::BOLD)
    }
    Some(_) => theme.text_style(),
    None => Style::default().fg(theme.border_dim),  // separator
};
```

## Bug 3: Esc doesn't close the Options menu

### Root cause

The Esc handler in `handle_browse_key()` (`src/tui/keybindings.rs:2690`) has a cascade of checks (type-ahead, search, visual mode, info focus, multi-selection, filter, archive) but does NOT check `app.browse.options_menu.is_open()`. Pressing Esc while the Options menu is open falls through to whatever else is active.

### Fix

Add the options menu check as the FIRST item in the Esc cascade, before all other checks. When the options menu is open, Esc should close it and do nothing else:

```rust
(KeyCode::Esc, _) => {
    if app.browse.options_menu.is_open() {
        app.browse.close_options_menu();
    } else if app.cancel_archive_listing() {
        // ... existing cascade
    }
}
```

If the menu is on a submenu (e.g., Columns, Layout), Esc should return to the Root menu first. A second Esc closes the menu entirely. Check `close_options_menu()` behavior — if it always goes to `Closed`, add a `back_or_close_options_menu()` method:

```rust
pub fn back_or_close_options_menu(&mut self) {
    match self.options_menu {
        BrowseOptionsMenu::Root => self.options_menu = BrowseOptionsMenu::Closed,
        BrowseOptionsMenu::Closed => {}
        _ => self.options_menu = BrowseOptionsMenu::Root,  // submenu → root
    }
}
```

## Bug 4: Click-outside doesn't close the Options menu

### Root cause

When the user clicks anywhere that is NOT an Options menu button, the menu stays open. There is no dismiss-on-click-outside logic.

### Fix

In the browse mouse click handler (`src/tui/keybindings.rs`), when processing a mouse click on the Browse screen while `options_menu.is_open()`:

- If the clicked `TuiButton` is an `BrowseOptions*` variant → handle it normally (existing code)
- If the clicked `TuiButton` is anything else (or no button was hit) → close the options menu first, then optionally handle the click

The simplest approach: at the top of the browse mouse button dispatch, if the options menu is open and the clicked button is NOT a `BrowseOptions*` variant, close the menu and consume the click (don't pass it through). This matches standard dropdown behavior — click-outside dismisses without triggering the underlying element.

Implementation pattern:

```rust
// At the top of browse mouse button dispatch:
if app.browse.options_menu.is_open() {
    let is_options_button = matches!(button,
        TuiButton::BrowseOptionsShowHidden
        | TuiButton::BrowseOptionsColumns
        | TuiButton::BrowseOptionsSort
        | TuiButton::BrowseOptionsFilter
        | TuiButton::BrowseOptionsArchiveListing
        | TuiButton::BrowseOptionsSaveLayout
        | TuiButton::BrowseOptionsRestoreDefaults
        | TuiButton::BrowseOptionsColumn(_)
        | TuiButton::BrowseOptionsSortChoice(_, _)
        | TuiButton::BrowseOptionsFilterChoice(_)
        | TuiButton::BrowseOptionsArchiveChoice(_)
        | TuiButton::BrowseOptionsLayout
        | TuiButton::BrowseOptionsToggleExplore
        | TuiButton::BrowseOptionsToggleInfo
        | TuiButton::BrowseToolbarOptions  // re-clicking Options toggles
    );
    if !is_options_button {
        app.browse.close_options_menu();
        return;  // consume the click
    }
}
```

## Feature: Layout submenu

### Important: "disabled" vs "collapsed" — two separate concepts

The browse screen has TWO distinct pane visibility states:

1. **Collapsed** (existing `▾`/`▸` click on pane title bar): Pane shrinks to a 3-column-wide vertical title bar. Still occupies space. Still visible. The user can click the bar to expand it again. Controlled by `explore_collapsed` / `info_collapsed`.

2. **Disabled** (NEW — Layout submenu toggle): Pane is **completely removed** from the layout. Zero columns. No vertical bar. No space used at all. The remaining panes expand to fill the entire content area. This is for users who don't want these panes at all, or who are working on small terminals and need to reclaim every column.

The Layout submenu controls the **disabled** state, not the collapsed state. When a pane is disabled, it does not appear in the layout at all — not even as a collapsed vertical bar. The `▾`/`▸` toggles on the pane title bars continue to control the collapsed/expanded state independently, but only matter when the pane is enabled.

### Design

Add a "Layout" submenu to the Options root menu, between "Show hidden files" and "Columns":

```
┌─ Options ─────────────────┐
│ ● Show hidden files       │
│ Layout                  ▸   │   <-- NEW
│ Columns               ▸   │
│ Default sort          ▸   │
│ Filter                ▸   │
│ Archive listing mode  ▸   │
│───────────────────────────│
│ Save layout as default    │
│ Restore defaults          │
└───────────────────────────┘
```

Layout submenu contents:

```
┌─ Layout ──────────────┐
│ ● Show Explore pane │
│ ● Show Info pane    │
└─────────────────────┘
```

Checkmarks reflect whether the pane is **enabled** (visible, whether collapsed or expanded) vs **disabled** (completely removed from layout). Clicking toggles the pane between enabled and disabled and persists via `persist_browse_config()`.

### New state fields

Add to `BrowseState`:

```rust
pub explore_enabled: bool,   // false = completely removed from layout
pub info_enabled: bool,      // false = completely removed from layout
```

Both default to `true`. These are independent of `explore_collapsed` / `info_collapsed`.

### New config fields

Add to `BrowsingConfig` in `src/config.rs`:

```rust
pub layout_explore_enabled: bool,   // default true
pub layout_info_enabled: bool,      // default true
```

These persist to `[browsing]` in config.toml as `layout_explore_enabled = true/false` and `layout_info_enabled = true/false`.

### Layout changes

`browse_content_layout()` in `src/tui/draw_browse.rs` must account for the enabled/disabled state. When a pane is disabled, it gets `Constraint::Length(0)` — not `Length(3)` like collapsed. The layout matrix becomes:

| Explore enabled | Info enabled | Explore state | Info state | Constraints |
|---|---|---|---|---|
| yes | yes | open | open | 20% / 50% / 30% |
| yes | yes | collapsed | open | 3 / 2:3 / 1:3 |
| yes | yes | open | collapsed | 20% / Min(40) / 3 |
| yes | yes | collapsed | collapsed | 3 / Min(40) / 3 |
| **no** | yes | — | open | **0** / 60% / 40% |
| **no** | yes | — | collapsed | **0** / Min(40) / 3 |
| yes | **no** | open | — | 20% / 80% / **0** |
| yes | **no** | collapsed | — | 3 / Min(40) / **0** |
| **no** | **no** | — | — | **0** / 100% / **0** |

When a pane is disabled, `draw_browse_screen()` must skip rendering it entirely (no `draw_explore_pane`, no `draw_collapsed_pane`, no button registration). The browse pane expands to fill the freed space.

### Implementation

1. **Add `BrowseOptionsMenu::Layout` variant** to `src/tui/browse.rs:934`:

```rust
pub enum BrowseOptionsMenu {
    Closed,
    Root,
    Layout,       // <-- NEW
    Columns,
    Sort,
    Filter,
    ArchiveListing,
}
```

2. **Add state fields** to `BrowseState`:

```rust
pub explore_enabled: bool,
pub info_enabled: bool,
```

3. **Add config fields** to `BrowsingConfig` in `src/config.rs`:

```rust
pub layout_explore_enabled: bool,
pub layout_info_enabled: bool,
```

With defaults of `true`, serde default, and normalization.

4. **Add `TuiButton` variants** to `src/tui/button_map.rs`:

```rust
BrowseOptionsLayout,              // Opens Layout submenu
BrowseOptionsToggleExplore,     // Toggle explore pane enabled/disabled
BrowseOptionsToggleInfo,        // Toggle info pane enabled/disabled
```

5. **Update `browse_content_layout()`** to check `explore_enabled` / `info_enabled` and use `Length(0)` when disabled.

6. **Update `draw_browse_screen()`** to skip rendering disabled panes entirely.

7. **Add Layout submenu rendering** in `draw_options_menu()`:

```rust
BrowseOptionsMenu::Layout => (
    "Layout",
    vec![
        (
            format!(" {} Show Explore pane", if browse.explore_enabled { "●" } else { "○" }),
            Some(TuiButton::BrowseOptionsToggleExplore),
        ),
        (
            format!(" {} Show Info pane", if browse.info_enabled { "●" } else { "○" }),
            Some(TuiButton::BrowseOptionsToggleInfo),
        ),
    ],
),
```

8. **Handle click events** in `src/tui/keybindings.rs`:

```rust
TuiButton::BrowseOptionsLayout => {
    app.browse.options_menu = BrowseOptionsMenu::Layout;
}
TuiButton::BrowseOptionsToggleExplore => {
    app.browse.explore_enabled = !app.browse.explore_enabled;
    persist_browse_config(app);
    // Keep menu open so user can toggle both panes without re-opening
}
TuiButton::BrowseOptionsToggleInfo => {
    app.browse.info_enabled = !app.browse.info_enabled;
    persist_browse_config(app);
}
```

9. **Wire config persistence**: `capture_browsing_config()` must include `explore_enabled` / `info_enabled`. `apply_browsing_config()` must restore them. "Restore defaults" resets both to `true`.

## Current code locations

- Options menu draw: `src/tui/draw_browse.rs:440` (`draw_options_menu`)
- Options menu call site: `src/tui/draw_browse.rs:191-192`
- BrowseOptionsMenu enum: `src/tui/browse.rs:934`
- `close_options_menu()`: `src/tui/browse.rs` (search `close_options_menu`)
- TuiButton variants: `src/tui/button_map.rs:211-221`
- Button click handlers: `src/tui/keybindings.rs:23784` (search `BrowseOptionsShowHidden`)
- Esc cascade: `src/tui/keybindings.rs:2690` (inside `handle_browse_key`)
- Pane toggle: `src/tui/browse.rs:1471` (`toggle_pane`)
- Config persistence: `src/tui/keybindings.rs:983` (`persist_browse_config`)
- Context menu Clear pattern: `src/tui/draw_overlays.rs:326`
- Context menu hover highlight pattern: `src/tui/draw_overlays.rs:358-369`

## Files to modify

1. **`src/tui/draw_browse.rs`** — Cascading two-panel menu render, Layout submenu rendering, skip disabled panes in layout/draw
2. **`src/tui/browse.rs`** — Add `BrowseOptionsMenu::Layout` variant, `explore_enabled`/`info_enabled` fields, `back_or_close_options_menu()` method, wire config apply/capture
3. **`src/tui/button_map.rs`** — Add `BrowseOptionsLayout`, `BrowseOptionsToggleExplore`, `BrowseOptionsToggleInfo` variants
4. **`src/tui/keybindings.rs`** — Handle Layout button clicks, toggle enabled state, persist config
5. **`src/config.rs`** — Add `layout_explore_enabled`, `layout_info_enabled` fields to `BrowsingConfig` with defaults and normalization

## Bug 6: Submenus replace root menu instead of cascading

### Root cause

Menu items with `▸` indicators (Columns, Default sort, Filter, Archive listing mode, Layout) promise fly-out cascading submenus. Instead, clicking them replaces the root menu content in the same box. This violates standard cascading menu UX — the root menu should remain visible with the selected item highlighted, and the submenu should appear as a separate panel to its right.

### Current behavior

Clicking "Columns ▸" sets `options_menu = BrowseOptionsMenu::Columns`, and on the next frame `draw_options_menu()` renders the Columns list in place of the Root menu, at the same position. The root menu disappears entirely.

### Required behavior

Clicking "Columns ▸" should:
1. Keep the root menu visible with "Columns ▸" highlighted
2. Open the Columns submenu as a separate panel to the right of the root menu
3. Both panels render simultaneously (root + active submenu)

### Fix

Refactor the options menu to render as a two-panel stack when a submenu is active:

1. **Always render the root menu** when any `BrowseOptionsMenu` state other than `Closed` is active.

2. **When on a submenu state** (Columns, Sort, Filter, ArchiveListing, Layout), additionally render the submenu panel positioned to the right of the root menu, offset by the root menu's width.

3. **Highlight the parent item** in the root menu that corresponds to the active submenu (e.g., highlight "Columns ▸" when `BrowseOptionsMenu::Columns` is active).

4. **Register button hit targets** for both panels. Clicks on root items should navigate to that submenu (or toggle/execute for Show hidden). Clicks on submenu items execute the action.

The context menu system in `src/tui/draw_overlays.rs` already implements proper cascading with `MenuLevel` stacks and `context_menu_stack_rects()`. The options menu should follow the same visual pattern:

```
┌─ Options ─────────────────┐┌─ Columns ─────────┐
│ ○ Show hidden files       ││ ☑ Name             │
│ Layout                ▸   ││ ☑ Size             │
│ Columns               ▸ ◄─┤│ ☑ Date             │
│ Default sort          ▸   ││ ☑ Type             │
│ Filter                ▸   ││ ☐ Format           │
│ Archive listing mode  ▸   ││ ☐ Codec            │
│───────────────────────────││ ☐ Sample rate      │
│ Save layout as default    ││ ☐ Channels         │
│ Restore defaults          ││ ☐ Duration         │
└───────────────────────────┘│ ☐ Artist           │
                             │ ☐ Album            │
                             └────────────────────┘
```

Implementation approach:
- Split `draw_options_menu()` into two phases: always draw root panel, then conditionally draw submenu panel to its right.
- The root panel is always the same `Root` menu content. When a submenu is active, the corresponding root item gets the hover/selected highlight style.
- The submenu panel uses the same `Clear` + `Block` + hover-highlight pattern.
- Position the submenu panel at `root_x + root_width` (with right-edge clamping if it would overflow the terminal).
- Both panels register their own button hit targets.

### Esc behavior with cascading

- Submenu open → Esc closes the submenu (returns to Root, submenu panel disappears)
- Root menu open (no submenu) → Esc closes the root menu entirely

This matches the existing `back_or_close_options_menu()` behavior.

## Additional code locations

- Context menu cascading renderer: `src/tui/draw_overlays.rs:251` (`draw_context_menu_stack`)
- Context menu rect computation: `src/tui/keybindings.rs:16915` (`context_menu_stack_rects`)
- Context menu panel renderer: `src/tui/draw_overlays.rs:312` (`render_menu_panel_at`)

## Exit criteria

- Options menu renders fully on top of browse pane content (Clear fix)
- Mouse hover highlights menu rows with `fg(bg).bg(blue)` style
- Esc closes the menu (submenu → root on first press, root → closed on second press)
- Clicking outside the menu dismisses it (click consumed, not passed through)
- Submenus open as cascading fly-out panels to the right of the root menu
- Root menu stays visible with the active submenu's parent item highlighted
- Layout submenu with Show Explore / Show Info toggles
- Disabling a pane removes it completely from the layout (zero columns, no vertical bar)
- Browse pane expands to fill space freed by disabled panes
- Disabling both side panes gives browse 100% of content width
- Pane enabled/disabled state persists to `[browsing]` config
- Toggling panes from Layout menu persists to config
- Menu stays open after toggling a pane (user can toggle both without re-opening)
- "Restore defaults" resets both panes to enabled
- `cargo check` — zero errors, zero warnings
- `cargo test --no-run` — zero errors, zero warnings
