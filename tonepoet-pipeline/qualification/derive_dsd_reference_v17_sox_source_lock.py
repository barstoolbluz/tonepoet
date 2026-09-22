#!/usr/bin/env python3
"""Deterministically verify DSD Reference policy v17 source lock and Int32 TPDF correction."""

from __future__ import annotations

import argparse
import copy
import hashlib
import json
import math
from decimal import Decimal, ROUND_FLOOR, localcontext
from pathlib import Path

FROZEN_V16 = {'derive_dsd_reference_v16_w64_integrity.py': '9eeed76b6267798d025876607c25fc6dafe4b919bf32eeb0befb1a94496bf093', 'dsd_reference_sox_ng_14_8_0_1_v16.json': 'cbea231eb727598ac547dc7346c5ab9a0f6182aeaacdc3e205ec916517ae2b53', 'dsd_reference_sox_ng_14_8_0_1_v16_candidate.json': 'cbea231eb727598ac547dc7346c5ab9a0f6182aeaacdc3e205ec916517ae2b53', 'dsd_reference_sox_ng_14_8_0_1_v16_certification.json': '0a771257f975cb02dc2ff11861880a0d9f503baf7fb422c72f0b08a7f126a315', 'dsd_reference_sox_ng_14_8_0_1_v16_report.md': '06f8eefe57db1de53cab4f0ea841f8e6bb4ff69414b656e0e949d3e517bbf587', 'dsd_reference_sox_ng_14_8_0_1_v8_terminal_source_proof.md': '870e3bfe3d4a9ed0a68aa7859e352a6fd5d0065c4fa4c316b3f0b4d36a2a0b35'}
NEW_REV = '9ed22fb3d813d6c02f67c254e57d162cee014a30'
NEW_NAR = 'sha256-WxMirop+SH3RzUM57QBDjetp9Q4qhFjzcfo2OJHStcs='
POLICY = 'sox_ng_14_8_0_1_v17'
FFMPEG_INT32_AUTHORITY = 'ffmpeg_7-full-n7.1.3+nixpkgs-dd9b079222d43e1943b6ebd802f04fd959dc8e61/libswresample-dbl-triangular-s32/v1'
INT32_REALIZATION = 'int32-ffmpeg-triangular-2lsb-plus-f64-scalar-2^-51'
INT32_Q63_CEIL = 8_589_940_737
INT32_SAFE_DBTP = '-1.010000010'

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
    expected_manifest["ffmpeg"]["version"] = "7.1.3"
    expected_manifest["ffmpeg"]["int32_triangular_terminal_authority"] = FFMPEG_INT32_AUTHORITY
    expected_manifest["terminal_bounds"]["int32_tpdf"] = {
        "max_added_peak_fs_q63_ceil": INT32_Q63_CEIL,
        "safe_pre_terminal_ceiling_dbtp": INT32_SAFE_DBTP,
        "realization": INT32_REALIZATION,
    }
    # These are pre-existing operator corrections in the bundled v17 head:
    # Int32 joined the Wave64 integrity matrix and the current sacd-rs source
    # identity was regenerated before this terminal correction.
    expected_manifest["w64_integrity"]["enabled_depths"] = [
        "int24", "int32", "float32", "float64"
    ]
    expected_manifest["w64_integrity"]["required_characterization_cell_count"] = 80
    expected_manifest["in_process"]["sacd_rs_build_identity"] = (
        "sacd-rs-0.1.0-src-sha256:494a98a23e8cf9db1e77416107cffcfa0819b7d1e769fd23a472052f6da018d8"
    )
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
        "Policy v17 preserves the v16 contract except for two append-only corrections: the immutable "
        "SoX-ng Wave64-finalization source lock and Reference Int32 terminal realization, which now "
        "uses the commissioned FFmpeg 7.1.3/libswresample Float64-to-S32 triangular-dither authority "
        "on a true-scale carrier after one certified binary64 scalar gain."
    )
    expected_manifest["runtime_activation"] = (
        "Fail closed unless the embedded v17 candidate is promoted by a passed release certification "
        "that binds the exact candidate bytes, the commissioned FFmpeg Int32 triangular terminal "
        "authority, and the complete W64/common-model qualification evidence under the v17 source lock."
    )
    if manifest != expected_manifest:
        raise AssertionError(
            "v17 manifest differs from the source-lock + Int32-TPDF correction contract"
        )
    if manifest.get("schema_version") != 17 or manifest.get("policy") != POLICY:
        raise AssertionError("v17 manifest identity is noncanonical")
    # Re-derive the commissioned terminal-only and Reference end-to-end Int32
    # stored-sample bounds from their source proof constants.  The Reference
    # bound charges the certified binary64 scalar separately from the exact
    # FFmpeg triangular quantizer, then closes both into its single terminal
    # realization error slot.
    lsb = 2.0 ** -31
    dbl_add_ulp = 2.0 ** -52
    scalar_mul_error = 2.0 ** -51
    ffmpeg_terminal = math.nextafter(lsb + lsb + dbl_add_ulp, math.inf)
    if math.ceil(ffmpeg_terminal * (2.0 ** 63)) != 8_589_936_641:
        raise AssertionError("FFmpeg Int32 terminal-only Q1.63 bound drifted")
    reference_int32 = math.nextafter(ffmpeg_terminal + scalar_mul_error, math.inf)
    if math.ceil(reference_int32 * (2.0 ** 63)) != INT32_Q63_CEIL:
        raise AssertionError("Reference Int32 scalar+terminal Q1.63 bound drifted")
    with localcontext() as context:
        context.prec = 120
        ln_10 = Decimal(10).ln()
        admitted_peak = (((Decimal("-1.000000000") - Decimal("0.010000000")) / 20) * ln_10).exp()
        epsilon = Decimal(INT32_Q63_CEIL) / Decimal(2**63)
        safe_db = Decimal(20) * (admitted_peak - epsilon).ln() / ln_10
        safe_dbnano = int(
            (safe_db * Decimal(1_000_000_000)).to_integral_value(rounding=ROUND_FLOOR)
        )
    if safe_dbnano != -1_010_000_010:
        raise AssertionError("Reference Int32 safe pre-terminal ceiling drifted")
    int32_bound = manifest.get("terminal_bounds", {}).get("int32_tpdf", {})
    if int32_bound != {
        "max_added_peak_fs_q63_ceil": INT32_Q63_CEIL,
        "safe_pre_terminal_ceiling_dbtp": INT32_SAFE_DBTP,
        "realization": INT32_REALIZATION,
    }:
        raise AssertionError("v17 Int32 terminal bound is noncanonical")
    ffmpeg = manifest.get("ffmpeg", {})
    if ffmpeg.get("version") != "7.1.3" or ffmpeg.get("int32_triangular_terminal_authority") != FFMPEG_INT32_AUTHORITY:
        raise AssertionError("v17 commissioned FFmpeg Int32 authority is noncanonical")

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
    expected_certification["required_gates"] = [
        "w64_exact_integrity_80_cell_matrix" if gate == "w64_exact_integrity_60_cell_matrix" else gate
        for gate in expected_certification["required_gates"]
    ]
    expected_certification["w64_exact_integrity"]["required_cell_count"] = 80
    expected_certification["w64_exact_integrity"]["uncharacterized_enabled_cells"] = 80
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
    report_text = report_path.read_text()
    require(report_text, f"sha256:{digest(proof_path)}", "v17 report")
    for marker in [FFMPEG_INT32_AUTHORITY, "8,589,940,737", "-1.010000010", "80-cell", "not_run"]:
        require(report_text, marker, "v17 report")

    planner = (root / "tonepoet-pipeline/src/dsd_reference.rs").read_text()
    settings = (root / "tonepoet-pipeline/src/settings.rs").read_text()
    semantic_plan = (root / "tonepoet-pipeline/src/semantic_plan.rs").read_text()
    executor = (root / "src/convert/pipeline/track_executor.rs").read_text()
    stages = (root / "src/convert/pipeline/stages.rs").read_text()
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
        INT32_REALIZATION,
        "ReferenceTerminalLowering::Int32Tpdf",
        "dither_method=triangular",
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
        "execute_reference_terminal_lowering",
        "RetainedPcmScalarPump",
        "int32_tpdf",
        "FFMPEG_INT32_TRIANGULAR_TERMINAL_AUTHORITY_ID",
        'ffmpeg.version != "7.1.3"',
    ]:
        require(executor, marker, "executor")
    require(schema, "sox_ng_14_8_0_1_v17+common-v17-candidate", "common schema")
    require(schema, "tonepoet:reference_terminal_realization/hq1024_linear_error/v2", "common schema")
    require(schema, "sealed-reference-terminal-depth-policy-v17-int32-ffmpeg-triangular-tpdf", "common schema")
    require(schema, "FFMPEG_INT32_TRIANGULAR_TERMINAL_AUTHORITY_ID", "common schema")
    require(
        stages,
        "commissioned FFmpeg/libswresample Int32 triangular terminal, Reference policy",
        "conversion log",
    )
    require(qualification, "DSD_REFERENCE_POLICY_V17_KEY", "qualification harness")
    require(album_bound_audit, NEW_REV, "album terminal-bound audit")
    if common_candidate.get("inherited_v16_evidence_sha256") != FROZEN_V16["dsd_reference_sox_ng_14_8_0_1_v16.json"]:
        raise AssertionError("common candidate no longer preserves exact inherited v16 evidence")
    if common_candidate.get("closure", {}).get("policy_identity") != "sox_ng_14_8_0_1_v17+common-v17-candidate":
        raise AssertionError("common candidate does not bind the active v17 policy identity")
    closure = common_candidate.get("closure", {})
    if closure.get("terminal_implementation") != "tonepoet:reference_terminal_realization/hq1024_linear_error/v2":
        raise AssertionError("common candidate does not bind terminal realization v2")
    if closure.get("dither_quantization_policy") != "sealed-reference-terminal-depth-policy-v17-int32-ffmpeg-triangular-tpdf":
        raise AssertionError("common candidate does not bind the Int32 TPDF depth policy")
    if FFMPEG_INT32_AUTHORITY not in closure.get("tool_identity_version_closure", ""):
        raise AssertionError("common candidate omits the commissioned FFmpeg Int32 authority")

    print("policy v17 SoX-ng source lock and Int32 TPDF correction verified")

if __name__ == "__main__":
    main()
