# Metadata shared-read authority and carrier-generation corrective round

Date: 2026-07-23

## Scope

This round closes three defects in the generic full-file metadata transaction:

1. the prior read API released its liveness lock before Lofty, FFmpeg, metaflac, or another reader opened and consumed the carrier;
2. device/inode identity alone could be inherited by an unrelated file after deletion and inode reuse;
3. absolute rollback, work-file, and locator paths did not survive a rename of the carrier's parent directory.

This report supersedes the read-barrier, generation-identity, and rename claims in `IMPLEMENTATION_REPORT_METADATA_CARRIER_GENERATION_READ_BARRIER_V25.md`. Device/inode identity remains useful for lock coordination, but it is no longer treated as sufficient recovery authorization for a current transaction.

## Shared authority held through the actual read

`Database::recover_metadata_before_read()` now returns a `MetadataReadAuthority` rather than `()`.

The authority owns:

- an open descriptor for the validated carrier generation;
- a shared OS lock in the same carrier-identity lock namespace used exclusively by generic metadata writers;
- the canonical carrier path used during authority establishment.

The value is marked `#[must_use]`. Callers retain it until the complete read operation has returned. For synchronous readers, that means through the Lofty or FFmpeg parse. For asynchronous and subprocess readers, the guard remains captured in the future until the child process or conversion pipeline exits.

Authority establishment follows this sequence:

1. canonicalize and open the carrier without following a final symlink;
2. derive the current filesystem identity from the opened descriptor;
3. acquire a shared lock for that identity;
4. prove that the opened descriptor and current pathname still identify the same file;
5. inspect carrier-local transaction authority while the shared lock remains held;
6. if no transaction is present, return the live read guard;
7. if an abandoned transaction is present, release shared authority, acquire exclusive authority, recover it, then repeat and return a shared guard for the recovered generation;
8. if a live writer or recovery owner holds exclusive authority, refuse the read.

A generic writer cannot begin staged publication after the precheck and before the actual read: its exclusive acquisition conflicts with the retained shared guard. Publication remains nonblocking and fail-closed rather than waiting indefinitely.

## Carrier-bound generation token

Current transactions no longer authorize recovery from device/inode equality alone.

On Linux, claim publishes a cryptographically random transaction token in the carrier's `user.tonepoet.metadata-transaction` extended attribute. The same token is stored in the durable carrier record, and the authoritative record filename is derived from a domain-separated SHA-256 of that token.

The token has the required generation semantics:

- it survives a file rename and parent-directory rename because it is attached to the inode;
- it survives in-place truncation and staged publication to that same inode;
- it does not transfer to a different file created after deletion;
- it is not recreated merely because the filesystem later reuses the old device/inode pair.

Device/inode identity now serves two narrower purposes:

- selecting the shared/exclusive lock namespace for a currently opened file;
- detecting pathname replacement while a transaction still owns an open descriptor.

Prepared rollback, staged commit publication, and terminal cleanup require both the recorded identity and the exact carrier-generation token. Identity equality without the token cannot authorize byte publication.

Pre-token identity-keyed records remain discoverable only for fail-closed migration and cleanup. A prepared legacy record cannot publish rollback bytes solely because a current file happens to have the same device/inode pair.

## Staged publication and token validation

Metadata libraries continue to mutate a transaction-owned work file. Before staged bytes or rollback bytes can be copied into the carrier, the transaction verifies:

- the source artifact's recorded filesystem identity;
- the source artifact's recorded byte length;
- the source artifact's recorded SHA-256;
- the carrier descriptor's recorded device/inode identity;
- the exact carrier-generation token;
- equality between the opened carrier descriptor and the current pathname.

The verified source is first copied into an anonymous process-owned snapshot. Only after all source checks pass is the carrier opened for publication. The generation token is checked before truncation and again after the copied bytes are synced.

If the pathname is replaced, the token is absent or different, or the named artifact changes identity during verification, publication is refused and recovery authority remains armed.

## Parent-directory rename recovery

The durable record still retains the original lossless paths for diagnosis and direct lookup, but rollback and work-file resolution no longer depends exclusively on those absolute paths.

