#!/usr/bin/env python3
"""Execution qualification for Tonepoet's SSRC-owned true-peak terminal.

This harness is intentionally outside Cargo/runtime admission.  It characterizes
one or more exact physical SSRC terminal cells, binds the run to the already
commissioned Binary64 SSRC executable/source/build identity, and emits evidence
for explicit review.  It never edits the production registry.

The reference for each gain point is produced by the already-commissioned
Float64 SSRC observation command.  The terminal then replays the exact same
Float64 ingress through the same executable with Tonepoet's production
`--mixChannels` matrix and the selected final PCM representation. Integer cells
use the selected SSRC dither/PDF and fixed seed; floating-point cells use SSRC's
`--bits -32/-64` modes with dither/PDF inactive. Integer error is measured in
target LSBs, while float error is measured as absolute linear full scale, both
against `observation_sample * production_directed_gain`.

Finite execution characterization is not silently promoted into authority.  A
separate promotion script validates the finalized report identity and produces
the source-controlled registry records for review.
"""
from __future__ import annotations

import argparse
import concurrent.futures
import dataclasses
import decimal
import hashlib
import json
import math
import os
import platform
import struct
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any, Iterable

CONTRACT_ID = "tonepoet:ssrc-true-peak-replay-terminal/v1"
GAIN_MODEL_ID = "tonepoet:ssrc-mixchannels-diagonal-gain/v1"
PINNED_SSRC_SOURCE_REV = "6b0bbfe1fff79c0399347f4e1fb027c9931ef6ea"
DITHER_SEED = 1
SCHEMA_VERSION = 1
INTEGER_BOUND_POLICY = "whole_lsb_ceiling(max(8, 2*observed_max_abs_error_lsb + 4))"
FLOAT_BOUND_POLICY = "nextafter(max(8*float_unit, 2*observed_max_abs_error_linear + 4*float_unit), +inf)"
DEFAULT_GAINS = ("-24.000000000", "-12.000000000", "-6.000000000", "-1.000000000",
                 "0.000000000", "1.000000000", "6.000000000", "12.000000000", "24.000000000")

W64_RIFF_GUID = b"riff.\x91\xcf\x11\xa5\xd6\x28\xdb\x04\xc1\x00\x00"
W64_WAVE_GUID = b"wave\xf3\xac\xd3\x11\x8c\xd1\x00\xc0O\x8e\xdb\x8a"
W64_FMT_GUID = b"fmt \xf3\xac\xd3\x11\x8c\xd1\x00\xc0O\x8e\xdb\x8a"
W64_DATA_GUID = b"data\xf3\xac\xd3\x11\x8c\xd1\x00\xc0O\x8e\xdb\x8a"
PCM_SUBFORMAT_GUID = b"\x01\x00\x00\x00\x00\x00\x10\x00\x80\x00\x00\xaa\x00\x38\x9b\x71"
FLOAT_SUBFORMAT_GUID = b"\x03\x00\x00\x00\x00\x00\x10\x00\x80\x00\x00\xaa\x00\x38\x9b\x71"

TONEPOET_AUTHORITY_FILES = (
    "tonepoet-pipeline/src/ssrc_true_peak_terminal.rs",
    "tonepoet-pipeline/src/bin/ssrc_true_peak_gain_qualification.rs",
    "tonepoet-pipeline/src/dsd_album_gain.rs",
    "tonepoet-pipeline/src/mapping.rs",
    "tonepoet-pipeline/src/plugins.rs",
    "tonepoet-pipeline/src/ssrc_binary64.rs",
    "tonepoet-pipeline/src/w64.rs",
    "tonepoet-pipeline/qualification/ssrc_true_peak_terminal/qualify_ssrc_true_peak_terminal.py",
    "tonepoet-pipeline/qualification/ssrc_true_peak_terminal/promote_ssrc_true_peak_terminal.py",
    "src/convert/pipeline/stages.rs",
    "src/convert/pipeline/plan_bridge.rs",
    "src/convert/pipeline/types.rs",
)


@dataclasses.dataclass(frozen=True)
class Cell:
    profile: str
    source_rate_hz: int
    target_rate_hz: int
    channels: int
    bits: int
    dither_id: int | None
    pdf: str | None

    def key(self) -> str:
        return (
            f"{self.profile}:{self.source_rate_hz}:{self.target_rate_hz}:"
            f"{self.channels}:{self.bits}:"
            f"{self.dither_id if self.dither_id is not None else 'none'}:"
            f"{self.pdf if self.pdf is not None else 'none'}"
        )

    def is_float(self) -> bool:
        return self.bits < 0

    def float_bits(self) -> int:
        if not self.is_float():
            raise ValueError("integer qualification cell has no floating-point width")
        return -self.bits

    def target_bit_depth(self) -> str:
        return f"Float{self.float_bits()}" if self.is_float() else f"Int{self.bits}"

    def scope(self, arch: str) -> dict[str, Any]:
        return {
            "profile": self.profile,
            "source_rate_hz": self.source_rate_hz,
            "target_rate_hz": self.target_rate_hz,
            "channels": self.channels,
            "target_bit_depth": self.target_bit_depth(),
            "dither_id": self.dither_id,
            "pdf_type": self.pdf,
            "base_attenuation_db": "0.0",
            "min_phase": False,
            "architecture": arch,
        }


def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for block in iter(lambda: f.read(1024 * 1024), b""):
            h.update(block)
    return h.hexdigest()


def tonepoet_source_authority(root: Path) -> dict[str, str]:
    root = root.resolve()
    result: dict[str, str] = {}
    for relative in TONEPOET_AUTHORITY_FILES:
        path = root / relative
        if not path.is_file():
            raise ValueError(f"Tonepoet authority file is missing: {relative}")
        result[relative] = sha256_file(path)
    return result


