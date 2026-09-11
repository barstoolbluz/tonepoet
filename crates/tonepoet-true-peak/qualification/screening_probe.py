#!/usr/bin/env python3
"""Count conservative raw-screen survivors without implementing the new meter.

Uses canonical 4096-interval tiles, 256-interval roots, 32-interval children,
32-value second-difference summaries, and same-channel *sample-only* lower
witnesses. No future halo sample or precomputed whole-file maximum is a witness.
Actual reconstructed HQ4 witnesses can only improve these rejection counts.
This tool does not measure Rust throughput and does not certify a peak result.
Requires NumPy; input is interleaved little-endian Float64 PCM.
"""
from __future__ import annotations
import argparse
import hashlib
import json
import math
from pathlib import Path
import sys
import numpy as np

TILE, ROOT, CHILD, HALO, BIN = 4096, 256, 32, 777, 32

def up(v: float) -> float:
    return math.nextafter(v, math.inf) if v != 0 else 0.0

def add(a: float, b: float) -> float:
    return up(a + b)

def mul(a: float, b: float) -> float:
    return up(a * b) if a != 0 and b != 0 else 0.0

def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open('rb') as stream:
        for data in iter(lambda: stream.read(4*1024*1024), b''):
            digest.update(data)
    return digest.hexdigest()

def validate_ordinary(values: np.ndarray) -> None:
    if not np.isfinite(values).all():
        raise ValueError('Input contains a nonfinite sample')
    nonzero = np.abs(values[values != 0])
    if len(nonzero) and (nonzero.min() < 1e-200 or nonzero.max() > 1e200):
        raise ValueError('Workload pilot accepts ordinary finite amplitudes only; use Rust exceptional-value tests separately')

