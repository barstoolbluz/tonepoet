# Brief R2 — the Problem 1 fix refuses a legitimate write

Date: 2026-09-29
Base: R1 corrective applied on `apply/gate-portable-reference-p3-r1` @ `e645317`
(your three commits cherry-picked unchanged, plus one compile fix)

Problem 1 is not fixed: the nondeterministic read-side failure is gone and a
deterministic write-side refusal has taken its place.

Problem 3 passes its regression here. Problem 2 compiles and breaks nothing in
the gate, but nothing in this run actually exercises it — portable Reference is
verified by requalification against a staged runtime tree and by a package that
does not exist yet, and neither has been done. Treat it as unverified, not as
confirmed.

---

## The regression

`tui::file_task_runtime::tests::compact_deltas_do_not_repeat_the_complete_job_plan`
now fails, **in isolation**, every time:

```
create journal: "persistent lease descriptor exceeds 1048576 bytes before publication:
  <tmp>/claims/journal-operation/<uuid>--<uuid>.lease"
```

That is the new pre-publication check rejecting a write. The test passed in both
pre-R1 gate runs on this machine. It is not contention-sensitive and it is not
the old flake.

The journal it creates carries 2,000 paste mappings, which become 4,000 path
claims — one for each source and destination
(`src/tui/file_task_runtime.rs:3131`, `:2189`). Nothing about that is pathological; it is
what a large copy job looks like.

Your own oversize regression asserts rejection at
`synthetic_descriptor_claims(4_000, 320)` — the same claim count. Whatever
separates "legitimate growth" from "genuinely oversized" currently does not
separate these two cases.

### Required

A large-but-ordinary file operation creates its journal successfully. A
descriptor that is genuinely unbounded still fails closed before publication.
The reader limit stays exactly 1 MiB and fail-closed reading is not weakened.

Whatever distinguishes those two cases is stated explicitly and covered by a
regression that uses a realistic claim set, not a synthetic one — the synthetic
case alone did not catch this.

---

## Also observed, lower priority

`tui::probe::tests::common_write_lock_parent_sync_failure_is_reported_after_committed_tag_write`
fails under the parallel gate and passes alone:

```
create persistent lease staging file
  <tmp>/claims/ephemeral-mutation/.<uuid>--<uuid>.lease.tmp-<uuid>:
  No such file or directory (os error 2)
```

A missing parent directory for a staging file, which reads like a shared
coordination root being torn down by another test rather than anything about
descriptor size. Different shape from the failure R1 addressed. Whether it is
in scope is your call; the deterministic-gate requirement from the previous
brief still stands and this is the only remaining nondeterministic failure.

---

## Not defects — expected, stated here so they are not chased

Two tests fail because the promoted evidence is deliberately stale:

- `convert::pipeline::track_executor::tests::phase5_promotion_binds_core_and_optional_metadata_closure_variants`
- `convert::pipeline::track_executor::tests::production_promoted_evidence_refuses_missing_sox_before_any_tool_launch`

Both refuse with "Reference qualification report is incomplete or does not bind
the exact candidate/required closure variants." That is the documented
consequence of the active v18 policy gaining the dispatch-variant set while the
checked-in promoted artifacts stay on the previous closure. They are expected to
clear on requalification and are a useful signal that it is still owed. Do not
hand-edit evidence to silence them.

---

## Gate state on this machine

`cargo test --workspace --no-fail-fast --exclude tonepoet-true-peak`:
7189 passed, 4 failed — the one regression, the one contention failure, and the
two expected qualification refusals.

`cargo check --workspace --all-targets` is clean. `cargo test -p tonepoet-pipeline`
is 315/315. Your Problem 3 regression,
`album_pcm_true_peak_rate_change_consumes_charged_rate_before_final_terminal_shape_check`,
passes.

## One compile fix was applied to your R1 bytes

`src/concurrency.rs`: the new
`oversized_schema_two_descriptor_fails_before_any_lease_is_published` called
`PersistentLease::acquire`, which does not exist; the constructor is
`create(family, &[PathClaim])`. Two unused test imports in
`track_executor.rs` were also dropped. Nothing else in your three commits was
altered. Build what you can — `tonepoet-pipeline` and `tonepoet-true-peak` both
compile in seconds and need no audio tools; only the root crate is out of reach.

## Unchanged from the previous brief

Sequencing, the requalification procedure and its ~46 minutes on our build host,
and the outstanding R6 items (4,368-cell physical SSRC grid, one real WavPack
decode comparison) all stand as previously stated.
