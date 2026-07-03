# Brief: Options Menu Follow-up Fixes

## Overview

Three fixes to the Options menu system on the Browse screen:

1. **Layout math**: Disabling the explore pane gives too much space to the info pane (exceeds its 33% max)
2. **Hover-driven submenu open/close**: Mouse hover should open/close submenus automatically (standard Windows/Linux behavior)
3. **Submenu vertical anchoring**: Submenu panel should be anchored to the row of the parent item that spawned it, not to the top of the root menu

## Fix 1: Layout math when explore pane is disabled

### Problem

When the explore pane is disabled via Layout menu, `browse_content_layout()` at `src/tui/draw_browse.rs:217` assigns:

```rust
(false, true, _, false) => vec![Constraint::Length(0), Constraint::Percentage(60), Constraint::Percentage(40)],
```

This gives the info pane 40% — but the info pane's design maximum is 33%. The recovered explore space should go to the browse pane, not the info pane.

Conversely, when the info pane is disabled (line 219):

```rust
(true, false, false, _) => vec![Constraint::Percentage(20), Constraint::Percentage(80), Constraint::Length(0)],
```

This correctly gives the browse pane the extra space (80%), keeping explore at its normal 20%.

### Fix

Change line 217 to cap info at 33%:

```rust
(false, true, _, false) => vec![Constraint::Length(0), Constraint::Ratio(2, 3), Constraint::Ratio(1, 3)],
```

This gives browse 67% and info 33% when explore is disabled — matching the existing pattern used when explore is collapsed (line 221).

## Fix 2: Hover-driven submenu open/close

### Problem

The options menu currently only opens submenus on click. Standard desktop dropdown behavior is:

- Hovering over a root menu item that has a `▸` submenu indicator automatically opens that submenu
- Moving the mouse away from that item (to a different root item or outside the menu) automatically closes the submenu
- The highlight bar follows the mouse across all visible menu panels

Currently `hover_target` is set on mouse move (line 22912-22914 in `src/tui/keybindings.rs`), and `draw_options_menu()` already receives and uses `hover` for highlighting. But there is no logic to auto-open/close submenus based on hover.

### Fix

Add hover-driven submenu logic to the mouse move handler. When the options menu is open and the mouse moves:

1. Check if the hover target is a root menu item with a submenu (`BrowseOptionsColumns`, `BrowseOptionsSort`, `BrowseOptionsFilter`, `BrowseOptionsArchiveListing`, `BrowseOptionsLayout`).
2. If so, set `app.browse.options_menu` to the corresponding submenu state. This causes the next frame to render the submenu panel.
3. If the hover target is a different root menu item WITHOUT a submenu (e.g., `BrowseOptionsShowHidden`, `BrowseOptionsSaveLayout`, `BrowseOptionsRestoreDefaults`), or a submenu item, leave the menu state as-is (the submenu stays open while the user navigates into it).
4. If the hover target is `None` (mouse is outside all menu panels) or is not an options menu button at all, close any open submenu (return to Root state). Do NOT close the root menu — only close the submenu.

The key subtlety: when the user moves from a root `▸` item into the submenu panel, the hover target transitions from the root item to a submenu item. The submenu must NOT close during this transition. So the rule is: close the submenu only when hovering over a non-submenu root item or when hovering outside all menu panels.

Implementation location: In the mouse move handler at `src/tui/keybindings.rs:22911-22915`, after setting `app.hover_target`, add:

```rust
if app.current_screen == AppScreen::Browse && app.browse.options_menu.is_open() {
    options_menu_hover_update(app);
}
```

Where `options_menu_hover_update()` implements the logic above. This function should:

