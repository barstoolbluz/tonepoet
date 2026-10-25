#!/usr/bin/env python3
"""Executable regressions for IP-01 trace analysis (not a Rust/PCM qualification)."""
import json
import math
import tempfile
import unittest
from pathlib import Path
from trace_summary import summarize, NA, main


def event(run, phase, at, **values):
    return json.dumps(dict(event='cue_ip01', cue_schema='tonepoet.cue.ip01.v1',
                           cue_run_id=run, cue_phase=phase, cue_monotonic_us=at, **values))


def native(run=None, phase='replaygain_measure_finished', **values):
    fields = dict(event=phase, **values)
    if run is not None:
        fields['cue_run_id'] = run
    return json.dumps(fields)


class TraceSummaryTests(unittest.TestCase):
    def test_interleaved_runs_reordered_lines_and_error_after_admission(self):
        a, b = 'a' * 24, 'b' * 24
        lines = [
            event(b, 'work_finished', 2500, group='cue-stream:0002-0002', elapsed_ms=1.2),
            event(a, 'report_terminal', 8000, terminal_status='completed', album_published=True),
            event(a, 'work_started', 3000, group='cue-stream:0001-0001'),
            event(b, 'replaygain_policy', 1500, requested='Off'),
            event(a, 'replaygain_policy', 1100, requested='Both'),
            event(b, 'report_terminal', 5000, terminal_status='failed', album_published=False),
            event(a, 'queue_admitted', 1000, group='cue-stream:0001-0001'),
            event(b, 'queue_admitted', 1200, group='cue-stream:0002-0002'),
            event(b, 'work_started', 1800, group='cue-stream:0002-0002'),
            event(a, 'replaygain_application_path', 7000, path='source_pass'),
            event(a, 'replaygain_application_settled', 7050, path='source_pass',
                  succeeded=True, tag_action='applied'),
            event(a, 'source_pass_observer_settled', 6000, elapsed_ms=3.7),
            event(a, 'route_selected', 3500, group='cue-stream:0001-0001',
                  route='s1', route_reason='direct_stream_admitted'),
            event(b, 'route_selected', 2000, group='cue-stream:0002-0002',
                  route='fallback', route_reason='non_stream_input'),
            event(a, 'work_finished', 6500, group='cue-stream:0001-0001', elapsed_ms=3.5),
        ]
        rows, errors = summarize(lines)
        self.assertEqual(errors, [])
        self.assertEqual(len(rows), 2)
        by_id = {row['cue_run_id']: row for row in rows}
        self.assertEqual(by_id[a]['rg_requested'], 'Both')
        self.assertEqual(by_id[b]['rg_requested'], 'Off')
        self.assertIs(by_id[a]['source_pass_applied'], True)
        self.assertEqual(by_id[b]['source_pass_applied'], NA)
        self.assertEqual(by_id[a]['queue_to_start_ms'], 2)
        self.assertEqual(by_id[b]['queue_to_start_ms'], 0.6)
        self.assertEqual(by_id[a]['queue_to_report_terminal_ms'], 7)
        self.assertEqual(by_id[b]['terminal_status'], 'failed')
        self.assertIs(by_id[b]['album_published'], False)
        self.assertEqual(by_id[a]['action_to_visible_wall_ms'], NA)
        self.assertEqual(by_id[a]['native_replaygain_measure_ms'], NA)

    def test_failed_source_pass_write_never_claims_application(self):
        run = 'a' * 24
        lines = [
            event(run, 'replaygain_application_path', 100, path='source_pass'),
            event(run, 'source_pass_replaygain_result', 110, source_pass_observed=True),
            event(run, 'replaygain_application_settled', 200, path='source_pass',
                  succeeded=False, tag_action='failed'),
            event(run, 'report_terminal', 300, terminal_status='failed', album_published=False),
        ]
        rows, errors = summarize(lines)
        self.assertEqual(errors, [])
        self.assertIs(rows[0]['source_pass_selected'], True)
        self.assertIs(rows[0]['source_pass_applied'], False)
        self.assertEqual(rows[0]['replaygain_tag_action'], 'failed')
        self.assertIn('tag application failed', rows[0]['warning'])
        # A selection marker, even with a successful source observation, is
        # insufficient evidence of a successful output metadata mutation.
        partial, errors = summarize([lines[0], lines[1], lines[3]])
        self.assertEqual(errors, [])
        self.assertEqual(partial[0]['source_pass_applied'], NA)
        self.assertIn('not settled', partial[0]['warning'])

    def test_skipped_tags_are_not_called_applied(self):
        run = 'a' * 24
        rows, errors = summarize([
            event(run, 'replaygain_application_path', 1, path='source_pass'),
            event(run, 'replaygain_application_settled', 2,
                  path='source_pass', succeeded=True, tag_action='skipped'),
            event(run, 'report_terminal', 3, terminal_status='completed', album_published=True),
        ])
        self.assertFalse(errors)
        self.assertIs(rows[0]['source_pass_applied'], False)

    def test_native_scan_joins_by_token_only_and_requires_both_boundaries(self):
        run = 'f' * 24
        lines = [
            event(run, 'post_output_replaygain_worker_settled', 1100,
                  elapsed_ms=49.2, worker_succeeded=True),
            native(run, 'replaygain_measure_started'),
            native(run, 'replaygain_measure_finished', elapsed_ms=43.1),
            event(run, 'replaygain_application_path', 900, path='post_output_candidate'),
            event(run, 'replaygain_application_settled', 1200,
                  path='post_output_candidate', succeeded=True, tag_action='applied'),
        ]
        rows, errs = summarize(lines)
        self.assertEqual(errs, [])
        self.assertEqual(rows[0]['native_replaygain_measure_ms'], 43.1)
        self.assertEqual(rows[0]['post_output_rg_worker_ms'], 49.2)
        partial, _ = summarize([line for line in lines if 'measure_started' not in line])
        self.assertEqual(partial[0]['native_replaygain_measure_ms'], NA)
        self.assertIn('incomplete', partial[0]['warning'])

    def test_a_foreign_scan_is_never_bound_to_a_cue_run_even_with_one_run_pair(self):
        run = 'f' * 24
        lines = [
            event(run, 'post_output_replaygain_worker_settled', 1100, elapsed_ms=49.2),
            native(None, 'replaygain_measure_finished', item_id='unrelated', elapsed_ms=10),
            native(None, 'replaygain_measure_started', item_id='unrelated'),
            json.dumps(dict(event='run_started', item_id='unrelated')),
            json.dumps(dict(event='run_finished', item_id='unrelated')),
        ]
        rows, errs = summarize(lines)
        self.assertEqual(errs, [])
        self.assertEqual(rows[0]['native_replaygain_measure_ms'], NA)
        self.assertIn('uncorrelated', rows[0]['warning'])

    def test_interleaved_two_cue_runs_do_not_cross_attribute_native_scan(self):
        a, b = 'a' * 24, 'b' * 24
        rows, errors = summarize([
            event(b, 'post_output_replaygain_worker_settled', 100, elapsed_ms=30),
            native(a, 'replaygain_measure_started'),
            event(a, 'post_output_replaygain_worker_settled', 200, elapsed_ms=45),
            native(b, 'replaygain_measure_started'),
            native(b, 'replaygain_measure_finished', elapsed_ms=25),
            native(a, 'replaygain_measure_finished', elapsed_ms=37),
            native(None, 'replaygain_measure_finished', elapsed_ms=700),
        ])
        self.assertEqual(errors, [])
        found = {row['cue_run_id']: row for row in rows}
        self.assertEqual(found[a]['native_replaygain_measure_ms'], 37)
        self.assertEqual(found[b]['native_replaygain_measure_ms'], 25)

    def test_source_pass_recovery_is_one_group_with_two_route_attempts(self):
        run = 'a' * 24
        group = 'cue-stream:0002-0004'
        rows, errors = summarize([
            event(run, 'route_selected', 1000, group=group, route='s1',
                  route_reason='direct_stream_admitted'),
            event(run, 's1_transport_settled', 1800, group=group, elapsed_ms=10),
            event(run, 'source_pass_observer_recovery_retry', 1900),
            event(run, 'route_selected', 2000, group=group, route='s1',
                  route_reason='direct_stream_admitted'),
            event(run, 's1_transport_settled', 3000, group=group, elapsed_ms=11),
        ])
        self.assertEqual(errors, [])
        row = rows[0]
        self.assertEqual(row['physical_group_count'], 1)
        self.assertEqual(row['route_attempt_count'], 2)
        self.assertEqual(row['source_pass_observer_recovery_retries'], 1)
        self.assertEqual(row['route'], 's1')
        self.assertEqual(row['s1_transport_wall_ms'], 21)
        self.assertNotIn('multiple physical', row['warning'])
        self.assertNotIn('unexplained', row['warning'])

    def test_early_observer_failure_recovers_mixed_already_and_never_routed_groups(self):
        run = 'c' * 24
        group_a = 'cue-stream:0001-0002'
        group_b = 'cue-stream:0008-0009'
        rows, errors = summarize([
            event(run, 'route_selected', 100, group=group_a, route='s1'),
            event(run, 'source_pass_observer_recovery_retry', 200),
            event(run, 'route_selected', 300, group=group_a, route='s1'),
            event(run, 'route_selected', 400, group=group_b, route='s1'),
        ])
        self.assertEqual(errors, [])
        row = rows[0]
        self.assertEqual(row['physical_group_count'], 2)
        self.assertEqual(row['route_attempt_count'], 3)
        self.assertEqual(row['source_pass_observer_recovery_retries'], 1)
        self.assertNotIn('unexplained repeated', row['warning'])

    def test_unexplained_repeat_attempt_and_actual_two_groups_warn_differently(self):
        run = 'a' * 24
        first = 'cue-stream:0002-0004'
        second = 'cue-stream:0008-0009'
        rows, _ = summarize([
            event(run, 'route_selected', 100, group=first, route='s1'),
            event(run, 'route_selected', 200, group=first, route='s1'),
            event(run, 'route_selected', 300, group=second, route='fallback'),
        ])
        self.assertEqual(rows[0]['physical_group_count'], 2)
        self.assertIn('unexplained repeated', rows[0]['warning'])
        self.assertIn('multiple physical', rows[0]['warning'])

    def test_queue_deferred_then_admitted_metrics_are_not_mislabeled(self):
        run = 'f' * 24
        group = 'cue-stream:0005-0009'
        rows, errors = summarize([
            event(run, 'queue_deferred', 1000, group=group, reason='queue_full'),
            event(run, 'queue_deferred', 1700, group=group, reason='queue_busy'),
            event(run, 'queue_admitted', 3000, group=group),
            event(run, 'work_started', 4100, group=group),
            event(run, 'report_terminal', 9000, terminal_status='completed', album_published=True),
        ])
        self.assertFalse(errors)
        self.assertEqual(rows[0]['queue_deferred_reasons'], 'queue_busy|queue_full')
        self.assertEqual(rows[0]['first_deferral_to_admission_ms'], 2)
        self.assertEqual(rows[0]['queue_to_start_ms'], 1.1)

    def test_cancel_before_admission_does_not_invent_queue_duration(self):
        run = 'f' * 24
        rows, errors = summarize([
            event(run, 'queue_deferred', 1000, group='cue-stream:0001-0001', reason='queue_full'),
            event(run, 'report_terminal', 1500, terminal_status='cancelled', album_published=False),
        ])
        self.assertFalse(errors)
        self.assertEqual(rows[0]['first_deferral_to_admission_ms'], NA)
        self.assertEqual(rows[0]['queue_to_start_ms'], NA)
        self.assertIn('cancelled before successful', rows[0]['warning'])

    def test_candidate_is_not_scan_and_schema_violations_fail_closed(self):
        run = 'f' * 24
        rows, errors = summarize([event(run, 'replaygain_application_path', 1000,
                                        path='post_output_candidate')])
        self.assertEqual(errors, [])
        self.assertFalse(rows[0]['post_output_worker_observed'])
        self.assertEqual(rows[0]['post_output_rg_worker_ms'], NA)
        self.assertIn('not proven', rows[0]['warning'])
        _, errors = summarize(['{not json}', json.dumps(dict(event='cue_ip01'))])
        self.assertEqual(len(errors), 2)

    def test_negative_and_nonfinite_durations_are_rejected(self):
        run = 'f' * 24
        for bad in (-12, float('nan'), float('inf'), -math.inf, True):
            with self.subTest(bad=bad):
                _, errors = summarize([native(run, elapsed_ms=bad)])
                self.assertEqual(len(errors), 1)
                self.assertIn('invalid native scan duration', errors[0])
                _, errors = summarize([event(run, 'work_finished', 20,
                                              group='cue-stream:0001-0001', elapsed_ms=bad)])
                self.assertEqual(len(errors), 1)
                self.assertIn('invalid CUE elapsed_ms', errors[0])
        _, errors = summarize([event(run, 'work_started', True)])
        self.assertEqual(len(errors), 1)

    def test_replaygain_child_times_not_summed_into_wall(self):
        run = 'f' * 24
        rows, errors = summarize([
            event(run, 's1_transport_settled', 20000, elapsed_ms=120),
            event(run, 's1_child_lifetimes', 21000, decoder_elapsed_ms=110,
                  encoders=[dict(track_ordinal=0, elapsed_ms=70),
                            dict(track_ordinal=1, elapsed_ms=70)]),
        ])
        self.assertFalse(errors)
        self.assertEqual(rows[0]['s1_transport_wall_ms'], 120)
        self.assertEqual(rows[0]['group_worker_inclusive_ms'], NA)

    def test_cli_rejects_bad_native_duration_instead_of_writing_csv(self):
        run = 'f' * 24
        with tempfile.TemporaryDirectory() as folder:
            source, target = Path(folder) / 'trace.jsonl', Path(folder) / 'out.csv'
            source.write_text(event(run, 'work_started', 1) + '\n' +
                              native(run, elapsed_ms=-12) + '\n')
            self.assertEqual(main([str(source), '--output', str(target)]), 2)
            self.assertFalse(target.exists())


if __name__ == '__main__':
    unittest.main()
