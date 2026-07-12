# Brief: post-action lifecycle never completes in the flat coordinated CLI flow

Date: 2026-07-13. Scope: ONE problem, strictly delimited. This brief is for a
fresh reasoning-model session; it assumes the applied conversion-actions
implementation (commit 4f0bd82 + acceptance fixes) and requires no knowledge
of the earlier design rounds beyond what is stated here.

## Ground truth from real-tree acceptance (reproduced deterministically)

Setup: config default pipeline with THREE actions — pre `runscript`
(probe.sh), post `runscript` (probe.sh), post `rename` (`*.log`,`*.cue`,
template mode). CLI: `tonepoet convert <album dir> --format flac -o <out>`.
The probe script appends a line to a marker file per invocation and, in the
post phase, records whether the album publication lock is held.

Case A — multi-disc album, `--disc-subfolders` (83 tracks, WarGames):
WORKS COMPLETELY. Post rename fires once per album with correct per-disc
numbers, workspace fully retired (zero `.tonepoet-*` residue at the output
root), two runs produce byte-identical trees including hidden entries.

Case B — flat single-album-dir batch (38 tracks, `CD 1` alone, no
disc-subfolders): pre runscript executes exactly ONCE (correct), conversion
succeeds 38/38, BUT:

1. The POST phase never executes: no post runscript invocation, companions
   keep their original names (the same rename works in case A).
2. The `.tonepoet-batch/<batch-id>/` workspace is retained after the run
   (state=complete, completion marker, drain manifest present).

No warning or error is logged for the missing post phase at RUST_LOG=warn.
The only action-related warn is the benign cgroup→subreaper fallback notice.

## What is already fixed (do not re-litigate)

The apply + acceptance rounds already fixed, with tests: fail-fast in-process
album authority during concurrent per-track publishes (blocking
`acquire_blocking_action_run_lock_*` variants used by the publish path);
descriptor routes persisted into companion disc-destination records
(`resolve_descriptor_route_to_stable`, fail closed); shared capability
registry across pre/post contexts in `collect_durable_action_reports_with_binding`
fabricating recovery reports that silently blocked participant drains;
stable-vs-route mismatches in the cleanup veto scan
(`workspace_has_unresolved_action_state_inner`) and the journal pruner
(`collect_terminal_action_journals`); terminal `actions-*.result.json`
missing from cleanup's recognized-artifact list; journal-root bootstrap
artifacts (`.tonepoet-action-journals`, `.tonepoet-root-authority-*`)
tripping the workspace claim check before first ownership (pre-action flow).
After those, case A is fully green end to end.

## The one remaining problem

In the flat coordinated flow (case B), the post-action election/lifecycle
never runs the post phase, and consequently the terminal cleanup gate
(`participant_drain_complete && durable_complete && lifecycle_complete` in
`finalize_report_with_binding`, src/convert/pipeline/stages.rs) can never
pass `lifecycle_complete`. Suspected area: the post-action GATING that
decides when the elected runner may execute —
`batch_post_action_completion_gate` / `copy_album_batch_companion_artifacts_once_ready_*`
(PostActionGate::Ready vs NotReady) interacting with:

- the flat flow's companion finalization (per-item vs batch-coordinated
  readiness; case B logs show per-item companion passes),
- the coordination-io descriptor route vs stable path in whatever predicate
  feeds the gate,
- possibly the same class as previous fixes: a predicate silently failing
  closed on a `/proc/self/fd/N/...` path.

Deliverable: post phase executes exactly once per album in case B (after
companions), the workspace retires to zero residue, and case A stays green.
Add a regression test pinning the flat coordinated flow (the existing suite
covers case A shapes and unit-level gates, 3087 tests green — none caught
this).

## Diagnostic hooks that worked well this round

- `RUST_LOG=warn` on the real CLI: every preserve/defer path in cleanup logs
  its reason; absence of logs means the call was never reached.
- The gate booleans in `finalize_report_with_binding` are the ground truth
  for terminal cleanup (temporary eprintln there located two earlier bugs).
- Acceptance harness: `XDG_CONFIG_HOME=<scratch>/config-home` with a COMPLETE
  config.toml (sparse configs fail parse and are silently replaced by
  defaults — `TonepoetConfig::load().unwrap_or_default()` in main.rs).

## Files in this bundle (the strict slice)

- this brief
- `src/convert/pipeline/stages.rs` — gate, election prep, companion
  readiness, cleanup, finalize_report_with_binding
- `src/convert/pipeline/actions.rs` — election, journals, drains, cleanup
  veto scan, ActionEngine
- `src/convert/pipeline/types.rs`, `src/convert/pipeline/errors.rs` — shapes
- `src/convert/cap_fs.rs` — descriptor-route primitives (context)
- `src/convert/processor.rs` — batch preparation and dispatch (context)

Constraints: suite baseline 3087/0, zero cold-build warnings; stdin sentinel
applies to any subprocess; never persist `/proc/self/fd` routes durably;
descriptor-route predicates must resolve to stable paths before comparing;
the sandbox cannot compile — favor mechanically verifiable changes.
