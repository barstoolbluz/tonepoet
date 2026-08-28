# Implementation report — large-job coordination failures

Date: 2026-08-27  
Baseline: `main` @ `1c8d87d` (v0.4.9)  
Status: **all four requested code outcomes implemented; required Nix test gate not executable in this authoring environment**

## Scope

The source tree, not the brief's suggested implementation details, was treated as authoritative. The production change is confined to four files:

- `src/concurrency.rs`
- `src/db.rs`
- `src/convert/cap_fs.rs`
- `src/convert/pipeline/stages.rs`

No worker-pool serialization, new coordination service, or broad pipeline redesign was introduced.

## Item 1 — transient registry contention no longer terminates a track

### Finding

`RegistryLock::acquire` used repeated `try_lock_exclusive` calls with a fixed 250 ms deadline and then returned `coordination registry busy; retry the operation`. The caller treated that error as terminal; there was no retry above it.

The hot mutation-admission path also scanned every coordination family, even though `QueueScope` and `QueueExecution` descriptors are lifecycle reservations and carry no filesystem conflict claims in this tree.

### Change

- Replaced the 250 ms polling/deadline loop with the filesystem lock's blocking exclusive acquisition. `Interrupted` is retried; genuine lock errors remain errors. There is no arbitrary job-size cliff and no misleading “retry” text.
- Added an explicit `LeaseFamily::carries_path_claims()` invariant.
- Enforced that `QueueScope` and `QueueExecution` cannot be created with path claims.
- Changed mutation admission to skip those two claimless queue-lifecycle namespaces entirely while continuing to scan all existing/future claim-bearing namespaces fail-closed.
- Kept the global registry critical section and conflict semantics otherwise intact.

This is intentionally smaller than sharding the registry. Blocking is the correctness fix; excluding provably claimless high-churn families removes the dominant irrelevant scan work without changing conflict semantics.

### Regression coverage

`registry_contention_waits_for_holder_instead_of_timing_out` uses eight contenders (the field worker count), deliberately holds the registry longer than the former 250 ms budget, and requires every contender to acquire successfully after release. This exercises real contention and would fail under the baseline timeout behavior.

`mutation_admission_ignores_claimless_queue_lifecycle_descriptors` proves that even malformed historical queue-lifecycle state is absent from mutation-claim admission cost. `queue_lifecycle_descriptors_reject_path_claims` locks in the invariant that makes that optimization safe.

## Item 2 — queue coordination state no longer grows across dead empty sessions

### Finding

There are two relevant lifecycle shapes:

1. A queue can drain to zero items while its `conversion_queue_scopes` row and durable `QueueScope` descriptor remain. `load_queue_items` returned early on an empty queue, before ordinary dead-scope recovery, so these empty dead scopes could survive indefinitely.
2. Descriptor-only setup orphans already have a safe cleanup path, but historical ext4 directory size remains after entries are unlinked.

This confirms the brief's central observation that queue lifecycle history can persist, but the useful correction is lifecycle cleanup at cold queue-session boundaries, not more work in per-track teardown.

### Change

- Added targeted reclamation for dead **empty** queue scopes. A candidate is considered only when it has neither queue-item rows nor queue-execution rows.
- A live descriptor is left untouched.
- An abandoned descriptor must first be acquired through the existing recovery lease protocol. The database emptiness condition is then re-checked inside an IMMEDIATE transaction before the scope row is deleted.
- The recovery lease is released only after the DB transition commits; descriptor retirement follows using the existing lifecycle retirement path.
- Missing, malformed, unexpectedly typed, or otherwise ambiguous ownership is left fail-closed rather than guessed away.
- The cleanup runs before the empty `load_queue_items` fast return **and** before creating any new queue scope. The second location matters for CLI conversion, which can enter through `sync_queue` without first loading the durable queue.
- At creation of a new `QueueScope` (a once-per-active-session cold boundary), the code best-effort resets the `queue-scope` and `queue-execution` family directories only if they are empty. This replaces historically enlarged ext4 directory inodes without adding recreation/fsync work to every track retirement.
- Lock-free queue-family readers now treat a concurrently absent empty family directory as empty, preserving safe behavior across that reset.

Queue lifecycle directories are also excluded from mutation admission by Item 1, so legitimate crash-recovery state cannot recreate the original per-track scan amplification while it remains recoverable.

### Regression coverage

`empty_dead_queue_scope_is_reclaimed_but_live_empty_scope_is_preserved` uses real lease state: while the owner DB remains alive, another DB sees the empty scope as live and must not reap it; after the owner is dropped, a subsequent load must remove both the empty scope row and descriptor.

`new_queue_scope_reclaims_abandoned_empty_scope_without_prior_load` models the CLI route: a prior scope is drained and abandoned, then a fresh DB publishes directly through `sync_queue`; the old scope row and descriptor must be retired without any preceding `load_queue_items` call.

`empty_queue_family_reset_recreates_only_empty_directories` verifies that compaction recreates an empty family directory and refuses to remove a non-empty one.

## Item 3 — same-album publication `ENOENT` race repaired

### Finding that narrows the brief's hypothesis

The brief suspected `remove_empty_action_authority_dirs_best_effort`. The tree exposes a more direct race that matches the exact field error.