If a stored artifact path is absent, recovery searches the current carrier's parent directory for the transaction-specific suffix:

- `.tonepoet-bak.txn-<transaction-id>`;
- `.tonepoet-work.txn-<transaction-id>`.

A candidate is accepted only when its filesystem identity matches the durable record. Missing or ambiguous candidates fail closed.

Authority cleanup now scans both the recorded parent and the current carrier parent for locators naming the transaction. After an album-directory rename, successful recovery removes the moved rollback marker, work file, locator, carrier-generation token, and global authority record.

## Production read inventory

The retained shared authority is wired through the actual operation for the generic carrier readers used by:

- ordinary metadata and artwork reads;
- Browse tag display and tag sorting;
- MusicBrainz, AccurateRip, pre-emphasis, and catalog metadata inspection;
- materializer and queue-expansion metadata reads;
- FFmpeg-based probing, analysis, HDCD detection, and STFT analysis;
- `ffprobe`, `ffmpeg`, `metaflac`, `wvunpack`, verification, and bit-comparison subprocesses;
- single-file and materialized conversion work units, where the guard is captured until the asynchronous pipeline and its child tools return.

Reads of transaction-owned work files intentionally do not acquire carrier authority: they cannot observe or modify the carrier until commit performs the verified exclusive publication step.

Native FLAC and DSF mutation protocols retain their format-specific recovery mechanisms. This round's shared/exclusive authority guarantee applies to the generic full-file transaction and its readers.

## Platform policy

Current generic full-file transactions require Linux filesystem support for writable user extended attributes, stable Unix file identity, and single-link verification. Token publication is mandatory; a filesystem that cannot durably attach and read the carrier-generation token cannot enter the metadata writer callback.

Non-Linux platforms do not receive a pathname-only or inode-only recovery promise. Generic full-file mutation fails closed rather than authorizing rollback without a carrier-bound generation token.

## Regression coverage

Added or strengthened tests prove:

- a shared read authority blocks a writer in another process until the actual guarded read scope ends;
- the writer proceeds after the read authority is dropped;
- ordinary reads refuse a live prepared owner;
- an abandoned transaction is recovered before the final shared read authority is returned;
- identity equality without the carrier-generation token cannot authorize rollback to an unrelated file;
- a renamed carrier remains recoverable through its generation token;
- a parent-directory rename preserves recovery and removes moved rollback, work, and locator artifacts;
- terminal cleanup resumes safely if a crash occurs after token removal but before record retirement;
- rollback and staged-work content corruption still fail before carrier publication;
- missing or different SQLite databases do not supersede carrier-local authority.

The inode-reuse regression uses a deterministic stronger false-positive fixture: it presents recovery with an unrelated file whose identity field has been made equal to the abandoned record while withholding the carrier token. Recovery refuses publication and leaves the unrelated bytes unchanged. This proves that identity equality is no longer sufficient authorization without relying on nondeterministic filesystem inode-reuse timing.

## Verification performed in this environment

- audited the shared/exclusive lock acquisition, upgrade, and drop paths;
- audited every production call to `recover_metadata_before_read()` and `recover_flac_metadata_before_read()` to confirm that the returned guard remains in scope through the corresponding parser, child process, or asynchronous pipeline;
- audited production Lofty, metaflac, FFmpeg, ffprobe, WavPack, verification, analysis, and conversion input paths;
- confirmed current prepared publication and rollback require the carrier-generation token in addition to file identity;
- confirmed parent-directory rename resolution verifies transaction-specific suffixes and recorded artifact identity;
- checked all modified Rust files with a raw-string/comment-aware delimiter scanner;
- checked modified files for tabs and trailing whitespace;
- confirmed no new dependency was added;
- regenerated and verified the self-excluding SHA-256 manifest;
- built the final archive twice with identical bytes;
- independently extracted and compared contents, modes, directories, and symlink targets.

## Verification not executable here

This environment does not contain `cargo`, `rustc`, or `rustfmt`, and no toolchain can be retrieved. The following required gates remain unexecuted:

```console
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo test --workspace --all-targets --no-fail-fast
```

The brief's DSD checks and live FLAC smoke also remain to be run in the intended build environment. No claim is made that executable gates passed here.
