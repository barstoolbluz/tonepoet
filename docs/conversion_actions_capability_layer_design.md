# Conversion Actions Capability Filesystem — Pass 2 Design

## Scope and invariant

This pass starts from the completed pass-1 bundle. It preserves the pass-1 action model, planner, election protocol, durable reports, cancellation semantics, TUI behavior, and operation state machines, while replacing the built-in apply/recovery backend with the reusable conversion-domain module `src/convert/cap_fs.rs`.

The governing invariant is:

> After an absolute pathname is used to acquire a logical root, every built-in lookup, mutation, publication, witness transition, cleanup, and recovery operation resolves validated relative components from a retained directory descriptor.

Absolute paths remain in plans and reports for compatibility and display. They are re-derived and validated, but they are not passed to ordinary pathname mutation APIs.

## Generic capability API

`CapabilityFilesystem` is independent of TUI and pipeline action types. It supplies:

- retained root directory descriptors;
- validated `ScopeId`, `RelativePath`, `ScopedPath`, `ScopeRecord`, and entry-identity types;
- scoped no-follow metadata inspection;
- checked regular-file and directory opens;
- exclusive regular-file creation;
- component-wise `mkdir -p`;
- descriptor-relative enumeration;
- recursive copy without following symlinks;
- no-clobber file publication and rename;
- checked rename against the planned device/inode/type;
- EXDEV reporting for the durable move fallback;
- conditional recursive disposal of journal-owned trees;
- file and directory synchronization;
- capability-relative journal writes and owned atomic replacement;
- root restoration and validation for recovery;
- bounded intermediate-directory descriptor caching;
- deterministic race hooks and syscall/cache counters under tests.

`CapabilityActionFilesystem` adapts this generic API to the existing pass-1 `ActionFilesystem` seam. The picker crate does not depend on conversion types.

## Root model

Each scope has:

1. an **acquisition directory**, whose descriptor is retained; and
2. a **logical root**, which is the configured authority boundary.

For an existing album/source/output root these are normally identical. When a configured external destination does not yet exist, the nearest existing ancestor is retained and `base_relative` records the validated component sequence to the logical root. This permits creation beneath the configured destination without authorizing its siblings.

A missing logical root is **not** created during planning, capability acquisition, or ordinary child mutation. The complete action plan is first journaled durably. Immediately before the action can enter `Running`, the engine explicitly calls root materialization; the capability layer creates every absent root component exclusively beneath the retained acquisition descriptor, retains a non-evictable descriptor for the new logical root, records its device/inode in the next journal generation, and synchronizes the parent chain. If any component appears concurrently, root materialization fails closed rather than adopting it. Multiple actions that name the same configured destination share one deterministic scope and therefore one materialized root.

Root acquisition is a no-follow walk from `/`. Repeated canonicalization is not used as a substitute for descriptor ownership. Once a logical-root descriptor exists, it remains the authority even if the visible pathname is renamed or replaced.

If more than one logical scope aliases a directory, resolution is deterministic: longest logical prefix, then lexicographically smallest scope ID. That rule is stable across serialization and recovery.

## Relative-path discipline

`RelativePath` validates at construction and deserialization. It rejects:

- absolute paths and path prefixes;
- `.` and `..`;
- empty normal components;
- NUL;
- invalid scope IDs;
- any component that is not a normal relative name.

A journal cannot turn an invalid string into a usable operand. Every recorded operation path is paired with a `ScopedPath`, and journal validation re-derives that pairing from the current pipeline and plan.

## Recovery authority and journal schema 3

Schema 3 adds:

- a monotonic journal generation;
- root `ScopeRecord`s containing immutable acquisition authority plus optional durably materialized logical-root device/inode;
- scoped journal and write-temporary paths;
- scoped workspace paths;
- scoped authority for every built-in operation operand.

Recovery:

1. re-derives the expected root set from the current pipeline and context;
2. walks only the no-follow ancestor chain of each expected logical root;
3. finds the recorded acquisition device/inode on that chain;
4. verifies the recorded logical/acquisition/base-relative relationship;
5. if the logical root was durably materialized, opens exactly that relative directory no-follow and verifies its recorded device/inode;
6. if it was not durably materialized, verifies that the first absent component is still absent and refuses an appeared pathname;
7. restores retained descriptors;
8. re-derives every operation and workspace scoped path;
9. validates action, state, result, and terminal invariants before mutation.

A journal-provided absolute acquisition path is diagnostic data, not opening authority.

### Journal generation publication

Journal updates use a deterministic same-directory write temporary. The new generation is written exclusively, flushed, and synchronized. If a prior final journal exists, publication uses an atomic descriptor-relative exchange (`RENAME_EXCHANGE` on Linux, `RENAME_SWAP` on macOS), verifies both exchanged inode identities, synchronizes the directory, conditionally removes the displaced old generation, and synchronizes again.

Recovery reads both final and write-temporary files. It accepts only generations with identical immutable journal authority. The sole permitted scope transition is an absent logical-root identity becoming one specific device/inode; demotion or identity replacement is a contradiction. The higher generation wins. Before any action continues, the selected generation is reconciled without first deleting it:

- a newer/equal authoritative temporary is exchanged/published into the final name;
- a proven older auxiliary temporary is removed only when the final file exactly equals the selected generation;
- equal-generation divergent content fails closed.

