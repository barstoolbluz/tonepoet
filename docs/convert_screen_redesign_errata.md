# Errata: Convert screen redesign — first implementation attempt

This document records issues from the first reasoning model implementation attempt. Include this alongside the brief so the next attempt avoids the same mistakes.

## Issue 1: Non-atomic apply script

The bundle used an `apply_bundle.py` that copies replacement files first, then applies patches. If patching fails, it returns an error but does **not roll back** the copied files. This leaves the repo in a half-modified state: `convert_screen.rs` and `draw_metadata.rs` are replaced while `app.rs`, `button_map.rs`, `command.rs`, `keybindings.rs`, and others remain unchanged. The binary won't compile.

**Requirement:** The implementation must be atomic. Either all files are successfully written/patched, or none are. If using a script, it must snapshot the original files before modification and restore them on any failure. Alternatively, produce complete replacement files for all modified sources — no patches.

## Issue 2: Missing file changes

The brief specifies changes to **11 files**:

| File | Required change |
|------|----------------|
| `src/tui/app.rs` | `ConvertLayout` enum, `layout` + `pane_title_last_click` fields on `ConvertState`, `is_collapsed()` / `is_maximized()` / `toggle_maximize()` methods, `file_scroll` on `MetadataState` |
| `src/tui/convert_screen.rs` | `pane_constraint()` closure, conditional draw dispatch, conditional button registration, `register_maximize_toggle()`, thread `source_mode` to metadata draw |
| `src/tui/draw_source.rs` | New `draw_source_title_bar()`, top border `╒`/`╕` corners + `═` fill + `◻`/`◼` indicator (new `maximized: bool` param) |
| `src/tui/draw_metadata.rs` | New `draw_metadata_title_bar()`, same top border changes, mode-dependent file-list rendering, new `source_mode` + `maximized` params |
| `src/tui/draw_output.rs` | New `draw_format_title_bar()`, same top border changes, fill extra rows when maximized |
| `src/tui/draw_output_options.rs` | New `draw_output_options_title_bar()`, same top border changes, fill extra rows when maximized |
| `src/tui/button_map.rs` | `MaximizeToggle(ConvertFocus)` + `MetadataFileRow(usize)` variants, `screen()` match update |
| `src/tui/command.rs` | `Command::Maximize` (`:max`) + `Command::Advanced` (`:adv`) with compound logic |
| `src/tui/keybindings.rs` | Remove bare `a` handler, add `is_collapsed()` guards, modify `AdvancedToggle` handler, add double-click detection to `Pane` handler, new metadata Up/Down/Enter handlers, new `MaximizeToggle` + `MetadataFileRow` mouse handlers |
| `src/tui/context_menu.rs` | `ContextAction::TogglePaneMaximize`, per-pane entries in `build_convert_menu()`, dispatch handler |
| `src/tui/draw_footer.rs` | `:max` hint at priority 2 |

The first attempt only produced full replacements for 2 of these files and a new support module. The other 9 were handled via patch fragments that didn't apply.

**Requirement:** Produce complete, compilable replacement files for **every** file that needs changes. Do not use patches — they are fragile and context-dependent. Each replacement file must be the full file content, ready to overwrite the original.

## Issue 3: Architectural shortcut — centralized title bar rendering

The first attempt avoided creating `draw_*_title_bar()` functions in each draw file. Instead, it rendered collapsed title bars from `convert_screen.rs` using a shared helper in a new `convert_redesign_support.rs` module.

This deviates from the brief's file-level design. The brief specifies title bar functions in each draw file because:

1. **Each pane's title bar has pane-specific content** — the title text differs (`" source "`, `" metadata "`, `" format "`, `" output options "`), and future `advanced_open` rendering will add pane-specific advanced content.
2. **The two-pass rendering pattern** expects each draw file to handle its own rendering. Button registration for each pane's title bar (`MaximizeToggle`, `AdvancedToggle`) should be co-located with that pane's registration logic in `register_buttons()`.
3. **No new modules.** The brief's "What NOT to change" and file-level change summary don't include a new `convert_redesign_support.rs`. Don't create new files unless the brief calls for them.

**Requirement:** Add `draw_*_title_bar()` functions to `draw_source.rs`, `draw_metadata.rs`, `draw_output.rs`, and `draw_output_options.rs` as specified. Do not create a centralized helper module.

## Issue 4: Top border character changes

The first attempt may not have applied the correct box-drawing characters for title bars. The brief specifies:

- **Corners:** `╒` (U+2552) left, `╕` (U+2555) right — NOT `┌`/`┐`
- **Fill:** `═` (U+2550, double horizontal) — NOT `─`
- **Indicator:** `◻` (U+25FB) default/collapsed, `◼` (U+25FC) maximized — NOT `▶`/`▼`

The existing `┌`/`┐` corners and `─` fill in the current draw files must be changed to `╒`/`╕` and `═` in the top border line. The bottom border (`└───┘`) remains unchanged.

## Summary of requirements for next attempt

1. **Produce complete replacement files** for all 11 modified source files. No patches, no partial files.
2. **No new modules** — don't create `convert_redesign_support.rs` or similar.
3. **Title bar functions** go in each draw file, not centralized.
4. **Atomic delivery** — all files must be present and consistent. The binary must compile after replacing all files simultaneously.
5. **Use the correct Unicode characters** — `╒`/`╕` corners, `═` fill, `◻`/`◼` indicators.
6. **The implementation must compile and run** with `cargo build` inside `nix develop`. Read the existing file contents carefully before producing replacements.
