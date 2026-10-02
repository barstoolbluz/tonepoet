# Brief R16 — R15 is correct; two older tests still assert what it removed

Date: 2026-10-02
Base: the supplied `tonepoet-src.tar.gz` is the unmerged branch
`apply/r15-interrupted-conversions-2026-10-02` — our `main` plus R15 corrective
R2 and the Reference requalification it forced. It is not merged, because the
gate is not green.

R15 is accepted in substance and field-verified. Two tests fail, and neither
is a defect in R15: both are pre-existing tests that pin the behavior R15 was
asked to change. They need to move with it.

---

## What R15 fixed, measured

Killing a 22-track SACD conversion mid-flight, no `scratch_directory`
configured:

| | before R15 | with R15 |
|---|---|---|
| output root after the kill | 658 MB – 1.8 GB orphaned | **0 bytes** |
| converting that album again | refused on every later attempt | **1/1 succeeded** |
| final folder | staging tree beside the audio | audio + companions only |

#44 and the staging half of #53 are resolved end to end. #53's manifest half
is untouched here: `.tonepoet-manifest.json` never reproduced on any route we
tried, before or after R15, so there is nothing to confirm. It stays open.

## The two failures

Both fail 3/3 in isolation, in both full-gate runs, and in all three loaded
lib runs. Neither appeared in any failure list across four pre-R15 gate runs
or three loaded lib runs on `main`. They are deterministic, not the known
flake.

### 1. A test still requires staging in the user's output root

`convert::processor::tests::scratch_post_materialization_stage_enospc_retries_disk_before_terminal_failure_publication`

```
expected: …/.tmpbyoxJk/out/.tonepoet-staging                       ← the output root
actual:   …/tonepoet-test-conversion-work-…/.tonepoet-staging      ← TonePoet cache
```

The assertion is `output_root.join(".tonepoet-staging")`. That is the defect
#53 exists to eliminate, written into a test. R15 does the right thing and the
test refuses it.

### 2. A test pins the old conflict wording

`tui::keybindings::artwork_file_picker_handoff_tests::artwork_host_rename_rejects_conflicting_tonepoet_claim_then_succeeds`

The rejection still happens — `expect_err` passes. Only the wording moved:

```
host-managed claimed rename failed for …/cover.jpg: filesystem mutation
conflicts with live owner: path '…/cover.jpg' is already reserved; retry after
the owning operation finishes
```

The test requires `"busy"` or `"overlap"`. The new text is better, and it is
what R15's brief asked for — a refusal naming something the user can act on.

## Required

The gate is green, and the expectations that move are the ones describing
behavior R15 deliberately changed. Nothing weakens: the ENOSPC test still
proves a disk retry happens and is logged with its job, item, path and cause;
the artwork test still proves a conflicting claim rejects the rename, leaves
the source intact, and admits it afterwards.

If any assertion cannot be updated without losing what it was protecting, say
so rather than deleting it.

## Not R15's

`convert::pipeline::memory_budget::tests::execution_staging_live_and_recovery_reserved_block_stale_cleanup_until_retired`,
the regression R15 added, failed once in three loaded runs and passes 3/3 in
isolation. That is the known coordination flake, not a delivery defect.

For scale: three loaded runs on `main` produced 8 failures across 5 tests;
three with R15 produced 10 across 6. R15 does not destabilise the suite. Only
the two tests above separate cleanly, 0/3 against 3/3.

## State

Reference requalified after R15 changed the locked `stages.rs`: 3903 positive /
34 negative, passed; runtime closure fingerprint `89252720…`. The freshness
gate added on 2026-10-01 caught that drift on its own, before anything merged.

## Build capability

`cargo test -p tonepoet-pipeline` builds in seconds. Both failing tests live in
the root `tonepoet` crate, which peaks at 6.24 GB in one `rustc` and will OOM
under a 4 GB ceiling; write those changes uncompiled and say so.
