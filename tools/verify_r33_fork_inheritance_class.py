#!/usr/bin/env python3
"""Static acceptance checks for R33 close-driven fork-inheritance tests.

R33 is test-only hardening. Close-driven authority may remain transiently live
when an unrelated parallel libtest worker forks before exec, even when the fd is
CLOEXEC. Tests must bound that kernel interval without serializing the suite or
weakening product contracts that are deliberately immediate.
"""
from __future__ import annotations

from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def read(relative: str) -> str:
    return (ROOT / relative).read_text(encoding="utf-8")


def section(text: str, start: str, end: str) -> str:
    begin = text.index(start)
    finish = text.index(end, begin)
    return text[begin:finish]


def require(condition: bool, message: str) -> None:
    if not condition:
        raise AssertionError(message)
    print(f"[ok] {message}")


def main() -> int:
    concurrency = read("src/concurrency.rs")
    db = read("src/db.rs")
    stages = read("src/convert/pipeline/stages.rs")
    supervisor = read("src/convert/script_supervisor.rs")

    lifecycle_wait = section(
        concurrency,
        "#[cfg(test)]\npub(crate) fn wait_for_close_driven_lifecycle_release(",
        "/// Classify a descriptor after the owning subsystem has durably established",
    )
    require(
        "PersistentLease::acquire_existing_recovery(path, expected_family)" in lifecycle_wait,
        "R33 lifecycle wait observes the real recovery-lock boundary",
    )
    for family in ["QueueScope", "QueueExecution", "ExecutionClaim", "ExecutionStaging"]:
        require(
            f"LeaseFamily::{family}" in lifecycle_wait,
            f"R33 lifecycle wait admits close-driven {family}",
        )
    require(
        "Duration::from_secs(5)" in lifecycle_wait
        and "Duration::from_millis(2)" in lifecycle_wait,
        "R33 lifecycle wait is bounded and cheap on the normal path",
    )
    require(
        'error.contains("live-owned")' in lifecycle_wait,
        "R33 lifecycle wait retries only live-owner contention",
    )
    require(
        "FileExt::unlock(lease.file.as_ref())" in lifecycle_wait,
        "R33 quiescence probe explicitly unlocks its own OFD before drop",
    )
    require(
        "test_coordination_serial" not in lifecycle_wait
        and "scoped_test_coordination_root" not in lifecycle_wait,
        "R33 does not serialize the suite to hide fork inheritance",
    )

    ephemeral_wait = section(
        concurrency,
        "fn reacquire_after_intentional_ephemeral_authority_closes(",
        "fn wait_for_recovery_reserved_after_deliberate_export_closes(",
    )
    require(
        "retired_descriptor: &Path" in ephemeral_wait,
        "R33 ephemeral close helper is keyed to the descriptor that must retire",
    )
    require(
        "Ok(guard) if !retired_descriptor.exists() => return guard" in ephemeral_wait,
        "R33 does not confuse competing admission with descriptor retirement",
    )
    require(
        "Ok(guard) if Instant::now() < deadline" in ephemeral_wait
        and "drop(guard);" in ephemeral_wait,
        "R33 retries scanner-inheritance races without retaining the replacement guard",
    )
    require(
        "parallel-fork grace window" in ephemeral_wait,
        "R33 bounded failure explains the fork-inheritance contract",
    )

    for name, end in [
        (
            "exported_ephemeral_lifetime_keeps_descriptor_visible_until_final_holder_closes",
            "#[cfg(unix)]\n    #[test]\n    fn raw_inherited_fd_export_keeps_descriptor_visible_until_duplicate_closes",
        ),
        (
            "raw_inherited_fd_export_keeps_descriptor_visible_until_duplicate_closes",
            "#[test]\n    fn into_lease_preserves_detached_ephemeral_authority",
        ),
        (
            "into_lease_preserves_detached_ephemeral_authority",
            "#[cfg(unix)]\n    #[test]\n    fn ephemeral_guard_retirement_never_unlinks_a_rebound_descriptor_path",
        ),
    ]:
        body = section(concurrency, f"fn {name}()", end)
        require(
            "reacquire_after_intentional_ephemeral_authority_closes(" in body
            and "&descriptor," in body,
            f"{name} uses the class-level descriptor-retirement wait",
        )

    empty_scope = section(
        db,
        "fn empty_dead_queue_scope_is_reclaimed_but_live_empty_scope_is_preserved()",
        "fn new_queue_scope_reclaims_abandoned_empty_scope_without_prior_load()",
    )
    require(
        empty_scope.index("drop(db_owner);")
        < empty_scope.index("wait_for_close_driven_lifecycle_release(")
        < empty_scope.index("load_queue_items()" , empty_scope.index("drop(db_owner);")),
        "observed empty-QueueScope flake waits only after the live owner is dropped",
    )
    require(
        "LeaseFamily::QueueScope { scope_id }" in empty_scope,
        "empty-QueueScope wait verifies the exact lifecycle family",
    )

    creation = section(
        db,
        "fn new_queue_scope_reclaims_abandoned_empty_scope_without_prior_load()",
        "fn concurrent_queue_scopes_preserve_rows_and_recover_in_scope_order()",
    )
    require(
        creation.index("drop(db_owner);")
        < creation.index("wait_for_close_driven_lifecycle_release(")
        < creation.index("sync_queue(&[&next_item])"),
        "CLI-style new-scope path waits for actual old-owner close before testing cleanup",
    )
    require(
        "load_queue_items()" not in creation,
        "new-scope regression still proves cleanup without a prior queue load",
    )

    recovered = section(
        db,
        "fn recovered_interrupted_rows_release_reservations_immediately()",
        "fn empty_queue_session_creates_no_durable_scope_history()",
    )
    require(
        recovered.index("drop(dead_scope_lease);")
        < recovered.index("wait_for_close_driven_lifecycle_release(")
        < recovered.index("recover_dead_queue_items()"),
        "dead-scope recovery waits for fixture kernel owners before asserting recovery",
    )
    for family in ["QueueScope", "QueueExecution", "ExecutionClaim", "ExecutionStaging"]:
        require(
            f"LeaseFamily::{family}" in recovered,
            f"dead-scope fixture covers close-driven {family}",
        )

    scratch = section(
        stages,
        "fn scratch_backed_live_companion_snapshot_is_protected_then_reaped_after_execution_release()",
        "fn uncoordinated_batch_cleans_its_coordination_workspace_once_complete()",
    )
    require(
        scratch.index("drop(queue_lease);")
        < scratch.index("wait_for_close_driven_lifecycle_release(")
        < scratch.index("retire_descriptor_after_lifecycle_release("),
        "scratch companion regression waits for QueueExecution close before lifecycle retirement",
    )

    immediate = section(
        concurrency,
        "fn journal_operation_final_logical_owner_unlocks_accidental_fork_copy_immediately()",
        "fn journal_operation_deliberate_lifetime_export_remains_live_until_export_closes()",
    )
    require(
        "wait_for_close_driven_lifecycle_release" not in immediate
        and "wait_for_recovery_reserved_after_deliberate_export_closes" not in immediate,
        "JournalOperation final logical-owner regression remains genuinely immediate",
    )
    require(
        "descriptor_availability(&path)" not in immediate
        and "retire_descriptor_after_lifecycle_release(&path, &family)" in immediate
        and immediate.index("drop(lease);")
        < immediate.index("retire_descriptor_after_lifecycle_release(&path, &family)"),
        "R33/R34 preserves the immediate JournalOperation product guarantee without a self-locking pre-probe",
    )

    staging_retry = section(
        concurrency,
        "fn lifecycle_retirement_waits_out_accidental_cloexec_coholder_but_not_live_authority()",
        "fn exported_ephemeral_lifetime_keeps_descriptor_visible_until_final_holder_closes()",
    )
    require(
        "wait_for_close_driven_lifecycle_release" not in staging_retry,
        "ExecutionStaging regression still exercises the production retirement retry itself",
    )

    eof_test = section(
        supervisor,
        "fn control_channel_eof_is_treated_as_cancellation()",
        "#[cfg(target_os = \"linux\")]\n    #[test]\n    fn linux_proc_stat_parser_handles_parentheses_in_comm()",
    )
    require(
        "Duration::from_secs(5)" in eof_test
        and "thread::sleep(CONTROL_POLL_INTERVAL)" in eof_test,
        "R32 control-channel EOF member of the class remains bounded",
    )

    targeted = empty_scope + creation + recovered + scratch
    require("#[ignore]" not in targeted, "R33 does not ignore close-driven regressions")

    print("R33 fork-inheritance class static verification passed")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (AssertionError, ValueError) as error:
        print(f"[FAIL] {error}")
        raise SystemExit(1)
