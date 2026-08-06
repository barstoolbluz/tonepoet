# tonepoet — Robust copy/move brief (2026-08-05)

You are starting **fresh** with no prior context. Everything you need is in this bundle. This
brief describes **outcomes and guardrails**; included diagnosis is *evidence for your use*, not
prescription — you choose HOW, so long as outcomes are met and guardrails hold.

**Project:** tonepoet, Rust CLI + TUI audio toolkit (ratatui 0.26 / crossterm 0.27, tokio,
edition 2021). Version 0.4.6 — **do not bump**. Workspace gate `cargo test --workspace
--no-fail-fast` is green (5524/0) and must stay green.

## The problem

Copying files in the Browse TUI over a slow sshfs mount (~3 MB/s, flaky VPN): when
connectivity hiccups, **the copy hangs and the user cannot cancel it**. We need the copy/move
facility hardened so that:
- cancel ALWAYS works, promptly, no matter what the filesystem is doing;
- cancelling or failing mid-operation reverts local state to known-good;
- interrupted operations can be transparently reconciled/resumed when connectivity returns
  (equally applicable to removable drives that get unplugged and reinserted).

This is a significant engineering task and we expect a carefully designed solution, not a
patch. A related feature — a copy/move **queue** — is explicitly OUT of scope, but see
Outcome O5: the engine you build should be the natural substrate for one.

## The existing facility (mapped; verify at will, don't re-derive)

The current implementation is already fairly capable — harden it, or restructure it while
preserving its behaviors. All in `src/tui/keybindings.rs` unless noted:

**Dispatch chain:** CTRL+V / CTRL+P → `handle_browse_filesystem_clipboard_key` (~5913) →
(NB: **SHIFT+CTRL+V is already taken** — it pastes from the HOST clipboard, a recently
shipped feature; do not repurpose or disturb that chord) →
`ContextAction::PasteSelection`/`TreePaste` (context_menu.rs) →
`start_filesystem_clipboard_paste` (~35724) → `start_file_op` (~35656) →
`spawn_file_task_worker` (~39070): `std::thread::spawn(|| FileTaskWorker::new(job, tx,
controls).run())`. Progress/completion flow back over mpsc as
`AppMessage::FileTaskProgress { session_id, update }` / `FileTaskComplete { session_id,
report, retry_plan }` (message.rs ~636), rendered by the `ActiveOverlay::FileTaskProgress`
overlay (draw_overlays.rs ~454).

