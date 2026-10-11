# BRIEF — R31 is accepted; and the true-peak row advertises a command instead of explaining itself

R31 is applied and gated. Both items are fixed and the gate is otherwise clean.

This is how it reads to us from the outside; we misapprehend this codebase
routinely, so where we are wrong, trust your own reading.

## What is verified

Gate: **7437 passed / 2 failed**. Compile clean, no unused or `missing_docs`
warnings. `r31_` 3 of 3, `r30_` 10 of 10.

```
execution_staging_live_and_recovery_reserved_block_stale_cleanup_until_retired
installed_reference_qualification_binds_the_current_locked_sources
```

The first passes 3 of 3 in isolation and is the documented lease-ownership
contention flake. The second is the expected freshness failure; `stages.rs` and
`settings.rs` moved in R30 and requalification has not yet been run.

Your correction was right and ours was wrong: `command.rs` assigns
`result.true_peak_dbtp` from `measure_loudness()` at two sites, so the
user-facing analysis does report true peak. Our grep was truncated and then
read as complete.

## 1. The true-peak row's trailing text

The row currently ends with:

```
  0.25 dB steps; :set dsd-true-peak-target <dBTP>
```

**What we want:** that text gone. The step size is self-evident from using the
control, and the row should not advertise a command. The `:set` keys keep
working exactly as they do now.

In its place, a `ⓘ` affordance that explains what true peak is. The pattern
already exists for pre-emphasis, which is the look being asked for.

How the explanation is surfaced is open — pre-emphasis renders it inline in the
row, which is the clutter being removed here, so inline may be the wrong answer
for a concept explanation that does not change run to run.

## Requalification

`R31_ACCEPTANCE.sh` chains the `r31_` regressions into the unchanged
`R30_ACCEPTANCE.sh`, which runs `scripts/requalify_reference_r20.sh`. We have
not run it yet. R31 itself edited no locked file; the outstanding freshness
failure is R30's.
