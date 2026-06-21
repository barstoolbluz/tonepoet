#!/usr/bin/env python3
"""Validate DVD-Audio MLP fixture framing outside the Rust test harness."""
from __future__ import annotations

import argparse
import shutil
import subprocess
import tempfile
from pathlib import Path

DVD_SECTOR_SIZE = 2048
PACK_START_CODE = b"\x00\x00\x01\xba"
PRIVATE_STREAM_1 = 0xBD
MLP_STREAM_ID = 0xA1
MLP_HEADER_BIAS = 5
MLP_LENGTH_MASK = 0x0FFF


def iter_mlp_packets(aob_bytes: bytes):
    if len(aob_bytes) % DVD_SECTOR_SIZE:
        raise ValueError("fixture does not contain whole 2048-byte sectors")
    for sector_index in range(0, len(aob_bytes), DVD_SECTOR_SIZE):
        sector = aob_bytes[sector_index : sector_index + DVD_SECTOR_SIZE]
        if sector[:4] != PACK_START_CODE:
            raise ValueError(f"sector {sector_index // DVD_SECTOR_SIZE} missing pack header")
        offset = 14 + (sector[13] & 0x07)
        while offset + 6 <= DVD_SECTOR_SIZE and sector[offset : offset + 3] == b"\x00\x00\x01":
            stream_id = sector[offset + 3]
            pes_len = int.from_bytes(sector[offset + 4 : offset + 6], "big")
            pes_end = offset + 6 + pes_len
            if pes_end > DVD_SECTOR_SIZE:
                raise ValueError(f"sector {sector_index // DVD_SECTOR_SIZE} has truncated PES")
            if stream_id == PRIVATE_STREAM_1:
                pes_header_len = sector[offset + 8]
                sub = offset + 9 + pes_header_len
                body = sector[sub:pes_end]
                if len(body) >= 10 and body[0] == MLP_STREAM_ID:
                    extra = body[3]
                    pointer = int.from_bytes(body[4:6], "big") if extra >= 2 else None
                    payload = body[4 + extra :]
                    yield sector_index // DVD_SECTOR_SIZE, pointer, payload
            offset = pes_end


def access_unit_len(buf: bytes, offset: int) -> int | None:
    if offset + 2 > len(buf):
        return None
    words = int.from_bytes(buf[offset : offset + 2], "big") & MLP_LENGTH_MASK
    length = words * 2
    if 4 <= length <= MLP_LENGTH_MASK * 2:
        return length
    return None


def reassemble(packets):
    started = False
    pending = bytearray()
    out = bytearray()
    stats = {
        "packets_seen": 0,
        "input_payload_bytes": 0,
        "leading_fragment_bytes": 0,
        "access_units": 0,
        "framed_bytes": 0,
        "padding_bytes": 0,
        "resync_bytes": 0,
        "carry_bytes": 0,
    }
    for _sector, pointer, payload in packets:
        stats["packets_seen"] += 1
        stats["input_payload_bytes"] += len(payload)
        if not started:
            if pointer is None or pointer < MLP_HEADER_BIAS:
                raise ValueError("first packet lacks a usable MLP first-access-unit pointer")
            skip = pointer - MLP_HEADER_BIAS
            if skip > len(payload):
                raise ValueError("first access-unit pointer exceeds payload length")
            skipped = payload[:skip]
            if any(skipped):
                stats["leading_fragment_bytes"] += len(skipped)
            else:
                stats["padding_bytes"] += len(skipped)
            payload = payload[skip:]
            started = True
        pending.extend(payload)
        while pending:
            stats["carry_bytes"] = max(stats["carry_bytes"], len(pending))
            if all(b == 0 for b in pending):
                stats["padding_bytes"] += len(pending)
                pending.clear()
                break
            if len(pending) < 2:
                break
            length = access_unit_len(pending, 0)
            if length is None:
                raise ValueError("invalid MLP access-unit length at current boundary")
            if len(pending) < length:
                break
            out.extend(pending[:length])
            del pending[:length]
            stats["access_units"] += 1
            stats["framed_bytes"] += length
    stats["carry_bytes"] = max(stats["carry_bytes"], len(pending))
    return bytes(out), bytes(pending), stats


def ffmpeg_sample_count(mlp: bytes) -> int | None:
    if shutil.which("ffmpeg") is None or shutil.which("ffprobe") is None:
        return None
    with tempfile.TemporaryDirectory() as td:
        inp = Path(td) / "fixture.mlp"
        wav = Path(td) / "fixture.wav"
        inp.write_bytes(mlp)
        subprocess.run(
            ["ffmpeg", "-v", "error", "-f", "mlp", "-i", str(inp), "-c:a", "pcm_s32le", str(wav)],
            check=True,
        )
        probe = subprocess.run(
            [
                "ffprobe",
                "-v",
                "error",
                "-show_entries",
                "stream=duration_ts",
                "-of",
                "default=nw=1:nk=1",
                str(wav),
            ],
            check=True,
            text=True,
            stdout=subprocess.PIPE,
        )
        return int(probe.stdout.strip())


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("fixture", type=Path)
    parser.add_argument("--ffmpeg", action="store_true")
    args = parser.parse_args()

    framed, carry, stats = reassemble(iter_mlp_packets(args.fixture.read_bytes()))
    for key in sorted(stats):
        print(f"{key}={stats[key]}")
    print(f"final_carry_bytes={len(carry)}")
    if args.ffmpeg:
        samples = ffmpeg_sample_count(framed)
        print(f"ffmpeg_decoded_samples={samples if samples is not None else 'unavailable'}")


if __name__ == "__main__":
    main()
