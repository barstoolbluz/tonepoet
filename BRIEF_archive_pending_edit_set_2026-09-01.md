# BRIEF — One pending edit set per archive, committed once

**Date:** 2026-09-01
**Base:** `main` @ `3714ac1`, with the passwords/RAR/prompt delivery and its RAR readback
corrective applied
**Related:** `OUTSTANDING_ISSUES.md` #23

## What the user wants

**Every change made inside an archive accumulates into one pending set for that archive, and
is written once when the user does something that forces a commit.**

"Every change" means every kind: renaming a file, renaming a folder including one that exists
only as an implicit path prefix, deleting files or folders, creating folders or adding files,
and editing member metadata — tags, artwork, ReplayGain.

Scope of a commit: **one pending set per archive, committed per archive.** Editing two
archives before navigating away is two commits, one for each.

The user should not be able to tell which operation took which internal path. Rename, delete,
create and metadata edit should behave identically and land in the same commit.

## Why this is being asked for

Renaming a folder inside an archive currently writes the archive immediately. Renaming a
second thing writes it again. On a remote archive each of those is a full round trip —
localize the archive to local storage, apply the rename, verify, copy back, install.

This is a regression in behaviour, introduced by `77d55bd` ("Archive access: mount-first
materialization and native structural renames"). It was reported by the user as *"this is even
worse than it was"*, and they are right: before that commit, renames accumulated and the
archive was rewritten once.

It was logged as `OUTSTANDING_ISSUES.md` #23 the same day and never scheduled. The locality
work in `653cb1e` then increased the per-operation cost, because a remote source is now copied
in and copied back rather than copied once beside itself.

## What exists today

### The deferred model already exists, and most operations use it

Delete, create, metadata edit, and the *extraction fallback* path of rename all stage their
work and report `"…; archive changes pending"`. The commit is triggered by
`deferred_browse_archive_exit`, `deferred_browse_archive_screen_switch`, or the
`quit_after_*` flags — that is, navigating away, switching screens, or quitting.

### Native rename does not

The native rename path — `7z rn` for 7z/ZIP, `xorriso -mv` for ISO-WV — installs its result
through the backup/install/restore transaction immediately. Its completion arm creates no
staging session and records no pending state; it reports
`"renamed archive entry in X: old -> new"` and the archive is already rewritten.

So two commit models operate on the same archive depending on which operation the user
performed and whether the tools for the native path happen to be available.

### The pending set already models every change kind

`ArchiveEdit` (`src/tui/browse.rs`) has five variants: `Rename`, `MetadataWrite`,
`ContentModified`, `Delete`, `Create`. `ArchiveStagingSession` carries `edits: Vec<ArchiveEdit>`
and is already persisted as the crash-recovery record.

### "Does this need saving" is already a computed question

`archive_staging_has_net_changes(listing, staging)` compares the staged state against the
archive listing, and `reconcile_active_archive_staging_dirty` assigns its result. That work
landed at `3714ac1`. So the system can already answer whether a pending set represents a real
difference, rather than merely that edits occurred.

### The batching primitive exists and is already used

`rename_archive_entry_native_transactional` takes `rename_pairs: &[ArchiveNativeRenamePair]` —
a slice, not a single pair — and the implicit-directory case already passes many pairs so that
one `7z rn` invocation renames a whole subtree. The ISO-WV branch currently rejects more than
one pair (`"ISO-WV native rename requires exactly one rename pair"`), which is a constraint to
resolve rather than a fixed limit.

### The unresolved interaction

The native path's admission check does not consider whether a dirty staging session already
exists for that archive. A user who deletes something (staged, pending) and then renames
something (native, immediate) has an immediate install landing underneath a pending staging
tree. Whether that is currently safe has not been traced, and a unified model removes the
question entirely.

## The design tension to resolve

The two models want different things at commit time.

The staged model repackages **from an extracted tree** — that is what makes it able to
express any change. Its cost is one extraction and one repackage per session.

The native model avoids extraction entirely by mutating the container, which is why it is
fast. But it can only express changes the container's tooling supports.

A unified pending set has to decide, at commit, how to apply an accumulated set of edits that
may mix both kinds. Whether that means always extracting, or inspecting the pending set and
choosing the cheapest expression of it, or something else, is the implementer's judgement.
The cost asymmetry is real: extraction of a multi-gigabyte archive is the operation this whole
line of work has been trying to avoid, and a design that reintroduces it for every session
would trade one regression for another.

## Outcomes wanted

- Every change inside an archive joins one pending set for that archive.
- Nothing is written to the archive until a commit is forced; the existing triggers
  (navigate away, screen switch, quit) remain the triggers.
- N changes in a session cost one commit, not N.
- The user cannot tell, from the behaviour, which internal path an individual change took.
- A pending set that no longer differs from the archive does not ask to be saved — the
  net-change computation already added for that purpose keeps working.
- Whatever the commit does, the result must be an archive Tonepoet can read back. The RAR
  corrective established that verification has to run through the real reader rather than the
  writer.

## Scope

**In scope:** the commit model for archive edits, across all five change kinds, and whatever
is required to make the native fast paths participate in it.

**Out of scope:** the larger deferred-commit / virtual-view redesign the user has parked for
a later round, and the pipeline redesign. This brief is about making the existing operations
agree on when they write.

Also out of scope: `OUTSTANDING_ISSUES.md` #22 (reflink/clonefile availability) and #27 (the
conversion manifest), which are open in the same area but independent.

## Working constraints

- The implementation container has no Rust toolchain, no Nix, and none of the archive tools.
  Running the gate is the operator's job; no delivery should assume it has been run.
- The archive transaction — backup, install, restore on failure, with fingerprint re-checks
  around long transfers — is established and should not regress. Commit becoming less frequent
  should not make it less safe.
- Passwords must not reach logs, status text, or sanitized command records.
- Plain letters in Browse remain reserved for type-ahead. No F-keys. No emoji or decorative
  unicode in UI text.
- Tests that mutate process-global state have caused repeated flakes in this project.
