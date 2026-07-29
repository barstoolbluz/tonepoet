# Corrective Round 9 - asynchronous invalid-APEv2 repair correction

Date: 2026-07-29
Target version: 0.4.4
Delivery type: fail-closed complete-file overlay against the exact received Round 9 preimages
Readiness classification: corrected static candidate; production handoff still requires the bundled full-workspace and live-fixture gates

## Executive result

This correction removes the invalid-APEv2 repair from synchronous TUI key handling and routes it through Tonepoet's existing metadata worker/message architecture. The editor remains responsive while large WavPack carriers are rewritten. A real `MetadataWriteCancelFlag` is shared with the worker, copy progress is reported through the existing metadata-write progress message, and one typed outcome is returned for every confirmed target.

The correction also separates mutation state from verification state. A caller can now distinguish:

- no mutation;
- cancellation before commit;
- unknown commit state;
- committed and verified, including durability warnings; and
- committed but post-commit verification failed, including durability warnings.

The complete received source archive is included under `PROVENANCE/`, closing the package-level preimage audit gap. That archive and its internal SHA-256 manifest establish the exact bytes received. They do not prove that those bytes came from Git commit `7843058`; that requires the complete repository's Git object database and is enforced by `RUN_FULL_ACCEPTANCE.sh`.

## 1. Foreground-blocking repair removed

### Production dispatch

`src/tui/keybindings.rs` now freezes the confirmed `(path, invalid-key-set)` snapshot, starts a metadata write generation, restores the editor overlay immediately, and launches the work through `tokio::task::spawn_blocking`.

The blocking worker owns:

- native APE inspection;
- bounded audio-prefix and suffix copying;
- replacement-tag construction;
- file and parent durability work;
- atomic replacement;
- Lofty read-back;
- strong native-row comparison; and
- the post-worker editor tag refresh.

No carrier parsing or byte rewriting remains in the confirmation handler.

### Cancellation and progress

The repair entry point now receives a real `MetadataWriteCancelFlag`. It checks cancellation before inspection and throughout the copy loops. Copies use a bounded 1 MiB buffer, and progress is emitted at phase transitions and approximately every 16 MiB rather than once per byte or buffer.

Progress phases are typed:

- inspecting;
- copying prefix;
- writing replacement tag;
- copying suffix;
- synchronizing replacement; and
- verifying commit.

Each worker progress update is sent through `AppMessage::MetadataEditorWriteProgress`, scoped by the metadata editor session and generation.

### Editor close and application quit

Closing the editor while a repair is active requests cancellation and retains editor ownership until the typed completion ledger arrives. It does not drop the editor while a worker may still commit.

Application quit behaves the same way: it signals the active repair cancel flag, keeps `should_quit` false, and waits for the completion message. The user may quit again only after the operation reconciles to a known or explicitly unknown state.

This is deliberately conservative. A worker cannot be force-aborted after it enters an atomic replacement boundary without losing commit-state knowledge.

## 2. Commit state is no longer conflated with verification state

### Low-level outcome model

`src/tui/probe.rs` adds `InvalidApeRepairOutcome`:

- `NotModifiedFailure`;
- `CancelledBeforeCommit`;
- `CommitStateUnknown`;
- `CommittedAndVerified`; and
- `CommittedButVerificationFailed`.

The commit-bearing variants carry `InvalidApeRepairCommit`, which includes removed keys and the existing `MetadataWriteCommitReport`.

The native writer's internal failure classification is now:

- `NotCommitted`; or
- `CommitStateUnknown`.

POSIX rename failure remains pre-commit. Windows replacement-call failure is treated conservatively as unknown because the platform call may have crossed the commit boundary before returning an error.

### App/message outcome model

`src/tui/app.rs` carries the same distinction through `MetadataEditorWriteOutcome::InvalidApeRepair`. Human-readable text is derived only after the typed result reaches the editor reducer.

A committed-but-unverified file is never counted or displayed as an ordinary failure. It is explicitly reported as changed, requires inspection before retry, invalidates cached probe facts, and preserves the recoverable warning until a successful refresh proves the new state.

A missing result, worker panic, or blocking-task join failure is classified as unknown commit state. The reducer never invents a not-modified result.

### Durability warnings

Durability warnings from `MetadataWriteCommitReport` now cross the worker boundary and are retained as per-file editor issues. They are included in the completion summary. The production caller no longer discards the commit report.

## 3. Confirmation authority and fail-closed behavior

The confirmation snapshot contains the exact invalid-key set for every selected carrier. The worker reparses immediately before mutation and compares sets order-independently. Any added, removed, or renamed invalid key refuses before mutation.

The repair still reuses the existing journaled native WavPack/APEv2 writer, file-identity admission, snapshot guards, atomic temporary replacement, and verification policy. It does not duplicate the write body.

