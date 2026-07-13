# Brief: pre-action journal scope-evolution fabricates recovery at finalize

Date: 2026-07-12. Scope: ONE problem, strictly delimited. For a fresh
reasoning-model session. Assumes the applied conversion-actions
implementation through commit 436c160 (lifecycle handoff applied; flat-flow
companion warning reclassified). Requires no knowledge of earlier rounds
beyond what is stated here.

## Ground truth from real-tree acceptance (reproduced deterministically)

Config default pipeline with a PRE `runscript` plus any post actions. CLI:
`tonepoet convert <album dir> --format flac -o <out>` (with or without
`--disc-subfolders`; both reproduce).

What works: pre script executes exactly once per batch; conversion succeeds;
post actions execute exactly once (rename lands, terminal post-action marker
committed). What fails: EVERY participant's finalize produces a fabricated
pre-phase report with `recovery_required=true`, so no participant ever
records a drain acknowledgment, so the terminal cleanup gate
(`participant_drain_complete && durable_complete && lifecycle_complete`)
never passes and `.tonepoet-batch/<id>/` is retained after every run.

Control: with the pre action removed (post-only pipeline), the identical
conversion drains all participants (`recovery_required=false`) and retires
the workspace to zero residue — flat and multi-disc flows both.

A/B: this reproduces identically on commit c85d416 (before the lifecycle
bundle). It is pre-existing, not a regression.

## Exact failure mechanism (probed, not inferred)

`collect_durable_action_reports_with_binding` (stages.rs) builds a PRE
context from the publication binding, so `prepare_context_for_journal_read`
retains live capabilities and `durable_phase_report` (actions.rs) takes the
`retained_live_context = true` branch. `restore_scope_records` (cap_fs.rs)
then fails with:

    action recovery contradiction: scope album conflicts with an already
    retained capability

Probe data from the retained-capability branch (cap_fs.rs, the
`!matches && !prior_generation_of_recoverable` rejection):

    scope=album
    existing_acq  = ".../out/Wargames (OST)"   base_relative=""            mat_token=None
    record_acq    = ".../out"                  base_relative="Wargames (OST)"  mat_token=Some("b29f…")
    logical_path  identical on both sides: ".../out/Wargames (OST)"
    dev/inode: existing = the album dir; record = the output root

Interpretation: the pre phase ran BEFORE the album directory existed, so its
journal recorded scope `album` parent-anchored at the output root with a
materialization token (`.tonepoet-root-authority-…` bootstrap authority). At
finalize time the album directory exists and the live context retains a
DIRECT capability for it (`base_relative=""`, no token). Same logical
object; the anchor legitimately evolved across the materialization boundary.

The existing tolerance in the `matches` computation accepts materialization
only when the RETAINED capability kept the parent-anchored shape
(`existing.materialization_token.is_some() && existing.base_relative ==
base_relative`). It does not accept the evolved direct anchor, in either the
retained-capability branch or the reopen branch (both emit the same
"conflicts with an already retained capability" error; both were probed and
the retained branch is the one firing here).

## The design question (yours to decide, since this is your capability model)

Under what conditions may a journal scope record that was parent-anchored
with a materialization token validate against a retained DIRECT capability
for the same logical path? The unforgeable link should presumably run
through the durable materialization authority
(`.tonepoet-root-authority-<scope>-…`, `MATERIALIZATION_AUTHORITY_PREFIX` in
cap_fs.rs): the token in the journal record must authenticate against the
authority record, and the retained direct capability's object identity must
match what that authority attests was materialized. A pure
logical-path-string match would open a scope-substitution hole (rename a
hostile directory into place) and is not acceptable.

Note `validate_scope_records` has a related-but-distinct transition comment
("a token-authenticated logical root may have materialized after the last
journal generation") — the accepted shape there also predates this
evolution. Both readers (restore + validate) and both branches (retained +
reopen) need a consistent rule.

## Deliverable

1. Pre+post pipelines drain and retire the batch workspace to zero residue
   in both flat and multi-disc coordinated flows, with the pre journal still
   present at finalize (do not "fix" this by retiring the journal earlier —
   participants finalize concurrently while the journal must remain for
   recovery).
2. The scope-substitution safety argument stated in comments at the
   acceptance rule.
3. Regression tests pinning: (a) the parent+token → direct-anchor evolution
   validates for the same materialized object; (b) a DIFFERENT object at the
   same logical path (recreate/substitute) still fails closed; (c) a
   missing/foreign materialization authority still fails closed.

## What is already fixed (do not re-litigate)

The lifecycle completion handoff (PublishedBatchCompletion), the
warning-free gate poisoning by out-of-scope disc skips, descriptor-route
resolution in records/veto scans/pruners, fresh capability registry per
pre/post context in durable-report collection, bootstrap-artifact tolerance
in the workspace claim check, blocking publish-side action-run locks.

## Diagnostic hooks that worked well

- Temporary eprintln in the two cap_fs.rs ScopeConflict sites printing
  existing vs record acquisition_path/base_relative/dev-inode/token — this
  is what produced the probe data above.
- Temporary eprintln before the drain gate in finalize_report_with_binding
  (stages.rs) printing `action_reports_require_recovery` and each report's
  (phase, recovery_required, notices).
- Real-tree harness: XDG_CONFIG_HOME with a COMPLETE config.toml (sparse
  configs are silently replaced by defaults); a probe script appending to a
  marker file per phase invocation.

## Files in this bundle (the strict slice)

- this brief
- `src/convert/cap_fs.rs` — restore_scope_records / validate_scope_records,
  materialization authority, both conflict branches
- `src/convert/pipeline/actions.rs` — durable_phase_report, journal
  load/validate, prepare_*_capabilities
- `src/convert/pipeline/stages.rs` — collect_durable_action_reports_with_binding,
  finalize_report_with_binding drain gate (context)
- `src/convert/pipeline/types.rs`, `src/convert/pipeline/errors.rs` — shapes

Constraints: suite baseline 3090/0, zero cold-build warnings; the sandbox
cannot compile — favor mechanically verifiable changes; never weaken
fail-closed behavior for objects that cannot be authenticated back to the
materialization authority.
