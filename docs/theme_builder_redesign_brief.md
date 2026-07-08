# Theme Builder Redesign — Implementation Brief

## Overview

Redesign the theme builder overlay from a multi-view architecture (Main/Preset/Derived/Apply/DeleteConfirm as separate full-screen replacements) to a persistent two-pane layout with three tabs controlling the right pane, two floating overlays, and a simplified footer.

**Files to modify:**
- `src/tui/theme_builder.rs` — state, keybindings, draw functions (primary target, ~2000 LOC rewrite)
- `src/tui/theme.rs` — derived element spec grouping metadata, lock slot model
- `src/tui/button_map.rs` — new TuiButton variants for tabs, context menu, gallery filter
- `src/tui/draw_overlays.rs` — dispatch changes (if any)

**Visual mockups** (included in this bundle as HTML files — open in a browser to see the rendered TUI mockups):
- `mockups/theme_builder_two_pane.html` — Edit tab and Preview tab side-by-side
- `mockups/derived_auto_vs_locked_card.html` — Auto vs Locked states of the derived editor card
- `mockups/theme_builder_derived_tab.html` — Full Derived tab with computed color list
- `mockups/gallery_overlay_and_more_menu.html` — Preset gallery overlay and … more popup

---

## 1. Layout Model

### Current state
The builder opens as a centered overlay. `ThemeBuilderView` enum selects which full-screen view to render:
- `Main` — three-panel: slot list + accents grid | editor with hex/RGB/depth/swatches/preview crammed together
- `Preset` — separate dropdown/gallery replacing the whole view
- `Derived` — separate two-panel view (derived list + detail)
- `Apply` — modal dialog
- `DeleteConfirm` — modal dialog

### Target state
A persistent two-pane frame with a tab strip controlling the right pane:

```
╔═ Theme Builder ══════════════════════════════════════════════════════════════╗
║ Tokyo Night Custom    Mode ● Dark    Depth True Color       p presets       ║
╠═══════════════════════════╤═══════════════════════════════════════════════════╣
║ [left pane]               │ Edit  Preview  Derived     [tab strip]          ║
║                           │                                                 ║
║ (content depends on tab)  │ (right card — adapts per tab)                   ║
║                           │                                                 ║
╠═══════════════════════════╧═══════════════════════════════════════════════════╣
║ ^s Save   a Apply   … more   Esc Cancel                                    ║
╚══════════════════════════════════════════════════════════════════════════════╝
```

**Left pane content by tab:**
- **Edit** and **Preview**: the 11 role slots + 16 accent grid (same as current `draw_slot_list`)
- **Derived**: the 29 computed colors with provenance marks and dim subheaders

**Right pane content by tab:**
- **Edit**: single bordered card for the selected role/accent — framed swatch, hex field, RGB sliders, depth+256 status line, collapsed swatches row
- **Preview**: single bordered card showing the live theme preview (the content currently at the bottom of `draw_editor` as `preview_lines`, promoted to fill the whole card)
- **Derived**: same color editor card, wrapped in derivation context (formula, auto value, lock toggle, consequence line)

### Structural changes to `ThemeBuilderView`

Replace the current enum:
```rust
// CURRENT
pub enum ThemeBuilderView {
    Main,
    Preset,
    Derived,
    Apply,
    DeleteConfirm,
}
```

With a tab + overlay model:
```rust
/// Which tab is active in the two-pane layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuilderTab {
    Edit,
    Preview,
    Derived,
}

/// Overlay state — floats on top of the two-pane layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuilderOverlay {
    None,
    Gallery,       // was ThemeBuilderView::Preset (when not preset_applies_on_select)
    MoreMenu,      // new: context menu popup
    Apply,         // was ThemeBuilderView::Apply
    DeleteConfirm, // was ThemeBuilderView::DeleteConfirm
}
```

Update `ThemeBuilderState` to use both:
```rust
pub struct ThemeBuilderState {
    pub tab: BuilderTab,
    pub overlay: BuilderOverlay,
    // ... rest of existing fields
}
```

