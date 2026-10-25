#!/usr/bin/env python3
"""Run a real TonePoet conversion while sampling Linux process-tree CPU.

Uses the existing opt-in baseline JSONL sink and the new CUE RG profiling
flag; does not change argv, conversion decisions, or output metadata. The
per-process CPU values are LOWER BOUNDS (short-lived processes can be missed).

    python3 tooling/rg_diag/run_host.py --out ./private-rg --label both-01 -- ./target/release/tonepoet

For an interactive TUI, stdin/stdout/stderr are inherited. Run precisely one
conversion per invocation, and close the application when it finishes.
"""

import argparse
from collections import defaultdict
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import threading
import time


def read_proc_stat(pid):
    """Return pid, ppid, process CPU ticks, start tick, and benign basename."""
    try:
        raw = Path(f"/proc/{pid}/stat").read_text()
        closing = raw.rfind(")")
        if closing < 0 or not raw.startswith(f"{pid} ("):
            return None
        comm = raw[len(f"{pid} ("):closing]
        fields = raw[closing + 2:].split()
        return (pid, int(fields[1]), int(fields[11]) + int(fields[12]), int(fields[19]), comm[:80])
    except (OSError, ValueError, IndexError):
        return None


def classify_process_role(basename, argv, is_root=False):
    """Classify only known *actual* CUE stream commands; never retain argv.

    The native ReplayGain observer is a thread inside TonePoet, not an external
    process. Root CPU includes other application work, so do not call its CPU
    seconds exclusive ReplayGain computation.
    """
    if is_root:
        return ("tonepoet_app_including_native_meter"
                if basename.casefold().startswith("tonepoet")
                else "root_application_unclassified")
    name = basename.casefold()
    if name.startswith("ffmpeg"):
        if any(argv[i:i + 2] == ["-i", "pipe:0"] for i in range(len(argv) - 1)):
            return "cue_stream_encoder"
        if "pipe:1" in argv and any(argv[i:i + 2] == ["-f", fmt]
                                        for i in range(len(argv) - 1)
                                        for fmt in ("s32le", "f32le", "f64le")):
            return "cue_stream_decoder"
        return "other_ffmpeg_or_unclassified"
    if name == "sox" or name == "sox_ng":
        if "-" in argv and "-S" in argv and "-D" in argv:
            return "cue_stream_encoder"
        return "other_sox_or_unclassified"
    return "other_or_unclassified"


def role_for_pid(pid, name, is_root):
    if is_root:
        return classify_process_role(name, [], is_root=True)
    if not (name.casefold().startswith("ffmpeg") or name.casefold() in ("sox", "sox_ng")):
        return "other_or_unclassified"
    try:
        raw = Path(f"/proc/{pid}/cmdline").read_bytes()[:65536]
        argv = [entry.decode("utf-8", errors="replace") for entry in raw.split(b"\0") if entry]
    except OSError:
        argv = []
    return classify_process_role(name, argv)


def process_snapshot():
    processes = {}
    try:
        names = os.listdir("/proc")
    except OSError:
        return processes
    for name in names:
        if name.isdigit():
            result = read_proc_stat(int(name))
            if result:
                processes[result[0]] = result
    return processes


def descendants(snapshot, root):
    children = defaultdict(list)
    for pid, ppid, _ticks, _start, _name in snapshot.values():
        children[ppid].append(pid)
    out = set()
    pending = [root]
    while pending:
        p = pending.pop()
        if p in out:
            continue
        if p not in snapshot:
            continue
        out.add(p)
        pending.extend(children[p])
    return out


def sample_tree(pid, interval, stop, samples):
    samples["count"] = 0
    known = {}
    while not stop.is_set():
        snap = process_snapshot()
        for child in descendants(snap, pid):
            cp, _ppid, ticks, start, name = snap[child]
            key = (cp, start)  # PID reuse cannot silently merge an unrelated process.
            old = known.get(key)
            if old is None or old["ticks"] < ticks:
                previous_role = old["role"] if old is not None else role_for_pid(cp, name, cp == pid)
                known[key] = {"pid": cp, "start_ticks": start, "ticks": ticks, "name": name,
                              "role": previous_role}
        samples["count"] += 1
        stop.wait(interval)
    # A final read closes as much of the tail as Linux allows. Any process
    # already reaped before sampling remains an acknowledged measurement gap.
    snap = process_snapshot()
    for child in descendants(snap, pid):
        cp, _ppid, ticks, start, name = snap[child]
        key = (cp, start)
        if key not in known or known[key]["ticks"] < ticks:
            previous_role = known[key]["role"] if key in known else role_for_pid(cp, name, cp == pid)
            known[key] = {"pid": cp, "start_ticks": start, "ticks": ticks, "name": name,
                          "role": previous_role}
    samples["processes"] = list(known.values())


