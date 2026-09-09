#!/usr/bin/env python3
"""Run every source-round offline qualifier without mutating checked-in outputs.

This is intentionally not a release qualifier: Cargo tests, shipping-codegen
regressions, and commissioning benchmarks remain operator owned. The runner
makes the source-only numerical derivations reproducible and idempotent.
"""
from __future__ import annotations

import pathlib
import subprocess
import sys
import tempfile


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

        run(
            PYTHON,
            "-B",
            QUALIFICATION / "generate_hq1024.py",
            "--rust-output",
            generated_rust,
            "--json-output",
            generated_json,
        )
        require_identical(generated_rust, CRATE_ROOT / "src" / "hq1024_coefficients.rs")
        require_identical(
            generated_json,
            QUALIFICATION / "hq1024_candidate_coefficients.json",
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
        require_identical(certified_report, QUALIFICATION / "certified_search_report.json")

    print("all offline source-round qualification checks passed", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
