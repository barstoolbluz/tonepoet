#!/usr/bin/env python3
"""Generate a reproducible *fixture*, not a production qualified benchmark.

Same underlying album signal per rate/depth/minutes can be split into variable
CUE track counts; uses ffmpeg to generate and encode instead of Python PCM loops.
Requires ffmpeg; does not need TonePoet or a Rust toolchain. Binary versions
are included in a manifest because the PCM output is tool-version dependent.
"""
import argparse
import hashlib
import json
import shutil
import subprocess
import tempfile
from pathlib import Path


def sha(path):
    h = hashlib.sha256()
    with path.open('rb') as f:
        for b in iter(lambda: f.read(1024*1024), b''):
            h.update(b)
    return h.hexdigest()


def main():
    p = argparse.ArgumentParser()
    p.add_argument('output_dir', type=Path, help='must not exist (atomic publication)')
    p.add_argument('--minutes', type=int, default=1)
    p.add_argument('--rate', type=int, choices=(44100,48000,96000), default=44100)
    p.add_argument('--depth', type=int, choices=(16,24), default=16)
    p.add_argument('--tracks', type=int, choices=(1,3,12,30), default=12)
    p.add_argument('--ffmpeg', default='ffmpeg')
    a = p.parse_args()
    if not (1 <= a.minutes <= 240):
        p.error('minutes must be 1..240')
    # 75-frames-per-second logical CD time index; use all integers.
    frames = a.minutes * 60 * 75
    if frames < a.tracks:
        p.error('fewer CUE frame slots than tracks')
    out = a.output_dir.resolve()
    if out.exists():
        p.error('refusing existing destination; no overwrites')
    out.parent.mkdir(parents=True, exist_ok=True)
    estimated_pcm = a.minutes*60*a.rate*2*(a.depth//8)
    if shutil.disk_usage(out.parent).free < (estimated_pcm*2 + 64*1024*1024):
        p.error('insufficient scratch headroom for WAV + FLAC + metadata')
    work = Path(tempfile.mkdtemp(prefix='.tonepoet-cue-fixture-', dir=out.parent))
    try:
        versions = subprocess.run([a.ffmpeg, '-version'], check=True,
                                  capture_output=True, text=True).stdout.splitlines()[0]
        source = work/'album.wav'
        compressed = work/'album.flac'
        audio = f'anoisesrc=r={a.rate}:d={a.minutes*60}:seed=20261022:color=white'
        subprocess.run([a.ffmpeg, '-nostdin', '-hide_banner', '-loglevel', 'error', '-y',
                        '-f', 'lavfi', '-i', audio,
                        '-af', 'pan=stereo|c0=c0|c1=-1*c0',
                        '-c:a', f'pcm_s{a.depth}le', str(source)],
                       check=True, capture_output=True)
        subprocess.run([a.ffmpeg, '-nostdin', '-hide_banner', '-loglevel', 'error', '-y',
                        '-i', str(source), '-map', '0:a:0', '-c:a', 'flac',
                        '-compression_level', '5', str(compressed)],
                       check=True, capture_output=True)
        cue = ['REM DATE 2002', 'PERFORMER "TonePoet fixture performer"',
               'TITLE "Deterministic Split Test"', 'FILE "album.flac" WAVE']
        starts = []
        for i in range(a.tracks):
            start_f = (i*frames)//a.tracks
            # Same integer rational operation as current TonePoet CUE index path.
            start_sample = (start_f*a.rate)//75
            starts.append({'track': i+1, 'cue_frames': start_f, 'sample_offset': start_sample})
            minute, rem = divmod(start_f, 75*60)
            second, frame = divmod(rem, 75)
            cue.extend([f'  TRACK {i+1:02d} AUDIO',
                        f'    TITLE "Track {i+1:02d}"',
                        f'    PERFORMER "Fixture performer"',
                        f'    INDEX 01 {minute:02d}:{second:02d}:{frame:02d}'])
        (work/'album.cue').write_text('\n'.join(cue)+'\n', encoding='utf-8')
        items = {path.name: {'size': path.stat().st_size, 'sha256': sha(path)}
                 for path in (source, compressed, work/'album.cue')}
        manifest = {'generator': 'tools/generate_cue_fixture.py',
                    'ffmpeg_version': versions, 'rate': a.rate, 'depth': a.depth,
                    'channels': 2, 'minutes': a.minutes, 'tracks': a.tracks,
                    'total_samples': a.minutes*60*a.rate,
                    'cue_tick_rate': 75, 'starts': starts, 'files': items,
                    'note': 'Synthesized input fixture only: not a certified reference to production outputs'}
        (work/'fixture_manifest.json').write_text(json.dumps(manifest, indent=2, sort_keys=True)+'\n')
        work.rename(out)
        print(f'Fixture ready: {out} ({a.tracks} tracks, {a.minutes} minutes)')
        print('FLAC SHA-256:', items['album.flac']['sha256'])
    except BaseException:
        shutil.rmtree(work, ignore_errors=True)
        raise


if __name__ == '__main__':
    main()
