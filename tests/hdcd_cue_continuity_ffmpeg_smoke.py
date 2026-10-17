#!/usr/bin/env python3
"""HDCD CUE control-state boundary regression, with real Peak Extend packets.

Checks sample-exact equality with continuous-programme HDCD decoding, not two
variations of trim-before-decode. The deliberately incorrect ordering must
produce different samples, so the fixture cannot silently become vacuous.
FFmpeg smoke is not a TonePoet end-to-end or pinned Reference qualification.
"""

from __future__ import annotations

import array
import struct
import subprocess
import tempfile
import wave
from pathlib import Path

RATE = 44_100
FILTER = "hdcd=bits_per_sample=16:disable_autoconvert=1"


def run(*args: str) -> str:
    result = subprocess.run(args, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    if result.returncode:
        raise AssertionError(f"FFmpeg command failed ({result.returncode}): {args!r}\n{result.stderr}")
    return result.stderr


def frames(path: Path) -> bytes:
    with wave.open(str(path), "rb") as wav:
        assert wav.getnchannels() == 2
        assert wav.getframerate() == RATE
        assert wav.getsampwidth() == 4
        return wav.readframes(wav.getnframes())


def make_hdcd_image(path: Path, boundary: int) -> None:
    total = 3 * RATE
    # High-level stereo signal makes Peak Extend's retained control state
    # observable immediately after the CUE cut; low-level fixtures can pass
    # incorrectly because peak-extension makes no visible sample change.
    source = array.array("h", (30_000 for _ in range(total * 2)))
    window, control = 0x7DE0_A5D0, 0x53
    packet = ([(window >> shift) & 1 for shift in range(30, -1, -1)]
              + [(control >> shift) & 1 for shift in range(7, -1, -1)])
    for start in (32, RATE - 300, boundary - 35, boundary + 157, boundary + 5_000):
        for offset, bit in enumerate(packet):
            for channel in (0, 1):
                sample_index = (start + offset) * 2 + channel
                source[sample_index] = (source[sample_index] & ~1) | bit
    with wave.open(str(path), "wb") as writer:
        writer.setnchannels(2)
        writer.setsampwidth(2)
        writer.setframerate(RATE)
        writer.writeframes(source.tobytes())


def decode(image: Path, path: Path, filters: str) -> None:
    run("ffmpeg", "-y", "-nostdin", "-hide_banner", "-loglevel", "error",
        "-i", str(image), "-map", "0:a:0", "-vn", "-sn", "-dn",
        "-af", filters, "-c:a", "pcm_s32le", "-f", "wav", str(path))


def main() -> None:
    with tempfile.TemporaryDirectory(prefix="tonepoet-hdcd-cue-continuous-") as tmp:
        root = Path(tmp)
        boundary = RATE + 997  # not second aligned
        count = RATE + 333
        end = boundary + count
        source = root / "album.wav"
        encoded = root / "album.flac"
        make_hdcd_image(source, boundary)
        run("ffmpeg", "-y", "-nostdin", "-loglevel", "error", "-i", str(source),
            "-c:a", "flac", str(encoded))

        whole = root / "whole.wav"
        cue = root / "cue.wav"
        incorrect = root / "incorrect.wav"
        decode(encoded, whole, FILTER)
        decode(encoded, cue,
               f"{FILTER},atrim=start_sample={boundary}:end_sample={end},asetpts=PTS-STARTPTS")
        decode(encoded, incorrect,
               f"atrim=start_sample={boundary}:end_sample={end},asetpts=PTS-STARTPTS,{FILTER}")

        expected = frames(whole)[boundary * 8:end * 8]
        assert len(expected) == count * 8
        assert frames(cue) == expected, "continuous decode -> CUE cut is not sample-identical"
        bad = frames(incorrect)
        assert len(bad) == len(expected)
        differences = sum(a != b for a, b in zip(struct.iter_unpack("<i", bad),
                                                  struct.iter_unpack("<i", expected)))
        assert differences > 0, "Peak Extend fixture no longer exposes incorrect decoder state"
        print(f"PASS: true HDCD CUE boundary ({boundary} samples), {count} exact frames,"
              f" zero mismatches after correct cut; {differences} wrong-path sample mismatches")

        # Mixed album: a neighboring track not HDCD-authorized retains its
        # original source-sample route, never the decoded-HDCD carrier.
        plain = root / "ordinary.wav"
        plain_end = RATE
        run("ffmpeg", "-y", "-nostdin", "-hide_banner", "-loglevel", "error",
            "-i", str(encoded), "-af",
            f"atrim=start_sample=0:end_sample={plain_end},asetpts=PTS-STARTPTS",
            "-c:a", "pcm_s16le", "-f", "wav", str(plain))
        with wave.open(str(plain), "rb") as wav:
            assert wav.getnframes() == plain_end
            assert wav.getsampwidth() == 2
            ordinary_samples = wav.readframes(plain_end)
        with wave.open(str(source), "rb") as wav:
            assert ordinary_samples == wav.readframes(plain_end)
        print("PASS: unaffected neighboring CUE member preserves original 16-bit samples")


if __name__ == "__main__":
    main()
