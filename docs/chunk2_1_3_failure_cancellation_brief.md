# Chunk 2.1.3: Cancellation and Mid-Chain Failure Tests

## For: Reasoning model (GPT Pro)
## Project: tonepoet — CLI + TUI audio conversion toolkit
## Language: Rust (edition 2021, async via Tokio)
## Quality bar: Rigor, correctness, robustness, idempotency, performance (in that order).
## Prerequisites: Chunks 1, 2, 2.1.1, and 2.1.2 are integrated and compiling. 76 tests pass across all suites.

---

## 1. What this chunk does

Prove that the orchestrator handles every failure mode deterministically. No corrupt output, no orphan files, no stuck workers, no post-processing after cancellation. This is a test-only chunk — no production code changes, only new tests that exercise existing failure and cleanup paths.

---

## 2. What exists today

### 2.1 Cancellation infrastructure (already built)

- **CancellationToken** from `tokio_util` — passed through every layer: processor → scheduler → track executor → ToolRunner → child process
- **RealToolRunner** — on cancellation, calls `start_kill()` on child process, waits for reap, returns `ToolRunnerError::Cancelled`
- **Worker loop** — checks `cancel.is_cancelled()` at top of each iteration, races `cancel.cancelled()` against `notify.notified()` via `tokio::select!`
- **Track loop** — checks `cancel.is_cancelled()` after each track and breaks early

### 2.2 Failure handling (already built)

- **StubToolRunner** — transcript-backed test double. `push_output()` queues success, `push_failure(stderr)` queues `NonZeroExit(1)`. Records every invocation in `transcript()`.
- **FailurePolicy** — `FailAlbumOnAnyTrackFailure` (fail-fast) or `AllowPartialAlbum` (continue with survivors)
- **AlbumOutcome** — `Complete`, `Partial`, or `Blocked { reason }` with `BlockReason` variants: `TrackFailures`, `RequiredStageFailure`, `MaterializeFailed`, `PlanFailed`, `PublishFailed`, `Cancelled`
- **Album completion tracker** — `AlbumCompletionTracker::mark_track_finished()` returns `AlbumReadiness::Failed` if any track fails under fail-fast policy
- **Post-processing gates** — metadata, ReplayGain, features, publish each check `current_outcome` and skip if `Blocked`

### 2.3 Cleanup (already built)

- **StagingDir** — `Drop` impl calls `remove_dir_all()` when `armed == true`
- **ConversionPlan.cleanup_paths()** — planner declares intermediate files; track executor deletes them on both success and error paths
- **Track work directories** — `.track-{ordinal}.work` directories cleaned up by `cleanup_track_work_dir()`
- **Publish recovery** — recovery marker + backup directory enables rollback from interrupted publish

### 2.4 What's missing

No tests exercise these paths systematically:
- No test cancels mid-command and verifies cleanup
- No test fails step 2 of a 3-step chain and checks intermediate file deletion
- No test verifies workers return to pool after failure
- No test verifies post-processing gates hold after cancellation
- No test fails at every major phase boundary
- No test mixes failures and successes across albums in the same worker pool

---

## 3. Deliverables

### 3.1 BlockingToolRunner — a controllable test double

Extend the testing toolkit with a `ToolRunner` implementation that can:
- **Block at specific points** — a command starts but doesn't complete until a signal is sent
- **Fail at specific positions** — the Nth command in sequence returns NonZeroExit
- **Succeed after delay** — simulate slow tools

The existing `StubToolRunner` queues fixed responses. The new `BlockingToolRunner` adds coordination:

```rust
pub struct BlockingToolRunner {
    // Queued behaviors: for each command invocation in order,
    // what should happen (succeed, fail, block-until-signal, etc.)
    behaviors: Mutex<VecDeque<ToolBehavior>>,
    // Transcript of all invocations
    transcript: Mutex<Vec<CommandRecord>>,
}

pub enum ToolBehavior {
    Succeed,
    FailWithStderr(String),
    BlockUntilSignal(tokio::sync::oneshot::Sender<()>),
    BlockThenFail(tokio::sync::oneshot::Sender<()>, String),
    BlockThenSucceed(tokio::sync::oneshot::Sender<()>),
}
```

