#!/usr/bin/env python3
"""Conformance check for Tonepoet's qualified FFmpeg S32 triangular-dither bound.

This test validates an implementation against the independently derived source-level
bound. It is not the source of that bound. A run is qualification-conforming only
when the exact executable path and SHA-256 bound by Tonepoet are supplied and match.
"""

from __future__ import annotations

import argparse
import hashlib
import math
import re
import shutil
import struct
import subprocess
import tempfile
from pathlib import Path

INT32_LSB = 2.0 ** -31
DOUBLE_ADD_ULP = 2.0 ** -52
BOUND = math.nextafter(INT32_LSB + INT32_LSB + DOUBLE_ADD_ULP, math.inf)


def ffmpeg_version(ffmpeg: Path) -> str:
    completed = subprocess.run(
        [str(ffmpeg), "-version"],
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    first = completed.stdout.splitlines()[0] if completed.stdout else ""
    match = re.search(r"ffmpeg version\s+(?:n)?([0-9]+(?:\.[0-9]+){1,2})", first)
    if not match:
        raise RuntimeError(f"could not parse FFmpeg version from: {first!r}")
    return match.group(1)


def file_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def adversarial_samples() -> list[float]:
    samples: list[float] = []
    offsets = [
        -0.5000001,
        -0.5,
        -0.4999999,
        -(2.0 ** -22),
        0.0,
        2.0 ** -22,
        0.4999999,
        0.5,
        0.5000001,
    ]
    lattice_points = [
        -(2**31),
        -(2**31) + 1,
        -1_000_003,
        -3,
        -2,
        -1,
        0,
        1,
        2,
        3,
        1_000_003,
        (2**31) - 3,
        (2**31) - 2,
        (2**31) - 1,
    ]
    for integer in lattice_points:
        for offset in offsets:
            value = (integer + offset) * INT32_LSB
            if -1.0 <= value <= 1.0:
                samples.append(value)

    # Long repeated runs exercise PRNG state progression while holding the signal
    # at boundaries where terminal error is easy to interpret.
    repeated = [
        0.0,
        0.5 * INT32_LSB,
        -0.5 * INT32_LSB,
        0.25,
        -0.25,
        1.0,
        -1.0,
        1.0 - INT32_LSB,
        -1.0 + INT32_LSB,
    ]
    for value in repeated:
        samples.extend([value] * 65536)
    return samples


def run(
    ffmpeg: Path,
    expected_version: str,
    expected_realpath: Path | None,
    expected_sha256: str | None,
    allow_unqualified_identity: bool,
) -> int:
    ffmpeg = ffmpeg.resolve(strict=True)
    version = ffmpeg_version(ffmpeg)
    sha256 = file_sha256(ffmpeg)
    version_match = version == expected_version
    path_match = expected_realpath is not None and ffmpeg == expected_realpath.resolve(strict=True)
    digest_match = expected_sha256 is not None and sha256 == expected_sha256.lower()
    qualification_identity_match = version_match and path_match and digest_match

    if not qualification_identity_match and not allow_unqualified_identity:
        missing = []
        if expected_realpath is None:
            missing.append("--expected-realpath")
        if expected_sha256 is None:
            missing.append("--expected-sha256")
        detail = f"; missing {' and '.join(missing)}" if missing else ""
        raise RuntimeError(
            "executable does not match the qualified Tonepoet identity "
            f"(version_match={version_match}, path_match={path_match}, "
            f"digest_match={digest_match}){detail}; use --allow-unqualified-identity "
            "only for non-qualifying implementation-model validation"
        )

    samples = adversarial_samples()
    with tempfile.TemporaryDirectory(prefix="tonepoet-ffmpeg-dither-") as temp:
        temp_path = Path(temp)
        source = temp_path / "input.f64le"
        output = temp_path / "output.s32le"
        with source.open("wb") as handle:
            for sample in samples:
                handle.write(struct.pack("<d", sample))

        subprocess.run(
            [
                str(ffmpeg),
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "f64le",
                "-ar",
                "48000",
                "-ac",
                "1",
                "-i",
                str(source),
                "-af",
                "aresample=resampler=soxr:out_sample_rate=48000:precision=33:cutoff=0.95:"
                "dither_method=triangular:out_sample_fmt=s32",
                "-f",
                "s32le",
                "-y",
                str(output),
            ],
            check=True,
        )
        raw = output.read_bytes()

    if len(raw) != len(samples) * 4:
        raise RuntimeError(
            f"output extent mismatch: {len(raw)} bytes for {len(samples)} input samples"
        )

    maximum = 0.0
    maximum_index = 0
    violations = 0
    for index, (sample, packed) in enumerate(zip(samples, struct.iter_unpack("<i", raw))):
        realized = packed[0] * INT32_LSB
        error = abs(realized - sample)
        if error > maximum:
            maximum = error
            maximum_index = index
        if error > BOUND:
            violations += 1

    print(f"ffmpeg_realpath={ffmpeg}")
    print(f"ffmpeg_sha256={sha256}")
    print(f"ffmpeg_version={version}")
    print(f"qualification_version_match={str(version_match).lower()}")
    print(f"qualification_path_match={str(path_match).lower()}")
    print(f"qualification_digest_match={str(digest_match).lower()}")
    print(f"qualification_identity_match={str(qualification_identity_match).lower()}")
    print(f"samples={len(samples)}")
    print(f"theoretical_bound_fs={BOUND:.18e}")
    print(f"theoretical_bound_lsb={BOUND / INT32_LSB:.12f}")
    print(f"observed_max_fs={maximum:.18e}")
    print(f"observed_max_lsb={maximum / INT32_LSB:.12f}")
    print(f"observed_max_index={maximum_index}")
    print(f"violations={violations}")
    return 0 if violations == 0 else 1


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--ffmpeg", type=Path, default=Path("ffmpeg"))
    parser.add_argument("--expected-version", default="7.1.3")
    parser.add_argument("--expected-realpath", type=Path)
    parser.add_argument("--expected-sha256")
    parser.add_argument("--allow-unqualified-identity", action="store_true")
    args = parser.parse_args()
    ffmpeg = args.ffmpeg
    if not ffmpeg.is_absolute():
        resolved = shutil.which(str(ffmpeg))
        if resolved is None:
            raise RuntimeError(f"could not resolve FFmpeg executable: {ffmpeg}")
        ffmpeg = Path(resolved)
    if args.expected_sha256 is not None and not re.fullmatch(r"[0-9a-fA-F]{64}", args.expected_sha256):
        raise RuntimeError("--expected-sha256 must be exactly 64 hexadecimal characters")
    return run(
        ffmpeg,
        args.expected_version,
        args.expected_realpath,
        args.expected_sha256,
        args.allow_unqualified_identity,
    )


if __name__ == "__main__":
    raise SystemExit(main())