def normalize_arch(raw: str) -> str:
    value = raw.lower().replace("-", "_")
    if value in {"amd64", "x64"}:
        return "x86_64"
    if value in {"arm64", "aarch64"}:
        return "aarch64"
    return value


def db_nano(raw: str) -> int:
    value = decimal.Decimal(raw)
    scaled = value * decimal.Decimal(1_000_000_000)
    if scaled != scaled.to_integral_value(rounding=decimal.ROUND_DOWN):
        raise ValueError(f"gain {raw!r} is not exactly representable as nanodecibels")
    return int(scaled)


def parse_cell(raw: str) -> Cell:
    """Parse PROFILE:SRC:DST:CHANNELS:BITS:DITHER:PDF.

    BITS uses 8/16/24/32 for integer PCM and SSRC-native -32/-64 for
    Float32/Float64. DITHER/PDF accept `none`; float cells require both `none`.
    Examples:
      high:176400:88200:2:24:2:triangular
      long:96000:44100:2:32:none:none
      high:96000:44100:2:-32:none:none
    """
    parts = raw.split(":")
    if len(parts) != 7:
        raise ValueError("cell must contain seven colon-separated fields")
    profile, src, dst, channels, bits, dither, pdf = parts
    if profile not in {"high", "long", "insane"}:
        raise ValueError("terminal qualification is restricted to commissioned double profiles")
    cell = Cell(
        profile=profile,
        source_rate_hz=int(src),
        target_rate_hz=int(dst),
        channels=int(channels),
        bits=int(bits),
        dither_id=None if dither == "none" else int(dither),
        pdf=None if pdf == "none" else pdf,
    )
    if cell.source_rate_hz <= 0 or cell.target_rate_hz <= 0 or not (1 <= cell.channels <= 64):
        raise ValueError("cell has invalid audio geometry")
    if cell.bits not in {8, 16, 24, 32, -32, -64}:
        raise ValueError("terminal cell bit depth must be one of 8, 16, 24, 32, -32, -64")
    if cell.pdf not in {None, "rectangular", "triangular"}:
        raise ValueError("PDF must be none, rectangular, or triangular")
    if cell.pdf is not None and cell.dither_id is None:
        raise ValueError("PDF cannot be active without a dither ID")
    if cell.is_float() and (cell.dither_id is not None or cell.pdf is not None):
        raise ValueError("floating-point terminal cells require dither and PDF to be none")
    return cell


SSRC_DITHER_PROBE_IDS = tuple(range(17)) + (90, 91, 92, 98, 99)


def _unsupported_dither_for_rate(stderr: str, dither_id: int, target_rate_hz: int) -> bool:
    text = " ".join(stderr.split())
    return (
        f"Dither type {dither_id}" in text
        and "is not available for destination sampling frequency" in text
        and f"{target_rate_hz}Hz" in text
    )


def discover_ssrc_dither_capabilities(
    exe: Path,
    helper: Path,
    target_rates_hz: Iterable[int],
    root: Path,
) -> dict[int, tuple[int, ...]]:
    """Probe the exact commissioned executable before enumerating production cells.

    SSRC's dither tables are destination-rate specific, including its nominal
    no-shaper ID 99. Probe the finite set of IDs admitted by Tonepoet's
    runtime mapping with a tiny deterministic carrier. A documented
    rate-unavailable diagnostic means that physical cell is omitted; every
    other execution failure is fatal so qualification cannot silently shrink
    its evidence grid for an unrelated reason.
    """
    root.mkdir(parents=True, exist_ok=True)
    capabilities: dict[int, tuple[int, ...]] = {}
    for target_rate_hz in sorted(set(int(rate) for rate in target_rates_hz)):
        inp = root / f"probe-{target_rate_hz}.wav"
        out = root / f"probe-{target_rate_hz}.w64"
        probe_frames = fixture_frames(1, 1.0)[:64]
        probe_source_rate_hz = 48_000 if target_rate_hz != 48_000 else 44_100
        write_float64_wav(inp, probe_source_rate_hz, 1, probe_frames)
        supported: list[int] = []
        for dither_id in SSRC_DITHER_PROBE_IDS:
            try:
                out.unlink()
            except FileNotFoundError:
                pass
            argv = [
                str(exe), "--rate", str(target_rate_hz), "--profile", "high",
                "--bits", "16", "--att", "0.0", "--dither", str(dither_id),
                "--seed", str(DITHER_SEED), "--pdf", "1", "--dstContainer", "w64",
                str(inp), str(out),
            ]
            proc = invoke(argv)
            if proc.returncode == 0:
                if not out.is_file():
                    raise RuntimeError(
                        f"SSRC dither capability probe succeeded without output for "
                        f"{target_rate_hz} Hz / ID {dither_id}"
                    )
                parse_w64(out, target_rate_hz, 1, helper)
                supported.append(dither_id)
                continue
            if _unsupported_dither_for_rate(proc.stderr, dither_id, target_rate_hz):
                continue
            raise RuntimeError(
                f"SSRC dither capability probe failed unexpectedly for {target_rate_hz} Hz / "
                f"ID {dither_id}: exit {proc.returncode}: {proc.stderr[-2048:]}"
            )
        capabilities[target_rate_hz] = tuple(supported)
    return capabilities


