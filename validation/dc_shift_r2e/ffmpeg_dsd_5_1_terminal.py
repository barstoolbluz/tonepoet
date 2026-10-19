#!/usr/bin/env python3
"""Standalone simulated six-channel DSD *post-reconstruction carrier* FLAC transport.

Checks raw Float64 ingress, speaker mask 0x3f, manual subtraction sign, and
24-bit encoded channel identity on host FFmpeg. Does NOT decode DSD, execute
TonePoet, or certify true-peak processing.
"""
import json
import math
from pathlib import Path
import struct
import subprocess
import tempfile


def command(*args):
    subprocess.run(args, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)


def main():
    rate, channels, frames = 176_400, 6, 12_000
    manual_offset = 0.003
    with tempfile.TemporaryDirectory(prefix='tonepoet-dsd-dc-terminal-') as tmp:
        root = Path(tmp)
        carrier, encoded, recovered = [root / name for name in
            ('corrected.f64le', 'final.flac', 'recovered.f64le')]
        expected = []
        with carrier.open('wb') as file:
            for index in range(frames):
                # Center-only programme. Other channels contain only the
                # offset to be subtracted, and must decode to digital silence.
                for channel in range(channels):
                    original = manual_offset + (0.45 * math.sin(2 * math.pi * index / 71)
                                                if channel == 2 else 0.0)
                    corrected = original - manual_offset
                    expected.append(corrected)
                    file.write(struct.pack('<d', corrected))
        command('ffmpeg', '-hide_banner', '-loglevel', 'error', '-y',
                '-f', 'f64le', '-ar', str(rate), '-ac', str(channels),
                '-channel_layout', '0x3f', '-i', str(carrier), '-map', '0:a:0',
                '-c:a', 'flac', '-sample_fmt', 's32', '-bits_per_raw_sample', '24',
                str(encoded))
        probe = json.loads(subprocess.check_output([
            'ffprobe', '-v', 'error', '-select_streams', 'a:0',
            '-show_entries', 'stream=channels,channel_layout,bits_per_raw_sample',
            '-of', 'json', str(encoded),
        ]))['streams'][0]
        assert int(probe['channels']) == channels, probe
        assert probe['channel_layout'] == '5.1', probe
        assert int(probe['bits_per_raw_sample']) == 24, probe
        command('ffmpeg', '-hide_banner', '-loglevel', 'error', '-y',
                '-i', str(encoded), '-map', '0:a:0', '-c:a', 'pcm_f64le',
                '-f', 'f64le', str(recovered))
        raw = recovered.read_bytes()
        assert len(raw) == len(expected) * 8
        samples = [x[0] for x in struct.iter_unpack('<d', raw)]
        max_error = max(abs(a - b) for a, b in zip(expected, samples))
        assert max_error <= 2**-22, max_error
        peaks = [max(abs(samples[channels * i + c]) for i in range(frames))
                 for c in range(channels)]
        assert 0.44 < peaks[2] < 0.46, peaks
        assert all(peaks[c] == 0 for c in range(channels) if c != 2), peaks
        print(json.dumps({
            'result': 'PASS', 'signal': 'synthetic DSD post-reconstruction f64 carrier',
            'sample_rate_hz': rate, 'channel_mask': '0x3f', 'layout': probe['channel_layout'],
            'bits_per_raw_sample': 24, 'source_manual_offset_subtracted': manual_offset,
            'center_channel_index': 2, 'decoded_channel_peaks': peaks,
            'max_pcm_abs_error': max_error,
            'qualification': 'FFmpeg terminal transport only; no DSD decode, Rust or true-peak',
        }, indent=2))


if __name__ == '__main__':
    main()
