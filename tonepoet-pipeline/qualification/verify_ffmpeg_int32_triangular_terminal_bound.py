#!/usr/bin/env python3
"""Verify the source-derived FFmpeg Float64->triangular->S32 terminal bound.

This is a mathematical reduced-state check. It does not characterize FFmpeg and
must not be used in place of inspecting or executing the qualified implementation.
"""

from __future__ import annotations

import json
import math

LSB = 2.0 ** -31
DBL_ADD_ULP = 2.0 ** -52
SCALAR_MUL_ERROR = 2.0 ** -51
HQ1024_LINF_GAIN = 4.68
BOUND = math.nextafter(LSB + LSB + DBL_ADD_ULP, math.inf)


def next_up_nonnegative(value: float) -> float:
    assert math.isfinite(value) and value >= 0.0
    return math.nextafter(value, math.inf)


def quantize_s32(value: float, mode: str) -> float:
    scaled = value * (2.0 ** 31)
    if mode == "down":
        rounded = math.floor(scaled)
    elif mode == "up":
        rounded = math.ceil(scaled)
    elif mode == "toward_zero":
        rounded = math.trunc(scaled)
    elif mode == "nearest":
        rounded = round(scaled)
    else:
        raise AssertionError(mode)
    clipped = max(-(2**31), min((2**31) - 1, rounded))
    return clipped * LSB


def main() -> int:
    assert BOUND > LSB + LSB + DBL_ADD_ULP
    assert 2.0 < BOUND / LSB < 2.000001

    extrema = 0
    maximum_extremal_error = 0.0
    for dither in (-LSB, LSB):
        for add_rounding in (-DBL_ADD_ULP, DBL_ADD_ULP):
            for integer_rounding in (-LSB, LSB):
                error = abs(dither + add_rounding + integer_rounding)
                maximum_extremal_error = max(maximum_extremal_error, error)
                assert error <= BOUND
                extrema += 1

    half_lsb = 0.5 * LSB
    positive_below = math.nextafter(half_lsb, -math.inf)
    positive_above = math.nextafter(half_lsb, math.inf)
    negative_below = math.nextafter(-half_lsb, -math.inf)
    negative_above = math.nextafter(-half_lsb, math.inf)
    boundary_inputs = [
        -1.0,
        -1.0 + 0.25 * LSB,
        -1.5 * LSB,
        negative_below,
        -half_lsb,
        negative_above,
        0.0,
        positive_below,
        half_lsb,
        positive_above,
        1.5 * LSB,
        1.0 - LSB,
        1.0 - 0.25 * LSB,
        1.0,
    ]
    modes = ("down", "up", "toward_zero", "nearest")
    states = 0
    maximum_boundary_error = 0.0
    for source in boundary_inputs:
        assert math.isfinite(source) and -1.0 <= source <= 1.0
        for dither in (-LSB, 0.0, LSB):
            for add_rounding in (-DBL_ADD_ULP, 0.0, DBL_ADD_ULP):
                dithered = source + dither + add_rounding
                for mode in modes:
                    realized = quantize_s32(dithered, mode)
                    error = abs(realized - source)
                    maximum_boundary_error = max(maximum_boundary_error, error)
                    assert error <= BOUND, (
                        source,
                        dither,
                        add_rounding,
                        mode,
                        error,
                        BOUND,
                    )
                    states += 1

    # Hard-ceiling saturation premise. The certified peak interval explicitly
    # includes the exact Float64 carrier sample maximum P. The existing solver
    # enforces G*P + Epost <= C <= 1, where Epost charges both the reconstructed
    # terminal bound and the distinct binary64 scalar-multiplication error.
    # Therefore an actually scaled sample is no larger than 1-Epost+Bscalar.
    # Show that even after worst-case triangular dither and binary64 addition
    # rounding the DBL value remains inside the interval on which llrint cannot
    # saturate S32 under any standard rounding direction.
    terminal_reconstructed = next_up_nonnegative(BOUND * HQ1024_LINF_GAIN)
    scalar_reconstructed = next_up_nonnegative(SCALAR_MUL_ERROR * HQ1024_LINF_GAIN)
    epost = next_up_nonnegative(terminal_reconstructed + scalar_reconstructed)
    max_scaled_sample = 1.0 - epost + SCALAR_MUL_ERROR
    max_pre_llrint = max_scaled_sample + LSB + DBL_ADD_ULP
    min_pre_llrint = -max_scaled_sample - LSB - DBL_ADD_ULP
    positive_s32_no_saturation_limit = 1.0 - LSB
    negative_s32_no_saturation_limit = -1.0
    assert max_pre_llrint <= positive_s32_no_saturation_limit
    assert min_pre_llrint >= negative_s32_no_saturation_limit

    result = {
        "schema": 1,
        "status": "pass",
        "int32_lsb_fs": LSB,
        "binary64_add_ulp_fs": DBL_ADD_ULP,
        "stored_error_upper_fs": BOUND,
        "stored_error_upper_lsb": BOUND / LSB,
        "extremal_sign_states_checked": extrema,
        "boundary_quantizer_states_checked": states,
        "maximum_extremal_error_fs": maximum_extremal_error,
        "maximum_boundary_error_fs": maximum_boundary_error,
        "hq1024_linf_gain_upper": HQ1024_LINF_GAIN,
        "scalar_mul_error_fs": SCALAR_MUL_ERROR,
        "post_gain_reconstructed_error_fs": epost,
        "max_scaled_sample_from_ceiling_solver_fs": max_scaled_sample,
        "max_pre_llrint_fs": max_pre_llrint,
        "positive_no_saturation_limit_fs": positive_s32_no_saturation_limit,
        "min_pre_llrint_fs": min_pre_llrint,
        "negative_no_saturation_limit_fs": negative_s32_no_saturation_limit,
        "rounding_modes": list(modes),
        "note": "reduced-state mathematical verification; FFmpeg implementation conformance is separate",
    }
    print(json.dumps(result, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
