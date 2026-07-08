# Theme Builder: Accent-to-Role Color Assignment

## The Bug

When a user selects a role (e.g., "title") and then clicks an accent color swatch in the grid, they expect the accent's color to be assigned to the role. Instead, the click just selects the accent as the new editing target, and the role retains its original color. Pressing Apply afterwards has no effect because no palette mutation occurred.

## Root Cause

The accent grid and the role list share the same click handler. Both are `TuiButton::ThemeBuilderSlot(slot)` targets that call `state.set_selected_slot(slot)` (`theme_builder.rs:1279-1282`):

```rust
Some(TuiButton::ThemeBuilderSlot(slot)) if matches!(state.tab, BuilderTab::Edit | BuilderTab::Preview) => {
    state.editor_focus = BuilderEditorFocus::Slots;
    state.set_selected_slot(slot);  // replaces the role selection with the accent
}
```

`set_selected_slot` (`theme_builder.rs:415-423`) simply updates `self.selected_slot` and syncs the hex/RGB display. It never copies any color anywhere. The accent grid is a symmetric slot selector, not an assignment picker.

## Why the Apply Dialog Doesn't Help

The Apply dialog's two switches ("Honor theme locks" / "Your overrides") control the **derived color** resolution layer — which `theme_lock` and `user_lock` slots participate. They have nothing to do with authored role colors.

When the user presses Apply, `apply_theme_builder_state` (`keybindings.rs:299-324`) calls `state.resolved_theme()`, which reads `state.palette` — the draft palette. Since clicking the accent never mutated the palette's role colors, the resolved theme is identical to what was loaded. The Apply is a no-op.

## The Design Problem

