# Brief: pre/post-conversion actions — user-authored file operations around the pipeline

Date: 2026-07-10

## The feature

An ordered, user-authored pipeline of file-level actions that runs around each
album conversion and touches things the conversion itself does not generate:
companion files, sidecars, folders that need creating, scripts the user wants
run. Two phases — **pre-conversion** and **post-conversion** — each an ordered
list. A TUI wizard (mockup: `conversion_actions_wizard.html` in this bundle)
shows available actions on the left and the pre/post pipeline on the right,
with per-action config boxes that include a live dry-run preview.

Motivating example: after converting
`Deep Purple – Nobody's Perfect (Japan / SHM)`, rename the copied EAC
`*.log` / `*.cue` companions using
`%ARTIST% - %ALBUM% (%TITLE_EXTRA%) [Disc %DISCNUMBER%]` so they match the
album's naming. (The disc-number token family is `%DISCNUMBER%` /
`%NNDISCNUMBER%` / `%NNNDISCNUMBER%` — evidence-gated, empty for
single-disc albums; see stages.rs:22676.)

The user's stated bar: extensible action set, and reliable / robust /
correct / idempotent execution — some users will do everything through
scripts, and the script contract must be as trustworthy as the built-ins.

## Built-in actions (v1)

| Action | Semantics |
|---|---|
| `rename` | Four modes: `template` (render a naming template per matched file/folder), `uppercase`, `lowercase`, `fixcaps` (existing capitalization heuristics). Extension is preserved; the template/mode applies to the stem (folders: whole name). |
| `copy` | Copy matched files/folders to a destination (within or outside the album dir). Preserve mtime; follow the companion-copy fidelity conventions. |
| `move` | Copy semantics + source removal. MUST handle `EXDEV` (copy+fsync+verify+delete fallback) — the user's library spans `fuse.sshfs` mounts and cross-mount `rename(2)` fails there. |
| `delete` | Remove matched files/folders. Rides the existing permanent-deletion guards (reject root paths, dot components, empty paths) plus the new source-protection rule (SR-3). |
| `create_folder` | `mkdir -p` semantics (trivially idempotent). Name may use template variables. Relative paths resolve against the phase's subject directory — album dir (post) / source dir (pre), the same rule as runscript's working directory; absolute paths allowed. |
| `runscript` | Execute a user script with the environment contract below. |

Modifier: every targeting action carries include globs AND exclude globs
(file-name level, `*`/`?`, case-insensitive) — reuse
`companion_wildcard_matches` (stages.rs:18036), which is differential-tested
against a reference implementation (10,571 cases). Do NOT write a new matcher.

`uppercase`/`lowercase`/`fixcaps` are rename MODES, not separate actions —
one action shares the target/preview/collision/idempotency machinery
(the mockup already draws it this way).

## Data model & persistence

- Serde-tagged enum, e.g.:

  ```toml
  [[actions.post]]
  type = "rename"
  target = ["*.log", "*.cue"]
  exclude = []
  mode = "template"
  template = "%ARTIST% - %ALBUM% (%TITLE_EXTRA%) [Disc %DISCNUMBER%]"
  ```

- New action = new enum variant implementing a common
  `plan(&ActionContext) -> ActionPlan` / `apply(ActionPlan)` interface.
  Deliberately NOT a plugin registry — `runscript` is the extension point.
- Unknown `type` values must fail deserialization LOUDLY (surfaced error),
  never silently drop.
- Persistence surfaces: preset TOML (presets carry opaque strings today —
  confirm round-trip), config default pipeline, and the queue item's
  `PipelineRequest` (types.rs:436) so a restored queue
  (`~/.cache/tonepoet/conversion_queue.json`) behaves identically.
  Backward compatibility is mandatory: the actions field must be
  `#[serde(default)]` on BOTH `TuiPreset` (presets.rs — the existing
  per-field-default pattern, no deny_unknown_fields) and `PipelineRequest`
  (types.rs already uses `#[serde(default)]` throughout), so preset and
  queue files written by older builds load unchanged.
- No CLI flag surface in v1 (mirrors the companion-exclude precedent:
  TUI + presets only).

## Execution model (the load-bearing design)

**Plan → preview → apply.** Every built-in computes a concrete operation list
against current state before touching anything. Idempotency falls out:
re-running plans zero ops when the target state already holds (template
rename with unchanged variables, case modes on already-cased names, existing
folder, absent delete target). The wizard's dry-run pane renders `plan()`
output directly.

**Election, then execution — in BOTH phases.** Album batches run N items;
actions are album-scoped and must run exactly once per album per run:

