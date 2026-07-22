#!/usr/bin/env python3
"""Verify append-only policy-v13 streamed-Float64-WAV header authority.

Historical-checker lineage contract: once shipped, this checker must remain valid
against every successor policy. It may pin immutable artifacts and persistent
policy identities from its own generation, but it must never assert the mutable
current-policy embed pointer.
"""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path

V12_HASHES = {
    "derive_dsd_reference_v12_streamed_wav_capacity.py": "53b671fb553591de55c0db94e819e89025fe5382e1fc3285e4d5f9af1868c424",
    "dsd_reference_sox_ng_14_8_0_1_v12.json": "67ee6ba9a0ae0d49f8dadb21085396f8a314b1b9b3c616d86e5110d6a55c3274",
    "dsd_reference_sox_ng_14_8_0_1_v12_candidate.json": "67ee6ba9a0ae0d49f8dadb21085396f8a314b1b9b3c616d86e5110d6a55c3274",
    "dsd_reference_sox_ng_14_8_0_1_v12_certification.json": "523f9d76775e5db87b3cf84b66878e00e4ce4b525263e56173a4f296bf609a99",
    "dsd_reference_sox_ng_14_8_0_1_v12_report.md": "1187d7a99cb268b562ba65266bb50ea3413b9508cbadeecaf59b08f4d48378b2",
}

