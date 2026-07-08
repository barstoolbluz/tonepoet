# Integration Test Triage: 4 Contract Test Failures

All 2438 lib tests pass. These are the last 4 failures — integration tests in `tests/chunk2_orchestrator_contract.rs` that grep source files for architectural invariants.

## How These Tests Work

These tests use `include_str!()` to read source files as strings and assert that specific patterns exist or don't exist. They're source-scanning contract tests, not runtime tests.

## Failure 1: `planner_metadata_disposition_is_consulted_after_topology_planning`

**Line 102:** `assert!(!bridge.contains("settings.metadata.transfer_tags = false"))`

**Problem:** `src/convert/pipeline/plan_bridge.rs` contains 3 occurrences of `settings.metadata.transfer_tags = false`. The contract says the bridge should NOT hard-code metadata transfer disabling — it should consult the planner's metadata disposition instead.

**Fix:** Either the bridge code needs to stop setting `transfer_tags = false` directly (using the planner disposition instead), or if the code is correct and the contract is stale, update the contract test.

## Failure 2: `legacy_compat_pipeline_settings_cover_the_legacy_option_surface_explicitly`

**Line 162:** `assert!(unified.contains("wavpack.hybrid"), "settings builder missing wavpack.hybrid")`

**Problem:** `src/convert/pipeline/unified_request.rs` exists but does not contain the string `wavpack.hybrid`. The settings builder is missing WavPack hybrid mode coverage.

**Fix:** Add `wavpack.hybrid` coverage to the settings builder in `unified_request.rs`, or update the contract if hybrid mode is intentionally excluded from the legacy compat surface.

## Failure 3: `every_external_process_boundary_runs_through_tool_runner_modules`

**Line 174:** `assert!(!contents.contains("std::process::Command"), "{path} spawns directly")`

**Problem:** `src/convert/pipeline/stages.rs` contains `use std::process::Command as ProcessCommand` at lines 3857 and 4538. Both are inside `#[cfg(test)]` blocks — test code that spawns processes directly for fixture setup, not production code.

**Fix:** The contract test should exclude test-only imports. Either:
- Change the assertion to check only non-test code (e.g., check that `std::process::Command` doesn't appear outside `#[cfg(test)]` blocks)
- Or rename the test imports to avoid the pattern (e.g., `use std::process::Command as TestProcessCommand` — but this is just obscuring it)

The cleanest fix: split the source string at the first `#[cfg(test)]` and only check the production portion.

## Failure 4: `compatibility_orchestrator_metadata_gate_matches_scheduler_gate`

**Line 184:** `stages.matches("planner_metadata_already_satisfied(artifacts.as_ref().expect(\"artifacts present\"), &req)").count() >= 2`

**Problem:** `stages.rs` contains 0 occurrences of this exact string. The function may have been renamed, the call signature may have changed, or the call sites may have been refactored.

**Fix:** Find what the current metadata satisfaction check is called and update the contract test to match, or update the code if the invariant was accidentally broken.

## Files in This Bundle

- `tests/chunk2_orchestrator_contract.rs` — the contract test file
- `src/convert/pipeline/stages.rs` — checked by tests 3 and 4
- `src/convert/pipeline/plan_bridge.rs` — checked by test 1
- `src/convert/pipeline/unified_request.rs` — checked by test 2
- `src/convert/pipeline/track_executor.rs` — checked by test 1
- `src/convert/pipeline/planned_adapter.rs` — checked by test 3
- `src/convert/processor.rs` — checked by test 3
- `src/convert/pipeline/mod.rs` — for module structure reference

## Your Task

Fix all 4 contract tests. These are architectural invariants — understand what each test is trying to enforce before changing it. If the production code correctly evolved past a contract, update the contract. If the production code accidentally broke an invariant, fix the production code.
