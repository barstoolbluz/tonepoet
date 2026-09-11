#!/usr/bin/env python3
"""Portable comparison helpers for offline floating-point qualification.

The frozen Rust coefficient tables remain bit-exact source of truth.  These
helpers are only for comparing a fresh NumPy/SciPy regeneration against those
frozen bits on a different conforming binary64/BLAS implementation.
"""
from __future__ import annotations

import ast
import json
import math
import re
import struct
from pathlib import Path
from typing import Any, Iterable

# The observed cross-BLAS HQ1024 regeneration delta is <= 4.542e-16 absolute.
# 2e-15 is deliberately only a small single-digit-ULP-scale envelope at unit
# magnitude, while leaving room for another conforming BLAS implementation.
BLAS_COEFFICIENT_ABS_TOL = 2.0e-15

# Derived diagnostics can sum thousands of regenerated terms.  They are not
# coefficient authority; the verifier's own hard predicates remain primary.
DERIVED_REPORT_ABS_TOL = 2.0e-12
DERIVED_REPORT_REL_TOL = 2.0e-13

_HQ_FLOAT_CONSTS = (
    "HQ1024_MEASURED_OPERATOR_LINF",
    "HQ1024_HALF_DELAY_COEFFICIENTS",
    "HQ1024_TAIL_COEFFICIENTS",
    "HQ1024_NODE_A_UPPER",
    "HQ1024_NODE_B_UPPER",
)
_HQ_HASH_CONSTS = (
    "HQ1024_FIRST_HALF_FNV1A64",
    "HQ1024_TAIL_FNV1A64",
    "HQ1024_NODE_A_FNV1A64",
    "HQ1024_NODE_B_FNV1A64",
)
_HQ_FNV_LINKS = {
    "first_half_fnv1a64": ("HQ1024_FIRST_HALF_FNV1A64", "HQ1024_HALF_DELAY_COEFFICIENTS"),
    "tail_fnv1a64": ("HQ1024_TAIL_FNV1A64", "HQ1024_TAIL_COEFFICIENTS"),
    "node_a_fnv1a64": ("HQ1024_NODE_A_FNV1A64", "HQ1024_NODE_A_UPPER"),
    "node_b_fnv1a64": ("HQ1024_NODE_B_FNV1A64", "HQ1024_NODE_B_UPPER"),
}


class PortableComparisonError(RuntimeError):
    """Raised when a regeneration differs by more than portable FP noise."""


def _const_rhs_span(text: str, name: str) -> tuple[str, tuple[int, int]]:
    match = re.search(r"\bconst\s+" + re.escape(name) + r"\s*:[^=]+\s*=", text)
    if match is None:
        raise PortableComparisonError(f"missing Rust constant {name}")
    start = match.end()
    end = text.find(";", start)
    if end < 0:
        raise PortableComparisonError(f"unterminated Rust constant {name}")
    return text[start:end].strip(), (start, end)


def rust_const_literal(path: Path, name: str) -> Any:
    rhs, _ = _const_rhs_span(path.read_text(encoding="utf-8"), name)
    try:
        return ast.literal_eval(rhs.replace("_", ""))
    except (SyntaxError, ValueError) as exc:
        raise PortableComparisonError(f"cannot parse Rust constant {name} in {path}") from exc


def rust_u64_const(path: Path, name: str) -> int:
    rhs, _ = _const_rhs_span(path.read_text(encoding="utf-8"), name)
    try:
        return int(rhs.replace("_", ""), 0)
    except ValueError as exc:
        raise PortableComparisonError(f"cannot parse Rust u64 constant {name} in {path}") from exc


def _shape_and_flatten(value: Any) -> tuple[tuple[int, ...], list[float]]:
    if isinstance(value, (int, float)) and not isinstance(value, bool):
        return (), [float(value)]
    if not isinstance(value, (list, tuple)):
        raise PortableComparisonError(f"expected numeric Rust literal, got {type(value).__name__}")
    if not value:
        return (0,), []
    child_shape, first = _shape_and_flatten(value[0])
    flat = list(first)
    for item in value[1:]:
        shape, child = _shape_and_flatten(item)
        if shape != child_shape:
            raise PortableComparisonError("ragged Rust numeric array")
        flat.extend(child)
    return (len(value),) + child_shape, flat


