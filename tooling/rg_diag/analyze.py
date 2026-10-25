#!/usr/bin/env python3
"""Validate and summarize IP-01 + opt-in ReplayGain profiling JSONL.

Do not sum concurrently executing observer and transfer durations. Host
process CPU is a sampled lower bound. An absent source-pass trace is NOT a
positive measurement of zero cost.
"""

import argparse
from collections import defaultdict
import json
from pathlib import Path
import statistics
import sys

MAX_RECORD_BYTES = 1_000_000
MAX_RECORDS = 2_000_000


def require_nonnegative_int(row, key):
    value = row.get(key)
    if type(value) is not int or value < 0:
        raise ValueError(f"invalid nonnegative {key} in rg_diag record")
    return value


def read_jsonl(path):
    if not path.exists():
        return []
    rows = []
    with path.open("rb") as source:
        for idx, line in enumerate(source, 1):
            if idx > MAX_RECORDS:
                raise ValueError("excess diagnostic rows")
            if len(line) > MAX_RECORD_BYTES:
                raise ValueError(f"overlong diagnostic row {idx}")
            if not line.endswith(b"\n"):
                raise ValueError(f"unterminated JSONL row {idx}; trace may be truncated")
            if not line.strip():
                continue
            try:
                data = json.loads(line)
            except (ValueError, UnicodeDecodeError) as exc:
                raise ValueError(f"invalid JSON row {idx}") from exc
            if not isinstance(data, dict):
                raise ValueError(f"non-object row {idx}")
            rows.append(data)
    return rows


TRANSPORT_TIME_KEYS = (
    "decoder_read_call_ns", "encoder_write_call_ns", "mirror_write_call_ns",
    "fifo_open_call_ns", "header_write_call_ns",
)
OBSERVER_TIME_KEYS = (
    "fifo_open_call_ns", "header_read_call_ns", "pcm_read_call_ns",
    "pcm_decode_call_ns", "loudness_push_call_ns",
    "reporting_peak_push_call_ns", "loudness_finalize_call_ns",
    "reporting_peak_finalize_call_ns", "eof_wait_call_ns",
)


def summary_by_run(rows):
    runs = defaultdict(lambda: {"manifest": [], "transport": [], "observer": [], "policy": [], "other_phases": []})
    for row in rows:
        if row.get("event") != "cue_ip01":
            continue
        if row.get("cue_schema") != "tonepoet.cue.ip01.v1":
            raise ValueError("unrecognized CUE record schema")
        run = row.get("cue_run_id")
        if not isinstance(run, str) or len(run) != 24 or any(c not in "0123456789abcdef" for c in run):
            raise ValueError("malformed or missing CUE run ID")
        phase = row.get("cue_phase")
        if phase == "rg_diag_manifest":
            if row.get("rg_diag_schema") != "tonepoet.cue.rgdiag.v1":
                raise ValueError("unrecognized rg_diag manifest schema")
            count = require_nonnegative_int(row, "track_count")
            ordinals = row.get("track_ordinals")
            if (not isinstance(ordinals, list) or not 0 < count <= 1000
                or len(ordinals) != count
                or any(type(n) is not int or n <= 0 or n > 0xFFFFFFFF for n in ordinals)
                or len(set(ordinals)) != count):
                raise ValueError("invalid or duplicate expected CUE track ordinals")
            runs[run]["manifest"].append(row)
        elif phase in ("rg_diag_transport", "rg_diag_observer"):
            if row.get("rg_diag_schema") != "tonepoet.cue.rgdiag.v1":
                raise ValueError("unrecognized rg_diag schema")
            if type(row.get("complete")) is not bool:
                raise ValueError("missing boolean completion evidence")
            require_nonnegative_int(row, "track_ordinal")
            if phase == "rg_diag_transport":
                for key in TRANSPORT_TIME_KEYS + ("elapsed_wall_ns", "pcm_bytes", "chunks"):
                    require_nonnegative_int(row, key)
                if sum(row[k] for k in TRANSPORT_TIME_KEYS) > row["elapsed_wall_ns"] + 10_000_000:
                    raise ValueError("transport phase durations exceed enclosing wall interval")
                runs[run]["transport"].append(row)
            else:
                for key in OBSERVER_TIME_KEYS + ("pcm_bytes", "blocks"):
                    require_nonnegative_int(row, key)
                # Historical v1 rows had no readiness counters. Corrected
                # producers advertise their wait strategy and must supply all
                # counters so missing new evidence cannot silently pass.
                if row.get("fifo_wait_strategy") == "poll_50ms_plus_no_writer_backoff":
                    for key in ("readiness_wait_call_ns", "readiness_wait_count",
                                "readiness_timeout_count", "no_writer_poll_count",
                                "would_block_poll_count", "poll_sleep_call_ns"):
                        require_nonnegative_int(row, key)
                    if row["readiness_timeout_count"] > row["readiness_wait_count"]:
                        raise ValueError("FIFO readiness timeouts exceed poll calls")
                elif "fifo_wait_strategy" in row and row["fifo_wait_strategy"] != "legacy_non_unix":
                    raise ValueError("unrecognized FIFO wait strategy")
                runs[run]["observer"].append(row)
        elif phase == "source_pass_replaygain_result":
            runs[run]["policy"].append(row)
        else:
            runs[run]["other_phases"].append(phase)
    return dict(runs)