The runner pops the next behavior on each `run()` call. `BlockUntilSignal` waits on a oneshot receiver — the test sends the signal when ready. This enables tests that:
- Start a command, verify state, then let it complete
- Start a command, cancel via CancellationToken, verify cleanup
- Interleave failures and successes across multiple tracks

### 3.2 Mid-chain failure tests

For a multi-step chain (e.g., ffmpeg → ssrc → sox producing 3 PlannedCommands):

| Failed step | Assertions |
|-------------|-----------|
| Step 1 fails | No intermediate files remain. No final output. Track marked failed. |
| Step 2 fails | Step 1's intermediate cleaned up via cleanup_paths(). No final output. Track marked failed. |
| Step 3 fails | Steps 1-2 intermediates cleaned up. No final output. Track marked failed. |
| All succeed | Final output exists. Intermediates cleaned up. Track marked ok. |

For each case assert:
- `ConversionPlan.cleanup_paths()` files are deleted
- Track work directory (`.track-N.work`) is deleted
- `ScheduledTrackOutput.ok == false` for failures
- `ScheduledTrackOutput.artifact == None` for failures
- Transcript shows commands were invoked in order up to the failure point
- No commands after the failure point were invoked

### 3.3 Cancellation at every phase

Inject cancellation via `CancellationToken::cancel()` at each major phase and verify:

| Cancellation point | Assertions |
|-------------------|-----------|
| Before materialization | No staged files. StagingDir cleaned up on drop. |
| During materialization | Materializer's child process killed. Partial staged files cleaned up. |
| After materialization, before planning | Staged files cleaned up via StagingDir drop. |
| During command 1 of encoding | Child process killed. Intermediates cleaned up. |
| Between command 1 and command 2 | Command 1's output cleaned up. Command 2 never invoked. |
| During metadata write | Metadata tool killed. Audio output may exist but not published. |
| During ReplayGain | Loudgain killed. Audio output may exist but not published. |
| During publish (atomic rename) | Recovery marker written. Backup exists. No corrupt final output. |

For each case assert:
- `AlbumOutcome::Blocked { reason: Cancelled }` (or the outcome reflects cancellation)
- No corrupt files at final output paths
- Workers return to pool (scheduler state is clean)
- No post-processing stages run after the cancellation point

### 3.4 FailurePolicy tests

**Fail-fast (FailAlbumOnAnyTrackFailure):**
- 5-track album, track 2 fails → tracks 3-5 still attempt (current behavior: loop continues, checks cancel at end) → album outcome is Blocked
- Post-processing does NOT run
- Manifest is NOT written

**Allow partial (AllowPartialAlbum):**
- 5-track album, track 2 fails → tracks 1,3,4,5 succeed → album outcome is Partial
- Post-processing DOES run for successful tracks
- Manifest records 4 successful + 1 failed track
- Published album has 4 files

### 3.5 Worker pool recovery tests

Using `BlockingToolRunner` + the scheduler:

| Scenario | Assertions |
|----------|-----------|
| 1 track fails in a 15-worker pool | 14 workers continue processing other jobs. Failed worker returns to pool. |
| Album fails under fail-fast | Workers processing that album's remaining tracks finish but results are discarded. Workers return to pool. |
| Cancellation of one album in a mixed queue | Other albums' workers continue. Cancelled album's workers drain and return. |
| Worker panic (if testable) | Pool doesn't deadlock. Other workers continue. |

### 3.6 Album post-processing gate tests

Verify the gate sequence using the real `finish_pipeline_album_for_scheduler()` with fake track outputs:

| Input state | Expected gate behavior |
|------------|----------------------|
| All tracks ok | Metadata → ReplayGain → Features → Publish all run |
| 1 track failed, fail-fast | Blocked after convert. No metadata, no RG, no features, no publish. |
| 1 track failed, allow-partial | Metadata on survivors → RG on survivors → Features → Publish with partial manifest |
| All tracks cancelled | Blocked with Cancelled. Nothing runs. |
| Metadata fails | Blocked after metadata. No RG, no features, no publish. |
| ReplayGain fails | Blocked after RG. No features, no publish. |
| Publish fails | Blocked with PublishFailed. Durable log still written. |