The standalone gallery mode (opened from Config's "Browse all" button, where `preset_applies_on_select = true`) remains a special case — it renders only the gallery overlay at full size, without the two-pane frame behind it.

---

## 2. Edit Tab — Right Card

The right card is a single bordered box titled with the selected slot name:

```
┌─ panel_bg ───────────────────────────────────┐
│                                              │
│   ▕██████████████▏  #1A1B26  rgb(26,27,38)   │
│                                              │
│   Hex  [ #1A1B26 ]                           │
│   R [██░░░░░░░░░░░░░░░░░░] 26               │
│   G [██░░░░░░░░░░░░░░░░░░] 27               │
│   B [███░░░░░░░░░░░░░░░░░] 38               │
│                                              │
│   Depth True Color · 256→ ██ #1C1C1C 234     │
│                                              │
│   Swatches (none · + to save)                │
│                                              │
└──────────────────────────────────────────────┘
```

### Changes from current `draw_editor`:
1. **Wrap in a bordered card** with the slot name as the title (currently unframed)
2. **Framed swatch**: `▕██████████████▏` with thin border characters, not a bare `████████████████` block. The frame in border color tells the user "this is a color sample and here's its extent" — important for near-background colors that would otherwise be invisible.
3. **Hex + rgb() on the swatch line** instead of separate
4. **Depth demoted to one line**: `Depth True Color · 256→ ██ #1C1C1C 234` — the three depth-mode pill buttons and the two-line readout collapse into a single status line. Clicking the depth label or pressing `D` (shift-d) cycles through TrueColor → Xterm256 → Ansi16.
5. **Swatches collapsed**: `Swatches (none · + to save)` — one line. No naming field, no `[] [+] [del]` litter. The naming dialog appears inline only when `+` is pressed and there's something to name. The row expands to show saved swatch chips only once swatches exist.
6. **Preview removed from editor** — it's now its own tab
7. **"Editing X" header removed** — the card title serves that purpose
8. **`+ Save` chip removed from the editor header** — swatch save is in the collapsed row

### `BuilderEditorFocus` simplification

The current 9-stop tab cycle is too many stops for the simplified card. Reduce to:
```rust
pub enum BuilderEditorFocus {
    Slots,        // left pane
    Hex,          // hex input field
    Red,          // R slider
    Green,        // G slider
    Blue,         // B slider
}
```

Depth is a status display (not an interactive focus target). Swatches expand on `+` as a mini inline interaction, not a persistent focus stop.

---

## 3. Preview Tab — Right Card

When the Preview tab is active, the right pane becomes a single bordered card showing the live theme preview. This is the content currently rendered by `preview_lines()` at the bottom of the editor, promoted to fill the entire card.

```
┌─ Live preview ───────────────────────────────┐
│                                              │
│  Metadata   Artwork   ReplayGain             │
│                                              │
│  General                                     │
│   Sample rate   44100 Hz                     │
│  ▸ resampler.rs   61 KB   Rust source        │
│                                              │
│  progress █████████░░░░░░  62%  OK   Esc     │
│                                              │
│  derived ██ ██ ██ auto (computed)             │
│                                              │
└──────────────────────────────────────────────┘
```

The preview should use `state.resolved_theme()` to render, showing how the theme looks with all current edits applied. The left pane stays on the role/accent list so you can see which slot you were editing.

---

## 4. Derived Tab

### Left pane: computed color list

When the Derived tab is active, the left pane swaps from roles/accents to the 29 computed colors. Each row shows:

```
  — surfaces —
○ surface              ██
○ border_dim           ██
  — text —
○ text_bright          ██
○ text_dim             ██
  — interaction —
○ hover_bg             ██
○ input_focused_bg     ██
...
  — progress dialog —
● progress_border      ██      (● = locked)
○ progress_bar_filled  ██
...
  — states —
○ error_dim            ██
```

**Provenance marks inside the builder:**
- `○` dim — auto (no `theme_lock` set)
- `●` single accent color — locked (has `theme_lock`)

The two-color amber/blue provenance distinction is NOT shown inside the builder because in the builder context, all locks write to `theme_lock`. There is only one layer to display, so one mark color suffices. The amber/blue split (amber = theme-authored lock, blue = user's personal lock) appears only at runtime (Apply dialog and any future runtime inspector) where both layers are visible and distinguishable.

**Dim subheaders** for scanability. The 29 derived colors group naturally:

| Group | Elements |
|-------|----------|
| — surfaces — | surface, border_dim |
| — text — | text_bright, text_dim |
| — interaction — | hover_bg, input_focused_bg, input_unfocused_bg, input_disabled_bg, dropdown_bg |
| — pills — | pill_active_bg, pill_active_fg, pill_dim_bg, pill_preset_bg, pill_preset_fg |
| — progress dialog — | progress_dialog_bg, progress_dialog_border, progress_dialog_text, progress_dialog_title, progress_dialog_label, progress_dialog_current_file, progress_dialog_dim, progress_dialog_bar_filled, progress_dialog_bar_unfilled, progress_dialog_percent, progress_dialog_button_bg, progress_dialog_button_fg, progress_dialog_abort_bg, progress_dialog_abort_fg |
| — states — | error_dim |

These subheaders are non-selectable rows rendered in dim text. They cost a few rows but turn a 29-item scroll into something scannable. Add a `group` field to `DerivedElementSpec` in `theme.rs` to drive this.

**Scrolling:** The pane holds ~16 visible rows. With 29 elements plus 6 subheaders the list is ~35 rows. Standard scrolling with a `[N/29]` element counter at the bottom (count selectable elements, not subheader rows). Subheader rows are skipped by cursor navigation — pressing `↓` on the last element of a group jumps to the first element of the next group.

**Legend** at the bottom of the list: `○ auto  ● locked`

### Right pane: derivation editor card

The same color editor from the Edit tab, wrapped in derivation context:

```
┌─ progress_dialog_border ─────────────────────┐
│                                              │
│   from = info accent  ██ #7AA2F7             │
│   ○ Auto  ● Locked   space toggles           │
│                                              │
│   ▕██████████████▏  #3B4261                  │
│   Hex  [ #3B4261 ]                           │
│   R [████░░░░░░░░░░░░░░░░] 59               │
│   G [████░░░░░░░░░░░░░░░░] 66               │
│   B [██████░░░░░░░░░░░░░░] 97               │
│                                              │
│   Pinned — ignores info accent edits.        │
│   used by  progress + conflict dialog border │
└──────────────────────────────────────────────┘
```

### Auto vs Locked states

**Auto state** (no lock set):
- Toggle shows `● Auto  ○ Lock`
- Hex field is read-only, showing the computed value in dim
- RGB sliders are greyed (uniform dim fill, no channel colors)
- Swatch shows the auto-computed value
- Consequence line: `Computed — tracks the info accent.`
- Prompt: `space to lock & edit`
- Pressing `Space` promotes to Locked and moves focus into the editor. The editor is not focusable while auto — space is the only entry point.

**Locked state** (lock set):
- Toggle shows `○ Auto  ● Locked`
- Hex field is active/editable
- RGB sliders have channel colors and are draggable
- Swatch shows the pinned value
- Consequence line: `Pinned — ignores info accent edits.`
- Prompt: `space releases to auto`

**When both theme_lock and user_lock are set (runtime context only, not in builder):**
Show a sub-line: `theme also pins this → #XXXXXX · release returns here` so the user knows release reveals the layer below, not the formula.

### Footer adaptation

When the Derived tab is active, the footer's third chip changes:
- Edit/Preview: `^s Save  a Apply  … more  Esc Cancel`
- Derived: `^s Save  a Apply  space Lock/Auto  Esc Cancel`

---

## 5. Derived Color Lock Model

### Data model: two independent optional slots with precedence

**This is the most important architectural point.** Each derived element has two independent nullable lock slots:

```rust
// In ThemePaletteDraft (theme-authored locks, saved to .theme file):
pub derived_locks: BTreeMap<String, Color>,   // already exists

// In ThemeOverrides (user's personal runtime layer):
pub overrides: BTreeMap<String, Color>,       // already exists
```

These are NOT mutually exclusive. Both can be populated for the same key simultaneously.

### Resolution precedence (top-down)

```
user_lock → theme_lock → formula
```

The resolved value is:
1. `user_overrides.overrides.get(key)` if present, else
2. `palette.derived_locks.get(key)` if present, else
3. Computed from the derivation formula

### Provenance mark derivation

The `●`/`○` mark is derived from which slots are populated, not stored:

| `theme_lock` | `user_lock` | Mark | Color (runtime) | Color (builder) |
|---|---|---|---|---|
| None | None | `○` | dim | dim |
| Some | None | `●` | amber | theme's lock color |
| None | Some | `●` | blue | n/a in builder |
| Some | Some | `●` | blue (user wins) | n/a in builder |

### Space toggle behavior (builder context)

In the builder, space always writes/clears `theme_lock` (because the builder is authoring a theme file):

| Before | Space does | After |
|---|---|---|
| `○ auto` (no theme_lock) | Set `theme_lock = current_computed_value` | `● locked` |
| `● locked` (has theme_lock) | Clear `theme_lock` | `○ auto` |

### Space toggle behavior (runtime/apply context)

At runtime (if a derived inspector is ever added), space writes/clears `user_lock`:

| `theme_lock` | `user_lock` | Mark | Space does | Result |
|---|---|---|---|---|
| None | None | `○ auto` | Set `user_lock = computed` | `● you` |
| Some | None | `● theme` | Set `user_lock = theme_lock_value` | `● you` (amber underneath) |
| None | Some | `● you` | Clear `user_lock` | `○ auto` |
| Some | Some | `● you` | Clear `user_lock` | `● theme` (amber resurfaces) |

**Key insight for row 4:** Release means "reveal the layer below," not "go to auto." Releasing a user lock that sits over a theme lock falls back to the theme's pin, not to the formula.

**Key insight for row 2:** Lock seeds from the resolved value, not the formula. Overriding a theme lock starts you at the theme's pinned color, because that's the value you're currently looking at.

### Apply dialog integration

The Apply dialog's two switches map cleanly to the two slots:

- **"Honor theme locks" / "Re-derive for my terminal"** → whether `theme_lock` participates in resolution
- **"Keep mine" / "Use theme as authored"** → whether `user_lock` participates in resolution

---

## 6. Gallery Overlay

### Entry points
- `p` key from the builder header
- "Browse all" button from the Config Appearance section

### Behavior
A floating overlay on top of the two-pane layout (not a tab). Opens, you pick or Esc, you're back exactly where you were.

### Layout

```
╔═ Themes ═════════════════════════════════════════════════════════════════════╗
║ Mode ● Dark  ○ Light      24 built-in · 0 custom      / filter             ║
╟──────────────────────────────────────────────────────────────────────────────╢
║ ▸ Tokyo Night        ████████████████████                                   ║
║   Gruvbox material   ████████████████████                                   ║
║   Catppuccin Mocha   ████████████████████                                   ║
║   Rosé Pine          ████████████████████                                   ║
║   Kanagawa           ████████████████████                                   ║
║   Everforest         ████████████████████                                   ║
║   Dracula            ████████████████████                                   ║
║   Nord               ████████████████████                                   ║
║   Solarized Dark     ████████████████████                                   ║
║   One Dark           ████████████████████                                   ║
║   Monokai Pro        ████████████████████                                   ║
║   Oxocarbon          ████████████████████                                   ║
╟──────────────────────────────────────────────────────────────────────────────╢
║ ↑↓←→ move   Enter apply   / filter   Esc close                             ║
╚══════════════════════════════════════════════════════════════════════════════╝
```

### Key behaviors

1. **Shows families, not individual entries.** The gallery groups themes into families. The mockup shows 12 families as a design target, but the current codebase has 6 built-in palettes (all dark-mode only) — the gallery should work with whatever `theme_choices()` returns. The `● Dark / ○ Light` toggle in the header reskins every ribbon in place. Flipping mode + picking a card is how you reach the light variant. If a family has no light variant, it still appears when Light is selected (showing its only available mode).

2. **Palette ribbon** — each card shows the theme name + a row of 10 accent color swatches (`██` per color). This is the recognition affordance — you identify a theme by its colors faster than by name.

3. **Active theme marked** with `▸` and selection highlight background.

4. **`/` filter** — typing after `/` filters the list by name. For when the library grows.

5. **Custom themes appear here** flagged as custom, once they've been saved.

6. **Two modes** (same as current):
   - From the builder (`preset_applies_on_select = false`): Enter loads the selected theme as an editable draft (forks built-ins into `{slug}-custom`)
   - From Config (`preset_applies_on_select = true`): Enter applies the theme directly

### Family grouping model

The current `theme_choices()` returns individual entries (one per slug, each with a `dark: bool`). The gallery shows **families** — a family is a set of themes that share the same base name but differ by mode (dark/light). Grouping is done at the UI level by stripping a trailing `-light` or `-dark` suffix (or by matching a shared `family` prefix if the naming convention isn't consistent). When the dark/light toggle flips, the gallery swaps each family's displayed ribbon to the other variant's accents. If a family has only one mode, it appears regardless of the toggle.

Custom themes appear in the family list as separate entries (they won't have a dark/light sibling unless the user creates one). Flag them with a `custom` badge.

### Implementation notes

The current `draw_preset_dropdown` is close but needs:
- Family grouping and ribbon layout instead of the current text-heavy individual rows
- Dark/Light mode toggle in the header that reskins ribbons in place
- `/` filter input state (a `TextInputState` field on `ThemeBuilderState`)
- Two-column grid if terminal is wide enough (the mockup shows 2 columns at 80 chars)

---

## 7. Context Menu (… more)

### Trigger
Click the `… more` footer chip, or press `.` to open the popup. The `.` key is chosen because it's mnemonic for `…` (ellipsis) and doesn't conflict with any other builder binding. The item-level shortcut keys (`r`, `d`, `x`, `e`, `i`) are active only while the menu is open.

### Layout

```
┌─ More ──────────────────┐
│ r  Revert               │  ← selection highlight on first item
│ d  Duplicate            │
│ x  Delete               │  ← only shown for custom themes
├─────────────────────────┤
│ e  Export .theme        │
│ i  Import .theme        │
└─────────────────────────┘
```

### Behaviors

1. **It's a real popup menu** with labels and keybindings, not a blind cycle. Anchored above the `… more` footer chip (bottom-left of the popup aligns with the chip's left edge, popup grows upward). Arrow keys navigate, Enter selects, Esc dismisses.

2. **Derived is NOT in this menu.** It's a tab. No second door.

3. **Delete is context-dependent.** Listed only when the loaded theme is a custom (deletable) one. When a built-in preset is loaded, Delete is absent (not greyed — absent), since built-ins can't be deleted.

4. **Duplicate** is always present. It's the bridge: Browse → Duplicate → edit in the three tabs → Save → shows up in gallery as custom. This is how a read-only built-in becomes editable.

5. **Grouped by divider:** theme lifecycle (Revert, Duplicate, Delete) above the `├───┤` divider, file operations (Export, Import) below.

6. **Export/Import** handle theme files to/from `~/.config/tonepoet/themes/`. The current on-disk format is `.toml` (via `ThemeFile`); the menu labels say `.theme` as a user-facing abstraction — the implementing model should use whichever extension the save/load code already uses (currently `.toml`). The file path lives in the Export/Import dialogs, not in the menu.

7. **Revert** restores from the last saved state on disk (same as current `revert_from_disk()`). Only meaningful for saved custom themes. Absent from the menu when the current theme is a built-in or an unsaved new draft (no saved state to revert to). Present but standard for saved customs with unsaved edits (`dirty == true`).

### State

```rust
pub struct MoreMenuState {
    pub cursor: usize,
    pub items: Vec<MoreMenuItem>,
}

pub enum MoreMenuItem {
    Revert,
    Duplicate,
    Delete,      // conditionally included
    Separator,
    Export,
    Import,
}
```

The menu rebuilds its item list each time it opens, based on the current theme's source:
- **Built-in** or **unsaved new draft** (`ThemeDraftSource::BuiltIn | NewCustom`): omit Revert and Delete
- **Saved custom** (`ThemeDraftSource::Custom`): include Revert and Delete
- Duplicate, Separator, Export, Import are always present

---

## 8. Footer

### Current (8 chips)
```
^s Save  a Apply  d Derived  m Mode  r Revert  x Delete  + Save swatch  Esc Cancel
```

### Target (4 chips, context-adaptive)

**Edit / Preview tabs:**
```
^s Save   a Apply   … more   Esc Cancel
```

**Derived tab:**
```
^s Save   a Apply   space Lock/Auto   Esc Cancel
```

### What moved where

| Current chip | New location |
|---|---|
| `^s Save` | Footer (stays) |
| `a Apply` | Footer (stays) |
| `d Derived` | Tab strip (Edit / Preview / Derived) |
| `m Mode` | Header toggle (`● Dark / ○ Light`) |
| `r Revert` | `… more` menu |
| `x Delete` | `… more` menu (custom themes only) |
| `+ Save swatch` | Inline in swatches row |
| `Esc Cancel` | Footer (stays) |

---

## 9. Header Row

```
Tokyo Night Custom    Mode ● Dark    Depth True Color       p presets
```

- **Theme name** — left-aligned, styled as title
- **Mode toggle** — `● Dark / ○ Light`, clickable, or `m` key. Moved here from the footer.
- **Depth indicator** — `True Color` displayed in the header. Clickable to cycle depth, or press `D` (shift-d)
- **`p presets`** — right-aligned shortcut hint, opens the gallery overlay

---

## 10. Keybinding Summary

### Global (all tabs, no overlay)

| Key | Action |
|---|---|
| `Esc` | Close builder |
| `^s` | Save theme |
| `a` | Open Apply dialog |
| `p` | Open gallery overlay |
| `.` | Open … more menu |
| Tab strip click | Switch tab (mouse only — no keyboard shortcut for tab switching, because digit/letter keys conflict with hex input. The tab strip is always visible and clickable) |

### Edit tab

| Key | Action |
|---|---|
| `Tab` / `Shift+Tab` | Cycle focus: Slots → Hex → R → G → B → Slots |
| `↑/↓` or `j/k` | Navigate slot list (when Slots focused) |
| `←/→` | Adjust slider (when slider focused) |
| Type | Edit hex field (when Hex focused) |
| `+` | Save current color as swatch |
| `m` | Toggle dark/light mode |
| `D` (shift-d) | Cycle depth: TrueColor → Xterm256 → Ansi16 |

### Preview tab

| Key | Action |
|---|---|
| `↑/↓` or `j/k` | Navigate slot list (same as Edit) |
| Selecting a slot updates the preview to highlight that role's usage |

### Derived tab

Focus in the Derived tab defaults to the left-pane list. When a derived color is locked, `Tab` moves focus into the right-pane editor (Hex → R → G → B → back to list). When a derived color is auto (unlocked), the editor is read-only and `Tab` stays on the list — there's nothing to focus in a read-only card. Pressing `Space` to lock also moves focus into the editor automatically.

| Key | Action |
|---|---|
| `↑/↓` or `j/k` | Navigate derived color list (when list focused) |
| `PageUp/PageDown` | Page through derived list |
| `Space` | Toggle lock on selected derived color (lock also moves focus to editor) |
| `Tab` / `Shift+Tab` | Move between list and editor (only when locked) |
| Type | Edit hex field (when locked and Hex focused) |
| `←/→` | Adjust slider (when locked and slider focused) |

### Gallery overlay

| Key | Action |
|---|---|
| `↑/↓/←/→` | Navigate cards |
| `Enter` | Apply/load selected theme |
| `/` | Open filter input |
| `Esc` | Close overlay |
| `m` | Toggle Dark/Light mode |

### … more menu

| Key | Action |
|---|---|
| `↑/↓` | Navigate items |
| `Enter` | Execute selected item |
| `r` | Revert (direct key) |
| `d` | Duplicate (direct key) |
| `x` | Delete (direct key, custom only) |
| `e` | Export |
| `i` | Import |
| `Esc` | Close menu |

---

## 11. Migration Checklist

### Remove
- `ThemeBuilderView` enum (replace with `BuilderTab` + `BuilderOverlay`)
- `BuilderEditorFocus::Depth`, `SwatchName`, `SavedSwatches`, `RecentSwatches` (simplify focus cycle)
- `DerivedLockTarget` enum — the builder always writes `theme_lock`; the runtime always writes `user_lock`. No toggle needed.
- Global `d` key for opening Derived view — Derived is now a tab, freeing `d` for Duplicate in the `… more` menu.
- The 8-chip footer
- Preview content inlined at the bottom of `draw_editor`

### Add
- `BuilderTab` enum
- `BuilderOverlay` enum
- `MoreMenuState` struct and draw function
- Gallery filter input state (`TextInputState` field, active when `/` is pressed in the gallery)
- Gallery dark/light mode toggle (local to the gallery, changes which variant's ribbon is shown)
- Gallery family grouping logic
- Derived list group subheaders (add `group: &'static str` to `DerivedElementSpec`)
- Framed swatch rendering helper (`▕██████████████▏` with border-color frame characters)
- Read-only editor state for auto-derived colors (greyed sliders, non-editable hex)
- Mouse click targets for the Edit/Preview/Derived tab strip (no keyboard shortcut — digit keys conflict with hex input)
- `D` (shift-d) keybinding for depth cycling
- `.` keybinding for opening the `… more` menu

### Preserve
- `ThemePaletteDraft` — unchanged
- `ThemeOverrides` — unchanged
- `ThemeApplyOptions` — unchanged
- `ApplyDialogState` — unchanged (the Apply dialog itself stays as-is)
- `DeleteConfirm` dialog — unchanged
- All theme file I/O (save/load/delete)
- The resolution cascade: user_lock → theme_lock → formula
- `preset_applies_on_select` for the standalone Config gallery mode
- Existing test **behaviors** (hex editing, swatch ops, derived locking) — but the tests themselves need mechanical adaptation for renamed/removed types; see section 13

### Behavioral invariants to maintain
1. Selecting a different role in the left pane reskins the right card and nothing else shifts
2. The gallery returns you exactly where you were on Esc
3. Space in Derived seeds from the resolved value (what you're looking at), not the formula
4. Release reveals the layer below, not necessarily auto
5. The builder writes `theme_lock` on space; runtime writes `user_lock`
6. Built-in themes fork to `{slug}-custom` on edit — never mutate the built-in

---

## 12. Derived Element Spec Grouping

Add a `group` field to `DerivedElementSpec` in `theme.rs`:

```rust
pub struct DerivedElementSpec {
    pub key: &'static str,
    pub label: &'static str,
    pub formula: &'static str,
    pub used_by: &'static str,
    pub group: &'static str,   // NEW
}
```

Update `DERIVED_SPECS`:

```rust
const DERIVED_SPECS: &[DerivedElementSpec] = &[
    DerivedElementSpec { key: "surface",       ..., group: "surfaces" },
    DerivedElementSpec { key: "border_dim",    ..., group: "surfaces" },
    DerivedElementSpec { key: "text_bright",   ..., group: "text" },
    DerivedElementSpec { key: "text_dim",      ..., group: "text" },
    DerivedElementSpec { key: "hover_bg",      ..., group: "interaction" },
    DerivedElementSpec { key: "input_focused_bg",   ..., group: "interaction" },
    DerivedElementSpec { key: "input_unfocused_bg",  ..., group: "interaction" },
    DerivedElementSpec { key: "input_disabled_bg",   ..., group: "interaction" },
    DerivedElementSpec { key: "dropdown_bg",   ..., group: "interaction" },
    DerivedElementSpec { key: "pill_active_bg", ..., group: "pills" },
    DerivedElementSpec { key: "pill_active_fg", ..., group: "pills" },
    DerivedElementSpec { key: "pill_dim_bg",    ..., group: "pills" },
    DerivedElementSpec { key: "pill_preset_bg", ..., group: "pills" },
    DerivedElementSpec { key: "pill_preset_fg", ..., group: "pills" },
    DerivedElementSpec { key: "progress_dialog_bg",      ..., group: "progress" },
    DerivedElementSpec { key: "progress_dialog_border",   ..., group: "progress" },
    // ... remaining progress_dialog_* entries, all group: "progress"
    DerivedElementSpec { key: "error_dim",     ..., group: "states" },
];
```

The derived list renderer inserts a dim `— {group} —` subheader line when the group changes between consecutive entries. These subheader rows are non-selectable and skipped by cursor navigation.

---

## 13. Test Expectations

### Existing tests to adapt
Tests in `theme_builder.rs::tests` test state mutations (hex editing, slot navigation, swatch operations, derived locking/releasing). The tested **behaviors** should be preserved, but several tests reference types that are renamed or removed and will need mechanical updates:
- `ThemeBuilderView::Derived` → `BuilderTab::Derived` (with `overlay: BuilderOverlay::None`)
- `ThemeBuilderView::Main` → `BuilderTab::Edit` (with `overlay: BuilderOverlay::None`)
- `DerivedLockTarget::UserOverride` / `DerivedLockTarget::ThemeAuthor` → removed; the builder always writes `theme_lock`, tests for user_lock behavior belong in runtime/apply tests
- `BuilderEditorFocus::SavedSwatches` / `RecentSwatches` / `Depth` / `SwatchName` → removed from enum; tests exercising swatch and depth behavior should be reworked to use the new interaction model (e.g., `+` key for swatch save rather than focus-cycling to swatch name field)
- The `derived_lock_can_target_author_or_user_layer` test (line 2007) tests the `DerivedLockTarget` toggle which is being removed — replace with a test that space in the Derived tab writes `theme_lock`

### New tests to add
- Tab switching: `BuilderTab::Edit` → `Preview` → `Derived` → `Edit` cycling
- Derived tab: space toggle writes `theme_lock`, not `user_lock`
- Derived tab: release with theme_lock underneath reveals theme lock (not auto)
- Derived tab: lock seeds from resolved value
- More menu: Delete item present for custom themes, absent for built-ins
- More menu: Duplicate creates `{slug}-custom` fork
- Gallery: dark/light toggle changes mode without changing cursor position
- Gallery: filter narrows visible entries
- Editor focus cycle is 5 stops, not 9

---

## 14. Summary of the Design Philosophy

1. **One task on screen at a time.** The right card shows exactly one thing — the editor, the preview, or the derivation context. Never all three fighting for space.

2. **Your place never moves.** Selecting a different role/accent reskins the right card. The left list is stable. Tab switches are the only structural changes.

3. **The same editor everywhere.** The Edit tab and the Derived tab use the same hex-field-plus-RGB-sliders control. In Derived it's wrapped in derivation context and can be read-only (auto) or live (locked). One control, two states.

4. **Navigation is overlays, editing is tabs.** Picking a different theme (gallery) and accessing occasional actions (… more) are transient overlays. Working on the theme (Edit/Preview/Derived) is the tab strip.

5. **Two lock layers, one authoring gesture.** The data model keeps `theme_lock` and `user_lock` as independent slots with precedence. The builder writes `theme_lock`. The runtime writes `user_lock`. The UI shows one toggle per context. The Apply dialog maps its two switches to the two layers.