def classify_run(run):
    trows, orows = run["transport"], run["observer"]
    recovery_retry = "source_pass_observer_recovery_retry" in run["other_phases"]
    if not trows and not orows:
        if recovery_retry:
            return "FALLBACK", ["Observer recovery retry; no complete source-pass profiles"], []
        return "NOT INSTRUMENTED", ["No source-pass profiler records; RG Off, fallback, or disabled profiling"], []
    warnings = []
    if len(run["manifest"]) > 1:
        return "AMBIGUOUS", ["Multiple expected-track manifests for one CUE run"], []
    expected = set(run["manifest"][0]["track_ordinals"]) if run["manifest"] else None
    if expected is None:
        warnings.append("Expected-track manifest missing: cannot prove album completeness")
    tmap = defaultdict(list)
    omap = defaultdict(list)
    for row in trows:
        tmap[row["track_ordinal"]].append(row)
    for row in orows:
        omap[row["track_ordinal"]].append(row)
    if any(len(values) != 1 for values in list(tmap.values()) + list(omap.values())):
        warnings.append("Repeated track ordinal: attempt correlation is ambiguous; do not merge attempts")
        return "AMBIGUOUS", warnings, []
    observed = set(tmap) | set(omap)
    if expected is not None:
        for ordinal in sorted(expected - observed):
            warnings.append(f"Missing all diagnostics for expected ordinal {ordinal}")
        for ordinal in sorted(observed - expected):
            warnings.append(f"Unexpected diagnostic ordinal {ordinal} not in expected manifest")
    pairs = []
    for ordinal in sorted(observed):
        t = tmap.get(ordinal, [None])[0]
        o = omap.get(ordinal, [None])[0]
        if t is None or o is None:
            warnings.append(f"Missing peer transport/observer for ordinal {ordinal}")
            continue
        if not t["complete"] or not o["complete"]:
            warnings.append(f"Incomplete transport/observer for ordinal {ordinal}")
        elif t["pcm_bytes"] != o["pcm_bytes"]:
            warnings.append(f"PCM byte mismatch for ordinal {ordinal}: {t['pcm_bytes']} vs {o['pcm_bytes']}")
        pairs.append((ordinal, t, o))
    if not pairs:
        return "INCOMPLETE", warnings, []
    policies = run["policy"]
    if len(policies) != 1 or type(policies[0].get("source_pass_observed")) is not bool:
        warnings.append("Missing, duplicated or invalid authoritative source-pass result")
    elif not policies[0]["source_pass_observed"]:
        warnings.append("Authoritative result did not retain source-pass ReplayGain; fallback or failure")
    if recovery_retry:
        warnings.append("Observer-causal recovery retry: source-pass timing is not a successful conversion")
    if any("mismatch" in w or "Unexpected diagnostic ordinal" in w for w in warnings):
        status = "FAIL"
    elif recovery_retry and len(policies) == 1 and policies[0].get("source_pass_observed") is True:
        status = "AMBIGUOUS"
    elif recovery_retry or (len(policies) == 1 and policies[0].get("source_pass_observed") is False):
        status = "FALLBACK"
    elif warnings:
        status = "INCOMPLETE"
    else:
        status = "PASS"
    return status, warnings, pairs


