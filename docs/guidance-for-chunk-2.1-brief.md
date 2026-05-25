# Follow-up Guidance for Integrated Chunk 2 Code

## Executive judgement

After reassessing the earlier guidance, I would keep the same three priority areas, but I would tune the emphasis.

The highest-value follow-up work should not start with more architectural expansion. The integrated bundle already compiles and passes tests, so the next phase should make production invariants explicit, testable, and hard to bypass.

I would prioritize:

1. **Unified path invariants** — every source type must enter the same planner-driven orchestration path, and direct process execution must stay quarantined inside `ToolRunner`.
2. **Full `PipelineSettings` semantic preservation** — every user-facing setting must survive queue construction, request construction, planning, execution, logging, and rerun decisions without silent fallback.
3. **Transactional/idempotent execution semantics** — partial outputs, cancellation, failures, and reruns must have defined behavior.
4. **Scheduler correctness under adversarial cases** — album gates, worker reuse, fairness, cancellation, and failure isolation must work under mixed jobs.
5. **Performance measurement before scheduler sophistication** — add counters and benchmarks before adding complex work-cost heuristics.

The main tweak from the earlier guidance: I would not add a more complex concurrency-class scheduler immediately. I would first add observability, bounded queues, state-machine tests, and workload benchmarks. Then use those results to decide whether the pool needs per-tool caps or cost-aware dispatch.

---

## 1. Unified path: make bypasses impossible or obvious

### Goal

All conversion work should pass through one route:

```text
source item -> PipelineRequest -> per-track PlanRequest -> plan_conversion() -> ToolRunner or PassthroughCopy -> post-processing -> publish
```

No source type should retain a private encode path, private copy path, or private process-spawning path.

### Target issues

#### 1.1 Add a routing invariant test

Add a test that scans production conversion code and fails when legacy execution symbols reappear outside approved locations.

Suggested denylist:

```text
tokio::process::Command
std::process::Command
convert_with_backend
copy_flac_with_full_pipeline
encode_command
dsd_to_pcm_command
backend_settings
tool_command_from_backend
CommandBuilder::new().build()
```

Allowed exceptions should be tiny and named. For example:

```text
ToolRunner implementation may spawn processes.
Tests may use fake commands or fake runners.
```

This catches future regressions cheaply.

#### 1.2 Add route tests for every supported source shape

Use fake materializers and a fake planner/runner where possible. The tests should assert that each source reaches the planner boundary.

Minimum matrix:

| Source shape            | Required route                              |
| ----------------------- | ------------------------------------------- |
| Single FLAC passthrough | planner -> `PlanAction::PassthroughCopy`    |
| Single FLAC re-encode   | planner -> command chain                    |
| Single WAV/AIFF         | planner -> command chain                    |
| CUE+image               | materialize -> planner per track            |
| SACD ISO                | materialize -> planner per track            |
| 7z archive              | extract -> planner per track                |
| Mixed queue             | all items share the same dispatch machinery |

Each test should assert planner invocation count and runner invocation count. Do not rely only on final output existence.

#### 1.3 Make new source types enter through one enum or trait boundary

If the code already has a canonical source abstraction, keep it. If not, add one small boundary rather than spreading source logic across the processor.

Example shape:

```rust
pub enum ConversionIngress {
    SingleFile(SingleFileInput),
    SacdIso(SacdInput),
    CueImage(CueInput),
    Archive(ArchiveInput),
}
```

Then require conversion into a `PipelineRequest` through one function or trait impl:

```rust
impl TryFrom<ConversionIngress> for PipelineRequest {
    type Error = OrchestrationError;

    fn try_from(input: ConversionIngress) -> Result<Self, Self::Error> {
        // source-specific materialization metadata lives here,
        // but encode execution does not.
    }
}
```

The point is not the enum itself. The point is that future source support has one obvious gate where review can check settings handoff and planner routing.

### Acceptance criteria

* A grep/static test rejects direct process spawning outside `ToolRunner`.
* Legacy encode/copy/backend symbols do not exist in production routing.
* Every supported source shape has a test proving it enters `plan_conversion()`.
* Adding a new source type requires editing one obvious ingress boundary.

---

## 2. Full `PipelineSettings` handoff: prove semantic preservation

### Goal

The orchestrator must not reinterpret user choices through a lossy intermediate type. `PipelineSettings` should behave as the authoritative conversion contract.

### Target issues

#### 2.1 Ban production `PipelineSettings::default()` at orchestration boundaries

Defaults belong at the UI/CLI/config layer, where user intent becomes explicit settings. The orchestrator should receive already-final settings.

