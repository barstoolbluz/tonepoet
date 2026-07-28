# Tag Interchange Round 6 — Corrected Engineering Report

## Executive summary

This overlay implements the Round-6 tag-interchange scope against the supplied `hardening` preimages while retaining application version **0.4.4**. The design uses one strict field-block module for text interchange, keeps the existing full-fidelity in-process tag clipboard intact, and routes direct file mutation through the classified metadata writer.

This handoff incorporates the prior correction rounds and resolves the final
quit-routing, modal-lifecycle, CUE-preview, and test-failure blockers:

1. editor file-import and transfer completions use the existing active-or-parked editor extraction/restoration seam, so ordinary tags popups and row context menus do not discard valid work;
2. Ctrl+Q is intercepted before ordinary overlay dispatch and always invokes the editor-aware application-quit reducer, preserving active-write safeguards and unsaved-editor confirmation semantics;
3. typed `:q` preserves the established editable-CUE-preview ownership rule: a preview parked for command input is cancelled first, while `:q` elsewhere falls through to the application-quit reducer;
4. CUE-preview footer and context-menu Cancel actions use a dedicated preview-cancellation helper and never dispatch `Command::Quit`;
5. competing confirmation seams in this overlay invalidate pending tag-interchange preparation before parking the editor: Esc-close/discard, application quit, Browse-archive quit reconciliation, embedded-CUESHEET deletion, and multi-presentation MusicBrainz apply;
6. an admitted editor-close or quit workflow therefore cancels and invalidates pending preparation before the editor is parked or dropped, so a late result cannot replace the newer confirmation, mutate the parked editor, or continue orphan work after a clean close;
7. Get-tags-from-File shares the same monotonic last-request-wins generation and cooperative cancellation slot as transfer preparation;
8. completion ownership is consumable: the current request is retired before reduction, so duplicate delivery cannot apply twice or launch the same outbound transfer twice;
9. clipboard-route tests use a thread-scoped clipboard implementation, and the parallel-isolation regression test joins both workers before asserting so an isolation regression fails rather than deadlocking; and
10. generated Python cache material is excluded from the archive.

The prior correction remains intact: Copy-tags status wording is pinned,
route-level transfer tests exercise the production seams, transfer progress is
bounded and nonblocking, the installer is convergent without a journal, and
manifest/validation claims distinguish source and overlay scopes.

## Scope delivered

### 1. Reusable field-block interchange module

`src/tui/tag_interchange.rs` provides:

- exact key grammar (`[A-Z0-9_]+`);
- one-or-more values per block and exact blank-line separation after CRLF normalization;
- reversible `~` empty-value and literal-tilde encoding;
- explicit omission/reporting for newline-bearing values;
- strict broadcast/positional count validation;
- duplicate-key rejection and all-or-nothing editor preflight;
- 1→N, N→N, and hard-failed N→M transfer planning;
- direct-transfer numbering-field broadcast exclusion; and
- typed canonical `ItemKey` recovery for newly created standard rows.

### 2. Copy tags to text clipboard

At the generation-winning Copy-tags completion, the code now:

- preserves the existing full-fidelity `TagClipboard`;
- serializes the text-representable subset;
- invokes the shared text-clipboard publication seam;
- emits OSC 52 only for payloads at or below 64 KiB;
- treats OSC 52 as advisory; and
- reports `Copied N field(s) from M file(s) (text clipboard)` plus read/serialization disclosures.

The completion route is pinned with an injected publisher spy. Tests that must
exercise the concrete shared-clipboard route install a thread-scoped clipboard
for the complete setup/dispatch/assertion sequence. Parallel tests therefore do
not race on the process-global clipboard, while production continues to use the
single shared clipboard without additional locking beyond its existing mutex.

### 3. Browse-side direct transfer

The deferred Paste-tags item is removed. `Tags & Tagging` exposes:

- Transfer to → Canonical
- Transfer to → All
- Transfer from → Canonical
- Transfer from → All