def run(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", required=True, help="Private results root (new label subdir)")
    parser.add_argument("--label", required=True, help="Unique run label, no spaces")
    parser.add_argument("--interval-ms", type=int, default=50)
    parser.add_argument("--no-instrument", action="store_true", help="Uninstrumented control")
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args(argv)
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,79}", args.label):
        parser.error("label must be 1..80 safe characters, no traversal")
    if not 20 <= args.interval_ms <= 1000:
        parser.error("--interval-ms must be between 20 and 1000")
    command = args.command[1:] if args.command and args.command[0] == "--" else args.command
    if not command:
        parser.error("pass the TonePoet command after --")
    results = Path(args.out).expanduser().resolve() / args.label
    results.mkdir(parents=True, exist_ok=False)
    try:
        os.chmod(results, 0o700)
    except OSError:
        pass
    env = os.environ.copy()
    trace = results / "cue.jsonl"
    if not args.no_instrument:
        # Preserve the normal conversion umask. Precreate *only* the private
        # diagnostic file with mode 0600; the Rust append sink retains it.
        fd = os.open(trace, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        os.close(fd)
    if args.no_instrument:
        env.pop("TONEPOET_CUE_RG_PROFILE", None)
        env.pop("TONEPOET_PIPELINE_BASELINE_JSONL", None)
    else:
        env["TONEPOET_CUE_RG_PROFILE"] = "1"
        env["TONEPOET_PIPELINE_BASELINE_JSONL"] = str(trace)
    # Neither the command's arguments nor audio paths are written to reports.
    record = {
        "schema": "tonepoet.rg_host.v1",
        "label": args.label,
        "instrumented": not args.no_instrument,
        "command_basename": Path(command[0]).name,
        "utc_start": datetime.now(timezone.utc).isoformat(),
        "platform": sys.platform,
        "host_cpu_count": os.cpu_count(),
        "sample_interval_ms": args.interval_ms,
    }
    started = time.monotonic_ns()
    try:
        process = subprocess.Popen(command, env=env, stdin=None, stdout=None, stderr=None)
    except OSError as exc:
        record["spawn_error"] = type(exc).__name__
        (results / "host.json").write_text(json.dumps(record, indent=2) + "\n")
        raise
    stopping = threading.Event()
    captures = {}
    sampler = None
    if sys.platform.startswith("linux"):
        sampler = threading.Thread(
            target=sample_tree,
            args=(process.pid, args.interval_ms / 1000.0, stopping, captures),
            daemon=True,
        )
        sampler.start()
    try:
        returncode = process.wait()
    except KeyboardInterrupt:
        # Do not send a second signal to the whole foreground TUI process
        # group; interactive shells have normally already sent SIGINT.
        returncode = process.wait()
    finally:
        stopping.set()
        if sampler is not None:
            sampler.join(timeout=2)
    record["utc_end"] = datetime.now(timezone.utc).isoformat()
    record["elapsed_wall_ns"] = time.monotonic_ns() - started
    record["returncode"] = returncode
    record["sample_count"] = captures.get("count", 0)
    if sys.platform.startswith("linux"):
        clk = os.sysconf("SC_CLK_TCK")
        proc = captures.get("processes", [])
        groups = defaultdict(lambda: {"processes": 0, "cpu_lower_bound_s": 0.0})
        roles = defaultdict(lambda: {"processes": 0, "cpu_lower_bound_s": 0.0})
        for x in proc:
            group = groups[x["name"]]
            group["processes"] += 1
            group["cpu_lower_bound_s"] += x["ticks"] / clk
            role = roles[x["role"]]
            role["processes"] += 1
            role["cpu_lower_bound_s"] += x["ticks"] / clk
        record["cpu_sampling"] = {
            "kind": "procfs_descendant_poll",
            "cpu_seconds_is_lower_bound": True,
            "short_lived_child_may_be_missing": True,
            "process_count_observed": len(proc),
            "aggregate_cpu_lower_bound_s": round(sum(x["ticks"] / clk for x in proc), 6),
            "by_executable_basename": dict(sorted(groups.items())),
            "by_inferred_role": dict(sorted(roles.items())),
            "role_classification": "heuristic_exact_CUE_S1_ffmpeg_and_sox_patterns_only; excludes_short_lived_unobserved_processes",
        }
    else:
        record["cpu_sampling"] = {"kind": "not_available", "reason": "Linux procfs required"}
    result_file = results / "host.json"
    result_file.write_text(json.dumps(record, indent=2, sort_keys=True) + "\n")
    try:
        os.chmod(result_file, 0o600)
        if trace.exists():
            os.chmod(trace, 0o600)
    except OSError:
        pass
    print(f"Host report: {result_file}", file=sys.stderr)
    print(f"CUE diagnostic trace: {trace if trace.exists() else '(not emitted)'}", file=sys.stderr)
    return returncode


if __name__ == "__main__":
    sys.exit(run())
