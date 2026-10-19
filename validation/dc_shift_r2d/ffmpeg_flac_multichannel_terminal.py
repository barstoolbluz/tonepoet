#!/usr/bin/env python3
"""Standalone FFmpeg raw-Float64 -> FLAC Int24 speaker-layout regression.

Checks the physical encoder-input premise only, not TonePoet's Rust planner,
DC estimator, or certified true-peak execution. Requires ffmpeg and ffprobe.
"""
import json
import math
import pathlib
import struct
import subprocess
import tempfile


def run(*args):
    subprocess.run(args, check=True, stdout=subprocess.DEVNULL)


def probe(path):
    return json.loads(subprocess.check_output([
        "ffprobe", "-v", "error", "-select_streams", "a:0", "-show_entries",
        "stream=channels,channel_layout,bits_per_raw_sample", "-of", "json", str(path),
    ]))["streams"][0]


def check_case(root, name, channels, mask, expected_layout, active_channel):
    frames = 8192
    source = root / (name + ".corrected.f64le")
    signal = [0.0] * (frames * channels)
    # A center-only or rear-left-only programme with corrected DC and safely
    # sub-ceiling peaks. On a manually shifted source every channel would
    # originally have +0.003 additional DC.
    for frame in range(frames):
        signal[frame * channels + active_channel] = 0.45 * math.sin(2 * math.pi * frame / 71)
    source.write_bytes(struct.pack("<" + "d" * len(signal), *signal))
    output = root / (name + ".flac")
    run("ffmpeg", "-hide_banner", "-loglevel", "error", "-y",
        "-f", "f64le", "-ar", "48000", "-ac", str(channels),
        "-channel_layout", mask, "-i", str(source), "-map", "0:a:0",
        "-c:a", "flac", "-sample_fmt", "s32", "-bits_per_raw_sample", "24",
        str(output))
    facts = probe(output)
    assert int(facts["channels"]) == channels, facts
    assert facts["channel_layout"] == expected_layout, facts
    assert str(facts["bits_per_raw_sample"]) == "24", facts
    recovered = root / (name + ".decoded.f64le")
    run("ffmpeg", "-hide_banner", "-loglevel", "error", "-y",
        "-i", str(output), "-map", "0:a:0", "-c:a", "pcm_f64le",
        "-f", "f64le", str(recovered))
    data = recovered.read_bytes()
    assert len(data) == len(signal) * 8
    decoded = [item[0] for item in struct.iter_unpack("<d", data)]
    worst_diff = max(abs(a - b) for a, b in zip(signal, decoded))
    assert worst_diff <= 2 ** -22, (name, worst_diff)
    for ch in range(channels):
        peak = max(abs(decoded[frame * channels + ch]) for frame in range(frames))
        if ch == active_channel:
            assert 0.44 <= peak <= 0.451, (name, ch, peak)
        else:
            assert peak == 0.0, (name, ch, peak)
    return {
        "case": name, "layout": facts["channel_layout"],
        "channels": channels, "bits_per_raw_sample": facts["bits_per_raw_sample"],
        "active_channel_index": active_channel, "maximum_absolute_pcm_error": worst_diff,
        "decoded_nonactive_channels_silent": True,
    }


def main():
    with tempfile.TemporaryDirectory(prefix="tonepoet-r2d-terminal-") as temp:
        root = pathlib.Path(temp)
        results = [
            check_case(root, "3.0_center", 3, "0x7", "3.0", 2),
            check_case(root, "quad_rear_left", 4, "0x33", "quad", 2),
        ]
    print(json.dumps(results, indent=2))


if __name__ == "__main__":
    main()