def require_float_sequences_close(
    generated: Iterable[float],
    frozen: Iterable[float],
    *,
    label: str,
    abs_tol: float = BLAS_COEFFICIENT_ABS_TOL,
) -> float:
    """Compare coefficient-like sequences with exact zero/sign topology.

    BLAS drift may perturb last bits of nonzero coefficients.  It must not add
    support where the frozen design has an exact zero, remove support, or flip a
    coefficient sign.  The returned value is the maximum absolute delta.
    """
    left = [float(value) for value in generated]
    right = [float(value) for value in frozen]
    if len(left) != len(right):
        raise PortableComparisonError(
            f"{label}: length differs ({len(left)} regenerated vs {len(right)} frozen)"
        )

    maximum = 0.0
    for index, (actual, expected) in enumerate(zip(left, right)):
        if not math.isfinite(actual) or not math.isfinite(expected):
            raise PortableComparisonError(f"{label}[{index}]: non-finite coefficient")
        if actual == expected:
            continue
        if actual == 0.0 or expected == 0.0:
            raise PortableComparisonError(
                f"{label}[{index}]: exact-zero support changed ({actual!r} vs {expected!r})"
            )
        if math.copysign(1.0, actual) != math.copysign(1.0, expected):
            raise PortableComparisonError(
                f"{label}[{index}]: coefficient sign changed ({actual!r} vs {expected!r})"
            )
        delta = abs(actual - expected)
        maximum = max(maximum, delta)
        if delta > abs_tol:
            raise PortableComparisonError(
                f"{label}[{index}]: |delta|={delta:.17g} exceeds {abs_tol:.17g} "
                f"({actual:.17g} regenerated vs {expected:.17g} frozen)"
            )
    return maximum


def _fnv1a64(values: Iterable[float]) -> int:
    materialized = [float(value) for value in values]
    result = 0xCBF29CE484222325
    prime = 0x100000001B3
    for byte in struct.pack("<Q", len(materialized)):
        result ^= byte
        result = (result * prime) & 0xFFFFFFFFFFFFFFFF
    for value in materialized:
        bits = struct.unpack("<Q", struct.pack("<d", value))[0]
        for byte in struct.pack("<Q", bits):
            result ^= byte
            result = (result * prime) & 0xFFFFFFFFFFFFFFFF
    return result


def _masked_hq_rust(text: str) -> str:
    names = _HQ_FLOAT_CONSTS + _HQ_HASH_CONSTS
    spans: list[tuple[int, int, str]] = []
    for name in names:
        _, (start, end) = _const_rhs_span(text, name)
        spans.append((start, end, name))
    for start, end, name in sorted(spans, reverse=True):
        text = text[:start] + f" __PORTABLE_VALUE_{name}__" + text[end:]
    return text


def validate_hq_rust_self_checks(path: Path) -> None:
    for hash_name, array_name in (
        ("HQ1024_FIRST_HALF_FNV1A64", "HQ1024_HALF_DELAY_COEFFICIENTS"),
        ("HQ1024_TAIL_FNV1A64", "HQ1024_TAIL_COEFFICIENTS"),
        ("HQ1024_NODE_A_FNV1A64", "HQ1024_NODE_A_UPPER"),
        ("HQ1024_NODE_B_FNV1A64", "HQ1024_NODE_B_UPPER"),
    ):
        _, values = _shape_and_flatten(rust_const_literal(path, array_name))
        actual = _fnv1a64(values)
        expected = rust_u64_const(path, hash_name)
        if actual != expected:
            raise PortableComparisonError(
                f"{path}: {hash_name} does not match its own {array_name} bits"
            )