The worker path uses bounded traversal-order expansion, generation supersession, cooperative cancellation, target-specific diffs, classified writes, and retained failure reporting. Binary entries and artwork are excluded. A 1→N transfer skips `TRACKNUMBER`, `TRACKTOTAL`, `DISCNUMBER`, and `DISCTOTAL`; N→N is positional; N→M fails before target I/O.

Progress is file-count based. The blocking worker calls a progress callback after each target outcome; the UI sender uses `try_send` and publishes the first, final, and every sixteenth file. This supplies meaningful intermediate status without blocking metadata writes or flooding the event channel.

### 4. Metadata-editor `tags` popup

The Editing footer pill is `tags`. Its bottom-anchored popup exposes eight leaves:

- Get tags from: MusicBrainz, gnuDB, Clipboard, File
- Transfer tags from: Canonical, All
- Transfer tags to: Canonical, All

A route test executes all eight `execute_context_action` arms and verifies each resulting editor, status, or picker state; it does not merely inspect menu variants.

Clipboard/File imports update the open editor for review. Inbound transfer applies to the editor. Outbound transfer writes the editor snapshot and requires a blocking confirmation when the snapshot contains unsaved edits.

### 5. Editor asynchronous request ordering and parking

Editor file import and source/target transfer preparation use one ownership
mechanism:

- `tag_transfer_prepare_generation: u64` identifies the newest accepted picker request;
- `tag_transfer_prepare_cancel` cooperatively stops the previous directory walk, source read, or bounded text-file read;
- every completion carries its `request_id` in addition to the existing editor session and content fingerprint;
- `take_tag_transfer_preparation(request_id)` consumes authority before the result is reduced, making duplicate delivery a no-op;
- completions use `take_metadata_editor_with_restore_slot` / `restore_taken_metadata_editor`, so the same reducer works whether the editor is active or parked behind an ordinary popup/context menu;
- `invalidate_tag_interchange_preparation()` cancels the active flag, clears ownership, and advances the generation before a newer competing confirmation or admitted editor exit takes ownership;
- `metadata_editor_prepare_for_competing_workflow()` is the constant-time shared lifecycle primitive used by Esc-close, application quit, Browse-archive quit reconciliation, and CUESHEET deletion; and
- Ctrl+Q is dispatched before ordinary overlays and always calls `request_application_quit()`. Typed `:q` preserves the established editable-CUE-preview ownership rule: a parked preview consumes `:q` as cancellation; otherwise `:q` falls through to the same application-quit reducer. That reducer refuses active writes, invalidates admitted background work, opens the same dirty-editor discard-before-quit confirmation, and leaves clean Browse-archive reconciliation to the existing event-loop authority.
- CUE-preview footer and context-menu Cancel actions call `cancel_pending_cue_preview()` directly and therefore cannot set `should_quit`; active-overlay Esc/Close paths retain their dedicated close-and-restore helper.

This is intentionally small. It adds no modal classification table, queue, journal, retry framework, or repeated filesystem verification. A newer interchange request cancels and supersedes the older request; a newer close, quit, or destructive confirmation invalidates the interchange slot; stale completions are ignored without overwriting the newer status or modal.

### 6. Full editor clipboard citizenship

Each `PresentationTab` owns `selected_rows: BTreeSet<usize>`. Selection and clipboard chords are scoped to the metadata editing surface. Ctrl+C serializes selected rows, Ctrl+X copies then marks permitted rows deleted, and Ctrl+V/Ctrl+P plus Editing-phase bracketed paste use the required known-key block precedence and exact row-count prevalidation. Key/value columns both expose row-level clipboard actions, and mixed rows expose per-file editing.

Structural refresh/removal paths clear or remap selected-row indices so selection cannot silently retarget another field.

### 7. Queue Ctrl+L

Queue Ctrl+L and its help/footer chord text are removed. Clear-finished remains available by mouse. File-picker Ctrl+L remains unchanged.

## Transfer-route regression coverage

The corrected tests include direct route pins rather than relying only on planner tests or pre-existing writer tests:

