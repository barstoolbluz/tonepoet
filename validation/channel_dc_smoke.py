#!/usr/bin/env python3
"""Compile-free test of actual SoX/FFmpeg fold and 2-Hz high-pass lowerings.

This does NOT replace Rust build tests or certify the TonePoet runtime planner.
"""
import array
import math
import pathlib
import re
import shutil
import subprocess
import sys
import tempfile

RATE = 48000
SOURCE = pathlib.Path(__file__).resolve().parents[1] / 'tonepoet-pipeline/src/semantic_plan.rs'
TEXT = SOURCE.read_text(encoding='utf-8')
PAN_MATCH = re.search(r'pub const STEREO_FOLD_FFMPEG_FILTER: &str = "([^"]+)";', TEXT)
assert PAN_MATCH, 'production filter authority was not found'
PAN = PAN_MATCH.group(1)
assert 'FfmpegAudioFilter(\n                STEREO_FOLD_FFMPEG_FILTER.to_owned()' in TEXT
assert '"highpass=f=2:p=2:precision=f64"' in TEXT
assert '"highpass".to_owned(), "2".to_owned()' in TEXT

def invoke(command):
    proc = subprocess.run(command, capture_output=True, text=True, check=False)
    if proc.returncode:
        raise AssertionError(f"{command[0]} returned {proc.returncode}: {proc.stderr[:1200]}")

def samples(path):
    data = array.array('d')
    with path.open('rb') as f:
        data.frombytes(f.read())
    if sys.byteorder != 'little':
        data.byteswap()
    return data

def write_samples(path, values):
    data = array.array('d', values)
    if sys.byteorder != 'little':
        data.byteswap()
    path.write_bytes(data.tobytes())

def rms(values):
    return math.sqrt(sum(x * x for x in values) / len(values))

def main():
    if not shutil.which('ffmpeg') or not shutil.which('sox'):
        print('SKIPPED: requires FFmpeg and SoX on PATH')
        return 77
    with tempfile.TemporaryDirectory(prefix='tonepoet-channel-dc-') as tmp:
        d = pathlib.Path(tmp)
        # The raw-input -ac channel order is chosen by FFmpeg. Five point one
        # has FL/FR/FC/LFE/BL/BR OR FL/FR/FC/LFE/SL/SR. The matrix gives the
        # two surround families symmetric weights so either maps correctly.
        for channels in range(1, 9):
            src, folded = d / f'in-{channels}.f64le', d / f'fold-{channels}.f64le'
            duration = RATE // 4
            # DC-neutral, source-sample peak-bounded waveforms.
            write_samples(src, (
                (0.21 * math.sin(2 * math.pi * (120 + c * 27) * n / RATE))
                for n in range(duration) for c in range(channels)
            ))
            invoke(['ffmpeg', '-nostdin', '-y', '-v', 'error',
                    '-f', 'f64le', '-ar', str(RATE), '-ac', str(channels),
                    '-i', str(src), '-af', PAN, '-c:a', 'pcm_f64le',
                    '-f', 'f64le', str(folded)])
            original, out = samples(src), samples(folded)
            assert len(out) == duration * 2, (channels, len(out))
            peak_orig, peak_folded = max(map(abs, original)), max(map(abs, out))
            assert peak_folded <= peak_orig + 1e-10, (channels, peak_folded, peak_orig)
            if channels == 2:
                assert max(abs(out[i] - original[i]) for i in range(len(out))) < 1e-10
            print(f'PASS: pan input={channels} channels, output=2, coherent-safe peaks')

        # Explicit physical layout metadata must not swap back vs side
        # surrounds: the production matrix uses speaker roles rather than
        # hard-coded positional indices. Both layouts carry the same content
        # in the standardized FL/FR/FC/LFE/[back or side] positions.
        for layout in ['5.1', '5.1(side)']:
            source = d / f'layout-{layout.replace("(", "_").replace(")", "")}.f64le'
            folded = d / f'layout-{layout.replace("(", "_").replace(")", "")}-stereo.f64le'
            write_samples(source, (
                (0.25 if c in (2, 4, 5) else 0.0)
                for _ in range(4096) for c in range(6)
            ))
            invoke(['ffmpeg', '-nostdin', '-y', '-v', 'error',
                    '-f', 'f64le', '-ar', str(RATE), '-ac', '6',
                    '-channel_layout', layout, '-i', str(source),
                    '-af', PAN, '-c:a', 'pcm_f64le', '-f', 'f64le', str(folded)])
            left, right = samples(folded)[:2]
            assert 0.14 < left < 0.15 and 0.14 < right < 0.15, (layout, left, right)
            print(f'PASS: explicit {layout} surround roles fold symmetrically')

        # A multichannel LFE-only signal must never leak into stereo output.
        src, out = d / 'lfe-in.f64le', d / 'lfe-out.f64le'
        write_samples(src, (0.8 if c == 3 else 0.0 for _ in range(4096) for c in range(6)))
        invoke(['ffmpeg', '-nostdin', '-y', '-v', 'error', '-f', 'f64le',
                '-ar', str(RATE), '-ac', '6', '-i', str(src), '-af', PAN,
                '-c:a', 'pcm_f64le', '-f', 'f64le', str(out)])
        assert max(map(abs, samples(out))) < 1e-11
        print('PASS: LFE-only input excluded from stereo fold')

        # Both backends need to materially suppress constant DC without
        # flattening 440 Hz programme content over a practical duration.
        src = d / 'dc-in.f64le'
        frames = RATE * 4
        write_samples(src, (0.08 + 0.1 * math.sin(2 * math.pi * 440 * n / RATE)
                            for n in range(frames)))
        for tool in ['ffmpeg', 'sox']:
            dst = d / f'dc-{tool}.f64le'
            if tool == 'ffmpeg':
                cmd = ['ffmpeg', '-nostdin', '-y', '-v', 'error', '-f', 'f64le',
                       '-ar', str(RATE), '-ac', '1', '-i', str(src),
                       '-af', 'highpass=f=2:p=2:precision=f64',
                       '-c:a', 'pcm_f64le', '-f', 'f64le', str(dst)]
            else:
                cmd = ['sox', '-t', 'raw', '-e', 'floating-point', '-b', '64', '-L',
                       '-r', str(RATE), '-c', '1', str(src), '-t', 'raw',
                       '-e', 'floating-point', '-b', '64', '-L', '-r', str(RATE),
                       '-c', '1', str(dst), 'highpass', '2']
            invoke(cmd)
            clean = samples(dst)
            assert len(clean) == frames
            settled = clean[2 * RATE:]
            mean = sum(settled) / len(settled)
            ac_rms = rms([x - mean for x in settled])
            assert abs(mean) < 0.0003, (tool, mean)
            assert 0.068 < ac_rms < 0.073, (tool, ac_rms)
            print(f'PASS: {tool} high-pass suppresses DC, preserves 440 Hz (RMS={ac_rms:.5f})')
    return 0

if __name__ == '__main__':
    sys.exit(main())
