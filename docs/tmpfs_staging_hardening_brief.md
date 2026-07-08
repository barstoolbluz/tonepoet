# tmpfs Staging Hardening: Fault-Injection Tests + Observability

The tmpfs staging feature landed in v9 and compiles/passes all tests. The core design is sound. This pass hardens it with fault-injection tests and structured observability — no architectural rewrites.

## 1. Fault-Injection Tests

Add tests that exercise the failure/retry/cleanup paths. These are the highest-value improvement — the risk surface is filesystem failure behavior, and none of it is currently tested.

### Tests to add:

**Scratch → disk retry:**
- Scratch materialization hits ENOSPC → retries on disk and succeeds
- Scratch track conversion hits ENOSPC → retries on disk before terminal fragment publication
- Scratch merge/metadata/features stage hits ENOSPC → retries on disk if the failure is scratch-scoped

**Output vs scratch discrimination:**
- Output publish hits ENOSPC → does NOT retry as a scratch failure (output exhaustion is a real failure)

**Reservation lifecycle:**
- Reservation is released after a failed scratch attempt (not leaked)
- Reservation is released when StagingDir is dropped (verify via budget accounting)

**Stale cleanup:**
- Held scratch lock prevents cleanup (active job's staging is not removed)
- Stale unlocked lock permits cleanup (abandoned staging from a crashed run is cleaned)
- Corrupt/unreadable owner marker falls back safely (doesn't panic or skip all cleanup)

### How to inject faults:

The retry path triggers on storage-exhaustion class errors (ENOSPC, EDQUOT, EFBIG, write-zero). Tests should:
- Create a scratch-backed staging dir
- Simulate the error at the appropriate stage (e.g., make a materializer/converter return an ENOSPC-class error)
- Verify the retry path fires, creates disk-backed staging, and the job completes
- Verify the scratch reservation was released

For the filesystem-level faults, you don't need to actually fill a tmpfs. The retry decision is based on error classification (`is_retryable_scratch_storage_exhaustion` or equivalent). Tests can construct the error directly and verify the classifier returns the right decision, then separately verify the retry machinery responds correctly to that decision.

## 2. Structured Observability

Add log lines (using the `log` crate, consistent with the rest of the codebase) at each decision point. These make the feature debuggable when users report unexpected behavior.

### Log lines to add:

| Event | Level | Content |
|---|---|---|
| Scratch admitted | `info` | Job ID, estimated bytes, budget remaining, scratch path |
| Scratch rejected: memory budget | `info` | Job ID, estimated bytes, configured limit, active reservations, available RAM |
| Scratch rejected: filesystem capacity | `info` | Job ID, estimated bytes, scratch filesystem free space |
| Scratch rejected: available memory | `info` | Job ID, estimated bytes, MemAvailable |
| Scratch retrying on disk | `warn` | Job ID, original error, disk staging path |
| Reservation acquired | `debug` | Job ID, bytes reserved, new total |
| Reservation released | `debug` | Job ID, bytes released, new total |
| Stale cleanup: removed tree | `info` | Staging path removed |
| Stale cleanup: skipped active lock | `debug` | Staging path, lock holder |
| Scratch non-tmpfs warning | `warn` | Configured scratch path is not on tmpfs/ramfs |

Use `log::info!`, `log::warn!`, `log::debug!` — no new dependencies needed.

## 3. Conservative Output-Capacity Preflight (Optional)

If time permits: before starting a job, check output filesystem free space. If estimated final output size + headroom > free space, log a warning. Do NOT hard-fail — encoding settings, metadata, and format conversion make exact size estimation unreliable. Only hard-fail if the shortfall is extreme (e.g., output filesystem has < 100MB free).

This is lower priority than tests and observability.

## What NOT to do

- Do not rewrite retry to requeue through the shared scheduler. The current serial-pipeline retry inside the postprocess worker is acceptable for a rare fallback path.
- Do not add adaptive estimation. No real-world data exists yet to tune against.
- Do not add TUI scratch-status UI. Build on stable counters/observability first.
- Do not attempt a streaming/piping architecture rewrite.
- Do not do a sweeping typed-error refactor. The current stage/path classifier is sufficient.

## Files

The test and observability code should be added to:
- `src/convert/pipeline/memory_budget.rs` — reservation lifecycle tests, budget accounting tests
- `src/convert/pipeline/stages.rs` — fault-injection tests for retry paths, observability log lines at staging selection and retry points
- `src/convert/pipeline/types.rs` — StagingDir drop verification tests

Supporting files (for context, may need minor additions for log lines):
- `src/convert/processor.rs` — scheduler entry points
- `src/convert/pipeline/track_executor.rs` — per-track conversion
- `src/config.rs` — scratch config fields
