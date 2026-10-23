#!/usr/bin/env python3
"""Evaluate user-measured, paired, complete-conversion wall-clock CSV records.

This is not a benchmark runner. An external stopwatch/TonePoet opt-in trace must
supply honest action-to-visible measurements and conformance test verdicts.
"""
import argparse
import csv
import math
import random
import re
import statistics as stats
from collections import defaultdict
from pathlib import Path

FIELDS = ('pair_id', 'host', 'storage', 'cache_state', 'tracks', 'replaygain',
          'route', 'wall_seconds', 'source_sha256', 'fixture_sha256',
          'configuration_sha256')


def percentile(items, q):
    values = sorted(items)
    at = (len(values)-1)*q
    lo = math.floor(at)
    hi = math.ceil(at)
    return values[lo] * (hi-at) + values[hi] * (at-lo) if lo != hi else values[lo]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('csv_path', type=Path, help='real, complete and paired S1/S3 conversion times')
    args = ap.parse_args()
    cohorts = defaultdict(dict)
    with args.csv_path.open(newline='') as f:
        reader = csv.DictReader(f)
        if not set(FIELDS).issubset(reader.fieldnames or []):
            raise SystemExit(f'expected CSV headers: {FIELDS}')
        for row in reader:
            key = tuple(row[x] for x in ('host','storage','cache_state','tracks','replaygain',
                                          'source_sha256','fixture_sha256','configuration_sha256'))
            if not re.fullmatch(r'[0-9a-f]{64}', row['configuration_sha256']):
                raise SystemExit('configuration_sha256 must be a lowercase 64-character SHA-256 digest of an identical canonical build/tool/settings manifest, excluding route')
            route = row['route']
            if route not in ('S1', 'S3') or not row['pair_id']:
                raise SystemExit('route must be S1/S3 and pair_id nonempty')
            try:
                wall = float(row['wall_seconds'])
            except ValueError:
                raise SystemExit('wall_seconds not a number')
            if not math.isfinite(wall) or wall <= 0:
                raise SystemExit('wall_seconds must be finite and positive')
            if route in cohorts[key].setdefault(row['pair_id'], {}):
                raise SystemExit('duplicate route in a matched pair')
            cohorts[key][row['pair_id']][route] = wall
    for key, by_pair in sorted(cohorts.items()):
        pairs = [(v['S1'], v['S3']) for v in by_pair.values() if set(v) == {'S1','S3'}]
        if len(pairs) < 5 or len(pairs) != len(by_pair):
            print(f'{key}: INSUFFICIENT paired trials ({len(pairs)}/{len(by_pair)} complete; need >=5)')
            continue
        s1 = [x[0] for x in pairs]
        s3 = [x[1] for x in pairs]
        ratios = [a/b for a,b in pairs]
        rng = random.Random(20261022)
        bootstrap = [stats.median([ratios[rng.randrange(len(ratios))]
                                   for _ in ratios]) for _ in range(3000)]
        med = stats.median(ratios)
        ci = (percentile(bootstrap, .025), percentile(bootstrap, .975))
        # Numeric screens alone never constitute an acceptance decision.
        gate = 'CANDIDATE' if med >= 1.25 and ci[0] > 1.0 else 'NO AUTOMATIC PROMOTION'
        print(f'{key}: n={len(pairs)} med_S1={stats.median(s1):.3f}s '
              f'med_S3={stats.median(s3):.3f}s paired_med_speedup={med:.3f}x '
              f'bootstrap95=({ci[0]:.3f},{ci[1]:.3f}) {gate}; '
              'CORRECTNESS/FAILURE/RESOURCE GATES STILL REQUIRED')


if __name__ == '__main__':
    main()
