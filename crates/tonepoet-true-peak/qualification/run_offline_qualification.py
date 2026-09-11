#!/usr/bin/env python3
"""Run every source-round offline qualifier without mutating checked-in outputs.

This is intentionally not a release qualifier: Cargo tests, shipping-codegen
regressions, and commissioning benchmarks remain operator owned. The runner
makes the source-only numerical derivations reproducible and idempotent.

NumPy/SciPy filter regeneration is compared numerically rather than bit-for-bit
because conforming BLAS/LAPACK implementations may differ in the last few
binary64 bits. Integer/rational/source-derived artifacts remain exact.
"""
from __future__ import annotations

import pathlib
import subprocess
import sys
import tempfile

from portable_compare import (
    require_hq_json_portable,
    require_hq_rust_portable,
    require_json_numeric_close,
)


def run(*args: object) -> None:
    command = [str(arg) for arg in args]
    print("+", " ".join(command), flush=True)
    subprocess.run(command, check=True)


def require_identical(generated: pathlib.Path, checked_in: pathlib.Path) -> None:
    generated_bytes = generated.read_bytes()
    checked_bytes = checked_in.read_bytes()
    if generated_bytes != checked_bytes:
        raise RuntimeError(
            f"generated output differs from checked-in file: {checked_in.relative_to(CRATE_ROOT)}"
        )
    print(f"identical: {checked_in.relative_to(CRATE_ROOT)}", flush=True)


CRATE_ROOT = pathlib.Path(__file__).resolve().parents[1]
QUALIFICATION = CRATE_ROOT / "qualification"
PYTHON = pathlib.Path(sys.executable).resolve()


def main() -> int:
    with tempfile.TemporaryDirectory(prefix="tonepoet-true-peak-qualification-") as raw_tmp:
        tmp = pathlib.Path(raw_tmp)
        generated_rust = tmp / "hq1024_coefficients.rs"
        generated_json = tmp / "hq1024_candidate_coefficients.json"
        qualified_prefix_rust = tmp / "qualified_prefix_coefficients.rs"
        qualified_prefix_report = tmp / "qualified_prefix_report.json"
        qualified_prefix_verification = tmp / "qualified_prefix_verification.json"
        certified_report = tmp / "certified_search_report.json"
        fast_metadata = tmp / "fast066_metadata.json"
        raw_screen_metadata = tmp / "raw_screen_metadata.json"
        fast_verification = tmp / "fast066_verification.json"

        # Guard the portable comparator itself: accept last-bit drift, but reject
        # larger coefficient changes and any change to exact-zero/sign topology.
        run(PYTHON, "-B", QUALIFICATION / "test_portable_compare.py")

        run(
            PYTHON,
            "-B",
            QUALIFICATION / "generate_hq1024.py",
            "--rust-output",
            generated_rust,
            "--json-output",
            generated_json,
        )
        rust_deltas = require_hq_rust_portable(
            generated_rust,
            CRATE_ROOT / "src" / "hq1024_coefficients.rs",
        )
        print(
            "portable-close: src/hq1024_coefficients.rs "
            f"(max coefficient |delta|={max(v for k, v in rust_deltas.items() if 'MEASURED' not in k):.3e})",
            flush=True,
        )
        json_deltas = require_hq_json_portable(
            generated_json,
            QUALIFICATION / "hq1024_candidate_coefficients.json",
            generated_rust,
            CRATE_ROOT / "src" / "hq1024_coefficients.rs",
        )
        print(
            "portable-close: qualification/hq1024_candidate_coefficients.json "
            f"(max derived |delta|={max(json_deltas.values(), default=0.0):.3e})",
            flush=True,
        )

        run(
            PYTHON,
            "-B",
            QUALIFICATION / "generate_qualified_prefix.py",
            "--rust-output",
            qualified_prefix_rust,
            "--json-output",
            qualified_prefix_report,
        )
        require_identical(
            qualified_prefix_rust,
            CRATE_ROOT / "src" / "qualified_prefix_coefficients.rs",
        )
        require_identical(
            qualified_prefix_report,
            QUALIFICATION / "qualified_prefix_report.json",
        )

        run(
            PYTHON,
            "-B",
            QUALIFICATION / "verify_qualified_prefix.py",
            "--output",
            qualified_prefix_verification,
        )
        require_identical(
            qualified_prefix_verification,
            QUALIFICATION / "qualified_prefix_verification.json",
        )

        run(
            PYTHON,
            "-B",
            QUALIFICATION / "verify_certified_search.py",
            "--output",
            certified_report,
        )
        certified_delta = require_json_numeric_close(
            certified_report,
            QUALIFICATION / "certified_search_report.json",
        )
        print(
            "portable-close: qualification/certified_search_report.json "
            f"(max diagnostic |delta|={certified_delta:.3e})",
            flush=True,
        )

        # This generator only reads the frozen HQ Rust bank. Its Fraction-based
        # dyadic checks and scalar nextafter accumulation do not regenerate the
        # SciPy design, so its output is still required to be exact.
        run(
            PYTHON,
            "-B",
            QUALIFICATION / "generate_fast_metadata.py",
            "--output",
            fast_metadata,
        )
        require_identical(fast_metadata, QUALIFICATION / "fast066_metadata.json")

        run(
            PYTHON,
            "-B",
            QUALIFICATION / "generate_raw_screen_metadata.py",
            "--crate",
            CRATE_ROOT,
            "--output",
            raw_screen_metadata,
        )
        require_identical(raw_screen_metadata, QUALIFICATION / "raw_screen_metadata.json")
        run(
            PYTHON,
            "-B",
            QUALIFICATION / "test_raw_screen.py",
            "--crate",
            CRATE_ROOT,
            "--metadata",
            raw_screen_metadata,
        )

        run(
            PYTHON,
            "-B",
            QUALIFICATION / "verify_fast_scan.py",
            "--output",
            fast_verification,
        )
        require_identical(fast_verification, QUALIFICATION / "fast066_verification.json")

        # Exercise the commissioning driver's pure manifest/stop-go helpers.
        # This does not claim Rust execution; the actual A/B/C/D run remains an
        # operator commissioning step.
        run(PYTHON, "-B", QUALIFICATION / "test_commission_fast066_v2.py")

    print("all offline source-round qualification checks passed", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
