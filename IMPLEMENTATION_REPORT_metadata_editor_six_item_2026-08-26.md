# Metadata editor six-item work order — corrected R2 implementation report

Baseline supplied by the work order: `main` @ `2cf31ac`.

## Result

All six requested behaviors are implemented in the supplied source tree, and the three defects found in review of the first implementation are corrected. The implementation remains localized: seven Rust source files differ from the supplied baseline, while the corrective R2 delta itself touches only `src/tui/event_loop.rs`, `src/tui/app.rs`, and `src/tui/tag_interchange.rs` plus focused tests in those same files. No persistence format, save protocol, metadata carrier format, recovery framework, or asynchronous worker architecture was changed.

## Decisions requested by the work order

### COUNTRY canonicalization

`COUNTRY` remains distinct from `RELEASECOUNTRY`. They are not folded into one another. `COUNTRY` is a first-class Canonical-view key immediately after `RELEASECOUNTRY` in `STANDARD_KEY_ORDER`, preserving the existing relative order of every previously-standard key while keeping the related fields adjacent.

The add-field suggestion corpus derives from `STANDARD_KEY_ORDER`; the former one-off `COUNTRY` append is gone. This removes the structural possibility that the built-in suggestion authority can offer a standard key Canonical view rejects. Curated COUNTRY value completions remain unchanged.

### Fields that default to the per-track editor

Only `TITLE` and `ISRC` use the per-track detail editor for the ordinary default gesture (Enter / value double-click). `TRACKNUMBER` and `DISCNUMBER` remain on their previous default routes even though they share the TrackScalar persistence/distribution class. Explicit `Edit in-place` remains available for TITLE and ISRC and retains positional semicolon distribution.

## Changes by item

1. **Add-field shortcut**
   - Added `Alt+F` in the metadata editor's Metadata tab.
   - It calls the existing `metadata_editor_open_add` entry point; there is no shortcut-specific add path.
   - The editor footer advertises `Alt+F add`; the footer hit target still dispatches the established `:a` command.
   - `metadata_editor_open_add` consistently refuses read-only editors, so all callers share the same admission rule.

2. **Newly-added field visibility**
   - Added minimal in-memory tracking of canonical keys added during the current editor session, scoped by the active surface's stable metadata-editor session id.
   - A newly-added custom key remains visible in Canonical view for that session only; pre-existing custom keys remain hidden.
   - Fresh editor state has no session-added set, so the exception does not persist.
   - Reused one cursor-visibility repair helper on view switches, presentation switches, direct add-key commit, and now ordinary successful post-save surface replacement. This closes the deterministic Delete -> Apply -> reread index-shift path that could strand the cursor on a hidden custom row.
   - `Get tags from Clipboard or File` now marks a key session-added only when transfer actually creates a missing row. Updating an already-present custom row does not grant visibility, preserving the scoped exception for user-created fields only.

3. **COUNTRY in Canonical view**
   - Added distinct `COUNTRY` membership to `STANDARD_KEY_ORDER`, adjacent to `RELEASECOUNTRY`.
   - Did not change metadata canonicalization/writeback identity for either field.
   - Added coverage that both are visible while an unrelated custom key remains hidden.

4. **Terminal paste undo granularity**
   - The seven affected single-line terminal-paste sites now perform one `insert_string` for a non-empty retained first line, preserving one-undo behavior for the entire paste.
   - A small local adapter preserves the pre-rewrite zero-character behavior when the retained first line is empty: it does not call `insert_string("")`, so a selected value, cursor, selection, and undo history remain untouched.
   - First-line truncation remains unchanged. BulkRename template, TextEdit, CommandInput, FileInput, MetadataAutoNumber, metadata DetailEdit row input, and metadata InlineEdit use the same narrow adapter.
   - The existing 300-character COMMENT regression still proves one undo restores the original value beyond the 128-snapshot history cap.
   - New selected-COMMENT coverage pastes `"\nignored"` and proves text, cursor, selection, and undo state are unchanged.