- POST: the batch finalizer already solves this election for companion copy —
  but companion finalization executes UNDER the album publish lock
  (`with_album_publish_lock_for_companion_copy`, stages.rs:16844, called at
  16769). Actions must NOT execute under that lock: user scripts of arbitrary
  duration inside the most delicate critical section is a design bug. The
  lock protects the ELECTION only: atomically claim an "actions runner"
  marker in the batch coordination workspace (same shape as the companion
  attempts records), release the lock, then run the pipeline.
- PRE: inverse election — the first item of a batch to start claims and runs
  the pre pipeline under the batch lock's election (execution outside it);
  sibling items wait for pre completion before materializing. Without this, a
  2×12-track album runs the pre pipeline 24 times.
- Single-item (non-batch) sources run both phases inline, same code path,
  degenerate election.

**Success gate.** Post actions share the EXACT gate companion copy uses:
`is_fully_successful` (stages.rs:16571, applied at 16815). Partially-failed
albums get no post actions — scripts must never see a half-baked directory.
State the skip in the item status.

**Rerun-skip policy.** The pipeline has a manifest-match skip path
(`RerunDecision`/`RerunReason`, rerun.rs:15/34). Policy: actions run only
when the album actually PUBLISHED this run; an all-skipped album does not
re-trigger them. To apply a pipeline to an already-converted album, add an
explicit `:actions-run [dir]` command (vi command mode, command.rs; the name
is free — no existing `:actions*` command) that runs the POST pipeline
against an existing album directory on demand — this also serves as the safe
way to test a pipeline against real files, and as the
retroactive-library-application story. Guard applicability for
`:actions-run`: SR-1 (hidden-entry protection) and SR-5/SR-6 apply in full;
SR-2/SR-3 have no batch context — the user's explicit directory choice IS
the scope consent, but the command still refuses filesystem roots and
requires the target to be a directory.