Strong verification proves that every valid neutral APE row remains unchanged. The only permitted semantic difference is removal of the confirmed invalid-key items.

## 4. Regression pins added

Low-level repair pins in `src/tui/probe.rs`:

- `invalid_ape_repair_removes_only_invalid_items_and_restores_lofty_route`
- `invalid_ape_repair_honors_precommit_cancellation_without_mutation`
- `invalid_ape_repair_reports_bounded_copy_and_verification_progress`

Worker/message/lifecycle pins in `src/tui/keybindings.rs`:

- `repair_completion_preserves_commit_and_durability_distinctions`
- `committed_but_unverified_is_never_reported_as_an_unmodified_failure`
- `missing_worker_result_is_classified_as_unknown_commit_state`
- `stale_repair_completion_cannot_reconcile_a_newer_editor_operation`
- `cancelled_multi_file_worker_returns_one_precommit_result_per_target`
- `production_dispatch_returns_control_and_close_requests_cancellation`

Quit lifecycle pin in `src/tui/event_loop.rs`:

- `quit_requests_invalid_ape_repair_cancellation_and_keeps_editor_owned`

The full acceptance runner requires every named pin to appear in the workspace-test output.

## 5. Existing Round 9 work retained

The corrected overlay retains the prior Round 9 implementation for:

- neutral bounded APE parsing in `metadata_persistence.rs`;
- full-provenance pipeline fallback mapping;
- loud tag-read degradation and completed-queue warning counts;
- structural completion-order album batches independent of log settings;
- leading/trailing dot preservation and manual extension joining;
- opt-in Windows-portable final path assembly;
- Browse Info and Convert copy behavior;
- whole-field copy without selection;
- shared-clipboard paste precedence;
- typed issue collapse;
- truthful preset semantic disclosure;
- APE allocation clamping; and
- stale picker-completion gating.

The scope fences in the governing brief remain unchanged.

## 6. Provenance

`PROVENANCE/` contains:

- the exact received `corrective_round9_bundle.tar.gz`;
- the separately uploaded brief;
- the separately uploaded handoff readme;
- SHA-256 hashes for all three; and
- a complete source-archive internal-manifest verification record.

The uploaded documents are byte-identical to the copies inside the source archive. Every modified file's preimage is derived from the archive's imported Git baseline and recorded in `PREIMAGE_SHA256SUMS`. Every delivered complete-file postimage is recorded in `POSTIMAGE_SHA256SUMS`.

The installer rejects unknown bytes, symlinks, hard-linked targets, unsafe manifest paths, malformed/duplicate manifest entries, and payload hash mismatches. It supports exact mixed preimage/postimage recovery after interruption and is idempotent once all postimages are installed.

## 7. Validation status

Completed in this environment:

- received archive path/type safety inspection;
- all source-archive internal SHA-256 checks;
- uploaded/archived governing-document byte identity;
- `git diff --check`;
- lexical delimiter/comment/string validation over all 31 Rust files in the sparse delivery;
- lexical validation over 333 Rust files in a non-authoritative reconstructed full workspace;
- exhaustive 23-file preimage/postimage manifest generation;
- complete-file overlay application to a pristine source extraction;
- exact postimage verification;
- idempotent second application;
- mixed exact preimage/postimage recovery;
- unknown-preimage refusal without collateral mutation;
- symlink and hard-link target refusal;
- package path/type safety and payload hash verification; and
- Bash syntax validation for both delivery scripts.

Not executable in this environment:

- `cargo fmt`;
- Rust compilation or name/trait/borrow checking;
- `cargo test --workspace`;
- the retained seven-track Supertramp field exercise.

The environment contains no Cargo, rustc, rustfmt, dependency cache, or authoritative complete Round 9 workspace. A prior complete workspace was used only as non-authoritative scaffolding to inspect the existing worker/message seams and to run whole-tree lexical checks. It is not represented as build evidence or source provenance.

`RUN_FULL_ACCEPTANCE.sh` is mandatory before production handoff. It requires a clean complete repository containing commit `7843058`, verifies ancestry, applies the exact overlay, runs formatting, workspace all-target checking, and the complete workspace test suite without truncating output, verifies all named repair pins, and rechecks every postimage after each executable gate.

## 8. Remaining mandatory field gate

The retained broken Supertramp WavPack album must still demonstrate:

- one album output directory;
- all seven tracks in that directory;
- recovered correct metadata and per-track titles;
- preserved title dot runs;
- invalid-key disclosure;
- completion-order disclosure; and
- successful invalid-key repair with responsive progress/cancellation behavior.

Until both `RUN_FULL_ACCEPTANCE.sh` and the field fixture pass, the honest classification is: strong corrected static candidate, not production-certified.