def global_dither_cells_for_rate(
    target_rate_hz: int,
    capabilities: dict[int, tuple[int, ...]],
) -> tuple[tuple[int | None, str | None], ...]:
    # These are the unique physical cells reachable from ordinary global Tonepoet
    # dither choices. Explicit SSRC-native overrides are intentionally not
    # generalized: qualify them with --cell so the actual native selection is
    # explicit in evidence. The exact executable decides which IDs exist.
    if target_rate_hz not in capabilities:
        raise ValueError(f"missing SSRC dither capability probe for {target_rate_hz} Hz")
    supported = set(capabilities[target_rate_hz])
    cells: list[tuple[int | None, str | None]] = [(None, None)]
    if 99 in supported:
        cells.append((99, "triangular"))
    # LowShibata and named-shaper approximations choose ATH Curve A intensity 0.
    if 0 in supported:
        cells.append((0, "triangular"))
    # Shibata and HighShibata clamp to the strongest available Curve A intensity
    # at or below their requested intensities (2 and 6 respectively).
    for ceiling in (2, 6):
        available = [value for value in supported if 0 <= value <= ceiling]
        if available:
            cell = (max(available), "triangular")
            if cell not in cells:
                cells.append(cell)
    return tuple(cells)


def production_cells(
    binary64: dict[str, Any],
    channels: Iterable[int],
    dither_capabilities: dict[int, tuple[int, ...]],
) -> list[Cell]:
    scope = binary64.get("ssrc_scope", {})
    profiles = tuple(scope.get("profiles", ()))
    rates = tuple(tuple(pair) for pair in scope.get("rate_scope", ()))
    out: set[Cell] = set()
    for profile in profiles:
        if profile not in {"high", "long", "insane"}:
            continue
        for src, dst in rates:
            for ch in channels:
                for bits in (8, 16, 24, 32):
                    # The certified terminal has its own exact execution evidence,
                    # so Int32 dither cells are qualified here rather than inheriting
                    # the ordinary plugin's historical pre-qualification gate.
                    for dither_id, pdf in global_dither_cells_for_rate(
                        int(dst), dither_capabilities
                    ):
                        out.add(Cell(profile, int(src), int(dst), int(ch), bits, dither_id, pdf))
                # Direct SSRC float output already suppresses dither/noise shaping.
                # Commission exactly those two native terminal modes.
                out.add(Cell(profile, int(src), int(dst), int(ch), -32, None, None))
                out.add(Cell(profile, int(src), int(dst), int(ch), -64, None, None))
    return sorted(out, key=lambda cell: cell.key())


def write_float64_wav(path: Path, rate: int, channels: int, frames: list[tuple[float, ...]]) -> None:
    payload = b"".join(struct.pack("<" + "d" * channels, *frame) for frame in frames)
    block_align = channels * 8
    fmt = struct.pack("<HHIIHH", 3, channels, rate, rate * block_align, block_align, 64)
    riff_size = 4 + 8 + len(fmt) + 8 + len(payload)
    if riff_size > 0xFFFFFFFF:
        raise ValueError("qualification fixture unexpectedly exceeds RIFF capacity")
    with path.open("wb") as f:
        f.write(b"RIFF")
        f.write(struct.pack("<I", riff_size))
        f.write(b"WAVEfmt ")
        f.write(struct.pack("<I", len(fmt)))
        f.write(fmt)
        f.write(b"data")
        f.write(struct.pack("<I", len(payload)))
        f.write(payload)


def fixture_frames(channels: int, scalar: float) -> list[tuple[float, ...]]:
    """Deterministic low-band periodic multitone safe across the full rate grid.

    Integer DFT bins make the fixture periodic at its boundary and put every
    component well below the narrowest destination Nyquist limit in the Binary64
    production grid. Channel discrimination is constant per channel (phase,
    polarity, and level), never a per-sample sign alternation that can synthesize
    source-Nyquist energy.
    """
    if channels < 1:
        raise ValueError("qualification fixture requires at least one channel")
    if not math.isfinite(scalar) or scalar < 0.0:
        raise ValueError("qualification fixture received an invalid gain scalar")
    frame_count = 16_384
    bins = (37, 149, 509)
    weights = (0.50, 0.30, 0.20)
    amplitude = 0.30 / max(1.0, scalar)
    frames: list[tuple[float, ...]] = []
    tau = 2.0 * math.pi
    for i in range(frame_count):
        row: list[float] = []
        for channel in range(channels):
            phase_base = (channel + 1) * math.pi / 11.0
            polarity = -1.0 if channel & 1 else 1.0
            level = 1.0 / (1.0 + 0.25 * channel)
            value = 0.0
            for tone_index, (bin_index, weight) in enumerate(zip(bins, weights)):
                phase = phase_base * (tone_index + 1)
                value += weight * math.sin(tau * bin_index * i / frame_count + phase)
            row.append(amplitude * polarity * level * value)
        frames.append(tuple(row))
    return frames


