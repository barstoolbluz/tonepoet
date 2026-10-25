#!/usr/bin/env python3
"""Independent IP-07 FFmpeg seek/CRC/atrim/s32le contract smoke fixture.

This does not execute Rust, ToolRunner, NativeReplayGainObserver, or the album
path; it proves only the standalone command's exact PCM slice for selected
formats/boundaries using the installed FFmpeg and independently decoded PCM.
"""
import argparse
import hashlib
import json
import subprocess
import tempfile
from pathlib import Path


def execute(args):
    result = subprocess.run(args, capture_output=True, check=False)
    if result.returncode:
        raise RuntimeError(f"command exited {result.returncode}: {' '.join(map(str,args))}\n{result.stderr[-1000:].decode('utf8','replace')}")
    return result.stdout


def digest(value):
    return hashlib.sha256(value).hexdigest()


def run(root):
    version = execute(['ffmpeg', '-version']).decode('utf8', 'replace').splitlines()[0]
    details = []
    for rate, channels, bits in [(44100, 1, 16), (44100, 2, 16), (44100, 2, 24),
                                  (48000, 2, 16), (96000, 1, 24)]:
        frames = rate * 3 + 137
        source = root / f'flac_{rate}_{channels}_{bits}.flac'
        create = ['ffmpeg', '-hide_banner', '-nostdin', '-loglevel', 'error', '-y',
                  '-f', 'lavfi', '-i', f'sine=frequency=701:sample_rate={rate}:duration=4',
                  '-af', f'atrim=end_sample={frames}', '-ac', str(channels),
                  '-c:a', 'flac']
        if bits == 24:
            create += ['-sample_fmt', 's32', '-bits_per_raw_sample', '24']
        else:
            create += ['-sample_fmt', 's16']
        execute(create + [str(source)])
        full = execute(['ffmpeg', '-v', 'error', '-hide_banner', '-nostdin',
                        '-err_detect', 'crccheck+explode', '-i', str(source),
                        '-map', '0:a:0', '-vn', '-sn', '-dn', '-f', 's32le',
                        '-c:a', 'pcm_s32le', 'pipe:1'])
        width = channels * 4
        if len(full) != frames * width:
            raise AssertionError(f'full decode frames differ from generated extent: {rate}/{channels}/{bits}')
        boundaries = [(0, 588), (588, 1101), (rate - 23, rate + 19),
                      (rate + 1, 2 * rate + 7), (2 * rate + 11, frames)]
        for start, end in boundaries:
            trim_start = start % rate
            command = ['ffmpeg', '-v', 'error', '-hide_banner', '-nostdin', '-xerror',
                       '-accurate_seek', '-ss', str(start // rate),
                       '-err_detect', 'crccheck+explode', '-i', str(source),
                       '-map', '0:a:0', '-vn', '-sn', '-dn', '-af',
                       f'atrim=start_sample={trim_start}:end_sample={trim_start + end - start},asetpts=N/SR/TB',
                       '-f', 's32le', '-c:a', 'pcm_s32le', 'pipe:1']
            candidate = execute(command)
            expected = full[start * width:end * width]
            if candidate != expected:
                raise AssertionError(f'PCM differs: {rate}/{channels}/{bits}, [{start}, {end})')
            details.append({'rate': rate, 'channels': channels, 'bits': bits,
                            'first_sample': start, 'end_sample': end,
                            'bytes': len(candidate), 'sha256': digest(candidate)})
    return {'status': 'PASS', 'scope': 'standalone_ffmpeg_ip02_command_only_NOT_rust_IP07',
            'ffmpeg_version': version, 'cases': len(details), 'results': details}


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--json', type=Path, help='save validated fixture results')
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix='cue-ip07-flac-range-') as scratch:
        output = run(Path(scratch))
    body = json.dumps(output, indent=2, sort_keys=True) + '\n'
    if args.json:
        args.json.write_text(body)
    print(f"{output['status']}: {output['cases']} standalone FFmpeg exact source-range PCM slices ({output['ffmpeg_version']})")
