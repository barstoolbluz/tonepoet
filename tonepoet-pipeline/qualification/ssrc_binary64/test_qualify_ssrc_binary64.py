#!/usr/bin/env python3
import importlib.util
import struct
import unittest
from pathlib import Path

MODULE_PATH = Path(__file__).with_name("qualify_ssrc_binary64.py")
spec = importlib.util.spec_from_file_location("qualify_ssrc_binary64", MODULE_PATH)
assert spec and spec.loader
qualification = importlib.util.module_from_spec(spec)
spec.loader.exec_module(qualification)


class QualificationGateTests(unittest.TestCase):
    def test_static_source_audit_requires_explicit_sleef_type_path_completion(self):
        audit = {
            "source_review": {
                "passed": True,
                "sleef_dft_type_path_audit": {"passed": False},
            }
        }
        self.assertFalse(qualification.static_source_audit_complete(audit))
        audit["source_review"]["sleef_dft_type_path_audit"]["passed"] = True
        self.assertTrue(qualification.static_source_audit_complete(audit))

    def test_otherwise_complete_evidence_without_tonepoet_physical_cell_stays_pending(self):
        results = [{"passed": True}]
        self.assertFalse(
            qualification.qualification_is_promotable([], True, True, True, results)
        )

    def test_all_prerequisites_including_tonepoet_physical_cell_can_be_review_candidate(self):
        results = [
            {
                "passed": True,
                "tonepoet_physical_cell": {"passed": True},
            }
        ]
        self.assertTrue(
            qualification.qualification_is_promotable([], True, True, True, results)
        )
        self.assertFalse(
            qualification.qualification_is_promotable(
                ["registry update intentionally separate"], True, True, True, results
            )
        )

    def test_characterization_argv_binds_declared_zero_db_attenuation(self):
        cell = qualification.characterization_cell("high", 96000, 44100, 2)
        argv = qualification.ssrc_argv(
            Path("/qualified/ssrc"), cell, Path("in.wav"), Path("out.w64")
        )
        att_index = argv.index("--att")
        self.assertEqual(argv[att_index + 1], "0.0")
        self.assertEqual(
            qualification.render_attenuation_db(cell["attenuation_db"]),
            "0.0",
        )

    def test_settled_dc_gain_rejects_gross_hidden_normalization(self):
        levels = []
        for level in (1.5, -1.5, 2.0, -2.0):
            levels.extend([level] * qualification.DC_PLATEAU_INPUT_FRAMES)
        exact_payload = b"".join(struct.pack("<d", value) for value in levels)
        scaled_payload = b"".join(struct.pack("<d", value * 0.8) for value in levels)
        exact = qualification.settled_dc_gain_check(exact_payload, 1, 48000, 48000, 0.0)
        scaled = qualification.settled_dc_gain_check(scaled_payload, 1, 48000, 48000, 0.0)
        self.assertTrue(exact["passed"])
        self.assertFalse(scaled["passed"])


    @staticmethod
    def _minimal_float64_w64(fact_body: bytes | None) -> bytes:
        rate = 48_000
        channels = 1
        payload = struct.pack("<dd", 0.25, -0.25)

        def chunk(guid: bytes, body: bytes) -> bytes:
            size = 24 + len(body)
            encoded = guid + struct.pack("<Q", size) + body
            return encoded + b"\0" * ((-len(encoded)) & 7)

        fmt = struct.pack(
            "<HHIIHH",
            3,
            channels,
            rate,
            rate * channels * 8,
            channels * 8,
            64,
        )
        chunks = [chunk(qualification.W64_FMT_GUID, fmt)]
        if fact_body is not None:
            chunks.append(chunk(qualification.W64_FACT_GUID, fact_body))
        chunks.append(chunk(qualification.W64_DATA_GUID, payload))
        body = b"".join(chunks)
        total = 40 + len(body)
        return (
            qualification.W64_RIFF_GUID
            + struct.pack("<Q", total)
            + qualification.W64_WAVE_GUID
            + body
        )

    def test_wave64_fact_uses_exact_u64_frame_count(self):
        import tempfile

        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "good.w64"
            path.write_bytes(self._minimal_float64_w64(struct.pack("<Q", 2)))
            parsed = qualification.parse_w64(path, 48_000, 1)
            self.assertEqual(parsed["sample_frames"], 2)

            riff_style = Path(directory) / "u32-fact.w64"
            riff_style.write_bytes(self._minimal_float64_w64(struct.pack("<I", 2)))
            with self.assertRaisesRegex(ValueError, "invalid/duplicate Wave64 fact chunk"):
                qualification.parse_w64(riff_style, 48_000, 1)

            missing = Path(directory) / "missing-fact.w64"
            missing.write_bytes(self._minimal_float64_w64(None))
            with self.assertRaisesRegex(ValueError, "fact chunk does not match"):
                qualification.parse_w64(missing, 48_000, 1)

    def test_promotion_requires_audit_identity_match_not_only_pass_booleans(self):
        audit = {
            "source_identity": {
                "owner": "barstoolbluz",
                "repo": "ssrc",
                "rev": qualification.SSRC_REV,
                "nar_hash": qualification.SSRC_NAR,
            },
            "dependency_identity": {"sleef_rev": qualification.SLEEF_REV},
            "build_audit": {
                "passed": True,
                "build_closure": "closure-a",
                "compiler_configuration": "compiler-a",
                "executable_sha256": "exe-a",
            },
        }
        matched, mismatches = qualification.audit_identity_consistency(
            audit, "closure-a", "compiler-a", "exe-a"
        )
        self.assertTrue(matched)
        self.assertEqual(mismatches, [])
        incomplete, incomplete_reasons = qualification.audit_identity_consistency(
            audit, None, None, None
        )
        self.assertFalse(incomplete)
        self.assertTrue(any("incomplete" in reason for reason in incomplete_reasons))
        mismatched, reasons = qualification.audit_identity_consistency(
            audit, "closure-b", "compiler-a", "exe-a"
        )
        self.assertFalse(mismatched)
        self.assertTrue(any("closure" in reason for reason in reasons))
        results = [{"passed": True, "tonepoet_physical_cell": {"passed": True}}]
        self.assertFalse(
            qualification.qualification_is_promotable([], True, True, False, results)
        )


if __name__ == "__main__":
    unittest.main()
