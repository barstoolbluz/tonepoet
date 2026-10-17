# BRIEF — issue #59: a second TUI instance blocks metadata saves on albums it never opened

The tree is clean: gate **7526 passed / 0 failed**, zero aborted test binaries,
Reference qualification binding, tagged `v0.5.5`.

This round is one issue, and it is a diagnosis task before it is a fix.

## The symptom

Two TUI instances running. Saving metadata on a nine-track DSF album failed in
the second:

```
Metadata: 0 saved, 9 failed, unsaved changes remain — metadata-editor save busy:
filesystem mutation conflicts with live owner: '/home/daedalus/torrents/...
```

Closing the other instance and retrying saved successfully. **That instance did
not have this album, or this folder, open.**

## What was established, and what was not

The guard refuses when a requested claim overlaps a claim held by a live owner
in a different execution lifecycle and coordination group.

The cause was never found. By the time anyone looked, the other instance had
exited and its surviving lease held empty paths and claims, so nothing on disk
explained the refusal. Three possibilities were listed and none eliminated: a
claim on an ancestor directory that overlaps every album beneath it, stale
claims from a completed operation, or something else.

One lead was noted and not pursued. An `__execution-item-supervisor` child had
been alive 97 minutes with no children, no CPU and only a socket descriptor,
parented to an idle TUI with no conversion running. Whether a stranded
supervisor keeps an execution identity live was not determined.

## Why it could not be diagnosed

Nothing logs claim registration or conflict. Verified today:
`src/concurrency.rs` contains exactly five logging calls, four warnings and one
error, all reporting failures — no trace, debug or info at any level.

From the original field session, recorded in the issue rather than measured
here: the application log carried 5,824 lines mentioning the other instance's
scope, every one the same five-second recovery poll, and not one recording what
that scope claimed. The status-bar message is truncated before the overlapping
path and owning execution, which is the only place that information appears at
all.

I believe this is why the issue cannot say what conflicted, and why a fix
attempted without closing this gap would be guesswork.

## What we want

A second instance that has not opened an album does not prevent saving it, and
whatever scope the guard uses is no broader than the work actually in flight.

When a claim conflict does occur, the reason names the overlapping path and the
owning execution somewhere durable, so a transient conflict can be
reconstructed after it clears.

We are asking for your analysis of the claim and lifecycle code and your
best determination of the cause, then the correction that follows from it —
not a broadened or disabled guard.
