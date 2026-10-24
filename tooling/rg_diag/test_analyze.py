#!/usr/bin/env python3
"""Offline negative/positive tests for evidence attribution and host parsing."""

import json
from pathlib import Path
import tempfile
import unittest

import analyze

RUN_ID = "a" * 24


def transport(ordinal=1, bytes_=80_000, complete=True):
    return {
        "event": "cue_ip01", "cue_schema": "tonepoet.cue.ip01.v1",
        "cue_run_id": RUN_ID, "cue_phase": "rg_diag_transport",
        "rg_diag_schema": "tonepoet.cue.rgdiag.v1", "track_ordinal": ordinal,
        "complete": complete, "pcm_bytes": bytes_, "chunks": 2,
        "elapsed_wall_ns": 200_000_000,
        "decoder_read_call_ns": 10_000_000,
        "encoder_write_call_ns": 30_000_000,
        "mirror_write_call_ns": 40_000_000,
        "fifo_open_call_ns": 10_000_000,
        "header_write_call_ns": 5_000_000,
    }


def observer(ordinal=1, bytes_=80_000, complete=True):
    return {
        "event": "cue_ip01", "cue_schema": "tonepoet.cue.ip01.v1",
        "cue_run_id": RUN_ID, "cue_phase": "rg_diag_observer",
        "rg_diag_schema": "tonepoet.cue.rgdiag.v1", "track_ordinal": ordinal,
        "complete": complete, "pcm_bytes": bytes_, "blocks": 1,
        "fifo_open_call_ns": 1, "header_read_call_ns": 2,
        "pcm_read_call_ns": 3, "pcm_decode_call_ns": 4,
        "loudness_push_call_ns": 5, "reporting_peak_push_call_ns": 6,
        "loudness_finalize_call_ns": 7, "reporting_peak_finalize_call_ns": 8,
        "eof_wait_call_ns": 9,
    }


def manifest(ordinals=None):
    ordinals = [1] if ordinals is None else ordinals
    return {
        "event": "cue_ip01", "cue_schema": "tonepoet.cue.ip01.v1",
        "cue_run_id": RUN_ID, "cue_phase": "rg_diag_manifest",
        "rg_diag_schema": "tonepoet.cue.rgdiag.v1",
        "track_count": len(ordinals), "track_ordinals": ordinals,
    }


def policy(observed=True):
    return {
        "event": "cue_ip01", "cue_schema": "tonepoet.cue.ip01.v1",
        "cue_run_id": RUN_ID, "cue_phase": "source_pass_replaygain_result",
        "source_pass_observed": observed,
    }


