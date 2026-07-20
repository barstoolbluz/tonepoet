#!/usr/bin/env python3
"""Regenerate standards-literal P0 DSTCoded=0 geometry fixtures.

A raw DST frame has one all-zero header byte (DSTCoded=0, DstXbits=0,
reserved=0) followed by the interleaved DSD payload byte-for-byte. The payload
is deterministic pseudodata derived from the case ID; no codec implementation
is used to establish the expected output.
"""
from hashlib import sha256
from pathlib import Path

ROOT = Path(__file__).resolve().parent
CASES = {
    "raw_dsd64_mono": 4_704,
    "raw_dsd64_6ch": 28_224,
    "raw_dsd128_mono": 9_408,
    "raw_dsd128_stereo": 18_816,
    "raw_dsd256_mono": 18_816,
    "raw_dsd256_stereo": 37_632,
}


def payload(case_id: str, length: int) -> bytes:
    out = bytearray()
    counter = 0
    while len(out) < length:
        out.extend(sha256(f"tonepoet-p0-dst-raw-v1:{case_id}:{counter}".encode()).digest())
        counter += 1
    return bytes(out[:length])


for case_id, length in CASES.items():
    dsd = payload(case_id, length)
    (ROOT / f"{case_id}.dsd.bin").write_bytes(dsd)
    (ROOT / f"{case_id}.dst.bin").write_bytes(b"\x00" + dsd)
