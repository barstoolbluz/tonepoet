#!/usr/bin/env python3
"""Offline developer tool for regenerating and auditing Headroom64x.

This script is intentionally outside the build/test/runtime path. It requires
NumPy/SciPy only when a developer explicitly regenerates or studies the frozen
filter design; ordinary Rust tests own shipped regression protection.
"""
from __future__ import annotations

import argparse
import json
import math
import pathlib
import random
import re
from typing import Any

import numpy as np
import scipy
from scipy.signal import remez, upfirdn, freqz

PI = math.pi
HALF_TAPS = 384
DESIGN_EDGE_NORMALIZED_NYQUIST = 0.99
QUALIFIED_MAX_FRACTION_FS = 0.495
OVERSAMPLE = 64
CALIBRATION_DB = -0.004
CALIBRATION = 10.0 ** (CALIBRATION_DB / 20.0)
DECLARED_MAX_UNDERREAD_DB = 0.030
DECLARED_MAX_ABS_ERROR_DB = 0.050
GRID_BOUND_DB = -20.0 * math.log10(math.cos(math.pi / (2.0 * OVERSAMPLE)))
RANDOM_CASES = 4000
NUMERICAL_ALLOWANCE_DB = 0.000_010


def generic_two_x(taps: int) -> np.ndarray:
    j = np.arange(taps, dtype=float)
    center = (taps - 1) / 2.0
    x = (j - center) / 2.0
    h = np.sinc(x)
    phase = j / (taps - 1)
    h *= 0.42 - 0.5 * np.cos(2 * np.pi * phase) + 0.08 * np.cos(4 * np.pi * phase)
    for p in range(2):
        h[p::2] /= h[p::2].sum()
    return h


def design_half_delay() -> np.ndarray:
    coefficients = remez(
        HALF_TAPS,
        [0.0, DESIGN_EDGE_NORMALIZED_NYQUIST],
        [1.0],
        fs=2.0,
        maxiter=500,
        grid_density=128,
    )
    coefficients /= coefficients.sum()
    return coefficients


def parse_checked_in_half(path: pathlib.Path) -> np.ndarray:
    text = path.read_text(encoding="utf-8")
    body = text.split("[", 2)[-1].rsplit("]", 1)[0]
    values = [float(token) for token in re.findall(r"[-+]?\d+\.\d+e[-+]\d+", body, re.I)]
    if len(values) != HALF_TAPS // 2:
        raise RuntimeError(f"expected 192 checked-in coefficients, found {len(values)}")
    return np.asarray(values, dtype=float)


def build_filters(c: np.ndarray) -> list[np.ndarray]:
    stage1 = np.zeros(2 * HALF_TAPS + 1)
    stage1[HALF_TAPS] = 1.0
    stage1[1 : 2 * HALF_TAPS : 2] = c
    return [stage1] + [generic_two_x(taps) for taps in (49, 25, 17, 13, 9)]


def delay_and_pre(filters: list[np.ndarray]) -> tuple[int, int]:
    delay = 0
    for h in filters:
        delay = delay * 2 + (len(h) - 1) // 2
    return delay, (delay + OVERSAMPLE - 1) // OVERSAMPLE