Add a static test that rejects these patterns in production conversion code:

```text
PipelineSettings::default()
..Default::default()
EncodeOptions
ConversionOptions -> PipelineSettings
```

Allow test fixtures and explicit migration code only when named.

#### 2.2 Add all-field sentinel tests

Build a `PipelineSettings` value where every field has a non-default sentinel value. Push it through the real queue/request construction path:

```text
user/config selections -> queue item -> PipelineRequest -> PlanRequest
```

Assert equality at the planner boundary.

This should cover every semantically meaningful field, including less common settings such as dither behavior, resampling policy, metadata disposition, ReplayGain policy, sample format, channel handling, encoder-specific quality knobs, and passthrough policy.

#### 2.3 Add field-coverage drift detection

When Chunk 1 adds a new `PipelineSettings` field, Chunk 2 tests should fail until someone decides how that field flows through orchestration.

Practical approaches:

* Serialize `PipelineSettings` to a stable map in tests and compare field names to a checked-in allowlist.
* Or maintain a test helper that constructs a sentinel settings value and fails to compile when a new required field appears.
* Or add a `settings_fingerprint()` function whose tests prove every field affects the fingerprint unless deliberately marked as UI-only.

#### 2.4 Add a stable settings fingerprint

Add a deterministic fingerprint for conversion identity:

```rust
pub fn settings_fingerprint(settings: &PipelineSettings) -> SettingsFingerprint
```

Rules:

* Include every conversion-affecting field.
* Exclude display-only/UI-only fields.
* Use stable field ordering.
* Treat enum names and values as part of the compatibility contract.
* Use the fingerprint in manifests, logs, and rerun decisions.

Add a test that mutates each conversion-affecting field and expects the fingerprint to change.

#### 2.5 Validate source/settings compatibility before planning

Add explicit validation for combinations that should not silently downgrade.

Examples:

| Combination                                                    | Expected behavior                                                  |
| -------------------------------------------------------------- | ------------------------------------------------------------------ |
| Passthrough requested but output format differs                | Fail or plan re-encode with an explicit reason                     |
| Metadata mutation requested during byte-preserving passthrough | Fail or switch to non-byte-preserving copy with an explicit reason |
| Album ReplayGain requested for a non-album item                | Fail or downgrade with a logged policy decision                    |
| DSD-only option on PCM input                                   | Clear validation error                                             |
| Codec-specific option used with another codec                  | Clear validation error                                             |

The important rule: no implicit fallback.

### Acceptance criteria

* No production orchestration path constructs default settings silently.
* Every `PipelineSettings` field survives queue -> request -> plan.
* New settings fields trigger test or compile review.
* Conversion identity includes a stable settings fingerprint.
* Invalid source/settings combinations fail with actionable errors.

---

## 3. Idempotent execution: make reruns deterministic

### Goal

A rerun should never depend on luck or leftover state. The code should classify every on-disk state and choose one defined action.

### Target issues

#### 3.1 Use transactional output publishing everywhere

Do not write directly to the final output path. Use staged names.

Suggested lifecycle:

```text
output.partial -> output.validated -> output
```

Recommended behavior:

| State on disk                               | Action on rerun                                        |
| ------------------------------------------- | ------------------------------------------------------ |
| Final output exists and manifest matches    | Skip or verify according to policy                     |
| Final output exists and manifest mismatches | Fail unless overwrite policy allows replacement        |
| `.partial` exists                           | Delete and retry                                       |
| `.validated` exists                         | Revalidate and publish atomically, or delete and retry |
| Manifest exists but output missing          | Treat as incomplete and retry                          |
| Output exists but manifest missing          | Fail or verify through a conservative import policy    |

Use atomic rename for the final publish on the same filesystem.

#### 3.2 Add a conversion manifest

For every published track, write a manifest entry or sidecar that records conversion identity and output facts.

Suggested fields:

```text
source path
source size
source mtime and/or content hash
source track identity when extracted
pipeline settings fingerprint
planner version/API version
planned command sequence hash
tool binary identities when available
output path
output size
output hash or validation status
publish timestamp
```

This manifest lets reruns decide whether to skip, verify, redo, or fail.

#### 3.3 Test interruption at every phase

Use a fake runner and fake filesystem where possible. Test interruption at these points:

```text
before materialization
during materialization
after materialization before planning
during command 1
between command 1 and command 2
during command 2
after encode before publish
during metadata write
during ReplayGain
during final publish
```

Each test should assert:

