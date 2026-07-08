# UX Batch Fix Round 2: 4 Issues

## 1. Conditional template blocks drop braces even when %TITLE_EXTRA% resolves

**Observed:** `The Allman Brothers Band - At Fillmore East (1971) [FLAC] MFSL` — the `MFSL` is present (correctly extracted as %TITLE_EXTRA%) but the `{` and `}` delimiters were stripped instead of being replaced with nothing. The expected output when %TITLE_EXTRA% resolves is to keep the content and drop only the braces, e.g., `{MFSL}` in the template becomes `MFSL` in output.

**The bug:** The `{...}` conditional block logic is stripping the braces unconditionally instead of only when a variable inside is empty. When ALL variables inside `{...}` resolve to non-empty values, the block content should be emitted (without the braces). When ANY variable is empty, the entire block (including content and braces) should be dropped.

Check the template rendering in `stages.rs` — the conditional block expansion is likely dropping the block entirely or stripping braces at the wrong stage.

## 2. Disc subfolders not created despite option being ticked

**Observed:** The `disc dirs` option is ticked in the UI but converted files are not grouped into `Disc 01`, `Disc 02` subfolders for multi-disc sets.

This could be:
- The `create_disc_subfolders` field not flowing from `ConversionOptions` through to the publish/naming stage
- The publish stage not reading the field
- The disc number detection not firing

Trace `create_disc_subfolders` from `ConversionOptions` through `PipelineRequest`/`PipelineSettings` to wherever the output directory structure is built during publish.

## 3. Force encode and disc dirs toggle click targets are offset

**Observed:** In the Output pane's below-the-fold section:
```
force enc    off    on
disc dirs    off    on
write log    yes    no
```

Clicking `off`/`on` for `force enc` does nothing. Clicking `off`/`on` for `disc dirs` does nothing. But clicking `off`/`on` for `force enc` actually toggles `write log` — suggesting the mouse hit targets are offset by one or two rows.

This is a button registration / hit-box issue in the draw code. The `force enc` and `disc dirs` rows were added but the button map coordinates for these rows (and the rows below them) weren't updated to account for the new rows.

Check `draw_output_options.rs` — the `record_rect` / button registration calls for the toggle switches. The Y coordinates are likely off because `ForceEncode` and `DiscSubfolders` rows were inserted but the button positions for `WriteLog` (and any rows below) weren't shifted down.

## 4. Ctrl+Shift+Tab doesn't reverse-cycle metadata overlay tabs

**Observed:** In the metadata overlay, both `Shift+Tab` and `Ctrl+Shift+Tab` cycle forward (left to right) across the Metadata, Details, ReplayGain, and Artwork tabs. `Ctrl+Shift+Tab` should cycle in reverse (right to left).

The keybinding handler for the metadata overlay is likely not distinguishing `Ctrl+Shift+Tab` from `Shift+Tab`. Check the key event matching — `Ctrl+Shift+Tab` would have `modifiers` containing both `KeyModifiers::CONTROL` and `KeyModifiers::SHIFT`. The handler probably matches on `KeyModifiers::SHIFT` alone without checking that CONTROL is absent, so both combos hit the same forward-cycle branch.

Note: The user confirmed `Ctrl+Shift+Tab` works correctly in the convert view, so the pattern exists elsewhere — check how the convert view handles it and replicate.

## Files

- `src/convert/pipeline/stages.rs` — template rendering, conditional blocks (#1), disc subfolder logic (#2)
- `src/tui/draw_output_options.rs` — toggle button hit targets (#3)
- `src/tui/keybindings.rs` — metadata overlay tab cycling keybindings (#4)
- `src/tui/convert_actions.rs` — `create_disc_subfolders` field wiring (#2)
- `src/tui/app.rs` — output options state, disc subfolders field (#2, #3)
