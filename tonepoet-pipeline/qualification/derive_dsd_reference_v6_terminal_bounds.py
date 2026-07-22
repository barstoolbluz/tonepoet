#!/usr/bin/env python3
"""Derive and verify the policy-v6 terminal true-peak bounds.

The calculation is intentionally standard-library-only and uses Decimal at
120-digit precision. For each qualified terminal realization it computes:

    A = 10 ** ((C - R) / 20)
    S = 20 * log10(A - epsilon)

where C is the public post-final ceiling, R is exactly one analyzer reporting
quantum, and epsilon is the upward-rounded Q1.63 additive peak bound. S is
rounded toward negative infinity to one nanodecibel.

Historical-checker lineage contract: once shipped, this checker must remain valid
against every successor policy. It may pin immutable artifacts and persistent
policy identities from its own generation, but it must never assert the mutable
current-policy embed pointer.
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
    if document.get("schema_version") != 6:
        raise AssertionError(f"{path}: expected schema_version 6")
    if document.get("policy") != "sox_ng_14_8_0_1_v6":
        raise AssertionError(f"{path}: unexpected policy identity")
    if document.get("status") != "qualification_candidate":
        raise AssertionError(f"{path}: v6 must remain an unpromoted candidate")

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


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--check",
        action="store_true",
        help="verify both checked-in v6 qualification artifacts",
    )
    args = parser.parse_args()

    if args.check:
        qualification_dir = Path(__file__).resolve().parent
        repository_root = qualification_dir.parent.parent
        current = qualification_dir / "dsd_reference_sox_ng_14_8_0_1_v6.json"
        candidate = qualification_dir / "dsd_reference_sox_ng_14_8_0_1_v6_candidate.json"
        verify_artifact(current, repository_root)
        verify_artifact(candidate, repository_root)
        if current.read_bytes() != candidate.read_bytes():
            raise AssertionError(
                "v6 current and candidate manifests differ before promotion"
            )
        compiled_policy = qualification_dir.parent / "src" / "dsd_reference.rs"
        compiled_source = compiled_policy.read_text(encoding="utf-8")
        for marker in (
            'pub const DSD_REFERENCE_POLICY_V6_KEY: &str = "sox_ng_14_8_0_1_v6";',
            "SoxNg14801V6",
        ):
            if marker not in compiled_source:
                raise AssertionError(
                    f"{compiled_policy}: append-only policy v6 identity is missing {marker!r}"
                )
        print("v6 terminal-bound derivation verified")
    else:
        print(json.dumps(derived_cells(), indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
