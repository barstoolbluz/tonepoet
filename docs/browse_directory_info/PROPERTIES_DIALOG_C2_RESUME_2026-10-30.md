# TonePoet Properties C2: precise correction and continuation handoff

Date: 2026-10-30

## Starting point

Use the attached complete `TonePoet_PROPERTIES_DIALOG_C2_SOURCE_2026-10-30.tar.gz`
or apply the `TonePoet_PROPERTIES_DIALOG_C2_PATCH_2026-10-30.tar.gz`
against the exact **prior** `TonePoet_PROPERTIES_DIALOG_SOURCE_2026-10-30.tar.gz`.
The preceding Browse Info and Properties implementation is preserved.

Base SHA-256: `d521fc6e3059ee283a5f7f85da98462d3a09a0a005e627f3dda25e615122a949`.

## The two defects and corrections

1. **Location was the item path.** `content_lines` in
   `src/tui/draw_directory_properties.rs` now calls the unchanged
   `display_location()` with `state.path.parent().unwrap_or(&state.path)`.
   Nested folders and files show their containing directory, while a filesystem
   root falls back to itself. Home abbreviation, Unicode, and existing
   horizontal-scrolling behavior remain in the same formatter.

2. **Symlinks were mistaken for folders and showed target-derived mtimes.**
   `open_for_path` in `src/tui/directory_properties.rs` now checks
   `entry.is_symlink` **before** excluding directory entries. Both file and
   directory symlinks use the existing `PropertiesSubject::File { is_symlink:
   true, ... }` display: no directory-accounting subscription and no target
   size or mtime represented as link facts. `modified` is explicitly `None`
   for symlinks because the existing Browse listing may have followed them.
   The renderer independently refuses a symlink mtime even if a caller
   supplies one. The existing unfiltered `all_dirs` snapshot covers tree-selected children
   that the visible list filters out. Ordinary file metadata and ordinary
   directory accounting remain unchanged.

The change does not introduce synchronous filesystem calls, new filesystem
scanners, database/persistence changes, or changes to Edit Metadata routing.
The rendering and controller are the **only two** modified Rust files.

## Focused regression tests added

- Folder, ordinary file, and filesystem-root Location values.
- Home-compatible location formatting, Unicode and horizontal scroll.
- Symlink-to-file and symlink-to-directory entries, with intentionally
  target-derived input mtimes: no subscription, no target timestamps.
- Direct renderer fixture with a supplied symlink mtime: target mtime hidden.
- Tree-selected link represented in the unfiltered directory snapshot.
- Ordinary file mtime preservation and ordinary folder accounting subscription.

These tests are **added, not executed**: this environment does not have
`cargo`, `rustc`, or `rustfmt`. Patch integrity tests and source review are
not substitutes for compilation or a working TUI test.

## Integration host acceptance

1. Apply patch to the exact baseline, or extract complete C2 source. Re-apply
   and confirm the no-op. If the integration tree has diverged, review the
   changes; the apply script deliberately refuses unknown file versions.
2. Run `cargo fmt --all --check`. If formatting fails, format and review **only
   these two files** and keep any other changes out of the fix.
3. Run targeted Rust tests and then `cargo test --workspace --locked`.
4. In Browse, open Properties for a nested directory and a file. Confirm the
   Location row displays the containing folder and a filesystem root displays
   its own root path.
5. Test file and directory symlinks (including broken symlinks) after Browse
   has listed them. Verify the dialog says "Symlink · target not followed",
   shows no target-derived Modified timestamp or size, and never starts a
   recursive accounting request. Ordinary directories should still scan.
6. Confirm the existing Ctrl+P, right-click Properties, Alt+P, Edit Metadata,
   Recalculate, Cancel, and OK routing. Compare the dialog against the retained
   Properties mockup under the intended theme.

## Scope and known data boundary

Tree nodes outside the current cached directory listing have no symlink flag.
This patch does not synchronously stat those paths or change the tree data
model. A tree-only symlink with **no corresponding cached Browse entry** cannot
be identified from the current in-memory data alone. Do not broaden this
correction into a tree-cache redesign without an observed reproduction and
an explicit performance decision. The reported file-list symlink scenario is
addressed.

## Resume

The patch archive contains `apply.py`, `manifest.json`, `replacements/`,
`changes.diff`, the reference PNG, and this handoff. `apply.py` verifies all
source and replacement hashes **before** writing and uses same-directory atomic
replacement per file. A partially completed application is restartable; a
second application is a no-op. Preserve the original base archive until
Cargo and interactive acceptance have passed.