def require_hq_rust_portable(generated: Path, frozen: Path) -> dict[str, float]:
    """Compare regenerated HQ Rust while preserving all non-FP structure exactly."""
    generated_text = generated.read_text(encoding="utf-8")
    frozen_text = frozen.read_text(encoding="utf-8")
    if _masked_hq_rust(generated_text) != _masked_hq_rust(frozen_text):
        raise PortableComparisonError(
            "regenerated HQ Rust changed structure/non-FP data outside portable float/hash payloads"
        )

    validate_hq_rust_self_checks(generated)
    validate_hq_rust_self_checks(frozen)

    deltas: dict[str, float] = {}
    for name in (
        "HQ1024_HALF_DELAY_COEFFICIENTS",
        "HQ1024_TAIL_COEFFICIENTS",
        "HQ1024_NODE_A_UPPER",
        "HQ1024_NODE_B_UPPER",
    ):
        generated_shape, generated_values = _shape_and_flatten(rust_const_literal(generated, name))
        frozen_shape, frozen_values = _shape_and_flatten(rust_const_literal(frozen, name))
        if generated_shape != frozen_shape:
            raise PortableComparisonError(
                f"{name}: shape differs ({generated_shape} regenerated vs {frozen_shape} frozen)"
            )
        deltas[name] = require_float_sequences_close(
            generated_values,
            frozen_values,
            label=name,
        )

    generated_op = float(rust_const_literal(generated, "HQ1024_MEASURED_OPERATOR_LINF"))
    frozen_op = float(rust_const_literal(frozen, "HQ1024_MEASURED_OPERATOR_LINF"))
    op_delta = abs(generated_op - frozen_op)
    if op_delta > DERIVED_REPORT_ABS_TOL:
        raise PortableComparisonError(
            "HQ1024_MEASURED_OPERATOR_LINF: "
            f"|delta|={op_delta:.17g} exceeds {DERIVED_REPORT_ABS_TOL:.17g}"
        )
    deltas["HQ1024_MEASURED_OPERATOR_LINF"] = op_delta
    return deltas


def _load_json(path: Path) -> Any:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise PortableComparisonError(f"cannot read JSON {path}") from exc


def _require_same_keys(left: dict[str, Any], right: dict[str, Any], label: str) -> None:
    if left.keys() != right.keys():
        missing = sorted(set(right) - set(left))
        extra = sorted(set(left) - set(right))
        raise PortableComparisonError(f"{label}: JSON keys differ; missing={missing}, extra={extra}")


def _validate_hq_json_checksums(report: dict[str, Any], rust_path: Path, label: str) -> None:
    checksums = report.get("checksums")
    if not isinstance(checksums, dict):
        raise PortableComparisonError(f"{label}.checksums: expected object")
    expected_keys = set(_HQ_FNV_LINKS) | {"full_sha256"}
    if set(checksums) != expected_keys:
        raise PortableComparisonError(f"{label}.checksums: unexpected checksum fields")
    for json_name, (rust_hash_name, _) in _HQ_FNV_LINKS.items():
        raw = checksums[json_name]
        if not isinstance(raw, str) or not re.fullmatch(r"[0-9a-f]{16}", raw):
            raise PortableComparisonError(f"{label}.checksums.{json_name}: malformed FNV1a64")
        if int(raw, 16) != rust_u64_const(rust_path, rust_hash_name):
            raise PortableComparisonError(
                f"{label}.checksums.{json_name}: does not match generated Rust {rust_hash_name}"
            )
    full_sha = checksums["full_sha256"]
    if not isinstance(full_sha, str) or not re.fullmatch(r"[0-9a-f]{64}", full_sha):
        raise PortableComparisonError(f"{label}.checksums.full_sha256: malformed SHA-256")


