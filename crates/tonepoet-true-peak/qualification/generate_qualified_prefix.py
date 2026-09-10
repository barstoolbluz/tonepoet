#!/usr/bin/env python3
"""Generate the fixed certified-prefix twiddles and numerical enclosure report.

The certified 2x prefix deliberately owns only the two power-of-two FFT sizes
used by the crate.  It is a plain radix-2 decimation-in-time transform with a
frozen 8192-root twiddle table.  This script derives an outward enclosure for
that exact construction; it does not infer authority from random FFT tests.

The bound covers, in order:

* packed complex input (two real channels);
* every radix-2 forward butterfly and frozen-twiddle approximation;
* runtime construction of the frozen-filter spectrum with the same FFT;
* complex pointwise multiplication;
* every inverse butterfly;
* exact power-of-two normalization;
* a conservative FTZ/DAZ absolute floor.

The final per-channel half-phase error is

    relative_error_upper * max_abs_component_across_packed_pair_and_block
      + absolute_error_upper

where the component peak includes the full overlap history and pending block.
"""
from __future__ import annotations

import argparse
import json
import math
import re
from pathlib import Path

import mpmath as mp

ROOT = Path(__file__).resolve().parents[1]
LEGACY_RUST = ROOT / "src" / "headroom64_coefficients.rs"
HQ_RUST = ROOT / "src" / "hq1024_coefficients.rs"

MAX_FFT_SIZE = 8192
LEGACY_FFT_SIZE = 2048
HQ_FFT_SIZE = 8192
UNIT_ROUNDOFF = mp.mpf(2) ** -53
MIN_NORMAL = mp.mpf(2) ** -1022

# Every frozen component is independently checked far inside one binary64 ulp.
# Publishing 2^-52 is therefore deliberately wider than the worst possible
# nearest-binary64 component rounding at magnitude <= 1.
TWIDDLE_COMPLEX_ERROR_UPPER = mp.mpf(2) ** -52

# For a complex multiplication implemented as four real products and two real
# additions, the normal-range arithmetic term is bounded by
#   2*u*(2+u) * |x| * |w|.
COMPLEX_MUL_RELATIVE = 2 * UNIT_ROUNDOFF * (2 + UNIT_ROUNDOFF)

# Absolute FTZ/DAZ floors, in MIN_NORMAL units, for one complex twiddle
# multiplication and one complex butterfly add/subtract respectively.  Each
# real multiply can lose one subnormal data operand plus one subnormal result;
# each real add/subtract can lose two subnormal operands plus its result.  The
# published complex floors round sqrt(2) amplification upward to integer units.
COMPLEX_TWIDDLE_MUL_MIN_NORMAL_UNITS = mp.mpf(12)
COMPLEX_ADD_MIN_NORMAL_UNITS = mp.mpf(6)


def parse_rust_f64_array(path: Path, name: str) -> list[float]:
    text = path.read_text()
    match = re.search(
        rf"const\s+{re.escape(name)}\s*:\s*\[f64;\s*\d+\]\s*=\s*\[(.*?)\];",
        text,
        re.S,
    )
    if not match:
        raise RuntimeError(f"could not find {name} in {path}")
    return [
        float(token)
        for token in match.group(1).replace("\n", " ").split(",")
        if token.strip()
    ]


def outward_float(value: mp.mpf) -> float:
    result = float(value)
    if not math.isfinite(result):
        return result
    # Always advance one binary64 value. This avoids relying on whether the
    # preceding mp->float conversion happened to land exactly on the value.
    return math.nextafter(result, math.inf)


def rust_float(value: float) -> str:
    if value == 0.0:
        return "0.0"
    if value == 1.0:
        return "1.0"
    if value == -1.0:
        return "-1.0"
    return repr(value)