CAPACITY = {
    "schema": "tonepoet-reference-streamed-wav-capacity/v1",
    "applies_to": "all_reference_float64_wav_streams",
    "riff_size_field_max": 4_294_967_295,
    "riff_size_overhead_bytes": 50,
    "max_audio_payload_bytes": 4_294_967_245,
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
    for name, expected in V12_HASHES.items():
        actual = sha256(q / name)
        if actual != expected:
            raise AssertionError(f"append-only v12 artifact changed: {name}: {actual}")

    v12 = json.loads((q / "dsd_reference_sox_ng_14_8_0_1_v12.json").read_text())
    current_path = q / "dsd_reference_sox_ng_14_8_0_1_v13.json"
    candidate_path = q / "dsd_reference_sox_ng_14_8_0_1_v13_candidate.json"
    current_bytes = current_path.read_bytes()
    candidate_bytes = candidate_path.read_bytes()
    if current_bytes != candidate_bytes:
        raise AssertionError("v13 current and preserved candidate are not byte-identical")
    v13 = json.loads(current_bytes)
    if v13.get("schema_version") != 13 or v13.get("policy") != "sox_ng_14_8_0_1_v13":
        raise AssertionError("v13 schema/policy identity is noncanonical")
    if v13.get("status") != "qualification_candidate":
        raise AssertionError("v13 must remain an unpromoted candidate")

    changed = {
        "schema_version",
        "policy",
        "qualification_basis",
        "runtime_activation",
        "qualification_report",
        "release_certification",
        "streamed_wav_capacity",
    }
    for key in sorted(set(v12) | set(v13)):
        if key not in changed and v12.get(key) != v13.get(key):
            raise AssertionError(f"v13 changed inherited v12 field {key!r}")

    if v13.get("streamed_wav_capacity") != CAPACITY:
        raise AssertionError("v13 streamed-WAV capacity contract is noncanonical")
    if CAPACITY["riff_size_field_max"] - CAPACITY["riff_size_overhead_bytes"] != CAPACITY["max_audio_payload_bytes"]:
        raise AssertionError("v13 streamed-WAV capacity arithmetic is inconsistent")
    stream_header_bytes = CAPACITY["riff_size_overhead_bytes"] + 8
    if stream_header_bytes != 58:
        raise AssertionError("v13 Float64 streamed-WAV header arithmetic drifted")
    aligned_max = CAPACITY["max_audio_payload_bytes"] // CAPACITY["bytes_per_sample"] * CAPACITY["bytes_per_sample"]
    if aligned_max != 4_294_967_240:
        raise AssertionError("v13 frame-aligned capacity edge drifted")
    first_rejected = aligned_max + CAPACITY["bytes_per_sample"]
    if first_rejected != 4_294_967_248:
        raise AssertionError("v13 first rejected capacity edge drifted")
    data_wrap_payload = (1 << 32) + CAPACITY["bytes_per_sample"]
    transition_count = (data_wrap_payload - aligned_max) // CAPACITY["bytes_per_sample"] + 1
    if data_wrap_payload != 4_294_967_304 or transition_count != 9:
        raise AssertionError("v13 contiguous transition scan arithmetic drifted")

    report_path = q / "dsd_reference_sox_ng_14_8_0_1_v13_report.md"
    inherited_report = dict(v12["qualification_report"])
    inherited_report["path"] = str(report_path.relative_to(root))
    inherited_report["sha256"] = sha256(report_path)
    if v13.get("qualification_report") != inherited_report:
        raise AssertionError("v13 qualification-report authority is noncanonical")

    release = {
        "schema": "tonepoet-dsd-reference-release-certification/v1",
        "path": "tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v13_certification.json",
        "candidate_manifest_path": "tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v13_candidate.json",
        "report_sha256": None,
        "candidate_manifest_sha256": None,
    }
    if v13.get("release_certification") != release:
        raise AssertionError("v13 release descriptor is noncanonical")
    certification = json.loads((q / "dsd_reference_sox_ng_14_8_0_1_v13_certification.json").read_text())
    if certification != {
        "schema_version": 13,
        "policy": "sox_ng_14_8_0_1_v13",
        "status": "not_run",
        "outcome": "not_run",
        "note": "Policy v13 is a source-controlled qualification candidate. Run the mandatory pinned real-tool gate and bind its exact report before promotion.",
    }:
        raise AssertionError("v13 certification stub is noncanonical")

    planner = (root / "tonepoet-pipeline/src/dsd_reference.rs").read_text()
    schema = (root / "tonepoet-pipeline/src/qualification_schema.rs").read_text()
    manifest = (root / "src/convert/pipeline/manifest.rs").read_text()
    manifest_builder = (root / "src/convert/pipeline/manifest_builder.rs").read_text()
    qualification = (root / "tests/dsd_reference_qualification.rs").read_text()
    findings = (root / "docs/findings_dsd_reference_p0_admission_round.md").read_text()

    for marker in (
        'pub const DSD_REFERENCE_POLICY_V13_KEY: &str = "sox_ng_14_8_0_1_v13";',
        "SoxNg14801V13",
        "REFERENCE_STREAMED_WAV_HEADER_BYTES: u64 = 58",
        "REFERENCE_STREAMED_WAV_RIFF_SIZE_OVERHEAD_BYTES: u64 =\n    REFERENCE_STREAMED_WAV_HEADER_BYTES - 8",
        "REFERENCE_STREAMED_WAV_MAX_AUDIO_PAYLOAD_BYTES",
        "validate_reference_streamed_wav_capacity",
        "DSD-REF-P0-025",
    ):
        require(planner, marker, "planner")
    require(manifest, "SoxNg14801V13", "durable manifest")
    require(manifest_builder, "SoxNg14801V13", "manifest builder")

    for marker in (
        "ReferenceStreamedWavCapacityEvidenceV3",
        "tonepoet-reference-streamed-wav-capacity/v3",
        "pub fn is_canonical_v13",
        "STREAM_HEADER_BYTES: u64 = REFERENCE_STREAMED_WAV_HEADER_BYTES",
        "V12_STREAMED_WAV_STREAM_HEADER_BYTES: u64 = 66",
        "V12_STREAMED_WAV_RIFF_SIZE_OVERHEAD_BYTES: u64 = 58",
        "pub fn is_canonical_v12",
        "#[serde(deny_unknown_fields)]",
    ):
        require(schema, marker, "shared qualification schema")

    report = report_path.read_text()
    for marker in (
        "F7 correction",
        "58 bytes",
        "50 bytes",
        "4,294,967,245",
        "4,294,967,240",
        "4,294,967,248",
        "nine contiguous frame-aligned payloads",
        "Policy v12 and all of its JSON, candidate, certification, report, and derivation artifacts remain byte-identical",
    ):
        require(report, marker, "v13 report")
    require(findings, "F7 resolution (policy v13 candidate, 2026-07-21)", "findings")

    forbid(qualification, "let stream_header_bytes = ReferenceStreamedWavCapacityEvidenceV3::STREAM_HEADER_BYTES;\n    assert_eq!(stream_header_bytes, 66);", "qualification harness")
    forbid(qualification, "/tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v12.json", "active qualification harness")
    forbid(planner, "REFERENCE_STREAMED_WAV_RIFF_SIZE_OVERHEAD_BYTES: u64 = 58", "active planner")
    forbid(schema, "STREAM_HEADER_BYTES: u64 = REFERENCE_STREAMED_WAV_RIFF_SIZE_OVERHEAD_BYTES + 8", "shared qualification schema")

    print("policy v13 streamed-WAV header derivation verified")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--repository-root",
        "--root",
        dest="root",
        type=Path,
        default=Path(__file__).resolve().parents[2],
    )
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    verify(args.root.resolve())


if __name__ == "__main__":
    main()