- `transfer_route_writes_native_flac_and_dsf_and_reports_file_progress`
  - invokes `execute_tag_transfer_from_entries`;
  - writes a synthetic native FLAC and a DSF through the production classified writer seam;
  - pins 1→N scalar broadcast, numbering exclusion, and per-file progress.
- `transfer_route_is_positional_for_n_to_n_and_fails_n_to_m_before_io`
  - invokes the production entries route;
  - verifies positional DSF writes;
  - verifies mismatch rejection before target reads.
- `transfer_from_paths_reads_source_and_writes_target_in_strong_mode`
  - invokes `execute_tag_transfer_from_paths`;
  - reads a DSF source and writes a DSF target under Strong verification.
- `transfer_route_forwards_target_diff_cancel_and_verification_to_writer`
  - invokes the same internal route used by production with a writer spy;
  - pins target-specific diff construction;
  - pins cancellation-handle forwarding;
  - pins both Standard and Strong verification values at the writer boundary.
- `metadata_tags_popup_all_eight_leaf_actions_execute_their_dispatch_routes`
  - executes all eight popup leaves through `execute_context_action`.
- `winning_copy_tags_completion_publishes_serialized_field_blocks`
  - invokes the generation-winning completion reducer and proves it calls the text-clipboard publication seam with exact field blocks.
- `tag_transfer_progress_updates_only_the_latest_active_request`
  - pins progress acceptance to the current transfer generation.
- `editor_transfer_preparation_is_last_request_wins_across_directions`
  - proves a newer inbound request cancels and supersedes an older outbound preparation and that the stale completion cannot mutate the editor or status.
- `parked_editor_transfer_completion_reduces_once_and_preserves_visible_overlay`
  - proves a valid completion reduces into a parked editor without replacing the visible overlay;
  - replays the completion and proves one-shot ownership prevents a second apply or status change.
- `editor_tag_interchange_preparation_authority_is_consumed_once`
  - directly pins that a current request token can be taken exactly once.
- `parked_editor_file_import_is_last_request_wins`
  - proves a newer file import supersedes an older import;
  - proves the accepted result applies to a parked editor while the visible overlay remains intact.
- `dirty_editor_close_confirmation_supersedes_late_tag_transfer_preparation`
  - begins an outbound preparation against a dirty editor, requests close, and delivers the older target completion;
  - proves the discard confirmation and its status remain unchanged, no transfer generation starts, the parked editor is not mutated, and request ownership remains invalidated.
- `metadata_editor_close_invalidates_direct_mb_apply_operation`
  - additionally pins that a clean close cancels and invalidates pending tag-interchange preparation, preventing bounded file/traversal work from continuing without an editor recipient.
- `colon_q_confirmation_supersedes_late_tag_transfer_preparation`
  - begins outbound preparation in a dirty editor, executes `:q`, delivers the obsolete completion, and proves the discard-before-quit confirmation, status, editor snapshot, and transfer generation remain unchanged.
- `ctrl_q_is_global_in_dirty_metadata_editor_and_uses_quit_confirmation`
  - proves Ctrl+Q bypasses metadata-editor overlay dispatch, uses the centralized quit reducer, cancels request ownership, and opens the same guarded quit confirmation as `:q`.
- `cuesheet_delete_confirmation_supersedes_late_tag_transfer_preparation`
  - begins outbound preparation, opens the embedded-CUESHEET deletion confirmation, delivers the obsolete completion, and proves the newer destructive confirmation remains authoritative and no transfer starts.
- `cue_preview_cancel_action_closes_preview_without_quitting`
  - executes the actual CUE-preview context-menu Cancel dispatch and proves the preview closes while `should_quit` remains false.
- `editable_cue_preview_cancel_button_closes_preview_without_quitting`
  - drives the editable preview's real footer-button mouse route and proves it uses preview cancellation rather than application quit.
- `colon_q_with_pending_cue_preview_cancels_preview_without_quitting`
  - pins the historical command-mode ownership rule: a parked editable preview consumes typed `:q` and leaves the application running.
- `ctrl_q_in_cue_preview_requests_application_quit`
  - proves Ctrl+Q bypasses preview cancellation and retains application-global quit behavior in `CuePreview`.

