# Theme Builder: Apply Does Not Persist Color Changes

## The Problem (Empirically Observed)

The following workflow does not work:

1. Open the theme builder (press Edit from Config > Appearance)
2. Select a **role** in the left pane (e.g., `panel_bg`, `title`, `tab_active`)
3. Click an **accent color swatch** in the accent grid below the roles
4. Click **Apply** in the footer
5. In the Apply dialog, choose either "Honor theme locks" or "Your overrides" — neither matters
6. Click **Apply** in the dialog

**Expected:** The role's color changes to the accent's color. The TUI redraws with the new theme.

**Actual:** Nothing happens. The color is not applied. The theme does not change.

This was tested after the `activate_slot` / `assign_accent_to_role` patch was applied, so the accent-to-role assignment code is present. The issue is somewhere in the pipeline between the assignment and the final theme resolution/application.

## What We Know

- The accent-to-role assignment code (`activate_slot`, `assign_accent_to_role`) was added in the most recent patch and compiles cleanly
- The status bar may or may not show "Assigned accent NN to title" — we haven't verified whether the assignment itself fires
- `apply_theme_builder_state` in `keybindings.rs:305` calls `state.resolved_theme()` which reads `state.palette`
- The Apply dialog's two switches ("Theme locked colors" / "Your overrides") control derived color layers, not authored role colors — so they shouldn't affect this workflow at all
- The previous diagnosis identified that clicking an accent used to just call `set_selected_slot()` (navigation, not assignment). The new code routes through `activate_slot()` which should call `assign_accent_to_role()` → `set_selected_color()` → `palette.set_color_at_slot()`. But empirically the change does not take effect.

## What We Don't Know

- Whether `activate_slot` is actually being called (vs. a different code path handling the click)
- Whether `set_selected_color` is actually mutating the palette
- Whether `resolved_theme()` is reading the mutated palette or a stale copy
- Whether `apply_theme_builder_state` is applying the resolved theme correctly to `app.theme`
- Whether the theme is applied but the UI doesn't redraw
- Whether the Apply dialog's resolution options are somehow stripping the change back out

## Your Task

**Diagnose the root cause and fix it.** Trace the complete path from mouse click through to `app.theme` assignment. Find where the mutation is lost and fix it. The source files in this bundle are the current state of the code.

Don't trust any prior diagnosis — start fresh from the empirical observation: "the color does not change after Apply."
