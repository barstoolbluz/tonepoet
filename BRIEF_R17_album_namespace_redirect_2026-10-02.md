# Brief R17 — a conversion fails because its album directory name changes mid-plan

Date: 2026-10-02
Base: the supplied `tonepoet-src.tar.gz` is our `main` at gate 7244/0.

Every track of an album failed in the TUI. The evidence below was captured
while the session was still open, and is offered **unclassified** — we have not
determined which observations are causes, symptoms, or unrelated. The raw
captures are in `evidence/`.

## What the user sees

```
01 - Shrimp Dance.flac                                              FLAC Failed
└ PlanOutputs: output concurrency admission failed: output planning
  redirected planner-authoritative album namespace from
  '/home/daedalus/temp/Hiroshi Suzuki - Cat (1975) [FLAC]' to
  '/home/daedalus/temp/Hiroshi Suzuki - Cat (1975) [FLAC] {We Release Jazz
  WRJ010LTD Reissue LP  24-96kHz}'
```

All five tracks fail with the same message. The TUI truncates it; the full text
above is from `~/.cache/tonepoet/tonepoet.log`.

## Observations, unclassified

Captured 2026-10-02 17:16 EDT, three minutes after the failures.

- The two paths in the message differ only by a trailing
  `{We Release Jazz WRJ010LTD Reissue LP  24-96kHz}`.
- All five failures carry one timestamp: `2026-10-02T21:14:09Z` (17:14:09 EDT).
- Nothing exists under `~/temp` for this album. No partial output, no staging.
- One `tonepoet` process is running: pid 2932280, `tonepoet tui`. It is the
  session that ran this conversion and it is still open.
- `conversion_queue_scopes` holds exactly one row: scope `59d0205e…`,
  `origin_identity` pid 2932280, created 2026-10-02 13:36:18 EDT.
- `~/.config/tonepoet/concurrency-v1/queue-scope/` holds exactly one lease
  file, mtime 13:36:18, matching that row.
- A different scope (`09cb2fc0…`, pid 800268, created 2026-10-01 21:11) was
  present in that table at 13:35 today and is no longer there.
- `conversion_queue_executions` is empty — 0 rows.
- 29 queue rows exist, all owned by scope `59d0205e…`. Five mention this album;
  all five have `execution_id = NULL`.
- Directory mtimes under the coordination root:

  ```
  13:36:18  queue-scope/<the one lease file>
  17:05:01  concurrency-v1/
  17:06:22  queue-scope/
  17:08:27  journal-operation/
  17:11:43  ephemeral-mutation/
  17:14:09  execution-claim/
  17:14:09  execution-staging/
  17:14:09  queue-execution/
  ```

  The last three carry the failure timestamp to the fraction
  (`17:14:09.2248087420`) and are empty now.
- The message text occurs once in the tree, at `src/convert/pipeline/stages.rs`
  in `admit_planned_output_claim`.
- The user's recollection is that this belongs to a family of failures
  involving concurrent sessions invalidating leases, related to issues #15,
  #18 and #59. We did not establish whether it does.

## What we did not determine

Whether a second session existed at 17:14:09; we found no trace of one in the
two places we looked, which is not the same as proving absence. Whether the
album directory name is expected to change after a claim is admitted. Whether
the empty `execution-claim`, `execution-staging` and `queue-execution`
directories were written and cleaned at the failure, or never written.

## The outcome we want

An album whose directory name is fully determined before work begins converts
successfully. Where the name legitimately changes while planning, the
conversion follows it rather than failing.

Where a conversion genuinely cannot proceed, every track does not fail with
one internal message about namespace redirection. The user is told what is
wrong with their album and what to do about it.

## State

Gate 7244 passed / 0 failed across 63 targets, zero warnings.
`tonepoet-true-peak` is gated separately and was not run today; CLAUDE.md
records it at 160/0. Reference qualification current as of 2026-10-02.

## Build capability

`cargo test -p tonepoet-pipeline` builds in seconds. `stages.rs` is in the root
`tonepoet` crate, which peaks at 6.24 GB in one `rustc` and will OOM under a
4 GB ceiling; write those changes uncompiled and say so. `stages.rs` is also
Reference-source-locked, so a change there requires requalification on our
build host — say so in the delivery notes and we will run it.
