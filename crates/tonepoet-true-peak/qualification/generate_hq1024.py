#!/usr/bin/env python3
"""Generate and audit the frozen HQ1024V1 reconstruction tables.

This is offline authoring/qualification tooling only. Cargo and the runtime do
not execute it. The generated binary64 tables define the HQ1024V1
reconstruction identity used by the Rust scanner.

The construction follows docs/tonepoet_true_peak_reference9_fast90_design.md:
* 1536-tap Kaiser(beta=14) symmetric half-sample first phase, DC normalized;
* 24- and 12-coefficient Type-II Remez half-delay stages over 0..0.5 and
  0..0.25 normalized Nyquist respectively, DC normalized;
* seven centered half-sample Lagrange stages with lengths 12,8,8,6,6,6,6;
* a frozen 512-phase 2x-to-1024x tail bank over coarse offsets -16..17;
* dyadic curvature metadata derived from the exact frozen tail rows.

The design document did not ship the research script it cited. The Remez dense
reference grid is made explicit here (128 points per band step); this setting
reproduces the design's published HQ root curvature and operator-norm values.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import struct
from fractions import Fraction
from pathlib import Path

import numpy as np
from scipy import signal

FIRST_TAPS = 1536
FIRST_BETA = 14.0
REMEZ_GRID_DENSITY = 128
TAIL_FACTOR = 512
TAIL_OFFSET_MIN = -16
TAIL_OFFSET_MAX = 17
TAIL_OFFSET_COUNT = TAIL_OFFSET_MAX - TAIL_OFFSET_MIN + 1
STAGE_LENGTHS = (24, 12, 12, 8, 8, 6, 6, 6, 6)
TARGET_FACTOR = 1024
EXPECTED_DELAY_SUBFRAMES = 795_354
EXPECTED_TAIL_DELAY_SUBFRAMES = 8_922
EXPECTED_OPERATOR_LINF = 4.676_026_435_151_283
EXPECTED_ROOT_A = 0.329_422_342_667


def first_half_coefficients() -> np.ndarray:
    index = np.arange(FIRST_TAPS, dtype=np.float64)
    centered = index - (FIRST_TAPS - 1) / 2.0
    full = np.sinc(centered) * np.kaiser(FIRST_TAPS, FIRST_BETA)
    full /= full.sum(dtype=np.float64)
    if not np.array_equal(full, full[::-1]):
        # NumPy's construction is expected to be exactly symmetric here. If a
        # future dependency changes last-bit evaluation, force the invariant
        # before freezing rather than silently emitting asymmetric pairs.
        full = 0.5 * (full + full[::-1])
        full /= full.sum(dtype=np.float64)
    return full


def remez_half_coefficients(length: int, band_edge: float) -> np.ndarray:
    coefficients = signal.remez(
        length,
        [0.0, band_edge],
        [1.0],
        fs=2.0,
        grid_density=REMEZ_GRID_DENSITY,
        maxiter=25,
    ).astype(np.float64)
    coefficients /= coefficients.sum(dtype=np.float64)
    # The even-length Type-II solution must be symmetric; make failure loud.
    if np.max(np.abs(coefficients - coefficients[::-1])) > 2.0e-15:
        raise RuntimeError("Remez half-delay coefficients lost symmetry")
    return coefficients


def lagrange_half_coefficients(length: int) -> tuple[np.ndarray, list[Fraction]]:
    delay = Fraction(length - 1, 2)
    exact: list[Fraction] = []
    for tap in range(length):
        value = Fraction(1, 1)
        for other in range(length):
            if other != tap:
                value *= (delay - other) / (tap - other)
        exact.append(value)
    return np.asarray([float(value) for value in exact], dtype=np.float64), exact


def stage_response(coefficients: np.ndarray) -> tuple[np.ndarray, int]:
    length = len(coefficients)
    if length % 2 != 0:
        raise ValueError("HQ half-phase coefficient count must be even")
    response = np.zeros(2 * length, dtype=np.float64)
    response[length] = 1.0  # exact integer phase, delayed by length / 2 inputs
    response[1::2] = coefficients
    while len(response) > 1 and response[-1] == 0.0:
        response = response[:-1]
    return response, length


def compose(initial: np.ndarray, initial_delay: int, stages: list[np.ndarray]) -> tuple[np.ndarray, int]:
    response = np.asarray(initial, dtype=np.float64)
    delay = initial_delay
    for coefficients in stages:
        stage, stage_delay = stage_response(coefficients)
        upsampled = np.zeros(response.size * 2 - 1, dtype=np.float64)
        upsampled[::2] = response
        response = np.convolve(upsampled, stage)
        delay = delay * 2 + stage_delay
        while response.size > 1 and response[-1] == 0.0:
            response = response[:-1]
    return response, delay


def tail_bank(tail: np.ndarray, delay: int) -> np.ndarray:
    bank = np.zeros((TAIL_FACTOR, TAIL_OFFSET_COUNT), dtype=np.float64)
    represented = np.zeros(tail.size, dtype=bool)
    for phase in range(TAIL_FACTOR):
        for offset in range(TAIL_OFFSET_MIN, TAIL_OFFSET_MAX + 1):
            response_index = phase + delay - TAIL_FACTOR * offset
            if 0 <= response_index < tail.size:
                bank[phase, offset - TAIL_OFFSET_MIN] = tail[response_index]
                represented[response_index] = True
    nonzero_missing = np.abs(tail[~represented]).sum(dtype=np.float64)
    if nonzero_missing != 0.0:
        raise RuntimeError(f"tail support window omitted {nonzero_missing:.17e} coefficient mass")
    return bank


def endpoint_row(bank: np.ndarray, phase: int) -> np.ndarray:
    if phase == TAIL_FACTOR:
        row = np.zeros(TAIL_OFFSET_COUNT, dtype=np.float64)
        row[1 - TAIL_OFFSET_MIN] = 1.0
        return row
    return bank[phase]


def exact_node_bound(bank: np.ndarray, start: int, end: int) -> tuple[Fraction, Fraction]:
    """Return exact A/B for the frozen binary64 tail rows.

    Every frozen f64 has an exact rational value. The phase interpolation
    weights are rational as well, so the moment correction and Q sums can be
    evaluated exactly instead of trusting a floating probe plus a magic margin.
    """
    def row(phase: int) -> list[Fraction]:
        if phase == TAIL_FACTOR:
            values = [Fraction(0, 1) for _ in range(TAIL_OFFSET_COUNT)]
            values[1 - TAIL_OFFSET_MIN] = Fraction(1, 1)
            return values
        return [Fraction.from_float(float(value)) for value in bank[phase]]

    maximum_a = Fraction(0, 1)
    maximum_b = Fraction(0, 1)
    left = row(start)
    right = row(end)
    width = end - start
    for phase in range(start + 1, end):
        weight = Fraction(phase - start, width)
        one_minus = Fraction(1, 1) - weight
        current = row(phase)
        residual = [
            current[index] - one_minus * left[index] - weight * right[index]
            for index in range(TAIL_OFFSET_COUNT)
        ]
        m0 = sum(residual, Fraction(0, 1))
        m1 = sum(
            (Fraction(offset, 1) * residual[offset - TAIL_OFFSET_MIN]
             for offset in range(TAIL_OFFSET_MIN, TAIL_OFFSET_MAX + 1)),
            Fraction(0, 1),
        )
        a = m0 - m1
        b = m1
        residual[0 - TAIL_OFFSET_MIN] -= a
        residual[1 - TAIL_OFFSET_MIN] -= b

        # Q[j] = (j+1)*sum(r[k]) - sum(k*r[k]) over k<=j. This
        # prefix form is algebraically identical to the triangular definition
        # and avoids quadratic exact-rational work.
        prefix = Fraction(0, 1)
        weighted_prefix = Fraction(0, 1)
        q_abs_sum = Fraction(0, 1)
        for j in range(TAIL_OFFSET_MIN, TAIL_OFFSET_MAX - 1):
            value = residual[j - TAIL_OFFSET_MIN]
            prefix += value
            weighted_prefix += Fraction(j, 1) * value
            q = Fraction(j + 1, 1) * prefix - weighted_prefix
            q_abs_sum += abs(q)
        maximum_a = max(maximum_a, q_abs_sum)
        maximum_b = max(maximum_b, abs(a) + abs(b))
    return maximum_a, maximum_b


def exact_fraction_upper(value: Fraction) -> float:
    """Smallest convenient binary64 value strictly above an exact positive rational."""
    if value < 0:
        raise ValueError("bound must be nonnegative")
    rounded = float(value)
    if Fraction.from_float(rounded) < value:
        rounded = float(np.nextafter(np.float64(rounded), np.float64(math.inf)))
    # Keep one additional ULP so later source parsing/formatting can never turn
    # a tight equality into an inward bound.
    return float(np.nextafter(np.float64(rounded), np.float64(math.inf)))


def node_bound(bank: np.ndarray, start: int, end: int) -> tuple[float, float]:
    exact_a, exact_b = exact_node_bound(bank, start, end)
    return exact_fraction_upper(exact_a), exact_fraction_upper(exact_b)

def node_tables(bank: np.ndarray) -> tuple[np.ndarray, np.ndarray]:
    # Heap indexing: root [0,512] is 1; children are 2*i and 2*i+1. Leaves of
    # width one begin at 512 and need no residual bound, so indices 1..511 are
    # the complete internal-node metadata set.
    a = np.zeros(TAIL_FACTOR, dtype=np.float64)
    b = np.zeros(TAIL_FACTOR, dtype=np.float64)
    for heap_index in range(1, TAIL_FACTOR):
        depth = heap_index.bit_length() - 1
        nodes_at_depth = 1 << depth
        width = TAIL_FACTOR // nodes_at_depth
        ordinal = heap_index - nodes_at_depth
        start = ordinal * width
        a[heap_index], b[heap_index] = node_bound(bank, start, start + width)
    return a, b


def full_response(first: np.ndarray, stages: list[np.ndarray]) -> tuple[np.ndarray, int]:
    response = np.zeros(FIRST_TAPS * 2, dtype=np.float64)
    response[FIRST_TAPS] = 1.0
    response[1::2] = first
    while response.size > 1 and response[-1] == 0.0:
        response = response[:-1]
    return compose(response, FIRST_TAPS, stages)


def fnv1a64(values: np.ndarray) -> int:
    value = 0xCBF29CE484222325
    prime = 0x100000001B3
    for raw in struct.pack("<Q", int(values.size)):
        value ^= raw
        value = (value * prime) & 0xFFFFFFFFFFFFFFFF
    for item in values.flat:
        for raw in struct.pack("<Q", struct.unpack("<Q", struct.pack("<d", float(item)))[0]):
            value ^= raw
            value = (value * prime) & 0xFFFFFFFFFFFFFFFF
    return value


def rust_float(value: float) -> str:
    if value == 0.0:
        return "0.0"
    return repr(float(value))


def rust_array(name: str, values: np.ndarray, rust_type: str = "f64", per_line: int = 4) -> str:
    flat = list(values.flat)
    lines = [f"pub(crate) const {name}: [{rust_type}; {len(flat)}] = ["]
    for index in range(0, len(flat), per_line):
        chunk = flat[index:index + per_line]
        if rust_type == "f64":
            rendered = ", ".join(rust_float(float(value)) for value in chunk)
        else:
            rendered = ", ".join(str(int(value)) for value in chunk)
        lines.append(f"    {rendered},")
    lines.append("];\n")
    return "\n".join(lines)


def render_rust(first: np.ndarray, bank: np.ndarray, node_a: np.ndarray, node_b: np.ndarray, operator_linf: float) -> str:
    half = first[: FIRST_TAPS // 2]
    nonzero_counts = np.count_nonzero(bank, axis=1).astype(np.uint8)
    parts = [
        "// @generated by qualification/generate_hq1024.py; do not hand-edit.\n",
        "// HQ1024V1 frozen binary64 reconstruction data.\n\n",
        f"pub(crate) const HQ1024_FIRST_HALF_DELAY_TAPS: usize = {FIRST_TAPS};\n",
        f"pub(crate) const HQ1024_TAIL_FACTOR: usize = {TAIL_FACTOR};\n",
        f"pub(crate) const HQ1024_TAIL_OFFSET_MIN: i32 = {TAIL_OFFSET_MIN};\n",
        f"pub(crate) const HQ1024_TAIL_OFFSET_MAX: i32 = {TAIL_OFFSET_MAX};\n",
        f"pub(crate) const HQ1024_TAIL_OFFSET_COUNT: usize = {TAIL_OFFSET_COUNT};\n",
        "#[cfg(test)]\n",
        f"pub(crate) const HQ1024_DELAY_SUBFRAMES: i128 = {EXPECTED_DELAY_SUBFRAMES};\n",
        "#[cfg(test)]\n",
        f"pub(crate) const HQ1024_TAIL_DELAY_SUBFRAMES: i32 = {EXPECTED_TAIL_DELAY_SUBFRAMES};\n",
        "#[cfg(test)]\n",
        f"pub(crate) const HQ1024_MEASURED_OPERATOR_LINF: f64 = {rust_float(operator_linf)};\n",
        "#[cfg(test)]\n",
        f"pub(crate) const HQ1024_FIRST_HALF_FNV1A64: u64 = 0x{fnv1a64(half):016x};\n",
        "#[cfg(test)]\n",
        f"pub(crate) const HQ1024_TAIL_FNV1A64: u64 = 0x{fnv1a64(bank):016x};\n",
        "#[cfg(test)]\n",
        f"pub(crate) const HQ1024_NODE_A_FNV1A64: u64 = 0x{fnv1a64(node_a):016x};\n",
        "#[cfg(test)]\n",
        f"pub(crate) const HQ1024_NODE_B_FNV1A64: u64 = 0x{fnv1a64(node_b):016x};\n\n",
        rust_array("HQ1024_HALF_DELAY_COEFFICIENTS", half),
    ]
    # The runtime consumes the precomposed 512-phase tail bank below.  The
    # individual stage-2..10 half-coefficient arrays remain source-of-truth
    # inputs to that composition and are retained in the JSON qualification
    # artifact; emitting another Rust copy only creates dead release data.
    parts.append(
        f"pub(crate) const HQ1024_TAIL_COEFFICIENTS: [[f64; {TAIL_OFFSET_COUNT}]; {TAIL_FACTOR}] = [\n"
    )
    for row in bank:
        parts.append("    [" + ", ".join(rust_float(float(value)) for value in row) + "],\n")
    parts.append("];\n\n")
    parts.append(rust_array("HQ1024_TAIL_NONZERO_COUNTS", nonzero_counts, "u8", per_line=16))
    parts.append(rust_array("HQ1024_NODE_A_UPPER", node_a))
    parts.append(rust_array("HQ1024_NODE_B_UPPER", node_b))
    parts.append(r"""
