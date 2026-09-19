#!/usr/bin/env python3
"""Paired-round timing protocol for the loudness SIMD backends.

Drives the `loudness_simd_timing` example per the commissioning record:
two warmups and nine measured repetitions per fixture, backend order rotated
each round, medians and paired wins against scalar, result-bit identity.
Writes a JSON report and prints a Markdown table.
"""
import argparse, json, statistics, subprocess, sys, platform
from pathlib import Path

FIXTURES = [(1, 600), (2, 600), (4, 120), (5, 120), (6, 120), (8, 120)]
BACKENDS = ["scalar", "sse2", "avx", "production"]

def run(binary, backend, channels, seconds):
    out = subprocess.run([binary, backend, str(channels), str(seconds)], check=True,
                         capture_output=True, text=True).stdout
    return json.loads(out)

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--binary", required=True, type=Path)
    ap.add_argument("--output", required=True, type=Path)
    ap.add_argument("--warmups", type=int, default=2)
    ap.add_argument("--rounds", type=int, default=9)
    ap.add_argument("--channels", type=int, nargs="*", help="restrict to these channel counts")
    args = ap.parse_args()
    fixtures = [f for f in FIXTURES if not args.channels or f[0] in args.channels]

    report = {"host": platform.uname()._asdict(), "fixtures": []}
    cpu = ""
    try:
        for line in Path("/proc/cpuinfo").read_text().splitlines():
            if line.startswith("model name"):
                cpu = line.split(":", 1)[1].strip(); break
    except OSError:
        pass
    report["cpu"] = cpu

    rows = []
    for channels, seconds in fixtures:
        for _ in range(args.warmups):
            for b in BACKENDS:
                run(args.binary, b, channels, seconds)
        samples = {b: [] for b in BACKENDS}
        bits = {b: set() for b in BACKENDS}
        for r in range(args.rounds):
            k = r % len(BACKENDS); order = BACKENDS[k:] + BACKENDS[:k]
            for b in order:
                res = run(args.binary, b, channels, seconds)
                samples[b].append(res["nanos"])
                bits[b].add((res["integrated_bits"], res["lra_bits"]))
            print(f"channels={channels} round {r+1}/{args.rounds} done", file=sys.stderr, flush=True)
        med = {b: statistics.median(samples[b]) for b in BACKENDS}
        wins = {b: sum(1 for s, c in zip(samples["scalar"], samples[b]) if c < s) for b in BACKENDS[1:]}
        all_bits = set().union(*bits.values())
        fx = {"channels": channels, "programme_seconds": seconds, "samples_ns": samples,
              "median_ns": med,
              "vs_scalar_pct": {b: (med["scalar"] - med[b]) / med["scalar"] * 100 for b in BACKENDS[1:]},
              "paired_wins": wins, "result_bit_tuples": sorted(all_bits),
              "bit_identical": len(all_bits) == 1}
        report["fixtures"].append(fx)
        rows.append(fx)

    args.output.write_text(json.dumps(report, indent=2))
    print("| Channels | Programme seconds | Scalar median | SSE2 median | SSE2 vs scalar | Paired wins | AVX median | AVX vs scalar | AVX paired wins | Production median | Production vs scalar | Production wins | Bit-identical |")
    print("| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :---: |")
    for fx in rows:
        m = fx["median_ns"]; v = fx["vs_scalar_pct"]; w = fx["paired_wins"]
        print(f"| {fx['channels']} | {fx['programme_seconds']} | {m['scalar']:,.0f} ns | {m['sse2']:,.0f} ns | {v['sse2']:+.3f}% | {w['sse2']}/{args.rounds} | {m['avx']:,.0f} ns | {v['avx']:+.3f}% | {w['avx']}/{args.rounds} | {m['production']:,.0f} ns | {v['production']:+.3f}% | {w['production']}/{args.rounds} | {'yes' if fx['bit_identical'] else 'NO'} |")

if __name__ == "__main__":
    main()
