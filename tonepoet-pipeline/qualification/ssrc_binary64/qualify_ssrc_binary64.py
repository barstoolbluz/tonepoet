#!/usr/bin/env python3
"""Outcome-C-safe qualification harness for Tonepoet's narrow SSRC Binary64 contract.

The harness never changes Tonepoet's source-controlled production registry.  It
emits a machine-readable report that may be reviewed and commissioned later.
A passing black-box run is insufficient to produce an established result unless
exact build/static-audit identity is supplied and complete.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import platform
import shutil
import struct
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any

CONTRACT_ID = "TonepoetBinary64OverloadPreservingResampleV1"
SSRC_REV = "6b0bbfe1fff79c0399347f4e1fb027c9931ef6ea"
SSRC_NAR = "sha256-S1AODgERVoo8mKLEJN9gj4d+EW/ABfxkZqz1p14prPI="
SLEEF_REV = "0c063a8f0e01c22fa1e473effd2e7a0c69b4963a"
INGRESS_AUTHORITY = "tonepoet:protected-pcm-f64le-riff-ingress/v1"
DOUBLE_PROFILES = ("high", "long", "insane")
SINGLE_PROFILES = ("standard", "short", "fast", "lightning")
RATE_PAIRS = ((44100, 48000), (48000, 44100), (96000, 44100), (44100, 96000), (88200, 48000))
QUALIFICATION_ATTENUATION_DB = 0.0
QUALIFICATION_MIN_PHASE = False
DC_PLATEAU_INPUT_FRAMES = 2048
DC_SETTLE_FRACTION = 0.25
DC_GAIN_ABS_TOLERANCE = 0.01
W64_RIFF_GUID = b"riff.\x91\xcf\x11\xa5\xd6\x28\xdb\x04\xc1\x00\x00"
W64_WAVE_GUID = b"wave\xf3\xac\xd3\x11\x8c\xd1\x00\xc0O\x8e\xdb\x8a"
W64_FMT_GUID = b"fmt \xf3\xac\xd3\x11\x8c\xd1\x00\xc0O\x8e\xdb\x8a"
W64_FACT_GUID = b"fact\xf3\xac\xd3\x11\x8c\xd1\x00\xc0O\x8e\xdb\x8a"
W64_DATA_GUID = b"data\xf3\xac\xd3\x11\x8c\xd1\x00\xc0O\x8e\xdb\x8a"
IEEE_FLOAT_SUBFORMAT_GUID = b"\x03\x00\x00\x00\x00\x00\x10\x00\x80\x00\x00\xaa\x00\x38\x9b\x71"


def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for block in iter(lambda: f.read(1024 * 1024), b""):
            h.update(block)
    return h.hexdigest()


def render_attenuation_db(value: float) -> str:
    return f"{value:.1f}"


def characterization_cell(profile: str, src_rate: int, dst_rate: int, channels: int) -> dict[str, Any]:
    return {
        "profile": profile,
        "source_rate_hz": src_rate,
        "target_rate_hz": dst_rate,
        "channels": channels,
        "attenuation_db": QUALIFICATION_ATTENUATION_DB,
        "min_phase": QUALIFICATION_MIN_PHASE,
        "output_bits": -64,
        "input_container": "wav",
        "input_format": "pcm_f64le",
        "output_container": "w64",
        "output_format": "pcm_f64le",
    }


def ssrc_argv(exe: Path, cell: dict[str, Any], inp: Path, out: Path) -> list[str]:
    argv = [
        str(exe),
        "--rate", str(cell["target_rate_hz"]),
        "--profile", str(cell["profile"]),
        "--att", render_attenuation_db(float(cell["attenuation_db"])),
        "--bits", str(cell["output_bits"]),
        "--dstContainer", str(cell["output_container"]),
    ]
    if cell["min_phase"]:
        argv.append("--minPhase")
    argv.extend([str(inp), str(out)])
    return argv


def write_float64_wav(path: Path, rate: int, channels: int, samples: list[tuple[float, ...]]) -> None:
    payload = b"".join(struct.pack("<" + "d" * channels, *frame) for frame in samples)
    block_align = channels * 8
    fmt = struct.pack("<HHIIHH", 3, channels, rate, rate * block_align, block_align, 64)
    riff_size = 4 + (8 + len(fmt)) + (8 + len(payload))
    with path.open("wb") as f:
        f.write(b"RIFF")
        f.write(struct.pack("<I", riff_size))
        f.write(b"WAVE")
        f.write(b"fmt ")
        f.write(struct.pack("<I", len(fmt)))
        f.write(fmt)
        f.write(b"data")
        f.write(struct.pack("<I", len(payload)))
        f.write(payload)


def fixture_frames(channels: int) -> list[tuple[float, ...]]:
    """Settling-aware finite corpus with long overload plateaus and edge probes."""
    tiny = float.fromhex("0x1.0p-1022")
    values: list[float] = []
    for level in (1.5, -1.5, 2.0, -2.0):
        values.extend([level] * DC_PLATEAU_INPUT_FRAMES)
    values.extend([0.0] * 256)
    values.extend([math.nextafter(1.0, 0.0)] * 512)
    values.extend([math.nextafter(1.0, math.inf)] * 512)
    values.extend([math.nextafter(-1.0, 0.0)] * 512)
    values.extend([math.nextafter(-1.0, -math.inf)] * 512)
    values.extend([0.0] * 128 + [2.0] + [0.0] * 127)
    values.extend([0.0] * 128 + [-2.0] + [0.0] * 127)
    values.extend([2.0 if i % 2 == 0 else -2.0 for i in range(1024)])
    values.extend([tiny if i % 2 == 0 else -tiny for i in range(512)])
    out: list[tuple[float, ...]] = []
    for i, value in enumerate(values):
        out.append(tuple(value if ch == 0 else value * (0.5 ** ch) for ch in range(channels)))
    return out


def binary32_probe_frames(channels: int, rounded: bool) -> list[tuple[float, ...]]:
    exact = 0.5 + 2.0 ** -40
    if rounded:
        exact = struct.unpack("<f", struct.pack("<f", exact))[0]
    return [tuple(exact * (1.0 if ch % 2 == 0 else -1.0) for ch in range(channels)) for _ in range(4096)]


def parse_w64(path: Path, expected_rate: int, expected_channels: int) -> dict[str, Any]:
    data = path.read_bytes()
    if len(data) < 40 or data[:16] != W64_RIFF_GUID or data[24:40] != W64_WAVE_GUID:
        raise ValueError("not an exact Wave64 RIFF/WAVE root")
    declared = struct.unpack_from("<Q", data, 16)[0]
    if declared != len(data):
        raise ValueError(f"Wave64 root declares {declared} bytes, physical file is {len(data)}")
    offset = 40
    fmt = None
    fact_frames = None
    payload = None
    data_offset = None
    chunks = 0
    while offset < len(data):
        if offset + 24 > len(data):
            raise ValueError("truncated Wave64 chunk header")
        guid = data[offset:offset + 16]
        size = struct.unpack_from("<Q", data, offset + 16)[0]
        if size < 24:
            raise ValueError("Wave64 chunk size is smaller than its header")
        end = offset + size
        if end > len(data):
            raise ValueError("Wave64 chunk extends beyond declared file")
        body = data[offset + 24:end]
        chunks += 1
        if guid == W64_FMT_GUID:
            if fmt is not None or len(body) not in (16, 40):
                raise ValueError("invalid/duplicate Wave64 fmt chunk")
            tag, ch, rate, byte_rate, align, bits = struct.unpack_from("<HHIIHH", body, 0)
            if tag == 0xFFFE:
                if len(body) != 40:
                    raise ValueError("invalid WAVEFORMATEXTENSIBLE length")
                valid_bits = struct.unpack_from("<H", body, 18)[0]
                subtype = body[24:40]
                floating = subtype == IEEE_FLOAT_SUBFORMAT_GUID
            else:
                valid_bits = bits
                floating = tag == 3
            fmt = dict(channels=ch, rate=rate, byte_rate=byte_rate, block_align=align,
                       bits=bits, valid_bits=valid_bits, floating=floating)
        elif guid == W64_FACT_GUID:
            if fact_frames is not None or len(body) != 8:
                raise ValueError("invalid/duplicate Wave64 fact chunk")
            fact_frames = struct.unpack_from("<Q", body, 0)[0]
        elif guid == W64_DATA_GUID:
            if payload is not None:
                raise ValueError("duplicate Wave64 data chunk")
            payload = body
            data_offset = offset + 24
        aligned = (end + 7) & ~7
        if any(data[end:aligned]):
            raise ValueError("non-zero Wave64 alignment padding")
        offset = aligned
    if offset != len(data) or fmt is None or payload is None:
        raise ValueError("incomplete Wave64 structure")
    if not fmt["floating"] or fmt["bits"] != 64 or fmt["valid_bits"] != 64:
        raise ValueError(f"Wave64 is not exact IEEE Float64: {fmt}")
    if fmt["rate"] != expected_rate or fmt["channels"] != expected_channels:
        raise ValueError(f"Wave64 geometry mismatch: {fmt}")
    expected_align = expected_channels * 8
    if fmt["block_align"] != expected_align or fmt["byte_rate"] != expected_rate * expected_align:
        raise ValueError("Wave64 block-align/byte-rate mismatch")
    if len(payload) % expected_align:
        raise ValueError("Wave64 payload is not whole-frame aligned")
    frames = len(payload) // expected_align
    if fact_frames is None or fact_frames != frames:
        raise ValueError("Float64 Wave64 fact chunk does not match payload frame count")
    return {
        "physical_file_bytes": len(data),
        "declared_file_bytes": declared,
        "chunk_count": chunks,
        "data_payload_offset": data_offset,
        "declared_data_bytes": len(payload),
        "sample_frames": frames,
        "payload_sha256": hashlib.sha256(payload).hexdigest(),
        "payload": payload,
    }


def analyze_payload(payload: bytes) -> dict[str, Any]:
    if len(payload) % 8:
        raise ValueError("Float64 payload is not 8-byte aligned")
    values = struct.unpack("<" + "d" * (len(payload) // 8), payload)
    finite = all(math.isfinite(v) for v in values)
    maximum = max(values) if values else 0.0
    minimum = min(values) if values else 0.0
    return {
        "finite": finite,
        "max": maximum,
        "min": minimum,
        "contains_over_full_scale_positive": maximum > 1.0,
        "contains_over_full_scale_negative": minimum < -1.0,
    }


def settled_dc_gain_check(
    payload: bytes,
    channels: int,
    src_rate: int,
    dst_rate: int,
    attenuation_db: float,
) -> dict[str, Any]:
    if channels <= 0 or len(payload) % (8 * channels):
        return {"passed": False, "error": "Float64 payload is not whole-frame aligned"}
    values = struct.unpack("<" + "d" * (len(payload) // 8), payload)
    frame_count = len(values) // channels
    expected_gain = 10.0 ** (-attenuation_db / 20.0)
    plateaus = []
    passed = True
    for plateau_index, level in enumerate((1.5, -1.5, 2.0, -2.0)):
        nominal_start = round(plateau_index * DC_PLATEAU_INPUT_FRAMES * dst_rate / src_rate)
        nominal_end = round((plateau_index + 1) * DC_PLATEAU_INPUT_FRAMES * dst_rate / src_rate)
        width = nominal_end - nominal_start
        guard = max(16, int(width * DC_SETTLE_FRACTION))
        settled_start = max(0, nominal_start + guard)
        settled_end = min(frame_count, nominal_end - guard)
        if settled_end <= settled_start:
            return {
                "passed": False,
                "error": "settled DC window is empty",
                "plateau_index": plateau_index,
            }
        for channel in range(channels):
            expected_input = level * (0.5 ** channel)
            settled = [
                values[frame * channels + channel]
                for frame in range(settled_start, settled_end)
            ]
            observed_mean = sum(settled) / len(settled)
            observed_gain = observed_mean / expected_input
            gain_error = abs(observed_gain - expected_gain)
            cell_passed = math.isfinite(observed_gain) and gain_error <= DC_GAIN_ABS_TOLERANCE
            passed = passed and cell_passed
            plateaus.append({
                "input_level": expected_input,
                "channel": channel,
                "settled_output_frame_range": [settled_start, settled_end],
                "observed_mean": observed_mean,
                "observed_gain": observed_gain,
                "expected_gain": expected_gain,
                "absolute_gain_error": gain_error,
                "passed": cell_passed,
            })
    return {
        "passed": passed,
        "settle_fraction_each_edge": DC_SETTLE_FRACTION,
        "absolute_gain_tolerance": DC_GAIN_ABS_TOLERANCE,
        "tolerance_rationale": (
            "The central half of each long DC plateau excludes transition/startup settling; "
            "a 1% absolute gain tolerance is deliberately loose relative to normal resampler "
            "DC accuracy while decisively rejecting gross hidden normalization or scaling."
        ),
        "expected_gain": expected_gain,
        "plateaus": plateaus,
    }


def _invoke_ssrc(
    exe: Path,
    cell: dict[str, Any],
    inp: Path,
    out: Path,
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ssrc_argv(exe, cell, inp, out),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        timeout=180,
    )


def run_tonepoet_w64_helper(
    helper: Path | None,
    w64_path: Path,
    expected_rate: int,
    expected_channels: int,
    expected_payload_sha256: str,
    work: Path,
) -> dict[str, Any]:
    if helper is None:
        return {
            "passed": False,
            "error": "Tonepoet exact W64 qualification helper unavailable",
        }
    raw = work / (w64_path.stem + ".tonepoet.f64le")
    proc = subprocess.run(
        [str(helper), str(w64_path), str(raw), str(expected_rate), str(expected_channels)],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        timeout=60,
    )
    result: dict[str, Any] = {
        "helper_path": str(helper),
        "returncode": proc.returncode,
        "stdout_tail": proc.stdout[-4096:],
        "stderr_tail": proc.stderr[-4096:],
        "expected": {
            "sample_rate_hz": expected_rate,
            "channels": expected_channels,
            "bits_per_sample": 64,
            "encoding": "floating_point",
        },
    }
    if proc.returncode != 0 or not raw.exists():
        result["passed"] = False
        return result
    try:
        helper_metadata = json.loads(proc.stdout.strip().splitlines()[-1])
    except (IndexError, json.JSONDecodeError) as exc:
        result.update({"passed": False, "error": f"helper emitted invalid JSON metadata: {exc}"})
        return result
    payload_sha256 = sha256_file(raw)
    payload_identity = payload_sha256 == expected_payload_sha256
    result.update({
        "helper_metadata": helper_metadata,
        "payload_sha256": payload_sha256,
        "matches_independent_payload_sha256": payload_identity,
        "passed": bool(
            payload_identity
            and helper_metadata.get("sample_rate_hz") == expected_rate
            and helper_metadata.get("channels") == expected_channels
            and helper_metadata.get("bits_per_sample") == 64
            and helper_metadata.get("encoding") == "floating_point"
        ),
    })
    return result


def run_ssrc(
    exe: Path,
    profile: str,
    src_rate: int,
    dst_rate: int,
    channels: int,
    work: Path,
    tonepoet_w64_helper: Path | None,
) -> dict[str, Any]:
    inp = work / f"in-{src_rate}-{channels}.wav"
    out = work / f"out-{profile}-{src_rate}-{dst_rate}-{channels}.w64"
    write_float64_wav(inp, src_rate, channels, fixture_frames(channels))
    cell = characterization_cell(profile, src_rate, dst_rate, channels)
    argv = ssrc_argv(exe, cell, inp, out)
    proc = _invoke_ssrc(exe, cell, inp, out)
    result: dict[str, Any] = {
        "profile": profile,
        "source_rate_hz": src_rate,
        "target_rate_hz": dst_rate,
        "channels": channels,
        "cell": cell,
        "argv": argv[1:],
        "returncode": proc.returncode,
        "stdout_tail": proc.stdout[-4096:],
        "stderr_tail": proc.stderr[-4096:],
    }
    if proc.returncode != 0:
        result["passed"] = False
        return result
    structure = parse_w64(out, dst_rate, channels)
    copied = work / (out.stem + ".f64le")
    copied.write_bytes(structure["payload"])
    payload_identity = sha256_file(copied) == structure["payload_sha256"]
    tonepoet_physical_cell = run_tonepoet_w64_helper(
        tonepoet_w64_helper,
        out,
        dst_rate,
        channels,
        structure["payload_sha256"],
        work,
    )
    analysis = analyze_payload(structure["payload"])
    dc_gain = settled_dc_gain_check(
        structure["payload"],
        channels,
        src_rate,
        dst_rate,
        float(cell["attenuation_db"]),
    )

    exact_probe = work / f"probe-exact-{src_rate}-{channels}.wav"
    rounded_probe = work / f"probe-f32-{src_rate}-{channels}.wav"
    exact_out = work / f"probe-exact-{profile}-{src_rate}-{dst_rate}-{channels}.w64"
    rounded_out = work / f"probe-f32-{profile}-{src_rate}-{dst_rate}-{channels}.w64"
    write_float64_wav(exact_probe, src_rate, channels, binary32_probe_frames(channels, False))
    write_float64_wav(rounded_probe, src_rate, channels, binary32_probe_frames(channels, True))
    exact_argv = ssrc_argv(exe, cell, exact_probe, exact_out)
    rounded_argv = ssrc_argv(exe, cell, rounded_probe, rounded_out)
    exact_proc = _invoke_ssrc(exe, cell, exact_probe, exact_out)
    rounded_proc = _invoke_ssrc(exe, cell, rounded_probe, rounded_out)
    narrowing_probe_distinct = False
    narrowing_probe_error = None
    if exact_proc.returncode == 0 and rounded_proc.returncode == 0:
        exact_structure = parse_w64(exact_out, dst_rate, channels)
        rounded_structure = parse_w64(rounded_out, dst_rate, channels)
        narrowing_probe_distinct = exact_structure["payload_sha256"] != rounded_structure["payload_sha256"]
    else:
        narrowing_probe_error = {
            "exact_returncode": exact_proc.returncode,
            "rounded_returncode": rounded_proc.returncode,
        }

    overload_preserved = (
        analysis["contains_over_full_scale_positive"]
        and analysis["contains_over_full_scale_negative"]
        and analysis["max"] > 1.25
        and analysis["min"] < -1.25
    )
    result.update({
        "w64": {k: v for k, v in structure.items() if k != "payload"},
        "payload_identity": payload_identity,
        "tonepoet_physical_cell": tonepoet_physical_cell,
        "analysis": analysis,
        "overload_preserved": overload_preserved,
        "settled_dc_gain": dc_gain,
        "binary32_rounding_falsifier": {
            "distinct_output": narrowing_probe_distinct,
            "error": narrowing_probe_error,
            "exact_argv": exact_argv[1:],
            "rounded_argv": rounded_argv[1:],
        },
        "passed": bool(
            payload_identity
            and tonepoet_physical_cell.get("passed")
            and analysis["finite"]
            and overload_preserved
            and dc_gain.get("passed")
            and narrowing_probe_distinct
        ),
    })
    return result


def load_static_audit(path: Path | None) -> dict[str, Any] | None:
    if path is None:
        return None
    return json.loads(path.read_text())


def static_source_audit_complete(static_audit: dict[str, Any] | None) -> bool:
    if not static_audit:
        return False
    review = static_audit.get("source_review", {})
    sleef = review.get("sleef_dft_type_path_audit", {})
    return bool(review.get("passed") and sleef.get("passed"))


def audit_identity_consistency(
    static_audit: dict[str, Any] | None,
    build_closure: str | None,
    compiler_config: str | None,
    executable_sha256: str | None,
) -> tuple[bool, list[str]]:
    if not static_audit:
        return False, ["static/build audit unavailable"]
    mismatches: list[str] = []
    expected_source = {
        "owner": "barstoolbluz",
        "repo": "ssrc",
        "rev": SSRC_REV,
        "nar_hash": SSRC_NAR,
    }
    if static_audit.get("source_identity") != expected_source:
        mismatches.append("static audit source identity does not match the pinned SSRC source")
    if static_audit.get("dependency_identity", {}).get("sleef_rev") != SLEEF_REV:
        mismatches.append("static audit SLEEF identity does not match the pinned dependency")
    build = static_audit.get("build_audit", {})
    if not build_closure or not build.get("build_closure"):
        mismatches.append("build closure identity is incomplete in the audit or characterized report")
    elif build.get("build_closure") != build_closure:
        mismatches.append("build audit closure does not match the characterized report closure")
    if not compiler_config or not build.get("compiler_configuration"):
        mismatches.append("compiler/build configuration identity is incomplete in the audit or characterized report")
    elif build.get("compiler_configuration") != compiler_config:
        mismatches.append("build audit compiler configuration does not match the characterized report")
    if not executable_sha256 or not build.get("executable_sha256"):
        mismatches.append("executable digest identity is incomplete in the audit or characterized report")
    elif build.get("executable_sha256") != executable_sha256:
        mismatches.append("build audit executable digest does not match the characterized executable")
    return not mismatches, mismatches


def qualification_is_promotable(
    unresolved: list[str],
    source_audit_ok: bool,
    build_audit_ok: bool,
    audit_identity_ok: bool,
    results: list[dict[str, Any]],
) -> bool:
    physical_cell_ok = bool(results) and all(
        r.get("tonepoet_physical_cell", {}).get("passed") for r in results
    )
    return bool(
        not unresolved
        and source_audit_ok
        and build_audit_ok
        and audit_identity_ok
        and results
        and physical_cell_ok
        and all(r.get("passed") for r in results)
    )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--ssrc", type=Path, help="exact SSRC executable to characterize")
    parser.add_argument(
        "--tonepoet-w64-helper",
        type=Path,
        help="compiled tonepoet-pipeline ssrc_w64_qualification helper",
    )
    parser.add_argument("--static-audit", type=Path, default=Path(__file__).with_name("static_audit_rev4.json"))
    parser.add_argument("--tonepoet-source-identity", help="archive/git identity of the implementation under qualification")
    parser.add_argument("--build-closure", help="Nix derivation/store closure or equivalent reproducible build identity")
    parser.add_argument("--compiler-config", help="compiler/build configuration bound to the executable digest")
    parser.add_argument("--version", help="known SSRC version/build metadata; otherwise --version is probed")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--characterize", action="store_true", help="run the full finite fixture matrix when an executable is present")
    args = parser.parse_args()

    static_audit = load_static_audit(args.static_audit)
    exe = args.ssrc or (Path(shutil.which("ssrc")) if shutil.which("ssrc") else None)
    helper = args.tonepoet_w64_helper
    helper_identity = None
    if helper is not None and helper.exists():
        helper = helper.resolve()
        helper_identity = {"path": str(helper), "sha256": sha256_file(helper)}
    else:
        helper = None
    exe_identity = None
    version = args.version
    unresolved: list[str] = []
    results: list[dict[str, Any]] = []

    if exe is None or not exe.exists():
        unresolved.append("exact SSRC executable unavailable")
    else:
        exe = exe.resolve()
        exe_identity = {"qualification_path": str(exe), "sha256": sha256_file(exe)}
        if version is None:
            for flag in ("--version", "-v"):
                try:
                    p = subprocess.run([str(exe), flag], stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                       text=True, timeout=10)
                    text = (p.stdout + "\n" + p.stderr).strip()
                    if text:
                        version = text.splitlines()[0][:512]
                        break
                except Exception:
                    pass
        if args.characterize:
            with tempfile.TemporaryDirectory(prefix="tonepoet-ssrc-binary64-") as td:
                work = Path(td)
                for profile in DOUBLE_PROFILES:
                    for src_rate, dst_rate in RATE_PAIRS:
                        for channels in (1, 2):
                            try:
                                results.append(
                                    run_ssrc(
                                        exe, profile, src_rate, dst_rate, channels, work, helper
                                    )
                                )
                            except Exception as exc:
                                results.append({"profile": profile, "source_rate_hz": src_rate,
                                                "target_rate_hz": dst_rate, "channels": channels,
                                                "passed": False, "error": str(exc)})
        else:
            unresolved.append("finite executable characterization not requested/run")

    source_audit_ok = static_source_audit_complete(static_audit)
    build_audit_declared_ok = bool(static_audit and static_audit.get("build_audit", {}).get("passed"))
    audit_identity_ok, audit_identity_mismatches = audit_identity_consistency(
        static_audit,
        args.build_closure,
        args.compiler_config,
        exe_identity.get("sha256") if exe_identity is not None else None,
    )
    build_audit_ok = build_audit_declared_ok and audit_identity_ok
    if not source_audit_ok:
        unresolved.append("static source audit incomplete")
    if not build_audit_declared_ok:
        unresolved.append("exact build audit incomplete")
    elif not audit_identity_ok:
        unresolved.extend(audit_identity_mismatches)
    if helper_identity is None:
        unresolved.append("Tonepoet exact W64 qualification helper unavailable")
    if not args.tonepoet_source_identity:
        unresolved.append("Tonepoet source/archive identity not supplied")
    if not args.build_closure:
        unresolved.append("exact build closure identity not supplied")
    if not args.compiler_config:
        unresolved.append("compiler/build configuration not supplied")
    if exe_identity is not None and not version:
        unresolved.append("SSRC version/build metadata unavailable")
    if args.characterize and results and not all(r.get("tonepoet_physical_cell", {}).get("passed") for r in results):
        unresolved.append("one or more Tonepoet protected physical-cell checks failed")
    if args.characterize and results and not all(r.get("passed") for r in results):
        unresolved.append("one or more finite characterization cells failed")
    if exe_identity is not None and args.characterize and not results:
        unresolved.append("finite characterization produced no results")

    # The current static audit intentionally leaves exact-build proof incomplete.
    # Therefore this implementation run is expected to remain Pending/Outcome C.
    promotable = qualification_is_promotable(unresolved, source_audit_ok, build_audit_ok, audit_identity_ok, results)
    outcome = "bounded_established" if promotable else "pending"
    decision = "candidate_for_review" if promotable else "outcome_c_evidence_unavailable"

    script_digest = sha256_file(Path(__file__))
    report: dict[str, Any] = {
        "schema_version": 1,
        "contract_id": CONTRACT_ID,
        "outcome": outcome,
        "evidence_identity": {
            "scheme": "sha256",
            "subject": "finalized qualification report bytes",
            "note": "The concrete EvidenceIdentity is emitted after the report is finalized to avoid self-reference.",
        },
        "tonepoet_source_identity": args.tonepoet_source_identity,
        "source_identity": {"owner": "barstoolbluz", "repo": "ssrc", "rev": SSRC_REV, "nar_hash": SSRC_NAR},
        "dependency_identity": {"sleef_rev": SLEEF_REV, "build_closure": args.build_closure},
        "executable": {**(exe_identity or {"qualification_path": None, "sha256": None}),
                       "version": version, "build_info": args.compiler_config},
        "platform": {"os": platform.system(), "arch": platform.machine()},
        "ssrc_scope": {
            "profiles": list(DOUBLE_PROFILES),
            "phase_modes": ["minimum" if QUALIFICATION_MIN_PHASE else "linear"],
            "attenuation": render_attenuation_db(QUALIFICATION_ATTENUATION_DB),
            "input_container": "wav", "input_format": "pcm_f64le",
            "output_container": "w64", "output_format": "pcm_f64le",
            "rate_scope": [list(pair) for pair in RATE_PAIRS], "architecture_scope": [platform.machine()],
            "single_precision_refuted_profiles": list(SINGLE_PROFILES)
        },
        "activation_path": {
            "protected_ingress_authority_id": INGRESS_AUTHORITY,
            "protected_ingress_boundary_contract": "source-rate/channel-preserving IEEE Float64 RIFF WAV; no gain/dither/mix/resample"
        },
        "static_audit": static_audit,
        "audit_identity_consistency": {
            "passed": audit_identity_ok,
            "mismatches": audit_identity_mismatches,
        },
        "harness": {"source_digest": script_digest, "fixtures": "deterministic settling-aware Float64 overload/narrowing/multichannel corpus",
                    "tolerances": {"w64_structure": "exact", "payload_identity": "exact", "finite": "exact predicate",
                                   "overload": "requires output extrema beyond +/-1.25 from +/-1.5 and +/-2.0 plateaus",
                                   "settled_dc_gain": {
                                       "absolute_gain_tolerance": DC_GAIN_ABS_TOLERANCE,
                                       "settle_fraction_each_edge": DC_SETTLE_FRACTION,
                                       "rationale": "central-plateau gain must match the declared attenuation closely enough to reject gross hidden normalization",
                                   },
                                   "binary32_narrowing": "exact payload inequality against binary32-rounded control"}, "results": results},
        "container_validation": {
            "independent_w64_structure": "exact parser executed per characterized cell" if results else "not_run",
            "independent_payload_identity": "exact SHA-256 copy check per characterized cell" if results else "not_run",
            "tonepoet_exact_w64_physical_cell": {
                "required": True,
                "helper": helper_identity,
                "all_characterized_cells_passed": bool(results) and all(
                    r.get("tonepoet_physical_cell", {}).get("passed") for r in results
                ),
                "implementation": "tonepoet-pipeline::copy_exact_w64_pcm_payload",
            },
        },
        "commissioning": {"decision": decision, "reason": unresolved or ["all finite/static/build prerequisites supplied; review still required before registry update"]}
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    report_sha256 = sha256_file(args.output)
    evidence_id = f"sha256:{report_sha256}"
    identity_path = args.output.with_name(args.output.name + ".identity.json")
    identity_path.write_text(
        json.dumps(
            {
                "evidence_id": evidence_id,
                "qualification_report_sha256": report_sha256,
                "qualification_report": str(args.output),
                "outcome": outcome,
            },
            indent=2,
            sort_keys=True,
        )
        + "\n"
    )
    print(json.dumps({
        "outcome": outcome,
        "output": str(args.output),
        "identity_output": str(identity_path),
        "evidence_id": evidence_id,
        "qualification_report_sha256": report_sha256,
        "unresolved": unresolved,
    }, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
