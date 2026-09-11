#!/usr/bin/env python3
from __future__ import annotations

import math
import tempfile
import unittest
from pathlib import Path

from portable_compare import (
    BLAS_COEFFICIENT_ABS_TOL,
    PortableComparisonError,
    _fnv1a64,
    require_float_sequences_close,
    require_hq_rust_portable,
)


def synthetic_hq_rust(
    *,
    half: list[float],
    tail: list[list[float]],
    node_a: list[float],
    node_b: list[float],
    tail_factor: int = 512,
) -> str:
    def flattened(values: list[float] | list[list[float]]) -> list[float]:
        if values and isinstance(values[0], list):
            return [item for row in values for item in row]  # type: ignore[union-attr]
        return list(values)  # type: ignore[arg-type]

    return f"""// synthetic portable-comparison fixture
pub(crate) const HQ1024_TAIL_FACTOR: usize = {tail_factor};
#[cfg(test)]
pub(crate) const HQ1024_MEASURED_OPERATOR_LINF: f64 = 4.676026435151282;
#[cfg(test)]
pub(crate) const HQ1024_FIRST_HALF_FNV1A64: u64 = 0x{_fnv1a64(flattened(half)):016x};
#[cfg(test)]
pub(crate) const HQ1024_TAIL_FNV1A64: u64 = 0x{_fnv1a64(flattened(tail)):016x};
#[cfg(test)]
pub(crate) const HQ1024_NODE_A_FNV1A64: u64 = 0x{_fnv1a64(flattened(node_a)):016x};
#[cfg(test)]
pub(crate) const HQ1024_NODE_B_FNV1A64: u64 = 0x{_fnv1a64(flattened(node_b)):016x};
pub(crate) const HQ1024_HALF_DELAY_COEFFICIENTS: [f64; {len(half)}] = {half!r};
pub(crate) const HQ1024_TAIL_COEFFICIENTS: [[f64; {len(tail[0])}]; {len(tail)}] = {tail!r};
pub(crate) const HQ1024_NODE_A_UPPER: [f64; {len(node_a)}] = {node_a!r};
pub(crate) const HQ1024_NODE_B_UPPER: [f64; {len(node_b)}] = {node_b!r};
"""


class PortableCoefficientComparisonTests(unittest.TestCase):
    def test_accepts_last_bit_cross_blas_drift(self) -> None:
        frozen = [1.0, -0.25, 1.0e-9]
        regenerated = [
            math.nextafter(math.nextafter(1.0, math.inf), math.inf),
            math.nextafter(-0.25, -math.inf),
            math.nextafter(1.0e-9, math.inf),
        ]
        require_float_sequences_close(regenerated, frozen, label="synthetic")

    def test_rejects_delta_beyond_portability_envelope(self) -> None:
        with self.assertRaises(PortableComparisonError):
            require_float_sequences_close(
                [1.0 + 2.0 * BLAS_COEFFICIENT_ABS_TOL],
                [1.0],
                label="synthetic",
            )

    def test_rejects_support_change_even_below_absolute_tolerance(self) -> None:
        with self.assertRaises(PortableComparisonError):
            require_float_sequences_close([1.0e-16], [0.0], label="synthetic")

    def test_rejects_sign_change_even_below_absolute_tolerance(self) -> None:
        with self.assertRaises(PortableComparisonError):
            require_float_sequences_close([1.0e-16], [-1.0e-16], label="synthetic")

    def test_full_hq_rust_accepts_close_values_with_different_self_consistent_hashes(self) -> None:
        frozen_half = [1.0, -0.25]
        regenerated_half = [math.nextafter(math.nextafter(1.0, math.inf), math.inf), -0.25]
        common = {
            "tail": [[1.0, 0.5]],
            "node_a": [0.125],
            "node_b": [0.0625],
        }
        with tempfile.TemporaryDirectory() as raw_tmp:
            tmp = Path(raw_tmp)
            frozen = tmp / "frozen.rs"
            regenerated = tmp / "regenerated.rs"
            frozen.write_text(synthetic_hq_rust(half=frozen_half, **common), encoding="utf-8")
            regenerated.write_text(
                synthetic_hq_rust(half=regenerated_half, **common),
                encoding="utf-8",
            )
            require_hq_rust_portable(regenerated, frozen)

    def test_full_hq_rust_rejects_non_fp_geometry_change(self) -> None:
        common = {
            "half": [1.0, -0.25],
            "tail": [[1.0, 0.5]],
            "node_a": [0.125],
            "node_b": [0.0625],
        }
        with tempfile.TemporaryDirectory() as raw_tmp:
            tmp = Path(raw_tmp)
            frozen = tmp / "frozen.rs"
            regenerated = tmp / "regenerated.rs"
            frozen.write_text(synthetic_hq_rust(**common), encoding="utf-8")
            regenerated.write_text(
                synthetic_hq_rust(**common, tail_factor=513),
                encoding="utf-8",
            )
            with self.assertRaises(PortableComparisonError):
                require_hq_rust_portable(regenerated, frozen)


if __name__ == "__main__":
    unittest.main(verbosity=2)
