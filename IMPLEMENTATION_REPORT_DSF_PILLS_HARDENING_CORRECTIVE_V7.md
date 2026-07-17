# Implementation report: DSF pills hardening round — corrective audit v7

## Delivery status and source limitation

This archive supersedes corrective v6. It addresses the stale-configuration recovery blocker: ambiguous backup selection, nondurable restore ordering, link-following candidate inspection, and silently discarded enumeration or cleanup failures.

The available source artifact remains the 35-file delivery subset rather than the complete HEAD repository described by the brief. It contains no `.git`, `docs/`, or `CLAUDE.md`. I therefore could not perform the required commit-level adversarial review of `7eb466e`, `6a56090`, `bdb0a43`, and `afffe61`, nor inspect omitted call sites. That requirement remains outstanding for application to the actual full tree.

No Rust compiler or Cargo runner was available. I did not run `cargo fmt`, `cargo test --workspace`, a cold warning-free build, or `TONEPOET_REQUIRE_TOOLS=1 cargo test --test depth_format_matrix`; I do not claim those acceptance commands pass.

## Corrective v7 outcomes

### 1. Recovery no longer guesses among replacement backups

`recover_stale_config_artifacts()` no longer sorts replacement backups by optional filesystem modification time.

While holding the existing store lock, it now:

- propagates `read_dir()` failure and every individual directory-entry error;
- identifies matching temporary and replacement-backup artifacts deterministically;
- rejects symbolic links and all other non-regular artifact types using no-follow metadata;
- opens backup candidates without following the final link and validates pathname-to-handle identity;
- reads every backup candidate and requires it to be valid UTF-8 and deserialize as `TonepoetConfig`;
- sorts candidates only to produce deterministic diagnostics, never to choose authority; and
- when the target is absent, restores only if exactly one valid backup exists.

Two or more valid candidates are now an explicit ambiguous state. Recovery returns an error naming every candidate and leaves the target, backups, temporary files, secret journal, and credential store untouched.

A malformed or symlinked replacement backup likewise stops recovery without cleanup or secret reconciliation.

### 2. A restored configuration becomes durable before it controls credentials

A single valid backup is no longer moved directly into place and immediately trusted.

Recovery now:

1. creates a unique owner-only recovery temporary file;
2. writes the validated backup bytes;
3. calls `sync_all()` on that file;
4. renames it to the missing target;
5. calls the publication-specific parent-directory durability barrier; and only then
6. considers removal of the original backup or any stale temporary file.

If the post-rename directory barrier fails, recovery returns an exact error stating that durability is unconfirmed. The original replacement backup is retained. Because `load_from_path()` invokes recovery before `reconcile_config_secret_publication_locked()`, the pending secret-publication journal and every referenced credential remain untouched.

Before deleting stale artifacts, recovery also requires the parent publication barrier to be available. This makes unsupported platforms fail before deletion rather than removing the only recovery authority and then reporting that durability could not be established. After deletion, the parent directory is synchronized again. Secret reconciliation begins only after both the target publication and all recovery cleanup have completed durably.

### 3. Cleanup and inspection failures are authoritative

Recovery no longer uses `entries.flatten()`, `let _ = remove_file(...)`, `Path::exists()`, or optional timestamp metadata in its decision path.

It now propagates:

- directory enumeration failures;
- individual entry failures;
- no-follow metadata failures;
- candidate open, identity, read, UTF-8, and TOML-validation failures;
- recovery temporary creation, write, file-sync, rename, and cleanup failures;
- stale temporary or obsolete backup removal failures; and
- both pre-cleanup and post-cleanup directory-durability failures.

A failed recovery-temporary cleanup is combined with the primary publication error rather than replacing or hiding it.

### 4. Existing published targets remain authoritative only when valid

When a regular target already exists and replacement backups are present, recovery validates the published target before deleting any backup. A malformed or non-regular target therefore cannot cause valid recovery authority to be silently discarded. Once the target parses, it is the authoritative published state and stale backups may be removed under the durable cleanup protocol.

## Tests added or strengthened in corrective v7

- `two_valid_replacement_backups_fail_closed_without_selecting_by_timestamp` — creates two different valid configurations and asserts the exact ambiguity error, absent target, and retention of both candidates.
- `malformed_replacement_backup_is_rejected_without_cleanup` — asserts the exact validation error and exact retained malformed bytes.
- `symlinked_replacement_backup_is_rejected_without_following_target` — Unix boundary pin proving the link and its target remain unchanged.
- `recovery_sync_failure_retains_backup_journal_and_credential_authority` — injects failure at the directory barrier after recovery rename and asserts the exact error, retained original backup, retained pending journal, exact surviving secret, and credential count.
- `stale_temporary_cleanup_failure_is_visible_and_retains_secret_authority` — injects a removal failure and asserts the exact error, exact stale bytes, retained journal, exact secret, and credential count.
- `single_valid_backup_is_restored_durably_before_cleanup` — asserts exact restored configuration bytes and removal of the backup only after successful durable recovery.
- `config_save_removes_stale_temporary_files` — now uses a syntactically and structurally valid replacement backup, so the test exercises the actual validation and recovery policy rather than relying on arbitrary bytes.