def _parse_w64_bytes(
    data: bytes,
    expected_rate: int,
    expected_channels: int,
    allow_final_data_tail_candidate: bool,
) -> dict[str, Any]:
    if len(data) < 40 or data[:16] != W64_RIFF_GUID or data[24:40] != W64_WAVE_GUID:
        raise ValueError("not an exact Wave64 RIFF/WAVE root")
    declared = struct.unpack_from("<Q", data, 16)[0]
    if declared != len(data):
        raise ValueError(f"Wave64 root declares {declared} bytes, physical file is {len(data)}")
    offset = 40
    fmt: dict[str, Any] | None = None
    payload: bytes | None = None
    ssrc_trailing_padding_bytes = 0
    while offset < len(data):
        if offset + 24 > len(data):
            raise ValueError("truncated Wave64 chunk header")
        guid = data[offset:offset + 16]
        size = struct.unpack_from("<Q", data, offset + 16)[0]
        if size < 24 or offset + size > len(data):
            raise ValueError("invalid Wave64 chunk size")
        body = data[offset + 24:offset + size]
        if guid == W64_FMT_GUID:
            if fmt is not None or len(body) not in {16, 18, 40}:
                raise ValueError("invalid or duplicate Wave64 fmt chunk")
            tag, ch, rate, byte_rate, align, bits = struct.unpack_from("<HHIIHH", body, 0)
            encoding = "unknown"
            valid_bits = bits
            if tag == 1:
                encoding = "pcm_integer"
            elif tag == 3:
                encoding = "pcm_float"
            elif tag == 0xFFFE and len(body) == 40:
                valid_bits = struct.unpack_from("<H", body, 18)[0]
                subtype = body[24:40]
                if subtype == PCM_SUBFORMAT_GUID:
                    encoding = "pcm_integer"
                elif subtype == FLOAT_SUBFORMAT_GUID:
                    encoding = "pcm_float"
            fmt = {
                "tag": tag, "channels": ch, "rate": rate, "byte_rate": byte_rate,
                "block_align": align, "bits": bits, "valid_bits": valid_bits,
                "encoding": encoding,
            }
        elif guid == W64_DATA_GUID:
            if payload is not None:
                raise ValueError("duplicate Wave64 data chunk")
            payload = body
        end = offset + size
        # Candidate extraction deliberately does not decide whether a final
        # SSRC tail is admissible. Any residual bytes after `data` are exposed
        # to the production Rust helper, which owns the zero-only policy.
        if allow_final_data_tail_candidate and guid == W64_DATA_GUID and end < len(data):
            ssrc_trailing_padding_bytes = len(data) - end
            offset = len(data)
            break
        aligned = (end + 7) & ~7
        if aligned > len(data) or any(data[end:aligned]):
            raise ValueError("invalid Wave64 alignment padding")
        offset = aligned
    if offset != len(data) or fmt is None or payload is None:
        raise ValueError("incomplete Wave64 file")
    if fmt["rate"] != expected_rate or fmt["channels"] != expected_channels:
        raise ValueError(f"Wave64 geometry mismatch: {fmt}")
    if fmt["block_align"] <= 0 or len(payload) % fmt["block_align"]:
        raise ValueError("Wave64 data is not whole-frame aligned")
    return {
        "format": fmt,
        "payload": payload,
        "payload_sha256": hashlib.sha256(payload).hexdigest(),
        "frames": len(payload) // fmt["block_align"],
        "file_sha256": hashlib.sha256(data).hexdigest(),
        "ssrc_trailing_padding_bytes": ssrc_trailing_padding_bytes,
    }


def _admit_ssrc_w64_with_production_helper(
    helper: Path,
    path: Path,
    expected_rate: int,
    expected_channels: int,
    candidate: dict[str, Any],
) -> None:
    fmt = candidate["format"]
    encoding = fmt["encoding"]
    if encoding not in {"pcm_integer", "pcm_float"}:
        raise ValueError(f"cannot delegate unknown Wave64 encoding to production: {encoding}")
    helper_encoding = "signed_integer" if encoding == "pcm_integer" else "floating_point"
    proc = invoke([
        str(helper), "--inspect-ssrc-w64", str(path), str(expected_rate),
        str(expected_channels), str(fmt["bits"]), helper_encoding,
    ])
    if proc.returncode != 0:
        raise ValueError(f"production SSRC Wave64 validator rejected carrier: {proc.stderr[-2048:]}")
    try:
        admitted = json.loads(proc.stdout)
    except json.JSONDecodeError as exc:
        raise ValueError("production SSRC Wave64 validator returned invalid JSON") from exc
    expected = {
        "sample_frames": candidate["frames"],
        "declared_data_bytes": len(candidate["payload"]),
        "physical_file_bytes": path.stat().st_size,
        "trailing_bytes_after_data": candidate["ssrc_trailing_padding_bytes"],
    }
    for key, value in expected.items():
        if admitted.get(key) != value:
            raise ValueError(
                f"production SSRC Wave64 validator disagrees on {key}: "
                f"reported {admitted.get(key)!r}, Python extraction found {value!r}"
            )


def parse_w64(
    path: Path,
    expected_rate: int,
    expected_channels: int,
    helper: Path | None = None,
) -> dict[str, Any]:
    data = path.read_bytes()
    try:
        return _parse_w64_bytes(data, expected_rate, expected_channels, False)
    except ValueError as exact_error:
        try:
            candidate = _parse_w64_bytes(data, expected_rate, expected_channels, True)
        except ValueError as ssrc_error:
            raise ValueError(
                f"Wave64 failed exact parsing ({exact_error}) and SSRC trailing-zero "
                f"candidate parsing ({ssrc_error})"
            ) from ssrc_error
        if helper is None:
            raise ValueError(
                "Wave64 requires the SSRC compatibility path, but no production "
                "qualification helper was supplied"
            ) from exact_error
        _admit_ssrc_w64_with_production_helper(
            helper, path, expected_rate, expected_channels, candidate
        )
        return candidate


