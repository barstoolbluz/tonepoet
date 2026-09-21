#!/usr/bin/env python3
"""Deterministically verify append-only DSD Reference policy v17 source lock."""

from __future__ import annotations

import argparse
import copy
import hashlib
import json
from pathlib import Path

FROZEN_V16 = {'derive_dsd_reference_v16_w64_integrity.py': '9eeed76b6267798d025876607c25fc6dafe4b919bf32eeb0befb1a94496bf093', 'dsd_reference_sox_ng_14_8_0_1_v16.json': 'cbea231eb727598ac547dc7346c5ab9a0f6182aeaacdc3e205ec916517ae2b53', 'dsd_reference_sox_ng_14_8_0_1_v16_candidate.json': 'cbea231eb727598ac547dc7346c5ab9a0f6182aeaacdc3e205ec916517ae2b53', 'dsd_reference_sox_ng_14_8_0_1_v16_certification.json': '0a771257f975cb02dc2ff11861880a0d9f503baf7fb422c72f0b08a7f126a315', 'dsd_reference_sox_ng_14_8_0_1_v16_report.md': '06f8eefe57db1de53cab4f0ea841f8e6bb4ff69414b656e0e949d3e517bbf587', 'dsd_reference_sox_ng_14_8_0_1_v8_terminal_source_proof.md': '870e3bfe3d4a9ed0a68aa7859e352a6fd5d0065c4fa4c316b3f0b4d36a2a0b35'}
NEW_REV = '9ed22fb3d813d6c02f67c254e57d162cee014a30'
NEW_NAR = 'sha256-WxMirop+SH3RzUM57QBDjetp9Q4qhFjzcfo2OJHStcs='
POLICY = 'sox_ng_14_8_0_1_v17'

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
    for name, expected in FROZEN_V16.items():
        actual = digest(q / name)
        if actual != expected:
            raise AssertionError(f"historical v16 evidence changed: {name}: {actual}")

    current_path = q / "dsd_reference_sox_ng_14_8_0_1_v17.json"
    candidate_path = q / "dsd_reference_sox_ng_14_8_0_1_v17_candidate.json"
    report_path = q / "dsd_reference_sox_ng_14_8_0_1_v17_report.md"
    certification_path = q / "dsd_reference_sox_ng_14_8_0_1_v17_certification.json"
    proof_path = q / "dsd_reference_sox_ng_14_8_0_1_v17_source_lock_proof.md"

    if current_path.read_bytes() != candidate_path.read_bytes():
        raise AssertionError("v17 current and candidate manifests are not byte-identical")
    manifest = json.loads(current_path.read_text())
    predecessor = json.loads((q / "dsd_reference_sox_ng_14_8_0_1_v16.json").read_text())
    expected_manifest = copy.deepcopy(predecessor)
    expected_manifest["schema_version"] = 17
    expected_manifest["policy"] = POLICY
    expected_manifest["sox_ng"]["revision"] = NEW_REV
    expected_manifest["sox_ng"]["nar_hash"] = NEW_NAR
    expected_manifest["qualification_report"]["path"] = (
        "tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v17_report.md"
    )
    expected_manifest["qualification_report"]["sha256"] = digest(report_path)
    expected_manifest["release_certification"]["path"] = (
        "tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v17_certification.json"
    )
    expected_manifest["release_certification"]["candidate_manifest_path"] = (
        "tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v17_candidate.json"
    )
    expected_manifest["qualification_basis"] = (
        "Policy v17 preserves the complete v16 policy contract and advances only the immutable "
        "SoX-ng source lock to the Wave64-finalization-corrected revision. Promotion requires "
        "the complete pinned real-tool release gate under the new source identity."
    )
    expected_manifest["runtime_activation"] = (
        "Fail closed unless the embedded v17 candidate is promoted by a passed release "
        "certification that binds the exact candidate bytes and the complete W64 integrity "
        "evidence under the v17 source lock."
    )
    if manifest != expected_manifest:
        raise AssertionError(
            "v17 manifest changes policy content beyond the append-only source-lock identity"
        )
    if manifest.get("schema_version") != 17 or manifest.get("policy") != POLICY:
        raise AssertionError("v17 manifest identity is noncanonical")
    if manifest.get("status") != "qualification_candidate":
        raise AssertionError("v17 must remain an unpromoted qualification candidate")
    if manifest.get("sox_ng") != {
        "version": "14.8.0.1",
        "revision": NEW_REV,
        "nar_hash": NEW_NAR,
        "required_probe_markers": ["sinc"],
    }:
        raise AssertionError("v17 SoX-ng source lock is noncanonical")
    report = manifest.get("qualification_report", {})
    if report.get("path") != "tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v17_report.md":
        raise AssertionError("v17 report path is noncanonical")
    if report.get("sha256") != digest(report_path):
        raise AssertionError("v17 report digest does not bind the report bytes")
    cert = manifest.get("release_certification", {})
    if cert.get("path") != "tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v17_certification.json" or cert.get("candidate_manifest_path") != "tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v17_candidate.json":
        raise AssertionError("v17 release-certification descriptor is noncanonical")
    if cert.get("report_sha256") is not None or cert.get("candidate_manifest_sha256") is not None:
        raise AssertionError("unpromoted v17 policy must not bind completed release evidence")

    certification = json.loads(certification_path.read_text())
    predecessor_certification = json.loads(
        (q / "dsd_reference_sox_ng_14_8_0_1_v16_certification.json").read_text()
    )
    expected_certification = copy.deepcopy(predecessor_certification)
    expected_certification["schema_version"] = 17
    expected_certification["policy"] = POLICY
    expected_certification["reason"] = (
        "The exact pinned v17 release qualification has not been executed in this environment."
    )
    if certification != expected_certification:
        raise AssertionError(
            "v17 certification skeleton changes content beyond its append-only identity"
        )
    if certification.get("schema_version") != 17 or certification.get("policy") != POLICY:
        raise AssertionError("v17 certification identity is noncanonical")
    if certification.get("status") != "not_run" or certification.get("outcome") != "not_run":
        raise AssertionError("v17 certification must remain fail-closed before commissioning")

    lock = json.loads((root / "flake.lock").read_text())
    sox = lock["nodes"]["sox_ng"]["locked"]
    if sox.get("rev") != NEW_REV or sox.get("narHash") != NEW_NAR:
        raise AssertionError("flake.lock SoX-ng source identity disagrees with v17 policy")

    proof = proof_path.read_text()
    for marker in [NEW_REV, NEW_NAR, "src/sox_ng.h", "src/gain.c", "src/dither.c", "src/rate.c", "src/dsdiff.c", "src/dsf.c", "src/formats_i.c", "src/sndfile.c", "byte-for-byte identical", "2^-32 + 2^-51", "2147487744"]:
        require(proof, marker, "v17 source-lock proof")
    require(report_path.read_text(), f"sha256:{digest(proof_path)}", "v17 report")

    planner = (root / "tonepoet-pipeline/src/dsd_reference.rs").read_text()
    settings = (root / "tonepoet-pipeline/src/settings.rs").read_text()
    semantic_plan = (root / "tonepoet-pipeline/src/semantic_plan.rs").read_text()
    executor = (root / "src/convert/pipeline/track_executor.rs").read_text()
    manifest_builder = (root / "src/convert/pipeline/manifest_builder.rs").read_text()
    manifest = (root / "src/convert/pipeline/manifest.rs").read_text()
    settings_sentinel = (root / "tests/settings_sentinel.rs").read_text()
    schema = (root / "tonepoet-pipeline/src/qualification_schema.rs").read_text()
    qualification = (root / "tests/dsd_reference_qualification.rs").read_text()
    album_bound_audit = (q / "verify_album_ceiling_terminal_bounds.py").read_text()
    common_candidate_path = q / "dsd_reference_common_v17_candidate.json"
    common_candidate = json.loads(common_candidate_path.read_text())

    for marker in [
        'pub const DSD_REFERENCE_POLICY_V17_KEY: &str = "sox_ng_14_8_0_1_v17";',
        "SoxNg14801V17", NEW_REV,
        'qualification/dsd_reference_sox_ng_14_8_0_1_v17.json',
    ]:
        require(planner, marker, "planner")
    require(settings, "SoxNg14801V17", "settings")
    require(semantic_plan, 'identity: "reference-v17-qualified"', "semantic plan")
    for text, label in [
        (manifest_builder, "manifest builder"),
        (manifest, "manifest validator"),
        (settings_sentinel, "settings sentinel"),
    ]:
        require(text, "SoxNg14801V17", label)
    require(
        manifest_builder,
        "Reference v16+ package identity mode",
        "manifest builder",
    )
    for marker in [
        "1..=16", "strict v17 activation", "manifest.schema_version != 17",
        "DSD_REFERENCE_POLICY_V17_KEY",
        "dsd_reference_sox_ng_14_8_0_1_v17.json",
        "dsd_reference_sox_ng_14_8_0_1_v17_report.md",
        "inherited_v16_bytes",
    ]:
        require(executor, marker, "executor")
    require(schema, "sox_ng_14_8_0_1_v17+common-v17-candidate", "common schema")
    require(qualification, "DSD_REFERENCE_POLICY_V17_KEY", "qualification harness")
    require(album_bound_audit, NEW_REV, "album terminal-bound audit")
    if common_candidate.get("inherited_v16_evidence_sha256") != FROZEN_V16["dsd_reference_sox_ng_14_8_0_1_v16.json"]:
        raise AssertionError("common candidate no longer preserves exact inherited v16 evidence")
    if common_candidate.get("closure", {}).get("policy_identity") != "sox_ng_14_8_0_1_v17+common-v17-candidate":
        raise AssertionError("common candidate does not bind the active v17 policy identity")
    predecessor_common_candidate = copy.deepcopy(common_candidate)
    predecessor_common_candidate["closure"]["policy_identity"] = (
        "sox_ng_14_8_0_1_v16+common-v17-candidate"
    )
    predecessor_common_bytes = (
        json.dumps(predecessor_common_candidate, indent=2, ensure_ascii=False) + "\n"
    ).encode()
    if hashlib.sha256(predecessor_common_bytes).hexdigest() != (
        "beedb97b4ba5c5214defcf772e96900d48510154b28ac6d78d78301d7479f469"
    ):
        raise AssertionError(
            "common v17 candidate changed beyond rebinding the active policy identity"
        )

    print("policy v17 SoX-ng source lock verified")

if __name__ == "__main__":
    main()
