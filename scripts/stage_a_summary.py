#!/usr/bin/env python3
"""Summarise a TONEPOET_PIPELINE_BASELINE_JSONL stream for the Stage A profile.

Reports per-item stage times, per-tool run counts and durations, concurrency
high-water marks (tools, certified scans, ReplayGain decoders), the ReplayGain
tail, and the selected-vs-emitted realization check. Read-only.
"""
import argparse, json, statistics, sys
from collections import defaultdict
from pathlib import Path

def load(path):
    for line in Path(path).read_text().splitlines():
        line = line.strip()
        if line:
            try:
                yield json.loads(line)
            except json.JSONDecodeError:
                yield {"event": "_unparseable", "raw": line[:200]}

def num(v):
    return float(v) if isinstance(v, (int, float)) else 0.0

def fmt_ms(ms):
    return f"{ms/1000:.1f}s" if ms >= 1000 else f"{ms:.0f}ms"

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("jsonl", nargs="+")
    ap.add_argument("--markdown", action="store_true")
    args = ap.parse_args()
    events = [e for p in args.jsonl for e in load(p)]
    by_event = defaultdict(int)
    for e in events:
        by_event[e.get("event", "?")] += 1

    runs = {}
    names = {}
    for e in events:
        if e.get("event") == "track_record" and e.get("output_file"):
            names.setdefault(e["item_id"], Path(e["output_file"]).name)
    stages = defaultdict(lambda: defaultdict(float))
    stage_totals = defaultdict(list)
    for e in events:
        ev = e.get("event")
        if ev == "run_finished":
            runs[e["item_id"]] = e
        elif ev == "stage_finished":
            stages[e["item_id"]][e["stage"]] += num(e.get("elapsed_ms"))
            stage_totals[e["stage"]].append(num(e.get("elapsed_ms")))

    tools = defaultdict(list)
    tool_hw = 0
    for e in events:
        if e.get("event") == "tool_run_finished":
            tools[e["tool"]].append(num(e.get("elapsed_ms")))
        if e.get("event") == "tool_run_started":
            tool_hw = max(tool_hw, int(num(e.get("active_high_water"))))

    scans = defaultdict(list)
    scan_hw = 0
    for e in events:
        if e.get("event") == "certified_scan_finished":
            scans[e.get("kind")].append((num(e.get("elapsed_ms")), num(e.get("logical_bytes"))))
        if e.get("event") == "certified_scan_started":
            scan_hw = max(scan_hw, int(num(e.get("active_high_water"))))

    rg = []
    rg_hw = 0
    for e in events:
        if e.get("event") == "replaygain_observation_finished":
            rg.append(e)
        if e.get("event") == "replaygain_observation_started":
            rg_hw = max(rg_hw, int(num(e.get("active_high_water"))))

    scoped = defaultdict(list)
    for e in events:
        ev = e.get("event", "")
        if ev.endswith("_finished") and ev not in ("run_finished", "stage_finished", "tool_run_finished", "certified_scan_finished", "replaygain_observation_finished"):
            scoped[ev].append(num(e.get("elapsed_ms")))

    sve = [e for e in events if e.get("event") == "selected_vs_emitted"]
    sve_mismatch = [e for e in sve if e.get("matches_selected_realization") is False]

    out = []
    p = out.append
    p(f"events: {sum(by_event.values())}  kinds: {dict(sorted(by_event.items()))}")
    p("")
    p("## Runs")
    p("| item | status | elapsed | rss KiB | tool hw | scan hw | rg hw |")
    p("|---|---|---:|---:|---:|---:|---:|")
    for item, r in runs.items():
        status = str(r.get('terminal_status', '')).split(' {')[0].split('(')[0]
        p(f"| {names.get(item, item[:8])[:48]} | {status} | {fmt_ms(num(r.get('elapsed_ms')))} | {r.get('rss_kib')} | {r.get('tool_active_high_water')} | {r.get('certified_scan_active_high_water')} | {r.get('replaygain_decoder_active_high_water')} |")
    p("")
    p("## Stage time, summed over items")
    p("| stage | n | total | median | max |")
    p("|---|---:|---:|---:|---:|")
    grand = sum(sum(v) for v in stage_totals.values()) or 1.0
    for st, v in sorted(stage_totals.items(), key=lambda kv: -sum(kv[1])):
        p(f"| {st} | {len(v)} | {fmt_ms(sum(v))} ({sum(v)/grand*100:.0f}%) | {fmt_ms(statistics.median(v))} | {fmt_ms(max(v))} |")
    p("")
    p(f"## Tool runs (high-water concurrent: {tool_hw})")
    p("| tool | runs | total | median | max |")
    p("|---|---:|---:|---:|---:|")
    for t, v in sorted(tools.items(), key=lambda kv: -sum(kv[1])):
        p(f"| {t} | {len(v)} | {fmt_ms(sum(v))} | {fmt_ms(statistics.median(v))} | {fmt_ms(max(v))} |")
    p("")
    p(f"## Certified scans (high-water concurrent: {scan_hw})")
    p("| kind | scans | total | MiB scanned | MiB/s |")
    p("|---|---:|---:|---:|---:|")
    for k, v in scans.items():
        ms = sum(x for x, _ in v); mb = sum(b for _, b in v) / 2**20
        p(f"| {k} | {len(v)} | {fmt_ms(ms)} | {mb:.0f} | {mb/(ms/1000) if ms else 0:.1f} |")
    p("")
    p(f"## ReplayGain observations (high-water concurrent decoders: {rg_hw})")
    if rg:
        ms = sum(num(e.get("elapsed_ms")) for e in rg); ib = sum(num(e.get("input_bytes")) for e in rg) / 2**20
        pcm = sum(num(e.get("decoded_pcm_bytes")) for e in rg) / 2**20
        allocs = sum(int(num(e.get("frame_allocations"))) for e in rg)
        p(f"files: {len(rg)}  total: {fmt_ms(ms)}  median: {fmt_ms(statistics.median(num(e.get('elapsed_ms')) for e in rg))}  input MiB: {ib:.0f}  decoded PCM MiB: {pcm:.0f}  frame allocations: {allocs}")
    else:
        p("none")
    p("")
    if scoped:
        p("## Other scoped events")
        p("| event | n | total | median |")
        p("|---|---:|---:|---:|")
        for ev, v in sorted(scoped.items(), key=lambda kv: -sum(kv[1])):
            p(f"| {ev} | {len(v)} | {fmt_ms(sum(v))} | {fmt_ms(statistics.median(v))} |")
        p("")
    p(f"## Selected vs emitted realization: {len(sve)} records, {len(sve_mismatch)} mismatches")
    for e in sve_mismatch[:10]:
        p(f"- {json.dumps(e)[:300]}")
    print("\n".join(out))

if __name__ == "__main__":
    main()
