#!/usr/bin/env python3
"""Compile-free witness for the FFmpeg sample-format cells used after HDCD.

This is a standalone encoder/filter probe, not TonePoet Rust execution, and
is not a substitute for the pinned-FFmpeg / full-workspace qualification gate.
"""
import json
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path


def run(*cmd):
    result = subprocess.run(cmd, capture_output=True, text=True)
    if result.returncode:
        raise RuntimeError(f"command failed ({result.returncode}): {cmd!r}\n{result.stderr[-2200:]}")
    return result


def stream(path):
    report = json.loads(run('ffprobe', '-v', 'error', '-select_streams', 'a:0',
                            '-show_entries', 'stream=sample_rate,codec_name',
                            '-of', 'json', str(path)).stdout)
    return report['streams'][0]


def main():
    if not shutil.which('ffmpeg') or not shutil.which('ffprobe'):
        print('SKIP: ffmpeg and ffprobe required')
        return 77
    with tempfile.TemporaryDirectory(prefix='hdcd-precision-') as tmp:
        tmp = Path(tmp)
        source = tmp / 'source.s32.wav'
        run('ffmpeg', '-hide_banner', '-loglevel', 'error', '-nostdin', '-y',
            '-f', 'lavfi', '-i', 'sine=frequency=997:sample_rate=44100:duration=0.31',
            '-ac', '2', '-c:a', 'pcm_s32le', str(source))
        # The source-first HDCD smoke is exercised by the sibling test. The
        # present fixture models the real decoder's signed-Int32 carrier.
        opus = tmp / 'out.opus'
        run('ffmpeg', '-hide_banner', '-loglevel', 'error', '-nostdin', '-y',
            '-i', str(source), '-af',
            'aresample=resampler=soxr:precision=28:cutoff=0.95:out_sample_rate=48000',
            '-ar', '48000', '-sample_fmt', 'flt', '-c:a', 'libopus', str(opus))
        assert int(stream(opus)['sample_rate']) == 48000
        print('PASS: S32 source -> explicit SoXR 44.1/48 -> Float32-input libopus')

        mp3 = tmp / 'out.mp3'
        run('ffmpeg', '-hide_banner', '-loglevel', 'error', '-nostdin', '-y',
            '-i', str(source), '-ar', '44100', '-sample_fmt', 'fltp',
            '-c:a', 'libmp3lame', str(mp3))
        assert int(stream(mp3)['sample_rate']) == 44100
        print('PASS: S32 source -> planar Float32-input libmp3lame')

        aac_filter = ('aresample=resampler=soxr:precision=28:cutoff=0.95:'
                      'dither_method=triangular:out_sample_fmt=s16')
        quantized = tmp / 'aac-terminal.pcm16.wav'
        run('ffmpeg', '-hide_banner', '-loglevel', 'error', '-nostdin', '-y',
            '-i', str(source), '-af', aac_filter,
            '-c:a', 'pcm_s16le', str(quantized))
        assert int(stream(quantized)['sample_rate']) == 44100
        print('PASS: S32 source -> one no-rate-change SoXR TPDF/Int16 terminal')

        encoders = run('ffmpeg', '-hide_banner', '-encoders').stdout
        if 'libfdk_aac' in encoders:
            aac = tmp / 'out.m4a'
            run('ffmpeg', '-hide_banner', '-loglevel', 'error', '-nostdin', '-y',
                '-i', str(source), '-af', aac_filter,
                '-ar', '44100', '-sample_fmt', 's16', '-c:a', 'libfdk_aac', str(aac))
            assert int(stream(aac)['sample_rate']) == 44100
            print('PASS: S32 source -> TPDF/Int16 -> libfdk_aac')
        else:
            print('NOT RUN: libfdk_aac unavailable (pinned-host gate required)')
    return 0


if __name__ == '__main__':
    try:
        sys.exit(main())
    except (AssertionError, KeyError, RuntimeError) as exc:
        print(f'FAIL: {exc}', file=sys.stderr)
        sys.exit(1)
