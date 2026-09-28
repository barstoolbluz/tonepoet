#!/usr/bin/env python3
"""Generate SSRC true-peak terminal registry records from reviewed evidence.

This is the only supported registry-writing path. It rejects incomplete,
failed, identity-mismatched, or hand-edited reports and writes deterministic
Rust source. Review the diff and rerun the full workspace gate after promotion.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import math
import struct
from pathlib import Path
from typing import Any

EXPECTED_CONTRACT = "tonepoet:ssrc-true-peak-replay-terminal/v1"
EXPECTED_GAIN_MODEL = "tonepoet:ssrc-mixchannels-diagonal-gain/v1"
EXPECTED_SOURCE_REV = "6b0bbfe1fff79c0399347f4e1fb027c9931ef6ea"
EXPECTED_SEED = 1
EXPECTED_INTEGER_BOUND_POLICY = "whole_lsb_ceiling(max(8, 2*observed_max_abs_error_lsb + 4))"
EXPECTED_FLOAT_BOUND_POLICY = "nextafter(max(8*float_unit, 2*observed_max_abs_error_linear + 4*float_unit), +inf)"
TONEPOET_AUTHORITY_FILES = (
    "tonepoet-pipeline/src/ssrc_true_peak_terminal.rs",
    "tonepoet-pipeline/src/bin/ssrc_true_peak_gain_qualification.rs",
    "tonepoet-pipeline/src/dsd_album_gain.rs",
    "tonepoet-pipeline/src/mapping.rs",
    "tonepoet-pipeline/src/plugins.rs",
    "tonepoet-pipeline/src/ssrc_binary64.rs",
    "tonepoet-pipeline/src/w64.rs",
    "tonepoet-pipeline/qualification/ssrc_true_peak_terminal/qualify_ssrc_true_peak_terminal.py",
    "tonepoet-pipeline/qualification/ssrc_true_peak_terminal/promote_ssrc_true_peak_terminal.py",
    "src/convert/pipeline/stages.rs",
    "src/convert/pipeline/plan_bridge.rs",
    "src/convert/pipeline/types.rs",
)


def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for block in iter(lambda: f.read(1024 * 1024), b""):
            h.update(block)
    return h.hexdigest()


def rust_string(value: str) -> str:
    return json.dumps(value, ensure_ascii=True)


def profile_variant(value: str) -> str:
    return {"high": "High", "long": "Long", "insane": "Insane"}[value]


def bit_depth_variant(value: str) -> str:
    if value not in {"Int8", "Int16", "Int24", "Int32", "Float32", "Float64"}:
        raise ValueError(f"unsupported target bit depth {value!r}")
    return value


def pdf_expr(value: str | None) -> str:
    if value is None:
        return "None"
    return {
        "rectangular": "Some(SsrcPdfType::Rectangular)",
        "triangular": "Some(SsrcPdfType::Triangular)",
    }[value]


def derive_bound_lsb(observed_max: float) -> int:
    return max(8, math.ceil(2.0 * observed_max + 4.0))


def float_error_unit(bits: int) -> float:
    if bits == 32:
        return 2.0 ** -24
    if bits == 64:
        return 2.0 ** -53
    raise ValueError(f"unsupported floating-point terminal width {bits}")


def derive_bound_linear(observed_max: float, bits: int) -> float:
    unit = float_error_unit(bits)
    return math.nextafter(max(8.0 * unit, 2.0 * observed_max + 4.0 * unit), math.inf)


def f64_bits(value: float) -> int:
    return struct.unpack("<Q", struct.pack("<d", value))[0]


def canonical_cell_key(scope: dict[str, Any]) -> str:
    bit_depth = scope.get("target_bit_depth")
    if bit_depth in {"Int8", "Int16", "Int24", "Int32"}:
        bits = bit_depth[3:]
    elif bit_depth in {"Float32", "Float64"}:
        bits = f"-{bit_depth[5:]}"
    else:
        raise ValueError("qualification cell has invalid target_bit_depth")
    dither = scope.get("dither_id")
    pdf = scope.get("pdf_type")
    return (
        f"{scope.get('profile')}:{scope.get('source_rate_hz')}:{scope.get('target_rate_hz')}:"
        f"{scope.get('channels')}:{bits}:{dither if dither is not None else 'none'}:"
        f"{pdf if pdf is not None else 'none'}"
    )


def validate_cell(cell: dict[str, Any], report_arch: str) -> None:
    key = cell.get("cell_key")
    scope = cell.get("scope")
    if not isinstance(scope, dict):
        raise ValueError(f"cell {key} has no scope")
    if key != canonical_cell_key(scope):
        raise ValueError(f"cell {key} key does not match its physical scope")
    if scope.get("profile") not in {"high", "long", "insane"}:
        raise ValueError(f"cell {key} has an uncommissioned SSRC profile")
    if not isinstance(scope.get("source_rate_hz"), int) or scope["source_rate_hz"] <= 0:
        raise ValueError(f"cell {key} has invalid source rate")
    if not isinstance(scope.get("target_rate_hz"), int) or scope["target_rate_hz"] <= 0:
        raise ValueError(f"cell {key} has invalid target rate")
    if not isinstance(scope.get("channels"), int) or not (1 <= scope["channels"] <= 64):
        raise ValueError(f"cell {key} has invalid channel count")
    if scope.get("target_bit_depth") not in {"Int8", "Int16", "Int24", "Int32", "Float32", "Float64"}:
        raise ValueError(f"cell {key} has invalid target bit depth")
    if scope.get("base_attenuation_db") != "0.0" or scope.get("min_phase") is not False:
        raise ValueError(f"cell {key} is outside the commissioned linear-phase 0.0 dB Binary64 scope")
    if scope.get("architecture") != report_arch:
        raise ValueError(f"cell {key} architecture does not match report platform")
    dither = scope.get("dither_id")
    pdf = scope.get("pdf_type")
    if dither is None and pdf is not None:
        raise ValueError(f"cell {key} activates a PDF without dither")
    if dither is not None and (not isinstance(dither, int) or not (0 <= dither <= 255)):
        raise ValueError(f"cell {key} has invalid dither ID")
    if pdf not in {None, "rectangular", "triangular"}:
        raise ValueError(f"cell {key} has invalid dither PDF")
    is_float = scope.get("target_bit_depth") in {"Float32", "Float64"}
    if is_float and (dither is not None or pdf is not None):
        raise ValueError(f"cell {key} floating-point terminal must disable dither/PDF")
    # Int32 dither is intentionally allowed here. Unlike the ordinary SSRC
    # plugin path, every promoted certified-terminal cell has exact execution
    # evidence for its bit depth + native dither/PDF tuple. Production still
    # fails closed unless this exact physical scope is present in the registry.

    points = cell.get("gain_points")
    if not isinstance(points, list) or not points:
        raise ValueError(f"cell {key} has no gain execution evidence")
    gains = []
    maximum_errors = []
    error_field = "maximum_abs_error_linear" if is_float else "maximum_abs_error_lsb"
    for point in points:
        if not point.get("passed"):
            raise ValueError(f"cell {key} has a failed gain point")
        if point.get("deterministic_repeat") is not True:
            raise ValueError(f"cell {key} has a nondeterministic terminal replay")
        if point.get("no_saturation_premise") is not True:
            raise ValueError(f"cell {key} has a gain point without the no-saturation premise")
        gain_nano = point.get("gain_db_nano")
        if not isinstance(gain_nano, int):
            raise ValueError(f"cell {key} has a gain point without exact nanodecibel identity")
        maximum_error = point.get(error_field)
        if not isinstance(maximum_error, (int, float)) or not math.isfinite(float(maximum_error)) or maximum_error < 0:
            raise ValueError(f"cell {key} has a non-finite/negative terminal error observation")
        gains.append(gain_nano)
        maximum_errors.append(float(maximum_error))
    if len(set(gains)) != len(gains):
        raise ValueError(f"cell {key} has duplicate gain points")
    if cell.get("minimum_gain_db_nano") != min(gains) or cell.get("maximum_gain_db_nano") != max(gains):
        raise ValueError(f"cell {key} gain corpus extent does not match executed gain points")
    observed_max = max(maximum_errors)
    if is_float:
        if cell.get("bound_policy") != EXPECTED_FLOAT_BOUND_POLICY:
            raise ValueError(f"cell {key} uses an unexpected floating-point terminal bound policy")
        reported_max = cell.get("observed_maximum_abs_error_linear")
        if not isinstance(reported_max, (int, float)) or not math.isfinite(float(reported_max)):
            raise ValueError(f"cell {key} has invalid observed maximum absolute-linear error")
        if float(reported_max) != observed_max:
            raise ValueError(f"cell {key} observed maximum error does not match gain evidence")
        bits = 32 if scope["target_bit_depth"] == "Float32" else 64
        expected_bound = derive_bound_linear(observed_max, bits)
        bound = cell.get("stored_sample_error_linear")
        if not isinstance(bound, (int, float)) or f64_bits(float(bound)) != f64_bits(expected_bound):
            raise ValueError(f"cell {key} floating stored-sample bound does not follow the declared policy")
        if cell.get("stored_sample_error_linear_bits") != f64_bits(expected_bound):
            raise ValueError(f"cell {key} floating stored-sample bit-bound is inconsistent")
    else:
        if cell.get("bound_policy") != EXPECTED_INTEGER_BOUND_POLICY:
            raise ValueError(f"cell {key} uses an unexpected integer terminal bound policy")
        reported_max = cell.get("observed_maximum_abs_error_lsb")
        if not isinstance(reported_max, (int, float)) or not math.isfinite(float(reported_max)):
            raise ValueError(f"cell {key} has invalid observed maximum LSB error")
        if float(reported_max) != observed_max:
            raise ValueError(f"cell {key} observed maximum error does not match gain evidence")
        bound_lsb = cell.get("stored_sample_error_lsb")
        expected_bound_lsb = derive_bound_lsb(observed_max)
        if bound_lsb != expected_bound_lsb:
            raise ValueError(f"cell {key} stored-sample bound does not follow the declared policy")
        if cell.get("stored_sample_error_lsb_nano") != expected_bound_lsb * 1_000_000_000:
            raise ValueError(f"cell {key} stored-sample nano-bound is inconsistent")


def load_and_validate(report_path: Path, identity_path: Path, tonepoet_root: Path) -> tuple[dict[str, Any], str]:
    report_sha = sha256_file(report_path)
    report = json.loads(report_path.read_text(encoding="utf-8"))
    identity = json.loads(identity_path.read_text(encoding="utf-8"))
    if identity.get("qualification_report_sha256") != report_sha:
        raise ValueError("identity file does not match finalized qualification report bytes")
    if identity.get("evidence_id") != f"sha256:{report_sha}":
        raise ValueError("identity evidence_id is not the report SHA-256")
    if report.get("schema_version") != 1:
        raise ValueError("unsupported qualification schema version")
    if report.get("contract_id") != EXPECTED_CONTRACT or report.get("gain_model_id") != EXPECTED_GAIN_MODEL:
        raise ValueError("qualification contract/gain-model identity mismatch")
    if report.get("dither_seed") != EXPECTED_SEED:
        raise ValueError("qualification dither seed does not match runtime contract")
    authority = report.get("tonepoet_source_authority", {}).get("files")
    if not isinstance(authority, dict) or set(authority) != set(TONEPOET_AUTHORITY_FILES):
        raise ValueError("qualification report does not bind the complete Tonepoet terminal source authority")
    root = tonepoet_root.resolve()
    for relative in TONEPOET_AUTHORITY_FILES:
        path = root / relative
        if not path.is_file():
            raise ValueError(f"current Tonepoet authority file is missing: {relative}")
        if authority.get(relative) != sha256_file(path):
            raise ValueError(f"current Tonepoet authority file differs from qualification evidence: {relative}")
    ssrc = report.get("ssrc_identity", {})
    if ssrc.get("source_revision") != EXPECTED_SOURCE_REV:
        raise ValueError("qualification source revision is not the pinned SSRC revision")
    if not ssrc.get("build_identity"):
        raise ValueError("qualification lacks build identity")
    exe_sha = ssrc.get("sha256", "")
    if len(exe_sha) != 64 or any(ch not in "0123456789abcdefABCDEF" for ch in exe_sha):
        raise ValueError("qualification has invalid SSRC executable SHA-256")
    if report.get("commissioning", {}).get("decision") != "candidate_for_review":
        raise ValueError("qualification report is not a candidate_for_review")
    platform_info = report.get("platform")
    if not isinstance(platform_info, dict) or not isinstance(platform_info.get("architecture"), str) or not platform_info["architecture"]:
        raise ValueError("qualification report lacks platform architecture")
    report_arch = platform_info["architecture"]
    cells = report.get("cells")
    if not isinstance(cells, list) or not cells:
        raise ValueError("qualification report has no physical cells")
    seen = set()
    for cell in cells:
        if not cell.get("passed"):
            raise ValueError(f"cell {cell.get('cell_key')} did not pass")
        key = cell.get("cell_key")
        if not key or key in seen:
            raise ValueError("qualification contains duplicate/invalid cell keys")
        seen.add(key)
        validate_cell(cell, report_arch)
    summary = report.get("summary")
    if not isinstance(summary, dict):
        raise ValueError("qualification report has no summary")
    if (summary.get("cell_count") != len(cells)
            or summary.get("passed_cells") != len(cells)
            or summary.get("failed_cells") != 0):
        raise ValueError("qualification summary does not match physical cell evidence")
    return report, report_sha


def generate(report: dict[str, Any], report_sha: str) -> str:
    evidence_id = f"sha256:{report_sha}"
    ssrc = report["ssrc_identity"]
    cells = sorted(report["cells"], key=lambda cell: cell["cell_key"])
    lines = [
        "// @generated by qualification/ssrc_true_peak_terminal/promote_ssrc_true_peak_terminal.py",
        f"// Qualification report SHA-256: {report_sha}",
        "// Do not hand-edit. Re-run qualification + promotion instead.",
        "",
        "use super::{CommissionedSsrcTruePeakTerminalRecord, SsrcTruePeakStoredSampleErrorBound};",
        "use crate::{PcmBitDepth, SsrcPdfType, SsrcProfile};",
        "",
        "pub(crate) const COMMISSIONED_SSRC_TRUE_PEAK_TERMINALS:",
        "    &[CommissionedSsrcTruePeakTerminalRecord] = &[",
    ]
    for cell in cells:
        scope = cell["scope"]
        dither = "None" if scope["dither_id"] is None else f"Some({int(scope['dither_id'])})"
        base_att = "None" if scope["base_attenuation_db"] is None else f"Some({rust_string(scope['base_attenuation_db'])})"
        if scope["target_bit_depth"].startswith("Float"):
            error_bound = (
                "SsrcTruePeakStoredSampleErrorBound::AbsoluteLinearFsBits("
                f"{int(cell['stored_sample_error_linear_bits'])})"
            )
        else:
            error_bound = (
                "SsrcTruePeakStoredSampleErrorBound::TargetLsbNano("
                f"{int(cell['stored_sample_error_lsb_nano'])})"
            )
        lines.extend([
            "    CommissionedSsrcTruePeakTerminalRecord {",
            f"        profile: SsrcProfile::{profile_variant(scope['profile'])},",
            f"        source_rate_hz: {int(scope['source_rate_hz'])},",
            f"        target_rate_hz: {int(scope['target_rate_hz'])},",
            f"        channels: {int(scope['channels'])},",
            f"        target_bit_depth: PcmBitDepth::{bit_depth_variant(scope['target_bit_depth'])},",
            f"        dither_id: {dither},",
            f"        pdf_type: {pdf_expr(scope['pdf_type'])},",
            f"        base_attenuation_db: {base_att},",
            f"        min_phase: {str(bool(scope['min_phase'])).lower()},",
            f"        architecture: {rust_string(scope['architecture'])},",
            f"        evidence_id: {rust_string(evidence_id)},",
            f"        qualification_report_sha256: {rust_string(report_sha)},",
            f"        expected_executable_sha256: {rust_string(ssrc['sha256'])},",
            f"        source_revision: {rust_string(ssrc['source_revision'])},",
            f"        build_identity: {rust_string(ssrc['build_identity'])},",
            f"        minimum_gain_db_nano: {int(cell['minimum_gain_db_nano'])},",
            f"        maximum_gain_db_nano: {int(cell['maximum_gain_db_nano'])},",
            f"        stored_sample_error_bound: {error_bound},",
            "    },",
        ])
    lines.extend(["];"])
    return "\n".join(lines) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--identity", type=Path,
                        help="defaults to REPORT.identity.json")
    parser.add_argument("--tonepoet-root", type=Path, required=True,
                        help="current source tree; must byte-match the authority files hashed by qualification")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    identity = args.identity or args.report.with_name(args.report.name + ".identity.json")
    report, report_sha = load_and_validate(args.report, identity, args.tonepoet_root)
    generated = generate(report, report_sha)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    temp = args.output.with_name(args.output.name + ".tmp")
    temp.write_text(generated, encoding="utf-8")
    temp.replace(args.output)
    print(json.dumps({"output": str(args.output), "records": len(report["cells"]), "report_sha256": report_sha}, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
