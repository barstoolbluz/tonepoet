#!/usr/bin/env python3
"""Independent offline qualification for the accelerated Reference scanner.

This is developer tooling only. Runtime/build/test paths do not depend on
Python, NumPy, SciPy, generated stamps, or an external qualification profile.
Ordinary Rust tests freeze the production constants independently.
"""
from __future__ import annotations

import argparse
import json
import math
import pathlib
import re

import numpy as np
from scipy.signal import upfirdn

HALF_DELAY_TAPS = 384
LATER_TAPS = (49, 25, 17, 13, 9)
RUNTIME_RESIDUAL_L1_UPPER = np.asarray((0.0122, 0.0199, 0.0199, 0.0122))
RUNTIME_RESIDUAL_SUM_UPPER = np.asarray((1.0e-12,) * 4)
RUNTIME_TAIL_L1_UPPER = 1.739
TAIL_OFFSET_MIN = -9
TAIL_OFFSET_MAX = 9


def parse_half_coefficients(path: pathlib.Path) -> np.ndarray:
    text = path.read_text(encoding="utf-8")
    body = text.split("[", 2)[-1].rsplit("]", 1)[0]
    values = [float(token) for token in re.findall(r"[-+]?\d+\.\d+e[-+]\d+", body, re.I)]
    if len(values) != HALF_DELAY_TAPS // 2:
        raise RuntimeError(f"expected 192 checked-in coefficients, found {len(values)}")
    half = np.asarray(values, dtype=np.float64)
    return np.concatenate((half, half[::-1]))


def generic_two_x(taps: int) -> np.ndarray:
    tap = np.arange(taps, dtype=np.float64)
    center = (taps - 1) / 2.0
    x = (tap - center) / 2.0
    h = np.sinc(x)
    phase = tap / (taps - 1)
    h *= 0.42 - 0.5 * np.cos(2.0 * np.pi * phase) + 0.08 * np.cos(4.0 * np.pi * phase)
    for branch in range(2):
        h[branch::2] /= h[branch::2].sum()
    return h


def stage1(half_delay: np.ndarray) -> np.ndarray:
    # Match HeadroomHalfSampleStage's finite impulse exactly: the identity
    # branch sits at physical index 384, and the 384 half-phase coefficients
    # occupy odd output indices 1..767. There is no synthetic trailing zero.
    response = np.zeros(2 * HALF_DELAY_TAPS, dtype=np.float64)
    response[HALF_DELAY_TAPS] = 1.0
    response[1 : 2 * HALF_DELAY_TAPS : 2] = half_delay
    return response


def runtime_two_x_response(taps: int) -> np.ndarray:
    # HeadroomTwoXStage allocates `delay_frames * 2`, which leaves one
    # explicit trailing zero for every odd-tap stage.
    return np.pad(generic_two_x(taps), (0, 1))