**Failure semantics.** Pre-action failure fails the item BEFORE conversion
starts (nothing irreversible has happened; the precondition was not met).
Post-action failure never rolls back published audio — the item completes
"with action errors", visibly reported. Per-action `continue_on_error` flag
(default false: a failed action stops the remainder of its phase's pipeline).

**Cancellation.** Cancel mid-run: post actions do not run; pre actions may
already have run and are not rolled back (scripts cannot be). Document.

**At-most-once, stated precisely:** once per successful conversion of an
album PER RUN. Deliberately re-converting re-runs actions. Scripts must
tolerate re-runs; we do not promise once-ever.

## The runscript contract

- Direct exec of the script file — NO shell interpolation, ever. Args are
  literal strings.
- Rendered template variables reach scripts as environment variables ONLY:
  `TONEPOET_PHASE`, `TONEPOET_ALBUM_DIR`, `TONEPOET_SOURCE_PATH`,
  `TONEPOET_OUTPUT_ROOT`, `TONEPOET_ARTIST`, `TONEPOET_ALBUM`,
  `TONEPOET_TITLE_EXTRA`, `TONEPOET_YEAR`, `TONEPOET_FORMAT`,
  `TONEPOET_DISC_COUNT`, … (album-scoped token set). NEVER substitute
  `%VARIABLES%` into command lines — metadata contains quotes/apostrophes
  (half the library: *Nobody's Perfect*) and argv templating is an injection
  factory.
- Env hygiene: strip control characters and newlines from values before
  export — tags are semi-trusted input from downloaded rips.
- stdin MUST be nulled — `tests/subprocess_stdin_convention.rs` scans all
  workspace sources and will fail any `.spawn()`/`.status()` without
  `.stdin(` configured. Working directory = album dir (post) / source dir
  (pre); for FILE sources (archive/ISO) the pre working directory is the
  source's parent — scripts get `TONEPOET_SOURCE_PATH` and must not assume
  the CWD is exclusive to this album. Mandatory timeout (configurable, sane
  default ~10 min). stdout/
  stderr tails captured and surfaced in the item report. Non-zero exit =
  action failure.
- Script path resolution: absolute and `~` accepted; bare relative resolves
  against `~/.config/tonepoet/scripts/`, never the process CWD. Script must
  exist and be executable — refuse loudly otherwise (no auto-chmod).
- Execution goes through a runner trait seam (mirror `ToolRunner`/
  `StubToolRunner`, tool.rs:155/185) so tests stub it. For real-execution
  tests, reuse the hardened `write_executable_script` fixture pattern
  (tool.rs:1344 — it already solves ETXTBSY and self-check injection).

## Safety perimeter (named requirements, each needs its own test)

- **SR-1 Pipeline internals are untouchable.** Wildcard targeting NEVER
  matches dot-prefixed (hidden) entries, and recursion never descends into
  dot-prefixed directories — the shell-glob convention. This is the complete
  rule, verified against the internal-artifact namespace: every internal
  that can appear in an action's subject directory (album dir / output
  root) is dot-prefixed, but NOT all are `.tonepoet-*` — the family also
  includes `.conversion-log-finalization.lock` (stages.rs:74),
  `.<job>-<item>.run.lock` run locks, `.<album>.tmp-*` publish temp dirs
  (stages.rs:24443), and `.tmp.*` staging files, so a prefix blocklist
  would leak. (Non-hidden internals exist but only inside materialization
  staging — e.g. `*.wav.lock` in the decoded-image cache, stages.rs:1115 —
  which is never an action subject.) Additionally protected regardless of hidden
  status: the pipeline-generated `conversion.log` and generated CUE sheets —
  excluded from wildcard matches unless explicitly named (an exact,
  wildcard-free target may touch anything; explicit naming is the escape
  hatch). The motivating `*.log` example must NOT rename `conversion.log`.
- **SR-2 Album-scope guard.** Album-scoped actions require the rendered album
  directory to be a proper subdirectory of the output root, created for this
  album. Flat layouts (tracks directly in output root) → actions are skipped
  with a visible notice. Never glob a directory shared across albums.
- **SR-3 Source protection.** `delete`, `move`, and `rename` refuse any path
  registered as a conversion input of this batch (the context carries the
  exact source path set) unless the action sets an explicit
  `allow_sources = true`. Rename is included because a renamed source breaks
  re-conversion of a persisted queue (recorded source paths go stale).
- **SR-4 Pre-phase restrictions.** Pre-phase built-ins that mutate files are
  v1-limited to `runscript` and `create_folder`. Destructive built-ins
  (`rename`, case modes, `delete`, `move`) are post-only in v1 — a glob
  accident must not be able to rename sshfs masters. (A script author opting
  to mutate sources has signed up for it.) Pre built-ins apply only when the
  source is a directory; archive/ISO sources get `runscript` only, with a
  visible notice.
- **SR-5 Plan discipline.** Collision detection runs against the planned
  END-STATE (two matched files rendering to the same name → refuse with a
  message, never last-write-wins; a planned rename landing on a name another
  planned op creates → same). Folder renames apply depth-first
  (children-before-parents) or re-resolve paths after directory ops.
- **SR-6 Preview never executes.** Dry-run/preview never runs scripts —
  script entries preview as "would run: <script>". Two preview levels,
  don't conflate them: (a) template-STRING preview against canonical
  example data — this pattern exists (`render_template_preview`,
  src/tui/template_builder.rs:568, hardcoded Pink Floyd 35DP-4 example;
  note it is NOT bound to the selected album and supports some tokens the
  pipeline renderer does not — pin action tokens to the PIPELINE's set,
  not that preview's). (b) The config-box dry-run against real file names —
  this is NEW work: simulate the destination as planned audio outputs +
  planned companion copies for the currently-selected source, labeled as
  simulated. `:actions-run` previews against the real directory listing
  before applying.
- **SR-7 Sanitizer parity.** Template-mode rename uses the SAME component
  sanitizer the naming pipeline uses. The album-scoped folder path feeds
  values through `sanitize_component` (stages.rs:23451, applied inside
  `render_folder_template_with_track`); reusing that builder (SR-8) gets
  parity for free. `sanitize_segment_component` (stages.rs:894) exists
  separately — confirm rendered OUTPUT segments also pass whichever
  segment-level sanitization the naming path applies; do not fork a third.
- **SR-8 Identity parity.** Action template rendering consumes the SAME
  resolved batch identity the naming templates used (`BatchResolvedAlbumIdentity`
  types.rs:77, `AlbumBatchContext` types.rs:120) — never re-derive metadata independently
  (the v20 lesson: parallel derivations diverge). The album-scoped token map
  ALREADY EXISTS: `render_folder_template_with_track` (stages.rs:22582)
  builds it (ARTIST/ALBUM/TITLE_EXTRA/YEAR/…, disc tokens via
  `insert_disc_template_tokens` with an optional track, values through
  `sanitize_component`). Extract that construction for reuse — do not build
  a parallel one. What actions add on top: per-FILE disc inference for
  `%DISCNUMBER%`-family tokens (a matched file's disc comes from its disc
  subfolder / filename hints — the same evidence machinery `%DISC_FOLDER%`
  uses: `disc_number_from_template_component_name` stages.rs:22993 for
  component names, `track_disc_number_hint` stages.rs:22794 for the track
  side; do not invent new detection). The track-scoped map for comparison
  lives in `render_track_template` (stages.rs:22452).

## Reuse map (do not reinvent these)

- `src/convert/renaming.rs` (971 lines, currently DORMANT — CLAUDE.md calls
  it "pending preset system"; this feature is that system). `capitalize_title`
  (:414) and `capitalize_section` (:473) are the fixcaps core, already
  string-generic in the convert layer. The rename action should absorb/consume
  this module.
- `render_template_with_tokens` (stages.rs:23047) — token-map-driven,
  conditional `{...}` blocks included; feed it the album-scoped map.
- `companion_wildcard_matches` (stages.rs:18036) — the glob matcher.
- Batch election machinery: `finalize_conversion_log_batch_coordination_if_complete`
  (stages.rs:7907), companion finalizer + attempts records
  (`copy_companion_artifacts_after_publish_best_effort`, stages.rs:16675).
- TUI: `ActiveOverlay` (app.rs:4165), overlay rendering in draw_overlays.rs,
  key/mouse dispatch in keybindings.rs, `ButtonRenderMap`, vi command mode in
  command.rs, Tokyo Night theme constants. The wizard is a new overlay pair
  (pipeline list + per-action config) per the mockup.

## Hard constraints

- Conversion pipeline behavior WITHOUT configured actions is byte-identical
  to today — this feature is additive. Companion copy / Output Options stay
  untouched (deliberate decision: do not migrate copy-files/copy-folders into
  actions in v1).
- Determinism: same tree + same pipeline → same result; re-run plans 0 ops
  for built-ins.
- Network filesystems are the design target (sshfs), not an afterthought —
  but no "if sshfs then X" special-casing.
- The stdin sentinel (`tests/subprocess_stdin_convention.rs`) scans all
  workspace sources; any new subprocess launch must configure stdin or the
  suite fails.
- Suite baseline: 2855 passed / 0 failed workspace-wide via plain
  `cargo test`, zero cold-build warnings (cold = touch lib.rs first; cargo
  suppresses warnings for cached crates). tui-file-picker runs separately
  (`cargo test -p tui-file-picker`, 70 passing).
- The sandbox cannot compile; the applier fixes compile errors and runs the
  real-tree acceptance. Favor mechanically verifiable changes; state intended
  behavior per semantic decision in tests. Process-global test state (env
  vars, hooks) must be scoped + serialized (the two-layer pattern used by the
  marks/bundle-hook fixes) — parallel test corruption has bitten repeatedly.

## Acceptance (real trees, run by the applier)

1. Motivating case: convert a multi-disc album with post
   `rename *.log *.cue` template
   `%ARTIST% - %ALBUM% (%TITLE_EXTRA%) [Disc %DISCNUMBER%]` → copied EAC
   logs/cues renamed per disc, `conversion.log` NOT touched and all hidden
   entries (batch workspace, markers) untouched (SR-1), rename runs once per
   album (not per track/disc), correct disc numbers per file.
2. Re-run the same conversion (manifest skip) → actions do not re-fire;
   `:actions-run` against the existing album dir plans 0 ops (idempotent).
3. Post `runscript` on a 2-disc batch → script executes exactly once, after
   companions, with correct `TONEPOET_*` env (spot-check TITLE_EXTRA with an
   apostrophe-containing album), stdout captured, album publish lock NOT held
   during execution (verify by lock probe from within the script).
4. Partial failure (force one track to fail) → no post actions, status says so.
5. Flat template (no album folder) + configured actions → skipped with
   notice, output root untouched (SR-2).
6. Pre `runscript` on a batch → runs once before any item materializes.
7. Delete action with `*` in an album dir that contains a batch source file →
   refused for that path (SR-3), rest of plan proceeds.
8. Determinism ×2 on (1) and (3).

## Files in this bundle

- `docs/conversion_actions_brief.md` — this brief
- `conversion_actions_wizard.html` — the user's TUI mockup (wizard + config box)
- `docs/disc_number_template_variables_brief.md`, `docs/disc_number_template_variables_implementation_notes.md` — prior art for template semantics + batch identity (context)
- Pipeline: `src/convert/pipeline/{stages.rs, types.rs, tool.rs, rerun.rs, reporter.rs, errors.rs}`
- Conversion domain: `src/convert/{formats.rs, processor.rs, queue.rs, renaming.rs, mod.rs}`
- TUI: `src/tui/{app.rs, keybindings.rs, draw_overlays.rs, command.rs, message.rs, event_loop.rs, button_map.rs, theme.rs}`
- Config/presets: `src/config.rs`, `src/tui/presets.rs`
- CLI: `src/main.rs`
- `tests/subprocess_stdin_convention.rs` — the stdin convention your subprocess code must satisfy
