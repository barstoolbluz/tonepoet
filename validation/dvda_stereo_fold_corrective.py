#!/usr/bin/env python3
"""DVD-A LPCM source order -> WAVEFORMATEXTENSIBLE -> SoX Float64 -> production pan.

Physical decoded-sample regressions, including wrong-layout reproduction and a
reused carrier. Uses actual SoX/FFmpeg. Rust DVD-A policy/cache wiring is covered
by unit tests, not executed by this script.
"""
import json
from pathlib import Path
import struct
import subprocess
import tempfile

from channel_layout_binding_corrective import PAN, stereo_raw, frame_levels, assert_close

RATE = 48_000
FRAMES = 4_800


def run(*argv):
    proc = subprocess.run(argv, capture_output=True, text=True)
    if proc.returncode:
        raise AssertionError(f"{argv!r} failed: {proc.stderr}")
    return proc.stdout


def pcm_s32(path: Path, frame):
    encoded = [int(value * (1 << 31)) for value in frame]
    path.write_bytes(struct.pack('<' + 'i' * len(frame), *encoded) * FRAMES)


def layout(path: Path):
    probe = run('ffprobe', '-v', 'error', '-select_streams', 'a:0',
                '-show_entries', 'stream=channels,channel_layout', '-of', 'json', str(path))
    return json.loads(probe)['streams'][0]


def mux(raw: Path, out: Path, channel_count: int, source_layout=None):
    args = ['ffmpeg', '-nostdin', '-y', '-hide_banner', '-loglevel', 'error',
            '-f', 's32le', '-ar', str(RATE), '-ac', str(channel_count)]
    if source_layout:
        args.extend(['-channel_layout', source_layout])
    run(*args, '-i', str(raw), '-map', '0:a:0',
        '-c:a', 'pcm_s32le', '-f', 'wav', str(out))


def wave_format_mask(path: Path):
    """Read the actual WAVEFORMATEXTENSIBLE dwChannelMask, not ffprobe inference."""
    payload = path.read_bytes()[:4096]
    assert payload[:4] in (b'RIFF', b'RF64') and payload[8:12] == b'WAVE'
    offset = 12
    while offset + 8 <= len(payload):
        size = struct.unpack_from('<I', payload, offset + 4)[0]
        if payload[offset:offset + 4] == b'fmt ':
            assert size >= 40 and struct.unpack_from('<H', payload, offset + 8)[0] == 0xfffe
            return struct.unpack_from('<I', payload, offset + 8 + 20)[0]
        offset += 8 + size + (size & 1)
    raise AssertionError(f'WAVEFORMATEXTENSIBLE fmt chunk missing: {path}')


def fold(source: Path, output: Path, mask=None):
    args = ['ffmpeg', '-nostdin', '-y', '-hide_banner', '-loglevel', 'error']
    if mask:
        args.extend(['-channel_layout', mask])
    run(*args, '-i', str(source), '-map', '0:a:0', '-af', PAN,
        '-c:a', 'pcm_f64le', '-f', 'f64le', str(output))
    return frame_levels(stereo_raw(output))


def run_case(directory: Path, name, assignment, source_order, permutation, wave_alias, mask, expected):
    # Each frame is in DVD-Audio *group* order. WFX reorders the actual sample
    # words; it does not only change the WAV header's description.
    raw = directory / f'{name}.dvd.s32le'
    original_wav = directory / f'{name}.wav'  # legacy source-order cache
    wave_raw = directory / f'{name}.wfx.s32le'
    wave_wav = directory / f'{name}_wfx.wav'  # separate policy cache
    pcm_s32(raw, source_order)
    mux(raw, original_wav, len(source_order))
    pcm_s32(wave_raw, [source_order[index] for index in permutation])
    mux(wave_raw, wave_wav, len(source_order), wave_alias)

    original_layout = layout(original_wav)
    fixed_layout = layout(wave_wav)
    assert fixed_layout == {'channels': len(source_order), 'channel_layout': wave_alias}, fixed_layout
    assert wave_format_mask(wave_wav) == int(mask, 16), (name, wave_format_mask(wave_wav), mask)

    # This first leg reproduces the actual defective behavior (FFmpeg's
    # default count-derived input layout on source-ordered samples).
    wrong = directory / f'{name}.wrong.f64le'
    old_left, old_right = fold(original_wav, wrong)
    if assignment == 3:  # quad RL misread as center: audio goes to both
        assert old_left > .05 and old_right > .05, (name, old_left, old_right)
    if assignment == 19:  # DVD-A channel 5 is C; guessed WAV channel 5 is BR
        assert old_left < 1e-9 and old_right > .05, (name, old_left, old_right)

    # Produce the same Float64 WAV intermediate that previously dropped the
    # source roles. Bind the verified WFX layout just before production pan.
    sox_wav = directory / f'{name}.sox.f64.wav'
    run('sox', str(wave_wav), '-t', 'wav', '-e', 'floating-point',
        '-b', '64', str(sox_wav))
    direct = directory / f'{name}.direct.f64le'
    fixed = directory / f'{name}.bound.f64le'
    reference_levels = fold(wave_wav, direct)
    levels = fold(sox_wav, fixed, mask)
    assert_close(stereo_raw(fixed), stereo_raw(direct))

    if expected == 'left':
        assert levels[0] > .05 and levels[1] < 1e-9, (name, levels)
    elif expected == 'both':
        assert levels[0] > .05 and levels[1] > .05
        assert abs(levels[0] - levels[1]) < 1e-9
    else:
        assert levels[0] < 1e-9 and levels[1] < 1e-9

    # A second consumer of the cached reordered carrier must see identical
    # samples; its name cannot collide with the source-ordered variant.
    assert original_wav != wave_wav
    from_cache = directory / f'{name}.cached.f64le'
    fold(sox_wav, from_cache, mask)
    assert_close(stereo_raw(fixed), stereo_raw(from_cache))

    # The ordinary channel-preserving decode remains sample-position neutral.
    plain = directory / f'{name}.plain.f64le'
    run('ffmpeg', '-nostdin', '-y', '-v', 'error', '-i', str(original_wav),
        '-map', '0:a:0', '-c:a', 'pcm_f64le', '-f', 'f64le', str(plain))
    first = struct.unpack('<' + 'd' * len(source_order), plain.read_bytes()[:8*len(source_order)])
    assert max(abs(x-y) for x, y in zip(first, source_order)) < 2e-8, (name, first)
    print(f'PASS DVD-A {name}: assignment={assignment} old={old_left:.5f}/{old_right:.5f} '
          f'wfx={levels[0]:.5f}/{levels[1]:.5f} cache=reusable '
          f'original_layout={original_layout["channel_layout"]}')


