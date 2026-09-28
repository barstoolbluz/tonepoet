#!/usr/bin/env python3
from __future__ import annotations

import hashlib
import importlib.util
import json
import struct
import sys
import tempfile
import unittest
from unittest import mock
from pathlib import Path

HERE = Path(__file__).resolve().parent


def load(name: str, filename: str):
    spec = importlib.util.spec_from_file_location(name, HERE / filename)
    module = importlib.util.module_from_spec(spec)
    assert spec and spec.loader
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


q = load("ssrc_terminal_qualify", "qualify_ssrc_true_peak_terminal.py")
p = load("ssrc_terminal_promote", "promote_ssrc_true_peak_terminal.py")


def w64_chunk(guid: bytes, body: bytes) -> bytes:
    out = guid + struct.pack("<Q", 24 + len(body)) + body
    return out + b"\0" * ((-len(out)) % 8)


def make_w64(rate: int, channels: int, bits: int, floating: bool, payload: bytes) -> bytes:
    tag = 3 if floating else 1
    align = channels * (bits // 8)
    fmt = struct.pack("<HHIIHH", tag, channels, rate, rate * align, align, bits)
    body = q.W64_WAVE_GUID + w64_chunk(q.W64_FMT_GUID, fmt) + w64_chunk(q.W64_DATA_GUID, payload)
    size = 24 + len(body)
    return q.W64_RIFF_GUID + struct.pack("<Q", size) + body


class QualificationUnitTests(unittest.TestCase):
    def test_cell_parser_is_fail_closed(self):
        cell = q.parse_cell("high:176400:88200:2:24:2:triangular")
        self.assertEqual(cell.bits, 24)
        self.assertEqual(cell.dither_id, 2)
        with self.assertRaises(ValueError):
            q.parse_cell("standard:176400:88200:2:24:2:triangular")
        int32 = q.parse_cell("high:176400:88200:2:32:99:triangular")
        self.assertEqual(int32.bits, 32)
        self.assertEqual(int32.dither_id, 99)
        with self.assertRaises(ValueError):
            q.parse_cell("high:176400:88200:2:24:none:triangular")
        f32 = q.parse_cell("high:176400:88200:2:-32:none:none")
        self.assertTrue(f32.is_float())
        self.assertEqual(f32.target_bit_depth(), "Float32")
        f64 = q.parse_cell("high:176400:88200:2:-64:none:none")
        self.assertEqual(f64.target_bit_depth(), "Float64")
        with self.assertRaises(ValueError):
            q.parse_cell("high:176400:88200:2:-32:2:triangular")

    def test_db_nano_rejects_subnanodecibel_values(self):
        self.assertEqual(q.db_nano("-6.000000001"), -6_000_000_001)
        with self.assertRaises(ValueError):
            q.db_nano("0.0000000001")

    def test_wave64_integer_and_float_decoders(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            fp = root / "f.w64"
            fp.write_bytes(make_w64(48_000, 1, 64, True, struct.pack("<dd", 0.25, -0.5)))
            parsed = q.parse_w64(fp, 48_000, 1)
            self.assertEqual(q.decode_float64(parsed), [0.25, -0.5])

            fp32 = root / "f32.w64"
            fp32.write_bytes(make_w64(48_000, 1, 32, True, struct.pack("<ff", 0.25, -0.5)))
            parsed = q.parse_w64(fp32, 48_000, 1)
            self.assertEqual(q.decode_float_pcm(parsed, 32), [0.25, -0.5])

            i24 = root / "i24.w64"
            # +0.5 and -0.5 in signed 24-bit normalized PCM.
            payload = bytes((0x00, 0x00, 0x40, 0x00, 0x00, 0xC0))
            i24.write_bytes(make_w64(48_000, 1, 24, False, payload))
            parsed = q.parse_w64(i24, 48_000, 1)
            self.assertEqual(q.decode_integer_normalized(parsed, 24), [0.5, -0.5])

    def test_bound_policy_is_monotone_and_reserves_eight_lsb(self):
        self.assertEqual(q.derive_bound_lsb(0.0), 8)
        self.assertEqual(q.derive_bound_lsb(1.75), 8)
        self.assertGreaterEqual(q.derive_bound_lsb(10.0), 24)
        self.assertLessEqual(q.derive_bound_lsb(9.0), q.derive_bound_lsb(10.0))

    def test_float_bound_policy_is_monotone_and_positive(self):
        for bits in (32, 64):
            first = q.derive_bound_linear(0.0, bits)
            second = q.derive_bound_linear(first, bits)
            self.assertGreater(first, 0.0)
            self.assertGreaterEqual(second, first)
            self.assertEqual(q.f64_bits(first), p.f64_bits(first))

    def test_float_terminal_command_uses_native_bits_and_no_dither_switches(self):
        cell = q.parse_cell("high:96000:44100:2:-32:none:none")
        argv = q.terminal_argv(Path("/ssrc"), cell, "1,0;0,1", Path("in.wav"), Path("out.w64"))
        self.assertIn("-32", argv)
        self.assertNotIn("--dither", argv)
        self.assertNotIn("--seed", argv)
        self.assertNotIn("--pdf", argv)

    def test_production_grid_includes_execution_qualified_int32_dither(self):
        report = {
            "ssrc_scope": {
                "profiles": ["high"],
                "rate_scope": [[176400, 88200]],
            }
        }
        cells = q.production_cells(report, [2], {88_200: (0, 1, 2, 99)})
        self.assertTrue(cells)
        self.assertTrue(any(c.bits == 32 and c.dither_id == 99 for c in cells))
        self.assertTrue(any(c.bits == 32 and c.dither_id == 2 for c in cells))
        self.assertTrue(any(c.bits == 24 and c.dither_id == 2 for c in cells))
        floats = [c for c in cells if c.bits in {-32, -64}]
        self.assertEqual({c.bits for c in floats}, {-32, -64})
        self.assertTrue(all(c.dither_id is None and c.pdf is None for c in floats))

    def test_fixture_is_periodic_low_band_and_channel_discriminating(self):
        frames = q.fixture_frames(2, 1.0)
        self.assertEqual(len(frames), 16_384)
        peak = max(abs(value) for frame in frames for value in frame)
        self.assertLessEqual(peak, 0.300000000000001)
        positive_gain = 10.0 ** (24.0 / 20.0)
        boosted_fixture = q.fixture_frames(2, positive_gain)
        boosted_peak = max(abs(value) for frame in boosted_fixture for value in frame)
        self.assertLessEqual(boosted_peak * positive_gain, 0.300000000000001)
        # A full-period integer-bin multitone has no source-Nyquist component.
        for channel in range(2):
            nyquist = sum(
                ((-1.0) ** index) * frame[channel]
                for index, frame in enumerate(frames)
            ) / len(frames)
            self.assertLess(abs(nyquist), 1e-12)
        self.assertNotEqual(
            [frame[0] for frame in frames[:128]],
            [frame[1] for frame in frames[:128]],
        )
        # Highest fixture tone remains below the narrowest destination Nyquist
        # ratio in the established 384 -> 44.1 kHz rate envelope.
        self.assertLess(509 / 16_384, 0.5 * 44_100 / 384_000)

    def test_production_grid_omits_dither_cells_the_executable_does_not_expose(self):
        report = {
            "ssrc_scope": {
                "profiles": ["high"],
                "rate_scope": [[192000, 176400]],
            }
        }
        cells = q.production_cells(report, [1], {176_400: ()})
        self.assertTrue(cells)
        self.assertTrue(all(cell.dither_id is None for cell in cells))
        self.assertEqual({cell.bits for cell in cells}, {8, 16, 24, 32, -32, -64})

    def test_global_grid_uses_only_discovered_reachable_dither_cells(self):
        capabilities = {192_000: (0, 1, 2, 98, 99)}
        self.assertEqual(
            q.global_dither_cells_for_rate(192_000, capabilities),
            ((None, None), (99, "triangular"), (0, "triangular"), (2, "triangular")),
        )

    def test_capability_probe_treats_only_rate_unavailable_as_unsupported(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)

            def fake_invoke(argv):
                dither_id = int(argv[argv.index("--dither") + 1])
                rate = int(argv[argv.index("--rate") + 1])
                if dither_id == 0:
                    Path(argv[-1]).write_bytes(b"probe")
                    return q.subprocess.CompletedProcess(argv, 0, "", "")
                return q.subprocess.CompletedProcess(
                    argv,
                    255,
                    "",
                    f"Dither type {dither_id} is not available for destination sampling frequency {rate}Hz",
                )

            with mock.patch.object(q, "invoke", side_effect=fake_invoke), mock.patch.object(
                q, "parse_w64", return_value={"frames": 1}
            ):
                found = q.discover_ssrc_dither_capabilities(Path("/ssrc"), [176_400], root)
            self.assertEqual(found, {176_400: (0,)})

            def bad_invoke(argv):
                return q.subprocess.CompletedProcess(argv, 7, "", "unexpected failure")

            with mock.patch.object(q, "invoke", side_effect=bad_invoke):
                with self.assertRaises(RuntimeError):
                    q.discover_ssrc_dither_capabilities(Path("/ssrc"), [176_400], root / "bad")

    def test_wave64_accepts_only_ssrc_final_two_zero_byte_pad(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            rate = 192_000
            channels = 1
            bits = 16
            payload = struct.pack("<hhhhh", 1, -2, 3, -4, 5)
            align = channels * (bits // 8)
            fmt = struct.pack("<HHIIHH", 1, channels, rate, rate * align, align, bits)
            fmt_chunk = w64_chunk(q.W64_FMT_GUID, fmt)
            data_chunk = q.W64_DATA_GUID + struct.pack("<Q", 24 + len(payload)) + payload
            body = q.W64_WAVE_GUID + fmt_chunk + data_chunk + b"\0\0"
            bytes_ = q.W64_RIFF_GUID + struct.pack("<Q", 24 + len(body)) + body
            path = root / "ssrc-pad.w64"
            path.write_bytes(bytes_)
            parsed = q.parse_w64(path, rate, channels)
            self.assertEqual(parsed["payload"], payload)
            self.assertEqual(parsed["frames"], 5)
            self.assertEqual(parsed["ssrc_trailing_padding_bytes"], 2)

            mutated = bytearray(bytes_)
            mutated[-1] = 1
            path.write_bytes(mutated)
            with self.assertRaises(ValueError):
                q.parse_w64(path, rate, channels)



class PromotionUnitTests(unittest.TestCase):
    def synthetic_report(self, path: Path, source_root: Path) -> Path:
        authority = {}
        for relative in p.TONEPOET_AUTHORITY_FILES:
            authority_path = source_root / relative
            authority_path.parent.mkdir(parents=True, exist_ok=True)
            authority_path.write_text(f"authority:{relative}\n")
            authority[relative] = hashlib.sha256(authority_path.read_bytes()).hexdigest()
        report = {
            "schema_version": 1,
            "contract_id": p.EXPECTED_CONTRACT,
            "gain_model_id": p.EXPECTED_GAIN_MODEL,
            "dither_seed": p.EXPECTED_SEED,
            "tonepoet_source_authority": {"files": authority},
            "ssrc_identity": {
                "sha256": "a" * 64,
                "source_revision": p.EXPECTED_SOURCE_REV,
                "build_identity": "/nix/store/example-ssrc.drv",
            },
            "platform": {"os": "Linux", "architecture": "x86_64"},
            "commissioning": {"decision": "candidate_for_review"},
            "cells": [{
                "cell_key": "high:176400:88200:2:24:2:triangular",
                "scope": {
                    "profile": "high", "source_rate_hz": 176400,
                    "target_rate_hz": 88200, "channels": 2,
                    "target_bit_depth": "Int24", "dither_id": 2,
                    "pdf_type": "triangular", "base_attenuation_db": "0.0",
                    "min_phase": False, "architecture": "x86_64",
                },
                "gain_points": [{
                    "gain_db_nano": -24_000_000_000,
                    "maximum_abs_error_lsb": 1.5,
                    "deterministic_repeat": True,
                    "no_saturation_premise": True,
                    "passed": True,
                }, {
                    "gain_db_nano": 24_000_000_000,
                    "maximum_abs_error_lsb": 1.75,
                    "deterministic_repeat": True,
                    "no_saturation_premise": True,
                    "passed": True,
                }],
                "minimum_gain_db_nano": -24_000_000_000,
                "maximum_gain_db_nano": 24_000_000_000,
                "observed_maximum_abs_error_lsb": 1.75,
                "bound_policy": p.EXPECTED_INTEGER_BOUND_POLICY,
                "stored_sample_error_lsb": 8,
                "stored_sample_error_lsb_nano": 8_000_000_000,
                "passed": True,
            }],
            "summary": {"cell_count": 1, "passed_cells": 1, "failed_cells": 0},
        }
        path.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
        digest = hashlib.sha256(path.read_bytes()).hexdigest()
        identity = path.with_name(path.name + ".identity.json")
        identity.write_text(json.dumps({
            "evidence_id": f"sha256:{digest}",
            "qualification_report_sha256": digest,
        }))
        return identity

    def test_promotion_is_identity_checked_and_deterministic(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            source_root = root / "source"
            report_path = root / "report.json"
            identity_path = self.synthetic_report(report_path, source_root)
            report, digest = p.load_and_validate(report_path, identity_path, source_root)
            first = p.generate(report, digest)
            second = p.generate(report, digest)
            self.assertEqual(first, second)
            self.assertIn("SsrcProfile::High", first)
            self.assertIn("PcmBitDepth::Int24", first)
            self.assertIn("Some(SsrcPdfType::Triangular)", first)
            self.assertIn(digest, first)

            report_path.write_text(report_path.read_text() + " ")
            with self.assertRaises(ValueError):
                p.load_and_validate(report_path, identity_path, source_root)

    def test_promotion_accepts_execution_qualified_int32_dither_cell(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            source_root = root / "source"
            report_path = root / "report.json"
            identity_path = self.synthetic_report(report_path, source_root)
            report = json.loads(report_path.read_text())
            cell = report["cells"][0]
            cell["cell_key"] = "high:176400:88200:2:32:2:triangular"
            cell["scope"]["target_bit_depth"] = "Int32"
            report_path.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
            digest = hashlib.sha256(report_path.read_bytes()).hexdigest()
            identity_path.write_text(json.dumps({
                "evidence_id": f"sha256:{digest}",
                "qualification_report_sha256": digest,
            }))

            validated, validated_digest = p.load_and_validate(
                report_path, identity_path, source_root
            )
            generated = p.generate(validated, validated_digest)
            self.assertIn("PcmBitDepth::Int32", generated)
            self.assertIn("dither_id: Some(2)", generated)

    def test_promotion_accepts_float_cell_and_emits_absolute_linear_bound(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            source_root = root / "source"
            report_path = root / "report.json"
            identity_path = self.synthetic_report(report_path, source_root)
            report = json.loads(report_path.read_text())
            cell = report["cells"][0]
            cell["cell_key"] = "high:176400:88200:2:-32:none:none"
            cell["scope"]["target_bit_depth"] = "Float32"
            cell["scope"]["dither_id"] = None
            cell["scope"]["pdf_type"] = None
            observed = 1.25e-7
            for point in cell["gain_points"]:
                point.pop("maximum_abs_error_lsb")
                point["maximum_abs_error_linear"] = observed
            bound = p.derive_bound_linear(observed, 32)
            cell.pop("observed_maximum_abs_error_lsb")
            cell.pop("stored_sample_error_lsb")
            cell.pop("stored_sample_error_lsb_nano")
            cell["observed_maximum_abs_error_linear"] = observed
            cell["bound_policy"] = p.EXPECTED_FLOAT_BOUND_POLICY
            cell["stored_sample_error_linear"] = bound
            cell["stored_sample_error_linear_bits"] = p.f64_bits(bound)
            report_path.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
            digest = hashlib.sha256(report_path.read_bytes()).hexdigest()
            identity_path.write_text(json.dumps({
                "evidence_id": f"sha256:{digest}",
                "qualification_report_sha256": digest,
            }))

            validated, validated_digest = p.load_and_validate(
                report_path, identity_path, source_root
            )
            generated = p.generate(validated, validated_digest)
            self.assertIn("PcmBitDepth::Float32", generated)
            self.assertIn("dither_id: None", generated)
            self.assertIn("pdf_type: None", generated)
            self.assertIn("SsrcTruePeakStoredSampleErrorBound::AbsoluteLinearFsBits", generated)

    def test_promotion_rejects_float_cell_with_dither(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            source_root = root / "source"
            report_path = root / "report.json"
            identity_path = self.synthetic_report(report_path, source_root)
            report = json.loads(report_path.read_text())
            cell = report["cells"][0]
            cell["cell_key"] = "high:176400:88200:2:-32:2:triangular"
            cell["scope"]["target_bit_depth"] = "Float32"
            report_path.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
            digest = hashlib.sha256(report_path.read_bytes()).hexdigest()
            identity_path.write_text(json.dumps({
                "evidence_id": f"sha256:{digest}",
                "qualification_report_sha256": digest,
            }))
            with self.assertRaisesRegex(ValueError, "disable dither"):
                p.load_and_validate(report_path, identity_path, source_root)

    def test_promotion_rejects_inconsistent_bound_and_scope(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            source_root = root / "source"
            report_path = root / "report.json"
            identity_path = self.synthetic_report(report_path, source_root)
            report = json.loads(report_path.read_text())
            report["cells"][0]["stored_sample_error_lsb"] = 9
            report_path.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
            digest = hashlib.sha256(report_path.read_bytes()).hexdigest()
            identity_path.write_text(json.dumps({
                "evidence_id": f"sha256:{digest}",
                "qualification_report_sha256": digest,
            }))
            with self.assertRaisesRegex(ValueError, "bound"):
                p.load_and_validate(report_path, identity_path, source_root)

            report["cells"][0]["stored_sample_error_lsb"] = 8
            report["cells"][0]["scope"]["architecture"] = "aarch64"
            report_path.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
            digest = hashlib.sha256(report_path.read_bytes()).hexdigest()
            identity_path.write_text(json.dumps({
                "evidence_id": f"sha256:{digest}",
                "qualification_report_sha256": digest,
            }))
            with self.assertRaisesRegex(ValueError, "architecture"):
                p.load_and_validate(report_path, identity_path, source_root)


if __name__ == "__main__":
    unittest.main()
