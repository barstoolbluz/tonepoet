#!/usr/bin/env python3
"""Compile-free *physical* HDCD/true-peak composition witness.

Exercises the FFmpeg commands that the existing source-first, Float64 carrier,
CD de-emphasis and selected-FFmpeg resampler branches use. Also checks a
sample-level gain envelope, CUE slicing, encoder-input formats and single
terminal quantization. This is NOT a Rust planner/executor test, not the
TonePoet certified meter, and not pinned-Reference requalification.
"""
from __future__ import annotations

import json
import math
import shutil
import struct
import subprocess
import tempfile
from pathlib import Path

import numpy as np
from scipy.signal import resample_poly


def cmd(*args: str) -> subprocess.CompletedProcess[str]:
    result = subprocess.run(args, capture_output=True, text=True, check=False)
    if result.returncode:
        raise AssertionError(f"FFmpeg command failed: {args!r}\n{result.stderr[-1800:]}")
    return result


def probe(path: Path) -> dict:
    return json.loads(cmd("ffprobe", "-v", "error", "-select_streams", "a:0",
                          "-show_entries", "stream=codec_name,sample_fmt,sample_rate,channels",
                          "-of", "json", str(path)).stdout)["streams"][0]


def raw(path: Path, channels: int = 2) -> np.ndarray:
    samples = np.fromfile(path, dtype="<f8")
    assert samples.size and samples.size % channels == 0
    samples = samples.reshape(-1, channels)
    assert np.isfinite(samples).all(), "nonfinite analysis carrier"
    return samples


def true_peak_approx(audio: np.ndarray) -> float:
    # Independent 8x FIR reconstruction for this *smoke* test. Not a stand-in
    # for TonePoet's certified true-peak scanner/error contract.
    return float(np.max(np.abs(resample_poly(audio, 8, 1, axis=0))))


def write_sample_exact_stereo(path: Path, frames: int = 44_100 * 2) -> None:
    sr = 44_100
    n = np.arange(frames, dtype=np.float64)
    channel_a = 10_000 * np.sin(2 * math.pi * 1_013 * n / sr)
    channel_b = 7_000 * np.sin(2 * math.pi * 9_971 * n / sr)
    stereo = np.column_stack((channel_a, channel_b)).astype("<i2")
    # HDCD control packets are embedded only in the least significant bit.
    # The source stays genuine signed 16-bit PCM, including for CUE slices.
    window, control = 0x7DE0A5D0, 0x53
    bits = [((window >> bit) & 1) for bit in range(30, -1, -1)]
    bits += [((control >> bit) & 1) for bit in range(7, -1, -1)]
    for start in [32, 44_100 + 65]:
        for offset, bit in enumerate(bits):
            stereo[start + offset, :] = (stereo[start + offset, :] & ~1) | bit
    pcm = stereo.tobytes()
    import wave
    with wave.open(str(path), "wb") as writer:
        writer.setnchannels(2)
        writer.setsampwidth(2)
        writer.setframerate(sr)
        writer.writeframes(pcm)


def decode_hdcd(original: Path, output: Path, filter_prefix: str = "") -> None:
    cmd("ffmpeg", "-y", "-nostdin", "-v", "error", "-i", str(original),
        "-map", "0:a:0", "-af", filter_prefix + "hdcd=bits_per_sample=16:disable_autoconvert=1",
        "-c:a", "pcm_s32le", "-f", "wav", str(output))
    info = probe(output)
    assert (info["sample_rate"], info["channels"], info["sample_fmt"], info["codec_name"]) == (
        "44100", 2, "s32", "pcm_s32le"), info


def to_double_wav(source: Path, dest: Path) -> None:
    cmd("ffmpeg", "-y", "-nostdin", "-v", "error", "-i", str(source),
        "-map", "0:a:0", "-c:a", "pcm_f64le", "-f", "wav", str(dest))
    assert probe(dest)["codec_name"] == "pcm_f64le"


def unwrap_double_wav(source: Path, dest: Path) -> np.ndarray:
    cmd("ffmpeg", "-y", "-nostdin", "-v", "error", "-i", str(source),
        "-map", "0:a:0", "-vn", "-sn", "-dn", "-c:a", "pcm_f64le",
        "-f", "f64le", str(dest))
    return raw(dest)


def apply_effect(source: Path, dest: Path, name: str, rate: int = 44100) -> np.ndarray:
    cmd("ffmpeg", "-y", "-nostdin", "-v", "error", "-f", "f64le",
        "-ar", str(rate), "-ac", "2", "-i", str(source),
        "-map", "0:a:0", "-vn", "-sn", "-dn", "-af", name,
        "-c:a", "pcm_f64le", "-f", "f64le", str(dest))
    return raw(dest)