#[cfg(test)]
mod integrity_tests {
    use super::*;

    const FNV1A64_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV1A64_PRIME: u64 = 0x0000_0100_0000_01b3;

    fn mix_bytes(mut hash: u64, bytes: impl IntoIterator<Item = u8>) -> u64 {
        for byte in bytes {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(FNV1A64_PRIME);
        }
        hash
    }

    fn hash_f64s(count: usize, values: impl IntoIterator<Item = f64>) -> u64 {
        let mut hash = mix_bytes(FNV1A64_OFFSET_BASIS, (count as u64).to_le_bytes());
        for value in values {
            hash = mix_bytes(hash, value.to_bits().to_le_bytes());
        }
        hash
    }

    #[test]
    fn frozen_descriptor_tables_match_their_checksums() {
        assert_eq!(
            hash_f64s(
                HQ1024_HALF_DELAY_COEFFICIENTS.len(),
                HQ1024_HALF_DELAY_COEFFICIENTS.iter().copied(),
            ),
            HQ1024_FIRST_HALF_FNV1A64,
        );
        assert_eq!(
            hash_f64s(
                HQ1024_TAIL_FACTOR * HQ1024_TAIL_OFFSET_COUNT,
                HQ1024_TAIL_COEFFICIENTS
                    .iter()
                    .flat_map(|row| row.iter().copied()),
            ),
            HQ1024_TAIL_FNV1A64,
        );
        assert_eq!(
            hash_f64s(HQ1024_NODE_A_UPPER.len(), HQ1024_NODE_A_UPPER.iter().copied()),
            HQ1024_NODE_A_FNV1A64,
        );
        assert_eq!(
            hash_f64s(HQ1024_NODE_B_UPPER.len(), HQ1024_NODE_B_UPPER.iter().copied()),
            HQ1024_NODE_B_FNV1A64,
        );
    }

