#!/usr/bin/env python3
"""Independent standards-literal oracle for P0 DSTCoded=0 fixtures.

This program does not import or invoke sacd-rs. It validates the reserved header
bits and decodes the ISO/IEC 14496-3 DSTCoded=0 form by returning the bytes after
the single zero header octet. Every result must equal its pinned `.dsd.bin`
companion byte-for-byte.
"""
from pathlib import Path

ROOT = Path(__file__).resolve().parent


def decode_dstcoded_zero(frame: bytes) -> bytes:
    if not frame:
        raise ValueError("empty DST frame")
    if frame[0] != 0:
        raise ValueError("not a canonical DSTCoded=0 frame")
    return frame[1:]


def main() -> None:
    pairs = sorted(ROOT.glob("raw_*.dst.bin"))
    if not pairs:
        raise SystemExit("no raw P0 fixtures found")
    for encoded_path in pairs:
        expected_path = encoded_path.with_name(
            encoded_path.name.replace(".dst.bin", ".dsd.bin")
        )
        actual = decode_dstcoded_zero(encoded_path.read_bytes())
        expected = expected_path.read_bytes()
        if actual != expected:
            raise SystemExit(f"oracle mismatch: {encoded_path.name}")
    print(f"verified {len(pairs)} standards-literal P0 fixtures")


if __name__ == "__main__":
    main()
