# BRIEF — two R25C fixtures claim a workspace they have made look live-owned

R25C is applied and committed as `9b2ca9e`. Gate: **7403 passed / 3 failed** —
the expected freshness failure, and the two fixtures below. Most of the round
passes; two of its four behaviours are left unverified, because the fixtures
that would prove them are the ones failing.

A note on how to read this. Quoted output and file citations are measured.
Everything else is our reading of the code and may be wrong; where we say what we
believe, treat it as a starting point rather than a finding.

## What landed

The provenance fix is better than the brief asked for. Rather than swapping one
hardcoded key for the other, `conversion_log_catalog_number_entry` resolves which
of `sacd_album_catalog_number` / `sacd_disc_catalog_number` holds the value, with
tests for the album-only and disc-only cases.
`r24_sidecar_field_origins_follow_selected_values_without_exporting_log_markers`
passes.

`r25_fail_if_exists_preflight_refuses_occupied_paths_before_extraction` passes,
which is #72 at its original scope.

`out_of_album_fragment_repair` (two tests),
`write_conversion_log_disabled_suppresses_fragment_for_batch_jobs` and
`multi_root_retry_repairs_roots_and_fragments_committed_before_batch_finalization`
all pass, so the R23 journal and the retained behaviour survived the removal.

The delivery notes decline to cover a crash window between removal of the
per-file rollback marker and durable installation of the success fragment, fail
closed there, and warn against restoring broad dead-owner byte-matching to
paper over it. That warning is worth keeping: broad byte-matching is what this
round removed.

R25C also did not compile. `stages.rs:74837` used
`super::coordination_name::album_coordination_token` from inside a test module,
where `super` does not resolve to the pipeline module; changed locally to
`crate::convert::pipeline::coordination_name::`, the form already in use at
`stages.rs:50340`.

---

## The two failing fixtures

```
r25c_recovery_requires_matching_installed_track_fragment
r25c_multi_root_retry_preflight_requires_every_installed_target_fragment

called `Result::unwrap()` on an `Err` value: Custom { kind: PermissionDenied,
error: "conversion-log workspace .../.tonepoet-batch/tonepoet-log-batch-v1-...-1
contains unowned state and cannot be claimed automatically" }
```

Both fail **3 of 3 in isolation**, so this is not the coordination-contention
flake that wanders through this area.

These two are the regression coverage for items 2 and 4 of R25C's own scope —
target-specific crash and retry evidence, and multi-root recovery. So those two
behaviours are currently unproven: the preflight and publisher changes are in
the tree, but nothing demonstrates that an occupied target is refused without
per-target evidence, or that a partially fragmented multi-root retry is refused.
That is the practical cost of these failures, beyond the gate count.

The batch ids in the paths decode to `r25c-multi-root-proof` and
`r25c-target-fragment-retry`, so these are workspaces the fixtures build for
themselves.

What we believe is happening, for you to confirm or correct. The discriminator
against the sibling that passes is the batch-id generator:

| fixture | generator | result |
| --- | --- | --- |
| `r25c_dead_owner_without_target_publication_evidence_cannot_claim_old_audio` | `generated_test_batch_id` | passes |
| `r25c_recovery_requires_matching_installed_track_fragment` | `generated_current_process_test_batch_id` | fails |
| `r25c_multi_root_retry_preflight_requires_every_installed_target_fragment` | `generated_current_process_test_batch_id` | fails |

Our reading is that a current-process batch id makes the planted workspace look
live-owned by the running process, so the claim path finds state for which it
holds no ownership record and refuses rather than adopting it. The dead-owner
fixture wants an ownerless workspace and gets one. Both failing fixtures appear
to want a previous batch that is provably finished or dead, while constructing a
workspace that looks like it belongs to this process right now.

**What we want.** These two fixtures stand up the state they are describing — a
prior batch whose per-target publication evidence exists, and a multi-root retry
whose fragments are installed — so that each one exercises the admission rule it
was written to prove. Whether that means a different batch identity or
registering the ownership record first is a judgement about the protocol R25C
intends, and we have not made it.

---

## Standing note on compile failures

Four deliveries in eight rounds have arrived with exactly one kind of compile
failure: a test naming a type, helper or module by a path that does not resolve.
R18 (`OverwritePolicy`), R22C (`SourceMetadata`), R24 (two private sibling-module
helpers), R25C (`super::coordination_name`). In two of them the exact working
path was already in use elsewhere in the same file; in R18 the same import form
sat on the adjacent line; R24 needed a visibility change rather than a path. Each is a minute to fix here and none
has ever indicated a problem with the delivered behaviour, so this is recorded
rather than raised — the static checks in these deliveries are thorough on patch
reproduction and hashes, and name resolution is simply not something they can
reach without a toolchain.

## Still owed

The live Browse → Convert → preset gesture against a real SACD ISO (#43).

## Reference qualification

`stages.rs` has moved again. Requalification has not been run, because the
runner would stop at the workspace suite while these two fixtures fail. One
requalification after they land is the economical order.