Before acquiring the shared `.<album>.lock`, `open_album_parent_capability` inventories the album parent so it can canonicalize the album component. `PinnedDirectoryCapability::list_entries` performed:

1. `readdir` to obtain a sibling entry name;
2. `fstatat(..., AT_SYMLINK_NOFOLLOW)` on that name.

Directory enumeration is not a snapshot. Another publisher can legitimately finish, remove its shared publication-lock directory entry while still holding the locked inode, and release it between those two syscalls. The `fstatat` then returns `ENOENT`. Baseline `list_entries` promoted that benign disappearance into `ActionError::Io`, producing the observed “could not acquire descriptor-bound album publication authority ... No such file or directory”.

The publication lock itself already defends against inode replacement: waiters revalidate the opened lock inode against the current directory entry and retry if it was unlinked/replaced. That existing protocol does not need redesign.

### Change

`list_entries` now treats only `ENOENT` after `readdir` as “this entry disappeared; continue”. Every other `fstatat` error remains fatal and diagnostic. The early-error path closes the DIR stream before returning.

### Regression coverage

`list_entries_tolerates_entry_removed_after_readdir` uses the existing race-hook mechanism to delete the sole directory entry after `readdir` returns it but before `fstatat`. Enumeration must succeed with an empty result instead of returning `ENOENT`. This deterministically exercises the race rather than asserting a happy path.

## Item 4 — terminal stage failures now leave an operator-log record

### Finding

Stage failures were preserved in the queue/TUI report, but there was no centralized terminal-stage WARN path. Only a narrower track-encode failure path logged, which explains why the field `PreActions` and `Publish` failures left no useful operator record.

### Change

Final report assembly now emits a WARN for each failed terminal stage only. It does **not** log ordinary stage transitions.

The record contains:

- job id;
- item/track id;
- stage;
- source container path;
- output root;
- album directory when available (from the plan, or from the submitted album batch when planning did not complete);
- the original error text, which carries the lock/path-specific detail.

Values are formatted with Rust debug escaping so embedded newlines/control characters do not forge extra operator-log lines.

### Regression coverage

`terminal_stage_failure_log_contains_request_and_stage_context` checks Publish-stage identity/path/error context, escaping, and verifies that an `Ok` stage generates no failure record.

A call-site audit confirmed normal terminal pipeline reports flow through `finalize_report` / `finalize_report_with_binding`, where this logging now lives. The separate scratch-retry report constructor is an intermediate retry attempt, not a terminal item result, so logging it here would create misleading duplicate failure records.

## Performance and integration decisions

The correction deliberately avoids three tempting but expensive changes:

- no larger fixed registry timeout;
- no batch-wide serialization;
- no per-track directory compaction or new coordination framework.

The mutation hot path does less work than baseline because claimless queue directories are no longer scanned. Blocking registry acquisition removes retry polling and cannot leave work terminally failed merely because another valid critical section exceeded 250 ms. Empty-scope reclamation does SQLite writes only for safely reclaimable stale candidates. Historical queue-directory compaction runs only when a new queue scope starts and only if the family directory is empty.

## Validation performed here

The work order requires this exact environment and gate:

```text
nix develop --extra-experimental-features 'nix-command flakes'
cargo test --workspace --no-fail-fast
```

and requires the workspace test to pass twice, with every `test result:` line showing `0 failed`.

This sandbox does **not** provide `nix` (nor `cargo`, `rustc`, or `rustfmt` on PATH). The exact gate invocation therefore fails before entering the dev shell:

```text
$ nix develop --extra-experimental-features 'nix-command flakes' --command cargo test --workspace --no-fail-fast
bash: line 4: nix: command not found
exit=127
```

Network/DNS access is unavailable as well, so the repository's Nix environment cannot be bootstrapped here. I did not substitute a system Cargo installation because the work order explicitly forbids building outside the Nix dev shell. Therefore this bundle is intentionally named `CORRECTED_UNCERTIFIED`: I am **not** claiming the two required workspace runs, compiler-warning status, formatting-tool status, or test certification.

### Source/static checks completed

- Production source diff is confined to the four files listed above.
- `git diff --no-index --check` reports no whitespace errors for the four source changes.
- The obsolete 250 ms registry constants and misleading `coordination registry busy; retry the operation` string are absent from corrected source.
- Existing repository static concurrency verifiers `round5`, `round6`, `round6_r1`, and `round7` pass on both baseline and corrected trees.
- `round1` and `round4` each retain one pre-existing static failure, reproduced identically on baseline (`large scheduler child future is boxed`; `quiescent v23 activation regression exists`).
- `audit_concurrent_mutation_entrypoints.py` and `audit_test_coordination_isolation.py` also retain baseline-identical pre-existing findings. They are outside this work order and were not broadened into unrelated fixes.
- The generated source patch is dry-run/apply checked against a fresh baseline copy, and application reproduces the four corrected source files byte-for-byte.
- `git apply --check --whitespace=error-all` passes for the generated patch.

## Required handoff gate

Before merge certification, run inside the repository's Nix development shell:

```bash
nix develop --extra-experimental-features 'nix-command flakes'
cargo test --workspace --no-fail-fast
cargo test --workspace --no-fail-fast
```

Every `test result:` line in both runs must report `0 failed`, and the build must introduce no new compiler warnings. If that gate exposes a compile/test issue, correct only the demonstrated issue; do not broaden the architecture on speculation.
