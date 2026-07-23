# Metadata transaction liveness, carrier identity, and recovery hardening

Date: 2026-07-22

## Scope

This corrective round addresses four findings in the generic Lofty full-file metadata transaction:

1. startup recovery could treat a live `prepared` transaction as abandoned;
2. canonical pathname authority did not unify hard links and pathname-local rollback could split aliases;
3. journal paths were persisted through lossy display strings;
4. full-carrier rollback artifacts could inherit permissions wider than the source carrier.

The startup call sites in `src/main.rs` and `src/tui/event_loop.rs` remain enabled. Safety is established inside `Database::recover_stale_metadata_writes()`, so every caller receives the same liveness-aware behavior.

## Implemented corrections

### Held cross-process liveness authority

Every generic transaction now derives a persistent per-carrier lock sidecar and acquires an exclusive OS file lock before inspecting or creating journal state. An in-process registry complements the OS lock because advisory-lock semantics can be process-scoped on some platforms.

The lock handle is held from transaction claim through terminal marker and journal cleanup. `RetainedMetadataWrite` owns the handle for its complete lifetime, including multi-file artwork operations. Dropping an abandoned retained claim releases only liveness; its `prepared` journal and rollback marker remain available for recovery.

Startup recovery attempts the same lock non-blockingly for each journal row:

- held lock: skip the row without changing the carrier, marker, or journal;
- acquired lock: no live owner remains, so allocating cleanup, rollback, or terminal cleanup may proceed.

No PID, timestamp, or expiry heuristic participates in the decision.

### Fail-closed hard-link policy

On Unix, generic full-file mutation rejects a carrier whose link count is greater than one. The check runs:

- before transaction authority is claimed;
- again immediately before entering `prepared` state; and
- before pathname-local rollback replacement.

This prevents two hard-link names from receiving independent transaction authority and prevents rollback from silently replacing one name with a new inode while another alias retains failed-transaction bytes. Symlink aliases remain unified through canonicalization.

### Lossless persistent paths

New metadata journal rows use a self-describing native path encoding:

- Unix: exact `OsStr` bytes encoded as hexadecimal;
- Windows: exact UTF-16 code units encoded as little-endian hexadecimal;
- other platforms: an explicitly tagged UTF-8 fallback.

Recovery decodes the stored native representation back to `PathBuf`. Legacy UTF-8 rows remain readable. Human-readable display conversion is now confined to diagnostics and compatibility views; it is no longer the recovery authority for new rows.

### Owner-only recovery artifacts

Unique transaction markers and the retained legacy backup helper now create files with owner-only permissions on Unix and reapply mode `0600` explicitly after open. Both paths use `O_NOFOLLOW`; unsafe artifacts are removed if permission hardening fails. Persistent liveness sidecars are likewise validated as regular, single-link files and forced to mode `0600`.

## Regression coverage added

`src/db.rs` now includes tests for:

- startup recovery while an ordinary writer is paused between its authoritative fresh read and save;
- startup recovery while a retained transaction is live in the same process;
- a second process running startup recovery while another process owns a live retained transaction;
- rejection of both names of a hard-linked carrier;
- refusal to perform stale rollback after a hard link appears, preserving the marker and journal;
- exact non-UTF-8 Unix path round-trip through persistent journal recovery;
- `0600` rollback-marker and liveness-sidecar modes.

The pre-existing file-backed authority, symlink-alias, rollback-versus-later-commit, durability, and retained-artwork tests remain in place.

## Verification performed in this environment

- inspected all generic metadata journal insertion, lookup, recovery, commit, and rollback paths;
- confirmed production journal insertion uses native path encoding;
- confirmed recovery acquires liveness authority before acting on every row;
- confirmed retained transactions own the lock handle until terminal cleanup or abandonment;
- confirmed generic transaction claim and rollback invoke hard-link rejection;
- confirmed both rollback-marker creation paths apply owner-only permissions;
- checked Rust delimiter balance with a string/comment-aware static scanner;
- regenerated and verified the self-excluding SHA-256 manifest after the final source tree was complete;
- built the handoff archive deterministically twice and compared hashes;
- independently extracted the archive and compared regular-file contents, modes, and symlink targets against the source tree.

## Verification not executable here

This environment does not contain `cargo`, `rustc`, or `rustfmt`. Therefore this round could not execute the brief's required Rust and live-tool gates. The recipient must run at minimum:

```console
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo test --workspace --all-targets --no-fail-fast
```

The brief's DSD checks and live FLAC smoke must also be run in the intended tool environment. No claim is made that those executable gates passed here.
