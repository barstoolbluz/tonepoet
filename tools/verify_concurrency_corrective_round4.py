#!/usr/bin/env python3
"""Static regression assertions for the round-4 concurrency corrective.

This is intentionally a source-level companion to, not a replacement for, the
Rust workspace gate. It locks down the safety mechanisms that are easiest to
accidentally regress while this patch is reviewed or rebased.
"""

from __future__ import annotations

from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[1]


def read(relative: str) -> str:
    return (ROOT / relative).read_text(encoding="utf-8")


def ok(label: str) -> None:
    print(f"[ok] {label}")


def require(condition: bool, label: str) -> None:
    if not condition:
        raise AssertionError(label)
    ok(label)


def require_order(text: str, earlier: str, later: str, label: str) -> None:
    left = text.find(earlier)
    right = text.find(later)
    require(left >= 0 and right >= 0 and left < right, label)


def section(text: str, start: str, end: str) -> str:
    begin = text.index(start)
    finish = text.index(end, begin)
    return text[begin:finish]


def main() -> int:
    concurrency = read("src/concurrency.rs")
    db = read("src/db.rs")
    file_tasks = read("src/tui/file_task_runtime.rs")
    app = read("src/tui/app.rs")
    recent = read("src/tui/recent_files.rs")
    tool = read("src/convert/pipeline/tool.rs")
    stages = read("src/convert/pipeline/stages.rs")
    streaming = read("src/convert/pipeline/progress/streaming.rs")
    probe = read("src/tui/probe.rs")
    source_guard = read("crates/tui-file-picker/src/source_guard.rs")
    script_supervisor = read("src/convert/script_supervisor.rs")

    # R1: ordinary quiescent v23 -> v24 activation must not depend on an env
    # acknowledgement, but must retain the live-signal gate and serialized lock.
    rust_sources = "\n".join(
        path.read_text(encoding="utf-8")
        for root in (ROOT / "src", ROOT / "crates")
        for path in root.rglob("*.rs")
    )
    require(
        "TONEPOET_CONFIRM_V24_UPGRADE" not in rust_sources,
        "v24 activation/adoption has no environment-confirmation dependency",
    )
    activation = section(db, "fn require_v24_activation_confirmation", "fn run_migration_step")
    require(
        activation.count("observable_legacy_live_signals()?") >= 2,
        "v24 activation keeps pre-boundary and final live-signal probes",
    )
    require(
        "acquire_v24_upgrade_lock" in db and "initial_version == 23" in db,
        "v23 activation remains serialized by the upgrade lock",
    )
    require(
        "quiescent_v23_startup_activates_v24_without_in_memory_fallback_or_env_gate" in app
        and "assert_eq!(version, 24" in app,
        "quiescent v23 activation regression exists",
    )
    require(
        'peer_signals = crate::db::observable_tonepoet_peer_processes()' in file_tasks,
        "legacy journal adoption still refuses observable peer sessions",
    )

    # R2: normal recovery stays strict. Only file-task recovery with a durable
    # handoff state can co-hold an exact process-local descriptor; foreign
    # owners remain live and generic admission never uses this escape hatch.
    recovery_impl = section(
        concurrency,
        "pub fn acquire_existing_recovery(",
        "pub fn acquire_existing(path:",
    )
    require(
        "acquire_existing_recovery_internal(path, expected_family, false)" in recovery_impl,
        "ordinary persistent-lease recovery remains strict",
    )
    require(
        "acquire_existing_recovery_internal(path, expected_family, true)" in recovery_impl
        and "current_process_coheld_descriptor" in recovery_impl,
        "explicit local recovery handoff reuses only the exact local descriptor",
    )
    require(
        "descriptor.owner != OwnerProcessIdentity::current()" in concurrency,
        "foreign descriptor owners cannot enter local co-hold recovery",
    )
    require(
        "!matches!(&descriptor.family, LeaseFamily::JournalOperation { .. })" in concurrency,
        "same-process co-hold is restricted to file-task JournalOperation leases",
    )
    require(
        "std::sync::Weak<File>" in concurrency
        and "unregister_local_persistent_lease" in concurrency
        and "impl Drop for PersistentLease" in concurrency,
        "process-local descriptor index is weak and pruned on final holder drop",
    )
    require(
        "permits_same_process_recovery_handoff" in file_tasks
        and "DurableFileTaskLifecycle::AwaitingReconciliation" in file_tasks
        and "descriptor_recovery_availability_with_local_handoff" in file_tasks,
        "file-task discovery gates local handoff on durable recovery lifecycle",
    )
    require(
        "same_process_recovery_coholds_exact_descriptor_without_weakening_strict_acquire" in concurrency,
        "strict-vs-local recovery regression exists",
    )

    # R3: PATH discovery and executable identity are deliberately separate.
    require(
        "fn resolve_executable_launch_path" in tool
        and "fn resolve_executable_path" in tool,
        "tool launch spelling is distinct from canonical executable identity",
    )
    resolver = section(tool, "pub(crate) fn resolve_command_launch_path", "impl RealToolRunner")
    require(
        "resolve_executable_launch_path(&candidate)" in resolver
        and "resolve_executable_path(&candidate)" not in resolver,
        "bare command launch preserves PATH-selected applet spelling",
    )
    supervised = section(tool, "async fn run_supervised_with_stdio", "pub(crate) async fn run_with_binary_path")
    require(
        "let launch_path = binary_path;" in supervised
        and "let reviewed_path = std::fs::canonicalize(&launch_path)" in supervised
        and "script_file: Arc::new(binary_file)" in supervised
        and "script: launch_path" in supervised,
        "supervision executes the reviewed inode while preserving argv[0] spelling",
    )
    tool_impl = tool.index("impl ToolRunner for RealToolRunner")
    pipeline_begin = tool.index("async fn run_pipeline(", tool_impl)
    pipeline_end = tool.index("#[cfg(not(unix))]", pipeline_begin)
    pipeline = tool[pipeline_begin:pipeline_end]
    require(
        "let pipeline_cancel = CancellationToken::new();" in pipeline,
        "pipeline owns a peer-cancellation token",
    )
    require_order(
        pipeline,
        "result = &mut producer_future => FirstStageResult::Producer(result)",
        "result = &mut consumer_future => FirstStageResult::Consumer(result)",
        "pipeline polls producer before consumer for deterministic spawn-failure reaping",
    )
    require(
        pipeline.count("pipeline_cancel.cancel();") >= 3
        and "producer_future.await" in pipeline
        and "consumer_future.await" in pipeline,
        "pipeline failures/cancellation synchronously reap the peer stage",
    )
    sigpipe_policy = section(
        tool,
        "fn consumer_failure_makes_producer_sigpipe_secondary(",
        "/// Maximum stdout/stderr tail any runner retains.",
    )
    require(
        "if *signal == libc::SIGPIPE" in sigpipe_policy
        and "ToolRunnerError::NonZeroExit { .. }" in sigpipe_policy
        and "ToolRunnerError::Cancelled" not in sigpipe_policy,
        "upstream SIGPIPE is secondary only to a substantive downstream failure",
    )
    require(
        "consumer_failure_makes_producer_sigpipe_secondary(" in pipeline
        and "let consumer_is_primary = prefer_consumer_error" in pipeline,
        "pipeline arbitration promotes downstream failure over secondary upstream SIGPIPE",
    )
    require(
        "producer_sigpipe_is_secondary_only_to_substantive_consumer_failure" in tool
        and "pipeline_reports_consumer_nonzero_and_closes_producer" in tool
        and "pipeline_reports_producer_nonzero_after_reaping_consumer" in tool,
        "SIGPIPE arbitration has deterministic policy and end-to-end regressions",
    )

    launcher = section(script_supervisor, "fn spawn_launcher(", "fn wait_launcher_ready")
    require(
        ".stdin(Stdio::inherit())" in launcher
        and ".stdin(Stdio::null())" not in launcher,
        "containment launcher preserves the supervisor-selected pipeline stdin",
    )

    # R4: keep the schema-v1 string wire for ordinary UTF-8 paths while adding
    # lossless platform-native representations for paths that cannot be UTF-8.
    require(
        "mod lossless_path_serde" in concurrency
        and "UnixBytes { unix_bytes:" in concurrency
        and "WindowsWide { windows_wide:" in concurrency,
        "persistent path identities have lossless platform-native serde",
    )
    require(
        "path_claim_json_round_trips_non_utf8_losslessly_and_keeps_utf8_wire_compatible" in concurrency
        and 'utf8_json["identity"]["original"].is_string()' in concurrency,
        "non-UTF round-trip and UTF-8 wire-compatibility regression exists",
    )

    # R5: reserve the old process-local writer contract before shared admission,
    # and make all failed shared admissions release that reservation.
    common_write = section(probe, "fn acquire_common_write_claim(", "fn acquire_common_write_claim_on_disk")
    require_order(
        common_write,
        "lock_set.insert(canonical_path.clone())",
        "current_mutation_authority_covers(&required_claim)",
        "native FLAC same-process writer slot is reserved before shared claim admission",
    )
    require(
        common_write.count("lock_set.remove(&canonical_path)") >= 3,
        "native FLAC writer reservation rolls back on every admission failure path",
    )

    # R6: heap-indirect the large CUE/conversion future chain at public and
    # high-cardinality fan-out boundaries, without introducing a stack mask.
    for anchor, label in (
        ("Box::pin(run_pipeline_item_with_tool_paths(", "public pipeline wrapper is boxed"),
        ("Box::pin(run_pipeline_item_with_tool_paths_and_tool_limits(", "tool-limit wrapper is boxed"),
        ("Box::pin(run_pipeline_item_with_tool_paths_and_tool_limits_scoped_inner(", "scoped pipeline future is boxed"),
        ("Box::pin(run_pipeline_item_with_tool_paths_and_tool_limits_once(", "pipeline retry future is boxed"),
        ("Box::pin(convert_tracks_with_reporter_with_tool_paths(", "convert-stage fan-out future is boxed"),
        ("Box::pin(convert_one_track_work(", "per-track future is boxed"),
        ("Box::pin(realize_track_with_tool_limits_and_stats(", "CUE realization future is boxed"),
        ("Box::pin(execute_planned_track_conversion(", "track execution future is boxed"),
    ):
        require(anchor in stages, label)
    require("RUST_MIN_STACK" not in rust_sources, "Rust source has no stack-size mask")

    # R7: preserve user-visible/native error contracts and explicit local
    # recovery handoff semantics in the source-guard crate.
    require(
        'map_err(|error| format!("queue load prepare: {error}"))' in db,
        "queue whole-query failure keeps its prepare-stage contract",
    )
    cancelled = section(
        streaming,
        "Err(ToolRunnerError::Cancelled { mut command })",
        "Err(ToolRunnerError::Termination",
    )
    require_order(
        cancelled,
        "cancel_requested_for_tool",
        "cancelled_at_last_progress",
        "stream cancellation emits tool-specific stopping status before final progress",
    )
    require(
        "fn owner_token_blocks_recovery" in source_guard
        and "is_active_removal_journal(journal_path)" in source_guard
        and source_guard.count("owner_token_blocks_recovery(") >= 4,
        "verified-removal recovery uses explicit local registration and foreign owner tokens",
    )
    require(
        "pragma_update(None, \"user_version\", 24)" in recent,
        "current-version malformed recent-files fixture targets v24 rather than migration",
    )

    print("round-4 static corrective assertions passed")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (AssertionError, ValueError) as error:
        print(f"[FAIL] {error}", file=sys.stderr)
        raise SystemExit(1)
