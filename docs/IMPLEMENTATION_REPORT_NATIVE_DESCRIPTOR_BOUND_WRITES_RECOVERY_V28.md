# Native descriptor-bound writes and non-reentrant FLAC recovery — v28

## Scope

This corrective round addresses two remaining P0 defects in the native metadata paths:

1. Native FLAC and DSF writers acquired exclusive authority for one opened carrier generation but later reopened the mutable user pathname for the actual mutation.
2. Stale FLAC artwork recovery acquired exclusive authority and then called a public snapshot-restore helper that attempted to acquire the same authority again.

The correction binds native in-place mutation to the retained read/write descriptor, revalidates descriptor identity immediately before pathname replacement publication, and makes recovery reentrancy explicit through capability-bearing internal APIs.

## Descriptor-bound native write authority

`NativeMetadataWriteAuthority` now retains a read/write carrier descriptor rather than a read-only identity witness. It exposes:

- `try_clone_carrier()`, which duplicates the exact descriptor protected by the exclusive carrier lock;
- `io_path()`, an extension-preserving private alias to that descriptor for path-only preparation helpers; and
- `validate_publication_path()`, which opens the current pathname without following symlinks and proves that it still denotes the retained descriptor generation.

See `src/db.rs` in the `NativeMetadataWriteAuthority` implementation and `Database::acquire_native_metadata_write_authority()`.

Authority acquisition opens the carrier read/write with `O_NOFOLLOW`, verifies that it is a regular file, acquires the identity-scoped exclusive liveness lock, and reopens the pathname with `O_NOFOLLOW` after lock acquisition to detect substitution during claim establishment. The private descriptor alias is created only after those checks succeed.

## FLAC mutation binding

Native FLAC operations now thread the owned `NativeMetadataWriteAuthority` through parsing, journal recovery, mutation, and rollback.

In-place metadata publication:

- clones the retained carrier descriptor;
- verifies FLAC magic through that descriptor;
- seeks and writes through that descriptor; and
- revalidates the publication pathname before the first metadata byte is written.

An external rename or replacement cannot redirect the descriptor write to the later pathname occupant.

Overflow rewrites read the source audio through the retained descriptor. Immediately before the temporary replacement is renamed over the user pathname, the writer verifies that the pathname still denotes the descriptor-bound carrier generation. A mismatch fails closed and leaves the replacement file untouched.

Artwork preview, write, removal, rollback-journal preparation, and rollback restore now accept the outer `FlacWriteClaim` or its carrier authority rather than reacquiring authority internally.

## DSF mutation binding

`DsfWriteAuthority` owns both the format-specific `StoreFileLock` and the common exclusive carrier authority.

DSF tail replacement, untagged append, artwork rollback, and tail-journal recovery now seek, truncate, and write through cloned authority descriptors. Full-file rewrites copy their source bytes through the retained descriptor and call `validate_publication_path()` immediately before `replace_config_file()` publishes the temporary file.

Pathnames remain in use for path-local recovery artifacts and final replacement publication, but not as an unvalidated substitute for the carrier object that owns write authority.

## Non-reentrant FLAC artwork recovery

Snapshot restoration is split into explicit capability-bearing forms:

- independent callers acquire one `FlacWriteClaim` and call the under-claim implementation;
- recovery code that already owns `NativeMetadataWriteAuthority` calls the under-authority implementation directly.

`recover_artwork_rollback_journal_under_authority()` therefore restores metadata without attempting to acquire a second exclusive authority. The public independent recovery wrapper may acquire authority once, but no locked recovery path recursively enters `acquire_common_write_claim()`.

The existing symlink recovery fixture now retains the original write claim through mutation, marks the artwork journal stale, releases that claim to model process death, and verifies that read recovery restores the target using one newly acquired authority.

## Regression coverage

The changed tests cover the reported races directly:

- an in-place FLAC write is paused after descriptor preparation; the original carrier is renamed and an unrelated valid FLAC occupies the pathname; the write fails closed and neither generation is modified;
- the existing FLAC overflow rewrite test replaces the pathname immediately before rename and proves that publication is refused;
- an in-place DSF append is paused before descriptor mutation; pathname replacement is detected and both files remain unchanged;
- a DSF full rewrite replaces the pathname immediately before commit and proves that publication is refused;
- stale FLAC artwork rollback recovery through a symlink completes without nested exclusive-authority acquisition;
- retained artwork rollback tests use explicit under-claim mutation and restore APIs, proving that one outer capability owns the complete lifecycle.

## Static verification performed

- Audited all changed Rust files for balanced delimiters, tabs, and trailing whitespace.
- Ran `git diff --no-index --check` against the v27 source for every changed Rust file.
- Audited direct native FLAC opens, in-place writes, overflow replacement publication, artwork mutation, and rollback recovery.
- Audited direct native DSF opens, tail writes, appends, artwork restore, journal recovery, and full-file replacement publication.
- Audited removed helper names and production call sites to confirm that locked callers use under-claim or under-authority APIs.
- Audited the existing FLAC artwork recovery test identified in review and the newly added pathname-substitution tests.

## Unexecuted gates

This environment does not contain `cargo`, `rustc`, or `rustfmt`. Consequently, the following required gates were not executed here:

- `cargo check --workspace --all-targets`;
- the full no-fail-fast Rust test suite;
- compiler warning validation;
- DSD qualification checks; and
- the live FLAC smoke test.

The implementation and tests are delivered for execution in the project toolchain environment. No claim is made that the unexecuted gates passed.