def analyze(path: Path, channels: int, rate: int, edge: str, metadata: dict) -> dict:
    size = path.stat().st_size
    if channels < 1 or rate < 1 or size == 0 or size % (channels*8):
        raise ValueError('Input must contain complete nonempty Float64 frames; rate/channels must be positive')
    x = np.memmap(path, mode='r', dtype='<f8').reshape(-1, channels)
    frames = len(x)
    validate_ordinary(np.asarray(x[:1]))
    if edge not in ('repeat', 'zero'):
        raise ValueError('Edge policy must be repeat or zero')
    A = float.fromhex(metadata['A_raw']['upper_hex'])
    B = float.fromhex(metadata['B_raw']['upper_hex'])
    counts = {k: 0 for k in ['channel_tiles','root_groups','raw_rejected_roots','child_groups_tested',
                            'raw_rejected_children','surviving_children','surviving_native_intervals',
                            'unique_requested_half_knots','channel_tiles_with_survivors']}
    limits = [32, 64, 128, 256, 512]
    tile_pair_dense = {str(k): 0 for k in limits}
    lower = np.zeros(channels)
    max_dense_occupancy = 0
    for a in range(0, max(0, frames-1), TILE):
        b = min(a+TILE, frames-1)
        nominal = np.asarray(x[a:b+1])
        validate_ordinary(nominal)
        lower = np.maximum(lower, np.abs(nominal).max(axis=0))
        positions = np.arange(a-HALO, b+HALO+1)
        tile = np.array(x[np.clip(positions,0,frames-1)], copy=True)
        if edge == 'zero':
            tile[(positions < 0) | (positions >= frames)] = 0.0
        validate_ordinary(tile)
        magnitude = np.abs(tile)
        d = np.abs((tile[:-2] - 2.0*tile[1:-1]) + tile[2:])
        d_bins = np.array([d[s:s+BIN].max(axis=0) for s in range(0,len(d),BIN)])
        X = magnitude.max(axis=0)
        requested_by_channel: list[set[int]] = []
        for c in range(channels):
            counts['channel_tiles'] += 1
            requested: set[int] = set()
            unit_roundoff = 2.0**-53
            gu = up(up(8.0 * unit_roundoff) / math.nextafter(1.0-up(8.0*unit_roundoff), 0.0))
            # Conservative ordinary-range numerical error. Absolute term covers
            # tiny intermediate arithmetic; it is irrelevant at accepted scales.
            e_d = add(mul(gu,mul(4.0,float(X[c]))), 128.0*sys.float_info.min)
            for gs in range(a,b,ROOT):
                ge = min(gs+ROOT,b)
                counts['root_groups'] += 1
                si, ei = gs-a+HALO, ge-a+HALO
                S = float(magnitude[si:ei+1,c].max())
                # Delta2 start offsets: [gs-775, ge+773]. Bin widening stays
                # inside the already available tile halo, including partial bins.
                di, dj = gs-a+2, ge-a+1550
                D = add(float(d_bins[di//BIN:dj//BIN+1,c].max()),e_d)
                def bound(s: float) -> float:
                    if X[c] == 0.0:
                        return 0.0
                    return add(add(s,mul(B,s)),mul(A,D))
                if bound(S) <= lower[c]:
                    counts['raw_rejected_roots'] += 1
                    continue
                for cs in range(gs,ge,CHILD):
                    ce = min(cs+CHILD,ge)
                    counts['child_groups_tested'] += 1
                    ss = float(magnitude[cs-a+HALO:ce-a+HALO+1,c].max())
                    # Child inherits the parent's D; no expensive repeated halo scan.
                    if bound(ss) <= lower[c]:
                        counts['raw_rejected_children'] += 1
                        continue
                    counts['surviving_children'] += 1
                    counts['surviving_native_intervals'] += ce-cs
                    # Tail support [2*cs-16,2*ce+16] requires odd coarse knots.
                    # Their native half centers m span [cs-8,ce+7], inclusive.
                    requested.update(range(cs-8,ce+8))
            requested_by_channel.append(requested)
            counts['unique_requested_half_knots'] += len(requested)
            if requested:
                counts['channel_tiles_with_survivors'] += 1
            max_dense_occupancy = max(max_dense_occupancy,len(requested))
        for c in range(0,channels,2):
            union = set().union(*requested_by_channel[c:c+2])
            for limit in limits:
                if len(union) > limit:
                    tile_pair_dense[str(limit)] += 1
    total_intervals = channels*max(0,frames-1)
    pair_tiles = ((max(0,frames-1)+TILE-1)//TILE)*((channels+1)//2)
    return {'input_filename': path.name, 'input_sha256': sha256(path), 'frames': frames,
            'sample_rate_hz': rate, 'channels': channels, 'edge_policy': edge,
            'programme_seconds': frames/rate, 'policy': {'tile_intervals': TILE,'root_intervals': ROOT,
                'child_intervals': CHILD,'summary_bin_starts': BIN,'halo_frames':HALO,
                'screen_threshold':'same-channel sample-only lower witness; no tolerance slack'},
            'counts':counts,'all_native_intervals_across_channels': total_intervals,
            'surviving_interval_fraction': counts['surviving_native_intervals']/max(1,total_intervals),
            'maximum_requested_half_knots_in_one_channel_tile':max_dense_occupancy,
            'pair_tiles':pair_tiles,'dense_pair_tiles_for_candidate_half_knot_thresholds':tile_pair_dense,
            'limitations':['Not a complete peak scanner or a wall-time benchmark.',
                          'No reconstructed lower-witness updates: survivor work is a conservative pilot estimate.',
                          'Dense pair decisions count the union of requested half-knot positions.',
                          'Ordinary finite input range only; exceptional floating-point behavior belongs in Rust tests.']}

def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--input',type=Path,required=True)
    parser.add_argument('--channels',type=int,required=True)
    parser.add_argument('--sample-rate',type=int,required=True)
    parser.add_argument('--edge',choices=['repeat','zero'],default='repeat')
    parser.add_argument('--metadata',type=Path,default=Path(__file__).resolve().parent/'raw_screen_metadata.json')
    parser.add_argument('--output',type=Path,required=True)
    args=parser.parse_args()
    result=analyze(args.input,args.channels,args.sample_rate,args.edge,json.loads(args.metadata.read_text()))
    args.output.parent.mkdir(parents=True,exist_ok=True)
    args.output.write_text(json.dumps(result,indent=2,sort_keys=True)+'\n',encoding='utf-8')
    print(json.dumps(result,indent=2,sort_keys=True))
    return 0

if __name__ == '__main__':
    raise SystemExit(main())
