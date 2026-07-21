#!/usr/bin/env python3
"""Derive and verify the policy-v5 terminal true-peak bounds.

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


def verify_artifact(path: Path) -> None:
    document = json.loads(path.read_text(encoding="utf-8"))
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
        help="verify both checked-in v5 qualification artifacts",
    )
    args = parser.parse_args()

    if args.check:
        qualification_dir = Path(__file__).resolve().parent
        for filename in (
            "dsd_reference_sox_ng_14_8_0_1_v5.json",
            "dsd_reference_sox_ng_14_8_0_1_v5_candidate.json",
        ):
            verify_artifact(qualification_dir / filename)
        compiled_policy = qualification_dir.parent / "src" / "dsd_reference.rs"
        compiled_source = compiled_policy.read_text(encoding="utf-8")
        current_manifests = (
            '"qualification/dsd_reference_sox_ng_14_8_0_1_v5.json"',
            '"qualification/dsd_reference_sox_ng_14_8_0_1_v6.json"',
            '"qualification/dsd_reference_sox_ng_14_8_0_1_v7.json"',
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
        print("v5 terminal-bound derivation verified")
    else:
        print(json.dumps(derived_cells(), indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
