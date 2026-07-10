# Disc-number template variables: implementation notes

Date: 2026-07-09

This bundle implements user-authored disc naming without preserving source
folder style in output paths.

## Semantic decisions

- `%DISC_FOLDER%` is the convenience token. It renders the standardized
  lowercase component `disc NN` for proven multi-disc layouts and renders empty
  otherwise.
- `%DISCNUMBER%`, `%NNDISCNUMBER%`, `%NNNDISCNUMBER%`, and `%DISCTOTAL%` are
  evidence-gated data tokens. They render only when the source is a proven
  multi-disc set, using the same evidence bar as `%DISC_FOLDER%`. `%DISCTOTAL%`
  is album-level data and also renders in folder-template contexts that do not
  have a current track, as long as the source is a proven multi-disc set.
- `create_disc_subfolders` does not gate token values. It remains a template
  projection at the request boundary: when enabled and no explicit
  `%DISC_FOLDER%` is present, `%DISC_FOLDER%/` is prepended to the filename
  template once. User-authored disc-number path components such as
  `disc %NNDISCNUMBER%/%TRACKNN% - %TITLE%` also suppress the convenience
  prefix, so explicit disc routing is not double-wrapped. `%DISCTOTAL%` alone
  is album-level data, not a per-disc route, so `%DISCTOTAL% discs/%TRACKNN% -
  %TITLE%` still receives the convenience prefix. A disc number used only in the
  leaf filename is not treated as routing and still receives the convenience
  prefix when the switch is on.
- `%DISC%` is intentionally left as the legacy raw/back-compatible token. It
  still renders the best available disc hint and defaults to `1` for single-disc
  sources. The new disc-number family is the gated alternative for conditional
  template blocks.
- Resolved conditional blocks containing disc-routing tokens are unwrapped, not
  emitted with literal braces. Thus `{disc %NNDISCNUMBER%/}%TRACKNN% - %TITLE%`
  renders as `disc 01/01 - Title` for proven multi-disc sources and as
  `01 - Title` for single-disc sources. Existing non-disc metadata conditionals
  such as `{%TITLE_EXTRA%}` keep the historical literal-brace behavior when
  they resolve.
- User-authored disc tokens are accepted in folder templates as well as filename
  templates. A multi-track planned item may intentionally render multiple album
  roots, for example `Album (CD1)` and `Album (CD2)`. The planner records the
  complete sorted set of rendered album roots. Publish now treats those roots as
  one umbrella transaction: it prepares and validates every root first, writes a
  durable multi-root recovery marker under the output root, commits roots under a
  single multi-root lock, and rolls back already exposed roots if any later root
  fails. On process crash or cancellation between roots, the next publish scans
  and repairs stale multi-root markers before destination preflight, so a retry
  does not fail on its own partial `Album (CD1)` output. It never falls back to
  publishing against `output_root`.

## Batch completion coordination

Album-batch conversion-log fragments no longer use the rendered album root as their coordination namespace. This matters for folder templates such as `%ALBUM% (CD%DISCNUMBER%)`, where independent track jobs legitimately render different album roots like `Album (CD1)` and `Album (CD2)`. Hidden fragment records now land under the stable batch workspace `<output_root>/.tonepoet-batch/<conversion_log_batch_id>/`, and a durable `complete` marker is written there once the dispatcher-declared fragment set is complete. Companion finalization consults that stable completion marker, not a per-disc rendered album directory or a one-shot `PublishedAlbum`. The finalization lock and in-process completed key are also batch-scoped rather than rendered-root-scoped, so concurrent per-disc publishers do not run duplicate scans merely because they published `Album (CD1)` and `Album (CD2)` through different roots. The rendered album roots remain only audio/companion destinations.

## Companion routing

Nested companion files under source disc directories route to the directory that
published audio for that disc actually uses. Non-batch routing is keyed by
`TrackId`/planned artifact identity, not by zipping source tracks to published
entries in vector order. Partial publishes and reordered entries therefore do
not silently misroute companions.

