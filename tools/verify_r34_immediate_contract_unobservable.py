#!/usr/bin/env python3
"""Static acceptance checks for R34's immediate-contract test correction.

R34 must keep the JournalOperation final-owner guarantee genuinely immediate
while removing an observational pre-probe that can manufacture its own inherited
flock under default-parallel libtest execution. Secondary fixture corrections
must remain test-only and narrowly scoped.
"""
from __future__ import annotations

from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def read(path: str) -> str:
    return (ROOT / path).read_text()


def section(text: str, start: str, end: str) -> str:
    begin = text.index(start)
    finish = text.index(end, begin)
    return text[begin:finish]


def require(condition: bool, message: str) -> None:
    if not condition:
        raise AssertionError(message)
    print(f"PASS: {message}")


def main() -> int:
    concurrency = read("src/concurrency.rs")
    bluray = read("src/disc/bluray_mapper.rs")
    fixture = ROOT / "fixtures/test-bin/ffprobe-fails.sh"

    drop_impl = section(
        concurrency,
        "impl Drop for PersistentLease {",
        "#[derive(Debug)]\npub struct MutationClaimGuard",
    )
    require(
        "LeaseFamily::JournalOperation" in drop_impl
        and "release_local_persistent_lease_owner" in drop_impl
        and "FileExt::unlock" in drop_impl,
        "production JournalOperation final-owner Drop still explicitly unlocks the shared OFD",
    )

    retirement = section(
        concurrency,
        "fn acquire_existing_for_lifecycle_retirement(",
        "fn acquire_existing_with_transient_contention(",
    )
    require(
        "LeaseFamily::ExecutionStaging" in retirement
        and "LeaseFamily::JournalOperation" not in retirement,
        "JournalOperation lifecycle retirement remains no-retry; only ExecutionStaging gets transient contention retry",
    )

    immediate = section(
        concurrency,
        "fn journal_operation_final_logical_owner_unlocks_accidental_fork_copy_immediately()",
        "fn journal_operation_deliberate_lifetime_export_remains_live_until_export_closes()",
    )
    require(
        "descriptor_availability(&path)" not in immediate,
        "protected immediate test no longer acquires a self-inheritable availability probe lock",
    )
    require(
        "wait_for_close_driven_lifecycle_release" not in immediate
        and "wait_for_close_driven_untyped_descriptor_release" not in immediate
        and "wait_for_recovery_reserved_after_deliberate_export_closes" not in immediate,
        "protected immediate test contains no retry/wait path",
    )
    drop_at = immediate.index("drop(lease);")
    retire_at = immediate.index("retire_descriptor_after_lifecycle_release(&path, &family)")
    release_child_at = immediate.index("libc::write(release[1]")
    require(
        drop_at < retire_at < release_child_at,
        "protected test performs no-retry lifecycle retirement after final logical-owner drop while deliberate fork copy is still open",
    )
    require(
        "local_persistent_lease_file(&path).is_none()" in immediate,
        "protected test separately records process-local logical-owner retirement without taking a kernel lock",
    )

    untyped_wait = section(
        concurrency,
        "fn wait_for_close_driven_untyped_descriptor_release(",
        "/// Classify a descriptor after the owning subsystem has durably established",
    )
    require(
        "#[cfg(all(test, unix))]" in concurrency[
            max(0, concurrency.index("fn wait_for_close_driven_untyped_descriptor_release(") - 64):
            concurrency.index("fn wait_for_close_driven_untyped_descriptor_release(")
        ],
        "malformed-descriptor quiescence helper is test-only and Unix-only",
    )
    require(
        "Duration::from_secs(5)" in untyped_wait
        and "try_lock_exclusive" in untyped_wait
        and "FileExt::unlock(&file)" in untyped_wait,
        "malformed-descriptor helper bounds inherited-probe waiting and explicitly unlocks its own successful probe",
    )

    truncated = section(
        concurrency,
        "fn truncated_durable_descriptor_routes_by_path_to_lifecycle_cleanup_only()",
        "fn zero_length_crash_orphan_is_reclaimed_by_next_admission()",
    )
    require(
        truncated.index("MutationClaimGuard::acquire_ephemeral(Vec::new()).unwrap_err()")
        < truncated.index("wait_for_close_driven_untyped_descriptor_release(")
        < truncated.index("retire_setup_orphan_by_path_identity("),
        "malformed-descriptor regression waits only after proving generic admission routes to lifecycle repair",
    )

    ffprobe_failure = section(
        bluray,
        "fn ffprobe_command_failure_reports_status_and_stderr()",
        "fn ffprobe_command_uses_injected_path_and_playlist_wide_audio_entries()",
    )
    require(
        "fixtures/test-bin/ffprobe-fails.sh" in ffprobe_failure
        and "std::fs::write" not in ffprobe_failure
        and "set_permissions" not in ffprobe_failure,
        "ffprobe failure regression executes a pre-existing fixture instead of write-then-exec racing parallel forks",
    )
    require(
        fixture.is_file()
        and fixture.stat().st_mode & 0o111
        and fixture.read_text().startswith("#!/bin/sh\n")
        and "synthetic ffprobe failure" in fixture.read_text()
        and "exit 42" in fixture.read_text(),
        "static ffprobe failure fixture is executable and preserves the intended nonzero-status plus stderr evidence",
    )

    print("R34 immediate-contract static verification passed")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (AssertionError, ValueError) as error:
        print(f"[FAIL] {error}")
        raise SystemExit(1)
