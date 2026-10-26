#!/usr/bin/env python3
"""Recalculate what the submitted IP-06 CSV *actually* supports.

Does not authenticate fixtures, configuration, binary, source or the clock.
Does not claim to replay any TonePoet conversion.
"""
import csv
import json
import statistics as stats
from collections import Counter, defaultdict
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
INPUT = ROOT / 'ip06_host_measurement' / 'RESULTS.csv'
OUTPUT = Path(__file__).with_name('SUBMITTED_CSV_RECALCULATION.json')


def audit(path):
    with path.open(newline='', encoding='utf-8') as f:
        reader = csv.DictReader(f)
        expected = ['cell', 'rep', 'route', 'rg', 'wall_s', 'rc', 'tracks']
        if reader.fieldnames != expected:
            raise ValueError(f'not the supplied schema: {reader.fieldnames!r}')
        rows = list(reader)
    cells = defaultdict(dict)
    for line, row in enumerate(rows, 2):
        if row['route'] not in ('s1', 's3') or row['rg'] not in ('off', 'both'):
            raise ValueError(f'line {line}: invalid route or mode')
        rep = int(row['rep'])
        wall = float(row['wall_s'])
        if not (1 <= rep <= 5 and 0 < wall < 1000000):
            raise ValueError(f'line {line}: invalid repetition/wall')
        if int(row['rc']) != 0 or int(row['tracks']) < 1:
            raise ValueError(f'line {line}: unsuccessful run or invalid track count')
        group = cells[(row['cell'], row['rg'])]
        key = (rep, row['route'])
        if key in group:
            raise ValueError(f'line {line}: duplicate cohort/rep/route')
        group[key] = wall
    if len(rows) != 140 or len(cells) != 14:
        raise ValueError(f'expected 140 rows and 14 cohorts; got {len(rows)}, {len(cells)}')
    computed = []
    for (cell, rg), runs in sorted(cells.items()):
        if set(runs) != {(rep, route) for rep in range(1, 6) for route in ('s1', 's3')}:
            raise ValueError(f'incomplete paired cohort {(cell, rg)!r}')
        a = [runs[rep, 's1'] for rep in range(1, 6)]
        b = [runs[rep, 's3'] for rep in range(1, 6)]
        computed.append({
            'cell': cell, 'replaygain_mode': rg, 'n_pairs': 5,
            's1_median_seconds': stats.median(a),
            's3_median_seconds': stats.median(b),
            'ratio_of_route_medians': stats.median(a) / stats.median(b),
            'median_of_paired_ratios': stats.median(x / y for x, y in zip(a, b)),
            'min_paired_ratio': min(x / y for x, y in zip(a, b)),
            'max_paired_ratio': max(x / y for x, y in zip(a, b)),
        })
    # The rows contain only seven fields. Do not infer source/config identity,
    # samples, publication, peak resources, or the route actually selected.
    return {
        'source': 'ip06_host_measurement/RESULTS.csv (user-supplied, not host-attested)',
        'columns': expected,
        'rows': len(rows), 'cohorts': len(cells), 'runs_per_cohort': 10,
        'reported_exit_codes': dict(Counter(row['rc'] for row in rows)),
        'timing_claims_independently_recalculated': computed,
        'unavailable_fields': ['source_sha256', 'cue_sha256', 'fixture_sha256',
                               'configuration_sha256', 'tool_versions_per_run',
                               'build_sha256', 'admission_to_visible_publication',
                               'actual_route', 'audio_hashes', 'tag_sets',
                               'destination_file_sets', 'peak_RSS', 'peak_FD',
                               'peak_temp', 'simultaneous_children',
                               'source_meter_intervals', 'cache_cohort'],
    }


if __name__ == '__main__':
    result = audit(INPUT)
    OUTPUT.write_text(json.dumps(result, indent=2, sort_keys=True) + '\n', encoding='utf-8')
    print(f'PASS: {result["rows"]} rows; {result["cohorts"]} complete Off/Both x workload cohorts; 5 pairs each; 140 reported rc=0')
    for c in result['timing_claims_independently_recalculated']:
        print(f'{c["cell"]:16s} {c["replaygain_mode"]:4s} '
              f'{c["ratio_of_route_medians"]:.3f}x medians '
              f'({c["median_of_paired_ratios"]:.3f}x paired median)')