Resolved album-batch companion coordination records each track's rendered audio
parent as its own durable per-dispatch, per-track record under the output root
before the batch finalization signal. This avoids the lost-update race of a
shared read/merge/write manifest: independent track completions never overwrite
each other's routing facts. Destination-record installation is itself
concurrency-safe: temp records are named with the writer PID and a monotonic
process-local sequence, created with `create_new(true)`, fsynced, and then
hard-linked into the immutable final record path. Temp cleanup only removes
record temps whose owner process is known dead, so one concurrent publisher
cannot delete another publisher's active temp between write and hard-link. The
final scan aggregates those immutable records after crashes/restarts and refuses
to run until at least the dispatcher-declared expected track count is
represented. If the batch completion signal arrives before every destination
record has been published, the code writes a durable pending-finalization marker
under the record directory. Every later successful destination-record writer
checks that marker, and also checks durable completion state such as an already
assembled `conversion.log`, so companion finalization does not depend on a
one-shot last-track `PublishedAlbum` object. If records are missing, unreadable,
or conflicting, companion finalization remains incomplete and emits an explicit
warning; it does not scan with recomputed `disc NN` destinations and does not
mark the batch finalized. Successful finalization removes the owned record state
and pending marker, again preserving live record temps if another writer is
still in flight.

Regular companion-file installation is now temp-first and atomic-or-skip. The
writer copies bytes only into a hidden same-directory `.tonepoet-copy.*.tmp`
file, preserves metadata on that temp, then installs it with hard-link
no-clobber semantics. If that atomic no-replace install is unavailable, the
file is skipped with an explicit warning; the code does not fall back to a
direct create-new copy into the final path because a crash in that window would
leave a persistent corrupt partial. Retries also sweep abandoned matching
`.tonepoet-copy` temps owned by dead processes before trying again, while
leaving current-process temps alone.

## Tests added or updated

- Disc-number variables are gated, padded, and conditional-block-friendly;
  `%DISCTOTAL%` renders in album/folder templates without a current track for
  proven multi-disc sources; multi-disc conditionals unwrap to real path
  segments instead of brace-polluted names.
- Single-disc sources carrying `DISCNUMBER=1` do not create `disc 01` through the
  new gated variables.
- `%DISC%` legacy behavior is covered explicitly.
- `%DISC_FOLDER%` standardizes source `CD N`/`Disc N` layouts to lowercase
  `disc NN`.
- Folder-template disc tokens work for single-track and ordinary multi-track
  planned items; multi-root publishes create each rendered album root without
  introducing stray `disc NN` folders. Transaction tests cover failure after the
  first root, retry after an interrupted partial root, marker repair before
  preflight, and cleanup of hidden group staging directories.
- `create_disc_subfolders` does not prepend `%DISC_FOLDER%/` to an explicit
  user-authored disc-number route.
- Nested companion copy and resolved album-batch companion copy follow published
  audio directories and do not invent stray `disc NN` folders when the rendered
  layout uses per-disc album directories such as `Album (CD1)` and `Album (CD2)`.
  Independent single-file album-batch publishes with `%ALBUM% (CD%DISCNUMBER%)`
  now prove the real fragment completion path uses the stable batch workspace and
  triggers companion finalization without a manually synthesized completion
  `PublishedAlbum`.
- Batch companion routing is crash/retry-durable through immutable per-track
  destination records. Record temp files are unique, no-clobber, PID-owned, and
  swept only when their owner is known dead. Finalization defers until all
  expected records are present, leaves a durable pending-finalization marker,
  and is retried by subsequent record writers after the batch completion signal
  has already been observed; missing routing state is a blocking
  incomplete-finalization condition, not a recomputed fallback.
- Non-batch companion routing is keyed by track/artifact identity rather than
  source/published vector order.
- Companion file hard-link fallback refuses unsafe final-path copying and leaves
  no partial destination behind. Filesystems that cannot hard-link within the
  destination directory intentionally run in a degraded mode for companion files:
  they warn and skip rather than sacrificing crash safety.
