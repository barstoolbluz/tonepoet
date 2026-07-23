# Native descriptor-bound writes and recovery — v29 corrective implementation report

Date: 2026-07-23

## Scope

This round closes the three P0 defects reported against the v28 FLAC and DSF native mutation paths:

1. a full-file rewrite could publish a stale snapshot after another process changed the retained source inode in place;
2. pathname validation and replacement were separate operations, allowing an unrelated destination inode to be overwritten between the check and rename; and
3. native crash journals were associated only with the original pathname and could not follow a renamed carrier.

The implementation changes are confined to `src/db.rs`, `src/tui/probe.rs`, and `src/dsf_tags.rs`, plus this report and the regenerated handoff manifest.

## Source-generation immutability

`NativeMetadataWriteAuthority` now exposes a full-source generation witness consisting of the retained inode's identity and metadata plus a SHA-256 digest of the complete file. Capture and validation bracket hashing with descriptor metadata checks. A rewrite is refused when the retained inode changes while the witness is captured or while the replacement is prepared.

Each rewrite also carries a SHA-256 witness for the source range actually copied into the replacement:

- FLAC hashes the copied audio range.
- DSF hashes the copied prefix through the metadata offset.

Both witnesses are revalidated immediately before publication and again against the displaced source after atomic exchange.

The preparation snapshot is also bound to the witnessed generation:

- FLAC validates the exact parsed raw metadata region against the retained descriptor before building the overflow replacement.
- DSF reruns text or artwork preparation after capturing the generation witness and requires the location, rollback snapshot, and encoded tag to match before proceeding.

This prevents an external same-inode metadata update between parsing and witness capture from being silently replaced by stale prepared metadata.

## Atomic conditional publication

On Linux, native full-file publication now uses `renameat2(..., RENAME_EXCHANGE)` through the repository's existing safe `rustix` dependency.

The publication protocol is:

1. verify the public pathname still denotes the descriptor-bound source;
2. validate the full-source and copied-range witnesses;
3. verify the prepared replacement pathname still denotes the opened replacement;
4. atomically exchange the destination and replacement pathnames;
5. identify both exchanged inodes from opened descriptors;
6. prove that the displaced destination is the retained source and that its contents still match both witnesses;
7. durably sync the exchange;
8. reverse the exchange on any mismatch, then durably sync the reversal; and
9. remove only the verified displaced source after successful validation.

A substitution after prevalidation therefore cannot be overwritten and discarded. The substituted destination is displaced without deletion, detected from its inode identity, and restored by reverse exchange. If ownership changes again before reversal, both pathnames are retained for explicit reconciliation rather than deleting an unowned object.

Platforms without the required atomic exchange primitive, and filesystems that reject it, fail closed for native full-file replacement.

Rewrite temporary files are no longer deleted by startup cleanup merely because their owner is gone. An interrupted exchange may have placed an unrelated displaced destination at that pathname, so such files are retained and block later writes until reconciled.

## Rename-resilient native recovery authority

Prepared FLAC and DSF native journals now have carrier-bound recovery authority:

- a random recovery token is stored in a Linux user xattr on the carrier inode;
- a private, durable authority record under the metadata authority root maps that token to the journal, companion paths, operation kind, state, and carrier identity; and
- startup recovery scans current carriers for the token and resolves the journal from the authority record rather than deriving the carrier solely from the old journal filename.

The token therefore follows an inode across rename. Recovery opens the renamed carrier, validates the token and filesystem identity, applies or retires the old path-local journal, removes recorded companion artifacts, clears the token, and removes the authority record only after the operation is terminal.

For journaled operations that require a full rewrite, publication records a durable two-inode handoff before exchange and temporarily places the same recovery token on the prepared replacement. Recovery accepts only provable old/new exchange layouts. Ambiguous third-inode layouts remain armed and fail closed. After a durable successful exchange, authority is rebound to the published inode before the displaced source is retired.

Journal publication now uses an atomic hard-link create-if-absent claim rather than ordinary `rename`, so an existing recovery journal is never replaced.

## Regression coverage added

The source tree includes regression tests for:

- FLAC and DSF pathname substitution after final prevalidation;
- FLAC and DSF same-inode mutation after final prevalidation, requiring reverse exchange and preservation of the external bytes;
- FLAC and DSF carrier rename after a prepared native journal, requiring token-based recovery at the new pathname; and
- startup retention of unresolved FLAC and DSF exchange temporary files.

Existing publication hooks now execute in the interval immediately before atomic exchange, covering the race that the v28 tests did not exercise.

## Verification performed

Completed in this environment:

- original v28 manifest verification: 708/708 entries passed before modification;
- structural delimiter and lexical-state checks for all three modified Rust files;
- whitespace/error checks against the v28 sources;
- call-site audit for the changed rewrite signatures and publication API;
- search confirming removal of the reported validate-then-`rename` publication patterns;
- search confirming no new `unsafe`, `TODO`, or `FIXME` additions;
- regenerated v29 SHA-256 manifest verification;
- clean extraction of the final archive followed by a second manifest verification; and
- verification of the retained fixture symlink target.

## Unavailable gate

This execution environment contains no `cargo`, `rustc`, or `rustfmt` binary. Consequently, `cargo test --workspace`, `cargo check`, and formatter verification could not be executed here. The implementation has been statically audited, but compilation and the full test suite remain mandatory handoff gates in a Rust-enabled environment.