def complete_cascade_response(filters: list[np.ndarray], delay: int) -> dict[str, Any]:
    """Measure every 64x polyphase branch over the qualified input band.

    The returned response includes the same scalar point calibration used by
    the Rust meter.  Removing each branch's ideal fractional delay leaves the
    complex interpolation error itself, so the one-sided magnitude deficit can
    be combined conservatively with the analytic 64x grid miss.
    """
    impulse = np.asarray([1.0], dtype=float)
    for h in filters:
        impulse = upfirdn(h, impulse, up=2)

    frequencies = np.linspace(0.0, QUALIFIED_MAX_FRACTION_FS, 10_001)
    omega = 2.0 * np.pi * frequencies
    worst_under = (0.0, None)
    worst_over = (0.0, None)
    worst_complex = (0.0, None)
    worst_phase = (0.0, None)

    for phase in range(OVERSAMPLE):
        branch = impulse[phase::OVERSAMPLE]
        _, response = freqz(branch, worN=omega)
        ratio = (
            response
            * np.exp(1j * omega * (delay - phase) / OVERSAMPLE)
            * CALIBRATION
        )
        magnitude_db = 20.0 * np.log10(np.abs(ratio))
        under_index = int(np.argmin(magnitude_db))
        over_index = int(np.argmax(magnitude_db))
        complex_error = np.abs(ratio - 1.0)
        complex_index = int(np.argmax(complex_error))
        phase_error = np.abs(np.angle(ratio))
        phase_index = int(np.argmax(phase_error))

        under = max(0.0, -float(magnitude_db[under_index]))
        over = max(0.0, float(magnitude_db[over_index]))
        complex_value = float(complex_error[complex_index])
        phase_value = float(phase_error[phase_index])
        if under > worst_under[0]:
            worst_under = (under, {
                "phase": phase,
                "frequency_fraction_of_sample_rate": float(frequencies[under_index]),
            })
        if over > worst_over[0]:
            worst_over = (over, {
                "phase": phase,
                "frequency_fraction_of_sample_rate": float(frequencies[over_index]),
            })
        if complex_value > worst_complex[0]:
            worst_complex = (complex_value, {
                "phase": phase,
                "frequency_fraction_of_sample_rate": float(frequencies[complex_index]),
            })
        if phase_value > worst_phase[0]:
            worst_phase = (phase_value, {
                "phase": phase,
                "frequency_fraction_of_sample_rate": float(frequencies[phase_index]),
            })

    return {
        "frequency_points": len(frequencies),
        "phase_count": OVERSAMPLE,
        "worst_interpolation_underresponse_db": worst_under[0],
        "worst_interpolation_underresponse_location": worst_under[1],
        "worst_interpolation_overresponse_db": worst_over[0],
        "worst_interpolation_overresponse_location": worst_over[1],
        "worst_complex_error_linear": worst_complex[0],
        "worst_complex_error_location": worst_complex[1],
        "worst_phase_error_radians": worst_phase[0],
        "worst_phase_error_location": worst_phase[1],
    }


def meter(samples: np.ndarray, filters: list[np.ndarray], delay: int, pre: int) -> float:
    samples = np.asarray(samples, dtype=float)
    padded = np.r_[np.full(pre, samples[0]), samples, np.full(pre, samples[-1])]
    y = padded
    for h in filters:
        y = upfirdn(h, y, up=2)
    start = pre * OVERSAMPLE + delay
    stop = start + (len(samples) - 1) * OVERSAMPLE + 1
    interpolated = float(np.max(np.abs(y[start:stop]))) * CALIBRATION
    return max(float(np.max(np.abs(samples))), interpolated)


def db(linear: float) -> float:
    return -math.inf if linear == 0.0 else 20.0 * math.log10(linear)


def aligned_multitone(
    frequencies: list[float], aligned_time: float, frames: int, scale: float | None = None
) -> np.ndarray:
    n = np.arange(frames, dtype=float)
    relative = n - aligned_time
    divisor = len(frequencies) if scale is None else scale
    return sum(np.cos(2.0 * np.pi * f * relative) for f in frequencies) / divisor


def enveloped_aligned(
    frequencies: list[float], aligned_time: float, frames: int
) -> tuple[np.ndarray, float]:
    n = np.arange(frames, dtype=float)
    relative = n - aligned_time
    half_period = max(aligned_time, frames - 1 - aligned_time)
    envelope_frequency = 1.0 / (2.0 * half_period)
    envelope = np.cos(np.pi * relative / (2.0 * half_period)) ** 2
    samples = envelope * sum(
        np.cos(2.0 * np.pi * f * relative) for f in frequencies
    ) / len(frequencies)
    return samples, max(frequencies) + envelope_frequency