- Abandoned `.tonepoet-copy` temp files for dead owners are swept on retry.

## v11 cleanup and visible-log correction

The stable `<output_root>/.tonepoet-batch/<conversion_log_batch_id>/` workspace is now strictly transient coordination state. It stores hidden fragments and the durable completion marker only long enough for batch completion and companion finalization. Successful finalization removes the owned per-dispatch workspace, its marker, any stale hidden `conversion.log` from older v10 runs, empty fragment directories, quarantine directories, and the empty `.tonepoet-batch` parent when possible. Runs with no configured companions clean the coordination workspace as soon as batch completion is observed, so reruns do not accumulate per-dispatch hidden state.

The final user-visible `conversion.log` is no longer assembled under the hidden coordination workspace. Each fragment records the rendered album root for its track. When the dispatcher-declared fragment set is complete, the assembler writes the same authoritative log to the deterministic set of rendered album roots represented by the batch, for example both `Album (CD1)/conversion.log` and `Album (CD2)/conversion.log` for `%ALBUM% (CD%DISCNUMBER%)`. If no visible rendered album root can be recovered, finalization fails explicitly rather than leaving the only log under `.tonepoet-batch`.

## v12 deterministic album-level companion folders and failure-fragment routing

Album-level companion folders are now routed by an explicit deterministic policy
for multi-root album-batch layouts. Each durable per-track destination record
also carries the rendered album root for that track. Batch companion
finalization aggregates the complete set of rendered album roots, sorts and
deduplicates it, and copies configured whole-folder companions such as `Scans/`
to every rendered album root represented by the batch. Thus a template such as
`%ALBUM% (CD%DISCNUMBER%)` produces `Album (CD1)/Scans/` and
`Album (CD2)/Scans/` deterministically, rather than copying to whichever worker
happened to observe completion. Root-level loose companions and album-level
`extra`/`extras` nested loose companions use the same rendered-root destination
set. Disc-scoped nested loose companions still route by the durable disc audio
parent map and still refuse to invent a fallback `disc NN` folder.

The pre-materialization failure path now requires the same stable
`<output_root>/.tonepoet-batch/<batch-id>/` coordination directory as successful
fragment staging. It no longer falls back to `album_batch.album_output_dir` for
failure/cancellation fragments, and incomplete-batch finalization no longer
scans provisional rendered roots. This keeps successful, failed, and cancelled
independent-file album-batch jobs in one batch namespace, including the
load-bearing per-disc album-folder template case.

## v13 hardening: crash-recoverable batch coordination and visible log commits

- `.tonepoet-batch/<batch-id>` remains a transient coordination workspace.  Successful runs still remove the current workspace, and later batch publishes/finalizers now also sweep stale generated workspaces whose batch-id owner PID is no longer live.  Completed workspaces get success-style cleanup; incomplete dead-owner workspaces are removed as transient retry debris.  The sweep only targets implementation-owned generated ids and skips the current/live batch, so active coordination state is not deleted.
- Visible multi-root `conversion.log` publication is no longer a sequential best-effort write.  The final assembled log is staged into every rendered album root first, guarded by a durable visible-log commit marker in the batch workspace, then installed as a small transaction.  If any root install fails, already-installed logs are rolled back and backups are restored.  A later run repairs any interrupted visible-log marker before writing or cleaning the stale coordination workspace.
- This keeps the hidden batch workspace out of byte-identical successful reruns while preserving explicit recovery semantics for crashes between batch completion, companion finalization, visible log publication, and cleanup.

## v14 hardening: roll-forward visible-log recovery

The visible multi-root `conversion.log` transaction marker now records the
intended log length and SHA-256 digest in addition to every final path, temp
path, and backup path. Recovery first checks whether all final visible logs
already contain the intended bytes. If they do, the transaction is treated as
committed: leftover temps, backups, and the marker are removed. This covers the
critical crash window after every visible log has been installed but before the
marker cleanup completed.

