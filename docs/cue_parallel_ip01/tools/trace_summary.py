#!/usr/bin/env python3
"""Offline IP-01 CUE JSONL summarizer. Never infer missing identities/outcomes.

All values are observations, not additive attribution of decoder/encoder CPU
usage. Native scan times require an explicit CUE run correlation token, emitted
inside the existing replaygain_measure scope by the post-output CUE worker.
"""
import argparse
import csv
import json
import math
import sys
from collections import defaultdict
from pathlib import Path

NA = 'NOT INSTRUMENTED'
FIELDS = [
    'cue_run_id', 'route', 'route_reason', 'physical_group_count',
    'route_attempt_count', 'source_pass_observer_recovery_retries',
    'rg_requested', 'source_pass_selected', 'source_pass_applied',
    'replaygain_tag_action', 'post_output_worker_observed',
    'post_output_rg_worker_ms', 'native_replaygain_measure_ms',
    'source_pass_observer_envelope_ms', 'queue_deferred_reasons',
    'first_deferral_to_admission_ms', 'queue_to_start_ms',
    'group_worker_inclusive_ms', 's1_transport_wall_ms',
    'terminal_status', 'album_published', 'queue_to_report_terminal_ms',
    'action_to_visible_wall_ms', 'warning',
]


def duration(value):
    """Time is an actual finite, nonnegative number; bool isn't a duration."""
    return type(value) in (float, int) and math.isfinite(value) and value >= 0


def metric(value):
    return round(value, 3) if duration(value) else NA


def elapsed_total(events, warnings, phase):
    if not events:
        return NA
    values = [event.get('elapsed_ms') for event in events]
    if any(not duration(value) for value in values):
        warnings.append(f'{phase}: missing elapsed_ms; interval not calculated')
        return NA
    return metric(sum(values))


