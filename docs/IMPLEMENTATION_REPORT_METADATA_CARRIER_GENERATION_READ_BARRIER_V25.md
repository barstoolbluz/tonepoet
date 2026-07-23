# Metadata carrier-generation and read-barrier corrective round

Date: 2026-07-22

## Scope

This round closes four remaining gaps in the generic full-file metadata transaction:

1. a prepared rollback could restore old bytes over an unrelated file that later occupied the original pathname;
2. a rename could move the owned carrier outside a basename-derived authority namespace;
3. rollback contents were identity-checked but not content-integrity-checked;
4. carrier-local authority was consulted by later writers but not consistently by startup recovery and ordinary metadata readers when the creating SQLite database was absent.

This report supersedes the recovery and publication details in `IMPLEMENTATION_REPORT_METADATA_CARRIER_LOCAL_AUTHORITY_V24.md`.

## Stable carrier-generation authority

On Unix, generic full-file transactions now key the liveness lock and authoritative transaction record by the carrier's device/inode identity in the private Tonepoet authority directory. A pathname-adjacent, owner-only locator remains as a pathname-reuse guard and points to the identity-keyed record.

A transaction records:

- transaction UUID;
- losslessly encoded original carrier path;
- rollback-marker path;
- staged-work path;
- allocating, prepared, committed, or rolled-back state;
- carrier identity;
- rollback-marker identity, length, and SHA-256;
- staged-work identity, length, and SHA-256 once the staged output is ready;
- start time.

A read or write through a renamed path derives the current carrier identity and therefore resolves the same global record and liveness lock. The original pathname locator prevents a replacement file at that pathname from silently bypassing the unresolved transaction.

## Staged mutation and owned-generation publication

Metadata libraries no longer mutate the carrier pathname directly. The transaction now follows this sequence:

1. claim the identity-scoped liveness lock and publish allocating authority;
2. open the claimed carrier generation and copy it into owner-only rollback and work files;
3. sync both files and their directory entries;
4. compute the rollback length and SHA-256 from the exact still-open rollback handle;
5. revalidate that the pathname still denotes the claimed carrier generation;
6. publish prepared authority;
7. run the authoritative fresh read and metadata mutation against the transaction-owned work file;
8. sync the work file and durably record its identity, length, and SHA-256;
9. verify the work artifact into an anonymous process-owned snapshot before opening the carrier for writing;
10. open the carrier, prove that its file descriptor and current pathname still denote the recorded generation, then copy the verified snapshot through that descriptor;
11. sync the carrier and parent directory, publish committed state, and retire recovery artifacts.

This design permits Lofty to replace the work-file inode without transferring mutation authority to an unrelated carrier pathname. A pathname replacement before writer entry or before publication is refused. Recovery likewise checks the current carrier identity before opening the rollback destination and again on the opened file descriptor.

## Rollback integrity

Prepared authority includes both rollback length and SHA-256. Recovery and same-process rollback require:

- a regular, single-link rollback artifact;
- the recorded rollback filesystem identity;
- the recorded byte length;
- the recorded SHA-256.

The named rollback file is copied into an anonymous verified snapshot while hashing. The carrier is not opened for writing until the complete snapshot matches the durable integrity record. Publication reads only from that anonymous handle, so an in-place change to the named artifact after validation cannot alter the bytes published to the carrier.

Staged commit data receives the same identity, length, and SHA-256 treatment.

## Pathname replacement and rename behavior

Prepared recovery refuses to act when the recovery pathname identifies a different device/inode than the transaction record. It leaves the carrier, rollback marker, work file, locator, and authority record untouched for explicit reconciliation.

A renamed owned carrier remains recoverable through its new pathname because read and write barriers query the identity-keyed authority record. Startup recovery cannot discover an arbitrary new pathname by scanning the filesystem; when the original pathname is unavailable or reused it defers without changing bytes, and the first read or write through the owned carrier's new pathname performs recovery under the same identity lock.

## Database-independent startup and read barriers

`recover_stale_metadata_writes()` now scans the private identity-keyed authority directory before reconciling the current SQLite index. This recovers unchanged-path abandoned transactions even when the creating database was deleted, unavailable, in-memory, or different from the current database.

`Database::recover_metadata_before_read()` is a database-independent read barrier. It:

- bypasses non-file probe targets;
- canonicalizes a file path;
- resolves pathname-locator or identity-keyed authority;
- nonblockingly acquires the same liveness lock;
- refuses reads while a live owner exists;
- recovers an abandoned owned generation before the caller opens metadata.

The barrier is now wired into generic Lofty reads, metaflac reads, ffmpeg-based probing, conversion inputs, embedded-CUE readers, Browse metadata paths, MusicBrainz and AccurateRip metadata paths, pre-emphasis metadata reads, and materializer metadata reads. Native FLAC and DSF paths retain their format-specific recovery layers.

## Platform policy

Successful generic full-file transactions remain Unix-only because this implementation can prove stable device/inode identity and single-link status there. Other platforms fail closed before transaction artifacts or callbacks are created rather than presenting pathname-only rollback as carrier-safe.

## Regression coverage

Added or strengthened tests prove:

- prepared recovery refuses an unrelated replacement at the original pathname;
- replacement after rollback population prevents writer entry;
- replacement between staged mutation and commit is never overwritten;
- a renamed abandoned carrier is recovered through an ordinary read;
- a new write through the renamed path recovers the old authority before entering its callback;
- startup scanning recovers an abandoned transaction after the creating database is deleted;
- an ordinary read recovers without the creating database;
- an ordinary read refuses a live owner;
- rollback corruption with unchanged device/inode is rejected by length/SHA-256;
- staged-work corruption after integrity publication is rejected before carrier publication;
- cross-database stale recovery cannot undo a later commit;
- non-UTF-8 paths, hard-link rejection, symlink aliases, owner-only artifacts, and production in-memory fallback remain covered.

## Verification performed in this environment

- audited all generic claim, prepare, staged-write, commit, rollback, startup-recovery, and read-barrier paths;
- audited every production `lofty::read_from_path` and `metaflac::Tag::read_from_path` call for a transaction read barrier or transaction-owned work path;
- confirmed carrier publication writes only through a file descriptor whose identity matches the durable authority record;
- confirmed prepared recovery performs the same generation proof before rollback publication;
- confirmed rollback and staged-work publication verify identity, length, and SHA-256;
- checked Rust delimiter balance with a raw-string/comment-aware scanner;
- checked modified Rust files for tabs and trailing whitespace;
- confirmed no new dependency was added;
- regenerated and verified the self-excluding SHA-256 manifest;
- built the final archive twice with identical bytes;
- independently extracted and compared file contents, modes, directories, and symlink targets.

## Verification not executable here

This environment does not contain `cargo`, `rustc`, or `rustfmt`, and no toolchain can be retrieved. The following required gates remain unexecuted:

```console
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo test --workspace --all-targets --no-fail-fast
```

The brief's DSD checks and live FLAC smoke also remain to be run in the intended build environment. No claim is made that executable gates passed here.
