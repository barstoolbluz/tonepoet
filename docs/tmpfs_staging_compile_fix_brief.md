# tmpfs Staging: 15 Compile Errors to Fix

The tmpfs staging bundle was applied but cannot compile. The model noted it had no compiler available. These are the errors to fix.

## Full `cargo check` error output

```
error[E0255]: the name `format_bytes` is defined multiple times
     --> src/convert/pipeline/stages.rs:10143:1

error[E0425]: cannot find value `artifacts` in this scope
     --> src/convert/pipeline/stages.rs:12116:21
     --> src/convert/pipeline/stages.rs:12128:13
     (8 instances of this same error)

error[E0382]: borrow of moved value: `staging`
     --> src/convert/pipeline/stages.rs:12596:17

error[E0063]: missing field `scratch_staging` in initializer of `PipelineRequest`
   --> src/convert/pipeline/unified_request.rs:101:8

error[E0063]: missing field `scratch_memory_limit_percent` in initializer of `ProcessorConfig`
   --> src/convert/mod.rs:462:50

error[E0063]: missing field `scratch_staging` in initializer of `PipelineRequest`
    --> src/tui/command.rs:4654:50

error: implementation of `FnOnce` is not general enough
     --> src/convert/pipeline/stages.rs:16325:5
     (2 instances)
```

## Error Analysis

1. **`format_bytes` duplicate** (stages.rs:10143) — a function with this name already exists. The new one needs to be renamed or the duplicate removed.

2. **`artifacts` not found** (stages.rs:12116, 12128, x8) — the serial pipeline fallback path references a variable `artifacts` that doesn't exist in scope. Likely a variable was renamed or the code was moved without updating references.

3. **`staging` moved value** (stages.rs:12596) — staging ownership was moved (probably into the retry-on-ENOSPC path) and then borrowed again after the move.

4. **Missing `scratch_staging` field** (unified_request.rs:101, command.rs:4654) — two `PipelineRequest` struct literals don't include the new `scratch_staging` field.

5. **Missing `scratch_memory_limit_percent`** (convert/mod.rs:462) — a `ProcessorConfig` constructor doesn't include the new field.

6. **`FnOnce` lifetime** (stages.rs:16325) — `copy_or_rename_into_publish_temp_with` passes `fs::rename` as a closure but the lifetime inference fails. This is likely a signature issue on the wrapper function.

## Your Task

Fix all 15 compile errors. The source files in this bundle are the current state after the bundle was applied. Do not revert the tmpfs staging changes — fix them so they compile.

## Files in This Bundle

All files referenced in the errors plus their dependencies:
- `src/convert/pipeline/stages.rs` — most errors are here
- `src/convert/pipeline/types.rs` — `PipelineRequest`, `StagingDir`
- `src/convert/pipeline/memory_budget.rs` — new module
- `src/convert/pipeline/mod.rs` — module registration
- `src/convert/pipeline/unified_request.rs` — missing `scratch_staging` field
- `src/convert/pipeline/track_executor.rs` — may reference changed types
- `src/convert/mod.rs` — missing `scratch_memory_limit_percent` in `ProcessorConfig`
- `src/convert/processor.rs` — `ProcessorConfig` definition, scheduler wiring
- `src/tui/command.rs` — missing `scratch_staging` field in `PipelineRequest`
- `src/config.rs` — `ConversionSettings` with new field
- `src/main.rs` — config wiring
