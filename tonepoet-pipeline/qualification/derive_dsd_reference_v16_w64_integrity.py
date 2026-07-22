#!/usr/bin/env python3
"""Deterministically verify append-only DSD Reference policy v16 W64 integrity."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path


FROZEN_V15 = {
    "derive_dsd_reference_v15_hardening.py": "f1b503025c5326ece03ec253f1db85e0a6ca1a00b2b9410d3ef700a8a7d63468",
    "dsd_reference_sox_ng_14_8_0_1_v15.json": "45be292a81c75f47874b26e371936a67b6d9a7673e31eef6ccafbd92167c8528",
    "dsd_reference_sox_ng_14_8_0_1_v15_candidate.json": "45be292a81c75f47874b26e371936a67b6d9a7673e31eef6ccafbd92167c8528",
    "dsd_reference_sox_ng_14_8_0_1_v15_certification.json": "39a895648e43e17fc55dd30069def2040b15f96b029c19dbbb89b33b883b4c3a",
    "dsd_reference_sox_ng_14_8_0_1_v15_report.md": "897c660c75f3d5f99903973ce2ccb8bae8107accfc8179905be8e6fa85f13202",
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
    for name, expected in FROZEN_V15.items():
        actual = digest(q / name)
        if actual != expected:
            raise AssertionError(f"historical v15 artifact changed: {name}: {actual}")

    current_path = q / "dsd_reference_sox_ng_14_8_0_1_v16.json"
    candidate_path = q / "dsd_reference_sox_ng_14_8_0_1_v16_candidate.json"
    report_path = q / "dsd_reference_sox_ng_14_8_0_1_v16_report.md"
    certification_path = q / "dsd_reference_sox_ng_14_8_0_1_v16_certification.json"
    if current_path.read_bytes() != candidate_path.read_bytes():
        raise AssertionError("v16 current and candidate manifests are not byte-identical")
    manifest = json.loads(current_path.read_text())
    if manifest.get("schema_version") != 16:
        raise AssertionError("v16 schema version is noncanonical")
    if manifest.get("policy") != "sox_ng_14_8_0_1_v16":
        raise AssertionError("v16 policy identity is noncanonical")
    if manifest.get("status") != "qualification_candidate":
        raise AssertionError("v16 must remain an unpromoted qualification candidate")
    if manifest["qualification_report"] != {
        "path": "tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v16_report.md",
        "sha256": digest(report_path),
    }:
        raise AssertionError("v16 report descriptor does not bind the report bytes")
    if manifest["release_certification"] != {
        "schema": "tonepoet-dsd-reference-release-certification/v1",
        "path": "tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v16_certification.json",
        "candidate_manifest_path": "tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v16_candidate.json",
        "report_sha256": None,
        "candidate_manifest_sha256": None,
    }:
        raise AssertionError("v16 release certification descriptor is noncanonical")

    integrity = manifest.get("w64_integrity")
    expected_rates = [44100, 48000, 88200, 96000, 176400, 192000, 352800, 384000, 705600, 768000]
    if integrity != {
        "schema": "tonepoet-reference-w64-exact-integrity/v1",
        "parser": "independent_root_and_chunk_traversal_exact/v1",
        "carrier_contract_digest": "tonepoet-reference-carrier-probe/v2",
        "production_disposition": "reject_before_publication_with_DSD-REF-P0-026",
        "required_invariants": [
            "declared_riff_extent_equals_physical_file_extent",
            "declared_data_extent_equals_exact_pcm_payload",
            "complete_alignment_valid_chunk_traversal",
            "exact_upstream_r64_frame_authority_for_terminal_qpcm",
            "no_undeclared_trailing_bytes",
            "cell_specific_boundary_region_bracketed_at_1_over_510_base",
            "independent_ffmpeg_full_decode_xerror",
        ],
        "enabled_depths": ["int24", "float32", "float64"],
        "rates_hz": expected_rates,
        "channels": [1, 2],
        "required_characterization_cell_count": 60,
        "boundary_region_resolution_base_fraction": "1/510",
        "trigger_claim": "encoded_all_zero_after_depth_and_effects_quantization; input threshold is measured per cell and is not assumed",
        "same_path_qpcm_package_hash_is_independent_packaging_evidence": False,
    }:
        raise AssertionError("v16 W64 integrity policy is noncanonical")
    packaging = manifest["packaging"]
    if packaging.get("w64_delivery_mode") != "terminal_qpcm_direct_delivery_after_exact_structure_and_independent_consumer_traversal":
        raise AssertionError("v16 W64 delivery mode is noncanonical")
    if packaging.get("w64_same_path_hash_disposition") != "identity continuity only; not independent packaging evidence":
        raise AssertionError("v16 same-path evidence disposition is noncanonical")

    certification = json.loads(certification_path.read_text())
    if certification.get("schema_version") != 16 or certification.get("policy") != "sox_ng_14_8_0_1_v16":
        raise AssertionError("v16 certification identity is noncanonical")
    if certification.get("status") != "not_run" or certification.get("outcome") != "not_run":
        raise AssertionError("v16 certification must remain not_run before commissioning")
    certification_w64 = certification.get("w64_exact_integrity", {})
    if certification_w64.get("uncharacterized_enabled_cells") != 60:
        raise AssertionError("uncommissioned v16 certification must fail closed")
    if certification_w64.get("malformed_w64_publication_allowed") is not False:
        raise AssertionError("uncommissioned v16 certification permits malformed W64 publication")
    if certification_w64.get("same_path_hash_counted_as_independent_packaging") is not False:
        raise AssertionError("uncommissioned v16 certification misstates same-path evidence")
    if certification_w64.get("boundary_region_resolution_base_fraction") != "1/510":
        raise AssertionError("uncommissioned v16 certification omits boundary resolution")

    planner = (root / "tonepoet-pipeline" / "src" / "dsd_reference.rs").read_text()
    parser_source = (root / "tonepoet-pipeline" / "src" / "w64.rs").read_text()
    executor = (root / "src" / "convert" / "pipeline" / "track_executor.rs").read_text()
    qualification = (root / "tests" / "dsd_reference_qualification.rs").read_text()
    settings = (root / "tonepoet-pipeline" / "src" / "settings.rs").read_text()
    handoff = (root / "docs" / "handoff_dsd_reference_p0_current.md").read_text()

    for marker in [
        'pub const DSD_REFERENCE_POLICY_V16_KEY: &str = "sox_ng_14_8_0_1_v16";',
        "SoxNg14801V16",
        'w64_structure_identity=exact/v1',
        "ReferenceErrorCode::W64StructuralIntegrity",
        "DSD-REF-P0-026",
        "dsd_reference_sox_ng_14_8_0_1_v16.json",
    ]:
        require(planner, marker, "planner")
    for marker in [
        "pub fn inspect_exact_w64_pcm",
        "pub fn validate_exact_w64_pcm",
        "declared_file_bytes != physical_file_bytes",
        "duplicate format chunk",
        "duplicate data chunk",
        "non-zero alignment padding",
        "data chunk declares",
        "valid bits per sample is",
        "channel mask",
        "rejects_exact_frame_count_mismatch",
        "floating-point Wave64 is missing its fact chunk",
        "chunk traversal ended",
    ]:
        require(parser_source, marker, "W64 parser")
    for marker in [
        "inspect_reference_w64_structure(",
        "exact_reference_w64_structure(",
        "Reference QPCM verification lacks independent R64 frame authority",
        "QPCM exact structure rejected before publication",
        "build_reference_ffmpeg_full_traversal_command",
        '"-xerror".to_string()',
        "ReferencePackagedSampleIdentityMode::DirectW64QpcmExactDelivery",
        r'b"tonepoet-reference-carrier-probe/v2\0"',
        "Preserve the frozen v1 identity exactly",
        "w64_exact_integrity",
        "uncharacterized_enabled_cells",
        "same_path_qpcm_package_hash_counted_as_independent_packaging",
        "dsd_reference_sox_ng_14_8_0_1_v16.json",
        "manifest.schema_version != 16",
    ]:
        require(executor, marker, "executor")
    for marker in [
        "qualify_w64_exact_integrity_contract",
        'let depths = ["int24", "float32", "float64"]',
        "smallest_reachable_nonzero_power_of_two_exponent",
        "below_boundary_structure",
        "ffmpeg_below_boundary_opened",
        "boundary_probe_denominator",
        "largest_zero_multiplier_numerator",
        "smallest_nonzero_multiplier_numerator",
        "boundary_region_width_base_fraction",
        "at_boundary_decoded_nonzero",
        "leading_silence_control",
        "trailing_silence_control",
        '"uncharacterized_enabled_cells": 0',
        '"w64_same_path_hash_counted_as_independent_packaging": false',
        "assert_exact_w64_package_probe",
        "validate_exact_w64_pcm",
    ]:
        require(qualification, marker, "qualification")
    require(settings, "SoxNg14801V16", "settings")
    require(handoff, "sox_ng_14_8_0_1_v16", "handoff")
    require(handoff, "DSD-REF-P0-026", "handoff")
    if "all_zero_content_not_threshold_or_first_block_silence" in report_path.read_text():
        raise AssertionError("v16 report retains the disproven categorical trigger claim")

    print("policy v16 exact Wave64 integrity derivation verified")


if __name__ == "__main__":
    main()
