#!/usr/bin/env python3
"""Unit tests for commissioning orchestration that do not execute Rust."""
from __future__ import annotations

import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest import mock

MODULE_PATH = Path(__file__).with_name("commission_fast066_v2.py")
CRATE_ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("commission_fast066_v2", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
commission = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(commission)


class CommissionDecisionTests(unittest.TestCase):
    def test_stop_go_threshold_is_strict(self) -> None:
        self.assertTrue(commission.c_allows_d(0.659, 60.0))
        self.assertFalse(commission.c_allows_d(0.660, 60.0))
        self.assertFalse(commission.c_allows_d(0.661, 60.0))

    def test_stop_go_normalizes_programme_duration(self) -> None:
        self.assertTrue(commission.c_allows_d(0.329, 30.0))
        self.assertFalse(commission.c_allows_d(0.330, 30.0))

    def test_benchmark_json_parser_uses_result_line(self) -> None:
        parsed = commission.parse_benchmark_json(
            "compiler chatter\n"
            '{"total_wall_seconds":0.25,"programme_seconds":60.0,"certificate":{}}\n'
        )
        self.assertEqual(parsed["total_wall_seconds"], 0.25)

    def test_benchmark_identity_is_tied_to_requested_input_and_revision(self) -> None:
        benchmark = {
            "tier": "Fast",
            "fast_algorithm_revision": "Fast066V2",
            "fast_configuration": "nomination-only",
            "bytes_read": 480,
            "frames": 60,
            "sample_rate_hz": 1,
            "channels": 1,
            "binding_fast_wall_gate": False,
            "build": {"fast_stage_timing_feature": True},
        }
        commission.validate_benchmark_identity(
            benchmark,
            carrier_bytes=480,
            sample_rate_hz=1,
            channels=1,
            configuration="fast-nominate",
            instrumented=True,
            expected_algorithm_revision="Fast066V2",
        )
        wrong = dict(benchmark)
        wrong["fast_algorithm_revision"] = "Fast066V1"
        with self.assertRaises(RuntimeError):
            commission.validate_benchmark_identity(
                wrong,
                carrier_bytes=480,
                sample_rate_hz=1,
                channels=1,
                configuration="fast-nominate",
                instrumented=True,
                expected_algorithm_revision="Fast066V2",
            )

    def test_main_emits_abcd_records_against_one_carrier_digest(self) -> None:
        with tempfile.TemporaryDirectory(prefix="fast066-commission-test-") as raw_tmp:
            tmp = Path(raw_tmp)
            v1_archive = tmp / "v1.tar.gz"
            v1_archive.write_bytes(b"synthetic pinned archive for orchestration test")
            required_digest = hashlib.sha256(v1_archive.read_bytes()).hexdigest()

            # Sixty one-channel f64 frames at 1 Hz make a 60-second programme
            # without a large fixture. Benchmark execution itself is mocked;
            # this test owns driver orchestration/provenance, not Rust timing.
            carrier = tmp / "carrier.f64le"
            carrier.write_bytes(b"\0" * (60 * 8))
            carrier_digest = hashlib.sha256(carrier.read_bytes()).hexdigest()
            output = tmp / "manifest.json"

            v1_crate = tmp / "v1-crate"
            v1_crate.mkdir()
            (v1_crate / "Cargo.toml").write_text(
                '[package]\nname="synthetic-v1"\nversion="0.0.0"\n'
                '[profile.release]\ncodegen-units=1\n',
                encoding="utf-8",
            )

            calls: list[dict[str, object]] = []

            def fake_run_benchmark(**kwargs: object) -> dict[str, object]:
                calls.append(dict(kwargs))
                label = str(kwargs["label"])
                binding = label == "D-binding"
                elapsed = {"A": 0.50, "B": 0.30, "C": 0.40,
                           "D-instrumented": 0.55, "D-binding": 0.50}[label]
                return {
                    "label": label,
                    "configuration_label": label.split("-", 1)[0],
                    "benchmark_configuration": kwargs["configuration"],
                    "instrumented": bool(kwargs["instrumented"]),
                    "binding": binding,
                    "edge_policy": commission.EDGE_POLICY,
                    "carrier_sha256": kwargs["carrier_sha256"],
                    "algorithm_revision": "Fast066V1" if label == "A" else "Fast066V2",
                    "seconds_per_programme_minute": elapsed,
                    "active_simd_backend": "scalar",
                    "benchmark": {
                        "total_wall_seconds": elapsed,
                        "programme_seconds": 60.0,
                        "point_dbtp": -0.1,
                        "fast_wall_target_met": True,
                        "binding_fast_wall_gate": binding,
                        "certificate": {},
                    },
                }

            compiler = {
                "rustc_vV": "rustc synthetic",
                "cargo_version": "cargo synthetic",
                "cargo_profile": "release",
                "operator_rustflags": "-Cllvm-args=-vectorize-loops=false",
                "operator_encoded_rustflags": "",
                "cargo_codegen_environment": {},
                "v1_release_profile": {"codegen-units": 1},
                "v2_release_profile": {"codegen-units": 1},
            }

            with (
                mock.patch.object(commission, "REQUIRED_V1_ARCHIVE_SHA256", required_digest),
                mock.patch.object(commission, "extract_verified_v1", return_value=v1_crate),
                mock.patch.object(commission, "compiler_metadata", return_value=compiler),
                mock.patch.object(commission, "run_benchmark", side_effect=fake_run_benchmark),
            ):
                status = commission.main([
                    "--v1-archive", str(v1_archive),
                    "--carrier", str(carrier),
                    "--sample-rate", "1",
                    "--channels", "1",
                    "--v2-crate", str(CRATE_ROOT),
                    "--expected-point-dbtp", "-0.1",
                    "--output", str(output),
                ])

            self.assertEqual(status, 0)
            manifest = json.loads(output.read_text(encoding="utf-8"))
            records = manifest["records"]
            self.assertEqual(
                [record["label"] for record in records],
                ["A", "B", "C", "D-instrumented", "D-binding"],
            )
            self.assertEqual(
                [record["configuration_label"] for record in records],
                ["A", "B", "C", "D", "D"],
            )
            self.assertEqual(
                [bool(record["instrumented"]) for record in records],
                [True, True, True, True, False],
            )
            self.assertEqual(
                {record["carrier_sha256"] for record in records},
                {carrier_digest},
            )
            self.assertTrue(manifest["stop_go"]["run_D"])
            self.assertTrue(manifest["release_gate"]["ready"])
            self.assertEqual(len(calls), 5)


if __name__ == "__main__":
    unittest.main()