Normal successful commits now also remove backup files before removing the
marker. If backup/temp/marker cleanup itself fails after the logs are installed,
the code does not roll back the committed logs. It leaves the marker in place so
a later retry can roll the transaction forward and finish cleanup. Only a
transaction whose final files do not all match the marker's expected bytes is
rolled back.

Regression coverage now includes crash-after-full-install recovery with no prior
logs, crash-after-full-install recovery with prior logs/backups, and successful
overwrite cleanup proving no `.tonepoet-visible-backup` files remain in rendered
album roots.

## v15 hardening: idempotent fragment repair and marker path validation

The hidden batch conversion-log fragment is now treated as part of the recoverable independent-track publish contract. There is still an unavoidable crash window after a rendered audio payload becomes visible and before its out-of-album coordination fragment is installed, but a later retry no longer wedges on the already-visible audio path. If the destination audio already exists, the publish path only enters repair mode when the staged audio artifact and existing destination are byte-for-byte identical and the plan carries an out-of-album coordination fragment. In that narrow case the retry publishes the missing fragment, runs the normal batch-completion check, and reports the existing audio as part of the successful publish. If the existing audio differs, `DestinationExists` remains a hard conflict and no hidden fragment is published.

Out-of-album coordination fragments are also idempotent: if the final fragment file already exists with identical bytes, the duplicate staged fragment is consumed and treated as published; if it exists with different bytes, publish fails rather than overwriting an immutable routing/completion fact.

Visible-log recovery now validates every final/temp/backup path recorded in a stale marker before removing, restoring, or deleting anything. Marker recovery derives the expected output root from `<output_root>/.tonepoet-batch/<batch-id>/visible-conversion-log-publish-in-progress` and rejects any marker entry outside that root. Invalid markers are left in place for explicit operator inspection instead of being trusted as deletion instructions.

## v16 hardening: multi-root fragment retention and batch-locked log finalization

Multi-root publishes now keep out-of-album batch coordination fragments outside
per-rendered-root artifact filtering. Rendered album roots are still grouped and
published transactionally, but hidden `.tonepoet-batch/<batch-id>/` conversion-log
fragment entries are collected from the original artifact set, published exactly
once after the rendered roots have been exposed, and then fed into the same
batch-completion/finalization path as ordinary independent-track publishes. This
prevents `%ALBUM% (CD%DISCNUMBER%)` single-item or CUE-style multi-root plans
from publishing all audio while silently dropping the hidden fragment that gates
visible `conversion.log` assembly and companion finalization.

Conversion-log batch finalization is now serialized by a batch-scoped lock in
the stable coordination workspace. Duplicate publishers from different rendered
album roots can no longer concurrently observe the complete fragment set and race
visible-log publication, completion marking, or fragment cleanup. A publisher
that acquires the lock after another finalizer has already marked the batch
complete treats that as idempotent success and repairs any interrupted visible-log
marker instead of reporting a misleading `DestinationExists`-style failure. The
success cleanup path also removes the transient finalization lock file if a
crashed process left one behind.

## v17 hardening: multi-root retry repair after pre-finalization crashes

Multi-root publish now has an idempotent repair path for the crash window where
all rendered album roots and hidden batch coordination fragments were exposed,
but the process died before batch finalization wrote visible `conversion.log`,
marked completion, triggered companion finalization, or cleaned the coordination
workspace.  Before destination preflight can fail on `FailIfExists`, the
multi-root path checks whether every newly staged rendered-root payload already
exists at its final path with identical bytes.  If the rendered roots match, the
retry treats those audio/sidecar payloads as already published, publishes the
current run's hidden batch fragment records idempotently, and runs the normal
batch completion/finalization path.  A true content mismatch still fails as a
hard destination conflict.

This is the chosen recovery strategy rather than widening the multi-root rollback
marker to cover batch finalization.  It keeps already-correct rendered audio
visible, repairs the missing batch coordination work on the next retry, and
prevents a fresh generated batch id from wedging behind output created by the
previous dispatch attempt.  Regression coverage injects a failure immediately
after multi-root roots and fragments are committed but before finalization, then
proves the retry completes visible logs from a fresh batch id without overwrite.

