# Implementation Report — One pending edit set per archive, committed once

**Date:** 2026-09-01  
**Brief:** `BRIEF_archive_pending_edit_set_2026-09-01.md`  
**Input base:** supplied `tonepoet_archive_pending_edit_set_2026-09-01_bundle.tar.gz`

## Result

Implemented the brief's archive commit model without replacing the existing archive transaction machinery.

All Browse archive edits now participate in one deferred pending set per archive. Native rename no longer installs an archive immediately. Rename-only sets stay logical and preserve the native fast path until a real commit trigger; mixed sets materialize once, replay the pending rename journal into that tree, and then continue through the existing staging/repackage transaction.

The established commit triggers remain the triggers: archive exit/navigation, screen switch, and quit. N edits therefore produce one archive commit, not N per-edit archive rewrites.

## Design decision: lazy logical staging

The central tradeoff in the brief is retained rather than erased:

- extraction-backed staging can express every edit type;
- native rename is substantially cheaper because it can mutate container metadata without extracting payloads.

A new `ArchiveStagingSession::tree_materialized` state distinguishes these cases.

### Rename-only pending sets

A native-capable, unencrypted rename now:

1. creates the normal durable pending-session recovery row;
2. creates a durable sibling marker identifying the staging session as **logical** rather than extracted;
3. appends the `ArchiveEdit::Rename` to the existing edit journal;
4. updates Browse from a projected archive listing;
5. reports `archive changes pending`;
6. does **not** write the archive.

The logical marker is written by temp-file + file sync + rename + parent-directory sync. Removal is idempotent. It is the crash-safe source of truth; `tree_materialized` is only the in-memory fast-path state.

### Commit-time native batching

At the shared archive save boundary, `pending_native_archive_rename_plan()` inspects a rename-only journal:

- independent 7z/ZIP renames are emitted as one native multi-pair transaction;
- an implicit-directory rename expands to its real descendant members rather than inventing an archive member;
- ISO-WV remains native only for one effective pair, matching the existing xorriso constraint;
- overlapping/cascaded rename journals conservatively decline the native batch and materialize once instead, preserving journal order and avoiding command-order ambiguity.

Thus repeated independent renames preserve the no-extraction fast path while still producing one commit.

## Mixed pending sets

If an operation that requires filesystem content follows one or more logical renames — delete, create, tags/artwork/ReplayGain metadata work, or a rename shape that cannot safely be expressed as one native batch — `materialize_logical_archive_staging_with_progress()` performs one promotion:

1. requires the durable logical marker;
2. validates the pending edit journal is rename-only at promotion time;
3. clears any stale partial staging/localization left by an interrupted attempt;
4. extracts the unchanged source archive once through the established password/locality-aware extractor;
5. rechecks the source fingerprint after the long extraction;
6. replays pending renames into the extracted tree in journal order, including existing ISO-WV CUE-reference repair behavior;
7. removes the logical marker only after replay completes.

After promotion, the existing operation continues against that same `ArchiveStagingSession` and appends its edit to the same journal. Promotion failure leaves the logical marker authoritative, so retry/recovery re-extracts rather than trusting a partial tree.

## Logical Browse projection

A logical pending set has no extracted tree, so Browse cannot derive the staged namespace from the filesystem. A narrow projection layer now applies structural `ArchiveEdit` entries to the real archive listing for:

- Browse rows and recursive navigation;
- existence checks;
- selection/search;
- metadata target collection, including implicit directory prefixes;
- mapping a renamed current path back to the original archive member for read-only probes.

This is intentionally limited to the existing archive edit/staging model and does not implement the larger parked virtual-view redesign.

## One pending owner per archive

The recovery database has one pending-session row per archive. Two Browse tabs must therefore not create competing staging owners for the same archive.

Browse now identifies the tab that owns a pending set for a given archive. Another tab may still view that archive, but edit entry points refuse to create a second pending owner and tell the user to finish/save in the owning tab. This preserves the brief's one-set-per-archive invariant without redesigning tab state sharing.

Different archives remain independent. Screen-switch handling now drains every Browse tab with pending archive staging before Browse is left, serializing one commit per archive. Quit already re-enters its preflight until pending tab-owned staging is drained.

## Edit-operation serialization

A submitted inline archive metadata write can outlive the initiating key event. Archive exit, screen switch, quit, and structural edit entry points now wait for that already-submitted operation rather than allowing commit/materialization to race the writer.

Preparation over an existing logical set is also serialized with structural edits. This uses the existing pending-operation state and cancellation tokens; no new global mutex or scheduler was added.

## Shared commit transaction remains authoritative

The final TUI source audit has exactly two production calls that can write an archive:

- `rename_archive_entry_native_transactional(...)`
- `repackage_archive_with_progress_and_cancel_with_password(...)`

