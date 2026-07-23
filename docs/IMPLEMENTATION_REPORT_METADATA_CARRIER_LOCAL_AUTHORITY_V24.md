> **Superseded:** The final carrier-generation, staged-publication, integrity, rename, and read-barrier design is documented in `IMPLEMENTATION_REPORT_METADATA_CARRIER_GENERATION_READ_BARRIER_V25.md`.

# Metadata carrier-local recovery authority corrective round

Date: 2026-07-22

## Scope

This round corrects the remaining authority gap in generic full-file Lofty metadata transactions:

1. abandoned transaction identity and state were still authoritative only inside one SQLite database;
2. production could fall back to a volatile in-memory database and then attempt a crash-sensitive write;
3. non-Unix builds could not prove the single-link invariant required by pathname-local rollback;
4. recovery of legacy symlink-keyed journal rows could derive a different lock authority from new canonical claims.

The prior per-carrier OS lock remains the liveness primitive. This round adds a separate carrier-adjacent durable transaction record as the recovery authority.

## Carrier-local authority

Every new generic full-file transaction now durably creates a no-clobber JSON record adjacent to the canonical carrier while holding the same per-carrier OS lock used by writers and recovery. The record contains:

- record format version;
- transaction UUID;
- losslessly encoded native carrier path;
- losslessly encoded rollback-marker path;
- allocating, prepared, committed, or rolled-back state;
- carrier filesystem identity;
- rollback-marker filesystem identity;
- transaction start time.

The record and rollback marker are regular, single-link, owner-only files on Unix. Record creation and state transitions sync file contents and the parent directory. Recovery validates the record path, exact carrier path, transaction identity, rollback-marker path, and rollback-marker filesystem identity before acting.

The liveness lock and carrier record have distinct roles:

- the held OS lock proves that a live process currently owns the carrier;
- the durable carrier record proves which abandoned transaction owns mutation and rollback rights after the process exits.

## Transaction ordering

A new claim now follows this order:

1. reject an in-memory SQLite database;
2. canonicalize the carrier and enforce the platform hard-link policy;
3. acquire the per-carrier OS lock;
4. reconcile any existing carrier record;
5. reconcile only explicitly legacy SQLite rows;
6. refuse deterministic or orphan UUID rollback markers;
7. reserve and sync the unique rollback marker;
8. durably publish the carrier record in `allocating` state;
9. add a non-authoritative SQLite index row;
10. populate and sync the rollback marker;
11. advance the SQLite index and then the carrier record to `prepared`;
12. enter the authoritative fresh-read and write callback.

The writer cannot run until the carrier record is durably `prepared`.

Commit remains ordered as:

1. sync destination bytes;
2. sync the destination parent directory;
3. durably publish carrier state `committed`;
4. update the SQLite index when available;
5. remove and sync retirement of the rollback marker;
6. remove and sync retirement of the carrier record;
7. remove the SQLite index row;
8. release the OS lock.

Rollback restores and syncs the rollback bytes before publishing `rolled_back`, then retires the same carrier-local artifacts and finally the SQLite index.

## SQLite is an index, not recovery authority

Schema version 24 adds nullable `metadata_journal.authority_kind`.

- New transactions store `carrier-v1`. If their carrier record is missing, startup recovery and new claims fail closed. SQLite alone is never promoted into rollback authority and no carrier bytes or recovery artifacts are changed.
- Rows migrated from schema versions 1–23 retain `NULL`. These rows predate the carrier record and may be promoted once, under the carrier lock, into a carrier-local record before legacy recovery proceeds.
- Unknown authority kinds fail closed.

This preserves upgrade recovery without allowing a new database row to re-acquire authority after its carrier record has been retired or lost.

## Cross-database recovery

A writer using database B must inspect the carrier record before consulting database B. If database A's owner crashed in `prepared` state, database B acquires the now-free carrier lock, restores database A's rollback marker through the carrier record, retires that record and marker, and only then begins its own transaction.