The stale-artifact cleanup tests that require a successful parent-directory durability barrier are Unix-only. Ambiguity and malformed-candidate validation remain cross-platform. No behavior assertion was weakened or removed.

## Files changed in corrective v7

- `src/config.rs` — replace timestamp-based stale recovery with fail-closed candidate validation; add no-follow regular-file and identity checks; implement durable copy-and-publish recovery; make every enumeration, validation, cleanup, and directory-sync failure visible; add fault injection and value-level recovery tests.
- `IMPLEMENTATION_REPORT_DSF_PILLS_HARDENING.md` — this corrective v7 report.

All other files are retained complete and unchanged from corrective v6 so the archive remains directly applicable as the same complete-file delivery surface.

## Earlier corrective work retained

The archive retains the v1-v6 corrections, including:

- actual DSF container I/O through generic `id3` stream APIs;
- DSF structural, pointer, ID3-extent, and trailing-data validation;
- atomic DSF publication and refusal to mutate through symbolic links;
- persistent native keyring feature selection and production mock-backend rejection;
- whole-transaction config and MRU store locking;
- crash-recoverable pending secret publication;
- deterministic config credential slots and queue migration references;
- retryable transient keychain failures;
- explicit set, retain, and clear config-secret semantics;
- safe `fs2` locking with immutable validated lock markers and no handwritten FFI;
- strict public configuration save behavior on unconfirmed durability;
- conservative Windows publication classification;
- complete bounded configuration-symlink-chain resolution;
- metadata-file durability before rollback-journal commit;
- production-shaped R8 coverage;
- behavioral ReplayGain runner-count tests;
- fail-closed DSF materializer metadata errors;
- no-clobber metadata rollback authority and state-aware recovery;
- source-relative format-pill corrections; and
- the supplied implementations for R1-R11, F1-F3, and H1-H3 within the 35-file surface.

Integration with files absent from the supplied artifact remains unreviewed.

## P1 recommendation

Recommendation remains **(b): suppress source `.cue` companions for Tracks-layout output**.

Keeping source cues preserves provenance but publishes `FILE` references to images absent from the output. Suppression is the smallest reversible change and leaves the generated per-track cue authoritative. Rewriting could preserve richer provenance but needs a separately designed reconciliation path.

P1 was not implemented. The brief leaves the decision open and permits implementation only for option (b); this delivery records the recommendation without silently changing product behavior.

## Assumptions and deliberate limits

- Exactly one valid replacement backup is authoritative only when the published target is absent. Multiple plausible backups are never ordered by timestamps or guessed from contents.
- A present, regular, parseable target is the published authority. Backups may be cleaned only after that target has been validated.
- Recovery backups are copied into an owner-only temporary file rather than renamed directly, so the original authority remains available until publication durability is established.
- Stale temporary files are not candidate configuration authority; they must be regular files and are removed only under the durable cleanup protocol. Replacement backups and any published target used to resolve them must parse as `TonepoetConfig`.
- Platforms without a supported parent-directory publication barrier cannot complete stale-artifact deletion or backup recovery. They fail visibly and retain recovery authority rather than claiming completion.
- General cross-store secret garbage collection remains outside this round. Recovery only controls credentials through the already journaled store transaction.
- No DSF artwork mutation, DFF tagging, or P1 product behavior change was added.
- The missing complete repository and Git history were not reconstructed or guessed.

## Static verification performed

Without a Rust toolchain, verification was by inspection and structural checks:

- inspected every call to `recover_stale_config_artifacts()` and confirmed recovery precedes secret reconciliation;
- confirmed timestamp ordering, flattened directory entries, and ignored recovery removals are absent;
- confirmed matching backup candidates are opened with final-component no-follow behavior and checked as regular files;
- traced the exact states for zero, one, and multiple backups; valid, malformed, and symlinked candidates; target-present cleanup; post-rename sync failure; stale-removal failure; and retry after an unconfirmed restore;
- confirmed the original backup remains after a post-rename sync failure;
- confirmed cleanup is preceded and followed by the publication-specific parent-directory barrier;
- confirmed all newly added `expect` calls are test-only and no new production `unsafe`, FFI, `panic!`, `unwrap()`, or `expect()` was introduced;
- parsed `Cargo.toml` with Python's TOML parser;
- checked the changed Rust file for balanced lexical delimiters and all text files for trailing whitespace; and
- extracted the final archive and compared every included file byte-for-byte against the delivery tree.

These checks do not substitute for Rust compilation or the acceptance suite.

## Complete files included in the archive

The archive retains the complete 35-file delivery surface from corrective v6, including this report and all previously delivered complete source files. No partial patches, elisions, or “rest unchanged” markers are included.
