#!/usr/bin/env python3
"""Verify append-only policy-v12 bounded streamed-WAV authority."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path

V11_HASHES = {
    "derive_dsd_reference_v11_runtime_mutator_binding.py": "26c096bd718e13c2bfa45006e6009cfcc494ada5f9a8ba7065e1c88a090f8264",
    "dsd_reference_sox_ng_14_8_0_1_v11.json": "8af10fe4eb028b203bdef5472fc3270d31fa24906808f9d115a695e7bf3dce0e",
    "dsd_reference_sox_ng_14_8_0_1_v11_candidate.json": "8af10fe4eb028b203bdef5472fc3270d31fa24906808f9d115a695e7bf3dce0e",
    "dsd_reference_sox_ng_14_8_0_1_v11_certification.json": "c356cf43b6d93bb9b4c6e3a6ee61e29612caef119cc8da0ed4226659f75bc893",
    "dsd_reference_sox_ng_14_8_0_1_v11_report.md": "6e0dba083fced7a70ed9dbf86de00325fbf1e73f0771be613d4d5e067988c135",
}

CAPACITY = {
    "schema": "tonepoet-reference-streamed-wav-capacity/v1",
    "applies_to": "all_reference_float64_wav_streams",
    "riff_size_field_max": 4_294_967_295,
    "riff_size_overhead_bytes": 58,
    "max_audio_payload_bytes": 4_294_967_237,
    "sample_encoding": "pcm_f64le",
    "bytes_per_sample": 8,
    "duration_guard_frames": 1,
    "admission_rule": "(ceil(duration_ns * target_rate_hz / 1000000000) + duration_guard_frames) * channels * bytes_per_sample <= max_audio_payload_bytes",
    "overflow_behavior": "sox_ng_unseekable_wav_overflow_riff_size_58_data_size_modulo_2^32",
    "overflow_error_code": "DSD-REF-P0-025",
    "future_lift": "append_only_policy_with_corrected_sox_ng_pin_or_independently_qualified_transport",
}


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def require(text: str, marker: str, label: str) -> None:
    if marker not in text:
        raise AssertionError(f"{label}: missing {marker!r}")


def forbid(text: str, marker: str, label: str) -> None:
    if marker in text:
        raise AssertionError(f"{label}: forbidden stale marker {marker!r}")


def verify(root: Path) -> None:
    q = root / "tonepoet-pipeline/qualification"
    for name, expected in V11_HASHES.items():
        actual = sha256(q / name)
        if actual != expected:
            raise AssertionError(f"append-only v11 artifact changed: {name}: {actual}")

    v11 = json.loads((q / "dsd_reference_sox_ng_14_8_0_1_v11.json").read_text())
    current_path = q / "dsd_reference_sox_ng_14_8_0_1_v12.json"
    candidate_path = q / "dsd_reference_sox_ng_14_8_0_1_v12_candidate.json"
    current_bytes = current_path.read_bytes()
    candidate_bytes = candidate_path.read_bytes()
    if current_bytes != candidate_bytes:
        raise AssertionError("v12 current and preserved candidate are not byte-identical")
    v12 = json.loads(current_bytes)
    if v12.get("schema_version") != 12 or v12.get("policy") != "sox_ng_14_8_0_1_v12":
        raise AssertionError("v12 schema/policy identity is noncanonical")
    if v12.get("status") != "qualification_candidate":
        raise AssertionError("v12 must remain an unpromoted candidate")

    changed = {
        "schema_version",
        "policy",
        "qualification_basis",
        "runtime_activation",
        "qualification_report",
        "release_certification",
        "analyzer",
        "streamed_wav_capacity",
    }
    for key in sorted(set(v11) | set(v12)):
        if key not in changed and v11.get(key) != v12.get(key):
            raise AssertionError(f"v12 changed inherited v11 field {key!r}")

    if v12.get("streamed_wav_capacity") != CAPACITY:
        raise AssertionError("v12 streamed-WAV capacity contract is noncanonical")
    if CAPACITY["riff_size_field_max"] - CAPACITY["riff_size_overhead_bytes"] != CAPACITY["max_audio_payload_bytes"]:
        raise AssertionError("streamed-WAV capacity arithmetic is inconsistent")
    overflow_payload = (1 << 32) + 8
    if overflow_payload & 0xFFFF_FFFF != 8:
        raise AssertionError("F6 data modulo fixture arithmetic drifted")

    expected_analyzer = json.loads(json.dumps(v11["analyzer"]))
    carrier = expected_analyzer["carrier"]
    carrier["schema"] = "tonepoet-reference-analyzer-carrier/v3"
    carrier["stream_header"] = "riff_wave_bounded_32_bit_sizes"
    del carrier["streaming_size_sentinel_floor"]
    del carrier["greater_than_4_gib_fixture_required"]
    carrier["overflow_fixture_required"] = True
    carrier["overflow_behavior"] = "sox_ng_unseekable_wav_overflow_riff_size_58_data_size_modulo_2^32"
    if v12.get("analyzer") != expected_analyzer:
        raise AssertionError("v12 changed analyzer authority outside the F6 carrier correction")

    report_path = q / "dsd_reference_sox_ng_14_8_0_1_v12_report.md"
    inherited_report = dict(v11["qualification_report"])
    inherited_report["path"] = str(report_path.relative_to(root))
    inherited_report["sha256"] = sha256(report_path)
    if v12.get("qualification_report") != inherited_report:
        raise AssertionError("v12 qualification-report authority is noncanonical")

    release = {
        "schema": "tonepoet-dsd-reference-release-certification/v1",
        "path": "tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v12_certification.json",
        "candidate_manifest_path": "tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v12_candidate.json",
        "report_sha256": None,
        "candidate_manifest_sha256": None,
    }
    if v12.get("release_certification") != release:
        raise AssertionError("v12 release descriptor is noncanonical")
    certification = json.loads((q / "dsd_reference_sox_ng_14_8_0_1_v12_certification.json").read_text())
    if certification != {
        "schema_version": 12,
        "policy": "sox_ng_14_8_0_1_v12",
        "status": "not_run",
        "outcome": "not_run",
        "note": "Policy v12 is a source-controlled qualification candidate. Run the mandatory pinned real-tool gate and bind its exact report before promotion.",
    }:
        raise AssertionError("v12 certification stub is noncanonical")

    planner = (root / "tonepoet-pipeline/src/dsd_reference.rs").read_text()
    settings = (root / "tonepoet-pipeline/src/settings.rs").read_text()
    executor = (root / "src/convert/pipeline/track_executor.rs").read_text()
    manifest = (root / "src/convert/pipeline/manifest.rs").read_text()
    manifest_builder = (root / "src/convert/pipeline/manifest_builder.rs").read_text()
    qualification = (root / "tests/dsd_reference_qualification.rs").read_text()
    qualification_schema = (root / "tonepoet-pipeline/src/qualification_schema.rs").read_text()
    metadata_rewrite = (root / "src/convert/pipeline/metadata_rewrite.rs").read_text()
    stages = (root / "src/convert/pipeline/stages.rs").read_text()
    sentinel = (root / "tests/dsd_reference_settings_sentinel.rs").read_text()
    handoff = (root / "docs/handoff_dsd_reference_p0_current.md").read_text()
    findings = (root / "docs/findings_dsd_reference_p0_admission_round.md").read_text()

    for marker in (
        'pub const DSD_REFERENCE_POLICY_V12_KEY: &str = "sox_ng_14_8_0_1_v12";',
        '"qualification/dsd_reference_sox_ng_14_8_0_1_v12.json"',
        "SoxNg14801V12",
        "REFERENCE_STREAMED_WAV_RIFF_SIZE_FIELD_MAX",
        "REFERENCE_STREAMED_WAV_RIFF_SIZE_OVERHEAD_BYTES",
        "REFERENCE_STREAMED_WAV_MAX_AUDIO_PAYLOAD_BYTES",
        "REFERENCE_STREAMED_WAV_BYTES_PER_SAMPLE",
        "REFERENCE_STREAMED_WAV_DURATION_GUARD_FRAMES",
        "validate_reference_streamed_wav_capacity",
        "checked_mul(u128::from(contract.sample_rate_hz))",
        "checked_add(999_999_999)",
        "ReferenceErrorCode::StreamedWavCapacity",
        "DSD-REF-P0-025",
        "streamed_wav_capacity_is_fail_closed_and_boundary_exact",
        "streamed_wav_capacity_applies_to_every_terminal_depth_and_delivery_container",
        "valid sub-cap {target:?}/{depth:?} plan was rejected",
    ):
        require(planner, marker, "planner")
    require(settings, "SoxNg14801V12", "settings")
    require(sentinel, "SoxNg14801V12", "settings sentinel")
    require(manifest, "SoxNg14801V12", "durable manifest")
    require(manifest_builder, "SoxNg14801V12", "manifest builder")

    for marker in (
        "EmbeddedStreamedWavCapacity",
        "streamed_wav_capacity: EmbeddedStreamedWavCapacity",
        "tonepoet-reference-streamed-wav-capacity/v1",
        "riff_wave_bounded_32_bit_sizes",
        "sox_ng_unseekable_wav_overflow_riff_size_58_data_size_modulo_2^32",
        "ReferenceStreamedWavCapacityEvidenceV2",
        "is_canonical_v12",
        "DSD_REFERENCE_POLICY_V12_KEY",
        "SoxNg14801V12",
        "schema_version != 12",
        "the streamed-WAV capacity must be directly bound",
    ):
        require(executor, marker, "runtime validator")
    for marker in (
        "ReferenceStreamedWavCapacityEvidenceV2",
        "ReferenceStreamedWavBoundaryObservationV2",
        "ReferenceStreamedWavDataWrapWitnessV2",
        "#[serde(deny_unknown_fields)]",
        "pub fn is_canonical_v12",
        "transition_scan",
        "first_observed_riff_wrap_offset_frames",
        "wrapped_header_is_sentinel",
        "consumer_completeness_claim",
        "Exact defective writer fields remain report data",
    ):
        require(qualification_schema, marker, "shared qualification schema")
    for marker in (
        "MetadataRewriteAttributes",
        "fs::symlink_metadata",
        "metadata.ctime()",
        "metadata.ctime_nsec()",
        "std::os::unix::fs::chown",
        "file.set_permissions",
        "set_linux_xattrs_exact",
        "fs::FileTimes::new()",
        "file.sync_all()",
        "fs::rename(&tmp.path, path)",
        "sync_parent_dir(path)",
        "temp_allocation_requires_an_existing_regular_target",
        "replacement_preserves_mode_ownership_and_timestamps",
        "replacement_preserves_linux_xattrs_and_posix_acl_xattr",
        "replacement_rejects_target_substitution",
        "replacement_rejects_in_place_permission_drift",
    ):
        require(metadata_rewrite, marker, "metadata rewrite contract")
    for marker in (
        "metadata_rewrite_temp_path(path)?",
        "replace_rewritten_metadata_file(path, tmp)?",
    ):
        require(stages, marker, "metadata rewrite integration")
    for marker in (
        "create_sparse_w64_capacity_fixture",
        "ReferenceStreamedWavCapacityEvidenceV2",
        "ReferenceStreamedWavCapacityEvidenceV2::CONTRACT",
        "transition_count, 10",
        "the largest admitted carrier must have exact, nonwrapped RIFF and data fields",
        "first_observed_riff_wrap_offset_frames",
        "assert_eq!(data_wrap.sample_frames, 536_870_913)",
        "assert_eq!(data_wrap.observed_riff_size_field, 58)",
        "assert_eq!(expected_modulo_data_size_field, 8)",
        "assert_eq!(data_wrap.observed_data_size_field, expected_modulo_data_size_field)",
        '"streamed_wav_capacity"',
        "wrapped_header_is_sentinel: false",
        "consumer_completeness_claim: false",
        '"schema_version": 12',
        "DSD_REFERENCE_POLICY_V12_KEY",
    ):
        require(qualification, marker, "qualification harness")
    for marker in (
        "F6 correction",
        "Accepted-edge and transition evidence",
        "4,294,967,237",
        "4,294,967,232",
        "536,870,905",
        "contiguous ten-point frame-aligned scan",
        "strongly typed serialization",
        "Rewritten-file attribute contract",
        "DSD-REF-P0-025",
        "wrapped fields are not treated as sentinels",
    ):
        require((q / "dsd_reference_sox_ng_14_8_0_1_v12_report.md").read_text(), marker, "v12 report")
    require(handoff, "derive_dsd_reference_v12_streamed_wav_capacity.py", "handoff")
    require(findings, "F6 resolution (policy v12 candidate, 2026-07-21)", "findings")

    for text, label in ((planner, "planner"), (executor, "runtime validator"), (qualification, "qualification harness")):
        forbid(text, "streaming_size_sentinel_floor", label)
        forbid(text, "greater_than_4_gib_stream", label)
        forbid(text, '"read_to_eof": true', label)

    print("policy v12 streamed-WAV capacity derivation verified")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repository-root", "--root", dest="root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    verify(args.root.resolve())


if __name__ == "__main__":
    main()