This prevents rollback to an older journal across the temporary-publication crash windows.

Pass-1 schema-2 journals intentionally fail closed. They did not record retained-root identity, so after process death there is no sound way to prove that a current pathname is the originally authorized directory. Their artifacts are preserved for administrative recovery rather than being reinterpreted as descriptor authority.

## Apply-time identity

The pass-1 content identity remains authoritative. Capability operations add type/device/inode checks at the final descriptor-relative boundary.

Destructive rename transitions (same-filesystem move, delete/source witness staging, and rename staging/installation) receive the planned entry identity. If the source was replaced after planning, the operation fails before the rename call. After a successful rename, the destination entry is checked again and the pass-1 content/provenance checks still run.

Device/inode is supporting evidence only. Cross-filesystem completion continues to require content verification.

## Operation behavior

### Rename

The pass-1 stage-all/install-all transaction remains intact. Sources, staging entries, and destinations are scoped operands. Stage and installation use checked no-clobber descriptor-relative rename, preserving nested and cyclic behavior.

### Copy

The journal-known temporary file/tree is created relative to the destination capability. Regular files use exclusive no-follow creation and opened descriptors. Directory traversal uses inode-checked directory opens and rejects symlinks/special files. Files and directories are synchronized; directory metadata is finalized child-before-parent. Publication is create-if-absent under the destination capability.

### Move

A durable direct-move state first attempts checked same-filesystem no-clobber rename. A successful direct move must retain the planned inode/type and pass the pass-1 content check. EXDEV or a cross-device parent identity transitions to copy → verify → publish → source witness → dispose. Source removal remains rooted in the original source capability.

### Delete

The planned object is moved, with identity checking, into the journal-owned witness. Disposal operates only on that witness. Re-creation at the original relative path is never treated as the witnessed object.

### Create folder

`mkdir_all` processes validated child components beneath an already retained logical root. Concurrent `EEXIST` is accepted only if the resulting component opens no-follow as a directory. Symlinks and non-directories fail. First-time **root** materialization is stricter: every absent root component must be created exclusively, and concurrent appearance is a contradiction rather than an adopted directory.

### Journal/workspace hygiene

New journal and rename-staging directories are created with private `0700` mode where they do not already exist. Cleanup recursively carries the enumerated inode identity into each conditional unlink/removal instead of re-baselining a replaced entry.

## Linux backend

Linux first attempts `openat2` with:

- `RESOLVE_BENEATH`;
- `RESOLVE_NO_MAGICLINKS`;
- `RESOLVE_NO_SYMLINKS`.

`ENOSYS`, `EINVAL`, `E2BIG`, or policy `EPERM` falls back to the portable retained `openat(O_NOFOLLOW|O_DIRECTORY)` component walk.

No-clobber rename uses `renameat2(RENAME_NOREPLACE)`. Journal replacement uses `renameat2(RENAME_EXCHANGE)`. Regular-file publication/fallback uses `linkat` without `AT_SYMLINK_FOLLOW`. Creation, inspection, and removal use `openat`, `mkdirat`, `fstatat(AT_SYMLINK_NOFOLLOW)`, and `unlinkat`.

## macOS backend

macOS uses the retained `openat` component walk for correctness. It resolves `renameatx_np` at runtime, then uses `RENAME_EXCL` for no-clobber rename and `RENAME_SWAP` for journal replacement. An unavailable symbol is reported as `ENOSYS` at the operation boundary rather than preventing process startup. Other operations use the same openat-family/no-follow discipline as Linux.

If an exact directory no-clobber or owned-exchange primitive is unavailable, the operation fails closed. It never degrades to existence-check-then-rename or overwrite-capable `renameat`.

## Cache

Each materialized logical root has a non-evictable retained descriptor. Beneath it, a bounded 128-entry intermediate-directory descriptor cache reduces repeated walks. A cached descriptor is not trusted merely because it is still open: its current parent entry must still be a directory with the same device/inode. Replacement, disappearance, or symlink substitution is a contradiction. Directory rename/removal invalidates cached topology.

## Dependency decision

No dependency was added and neither manifest nor lockfile changed.

The workspace already pins:

- `libc = "0.2"`, locked to `0.2.182`;
- `rustix = { version = "0.38", features = ["fs"] }`, locked to `0.38.44`;
- `rust-version = "1.82"`.

The capability backend uses the existing `libc` dependency in one isolated low-level module. This was chosen because the required contract includes Linux `openat2`/`renameat2`, macOS `renameatx_np`, `fdopendir`, exact errno fallback, and fd-relative synchronization. The pinned rustix 0.38 API does not expose every required cross-platform primitive with the necessary control. `cap-std` would add a second capability abstraction and still require these platform shims. Unsafe code is confined to reviewed wrappers that immediately return owned descriptors or safe result types; it does not spread into planning, journaling, or action state machines.

The existing rustix dependency remains used by the pass-1 coordination code; this pass does not alter it.

## Non-goals

- Unrelated metadata, Blu-ray, CUE, and command atomic-replacement paths are unchanged.
- Browse file tasks are not migrated, although the low-level API is conversion-type-independent.
- Script containment remains the pass-3 supervisor/cgroup problem.