def deterministic_cases() -> list[tuple[str, np.ndarray, float, dict[str, Any]]]:
    cases: list[tuple[str, np.ndarray, float, dict[str, Any]]] = []
    rng = random.Random(0x544F4E45504F4554)
    for index in range(RANDOM_CASES):
        frames = rng.choice([512, 640, 768, 896, 1024, 1280, 1536, 2048, 3072, 4096])
        fraction = rng.random()
        aligned_time = (frames - 1) / 2.0 + (fraction - 0.5) * 0.8
        half_period = max(aligned_time, frames - 1 - aligned_time)
        envelope_frequency = 1.0 / (2.0 * half_period)
        if rng.random() < 0.65:
            high = QUALIFIED_MAX_FRACTION_FS - envelope_frequency - rng.uniform(0.0, 0.006)
            low = rng.uniform(max(0.30, high - rng.uniform(0.002, 0.08)), high - 0.0002)
        else:
            high = rng.uniform(0.2, QUALIFIED_MAX_FRACTION_FS - envelope_frequency)
            low = rng.uniform(0.005, max(0.006, high - 0.005))
        count = rng.choice([1, 2, 3, 5, 7, 9, 11, 15])
        frequencies = (
            sorted(rng.uniform(low, high) for _ in range(count)) if count > 1 else [high]
        )
        samples, support = enveloped_aligned(frequencies, aligned_time, frames)
        assert support <= QUALIFIED_MAX_FRACTION_FS + 1e-12
        cases.append((
            f"case-{index:04d}",
            samples,
            support,
            {
                "frames": frames,
                "aligned_time": aligned_time,
                "frequencies_fraction_fs": frequencies,
                "envelope_frequency_fraction_fs": envelope_frequency,
                "support_fraction_fs": support,
            },
        ))
    return cases


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--source", type=pathlib.Path, required=True)
    parser.add_argument("--report", type=pathlib.Path, required=True)
    args = parser.parse_args()

    c = design_half_delay()
    checked_half = parse_checked_in_half(args.source / "src" / "headroom64_coefficients.rs")
    coefficient_max_abs_delta = float(np.max(np.abs(checked_half - c[: HALF_TAPS // 2])))
    if coefficient_max_abs_delta > 5e-16:
        raise RuntimeError(f"checked-in filter differs from frozen design: {coefficient_max_abs_delta}")

    filters = build_filters(c)
    delay, pre = delay_and_pre(filters)
    complete_response = complete_cascade_response(filters, delay)
    component_budget_db = (
        complete_response["worst_interpolation_underresponse_db"]
        + GRID_BOUND_DB
        + NUMERICAL_ALLOWANCE_DB
    )
    if component_budget_db > DECLARED_MAX_UNDERREAD_DB:
        raise RuntimeError(
            "complete interpolation + grid + numerical authority budget exceeds reserve: "
            f"{component_budget_db:.9f} > {DECLARED_MAX_UNDERREAD_DB:.9f} dB"
        )

    required: dict[str, dict[str, Any]] = {}
    three = aligned_multitone([0.30, 0.35, 0.40], 2000.5, 4096, scale=6.0)
    five = aligned_multitone([0.4850, 0.4875, 0.4900, 0.4925, 0.4950], 511.5, 1024)
    near, near_support = enveloped_aligned(
        [0.4980, 0.4982, 0.4984, 0.4986, 0.4988], 511.5, 1024
    )
    for name, samples, truth_db, support in (
        ("reviewer_three_tone", three, 20.0 * math.log10(0.5), 0.40),
        ("reviewer_five_tone", five, 0.0, 0.495),
    ):
        measured = db(meter(samples, filters, delay, pre))
        error = measured - truth_db
        required[name] = {
            "support_fraction_fs": support,
            "truth_dbtp": truth_db,
            "measured_dbtp": measured,
            "error_db": error,
            "one_sided_underread_db": max(0.0, -error),
            "absolute_error_db": abs(error),
        }
        if max(0.0, -error) > DECLARED_MAX_UNDERREAD_DB or abs(error) > DECLARED_MAX_ABS_ERROR_DB:
            raise RuntimeError(f"{name} failed authority/accuracy contract: {required[name]}")

    near_measured = db(meter(near, filters, delay, pre))
    required["r3_near_nyquist_envelope"] = {
        "generator_aligned_peak_dbtp": 0.0,
        "support_fraction_fs": near_support,
        "measured_point_dbtp": near_measured,
        "authority_qualified": near_support <= QUALIFIED_MAX_FRACTION_FS,
        "note": "retained diagnostic; support exceeds qualified band, so no Headroom64x authority is issued",
    }

    worst_under = (0.0, None)
    worst_abs = (0.0, None)
    for name, samples, support, metadata in deterministic_cases():
        measured = db(meter(samples, filters, delay, pre))
        under = max(0.0, -measured)  # analytical aligned truth is 0 dBTP
        absolute = abs(measured)
        if under > worst_under[0]:
            worst_under = (under, {"case": name, "measured_dbtp": measured, **metadata})
        if absolute > worst_abs[0]:
            worst_abs = (absolute, {"case": name, "measured_dbtp": measured, **metadata})

    frequencies = np.linspace(0.0, QUALIFIED_MAX_FRACTION_FS, 20001)
    omega = 2.0 * np.pi * frequencies
    _, response = freqz(c, worN=omega)
    desired = np.exp(-1j * omega * (HALF_TAPS - 1) / 2.0)
    response_db = 20.0 * np.log10(np.abs(response) * CALIBRATION)
    response_complex_error = np.abs(response / desired * CALIBRATION - 1.0)

    r3_products = 2001 + 49 * 2 + 25 * 4 + 17 * 8
    r4_products = 192 + 49 * 2 + 25 * 4 + 17 * 8 + 13 * 16 + 9 * 32
    report = {
        "schema": "tonepoet-headroom64-design-qualification-v1",
        "architecture": {
            "oversample_factor": OVERSAMPLE,
            "first_stage": "384-tap Type-II equiripple half-sample fractional delay; exact integer phase; symmetric 192-product execution",
            "later_two_x_taps": [49, 25, 17, 13, 9],
            "calibration_db": CALIBRATION_DB,
            "pre_post_input_frames": pre,
            "final_grid_delay_subframes": delay,
        },
        "authority_contract": {
            "qualified_max_frequency_fraction_of_sample_rate": QUALIFIED_MAX_FRACTION_FS,
            "analytic_grid_max_underread_db": GRID_BOUND_DB,
            "measured_complete_cascade_interpolation_underresponse_db": complete_response["worst_interpolation_underresponse_db"],
            "numerical_allowance_db": NUMERICAL_ALLOWANCE_DB,
            "derived_component_budget_db": component_budget_db,
            "declared_total_one_sided_underread_db": DECLARED_MAX_UNDERREAD_DB,
            "remaining_margin_db": DECLARED_MAX_UNDERREAD_DB - component_budget_db,
            "declared_point_abs_error_target_db": DECLARED_MAX_ABS_ERROR_DB,
            "global_full_band_theorem_claimed": False,
        },
        "complete_cascade_response": complete_response,
        "coefficient_reproduction": {
            "scipy_version": scipy.__version__,
            "numpy_version": np.__version__,
            "remez_edge_normalized_nyquist": DESIGN_EDGE_NORMALIZED_NYQUIST,
            "max_abs_delta_checked_in_vs_regenerated": coefficient_max_abs_delta,
        },
        "first_stage_complex_response": {
            "min_magnitude_db_including_calibration": float(response_db.min()),
            "max_magnitude_db_including_calibration": float(response_db.max()),
            "max_complex_error_linear_including_calibration": float(response_complex_error.max()),
        },
        "required_regressions": required,
        "deterministic_analytic_search": {
            "case_count": RANDOM_CASES,
            "generator_rng_seed": "0x544F4E45504F4554",
            "worst_one_sided_underread_db": worst_under[0],
            "worst_one_sided_underread_case": worst_under[1],
            "worst_absolute_error_db": worst_abs[0],
            "worst_absolute_error_case": worst_abs[1],
            "margin_to_declared_reserve_db": DECLARED_MAX_UNDERREAD_DB - worst_under[0],
        },
        "algorithmic_cost": {
            "r3_headroom16_coefficient_products_per_input_channel": r3_products,
            "r4_headroom64_coefficient_products_per_input_channel": r4_products,
            "r4_fraction_of_r3": r4_products / r3_products,
            "reduction_percent": 100.0 * (1.0 - r4_products / r3_products),
            "note": "operation-count model excludes ring/index/add overhead; it compares FIR coefficient products only",
        },
        "mathematical_limit": {
            "statement": "No finite-sample meter can infer an arbitrary exact-Nyquist quadrature component from real samples; Headroom64x therefore refuses safety authority above its frozen 0.495*Fs qualified band rather than claiming a false global theorem.",
        },
    }

    if worst_under[0] > DECLARED_MAX_UNDERREAD_DB:
        raise RuntimeError(f"deterministic search exceeded reserve: {worst_under}")
    if worst_abs[0] > DECLARED_MAX_ABS_ERROR_DB:
        raise RuntimeError(f"deterministic search exceeded point-error target: {worst_abs}")

    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