* No corrupt final output appears.
* Planned cleanup paths get cleared.
* Workers return to the pool.
* Album post-processing does not run too early.
* A rerun reaches a valid final state.

#### 3.4 Test mid-chain failure cleanup

For chains like:

```text
ffmpeg -> ssrc -> sox
```

fail each step in turn and assert exact cleanup behavior.

| Failed step      | Required outcome                                                      |
| ---------------- | --------------------------------------------------------------------- |
| First command    | No final output; delete step partials                                 |
| Middle command   | Delete prior intermediates marked for cleanup; no publish             |
| Final command    | Delete intermediates and final partial; no publish                    |
| Metadata stage   | Policy states whether encoded audio remains staged or job fails fully |
| ReplayGain stage | Policy states whether audio publish may continue without ReplayGain   |

This gives idempotency real teeth.

### Acceptance criteria

* Final outputs appear only after successful validation/publish.
* Every partial state has a defined rerun action.
* Manifests drive skip/verify/redo decisions.
* Cancellation and mid-chain failure tests pass for each major phase.

---

## 4. Shared worker pool: prove scheduler correctness before adding complexity

### Goal

The scheduler should handle mixed jobs, album dependencies, cancellation, failure isolation, and worker reuse without race-dependent behavior.

### Target issues

#### 4.1 Encode the scheduler lifecycle as a state machine

Document and test legal transitions.

Suggested states:

```text
Queued
Materializing
MaterializedTrackReady
EncodingTrack
TrackDone
TrackFailed
AlbumReadyForPost
PostProcessing
Published
Failed
Cancelled
```

The exact names can differ. The value comes from testing impossible transitions.

Examples of illegal transitions:

```text
EncodingTrack -> Published without post-processing gate
TrackFailed -> AlbumReadyForPost under fail-fast policy
Cancelled -> PostProcessing
Queued -> TrackDone
```

#### 4.2 Add deterministic scheduler tests

Avoid sleep-based timing tests. Use fake workers, channels, or a simulated clock.

Minimum cases:

| Scenario                        | Assertion                                                           |
| ------------------------------- | ------------------------------------------------------------------- |
| 100 single tracks               | Worker pool drains queue without starvation                         |
| 1 archive with 20 tracks        | Extraction gates track fanout correctly                             |
| 2 SACDs                         | Materialized tracks from both albums can progress                   |
| 5 singles + 2 SACDs + 1 archive | Singles start immediately; materialized tracks join same pool       |
| One album fails                 | Unrelated jobs continue unless fail-fast is active                  |
| Album ReplayGain                | Starts only after all tracks in that album reach the required state |
| Merge                           | Starts only after its source tracks reach the required state        |
| Cancellation under load         | Workers return and post-processing does not start                   |

#### 4.3 Add bounded queues before cost-aware scheduling

Materialization can produce many tracks. Use bounded queues to prevent unbounded memory/path growth.

Suggested configurable limits:

```text
ready_tracks_capacity
ready_jobs_capacity
post_processing_capacity
```

Test that materializers pause when queues fill and resume when workers drain them.

#### 4.4 Add observability before per-tool scheduling rules

Before adding complex scheduling heuristics, collect data.

Counters to add:

```text
jobs_queued
jobs_completed
jobs_failed
tracks_materialized
tracks_encoded
commands_started
commands_failed
workers_busy
worker_idle_ms
ready_queue_depth
post_queue_depth
album_post_wait_ms
tool_runtime_ms by tool
bytes_read
bytes_written
cleanup_paths_deleted
cleanup_paths_failed
```

These counters will show whether 7zz, ffmpeg, sox, SACD extraction, or ReplayGain actually dominate runtime.

#### 4.5 Defer per-tool concurrency classes until metrics justify them

Earlier guidance suggested adding work-cost classes such as `CpuBound`, `IoBound`, and `ExternalThreaded`. I would now treat that as second-phase work.

Do first:

1. Bounded queues.
2. Worker utilization counters.
3. Mixed workload benchmarks.
4. Cancellation/failure tests.

Then add per-tool caps only if metrics show oversubscription or starvation.

Possible later shape:

```rust
pub enum WorkClass {
    Encode,
    Extraction,
    Metadata,
    AlbumAnalysis,
}
```

and configurable caps:

```toml
[scheduler]
worker_count = 15
max_extractions = 2
max_album_analysis = 1
```

But do not add this until the simpler pool has measured problems.

### Acceptance criteria

* Scheduler state transitions have tests.
* Album-level gates cannot fire early.
* Cancellation returns workers to the pool.
* Failure in one job does not poison unrelated jobs unless policy says so.
* Queues have capacity controls.
* Metrics show worker utilization and queue behavior.

