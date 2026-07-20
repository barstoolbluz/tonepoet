#!/usr/bin/env python3
"""Derive and verify the policy-v7 terminal true-peak bounds.

The calculation is intentionally standard-library-only and uses Decimal at
120-digit precision. For each qualified terminal realization it computes:

    A = 10 ** ((C - R) / 20)
    S = 20 * log10(A - epsilon)

where C is the public post-final ceiling, R is exactly one analyzer reporting
quantum, and epsilon is the upward-rounded Q1.63 additive peak bound. S is
rounded toward negative infinity to one nanodecibel.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
from decimal import Decimal, ROUND_FLOOR, localcontext
from pathlib import Path
from typing import Final

NANO: Final[Decimal] = Decimal(1_000_000_000)
Q63_DENOMINATOR: Final[Decimal] = Decimal(2) ** 63
PUBLIC_CEILING_DB: Final[Decimal] = Decimal("-1.000000000")
REPORTING_QUANTUM_DB: Final[Decimal] = Decimal("0.010000000")

CELLS: Final[dict[str, int]] = {
    "int24_tpdf": 2_199_023_255_552,
    "float32": 1_099_511_627_776,
    "float64": 4_096,
}


def derive_safe_dbnano(q63_ceil: int) -> int:
    """Return the conservative pre-terminal ceiling in signed nanodecibels."""
    with localcontext() as context:
        context.prec = 120
        ln_10 = Decimal(10).ln()
        admitted_peak = (
            ((PUBLIC_CEILING_DB - REPORTING_QUANTUM_DB) / 20) * ln_10
        ).exp()
        epsilon = Decimal(q63_ceil) / Q63_DENOMINATOR
        if epsilon <= 0 or epsilon >= admitted_peak:
            raise ValueError("terminal epsilon is outside the derivable amplitude domain")
        safe_db = Decimal(20) * (admitted_peak - epsilon).ln() / ln_10
        return int((safe_db * NANO).to_integral_value(rounding=ROUND_FLOOR))


def render_dbnano(value: int) -> str:
    sign = "-" if value < 0 else ""
    magnitude = abs(value)
    return f"{sign}{magnitude // 1_000_000_000}.{magnitude % 1_000_000_000:09d}"


def derived_cells() -> dict[str, dict[str, int | str]]:
    return {
        name: {
            "max_added_peak_fs_q63_ceil": q63,
            "safe_pre_terminal_ceiling_dbtp": render_dbnano(derive_safe_dbnano(q63)),
        }
        for name, q63 in CELLS.items()
    }


def sha256_file(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def verify_artifact(path: Path, repository_root: Path) -> None:
    document = json.loads(path.read_text(encoding="utf-8"))
    if document.get("schema_version") != 7:
        raise AssertionError(f"{path}: expected schema_version 7")
    if document.get("policy") != "sox_ng_14_8_0_1_v7":
        raise AssertionError(f"{path}: unexpected policy identity")
    if document.get("status") != "qualification_candidate":
        raise AssertionError(f"{path}: v7 must remain an unpromoted candidate")

    carrier = document["analyzer"]["carrier"]
    if carrier.get("schema") != "tonepoet-reference-analyzer-carrier/v2":
        raise AssertionError(f"{path}: unexpected analyzer carrier schema")
    if carrier.get("parser") != "ffmpeg_loudnorm_input_tp_v3":
        raise AssertionError(f"{path}: unexpected analyzer parser")
    if (
        carrier.get("routing_rule")
        != "float32_w64_direct_ffmpeg_else_sox_f64_wav_stream"
    ):
        raise AssertionError(f"{path}: unexpected analyzer routing rule")
    if carrier.get("disk_intermediate") is not False:
        raise AssertionError(f"{path}: analyzer contract introduced a disk intermediate")

    packaging = document.get("packaging", {})
    if packaging.get("schema") != "tonepoet-reference-lossless-packaging/v3":
        raise AssertionError(f"{path}: unexpected packaging schema")
    if packaging.get("float64_wav_targets") != ["wav_riff", "wav_rf64"]:
        raise AssertionError(f"{path}: unexpected Float64 package targets")
    if packaging.get("producer_tool") != "sox_ng" or packaging.get("consumer_tool") != "ffmpeg":
        raise AssertionError(f"{path}: unexpected Float64 package tools")
    if packaging.get("producer_args_template") != [
        "-S", "-D", "{qpcm_w64}", "-t", "raw", "-e", "floating-point", "-b", "64", "-L", "-"
    ]:
        raise AssertionError(f"{path}: unexpected Float64 package producer argv")
    if packaging.get("consumer_args_template") != [
        "-y", "-hide_banner", "-nostdin", "-f", "f64le", "-ar", "{sample_rate_hz}",
        "-ac", "{channels}", "-i", "pipe:0", "-map", "0:a:0", "-map_metadata", "-1",
        "-vn", "-sn", "-dn", "-c:a", "pcm_f64le", "-f", "wav", "{rf64_args}", "{output}"
    ]:
        raise AssertionError(f"{path}: unexpected Float64 package consumer argv")
    if packaging.get("rf64_args") != ["-rf64", "always"]:
        raise AssertionError(f"{path}: unexpected RF64 package args")
    if (
        packaging.get("transport") != "direct_stdout_to_stdin_no_shell"
        or packaging.get("stream_encoding") != "pcm_f64le"
        or packaging.get("stream_framing") != "headerless_raw_pcm"
        or packaging.get("endianness") != "little"
        or packaging.get("disk_intermediate") is not False
    ):
        raise AssertionError(f"{path}: Float64 package transport is not canonical")
    if packaging.get("environment_policy") != "clear_and_set" or packaging.get("environment") != {"LC_ALL": "C"}:
        raise AssertionError(f"{path}: Float64 package environment is not canonical")
    if packaging.get("forbidden_route") != "ffmpeg_direct_decode_of_float64_qpcm_w64":
        raise AssertionError(f"{path}: Float64 forbidden route is not bound")

    identity = document.get("sample_identity", {})
    expected_identity = {
        "schema": "tonepoet-reference-sample-identity/v5",
        "route_authority": "typed_plan_carrier_path_role_target_depth_v2",
        "routes": {
            "r64_float64_w64": "sox_f64le_raw_stream",
            "qpcm_int24_w64": "ffmpeg_direct",
            "qpcm_float32_w64": "ffmpeg_direct",
            "qpcm_float64_w64": "sox_f64le_raw_stream",
            "packaged_int24_w64": "ffmpeg_direct",
            "packaged_float32_w64": "ffmpeg_direct",
            "packaged_float64_w64": "sox_f64le_raw_stream",
            "packaged_non_w64": "ffmpeg_direct",
            "post_metadata_int24_w64": "ffmpeg_direct",
            "post_metadata_float32_w64": "ffmpeg_direct",
            "post_metadata_float64_w64": "sox_f64le_raw_stream",
            "post_metadata_non_w64": "ffmpeg_direct",
        },
        "hash_format": "interleaved_depth_native_le_sha256",
        "hash_codecs": {
            "int24": "pcm_s24le",
            "float32": "pcm_f32le",
            "float64": "pcm_f64le",
        },
        "forbidden_route": "ffmpeg_direct_decode_of_float64_w64",
        "oracle_independence": "float64_w64_source_sox_decode_vs_riff_rf64_output_ffmpeg_decode",
        "environment_policy": "clear_and_set",
        "environment": {"LC_ALL": "C"},
    }
    if identity != expected_identity:
        raise AssertionError(f"{path}: decoded-sample identity contract is not canonical")

    report = document["qualification_report"]
    bound_paths = {
        "sha256": report["path"],
        "guidance_sha256": "docs/tonepoet_dsd_to_pcm_guidance_evidence_based_v9.md",
        "decimation_report_sha256": "docs/sox_ng_dsd_decimation_test_report_v5.md",
        "commission_sha256": "docs/brief_dsd_reference_p0_scope_and_commission.md",
        "amendment_sha256": "docs/brief_dsd_reference_p0_policy_v3_amendment.md",
        "analyzer_corrective_brief_sha256": (
            "docs/brief_dsd_reference_p0_corrective_analyzer_carrier.md"
        ),
        "runtime_defaults_corrective_brief_sha256": (
            "docs/brief_dsd_reference_p0_corrective_runtime_defaults.md"
        ),
    }
    for field, relative in bound_paths.items():
        bound_path = repository_root / relative
        actual = sha256_file(bound_path)
        if report.get(field) != actual:
            raise AssertionError(
                f"{path}: {field}={report.get(field)!r}, expected {actual!r} "
                f"for {relative}"
            )
    analyzer_uncertainty = document["analyzer"]["reporting_uncertainty_db"]
    terminal = document["terminal_bounds"]
    reserve = terminal["post_final_acceptance_reserve_db"]
    if analyzer_uncertainty != "0.010000000":
        raise AssertionError(
            f"{path}: unexpected analyzer reporting uncertainty "
            f"{analyzer_uncertainty}"
        )
    if reserve != analyzer_uncertainty:
        raise AssertionError(
            f"{path}: terminal reserve {reserve} != analyzer uncertainty "
            f"{analyzer_uncertainty}"
        )
    expected_cells = derived_cells()
    for name, expected in expected_cells.items():
        actual = terminal[name]
        for field, value in expected.items():
            if actual[field] != value:
                raise AssertionError(
                    f"{path}: {name}.{field}={actual[field]!r}, expected {value!r}"
                )


def verify_compiled_policy(path: Path) -> None:
    source = path.read_text(encoding="utf-8")
    constant_patterns = {
        "REFERENCE_CEILING": -1_000_000_000,
        "POST_FINAL_ACCEPTANCE_RESERVE": 10_000_000,
    }
    for name, expected in constant_patterns.items():
        match = re.search(
            rf"pub const {name}: Self = Self\((-?[0-9_]+)\);",
            source,
        )
        if match is None:
            raise AssertionError(f"{path}: cannot locate DbNano::{name}")
        actual = int(match.group(1).replace("_", ""))
        if actual != expected:
            raise AssertionError(f"{path}: DbNano::{name}={actual}, expected {expected}")

    variants = {
        "int24_tpdf": "Int24",
        "float32": "Float32",
        "float64": "Float64",
    }
    expected_cells = derived_cells()
    for name, variant in variants.items():
        match = re.search(
            rf"PcmBitDepth::{variant}\s*=>\s*\(\s*([0-9_]+)\s*,\s*(-?[0-9_]+)",
            source,
        )
        if match is None:
            raise AssertionError(f"{path}: cannot locate compiled {variant} terminal cell")
        actual_q63 = int(match.group(1).replace("_", ""))
        actual_safe = int(match.group(2).replace("_", ""))
        expected = expected_cells[name]
        if actual_q63 != expected["max_added_peak_fs_q63_ceil"]:
            raise AssertionError(
                f"{path}: compiled {variant} q63={actual_q63}, "
                f"expected {expected['max_added_peak_fs_q63_ceil']}"
            )
        expected_safe = int(
            str(expected["safe_pre_terminal_ceiling_dbtp"]).replace(".", "")
        )
        if actual_safe != expected_safe:
            raise AssertionError(
                f"{path}: compiled {variant} safe={actual_safe}, expected {expected_safe}"
            )


def verify_compiled_v7_routes(repository_root: Path) -> None:
    planner = (repository_root / "tonepoet-pipeline/src/dsd_reference.rs").read_text(encoding="utf-8")
    executor = (repository_root / "src/convert/pipeline/track_executor.rs").read_text(encoding="utf-8")
    qualification = (
        repository_root / "tests/dsd_reference_qualification.rs"
    ).read_text(encoding="utf-8")
    manifest = (repository_root / "src/convert/pipeline/manifest.rs").read_text(encoding="utf-8")
    manifest_builder = (repository_root / "src/convert/pipeline/manifest_builder.rs").read_text(encoding="utf-8")

    route_pattern = re.compile(
        r"ReferenceDecodeRouteRule::new\(\s*"
        r"ReferenceDecodeRoleClass::(\w+),\s*"
        r"PcmBitDepth::(\w+),\s*"
        r"ReferenceDecodeMechanism::(\w+),\s*"
        r"ReferenceSampleHashEncoding::(\w+),?\s*\)",
        re.MULTILINE,
    )
    actual_routes = set(route_pattern.findall(planner))
    expected_routes = {
        ("ReconstructionR64W64", "Float64", "SoxFloat64W64RawStream", "Float64Le"),
        ("TerminalQpcmW64", "Int24", "DirectFfmpeg", "SignedInt24Le"),
        ("TerminalQpcmW64", "Float32", "DirectFfmpeg", "Float32Le"),
        ("TerminalQpcmW64", "Float64", "SoxFloat64W64RawStream", "Float64Le"),
        ("PackagedW64", "Int24", "DirectFfmpeg", "SignedInt24Le"),
        ("PackagedW64", "Float32", "DirectFfmpeg", "Float32Le"),
        ("PackagedW64", "Float64", "SoxFloat64W64RawStream", "Float64Le"),
        ("PackagedNonW64", "Int24", "DirectFfmpeg", "SignedInt24Le"),
        ("PackagedNonW64", "Float32", "DirectFfmpeg", "Float32Le"),
        ("PackagedNonW64", "Float64", "DirectFfmpeg", "Float64Le"),
        ("PostMetadataW64", "Int24", "DirectFfmpeg", "SignedInt24Le"),
        ("PostMetadataW64", "Float32", "DirectFfmpeg", "Float32Le"),
        ("PostMetadataW64", "Float64", "SoxFloat64W64RawStream", "Float64Le"),
        ("PostMetadataNonW64", "Int24", "DirectFfmpeg", "SignedInt24Le"),
        ("PostMetadataNonW64", "Float32", "DirectFfmpeg", "Float32Le"),
        ("PostMetadataNonW64", "Float64", "DirectFfmpeg", "Float64Le"),
    }
    if actual_routes != expected_routes:
        missing = sorted(expected_routes - actual_routes)
        extra = sorted(actual_routes - expected_routes)
        raise AssertionError(
            f"compiled v7 decode route table mismatch: missing={missing}, extra={extra}"
        )
    if len(route_pattern.findall(planner)) != len(expected_routes):
        raise AssertionError("compiled v7 decode route table contains duplicate rules")
    if "interleaved_depth_native_le_sha256" not in planner:
        raise AssertionError("compiled v7 hash format is not depth-native")
    if (
        "pub struct ReferenceDecodeAuthority" not in planner
        or "role: ReferenceDecodedSampleRole" not in planner
        or "pub struct ReferenceDecodedCarrier" not in planner
        or "path: PathBuf" not in planner
        or "pub enum ReferenceDecodedCarrierSelector" not in planner
        or "pub fn bind_decoded_carrier" not in planner
    ):
        raise AssertionError(
            "compiled v7 route authority is not opaque, exact-path, and role-bound"
        )
    if "validate_reference_decode_mechanism" not in planner:
        raise AssertionError("compiled v7 route validator is missing")

    required_planner = [
        "build_float64_wav_package_pipeline",
        "PlannedExecutionStep::Pipeline",
        "Float64 RIFF/RF64 packaging must use the qualified typed stream",
        "CommandEnvironmentPolicy::ClearAndSet",
    ]
    for marker in required_planner:
        if marker not in planner:
            raise AssertionError(f"compiled planner is missing v7 package marker: {marker}")

    if "ReferenceSampleHashRoute" in executor:
        raise AssertionError("executor still exposes caller-selected ReferenceSampleHashRoute")
    required_executor = [
        "build_reference_sample_hash_plan",
        "carrier: &ReferenceDecodedCarrier",
        "ReferenceDecodedCarrierSelector::PackagedOutput",
        "ReferenceDecodedCarrierSelector::TerminalQpcm",
        "ReferenceDecodedCarrierSelector::PostMetadataOutput",
        "bind_decoded_carrier",
        "artifact: &mut super::types::TrackArtifact",
        "build_reference_float64_w64_hash_pipeline",
        "post_metadata_verification_commands",
        "for rule in tonepoet_pipeline::REFERENCE_DECODE_ROUTE_RULES",
        "decode_routes.len()",
        "CommandEnvironmentPolicy::ClearAndSet",
    ]
    for marker in required_executor:
        if marker not in executor:
            raise AssertionError(f"compiled executor is missing v7 authority marker: {marker}")

    carrier_only_signatures = {
        "direct hash builder": (
            r"fn\s+build_reference_direct_hash_command\(\s*"
            r"carrier:\s*&ReferenceDecodedCarrier,\s*"
            r"description:\s*&str,?\s*\)"
        ),
        "Float64 W64 hash builder": (
            r"fn\s+build_reference_float64_w64_hash_pipeline\(\s*"
            r"carrier:\s*&ReferenceDecodedCarrier,\s*"
            r"description:\s*&str,?\s*\)"
        ),
        "sample hash planner": (
            r"fn\s+build_reference_sample_hash_plan\(\s*"
            r"carrier:\s*&ReferenceDecodedCarrier,\s*"
            r"description:\s*&str,?\s*\)"
        ),
    }
    for label, pattern in carrier_only_signatures.items():
        if re.search(pattern, executor, re.MULTILINE) is None:
            raise AssertionError(
                f"compiled {label} does not accept only an exact-path carrier binding"
            )

    post_metadata_signature = (
        r"pub\(crate\)\s+async\s+fn\s+verify_reference_output_after_metadata\(\s*"
        r"artifact:\s*&mut\s+super::types::TrackArtifact,"
    )
    if re.search(post_metadata_signature, executor, re.MULTILINE) is None:
        raise AssertionError(
            "post-metadata verification still accepts a free-form path or evidence object"
        )
    post_metadata_binding = (
        r"bind_decoded_carrier\(\s*"
        r"ReferenceDecodedCarrierSelector::PostMetadataOutput,\s*&path,?\s*\)"
    )
    if re.search(post_metadata_binding, executor, re.MULTILINE) is None:
        raise AssertionError(
            "post-metadata verification does not bind the track artifact path to the plan"
        )

    mislabeled_carrier_regression = (
        r"bind_decoded_carrier\(\s*"
        r"ReferenceDecodedCarrierSelector::PackagedOutput,\s*"
        r"&carrier_summary\.qpcm_path,?\s*\)"
    )
    if re.search(mislabeled_carrier_regression, qualification, re.MULTILINE) is None:
        raise AssertionError(
            "qualification does not reject QPCM W64 presented as a RIFF package carrier"
        )

    if '"sample_identity_oracle": qualification["sample_identity"].clone()' in qualification:
        raise AssertionError("qualification report still copies declarative sample-identity policy")
    required_qualification = [
        "validate_reference_decode_mechanism",
        "forbidden_float64_w64_direct_route_regression",
        "measured_route_case_counts",
        "measured_hash_encoding_case_counts",
        "measured_terminal_realization_route_case_counts",
        '"sample_identity_oracle": sample_identity_oracle',
        "post_metadata_decode_authority",
        "r64_decode_authority",
        "qpcm_decode_authority",
        "packaged_decode_authority",
        "decoded_sample_hash(\n    carrier: &ReferenceDecodedCarrier",
        "ReferenceDecodedCarrierSelector::PostMetadataOutput",
        "mislabeled_carrier_regression",
        "rejected_before_command_construction",
        "for rule in REFERENCE_DECODE_ROUTE_RULES",
        "rule.hash_encoding().key()",
    ]
    for marker in required_qualification:
        if marker not in qualification:
            raise AssertionError(
                f"qualification harness is missing measured route authority: {marker}"
            )

    required_manifest = [
        "executed_evidence_digest_v3",
        'skip_serializing_if = "is_zero_sha256_digest"',
        "Reference v7 track is missing v1, v2, or v3 executed verification authority",
    ]
    required_manifest_builder = [
        r"tonepoet-reference-executed-evidence/v3\0",
        "post_metadata_verification_commands",
        "command.environment_policy",
        "command.environment",
    ]
    for marker in required_manifest:
        if marker not in manifest:
            raise AssertionError(f"compiled manifest is missing v7 authority marker: {marker}")
    for marker in required_manifest_builder:
        if marker not in manifest_builder:
            raise AssertionError(
                f"compiled manifest builder is missing v7 evidence marker: {marker}"
            )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--check",
        action="store_true",
        help="verify both checked-in v7 qualification artifacts",
    )
    args = parser.parse_args()

    if args.check:
        qualification_dir = Path(__file__).resolve().parent
        repository_root = qualification_dir.parent.parent
        current = qualification_dir / "dsd_reference_sox_ng_14_8_0_1_v7.json"
        candidate = qualification_dir / "dsd_reference_sox_ng_14_8_0_1_v7_candidate.json"
        verify_artifact(current, repository_root)
        verify_artifact(candidate, repository_root)
        if current.read_bytes() != candidate.read_bytes():
            raise AssertionError(
                "v7 current and candidate manifests differ before promotion"
            )
        verify_compiled_policy(qualification_dir.parent / "src" / "dsd_reference.rs")
        verify_compiled_v7_routes(repository_root)
        print("v7 terminal-bound derivation and route contracts verified")
    else:
        print(json.dumps(derived_cells(), indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
