# Round 11 — apply-side addendum (changes Claude Code made to your delivery)

Records everything changed on the applying side after your round-11 v2 overlay was applied,
so your model of the tree stays accurate. None of this expanded scope; it is compile-fixes,
one design decision you were not in the room for, and test corrections.

## Compile-fixes (out-of-bundle; you had no compiler)

- `crates/tui-file-picker/src/text_input.rs` — 2 borrow hoists in `undo()`/`redo()`
  (`let current = self.snapshot();` before `Self::push_bounded(&mut self.<hist>, current)`;
  the inline `self.snapshot()` while `&mut self.<hist>` was borrowed was E0502).
- `src/tui/keybindings.rs` — 1 borrow hoist in the Up/Down inline-nav handler
  (`let maximized = state.maximized;` before `state.edit_input.as_mut()`); removed one unused
  `let total_rows = …` local.
- **14 `SourceInfo` literals** across `db.rs`, `browse.rs`, `command.rs`, `disc_browser.rs`,
  and `metadata_view_models.rs` gained `sample_format_is_float: None` (the new field from
  item 3). All are non-probe construction sites (db/disc/test fixtures), so `None`
  (unknown int/float) is correct.
- **New `TuiButton` variants** (`MetadataEditorTitle`, `MetadataEditorViewCanonical`,
  `MetadataEditorViewAll`) added to the non-exhaustive matches in `button_map.rs::screen()`
  (the overlay-`None` group) and the `keybindings.rs` button dispatch (the overlay-input
  no-op arm). The real click handling was already present at `keybindings.rs:~28571`; these
  were only exhaustiveness gaps.

## Design decision — the canonical field set (item 6)

Your item-6 "Canonical" view filtered to `STANDARD_KEY_ORDER` (26 fields). The user's intent
for Canonical is broader than the pure-standard set. `STANDARD_KEY_ORDER` was **extended by 8
entries** (now 34), appended after the MusicBrainz block:

```
REPLAYGAIN_TRACK_GAIN, REPLAYGAIN_TRACK_PEAK, REPLAYGAIN_ALBUM_GAIN,
REPLAYGAIN_ALBUM_PEAK, REPLAYGAIN_REFERENCE_LOUDNESS, CUESHEET, LINEAGE, DISCOGS_URL
```

Rationale: the user wants ReplayGain, embedded CUESHEET, and specific curated custom tags
(`LINEAGE`, `DISCOGS_URL`) visible in the default view. Keys canonicalize uppercased with
underscores preserved, so these match their stored form. This is the single source of truth
for both canonical filtering and sort order; appending preserved the existing fields' relative
positions (the MusicBrainz-position invariant holds). Treat this extended list as the current
canonical spec going forward.

## Test corrections (3 pre-existing tests, updated to your item-6 model)

Item 6 changed the cursor/scroll model: `state.cursor` is a raw entry index, but `state.scroll`
and cursor-visibility now operate in **visible-row-position** space (position within the
view-filtered `visible_metadata_rows()`). Three pre-existing tests encoded the old 1:1
assumption and were updated to assert on the visible position:

- `context_menu.rs :: context_menu_add_field_routes_through_editor_open_and_scrolls_input_visible`
- `keybindings.rs :: metadata_editor_add_row_scroll_uses_rendered_content_height`
- `keybindings.rs :: metadata_mouse_double_click_edit_refuses_unpersistable_unified_per_track_key`
  (its setup set `scroll = composer_idx` (raw); now sets `scroll` = the composer row's visible
  position so the top-of-content double-click still lands on it).

Your item-6 scroll/render logic was found **correct**; these were stale test expectations, not a
feature bug.

A fourth pre-existing test, `metadata_ctrl_x_marks_writable_rows_deleted_and_honors_cuesheet_refusal`,
failed only because CUESHEET was hidden in Canonical; the canonical-list extension above
(CUESHEET now canonical) fixed it with no test change.

## Item 2b (move undo/redo) — do NOT re-add the rejected mechanism

The brief listed Item 2b (deterministic reverse-replay move undo/redo). Your v2 delivery
did **not** add a new `move_replay_invalidation_reason`, and that is **correct** — do not
add one. Audit finding (verified on the applying side): the move-undo capability already
exists in the baseline and is what Item 2b describes:

- `FileOperationUndoJournal` records `FileOperationUndoKind::Move` entries via
  `record_completed_file_task_for_undo` (keybindings.rs), wired to the file-task completion
  handler (event_loop.rs), with a reversible-provenance / staleness guard (moves that cannot
  establish reversible provenance — e.g. overwrite/merge — are excluded by design). Undo is
  performed by `execute_file_operation_undo`.
- Your Item 2a fix is what completes 2b: a new-path directory move now **succeeds** (instead
  of aborting on the unsupported atomic primitive), and the publish path captures rename
  provenance via `verify_committed_rename` in **both** `NoClobberRenameMode::Atomic` and
  `CheckedBestEffort` — so the completed move reaches the recorder and becomes undoable.

The rejected round-11 attempt reimplemented this as `move_replay_invalidation_reason` on top
of its (rejected) proof/ownership machinery. That was redundant with the existing journal and
is exactly the over-engineering we removed. **Item 2b is considered delivered via the existing
journal + your 2a fix. Do not build a parallel move-replay/proof/journal system for it.**

## Cluster-B (separate bounce)

Four new all-view ID3v1-only MP3 tests are `#[ignore]`d and bounced — see
`docs/round11-clusterB-bounce.md`. Their fixture is impossible with lofty's default save.

## Gate

`cargo test --workspace` inside `nix develop` after all the above. Baseline was 5384/0;
target is 5384 + your new item pins, with the 4 Cluster-B tests ignored. Version stays 0.4.4.
