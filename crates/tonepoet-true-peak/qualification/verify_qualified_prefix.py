#!/usr/bin/env python3
"""Independent source-round audit of the authoritative fixed 2x prefix.

This verifier intentionally does not import generate_qualified_prefix.py.  It
reconstructs the numerical enclosure from the declared fixed radix-2 geometry,
checks every frozen twiddle against a high-precision root of unity, checks the
published constants are outward, and audits the Rust source shape that binds
those constants to the certified scanner.

Shipping Rust tests/codegen and the commissioning benchmark remain separate
operator release gates; this file proves the source construction, not that an
unrun release binary has been commissioned.
"""
from __future__ import annotations

import argparse
import json
import math
import re
from pathlib import Path

import mpmath as mp

ROOT = Path(__file__).resolve().parents[1]
COEFF = ROOT / "src" / "qualified_prefix_coefficients.rs"
EXECUTOR = ROOT / "src" / "qualified_half_delay_fft.rs"
SCANNER = ROOT / "src" / "certified_scan.rs"
LEGACY = ROOT / "src" / "headroom64_coefficients.rs"
HQ = ROOT / "src" / "hq1024_coefficients.rs"

U = mp.mpf(2) ** -53
MIN_NORMAL = mp.mpf(2) ** -1022
MAX_FFT = 8192
LEGACY_N = 2048
HQ_N = 8192
TWIDDLE_ERROR_BOUND = mp.mpf(2) ** -52
CMUL_REL = 2 * U * (2 + U)
TWIDDLE_ABS_UNITS = mp.mpf(12)
BUTTERFLY_ABS_UNITS = mp.mpf(6)


def parse_scalar(path: Path, name: str, rust_type: str) -> float:
    text = path.read_text()
    m = re.search(
        rf"const\s+{re.escape(name)}\s*:\s*{re.escape(rust_type)}\s*=\s*([^;]+);",
        text,
    )
    if not m:
        raise RuntimeError(f"missing {name} in {path.name}")
    token = m.group(1).replace("_", "").strip()
    return float(token) if rust_type == "f64" else int(token, 0)


def parse_f64_array(path: Path, name: str) -> list[float]:
    text = path.read_text()
    m = re.search(
        rf"const\s+{re.escape(name)}\s*:\s*\[f64;\s*\d+\]\s*=\s*\[(.*?)\];",
        text,
        re.S,
    )
    if not m:
        raise RuntimeError(f"missing {name} in {path.name}")
    return [float(x) for x in m.group(1).replace("\n", " ").split(",") if x.strip()]


def parse_twiddles() -> list[tuple[float, float]]:
    text = COEFF.read_text()
    m = re.search(
        r"QUALIFIED_PREFIX_TWIDDLES_8192\s*:\s*\[\(f64, f64\);\s*4096\]\s*=\s*\[(.*?)\];",
        text,
        re.S,
    )
    if not m:
        raise RuntimeError("missing frozen qualified-prefix twiddle table")
    return [
        (float(a), float(b))
        for a, b in re.findall(r"\(([^,]+),\s*([^\)]+)\)", m.group(1))
    ]


def transform_enclosure(n: int) -> tuple[mp.mpf, mp.mpf]:
    if n <= 0 or n & (n - 1):
        raise ValueError("n must be a power of two")
    rel = mp.mpf(0)
    abs_units = mp.mpf(0)
    exact_peak = mp.mpf(1)
    for _ in range(n.bit_length() - 1):
        product_rel = (
            (1 + TWIDDLE_ERROR_BOUND) * rel
            + TWIDDLE_ERROR_BOUND * exact_peak
            + CMUL_REL * (1 + TWIDDLE_ERROR_BOUND) * (exact_peak + rel)
        )
        product_abs = (
            (1 + TWIDDLE_ERROR_BOUND) * abs_units
            + CMUL_REL * (1 + TWIDDLE_ERROR_BOUND) * abs_units
            + TWIDDLE_ABS_UNITS
        )
        rel = rel + product_rel + U * (2 * exact_peak + rel + product_rel)
        abs_units = (
            abs_units
            + product_abs
            + U * (abs_units + product_abs)
            + BUTTERFLY_ABS_UNITS
        )
        exact_peak *= 2
    return rel, abs_units


