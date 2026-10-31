# TonePoet Properties dialog: implementation and continuation handoff

Date: October 30, 2026

## Baseline and deliverables

Baseline: `TonePoet_BROWSE_INFO_VISUAL_SOURCE_2026-10-30.tar.gz`  
Baseline SHA-256: `a073d8ff481248b27f0c6d9db2a6dc1012141c78dda81b169e422723e2dab202`

Deliverables:

- `TonePoet_PROPERTIES_DIALOG_SOURCE_2026-10-30.tar.gz`: full updated source, this handoff, and the supplied Properties-dialog PNG.
- `TonePoet_PROPERTIES_DIALOG_PATCH_2026-10-30.tar.gz`: seven verified source replacements, human-readable `changes.diff`, `manifest.json`, `apply.py`, this handoff, and the PNG.
- `TonePoet_PROPERTIES_DIALOG_HANDOFF_2026-10-30.md`: this standalone continuation document.

Start from **the visual-source archive above** or use the complete updated source. Do not apply the patch to an older snapshot or assume it works on a separately modified checkout.

## Implemented behavior

### Dialog

The read-only filesystem Properties dialog matches the new mockup's hierarchy: centered amber `{name} Properties` border title, filename and muted type line, subdued separators, aligned Location/Size/Contains/Created/Modified fields, Size-row Recalculate, and lower-left Edit Metadata... / lower-right OK buttons. Labels and colors use the active TUI theme, not fixed RGB literals.

The folder controller retains the shared directory-accounting service and independent subscriber token. Existing complete counts are labeled as cached; any stale/failed/cancelled state is visible without misrepresenting partial scan numbers as a final total. Recalculate requests the existing forced scan; Cancel explicitly cancels it. While cancellation is in flight, the control shows 'Cancelling...' and does not offer a no-op Recalculate. Closing detaches the modal subscriber without terminating scans owned by other consumers.

The renderer uses no filesystem or SQLite access. Directory size, count, and timestamps come from the existing accounting view. File Properties obtains size and mtime from the existing Browse entry at dialog opening and **does not** start a recursive scan. Unavailable timestamps remain unavailable. Long paths can be horizontally scrolled, and short terminals use a scrollable content viewport with footer controls fixed in place.

### Interactions

- **Ctrl+P** in filesystem Browse opens Properties for the selected folder or file. Synthetic archive contents remain excluded. Ctrl+Shift+P continues to mean Paste; Ctrl+P in text editor surfaces retains those surfaces' own semantics.
- A filesystem **right-click -> Properties** menu action opens Properties with the clicked entry's exact path, including ordinary files, audio files, disc images, and folders. Tree-pane Properties already routed to the filesystem modal and continues to do so.
- **Alt+P** in Browse retains the existing Metadata Editor route. The **Edit Metadata...** button inside Properties invokes the same backend and admission policy, but explicitly targets the Properties path so a different marked selection or Tree cursor cannot redirect it. If that item cannot be edited, the pre-existing metadata-editor policy reports the reason; this change does not extend metadata support.
- **OK**, **Esc**, and clicks on OK close Properties. **R** / the Size-row Recalculate control start the existing forced scan; **C** / Cancel cancels while running. **E**, **Alt+P**, and the Edit Metadata... button open metadata editing. **Tab** and **Shift+Tab** traverse actionable controls. Arrow keys scroll paths or content as appropriate.
- Mouse actions use the *rendered* button-map bounds. Properties consumes all other pointer events to prevent click-through into Browse.
- Archive-internal context actions remain on the original archive-aware path; no filesystem Properties is offered for synthetic archive paths.

### Source footprint

Modified source files only:

1. `src/tui/directory_properties.rs` (controller, keyboard, mouse, target snapshot)
2. `src/tui/draw_directory_properties.rs` (dialog visual layout, accounting presentation, TestBackend regressions)
3. `src/tui/draw_overlays.rs` (connect renderer to button map)
4. `src/tui/button_map.rs` (four dedicated Properties hit targets)
5. `src/tui/keybindings.rs` (mouse modality, exact-target metadata wrapper, Ctrl+P file regression)
6. `src/tui/context_menu.rs` (filesystem Properties routing, archive preservation, action regression)
7. `src/tui/help.rs` (shortcut description)