### 3.7 Manifest interaction with failures

- **Successful conversion + manifest written** → rerun with SkipIfManifestMatch → skips
- **Failed conversion, no manifest** → rerun → proceeds normally
- **Partial conversion, manifest records failures** → rerun → redoes (fingerprint may match but track count differs)
- **Cancelled conversion, no publish, no manifest** → rerun → proceeds normally (StagingDir cleaned up)

---

## 4. Design constraints

1. **Test-only chunk.** No production code changes. All new code is in test files.
2. **Use existing infrastructure.** StubToolRunner, CancellationToken, FailurePolicy, AlbumOutcome, StagingDir — all already exist. Build on them.
3. **BlockingToolRunner is a test utility.** It lives in a test helper module, not production code.
4. **Deterministic tests.** No sleep-based timing. Use oneshot channels for synchronization and fake runners for control.
5. **The tonepoet-pipeline crate is not modified.**

---

## 5. Code files the reasoning model needs

1. **tool.rs** — ToolRunner trait, StubToolRunner, RealToolRunner, ToolRunnerError, CancellationToken usage
2. **types.rs** — FailurePolicy, AlbumOutcome, BlockReason, StagingDir, PipelineReport
3. **scheduler.rs** — SharedWorkerPool, AlbumCompletionTracker, AlbumReadiness, work unit kinds
4. **track_executor.rs** — execute_planned_track_conversion, execute_commands, cleanup logic
5. **stages.rs excerpts** — finish_pipeline_album_for_scheduler (post-processing gates), convert_tracks (track loop with cancellation)
6. **rerun.rs** — RerunDecision (for manifest interaction tests)
7. **manifest.rs** — ConversionManifest (for failure + manifest interaction tests)
8. **tonepoet-pipeline/src/plan.rs** — ConversionPlan, cleanup_paths(), PlanAction

---

## 6. Deliverables

1. **BlockingToolRunner** — controllable test double with ToolBehavior enum, oneshot coordination, transcript recording.

2. **Mid-chain failure tests** — fail each step of a 3-step chain, assert cleanup and state.

3. **Cancellation phase tests** — cancel at each major phase, assert no corrupt output, workers return, no post-processing after cancellation.

4. **FailurePolicy tests** — fail-fast vs allow-partial with multi-track albums, assert correct outcome and gate behavior.

5. **Worker pool recovery tests** — failures and cancellations in a shared pool, assert other jobs continue.

6. **Post-processing gate tests** — feed fake track outputs to `finish_pipeline_album_for_scheduler()`, assert each gate's go/no-go decision.

7. **Manifest interaction tests** — verify manifest behavior after failures, partial completions, and cancellations.

8. **Sequenced test file organization** — group tests logically (mid-chain, cancellation, policy, pool, gates, manifest).

For each test: clear setup, deterministic control flow, specific assertions on state/cleanup/outcome. No timing-dependent tests.

---

## 7. Acceptance criteria

- [ ] BlockingToolRunner exists with block/fail/succeed behaviors and oneshot coordination
- [ ] Mid-chain failure cleanup is tested for every step position in a multi-step chain
- [ ] Cancellation is tested at every major phase (materialization through publish)
- [ ] No test leaves corrupt files at final output paths
- [ ] Workers return to pool after failure and cancellation
- [ ] Post-processing gates correctly block after failure/cancellation
- [ ] FailurePolicy::FailAlbumOnAnyTrackFailure blocks post-processing
- [ ] FailurePolicy::AllowPartialAlbum allows post-processing for survivors
- [ ] Manifest is not written after failed or cancelled conversions
- [ ] Rerun after failure proceeds normally (no stale state)
- [ ] All tests are deterministic (no sleeps, no timing dependencies)
- [ ] All tests pass, clippy clean, no new warnings
