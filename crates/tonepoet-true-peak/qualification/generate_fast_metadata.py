#!/usr/bin/env python3
"""Derive deterministic Fast066 execution metadata from the frozen HQ1024 bank.

This generator does not modify the waveform coefficient source.  The runtime
intentionally derives the same small metadata set from that frozen bank once per
process; this file exists so the derivation is independently reproducible and
byte-comparable in offline qualification.
"""
from __future__ import annotations

import argparse
import ast
import hashlib
import importlib.util
import json
import math
import re
from fractions import Fraction
from pathlib import Path

import numpy as np

CRATE_ROOT = Path(__file__).resolve().parents[1]
COEFFICIENT_SOURCE = CRATE_ROOT / "src" / "hq1024_coefficients.rs"
GENERATOR_SOURCE = CRATE_ROOT / "qualification" / "generate_hq1024.py"
MIDPOINT_PHASE = 256
MIDPOINT_NONZERO_START = 5
MIDPOINT_NONZERO_END_EXCLUSIVE = 29


def rust_array(text: str, name: str) -> np.ndarray:
    match = re.search(r"const\s+" + re.escape(name) + r"\s*:[^=]+=", text)
    if match is None:
        raise RuntimeError(f"missing Rust constant {name}")
    start = text.index("[", match.end())
    end = text.index(";", start)
    return np.asarray(ast.literal_eval(text[start:end].replace("_", "")), dtype=np.float64)


def outward_sum(values: np.ndarray) -> float:
    total = 0.0
    for raw in values:
        value = abs(float(raw))
        if not math.isfinite(value):
            raise RuntimeError("non-finite coefficient")
        if value == 0.0:
            continue
        total = math.nextafter(total + value, math.inf)
    if not math.isfinite(total):
        raise RuntimeError("L1 sum overflow")
    return total


def load_hq_generator():
    spec = importlib.util.spec_from_file_location("tonepoet_generate_hq1024", GENERATOR_SOURCE)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot import {GENERATOR_SOURCE}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def fraction_hex(value: Fraction) -> dict[str, str]:
    # Fractions are exact and enormous.  Decimal strings remain deterministic
    # while avoiding a lossy float conversion in the qualification artifact.
    return {"numerator": str(value.numerator), "denominator": str(value.denominator)}


def derive() -> dict[str, object]:
    source_bytes = COEFFICIENT_SOURCE.read_bytes()
    text = source_bytes.decode("utf-8")
    bank = rust_array(text, "HQ1024_TAIL_COEFFICIENTS")
    node_a = rust_array(text, "HQ1024_NODE_A_UPPER")
    node_b = rust_array(text, "HQ1024_NODE_B_UPPER")

    if bank.shape != (512, 34):
        raise RuntimeError(f"unexpected HQ1024 tail shape: {bank.shape}")
    midpoint = bank[MIDPOINT_PHASE]
    nonzero = np.flatnonzero(midpoint != 0.0)
    expected = np.arange(MIDPOINT_NONZERO_START, MIDPOINT_NONZERO_END_EXCLUSIVE)
    if not np.array_equal(nonzero, expected):
        raise RuntimeError(f"midpoint support changed: {nonzero.tolist()}")
    active = midpoint[MIDPOINT_NONZERO_START:MIDPOINT_NONZERO_END_EXCLUSIVE]
    if not np.array_equal(active, active[::-1]):
        raise RuntimeError("midpoint row lost exact binary64 symmetry")

    generator = load_hq_generator()
    children: list[dict[str, object]] = []
    for node, start, end in ((2, 0, 256), (3, 256, 512)):
        exact_a, exact_b = generator.exact_node_bound(bank, start, end)
        stored_a = Fraction.from_float(float(node_a[node]))
        stored_b = Fraction.from_float(float(node_b[node]))
        if stored_a < exact_a or stored_b < exact_b:
            raise RuntimeError(f"stored node {node} constants no longer enclose exact derivation")
        children.append(
            {
                "node": node,
                "phase_start": start,
                "phase_end": end,
                "stored_a_hex": float(node_a[node]).hex(),
                "stored_b_hex": float(node_b[node]).hex(),
                "exact_a": fraction_hex(exact_a),
                "exact_b": fraction_hex(exact_b),
            }
        )

    phase_l1 = [outward_sum(row) for row in bank]
    a4 = max(float(node_a[2]), float(node_a[3]))
    b4 = max(float(node_b[2]), float(node_b[3]))
    return {
        "schema": "tonepoet-fast066-metadata-v2",
        "algorithm_revision": "Fast066V2",
        "coefficient_source_sha256": hashlib.sha256(source_bytes).hexdigest(),
        "midpoint_phase": MIDPOINT_PHASE,
        "midpoint_nonzero_count": int(np.count_nonzero(midpoint)),
        "midpoint_support_indices": [MIDPOINT_NONZERO_START, MIDPOINT_NONZERO_END_EXCLUSIVE - 1],
        "midpoint_pair_coefficients_hex": [float(value).hex() for value in active[:12]],
        "midpoint_l1_upper_hex": phase_l1[MIDPOINT_PHASE].hex(),
        "phase_l1_upper_hex": [value.hex() for value in phase_l1],
        "tail_l1_upper_hex": max(phase_l1).hex(),
        "a4_upper_hex": a4.hex(),
        "b4_upper_hex": b4.hex(),
        "exact_dyadic_child_checks": children,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--output",
        type=Path,
        default=CRATE_ROOT / "qualification" / "fast066_metadata.json",
    )
    args = parser.parse_args()
    report = derive()
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