def secs_ns(value):
    return value / 1_000_000_000


def as_s(value):
    return f"{secs_ns(value):.3f}"


def summarize_benchmark(path):
    rows = read_jsonl(path)
    groups = defaultdict(list)
    for row in rows:
        if row.get("schema") != "tonepoet.rg_native_bench.v1":
            raise ValueError("unexpected benchmark schema")
        for key in ("wall_ns", "read_ns", "decode_ns", "loudness_ns", "peak_ns", "frames", "sample_rate_hz"):
            require_nonnegative_int(row, key)
        if row["sample_rate_hz"] == 0 or row["frames"] == 0:
            raise ValueError("invalid benchmark geometry")
        groups[(row["mode"], row["encoding"], row["sample_rate_hz"], row["channels"])].append(row)
    lines = ["## Standalone native-meter benchmark", "", "| Mode | Repeats | Median wall (s) | Loudness call (s) | Reporting peak call (s) | Wall / programme |", "|---|---:|---:|---:|---:|---:|"]
    for key, values in sorted(groups.items()):
        # Never mix geometry within a median cohort.
        lengths = {r["frames"] for r in values}
        if len(lengths) != 1:
            raise ValueError("mixed programme lengths in benchmark cohort")
        results = {(r.get("lufs_bits"), r.get("peak_linear_bits")) for r in values}
        if len(results) != 1:
            raise ValueError("standalone meter outputs changed across identical repeats")
        mode, enc, rate, ch = key
        median = lambda name: statistics.median(r[name] for r in values)
        programme_s = next(iter(lengths)) / rate
        lines.append(f"| {mode} ({enc}, {rate} Hz, {ch}ch) | {len(values)} | {as_s(median('wall_ns'))} | {as_s(median('loudness_ns'))} | {as_s(median('peak_ns'))} | {secs_ns(median('wall_ns')) / programme_s:.3f}× |")
    lines += ["", "Standalone PCM file reads, decoding, OS caching, finalization and meter costs are not equivalent to pipelined FIFO backpressure.", ""]
    return lines


