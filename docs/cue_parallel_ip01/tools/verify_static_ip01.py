#!/usr/bin/env python3
"""Compile-free IP-01 source invariants; does not qualify Rust behavior."""
import argparse
import hashlib
import json
import pathlib
import sys
import tarfile

CHANGED_CODE = {
    'src/convert/pipeline/baseline.rs',
    'src/convert/pipeline/stages.rs',
    'src/convert/processor.rs',
}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source', type=pathlib.Path, required=True)
    parser.add_argument('--input-snapshot', type=pathlib.Path, required=True)
    parser.add_argument('--plan', type=pathlib.Path, required=True)
    args = parser.parse_args()
    expected = '0740a9c8cbd8e59648981207daf62e6373f4b544ad3a40c7872b3890c9d76d26'
    sha = hashlib.sha256(args.input_snapshot.read_bytes()).hexdigest()
    assert sha == expected, f'unexpected source input hash: {sha}'
    with tarfile.open(args.input_snapshot, 'r:gz') as tar:
        original = {}
        for member in tar:
            if member.isfile():
                key = member.name.removeprefix('tonepoet/')
                original[key] = hashlib.sha256(tar.extractfile(member).read()).hexdigest()
    changed = []
    for key, baseline_hash in original.items():
        file = args.source / key
        assert file.is_file(), f'baseline file missing: {key}'
        actual = hashlib.sha256(file.read_bytes()).hexdigest()
        if actual != baseline_hash:
            changed.append(key)
    assert set(changed) == CHANGED_CODE, f'unexpected original-file mutation: {changed}'
    print(f'PASS snapshot identity: {expected}')
    print(f'PASS original files intact except these 3 Rust modules: {sorted(changed)}')
    # Amended IP-06 authority was included verbatim. No separate S3/observer packet landed.
    amendment = (args.plan / '08_HOST_GATE_AMENDMENT.md').read_text()
    assert 'ReplayGain' in amendment and 'deferred' in amendment and 'PX-C' in amendment
    plan_copy = args.source / 'docs/cue_parallel_ip01/planning_authority'
    for file in args.plan.rglob('*'):
        if file.is_file():
            rel = file.relative_to(args.plan)
            assert (plan_copy / rel).read_bytes() == file.read_bytes(), f'planning drift: {rel}'
    print('PASS amended planning authority byte-identical including IP-06 decision')
    base = (args.source / 'src/convert/pipeline/baseline.rs').read_text()
    stage = (args.source / 'src/convert/pipeline/stages.rs').read_text()
    queue = (args.source / 'src/convert/processor.rs').read_text()
    for k in ['TONEPOET_PIPELINE_BASELINE_JSONL', 'cue_event_value',
              'cue_timestamp_us', 'cue_ip01_tests', 'cue_run_id',
              'if event != "cue_ip01"']:
        assert k in base, f'absent privacy/timing gate {k}'
    for k in ['queue_admitted', 'work_started', 'work_finished', 'cue_timestamp_us']:
        assert k in queue, f'absent scheduler timing {k}'
    for k in ['source_pass_replaygain_result', 'source_pass_track_observer_settled',
              'replaygain_application_path', 'post_output_replaygain_worker_settled',
              's1_child_lifetimes', 's1_transport_settled', 'route_execution_refused',
              'report_terminal', 'album_published', 'cue_ip01_replaygain_off_both_and_ineligible_source']:
        assert k in stage, f'absent route/replaygain/terminal evidence {k}'
    assert stage.count('cue_ip01_route(&req, &tracks, "fallback",') == 8
    assert stage.count('cue_ip01_route(&req, &tracks, "s1",') == 1
    assert 'observer_storage_limit' in stage and 'observe_cue_mirror(' in stage
    # Audit D-01 through D-06 are source-local changes; these string anchors
    # are NOT a Rust compile, semantic proof or native parity qualification.
    assert 'replaygain_application_settled' in stage
    assert 'cue_ip01_replaygain_tag_action' in stage
    assert 'source_pass_observer_recovery_retry' in stage
    assert 'cue_ip01_group_id' in stage
    assert '"track_ordinal": source_ordinal' in stage
    assert 'with_cue_native_replaygain_correlation(job_id, measure)' in stage
    assert 'CUE_NATIVE_REPLAYGAIN_RUN' in base
    assert '"queue_deferred"' in queue
    assert '"queue_busy"' in queue and '"queue_full"' in queue
    assert 'cue_ip01_real_enabled_disabled_sink_and_concurrent_identity' in base
    assert 'cue_ip01_real_queue_deferred_event_before_admission' in queue
    assert 'cue_ip01_real_s1_and_fallback_diagnostic_routes_on_isolated_sink' in stage
    analyzer = (args.source / 'docs/cue_parallel_ip01/tools/trace_summary.py').read_text()
    for marker in ['replaygain_application_settled', 'source_pass_selected',
                   'first_deferral_to_admission_ms', 'route_attempt_count',
                   'scan_starts[run]', "not duration(obj.get('elapsed_ms'))"]:
        assert marker in analyzer, f'missing corrective analysis assertion: {marker}'
    print('PASS IP-01 R2 corrective source anchors (D-01..D-06)')
    print('PASS instrumentation anchors, prelaunch route reason inventory and native observer reuse')
    # No source file other than the three intended modules changed; build and
    # runtime tests MUST be executed separately on the qualified host.
    print('QUALIFICATION NOT RUN: cargo fmt, cargo tests, native runtime/benchmarks')

if __name__ == '__main__':
    try:
        main()
    except (AssertionError, OSError, tarfile.TarError) as exc:
        print(f'FAIL: {exc}', file=sys.stderr)
        sys.exit(1)
