#!/usr/bin/env python3
"""Target-free source invariants for the R4 residual corrective.

This is intentionally a source-level guard, not a substitute for Rust tests,
full gates, real-closure staging, or staged Reference qualification on the
build host.
"""
from __future__ import annotations

from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[1]


def read(path: str) -> str:
    return (ROOT / path).read_text(encoding="utf-8")


def section(text: str, start: str, end: str) -> str:
    i = text.index(start)
    j = text.index(end, i)
    return text[i:j]


def require(condition: bool, label: str) -> None:
    if not condition:
        raise AssertionError(label)
    print(f"[ok] {label}")


def require_order(text: str, first: str, second: str, label: str) -> None:
    a = text.index(first)
    b = text.index(second, a)
    require(a < b, label)


def main() -> int:
    executor = read("src/convert/pipeline/track_executor.rs")
    stages = read("src/convert/pipeline/stages.rs")
    processor = read("src/convert/processor.rs")
    concurrency = read("src/concurrency.rs")
    stager = read("tools/stage_reference_runtime_closure.py")
    staging_tests = read("tools/test_stage_reference_runtime_closure.py")
    timing = read("crates/tonepoet-true-peak/examples/loudness_simd_timing.rs")

    realization = section(
        executor,
        "fn qualified_ffmpeg_int32_execution_realization(",
        "fn qualified_ffmpeg_int32_dither_terminal_binding(",
    )
    require(
        "TrackSourceRef::PcmTruePeakCarrier" not in realization,
        "R4-1 final execution realization no longer keys on a source-ref enum variant",
    )
    for token, label in (
        ("pre_observation_rate_edge_consumed", "requires explicit consumed-rate execution state"),
        ("charged_rate_hz != carrier_rate_hz", "requires charged target to equal measured carrier rate"),
        ("plan_request.source.sample_rate_hz != Some(carrier_rate_hz)", "requires final planner source rate to equal measured carrier rate"),
        ("target_sample_rate", "requires final planner target to preserve the measured carrier rate"),
        ("plan_typed(plan_request)", "rebuilds the final typed plan"),
        ("PlanOperation::ResamplePcm", "refuses a surviving final typed resample"),
        ("unique_planned_arg_value(&terminal.args, \"-ar\")", "checks physical FFmpeg raw-input rate"),
        ("execution_realization.target_rate_hz = None", "consumes the rate edge only in the private execution realization"),
    ):
        require(token in realization, f"R4-1 {label}")

    require(
        realization.index("charged_rate_hz != carrier_rate_hz")
        < realization.index("if !execution_state.pre_observation_rate_edge_consumed"),
        "R4-1 charged/carrier disagreement is refused before consumed-edge authorization",
    )

    real_flow = section(
        executor,
        "async fn album_pcm_true_peak_rate_change_real_preparation_gain_and_terminal_path()",
        "fn album_pcm_true_peak_rate_change_consumes_charged_rate_before_final_terminal_shape_check()",
    )
    for token, label in (
        ("prepare_pcm_true_peak_carrier_for_regression", "crosses production PCM carrier preparation"),
        ("resolve_pcm_true_peak_submission_albums", "crosses the album gain barrier"),
        ("qualified_ffmpeg_int32_dither_terminal_binding", "reaches the production qualified terminal binding"),
        ("192_000", "uses the reported 192 kHz source"),
        ("176_400", "uses the reported 176.4 kHz target"),
        ("AudioFormat::Flac", "uses the reported FLAC target"),
        ("DitherType::Tpdf", "uses the reported TPDF policy"),
    ):
        require(token in real_flow, f"R4-1 regression {label}")

    pending = section(processor, "struct PendingAlbum {", "struct PendingDsdAlbumGainSubmission")
    require("causal_failure: Option<ScheduledAlbumFailureCause>" in pending, "R4-2 scheduler stores the initiating failure separately")
    require("collateral_cancellations: BTreeSet<TrackId>" in pending, "R4-2 scheduler tracks cancellation fallout separately")
    cancellation_classifier = section(
        processor,
        "fn scheduler_cancellation_failure(",
        "/// Update scheduler-only failure provenance",
    )
    require(
        'const PREFIX: &str = "PCM true-peak measurement failed for track ";'
        in cancellation_classifier,
        "R4-2 recognizes the production PCM true-peak cancellation wrapper",
    )
    require(
        'const LEAF: &str = "PCM true-peak scan cancelled";'
        in cancellation_classifier,
        "R4-2 wrapped cancellation matching is pinned to the exact scanner leaf",
    )
    require(
        "ordinal.parse::<u32>() == Ok(source_ordinal)" in cancellation_classifier,
        "R4-2 wrapped cancellation must name the same track as the failed output",
    )
    require(
        'contains("cancelled")' not in cancellation_classifier,
        "R4-2 cancellation classification does not use a broad substring match",
    )
    encoded = section(processor, "Ok(QueueWorkOutput::Encoded { job_id, output }) =>", "Ok(QueueWorkOutput::EncodedBatch")
    require_order(encoded, "record_scheduler_failure_provenance(", "pending.job_cancel.cancel();", "R4-2 causal provenance is captured before fail-fast cancellation")
    summary = section(stages, "fn convert_stage_failure_message_with_context(", "fn convert_stage_failure_message(records")
    require("!context.collateral_cancellations.contains(&record.track_id)" in summary, "R4-2 collateral cancellations cannot compete as peer failures")
    require("context.cause.error.clone()" in summary, "R4-2 initiating actionable error remains the headline")
    require("causal_album_failure_outranks_lower_index_collateral_cancellation" in stages, "R4-2 lower-index cancellation regression exists")
    require("causal_album_failure_keeps_independent_second_failure_but_not_cancellations" in stages, "R4-2 independent-second-failure regression exists")
    require(
        "PCM true-peak measurement failed for track 1: PCM true-peak scan cancelled"
        in processor,
        "R4-2 scheduler regression uses the real wrapped true-peak cancellation text",
    )
    require(
        "PCM true-peak measurement failed for track 1: PCM true-peak scan cancelled"
        in stages,
        "R4-2 failure-summary regression uses the real wrapped true-peak cancellation text",
    )

    shared = section(concurrency, "struct PersistentLeaseSharedState {", "pub struct PersistentLease {")
    require("logical_owners" in shared and "lifetime_file_exported" in shared, "R4-3 same-OFD logical ownership and export state are shared")
    drop_impl = section(concurrency, "impl Drop for PersistentLease {", "#[derive(Debug)]\npub struct MutationClaimGuard")
    require("LeaseFamily::JournalOperation" in drop_impl, "R4-3 explicit logical unlock is JournalOperation-only")
    require("release_local_persistent_lease_owner" in drop_impl, "R4-3 unlock waits for the final in-process logical owner")
    require("FileExt::unlock" in drop_impl, "R4-3 final unexported JournalOperation explicitly releases flock")
    retirement = section(concurrency, "fn acquire_existing_for_lifecycle_retirement(", "fn acquire_existing_with_transient_contention(")
    require("LeaseFamily::ExecutionStaging" in retirement and "JournalOperation" not in retirement, "R4-3 JournalOperation is not fixed by extending retry")
    for test_name in (
        "journal_operation_final_logical_owner_unlocks_accidental_fork_copy_immediately",
        "journal_operation_deliberate_lifetime_export_remains_live_until_export_closes",
        "same_process_journal_coholder_shares_deliberate_export_authority",
    ):
        require(test_name in concurrency, f"R4-3 regression exists: {test_name}")

    require('store_object.endswith("-bluez-5.84")' in stager, "R4-4 exemption is pinned to the observed BlueZ 5.84 store output")
    require('BLUEZ_HOST_CONFIG_LINKS = frozenset({"input.conf", "main.conf", "network.conf"})' in stager, "R4-4 only the three observed BlueZ config names are eligible")
    require('target_text == f"/etc/bluetooth/{name}"' in stager, "R4-4 each omission requires its exact expected host target")
    require('"external_absolute_symlinks_omitted": omitted_external_symlinks' in stager, "R4-4 omissions are recorded in staging metadata")
    require("raise StageError(f\"absolute symlink points outside Nix store" in stager, "R4-4 every unrecognized external absolute symlink remains fail-closed")
    for test_name in (
        "test_bluez_host_config_symlinks_are_omitted_and_recorded",
        "test_bluez_policy_is_pinned_to_the_observed_5_84_output",
        "test_bluez_policy_does_not_admit_unknown_external_config",
        "test_absolute_symlink_is_rewritten_inside_private_store",
    ):
        require(test_name in staging_tests, f"R4-4 regression exists: {test_name}")

    require(
        "4 => vec![Left, Right, LeftSurround, RightSurround]" in timing,
        "R4-5 four-channel AVX qualification geometry remains L/R/Ls/Rs",
    )

    print("R4 residual corrective target-free static invariants passed")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (AssertionError, ValueError) as error:
        print(f"[FAIL] {error}", file=sys.stderr)
        raise SystemExit(1)