If database A later restarts, its remaining SQLite row is only an index. Because the authoritative record and rollback marker no longer exist, recovery retires the stale row without changing carrier bytes. It cannot overwrite database B's later commit.

## Volatile database fallback

`Database` now determines whether SQLite's main database is persistent from `PRAGMA database_list`; the selected pragma profile alone is not trusted. Generic full-file metadata mutation returns before canonicalization, locking, marker creation, or callback execution when the database is volatile.

The production TUI may still fall back to an in-memory database for read-only continuity when the production database cannot open, but its warning now states that generic full-file metadata writes are disabled. A subprocess-isolated test exercises the actual production database source, forces its open to fail, and proves the fallback cannot enter a metadata writer.

## Hard-link safety on all builds

On Unix, the transaction checks `nlink` before claim, before prepared writer entry, and before rollback publication. Hard-linked carriers fail closed.

On non-Unix builds, this codebase has no safe stable mechanism that proves both persistent carrier identity and a single-link pathname. Generic full-file metadata transactions therefore fail before creating transaction artifacts. This deliberately preserves the rollback invariant rather than presenting partially protected Windows behavior. Native or format-specific write paths remain governed by their own safety contracts.

## Legacy symlink recovery

Startup recovery now canonicalizes every existing journal target before deriving the liveness lock, carrier record path, or rollback authority. A legacy row keyed by a symlink alias therefore contends on the same canonical lock as a live current transaction and cannot recover through an alias-local lock while the canonical owner is active.

## Regression coverage

The round adds or strengthens tests for:

- a crashed database-A owner recovered by a database-B writer;
- database A restarting after database B commits and proving its stale row cannot overwrite the later commit;
- a live owner blocking a writer that uses a different SQLite database;
- a carrier-index row whose carrier record is missing, proving both startup recovery and new claims leave bytes and artifacts untouched;
- migration of v23 rows as explicitly legacy authority;
- an orphan `.tonepoet-bak.txn-*` marker with no current-database row or carrier record;
- direct in-memory SQLite and a file-backed pragma profile over volatile SQLite failing before mutation artifacts;
- the actual production DB-open failure and in-memory fallback path in an isolated subprocess;
- legacy symlink journal recovery contending on the canonical live lock;
- Unix hard-link claim and rollback refusal;
- non-Unix fail-closed behavior when single-link authority cannot be proved;
- non-UTF-8 Unix recovery paths;
- owner-only rollback, lock, and carrier-record sidecars.

Cross-platform tests that require successful generic full-file transactions are now Unix-gated to match the explicit fail-closed non-Unix production contract.

## Verification performed in this environment

- audited all carrier record, SQLite index, lock, backup, claim, prepare, commit, rollback, and startup recovery paths;
- confirmed every production generic full-file transaction inserts `authority_kind = 'carrier-v1'`;
- confirmed only `NULL` pre-v24 rows can enter legacy promotion;
- confirmed new carrier-index rows with missing sidecars cannot restore bytes;
- confirmed carrier reconciliation occurs before current-database reconciliation and before new marker allocation;
- confirmed in-memory rejection occurs before filesystem transaction artifacts are created;
- confirmed legacy recovery canonicalizes existing targets before lock derivation;
- confirmed no new `unsafe` code or dependency was added;
- checked Rust delimiter balance with a string/comment-aware static scanner;
- checked modified files for whitespace errors;
- regenerated and verified the self-excluding SHA-256 manifest;
- built the handoff archive deterministically twice and compared hashes;
- independently extracted the archive and compared contents, modes, directories, and symlink targets.

## Verification not executable here

This environment does not contain `cargo`, `rustc`, or `rustfmt`, and outbound package/toolchain retrieval is unavailable. Therefore this round could not execute the brief's required Rust or live-tool gates. The recipient must run at minimum:

```console
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo test --workspace --all-targets --no-fail-fast
```

The brief's DSD checks and live FLAC smoke must also run in the intended tool environment. No claim is made that those executable gates passed here.