def transform_bound(size: int) -> tuple[mp.mpf, mp.mpf]:
    """Return (relative_to_input_complex_peak, MIN_NORMAL_units)."""
    if size <= 0 or size & (size - 1):
        raise ValueError("qualified FFT size must be a power of two")
    relative = mp.mpf(0)
    absolute_units = mp.mpf(0)
    for stage in range(size.bit_length() - 1):
        exact_stage_peak = mp.mpf(2) ** stage
        product_relative = (
            (1 + TWIDDLE_COMPLEX_ERROR_UPPER) * relative
            + TWIDDLE_COMPLEX_ERROR_UPPER * exact_stage_peak
            + COMPLEX_MUL_RELATIVE
            * (1 + TWIDDLE_COMPLEX_ERROR_UPPER)
            * (exact_stage_peak + relative)
        )
        product_absolute = (
            (1 + TWIDDLE_COMPLEX_ERROR_UPPER) * absolute_units
            + COMPLEX_MUL_RELATIVE
            * (1 + TWIDDLE_COMPLEX_ERROR_UPPER)
            * absolute_units
            + COMPLEX_TWIDDLE_MUL_MIN_NORMAL_UNITS
        )

        relative = (
            relative
            + product_relative
            + UNIT_ROUNDOFF
            * (2 * exact_stage_peak + relative + product_relative)
        )
        absolute_units = (
            absolute_units
            + product_absolute
            + UNIT_ROUNDOFF * (absolute_units + product_absolute)
            + COMPLEX_ADD_MIN_NORMAL_UNITS
        )
    return relative, absolute_units


def prefix_bound(size: int, half_coefficients: list[float]) -> dict[str, mp.mpf]:
    full = [mp.mpf(value) for value in half_coefficients]
    full += list(reversed(full))
    filter_peak = max(abs(value) for value in full)
    filter_l1 = mp.fsum(abs(value) for value in full)
    transform_relative, transform_absolute_units = transform_bound(size)
    sqrt2 = mp.sqrt(2)

    # The filter spectrum is constructed once with the same qualified FFT.
    # Any subnormal spectrum component is canonicalized to zero at runtime;
    # sqrt(2)*MIN_NORMAL encloses that two-component perturbation.
    filter_spectrum_error = (
        transform_relative * filter_peak
        + (transform_absolute_units + sqrt2) * MIN_NORMAL
    )
    filter_spectrum_magnitude_upper = filter_l1 + filter_spectrum_error

    # Signal forward-transform error. P is the maximum absolute real component
    # across both packed channels and every contributing history/pending frame.
    signal_forward_relative = transform_relative * sqrt2
    # Input packing itself is exact. If an overflow-protection power-of-two
    # downscale drives a component into the subnormal range, however, FTZ/DAZ
    # may erase up to one MIN_NORMAL unit per real component before the first
    # butterfly. Propagating that perturbation through the exact DFT costs at
    # most N*sqrt(2) units per complex output.
    input_scaling_absolute_units = mp.mpf(size) * sqrt2
    signal_forward_absolute_units = (
        transform_absolute_units + input_scaling_absolute_units
    )
    exact_signal_spectrum_relative = mp.mpf(size) * sqrt2
    computed_signal_spectrum_relative = (
        exact_signal_spectrum_relative + signal_forward_relative
    )

    # Pointwise complex product.  Sanitized filter-spectrum components are
    # normal or zero, so DAZ on a subnormal signal component is amplified by at
    # most the filter-spectrum norm.  The integer-unit floor below is a simple
    # outward bound for both output components.
    product_floor_units = 4 * filter_spectrum_magnitude_upper + 12
    product_error_relative = (
        filter_l1 * signal_forward_relative
        + exact_signal_spectrum_relative * filter_spectrum_error
        + signal_forward_relative * filter_spectrum_error
        + COMPLEX_MUL_RELATIVE
        * computed_signal_spectrum_relative
        * filter_spectrum_magnitude_upper
    )
    product_error_absolute_units = (
        filter_l1 * signal_forward_absolute_units
        + signal_forward_absolute_units * filter_spectrum_error
        + COMPLEX_MUL_RELATIVE
        * signal_forward_absolute_units
        * filter_spectrum_magnitude_upper
        + product_floor_units
    )

    computed_product_relative = (
        mp.mpf(size) * sqrt2 * filter_l1 + product_error_relative
    )
    computed_product_absolute_units = product_error_absolute_units

    # The exact unnormalized inverse transforms the product error with norm N;
    # the qualified inverse contributes its own transform bound. Division by N
    # is multiplication by an exact binary power for both supported sizes.
    inverse_algorithm_relative = transform_relative * computed_product_relative
    inverse_algorithm_absolute_units = (
        transform_relative * computed_product_absolute_units
        + transform_absolute_units
    )
    output_relative = (
        product_error_relative + inverse_algorithm_relative / mp.mpf(size)
    )
    # The reciprocal 1/N is exactly representable. Normal results therefore
    # incur no relative rounding, but a subnormal normalized result may still
    # be flushed; charge one final MIN_NORMAL unit explicitly.
    normalization_absolute_units = mp.mpf(1)
    output_absolute_units = (
        product_error_absolute_units
        + inverse_algorithm_absolute_units / mp.mpf(size)
        + normalization_absolute_units
    )

    return {
        "filter_coefficient_peak": filter_peak,
        "filter_l1": filter_l1,
        "transform_relative": transform_relative,
        "transform_absolute_units": transform_absolute_units,
        "input_scaling_absolute_units": input_scaling_absolute_units,
        "normalization_absolute_units": normalization_absolute_units,
        "filter_spectrum_error": filter_spectrum_error,
        "output_relative": output_relative,
        "output_absolute_units": output_absolute_units,
        "output_absolute": output_absolute_units * MIN_NORMAL,
    }


