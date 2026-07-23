# Native descriptor-bound writes and recovery — v30 corrective implementation report

Date: 2026-07-23

## Scope

This round closes the three defects reported against v29:

1. displaced-source and temporary-artifact cleanup could validate a pathname and then unlink a different object installed before `remove_file()`;
2. DSF full-file replacement did not preserve the complete supported Linux filesystem metadata set and could split a hard-linked carrier from its aliases; and
3. native recovery records retained absolute artifact paths that became stale when a carrier's parent directory was renamed.

The implementation changes are confined to `src/db.rs`, `src/tui/probe.rs`, and `src/dsf_tags.rs`, plus this report and the regenerated handoff manifest.

## Descriptor-bound conditional retirement

`Database::remove_owned_path_atomically()` is the common Linux retirement primitive for transaction-owned temporary files and displaced rewrite sources.

It does not validate and then unlink the public pathname. Instead it:

1. retains a descriptor and `SameFileHandle` for the owned object;
2. creates a random, non-listable same-directory quarantine;
3. atomically exchanges the public candidate with a private sentinel using `renameat2(RENAME_EXCHANGE)`;
4. identifies the quarantined object through an `O_PATH|O_NOFOLLOW` descriptor;
5. restores an unrelated candidate when the public pathname did not identify the owned descriptor;
6. removes the public sentinel by atomic rename rather than unlink;
7. detects and restores a replacement installed after the exchange; and
8. unlinks only descriptor-verified objects inside the private quarantine.

Any ambiguous layout retains every object fail-closed. On platforms without this conditional-retirement protocol, an existing artifact is retained rather than removed.

The full-rewrite publication path now retains a descriptor for the displaced source and passes it to this primitive. `CleanupPath` in the FLAC writer is descriptor-bound and no longer calls raw `remove_file()` in `Drop`. DSF rewrite-temp and tail-journal temporary cleanup use the same primitive. Atomic journal publication still uses a hard-link create-if-absent claim; retiring its private source name is descriptor-bound, and a cleanup failure leaves the extra hard link rather than risking an unrelated replacement.

## DSF filesystem metadata and hard-link policy

DSF full-file rewrite now fails closed unless the supported filesystem metadata can be reproduced and verified.

On Linux, `DsfReplacementMetadata` captures from the retained source descriptor and reapplies to the prepared replacement:

- numeric owner and group;
- all permission and special mode bits;
- access and modification timestamps, including nanoseconds; and
- every readable extended attribute, which includes POSIX ACL and security-label/capability namespaces when exposed by the filesystem.

Ownership is applied before mode; mode is applied before ACL-bearing xattrs because `chmod` can rewrite the POSIX ACL mask. Timestamps are applied after the other metadata. The replacement is then re-read through its descriptor and owner, group, mode, timestamps, and the complete sorted xattr name/value set must match exactly. A permission or namespace that cannot be copied causes the rewrite to abort before publication. Non-Linux full-file DSF replacement fails closed because this complete preservation contract is not implemented there.

DSF replacement rejects a source with more than one hard link. Link count and the source xattr digest are also part of the native source-generation witness, so a hard link or xattr-only change introduced after the early check is detected after exchange and forces a verified reverse exchange.

## Parent-directory relocation recovery

Native recovery artifact resolution now derives each journal, companion, publication-handoff, and recorded publication pathname relative to the recorded carrier directory and rebases it under the descriptor-bound carrier's current directory.

Resolution rules are fail-closed:

- when only the moved/rebased path exists, recovery uses it;
- when only the recorded path exists, recovery uses it, preserving file-only rename behavior;
- when both paths identify the same inode, recovery uses the rebased path; and
- when both paths exist as different objects, recovery refuses the ambiguity and removes neither.

Filesystem errors other than `NotFound` are not treated as absence.

FLAC and DSF startup recovery tests now rename the containing directory, not merely the carrier basename, and verify exact rollback plus retirement of the moved journal and companion lock. The database authority tests also cover successful rebasing and refusal when stale and rebased paths contain different objects.

## Regression coverage added or strengthened

The source tree includes tests for:

- successful descriptor-bound retirement of an owned artifact;
- substitution after quarantine, requiring restoration of the unrelated replacement and retention of the owned artifact;
- DSF refusal of an already hard-linked carrier;
- DSF reversal when a hard link appears immediately before exchange;
- DSF reversal when an xattr changes immediately before exchange;
- DSF preservation of owner, group, special mode bits, nanosecond timestamps, and xattrs;
- generic native recovery artifact rebasing after parent-directory rename;
- ambiguity refusal when both old and rebased artifact paths contain different objects;
- FLAC journal recovery after parent-directory rename; and
- DSF tail-journal recovery after parent-directory rename.

## Verification performed

Completed in this environment:

- the v29 input manifest was verified before modification: 709/709 entries passed;
- lexical-state and delimiter checks passed for all three modified Rust files;
- whitespace/error checks passed against the v29 source files;
- changed function signatures and every call site were audited;
- searches confirmed removal of the reported raw `CleanupPath` deletion, displaced-source `remove_file()` call, and DSF `mode() & 0o777` preservation pattern;
- the host filesystem was probed successfully for `renameat2(RENAME_EXCHANGE)`, special mode bits, user xattrs, and hard links;
- the v30 SHA-256 manifest was regenerated and verified;
- the final archive was extracted into a clean directory and its manifest was verified again; and
- archive path traversal, absolute path, and symlink-target checks were performed.

## Unavailable gate

This execution environment contains no `cargo`, `rustc`, or `rustfmt` binary. Consequently, `cargo check`, `cargo test --workspace`, and formatter verification could not be executed here. Compilation and the full Rust test suite remain mandatory handoff gates in a Rust-enabled environment.