def symmetric_two_x_response(taps: int) -> np.ndarray:
    """Match FastHeadroomTwoXStage's paired-coefficient execution."""
    h = generic_two_x(taps).copy()
    half = h[1::2].copy()
    for index in range(half.size // 2):
        mirror = half.size - 1 - index
        average = 0.5 * (half[index] + half[mirror])
        half[index] = average
        half[mirror] = average
    h[1::2] = half
    return np.pad(h, (0, 1))


def cascade(filters: list[np.ndarray], *, trim_trailing_zeroes: bool = False) -> np.ndarray:
    response = np.asarray([1.0], dtype=np.float64)
    for h in filters:
        upsampled = np.zeros(response.size * 2 - 1, dtype=np.float64)
        upsampled[::2] = response
        response = np.convolve(upsampled, h)
        if trim_trailing_zeroes:
            while response.size > 1 and response[-1] == 0.0:
                response = response[:-1]
    return response


def aligned_phase_kernel(response: np.ndarray, factor: int, phase: int, total_delay: int) -> dict[int, float]:
    """Map x[n+k] coefficients to output knot factor*n+phase after delay removal."""
    out: dict[int, float] = {}
    # y[m] = sum_i response[i] x_up[m-i]; nonzero x_up indices are factor*q.
    # For nominal m = factor*n + phase + total_delay, q=n+k.
    # i = phase + total_delay - factor*k.
    k_min = math.floor((phase + total_delay - (len(response) - 1)) / factor)
    k_max = math.ceil((phase + total_delay) / factor)
    for k in range(k_min, k_max + 1):
        index = phase + total_delay - factor * k
        if 0 <= index < len(response):
            value = float(response[index])
            if value != 0.0:
                out[k] = value
    return out


def impulse_sample(response: np.ndarray, response_delay: int, physical_index: int) -> float:
    index = physical_index + response_delay
    if index < 0 or index >= response.size:
        return 0.0
    return float(response[index])


def cubic_sample(response: np.ndarray, response_delay: int, fine_index: int, ratio: int) -> float:
    q, remainder = divmod(fine_index, ratio)
    t = remainder / ratio
    ym1 = impulse_sample(response, response_delay, q - 1)
    y0 = impulse_sample(response, response_delay, q)
    y1 = impulse_sample(response, response_delay, q + 1)
    y2 = impulse_sample(response, response_delay, q + 2)
    wm1 = -t * (t - 1.0) * (t - 2.0) / 6.0
    w0 = (t + 1.0) * (t - 1.0) * (t - 2.0) / 2.0
    w1 = -(t + 1.0) * t * (t - 2.0) / 2.0
    w2 = (t + 1.0) * t * (t - 1.0) / 6.0
    return wm1 * ym1 + w0 * y0 + w1 * y1 + w2 * y2


def check_tail_identity(tail: np.ndarray, tail_delay: int, seed: int = 0x544F4E45) -> float:
    rng = np.random.default_rng(seed)
    source = rng.normal(size=257).astype(np.float64)
    direct = source
    for taps in LATER_TAPS[1:]:
        direct = upfirdn(generic_two_x(taps), direct, up=2)
    composed = upfirdn(tail, source, up=16)
    if direct.shape != composed.shape:
        raise RuntimeError(f"tail identity length mismatch {direct.shape} != {composed.shape}")
    worst = float(np.max(np.abs(direct - composed)))
    # This is ordinary binary64 composition, not a runtime rounding certificate.
    if worst > 8.0e-15:
        raise RuntimeError(f"tail factorization mismatch {worst:.17g}")
    if tail_delay != 144:
        raise RuntimeError(f"unexpected tail delay {tail_delay}")
    return worst


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--source",
        type=pathlib.Path,
        default=pathlib.Path(__file__).resolve().parents[1],
        help="tonepoet-true-peak crate root",
    )
    parser.add_argument("--report", type=pathlib.Path)
    args = parser.parse_args()

    source = args.source.resolve()
    half = parse_half_coefficients(source / "src" / "headroom64_coefficients.rs")
    filters = [stage1(half)] + [runtime_two_x_response(taps) for taps in LATER_TAPS]
    full = cascade(filters)
    full_delay = 12_816
    prefix4_filters = filters[:2]
    prefix4 = cascade(prefix4_filters)
    prefix4_delay = 792
    runtime_prefix4 = cascade([filters[0], symmetric_two_x_response(LATER_TAPS[0])])
    if runtime_prefix4.shape != prefix4.shape:
        raise RuntimeError("symmetric Reference-4x prefix geometry changed")
    runtime_prefix4_l1_delta = max(
        sum(abs(float(runtime_prefix4[index] - prefix4[index]))
            for index in range(phase, prefix4.size, 4))
        for phase in range(4)
    )
    if runtime_prefix4_l1_delta > 1.0e-12:
        raise RuntimeError(
            f"symmetric stage-2 execution delta {runtime_prefix4_l1_delta:.17g} is unexpectedly large"
        )
    tail_filters = [generic_two_x(taps) for taps in LATER_TAPS[1:]]
    tail = cascade(tail_filters, trim_trailing_zeroes=True)
    tail_delay = 144

    if (len(full), full_delay) != (25_632, 12_816):
        raise RuntimeError(f"unexpected full geometry {len(full)}/{full_delay}")
    if (len(tail), tail_delay) != (289, 144):
        raise RuntimeError(f"unexpected tail geometry {len(tail)}/{tail_delay}")

    full_min = -full_delay
    full_max = full.size - 1 - full_delay
    coarse_min = -prefix4_delay
    coarse_max = prefix4.size - 1 - prefix4_delay
    min_physical = min(full_min, 16 * (coarse_min - 2)) - 128
    max_physical = max(full_max, 16 * (coarse_max + 2) + 15) + 128
    phase_l1 = np.zeros(64, dtype=np.float64)
    phase_sum = np.zeros(64, dtype=np.float64)
    for phase in range(64):
        physical = min_physical + ((phase - min_physical) % 64)
        absolute_sum = 0.0
        signed_sum = 0.0
        while physical <= max_physical:
            residual = (
                impulse_sample(full, full_delay, physical)
                - cubic_sample(prefix4, prefix4_delay, physical, 16)
            )
            absolute_sum += abs(residual)
            signed_sum += residual
            physical += 64
        phase_l1[phase] = absolute_sum
        phase_sum[phase] = abs(signed_sum)
    residual_l1 = np.asarray([max(phase_l1[p * 16 : (p + 1) * 16]) for p in range(4)])
    residual_sum = np.asarray([max(phase_sum[p * 16 : (p + 1) * 16]) for p in range(4)])
    worst_residual_subphase = np.asarray(
        [int(np.argmax(phase_l1[p * 16 : (p + 1) * 16])) for p in range(4)],
        dtype=np.int64,
    )

    if np.any(residual_l1 > RUNTIME_RESIDUAL_L1_UPPER):
        raise RuntimeError(
            f"residual L1 {residual_l1.tolist()} exceeds runtime upper "
            f"{RUNTIME_RESIDUAL_L1_UPPER.tolist()}"
        )
    if np.any(residual_sum > RUNTIME_RESIDUAL_SUM_UPPER):
        raise RuntimeError(
            f"residual signed sum {residual_sum.tolist()} exceeds runtime upper "
            f"{RUNTIME_RESIDUAL_SUM_UPPER.tolist()}"
        )

    tail_phase_l1 = []
    tail_phase_nonzero = []
    tail_phase_outside_runtime_window = []
    for phase in range(16):
        kernel = aligned_phase_kernel(tail, 16, phase, tail_delay)
        tail_phase_l1.append(sum(abs(v) for v in kernel.values()))
        tail_phase_nonzero.append(len(kernel))
        tail_phase_outside_runtime_window.append(
            max((abs(v) for k, v in kernel.items() if k < TAIL_OFFSET_MIN or k > TAIL_OFFSET_MAX), default=0.0)
        )
    max_tail_l1 = max(tail_phase_l1)
    if max_tail_l1 > RUNTIME_TAIL_L1_UPPER:
        raise RuntimeError(f"tail L1 {max_tail_l1:.17g} exceeds runtime upper {RUNTIME_TAIL_L1_UPPER}")
    if max(tail_phase_outside_runtime_window) != 0.0:
        raise RuntimeError("runtime tail window omits a nonzero coefficient")

    tail_identity_error = check_tail_identity(tail, tail_delay)

    report = {
        "full_reference": {
            "length_64x": len(full),
            "delay_64x": full_delay,
        },
        "reference_4x_prefix": {
            "length_4x": len(prefix4),
            "delay_4x": prefix4_delay,
            "residual_l1_by_coarse_phase": residual_l1.tolist(),
            "residual_signed_sum_abs_by_coarse_phase": residual_sum.tolist(),
            "worst_residual_subphase_by_coarse_phase": worst_residual_subphase.tolist(),
            "runtime_residual_l1_upper": RUNTIME_RESIDUAL_L1_UPPER.tolist(),
            "runtime_residual_sum_upper": RUNTIME_RESIDUAL_SUM_UPPER.tolist(),
            "symmetric_stage2_prefix_l1_delta": runtime_prefix4_l1_delta,
        },
        "reference_4x_to_64x_tail": {
            "length_64x": len(tail),
            "delay_64x": tail_delay,
            "l1_by_phase": tail_phase_l1,
            "max_l1": max_tail_l1,
            "runtime_l1_upper": RUNTIME_TAIL_L1_UPPER,
            "nonzero_positions_by_phase": tail_phase_nonzero,
            "runtime_offset_range": [TAIL_OFFSET_MIN, TAIL_OFFSET_MAX],
            "max_omitted_coefficient_abs": max(tail_phase_outside_runtime_window),
            "binary64_factorization_worst_abs_difference": tail_identity_error,
        },
        "qualification_scope": {
            "coefficient_factorization": True,
            "finite_operator_residual": True,
            "runtime_fft_rounding_proof": False,
            "performance_benchmark": False,
        },
    }
    rendered = json.dumps(report, indent=2, sort_keys=True) + "\n"
    if args.report:
        args.report.write_text(rendered, encoding="utf-8")
    print(rendered, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