Both are in `src/tui/event_loop.rs` inside the shared deferred archive commit worker. No Browse edit entry point invokes an archive-write primitive directly.

The existing exact archive mutation claim, final fingerprint conflict check, backup/install/restore transaction, cancellation boundary, recovery ownership, and cache/probe invalidation remain the commit boundary.

## Readback verification

The brief requires successful commit verification through Tonepoet's real reader, not merely the writer that produced the container.

The supplied base already enforced that correction for RAR. This delivery applies the same invariant to the remaining writer-specific verification paths affected by shared commits:

- TAR/TAR.GZ still write with `tar`, but preflight now also requires 7z/7zz and verification uses `7z l -slt`;
- ISO-WV still writes/renames with `xorriso`, but preflight now also requires 7z/7zz and verification uses `7z l -slt`;
- native 7z/ZIP verification adds the standard `--` switch terminator before the archive path.

This does not add payload decoding to the native rename path; structured listing remains header-oriented and keeps the performance rationale for the fast path.

## Crash safety and idempotency

Key crash windows were handled explicitly:

- **Logical marker exists, extraction absent/partial:** recovery treats the session as logical and re-materializes from the source archive.
- **Promotion completes but marker removal is not durably recorded:** recovery may conservatively re-materialize and replay, rather than trust stale staged bytes.
- **Native commit installs successfully but cleanup/recovery-row deletion is interrupted:** on restart, the fresh archive listing already contains the renamed paths; net-change reconciliation makes the journal clean rather than blindly committing the rename again.
- **Materialization or mixed-edit preparation is cancelled:** staging ownership is retained when prior user edits exist, so the previous pending set remains saveable/recoverable.

Passwords remain transient; the logical recovery marker and recovery row contain no password material.

## Regression coverage added

Focused tests cover the new semantics, including:

- logical listing projection and reverse original-path mapping;
- logical marker create/rewrite/clear idempotency;
- independent pending renames forming one native batch;
- cascaded renames declining the unsafe native batch;
- implicit ZIP directory rename expansion;
- implicit-directory logical rename followed by metadata target collection;
- quit/archive-exit waiting for already-submitted archive metadata work;
- logical inline metadata write surviving deferred navigation;
- net-clean deferred metadata navigation avoiding repackage;
- screen switch serializing pending sets across two Browse tabs;
- stale/detached async ownership behavior;
- TAR and ISO-WV preflight requiring Tonepoet's actual reader for verification.

## Files changed

Production source changes are limited to five files:

- `src/convert/pipeline/materializer_archive.rs`
- `src/tui/app.rs`
- `src/tui/browse.rs`
- `src/tui/event_loop.rs`
- `src/tui/keybindings.rs`

Delivery documentation adds this report and `PATCH_archive_pending_edit_set_2026-09-01.diff`.

## Validation performed in this container

The supplied brief states that the implementation container has no Rust toolchain, Nix, or archive tools. That is true here:

- `rustc`: unavailable
- `cargo`: unavailable
- `rustfmt`: unavailable
- `nix`: unavailable
- `7z` / `7zz`: unavailable
- `xorriso`: unavailable

Accordingly, **this delivery has not been compiled and the Rust/archive-tool acceptance gate has not been run**. No claim to the contrary is made.

Available source-level checks performed:

- all five changed Rust files pass a lexical delimiter-balance check that ignores comments/string/raw-string content;
- no trailing whitespace was introduced in the changed source files;
- changed-source inventory confirms exactly the five files listed above;
- production TUI archive-write call-site audit finds only the two shared-commit calls described above;
- no new password value is interpolated into added status/log text;
- added UI status/message text is plain ASCII;
- Python audit scripts compile with `python3 -m py_compile`.

Two repository audit scripts report failures, but comparison against the pristine supplied input confirms both are pre-existing baseline findings, not regressions from this implementation:

1. `tools/audit_concurrent_mutation_entrypoints.py` reports the same existing incomplete external-launch inventory (`materializer_archive.rs`: 3, `tool.rs`: 1).
2. `tools/audit_test_coordination_isolation.py` reports the same four existing unscoped permanent-delete tests; only their line numbers shift because this implementation adds code earlier in `keybindings.rs`.

Those baseline issues were intentionally not expanded into this brief.

## Required operator gate

Per `CLAUDE.md`, use the project Nix environment rather than system Rust:

```bash
nix develop --extra-experimental-features 'nix-command flakes'
cargo check
cargo test --workspace
```

For full package/tool acceptance where required by the existing suite:

```bash
nix build --extra-experimental-features 'nix-command flakes'
```

The operator should inspect every workspace `test result:` line and require zero failures, per the repository instructions.
