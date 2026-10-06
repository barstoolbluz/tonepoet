#!/usr/bin/env python3
"""Static acceptance checks for R32 parallel-fork inheritance hardening.

R32 must not change production coordination/supervisor semantics or serialize
libtest. It only makes tests whose contract depends on *final kernel close*
tolerate the short pre-exec interval in which an unrelated concurrent fork can
inherit a CLOEXEC descriptor.
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
    supervisor = read("src/convert/script_supervisor.rs")

    wait_helper = section(
        concurrency,
        "fn wait_for_recovery_reserved_after_deliberate_export_closes(",
        "#[test]\n    fn registry_contention_waits_for_holder_instead_of_timing_out()",
    )
    require(
        "descriptor_availability(path)" in wait_helper,
        "R32 wait observes the real descriptor availability classifier",
    )
    require(
        "ClaimAvailability::RecoveryReserved" in wait_helper,
        "R32 wait succeeds only at recovery-reserved durable authority",
    )
    require(
        "ClaimAvailability::Live" in wait_helper,
        "R32 wait retries the transient live classification",
    )
    require(
        "Duration::from_secs(5)" in wait_helper
        and "Duration::from_millis(2)" in wait_helper,
        "R32 deliberate-export grace is bounded and cheap on the normal path",
    )
    require(
        "Err(error) => panic!" in wait_helper,
        "R32 does not retry or mask descriptor I/O/classification errors",
    )
    require(
        "test_coordination_serial" not in wait_helper
        and "scoped_test_coordination_root" not in wait_helper,
        "R32 does not serialize the suite to hide fork inheritance",
    )

    deliberate = section(
        concurrency,
        "fn journal_operation_deliberate_lifetime_export_remains_live_until_export_closes()",
        "fn lifecycle_retirement_waits_out_accidental_cloexec_coholder_but_not_live_authority()",
    )
    require(
        deliberate.index("drop(exported);")
        < deliberate.index("wait_for_recovery_reserved_after_deliberate_export_closes("),
        "deliberate JournalOperation export waits only after its owned export closes",
    )
    require(
        "ClaimAvailability::Live" in deliberate.split("drop(exported);", 1)[0],
        "deliberate export remains synchronously live while the exported fd is owned",
    )

    coholder = section(
        concurrency,
        "fn same_process_journal_coholder_shares_deliberate_export_authority()",
        "fn synthetic_descriptor_claims(",
    )
    require(
        coholder.index("drop(exported);")
        < coholder.index("wait_for_recovery_reserved_after_deliberate_export_closes("),
        "same-OFD coholder regression waits only after deliberate export close",
    )
    require(
        "ClaimAvailability::Live" in coholder.split("drop(exported);", 1)[0],
        "same-OFD export still proves live authority before final close",
    )

    immediate = section(
        concurrency,
        "fn journal_operation_final_logical_owner_unlocks_accidental_fork_copy_immediately()",
        "fn journal_operation_deliberate_lifetime_export_remains_live_until_export_closes()",
    )
    require(
        "wait_for_recovery_reserved_after_deliberate_export_closes" not in immediate,
        "unexported JournalOperation immediate-unlock regression stays immediate",
    )
    require(
        "descriptor_availability(&path)" in immediate
        and "ClaimAvailability::RecoveryReserved" in immediate,
        "R32 preserves the immediate unexported logical-owner contract",
    )

    poll_control = section(supervisor, "fn poll_control(fd: RawFd)", "fn send_control(")
    require(
        "if count == 0" in poll_control and "CONTROL_PARENT_GONE" in poll_control,
        "production control-channel EOF remains parent-disconnect cancellation",
    )
    require(
        "Some(libc::EAGAIN) => Ok(None)" in poll_control,
        "production nonblocking EAGAIN remains a normal poll miss",
    )
    require(
        "sleep(" not in poll_control and "deadline" not in poll_control,
        "R32 adds no retry or latency to production poll_control",
    )

    eof_test = section(
        supervisor,
        "fn control_channel_eof_is_treated_as_cancellation()",
        "#[cfg(target_os = \"linux\")]\n    #[test]\n    fn linux_proc_stat_parser_handles_parentheses_in_comm()",
    )
    require(
        "Duration::from_secs(5)" in eof_test
        and "thread::sleep(CONTROL_POLL_INTERVAL)" in eof_test,
        "control EOF regression mirrors production polling with a bounded grace window",
    )
    require(
        "assert_eq!(control, CONTROL_PARENT_GONE)" in eof_test,
        "control EOF regression still requires the exact parent-gone signal",
    )
    require(
        "parallel-fork grace window" in eof_test,
        "control EOF regression fails loudly if a real missing-EOF defect persists",
    )

    require("#[ignore]" not in coholder and "#[ignore]" not in eof_test,
            "R32 does not ignore either residual flaky regression")

    print("R32 parallel-fork inheritance static verification passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
