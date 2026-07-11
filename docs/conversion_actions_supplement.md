# Supplement: filesystem prior art for the conversion-actions bundle

Date: 2026-07-10. Companion to `docs/conversion_actions_brief_v2.md` — answers
the implementer's question "is there a shared filesystem abstraction I should
integrate with instead of inventing a parallel one?"

## Direct answer

There is NO existing descriptor/capability-based filesystem abstraction in
the workspace — zero `openat`/`dirfd`/`O_NOFOLLOW`/`cap-std` usage anywhere.
You would be introducing the first one. That is acceptable, but it makes the
dependency choice an explicit decision, not a discovery: `cap-std` vs.
`rustix`/`libc` openat-family. Note macOS is a supported target — Linux-only
`openat2(RESOLVE_BENEATH)` cannot be the sole mechanism. Current related
deps: `libc`, `fs2` (the album publish lock's flock), `tempfile`.

However, there IS scattered filesystem prior art you have not seen, and two
pieces are directly in the rename action's blast radius:

## Files added in this supplement (not in the original 27-file bundle)

- `src/tui/rename_plan.rs` (505 lines) — **an existing, live rename engine**
  used by the Browse screen (wired into app.rs, keybindings.rs, event_loop.rs,
  draw_overlays.rs): `RenamePlan`/`RenameOp`/`OpStatus`, `sanitize_path`
  (:59), `validate_plan` with conflict counting (:129), `execute_plan` (:209)
  with target-directory pre-creation and a journal-based rollback. The
  actions `rename` MUST either build on this or consciously diverge with the
  difference documented — do not create a second parallel rename engine
  blindly.
- `src/tui/rename_template.rs` (145 lines) — `resolve_template` (:28), a
  THIRD template-resolution surface besides the pipeline renderer
  (`render_template_with_tokens`) and `convert/renaming.rs`. SR-7/SR-8 parity
  means consolidating toward the pipeline renderer, not adding a fourth.
- `src/db.rs` (3,317 lines) — `atomic_metadata_write` (:2195), the metadata
  write journal (`begin_metadata_write`/`complete_metadata_write`), backup
  path conventions. Crash-safety prior art for any write-verify-commit design.
- `src/tui/convert_actions.rs` (1,106 lines) — Convert-screen action
  handlers; holds the production default template literals (e.g. the default
  filename template passed to `effective_naming_template` at :567). Included
  because its name collides with this feature and because default-template
  provenance matters for preview.
- `src/convert/pipeline/materializer_archive.rs` — included for
  `reject_external_symlink_target` (:1557) and its tests: the existing
  symlink-escape defense pattern (staging-root confinement). Your capability
  layer should subsume or match this behavior.
- `src/convert/pipeline/bluray_realize.rs` — `atomic_replace_file` (:761)
  with platform-specific impls; `src/convert/cue_parser.rs` is already in the
  base bundle and has its own `atomic_replace` (:1685). Atomic-replace is
  currently duplicated per-site; a shared layer may rationalize it, but do
  not change those call sites' behavior in this feature.

## The file-task engine (in a file you ALREADY have — do not miss this)

`src/tui/keybindings.rs` contains a complete copy/move/delete execution
engine for the Browse screen's file operations ("Move to…", copy, delete):

- `FileTaskJob` (:19665), `FileTaskPathStats` (:19682), `FileTaskPlanKind`
  (:19701), `FileTaskPlanNode` (:19708), `FileTaskPlan` (:19717),
  `FileTaskPlanBuildStep` (:19723) — a measure→plan→execute pipeline with
  per-node progress.
- `move_via_copy_verify_remove_node` (:20524) — the EXDEV fallback the
  brief's `move` action demands ALREADY EXISTS: rename first, then
  copy-verify-remove on cross-device/unsupported/already-exists, routed
  through an explicit conflict/finalization engine.
- `is_cross_device_error` (:21155), `copy_path_progress*` (:20594+).
- Progress/conflict vocabulary comes from the tui-file-picker crate:
  `FileTaskPhase`, `FileTaskProgressUpdate`, `ConflictPolicyPreset`,
  `ConflictResolution`, `CrossDeviceCutPolicy`, `DeletePolicy` — see below.

The actions `copy`/`move`/`delete` should reuse these primitives (the plan
node model, copy-verify-remove, cross-device detection) or consciously
diverge with the difference documented. Note the impedance difference:
the file-task engine is interactive (conflict prompts, progress UI); action
pipelines are headless and must resolve conflicts by policy, never by
prompting. The reusable layer is the primitives, not the interaction loop.

## Files added: the tui-file-picker policy/progress vocabulary

`crates/tui-file-picker/src/{lib.rs, state.rs, progress.rs}` are included
because the file-task engine's types live there: `CrossDeviceCutPolicy`
(state.rs:243, default `Reject`; `CopyThenDelete` arm at :1838 with the
EXDEV constant at :2193), `DeleteMode`/`DeletePolicy`,
`FileOperationPolicy`, `ConflictPolicyPreset`, and the progress model
(progress.rs). If the actions engine adopts these policy types, conversions
and Browse file ops share one vocabulary — desirable, but the picker crate
is a standalone widget crate: do not make it depend on conversion types
(dependency direction: main crate → picker, never the reverse).

## Pointers into files you ALREADY have (easy to miss in large files)

- Permanent-deletion guards: `delete_path_permanently`
  (src/tui/keybindings.rs:23733 — rejects unstable dot components, idempotent
  on missing) and `permanently_delete_paths` (:23759 — deduplicates, deletes
  children before parents: the SR-5 ordering rule, already implemented).
  The `delete` action rides these.
- `path_is_under_root` (src/convert/pipeline/stages.rs:23497) — the
  containment predicate the publish path uses; SR-2's guard should use the
  same predicate, not a reimplementation.
- Symlink policy precedent: directory scans do not follow symlinks (Browse
  policy); archive staging rejects symlink escapes (above).

## Constraint reminder

All of the base brief's constraints apply unchanged — in particular SR-1..8,
the stdin sentinel for any subprocess, and "pipeline behavior without
configured actions is byte-identical". A new dependency (if you choose
cap-std or rustix) is a workspace `Cargo.toml` change — declare it in the
implementation notes with the version pinned and the reason stated.