```rust
fn options_menu_hover_update(app: &mut AppState) {
    let hover = app.hover_target;
    match hover {
        // Hovering a root ▸ item → open its submenu
        Some(TuiButton::BrowseOptionsLayout) => app.browse.options_menu = BrowseOptionsMenu::Layout,
        Some(TuiButton::BrowseOptionsColumns) => app.browse.options_menu = BrowseOptionsMenu::Columns,
        Some(TuiButton::BrowseOptionsSort) => app.browse.options_menu = BrowseOptionsMenu::Sort,
        Some(TuiButton::BrowseOptionsFilter) => app.browse.options_menu = BrowseOptionsMenu::Filter,
        Some(TuiButton::BrowseOptionsArchiveListing) => app.browse.options_menu = BrowseOptionsMenu::ArchiveListing,

        // Hovering a non-▸ root item → close submenu, stay on Root
        Some(TuiButton::BrowseOptionsShowHidden)
        | Some(TuiButton::BrowseOptionsSaveLayout)
        | Some(TuiButton::BrowseOptionsRestoreDefaults) => {
            if app.browse.options_menu != BrowseOptionsMenu::Root {
                app.browse.options_menu = BrowseOptionsMenu::Root;
            }
        }

        // Hovering a submenu item or the Options toolbar button → leave state alone
        Some(btn) if is_browse_options_menu_button(btn) => {}

        // Hovering outside all menu panels → close submenu (but keep root open)
        _ => {
            if app.browse.options_menu != BrowseOptionsMenu::Root
                && app.browse.options_menu != BrowseOptionsMenu::Closed
            {
                app.browse.options_menu = BrowseOptionsMenu::Root;
            }
        }
    }
}
```

Note: `BrowseOptionsMenu` must derive or implement `PartialEq` for the `!=` comparisons.

## Fix 3: Submenu vertical anchoring

### Problem

The submenu panel is currently anchored to `root_area.y` — the same vertical position as the root menu's top border. This means all submenus open with their top edge aligned to the root menu's top edge:

```
┌─ Options ─────────┐┌─ Layout ──────────┐
│ ○ Show hidden     ││ ● Show Explore    │
│ Layout          ▸ ││ ● Show Info       │
│ Columns         ▸ │└────────────────────┘
│ ...               │
└───────────────────┘
```

The submenu should be anchored to the row of the parent item that spawned it:

```
┌─ Options ─────────┐
│ ○ Show hidden     │
│ Layout          ▸ │┌─ Layout ──────────┐
│ Columns         ▸ ││ ● Show Explore    │
│ ...               ││ ● Show Info       │
└───────────────────┘└────────────────────┘
```

### Fix

`options_submenu_area()` at `src/tui/draw_browse.rs:655` currently uses `root_area.y` for the submenu's y position. It needs the row index of the active parent item within the root menu to compute the correct y offset.

Change `draw_options_menu()` to pass the parent row index to `options_submenu_area()`. The parent row index can be derived from `active_options_parent_button()` — find which row in `root_rows` has that button, and the submenu's y position becomes:

```rust
let parent_row_index = root_rows.iter().position(|(_, btn)| *btn == active_parent).unwrap_or(0);
let submenu_y = root_area.y + 1 + parent_row_index as u16;  // +1 for top border
```

Then in `options_submenu_area()`, use `submenu_y` instead of `root_area.y`. Ensure the submenu doesn't extend below the terminal — clamp: `submenu_y = submenu_y.min(terminal_height.saturating_sub(submenu_height))`.

## Current code locations

- `browse_content_layout()`: `src/tui/draw_browse.rs:208`
- `draw_options_menu()`: `src/tui/draw_browse.rs:462`
- `options_submenu_area()`: `src/tui/draw_browse.rs:655`
- `render_options_menu_panel()`: `src/tui/draw_browse.rs:730`
- `active_options_parent_button()`: `src/tui/draw_browse.rs:535`
- `options_root_rows()`: `src/tui/draw_browse.rs:509`
- Mouse move handler: `src/tui/keybindings.rs:22911`
- `is_browse_options_menu_button()`: `src/tui/keybindings.rs:1001`
- `BrowseOptionsMenu` enum: `src/tui/browse.rs:934`

## Files to modify

1. **`src/tui/draw_browse.rs`** — Fix layout math (line 217), pass parent row index to submenu area, anchor submenu to parent row
2. **`src/tui/keybindings.rs`** — Add `options_menu_hover_update()`, call it on mouse move when menu is open
3. **`src/tui/browse.rs`** — Add `PartialEq` derive to `BrowseOptionsMenu` if not already present

## Exit criteria

- Disabling explore pane: info stays at 33%, browse gets 67%
- Disabling info pane: explore stays at 20%, browse gets 80% (already correct)
- Mouse hover over `▸` root items auto-opens their submenu
- Mouse hover over non-`▸` root items closes any open submenu
- Mouse leaving all menu panels closes submenu (root stays open)
- Mouse moving from root `▸` item into submenu panel keeps submenu open
- Highlight bar follows mouse across both panels
- Submenu panel top edge aligns with the parent item row, not the root menu top
- Submenu clamped to terminal bottom edge
- `cargo check` — zero errors, zero warnings
- `cargo test --no-run` — zero errors, zero warnings