def main() -> None:
    for program in ("ffmpeg", "ffprobe"):
        if not shutil.which(program):
            raise SystemExit(f"SKIP: {program} unavailable")

    with tempfile.TemporaryDirectory(prefix="tp-hdcd-truepeak-") as dirname:
        root = Path(dirname)
        wav = root / "original-16.wav"
        flac = root / "original-16.flac"
        hdcd = root / "source-first-hdcd-s32.wav"
        high_precision = root / "hdcd-f64.wav"
        base_raw = root / "observed-base.f64le"
        write_sample_exact_stereo(wav)
        cmd("ffmpeg", "-y", "-nostdin", "-v", "error", "-i", str(wav),
            "-c:a", "flac", str(flac))
        decode_hdcd(flac, hdcd)
        to_double_wav(hdcd, high_precision)
        base = unwrap_double_wav(high_precision, base_raw)
        assert base.shape == (88_200, 2)
        # This is the actual post-HDCD signal. No +6 dB decoder compensation.
        assert 0.01 < float(np.max(abs(base))) < 0.5
        print("PASS source-first signed16 HDCD -> signed32 -> Float64, no compensating gain")

        deemph_path = root / "post-deemphasis.f64le"
        deemph = apply_effect(base_raw, deemph_path, "aemphasis=mode=reproduction:type=cd")
        assert deemph.shape == base.shape
        assert float(np.max(np.abs(deemph - base))) > 0.0001
        assert 0.0 < true_peak_approx(deemph) < true_peak_approx(base) * 1.2
        print("PASS CD de-emphasis consumes decoded Float64 without integer terminal")

        resampled_path = root / "post-soxr-88200.f64le"
        changed_rate = apply_effect(
            deemph_path, resampled_path,
            "aresample=resampler=soxr:precision=28:cutoff=0.95:out_sample_rate=88200",
        )
        assert abs(len(changed_rate) - len(deemph) * 2) < 80
        assert float(np.max(np.abs(changed_rate))) > 0
        assert not np.array_equal(changed_rate[:100], deemph[:100])
        print("PASS decoded -> Float64 de-emphasis -> one SoXR rate change -> Float64 scan input")

        # Track/Album scalar calculations deliberately use the *processed*
        # data. The fidelity/ceiling witness uses independent FIR approximation
        # and does not pretend to exercise TonePoet's certified scalar resolver.
        members = [changed_rate[: len(changed_rate) // 2],
                   changed_rate[len(changed_rate) // 2 :]]
        peaks = [true_peak_approx(member) for member in members]
        target_peak = 10.0 ** (-1.0 / 20.0)
        track_guards = [min(1.0, target_peak / peak) for peak in peaks]
        track_norms = [target_peak / peak for peak in peaks]
        album_guard = min(track_guards)
        album_norm = target_peak / max(peaks)
        for gain in track_guards + track_norms + [album_guard, album_norm]:
            assert math.isfinite(gain) and gain > 0
        for member, gain in zip(members, track_guards):
            assert true_peak_approx(member * gain) <= target_peak * 1.000_001
        for member, gain in zip(members, track_norms):
            assert abs(true_peak_approx(member * gain) - target_peak) < 1e-6
        for member in members:
            assert true_peak_approx(member * album_guard) <= target_peak * 1.000_001
            assert true_peak_approx(member * album_norm) <= target_peak * 1.000_001
        print("PASS independent processed-signal Track/Album Guard+Normalize scalar witnesses")

        # A CUE member must equal a cut of the continuous HDCD reconstruction,
        # not a fresh decoder invocation with its prior control state omitted.
        # The independent Peak Extend fixture tests the difference explicitly.
        start, count = 44_100 + 588, 43_000
        actual_cue = root / "cue-source-first.wav"
        cmd("ffmpeg", "-y", "-nostdin", "-v", "error", "-i", str(flac),
            "-map", "0:a:0", "-af",
            f"hdcd=bits_per_sample=16:disable_autoconvert=1,atrim=start_sample={start}:end_sample={start+count},asetpts=PTS-STARTPTS",
            "-c:a", "pcm_s32le", "-f", "wav", str(actual_cue))
        import wave
        with wave.open(str(actual_cue)) as f:
            actual_bytes = f.readframes(f.getnframes())
            assert f.getnframes() == count
        with wave.open(str(hdcd)) as f:
            whole_bytes = f.readframes(f.getnframes())
        assert actual_bytes == whole_bytes[start*8:(start+count)*8]
        print("PASS exact original-source CUE range after continuous HDCD reconstruction")

        # Terminal conversion from the final-rate *measured* signal, not the
        # encoded source. One Int16 dither/quantization, one lossy encoder.
        f64_wav = root / "final-rate-64.wav"
        cmd("ffmpeg", "-y", "-nostdin", "-v", "error", "-f", "f64le",
            "-ar", "88200", "-ac", "2", "-i", str(resampled_path),
            "-c:a", "pcm_f64le", str(f64_wav))
        int16 = root / "out-int16.wav"
        cmd("ffmpeg", "-y", "-nostdin", "-v", "error", "-i", str(f64_wav),
            "-af", "aresample=dither_method=triangular:out_sample_fmt=s16",
            "-c:a", "pcm_s16le", str(int16))
        assert probe(int16)["codec_name"] == "pcm_s16le"
        assert probe(int16)["sample_rate"] == "88200"
        print("PASS final-only Int16 TPDF/quantization from retained Float64")

        lossy = root / "out.mp3"
        cmd("ffmpeg", "-y", "-nostdin", "-v", "error", "-i", str(high_precision),
            "-c:a", "libmp3lame", "-b:a", "192k", str(lossy))
        assert probe(lossy)["codec_name"] == "mp3"
        assert probe(lossy)["sample_rate"] == "44100"
        print("PASS Float64 44.1-kHz -> MP3 terminal encoder without hidden rate change")


if __name__ == "__main__":
    main()
