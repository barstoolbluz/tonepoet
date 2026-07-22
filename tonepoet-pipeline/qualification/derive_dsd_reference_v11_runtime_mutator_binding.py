#!/usr/bin/env python3
"""Verify append-only policy-v11 runtime-bound metadata-mutator authority.

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

V10_HASHES = {
    "derive_dsd_reference_v10_production_metadata.py": "af4899a3974b8950b22afcc1e53968e60086d089d55fb199735d136a95aa6610",
    "dsd_reference_sox_ng_14_8_0_1_v10.json": "9fadf78613baa1f170499764da0285088a0e5720ff7d22e24163170d3174aab3",
    "dsd_reference_sox_ng_14_8_0_1_v10_candidate.json": "9fadf78613baa1f170499764da0285088a0e5720ff7d22e24163170d3174aab3",
    "dsd_reference_sox_ng_14_8_0_1_v10_certification.json": "be1a41e509013909cb5915ae92c6e7b98cf4f4a848e30fc6c4aa983240c530f4",
    "dsd_reference_sox_ng_14_8_0_1_v10_report.md": "130e4ded9600559bfebab36e78ff9f94bf14c56516193d339fca68d1a6bdda41",
}


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def require(text: str, marker: str, label: str) -> None:
    if marker not in text:
        raise AssertionError(f"{label}: missing {marker!r}")


def verify(root: Path) -> None:
    q = root / "tonepoet-pipeline/qualification"
    for name, expected in V10_HASHES.items():
        actual = sha256(q / name)
        if actual != expected:
            raise AssertionError(f"append-only v10 artifact changed: {name}: {actual}")

    v10 = json.loads((q / "dsd_reference_sox_ng_14_8_0_1_v10.json").read_text())
    current_path = q / "dsd_reference_sox_ng_14_8_0_1_v11.json"
    candidate_path = q / "dsd_reference_sox_ng_14_8_0_1_v11_candidate.json"
    current_bytes = current_path.read_bytes()
    candidate_bytes = candidate_path.read_bytes()
    if current_bytes != candidate_bytes:
        raise AssertionError("v11 current and preserved candidate are not byte-identical")
    v11 = json.loads(current_bytes)
    if v11.get("schema_version") != 11 or v11.get("policy") != "sox_ng_14_8_0_1_v11":
        raise AssertionError("v11 schema/policy identity is noncanonical")
    if v11.get("status") != "qualification_candidate":
        raise AssertionError("v11 must remain an unpromoted candidate")

    changed = {
        "schema_version",
        "policy",
        "qualification_basis",
        "runtime_activation",
        "qualification_report",
        "release_certification",
        "sample_identity",
    }
    for key in sorted(set(v10) | set(v11)):
        if key not in changed and v10.get(key) != v11.get(key):
            raise AssertionError(f"v11 changed inherited v10 field {key!r}")

    identity = v11["sample_identity"]
    inherited_identity = json.loads(json.dumps(v10["sample_identity"]))
    inherited_mutation = inherited_identity["metadata_mutation"]
    expected_additions = {
        "runtime_identity_binding": "certified_report_to_compiled_store_to_runner_resolution",
        "execution_authority": "exact_canonical_path_plus_executable_sha256",
        "pre_mutation_reverification": "path_sha256_version_closure",
        "per_output_authority": "ReferenceToolchainEvidence.metadata_mutators_and_execution_fingerprint_v1",
    }
    inherited_mutation.update(expected_additions)
    if identity != inherited_identity:
        raise AssertionError("v11 changed sample-identity authority outside runtime binding")

    report_path = q / "dsd_reference_sox_ng_14_8_0_1_v11_report.md"
    if v11["qualification_report"]["path"] != str(report_path.relative_to(root)):
        raise AssertionError("v11 report path is noncanonical")
    if v11["qualification_report"]["sha256"] != sha256(report_path):
        raise AssertionError("v11 report digest is stale")
    inherited_report = dict(v10["qualification_report"])
    inherited_report["path"] = str(report_path.relative_to(root))
    inherited_report["sha256"] = sha256(report_path)
    if v11["qualification_report"] != inherited_report:
        raise AssertionError("v11 changed inherited qualification-report authority")

    release = v11["release_certification"]
    if release != {
        "schema": "tonepoet-dsd-reference-release-certification/v1",
        "path": "tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v11_certification.json",
        "candidate_manifest_path": "tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v11_candidate.json",
        "report_sha256": None,
        "candidate_manifest_sha256": None,
    }:
        raise AssertionError("v11 release descriptor is noncanonical")
    certification = json.loads((q / "dsd_reference_sox_ng_14_8_0_1_v11_certification.json").read_text())
    if certification != {
        "schema_version": 11,
        "policy": "sox_ng_14_8_0_1_v11",
        "status": "not_run",
        "outcome": "not_run",
        "note": "Policy v11 is a source-controlled qualification candidate. Run the mandatory pinned real-tool gate and bind its exact report before promotion.",
    }:
        raise AssertionError("v11 certification stub is noncanonical")

    planner = (root / "tonepoet-pipeline/src/dsd_reference.rs").read_text()
    fingerprint = (root / "tonepoet-pipeline/src/fingerprint.rs").read_text()
    build = (root / "build.rs").read_text()
    flake = (root / "flake.nix").read_text()
    tool = (root / "src/convert/pipeline/tool.rs").read_text()
    executor = (root / "src/convert/pipeline/track_executor.rs").read_text()
    stages = (root / "src/convert/pipeline/stages.rs").read_text()
    qualification = (root / "tests/dsd_reference_qualification.rs").read_text()
    findings = (root / "docs/findings_dsd_reference_p0_admission_round.md").read_text()

    for marker in (
        'pub const DSD_REFERENCE_POLICY_V11_KEY: &str = "sox_ng_14_8_0_1_v11";',
        "SoxNg14801V11",
    ):
        require(planner, marker, "historical v11 planner identity")

    for env_name in (
        "TONEPOET_REFERENCE_METAFLAC_STORE_PATH",
        "TONEPOET_REFERENCE_WVTAG_STORE_PATH",
        "TONEPOET_REFERENCE_ATOMIC_PARSLEY_STORE_PATH",
    ):
        require(build, env_name, "build store binding")
        require(flake, env_name, "flake store binding")

    for marker in (
        "pub struct BoundToolExecutable",
        "async fn run_bound",
        "tool runner does not implement exact bound executable execution",
        "path drift: expected",
        "executable digest drift",
        "self.run_with_binary_path(cmd, executable.canonical_path.clone(), cancel)",
        "default_bound_execution_fails_closed",
        "bound_execution_spawns_the_exact_attested_executable",
        "bound_execution_rejects_runner_path_override_drift",
        "bound_execution_rejects_executable_content_drift",
    ):
        require(tool, marker, "bound tool runner")

    for marker in (
        "ReferenceMetadataMutatorIdentityInput",
        "ReferenceMetadataMutatorToolchainInput",
        "metadata_mutators: Option<ReferenceMetadataMutatorToolchainInput>",
        'format!("metadata_mutator.{name}.canonical_path")',
        'format!("metadata_mutator.{name}.sha256")',
        'format!("metadata_mutator.{name}.version")',
        'format!("metadata_mutator.{name}.closure")',
        "execution_fingerprint_binds_every_metadata_mutator_identity_component",
    ):
        require(fingerprint, marker, "execution fingerprint")

    for marker in (
        "ReferenceMetadataMutatorIdentity",
        "ReferenceMetadataMutatorToolchain",
        "metadata_mutators: Option<ReferenceMetadataMutatorToolchain>",
        "CertifiedMetadataMutatorToolchain",
        "store_path: PathBuf",
        "certified.store_path.as_path() != Path::new(compiled_store)",
        '"runtime_metadata_mutator_binding"',
        '"must_equal_compiled_store_and_certified_canonical_path"',
        '"resolved_canonical_path_must_equal_certified_path"',
        '"exact_canonical_path_plus_executable_sha256"',
        '"path_sha256_version_closure"',
        '"ReferenceToolchainEvidence.metadata_mutators_and_execution_fingerprint_v1"',
        "attest_certified_metadata_mutator",
        "resolve_policy_owned_reference_tool_path(binary)",
        "compiled_reference_executable_path(binary)",
        "verify_reference_metadata_toolchain_before_mutation",
        "reference_bound_metadata_executable",
        "reference_metadata_toolchains_match",
        'option_env!("TONEPOET_REFERENCE_METAFLAC_STORE_PATH")',
        'option_env!("TONEPOET_REFERENCE_WVTAG_STORE_PATH")',
        'option_env!("TONEPOET_REFERENCE_ATOMIC_PARSLEY_STORE_PATH")',
    ):
        require(executor, marker, "runtime certification and attestation")

    for marker in (
        "ReferenceBoundMetadataRunner",
        "reference_bound_metadata_executable(self.toolchain, cmd.binary)",
        "verify_reference_metadata_toolchain_before_mutation(authority, runner, cancel)",
        "Reference tracks disagree on the attested metadata toolchain",
        "apply_metadata_to_track_artifact",
    ):
        require(stages, marker, "production metadata execution")

    for marker in (
        "METAFLAC_STORE_ENV",
        "WVTAG_STORE_ENV",
        "ATOMIC_PARSLEY_STORE_ENV",
        '"runtime_metadata_mutator_binding"',
        '"status": "passed"',
        '"runner_resolution_policy": "resolved_canonical_path_must_equal_certified_path"',
        '"execution_authority": "exact_canonical_path_plus_executable_sha256"',
        '"pre_mutation_reverification": "path_sha256_version_closure"',
        '"per_output_authority": "ReferenceToolchainEvidence.metadata_mutators_and_execution_fingerprint_v1"',
    ):
        require(qualification, marker, "qualification harness")

    for marker in (
        "F5 runtime identity completion",
        "ProcessorConfig.tool_paths",
        "exact canonical-path-plus-SHA-256",
    ):
        require(findings, marker, "findings resolution")
    for marker in (
        "runtime-bound metadata-mutator",
        "ProcessorConfig.tool_paths",
        "execution_fingerprint_v1",
        "Artwork",
        "ReplayGain",
    ):
        require(report_path.read_text(), marker, "v11 report")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repository-root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    verify(args.repository_root.resolve())
    print("DSD Reference policy v11 runtime-mutator binding verification passed")


if __name__ == "__main__":
    main()