The pre-existing native writer tests remain useful lower-layer evidence, but they are not presented as substitutes for the new route tests.

## Other named regression tests

### Field blocks and editor application

- `field_blocks_round_trip_empty_tildes_whitespace_and_crlf`
- `serializer_skips_multiline_values_without_altering_them`
- `serializer_fails_closed_on_noncanonical_or_duplicate_keys`
- `parser_rejects_malformed_blocks_and_count_mismatches`
- `serializer_parser_identity_on_serialized_subset`
- `field_block_round_trip_property_over_generated_values`
- `block_apply_prevalidates_all_counts_and_pins_success_wording`
- `editor_apply_rejects_ambiguous_duplicate_target_rows_before_mutation`
- `transfer_plan_broadcasts_scalars_but_never_numbering`
- `transfer_plan_preserves_positional_traversal_order_and_fails_mismatch`
- `transfer_plan_rejects_duplicate_or_structurally_short_sources_before_writes`

### Clipboard, menus, selection, paste, and geometry

- `public_shared_clipboard_api_round_trips_exact_text`
- `scoped_shared_clipboards_are_isolated_between_parallel_tests`
  - records both workers' observations, completes both barrier rendezvous, joins both workers, and only then asserts isolation; a regression cannot strand a barrier participant.
- `osc52_tag_clipboard_write_is_exact_and_size_gated`
- `browse_transfer_menu_has_four_terminal_routes_and_no_paste_stub`
- `metadata_tags_popup_exposes_exactly_eight_leaf_routes`
- `invalid_tonepoet_clipboard_status_wording_is_pinned`
- `metadata_row_selection_is_owned_per_presentation_surface`
- `metadata_ctrl_c_serializes_only_selected_rows_as_field_blocks`
- `metadata_ctrl_x_marks_writable_rows_deleted_and_honors_cuesheet_refusal`
- `editing_row_paste_pins_duke_positional_known_block_precedence_and_album_modes`
- `editing_phase_bracketed_paste_uses_block_then_row_classification_and_reports_errors`
- `metadata_row_context_menu_classifies_both_columns_and_exposes_per_file_edit`
- `metadata_tags_popup_root_is_bottom_anchored_above_footer`
- `successful_surface_reread_clears_row_selection_bound_to_old_entries`
- `saved_row_removal_remaps_selection_indices_without_retargeting`
- `bounded_tag_block_file_reader_accepts_valid_utf8_blocks`
- `bounded_tag_block_file_reader_rejects_invalid_utf8`
- `bounded_tag_block_file_reader_rejects_oversized_regular_file_before_reading`
- `bounded_tag_block_file_reader_honors_pre_cancel`
- `queue_ctrl_l_is_unbound_while_clear_finished_remains_a_mouse_action`

## Broadcast regimes (intentionally distinct)

1. **Editor block apply:** one value broadcasts for any key, including numbering keys, because the result remains reviewable before save.
2. **Direct transfer:** one source file may broadcast scalar fields to N targets, but track/disc numbering keys are skipped because the write is immediate.
3. **Detail-edit paste:** `metadata_editor_apply_detail_paste` remains unchanged. The new row-paste entrance validates counts before calling it.

## Installer design and idempotency balance

The installer deliberately does **not** implement a transaction journal, repository snapshotter, or recursive defensive filesystem framework.

Its bounded approach is:

- acquire one advisory exclusive installer lock;
- validate payload hashes and reject unsafe manifest paths;
- reject symlinked target-parent components;
- accept each target only when it is the exact manifest preimage or exact postimage;
- stage payload files on the repository filesystem;
- re-hash each pending target immediately before replacement;
- copy and verify a rollback backup;
- atomically replace the target with `os.replace` and fsync its parent;
- verify all postimages; and
- preserve the backup directory and print its location if rollback itself encounters an error.

Atomic replacement means interruption cannot create the prior move-to-backup missing-file window. After a crash, every target pathname is expected to remain either its exact preimage or exact postimage; a rerun accepts that mixed known state and converges. Temporary stage/backup directories may remain after SIGKILL, but they are not consulted and do not affect repository correctness. This installer work adds no cost to Tonepoet runtime or transfer performance.