def prefix_enclosure(n: int, half: list[float]) -> dict[str, mp.mpf]:
    full = [mp.mpf(x) for x in half] + [mp.mpf(x) for x in reversed(half)]
    l1 = mp.fsum(abs(x) for x in full)
    coeff_peak = max(abs(x) for x in full)
    transform_rel, transform_abs = transform_enclosure(n)
    sqrt2 = mp.sqrt(2)

    # Same mathematical obligations as the executor, independently written:
    # filter FFT, packed-signal FFT, pointwise complex product, inverse FFT,
    # exact 1/N normalization, optional power-of-two input scaling, FTZ/DAZ.
    filter_error = (
        transform_rel * coeff_peak + (transform_abs + sqrt2) * MIN_NORMAL
    )
    filter_mag = l1 + filter_error

    signal_rel = transform_rel * sqrt2
    input_scaling_abs = mp.mpf(n) * sqrt2
    signal_abs = transform_abs + input_scaling_abs
    exact_signal_spectrum = mp.mpf(n) * sqrt2
    computed_signal_spectrum = exact_signal_spectrum + signal_rel

    product_floor = 4 * filter_mag + 12
    product_rel = (
        l1 * signal_rel
        + exact_signal_spectrum * filter_error
        + signal_rel * filter_error
        + CMUL_REL * computed_signal_spectrum * filter_mag
    )
    product_abs = (
        l1 * signal_abs
        + signal_abs * filter_error
        + CMUL_REL * signal_abs * filter_mag
        + product_floor
    )
    computed_product_rel = mp.mpf(n) * sqrt2 * l1 + product_rel
    inverse_rel = transform_rel * computed_product_rel
    inverse_abs = transform_rel * product_abs + transform_abs

    output_rel = product_rel + inverse_rel / mp.mpf(n)
    output_abs_units = product_abs + inverse_abs / mp.mpf(n) + 1
    return {
        "filter_l1": l1,
        "transform_relative": transform_rel,
        "transform_absolute_units": transform_abs,
        "input_scaling_absolute_units": input_scaling_abs,
        "filter_spectrum_error": filter_error,
        "output_relative_required": output_rel,
        "output_absolute_required": output_abs_units * MIN_NORMAL,
    }


def runtime_l1_upper(half: list[float]) -> float:
    """Independently simulate the Rust outward-add L1 construction."""
    total = 0.0
    for coefficient in half:
        total = math.nextafter(total + abs(coefficient), math.inf)
    return math.nextafter(total + total, math.inf)


