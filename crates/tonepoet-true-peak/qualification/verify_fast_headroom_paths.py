#!/usr/bin/env python3
"""Independent qualification for Tonepoet's opt-in fast headroom scans.

This is an offline developer gate, not a runtime dependency.  It reconstructs
exactly the checked-in first stage and the generated/symmetrized later stages,
then verifies the public one-sided point reserves, the operation-count speed
model, and the fast-to-full-64x hard-ceiling bridge constants.
"""
from __future__ import annotations

import argparse
import json
import math
import pathlib
import random
import re
from dataclasses import dataclass
from typing import Any

import numpy as np
import scipy
from scipy.signal import freqz, upfirdn

PI = math.pi
COEFFICIENT_EPSILON = 1.0e-15
HALF_TAPS = 384
QUALIFIED_MAX_FRACTION_FS = 0.495
NUMERICAL_ALLOWANCE_DB = 0.000_010
FIRST_STAGE_PRODUCTS = 192
REFERENCE_PRODUCTS = 576
FAST16_PRODUCTS = 272
FAST8_PRODUCTS = 240
# The fast ceiling modes compute two interior Bernstein controls per coarse
# interval. There are 16 intervals/original frame at 16x and 8 at 8x; endpoint
# controls are already represented by the coarse prefix peak.
FAST16_CEILING_BRIDGE_MULTIPLIES = 32
FAST8_CEILING_BRIDGE_MULTIPLIES = 16
BRIDGE16_DECLARED = 0.0030
BRIDGE8_DECLARED = 0.0030
RANDOM_SEED = 0x46535454504B3236
DEFAULT_RANDOM_CASES = 1000


@dataclass(frozen=True)
class Mode:
    name: str
    factor: int
    taps: tuple[int, ...]
    calibration_db: float
    declared_underread_db: float
    products: int


MODES = (
    Mode("fast", 16, (49, 25, 17), 0.007, 0.044, FAST16_PRODUCTS),
    Mode("fastest", 8, (49, 25), 0.088, 0.084, FAST8_PRODUCTS),
)


def parse_checked_in_half(path: pathlib.Path) -> np.ndarray:
    text = path.read_text(encoding="utf-8")
    body = text.split("[", 2)[-1].rsplit("]", 1)[0]
    values = [float(token) for token in re.findall(r"[-+]?\d+\.\d+e[-+]\d+", body, re.I)]
    if len(values) != HALF_TAPS // 2:
        raise RuntimeError(f"expected 192 checked-in coefficients, found {len(values)}")
    return np.asarray(values, dtype=float)


def first_stage(checked_half: np.ndarray) -> np.ndarray:
    full_half_phase = np.r_[checked_half, checked_half[::-1]]
    h = np.zeros(2 * HALF_TAPS + 1, dtype=float)
    h[HALF_TAPS] = 1.0
    h[1 : 2 * HALF_TAPS : 2] = full_half_phase
    return h