def assignment_18_case(directory: Path, name, source_order, expected):
    # DVD-A 18: L,R,Ls,Rs,LFE. The existing native LPCM WaveExtensible
    # reordering produces FL,FR,LFE,BL,BR, with physical mask 0x3b.
    assert len(source_order) == 5
    raw = directory / f'{name}.dvd.s32le'
    legacy = directory / f'{name}.wav'
    wave_raw = directory / f'{name}.wfx.s32le'
    wave_wav = directory / f'{name}_wfx.wav'
    pcm_s32(raw, source_order)
    mux(raw, legacy, 5)
    reordered = [source_order[i] for i in (0, 1, 4, 2, 3)]
    pcm_s32(wave_raw, reordered)

    # Reproduce the *previous bundle's* failed cache content. Correctly
    # reordered samples labelled as FFmpeg 4.1 are not DVD-A assignment 18.
    mux(wave_raw, wave_wav, 5, '4.1')
    assert layout(wave_wav)['channel_layout'] == '4.1'
    assert wave_format_mask(wave_wav) == 0x10f
    old = directory / f'{name}.old.f64le'
    old_left, old_right = fold(wave_wav, old)
    if expected == 'none':
        assert old_left > .05 and old_right > .05, (name, old_left, old_right)
    elif expected == 'left':
        assert old_left < 1e-9 and old_right < 1e-9, (name, old_left, old_right)
    else:
        assert old_left > .05 and old_right > .05, (name, old_left, old_right)

    # The Rust cache validator now rejects the stale 4.1 carrier and
    # regenerates it. This physical reproducer models that regeneration;
    # cache invalidation itself is asserted in a Rust unit test.
    mux(wave_raw, wave_wav, 5, '0x3b')
    assert layout(wave_wav) == {
        'channels': 5, 'channel_layout': '5 channels (FL+FR+LFE+BL+BR)'
    }
    assert wave_format_mask(wave_wav) == 0x3b
    sox_wav = directory / f'{name}.sox.f64.wav'
    run('sox', str(wave_wav), '-t', 'wav', '-e', 'floating-point',
        '-b', '64', str(sox_wav))
    direct = directory / f'{name}.direct.f64le'
    fixed = directory / f'{name}.bound.f64le'
    cached = directory / f'{name}.cached.f64le'
    fold(wave_wav, direct)
    levels = fold(sox_wav, fixed, '0x3b')
    assert_close(stereo_raw(fixed), stereo_raw(direct))
    fold(sox_wav, cached, '0x3b')
    assert_close(stereo_raw(fixed), stereo_raw(cached))
    if expected == 'none':
        assert levels[0] < 1e-9 and levels[1] < 1e-9, (name, levels)
    elif expected == 'left':
        assert levels[0] > .05 and levels[1] < 1e-9, (name, levels)
    else:
        assert levels[0] < 1e-9 and levels[1] > .05, (name, levels)
    # Default DVD-A channel-preserving carrier remains in original group order.
    plain = directory / f'{name}.plain.f64le'
    run('ffmpeg', '-nostdin', '-y', '-v', 'error', '-i', str(legacy),
        '-map', '0:a:0', '-c:a', 'pcm_f64le', '-f', 'f64le', str(plain))
    first = struct.unpack('<5d', plain.read_bytes()[:40])
    assert max(abs(a-b) for a, b in zip(first, source_order)) < 2e-8
    print(f'PASS DVD-A {name}: assignment=18 old={old_left:.5f}/{old_right:.5f} '
          f'correct={levels[0]:.5f}/{levels[1]:.5f} cache=consistent mask=0x3b')


def main():
    with tempfile.TemporaryDirectory(prefix='tonepoet-dvda-stereo-') as temp:
        p = Path(temp)
        run_case(p, 'quad_rl_only', 3, [0, 0, .45, 0], [0, 1, 2, 3], 'quad', '0x33', 'left')
        run_case(p, 'five_center_only', 19, [0, 0, 0, 0, .45], [0, 1, 4, 2, 3], '5.0', '0x37', 'both')
        run_case(p, '2_1_lfe_only', 4, [0, 0, .45], [0, 1, 2], '2.1', '0xb', 'none')
        assignment_18_case(p, 'five_lfe_only', [0, 0, 0, 0, .45], 'none')
        assignment_18_case(p, 'five_rear_left_only', [0, 0, .45, 0, 0], 'left')
        assignment_18_case(p, 'five_rear_right_only', [0, 0, 0, .45, 0], 'right')
    print('PASS: 6 DVD-A decoded-sample regressions, prior failures and cache readback')


if __name__ == '__main__':
    main()
