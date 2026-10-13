# R36 static verification — 2026-10-12

## Result

**Source correction constructed and independently patch-replayed; build-host acceptance NOT YET RUN.** The corrected regression must be executed on a Cargo-enabled audio host. Do not treat this audit as a passing Rust test or completed Reference requalification.

## Inputs and provenance

- Input: `TonePoet_LOGGING_R36_core_editor_row_2026-10-12.tar.gz` (R35 source snapshot supplied for this round).
- Input archive SHA-256: `d724434c87b578e5b79206572af50ecb7f41e59cc0c9a18559301e07aabacf50`.
- Input R35 manifest: **972 of 972 listed regular files verified** before any edits.
- Authority: `BRIEF_LOGGING_R36_core_editor_row_2026-10-12.md` (included in this bundle).

## Defect and correction

The R35 test `r33c_album_view_discnumber_follows_selected_carrier_authority` expected that File tags omit a `DISCNUMBER` row when neither FLAC stores the tag, then tried adding the key. That assumption is invalid: `src/tui/probe.rs` calls `ensure_standard_fields_present`, which injects `DISCNUMBER` as an empty File-scoped core row with zero stored values in all file slots.

Only the R35 native-authority half of that regression was changed, in `src/tui/keybindings.rs`:

1. Locate the already-present File-scoped `DISCNUMBER` row.
2. Assert two empty values, zero original stored-value counts, and no difference between original/current values.
3. Begin inline editing via `metadata_editor_begin_cursor_value_edit` (the normal existing-row editor route), assign explicit positional values `1; 2` by path, and commit.
4. Keep all existing production Save-path and post-Save assertions: two native FLAC carriers changed; both sidecar CUEs unchanged byte-for-byte; tags reread as DISCNUMBER 1 and 2; reopening yields the same File-scoped values and a clean editor.

No production Rust logic, metadata grouping, authority routing, audio pipeline, or performance-sensitive code changed. No speculative fixes to additional tests were made.

## Local checks performed

- Baseline input archive extracted with no absolute/member traversal paths; existing repository-local symlink preserved.
- All 972 entries in the supplied R35 SHA-256 manifest verified before editing.
- After editing, comparison with the complete R35 manifest identifies **exactly one pre-existing changed file**, `src/tui/keybindings.rs`.
- Unified patch `R36_FROM_R35.patch` applied cleanly to an independent copy of that file from the R35 input, producing a byte-identical R36 source. Reversing the patch reproduced the input source byte-for-byte.
- `git apply --reverse --check R36_FROM_R35.patch` passes in the delivered tree.
- `bash -n R36_ACCEPTANCE.sh` passes; the named focused regression and adjacent test selectors exist in the repository.
- The File tags injection, initial zero stored-value count, inline edit plan, and Save helper were inspected in the source.

## Not run and remaining acceptance

- **Rust compilation, test execution, and full workspace regression gate:** not executed; neither `cargo` nor `rustc` is available in this environment. No new 7,457-test pass count is claimed.
- **Physical SACD Reference requalification:** not executed; requires the configured audio host and actual SACD ISO. The upstream report says the unrelated source-lock freshness test was failing; R36 does not assert that this gate is already resolved.

First run `cargo test --locked --lib r33c_album_view_discnumber_follows_selected_carrier_authority`. Then run `bash ./R36_ACCEPTANCE.sh /absolute/path/to/real-SACD.iso` for the complete preserved R34D-to-Reference gate and final workspace test. The R36 runner deliberately does not call `R35_ACCEPTANCE.sh`, because that runner verifies a *historical* R35 manifest and would correctly reject the R36 source edit. The R36 runner instead performs the three R35 focused tests directly before chaining into R34D.

Reference requalification can update historical source-lock evidence; any post-qualification source changes require regenerating the R36 inventory and archive before redistributing that qualified tree.
