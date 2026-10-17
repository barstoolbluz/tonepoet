# Issue #59 — cross-instance metadata-save conflict: diagnosis and narrowly scoped delivery

**Delivery:** October 17, 2026. **Input:** `TonePoet_LOGGING_59_second_instance_2026-10-17.tar.gz`, a 0.5.5 source bundle. **Source change:** `src/concurrency.rs` only. **Status:** compile-free verified; **build-host acceptance required**. Historical root cause is not established from surviving evidence.

## What the code proves

1. `QueueScope` / `QueueExecution` never admit path claims (`LeaseFamily::carries_path_claims`, `PersistentLease::create_while_registry_locked`). Mutation admission does not even scan these two lifecycle directories (`claim_descriptor_paths`). An unrelated idle queue scope cannot *by itself* exclude a metadata write.
2. `MutationClaimGuard::acquire_grouped_internal` only denies a request when an actual published descriptor with claims is not excluded by same-execution/cohort rules, is not a reclaimable ephemeral, and `first_conflict` reports overlapping claims. This may be a live holder or, after the holder disappears, an intentional recovery reservation.
3. The 97-minute-old, childless, socket-only `__execution-item-supervisor` is **not evidence of a leaked execution**. `src/convert/script_supervisor.rs` deliberately caches a *shared unleased* command supervisor started with zero lease FDs. A per-item supervisor is different: it receives queue and path lease descriptors until terminality.
4. A real possibility deserving investigation is broad or longer-lived `ExecutionClaim` data. Automatic conversion actions can claim `context.output_root` with `Write/Subtree` (notably user scripts), and `register_runtime_supplemental_lease` retains their descriptors until item-execution terminality. This is intentionally tied to durable recovery and contained worker lifetime. A descriptor from the historical failure was not preserved; **we cannot prove such an action was involved**, nor safely shorten its authority speculatively.
5. Metadata batch save admission (`src/tui/probe.rs` and `src/tui/keybindings.rs`) already resolves the affected member/sidecar paths to *exact* mutation claims, rather than claiming the whole album. Widening the save admission is unnecessary and harmful.

**Decision:** Preserve collision rules and lifecycle/recovery authority. Correct the demonstrated forensic gap so a future conflict identifies its exact cause. Do not claim that this retroactively proves or eliminates every cause of the historical report.

## Changes

- Retain full immutable owner `LeaseDescriptor` provenance through classification instead of discarding it before collision detection.
- Emit a single **WARN**-level `filesystem mutation claim conflict` diagnostic for each rejected conflicting admission; it contains requested and held original paths, normalized and namespace paths, claim modes/scopes, live versus recovery-reserved status, owner family/execution UUID, descriptor ID/path, descriptor creation timestamp, origin PID/process-start ticks/boot identity hash, and cohort.
- Use `Debug` formatting for filesystem paths in the log, escaping newlines and preserving non-UTF-8 representations as far as Rust `Path` debug formatting permits.
- Log path-bearing descriptor registration at **INFO**, including claim count and a bounded set of exact paths (first eight; all broad `Subtree` claims); additional exact claims appear at **DEBUG**. No log data is truncated on the actual conflicting path: the WARN event is complete regardless of index.
- Log explicit post-terminal retirement of path-bearing durable descriptors at INFO.
- Keep log writes **outside the global coordination registry lock**. Ordinary uncontended exclusion comparisons, path normalization, on-disk schema, owner lock semantics, and claim scope are unchanged.
- Three focused unit tests assert that idle claimless sessions permit DSF metadata admission; live subtree claims on a sibling album permit admission; and a real ancestor subtree conflict still fails with a diagnostic preserving the holder's execution and paths.

No new crates, background threads, migrations, policy exceptions, schema changes, global process logging configuration, or changes in source-locked DSP/Reference files.

## Diagnosing a recurrence

Run the TUI with default or explicit `RUST_LOG=info` (or more verbose). The standard TUI appends to `~/.cache/tonepoet/tonepoet.log` on a standard Unix environment (`src/main.rs`); the actual cache location is platform dependent.

1. Find the `filesystem mutation claim conflict:` WARN event at the time of save failure. Read `held_path`, `held_scope`, `owner_family`, `owner_lifecycle`, `owner_descriptor_id`, `owner_created_unix_ms`, and `availability`.
2. Search by `owner_descriptor_id` for a prior `filesystem mutation claim lease registered:` event and its `filesystem mutation claim registered:` path entries. Search by `owner_lifecycle` for the corresponding conversion/journal state. Also check `origin_pid` and `origin_start_ticks`, bearing in mind the originator may have delegated lease authority to an item supervisor.
3. If `held_scope=Subtree` names a broad source/library root, identify the action that reserved that root. If the active operation has finished but the descriptor is still `Live`, inspect the per-item supervisor/terminality handoff. If `availability=RecoveryReserved`, inspect interrupted-operation reconciliation instead of killing or deleting a descriptor.
4. Preserve the application log, including the *entire* WARN line; the TUI status bar is intentionally not a complete forensic record.

## Acceptance and limits

The source brief reported a clean **7,526 passed / 0 failed** gate **for the input baseline**; that result is not a claim about this patch. This environment has no `cargo`, `rustc`, or `nix`, so no Rust typecheck, native tests, Reference qualification, or real two-TUI runtime reproduction was possible here.

On a qualified build host, enter the project's Nix environment and run:

```bash
nix develop --extra-experimental-features 'nix-command flakes'
cargo test -p tonepoet --lib issue_59 -- --nocapture
cargo check --workspace --locked
cargo test --workspace --locked --no-fail-fast --exclude tonepoet-true-peak
```

Run any separately required Reference/true-peak host qualification specified by the existing project contract; none is silently waived. For a genuine runtime repro, start two TUIs, keep the first idle with an unrelated album, edit nine DSFs in the second, save, and confirm no conflict; separately run an overlapping conversion/action in the first and verify the second's save is correctly denied **and** its complete WARN provenance is in the application log.

## Package integrity

`ISSUE59_SOURCE_TREE_SHA256SUMS.txt` is the **current authoritative SHA-256 manifest**, covering all package files except itself. Verify from the package root with `sha256sum --check ISSUE59_SOURCE_TREE_SHA256SUMS.txt`.

The original root `SHA256SUMS` file is preserved **without changes**, but was already stale on the input snapshot for **18 files**; after changing `src/concurrency.rs` there are 19 mismatches. Do not use it to validate this derived archive. Rewriting unrelated historical receipts would disguise a baseline issue rather than correct it.

`ISSUE59_CODE_DELTA.patch` is an independently replayable patch against the supplied input's `src/concurrency.rs`. No other preexisting source files were edited.
