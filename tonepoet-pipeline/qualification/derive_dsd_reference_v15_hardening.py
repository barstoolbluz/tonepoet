#!/usr/bin/env python3
"""Deterministically verify append-only DSD Reference policy v15 hardening.

Historical-checker lineage contract: once shipped, this checker must remain valid
against every successor policy. It may pin immutable artifacts and persistent
policy identities from its own generation, but it must never assert the mutable
current-policy embed pointer.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
from pathlib import Path


FROZEN_V14 = {
    "derive_dsd_reference_v14_true_peak_analyzer.py": "873e6eabb3630345f36d1adb554b93e84886d9299d59dca0f170416b9a0d0d13",
    "dsd_reference_sox_ng_14_8_0_1_v14.json": "392aa682756bfdb882a77d4e262b85c1eb3db274d31b7862b053aa09c855adc1",
    "dsd_reference_sox_ng_14_8_0_1_v14_candidate.json": "392aa682756bfdb882a77d4e262b85c1eb3db274d31b7862b053aa09c855adc1",
    "dsd_reference_sox_ng_14_8_0_1_v14_certification.json": "f68fc3cd0d37f9c06184701706bc61ee059e55fd5b9e2d37e37cd4d3a05feae0",
    "dsd_reference_sox_ng_14_8_0_1_v14_report.md": "3f99f1cdaad1bd2bf7c7361ed63552a4be61f9e3380373eabf65750a13179ef8",
}


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def require(text: str, marker: str, label: str) -> None:
    if marker not in text:
        raise AssertionError(f"{label} omits required marker: {marker}")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    parser.parse_args()

    root = Path(__file__).resolve().parents[2]
    q = root / "tonepoet-pipeline" / "qualification"
    current_path = q / "dsd_reference_sox_ng_14_8_0_1_v15.json"
    candidate_path = q / "dsd_reference_sox_ng_14_8_0_1_v15_candidate.json"
    report_path = q / "dsd_reference_sox_ng_14_8_0_1_v15_report.md"
    certification_path = q / "dsd_reference_sox_ng_14_8_0_1_v15_certification.json"

    for name, expected in FROZEN_V14.items():
        actual = digest(q / name)
        if actual != expected:
            raise AssertionError(f"historical v14 artifact changed: {name}: {actual}")

    if current_path.read_bytes() != candidate_path.read_bytes():
        raise AssertionError("v15 current and candidate manifests are not byte-identical")
    manifest = json.loads(current_path.read_text())
    if manifest.get("schema_version") != 15:
        raise AssertionError("v15 schema version is noncanonical")
    if manifest.get("policy") != "sox_ng_14_8_0_1_v15":
        raise AssertionError("v15 policy identity is noncanonical")
    if manifest.get("status") != "qualification_candidate":
        raise AssertionError("v15 must remain an unpromoted qualification candidate")

    analyzer = manifest["analyzer"]
    if analyzer.get("qualification_schema") != "tonepoet-reference-analyzer-qualification/v6":
        raise AssertionError("v15 analyzer qualification schema is noncanonical")
    if analyzer.get("required_case_count") != 2168:
        raise AssertionError("v15 analyzer case count is noncanonical")
    if analyzer.get("adversarial_case_count") != 200:
        raise AssertionError("v15 adversarial case count is noncanonical")
    if analyzer.get("adversarial_oracle_oversample_factor") != 64:
        raise AssertionError("v15 adversarial oracle factor is noncanonical")
    expected_waveforms = [
        "single_tone",
        "fixed_frequency_single_tone",
        "phase_aligned_multitone",
        "impulse",
        "near_band_edge_burst",
        "alternating_sign",
        "broadband_deterministic",
        "boundary_transient",
    ]
    if analyzer.get("waveform_families") != expected_waveforms:
        raise AssertionError("v15 waveform family list is noncanonical")

    grid = -20.0 * math.log10(math.cos(math.pi / 32.0))
    grid_rounded_up = math.ceil(grid * 1_000_000_000.0) / 1_000_000_000.0
    if f"{grid_rounded_up:.9f}" != "0.041925957":
        raise AssertionError("analytic 16x grid derivation changed")
    residual = analyzer["residual_authority"]
    expected_residual = {
        "schema": "tonepoet-reference-analyzer-residual-authority/v1",
        "ideal_grid_component_db": "0.041925957",
        "pinned_resampler_component_limit_db": "0.058074043",
        "reporting_quantization_component_db": "0.010000000",
        "analyzer_residual_sum_db": "0.100000000",
        "one_sided_total_db": "0.110000000",
        "resampler_authority_method": "pinned_sox_ng_14_8_0_1_empirical_matrix_with_64x_adversarial_oracle",
        "status": "requires_pinned_real_tool_qualification",
    }
    if residual != expected_residual:
        raise AssertionError("v15 residual authority is noncanonical")
    if round(float(residual["ideal_grid_component_db"]) + float(residual["pinned_resampler_component_limit_db"]), 9) != 0.1:
        raise AssertionError("v15 analyzer residual components do not sum to 0.100000000 dB")
    if round(float(residual["analyzer_residual_sum_db"]) + float(residual["reporting_quantization_component_db"]), 9) != 0.11:
        raise AssertionError("v15 total one-sided authority does not sum to 0.110000000 dB")

    deadline = analyzer["deadline_model"]
    expected_deadline = {
        "schema": "tonepoet-reference-analyzer-deadline/v1",
        "startup_seconds": 120,
        "minimum_oversampled_sample_values_per_second": 1_000_000,
        "duration_guard_frames": 1,
        "workload_rule": "(ceil(duration_ns * sample_rate_hz / 1000000000) + duration_guard_frames) * channels * oversample_factor",
        "deadline_rule": "startup_seconds + ceil(workload_sample_values / minimum_oversampled_sample_values_per_second)",
        "max_admitted_workload_sample_values": 8_589_934_480,
        "max_deadline_seconds": 8_710,
        "required_benchmark": "pinned_toolchain_throughput_floor_and_maximum_admission_arithmetic",
    }
    if deadline != expected_deadline:
        raise AssertionError("v15 workload deadline model is noncanonical")
    if deadline["startup_seconds"] + math.ceil(deadline["max_admitted_workload_sample_values"] / deadline["minimum_oversampled_sample_values_per_second"]) != deadline["max_deadline_seconds"]:
        raise AssertionError("v15 maximum deadline arithmetic is inconsistent")

    carrier = analyzer["carrier"]
    if carrier.get("oversample_factor") != 16:
        raise AssertionError("v15 carrier oversampling factor changed")
    if carrier.get("analytic_grid_bound_db") != "0.041925957":
        raise AssertionError("v15 carrier grid bound changed")
    if carrier.get("overflow_behavior") != "not_applicable_to_v15_analyzer":
        raise AssertionError("v15 carrier overflow statement is noncanonical")

    qualification_report = manifest["qualification_report"]
    if qualification_report.get("path") != "tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v15_report.md":
        raise AssertionError("v15 qualification report path is noncanonical")
    if qualification_report.get("sha256") != digest(report_path):
        raise AssertionError("v15 qualification report digest does not match its bytes")
    release = manifest["release_certification"]
    expected_release = {
        "schema": "tonepoet-dsd-reference-release-certification/v1",
        "path": "tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v15_certification.json",
        "candidate_manifest_path": "tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v15_candidate.json",
        "report_sha256": None,
        "candidate_manifest_sha256": None,
    }
    if release != expected_release:
        raise AssertionError("v15 release certification descriptor is noncanonical")
    certification = json.loads(certification_path.read_text())
    if certification.get("schema_version") != 15 or certification.get("policy") != "sox_ng_14_8_0_1_v15":
        raise AssertionError("v15 certification stub identity is noncanonical")
    if certification.get("status") != "not_run" or certification.get("outcome") != "not_run":
        raise AssertionError("v15 certification stub must remain not_run")

    planner = (root / "tonepoet-pipeline" / "src" / "dsd_reference.rs").read_text()
    executor = (root / "src" / "convert" / "pipeline" / "track_executor.rs").read_text()
    qualification = (root / "tests" / "dsd_reference_qualification.rs").read_text()
    manifest_source = (root / "src" / "convert" / "pipeline" / "manifest.rs").read_text()
    settings = (root / "tonepoet-pipeline" / "src" / "settings.rs").read_text()
    sentinel = (root / "tests" / "dsd_reference_settings_sentinel.rs").read_text()

    for marker in [
        'pub const DSD_REFERENCE_POLICY_V15_KEY: &str = "sox_ng_14_8_0_1_v15";',
        "reference_true_peak_measurement_deadline(",
        "REFERENCE_TRUE_PEAK_MAX_DEADLINE_SECONDS: u64 = 8_710",
        "REFERENCE_TRUE_PEAK_RESAMPLER_COMPONENT_LIMIT: DbNano = DbNano(58_074_043)",
        "REFERENCE_TRUE_PEAK_ANALYZER_RESIDUAL: DbNano = DbNano(100_000_000)",
        "REFERENCE_TRUE_PEAK_ONE_SIDED_AUTHORITY: DbNano = DbNano(110_000_000)",
        "pub analyzer_deadline: std::time::Duration",
        "deadline_identity=workload/v1",
        "normalize_step_for_hash_v15",
        "Some(expected_duration)",
        "SoxNg14801V15",
        "build_reference_silence_scan_command(",
        "ReferenceDecodeMechanism::SoxFloat64W64RawStream",
        "reference_silence_scan_obeys_the_decode_route_table",
    ]:
        require(planner, marker, "planner")
    pipeline_sites = [index for index in range(len(executor)) if executor.startswith(".run_pipeline(", index)]
    if len(pipeline_sites) != 3:
        raise AssertionError(f"expected exactly three production Reference pipeline sites, found {len(pipeline_sites)}")
    for index in pipeline_sites:
        if "acquire_reference_pipeline_permits(" not in executor[max(0, index - 1_200):index]:
            raise AssertionError("a production Reference pipeline bypasses composite permit acquisition")
    if "acquire_reference_pipeline_permit(" in executor:
        raise AssertionError("the obsolete producer-order single-family helper remains reachable")

    for marker in [
        "enum ToolConcurrencyFamily",
        "Sox,\n    Ffmpeg,\n    Ssrc",
        "collect::<BTreeSet<_>>()",
        "acquire_reference_pipeline_permits(",
        "ReferencePipelinePermitSet",
        "reference_pipeline_composite_permits_prevent_opposite_direction_deadlock",
        "reference_pipeline_composite_permits_deduplicate_and_release_partial_acquisition",
        "tokio::sync::Barrier::new(3)",
        "producer.expected_duration != measurement.command.expected_duration",
        "producer is bound to the wrong carrier path",
        "crossed Float32 producer contract was accepted for {target:?}",
        "silent_w64_header_finalization_defect_valid",
        "sox_writer_defect_reproduced_and_bounded",
        "all_w64_structure_probes_use_sox_info",
        'silent_w64_u64("file_bytes") == Some(70_696)',
        'silent_w64_u64("silence_riff_size_field") == Some(136)',
        'silent_w64_u64("silence_data_chunk_size_field") == Some(24)',
        'silent_w64_bool("direct_ffmpeg_tiny_nonzero_opened") == Some(true)',
        "correctly_refuses_declared_empty_w64_payload",
        "float64_w64_open_and_silence_proof_use_qualified_sox_route",
        "command.expected_duration == Some(summary.analyzer_deadline)",
        "REFERENCE_TRUE_PEAK_MAX_DEADLINE_SECONDS",
        "dsd_reference_sox_ng_14_8_0_1_v15.json",
        "manifest.schema_version != 15",
    ]:
        require(executor, marker, "executor")
    for marker in [
        "AdversarialAnalyzerFixture",
        "NearBandEdgeBurst",
        "BoundaryTransient",
        "measurement_with_oversample_factor",
        "adversarial_oracle_oversample_factor",
        "maximum_empirical_resampler_component_db",
        "qualify_analyzer_deadline_model",
        "probe_direct_ffmpeg_f64_w64",
        "inspect_w64_header",
        "assert_exact_w64_package_probe",
        '"silent_w64_header_finalization_defect"',
        '"all_zero_content_not_threshold_or_first_block_silence"',
        "tiny_nonzero_samples[sample_frames / 2] = 2_f64.powi(-24);",
        "leading_silence_samples[sample_frames / 2..]",
        "trailing_silence_samples[..sample_frames / 2]",
        "assert_eq!(silence_header.data_chunk_offset, 112);",
        "sox_reported_sample_frames",
        '"schema_version": 15',
    ]:
        require(qualification, marker, "qualification")
    obsolete = "silent_float64_w64_open_defect"
    if obsolete in executor or obsolete in qualification:
        raise AssertionError("obsolete consumer-side silent-W64 defect evidence remains")

    require(manifest_source, "SoxNg14801V15", "execution manifest")
    require(settings, "SoxNg14801V15", "settings")
    require(sentinel, "SoxNg14801V15", "settings sentinel")

    print("policy v15 analyzer hardening derivation verified")


if __name__ == "__main__":
    main()
