#!/usr/bin/env python3
"""Physical FFmpeg HDCD source-first/CUE sample-range smoke test.

This does not replace Cargo/planner or Reference qualification. It checks the
pinned filter graph's PCM-s32 output and seek/atrim sample alignment using a
16/44.1 FLAC carrier, independently of TonePoet's Rust command construction.
"""

from __future__ import annotations

import array
import json
import math
import struct
import subprocess
import tempfile
import wave
from pathlib import Path


def run(*args: str) -> None:
    subprocess.run(args, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)


def decoded_samples(path: Path) -> bytes:
    with wave.open(str(path), "rb") as reader:
        assert reader.getframerate() == 44_100, "HDCD changed sample rate"
        assert reader.getnchannels() == 2, "HDCD changed channel count"
        assert reader.getsampwidth() == 4, "HDCD did not emit signed 32-bit PCM"
        return reader.readframes(reader.getnframes())



def verify_detector_scans_beyond_first_second(tmp: Path) -> None:
    """Exercise the detector's actual FFmpeg argument contract, including -y.

    A valid peak-extend control packet is planted after 1.5 seconds. Both
    decoder output and the final info-level summary require processing past
    the initial status line (which normally says "HDCD detected: no").
    """
    sr = 44_100
    frame_count = 2 * sr
    window = 0x7DE0_A5D0
    control = 0x53
    threshold = sr * 3 // 2
    packet_start = 32 + 31 * ((max(threshold - 32, 0) + 30) // 31)
    bits = ([(window >> bit) & 1 for bit in range(30, -1, -1)]
            + [(control >> bit) & 1 for bit in range(7, -1, -1)])
    assert packet_start > sr
    path = tmp / "late-packet.wav"
    with path.open("wb") as f:
        size = frame_count * 4
        f.write(b"RIFF" + struct.pack("<I", 36 + size) + b"WAVEfmt ")
        f.write(struct.pack("<IHHIIHH", 16, 1, 2, sr, sr * 4, 4, 16))
        f.write(b"data" + struct.pack("<I", size))
        for frame in range(frame_count):
            bit = bits[frame - packet_start] if packet_start <= frame < packet_start + len(bits) else 0
            f.write(struct.pack("<hh", 1_000 + bit, 1_000 + bit))

    for name, limit, expected in (("bounded", ["-t", "1.000000"], False),
                                  ("whole", [], True)):
        command = ["ffmpeg", "-hide_banner", "-nostats", "-y", "-v", "info",
                   *limit, "-i", str(path),
                   "-af", "hdcd=bits_per_sample=16:disable_autoconvert=1",
                   "-f", "s24le", "/dev/null"]
        result = subprocess.run(command, capture_output=True, text=True, check=True)
        summaries = [line for line in result.stderr.splitlines() if "HDCD detected:" in line]
        assert summaries, f"{name}: FFmpeg did not emit an HDCD summary"
        detected = any("HDCD detected: yes" in line for line in summaries)
        assert detected is expected, f"{name}: unexpected HDCD classification: {summaries!r}"
        if expected:
            assert any("peak_extend: enabled" in line for line in summaries)
    # This is the *scan-time per-member classification*, not the conversion
    # decoder. The scanner intentionally tests each member's own control bits;
    # conversion must retain prior decoder state for authorized members.
    flac = tmp / "late-packet.flac"
    run("ffmpeg", "-v", "error", "-y", "-i", str(path), "-c:a", "flac", str(flac))
    split = 44_113
    for name, start_sample, sample_count, expected in (
        ("pre-packet CUE member", 0, split, False),
        ("peak-extend CUE member", split, frame_count - split, True),
    ):
        whole_seconds, residual = divmod(start_sample, sr)
        seek = ["-ss", str(whole_seconds)] if whole_seconds else []
        filt = (f"atrim=start_sample={residual}:end_sample={residual + sample_count},"
                "asetpts=PTS-STARTPTS,hdcd=bits_per_sample=16:disable_autoconvert=1")
        command = ["ffmpeg", "-hide_banner", "-nostats", "-y", "-v", "info",
                   *seek, "-i", str(flac), "-af", filt, "-f", "s24le", "/dev/null"]
        result = subprocess.run(command, capture_output=True, text=True, check=True)
        summaries = [line for line in result.stderr.splitlines() if "HDCD detected:" in line]
        assert summaries, f"{name}: missing HDCD classification"
        found = any("HDCD detected: yes" in line for line in summaries)
        assert found is expected, f"{name}: incorrect HDCD classification: {summaries!r}"
    print("PASS: late HDCD detection and independently classified FLAC/CUE scan members")


def main() -> None:
    with tempfile.TemporaryDirectory(prefix="tonepoet-hdcd-") as root:
        tmp = Path(root)
        verify_detector_scans_beyond_first_second(tmp)
        wav = tmp / "source.wav"
        flac = tmp / "source.flac"
        seconds = 4
        pcm = array.array("h", (
            int(5000 * math.sin(2 * math.pi * 523 * n / 44_100))
            for n in range(44_100 * seconds)
            for _channel in range(2)
        ))
        with wave.open(str(wav), "wb") as output:
            output.setnchannels(2)
            output.setsampwidth(2)
            output.setframerate(44_100)
            output.writeframes(pcm.tobytes())
        run("ffmpeg", "-v", "error", "-y", "-i", str(wav), "-c:a", "flac", str(flac))

        # CUE conversion must decode continuous source PCM first and cut the
        # reconstructed signal afterwards. A non-second-aligned boundary must
        # yield exactly the corresponding subset of the full decoder output.
        start = 44_100 + 588
        count = 44_159
        cue = tmp / "cue.wav"
        full = tmp / "full.wav"
        suffix = "hdcd=bits_per_sample=16:disable_autoconvert=1"
        run("ffmpeg", "-nostdin", "-v", "error", "-y", "-i", str(flac),
            "-map", "0:a:0", "-af",
            f"{suffix},atrim=start_sample={start}:end_sample={start + count},asetpts=PTS-STARTPTS",
            "-c:a", "pcm_s32le", "-f", "wav", str(cue))
        run("ffmpeg", "-nostdin", "-v", "error", "-y", "-i", str(flac),
            "-map", "0:a:0", "-af", suffix,
            "-c:a", "pcm_s32le", "-f", "wav", str(full))
        lhs = decoded_samples(cue)
        rhs = decoded_samples(full)[start*8:(start + count)*8]
        assert len(lhs) == count * 2 * 4
        assert lhs == rhs, "continuous HDCD -> exact CUE cut differs from full decode"
        probe = subprocess.check_output([
            "ffprobe", "-v", "error", "-select_streams", "a:0",
            "-show_entries", "stream=sample_rate,channels,sample_fmt,codec_name",
            "-of", "json", str(cue),
        ])
        audio = json.loads(probe)["streams"][0]
        assert audio == {
            "codec_name": "pcm_s32le", "sample_fmt": "s32",
            "sample_rate": "44100", "channels": 2,
        }, audio
        print(f"PASS: FLAC source-first HDCD decode, {count} exact stereo S32 frames, no rate change")


if __name__ == "__main__":
    main()
