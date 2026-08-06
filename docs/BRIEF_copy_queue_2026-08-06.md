# tonepoet — Copy/move queue + minimizable progress brief (2026-08-06)

You are starting **fresh** with no prior context. Everything you need is in this bundle. This
brief describes **outcomes and guardrails**; included diagnosis is *evidence* — you choose
HOW, so long as outcomes are met and guardrails hold.

**Project:** tonepoet, Rust CLI + TUI audio toolkit (ratatui 0.26 / crossterm 0.27, tokio,
edition 2021), version 0.4.6 — **do not bump**. Gate `cargo test --workspace --no-fail-fast`
is green (5595/0) and must stay green.

## The feature

Browse copy/move (paste) operations gain a **queue**: starting a paste while one runs
ENQUEUES it instead of refusing; the progress overlay becomes **minimizable** to a live
footer segment; queued jobs are visible and manageable. This completes the seam the
copy-hardening round deliberately left ("job identity, generation, persisted lifecycle,
exact mappings, and supervisor boundaries form the additive seam for a future queue").

## What exists (mapped; verify at will, don't re-derive)

### Engine layer — ALREADY multi-job ready. Reuse, don't rebuild.
- One helper subprocess per job, one supervisor thread per job
  (`file-task-supervisor-{session_id}`, keybindings.rs ~41507), per-job control channel.
- Durable journal per job: `{job_id}.jsonl` (file_task_runtime.rs), `pending_journals()`
  returns a Vec (~1161); temp artifacts are `.tonepoet-part-{job_id}-{generation}-…` —
  collision-free across jobs/generations by construction.
- Messages route by `session_id` (`FileTaskProgress`/`FileTaskComplete`, message.rs ~640);
  stale completions are already ignored by id.

### TUI layer — the single-job bottleneck. This is where the work is.
- `ActiveOverlay::FileTaskProgress(session)` — one modal overlay, single session
  (app.rs ~5649/6132; session ids from a global atomic counter).
- `browse.pending_clipboard_paste: Option<PendingClipboardPaste>` (browse.rs ~2498) and the
  HARD REFUSAL: "A clipboard paste is already running" (keybindings.rs ~36256-36258).
- `browse.filesystem_clipboard_retry_plan: Option<BrowsePasteRetryPlan>` (~2494) — one
  retained recovery token.
- Completion reducer `reduce_file_task_complete` (event_loop.rs ~1540-1801): per-job
  retention → undo recording → clipboard repair → non-blocking browse refresh → retry-plan
  retention; control-plane-only (no synchronous filesystem probes — PRESERVE this).
- Startup recovery restores exactly ONE pending journal (`next_back()` = most recent,
  file_task_runtime.rs ~1204) even when several exist; the recovery status already counts
  `total_pending_jobs`.

### Footer "details" slot — the user-designated minimize target.
- Right side of the SHARED footer context bar — `draw_footer` (draw_footer.rs:17) is
  rendered by multiple screens (draw.rs ~77/453/480, convert_screen.rs ~127), each passing
  `app.last_file_task_progress.is_some()`, so the slot is app-wide, not Browse-only:
  `TuiButton::FileTaskMessages`
  (draw_footer.rs ~163-199, button_map.rs ~30), cyan bold, responsive
  (` details ` ≥18 cells → ` msgs ` → `d` at 1 cell, hitbox preserved at every width),
  clickable; today it appears only AFTER a task completes
  (`app.last_file_task_progress`, app.rs ~11227) and reopens the (modal) progress overlay
  (`:messages` command is the keyboard route, event_loop.rs ~10162/14766).
- A reusable one-line progress-bar primitive exists:
  `progress_bar_spans` (tui-file-picker progress.rs ~1596) rendering `[█░░] 45%`.
- The overlay is currently MODAL and Esc while running means CANCEL — "minimize" is new UX
  and must not be confusable with cancel.

## Outcomes

### Q1 — Paste enqueues instead of refusing
With a job running, a new paste (all routes: CTRL+V/CTRL+P, tree/file context menus)
creates a QUEUED job — snapshotting its clipboard, plan intent, and destination at enqueue
time — and tells the user its queue position. The "already running" refusal disappears.
Queued jobs start automatically (FIFO) when the running job reaches a terminal state.
**Serial execution (one running job) is the required v1 model** — do not build concurrent
execution; keep the code shaped so N>1 could later be a config, but ship serial.

### Q2 — Plans are validated at START, not trusted from enqueue
A queued job's destination mappings were computed while an earlier job was still mutating
the filesystem (suffix decisions can collide with the earlier job's outputs; sources can
vanish; the destination dir can move). At dequeue/start, re-validate — and where the
existing machinery supports it, re-plan — the job against current reality using the same
no-clobber/conflict machinery a fresh paste would use. Surprises surface through the
existing conflict-resolution flow, not silent overwrites. A queued CUT job whose sources
were meanwhile moved/deleted degrades honestly (per-root failure/skip, not a wedge).

### Q3 — Minimizable progress (the user's core ask)
The progress overlay gains an explicit MINIMIZE affordance (a clickable button in the
overlay AND a byobu-safe key — NOT Esc, which stays cancel/close-per-current-semantics).
While minimized:
- the overlay closes and the full TUI is interactive;
- the footer's details slot shows a LIVE segment: compact progress meter
  (reuse `progress_bar_spans` or tighter) + something like percent and queue depth
  (e.g. `[██░] 61% +2` — exact composition is yours; it must degrade gracefully through
  the existing narrow-width tiers and keep a clickable hitbox at 1 cell);
- clicking the segment (or `:messages`) RESTORES the overlay to its live state;
- job transitions while minimized keep the segment accurate (next queued job starts,
  totals roll over) without stealing focus;
- the segment is visible from EVERY screen that renders the shared footer (the flag is
  already passed at each `draw_footer` call site) — switching tabs must not lose the meter.
Post-completion, the slot returns to today's "details" behavior (retained last report).

### Q4 — Attention-demanding states never hide
While minimized: a CONFLICT prompt, a STALL, or a terminal failure must surface — restore
the overlay automatically (recommended) or make the segment visually demand attention and
block the queue until answered (justify whichever you choose; silence is not acceptable).
The stall/cancel guarantees of the hardened engine (cancel ≤ ~1s even when wedged) apply
identically to minimized and queued states.

### Q5 — Queue visibility and control
The progress overlay grows a queued-jobs section: source/destination summary and count per
queued job, with the ability to CANCEL a queued job (removing it before start) and to
cancel the running job (existing control). Reordering is optional — include only if cheap.
No new top-level screen; the conversion Queue tab (tab 4) is a different feature — do not
entangle or confuse naming (this is the "file operations queue" / "transfers", label
choice yours but visibly distinct from conversion queue).

### Q6 — Per-job bookkeeping replaces the singletons
`pending_clipboard_paste`, retry-plan retention, undo recording, clipboard repair, and
browse refresh must become per-job (keyed by session/job id) so completions of successive
jobs each reconcile correctly. The completion reducer stays control-plane-only. The
user-facing retry-plan slot may remain "most recent incomplete job" (status quo semantics)
— but a queued or failed job's durable journal must never be orphaned by another job's
completion. New Copy/Cut must not corrupt already-enqueued jobs (they own snapshots).

### Q7 — Startup recovery understands multiples
With multiple pending journals, recovery restores the most recent as today AND surfaces
the others (count + a way to resume them — reusing the existing resume path per journal is
sufficient; auto-enqueueing them as queued reconciliation jobs is acceptable if you can do
it honestly). No journal is silently dropped.

### Q8 — Quit with queued work is explicit
Quitting while jobs are queued (not yet started) warns and requires confirmation (running
jobs already have journal protection; queued-but-unstarted jobs have none — say so in the
prompt). Persisting the queue across restarts is NOT required for v1; if you don't persist,
the prompt must make the loss explicit.

## Guardrails
- Byobu-safe input rules: no F-keys; no Shift+Click/Shift+arrows/Ctrl+Space as the only
  path to anything. Esc's existing meanings in the overlay must not silently change.
- Serial v1 (Q1). Do NOT add concurrent job execution, worker pools, or scheduling config.
- Preserve every copy-hardening invariant: journal authority, generation barriers,
  late-wake safety, proof-gated undo recording, control-plane-only completion reduction,
  non-blocking progress, stall detection, `:clipboard`-style honesty about outcomes.
- Preserve existing tests; the single-job flows (one paste, no queue involvement) must
  behave byte-identically except for message wording you intentionally improve.
- The footer hint area must keep working: the live segment shares the row with keybinding
  hints/status messages exactly as the details slot does today (right-aligned, hints
  truncate first).
- The conversion queue (convert/queue.rs, Queue tab) is OUT of scope — no shared state, no
  UI mixing.
- No new dependencies. Version stays 0.4.6.
- Tests, minimum: (a) enqueue-while-running → runs after completion (fake/stub worker at
  the dispatch layer as existing supervisor tests do); (b) queued-job cancel before start;
  (c) re-validation at start catches a destination collision created by the prior job;
  (d) minimized-state segment reflects running→queued transitions; (e) conflict while
  minimized surfaces per your Q4 choice; (f) per-job completion reconciliation for two
  successive jobs (undo recorded per job, clipboard repaired per job); (g) quit-with-queue
  confirmation; (h) multi-journal startup surfacing. Dispatch-level tests preferred
  (handle_key / reducer level, per the house pattern).

## Deliverables
Complete replacement files (or unambiguous per-file patches); architecture summary with
WHY (queue data model, minimize/restore state machine, Q4 choice); test list; honest
statement of what you could not verify (no real terminal).

## Bundle manifest
- This brief.
- Complete `src/` tree (all referenced code: keybindings.rs, app.rs, browse.rs,
  event_loop.rs, message.rs, draw_footer.rs, draw_overlays.rs, draw_browse.rs,
  button_map.rs, file_task_runtime.rs, command.rs, config.rs, convert/queue.rs for
  contrast, etc.).
- Complete `crates/tui-file-picker/` (progress.rs — FileTaskProgressState, phases,
  progress_bar_spans; state.rs — PastePlan/plan_filesystem_paste; filesystem_clipboard.rs).
- Root `Cargo.toml`, `CLAUDE.md`.

NOT included (not germane): other workspace crates, `target/`, other docs. If anything you
need is missing, say so explicitly rather than guessing.
