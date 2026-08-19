#!/usr/bin/env python3
"""Static invariants for the round-5 concurrent-sessions corrective.

This verifier is intentionally conservative: it proves source-level ordering,
authority, and regression-test properties that should survive rebases. It does
not replace the required repeated Rust workspace gate.
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


def test_section(text: str, test_name: str) -> str:
    begin = text.index(f"fn {test_name}")
    next_test = text.find("\n    #[", begin + 3)
    if next_test < 0:
        next_test = len(text)
    return text[begin:next_test]


def main() -> int:
    concurrency = read("src/concurrency.rs")
    probe = read("src/tui/probe.rs")
    source_guard = read("crates/tui-file-picker/src/source_guard.rs")
    script_supervisor = read("src/convert/script_supervisor.rs")
    tool = read("src/convert/pipeline/tool.rs")
    streaming = read("src/convert/pipeline/progress/streaming.rs")
    archive = read("src/convert/pipeline/materializer_archive.rs")
    file_tasks = read("src/tui/file_task_runtime.rs")
    keybindings = read("src/tui/keybindings.rs")
    interchange = read("src/tui/tag_interchange.rs")

    # D1a: native FLAC authority retires at the native lock boundary, even if
    # an artwork rollback token deliberately outlives the committed write.
    release = section(
        probe,
        "pub(super) fn release_with_warning",
        "fn release_best_effort",
    )
    require_order(
        release,
        "std::fs::remove_file(&self.lock_path)",
        "self._mutation_claim.take()",
        "FLAC shared claim retires after native lock removal",
    )
    require_order(
        release,
        "post_commit_parent_sync_warning(&self.lock_path, context)",
        "self._mutation_claim.take()",
        "FLAC shared claim retires after native lock parent sync",
    )
    require_order(
        release,
        "self._mutation_claim.take()",
        "self.release_process_claim()",
        "FLAC process-local reservation retires after shared mutation authority",
    )
    best_effort = section(probe, "fn release_best_effort", "impl Drop for FlacWriteClaim")
    require_order(
        best_effort,
        "std::fs::remove_file(&self.lock_path)",
        "self._mutation_claim.take()",
        "best-effort FLAC shared claim retires after native lock removal",
    )
    require_order(
        best_effort,
        'sync_parent_dir(&self.lock_path, "FLAC common write lock removal")',
        "self._mutation_claim.take()",
        "best-effort FLAC shared claim retires after native lock parent sync",
    )
    require_order(
        best_effort,
        "self._mutation_claim.take()",
        "self.release_process_claim()",
        "best-effort FLAC process-local reservation retires after shared mutation authority",
    )
    for test_name in (
        "common_write_cleanup_keeps_process_reservation_until_shared_claim_retires",
        "common_write_drop_cleanup_keeps_process_reservation_until_shared_claim_retires",
    ):
        regression = test_section(probe, test_name)
        require(
            "assert_flac_cleanup_preserves_native_contention" in regression,
            f"{test_name} exercises synchronized FLAC cleanup contention",
        )
    contention_helper = section(
        probe,
        "fn assert_flac_cleanup_preserves_native_contention",
        "#[test]\n    fn common_write_cleanup_keeps_process_reservation_until_shared_claim_retires",
    )
    require(
        "recv_timeout" in contention_helper
        and "native_lock_removed" in contention_helper
        and "regression must suspend after native FLAC lock-file removal" in contention_helper
        and "filesystem mutation conflicts with live owner" in contention_helper
        and '!competing_err.contains("overlaps")' in contention_helper
        and "writer C should proceed after both FLAC cleanup authorities retire" in contention_helper,
        "FLAC cleanup regression proves native contention, excludes shared self-conflict, and proves eventual retirement",
    )

    # D1b: do not wrap native FLAC in a generic outer ephemeral claim. Its
    # native writer reserves the established process-local slot first, then
    # obtains shared authority, preserving the native contention contract.
    single_admission = section(
        probe,
        "fn with_single_metadata_path_admission",
        "/// Write a batch of tag changes",
    )
    require(
        "MetadataPersistenceRoute::NativeFlacVorbis" in single_admission
        and "PathResolutionSemantics::NamespaceObject" in single_admission
        and "admit_single_metadata_path(path, operation)" in single_admission
        and "admission.run(|| action(&admitted_path))" in single_admission,
        "single-path metadata admission uses native-FLAC ordering and generic fallback",
    )
    common_write = section(
        probe,
        "fn acquire_common_write_claim(",
        "fn acquire_common_write_claim_on_disk",
    )
    require_order(
        common_write,
        "lock_set.insert(canonical_path.clone())",
        "MutationClaimGuard::acquire_ephemeral",
        "native FLAC reserves local writer authority before shared mutation authority",
    )
    require(
        "current_mutation_authority_covers(&required_claim)" in common_write,
        "nested/scoped native FLAC mutations reuse existing exact authority",
    )

    # D2: the command timeout starts at UserCodeReleased rather than charging
    # secure containment setup. Cancellation remains effective at the pre-exec
    # ContainmentPrepared gate.
    direct = section(
        script_supervisor,
        "pub fn run_supervised<F, E>",
        "pub fn run_supervised_via_item_supervisor<F, E>",
    )
    require(
        "let mut user_code_started: Option<Instant> = None;" in direct
        and "ScriptLifecycleEvent::UserCodeReleased" in direct
        and "started.elapsed() >= invocation.timeout" in direct,
        "direct supervisor measures timeout from user-code release",
    )
    prepared_branch = section(
        direct,
        "ScriptLifecycleEvent::ContainmentPrepared",
        "event_parent.write_all(&[EVENT_ACK])",
    )
    require(
        "is_cancelled()" in prepared_branch
        and "CONTROL_CANCEL" in prepared_branch
        and "CONTROL_TIMEOUT" not in prepared_branch,
        "pre-exec gate preserves cancellation without pre-exec timeout",
    )
    item = section(
        script_supervisor,
        "pub fn run_supervised_via_item_supervisor<F, E>",
        "pub fn recover_supervised(",
    )
    require(
        "user_code_started" in item
        and "supervisor_started" in item
        and "LIFECYCLE_IO_TIMEOUT" in item,
        "persistent item supervisor separates runtime timeout from setup protocol bound",
    )

    # Reaping regressions use explicit readiness, never fixed startup sleeps.
    cancel_pipeline = test_section(
        tool,
        "pipeline_cancellation_terminates_and_reaps_both_stages",
    )
    require(
        "wait_for_child_pid_files" in tool
        and "wait_for_child_pid_files(&readiness_paths)" in cancel_pipeline
        and "assert_pid_reaped(&producer_pid_path)" in cancel_pipeline
        and "assert_pid_reaped(&consumer_pid_path)" in cancel_pipeline,
        "pipeline cancellation waits for user-code readiness then proves both children reaped",
    )
    producer_timeout = test_section(
        tool,
        "pipeline_producer_timeout_terminates_and_reaps_both_stages",
    )
    consumer_timeout = test_section(
        tool,
        "pipeline_consumer_timeout_terminates_and_reaps_both_stages",
    )
    require(
        "Duration::from_millis(500)" not in producer_timeout + consumer_timeout
        and producer_timeout.count("Duration::from_secs(2)") >= 1
        and consumer_timeout.count("Duration::from_secs(2)") >= 1,
        "pipeline timeout regressions leave scheduler headroom after user-code release",
    )
    consumer_nonzero = test_section(
        tool,
        "pipeline_reports_consumer_nonzero_and_closes_producer",
    )
    require(
        consumer_nonzero.count("Duration::from_secs(10)") >= 2
        and "ProcessExit::Code(9)" in consumer_nonzero,
        "SIGPIPE arbitration regression cannot be masked by a short test-only deadline",
    )

    # D3: local journal recovery remains an exact current-process OFD handoff,
    # never a generic same-PID bypass. Browse-parity direct-worker fixtures must
    # persist the same lifecycle handoff that the production supervisor writes.
    require(
        "!matches!(&descriptor.family, LeaseFamily::JournalOperation { .. })" in concurrency
        and "descriptor.owner != OwnerProcessIdentity::current()" in concurrency
        and "current_process_coheld_descriptor" in concurrency,
        "JournalOperation local handoff stays exact-family/current-process/descriptor-scoped",
    )
    permit = section(
        file_tasks,
        "fn permits_same_process_recovery_handoff",
        "#[derive(Debug, Clone)]\npub struct FileTaskJournalHandle",
    )
    require(
        "DurableFileTaskLifecycle::AwaitingReconciliation" in permit
        and "DurableFileTaskLifecycle::Running" not in permit
        and "DurableFileTaskLifecycle::Planned" not in permit,
        "live Planned/Running journals cannot use same-process recovery handoff",
    )
    browse_resume = section(
        keybindings,
        "pub(super) fn resume_test_clipboard_move_worker",
        "#[cfg(unix)]\n    #[test]",
    )
    require_order(
        browse_resume,
        "DurableFileTaskLifecycle::AwaitingReconciliation",
        "FileTaskJournalHandle::resume",
        "browse-parity fixture persists production handoff state before resume",
    )
    mappings = section(
        file_tasks,
        "pub fn mappings_for_reconciliation",
        "pub fn needs_reconciliation",
    )
    require(
        "self.logical_source_for_admitted(&artifact.original_source)" in mappings
        and "mapping.source == artifact.original_source" in mappings,
        "reconciliation retains journal-owned quarantine roots in admitted or logical spelling",
    )

    # D4: durable process identity—not registry membership—controls verified
    # removal recovery; streaming progress and archive errors stay observable.
    owner_recovery = section(
        source_guard,
        "fn owner_token_blocks_recovery",
        "fn legacy_recovery_journal_binding",
    )
    require(
        "owner_token_is_live(token)" in owner_recovery
        and "is_active_removal_journal" not in owner_recovery
        and "live_current_process_owner_defers_recovery_even_without_registry_entry" in source_guard,
        "verified-removal recovery defers for a live current process without a registry entry",
    )
    progress_test = test_section(
        streaming,
        "cancelled_stream_preserves_last_known_progress_percentage",
    )
    require(
        "tokio::sync::Notify" in progress_test
        and "progress_seen.notify_one()" in progress_test
        and "Cancelled at 37%" in progress_test
        and "Duration::from_millis(200)" not in progress_test,
        "cancelled-stream regression synchronizes on measured progress rather than wall-clock sleep",
    )
    direct_repackage = section(
        archive,
        "pub async fn repackage_archive_with_progress_and_cancel",
        "fn validate_repackage_staging_tree",
    )
    require_order(
        direct_repackage,
        "require_repackage_format_tool_available(format, tool_paths)?",
        "validate_repackage_staging_tree",
        "direct archive repackage reports missing creator before staging traversal",
    )

    # Flaky-cluster isolation: process-visible coordination test roots are
    # serialized before XDG/database overrides, and formerly unscoped target
    # regressions explicitly join that protocol.
    metadata_guard = section(
        probe,
        "struct IsolatedMetadataJournalHomeGuard",
        "fn native_ape_physical_text_values",
    )
    require_order(
        metadata_guard,
        "scoped_test_coordination_root()",
        "XdgConfigHomeGuard::new(prefix)",
        "metadata regression helper follows coordination-before-XDG lock order",
    )
    for name, text, test_name in (
        ("APE numbering", probe, "ape_numbering_capability_matches_production_round_trip"),
        ("MP4 alias numbering", probe, "mp4_numbering_alias_conflicts_fail_closed_and_equal_aliases_coalesce"),
        ("MP4 numbering pairs", probe, "mp4_numbering_pairs_round_trip_without_free_form_atoms"),
        ("prefixed FLAC", probe, "id3v23_prefixed_flac_native_in_place_and_overflow_writes_preserve_prefix_and_audio"),
        ("common write lock", probe, "active_common_write_lock_blocks_reads_and_competing_native_writes"),
        ("artwork common claim", probe, "active_artwork_common_claim_blocks_tag_write_from_another_thread"),
        ("tag transfer route", interchange, "transfer_route_is_positional_for_n_to_n_and_fails_n_to_m_before_io"),
    ):
        require(
            "scoped_test_coordination_root()" in test_section(text, test_name),
            f"{name} regression participates in coordination-root isolation",
        )
    require(
        "test_environment_lock()" in test_section(
            keybindings,
            "artwork_plus_opens_crate_picker_with_images_and_explicit_non_mutating_policy",
        ),
        "artwork-picker startup test serializes process-global file-task environment",
    )

    # The executable acceptance gate must retain the brief's dynamic bar.
    gate = read("scripts/validate_concurrency_round5.sh")
    require(
        "cargo test --workspace --no-fail-fast" in gate
        and "common_write_cleanup_keeps_process_reservation_until_shared_claim_retires" in gate
        and "common_write_drop_cleanup_keeps_process_reservation_until_shared_claim_retires" in gate
        and "seq 1 5" in gate
        and "seq 1 50" in gate
        and "--test-threads" not in gate
        and "RUST_MIN_STACK" in gate,
        "round-5 gate runs focused FLAC teardown regressions, five full default-parallel runs, and 50x SIGPIPE stress without stack mask",
    )

    print("round-5 static corrective assertions passed")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (AssertionError, ValueError) as error:
        print(f"[FAIL] {error}", file=sys.stderr)
        raise SystemExit(1)
