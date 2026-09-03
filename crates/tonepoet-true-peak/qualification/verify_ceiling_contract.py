#!/usr/bin/env python3
"""Offline, independent audit of the R10 hard-ceiling reconstruction contract.

This program is developer qualification tooling only. It is not imported by
Cargo, build.rs, tests, runtime code, or flake.nix. Ordinary Rust regressions
freeze the important results so normal correctness does not depend on Python,
NumPy, SciPy, external executables, or the network.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import math
import pathlib
import re
from decimal import Decimal, getcontext

import numpy as np
from scipy.signal import upfirdn

OVERSAMPLE = 64
HALF_DELAY_TAPS = 384
LATER_TAPS = (49, 25, 17, 13, 9)
NUMERIC_ALLOWANCE_PER_INPUT_PEAK = 1.0e-11
FROZEN_RECONSTRUCTION_LINF_UPPER = 4.09
FROZEN_FIXTURE_SHA256 = "b6ba8b041ebd87543f04f92267487937128acc9905fc743323567682ef77fd20"


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


def build_filters(half_delay: np.ndarray) -> list[np.ndarray]:
    # The first 2x stage has an exact identity phase at even output indices and
    # the checked-in 384-tap half-sample FIR at odd indices.
    stage1 = np.zeros(2 * HALF_DELAY_TAPS + 1, dtype=np.float64)
    stage1[HALF_DELAY_TAPS] = 1.0
    stage1[1 : 2 * HALF_DELAY_TAPS : 2] = half_delay
    return [stage1] + [generic_two_x(taps) for taps in LATER_TAPS]


def cascade_delay_and_padding(filters: list[np.ndarray]) -> tuple[int, int]:
    delay = 0
    for h in filters:
        delay = delay * 2 + (len(h) - 1) // 2
    return delay, (delay + OVERSAMPLE - 1) // OVERSAMPLE


def complete_impulse(filters: list[np.ndarray]) -> np.ndarray:
    response = np.asarray([1.0], dtype=np.float64)
    for h in filters:
        response = upfirdn(h, response, up=2)
    return response


def reconstruction_linf(filters: list[np.ndarray]) -> tuple[float, int]:
    impulse = complete_impulse(filters)
    values = []
    for phase in range(OVERSAMPLE):
        values.append(float(np.abs(impulse[phase::OVERSAMPLE]).sum()))
    phase = int(np.argmax(values))
    return values[phase], phase


def reconstruct_peak(
    samples: np.ndarray,
    filters: list[np.ndarray],
    delay: int,
    padding: int,
    edge: str,
) -> float:
    samples = np.asarray(samples, dtype=np.float64)
    if samples.ndim != 1 or samples.size == 0:
        raise ValueError("reconstruct_peak requires one nonempty channel")
    if edge == "repeat":
        padded = np.concatenate((
            np.full(padding, samples[0]),
            samples,
            np.full(padding, samples[-1]),
        ))
    elif edge == "zero":
        padded = np.concatenate((np.zeros(padding), samples, np.zeros(padding)))
    else:
        raise ValueError(edge)
    out = padded
    for h in filters:
        out = upfirdn(h, out, up=2)
    start = padding * OVERSAMPLE + delay
    stop = start + (samples.size - 1) * OVERSAMPLE + 1
    return float(np.max(np.abs(out[start:stop])))


def with_numeric_upper(raw_peak: float, sample_peak: float) -> float:
    value = raw_peak + sample_peak * NUMERIC_ALLOWANCE_PER_INPUT_PEAK
    return float(np.nextafter(value, math.inf)) if value != 0.0 else 0.0


def aligned_multitone(frames: int, aligned_time: float, frequencies: list[float], divisor: float) -> np.ndarray:
    n = np.arange(frames, dtype=np.float64)
    relative = n - aligned_time
    return sum(np.cos(2.0 * np.pi * f * relative) for f in frequencies) / divisor


def db(linear: float) -> float:
    return -math.inf if linear == 0.0 else 20.0 * math.log10(linear)


def decimal_db_to_linear(text: str) -> str:
    getcontext().prec = 80
    value = Decimal(text)
    return str((Decimal(10).ln() * value / Decimal(20)).exp())


def check_close(name: str, value: float, frozen: float, tolerance: float = 5.0e-13) -> None:
    if abs(value - frozen) > tolerance:
        raise RuntimeError(f"{name}: {value:.17g} differs from frozen {frozen:.17g}")


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
    coefficients = parse_half_coefficients(source / "src" / "headroom64_coefficients.rs")
    filters = build_filters(coefficients)
    delay, padding = cascade_delay_and_padding(filters)
    if delay != 12_816 or padding != 201:
        raise RuntimeError(f"unexpected cascade delay/padding: {delay}/{padding}")

    linf, linf_phase = reconstruction_linf(filters)
    if linf > FROZEN_RECONSTRUCTION_LINF_UPPER:
        raise RuntimeError(
            f"reconstruction L-inf norm {linf:.17g} exceeds published upper "
            f"{FROZEN_RECONSTRUCTION_LINF_UPPER:.17g}"
        )

    three = aligned_multitone(4096, 2000.5, [0.30, 0.35, 0.40], 6.0)
    five = aligned_multitone(1024, 511.5, [0.4850, 0.4875, 0.4900, 0.4925, 0.4950], 5.0)
    short = np.asarray([0.25, -0.4, 0.1], dtype=np.float64)

    cases: dict[str, dict[str, float]] = {}
    frozen = {
        ("three_tone", "repeat"): 0.5016393168029930,
        ("three_tone", "zero"): 0.5027431266912155,
        ("five_tone", "repeat"): 1.0053238867134490,
        ("five_tone", "zero"): 1.0052945328528555,
        ("short", "repeat"): 0.40259347268375706,
        ("short", "zero"): 0.40520884263435925,
    }
    signals = {"three_tone": three, "five_tone": five, "short": short}
    truth = {"three_tone": 0.5, "five_tone": 1.0}
    for name, signal in signals.items():
        cases[name] = {}
        sample_peak = float(np.max(np.abs(signal)))
        for edge in ("repeat", "zero"):
            raw = reconstruct_peak(signal, filters, delay, padding, edge)
            check_close(f"{name}/{edge}", raw, frozen[(name, edge)])
            upper = with_numeric_upper(raw, sample_peak)
            if upper < raw:
                raise RuntimeError(f"{name}/{edge}: upper failed to contain reconstruction")
            cases[name][f"{edge}_raw_linear"] = raw
            cases[name][f"{edge}_upper_linear"] = upper
            cases[name][f"{edge}_numeric_slack_linear"] = upper - raw
            if name in truth:
                cases[name][f"{edge}_external_ideal_delta_db"] = db(raw) - db(truth[name])

    fixture = source / "tests" / "fixtures" / "real_reference_48k_stereo.f64le"
    fixture_bytes = fixture.read_bytes()
    fixture_sha = hashlib.sha256(fixture_bytes).hexdigest()
    if fixture_sha != FROZEN_FIXTURE_SHA256:
        raise RuntimeError(f"real fixture hash changed: {fixture_sha}")
    real = np.frombuffer(fixture_bytes, dtype="<f8").reshape(-1, 2)
    real_raw = [reconstruct_peak(real[:, ch], filters, delay, padding, "repeat") for ch in range(2)]
    for ch, value in enumerate(real_raw):
        check_close(f"real/ch{ch}", value, 0.9879574206349788, tolerance=8.0e-13)
    real_sample_peak = float(np.max(np.abs(real)))
    real_upper = with_numeric_upper(max(real_raw), real_sample_peak)

    directed_references = {
        text: decimal_db_to_linear(text)
        for text in ("-12.000000000", "-0.150000000", "3.141592653", "24.000000000")
    }

    report = {
        "contract": {
            "oversample": OVERSAMPLE,
            "edge_policies": ["RepeatEndpoints", "ZeroExtend"],
            "between_knots": "straight-line",
            "calibration_in_ceiling_reconstruction": False,
            "spectral_support_claim": None,
            "cascade_delay_subframes": delay,
            "pre_post_input_frames": padding,
            "numeric_allowance_per_input_peak_linear": NUMERIC_ALLOWANCE_PER_INPUT_PEAK,
        },
        "linf": {
            "independent_max_abs_polyphase_sum": linf,
            "max_phase": linf_phase,
            "published_upper": FROZEN_RECONSTRUCTION_LINF_UPPER,
            "margin": FROZEN_RECONSTRUCTION_LINF_UPPER - linf,
        },
        "analytical": cases,
        "real_fixture": {
            "sha256": fixture_sha,
            "frames": int(real.shape[0]),
            "channels": int(real.shape[1]),
            "raw_channel_peaks": real_raw,
            "raw_overall_peak": max(real_raw),
            "upper_overall_peak": real_upper,
            "numeric_slack_linear": real_upper - max(real_raw),
        },
        "directed_db_to_linear_high_precision": directed_references,
        "interpretation": {
            "three_tone_external_ideal_truth_linear": 0.5,
            "five_tone_external_ideal_truth_linear": 1.0,
            "note": (
                "The external ideal-tone deltas are model-comparison diagnostics, not hidden "
                "ceiling reserve. Under the declared finite reconstruction, the verifier upper "
                "contains the actual reconstruction peak only by the explicit binary64 allowance."
            ),
        },
    }

    rendered = json.dumps(report, indent=2, sort_keys=True) + "\n"
    if args.report:
        args.report.parent.mkdir(parents=True, exist_ok=True)
        args.report.write_text(rendered, encoding="utf-8")
    else:
        print(rendered, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
