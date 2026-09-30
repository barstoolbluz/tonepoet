# Brief R5 — your new fail-closed regression does not pass

Date: 2026-09-29
Base: `main` @ `191fc8b` with Corrective R3 applied, plus two compile fixes

R3 applies cleanly and the gate is 7203 passed / 1 failed. The one failure is a
regression R3 added.

---

## The failure

`convert::pipeline::track_executor::tests::pcm_true_peak_consumed_rate_mismatch_still_fails_closed`

Deterministic — it fails in isolation, not only under the parallel gate.

```
a charged rate that disagrees with the realized carrier must refuse:
SelectedPcmTerminalRealization { kind: FfmpegDirect, ..., target_rate_hz: Some(192000),
  target_bit_depth: Int32, effective_dither: Some(Tpdf), dither_owner: SelectedTerminal }
```

The test expects a refusal and gets an accepted realization.

## What is established

The guard returns early at `if !execution_state.pre_observation_rate_edge_consumed`,
so the rate comparison below it never runs.

That flag is computed as *charged target rate equals measured carrier rate*, and
additionally that the original source rate differs from the carrier rate.

The fixture builds the track with a carrier at 176400 and a terminal-candidate
charged rate of `Some(192000)`. Those are unequal, so the flag is false, so the
guard returns `Ok` before reaching the comparison the test is asserting on.

The flag is derived from the track's terminal candidate when the execution state
is published. The comparison is made against the charged realization passed in
at execution time. The function's own documentation says it is written so the
decision survives "a later execution step chang[ing] the concrete
`TrackSourceRef` representation", so those two values are expected to be able to
diverge.

No other test exercises the flag-false path. The sibling test directly above
does assert `!pre_observation_rate_edge_consumed`, but its charged realization
has `target_rate_hz == None`, so the guard returns at its first early return
and never reaches the flag. Nothing in the suite currently establishes what
flag-false is meant to do when a charged rate is present.

## Two readings

Either the fixture is wrong — to reach the mismatch it would charge 176400 so
the flag is true, then pass a divergent 192000 as the charged realization — or
the guard is wrong and the rate comparison belongs outside the flag gate.

We cannot tell which you intended, and the difference matters: one is a test
correction, the other means a charged rate that disagrees with the realized
carrier is currently accepted rather than refused. That is the condition behind
issue #55, so if it is the second, the guard meant to catch it is not catching
it.

## Required

The regression passes, and a charged terminal rate that disagrees with the
realized true-peak carrier rate is refused wherever that disagreement can
actually arise. State which of the two readings was intended.

---

## Two compile errors were fixed here

Both would have been caught by compiling; neither is a design problem.

`src/convert/pipeline/stages.rs` — the sequential convert path was given a
`failure_context` argument that is never bound in that function; only the
scheduler path carries one. It now passes `None`, which the helper already
handles by falling back to the plain multi-failure message. Confirm that is
what you intended for the non-scheduler path.

`src/convert/pipeline/track_executor.rs` — `AudioFormat` is not `Copy` and
`target_format` was used after being moved into `request.settings`. Now cloned.

## One dead-code warning left in place

Your refactor removed the last caller of
`src/concurrency.rs::local_persistent_lease_file`, which now warns as unused. It
is pre-existing and private; it was left alone rather than deleted here.

## Otherwise accepted

The other four R4 items look right and are not reopened. The four-channel AVX
geometry is confirmed as Left/Right/LeftSurround/RightSurround, matching what
was chosen on this side, and a static invariant now asserts it.

16/16 staging tests pass. Your target-free verifier passes. No contention
flake appeared in this gate run.

## Still unproven

Issue #55 cannot be called closed. The real conversion has not been re-run
against R3, and the guard that would catch the disagreeing-rate condition is
the subject of this brief.

## Build capability

`cargo test -p tonepoet-pipeline` and `cargo test -p tonepoet-true-peak` build
in seconds and need no audio tools. The root `tonepoet` crate peaks at 6.24 GB
in one `rustc` even at `-j 1` with debug info off and will OOM under a 4 GB
ceiling; write those changes uncompiled and say which ones they are.
