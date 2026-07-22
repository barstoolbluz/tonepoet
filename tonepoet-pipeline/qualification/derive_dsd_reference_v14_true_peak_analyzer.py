#!/usr/bin/env python3
"""Verify append-only policy-v14 oversampled true-peak analyzer authority.

Historical-checker lineage contract: once shipped, this checker must remain valid
against every successor policy. It may pin immutable artifacts and persistent
policy identities from its own generation, but it must never assert the mutable
current-policy embed pointer.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
from pathlib import Path

V13_HASHES = {
    "derive_dsd_reference_v13_streamed_wav_header.py": "6cdddbb93d010f0d22b3dd8fe604229603574e9bc1d118ec6a40e47367ec33f1",
    "dsd_reference_sox_ng_14_8_0_1_v13.json": "9747e100c1febd5527130ab8b3c0d232d279bdc67463e32619c086460e188de6",
    "dsd_reference_sox_ng_14_8_0_1_v13_candidate.json": "9747e100c1febd5527130ab8b3c0d232d279bdc67463e32619c086460e188de6",
    "dsd_reference_sox_ng_14_8_0_1_v13_certification.json": "bbbc872f1638a80e96cc5008872cdabff5e3358693a07cbb06c74faa7ac41492",
    "dsd_reference_sox_ng_14_8_0_1_v13_report.md": "85138b0945d22ce79ba11d12ed555f5f9150ae082873ce625d0ba7702a0f792c",
}

RATES = [44_100, 48_000, 88_200, 96_000, 176_400, 192_000, 352_800, 384_000, 705_600, 768_000]
FIXED_FREQUENCIES = [1_000, 20_000, 48_000, 70_000]
OVERSAMPLE_FACTOR = 16
GRID_BOUND_DB = 0.041925957
REPORTING_UNCERTAINTY_DB = 0.010000000
ANALYZER_RESIDUAL_DB = 0.100000000
REQUIRED_CASE_COUNT = 1_968


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def require(text: str, marker: str, label: str) -> None:
    if marker not in text:
        raise AssertionError(f"{label}: missing {marker!r}")


def verify(root: Path) -> None:
    q = root / "tonepoet-pipeline/qualification"
    for name, expected in V13_HASHES.items():
        actual = sha256(q / name)
        if actual != expected:
            raise AssertionError(f"append-only v13 artifact changed: {name}: {actual}")

    v13 = json.loads((q / "dsd_reference_sox_ng_14_8_0_1_v13.json").read_text())
    current_path = q / "dsd_reference_sox_ng_14_8_0_1_v14.json"
    candidate_path = q / "dsd_reference_sox_ng_14_8_0_1_v14_candidate.json"
    current_bytes = current_path.read_bytes()
    candidate_bytes = candidate_path.read_bytes()
    if current_bytes != candidate_bytes:
        raise AssertionError("v14 current and preserved candidate are not byte-identical")
    v14 = json.loads(current_bytes)
    if v14.get("schema_version") != 14 or v14.get("policy") != "sox_ng_14_8_0_1_v14":
        raise AssertionError("v14 schema/policy identity is noncanonical")
    if v14.get("status") != "qualification_candidate":
        raise AssertionError("v14 must remain an unpromoted candidate")

    changed = {
        "schema_version",
        "policy",
        "analyzer",
        "qualification_basis",
        "runtime_activation",
        "qualification_report",
        "release_certification",
    }
    for key in sorted(set(v13) | set(v14)):
        if key not in changed and v13.get(key) != v14.get(key):
            raise AssertionError(f"v14 changed inherited v13 field {key!r}")

    analyzer = v14["analyzer"]
    if analyzer["reporting_uncertainty_db"] != f"{REPORTING_UNCERTAINTY_DB:.9f}":
        raise AssertionError("v14 widened or changed reporting uncertainty")
    if analyzer["analyzer_residual_db"] != f"{ANALYZER_RESIDUAL_DB:.9f}":
        raise AssertionError("v14 widened or changed analyzer residual")
    if analyzer["qualification_schema"] != "tonepoet-reference-analyzer-qualification/v5":
        raise AssertionError("v14 analyzer qualification schema is noncanonical")
    if analyzer["required_case_count"] != REQUIRED_CASE_COUNT:
        raise AssertionError("v14 analyzer case count is noncanonical")
    if analyzer["target_rates_hz"] != RATES:
        raise AssertionError("v14 target-rate matrix drifted")
    if analyzer["fixed_frequencies_hz"] != FIXED_FREQUENCIES:
        raise AssertionError("v14 fixed-frequency matrix drifted")
    if analyzer["fixed_frequency_max_normalized"] != "0.490000000":
        raise AssertionError("v14 fixed-frequency admission ratio drifted")
    if analyzer["fixed_frequency_duration_seconds"] != "0.250000000":
        raise AssertionError("v14 fixed-frequency duration drifted")
    if analyzer["peak_positions"] != ["early", "late"]:
        raise AssertionError("v14 must retain early and tail-position qualification")
    if analyzer["waveform_families"] != [
        "single_tone",
        "fixed_frequency_single_tone",
        "phase_aligned_multitone",
    ]:
        raise AssertionError("v14 waveform family authority drifted")

    fixed_rate_frequency_cells = sum(
        1
        for rate in RATES
        for frequency in FIXED_FREQUENCIES
        if frequency / rate <= 0.49
    )
    fixed_cases = fixed_rate_frequency_cells * 2 * 2 * 3 * 2
    normalized_cases = len(RATES) * 2 * 2 * 2 * 3 * 2 * 2
    multitone_cases = len(RATES) * 2 * 2 * 3 * 2
    if (fixed_rate_frequency_cells, fixed_cases, normalized_cases, multitone_cases) != (
        32,
        768,
        960,
        240,
    ):
        raise AssertionError("v14 qualification matrix arithmetic drifted")
    if normalized_cases + fixed_cases + multitone_cases != REQUIRED_CASE_COUNT:
        raise AssertionError("v14 qualification case total is inconsistent")

    carrier = analyzer["carrier"]
    expected_carrier = {
        "schema": "tonepoet-reference-analyzer-carrier/v4",
        "source_container": "carrier_sensitive_w64",
        "producer_tool": "ffmpeg",
        "producer_args_template": [
            "-nostdin", "-hide_banner", "-nostats", "-loglevel", "error", "-i",
            "{carrier_w64}", "-map", "0:a:0", "-vn", "-sn", "-dn", "-c:a",
            "pcm_f64le", "-f", "f64le", "pipe:1",
        ],
        "environment_policy": "clear_and_set",
        "environment": {"LC_ALL": "C"},
        "transport": "direct_stdout_to_stdin_no_shell",
        "consumer_tool": "sox_ng",
        "consumer_input_args": [
            "-t", "raw", "-e", "floating-point", "-b", "64", "-L", "-r",
            "{sample_rate_hz}", "-c", "{channels}", "-",
        ],
        "consumer_args_template": [
            "-S", "-D", "-t", "raw", "-e", "floating-point", "-b", "64", "-L",
            "-r", "{sample_rate_hz}", "-c", "{channels}", "-", "-n", "rate", "-v",
            "-L", "-s", "{sample_rate_hz_x16}", "stats",
        ],
        "parser": "sox_stats_pk_lev_db_v1",
        "stream_encoding": "pcm_f64le",
        "stream_header": "headerless_raw_pcm",
        "disk_intermediate": False,
        "exact_recontainer": False,
        "overflow_fixture_required": False,
        "overflow_behavior": "not_applicable_to_v14_analyzer",
        "known_ffmpeg_w64_defect": "ffmpeg_7_1_scales_sox_ieee_float64_w64_by_2^31",
        "routing_rule": "float32_w64_ffmpeg_f64le_raw_to_sox_else_sox_path",
        "direct_float32_input": "ffmpeg_direct_w64_to_headerless_f64le_stdout",
        "direct_float32_consumer_args_template": [
            "-S", "-D", "-t", "raw", "-e", "floating-point", "-b", "64", "-L",
            "-r", "{sample_rate_hz}", "-c", "{channels}", "-", "-n", "rate", "-v",
            "-L", "-s", "{sample_rate_hz_x16}", "stats",
        ],
        "known_sox_float32_w64_defect": "sox_ng_14_8_0_1_misscales_its_float32_w64_on_decode",
        "direct_tool": "sox_ng",
        "direct_args_template": [
            "-S", "-D", "{carrier_w64}", "-n", "rate", "-v", "-L", "-s",
            "{sample_rate_hz_x16}", "stats",
        ],
        "oversample_factor": OVERSAMPLE_FACTOR,
        "oversampled_rate_rule": "sample_rate_hz * oversample_factor",
        "analytic_grid_bound_db": f"{GRID_BOUND_DB:.9f}",
    }
    if carrier != expected_carrier:
        raise AssertionError("v14 analyzer carrier contract is noncanonical")

    exact_grid_bound = -20.0 * math.log10(math.cos(math.pi / (2.0 * OVERSAMPLE_FACTOR)))
    if not (GRID_BOUND_DB >= exact_grid_bound and GRID_BOUND_DB - exact_grid_bound < 1e-9):
        raise AssertionError("v14 analytic grid bound is not a conservative nanodecibel rounding")
    if GRID_BOUND_DB > ANALYZER_RESIDUAL_DB:
        raise AssertionError("v14 grid bound is not contained in inherited analyzer residual")
    if REPORTING_UNCERTAINTY_DB + ANALYZER_RESIDUAL_DB != 0.11:
        raise AssertionError("v14 Q+E authority changed")

    report_path = q / "dsd_reference_sox_ng_14_8_0_1_v14_report.md"
    inherited_report = dict(v13["qualification_report"])
    inherited_report["path"] = str(report_path.relative_to(root))
    inherited_report["sha256"] = sha256(report_path)
    if v14["qualification_report"] != inherited_report:
        raise AssertionError("v14 qualification-report authority is noncanonical")

    release = {
        "schema": "tonepoet-dsd-reference-release-certification/v1",
        "path": "tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v14_certification.json",
        "candidate_manifest_path": "tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v14_candidate.json",
        "report_sha256": None,
        "candidate_manifest_sha256": None,
    }
    if v14["release_certification"] != release:
        raise AssertionError("v14 release descriptor is noncanonical")
    certification = json.loads((q / "dsd_reference_sox_ng_14_8_0_1_v14_certification.json").read_text())
    if certification != {
        "schema_version": 14,
        "policy": "sox_ng_14_8_0_1_v14",
        "status": "not_run",
        "outcome": "not_run",
        "note": "Policy v14 is a source-controlled qualification candidate. Run the mandatory pinned real-tool gate and bind its exact report before promotion.",
    }:
        raise AssertionError("v14 certification stub is noncanonical")

    planner = (root / "tonepoet-pipeline/src/dsd_reference.rs").read_text()
    findings = (root / "docs/findings_dsd_reference_p0_admission_round.md").read_text()

    for marker in (
        'pub const DSD_REFERENCE_POLICY_V14_KEY: &str = "sox_ng_14_8_0_1_v14";',
        "SoxNg14801V14",
        "REFERENCE_TRUE_PEAK_OVERSAMPLE_FACTOR: u32 = 16",
        "REFERENCE_TRUE_PEAK_GRID_BOUND: DbNano = DbNano(41_925_957)",
        "SoxStatsPkLevDbV1",
        "extract_single_sox_stats_peak_report",
        "parse_reference_sox_stats_true_peak_measurement",
        '"pcm_f64le".to_string()',
        '"f64le".to_string()',
        '"stats".to_string()',
    ):
        require(planner, marker, "planner")
    report = report_path.read_text()
    for marker in (
        "F8 correction",
        "measurement-only",
        "0.041925957 dB",
        "0.110000000 dB",
        "1,968 cases",
        "352.8/384 kHz",
        "byte-identical",
    ):
        require(report, marker, "v14 report")
    require(findings, "F8 resolution (policy v14 candidate, 2026-07-21)", "findings")

    print("policy v14 oversampled true-peak analyzer derivation verified")


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
