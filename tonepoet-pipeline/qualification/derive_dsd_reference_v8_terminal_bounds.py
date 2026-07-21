#!/usr/bin/env python3
"""Derive and verify the policy-v8 terminal true-peak bounds.

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
    "float64": 2_147_487_744,
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
    if document.get("schema_version") != 8:
        raise AssertionError(f"{path}: expected schema_version 8")
    if document.get("policy") != "sox_ng_14_8_0_1_v8":
        raise AssertionError(f"{path}: unexpected policy identity")
    if document.get("status") != "qualification_candidate":
        raise AssertionError(f"{path}: v8 must remain an unpromoted candidate")

    historical_v7_path = path.parent / "dsd_reference_sox_ng_14_8_0_1_v7.json"
    historical_v7 = json.loads(historical_v7_path.read_text(encoding="utf-8"))
    for field in (
        "sox_ng",
        "ffmpeg",
        "in_process",
        "analyzer",
        "subprocess_environment",
        "qualification_supervision",
        "profiles",
        "cell_contract",
        "riff_capacity",
        "packaging",
        "sample_identity",
    ):
        if document.get(field) != historical_v7.get(field):
            raise AssertionError(f"{path}: inherited v7 field {field!r} changed")
    for field in (
        "target_rates_hz",
        "derivation_schema",
        "int16_shibata",
        "int24_tpdf",
        "float32",
        "post_final_acceptance_reserve_db",
        "post_final_acceptance_reserve_basis",
    ):
        if document["terminal_bounds"].get(field) != historical_v7["terminal_bounds"].get(field):
            raise AssertionError(f"{path}: inherited terminal field {field!r} changed")

    supported_target_depth_cells = [
        (cell["target"], cell["depth"])
        for cell in document["cell_contract"]["target_depth_cells"]
        if cell["result"] == "supported"
    ]
    expected_supported_target_depth_cells = [
        ("flac_native", "int24"),
        ("wav_riff", "int24"),
        ("wav_riff", "float32"),
        ("wav_riff", "float64"),
        ("wav_rf64", "int24"),
        ("wav_rf64", "float32"),
        ("wav_rf64", "float64"),
        ("wav_w64", "int24"),
        ("wav_w64", "float32"),
        ("wav_w64", "float64"),
        ("aiff_native", "int24"),
        ("wavpack_native", "int24"),
        ("alac_m4a", "int24"),
    ]
    if supported_target_depth_cells != expected_supported_target_depth_cells:
        raise AssertionError(
            f"{path}: enabled target/depth cells changed: {supported_target_depth_cells!r}"
        )
    rejected_int16 = [
        cell for cell in document["cell_contract"]["target_depth_cells"]
        if cell["depth"] == "int16"
    ]
    if not rejected_int16 or any(
        cell["result"] != "error:DSD-REF-P0-022" for cell in rejected_int16
    ):
        raise AssertionError(f"{path}: Int16 cells are not uniformly rejected by P0-022")

    expected_activation = (
        "Fail closed unless status is qualified_release and release certification binds "
        "this exact v8 candidate snapshot and the exact schema-version-8 machine report "
        "generated by the mandatory commissioned real-tool gate."
    )
    if document.get("runtime_activation") != expected_activation:
        raise AssertionError(f"{path}: v8 runtime activation text is stale or noncanonical")
    release = document.get("release_certification", {})
    if release != {
        "schema": "tonepoet-dsd-reference-release-certification/v1",
        "path": "tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v8_certification.json",
        "candidate_manifest_path": "tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v8_candidate.json",
        "report_sha256": None,
        "candidate_manifest_sha256": None,
    }:
        raise AssertionError(f"{path}: v8 release-certification descriptor is noncanonical")

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

    expected_realizations = {
        "int24_tpdf": "int24-tpdf-2lsb",
        "float32": "float32-2^-23",
        "float64": "float64-sox-s32-effects-half-lsb-plus-f64-2^-51",
    }
    for name, realization in expected_realizations.items():
        if terminal[name].get("realization") != realization:
            raise AssertionError(
                f"{path}: {name}.realization={terminal[name].get('realization')!r}, "
                f"expected {realization!r}"
            )
    basis = document.get("qualification_basis", "")
    for marker in ("signed-32-bit effects", "2^-32", "Int24", "Float32", "no enabled cell"):
        if marker not in basis:
            raise AssertionError(f"{path}: qualification basis omits {marker!r}")

    certification_path = repository_root / release["path"]
    certification = json.loads(certification_path.read_text(encoding="utf-8"))
    if certification != {
        "schema_version": 8,
        "policy": "sox_ng_14_8_0_1_v8",
        "status": "not_run",
        "outcome": "not_run",
        "note": (
            "Policy v8 is a source-controlled qualification candidate. Run the mandatory "
            "pinned real-tool gate and bind its exact report before promotion."
        ),
    }:
        raise AssertionError(f"{certification_path}: invalid v8 not-run certification stub")


def verify_compiled_policy(path: Path) -> None:
    source = path.read_text(encoding="utf-8")
    for marker in (
        'pub const DSD_REFERENCE_POLICY_V8_KEY: &str = "sox_ng_14_8_0_1_v8";',
        '"qualification/dsd_reference_sox_ng_14_8_0_1_v8.json"',
        '"float64-sox-s32-effects-half-lsb-plus-f64-2^-51"',
        'Reference policy sox_ng_14_8_0_1_v8',
    ):
        if marker not in source:
            raise AssertionError(f"{path}: missing compiled v8 marker {marker!r}")
    stale_current_errors = [
        line for line in source.splitlines()
        if "DSD-REF-" in line and "sox_ng_14_8_0_1_v7" in line
    ]
    if stale_current_errors:
        raise AssertionError(f"{path}: current error authority still names v7: {stale_current_errors}")
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


def verify_compiled_v8_routes(repository_root: Path) -> None:
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
            f"compiled v8 decode route table mismatch: missing={missing}, extra={extra}"
        )
    if len(route_pattern.findall(planner)) != len(expected_routes):
        raise AssertionError("compiled v8 decode route table contains duplicate rules")
    if "interleaved_depth_native_le_sha256" not in planner:
        raise AssertionError("compiled v8 hash format is not depth-native")
    if (
        "pub struct ReferenceDecodeAuthority" not in planner
        or "role: ReferenceDecodedSampleRole" not in planner
        or "pub struct ReferenceDecodedCarrier" not in planner
        or "path: PathBuf" not in planner
        or "pub enum ReferenceDecodedCarrierSelector" not in planner
        or "pub fn bind_decoded_carrier" not in planner
    ):
        raise AssertionError(
            "compiled v8 route authority is not opaque, exact-path, and role-bound"
        )
    if "validate_reference_decode_mechanism" not in planner:
        raise AssertionError("compiled v8 route validator is missing")

    required_planner = [
        "build_float64_wav_package_pipeline",
        "PlannedExecutionStep::Pipeline",
        "Float64 RIFF/RF64 packaging must use the qualified typed stream",
        "CommandEnvironmentPolicy::ClearAndSet",
    ]
    for marker in required_planner:
        if marker not in planner:
            raise AssertionError(f"compiled planner is missing v8 package marker: {marker}")

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
            raise AssertionError(f"compiled executor is missing v8 authority marker: {marker}")

    if not any(
        marker in executor
        for marker in ("manifest.schema_version != 8", "manifest.schema_version != 9", "manifest.schema_version != 10", "manifest.schema_version != 11", "manifest.schema_version != 12")
    ):
        raise AssertionError(
            "compiled executor has no recognized v8-or-later manifest-schema guard"
        )

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
    for marker in (
        "terminal_observed_max_error_by_depth",
        "terminal_effects_boundary_audit",
        '"enabled_cells_rejected": 0',
        '"float64_disposition": "corrected_to_2^-32_plus_2^-51"',
        "terminal_effects_source_proof",
        "tonepoet-reference-terminal-effects-source-proof/v1",
        "dsd_reference_sox_ng_14_8_0_1_v8_terminal_source_proof.md",
        "gain.c:flow_gain:SOX_ROUND_CLIP_COUNT(*ibuf * mult, effp->clips)",
        '"gain_mode_scope": ["reference_compensated", "native_level_exact", "fixed_exact"]',
        '"proof_sha256": sha256_hex(include_bytes!(concat!(',
    ):
        if marker not in qualification:
            raise AssertionError(
                f"qualification harness is missing v8 effects-boundary audit marker: {marker}"
            )

    validator_start = executor.find("fn validate_embedded_release_certification(")
    validator_end = executor.find(
        "\nfn validate_embedded_reference_policy_tables(", validator_start
    )
    if validator_start < 0 or validator_end < 0:
        raise AssertionError("compiled executor has no bounded release-certification validator")
    validator_body = executor[validator_start:validator_end]
    if "validate_terminal_effects_certification(package_evidence, manifest)?" not in validator_body:
        raise AssertionError(
            "release-certification validator does not consume the v8 F4 evidence validator"
        )

    effects_validator_start = executor.find("fn validate_terminal_effects_certification(")
    if effects_validator_start < 0 or effects_validator_start >= validator_start:
        raise AssertionError("compiled executor has no terminal-effects certification validator")
    effects_validator_body = executor[effects_validator_start:validator_start]
    required_effects_validation = (
        "expected_package_fields",
        "terminal_observed_max_error_by_depth",
        'BTreeSet::from(["int24", "float32", "float64"])',
        ".is_finite()",
        "terminal_realization_bound(rate_hz, depth)",
        "max_added_peak_fs_q63_ceil",
        "maximum > compiled_maximum",
        "terminal_effects_boundary_audit",
        'Some("signed_q1_31")',
        'Some("2^-32")',
        'Some("2^-51")',
        'Some("2^-32_plus_2^-51")',
        'Some("retained_2^-22_bound_contains_effects_rounding")',
        'Some("retained_2^-23_bound_contains_effects_and_carrier_rounding")',
        'Some("corrected_to_2^-32_plus_2^-51")',
        'get("enabled_cells_rejected")',
        "!= Some(0)",
        "terminal_effects_source_proof",
        "expected_source_proof_fields",
        "Sha256Digest::of_bytes(source_proof_bytes).to_hex()",
        "source_proof_sha256.as_str()",
        'Some("324b8cf873fd7836e8848bd87f7a90d8faa6f849")',
        'Some("sha256-LjGx+yaWi5EcZsXhTmdRaf9utFXcCXASMmjRtm6vUc8=")',
        'Some("exact_for_every_sox_sample_t_grid_value")',
        'Some("one_half_internal_sample_equals_2^-32_fs")',
        'Some("reference_compensated")',
        'Some("native_level_exact")',
        'Some("fixed_exact")',
    )
    for marker in required_effects_validation:
        if marker not in effects_validator_body:
            raise AssertionError(
                f"compiled release validator is missing v8 F4 semantic check: {marker}"
            )

    source_proof = repository_root / (
        "tonepoet-pipeline/qualification/"
        "dsd_reference_sox_ng_14_8_0_1_v8_terminal_source_proof.md"
    )
    if not source_proof.is_file():
        raise AssertionError("v8 terminal-effects source proof is missing")
    source_proof_text = source_proof.read_text(encoding="utf-8")
    for marker in (
        "324b8cf873fd7836e8848bd87f7a90d8faa6f849",
        "sha256-LjGx+yaWi5EcZsXhTmdRaf9utFXcCXASMmjRtm6vUc8=",
        "typedef sox_int32_t sox_sample_t",
        "SOX_SAMPLE_TO_FLOAT_64BIT",
        "SOX_FLOAT_64BIT_TO_SAMPLE",
        "SOX_ROUND_CLIP_COUNT(*ibuf * mult, effp->clips)",
        "priv_t.fixed_gain",
        "dB_to_linear",
        "exactly one sample-domain conversion",
        "NormalizePeak",
        "ReferenceCompensated",
        "NativeLevelExact",
        "FixedExact",
        "2^-32 + 2^-51",
        "2147487744",
    ):
        if marker not in source_proof_text:
            raise AssertionError(f"v8 terminal-effects source proof is incomplete: {marker}")

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
        "Reference v7+ track is missing v1, v2, or v3 executed verification authority",
    ]
    required_manifest_builder = [
        r"tonepoet-reference-executed-evidence/v3\0",
        "post_metadata_verification_commands",
        "command.environment_policy",
        "command.environment",
    ]
    for marker in required_manifest:
        if marker not in manifest:
            raise AssertionError(f"compiled manifest is missing v8 authority marker: {marker}")
    for marker in required_manifest_builder:
        if marker not in manifest_builder:
            raise AssertionError(
                f"compiled manifest builder is missing v8 evidence marker: {marker}"
            )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--check",
        action="store_true",
        help="verify both checked-in v8 qualification artifacts",
    )
    args = parser.parse_args()

    if args.check:
        qualification_dir = Path(__file__).resolve().parent
        repository_root = qualification_dir.parent.parent
        current = qualification_dir / "dsd_reference_sox_ng_14_8_0_1_v8.json"
        candidate = qualification_dir / "dsd_reference_sox_ng_14_8_0_1_v8_candidate.json"
        verify_artifact(current, repository_root)
        verify_artifact(candidate, repository_root)
        if current.read_bytes() != candidate.read_bytes():
            raise AssertionError(
                "v8 current and candidate manifests differ before promotion"
            )
        compiled_policy = qualification_dir.parent / "src" / "dsd_reference.rs"
        compiled_source = compiled_policy.read_text(encoding="utf-8")
        current_manifests = (
            '"qualification/dsd_reference_sox_ng_14_8_0_1_v8.json"',
            '"qualification/dsd_reference_sox_ng_14_8_0_1_v9.json"',
            '"qualification/dsd_reference_sox_ng_14_8_0_1_v10.json"',
            '"qualification/dsd_reference_sox_ng_14_8_0_1_v11.json"',
            '"qualification/dsd_reference_sox_ng_14_8_0_1_v12.json"',
        )
        active = [marker for marker in current_manifests if marker in compiled_source]
        if len(active) != 1:
            raise AssertionError(
                f"{compiled_policy}: expected exactly one recognized current policy manifest, got {active}"
            )
        if active[0] == current_manifests[0]:
            verify_compiled_policy(compiled_policy)
        else:
            inherited_source = "\n".join(
                (
                    compiled_source,
                    (repository_root / "tests/dsd_reference_qualification.rs").read_text(
                        encoding="utf-8"
                    ),
                    (repository_root / "src/convert/pipeline/track_executor.rs").read_text(
                        encoding="utf-8"
                    ),
                )
            )
            for marker in (
                'pub const DSD_REFERENCE_POLICY_V8_KEY: &str = "sox_ng_14_8_0_1_v8";',
                'SoxNg14801V8',
                '"tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v8_terminal_source_proof.md"',
            ):
                if marker not in inherited_source:
                    raise AssertionError(
                        f"{repository_root}: append-only v9 policy dropped inherited v8 authority {marker!r}"
                    )
        verify_compiled_v8_routes(repository_root)
        print("v8 terminal-bound derivation, effects-boundary audit, and route contracts verified")
    else:
        print(json.dumps(derived_cells(), indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
