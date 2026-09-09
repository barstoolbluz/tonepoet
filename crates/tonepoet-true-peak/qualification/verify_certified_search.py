#!/usr/bin/env python3
"""Independent offline audit for the shared certified-search coefficient data.

This script deliberately does not import generate_hq1024.py. It reconstructs
both frozen reconstruction tails from the published construction and checks the
finite coefficient identities used by the Rust 64/8/1 and dyadic bounds.

The certified engine now uses a crate-owned fixed radix-2 2x prefix whose
numerical enclosure is qualified independently by verify_qualified_prefix.py.
That owned executor may dispatch pairs of the same certified butterfly graph
through AVX, with a scalar fallback; no external planner or alternate SIMD FFT
backend is part of certified execution. This qualifier checks the tail/search
coefficient identities and direct/dense arithmetic that consume the enclosed
prefix.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import math
import re
import struct
from fractions import Fraction
from pathlib import Path

import numpy as np
from scipy import signal

ROOT = Path(__file__).resolve().parents[1]
DESIGN = ROOT.parents[1] / "docs" / "tonepoet_true_peak_reference9_fast90_design.md"
HQ_RUST = ROOT / "src" / "hq1024_coefficients.rs"
LEGACY_RUST = ROOT / "src" / "headroom64_coefficients.rs"
SCANNER_RUST = ROOT / "src" / "certified_scan.rs"
LIB_RUST = ROOT / "src" / "lib.rs"
BENCH_RUST = ROOT / "examples" / "bench_ceiling_f64le.rs"
METER_TEST_RUST = ROOT / "tests" / "meter.rs"
CARGO_TOML = ROOT / "Cargo.toml"
HQ_GENERATOR = ROOT / "qualification" / "generate_hq1024.py"

COEFFICIENT_EPSILON = 1.0e-15
LEGACY_STAGE_LENGTHS = (49, 25, 17, 13, 9)
LEGACY_TAIL_FACTOR = 32
LEGACY_OFFSET_MIN = -15
LEGACY_OFFSET_MAX = 16
HQ_STAGE_LENGTHS = (24, 12, 12, 8, 8, 6, 6, 6, 6)
HQ_TAIL_FACTOR = 512
HQ_OFFSET_MIN = -16
HQ_OFFSET_MAX = 17
REMEZ_GRID_DENSITY = 128


def parse_rust_f64_array(path: Path, name: str) -> np.ndarray:
    text = path.read_text()
    match = re.search(
        rf"const\s+{re.escape(name)}\s*:\s*\[f64;\s*\d+\]\s*=\s*\[(.*?)\];",
        text,
        re.S,
    )
    if not match:
        raise RuntimeError(f"could not find {name} in {path}")
    return np.asarray(
        [float(token) for token in match.group(1).replace("\n", " ").split(",") if token.strip()],
        dtype=np.float64,
    )


def parse_rust_u64(path: Path, name: str) -> int:
    text = path.read_text()
    match = re.search(rf"const\s+{re.escape(name)}\s*:\s*u64\s*=\s*(0x[0-9a-fA-F_]+|\d+)", text)
    if not match:
        raise RuntimeError(f"could not find {name}")
    return int(match.group(1).replace("_", ""), 0)


def parse_rust_usize(path: Path, name: str) -> int:
    text = path.read_text()
    match = re.search(rf"const\s+{re.escape(name)}\s*:\s*usize\s*=\s*(0x[0-9a-fA-F_]+|\d+)", text)
    if not match:
        raise RuntimeError(f"could not find {name}")
    return int(match.group(1).replace("_", ""), 0)


def parse_rust_u64_constant(path: Path, name: str) -> int:
    return parse_rust_u64(path, name)


def fnv1a64(values: np.ndarray) -> int:
    value = 0xCBF29CE484222325
    prime = 0x100000001B3
    for byte in struct.pack("<Q", int(values.size)):
        value ^= byte
        value = (value * prime) & 0xFFFFFFFFFFFFFFFF
    for item in values.flat:
        bits = struct.unpack("<Q", struct.pack("<d", float(item)))[0]
        for byte in struct.pack("<Q", bits):
            value ^= byte
            value = (value * prime) & 0xFFFFFFFFFFFFFFFF
    return value


def legacy_half_stage(taps: int) -> np.ndarray:
    delay_frames = (taps + 1) // 2
    phase_coefficients = [[], []]
    phase_indices = [[], []]
    for tap in range(taps):
        centered = float(tap) - float(taps - 1) / 2.0
        if abs(centered) < np.finfo(np.float64).eps:
            sinc = 1.0
        else:
            argument = centered * math.pi / 2.0
            sinc = math.sin(argument) / argument
        phase_fraction = float(tap) / float(taps - 1)
        window = (
            0.42
            - 0.5 * math.cos(2.0 * math.pi * phase_fraction)
            + 0.08 * math.cos(4.0 * math.pi * phase_fraction)
        )
        coefficient = sinc * window
        if abs(coefficient) <= COEFFICIENT_EPSILON:
            continue
        phase = tap % 2
        phase_indices[phase].append(tap // 2)
        phase_coefficients[phase].append(coefficient)
    for phase in range(2):
        total = sum(phase_coefficients[phase])
        phase_coefficients[phase] = [value / total for value in phase_coefficients[phase]]
    response = np.zeros(delay_frames * 2, dtype=np.float64)
    for phase in range(2):
        for index, coefficient in zip(phase_indices[phase], phase_coefficients[phase]):
            response[index * 2 + phase] = coefficient
    while response.size > 1 and response[-1] == 0.0:
        response = response[:-1]
    return response



BINARY64_UNIT_ROUNDOFF = 2.0 ** -53
BINARY64_MIN_NORMAL = float.fromhex("0x1.0p-1022")
DAZ_INPUT_UNITS_PER_OPERATION = 2.0
FTZ_RESULT_UNITS_PER_OPERATION = 1.0
FUSED_RESIDUAL_UNDERFLOW_UNITS = 1.0
DIRECT_UNDERFLOW_UNITS_PER_OPERATION = (
    DAZ_INPUT_UNITS_PER_OPERATION
    + FTZ_RESULT_UNITS_PER_OPERATION
    + FUSED_RESIDUAL_UNDERFLOW_UNITS
)


def upper_nonnegative(value: float) -> float:
    if value == 0.0 or math.isinf(value):
        return value
    return math.nextafter(value, math.inf)


def downward_nonnegative(value: float) -> float:
    if value <= 0.0 or math.isinf(value):
        return value
    return math.nextafter(value, 0.0)


def lower_sub(magnitude: float, error: float) -> float:
    if not (magnitude >= 0.0 and error >= 0.0) or not math.isfinite(magnitude) or not math.isfinite(error):
        return 0.0
    if error == 0.0:
        return magnitude
    if magnitude < BINARY64_MIN_NORMAL:
        return 0.0
    if error < BINARY64_MIN_NORMAL:
        error = BINARY64_MIN_NORMAL
    if error >= magnitude:
        return 0.0
    return downward_nonnegative(magnitude - error)


def upper_add(left: float, right: float) -> float:
    if 0.0 < left < BINARY64_MIN_NORMAL:
        left = BINARY64_MIN_NORMAL
    if 0.0 < right < BINARY64_MIN_NORMAL:
        right = BINARY64_MIN_NORMAL
    value = left + right
    return upper_nonnegative(value) if math.isfinite(value) else math.inf


def upper_mul(left: float, right: float) -> float:
    if left == 0.0 or right == 0.0:
        return 0.0
    if left < BINARY64_MIN_NORMAL:
        left = BINARY64_MIN_NORMAL
    if right < BINARY64_MIN_NORMAL:
        right = BINARY64_MIN_NORMAL
    value = left * right
    if not math.isfinite(value):
        return math.inf
    if value < BINARY64_MIN_NORMAL:
        return BINARY64_MIN_NORMAL
    return upper_nonnegative(value)


def phase_l1_outward_audit(bank: np.ndarray) -> tuple[bool, float, float]:
    """Mirror TailMetadata::phase_l1_upper and prove exact binary64 L1 enclosure."""
    outward_maximum = 0.0
    exact_maximum = Fraction(0, 1)
    for row in bank:
        outward = 0.0
        exact = Fraction(0, 1)
        for coefficient in row:
            magnitude = abs(float(coefficient))
            outward = upper_add(outward, magnitude)
            exact += Fraction.from_float(magnitude)
        outward_maximum = max(outward_maximum, outward)
        exact_maximum = max(exact_maximum, exact)
        if Fraction.from_float(outward) < exact:
            return False, outward_maximum, float(exact_maximum)
    return (
        Fraction.from_float(outward_maximum) >= exact_maximum,
        outward_maximum,
        float(exact_maximum),
    )


def gamma_upper(operations: int) -> float:
    if operations == 0:
        return 0.0
    numerator = upper_mul(float(operations), BINARY64_UNIT_ROUNDOFF)
    if numerator >= 1.0:
        return math.inf
    denominator = downward_nonnegative(1.0 - numerator)
    if denominator <= 0.0 or not math.isfinite(denominator):
        return math.inf
    return upper_nonnegative(numerator / denominator)


def direct_underflow_upper(operations: int) -> float:
    return upper_mul(
        float(operations) * DIRECT_UNDERFLOW_UNITS_PER_OPERATION,
        BINARY64_MIN_NORMAL,
    )


def dot_rounding_upper(operations: int, absolute_sum_upper: float) -> float:
    return upper_add(
        upper_mul(gamma_upper(operations), absolute_sum_upper),
        direct_underflow_upper(operations),
    )


def audit_outward_bound_primitives() -> bool:
    min_subnormal = float.fromhex("0x0.0000000000001p-1022")
    max_subnormal = math.nextafter(BINARY64_MIN_NORMAL, 0.0)
    values = [
        0.0,
        min_subnormal,
        max_subnormal,
        BINARY64_MIN_NORMAL,
        BINARY64_MIN_NORMAL * 2.0,
        0.5,
        1.0,
        4.68,
        1.0e100,
        1.0e307,
    ]
    for left in values:
        for right in values:
            exact_add = Fraction.from_float(left) + Fraction.from_float(right)
            add_bound = upper_add(left, right)
            if not math.isinf(add_bound) and Fraction.from_float(add_bound) < exact_add:
                return False
            exact_mul = Fraction.from_float(left) * Fraction.from_float(right)
            mul_bound = upper_mul(left, right)
            if not math.isinf(mul_bound) and Fraction.from_float(mul_bound) < exact_mul:
                return False
            exact_lower = max(Fraction(0, 1), Fraction.from_float(left) - Fraction.from_float(right))
            lower_bound = lower_sub(left, right)
            if Fraction.from_float(lower_bound) > exact_lower:
                return False

    # Explicitly cover the DAZ-dangerous shape: a normal magnitude minus a
    # positive subnormal uncertainty.  A round-to-nearest subtraction with
    # the uncertainty silently treated as zero would be an invalid lower.
    daz_magnitude = BINARY64_MIN_NORMAL * 2.0
    daz_error = max_subnormal
    if Fraction.from_float(lower_sub(daz_magnitude, daz_error)) > (
        Fraction.from_float(daz_magnitude) - Fraction.from_float(daz_error)
    ):
        return False

    u = Fraction(1, 1 << 53)
    for operations in [0, 1, 2, 3, 8, 24, 49, 192, 384, 1536, 3072, 8192]:
        gamma = gamma_upper(operations)
        exact = Fraction(0, 1) if operations == 0 else Fraction(operations, 1) * u / (1 - Fraction(operations, 1) * u)
        if not math.isinf(gamma) and Fraction.from_float(gamma) < exact:
            return False
    return True


def rust_convolve_with_error(
    left: list[float], left_error: list[float], right: np.ndarray
) -> tuple[list[float], list[float]]:
    """Independent reproduction of certified_scan.rs::convolve_with_error."""
    output_len = len(left) + len(right) - 1
    output = [0.0] * output_len
    propagated = [0.0] * output_len
    absolute_sum = [0.0] * output_len
    term_counts = [0] * output_len
    for left_index, left_value in enumerate(left):
        if left_value == 0.0 and left_error[left_index] == 0.0:
            continue
        for right_index, right_value_np in enumerate(right):
            right_value = float(right_value_np)
            if right_value == 0.0:
                continue
            index = left_index + right_index
            if left_value != 0.0:
                output[index] += left_value * right_value
                absolute_sum[index] = upper_add(
                    absolute_sum[index], upper_mul(abs(left_value), abs(right_value))
                )
                term_counts[index] += 1
            if left_error[left_index] != 0.0:
                propagated[index] = upper_add(
                    propagated[index],
                    upper_mul(left_error[left_index], abs(right_value)),
                )
    error = [
        upper_add(
            propagated[index],
            dot_rounding_upper(term_counts[index] * 2, absolute_sum[index]),
        )
        for index in range(output_len)
    ]
    while len(output) > 1 and output[-1] == 0.0 and error[-1] == 0.0:
        output.pop()
        error.pop()
    return output, error


def compose_legacy_rust_order(
    stages: list[np.ndarray],
) -> tuple[np.ndarray, np.ndarray, int, list[Fraction]]:
    response = [1.0]
    response_error = [0.0]
    exact = [Fraction(1, 1)]
    delay = 0
    for stage in stages:
        upsampled = [0.0] * (len(response) * 2 - 1)
        upsampled_error = [0.0] * (len(response_error) * 2 - 1)
        exact_upsampled = [Fraction(0, 1)] * (len(exact) * 2 - 1)
        for index, value in enumerate(response):
            upsampled[index * 2] = value
            upsampled_error[index * 2] = response_error[index]
        for index, value in enumerate(exact):
            exact_upsampled[index * 2] = value

        response, response_error = rust_convolve_with_error(
            upsampled, upsampled_error, stage
        )
        stage_exact = [Fraction.from_float(float(value)) for value in stage]
        exact_next = [Fraction(0, 1)] * (len(exact_upsampled) + len(stage_exact) - 1)
        for left_index, left_value in enumerate(exact_upsampled):
            if left_value == 0:
                continue
            for right_index, right_value in enumerate(stage_exact):
                if right_value != 0:
                    exact_next[left_index + right_index] += left_value * right_value
        while len(exact_next) > 1 and exact_next[-1] == 0:
            exact_next.pop()
        exact = exact_next
        identity = np.flatnonzero(stage == 1.0)
        if identity.size != 1:
            raise RuntimeError("legacy stage has no unique identity coefficient")
        delay = delay * 2 + int(identity[0])
    return (
        np.asarray(response, dtype=np.float64),
        np.asarray(response_error, dtype=np.float64),
        delay,
        exact,
    )


def phase_bank_error(
    error: np.ndarray, delay: int, factor: int, lo: int, hi: int
) -> float:
    maximum = 0.0
    for phase in range(factor):
        phase_error = 0.0
        for offset in range(lo, hi + 1):
            index = phase + delay - factor * offset
            if 0 <= index < error.size:
                phase_error = upper_add(phase_error, float(error[index]))
        maximum = max(maximum, phase_error)
    return maximum


def exact_phase_bank_discrepancy(
    rounded: np.ndarray,
    exact: list[Fraction],
    delay: int,
    factor: int,
    lo: int,
    hi: int,
) -> Fraction:
    maximum = Fraction(0, 1)
    for phase in range(factor):
        total = Fraction(0, 1)
        for offset in range(lo, hi + 1):
            index = phase + delay - factor * offset
            if 0 <= index < len(exact):
                total += abs(
                    exact[index] - Fraction.from_float(float(rounded[index]))
                )
        maximum = max(maximum, total)
    return maximum

def hq_remez(length: int, edge: float) -> np.ndarray:
    values = signal.remez(
        length,
        [0.0, edge],
        [1.0],
        fs=2.0,
        grid_density=REMEZ_GRID_DENSITY,
        maxiter=25,
    ).astype(np.float64)
    values /= values.sum(dtype=np.float64)
    return values


def lagrange_half(length: int) -> np.ndarray:
    delay = Fraction(length - 1, 2)
    values = []
    for tap in range(length):
        value = Fraction(1, 1)
        for other in range(length):
            if other != tap:
                value *= (delay - other) / (tap - other)
        values.append(float(value))
    return np.asarray(values, dtype=np.float64)


def hq_stages() -> list[np.ndarray]:
    return [
        hq_remez(24, 0.5),
        hq_remez(12, 0.25),
        lagrange_half(12),
        lagrange_half(8),
        lagrange_half(8),
        lagrange_half(6),
        lagrange_half(6),
        lagrange_half(6),
        lagrange_half(6),
    ]


def compose_tail(stage_responses: list[np.ndarray]) -> tuple[np.ndarray, int]:
    response = np.asarray([1.0], dtype=np.float64)
    delay = 0
    for stage in stage_responses:
        upsampled = np.zeros(response.size * 2 - 1, dtype=np.float64)
        upsampled[::2] = response
        response = np.convolve(upsampled, stage)
        # Both legacy and HQ response builders place the exact identity phase at
        # a stage-specific integer delay. Infer it from the exact coefficient 1.
        exact = np.flatnonzero(stage == 1.0)
        if exact.size != 1:
            raise RuntimeError("stage has no unique exact identity coefficient")
        delay = delay * 2 + int(exact[0])
        while response.size > 1 and response[-1] == 0.0:
            response = response[:-1]
    return response, delay


def hq_stage_response(coefficients: np.ndarray) -> np.ndarray:
    response = np.zeros(2 * len(coefficients), dtype=np.float64)
    response[len(coefficients)] = 1.0
    response[1::2] = coefficients
    while response.size > 1 and response[-1] == 0.0:
        response = response[:-1]
    return response


def make_bank(tail: np.ndarray, delay: int, factor: int, lo: int, hi: int) -> np.ndarray:
    bank = np.zeros((factor, hi - lo + 1), dtype=np.float64)
    represented = np.zeros(tail.size, dtype=bool)
    for phase in range(factor):
        for offset in range(lo, hi + 1):
            index = phase + delay - factor * offset
            if 0 <= index < tail.size:
                bank[phase, offset - lo] = tail[index]
                represented[index] = True
    if np.any(tail[~represented] != 0.0):
        raise RuntimeError("declared tail support omits nonzero coefficients")
    return bank


def exact_node_bound(
    bank: np.ndarray,
    factor: int,
    lo: int,
    hi: int,
    start: int,
    end: int,
) -> tuple[Fraction, Fraction]:
    def row(phase: int) -> list[Fraction]:
        if phase == factor:
            out = [Fraction(0, 1) for _ in range(hi - lo + 1)]
            out[1 - lo] = Fraction(1, 1)
            return out
        return [Fraction.from_float(float(value)) for value in bank[phase]]

    maximum_a = Fraction(0, 1)
    maximum_b = Fraction(0, 1)
    left, right = row(start), row(end)
    width = end - start
    for phase in range(start + 1, end):
        weight = Fraction(phase - start, width)
        one_minus = Fraction(1, 1) - weight
        current = row(phase)
        residual = [
            current[index] - one_minus * left[index] - weight * right[index]
            for index in range(hi - lo + 1)
        ]
        m0 = sum(residual, Fraction(0, 1))
        m1 = sum(
            (Fraction(offset, 1) * residual[offset - lo] for offset in range(lo, hi + 1)),
            Fraction(0, 1),
        )
        a, b = m0 - m1, m1
        residual[-lo] -= a
        residual[1 - lo] -= b
        prefix = Fraction(0, 1)
        weighted_prefix = Fraction(0, 1)
        q_abs_sum = Fraction(0, 1)
        for j in range(lo, hi - 1):
            value = residual[j - lo]
            prefix += value
            weighted_prefix += Fraction(j, 1) * value
            q_abs_sum += abs(Fraction(j + 1, 1) * prefix - weighted_prefix)
        maximum_a = max(maximum_a, q_abs_sum)
        maximum_b = max(maximum_b, abs(a) + abs(b))
    return maximum_a, maximum_b


def node_bound(bank: np.ndarray, factor: int, lo: int, hi: int, start: int, end: int) -> tuple[float, float]:
    exact_a, exact_b = exact_node_bound(bank, factor, lo, hi, start, end)
    return float(exact_a), float(exact_b)

def max_node_a_for_width(bank: np.ndarray, factor: int, lo: int, hi: int, width: int) -> float:
    return max(node_bound(bank, factor, lo, hi, start, start + width)[0] for start in range(0, factor, width))




def compensated_sum(values: list[float]) -> float:
    total = 0.0
    correction = 0.0
    for value in values:
        next_total = total + value
        if abs(total) >= abs(value):
            correction += (total - next_total) + value
        else:
            correction += (value - next_total) + total
        total = next_total
    return total + correction


def outward_nonnegative(value: float) -> float:
    if value == 0.0 or math.isinf(value):
        return value
    return math.nextafter(value, math.inf)


def rust_runtime_node_metadata(
    bank: np.ndarray, factor: int, lo: int, hi: int
) -> tuple[np.ndarray, np.ndarray]:
    """Reproduce certified_scan.rs::build_node_metadata independently."""
    count = hi - lo + 1

    def row(phase: int) -> np.ndarray:
        if phase == factor:
            out = np.zeros(count, dtype=np.float64)
            out[1 - lo] = 1.0
            return out
        return bank[phase].copy()

    a_upper = np.zeros(factor, dtype=np.float64)
    b_upper = np.zeros(factor, dtype=np.float64)
    for tree_index in range(1, factor):
        depth = tree_index.bit_length() - 1
        nodes_at_depth = 1 << depth
        width = factor // nodes_at_depth
        ordinal = tree_index - nodes_at_depth
        start = ordinal * width
        end = start + width
        left = row(start)
        right = row(end)
        maximum_a = 0.0
        maximum_b = 0.0
        for phase in range(start + 1, end):
            weight = (phase - start) / (end - start)
            residual = bank[phase] - (1.0 - weight) * left - weight * right
            m0 = compensated_sum([float(value) for value in residual])
            m1 = compensated_sum(
                [float(offset) * float(residual[offset - lo]) for offset in range(lo, hi + 1)]
            )
            a = m0 - m1
            b = m1
            residual = residual.copy()
            residual[-lo] -= a
            residual[1 - lo] -= b
            q_abs_sum = 0.0
            q_correction = 0.0
            for j in range(lo, hi - 1):
                q = compensated_sum(
                    [float(j - k + 1) * float(residual[k - lo]) for k in range(lo, j + 1)]
                )
                value = abs(q)
                next_sum = q_abs_sum + value
                if abs(q_abs_sum) >= abs(value):
                    q_correction += (q_abs_sum - next_sum) + value
                else:
                    q_correction += (value - next_sum) + q_abs_sum
                q_abs_sum = next_sum
            maximum_a = max(maximum_a, q_abs_sum + q_correction)
            maximum_b = max(maximum_b, abs(a) + abs(b))
        a_upper[tree_index] = outward_nonnegative(
            maximum_a + 2.0e-15 + abs(maximum_a) * 2.0e-13
        )
        b_upper[tree_index] = outward_nonnegative(
            maximum_b + 2.0e-14 + abs(maximum_b) * 2.0e-13
        )
    return a_upper, b_upper


def all_nodes_exactly_outward(
    bank: np.ndarray, factor: int, lo: int, hi: int, a_upper: np.ndarray, b_upper: np.ndarray
) -> bool:
    for heap_index in range(1, factor):
        depth = heap_index.bit_length() - 1
        nodes_at_depth = 1 << depth
        width = factor // nodes_at_depth
        ordinal = heap_index - nodes_at_depth
        start = ordinal * width
        exact_a, exact_b = exact_node_bound(bank, factor, lo, hi, start, start + width)
        if Fraction.from_float(float(a_upper[heap_index])) < exact_a:
            return False
        if Fraction.from_float(float(b_upper[heap_index])) < exact_b:
            return False
    return True


def deterministic_bound_stress(
    bank: np.ndarray, factor: int, lo: int, hi: int, a_upper: np.ndarray, b_upper: np.ndarray, trials: int
) -> bool:
    """Exercise every dyadic interval against finite arbitrary coarse data."""
    rng = np.random.default_rng(0x54504B5F52454639 + factor)
    right_identity = np.zeros(hi - lo + 1, dtype=np.float64)
    right_identity[1 - lo] = 1.0
    rows = np.vstack((bank, right_identity))
    for _ in range(trials):
        samples = rng.uniform(-1.0, 1.0, size=hi - lo + 1).astype(np.float64)
        magnitude = float(np.max(np.abs(samples)))
        d2 = float(np.max(np.abs(samples[:-2] - 2.0 * samples[1:-1] + samples[2:])))
        values = rows @ samples
        for heap_index in range(1, factor):
            depth = heap_index.bit_length() - 1
            nodes_at_depth = 1 << depth
            width = factor // nodes_at_depth
            ordinal = heap_index - nodes_at_depth
            start = ordinal * width
            end = start + width
            endpoint = max(abs(float(values[start])), abs(float(values[end])))
            upper = endpoint + float(a_upper[heap_index]) * d2 + float(b_upper[heap_index]) * magnitude
            if float(np.max(np.abs(values[start : end + 1]))) > math.nextafter(upper, math.inf):
                return False
    return True


def sampled_hq_response_audit(
    first: np.ndarray, bank: np.ndarray, frequency_points: int = 4097
) -> dict:
    """Audit the frozen-construction steady-state response over 0..0.495 Fs.

    This is deliberately a sampled response sanity check, not a continuous
    frequency theorem. It evaluates every one of the 1024 target phases from
    the 2x first-stage response and the finite 512-phase tail bank.
    """
    frequencies = np.linspace(0.0, 0.495, frequency_points, dtype=np.float64)
    offsets = np.arange(HQ_OFFSET_MIN, HQ_OFFSET_MAX + 1, dtype=np.int64)
    taps = np.arange(first.size, dtype=np.float64)
    phases = np.arange(1024, dtype=np.float64)
    minimum_db = math.inf
    maximum_db = -math.inf
    maximum_complex_error = 0.0
    minimum_at = None
    maximum_at = None
    complex_at = None

    for start in range(0, frequency_points, 64):
        batch_f = frequencies[start : start + 64]
        omega = 2.0 * math.pi * batch_f
        half = np.sum(
            first[None, :]
            * np.exp(
                1j
                * omega[:, None]
                * (float(first.size // 2) - taps)[None, :]
            ),
            axis=1,
        )

        target_halves = []
        for coarse_cell in (0, 1):
            coarse_indices = coarse_cell + offsets
            source_frames = np.floor_divide(coarse_indices, 2)
            odd = np.mod(coarse_indices, 2).astype(bool)
            coarse = np.exp(1j * omega[:, None] * source_frames[None, :])
            coarse[:, odd] *= half[:, None]
            target_halves.append(coarse @ bank.T)
        response = np.concatenate(target_halves, axis=1)
        ideal = np.exp(1j * omega[:, None] * phases[None, :] / 1024.0)
        magnitude_db = 20.0 * np.log10(np.abs(response))
        complex_error = np.abs(response - ideal)

        index = np.unravel_index(np.argmin(magnitude_db), magnitude_db.shape)
        value = float(magnitude_db[index])
        if value < minimum_db:
            minimum_db = value
            minimum_at = [float(batch_f[index[0]]), int(index[1])]

        index = np.unravel_index(np.argmax(magnitude_db), magnitude_db.shape)
        value = float(magnitude_db[index])
        if value > maximum_db:
            maximum_db = value
            maximum_at = [float(batch_f[index[0]]), int(index[1])]

        index = np.unravel_index(np.argmax(complex_error), complex_error.shape)
        value = float(complex_error[index])
        if value > maximum_complex_error:
            maximum_complex_error = value
            complex_at = [float(batch_f[index[0]]), int(index[1])]

    grid_component_db = -20.0 * math.log10(math.cos(math.pi * 0.495 / 1024.0))
    return {
        "frequency_points": frequency_points,
        "phase_count": 1024,
        "minimum_magnitude_deviation_db": minimum_db,
        "minimum_at_fraction_and_phase": minimum_at,
        "maximum_magnitude_deviation_db": maximum_db,
        "maximum_at_fraction_and_phase": maximum_at,
        "maximum_complex_response_error": maximum_complex_error,
        "complex_error_at_fraction_and_phase": complex_at,
        "ideal_grid_component_db_at_0_495_fs": grid_component_db,
        "sampled_single_tone_abs_error_plus_grid_db": max(abs(minimum_db), abs(maximum_db))
        + grid_component_db,
    }


def exact_pairing_bound_for_legacy_stage(stage: np.ndarray) -> tuple[bool, float, float]:
    """Audit the pair-mean coefficient enclosure used by the dense Legacy path."""
    half = [float(value) for value in stage[1::2]]
    if len(half) % 2:
        raise RuntimeError("Legacy dense half phase lost even symmetry geometry")
    maximum_exact = Fraction(0, 1)
    maximum_bound = 0.0
    for index in range(len(half) // 2):
        mirror = len(half) - 1 - index
        left = half[index]
        right = half[mirror]
        # Multiplication by 0.5 is exact for these normal coefficients; the
        # addition rounds once exactly as the Rust expression does.
        mean = 0.5 * left + 0.5 * right
        bound = upper_nonnegative(max(abs(left - mean), abs(right - mean)))
        exact_left = abs(Fraction.from_float(left) - Fraction.from_float(mean))
        exact_right = abs(Fraction.from_float(right) - Fraction.from_float(mean))
        exact = max(exact_left, exact_right)
        maximum_exact = max(maximum_exact, exact)
        maximum_bound = max(maximum_bound, bound)
        if Fraction.from_float(bound) < exact:
            return False, float(maximum_exact), maximum_bound
    return True, float(maximum_exact), maximum_bound


def dense_stage_required_source_range_py(
    target_start: int, target_end: int, stage: np.ndarray, taps: int
) -> tuple[int, int]:
    half_len = len(stage[1::2])
    group_delay = (taps - 1) // 4
    source_start = 1 << 62
    source_end = -(1 << 62)
    for target in range(target_start, target_end + 1):
        if target % 2 == 0:
            source = target // 2
            source_start = min(source_start, source)
            source_end = max(source_end, source)
        else:
            cell = (target - 1) // 2
            source_start = min(source_start, cell + group_delay - (half_len - 1))
            source_end = max(source_end, cell + group_delay)
    return source_start, source_end


def legacy_dense_ranges_py(
    start_cell: int, end_cell: int, stages: list[np.ndarray], taps: tuple[int, ...]
) -> list[tuple[int, int]]:
    ranges = [(0, 0)] * (len(stages) + 1)
    ranges[-1] = (start_cell * LEGACY_TAIL_FACTOR, end_cell * LEGACY_TAIL_FACTOR)
    for stage_index in range(len(stages) - 1, -1, -1):
        ranges[stage_index] = dense_stage_required_source_range_py(
            *ranges[stage_index + 1], stages[stage_index], taps[stage_index]
        )
    return ranges


def legacy_dense_product_estimate_py(
    start_cell: int, end_cell: int, stages: list[np.ndarray], taps: tuple[int, ...]
) -> tuple[int, list[tuple[int, int]]]:
    ranges = legacy_dense_ranges_py(start_cell, end_cell, stages, taps)
    products = 0
    for stage_index, stage in enumerate(stages):
        target_start, target_end = ranges[stage_index + 1]
        odd_count = sum(1 for index in range(target_start, target_end + 1) if index % 2)
        products += odd_count * (len(stage[1::2]) // 2)
    return products, ranges


def sparse_full_phase_work_py(bank: np.ndarray, cell_count: int, node_credits: int) -> int:
    nonzero_counts = np.count_nonzero(bank, axis=1)
    per_cell = sum(int(nonzero_counts[phase]) + node_credits for phase in range(1, LEGACY_TAIL_FACTOR))
    return per_cell * cell_count


def operator_norm(first_full: np.ndarray, first_delay_index: int, stages: list[np.ndarray]) -> tuple[float, int]:
    response = first_full
    delay = first_delay_index
    factor = 2
    for stage in stages:
        upsampled = np.zeros(response.size * 2 - 1, dtype=np.float64)
        upsampled[::2] = response
        response = np.convolve(upsampled, stage)
        exact = np.flatnonzero(stage == 1.0)
        delay = delay * 2 + int(exact[0])
        factor *= 2
    phase_l1 = [float(np.sum(np.abs(response[phase::factor]), dtype=np.float64)) for phase in range(factor)]
    return max(phase_l1), delay


def build_hq_first() -> np.ndarray:
    index = np.arange(1536, dtype=np.float64)
    centered = index - 767.5
    full = np.sinc(centered) * np.kaiser(1536, 14.0)
    full /= full.sum(dtype=np.float64)
    if not np.array_equal(full, full[::-1]):
        full = 0.5 * (full + full[::-1])
        full /= full.sum(dtype=np.float64)
    return full


def first_response(coefficients: np.ndarray) -> np.ndarray:
    n = len(coefficients)
    response = np.zeros(2 * n, dtype=np.float64)
    response[n] = 1.0
    response[1::2] = coefficients
    while response.size > 1 and response[-1] == 0.0:
        response = response[:-1]
    return response


def audit() -> dict:
    if not DESIGN.exists():
        raise RuntimeError(f"missing controlling design: {DESIGN}")

    outward_primitives_exact = audit_outward_bound_primitives()

    # HQ: reconstruct from the design, then compare the checked-in frozen first
    # phase and finite identities. This does not import the generator.
    hq_first = build_hq_first()
    frozen_hq_half = parse_rust_f64_array(HQ_RUST, "HQ1024_HALF_DELAY_COEFFICIENTS")
    if not np.array_equal(hq_first[:768], frozen_hq_half):
        raise RuntimeError("frozen HQ first half does not reproduce the published construction")
    hq_stage_coefficients = hq_stages()
    hq_stage_responses = [hq_stage_response(stage) for stage in hq_stage_coefficients]
    hq_tail, hq_tail_delay = compose_tail(hq_stage_responses)
    hq_bank = make_bank(hq_tail, hq_tail_delay, HQ_TAIL_FACTOR, HQ_OFFSET_MIN, HQ_OFFSET_MAX)
    hq_root_a, hq_root_b = node_bound(hq_bank, HQ_TAIL_FACTOR, HQ_OFFSET_MIN, HQ_OFFSET_MAX, 0, HQ_TAIL_FACTOR)
    hq_op, hq_delay = operator_norm(first_response(hq_first), 1536, hq_stage_responses)
    hq_response = sampled_hq_response_audit(hq_first, hq_bank)

    frozen_tail_fnv = parse_rust_u64(HQ_RUST, "HQ1024_TAIL_FNV1A64")
    if fnv1a64(hq_bank) != frozen_tail_fnv:
        raise RuntimeError("frozen HQ tail checksum does not reproduce the independent construction")

    frozen_node_a = parse_rust_f64_array(HQ_RUST, "HQ1024_NODE_A_UPPER")
    frozen_node_b = parse_rust_f64_array(HQ_RUST, "HQ1024_NODE_B_UPPER")
    if frozen_node_a.size != HQ_TAIL_FACTOR or frozen_node_b.size != HQ_TAIL_FACTOR:
        raise RuntimeError("frozen HQ node metadata has the wrong geometry")
    exact_node_metadata_outward = all_nodes_exactly_outward(
        hq_bank,
        HQ_TAIL_FACTOR,
        HQ_OFFSET_MIN,
        HQ_OFFSET_MAX,
        frozen_node_a,
        frozen_node_b,
    )
    hq_bound_stress = deterministic_bound_stress(
        hq_bank,
        HQ_TAIL_FACTOR,
        HQ_OFFSET_MIN,
        HQ_OFFSET_MAX,
        frozen_node_a,
        frozen_node_b,
        8,
    )

    # Legacy tail: independently reproduce the runtime-generated Blackman
    # stages. The first phase is frozen in source, so reconstruct its symmetric
    # full vector from that source table before measuring the full operator norm.
    published_stage_lengths = tuple(
        parse_rust_usize(LIB_RUST, f"HEADROOM64_STAGE_{index}_TAPS")
        for index in range(2, 7)
    )
    legacy_stages = [legacy_half_stage(taps) for taps in published_stage_lengths]
    dense_pair_audits = [exact_pairing_bound_for_legacy_stage(stage) for stage in legacy_stages]
    dense_pairing_outward = all(result[0] for result in dense_pair_audits)
    dense_pair_exact_max = max(result[1] for result in dense_pair_audits)
    dense_pair_bound_max = max(result[2] for result in dense_pair_audits)
    authority_coefficients = [
        *[float(value) for value in frozen_hq_half],
        *[float(value) for stage in hq_stage_responses for value in stage],
        *[float(value) for value in parse_rust_f64_array(LEGACY_RUST, "HEADROOM64_HALF_DELAY_COEFFICIENTS")],
        *[float(value) for stage in legacy_stages for value in stage],
    ]
    nonzero_authority_coefficients = [abs(value) for value in authority_coefficients if value != 0.0]
    authority_coefficients_normal_and_unit_bounded = (
        max(nonzero_authority_coefficients) <= 1.0
        and min(nonzero_authority_coefficients) >= BINARY64_MIN_NORMAL
    )
    authority_coefficient_abs_max = max(nonzero_authority_coefficients)
    authority_coefficient_abs_min_nonzero = min(nonzero_authority_coefficients)
    legacy_tail, legacy_tail_error, legacy_tail_delay, legacy_tail_exact = (
        compose_legacy_rust_order(legacy_stages)
    )
    legacy_composition_error_upper = phase_bank_error(
        legacy_tail_error,
        legacy_tail_delay,
        LEGACY_TAIL_FACTOR,
        LEGACY_OFFSET_MIN,
        LEGACY_OFFSET_MAX,
    )
    legacy_exact_composition_discrepancy = exact_phase_bank_discrepancy(
        legacy_tail,
        legacy_tail_exact,
        legacy_tail_delay,
        LEGACY_TAIL_FACTOR,
        LEGACY_OFFSET_MIN,
        LEGACY_OFFSET_MAX,
    )
    legacy_bank = make_bank(
        legacy_tail,
        legacy_tail_delay,
        LEGACY_TAIL_FACTOR,
        LEGACY_OFFSET_MIN,
        LEGACY_OFFSET_MAX,
    )
    hq_l1_outward, hq_l1_upper, hq_l1_exact = phase_l1_outward_audit(hq_bank)
    legacy_l1_outward, legacy_l1_upper, legacy_l1_exact = phase_l1_outward_audit(legacy_bank)
    node_processing_credits = parse_rust_u64_constant(SCANNER_RUST, "NODE_PROCESSING_CREDITS")
    dense_products_8, dense_ranges_8 = legacy_dense_product_estimate_py(
        0, 8, legacy_stages, published_stage_lengths
    )
    sparse_work_8 = sparse_full_phase_work_py(legacy_bank, 8, node_processing_credits)
    legacy_root_a, legacy_root_b = node_bound(
        legacy_bank,
        LEGACY_TAIL_FACTOR,
        LEGACY_OFFSET_MIN,
        LEGACY_OFFSET_MAX,
        0,
        LEGACY_TAIL_FACTOR,
    )
    legacy_node_a, legacy_node_b = rust_runtime_node_metadata(
        legacy_bank, LEGACY_TAIL_FACTOR, LEGACY_OFFSET_MIN, LEGACY_OFFSET_MAX
    )
    legacy_node_metadata_outward = all_nodes_exactly_outward(
        legacy_bank,
        LEGACY_TAIL_FACTOR,
        LEGACY_OFFSET_MIN,
        LEGACY_OFFSET_MAX,
        legacy_node_a,
        legacy_node_b,
    )
    legacy_bound_stress = deterministic_bound_stress(
        legacy_bank,
        LEGACY_TAIL_FACTOR,
        LEGACY_OFFSET_MIN,
        LEGACY_OFFSET_MAX,
        legacy_node_a,
        legacy_node_b,
        64,
    )

    legacy_half = parse_rust_f64_array(LEGACY_RUST, "HEADROOM64_HALF_DELAY_COEFFICIENTS")
    legacy_full = np.concatenate([legacy_half, legacy_half[::-1]])
    legacy_op, legacy_delay = operator_norm(first_response(legacy_full), 384, legacy_stages)

    scanner_source = SCANNER_RUST.read_text()
    meter_test_source = METER_TEST_RUST.read_text()
    lib_source = LIB_RUST.read_text()
    peak_interval_source = lib_source.split("impl PeakInterval {", 1)[1].split(
        "/// Work and numerical diagnostics", 1
    )[0]
    width_db_source = peak_interval_source.split("pub fn width_db", 1)[1].split("pub fn upper_level", 1)[0]
    upper_level_source = peak_interval_source.split("pub fn upper_level", 1)[1].split("pub const fn is_silence", 1)[0]
    is_silence_source = peak_interval_source.split("pub const fn is_silence", 1)[1]
    qualified_prefix_authority = (
        "use super::qualified_half_delay_fft::QualifiedHalfDelayFft;" in scanner_source
        and "coarse_group_upper" in scanner_source
        and "CoarseChannelSummary::build" in scanner_source
        and "authoritative_coarse_values" in scanner_source
        and "raw_group_upper" not in scanner_source
        and "CandidateWork::RawCell" not in scanner_source
    )
    fast90_l1_prefilter_is_authority_bearing = (
        "phase_l1_upper: f64" in scanner_source
        and "fn phase_l1_upper(" in scanner_source
        and "fn fast90_l1_group_upper(" in scanner_source
        and "let magnitude_upper = summary.group_support_max" in scanner_source
        and "tail.phase_l1_upper" in scanner_source
        and "tail.composition_error_per_coarse_peak_upper" in scanner_source
        and "upper_mul(coefficient_upper, magnitude_upper)" in scanner_source
        and "if l1_upper <= search_lower" in scanner_source
        and "generate_fast90_channel_candidates" in scanner_source
    )
    fast90_prefix_executor_is_policy_scoped = (
        "SearchPolicy::Reference9 => QualifiedHalfDelayFft::new(spec.id, channels)" in scanner_source
        and "SearchPolicy::Fast90 | SearchPolicy::Fast1sPerMinute" in scanner_source
        and "QualifiedHalfDelayFft::new_fast90(spec.id, channels)" in scanner_source
    )
    daz_safe_logarithmic_views = (
        "fn positive_finite_linear_to_dbtp(linear: f64) -> f64" in lib_source
        and "1074.0 * std::f64::consts::LOG10_2" in lib_source
        and "const fn f64_magnitude_bits_for_db(value: f64) -> u64" in lib_source
        and "value.to_bits() & F64_MAGNITUDE_MASK_FOR_DB" in lib_source
        and "core::mem::transmute::<f64, u64>(value)" not in lib_source
        and "let upper_bits = f64_magnitude_bits_for_db(self.upper_linear);" in lib_source
        and "let lower_bits = f64_magnitude_bits_for_db(self.lower_linear);" in lib_source
        and "if upper_bits == 0" in lib_source
        and "if lower_bits == 0" in lib_source
        and "if f64_magnitude_bits_for_db(self.upper_linear) == 0" in lib_source
        and "f64_magnitude_bits_for_db(self.lower_linear) == 0" in lib_source
        and "&& f64_magnitude_bits_for_db(self.upper_linear) == 0" in lib_source
        and "self.upper_linear == 0.0" not in width_db_source
        and "self.lower_linear == 0.0" not in width_db_source
        and "self.upper_linear == 0.0" not in upper_level_source
        and "self.upper_linear == 0.0" not in is_silence_source
        and "self.lower_linear == 0.0" not in is_silence_source
        and "positive_finite_linear_to_dbtp(self.upper_linear)" in lib_source
        and "positive_finite_linear_to_dbtp(linear)" in lib_source
        and "assert!(!subnormal_interval.is_silence());" in scanner_source
        and "subnormal_interval.width_db().unwrap().is_finite()" in scanner_source
        and "produced non-finite dBTP for finite subnormal input" in scanner_source
        and "assert!(silence_interval.is_silence());" in scanner_source
        and "assert!(silence_interval.width_db().is_none());" in scanner_source
    )
    fast90_coarse_lower_and_magnitude_are_single_pass = (
        "fn build_fast90_magnitude_summary(" in scanner_source
        and "upper_values.push(evaluation.upper());" in scanner_source
        and "point_peak = max_exact_magnitude(point_peak, evaluation.value);" in scanner_source
        and "lower_peak = max_nonnegative_finite(lower_peak, evaluation.lower());" in scanner_source
        and "CoarseMagnitudeSummary::from_upper_values" in scanner_source
    )
    fast90_curvature_summary_reuses_rolling_coarse_knots = (
        "Each adjacent d2 shares two of its three coarse knots with the next." in scanner_source
        and "let mut first = evaluation_at(0);" in scanner_source
        and "let mut middle = evaluation_at(1);" in scanner_source
        and "d2_values.push(second_difference_upper(first, middle, last));" in scanner_source
        and "first = middle;" in scanner_source
        and "middle = last;" in scanner_source
    )
    reference9_release_incomplete_fails_closed = (
        "SearchPolicy::Reference9 => return Err(TruePeakError::ReferenceSearchIncomplete)" in scanner_source
        and "reference9_live_unresolved_upper_fails_closed_in_release_semantics" in scanner_source
        and "ReferenceSearchIncomplete" in lib_source
        and 'debug_assert!(false, "Reference9 must not finalize with a live unresolved upper")' not in scanner_source
    )
    reference9_dense_and_rescore_present = (
        "apply_legacy_dense_stage" in scanner_source
        and "legacy_dense_product_estimate" in scanner_source
        and "dense_region_is_cheaper" in scanner_source
        and "dense_complete_regions" in scanner_source
        and "direct_rescore_evaluations" in scanner_source
        and "RESCORE_FRONTIER_CAPACITY" in scanner_source
    )
    oracle_regression = re.search(
        r"fn hq_reference_is_cross_checked_against_legacy_direct_oracle_on_analytical_intersample_carrier\(\) \{"
        r".*?\n    \}\n\n    #\[test\]\n    fn reference9_contains_independent_exhaustive_short_reconstructions",
        scanner_source,
        re.DOTALL,
    )
    constant_chunk_regression = re.search(
        r"fn reference9_constant_carrier_preserves_certificate_across_former_1_vs_37_boundary\(\) \{"
        r".*?\n    \}\n\n    #\[test\]\n    fn endpoint_winners_use_real_track_edges_during_rescore",
        scanner_source,
        re.DOTALL,
    )
    legacy_oracle_is_actually_exercised = (
        oracle_regression is not None
        and "let legacy_raw = legacy_direct_oracle_peak(" in oracle_regression.group(0)
        and "ReconstructionId::Hq1024V1" in oracle_regression.group(0)
        and "LEGACY_QUALIFIED_ABS_ERROR_DB" in oracle_regression.group(0)
        and "sample_peak < analytical_peak * 0.92" in oracle_regression.group(0)
    )
    focused_reference9_regressions_present = (
        legacy_oracle_is_actually_exercised
        and constant_chunk_regression is not None
        and "37," in constant_chunk_regression.group(0)
        and "scan_with_chunk_pattern(" in constant_chunk_regression.group(0)
        and "ReconstructionId::LegacyHeadroom64" in constant_chunk_regression.group(0)
        and "ReconstructionId::Hq1024V1" in constant_chunk_regression.group(0)
        and "fn public_reference_constant_carrier_is_chunk_invariant_across_edges()" in meter_test_source
        and "let thirty_seven = run(&[37]);" in meter_test_source
        and "EdgePolicy::RepeatEndpoints, EdgePolicy::ZeroExtend" in meter_test_source
        and "rescore_frontier_equal_upper_retention_is_canonical" in scanner_source
        and "candidate_node_order_is_total_for_equal_upper_distinct_work" in scanner_source
        and "rescore_frontier_returns_every_live_competitor_it_cannot_retain" in scanner_source
        and "endpoint_winners_use_real_track_edges_during_rescore" in scanner_source
        and "legacy_complete_dense_fallback_has_a_stable_forcing_fixture" in scanner_source
        and "forcing fixture must enter the complete Legacy64 dense fallback" in scanner_source
        and "hq_reference_dense_intermediate_path_has_a_stable_forcing_fixture" in scanner_source
        and "forcing fixture must enter HQ intermediate dense execution" in scanner_source
        and "reference9_certificate_is_chunk_invariant_on_non_degenerate_audio" in scanner_source
        and "reference9_dense_and_rescore_paths_are_observable_and_chunk_invariant" not in scanner_source
    )
    shipping_stress_fixtures_self_validate = (
        "hot/silence fixture lost its hot channel" in scanner_source
        and "hot/silence fixture lost its exactly silent packed partner" in scanner_source
        and "cancellation fixture no longer reaches the intended near-cancellation state" in scanner_source
        and "odd-channel fixture must remain odd" in scanner_source
        and "subnormal fixture must contain a positive subnormal packed partner" in scanner_source
        and "fixture must force the final partial FFT block through flush" in scanner_source
        and "normal Fast90 must not reconstruct the long first-stage FIR candidate by candidate" in scanner_source
        and "fixture must process at least one complete canonical tile through the qualified 2x authority layer" in scanner_source
        and "localized-peak fixture must demonstrate certified 64/8/1 rejection" in scanner_source
        and "mono fixture must process a complete tile through qualified 2x authority" in scanner_source
        and "fixture must inject enclosed perturbations across the coarse support" in scanner_source
    )
    bench_source = BENCH_RUST.read_text()
    binding_benchmark_uses_total_timer_without_projection = (
        "let total_started = Instant::now();" in bench_source
        and "let total_seconds = total_started.elapsed().as_secs_f64();" in bench_source
        and "total_wall_seconds" in bench_source
        and "total_seconds_for_40_minute_album" not in bench_source
    )
    cargo_source = CARGO_TOML.read_text()
    stale_msrv_workaround_is_retired = (
        'rust-version = "1.93"' in cargo_source
        and "core::mem::transmute::<f64, u64>(value)" not in lib_source
    )
    public_surface_is_exactly_three_hq_tiers = (
        "pub enum PeakTier" in lib_source
        and all(name in lib_source for name in ["Reference,", "Standard,", "Fast,"])
        and "pub enum CertifiedReconstruction" in lib_source
        and "Hq1024V1," in lib_source
        and "pub enum TruePeakMode" not in lib_source
        and "pub enum HeadroomScanMode" not in lib_source
        and "pub struct HeadroomCeilingMeter" not in lib_source
        and "ReconstructionId::Hq1024V1" in lib_source
        and "pub struct ReportingPeakMeter" in lib_source
    )
    fast_time_policy_is_rate_based_and_fail_closed = (
        "FAST_WALL_SECONDS_PER_PROGRAMME_MINUTE: u64 = 1" in lib_source
        and "const FAST_METER_NANOS_PER_PROGRAMME_MINUTE: u128 = 900_000_000;" in scanner_source
        and "const FAST_STARTUP_BURST_NANOS: u128 = 50_000_000;" in scanner_source
        and "Fast1sPerMinute" in scanner_source
        and "fn fast_allowed_processing_nanos" in scanner_source
        and "u128::from(self.sample_rate_hz).saturating_mul(60)" in scanner_source
        and "FAST_STARTUP_BURST_NANOS.saturating_add(" in scanner_source
        and ".saturating_mul(FAST_METER_NANOS_PER_PROGRAMME_MINUTE)" in scanner_source
        and "fn fast_time_budget_exhausted" in scanner_source
        and "SearchStatus::TimeLimited" in scanner_source
        and "time_limited_tiles" in scanner_source
        and "time_bounded_prefix_blocks_skipped" in scanner_source
        and "fn retain_fast_tile_fallback" in scanner_source
        and "raw_support_for_coarse_range(required_coarse_start, required_coarse_end)" in scanner_source
        and "raw_peak = max_exact_magnitude(raw_peak, self.raw.sample(input_index, channel));" in scanner_source
        and "upper_mul(raw_peak, reconstruction_gain)" in scanner_source
        and "process_frame_with_fft_permission" in scanner_source
        and "fast_tile_fallback_is_local_to_the_skipped_tile_support" in scanner_source
        and "mark_fast_partial_candidate_tile_time_limited" in scanner_source
        and "fast_partial_candidate_deadline_preserves_completed_channel_root" in scanner_source
        and "exhausted_fast_clock_retains_unresolved_upper_and_reports_time_limited" in scanner_source
    )
    legacy_oracle_is_internal_only = (
        "pub(crate) enum ReconstructionId" in lib_source
        and "LegacyHeadroom64" in lib_source
        and "pub(crate) fn legacy_headroom64_direct_oracle_peak" in lib_source
    )
    benchmark_exposes_only_three_tiers = (
        '"reference" => PeakTier::Reference' in bench_source
        and '"standard" => PeakTier::Standard' in bench_source
        and '"fast" => PeakTier::Fast' in bench_source
        and "legacy-reference" not in bench_source
        and "fast_wall_target_met" in bench_source
    )
    hq_source = HQ_RUST.read_text()
    hq_generator_source = HQ_GENERATOR.read_text()
    hq_runtime_has_no_redundant_stage_tables = (
        re.search(r"HQ1024_STAGE_(?:[2-9]|10)_HALF_COEFFICIENTS", hq_source) is None
        and "for index, coefficients in enumerate(stages, start=2)" not in hq_generator_source
        and "tail, tail_delay = compose(np.asarray([1.0], dtype=np.float64), 0, stages)" in hq_generator_source
        and "\"stage_half_lengths\": list(STAGE_LENGTHS)" in hq_generator_source
        and all(
            f"#[cfg(test)]\npub(crate) const {name}" in hq_source
            for name in [
                "HQ1024_DELAY_SUBFRAMES",
                "HQ1024_TAIL_DELAY_SUBFRAMES",
                "HQ1024_MEASURED_OPERATOR_LINF",
                "HQ1024_FIRST_HALF_FNV1A64",
                "HQ1024_TAIL_FNV1A64",
                "HQ1024_NODE_A_FNV1A64",
                "HQ1024_NODE_B_FNV1A64",
            ]
        )
    )
    legacy_composition_exact = float(legacy_exact_composition_discrepancy)

    checks = {
        "direct_bound_primitives_are_exactly_outward_on_edge_cases": outward_primitives_exact,
        "hq_delay_target_subframes": hq_delay == 795_354,
        "hq_tail_delay_subframes": hq_tail_delay == 8_922,
        "hq_operator_matches_design_probe": abs(hq_op - 4.676026435151283) < 5.0e-14,
        "hq_root_a_matches_design_probe": abs(hq_root_a - 0.329422342667) < 5.0e-13,
        "hq_root_affine_defect_small": hq_root_b < 2.0e-15,
        "hq_sampled_response_within_design_envelope": (
            hq_response["minimum_magnitude_deviation_db"] > -2.0e-6
            and hq_response["maximum_magnitude_deviation_db"] < 2.0e-6
            and hq_response["maximum_complex_response_error"] < 2.1e-7
        ),
        "hq_sampled_single_tone_objective_has_margin": (
            hq_response["sampled_single_tone_abs_error_plus_grid_db"] < 1.0e-4
        ),
        "hq_all_frozen_dyadic_node_bounds_are_exactly_outward": exact_node_metadata_outward,
        "hq_dyadic_bound_stress": hq_bound_stress,
        "hq_width_256_matches_design_probe": abs(max_node_a_for_width(hq_bank, HQ_TAIL_FACTOR, HQ_OFFSET_MIN, HQ_OFFSET_MAX, 256) - 0.076668792502) < 5.0e-13,
        "hq_width_128_matches_design_probe": abs(max_node_a_for_width(hq_bank, HQ_TAIL_FACTOR, HQ_OFFSET_MIN, HQ_OFFSET_MAX, 128) - 0.021697051293) < 5.0e-13,
        "hq_width_2_matches_design_probe": abs(max_node_a_for_width(hq_bank, HQ_TAIL_FACTOR, HQ_OFFSET_MIN, HQ_OFFSET_MAX, 2) - 5.5029401967e-6) < 5.0e-15,
        "legacy_root_a_matches_design_probe": abs(legacy_root_a - 0.385054359363) < 5.0e-12,
        "legacy_runtime_dyadic_node_bounds_are_exactly_outward": legacy_node_metadata_outward,
        "legacy_dyadic_bound_stress": legacy_bound_stress,
        "legacy_public_norm_contains_operator": legacy_op <= 4.09,
        "legacy_runtime_composition_bound_contains_exact_rational_discrepancy": (
            Fraction.from_float(legacy_composition_error_upper)
            >= legacy_exact_composition_discrepancy
        ),
        "legacy_runtime_composition_bound_is_usefully_tight": (
            legacy_composition_error_upper < 2.0e-14
        ),
        "authority_coefficients_are_normal_and_abs_at_most_one": (
            authority_coefficients_normal_and_unit_bounded
        ),
        "legacy_dense_pairing_bounds_are_exactly_outward": dense_pairing_outward,
        "legacy_dense_halo_matches_declared_tail_support": (
            dense_ranges_8[0] == (LEGACY_OFFSET_MIN, 7 + LEGACY_OFFSET_MAX)
        ),
        "legacy_dense_work_model_prefers_complete_block_for_8_cells": (
            dense_products_8 < sparse_work_8
        ),
        "qualified_2x_prefix_is_normal_certificate_authority": qualified_prefix_authority,
        "fast90_l1_prefilter_is_authority_bearing": fast90_l1_prefilter_is_authority_bearing,
        "fast90_phase_l1_bounds_are_exactly_outward": hq_l1_outward and legacy_l1_outward,
        "fast90_prefix_executor_is_policy_scoped": fast90_prefix_executor_is_policy_scoped,
        "daz_safe_logarithmic_views": daz_safe_logarithmic_views,
        "fast90_coarse_lower_and_magnitude_are_single_pass": fast90_coarse_lower_and_magnitude_are_single_pass,
        "fast90_curvature_summary_reuses_rolling_coarse_knots": fast90_curvature_summary_reuses_rolling_coarse_knots,
        "reference9_release_incomplete_fails_closed": reference9_release_incomplete_fails_closed,
        "reference9_dense_and_rescore_paths_present": reference9_dense_and_rescore_present,
        "legacy_direct_oracle_is_exercised_against_hq_analytical_reference": (
            legacy_oracle_is_actually_exercised
        ),
        "reference9_focused_oracle_frontier_edge_regressions_present": (
            focused_reference9_regressions_present
        ),
        "shipping_stress_fixtures_self_validate": shipping_stress_fixtures_self_validate,
        "binding_benchmark_uses_total_timer_without_projection": (
            binding_benchmark_uses_total_timer_without_projection
        ),
        "stale_msrv_workaround_is_retired": stale_msrv_workaround_is_retired,
        "public_surface_is_exactly_three_hq_tiers": public_surface_is_exactly_three_hq_tiers,
        "fast_time_policy_is_rate_based_and_fail_closed": fast_time_policy_is_rate_based_and_fail_closed,
        "legacy_oracle_is_internal_only": legacy_oracle_is_internal_only,
        "benchmark_exposes_only_three_tiers": benchmark_exposes_only_three_tiers,
        "hq_runtime_omits_redundant_stage_tables_but_generator_retains_construction": (
            hq_runtime_has_no_redundant_stage_tables
        ),
    }
    if not all(checks.values()):
        failed = [name for name, passed in checks.items() if not passed]
        raise RuntimeError("certified-search audit failed: " + ", ".join(failed))

    source_digest = hashlib.sha256(HQ_RUST.read_bytes()).hexdigest()
    return {
        "schema": 1,
        "status": "pass",
        "checks": checks,
        "direct_authority": {
            "binary64_unit_roundoff": BINARY64_UNIT_ROUNDOFF,
            "daz_input_units_per_operation": DAZ_INPUT_UNITS_PER_OPERATION,
            "ftz_result_units_per_operation": FTZ_RESULT_UNITS_PER_OPERATION,
            "fused_residual_underflow_units": FUSED_RESIDUAL_UNDERFLOW_UNITS,
            "absolute_underflow_units_per_operation": DIRECT_UNDERFLOW_UNITS_PER_OPERATION,
            "authority_coefficient_abs_max": authority_coefficient_abs_max,
            "authority_coefficient_abs_min_nonzero": authority_coefficient_abs_min_nonzero,
        },
        "hq1024": {
            "delay_target_subframes": hq_delay,
            "tail_delay_subframes": hq_tail_delay,
            "tail_support": [HQ_OFFSET_MIN, HQ_OFFSET_MAX],
            "tail_nonzero_count_range": [int(np.count_nonzero(hq_bank, axis=1).min()), int(np.count_nonzero(hq_bank, axis=1).max())],
            "operator_linf_measured_from_frozen_construction": hq_op,
            "root_curvature_a": hq_root_a,
            "root_affine_b": hq_root_b,
            "phase_l1_outward_upper": hq_l1_upper,
            "phase_l1_exact_binary64_sum": hq_l1_exact,
            "node_a_width_256_max": max_node_a_for_width(hq_bank, HQ_TAIL_FACTOR, HQ_OFFSET_MIN, HQ_OFFSET_MAX, 256),
            "node_a_width_128_max": max_node_a_for_width(hq_bank, HQ_TAIL_FACTOR, HQ_OFFSET_MIN, HQ_OFFSET_MAX, 128),
            "node_a_width_2_max": max_node_a_for_width(hq_bank, HQ_TAIL_FACTOR, HQ_OFFSET_MIN, HQ_OFFSET_MAX, 2),
            "frozen_rust_sha256": source_digest,
            "sampled_response_audit": hq_response,
        },
        "legacy64": {
            "delay_target_subframes": legacy_delay,
            "tail_delay_subframes": legacy_tail_delay,
            "tail_support": [LEGACY_OFFSET_MIN, LEGACY_OFFSET_MAX],
            "operator_linf_measured_from_reconstructed_runtime_filters": legacy_op,
            "root_curvature_a": legacy_root_a,
            "root_affine_b": legacy_root_b,
            "phase_l1_outward_upper": legacy_l1_upper,
            "phase_l1_exact_binary64_sum": legacy_l1_exact,
            "runtime_root_curvature_a_upper": float(legacy_node_a[1]),
            "runtime_root_affine_b_upper": float(legacy_node_b[1]),
            "exact_real_vs_rounded_tail_max_phase_l1": legacy_composition_exact,
            "derived_runtime_composition_error_per_coarse_peak_upper": legacy_composition_error_upper,
            "dense_pair_exact_coefficient_discrepancy_max": dense_pair_exact_max,
            "dense_pair_outward_coefficient_bound_max": dense_pair_bound_max,
            "dense_8_cell_required_coarse_range": list(dense_ranges_8[0]),
            "dense_8_cell_pair_products": dense_products_8,
            "sparse_8_cell_full_phase_product_plus_node_work": sparse_work_8,
        },
        "qualification_scope": {
            "coefficient_construction_reproduced": True,
            "finite_tail_support_checked": True,
            "dyadic_coefficient_identities_checked": True,
            "operator_norm_recomputed": True,
            "sampled_hq_response_checked": True,
            "certified_fft_authoritative": True,
            "runtime_fft_rounding_proof": True,
            "certified_fft_implementation": "crate-owned fixed radix-2 prefix; scalar graph with same-graph AVX pair dispatch",
            "rustfft_runtime_backend_is_certificate_authority": False,
            "direct_authority_rounding_derivation_checked": True,
            "direct_authority_coefficient_domain_checked": True,
            "legacy_composition_rounding_derivation_checked": True,
            "legacy_dense_pairing_enclosure_checked": True,
            "legacy_dense_source_work_model_recomputed": True,
            "legacy_dense_shipping_crossover_measured": False,
            "shipping_backend_verified": False,
            "shipping_rust_tests_run_here": False,
            "commissioning_benchmark_run_here": False,
        },
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--output",
        type=Path,
        default=Path(__file__).with_name("certified_search_report.json"),
    )
    args = parser.parse_args()
    report = audit()
    args.output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    print(json.dumps(report, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
