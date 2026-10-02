# Brief R15 — an interrupted conversion leaves bytes in the user's folder and a reservation that bricks the album

Date: 2026-10-02
Base: the supplied `tonepoet-src.tar.gz` is our `main` at gate 7230/0.

Issues #53 and #44 are the same event seen from two sides. Killing a
conversion mid-flight leaves two things behind: hundreds of megabytes of
staging in the user's output folder, and a durable reservation that makes that
album permanently unconvertible. Neither is cleaned up by anything.

Both were reproduced on a default configuration today, deterministically.

---

## What was measured

A 22-track DSD64 SACD ISO converting to FLAC, `SIGKILL`ed about fifteen seconds
in, with no `scratch_directory` configured — the default any user has.

**Left in the output root:**

```
.tonepoet-staging/
.tonepoet-staging/.job-<uuid>.run.lock
.tonepoet-staging/job-<uuid>-<uuid>/converted/
.tonepoet-staging/job-<uuid>-<uuid>/realized-sacd-tracks/
    .sacd_bach_vol_1_…_track_001_….dsf.<pid>.<nanos>.tmp
    … one per track realized so far
```

How much depends on how far it got: two runs killed at different moments left
658 MB and 1.8 GB (11 `.tmp` files, 44 entries, 1 lock).

Nothing removes it afterwards. Converting a *different* album into the same
output root succeeds — `1/1 succeeded` — and the orphan is untouched: still
1.8 GB, still 11 `.tmp` files, same `job-<uuid>-<uuid>` directory. The root now
holds the new album beside a staging tree from a process that died.

**And the album is now permanently unconvertible:**

```
PlanOutputs: output concurrency admission failed: filesystem mutation conflicts
with recovery reservation: '…/Bachkantaten (BWV 55, 56, 98, 180)' overlaps
'…/Bachkantaten (BWV 55, 56, 98, 180)'; queue execution
fe1f6d05-e666-466b-8005-6ab8017106f1
```

The path is reported as overlapping itself. The named queue execution is the
killed process: row `fe1f6d05-…` in `conversion_queue_executions` carries
`item_id 87876a7a-…`, the job that was running, and
`origin_identity {"pid":1185854,…}`, the pid that was killed.

Three further measurements narrow it:

- Deleting `.tonepoet-staging` entirely does **not** release the reservation.
  It survives the directory.
- A brand-new output root with the same album name converts fine. The
  reservation is scoped to that album path, not global.
- The row persists with `state = 'interrupted'` and `external_released = 0`,
  across process exits and later successful conversions. Two such rows are
  present after two killed runs. Nothing reaps them.

## What is already fine

- With `scratch_directory` set, staging goes to tmpfs and the output root stays
  clean; `/dev/shm/tonepoet` is 0 bytes after successful runs. The leak is
  specific to staging living in the output root, which is the default.
- On the success path the output root ends clean either way: audio, plus the
  companion files copied from the source folder, and nothing else.

## What did not reproduce

`.tonepoet-manifest.json` did not appear in any run — not with default flags,
not on the Reference SACD route, not in the output root nor the album folder.
The code #53 names is still present and unchanged (`stages.rs`, "Native
Reference publication always carries manifest-v2 authority",
`reference_manifest_required`), so either that publish site is not reached on
this route or the condition no longer fires. We did not establish which, and
we are not claiming it is fixed.

---

## Outcome 1 — an interrupted conversion leaves nothing in the user's folder

No bytes tonepoet created for its own purposes survive in a destination the
user did not ask for them in — not on success, not on failure, not on cancel,
not after a kill. Working space belongs in tonepoet's own temp or scratch
space by default, not in the output root.

Where a same-filesystem atomic publish genuinely requires a sibling of the
destination, it is removed on every exit path, and anything orphaned by a
process that died without running its cleanup is swept later, without the user
having to know it exists.

This is the third time this pattern has been filed. The standing rule from #53
holds: a conversion writes the audio, and when enabled the log, CUE sheet and
companion artwork. Nothing else, on any route, under any option.

## Outcome 2 — a dead execution does not own anything

A reservation held by a process that no longer exists does not block a new
conversion. Recovering or releasing it is tonepoet's job, not the user's, and
it does not require them to find and delete anything by hand.

Keep the guard. It is protecting a real invariant, and it should still refuse a
genuine live conflict. What it must not do is let a killed process hold an
album forever.

The refusal text should also name something the user can act on. Reporting
that a path overlaps itself, and quoting the UUID of a queue execution that is
gone, tells them nothing.

---

## Likely related, not verified

The same reservation machinery and the same "filesystem mutation conflicts
with …" wording appear in #59 (a second TUI instance blocking metadata saves
on albums it never opened) and #18 (two conversions into one album directory
refusing each other). #59 has been hard to pursue because the evidence vanished
before it could be inspected. If these share a root, this reproduction is a way
in. We have not confirmed that they do.

## State

Gate 7230 passed / 0 failed across 63 targets, zero warnings. The Reference
qualification is current as of 2026-10-01 and
`tests/reference_qualification_freshness.rs` will fail the gate if a locked
source drifts from it.

## Build capability

`cargo test -p tonepoet-pipeline` builds in seconds. The staging and admission
code is in the root `tonepoet` crate, which peaks at 6.24 GB in one `rustc` and
will OOM under a 4 GB ceiling; write those changes uncompiled and say so.