def make_twiddles() -> tuple[list[tuple[float, float]], mp.mpf]:
    mp.mp.dps = 120
    twiddles: list[tuple[float, float]] = []
    maximum_error = mp.mpf(0)
    for k in range(MAX_FFT_SIZE // 2):
        angle = 2 * mp.pi * mp.mpf(k) / mp.mpf(MAX_FFT_SIZE)
        exact_re = mp.cos(angle)
        exact_im = -mp.sin(angle)
        stored_re = float(exact_re)
        stored_im = float(exact_im)
        if stored_re == 0.0:
            stored_re = 0.0
        if stored_im == 0.0:
            stored_im = 0.0
        error = mp.sqrt(
            (mp.mpf(stored_re) - exact_re) ** 2
            + (mp.mpf(stored_im) - exact_im) ** 2
        )
        maximum_error = max(maximum_error, error)
        twiddles.append((stored_re, stored_im))
    if maximum_error > TWIDDLE_COMPLEX_ERROR_UPPER:
        raise RuntimeError(
            f"frozen twiddle error {maximum_error} exceeds published bound "
            f"{TWIDDLE_COMPLEX_ERROR_UPPER}"
        )
    return twiddles, maximum_error


def render_rust(
    twiddles: list[tuple[float, float]],
    legacy: dict[str, mp.mpf],
    hq: dict[str, mp.mpf],
) -> str:
    lines = [
        "// @generated by qualification/generate_qualified_prefix.py; do not edit.",
        "// Fixed binary64 roots and source-derived enclosures for the certified 2x prefix.",
        "",
        f"pub(crate) const QUALIFIED_PREFIX_MAX_FFT_SIZE: usize = {MAX_FFT_SIZE};",
        f"pub(crate) const QUALIFIED_PREFIX_TWIDDLE_COMPLEX_ERROR_UPPER: f64 = {rust_float(outward_float(TWIDDLE_COMPLEX_ERROR_UPPER))};",
        f"pub(crate) const LEGACY_QUALIFIED_PREFIX_ERROR_PER_PACKED_COMPONENT_PEAK_UPPER: f64 = {rust_float(outward_float(legacy['output_relative']))};",
        f"pub(crate) const LEGACY_QUALIFIED_PREFIX_ABSOLUTE_ERROR_UPPER: f64 = {rust_float(outward_float(legacy['output_absolute']))};",
        f"pub(crate) const HQ1024_QUALIFIED_PREFIX_ERROR_PER_PACKED_COMPONENT_PEAK_UPPER: f64 = {rust_float(outward_float(hq['output_relative']))};",
        f"pub(crate) const HQ1024_QUALIFIED_PREFIX_ABSOLUTE_ERROR_UPPER: f64 = {rust_float(outward_float(hq['output_absolute']))};",
        "",
        "pub(crate) const QUALIFIED_PREFIX_TWIDDLES_8192: [(f64, f64); 4096] = [",
    ]
    lines.extend(
        f"    ({rust_float(re)}, {rust_float(im)})," for re, im in twiddles
    )
    lines.extend(["];", ""])
    return "\n".join(lines)


def report_dict(maximum_twiddle_error: mp.mpf, legacy: dict[str, mp.mpf], hq: dict[str, mp.mpf]) -> dict:
    def serial(value: mp.mpf) -> float:
        return float(value)

    return {
        "schema": 1,
        "status": "pass",
        "construction": {
            "algorithm": "fixed radix-2 decimation-in-time complex FFT",
            "max_fft_size": MAX_FFT_SIZE,
            "legacy_fft_size": LEGACY_FFT_SIZE,
            "hq1024_fft_size": HQ_FFT_SIZE,
            "twiddle_table_entries": MAX_FFT_SIZE // 2,
            "twiddle_reference_precision_decimal_digits": mp.mp.dps,
            "twiddle_complex_error_measured_max": serial(maximum_twiddle_error),
            "twiddle_complex_error_published_upper": serial(TWIDDLE_COMPLEX_ERROR_UPPER),
            "unit_roundoff": serial(UNIT_ROUNDOFF),
            "complex_mul_relative_factor": serial(COMPLEX_MUL_RELATIVE),
            "complex_twiddle_mul_min_normal_units": serial(COMPLEX_TWIDDLE_MUL_MIN_NORMAL_UNITS),
            "complex_add_min_normal_units": serial(COMPLEX_ADD_MIN_NORMAL_UNITS),
            "filter_spectrum_subnormals_canonicalized_to_zero": True,
            "normalization": "exact power-of-two reciprocal",
            "packed_pair_scale": "maximum absolute real component over both channels and full overlap-save block",
        },
        "legacy64": {
            key: serial(value) for key, value in legacy.items()
        },
        "hq1024": {
            key: serial(value) for key, value in hq.items()
        },
        "checks": {
            "twiddle_reference_inside_published_upper": maximum_twiddle_error < TWIDDLE_COMPLEX_ERROR_UPPER,
            "legacy_output_error_below_1e_6_linear": legacy["output_relative"] < mp.mpf("1e-6"),
            "hq_output_error_below_1e_6_linear": hq["output_relative"] < mp.mpf("1e-6"),
            "absolute_underflow_terms_are_positive": legacy["output_absolute"] > 0 and hq["output_absolute"] > 0,
        },
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--rust-output", type=Path, required=True)
    parser.add_argument("--json-output", type=Path, required=True)
    args = parser.parse_args()

    twiddles, maximum_twiddle_error = make_twiddles()
    legacy_half = parse_rust_f64_array(
        LEGACY_RUST, "HEADROOM64_HALF_DELAY_COEFFICIENTS"
    )
    hq_half = parse_rust_f64_array(
        HQ_RUST, "HQ1024_HALF_DELAY_COEFFICIENTS"
    )
    legacy = prefix_bound(LEGACY_FFT_SIZE, legacy_half)
    hq = prefix_bound(HQ_FFT_SIZE, hq_half)

    rust = render_rust(twiddles, legacy, hq)
    report = report_dict(maximum_twiddle_error, legacy, hq)
    if not all(report["checks"].values()):
        raise RuntimeError(f"qualified-prefix derivation failed: {report['checks']}")

    args.rust_output.parent.mkdir(parents=True, exist_ok=True)
    args.json_output.parent.mkdir(parents=True, exist_ok=True)
    args.rust_output.write_text(rust)
    args.json_output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