The installer lock serializes concurrent invocations of this overlay. The immediate preimage comparison detects ordinary edits between preflight and replacement. It does not claim to prevent a hostile, non-cooperating process from racing within the final hash-to-rename interval; eliminating that residual pathname race would require substantially more platform-specific machinery than is proportionate for this source-overlay installer.

## Pinned user-visible statuses

Successful block apply retains the required form, for example:

> applied TITLE (broadcast to 12 files), TRACKNUMBER (positional) — review before save

Empty/invalid tonepoet clipboard retains:

> tonepoet's clipboard has no tag blocks; paste from the system clipboard with your terminal's paste key instead

Copy tags now pins:

> Copied 1 field from 2 files (text clipboard)

## Disclosed limitations

- OSC 52 is advisory; terminal/multiplexer support varies, and payloads above 64 KiB are not emitted.
- Tonepoet cannot initiate a reliable system-clipboard read through crossterm/OSC 52. The Clipboard menu reads Tonepoet's shared text clipboard; system clipboard text enters through terminal paste/bracketed paste.
- Artwork/pictures are not serialized or transferred.
- The gnuDB entry remains exposed while the documented endpoint is dark; protocol migration is outside this round.
- The Custom tag builder remains deferred.
- Transfer progress is file-count based rather than byte based. It is intentionally nonblocking and rate-limited.

## Validation actually performed in this environment

The following validations were executed against the corrected source and final overlay; command transcripts are summarized in the bundle-root `VALIDATION.md`:

- `sha256sum -c SHA256SUMS` on the **supplied source handoff**, verifying all **29** entries in that source-handoff manifest.
- `git diff --no-index --check` between the prior corrected payload and this final payload.
- Python syntax compilation of `apply-overlay.py` without writing bytecode.
- a lexical Rust comment/string/delimiter scan over all 16 modified or added Rust files.
- exact overlay-manifest verification for **17** payload files:
  - `PREIMAGE_SHA256SUMS` has 17 entries;
  - `POSTIMAGE_SHA256SUMS` has 17 entries;
  - `CHANGED_FILES` has the same 17 paths.
- disposable-tree installer checks covering:
  - clean `--check` and apply;
  - exact postimage verification;
  - repeat apply;
  - convergence from a deliberate pre/post mixed state;
  - refusal of an unexpected target hash before replacement;
  - compare-before-replace rejection when a target changes after preflight;
  - symlinked parent rejection;
  - rollback after an injected later-file failure; and
  - preservation/reporting of surviving backups after an injected rollback failure.
- concurrent-installer lock serialization (more than one second of observed blocking in the disposable test).
- archive path-safety, nested-root, payload-hash, and checksum verification.
- explicit verification that the archive contains no `__pycache__` directory or `.pyc` file.

### Execution boundary

`cargo test --workspace` was **not run** here. The execution image has no Rust toolchain, and the supplied handoff is a source subset whose declared workspace members are not all present. Therefore **5,214 passed / 0 failed** remains the supplied baseline, not a result re-certified by this environment. The new tests have been statically audited but must be compiled and executed after applying the overlay to the exact complete `hardening` tree.

## Manifest accounting

There are two different integrity scopes:

- The original handoff's `SHA256SUMS` covers 29 supplied source files and was verified before modification.
- The delivered overlay's `PREIMAGE_SHA256SUMS`, `POSTIMAGE_SHA256SUMS`, and `CHANGED_FILES` each cover the 17 files this overlay installs.

The delivered archive does not claim to contain a root file named `SHA256SUMS`.

## Deviations from the brief

No requested application feature was intentionally removed. The only deliberate implementation choice requiring explanation is installer crash idempotency: exact pre/post convergence plus atomic per-file replacement is used instead of a durable multi-file journal. This directly removes the missing-file crash window and permits reliable rerun convergence while avoiding the complexity and performance failure mode the user explicitly rejected.
