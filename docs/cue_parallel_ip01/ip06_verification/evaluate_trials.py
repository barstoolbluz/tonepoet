#!/usr/bin/env python3
"""IP-06 H-01 corrected evaluator for matched complete-conversion trials.

Numeric candidate screening only: complete application parity, resource and
publication gates require separate source-bound proof. Historical planning
bundle is immutable, so this corrected evaluator supersedes only its evaluator.
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
PAIR_FIELDS = ('host', 'storage', 'cache_state', 'tracks', 'replaygain',
               'source_sha256', 'fixture_sha256', 'pair_id')
HEX_SHA256 = re.compile(r'[0-9a-f]{64}\Z')


def percentile(items, q):
    values = sorted(items)
    at = (len(values) - 1) * q
    lo = math.floor(at)
    hi = math.ceil(at)
    return values[lo] * (hi - at) + values[hi] * (at - lo) if lo != hi else values[lo]


def evaluate(path):
    pairs = defaultdict(dict)
    digest_mode = {}
    with path.open(newline='', encoding='utf-8') as f:
        reader = csv.DictReader(f)
        if not set(FIELDS).issubset(reader.fieldnames or []):
            raise ValueError(f'expected CSV headers: {FIELDS}')
        for line, row in enumerate(reader, 2):
            for field in ('source_sha256', 'fixture_sha256', 'configuration_sha256'):
                if not HEX_SHA256.fullmatch(row[field] or ''):
                    raise ValueError(f'line {line}: {field} must be a lowercase SHA-256')
            if any(not (row[field] or '').strip() for field in PAIR_FIELDS):
                raise ValueError(f'line {line}: missing pair identity field')
            mode = row['replaygain'].strip().lower()
            if mode not in ('off', 'track', 'album', 'both'):
                raise ValueError(f'line {line}: unsupported ReplayGain mode {mode!r}')
            # H-01: the canonical configuration must include ReplayGain mode.
            digest = row['configuration_sha256']
            prior = digest_mode.setdefault(digest, mode)
            if prior != mode:
                raise ValueError(
                    f'line {line}: configuration_sha256 reused across ReplayGain '
                    f'modes ({prior} and {mode}); include ReplayGain in the manifest')
            route = row['route']
            if route not in ('S1', 'S3'):
                raise ValueError(f'line {line}: route must be S1/S3')
            try:
                wall = float(row['wall_seconds'])
            except (ValueError, TypeError):
                raise ValueError(f'line {line}: wall_seconds not a number') from None
            if not math.isfinite(wall) or wall <= 0:
                raise ValueError(f'line {line}: wall_seconds must be finite and positive')
            row['replaygain'] = mode
            key = tuple(row[field] for field in PAIR_FIELDS)
            if route in pairs[key]:
                raise ValueError(f'line {line}: duplicate route within matched pair')
            pairs[key][route] = (wall, digest)

    if not pairs:
        raise ValueError("empty trial evidence")

    cohorts = defaultdict(dict)
    for key, routes in pairs.items():
        # The pairing key deliberately excludes the digest: two halves of one
        # pair with different hashes must FAIL, not become two partial cohorts.
        if set(routes) == {'S1', 'S3'}:
            (s1, a), (s3, b) = routes['S1'], routes['S3']
            if a != b:
                raise ValueError(
                    f'pair {key[-1]!r}: route configuration SHA-256 mismatch')
            digest = a
            result = (s1, s3)
        else:
            digest = next(iter(routes.values()))[1]
            result = None
        cohorts[key[:-1] + (digest,)][key[-1]] = result

    lines = []
    for key, by_pair in sorted(cohorts.items()):
        complete = [v for v in by_pair.values() if v is not None]
        if len(complete) < 5 or len(complete) != len(by_pair):
            lines.append(f'{key}: INSUFFICIENT paired trials '
                         f'({len(complete)}/{len(by_pair)} complete; need >=5)')
            continue
        s1, s3 = zip(*complete)
        ratios = [a / b for a, b in complete]
        rng = random.Random(20261022)
        bootstrap = [stats.median([ratios[rng.randrange(len(ratios))]
                                   for _ in ratios]) for _ in range(3000)]
        med = stats.median(ratios)
        ci = (percentile(bootstrap, .025), percentile(bootstrap, .975))
        gate = 'CANDIDATE' if med >= 1.25 and ci[0] > 1.0 else 'NO AUTOMATIC PROMOTION'
        lines.append(f'{key}: n={len(complete)} med_S1={stats.median(s1):.3f}s '
                     f'med_S3={stats.median(s3):.3f}s paired_med_speedup={med:.3f}x '
                     f'bootstrap95=({ci[0]:.3f},{ci[1]:.3f}) {gate}; '
                     'CORRECTNESS/FAILURE/RESOURCE GATES STILL REQUIRED')
    return lines


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('csv_path', type=Path)
    args = parser.parse_args()
    try:
        lines = evaluate(args.csv_path)
    except (ValueError, OSError) as error:
        parser.exit(2, f'INVALID TRIAL EVIDENCE: {error}\n')
    print('\n'.join(lines))


if __name__ == '__main__':
    main()
