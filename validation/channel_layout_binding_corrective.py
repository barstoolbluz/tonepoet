#!/usr/bin/env python3
"""Independent decoded-audio regressions for verified speaker-role binding.

All physical fold filters are loaded from the production semantic plan. The
pipeline under test is actual FFmpeg/SoX, not a mathematical reimplementation.
No Rust qualification is implied by this external smoke test.
"""
import json
from pathlib import Path
import re
import struct
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parent.parent
SEMANTIC = (ROOT / 'tonepoet-pipeline/src/semantic_plan.rs').read_text()
PAN = re.search(r'STEREO_FOLD_FFMPEG_FILTER: &str = "([^"]+)"', SEMANTIC).group(1)


def run(*args):
    result = subprocess.run(args, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    if result.returncode:
        raise AssertionError(f'process {args} failed:\n{result.stderr}')
    return result.stdout


def stereo_raw(path):
    data = path.read_bytes()
    assert data and len(data) % 16 == 0, (path, len(data))
    return list(struct.iter_unpack('<dd', data))


def assert_close(actual, reference, tol=2e-5):
    assert len(actual) == len(reference), (len(actual), len(reference))
    for got, want in zip(actual, reference):
        assert abs(got[0] - want[0]) < tol, (got, want)
        assert abs(got[1] - want[1]) < tol, (got, want)


def frame_levels(frames):
    return (max(abs(left) for left, _ in frames), max(abs(right) for _, right in frames))


CASES = [
    # name, layout alias, layout mask, channel values, expected channel behavior
    ('3.0_center', '3.0', '0x7', [0, 0, .5], 'both'),
    ('2.1_lfe', '2.1', '0xb', [0, 0, .5], 'none'),
    ('quad_rear_left', 'quad', '0x33', [0, 0, .5, 0], 'left'),
    ('4.0_back_center', '4.0', '0x107', [0, 0, 0, .5], 'both'),
    ('5.1_back_left', '5.1', '0x3f', [0, 0, 0, 0, .5, 0], 'left'),
    ('5.1_side_left', '5.1(side)', '0x60f', [0, 0, 0, 0, .5, 0], 'left'),
    ('mono', 'mono', '0x4', [.5], 'both'),
    ('stereo', 'stereo', '0x3', [.5, 0], 'left'),
]


def run_case(tmp, name, alias, mask, values, behavior, raw=False):
    path = tmp / (name + '.wav')
    f64wave = tmp / (name + '_sox.wav')
    direct = tmp / (name + '_direct.raw')
    fixed = tmp / (name + '_fixed.raw')
    wrong = tmp / (name + '_wrong.raw')
    expressions = '|'.join(str(v) for v in values)
    run('ffmpeg', '-nostdin', '-hide_banner', '-loglevel', 'error', '-y',
        '-f', 'lavfi', '-i', f'aevalsrc={expressions}:channel_layout={alias}:sample_rate=48000:duration=0.1',
        '-c:a', 'pcm_s24le', str(path))
    # This is the source-to-intermediate operation that erased the mask.
    run('sox', str(path), '-t', 'wav', '-e', 'floating-point', '-b', '64', str(f64wave))
    run('ffmpeg', '-nostdin', '-hide_banner', '-loglevel', 'error', '-y',
        '-i', str(path), '-af', PAN, '-c:a', 'pcm_f64le', '-f', 'f64le', str(direct))
    ingress = f64wave
    args = []
    if raw:
        ingress = tmp / (name + '_sox.f64le')
        run('ffmpeg', '-nostdin', '-hide_banner', '-loglevel', 'error', '-y',
            '-i', str(f64wave), '-c:a', 'pcm_f64le', '-f', 'f64le', str(ingress))
        args = ['-f', 'f64le', '-ar', '48000', '-ac', str(len(values))]
    run('ffmpeg', '-nostdin', '-hide_banner', '-loglevel', 'error', '-y',
        *args, '-channel_layout', mask, '-i', str(ingress),
        '-af', PAN, '-c:a', 'pcm_f64le', '-f', 'f64le', str(fixed))
    golden = stereo_raw(direct)
    actual = stereo_raw(fixed)
    assert_close(actual, golden)
    left, right = frame_levels(actual)
    if behavior == 'both':
        assert left > .02 and right > .02, (name, left, right)
    elif behavior == 'left':
        assert left > .02 and right < 1e-9, (name, left, right)
    elif behavior == 'none':
        assert left < 1e-9 and right < 1e-9, (name, left, right)

    if name == '3.0_center':
        run('ffmpeg', '-nostdin', '-hide_banner', '-loglevel', 'error', '-y',
            '-i', str(f64wave), '-af', PAN, '-c:a', 'pcm_f64le', '-f', 'f64le', str(wrong))
        assert max(frame_levels(stereo_raw(wrong))) < 1e-9, 'the original silent-center bug did not reproduce'
    print(f'PASS {name}{" (raw Float64 ingress)" if raw else " (SoX WAV ingress)"}: L={left:.6f} R={right:.6f}')


def run_codec_case(tmp, name, alias, mask, codec):
    """Exercise a real decoded FLAC/WavPack ingress and a SoX F64 WAV carrier."""
    source = tmp / (name + '.wav')
    encoded = tmp / f'{name}.{codec}'
    sox_wave = tmp / f'{name}.{codec}.sox.wav'
    direct = tmp / f'{name}.{codec}.direct.raw'
    fixed = tmp / f'{name}.{codec}.fixed.raw'
    encoder = 'flac' if codec == 'flac' else 'wavpack'
    run('ffmpeg', '-nostdin', '-hide_banner', '-loglevel', 'error', '-y',
        '-i', str(source), '-c:a', encoder, str(encoded))
    probe = run('ffprobe', '-v', 'error', '-select_streams', 'a:0',
                '-show_entries', 'stream=codec_name,channels,channel_layout',
                '-of', 'json', str(encoded))
    stream = json.loads(probe)['streams'][0]
    assert stream['channels'] == (3 if alias == '3.0' else 4), stream
    assert stream['channel_layout'] == alias, (name, codec, stream)
    # Decode with the same real SoX backend that loses original channel masks.
    run('sox', str(encoded), '-t', 'wav', '-e', 'floating-point', '-b', '64', str(sox_wave))
    run('ffmpeg', '-nostdin', '-hide_banner', '-loglevel', 'error', '-y',
        '-i', str(encoded), '-af', PAN, '-c:a', 'pcm_f64le', '-f', 'f64le', str(direct))
    run('ffmpeg', '-nostdin', '-hide_banner', '-loglevel', 'error', '-y',
        '-channel_layout', mask, '-i', str(sox_wave), '-af', PAN,
        '-c:a', 'pcm_f64le', '-f', 'f64le', str(fixed))
    assert_close(stereo_raw(fixed), stereo_raw(direct))
    left, right = frame_levels(stereo_raw(fixed))
    assert left > .02 and (right > .02 if alias == '3.0' else right < 1e-9)
    print(f'PASS {name} {codec} source + SoX WAV: L={left:.6f} R={right:.6f}')


def main():
    with tempfile.TemporaryDirectory(prefix='tonepoet-layout-corrective-') as root:
        tmp = Path(root)
        for case in CASES:
            run_case(tmp, *case)
        run_case(tmp, *CASES[0], raw=True)
        run_case(tmp, *CASES[2], raw=True)
        for codec in ('flac', 'wv'):
            run_codec_case(tmp, '3.0_center', '3.0', '0x7', codec)
            run_codec_case(tmp, 'quad_rear_left', 'quad', '0x33', codec)
        # The original source must differentiate quad and 4.0. A four-channel
        # count cannot decide whether channel 3 is rear left or front center.
        probe = run('ffprobe', '-v', 'error', '-select_streams', 'a:0',
                    '-show_entries', 'stream=channels,channel_layout', '-of', 'json',
                    str(tmp / 'quad_rear_left.wav'))
        streams = json.loads(probe)['streams']
        assert streams[0]['channel_layout'] == 'quad', streams
    print(f'PASS: {len(CASES) + 6} decoded-audio correction cases')


if __name__ == '__main__':
    main()