def runtime_two_x(taps: int, *, symmetrize_half: bool) -> np.ndarray:
    """Reproduce build_polyphase_filters(taps, 2, ..., Blackman, true)."""
    phases: list[list[tuple[int, float]]] = [[], []]
    for tap in range(taps):
        centered = tap - (taps - 1) / 2.0
        if abs(centered) < np.finfo(float).eps:
            sinc = 1.0
        else:
            argument = centered * PI / 2.0
            sinc = math.sin(argument) / argument
        phase_fraction = tap / (taps - 1)
        window = (
            0.42
            - 0.5 * math.cos(2.0 * PI * phase_fraction)
            + 0.08 * math.cos(4.0 * PI * phase_fraction)
        )
        coefficient = sinc * window
        if abs(coefficient) <= COEFFICIENT_EPSILON:
            continue
        phases[tap % 2].append((tap, coefficient))

    for phase in range(2):
        total = sum(value for _, value in phases[phase])
        phases[phase] = [(tap, value / total) for tap, value in phases[phase]]

    if symmetrize_half:
        half = phases[1]
        if len(half) % 2:
            raise RuntimeError(f"stage {taps} half phase is not even-length")
        for index in range(len(half) // 2):
            mirror = len(half) - 1 - index
            average = 0.5 * (half[index][1] + half[mirror][1])
            half[index] = (half[index][0], average)
            half[mirror] = (half[mirror][0], average)

    h = np.zeros(taps, dtype=float)
    for phase in phases:
        for tap, value in phase:
            h[tap] = value
    return h


def cascade(checked_half: np.ndarray, later_taps: tuple[int, ...], *, fast: bool) -> tuple[np.ndarray, int]:
    filters = [first_stage(checked_half)] + [
        runtime_two_x(taps, symmetrize_half=fast) for taps in later_taps
    ]
    impulse = np.asarray([1.0], dtype=float)
    delay = 0
    for h in filters:
        impulse = upfirdn(h, impulse, up=2)
        delay = delay * 2 + (len(h) - 1) // 2
    return impulse, delay


def phase_response(impulse: np.ndarray, factor: int, calibration_db: float) -> dict[str, Any]:
    frequencies = np.linspace(0.0, QUALIFIED_MAX_FRACTION_FS, 10_001)
    omega = 2.0 * PI * frequencies
    calibration = 10.0 ** (calibration_db / 20.0)
    minimum = (math.inf, None)
    maximum = (-math.inf, None)
    for phase in range(factor):
        branch = impulse[phase::factor]
        _, response = freqz(branch, worN=omega)
        magnitude_db = 20.0 * np.log10(np.abs(response) * calibration)
        lo_index = int(np.argmin(magnitude_db))
        hi_index = int(np.argmax(magnitude_db))
        lo = float(magnitude_db[lo_index])
        hi = float(magnitude_db[hi_index])
        if lo < minimum[0]:
            minimum = (lo, {"phase": phase, "frequency_fraction_fs": float(frequencies[lo_index])})
        if hi > maximum[0]:
            maximum = (hi, {"phase": phase, "frequency_fraction_fs": float(frequencies[hi_index])})
    return {
        "minimum_magnitude_db": minimum[0],
        "minimum_location": minimum[1],
        "maximum_magnitude_db": maximum[0],
        "maximum_location": maximum[1],
    }


def qualified_grid_miss_db(factor: int) -> float:
    return -20.0 * math.log10(math.cos(PI * QUALIFIED_MAX_FRACTION_FS / factor))


def meter(samples: np.ndarray, impulse: np.ndarray, delay: int, factor: int, calibration_db: float) -> float:
    samples = np.asarray(samples, dtype=float)
    pre = (delay + factor - 1) // factor
    padded = np.r_[np.full(pre, samples[0]), samples, np.full(pre, samples[-1])]
    # The complete cascade impulse is already expressed on the final grid.
    y = upfirdn(impulse, padded, up=factor)
    start = pre * factor + delay
    stop = start + (len(samples) - 1) * factor + 1
    interpolated = float(np.max(np.abs(y[start:stop]))) * 10.0 ** (calibration_db / 20.0)
    return max(float(np.max(np.abs(samples))), interpolated)


def aligned_multitone(frequencies: list[float], aligned_time: float, frames: int) -> np.ndarray:
    n = np.arange(frames, dtype=float)
    relative = n - aligned_time
    return sum(np.cos(2.0 * PI * f * relative) for f in frequencies) / len(frequencies)


def enveloped_aligned(frequencies: list[float], aligned_time: float, frames: int) -> tuple[np.ndarray, float]:
    n = np.arange(frames, dtype=float)
    relative = n - aligned_time
    half_period = max(aligned_time, frames - 1 - aligned_time)
    envelope_frequency = 1.0 / (2.0 * half_period)
    envelope = np.cos(PI * relative / (2.0 * half_period)) ** 2
    samples = envelope * sum(np.cos(2.0 * PI * f * relative) for f in frequencies) / len(frequencies)
    return samples, max(frequencies) + envelope_frequency


def deterministic_cases(count: int) -> list[tuple[np.ndarray, dict[str, Any]]]:
    rng = random.Random(RANDOM_SEED)
    cases = []
    for index in range(count):
        frames = rng.choice([512, 640, 768, 896, 1024, 1280, 1536, 2048])
        aligned_time = (frames - 1) / 2.0 + (rng.random() - 0.5) * 0.8
        half_period = max(aligned_time, frames - 1 - aligned_time)
        envelope_frequency = 1.0 / (2.0 * half_period)
        upper = QUALIFIED_MAX_FRACTION_FS - envelope_frequency - rng.uniform(0.0, 0.006)
        if rng.random() < 0.70:
            lower = max(0.25, upper - rng.uniform(0.004, 0.12))
        else:
            lower = rng.uniform(0.005, max(0.006, upper - 0.005))
        components = rng.choice([1, 2, 3, 5, 7, 9, 11])
        frequencies = sorted(rng.uniform(lower, upper) for _ in range(components))
        samples, support = enveloped_aligned(frequencies, aligned_time, frames)
        if support > QUALIFIED_MAX_FRACTION_FS + 1.0e-12:
            raise RuntimeError("generator exceeded qualified band")
        cases.append((samples, {
            "case": index,
            "frames": frames,
            "aligned_time": aligned_time,
            "frequencies_fraction_fs": frequencies,
            "support_fraction_fs": support,
        }))
    return cases


def sample_at(impulse: np.ndarray, delay: int, physical_index: int) -> float:
    index = physical_index + delay
    if index < 0 or index >= len(impulse):
        return 0.0
    return float(impulse[index])


def cubic_at(impulse: np.ndarray, delay: int, fine_index: int, ratio: int) -> float:
    q, r = divmod(fine_index, ratio)
    t = r / float(ratio)
    ym1 = sample_at(impulse, delay, q - 1)
    y0 = sample_at(impulse, delay, q)
    y1 = sample_at(impulse, delay, q + 1)
    y2 = sample_at(impulse, delay, q + 2)
    wm1 = -t * (t - 1.0) * (t - 2.0) / 6.0
    w0 = (t + 1.0) * (t - 1.0) * (t - 2.0) / 2.0
    w1 = -(t + 1.0) * t * (t - 2.0) / 2.0
    w2 = (t + 1.0) * t * (t - 1.0) / 6.0
    return wm1 * ym1 + w0 * y0 + w1 * y1 + w2 * y2


def bridge_linf(
    full: np.ndarray,
    full_delay: int,
    coarse: np.ndarray,
    coarse_delay: int,
    ratio: int,
) -> tuple[float, int]:
    # Wide finite bounds cover the union of full and interpolated coarse
    # supports, including the cubic's two neighboring coarse knots.
    min_physical = min(-full_delay, ratio * (-(coarse_delay) - 2)) - 128
    max_physical = max(
        len(full) - 1 - full_delay,
        ratio * (len(coarse) - 1 - coarse_delay + 2) + ratio - 1,
    ) + 128
    worst = 0.0
    worst_phase = 0
    for phase in range(64):
        total = 0.0
        first = min_physical + ((phase - min_physical) % 64)
        for physical in range(first, max_physical + 1, 64):
            reference = sample_at(full, full_delay, physical)
            approximation = cubic_at(coarse, coarse_delay, physical, ratio)
            total += abs(reference - approximation)
        if total > worst:
            worst = total
            worst_phase = phase
    return worst, worst_phase


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--source", type=pathlib.Path, required=True)
    parser.add_argument("--report", type=pathlib.Path, required=True)
    parser.add_argument("--random-cases", type=int, default=DEFAULT_RANDOM_CASES)
    args = parser.parse_args()

    checked_half = parse_checked_in_half(args.source / "src" / "headroom64_coefficients.rs")
    full64, full64_delay = cascade(checked_half, (49, 25, 17, 13, 9), fast=False)
    fast16, fast16_delay = cascade(checked_half, MODES[0].taps, fast=True)
    fast8, fast8_delay = cascade(checked_half, MODES[1].taps, fast=True)

    expected_delays = {64: 12_816, 16: 3_200, 8: 1_596}
    actual_delays = {64: full64_delay, 16: fast16_delay, 8: fast8_delay}
    if actual_delays != expected_delays:
        raise RuntimeError(f"cascade delay drift: {actual_delays} != {expected_delays}")

    def modeled_reference_products(later_taps: tuple[int, ...]) -> int:
        products = FIRST_STAGE_PRODUCTS
        input_rate = 2
        for taps in later_taps:
            products += input_rate * ((taps - 1) // 2)
            input_rate *= 2
        return products

    def modeled_fast_products(later_taps: tuple[int, ...]) -> int:
        products = FIRST_STAGE_PRODUCTS
        input_rate = 2
        for taps in later_taps:
            if (taps - 1) % 4 != 0:
                raise RuntimeError(f"stage {taps} cannot use paired symmetric half-phase products")
            products += input_rate * ((taps - 1) // 4)
            input_rate *= 2
        return products

    reference_products = modeled_reference_products((49, 25, 17, 13, 9))
    fast_products = modeled_fast_products(MODES[0].taps)
    fastest_products = modeled_fast_products(MODES[1].taps)
    expected_products = (REFERENCE_PRODUCTS, FAST16_PRODUCTS, FAST8_PRODUCTS)
    actual_products = (reference_products, fast_products, fastest_products)
    if actual_products != expected_products:
        raise RuntimeError(f"operation-count model drift: {actual_products} != {expected_products}")

    fast_ceiling_multiplies = fast_products + FAST16_CEILING_BRIDGE_MULTIPLIES
    fastest_ceiling_multiplies = fastest_products + FAST8_CEILING_BRIDGE_MULTIPLIES
    operation_model = {
        "reference_fir_products_per_input_channel": reference_products,
        "fast_fir_products_per_input_channel": fast_products,
        "fastest_fir_products_per_input_channel": fastest_products,
        "fast_ceiling_bridge_multiplies_per_input_channel": FAST16_CEILING_BRIDGE_MULTIPLIES,
        "fastest_ceiling_bridge_multiplies_per_input_channel": FAST8_CEILING_BRIDGE_MULTIPLIES,
        "fast_modeled_multiplies_per_input_channel": fast_ceiling_multiplies,
        "fastest_modeled_multiplies_per_input_channel": fastest_ceiling_multiplies,
        "fast_fraction_of_reference": fast_ceiling_multiplies / reference_products,
        "fastest_fraction_of_reference": fastest_ceiling_multiplies / reference_products,
        "fastest_fraction_of_fast": fastest_ceiling_multiplies / fast_ceiling_multiplies,
        "designed_cpu_minutes_from_12_7_reference": {
            "fast": 12.7 * fast_ceiling_multiplies / reference_products,
            "fastest": 12.7 * fastest_ceiling_multiplies / reference_products,
        },
        "note": (
            "static floating-multiply model for the production ceiling path; "
            "designed-for only, not measured throughput; additions, abs/max, memory, and I/O remain unmodeled"
        ),
    }
    if operation_model["designed_cpu_minutes_from_12_7_reference"]["fast"] >= 7.7:
        raise RuntimeError("middle rung operation model misses the brief's speed floor")
    if fastest_ceiling_multiplies / fast_ceiling_multiplies > 0.85:
        raise RuntimeError("fastest rung is not meaningfully separated in production multiply count")

    impulses = {"fast": (fast16, fast16_delay), "fastest": (fast8, fast8_delay)}
    point_reports: dict[str, Any] = {}
    required_signals = {
        "three_tone": (aligned_multitone([0.30, 0.35, 0.40], 2000.5, 4096), 0.0),
        "five_tone_upper_band": (
            aligned_multitone([0.4850, 0.4875, 0.4900, 0.4925, 0.4950], 511.5, 1024),
            0.0,
        ),
    }
    random_cases = deterministic_cases(args.random_cases)

    for mode in MODES:
        impulse, delay = impulses[mode.name]
        response = phase_response(impulse, mode.factor, mode.calibration_db)
        grid = qualified_grid_miss_db(mode.factor)
        # The qualified-band response audit minimum is credited against the
        # worst grid miss, matching Headroom64x's engineering-qualification
        # methodology. This is not presented as a theorem for arbitrary
        # critical-Nyquist content. Both fast rungs intentionally carry small
        # positive calibration biases.
        component_budget = grid - response["minimum_magnitude_db"] + NUMERICAL_ALLOWANCE_DB
        if component_budget > mode.declared_underread_db:
            raise RuntimeError(
                f"{mode.name} component budget {component_budget:.9f} exceeds "
                f"declared {mode.declared_underread_db:.9f} dB"
            )

        required: dict[str, Any] = {}
        for name, (samples, truth_db) in required_signals.items():
            measured_db = 20.0 * math.log10(meter(samples, impulse, delay, mode.factor, mode.calibration_db))
            under = max(0.0, truth_db - measured_db)
            if under > mode.declared_underread_db:
                raise RuntimeError(f"{mode.name}/{name} under-read {under:.9f} dB")
            required[name] = {"measured_dbtp": measured_db, "one_sided_underread_db": under}

        worst = (0.0, None)
        worst_over = (0.0, None)
        for samples, metadata in random_cases:
            measured_db = 20.0 * math.log10(meter(samples, impulse, delay, mode.factor, mode.calibration_db))
            # The aligned/enveloped generator has exact continuous peak 1.0.
            under = max(0.0, -measured_db)
            over = max(0.0, measured_db)
            if under > worst[0]:
                worst = (under, {**metadata, "measured_dbtp": measured_db})
            if over > worst_over[0]:
                worst_over = (over, {**metadata, "measured_dbtp": measured_db})
        if worst[0] > mode.declared_underread_db:
            raise RuntimeError(f"{mode.name} deterministic search exceeded reserve: {worst}")

        point_reports[mode.name] = {
            "factor": mode.factor,
            "calibration_db": mode.calibration_db,
            "response": response,
            "qualified_grid_max_underread_db": grid,
            "numerical_allowance_db": NUMERICAL_ALLOWANCE_DB,
            "derived_component_budget_db": component_budget,
            "declared_one_sided_underread_db": mode.declared_underread_db,
            "component_margin_db": mode.declared_underread_db - component_budget,
            "required_regressions": required,
            "deterministic_analytic_search": {
                "case_count": args.random_cases,
                "seed": hex(RANDOM_SEED),
                "worst_one_sided_underread_db": worst[0],
                "worst_case": worst[1],
                "worst_overread_db": worst_over[0],
                "worst_overread_case": worst_over[1],
            },
        }

    bridge16, bridge16_phase = bridge_linf(
        full64, full64_delay, fast16, fast16_delay, 4
    )
    bridge8, bridge8_phase = bridge_linf(
        full64, full64_delay, fast8, fast8_delay, 8
    )
    if bridge16 > BRIDGE16_DECLARED:
        raise RuntimeError(f"16x bridge norm {bridge16:.16g} exceeds {BRIDGE16_DECLARED}")
    if bridge8 > BRIDGE8_DECLARED:
        raise RuntimeError(f"8x bridge norm {bridge8:.16g} exceeds {BRIDGE8_DECLARED}")

    report = {
        "schema": "tonepoet-fast-headroom-qualification-v1",
        "numpy_version": np.__version__,
        "scipy_version": scipy.__version__,
        "qualified_max_frequency_fraction_fs": QUALIFIED_MAX_FRACTION_FS,
        "frozen_first_stage_coefficient_count": int(len(checked_half)),
        "operation_model": operation_model,
        "point_authority": point_reports,
        "hard_ceiling_bridge": {
            "governed_reference": "uncalibrated finite Headroom64x reconstruction",
            "fast16": {
                "interpolator": "four-point cubic; runtime envelope via two interior Bernstein controls",
                "measured_phase_l1_max": bridge16,
                "worst_phase": bridge16_phase,
                "declared_linf_error_upper": BRIDGE16_DECLARED,
                "margin": BRIDGE16_DECLARED - bridge16,
            },
            "fastest8": {
                "interpolator": "four-point cubic; runtime envelope via two interior Bernstein controls",
                "measured_phase_l1_max": bridge8,
                "worst_phase": bridge8_phase,
                "declared_linf_error_upper": BRIDGE8_DECLARED,
                "margin": BRIDGE8_DECLARED - bridge8,
            },
            "proof_note": (
                "For any bounded input sequence, each phase error is bounded by the absolute "
                "coefficient-sum norm times input sample peak. RepeatEndpoints and ZeroExtend "
                "remain bounded by that same sample peak, so finite-stream edges do not invalidate it."
            ),
        },
    }

    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(json.dumps({
        "fast_component_budget_db": point_reports["fast"]["derived_component_budget_db"],
        "fastest_component_budget_db": point_reports["fastest"]["derived_component_budget_db"],
        "fast_random_worst_underread_db": point_reports["fast"]["deterministic_analytic_search"]["worst_one_sided_underread_db"],
        "fastest_random_worst_underread_db": point_reports["fastest"]["deterministic_analytic_search"]["worst_one_sided_underread_db"],
        "bridge16_linf": bridge16,
        "bridge8_linf": bridge8,
    }, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