def require_hq_json_portable(
    generated_json: Path,
    frozen_json: Path,
    generated_rust: Path,
    frozen_rust: Path,
) -> dict[str, float]:
    """Compare HQ authoring JSON without requiring bit hashes to match across BLAS."""
    generated = _load_json(generated_json)
    frozen = _load_json(frozen_json)
    if not isinstance(generated, dict) or not isinstance(frozen, dict):
        raise PortableComparisonError("HQ candidate JSON root must be an object")
    _require_same_keys(generated, frozen, "hq1024_candidate_coefficients")

    # These portions are integer/rational/static construction data and should
    # remain byte-for-byte equivalent at the parsed-value level.
    for name in ("construction", "geometry", "lagrange_exact"):
        if generated.get(name) != frozen.get(name):
            raise PortableComparisonError(f"HQ candidate JSON exact field changed: {name}")

    generated_measured = generated.get("measured")
    frozen_measured = frozen.get("measured")
    if not isinstance(generated_measured, dict) or not isinstance(frozen_measured, dict):
        raise PortableComparisonError("HQ candidate JSON measured field must be an object")
    _require_same_keys(generated_measured, frozen_measured, "hq1024_candidate_coefficients.measured")
    deltas: dict[str, float] = {}
    for name in generated_measured:
        actual = generated_measured[name]
        expected = frozen_measured[name]
        if not isinstance(actual, (int, float)) or isinstance(actual, bool):
            raise PortableComparisonError(f"measured.{name}: expected numeric value")
        if not isinstance(expected, (int, float)) or isinstance(expected, bool):
            raise PortableComparisonError(f"measured.{name}: frozen value is not numeric")
        delta = abs(float(actual) - float(expected))
        if delta > DERIVED_REPORT_ABS_TOL:
            raise PortableComparisonError(
                f"measured.{name}: |delta|={delta:.17g} exceeds {DERIVED_REPORT_ABS_TOL:.17g}"
            )
        deltas[name] = delta

    _validate_hq_json_checksums(generated, generated_rust, "regenerated")
    _validate_hq_json_checksums(frozen, frozen_rust, "frozen")
    return deltas


def require_json_numeric_close(
    generated_path: Path,
    frozen_path: Path,
    *,
    abs_tol: float = DERIVED_REPORT_ABS_TOL,
    rel_tol: float = DERIVED_REPORT_REL_TOL,
) -> float:
    """Require identical JSON structure/non-floats and tightly close float diagnostics."""
    generated = _load_json(generated_path)
    frozen = _load_json(frozen_path)
    maximum = 0.0

    def compare(actual: Any, expected: Any, path: str) -> None:
        nonlocal maximum
        if isinstance(expected, bool) or expected is None or isinstance(expected, (str, int)):
            if type(actual) is not type(expected) or actual != expected:
                raise PortableComparisonError(f"{path}: exact JSON value changed")
            return
        if isinstance(expected, float):
            if not isinstance(actual, float):
                raise PortableComparisonError(f"{path}: expected floating diagnostic")
            actual_float = float(actual)
            if not math.isfinite(actual_float) or not math.isfinite(expected):
                if actual_float != expected:
                    raise PortableComparisonError(f"{path}: non-finite JSON value changed")
                return
            delta = abs(actual_float - expected)
            maximum = max(maximum, delta)
            if not math.isclose(actual_float, expected, rel_tol=rel_tol, abs_tol=abs_tol):
                raise PortableComparisonError(
                    f"{path}: |delta|={delta:.17g} exceeds numeric report tolerance "
                    f"(abs={abs_tol:.17g}, rel={rel_tol:.17g})"
                )
            return
        if isinstance(expected, list):
            if not isinstance(actual, list) or len(actual) != len(expected):
                raise PortableComparisonError(f"{path}: JSON list geometry changed")
            for index, (actual_item, expected_item) in enumerate(zip(actual, expected)):
                compare(actual_item, expected_item, f"{path}[{index}]")
            return
        if isinstance(expected, dict):
            if not isinstance(actual, dict):
                raise PortableComparisonError(f"{path}: JSON object became {type(actual).__name__}")
            _require_same_keys(actual, expected, path)
            for key in expected:
                compare(actual[key], expected[key], f"{path}.{key}" if path else key)
            return
        raise PortableComparisonError(f"{path}: unsupported JSON value type {type(expected).__name__}")

    compare(generated, frozen, "")
    return maximum
