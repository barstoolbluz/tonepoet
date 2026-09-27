# Delivery notes: metadata FieldSet paste fixes

Date: 2026-09-26
Base delivery: `tonepoet_cue_menu_clipboard_fixes_delivery_2026-09-26.tar.gz`
Underlying snapshot: `main` at `6cd5b4f` (per bundle name)

## Scope

This follow-up is intentionally limited to the two remaining metadata FieldSet paste defects reported after the CUE-menu / unified-filesystem-clipboard delivery:

1. Structured multi-field paste was still unnecessarily anchored to an existing metadata row.
2. FieldSet application still validated and constructed every field against `PresentationTab.paths.len()`, which rejects positional logical-track data on unified CUE surfaces whose presentation/file and track dimensions differ.

The structured clipboard envelope and the host-clipboard authority model are unchanged.

## Corrections

### Rowless metadata FieldSet paste

- Ctrl+V, Ctrl+P, and raw Ctrl+Shift+V now request the host clipboard in Metadata Editing mode even when the cursor is on the `+ Add field...` sentinel instead of an existing field row.
- Bracketed terminal paste already carries authoritative host text; the same rowless sentinel is now accepted by the structured FieldSet path.
- Right-click Paste is exposed from every non-row location inside the Metadata overlay, including `+ Add field...`, blank metadata content, and overlay chrome. Existing field rows keep their existing row-specific context menus.
- Rowless paste uses `field_index == entries.len()` as an explicit sentinel. This keeps the async session/cursor ownership checks intact without introducing another clipboard target type.
- The sentinel is valid only for a structured `FieldSet`. Structured single-field payloads and plain text still require a real destination row, so rowless paste cannot silently overwrite the field that happened to be selected before the click.
- FieldSet overwrite confirmation now accepts the sentinel (`field_index == entries.len()`) while still rejecting indices beyond it. Confirmation therefore works for rowless multi-field paste without weakening stale-editor protection.

### Unified-CUE semantic dimensions

- FieldSet preflight no longer applies one global `paths.len()` cardinality to every field.
- Each destination field resolves its target axis from the existing unified-CUE semantic row-shape authority (`UnifiedCueDimensions` + `unified_cue_row_shape`).
- Recognized track-scoped fields such as `TITLE`, `ISRC`, and `TRACKNUMBER` validate/apply against `cue_album_synthetic_sheet.track_sources.len()`.
- Album/file-scoped fields such as `ALBUM`, `ALBUMARTIST`, `DATE`, `GENRE`, and `CATALOGNUMBER` validate/apply against the synthetic sheet's audio-file dimension.
- `CUESHEET` retains its presentation-file dimension.
- `ARTIST` preserves the existing declared-scope rule; a missing `ARTIST` row defaults to Track scope, matching the existing unified-CUE transfer planner.
- Existing custom/declared-only rows retain their declared/effective scope instead of inferring scope from coincident vector lengths.
- Missing recognized track rows are now created with `RowScope::Track` and track-sized value/original/cardinality vectors. Missing file rows remain file-scoped.
- FieldSet validation remains atomic: every block is resolved and cardinality-checked before any editor row is mutated.
- Canonicalized field names are used for destination lookup and duplicate-block preflight, preventing aliases from targeting the same row twice in one apply operation.

The public `FieldBlockApplyReport` structure and `success_status(file_count)` API remain unchanged; the correction is internal to destination planning/application.

## Changed files relative to the accepted base delivery

- `src/tui/app.rs`
- `src/tui/event_loop.rs`
- `src/tui/keybindings.rs`
- `src/tui/message.rs`
- `src/tui/tag_interchange.rs`
- `DELIVERY_NOTES_metadata_fieldset_paste_fixes_2026-09-26.md` (this file)

## Regression coverage added

Coverage now includes:

- Ctrl+V, Ctrl+P, and Ctrl+Shift+V initiating a fresh host-clipboard read from the `+ Add field...` sentinel;
- structured FieldSet paste succeeding from the rowless sentinel;
- rowless FieldSet overwrite confirmation preserving and accepting the sentinel;
- bracketed terminal FieldSet paste succeeding from the rowless sentinel;
- right-click Paste being present on `+ Add field...`, blank metadata content, and non-row overlay chrome;
- a single-image unified CUE with one presentation/audio file and ten logical tracks accepting a ten-position `TITLE` together with a one-position `ALBUM` in the same FieldSet;
- a missing unified-CUE `TITLE` being created as a ten-position `RowScope::Track` row rather than a one-position file row.

## Validation performed in this runner

This environment has no `nix`, `flox`, `cargo`, `rustc`, `rustfmt`, or `rust-analyzer`. The repository requires its pinned Nix development shell, so this delivery does **not** claim a compile, format, or test pass in this sandbox.

Static validation performed:

- audited every `MetadataRows` host-paste initiation/completion/confirmation path affected by the rowless sentinel;
- audited bracketed-paste routing separately from asynchronous host reads;
- audited FieldSet preflight to confirm `validate_block_count` now receives the per-field semantic target count rather than a global presentation-file count;
- verified the public `FieldBlockApplyReport` struct and `success_status(file_count)` implementation are byte-for-byte unchanged from the accepted base delivery;
- checked all changed Rust files with a lexical delimiter-balance scan;
- ran `git diff --no-index --check` against the accepted base; it reported no whitespace-error diagnostics.

Required handoff gate in the repository's pinned Nix shell:

```sh
nix develop --extra-experimental-features 'nix-command flakes'
cargo fmt --check
cargo test --workspace --no-fail-fast --exclude tonepoet-true-peak
```

The true-peak crate was not changed and does not require its separate ~41-minute qualification gate for this patch.