def make_report(run_dirs, benchmarks=None):
    lines = [
        "# TonePoet ReplayGain source-pass performance investigation", "",
        "Instrumented measurements are inclusive wall-clock call durations, not CPU times or pure FIFO blocked time.",
        "The transport and observer execute concurrently. **Never add their call durations as an album critical path.**", "",
        "| Run | TonePoet conversion wall (s) | Host process wall (s) | CPU lower bound (s) | CUE run IDs | Diagnostic status |",
        "|---|---:|---:|---:|---:|---|",
    ]
    details = []
    for folder in run_dirs:
        host = json.loads((folder / "host.json").read_text())
        if host.get("schema") != "tonepoet.rg_host.v1":
            raise ValueError(f"invalid host schema in {folder}")
        label = host["label"]
        if type(host.get("elapsed_wall_ns")) is not int or host["elapsed_wall_ns"] < 0:
            raise ValueError(f"invalid host wall time in {folder}")
        rows = read_jsonl(folder / "cue.jsonl")
        groups = summary_by_run(rows)
        statuses = [(rid, *classify_run(data)) for rid, data in sorted(groups.items())]
        status_label = ", ".join(f"{r[:8]}:{status}" for r, status, _, _ in statuses) or "NOT INSTRUMENTED"
        cpu = host.get("cpu_sampling", {}).get("aggregate_cpu_lower_bound_s")
        cpu_label = f"{cpu:.3f}" if isinstance(cpu, (int, float)) else "N/A"
        # A TUI process can remain open long after the conversion. Only an
        # unambiguous single completed conversion's own timer is meaningful.
        conversions = [r for r in rows if r.get("event") == "run_finished"
                       and type(r.get("elapsed_ms")) in (int, float)
                       and r["elapsed_ms"] >= 0]
        conversion_wall = f"{conversions[0]['elapsed_ms'] / 1000:.3f}" if len(conversions) == 1 else "N/A"
        lines.append(f"| {label} | {conversion_wall} | {as_s(host['elapsed_wall_ns'])} | {cpu_label} | {len(groups)} | {status_label} |")
        details.extend([f"### {label}", "", f"Host return code: `{host.get('returncode', 'unknown')}`. Profiling enabled: `{host.get('instrumented')}`. Trace records: {len(rows)}.", ""])
        by_role = host.get("cpu_sampling", {}).get("by_inferred_role", {})
        if by_role:
            details.append("Inferred per-role process CPU lower bounds (CUE S1 command signature heuristics; not exclusive native ReplayGain time): " + ", ".join(
                f"{name}: {value['cpu_lower_bound_s']:.3f} s" for name, value in sorted(by_role.items())))
            details.append("")
        by_name = host.get("cpu_sampling", {}).get("by_executable_basename", {})
        if by_name:
            details.append("Sampled process-tree CPU lower bounds: " + ", ".join(f"{name}: {value['cpu_lower_bound_s']:.3f} s" for name, value in sorted(by_name.items())))
            details.append("")
        if not statuses:
            details.extend(["No CUE runs observed. Route fallback, RG Off, diagnostic disabled, or an uninstrumented execution are possible; **not a zero-cost result**.", ""])
        for rid, status, warnings, pairs in statuses:
            policy = groups[rid]["policy"]
            details += [f"**CUE run `{rid}` — {status}**", ""]
            if len(policy) == 1:
                details.append(f"Source-pass RG observed: `{policy[0].get('source_pass_observed', 'unknown')}`. ")
            elif len(policy) != 1:
                details.append(f"Source-pass result event count: {len(policy)}; do not infer observer success from eligibility.")
            if warnings:
                details.append("Evidence warnings: " + "; ".join(warnings))
            details.append("")
            if pairs:
                details.extend([
                    "| Ordinal | PCM MiB | Read call (s) | Encoder write call (s) | Mirror write call (s) | Observer PCM read call (s) | Decode PCM call (s) | Loudness call (s) | Reporting peak call (s) |", 
                    "|---:|---:|---:|---:|---:|---:|---:|---:|---:|",
                ])
                for ordinal, t, o in pairs:
                    MiB = t["pcm_bytes"] / (1024 * 1024)
                    details.append(
                        f"| {ordinal} | {MiB:.2f} | {as_s(t['decoder_read_call_ns'])} | {as_s(t['encoder_write_call_ns'])} | {as_s(t['mirror_write_call_ns'])} | "
                        f"{as_s(o['pcm_read_call_ns'])} | {as_s(o['pcm_decode_call_ns'])} | "
                        f"{as_s(o['loudness_push_call_ns'] + o['loudness_finalize_call_ns'])} | "
                        f"{as_s(o['reporting_peak_push_call_ns'] + o['reporting_peak_finalize_call_ns'])} |"
                    )
                details.append("")
                strategies = {o.get("fifo_wait_strategy") for _, _, o in pairs}
                if strategies == {"poll_50ms_plus_no_writer_backoff"}:
                    waits = sum(o["readiness_wait_count"] for _, _, o in pairs)
                    timeouts = sum(o["readiness_timeout_count"] for _, _, o in pairs)
                    eagain = sum(o["would_block_poll_count"] for _, _, o in pairs)
                    readiness_ns = sum(o["readiness_wait_call_ns"] for _, _, o in pairs)
                    setup_ns = sum(o["poll_sleep_call_ns"] for _, _, o in pairs)
                    details.append(
                        f"FIFO readiness: {eagain} EAGAIN reads, {waits} poll(2) calls "
                        f"({timeouts} timeouts), {as_s(readiness_ns)} cumulative poll wall seconds, "
                        f"{as_s(setup_ns)} cumulative pre-writer backoff seconds. "
                        "These per-observer durations overlap; they are not album elapsed time."
                    )
                    details.append("EAGAIN now waits on descriptor readiness. Only the initial no-writer setup retains a 5-ms sleep.")
                elif strategies == {None}:
                    details.append("Legacy FIFO observer used 5-ms sleeps after EAGAIN and before the first writer connected.")
                else:
                    details.append("Mixed/other FIFO wait strategies: compare only homogeneous runs, not legacy and corrected sleep counters.")
                details.append("These track-level call-wall totals include scheduler delays. Loudness and reporting peak are distinct meter calls, not exclusive CPU costs. A long mirror-write call is consistent with backpressure but is not definitive evidence of kernel blocking.")
                details.append("")
        if host.get("returncode") != 0:
            details.append("**Host execution failed or was interrupted; exclude this trial from performance acceptance.**")
            details.append("")
    lines += ["", "## Per-run evidence", ""] + details
    if benchmarks:
        for bench in benchmarks:
            lines += summarize_benchmark(bench)
    lines += [
        "## Interpretation guardrails", "",
        "- First compare repeated uninstrumented RG Off/Both end-to-end runs, on the same host, source, conversion settings and warm/cold cache class. Do not average failed or fallback attempts into a claimed source-pass result.",
        "- Separately compare instrumented RG Both against uninstrumented RG Both: the difference bounds diagnostic overhead but is not itself correction for all run-to-run variability.",
        "- Distinguish `source_pass_observed=true` from policy eligibility, and require transport and observer `complete=true` plus exact PCM byte parity.",
        "- If native push calls dominate and the standalone benchmark is similarly slow, investigate computational kernels; if mirror writes are long while the meter is fast standalone, investigate scheduling/buffering with targeted host profiling.",
        "- A long mirror-write wall interval does NOT alone prove the FIFO is full; tokio task wake-up, thread scheduling and kernel copy can contribute. For true block/wake evidence, use OS-level sched/syscall tracing on the exact run.",
        "- Host-process wall includes CLI startup or TUI interaction; use TonePoet's single `run_finished.elapsed_ms` when available for actual conversion wall. Never substitute interactive host wall as conversion duration.",
        "- Linux `/proc` process-tree CPU can miss exited workers and reports lower bounds; use `perf stat` or OS profiling for authoritative CPU attribution where available.",
        "- Do not infer an upper bound on parallel CUE speedup by subtracting RG On from Off elapsed times. Parallelism and meter work can overlap differently after architecture changes.",
        "",
    ]
    return "\n".join(lines)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("run_dirs", type=Path, nargs="+", help="Run directories containing host.json and cue.jsonl")
    parser.add_argument("--bench", type=Path, action="append", help="JSONL stdout from rg_native_bench")
    parser.add_argument("--output", type=Path, help="Optional report path; fails if existing")
    args = parser.parse_args(argv)
    report = make_report(args.run_dirs, args.bench)
    if args.output:
        with args.output.open("x") as output:
            output.write(report + "\n")
        print(f"Wrote report: {args.output}")
    else:
        print(report)


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, KeyError) as exc:
        print(f"diagnostic analysis failed: {exc}", file=sys.stderr)
        sys.exit(2)