    #[test]
    fn frozen_descriptor_geometry_is_self_consistent() {
        assert_eq!(HQ1024_HALF_DELAY_COEFFICIENTS.len() * 2, HQ1024_FIRST_HALF_DELAY_TAPS);
        assert_eq!(HQ1024_TAIL_COEFFICIENTS.len(), HQ1024_TAIL_FACTOR);
        assert!(HQ1024_TAIL_COEFFICIENTS
            .iter()
            .all(|row| row.len() == HQ1024_TAIL_OFFSET_COUNT));
        assert_eq!(HQ1024_TAIL_COEFFICIENTS[0][(0 - HQ1024_TAIL_OFFSET_MIN) as usize], 1.0);
        assert_eq!(HQ1024_TAIL_NONZERO_COUNTS[0], 1);
    }
}
""")
    return "".join(parts)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--rust-output", type=Path)
    parser.add_argument("--json-output", type=Path)
    args = parser.parse_args()

    first = first_half_coefficients()
    stage2 = remez_half_coefficients(24, 0.5)
    stage3 = remez_half_coefficients(12, 0.25)
    lagrange_lengths = STAGE_LENGTHS[2:]
    lagrange_pairs = [lagrange_half_coefficients(length) for length in lagrange_lengths]
    stages = [stage2, stage3] + [pair[0] for pair in lagrange_pairs]

    tail, tail_delay = compose(np.asarray([1.0], dtype=np.float64), 0, stages)
    if tail_delay != EXPECTED_TAIL_DELAY_SUBFRAMES:
        raise RuntimeError(f"unexpected tail delay {tail_delay}")
    bank = tail_bank(tail, tail_delay)
    if not (bank[0, 0 - TAIL_OFFSET_MIN] == 1.0 and np.count_nonzero(bank[0]) == 1):
        raise RuntimeError("HQ tail phase zero is not the exact left identity")
    node_a, node_b = node_tables(bank)

    full, delay = full_response(first, stages)
    if delay != EXPECTED_DELAY_SUBFRAMES:
        raise RuntimeError(f"unexpected full delay {delay}")
    operator_linf = max(
        np.abs(full[phase::TARGET_FACTOR]).sum(dtype=np.float64)
        for phase in range(TARGET_FACTOR)
    )
    if abs(operator_linf - EXPECTED_OPERATOR_LINF) > 2.0e-12:
        raise RuntimeError(
            f"operator norm drifted: {operator_linf:.17g} vs {EXPECTED_OPERATOR_LINF:.17g}"
        )
    raw_root_a, _ = node_bound(bank, 0, TAIL_FACTOR)
    if abs(raw_root_a - EXPECTED_ROOT_A) > 2.0e-12:
        raise RuntimeError(
            f"root curvature drifted: {raw_root_a:.17g} vs {EXPECTED_ROOT_A:.17g}"
        )

    report = {
        "construction": {
            "first_taps": FIRST_TAPS,
            "first_beta": FIRST_BETA,
            "remez_grid_density": REMEZ_GRID_DENSITY,
            "stage_half_lengths": list(STAGE_LENGTHS),
            "tail_factor": TAIL_FACTOR,
            "tail_offset_min": TAIL_OFFSET_MIN,
            "tail_offset_max": TAIL_OFFSET_MAX,
        },
        "geometry": {
            "delay_target_subframes": delay,
            "delay_original_frames": delay / TARGET_FACTOR,
            "tail_delay_subframes": tail_delay,
            "tail_nonzero_count_min": int(np.count_nonzero(bank, axis=1).min()),
            "tail_nonzero_count_max": int(np.count_nonzero(bank, axis=1).max()),
        },
        "measured": {
            "operator_linf": operator_linf,
            "root_curvature_a": raw_root_a,
            "root_affine_b": node_bound(bank, 0, TAIL_FACTOR)[1],
            "node_a_width_256_max": float(max(node_a[2:4])),
            "node_a_width_128_max": float(max(node_a[4:8])),
            "node_a_width_2_max": float(max(node_a[256:512])),
        },
        "checksums": {
            "first_half_fnv1a64": f"{fnv1a64(first[: FIRST_TAPS // 2]):016x}",
            "tail_fnv1a64": f"{fnv1a64(bank):016x}",
            "node_a_fnv1a64": f"{fnv1a64(node_a):016x}",
            "node_b_fnv1a64": f"{fnv1a64(node_b):016x}",
            "full_sha256": hashlib.sha256(full.tobytes(order="C")).hexdigest(),
        },
        "lagrange_exact": [
            [f"{value.numerator}/{value.denominator}" for value in exact]
            for _, exact in lagrange_pairs
        ],
    }

    if args.rust_output:
        args.rust_output.write_text(
            render_rust(first, bank, node_a, node_b, operator_linf),
            encoding="utf-8",
        )
    if args.json_output:
        args.json_output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    if not args.rust_output and not args.json_output:
        print(json.dumps(report, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