---

## 5. Performance: measure representative workloads

### Goal

Performance claims should come from repeatable workloads, not from architecture alone.

### Target issues

#### 5.1 Add a benchmark harness outside normal unit tests

Keep real-media/tool benchmarks separate from normal CI if they require external binaries or large fixtures.

Suggested scenarios:

| Scenario                        | Purpose                               |
| ------------------------------- | ------------------------------------- |
| 100 individual FLAC re-encodes  | Worker saturation and queue drain     |
| 100 passthrough FLACs           | Copy/publish overhead and idempotency |
| 1 archive with 20 tracks        | Extract then fan out                  |
| 2 SACD ISOs                     | Parallel materialization behavior     |
| 1 CUE+image with 20 tracks      | Parallel splitting behavior           |
| 5 singles + 2 SACDs + 1 archive | Mixed scheduling                      |
| Failed command under load       | Worker recovery and cleanup cost      |
| Cancellation under load         | Shutdown latency and restart validity |

Report:

```text
total runtime
tracks/minute
worker utilization
peak ready queue depth
peak post queue depth
tool runtime distribution
bytes read/written
cleanup count
rerun skip/redo counts
```

#### 5.2 Add performance regression thresholds carefully

Do not make CI flaky with strict wall-clock thresholds. Prefer relative or structural assertions in CI:

```text
all workers receive work
queue depth eventually drains
album gates do not block unrelated jobs
rerun skips previously completed matching outputs
```

Keep wall-clock benchmarking as a developer or nightly job.

### Acceptance criteria

* Mixed workload benchmark exists.
* Benchmark reports scheduler and tool metrics.
* Normal CI has deterministic structural performance tests.
* Wall-clock checks do not make common CI runs flaky.

---

## 6. Revised priority order

If the next session has limited time, I would order the work like this:

1. **Routing guardrails**

   * Static test blocks direct process spawning outside `ToolRunner`.
   * Static test blocks legacy encode/copy/backend symbols.
   * Route tests prove every source enters planning.

2. **Settings sentinel tests**

   * Every `PipelineSettings` field survives queue -> request -> plan.
   * New fields trigger review.
   * Stable settings fingerprint exists.

3. **Transactional publish and manifest rerun policy**

   * Final output appears only after validation.
   * Manifest defines conversion identity.
   * Rerun behavior handles partial/final/mismatched states.

4. **Failure and cancellation torture tests**

   * Fake runner blocks at known points.
   * Cleanup paths get deleted.
   * Workers return.
   * Album post-processing does not start after cancellation/failure.

5. **Scheduler state-machine tests**

   * Album gates fire only when legal.
   * Mixed source queues share workers.
   * Unrelated jobs continue after a job failure unless policy says otherwise.

6. **Bounded queues and metrics**

   * Queue capacities prevent runaway materialization.
   * Counters expose worker and queue behavior.

7. **Benchmarks and later scheduler tuning**

   * Measure representative workloads.
   * Add per-tool concurrency caps only when data shows need.

---

## Three highest-value tasks for the next session

If I had to pick only three, I would pick these:

### 1. Full `PipelineSettings` sentinel coverage

This guards against silent semantic loss, especially as Chunk 1 evolves.

Deliverables:

* Sentinel `PipelineSettings` fixture.
* Queue -> request -> plan equality assertions.
* Field drift test.
* Settings fingerprint with per-field mutation tests.

### 2. Transactional publish + manifest-based rerun behavior

This turns idempotency from a goal into explicit behavior.

Deliverables:

* Staged output lifecycle.
* Manifest schema.
* Rerun-state decision table in code and tests.
* Tests for `.partial`, `.validated`, final-with-match, final-with-mismatch, and manifest/output mismatch.

### 3. Cancellation and mid-chain failure tests

This is the most important robustness work after compilation passes.

Deliverables:

* Fake blocking `ToolRunner`.
* Failure injection at every command position.
* Cancellation injection at every major phase.
* Cleanup assertions.
* Worker recovery assertions.
* Album post-processing gate assertions.

---

## Final recommendation

Do not start by making the scheduler more elaborate. First make the existing integrated design auditable:

```text
single route
full settings preservation
transactional outputs
manifest-based reruns
deterministic failure cleanup
scheduler state-machine tests
bounded queues
metrics
benchmarks
```

That sequence best advances rigor, correctness, robustness, idempotency, and performance without adding unnecessary machinery before the data calls for it.