class AnalyzeTests(unittest.TestCase):
    def classify(self, rows, add_manifest=True):
        result = analyze.summary_by_run(([manifest()] if add_manifest else []) + rows)
        self.assertEqual(list(result), [RUN_ID])
        return analyze.classify_run(result[RUN_ID])

    def test_corrected_fifo_readiness_is_validated_and_reported(self):
        fixed = observer()
        fixed.update(fifo_wait_strategy="poll_50ms_plus_no_writer_backoff",
                     readiness_wait_call_ns=10_000_000, readiness_wait_count=4,
                     readiness_timeout_count=2, no_writer_poll_count=1,
                     would_block_poll_count=2, poll_sleep_call_ns=5_000_000)
        status, warnings, pairs = self.classify([transport(), fixed, policy()])
        self.assertEqual((status, warnings), ("PASS", []))
        self.assertEqual(len(pairs), 1)
        invalid = fixed.copy()
        invalid["readiness_timeout_count"] = 5
        with self.assertRaisesRegex(ValueError, "timeouts exceed"):
            analyze.summary_by_run([manifest(), transport(), invalid, policy()])
        missing = fixed.copy()
        del missing["readiness_wait_call_ns"]
        with self.assertRaises(ValueError):
            analyze.summary_by_run([manifest(), transport(), missing, policy()])

    def test_complete_equal_bytes_pass(self):
        status, warnings, pairs = self.classify([transport(), observer(), policy()])
        self.assertEqual(status, "PASS")
        self.assertEqual(warnings, [])
        self.assertEqual(len(pairs), 1)

    def test_byte_mismatch_fails(self):
        status, warnings, _ = self.classify([transport(), observer(bytes_=80_001), policy()])
        self.assertEqual(status, "FAIL")
        self.assertTrue(any("mismatch" in w for w in warnings))

    def test_partial_observer_is_not_success(self):
        status, warnings, _ = self.classify([transport(), observer(complete=False), policy()])
        self.assertEqual(status, "INCOMPLETE")
        self.assertTrue(warnings)

    def test_authoritative_fallback_not_misreported_as_success(self):
        status, warnings, _ = self.classify([transport(), observer(), policy(False)])
        self.assertEqual(status, "FALLBACK")
        self.assertTrue(warnings)

    def test_missing_policy_is_incomplete(self):
        status, warnings, _ = self.classify([transport(), observer()])
        self.assertEqual(status, "INCOMPLETE")
        self.assertTrue(warnings)

    def test_missing_peer_is_not_success(self):
        status, warnings, pairs = self.classify([transport(), policy()])
        self.assertEqual(status, "INCOMPLETE")
        self.assertFalse(pairs)
        self.assertTrue(warnings)

    def test_duplicate_attempt_not_misinterpreted_as_two_tracks(self):
        status, warnings, _ = self.classify([transport(), transport(), observer(), policy()])
        self.assertEqual(status, "AMBIGUOUS")
        self.assertTrue(any("Repeated track ordinal" in w for w in warnings))

    def test_instrumentation_absent_is_not_zero_cost(self):
        status, warnings, pairs = self.classify([policy(False)])
        self.assertEqual(status, "NOT INSTRUMENTED")
        self.assertTrue(warnings)
        self.assertFalse(pairs)

    def test_observer_recovery_retry_is_fallback_not_success(self):
        recovery = {"event": "cue_ip01", "cue_schema": "tonepoet.cue.ip01.v1",
                    "cue_run_id": RUN_ID, "cue_phase": "source_pass_observer_recovery_retry"}
        status, warnings, _ = self.classify([transport(), observer(), recovery])
        self.assertEqual(status, "FALLBACK")
        self.assertTrue(any("recovery retry" in w for w in warnings))

    def test_missing_expected_track_fails_completeness(self):
        status, warnings, pairs = self.classify(
            [manifest([1, 2]), transport(), observer(), policy()], add_manifest=False)
        self.assertEqual(status, "INCOMPLETE")
        self.assertEqual(len(pairs), 1)
        self.assertTrue(any("expected ordinal 2" in w for w in warnings))

    def test_missing_manifest_fails_completeness(self):
        status, warnings, pairs = self.classify(
            [transport(), observer(), policy()], add_manifest=False)
        self.assertEqual(status, "INCOMPLETE")
        self.assertEqual(len(pairs), 1)
        self.assertTrue(any("manifest missing" in w for w in warnings))

    def test_unexpected_ordinal_fails_manifest_binding(self):
        status, warnings, _ = self.classify(
            [manifest([2]), transport(), observer(), policy()], add_manifest=False)
        self.assertEqual(status, "FAIL")
        self.assertTrue(any("Unexpected" in w for w in warnings))

    def test_duplicate_manifest_is_ambiguous(self):
        status, warnings, _ = self.classify(
            [manifest(), manifest(), transport(), observer(), policy()], add_manifest=False)
        self.assertEqual(status, "AMBIGUOUS")
        self.assertTrue(warnings)

    def test_invalid_manifest_rejected(self):
        bad = manifest([1, 1])
        with self.assertRaisesRegex(ValueError, "duplicate"):
            analyze.summary_by_run([bad])

    def test_inconsistent_elapsed_detected(self):
        bad = transport()
        bad["elapsed_wall_ns"] = 1
        with self.assertRaises(ValueError):
            analyze.summary_by_run([bad])

    def test_unterminated_trace_rejected(self):
        with tempfile.TemporaryDirectory() as d:
            p = Path(d) / "trace.jsonl"
            p.write_text(json.dumps(transport()))
            with self.assertRaisesRegex(ValueError, "unterminated"):
                analyze.read_jsonl(p)

    def test_bad_schema_rejected(self):
        bad = transport()
        bad["rg_diag_schema"] = "other"
        with self.assertRaises(ValueError):
            analyze.summary_by_run([bad])

    def test_malformed_and_overlong_jsonl_rejected(self):
        with tempfile.TemporaryDirectory() as d:
            p = Path(d) / "test.jsonl"
            p.write_text('{"a": 1}\n{"bad":}\n')
            with self.assertRaises(ValueError):
                analyze.read_jsonl(p)
            p.write_bytes(b"a" * (analyze.MAX_RECORD_BYTES + 1) + b"\n")
            with self.assertRaises(ValueError):
                analyze.read_jsonl(p)

    def test_concrete_summary_renders_without_user_paths(self):
        with tempfile.TemporaryDirectory() as d:
            run = Path(d)
            (run / "host.json").write_text(json.dumps({
                "schema": "tonepoet.rg_host.v1", "label": "test",
                "elapsed_wall_ns": 1_000_000_000, "returncode": 0,
                "instrumented": True,
                "cpu_sampling": {"aggregate_cpu_lower_bound_s": 0.5},
            }))
            (run / "cue.jsonl").write_text("\n".join(json.dumps(r) for r in [manifest(), transport(), observer(), policy()]) + "\n")
            report = analyze.make_report([run])
            self.assertIn("PASS", report)
            self.assertIn("Loudness call", report)
            self.assertNotIn(str(run), report)


if __name__ == "__main__":
    unittest.main()
