#!/usr/bin/env python3
"""Verify the append-only policy-v10 exact production metadata qualification.

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

V9_HASHES = {
    "dsd_reference_sox_ng_14_8_0_1_v9.json": "9b6b924d4164aaf9907edc91edbbd5ccee7479d7bbc9d85857e4756911de9ea6",
    "dsd_reference_sox_ng_14_8_0_1_v9_candidate.json": "9b6b924d4164aaf9907edc91edbbd5ccee7479d7bbc9d85857e4756911de9ea6",
    "dsd_reference_sox_ng_14_8_0_1_v9_certification.json": "e792ce06704d988f50c40adbea8462b71d86bff49e9dd42774b032c2b4f15ad3",
    "dsd_reference_sox_ng_14_8_0_1_v9_report.md": "860d72b571e063797a245ec5e95c5da55391481dd94efafaebbf155e70a36fbc",
}


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def require(text: str, marker: str, label: str) -> None:
    if marker not in text:
        raise AssertionError(f"{label}: missing {marker!r}")


def verify(root: Path) -> None:
    q = root / "tonepoet-pipeline/qualification"
    for name, expected in V9_HASHES.items():
        actual = sha256(q / name)
        if actual != expected:
            raise AssertionError(f"append-only v9 artifact changed: {name}: {actual}")

    v9 = json.loads((q / "dsd_reference_sox_ng_14_8_0_1_v9.json").read_text())
    current_path = q / "dsd_reference_sox_ng_14_8_0_1_v10.json"
    candidate_path = q / "dsd_reference_sox_ng_14_8_0_1_v10_candidate.json"
    current_bytes = current_path.read_bytes()
    candidate_bytes = candidate_path.read_bytes()
    if current_bytes != candidate_bytes:
        raise AssertionError("v10 current and preserved candidate are not byte-identical")
    v10 = json.loads(current_bytes)
    if v10.get("schema_version") != 10 or v10.get("policy") != "sox_ng_14_8_0_1_v10":
        raise AssertionError("v10 schema/policy identity is noncanonical")
    if v10.get("status") != "qualification_candidate":
        raise AssertionError("v10 must remain an unpromoted candidate")

    changed = {
        "schema_version",
        "policy",
        "qualification_basis",
        "runtime_activation",
        "qualification_report",
        "release_certification",
        "sample_identity",
    }
    for key in sorted(set(v9) | set(v10)):
        if key not in changed and v9.get(key) != v10.get(key):
            raise AssertionError(f"v10 changed inherited v9 field {key!r}")

    identity = v10["sample_identity"]
    if identity.get("schema") != "tonepoet-reference-sample-identity/v7":
        raise AssertionError("v10 sample-identity schema is noncanonical")
    inherited = dict(v9["sample_identity"])
    inherited["schema"] = "tonepoet-reference-sample-identity/v7"
    inherited["metadata_mutation"] = identity["metadata_mutation"]
    if identity != inherited:
        raise AssertionError("v10 changed sample-identity fields outside metadata mutation")
    mutation = identity["metadata_mutation"]
    expected_mutation = {
        "w64": "error:DSD-REF-P0-024",
        "production_entry_point": "tonepoet::convert::pipeline::qualify_production_metadata_mutation",
        "shared_production_implementation": "apply_production_metadata_to_file",
        "authoritative_tag_source": "authoritative_metadata_tags",
        "qualification_scope": "authoritative_tag_mutation_without_artwork_or_replaygain",
        "environment_policy": "clear_and_set",
        "environment": {"LC_ALL": "C"},
        "qualified_post_metadata_targets": [
            "flac_native", "wav_riff", "wav_rf64", "aiff_native", "wavpack_native", "alac_m4a"
        ],
        "admitted_cell_count": 420,
        "primary_mutator_case_counts": {"ffmpeg": 160, "metaflac": 180, "wvtag": 80},
        "m4a_atomicparsley_freeform_case_count": 20,
        "w64_rejection": {
            "planner_entry_point": "plan_request_for_track",
            "planner_case_count": 60,
            "metadata_entry_point": "qualify_production_metadata_mutation",
            "metadata_case_count": 60,
            "code": "DSD-REF-P0-024",
        },
        "post_mutation_container_contract_rechecked": True,
        "rf64_preservation": "source_magic_RF64_requires_ffmpeg_-rf64_always",
        "w64_non_8_aligned_int24_mono_probe": "known_muxer_defect_phantom_sample",
        "riff_odd_byte_int24_mono_probe": "qualified_via_exact_production_ffmpeg_route",
    }
    if mutation != expected_mutation:
        raise AssertionError("v10 production metadata contract is noncanonical")

    report_path = q / "dsd_reference_sox_ng_14_8_0_1_v10_report.md"
    if v10["qualification_report"]["path"] != str(report_path.relative_to(root)):
        raise AssertionError("v10 report path is noncanonical")
    if v10["qualification_report"]["sha256"] != sha256(report_path):
        raise AssertionError("v10 report digest is stale")
    release = v10["release_certification"]
    if release != {
        "schema": "tonepoet-dsd-reference-release-certification/v1",
        "path": "tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v10_certification.json",
        "candidate_manifest_path": "tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v10_candidate.json",
        "report_sha256": None,
        "candidate_manifest_sha256": None,
    }:
        raise AssertionError("v10 release descriptor is noncanonical")
    certification = json.loads((q / "dsd_reference_sox_ng_14_8_0_1_v10_certification.json").read_text())
    if certification.get("schema_version") != 10 or certification.get("status") != "not_run":
        raise AssertionError("v10 certification stub is noncanonical")

    planner = (root / "tonepoet-pipeline/src/dsd_reference.rs").read_text()
    stages = (root / "src/convert/pipeline/stages.rs").read_text()
    tool_runner = (root / "src/convert/pipeline/tool.rs").read_text()
    executor = (root / "src/convert/pipeline/track_executor.rs").read_text()
    qualification = (root / "tests/dsd_reference_qualification.rs").read_text()
    flake = (root / "flake.nix").read_text()
    finding = (root / "docs/findings_dsd_reference_p0_admission_round.md").read_text()

    for marker in (
        'pub const DSD_REFERENCE_POLICY_V10_KEY: &str = "sox_ng_14_8_0_1_v10";',
        "SoxNg14801V10",
    ):
        require(planner, marker, "historical v10 planner identity")
    for marker in (
        "apply_production_metadata_to_file",
        "pub async fn qualify_production_metadata_mutation",
        "ProductionMetadataMutationOutcome",
        "wave_metadata_rewrite_requires_rf64",
        'args.push("-rf64".into())',
        'args.push("always".into())',
        '"w64" =>',
        "W64MetadataMutationUnqualified",
        "wav_metadata_rewrite_preserves_rf64_container_identity",
        "production_metadata_commands_use_closed_locale_environment",
        "deterministic_metadata_tool_environment",
        "CommandEnvironmentPolicy::ClearAndSet",
    ):
        require(stages, marker, "production metadata stage")

    for marker in (
        'ToolBinary::AtomicParsley => &[]',
        "atomic_parsley_version_probe_uses_zero_argument_banner",
        "AtomicParsley version: 20240608.083822.1ed9031",
    ):
        require(tool_runner, marker, "production tool identity probe")

    for marker in (
        "tonepoet-reference-sample-identity/v7",
        "tonepoet-reference-sample-identity-oracle/v4",
        '"container_level_post_mutation_sample_identity"',
        '"production_metadata_mutation_qualification"',
        '"qualification_scope"',
        'value.get("environment_policy")',
        'value.get("environment")',
        'json_object_u64(production_mutator_counts, "ffmpeg") != Some(160)',
        'json_object_u64(production_mutator_counts, "metaflac") != Some(180)',
        'json_object_u64(production_mutator_counts, "wvtag") != Some(80)',
        'json_object_u64(production_w64_rejection, "planner_case_count") != Some(60)',
        'json_object_u64(production_w64_rejection, "metadata_case_count") != Some(60)',
        '"production_metadata_mutators"',
    ):
        require(executor, marker, "runtime validator")
    if "production_metadata_mutator_qualification" in executor:
        raise AssertionError("runtime validator still accepts the overstated v9 claim")

    for marker in (
        "qualify_production_metadata_mutation(",
        "plan_request_for_track(",
        "assert_eq!(post_metadata_identity_comparison_count, 420)",
        "assert_eq!(w64_planner_entry_rejection_count, 60)",
        "assert_eq!(w64_metadata_entry_rejection_count, 60)",
        '("ffmpeg".to_string(), 160)',
        '("metaflac".to_string(), 180)',
        '("wvtag".to_string(), 80)',
        "assert_eq!(production_m4a_freeform_case_count, 20)",
        '"qualification_scope": "authoritative_tag_mutation_without_artwork_or_replaygain"',
        '"environment_policy": "clear_and_set"',
        '"environment": {"LC_ALL": "C"}',
        '"post_mutation_container_contract_rechecked": true',
        '"rf64_preservation": "source_magic_RF64_requires_ffmpeg_-rf64_always"',
        '"schema": "tonepoet-reference-sample-identity-oracle/v4"',
        'let atomic_parsley_version = combined(&run(&atomic_parsley, &[]));',
        'atomic_parsley_reported_version',
        'contains("atomicparsley version")',
        '"reported_version": atomic_parsley_reported_version',
    ):
        require(qualification, marker, "qualification harness")
    for env_name in (
        "TONEPOET_REFERENCE_METAFLAC_PATH",
        "TONEPOET_REFERENCE_WVTAG_PATH",
        "TONEPOET_REFERENCE_ATOMIC_PARSLEY_PATH",
    ):
        require(flake, env_name, "flake tool pinning")

    for marker in ("v10", "420", "160", "180", "80", "20", "60", "RF64"):
        require(report_path.read_text(), marker, "v10 report")
    require(finding, "F5 evidence completion", "findings resolution")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repository-root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    verify(args.repository_root.resolve())
    print("DSD Reference policy v10 production-metadata verification passed")


if __name__ == "__main__":
    main()