def outward_and_tight(published: float, required: mp.mpf) -> bool:
    p = mp.mpf(published)
    if p < required:
        return False
    # Generated values are meant to be one outward binary64 step, not a magic
    # safety literal. A very loose mutation should fail the qualifier too.
    return p <= required * (1 + mp.mpf("1e-12")) + mp.mpf("1e-320")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    mp.mp.dps = 120

    assert parse_scalar(COEFF, "QUALIFIED_PREFIX_MAX_FFT_SIZE", "usize") == MAX_FFT
    twiddles = parse_twiddles()
    if len(twiddles) != MAX_FFT // 2:
        raise RuntimeError("qualified-prefix twiddle table length changed")
    max_twiddle_error = mp.mpf(0)
    for k, (re_value, im_value) in enumerate(twiddles):
        angle = 2 * mp.pi * mp.mpf(k) / mp.mpf(MAX_FFT)
        exact_re = mp.cos(angle)
        exact_im = -mp.sin(angle)
        error = mp.sqrt((mp.mpf(re_value) - exact_re) ** 2 + (mp.mpf(im_value) - exact_im) ** 2)
        max_twiddle_error = max(max_twiddle_error, error)

    legacy_half = parse_f64_array(LEGACY, "HEADROOM64_HALF_DELAY_COEFFICIENTS")
    hq_half = parse_f64_array(HQ, "HQ1024_HALF_DELAY_COEFFICIENTS")
    legacy = prefix_enclosure(LEGACY_N, legacy_half)
    hq = prefix_enclosure(HQ_N, hq_half)
    legacy_skip_l1_upper = runtime_l1_upper(legacy_half)
    hq_skip_l1_upper = runtime_l1_upper(hq_half)
    published = {
        "legacy_relative": parse_scalar(COEFF, "LEGACY_QUALIFIED_PREFIX_ERROR_PER_PACKED_COMPONENT_PEAK_UPPER", "f64"),
        "legacy_absolute": parse_scalar(COEFF, "LEGACY_QUALIFIED_PREFIX_ABSOLUTE_ERROR_UPPER", "f64"),
        "hq_relative": parse_scalar(COEFF, "HQ1024_QUALIFIED_PREFIX_ERROR_PER_PACKED_COMPONENT_PEAK_UPPER", "f64"),
        "hq_absolute": parse_scalar(COEFF, "HQ1024_QUALIFIED_PREFIX_ABSOLUTE_ERROR_UPPER", "f64"),
    }

    executor = EXECUTOR.read_text()
    scanner = SCANNER.read_text()
    source_checks = {
        "owned_fft_sizes_match_derivation": (
            "const LEGACY_FFT_SIZE: usize = 2048;" in executor
            and "const HQ1024_FFT_SIZE: usize = 8192;" in executor
        ),
        "owned_radix2_executor_present": (
            "fn fixed_radix2_fft" in executor
            and "bit_reverse_permute(values);" in executor
            and "QUALIFIED_PREFIX_TWIDDLES_8192" in executor
            and "FftPlanner" not in executor
        ),
        "complex_multiply_graph_matches_derivation": (
            "let rr = left.re * right.re;" in executor
            and "let ii = left.im * right.im;" in executor
            and "let ri = left.re * right.im;" in executor
            and "let ir = left.im * right.re;" in executor
            and "Complex64::new(rr - ii, ri + ir)" in executor
            and ".mul_add(" not in executor
        ),
        "avx_dispatch_preserves_the_certified_arithmetic_graph": (
            "is_x86_feature_detected!(\"avx\")" in executor
            and "#[target_feature(enable = \"avx\")]" in executor
            and "unsafe fn complex_mul_pair_avx" in executor
            and "_mm256_mul_pd" in executor
            and "_mm256_addsub_pd" in executor
            and "_mm256_add_pd(upper, rotated)" in executor
            and "_mm256_sub_pd(upper, rotated)" in executor
            and "_mm256_fmadd" not in executor
            and "_mm256_fmsub" not in executor
            and "avx_executor_is_bitwise_identical_to_scalar_authority_graph" in executor
            and "fixed_radix2_fft_scalar_stages(values, inverse);" in executor
            and "fn fixed_radix2_fft_fast90" in executor
            and "fast90_same_graph_avx: bool" in executor
            and "if self.fast90_same_graph_avx" in executor
            and "pub(crate) fn new_fast90" in executor
            and "request_fast90_same_graph_avx && fast90_avx_available()" in executor
        ),
        "fast90_bit_reversal_is_precomputed_without_changing_permutation": (
            "fn bit_reverse_swaps(len: usize)" in executor
            and "fn bit_reverse_permute_precomputed" in executor
            and "bit_reverse_swaps: Vec<(u16, u16)>" in executor
            and "bit_reverse_permute_precomputed(values, bit_reverse_swaps);" in executor
            and "let swaps = bit_reverse_swaps(fft_size);" in executor
        ),
        "radix2_butterfly_graph_matches_derivation": (
            "let mut span = 2usize;" in executor
            and "while span <= fft_size" in executor
            and "let half = span / 2;" in executor
            and "let twiddle_stride = QUALIFIED_PREFIX_MAX_FFT_SIZE / span;" in executor
            and "for base in (0..fft_size).step_by(span)" in executor
            and "for offset in 0..half" in executor
            and "let rotated = complex_mul(values[base + offset + half], twiddle);" in executor
            and "upper.re + rotated.re" in executor
            and "upper.im + rotated.im" in executor
            and "upper.re - rotated.re" in executor
            and "upper.im - rotated.im" in executor
            and "span *= 2;" in executor
        ),
        "overlap_save_arithmetic_matches_derivation": (
            "self.history[channel_pair_start][history_index] * block_scale" in executor
            and "self.pending[frame_index * self.channels + channel_pair_start] * block_scale" in executor
            and "*bin = complex_mul(*bin, filter);" in executor
            and "let re = (output.re * inverse_fft_scale) * inverse_block_scale;" in executor
            and "let im = (output.im * inverse_fft_scale) * inverse_block_scale;" in executor
        ),
        "overflow_scaling_is_binary_power_only": (
            "fn normal_power_of_two(exponent: i32) -> f64" in executor
            and "f64::from_bits(((exponent + 1023) as u64) << 52)" in executor
            and "normal_power_of_two(-shift)" in executor
            and "normal_power_of_two(shift)" in executor
            and ".powf(" not in executor
            and ".exp(" not in executor
        ),
        "filter_spectrum_uses_same_owned_transform": (
            "fixed_radix2_fft(&mut filter_spectrum, false);" in executor
            and "filter_spectrum[taps - 1 - index].re = coefficient;" in executor
        ),
        "inverse_normalization_matches_derivation": (
            "let inverse_fft_scale = 1.0 / fft_size as f64;" in executor
            and "fixed_radix2_fft(&mut self.fft_buffer, true);" in executor
        ),
        "packed_scale_covers_pair_and_full_overlap_block": (
            "for channel_pair_start in (0..self.channels).step_by(2)" in executor
            and "for history_index in 0..overlap" in executor
            and "for frame_index in 0..count" in executor
            and "let mut component_peak_bits = 0_u64;" in executor
            and "component_peak_bits = component_peak_bits.max(magnitude_bits(" in executor
            and "if component_peak_bits == 0" in executor
            and "let component_peak = f64::from_bits(component_peak_bits);" in executor
            and "component_peak = component_peak.max" not in executor
        ),
        "subnormal_filter_components_are_canonicalized_bitwise": (
            "is_subnormal_magnitude_bits(magnitude_bits(bin.re))" in executor
            and "is_subnormal_magnitude_bits(magnitude_bits(bin.im))" in executor
            and "bin.re.abs() < f64::MIN_POSITIVE" not in executor
            and "bin.im.abs() < f64::MIN_POSITIVE" not in executor
        ),
        "authority_zero_and_subnormal_classification_is_daz_independent": (
            "fn magnitude_bits(value: f64) -> u64" in executor
            and "fn nonnegative_finite_bits(value: f64) -> Option<u64>" in executor
            and "fn widened_nonnegative_bound(bits: u64) -> f64" in executor
            and "if left_bits == 0 || right_bits == 0" in executor
            and "if left == 0.0 || right == 0.0" not in executor
            and "if component_peak_bits == 0" in executor
            and "let component_peak = f64::from_bits(component_peak_bits);" in executor
            and "fn nonzero_authority_upper(bound: f64) -> f64" in executor
            and "if magnitude_bits(bound) < F64_MIN_NORMAL_BITS" in executor
            and "if component_peak == 0.0" not in executor
            and "fn magnitude_bits(value: f64) -> u64" in scanner
            and "fn nonnegative_finite_bits(value: f64) -> Option<u64>" in scanner
            and "if magnitude_bits(evaluation.error) == 0" in scanner
            and "if evaluation.error == 0.0" not in scanner
            and "*peak = max_exact_magnitude(*peak, sample);" in scanner
            and "*peak = (*peak).max(sample.abs());" not in scanner
        ),
        "time_bounded_prefix_skip_uses_source_derived_l1_enclosure": (
            "fn first_stage_l1_upper(kind: QualifiedHalfDelayKind) -> f64" in executor
            and ".fold(0.0_f64, |sum, coefficient| outward_add(sum, coefficient.abs()))" in executor
            and "outward_add(half, half)" in executor
            and "fn process_frame_with_fft_permission" in executor
            and "if !execute_fft" in executor
            and "nonzero_authority_upper(outward_mul(" in executor
            and "first_stage_l1_upper(self.kind)" in executor
            and "component_peak" in executor
            and "self.half_output[frame_index * self.channels + channel_pair_start] = 0.0;" in executor
            and "skipped_fft_block_is_conservative_and_does_not_break_later_fft_state" in executor
            and "skipped_fft_l1_enclosure_survives_daz_ftz_subnormal_input" in executor
        ),
        "certified_scanner_uses_owned_prefix": (
            "use super::qualified_half_delay_fft::QualifiedHalfDelayFft;" in scanner
            and "inner: QualifiedHalfDelayFft" in scanner
        ),
        "retired_unqualified_executor_is_absent": (
            not (ROOT / "src" / "fixed_half_delay_fft.rs").exists()
            and "mod fixed_half_delay_fft" not in (ROOT / "src" / "lib.rs").read_text()
        ),
        "raw_domain_normal_screen_removed": (
            "raw_group_upper" not in scanner
            and "RawChannelBounds" not in scanner
            and "CandidateWork::RawCell" not in scanner
        ),
        "coarse_authority_screen_present": (
            "CoarseChannelSummary::build" in scanner
            and "coarse_group_upper" in scanner
            and "authoritative_coarse_values" in scanner
            and "authoritative_coarse_groups" in scanner
        ),
    }

    checks = {
        "all_frozen_twiddles_inside_declared_complex_error": max_twiddle_error <= TWIDDLE_ERROR_BOUND,
        "published_twiddle_error_is_outward": parse_scalar(COEFF, "QUALIFIED_PREFIX_TWIDDLE_COMPLEX_ERROR_UPPER", "f64") >= float(TWIDDLE_ERROR_BOUND),
        "legacy_relative_bound_rederived_outward_and_tight": outward_and_tight(published["legacy_relative"], legacy["output_relative_required"]),
        "legacy_absolute_bound_rederived_outward_and_tight": outward_and_tight(published["legacy_absolute"], legacy["output_absolute_required"]),
        "hq_relative_bound_rederived_outward_and_tight": outward_and_tight(published["hq_relative"], hq["output_relative_required"]),
        "hq_absolute_bound_rederived_outward_and_tight": outward_and_tight(published["hq_absolute"], hq["output_absolute_required"]),
        "hq_prefix_error_has_1e_6_linear_margin": published["hq_relative"] < 1.0e-6,
        "legacy_skipped_prefix_l1_is_outward": mp.mpf(legacy_skip_l1_upper) >= legacy["filter_l1"],
        "hq_skipped_prefix_l1_is_outward": mp.mpf(hq_skip_l1_upper) >= hq["filter_l1"],
        **source_checks,
    }
    failed = [name for name, value in checks.items() if not value]
    if failed:
        raise RuntimeError("qualified-prefix audit failed: " + ", ".join(failed))

    report = {
        "schema": 1,
        "status": "pass",
        "checks": checks,
        "twiddles": {
            "entries": len(twiddles),
            "measured_max_complex_error": float(max_twiddle_error),
            "declared_complex_error_upper": float(TWIDDLE_ERROR_BOUND),
        },
        "legacy64": {key: float(value) for key, value in legacy.items()} | {
            "published_relative_error_upper": published["legacy_relative"],
            "published_absolute_error_upper": published["legacy_absolute"],
            "time_bounded_skip_l1_upper": legacy_skip_l1_upper,
        },
        "hq1024": {key: float(value) for key, value in hq.items()} | {
            "published_relative_error_upper": published["hq_relative"],
            "published_absolute_error_upper": published["hq_absolute"],
            "time_bounded_skip_l1_upper": hq_skip_l1_upper,
        },
        "qualification_scope": {
            "source_round_enclosure_rederived_independently": True,
            "owned_fixed_fft_is_certificate_authority": True,
            "external_planner_or_alternate_simd_fft_is_certificate_authority": False,
            "same_graph_avx_executor_source_audited": True,
            "shipping_rust_tests_run_here": False,
            "shipping_codegen_verified_here": False,
            "commissioning_benchmark_run_here": False,
        },
    }
    args.output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    print(json.dumps(report, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