## v18 hardening: same-process stale batch cleanup and explicit workspace state

Batch coordination cleanup no longer relies on the generated batch id's owner
PID alone.  Every active `.tonepoet-batch/<batch-id>/` workspace now writes a
small durable `state` file recording the generated batch id, owning PID, and
workspace status.  The current process also keeps a process-local active-batch
registry with reference counts, so stale cleanup can distinguish another live
same-process batch from an old failed attempt that merely has the current PID in
its generated id.

The stale sweep now skips the current batch and any batch registered active in
this process.  It removes completed generated workspaces only when their durable
state or generated owner id no longer proves a live owner, and it can remove
incomplete same-process workspaces once their active registry guard has dropped.  This fixes the handled-failure retry case where the
app process stays alive: a failed old dispatch no longer leaves hidden
`.tonepoet-batch/<old-id>` debris just because `<old-id>` embeds the current
PID.  The sweep still refuses to delete non-generated directories or live
registered batches.

Regression coverage now uses a production-shaped generated batch id containing
the current process PID, injects failure after multi-root roots/fragments are
committed but before finalization, retries in the same process with a fresh batch
id, and asserts the old hidden batch workspace is removed while visible logs are
completed.

## v19 hardening: non-terminal same-process batch workspaces are live

The v18 cleanup rule was too aggressive for concurrent batches in the same long-running process. A batch can have already published one or more hidden coordination fragments while sibling tracks are still converting, yet no short publish/finalization guard is held at the instant another batch sweeps the shared output root. Treating an unregistered same-process workspace as stale could delete those fragments and permanently prevent the first batch from reaching its dispatcher-declared expected count.

Workspace cleanup is now state-driven rather than PID-only. Non-terminal same-process workspaces with `active` state are treated as live even when no short critical-section guard is currently registered. Cleanup may remove same-process workspaces only when they are explicitly terminal, such as `complete`, `failed`, or `abandoned`, or when the generated owner is no longer live. Batch finalization failures and the multi-root post-fragment failure hook mark the workspace `failed`, so handled same-process failures remain cleanable without making in-progress sibling-track batches vulnerable.

The stale sweep also checks workspace liveness before attempting visible-log marker repair, so a live same-process batch cannot have its in-progress hidden transaction mutated by a sweep from another batch. Regression coverage now includes the production-shaped gap: one same-process batch has a partial hidden fragment and active state, no process-local guard is held, another batch sweeps the same output root, and the first batch's workspace and fragment must survive.

## v20 hardening: failed publish attempts cannot leave immortal active workspaces

The v19 cleanup rule correctly preserved non-terminal same-process workspaces so
live sibling-track batches could not be deleted between short publish/finalization
critical sections.  The missing half was terminal-state discipline: a publish
attempt that wrote `state = active` and then returned through an ordinary error
path could leave an active same-process workspace behind forever, because later
sweeps would treat the current PID as live.

Publish and multi-root publish now use a failure-aware workspace guard whenever a
batch coordination fragment is part of the publish contract.  The guard writes
`active` on entry, but it is armed to mark the workspace `failed` if the publish
path exits before it reaches a successful, idempotent post-publish state.  Normal
success, idempotent existing-payload repair, and legitimate deferred finalization
disarm the guard; ordinary errors such as staging problems, destination
conflicts, coordination-fragment failures, rename/backup failures, or visible-log
finalization errors leave a terminal `failed` state.  A later same-process retry
can then safely sweep the old generated workspace instead of preserving it as a
live active batch.

Activation also refuses to downgrade terminal workspace state back to `active`.
If a sibling worker reaches a late publish hook after the batch has already been
marked `failed`, it may still run its local cleanup/publish code, but it will not
resurrect the old coordination directory into an immortal active state.
Regression coverage now proves both sides of the lifecycle rule: live active
same-process workspaces with partial fragments survive sibling sweeps, while an
ordinary same-process publish failure after activation becomes terminal and is
removed by the next retry.