No changes to the C4 directory-accounting service, SQLite schema, scanner, media probing, disc classifiers, conversion queue, or metadata editor implementation. All Browse Info album/disc and VOB/M2TS corrections from the visual-source baseline are retained.

## Verification performed in this environment

- Verified all seven baseline source files directly against the supplied visual-source archive and recorded individual baseline and target SHA-256 values.
- Generated a unified review patch and verified `git apply --check` against the exact baseline.
- Checked the seven changed files for whitespace problems with `git diff --no-index --check`.
- Replayed the supplied patch applicator against a fresh extraction of the full baseline: all seven files were installed and matched the final source byte for byte.
- Replayed the patch applicator a second time: no-op, all checksums still matched.
- Tampered with one baseline source file and verified that preflight refused the entire update before modifying any other file.
- Added (but could not run) TestBackend and routing tests for known measurements, cancellation, running scans, unknown values, narrow layouts, theme colors, row order, button locations, file Properties, Ctrl+P, context-menu dispatch and mouse hit testing.

**Not verified:** Rust compilation, `cargo fmt --check`, the complete Rust workspace test gate, or actual interactive screenshots. `rustc`, `cargo` and `rustfmt` are unavailable on this host. Do not claim the source is release-accepted from static checks alone.

## Required integration-host acceptance

On a Rust-equipped integration system:

1. Apply `TonePoet_PROPERTIES_DIALOG_PATCH_2026-10-30.tar.gz` to the exact baseline using `python3 apply.py /path/to/tonepoet`, or use the complete source archive. Run `apply.py` again and confirm the no-op.
2. Execute `cargo fmt --all --check`. If format differences arise, format only the affected files and review the resulting diff. This implementation has **not** been rustfmt-verified here.
3. Execute `cargo test --workspace --locked` and retain the full result log. First fix any compile errors or failing targeted tests. Verify previously passing Browse Info, directory accounting and context-menu tests remain green.
4. Compare the Properties dialog against the supplied `Proposed Properties dialog@2x.png` at 80x24 and a wider terminal using the matching theme. Validate borders, title, spacers, label/value alignment, colors, separator widths, pill labels and button hit areas. Also inspect approximately 46x12 and a very long Unicode path.
5. Check Ctrl+P for both filesystem files and folders, right-click Properties from both Browse file list and tree, Alt+P on the same selection, Edit Metadata... from the Properties modal, and OK/Esc. Ensure the exact item receives the edit command.
6. Test cold cache, a complete cached measurement, a known-invalid or unverified cached measurement, an active scan, cancellation-in-flight, cancelled scan with retained cache, and Recalculate after cancellation completes. Confirm no synchronous drawing-path I/O or stale-click-through.

### Remaining product choices and constraints

- The mockup describes a **folder**. For regular files, this implementation uses the same shell but displays file facts from the Browse listing and omits folder-specific Contains and Recalculate. This is a conservative extension required to make right-click Properties meaningful for files.
- Edit Metadata... deliberately delegates to the **existing** editor, which may decline unsupported folder types or non-audio files. Do not add arbitrary media editing support to satisfy a dialog-only change.
- Theme colors can differ from the PNG if a different user theme is active. Judge reference parity under the screenshot's theme and preserve theme switching rather than hard-coding colors.
- No architectural or persistence expansion is warranted without an independently observed defect. Preserve prior Browse Info behavior.

## Resume procedure

The patch archive has `manifest.json`, `changes.diff`, complete replacements, and a standard-library-only `apply.py` that accepts a partially applied state consisting solely of exact baseline or expected target files. It checks all source files and replacement hashes **before** changing any files, uses atomic same-directory replacements, and can be rerun after interruption. If an integrator has diverged from the supplied baseline, the script fails rather than overwriting unfamiliar edits; reconcile those changes deliberately.

The remaining work is primarily **compile, test, and interactive visual acceptance**, not redesign. Preserve all source and test results and amend this handoff with the actual `cargo` gate outcome.