The accent grid currently serves one purpose: selecting an accent slot for direct editing (change accent #7's color via hex/RGB). But users also expect a second purpose: using an accent as a color source to assign to a role.

The saved-swatch system already has this "pick to assign" interaction — `apply_saved_swatch` (`theme_builder.rs:626-643`) copies a swatch's color into the currently selected slot. The accent grid has no equivalent.

## What Needs to Change

When a role is selected and the user clicks an accent swatch, the accent's color should be copied to the role. The conceptual model is: **roles are assignment targets, accents are color sources** (among other things).

### Behavioral specification

1. **Role selected + click accent** → copy `palette.accents[N]` to the selected role, mark dirty, keep the role selected so the user can see the result in the editor card. The accent remains unmodified.

2. **Accent selected + click accent** → switch to that accent (current behavior, for direct editing).

3. **Role selected + click different role** → switch to that role (current behavior).

4. **Accent selected + click role** → switch to that role (current behavior).

The asymmetry is: accents act as color sources only when a role is the active editing target. When an accent is the active target, clicking another accent just navigates.

### Keyboard equivalent

The same interaction should work via keyboard: when a role is selected and the user navigates down past the last role into the accent grid, pressing Enter on an accent should assign its color to the most recently selected role.

Alternatively, a simpler keyboard model: a dedicated key (e.g., `Enter` or `y`) while an accent is highlighted and a role was previously selected to "apply accent color to last role."

### Visual feedback

When a role is selected and the user hovers/clicks an accent, the editor card should preview the accent's color applied to the role (framed swatch updates, hex updates) so the change is visible before committing. If the user navigates away without confirming, revert.

Or, simpler: just apply immediately on click (like saved swatches do), since the user can always undo by editing the hex field or reverting.

## Files to Modify

- `src/tui/theme_builder.rs` — the mouse handler for `ThemeBuilderSlot` clicks needs to detect "role was selected, accent was clicked" and copy the color. Also the keyboard handler for slot navigation.
- No changes needed to `theme.rs`, `keybindings.rs`, or `button_map.rs`.

## Current Code References

- `set_selected_slot()` — `theme_builder.rs:415-423`
- `set_selected_color()` — `theme_builder.rs:398-406` (this is the mutation method that applies a color to the current slot, marks dirty, pushes to recent)
- `apply_saved_swatch()` — `theme_builder.rs:626-643` (reference implementation of "pick a color source, assign to selected slot")
- Mouse handler — `theme_builder.rs:1279-1282`
- Keyboard slot navigation — `theme_builder.rs:914-920` (inside `BuilderEditorFocus::Slots` match arm)
- `color_at_slot()` — `theme.rs` (reads color from a BuilderSlot)

## Suggested Implementation

In the mouse handler, when `ThemeBuilderSlot(accent)` is clicked and the current `state.selected_slot` is a `Role`:

```rust
Some(TuiButton::ThemeBuilderSlot(slot)) if matches!(state.tab, BuilderTab::Edit | BuilderTab::Preview) => {
    match (state.selected_slot, slot) {
        (BuilderSlot::Role(_), BuilderSlot::Accent(idx)) => {
            // Accent clicked while role is selected: assign accent color to role
            let accent_color = state.palette.accents[idx];
            state.set_selected_color(accent_color);
            // Role stays selected — user sees the result in the editor card
        }
        _ => {
            // All other cases: just navigate
            state.editor_focus = BuilderEditorFocus::Slots;
            state.set_selected_slot(slot);
        }
    }
}
```

This mirrors the saved-swatch pattern: `apply_saved_swatch` reads the swatch color and calls `set_selected_color()`, which mutates the palette, marks dirty, pushes to recent history, and syncs the editor display.

## Test Cases

1. **Role selected, click accent** → role's color in the palette draft changes to the accent's color, `dirty` is true, `selected_slot` remains the role
2. **Accent selected, click accent** → `selected_slot` changes to the clicked accent, no palette mutation
3. **Role selected, click accent, then Apply** → the resolved theme reflects the new role color
4. **Role selected, click accent** → the previous role color appears in recent history
5. **Keyboard: role selected, navigate to accent, press Enter** → same as click behavior (if keyboard equivalent is implemented)

---

## IMPORTANT: Broader Audit Request

The theme builder has been developed through iterative UX design passes but has **never been tested empirically by a user running the application**. The accent-to-role assignment bug above was found during the first real hands-on session. There are likely other similar issues — places where the UI suggests an interaction that the code doesn't actually wire up, or where state mutations don't propagate correctly to the resolved theme.

**Before implementing the accent fix, audit the entire theme builder for similar classes of bugs.** Specifically:

1. **Click targets that navigate instead of acting.** The accent bug is an instance of this pattern. Are there other places where clicking something looks like it should apply/assign/toggle but actually just selects? Check every `TuiButton::ThemeBuilder*` handler in the mouse dispatch.

2. **State mutations that don't reach the palette draft.** The builder has multiple editing paths (hex input, RGB sliders, swatch assignment, derived locking). Trace each one and verify that the mutation actually lands in `state.palette` (for role/accent edits) or `state.palette.derived_locks` / `state.user_overrides` (for derived edits). Look for paths where `sync_hex_and_rgb_from_slot()` is called but `set_selected_color()` is not — that would mean the display updates but the palette doesn't.

3. **Apply resolves a stale palette.** `apply_theme_builder_state` calls `state.resolved_theme()` which reads `state.palette`. Are there editing paths where the user's changes live somewhere other than the palette (e.g., only in `hex_input.text` or `rgb_values` without having been flushed to the palette via `set_selected_color`)? If the user types a hex value but doesn't press Enter, does Apply capture it?

4. **Save without Apply.** `^s Save` calls `save_theme_builder_state` — does it also flush pending hex/RGB edits to the palette before serializing? Or can it save a stale draft?

5. **Dirty flag accuracy.** Is `dirty` set for every path that mutates the palette? Are there mutations that skip it? Conversely, are there paths that set `dirty` without actually changing anything (false positives that would cause "unsaved changes" warnings on clean state)?

6. **Tab switching drops edits.** When switching between Edit/Preview/Derived tabs, is any in-progress editing state (e.g., partially typed hex value) flushed or discarded? What's the expected behavior?

7. **Derived tab lock/unlock.** The space-toggle for derived colors was redesigned (builder writes `theme_lock`). Verify that space actually calls the right mutation method and that the lock state is visible in the resolved theme after Apply.

8. **Gallery overlay interaction.** After selecting a preset from the gallery, does the builder state reset correctly? Does the dirty flag reset? Does the left-pane selection reset to Role(0)?

9. **More menu actions.** Trace Revert, Duplicate, Delete, Export, Import — do they all work end-to-end? Revert should reload from disk. Duplicate should create a `{slug}-custom` fork. Delete should remove the file and update the gallery. Export/Import may be stubs — if so, note that.

10. **Esc from overlays.** Pressing Esc on the gallery, more menu, apply dialog, and delete confirm should return to the builder with no state changes. Verify nothing leaks.

Report each issue found with the same level of detail as the accent-assignment bug above: what the user expects, what actually happens, the root cause in the code, and a suggested fix. Fix all issues found in a single coherent patch alongside the accent-assignment fix.