def decode_float_pcm(w64: dict[str, Any], expected_bits: int) -> list[float]:
    fmt = w64["format"]
    if (fmt["encoding"] != "pcm_float" or fmt["bits"] != expected_bits
            or fmt["valid_bits"] != expected_bits):
        raise ValueError(f"terminal is not exact Float{expected_bits} PCM: {fmt}")
    payload = w64["payload"]
    if expected_bits == 32:
        return [float(value) for value in struct.unpack("<" + "f" * (len(payload) // 4), payload)]
    if expected_bits == 64:
        return list(struct.unpack("<" + "d" * (len(payload) // 8), payload))
    raise AssertionError(expected_bits)


def decode_float64(w64: dict[str, Any]) -> list[float]:
    return decode_float_pcm(w64, 64)


def decode_integer_normalized(w64: dict[str, Any], expected_bits: int) -> list[float]:
    fmt = w64["format"]
    if fmt["encoding"] != "pcm_integer" or fmt["bits"] != expected_bits:
        raise ValueError(f"terminal is not expected integer PCM: {fmt}")
    payload = w64["payload"]
    scale = float(1 << (expected_bits - 1))
    if expected_bits == 8:
        return [(b - 128) / scale for b in payload]
    if expected_bits == 16:
        ints = struct.unpack("<" + "h" * (len(payload) // 2), payload)
        return [v / scale for v in ints]
    if expected_bits == 24:
        out = []
        for i in range(0, len(payload), 3):
            raw = payload[i] | (payload[i + 1] << 8) | (payload[i + 2] << 16)
            if raw & 0x800000:
                raw -= 1 << 24
            out.append(raw / scale)
        return out
    if expected_bits == 32:
        ints = struct.unpack("<" + "i" * (len(payload) // 4), payload)
        return [v / scale for v in ints]
    raise AssertionError(expected_bits)


def invoke(argv: list[str]) -> subprocess.CompletedProcess[str]:
    # Mirrors runtime ClearAndSet: no ambient loader/interposition/config vars
    # can silently become part of qualification. The exact Nix build is RPATH-
    # complete, and every executable is launched by absolute path.
    return subprocess.run(
        argv,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        env={},
        check=False,
    )


def render_gain_matrix(helper: Path, gain: str, channels: int) -> tuple[str, float]:
    proc = invoke([str(helper), gain, str(channels)])
    if proc.returncode != 0:
        raise RuntimeError(f"gain helper failed for {gain} dB/{channels} ch: {proc.stderr[-2048:]}")
    matrix = proc.stdout.strip()
    rows = matrix.split(";")
    if len(rows) != channels:
        raise RuntimeError("gain helper returned wrong matrix row count")
    diagonal: float | None = None
    for row_index, row in enumerate(rows):
        cols = row.split(",")
        if len(cols) != channels:
            raise RuntimeError("gain helper returned wrong matrix column count")
        for col_index, raw in enumerate(cols):
            value = float(raw)
            if row_index == col_index:
                if diagonal is None:
                    diagonal = value
                elif value != diagonal:
                    raise RuntimeError("gain helper returned unequal diagonal scalars")
            elif value != 0.0:
                raise RuntimeError("gain helper returned a nonzero off-diagonal coefficient")
    if diagonal is None or not math.isfinite(diagonal) or diagonal < 0.0:
        raise RuntimeError("gain helper returned invalid diagonal scalar")
    return matrix, diagonal


def observation_argv(exe: Path, cell: Cell, inp: Path, out: Path) -> list[str]:
    return [
        str(exe), "--rate", str(cell.target_rate_hz), "--profile", cell.profile,
        "--bits", "-64", "--att", "0.0", "--dstContainer", "w64",
        str(inp), str(out),
    ]


def terminal_argv(exe: Path, cell: Cell, matrix: str, inp: Path, out: Path) -> list[str]:
    args = [
        str(exe), "--rate", str(cell.target_rate_hz), "--profile", cell.profile,
        "--bits", str(cell.bits), "--mixChannels", matrix, "--att", "0.0",
    ]
    if cell.dither_id is not None:
        args.extend(["--dither", str(cell.dither_id), "--seed", str(DITHER_SEED)])
    if cell.pdf is not None:
        args.extend(["--pdf", "0" if cell.pdf == "rectangular" else "1"])
    args.extend(["--dstContainer", "w64", str(inp), str(out)])
    return args


def command_for_report(argv: list[str]) -> list[str]:
    # Do not make evidence identity depend on random temporary directory names.
    out = list(argv)
    if len(out) >= 2:
        out[-2:] = ["<protected-ingress.wav>", "<output.w64>"]
    return out


def run_gain_point(exe: Path, helper: Path, cell: Cell, gain: str, root: Path) -> dict[str, Any]:
    gain_key = gain.replace("+", "p").replace("-", "m").replace(".", "_")
    work = root / gain_key
    work.mkdir(parents=True, exist_ok=True)
    matrix, scalar = render_gain_matrix(helper, gain, cell.channels)
    inp = work / "protected-ingress.wav"
    observation = work / "observation.w64"
    terminal_a = work / "terminal-a.w64"
    terminal_b = work / "terminal-b.w64"
    write_float64_wav(inp, cell.source_rate_hz, cell.channels, fixture_frames(cell.channels, scalar))

    obs_argv = observation_argv(exe, cell, inp, observation)
    obs_proc = invoke(obs_argv)
    if obs_proc.returncode != 0:
        return {
            "gain_db": gain, "gain_db_nano": db_nano(gain), "passed": False,
            "failure": "observation execution failed", "returncode": obs_proc.returncode,
            "stderr_tail": obs_proc.stderr[-4096:], "observation_argv": command_for_report(obs_argv),
        }
    term_argv_a = terminal_argv(exe, cell, matrix, inp, terminal_a)
    term_argv_b = terminal_argv(exe, cell, matrix, inp, terminal_b)
    term_a_proc = invoke(term_argv_a)
    term_b_proc = invoke(term_argv_b)
    if term_a_proc.returncode != 0 or term_b_proc.returncode != 0:
        return {
            "gain_db": gain, "gain_db_nano": db_nano(gain), "passed": False,
            "failure": "terminal execution failed",
            "returncodes": [term_a_proc.returncode, term_b_proc.returncode],
            "stderr_tail": [term_a_proc.stderr[-4096:], term_b_proc.stderr[-4096:]],
            "terminal_argv": command_for_report(term_argv_a),
        }

    try:
        observed_w64 = parse_w64(observation, cell.target_rate_hz, cell.channels, helper)
        terminal_w64_a = parse_w64(terminal_a, cell.target_rate_hz, cell.channels, helper)
        terminal_w64_b = parse_w64(terminal_b, cell.target_rate_hz, cell.channels, helper)
        observed = decode_float64(observed_w64)
        if cell.is_float():
            realized = decode_float_pcm(terminal_w64_a, cell.float_bits())
            _ = decode_float_pcm(terminal_w64_b, cell.float_bits())
        else:
            realized = decode_integer_normalized(terminal_w64_a, cell.bits)
            _ = decode_integer_normalized(terminal_w64_b, cell.bits)
    except Exception as exc:
        return {
            "gain_db": gain, "gain_db_nano": db_nano(gain), "passed": False,
            "failure": f"output validation failed: {exc}",
            "observation_argv": command_for_report(obs_argv),
            "terminal_argv": command_for_report(term_argv_a),
        }

    deterministic = (
        terminal_w64_a["file_sha256"] == terminal_w64_b["file_sha256"]
        and terminal_w64_a["payload_sha256"] == terminal_w64_b["payload_sha256"]
    )
    same_length = len(observed) == len(realized)
    finite = all(math.isfinite(v) for v in observed) and all(math.isfinite(v) for v in realized)
    reference_peak = max((abs(v * scalar) for v in observed), default=0.0)
    no_saturation_premise = reference_peak < 0.90 and max((abs(v) for v in realized), default=0.0) < 1.0
    max_error = math.inf
    error_sample_index = None
    if same_length and finite:
        max_error = -1.0
        lsb_scale = None if cell.is_float() else float(1 << (cell.bits - 1))
        for index, (obs, out) in enumerate(zip(observed, realized)):
            error_linear = abs(out - obs * scalar)
            error = error_linear if lsb_scale is None else error_linear * lsb_scale
            if error > max_error:
                max_error = error
                error_sample_index = index
    passed = bool(
        deterministic and same_length and finite and no_saturation_premise
        and math.isfinite(max_error) and max_error >= 0.0
    )
    result = {
        "gain_db": gain,
        "gain_db_nano": db_nano(gain),
        "gain_matrix": matrix,
        "directed_gain_scalar": scalar,
        "protected_ingress_sha256": sha256_file(inp),
        "observation_argv": command_for_report(obs_argv),
        "terminal_argv": command_for_report(term_argv_a),
        "observation_payload_sha256": observed_w64["payload_sha256"],
        "terminal_payload_sha256": terminal_w64_a["payload_sha256"],
        "terminal_repeat_payload_sha256": terminal_w64_b["payload_sha256"],
        "deterministic_repeat": deterministic,
        "observation_frames": observed_w64["frames"],
        "terminal_frames": terminal_w64_a["frames"],
        "reference_peak_abs": reference_peak,
        "no_saturation_premise": no_saturation_premise,
        "maximum_error_sample_index": error_sample_index,
        "passed": passed,
    }
    if cell.is_float():
        result["maximum_abs_error_linear"] = max_error
    else:
        result["maximum_abs_error_lsb"] = max_error
    return result


def derive_bound_lsb(observed_max: float) -> int:
    # Deliberately conservative empirical enclosure. The factor-of-two and four
    # whole-LSB reserve are visible policy, not hidden commissioning magic; a
    # minimum of eight LSB avoids false precision when the measured cell is
    # almost exact. Runtime then lifts this stored-sample bound through the same
    # certified reconstruction L-infinity constant as other terminals.
    return max(8, math.ceil(2.0 * observed_max + 4.0))


def float_error_unit(bits: int) -> float:
    # Maximum nearest-representation error below full scale. This is only the
    # reserve unit; the actual bound remains driven by executed SSRC error.
    if bits == 32:
        return 2.0 ** -24
    if bits == 64:
        return 2.0 ** -53
    raise ValueError(f"unsupported floating-point terminal width {bits}")


def derive_bound_linear(observed_max: float, bits: int) -> float:
    unit = float_error_unit(bits)
    bound = max(8.0 * unit, 2.0 * observed_max + 4.0 * unit)
    return math.nextafter(bound, math.inf)


def f64_bits(value: float) -> int:
    return struct.unpack("<Q", struct.pack("<d", value))[0]


def run_cell(exe: Path, helper: Path, cell: Cell, gains: tuple[str, ...], root: Path, arch: str) -> dict[str, Any]:
    cell_root = root / hashlib.sha256(cell.key().encode()).hexdigest()[:16]
    cell_root.mkdir(parents=True, exist_ok=True)
    points = [run_gain_point(exe, helper, cell, gain, cell_root) for gain in gains]
    all_passed = bool(points) and all(point.get("passed") for point in points)
    error_field = "maximum_abs_error_linear" if cell.is_float() else "maximum_abs_error_lsb"
    observed_max = max(
        (float(point[error_field]) for point in points if point.get("passed")),
        default=math.inf,
    )
    result = {
        "cell_key": cell.key(),
        "scope": cell.scope(arch),
        "gain_points": points,
        "minimum_gain_db_nano": min(db_nano(g) for g in gains),
        "maximum_gain_db_nano": max(db_nano(g) for g in gains),
        "passed": all_passed,
    }
    if cell.is_float():
        bound_linear = derive_bound_linear(observed_max, cell.float_bits()) if all_passed else None
        result.update({
            "observed_maximum_abs_error_linear": observed_max,
            "bound_policy": FLOAT_BOUND_POLICY,
            "stored_sample_error_linear": bound_linear,
            "stored_sample_error_linear_bits": None if bound_linear is None else f64_bits(bound_linear),
        })
    else:
        bound_lsb = derive_bound_lsb(observed_max) if all_passed else None
        result.update({
            "observed_maximum_abs_error_lsb": observed_max,
            "bound_policy": INTEGER_BOUND_POLICY,
            "stored_sample_error_lsb": bound_lsb,
            "stored_sample_error_lsb_nano": None if bound_lsb is None else bound_lsb * 1_000_000_000,
        })
    return result


def validate_binary64_authority(report: dict[str, Any], exe: Path) -> dict[str, Any]:
    source = report.get("source_identity", {})
    executable = report.get("executable", {})
    dependency = report.get("dependency_identity", {})
    scope = report.get("ssrc_scope", {})
    problems = []
    if report.get("outcome") != "bounded_established":
        problems.append("Binary64 report is not bounded_established")
    if source.get("rev") != PINNED_SSRC_SOURCE_REV:
        problems.append("Binary64 report source revision is not the pinned SSRC revision")
    actual_sha = sha256_file(exe)
    if executable.get("sha256") != actual_sha:
        problems.append("exact SSRC executable SHA-256 differs from established Binary64 evidence")
    if not dependency.get("build_closure"):
        problems.append("Binary64 report lacks build-closure identity")
    if scope.get("attenuation") != "0.0" or scope.get("phase_modes") != ["linear"]:
        problems.append("Binary64 authority is not the expected 0.0 dB linear-phase scope")
    return {
        "passed": not problems,
        "problems": problems,
        "source_revision": source.get("rev"),
        "source_nar_hash": source.get("nar_hash"),
        "executable_sha256": actual_sha,
        "expected_executable_sha256": executable.get("sha256"),
        "build_identity": dependency.get("build_closure"),
        "binary64_tonepoet_source_identity": report.get("tonepoet_source_identity"),
    }


def validate_cells_against_binary64(cells: list[Cell], report: dict[str, Any]) -> list[str]:
    scope = report.get("ssrc_scope", {})
    profiles = set(scope.get("profiles", ()))
    rates = {tuple(pair) for pair in scope.get("rate_scope", ())}
    errors = []
    for cell in cells:
        if cell.profile not in profiles:
            errors.append(f"{cell.key()}: profile is not commissioned for Binary64 observation")
        if (cell.source_rate_hz, cell.target_rate_hz) not in rates:
            errors.append(f"{cell.key()}: rate pair is not commissioned for Binary64 observation")
    return errors


def main() -> int:
    here = Path(__file__).resolve().parent
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--ssrc", type=Path, required=True, help="exact commissioned SSRC executable")
    parser.add_argument("--gain-helper", type=Path, required=True,
                        help=("compiled tonepoet-pipeline ssrc_true_peak_gain_qualification "
                              "binary (gain authority plus SSRC Wave64 admission)"))
    parser.add_argument("--binary64-outcome", type=Path,
                        default=here.parent / "ssrc_binary64" / "outcome_grid42_2026-09-20.json")
    parser.add_argument("--tonepoet-root", type=Path, required=True,
                        help="exact Tonepoet source tree being qualified; authoritative source files are hashed")
    parser.add_argument("--tonepoet-source-identity", default="unlabeled",
                        help="optional human label (commit/archive identity); file hashes remain authoritative")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--cell", action="append", default=[], help="explicit physical cell; repeatable")
    parser.add_argument("--production-grid", action="store_true",
                        help="qualify global-UI dither cells across the established Binary64 grid")
    parser.add_argument("--channels", default="1,2",
                        help="comma-separated channels for --production-grid (default: 1,2)")
    parser.add_argument("--gain-db", action="append", default=[],
                        help="exact gain point in dB; repeatable (default: -24..+24 corpus)")
    parser.add_argument("--jobs", type=int, default=1,
                        help="parallel physical cells; each SSRC process may itself be multithreaded")
    args = parser.parse_args()
    qualification_started = time.monotonic()

    exe = args.ssrc.resolve()
    helper = args.gain_helper.resolve()
    if not exe.is_file() or not helper.is_file():
        parser.error("--ssrc and --gain-helper must name existing files")
    binary64 = json.loads(args.binary64_outcome.read_text(encoding="utf-8"))
    identity = validate_binary64_authority(binary64, exe)
    if not identity["passed"]:
        print("Binary64 authority validation failed:", file=sys.stderr)
        for problem in identity["problems"]:
            print(f"  - {problem}", file=sys.stderr)
        return 2

    if args.jobs < 1:
        parser.error("--jobs must be >= 1")

    gains = tuple(args.gain_db or DEFAULT_GAINS)
    # Malformed/over-precision values fail before any capability/audio probe.
    gain_nanos = [db_nano(gain) for gain in gains]
    if len(set(gain_nanos)) != len(gain_nanos):
        parser.error("gain corpus contains duplicate nanodecibel values")
    gains = tuple(gain for _, gain in sorted(zip(gain_nanos, gains)))

    production_channels: tuple[int, ...] = ()
    if args.production_grid:
        production_channels = tuple(int(raw) for raw in args.channels.split(",") if raw)
        if not production_channels or any(ch < 1 or ch > 64 for ch in production_channels):
            parser.error("--channels must contain at least one value in 1..64")

    arch = normalize_arch(platform.machine())
    expected_arches = {
        normalize_arch(value)
        for value in binary64.get("ssrc_scope", {}).get("architecture_scope", ())
    }
    if arch not in expected_arches:
        print(
            f"runtime architecture {arch} is outside Binary64 evidence {sorted(expected_arches)}",
            file=sys.stderr,
        )
        return 2

    cells = [parse_cell(raw) for raw in args.cell]
    explicit_cell_errors = validate_cells_against_binary64(cells, binary64)
    if explicit_cell_errors:
        for error in explicit_cell_errors:
            print(error, file=sys.stderr)
        return 2
    if args.production_grid:
        target_rates = {
            int(pair[1])
            for pair in binary64.get("ssrc_scope", {}).get("rate_scope", ())
            if len(pair) == 2
        }
    else:
        # A single explicit cell is also the preflight cost probe. Do not pay
        # for the whole production grid's 22-ID capability sweep merely to time
        # one cell; probe only destination rates whose explicit cell uses dither.
        target_rates = {
            cell.target_rate_hz for cell in cells if cell.dither_id is not None
        }
    with tempfile.TemporaryDirectory(prefix="tonepoet-ssrc-dither-probe-") as probe_td:
        dither_capabilities = discover_ssrc_dither_capabilities(
            exe, helper, target_rates, Path(probe_td)
        )
    for cell in cells:
        if (
            cell.dither_id is not None
            and cell.dither_id not in dither_capabilities.get(cell.target_rate_hz, ())
        ):
            parser.error(
                f"explicit cell {cell.key()} selects SSRC dither ID {cell.dither_id}, "
                f"which the exact executable does not expose at "
                f"{cell.target_rate_hz} Hz"
            )
    if args.production_grid:
        cells.extend(production_cells(binary64, production_channels, dither_capabilities))
    cells = sorted(set(cells), key=lambda cell: cell.key())
    if not cells:
        parser.error("provide at least one --cell or --production-grid")
    cell_errors = validate_cells_against_binary64(cells, binary64)
    if cell_errors:
        for error in cell_errors:
            print(error, file=sys.stderr)
        return 2

    args.output.parent.mkdir(parents=True, exist_ok=True)
    execution_started = time.monotonic()
    with tempfile.TemporaryDirectory(prefix="tonepoet-ssrc-terminal-qualification-") as td:
        root = Path(td)
        if args.jobs == 1:
            results = [run_cell(exe, helper, cell, gains, root, arch) for cell in cells]
        else:
            with concurrent.futures.ThreadPoolExecutor(max_workers=args.jobs) as pool:
                futures = [pool.submit(run_cell, exe, helper, cell, gains, root, arch) for cell in cells]
                results = [future.result() for future in futures]
    execution_wall_seconds = time.monotonic() - execution_started
    results.sort(key=lambda item: item["cell_key"])

    all_passed = all(result.get("passed") for result in results)
    report = {
        "schema_version": SCHEMA_VERSION,
        "contract_id": CONTRACT_ID,
        "gain_model_id": GAIN_MODEL_ID,
        "dither_seed": DITHER_SEED,
        "tonepoet_source_identity": args.tonepoet_source_identity,
        "tonepoet_source_authority": {
            "files": tonepoet_source_authority(args.tonepoet_root),
        },
        "platform": {"os": platform.system(), "architecture": arch},
        "ssrc_identity": {
            "path": str(exe),
            "sha256": identity["executable_sha256"],
            "source_revision": identity["source_revision"],
            "source_nar_hash": identity["source_nar_hash"],
            "build_identity": identity["build_identity"],
        },
        "gain_helper_identity": {"path": str(helper), "sha256": sha256_file(helper)},
        "binary64_authority": {
            "report": str(args.binary64_outcome),
            "report_sha256": sha256_file(args.binary64_outcome),
            "validation": identity,
        },
        "gain_corpus_db": list(gains),
        "dither_capability_probe": {
            "method": "exact_executable_tiny_carrier_execution/v1",
            "candidate_ids": list(SSRC_DITHER_PROBE_IDS),
            "supported_ids_by_target_rate_hz": {
                str(rate): list(ids) for rate, ids in sorted(dither_capabilities.items())
            },
        },
        "cells": results,
        "summary": {
            "cell_count": len(results),
            "passed_cells": sum(bool(r.get("passed")) for r in results),
            "failed_cells": sum(not bool(r.get("passed")) for r in results),
        },
        "commissioning": {
            "decision": "candidate_for_review" if all_passed else "not_qualified",
            "reason": (
                "all requested physical cells executed twice per gain point with exact deterministic replay and bounded finite error"
                if all_passed else
                "one or more requested physical cells failed execution or validation"
            ),
            "registry_was_modified": False,
        },
    }
    args.output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    report_sha = sha256_file(args.output)
    identity_path = args.output.with_name(args.output.name + ".identity.json")
    identity_path.write_text(json.dumps({
        "evidence_id": f"sha256:{report_sha}",
        "qualification_report": str(args.output),
        "qualification_report_sha256": report_sha,
        "outcome": "candidate_for_review" if all_passed else "not_qualified",
    }, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    total_wall_seconds = time.monotonic() - qualification_started
    print(json.dumps({
        "output": str(args.output),
        "identity": str(identity_path),
        "sha256": report_sha,
        "cells": len(results),
        "passed": all_passed,
        "total_wall_seconds": round(total_wall_seconds, 6),
        "execution_wall_seconds": round(execution_wall_seconds, 6),
        "fixed_overhead_wall_seconds": round(total_wall_seconds - execution_wall_seconds, 6),
        "wall_seconds_per_cell": round(execution_wall_seconds / len(results), 6),
    }, sort_keys=True))
    return 0 if all_passed else 1


if __name__ == "__main__":
    raise SystemExit(main())
