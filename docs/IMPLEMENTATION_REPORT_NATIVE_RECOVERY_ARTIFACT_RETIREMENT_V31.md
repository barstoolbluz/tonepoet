# Native recovery artifact retirement — v31 corrective implementation report

Date: 2026-07-23

## Scope

This narrow round closes the remaining v30 recovery-artifact ownership defect:

1. after a carrier's parent directory moved, recovery could select an object that existed only at the stale recorded path;
2. native journals and companion locks were not represented by persistent artifact identities; and
3. terminal FLAC, DSF, and companion cleanup still used raw pathname unlink operations.

The implementation changes are confined to `src/db.rs`, `src/tui/probe.rs`, and `src/dsf_tags.rs`, plus this report and the regenerated handoff manifest.

## Relocated-path selection

`NativeMetadataWriteAuthority::resolve_recorded_recovery_path()` no longer accepts an artifact that exists only at the old recorded path after the descriptor-bound carrier's current parent differs from its recorded parent.

The relocation rules are now:

- if the current and recorded carrier parents are the same, the recorded artifact path remains valid for file-only carrier renames;
- if the parent moved and only the rebased artifact exists, recovery uses the rebased path;
- if the parent moved and neither artifact exists, recovery returns the rebased missing path so state-specific missing-artifact handling can run;
- if the parent moved and only the stale recorded path exists, recovery fails closed;
- if both paths exist and identify the same inode, recovery uses the rebased path; and
- if both paths exist as different objects, recovery refuses the ambiguity.

Therefore a journal or lock created later by a different carrier at the former pathname cannot become carrier A's recovery authority merely by occupying carrier A's recorded path.

## Persistent journal and companion identities

`NativeMetadataRecoveryRecord` now retains:

- `journal_identity`; and
- one optional identity slot for each companion path.

For new writes, the journal identity is captured from the already-synced private journal descriptor before the journal is published. FLAC metadata journals, FLAC artwork journals, and DSF tail journals all pass that retained descriptor into `begin_native_recovery_artifact_from_file()`.

This ordering avoids a crash window between public journal creation and identity persistence. If publication succeeds but the process dies before the `PREPARED` transition, the `ALLOCATING` authority record already knows the journal inode.

Companion identities are captured when the recovery authority is allocated. The `PREPARED` transition verifies that:

- the public journal path names the stored journal identity;
- every companion that existed at allocation still names its stored identity; and
- no previously absent companion appeared before preparation.

Existing records without identities are readable through serde defaults, but destructive retirement of an existing unidentified artifact fails closed.

## Descriptor-bound retirement

Token-bound journal cleanup now uses `NativeMetadataWriteAuthority::remove_native_recovery_journal_atomically()`.

That method:

1. re-reads and validates the carrier token and authority record;
2. requires terminal state;
3. resolves the journal under the relocation rules;
4. opens the journal with `O_NOFOLLOW`;
5. verifies the opened descriptor against the stored journal identity; and
6. passes that descriptor to `Database::remove_owned_path_atomically()`.

FLAC metadata-journal cleanup, FLAC artwork-journal cleanup, and DSF tail-journal cleanup no longer call `remove_file()` on the public journal pathname.

Companion retirement similarly resolves each companion, opens it with `O_NOFOLLOW`, verifies its stored identity, and retires it through `remove_owned_path_atomically()`. A path substitution before descriptor acquisition is rejected by identity mismatch; a substitution after descriptor acquisition is handled by the quarantine protocol, which restores or retains the unrelated replacement and never unlinks it through the public pathname.

Legacy/unbound journal cleanup also uses the descriptor-bound quarantine primitive rather than a validation-followed-by-raw-unlink sequence.

## Regression coverage

The database authority tests now cover:

- artifact identity persistence across a parent-directory rename;
- refusal when old and rebased artifact paths contain different objects;
- refusal to retire a companion replaced after its identity was recorded; and
- the reported two-carrier sequence: carrier A's parent moves, A becomes terminal, A's rebased journal is absent, the old directory is recreated with carrier B's live journal and lock, and recovery of A leaves both B artifacts byte-for-byte untouched.

The existing descriptor-bound cleanup race test continues to inject a replacement after atomic quarantine and verifies restoration of the unrelated file plus retention of the owned artifact.

## Verification performed

Completed in this environment:

- the v30 input manifest was verified before modification: 710/710 entries passed;
- delimiter and lexer-state checks passed for all three modified Rust files;
- `git diff --check`-equivalent whitespace checks passed for each modified source file;
- all `begin_native_recovery_artifact*()` call sites were audited;
- searches confirmed that the reported FLAC, DSF, and companion retirement paths no longer contain raw `remove_file()` calls;
- the source manifest was regenerated and verified;
- the final archive was extracted into a clean directory and its manifest was verified again; and
- archive traversal, absolute-path, `.git`, `target`, and symlink-target checks were performed.

## Unavailable gate

This execution environment contains no `cargo`, `rustc`, or `rustfmt` binary. Consequently, `cargo check`, `cargo test --workspace`, and formatter verification could not be executed here. Compilation and the full Rust test suite remain mandatory handoff gates in a Rust-enabled environment.