**Copy I/O:** `copy_regular_file_progress_resolved` (~38590): open source → temp output file
at destination (`create_temp_output_file`, ~39488) → 1 MB read/write loop → flush → optional
`sync_all` → atomic `finalize_temp_file` rename publish (~39724). Optional SHA256 under
`VerificationMode::Strong` (types in tui-file-picker). On abort/skip mid-file the loop drops
the output and runs `cleanup_temp_file` — the local-cancel baseline of O3 already exists.
Directory recursion: `copy_dir_plan_progress_resolved` (~38451), pre-planned tree. Symlinks:
`copy_symlink_atomic` (~40447, platform-cfg'd variants).

**Move:** `move_path_progress_node` (~37070): no-clobber rename first (`try_no_clobber_rename`,
~39659); EXDEV/Unsupported (`is_cross_device_error`, ~39698) → copy fallback, then
verified-publication source deletion (`remove_source_plan_progress`, ~37919, gated by the
`forced_control_after_verified_publication` step machinery, ~36013/~37654) backed by
`BrowseMoveRecoveryProof { source_manifest, destination_manifest }` (browse.rs ~2211).

**Controls:** `FileTaskControl { Abort, SkipCurrent, Pause, Resume, ConflictResolution }`
(app.rs ~6099); worker `poll_controls` (~38804) is checked **between I/O calls** in the copy
loop; pause is a 100 ms recheck loop; progress snapshots every 80 ms via
`tx.blocking_send`.

**Retry:** `BrowsePasteRetryPlan` (browse.rs ~2221) + `PendingClipboardPaste` (~2288):
incomplete MOVE operations retain exact source→destination mappings and per-source recovery
proofs so a re-paste resumes the same plan without re-suffixing destinations. Completion
stores the plan; `retain_sources` filters to incomplete sources.

**Undo/redo (IMPORTANT — easy to miss):** completed copy/move operations are recorded in an
**in-memory** `FileOperationUndoJournal` (app.rs ~11011, field `file_operation_undo` ~11200):
per-session entries (`record_task_once`, keybindings.rs ~1274–1430) carrying per-mapping
source/destination manifest **proofs**; undo/redo executes via replay workers
(`execute_file_operation_replay_worker`, keybindings.rs ~1925; result message
`AppMessage::FileOperationReplayComplete`, message.rs ~656) — e.g. undo of a copy detaches
every destination root only if its recorded proof still matches, two-phase (detach-all, then
delete). This journal does NOT persist across restarts. Your work must not break it, and
your O3/O4 journal must be designed AGAINST it: one coherent revert story, not two rival
mechanisms with divergent views of what happened. Wedged/abandoned sessions must never
record undo entries claiming completions that didn't happen.

**THE DEFECT.** Cancellation is cooperative-only. When the filesystem itself wedges — a
`read()`/`write()`/`sync_all()`/`rename()` against a dead sshfs mount blocks in
uninterruptible sleep — the worker never reaches the next `poll_controls` and Abort is never
observed. The UI overlay stays up with no way out. Note also `tx.blocking_send` from the
worker: another potential (secondary) blocking edge.

**Hard constraint you must design around:** a thread blocked in uninterruptible filesystem
I/O CANNOT be cancelled, killed, or joined from userspace. No flag, no signal, no timeout on
the blocked call will help. Whatever you build, the *controlling* side must never depend on a
possibly-wedged thread making progress. Known viable shapes (choose/combine/improve):
sacrificial detached I/O threads whose results are accepted only if still current
(generation-checked) and abandoned otherwise; subprocess-based I/O that can be SIGKILLed
(house pattern: `wait_for_child_with_timeout`, host_clipboard.rs ~272); bounded-size I/O ops
with watchdog + abandon. Beware late-wake hazards: an abandoned thread that eventually
unwedges must not corrupt state — it must re-validate its generation/session before ANY
side effect (temp-file writes, renames, journal updates, mpsc sends), and temp paths must
never be reused across generations.

## Outcomes

### O1 — Cancel always works
From the progress overlay, the user can always cancel a copy/move and regain full TUI
control within ~1 second, regardless of filesystem state — mid-transfer on a dead mount,
during sync, during rename, during source deletion. Cancel of a wedged operation means:
worker abandoned (with late-wake safety per above), session terminated, state recorded
honestly (what completed, what is partial, what is unknown-because-wedged). The TUI must
never be blocked or degraded by an abandoned worker's existence.

### O2 — Stall honesty
The progress overlay must distinguish: running (with throughput), **stalled** (no forward
progress for N seconds — pick a sensible default, make it config-tunable), paused, and
cancelling/abandoning. A stalled operation surfaces its options (keep waiting / cancel).
No silent infinite hang states.

### O3 — Revert to known-good
On cancel or failure: destination is left with **no partial artifacts** where the
filesystem is reachable (temp files removed; published files are complete by construction
via the existing temp+rename discipline — preserve it). Where cleanup itself cannot proceed
(unreachable mount), the obligation is **journaled and deferred** — executed when the
location becomes reachable again, or surfaced to the user if it never does. Move semantics:
source deletion only ever after verified publication (preserve the existing
manifest/proof machinery, or an equivalent you can defend). A cancelled cross-device move
must leave the source fully intact.

### O4 — Persistent journal + reconciliation
Operations (at minimum: cut/move pastes and multi-file copies) are journaled durably enough
that: (a) an interrupted/cancelled/crashed operation is visible after TUI restart; (b) when
the destination becomes reachable again (mount recovers, drive reinserted), tonepoet can
offer — or be asked — to resume/reconcile: complete remaining files, clean deferred
partials, finish deferred source deletions. Resume of a partially-copied file may restart
the file or continue from a verified offset — your choice, but justify it; cheap
verification at resume boundaries by default, full-hash verification only under the
existing `VerificationMode::Strong` (house directive: fast path fast, heavy rigor opt-in).
Build on/replace `BrowsePasteRetryPlan` coherently — one mechanism, not two overlapping
ones — and define the relationship to the in-memory `FileOperationUndoJournal` explicitly
(subsume it, feed it, or coexist with documented authority boundaries; never two rival
revert paths). The DB layer (src/db.rs) already has a PREPARED→COMMITTED journal pattern
for metadata writes you may imitate; do NOT entangle with or modify the metadata-write
journal itself.

### O5 — Job-shaped engine (queuing seam, no queue)
Model each copy/move operation as a first-class job with an explicit lifecycle
(planned → running → stalled → paused → cancelling → cancelled/failed/completed →
awaiting-reconciliation → reconciled). One active job at a time is fine (status quo). NO
queue UI, NO multi-job scheduling — but the abstraction must make a future queue an
additive change, not a rewrite.

### O6 — Removable-drive parity
The same stall/cancel/journal/reconcile machinery works when the destination or source is a
removable drive that disappears: operation stalls or fails cleanly, journal records it,
reinsertion enables reconciliation. No sshfs-specific hacks.

## Guardrails

- Preserve existing correct behaviors: temp+atomic-rename publishes, no-clobber rename-first
  moves, EXDEV fallback, verified-publication source deletion, conflict resolution,
  Pause/Resume/SkipCurrent, suffix-stable retry destinations, deletion safety guards
  (root/dot-component/empty rejection, children-before-parents ordering, ~43320).
- Local fast path must stay fast: no per-file subprocess spawn overhead or added fsyncs on
  ordinary local copies unless opted in; keep the 1 MB streaming discipline or better.
- The TUI event loop must never block on the engine (progress sends stay non-stalling —
  fix the `blocking_send` edge if you judge it real).
- House async conventions (see the surveyed patterns): generation counters + stale-completion
  rejection, `Arc<AtomicBool>`-style flags where cooperative checks suffice, AppMessage
  completion routing, non-blocking progress. Imitate `wait_for_child_with_timeout` for any
  subprocess supervision.
- Scope: filesystem copy/move ONLY. Do not touch the metadata tag-write transaction /
  `.tonepoet-bak` machinery, the conversion pipeline, or the tag clipboard.
- Input rules (byobu/tmux): no F-keys; no Shift+Click/Shift+arrows/Ctrl+Space as the only
  path to anything; keep existing binding conventions for the overlay.
- No heavyweight new dependencies; pure Rust preferred; anything added must build in the nix
  sandbox. No nightly.
- Testability is a first-class outcome: the I/O layer must be structured so tests can inject
  hangs, partial writes, EXDEV, and vanished mounts WITHOUT a real network filesystem
  (trait-level seam, injectable I/O, or equivalent). Ship fault-injection tests proving O1
  (cancel during injected wedge), O3 (no partials / deferred cleanup), O4 (journal survives
  restart; reconciliation completes), and stall detection.
- Full workspace gate green; version stays 0.4.6.

## Deliverables

Complete replacement files (or unambiguous per-file patches) for every changed file; new
modules as complete files; a summary of the architecture chosen and WHY (especially the
wedged-I/O cancellation mechanism and the journal format/location); the test list; and an
honest statement of what you could not verify in your environment.

## Bundle manifest

- This brief.
- Complete `src/` tree of the main crate (all referenced code lives there: keybindings.rs,
  browse.rs, app.rs, message.rs, context_menu.rs, event_loop.rs, draw_overlays.rs, db.rs,
  host_clipboard.rs, bookmark_workers.rs, probe.rs, etc.).
- Complete `crates/tui-file-picker/` (FilesystemClipboard, PastePlan, manifests, progress
  types).
- Root `Cargo.toml` and `CLAUDE.md` (project overview, build/test commands).

NOT included (not germane): other workspace crates (`tonepoet-backend`, `tonepoet-features`,
`sacd-rs`, `dvda-demuxer`, `dvdvideo`, `tonepoet-wizard`), `target/`, other docs. If anything
you need is missing, say so explicitly rather than guessing.