def summarize(lines):
    cue = defaultdict(list)
    scans = defaultdict(list)
    scan_starts = defaultdict(list)
    uncorrelated_scans = 0
    malformed = []
    for line_no, raw in enumerate(lines, 1):
        if not raw.strip():
            continue
        try:
            obj = json.loads(raw)
        except (ValueError, TypeError) as exc:
            malformed.append(f'line {line_no}: invalid JSON: {exc}')
            continue
        if not isinstance(obj, dict):
            malformed.append(f'line {line_no}: event not JSON object')
            continue
        event = obj.get('event')
        if event in ('replaygain_measure_started', 'replaygain_measure_finished'):
            if event == 'replaygain_measure_finished' and not duration(obj.get('elapsed_ms')):
                malformed.append(f'line {line_no}: invalid native scan duration')
                continue
            token = obj.get('cue_run_id')
            if token is None:
                if event == 'replaygain_measure_finished':
                    uncorrelated_scans += 1
                continue
            if not isinstance(token, str) or len(token) != 24:
                malformed.append(f'line {line_no}: malformed native CUE correlation')
                continue
            if event == 'replaygain_measure_finished':
                scans[token].append(obj)
            else:
                scan_starts[token].append(obj)
            continue
        if event != 'cue_ip01':
            continue
        if obj.get('cue_schema') != 'tonepoet.cue.ip01.v1':
            malformed.append(f'line {line_no}: unknown CUE schema')
            continue
        run = obj.get('cue_run_id')
        if not isinstance(run, str) or len(run) != 24:
            malformed.append(f'line {line_no}: invalid run correlation id')
            continue
        at = obj.get('cue_monotonic_us')
        if type(at) is not int or at < 0:
            malformed.append(f'line {line_no}: invalid monotonic timestamp')
            continue
        if 'elapsed_ms' in obj and not duration(obj['elapsed_ms']):
            malformed.append(f'line {line_no}: invalid CUE elapsed_ms')
            continue
        cue[run].append(obj)

    rows = []
    for run, events in sorted(cue.items()):
        events.sort(key=lambda event: event['cue_monotonic_us'])
        phases = defaultdict(list)
        for event in events:
            phases[event.get('cue_phase')].append(event)
        first = lambda name: phases[name][0] if phases[name] else None
        last = lambda name: phases[name][-1] if phases[name] else None
        policy = first('replaygain_policy')
        selected = last('replaygain_application_path')
        settled = last('replaygain_application_settled')
        terminal = last('report_terminal')
        warnings = []

        # Preserve physical group identity across the preexisting one-time
        # source-pass observer recovery. Two S1 routes are two attempts, not
        # two albums/groups, if a matching recovery marker exists.
        by_group = defaultdict(list)
        for index, event in enumerate(phases['route_selected']):
            group = event.get('group')
            if not isinstance(group, str) or not group:
                group = f'UNIDENTIFIED-{index}'
                warnings.append('route missing physical group identity')
            by_group[group].append(event)
        recoveries = phases['source_pass_observer_recovery_retry']
        for group, attempts in by_group.items():
            if len(attempts) <= 1:
                continue
            # An original group may be routed again once after each causal
            # recovery boundary; a group not attempted initially may appear
            # only on the rerun. Never count either as additional groups.
            recovered_attempts = sum(1 for marker in recoveries if
                attempts[0]['cue_monotonic_us'] < marker['cue_monotonic_us']
                <= attempts[-1]['cue_monotonic_us'])
            if len(attempts) - 1 != recovered_attempts:
                warnings.append(f'{group}: unexplained repeated route attempt')
        if len(by_group) > 1:
            warnings.append('multiple physical CUE groups; route is group-specific')
        # Each grouped route represents one physical group. Attempt counts and
        # recovery attribution remain explicit, not smuggled into group count.
        routes = [group_events[-1].get('route', NA) for group_events in by_group.values()]
        reasons = [group_events[-1].get('route_reason', NA) for group_events in by_group.values()]

        queues = {}
        deferred = defaultdict(list)
        for event in phases['queue_admitted']:
            group = event.get('group')
            if group is not None:
                queues[group] = min(queues.get(group, event['cue_monotonic_us']),
                                    event['cue_monotonic_us'])
        for event in phases['queue_deferred']:
            deferred[event.get('group')].append(event)
        waits = []
        for event in phases['work_started']:
            admitted = queues.get(event.get('group'))
            if admitted is not None and event['cue_monotonic_us'] >= admitted:
                waits.append((event['cue_monotonic_us'] - admitted) / 1000)
        deferral_waits = []
        for group, deferrals in deferred.items():
            admitted = queues.get(group)
            if admitted is not None:
                initial = min(event['cue_monotonic_us'] for event in deferrals)
                if admitted >= initial:
                    deferral_waits.append((admitted - initial) / 1000)
            elif terminal and terminal.get('terminal_status') == 'cancelled':
                warnings.append(f'{group}: cancelled before successful queue admission')
        reasons_deferred = sorted({event.get('reason', 'unspecified')
                                   for group_events in deferred.values() for event in group_events})

        post = phases['post_output_replaygain_worker_settled']
        observer = phases['source_pass_observer_settled']
        if not policy:
            warnings.append('RG policy unavailable')
        source_selected = (selected.get('path') == 'source_pass') if selected else NA
        # Selection of a retained scan is not evidence of successful tag writes.
        # A missing settled event is never silently promoted to success.
        applied = NA
        tag_action = NA
        if settled:
            tag_action = settled.get('tag_action', NA)
            if settled.get('succeeded') is True and tag_action == 'applied':
                applied = settled.get('path') == 'source_pass'
            elif settled.get('succeeded') is False or tag_action in ('failed', 'skipped', 'not_applied'):
                applied = False
            else:
                warnings.append('ReplayGain settled event has ambiguous outcome')
            if selected and settled.get('path') != selected.get('path'):
                warnings.append('ReplayGain selected/settled path mismatch')
        elif selected:
            warnings.append('ReplayGain path selected but tag application not settled')
        if post and not settled:
            warnings.append('output worker measured but application outcome missing')
        if selected and selected.get('path') == 'post_output_candidate' and not post:
            warnings.append('post-output scan not proven by candidate marker')
        if settled and settled.get('path') == 'source_pass' and settled.get('succeeded') is False:
            warnings.append('source-pass retained scan tag application failed')
        if terminal and terminal.get('album_published') is not True and applied is True:
            warnings.append('source-pass tags applied to staging; album not published')
        if any(event.get('worker_succeeded') is False for event in post):
            warnings.append('post-output ReplayGain worker failed')

        # A native scan can only be joined with the CUE run by the explicit
        # thread-local correlation token. Legacy event cardinality and item_id
        # matching are NOT valid substitutes, even for a single CUE run.
        native_ms = NA
        if len(scans[run]) == 1 and len(scan_starts[run]) == 1 and len(post) == 1:
            native_ms = metric(scans[run][0]['elapsed_ms'])
        elif scans[run] or scan_starts[run]:
            warnings.append('native scan correlation incomplete/ambiguous')
        elif uncorrelated_scans and post:
            warnings.append('uncorrelated legacy native scan ignored')

        admission = min(queues.values()) if queues else None
        terminal_t = terminal['cue_monotonic_us'] if terminal else None
        rows.append({
            'cue_run_id': run,
            'route': '|'.join(routes) or NA,
            'route_reason': '|'.join(reasons) or NA,
            'physical_group_count': len(by_group) if by_group else NA,
            'route_attempt_count': sum(len(attempts) for attempts in by_group.values()) if by_group else NA,
            'source_pass_observer_recovery_retries': len(recoveries),
            'rg_requested': policy.get('requested', NA) if policy else NA,
            'source_pass_selected': source_selected,
            'source_pass_applied': applied,
            'replaygain_tag_action': tag_action,
            'post_output_worker_observed': bool(post),
            'post_output_rg_worker_ms': elapsed_total(post, warnings, 'post_output_replaygain_worker_settled'),
            'native_replaygain_measure_ms': native_ms,
            'source_pass_observer_envelope_ms': elapsed_total(observer, warnings, 'source_pass_observer_settled'),
            'queue_deferred_reasons': '|'.join(reasons_deferred) if reasons_deferred else NA,
            'first_deferral_to_admission_ms': metric(max(deferral_waits) if deferral_waits else None),
            'queue_to_start_ms': metric(max(waits) if waits else None),
            'group_worker_inclusive_ms': elapsed_total(phases['work_finished'], warnings, 'work_finished'),
            's1_transport_wall_ms': elapsed_total(phases['s1_transport_settled'], warnings, 's1_transport_settled'),
            'terminal_status': terminal.get('terminal_status', NA) if terminal else NA,
            'album_published': terminal.get('album_published', NA) if terminal else NA,
            'queue_to_report_terminal_ms': metric((terminal_t - admission) / 1000
                if admission is not None and terminal_t is not None and terminal_t >= admission else None),
            'action_to_visible_wall_ms': NA,
            'warning': '; '.join(warnings),
        })
    return rows, malformed


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('jsonl', type=Path)
    parser.add_argument('--output', type=Path, default=None)
    args = parser.parse_args(argv)
    with args.jsonl.open('r', encoding='utf-8') as file:
        rows, errors = summarize(file)
    if errors:
        for error in errors:
            print(error, file=sys.stderr)
        return 2
    if not rows:
        print('No valid cue_ip01 events; no inferred performance records', file=sys.stderr)
        return 3
    if args.output:
        with args.output.open('w', newline='', encoding='utf-8') as stream:
            writer = csv.DictWriter(stream, FIELDS)
            writer.writeheader()
            writer.writerows(rows)
    else:
        writer = csv.DictWriter(sys.stdout, FIELDS)
        writer.writeheader()
        writer.writerows(rows)
    return 0

if __name__ == '__main__':
    sys.exit(main())