5. **ISRC default editor**
   - Added a narrow affordance policy beside the authoritative metadata taxonomy: TrackScalar plus explicit `TITLE | ISRC` policy.
   - Default Enter and value double-click route ISRC to DetailEdit.
   - Explicit inline ISRC editing remains InlineEdit and preserves semicolon positional distribution.
   - Negative coverage confirms TRACKNUMBER and DISCNUMBER do not inherit the new default.

6. **Context-menu wording**
   - Centralized the two labels as `Edit in-place` and `Edit per track`.
   - All metadata row field classes use those exact strings whenever the corresponding action is present.
   - Status text that directs the user to the per-track action uses the same label constant; TITLE/ISRC explicit-inline blocked-slot guidance continues to say `Enter` because Enter is their default detail route.
   - Existing action-availability rules were not broadened.

## Corrective R2 regression coverage

- `inline_comment_empty_first_line_terminal_paste_preserves_selected_input`
  - selected `before` value;
  - terminal paste starts with newline;
  - text, cursor, selection, and undo history remain unchanged.
- `successful_surface_reread_rehomes_cursor_when_model_shift_lands_on_hidden_row`
  - Canonical cursor begins on COUNTRY;
  - reread removes COUNTRY so hidden `ZZZ_CUSTOM` shifts into its model index;
  - post-refresh cursor is asserted to be visible and not on the hidden row.
- `transfer_created_custom_field_is_session_visible_but_existing_custom_field_stays_hidden`
  - transfer-created `CUSTOM_LABEL` becomes visible for the session;
  - the same transfer into a pre-existing `CUSTOM_LABEL` updates its value but leaves it hidden in Canonical view.

## Source-level audit performed here

The execution sandbox does not contain Nix (`/nix` is absent), `cargo`, `rustc`, or `rustfmt`. Per the work order, I did **not** substitute a non-Nix build/test command.

I did perform non-build source checks:

- compared corrected R2 against the first implementation bundle and confirmed only the three expected Rust files differ;
- verified the terminal-paste helper has exactly seven production call sites, matching the seven converted single-line paths;
- verified the affected `handle_paste` region has no character-loop / `insert_char` insertion path;
- verified `replace_saved_surface_entries` now invokes the existing cursor-visibility repair after installing/clamping the reread surface;
- verified tag transfer calls `remember_session_added_metadata_key` only in the missing-row creation branch, while direct add-field commit retains its existing call;
- verified the complete implementation differs from the supplied baseline in only the seven listed Rust source files.

## Required certification still outstanding

This bundle is therefore **implemented but UNCERTIFIED**. The mandatory gate from the work order could not be executed in this environment. It must be run in an environment with the repository's Nix tooling:

```sh
nix develop --extra-experimental-features 'nix-command flakes'
cargo test --workspace --no-fail-fast
cargo test --workspace --no-fail-fast
```

Every `test result:` line must report `0 failed`, and compiler output should be checked for new warnings before hand-off is certified.

## Files changed from the supplied baseline

- `src/metadata_persistence.rs`
- `src/tui/app.rs`
- `src/tui/draw_overlays.rs`
- `src/tui/event_loop.rs`
- `src/tui/keybindings.rs`
- `src/tui/probe.rs`
- `src/tui/tag_interchange.rs`

A reviewable unified diff is included as `PATCH_metadata_editor_six_item_2026-08-26_CORRECTED_R2.diff`.

## Brief contradictions / incomplete diagnosis found

One diagnosis in the work order was slightly incomplete, exactly as the review identified: `TextInputState::insert_char` and `insert_string` have equivalent selection behavior only when at least one character/string byte is actually inserted. The old terminal-paste character loop performed no operation for an empty retained first line, while `insert_string("")` deliberately deletes an active selection. The corrected adapter preserves the old zero-character no-op without changing the shared `TextInputState::insert_string` primitive.

No other material work-order diagnosis was contradicted by the supplied tree. The existing TITLE key-column double-click remains on its legacy inline route; the TITLE/ISRC default-detail change applies to the value-edit default gesture, matching the existing mouse column semantics rather than broadening double-click behavior beyond the value editor.
