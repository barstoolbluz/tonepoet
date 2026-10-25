#!/usr/bin/env python3
"""IP-03 independent native *resource envelope* probe; not a Rust S3 gate.

Creates one reproducible lossless FLAC, pipes FFmpeg ranged decode directly
into FFmpeg FLAC encoding with one or two concurrent pairs, then compares all
reconstructed PCM bytes against the input reference. Samples Linux /proc
FD/RSS and process counts. Does not claim to test Tokio permits/supervision.
"""

import argparse
from concurrent.futures import ThreadPoolExecutor
import hashlib
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess
import tempfile
import threading
import time

RATE = 44100
CHANNELS = 2
SECONDS = 8
SAMPLES = RATE * SECONDS


def sha(data):
    return hashlib.sha256(data).hexdigest()


def run_checked(argv, **kwargs):
    return subprocess.run(argv, check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, **kwargs).stdout


def generate_pcm():
    raw = bytearray(SAMPLES * CHANNELS * 2)
    for i in range(SAMPLES):
        left = ((i * 241 + (i // 588) * 1709) % 65536) - 32768
        right = ((i * 509 + (i // 587) * 91 + 1123) % 65536) - 32768
        struct.pack_into('<hh', raw, i * CHANNELS * 2, left, right)
    return raw


def proc_stats(pid):
    try:
        with open(f'/proc/{pid}/status', encoding='ascii') as f:
            rss_lines = [line for line in f if line.startswith('VmRSS:')]
        rss_kib = int(rss_lines[0].split()[1]) if rss_lines else 0
        fd_count = len(os.listdir(f'/proc/{pid}/fd'))
        return rss_kib, fd_count
    except (OSError, IndexError, ValueError):
        return 0, 0


def probe(ffmpeg, flac, s32_reference, ranges, maximum, workdir):
    lock = threading.Lock()
    active = set()
    records = []
    maxima = {'total_native_children': 0, 'sampled_rss_kib_parent_plus_children': 0,
              'sampled_fd_parent_plus_children': 0, 'sampled_ffmpeg_rss_kib': 0,
              'sampled_ffmpeg_fd_count': 0, 'kernel_pipe_bytes': 0}
    stopping = threading.Event()
    sampler_pid = os.getpid()

    def sample():
        while not stopping.is_set():
            with lock:
                pids = sorted(active)
                maxima['total_native_children'] = max(maxima['total_native_children'], len(pids))
            rss, fds = proc_stats(sampler_pid)
            for pid in pids:
                crss, cfds = proc_stats(pid)
                maxima['sampled_ffmpeg_rss_kib'] = max(maxima['sampled_ffmpeg_rss_kib'], crss)
                maxima['sampled_ffmpeg_fd_count'] = max(maxima['sampled_ffmpeg_fd_count'], cfds)
                rss += crss
                fds += cfds
            maxima['sampled_rss_kib_parent_plus_children'] = max(
                maxima['sampled_rss_kib_parent_plus_children'], rss)
            maxima['sampled_fd_parent_plus_children'] = max(
                maxima['sampled_fd_parent_plus_children'], fds)
            stopping.wait(0.003)

    def one_range(item):
        ordinal, start, end = item
        sec, offset = divmod(start, RATE)
        length = end - start
        dest = workdir / f'{maximum}-{ordinal:03d}.flac'
        decoder = [ffmpeg, '-v', 'error', '-hide_banner', '-nostdin', '-xerror',
                   '-accurate_seek', '-ss', str(sec), '-err_detect', 'crccheck+explode',
                   '-i', str(flac), '-map', '0:a:0', '-vn', '-sn', '-dn',
                   '-af', f'atrim=start_sample={offset}:end_sample={offset + length},asetpts=N/SR/TB',
                   '-f', 's32le', '-c:a', 'pcm_s32le', 'pipe:1']
        encoder = [ffmpeg, '-v', 'error', '-hide_banner', '-nostdin', '-xerror',
                   '-f', 's32le', '-ar', str(RATE), '-ac', str(CHANNELS), '-i', 'pipe:0',
                   '-map', '0:a:0', '-sample_fmt', 's16', '-c:a', 'flac', '-y', str(dest)]
        producer = subprocess.Popen(decoder, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                    stdin=subprocess.DEVNULL)
        with lock:
            active.add(producer.pid)
        consumer = None
        try:
            if hasattr(os, 'getpid') and os.path.exists('/proc/self/fd'):
                try:
                    import fcntl
                    maxima['kernel_pipe_bytes'] = max(maxima['kernel_pipe_bytes'],
                        fcntl.fcntl(producer.stdout.fileno(), fcntl.F_GETPIPE_SZ))
                except (OSError, AttributeError):
                    pass
            consumer = subprocess.Popen(encoder, stdin=producer.stdout, stdout=subprocess.DEVNULL,
                                        stderr=subprocess.PIPE)
            with lock:
                active.add(consumer.pid)
            # Prevent parent-owned pipe write/read handles extending either child lifetime.
            producer.stdout.close()
            error_consumer = consumer.communicate(timeout=25)[1]
            error_producer = producer.stderr.read(65537)
            producer_code = producer.wait(timeout=10)
            if len(error_producer) > 65536 or len(error_consumer) > 65536:
                raise AssertionError('unbounded stderr in probe')
            if producer_code != 0 or consumer.returncode != 0:
                raise AssertionError(f'range {ordinal} producer={producer_code} consumer={consumer.returncode}: '
                                     f'{error_producer!r}, {error_consumer!r}')
            decoded = run_checked([ffmpeg, '-v', 'error', '-nostdin', '-i', str(dest),
                                   '-map', '0:a:0', '-f', 's32le', '-c:a', 'pcm_s32le', 'pipe:1'])
            expected = s32_reference[start * CHANNELS * 4: end * CHANNELS * 4]
            if decoded != expected:
                raise AssertionError(f'PCM mismatch in range {ordinal}: expected {sha(expected)} got {sha(decoded)}')
            if len(decoded) != length * CHANNELS * 4:
                raise AssertionError(f'extent mismatch in range {ordinal}')
            return {'ordinal': ordinal, 'start': start, 'end': end, 'pcm_sha256': sha(decoded),
                    'decoded_pcm_bytes': len(decoded), 'flac_sha256': sha(dest.read_bytes())}
        finally:
            for child in [consumer, producer]:
                if child is not None and child.poll() is None:
                    child.kill()
                    child.wait(timeout=10)
            with lock:
                active.discard(producer.pid)
                if consumer is not None:
                    active.discard(consumer.pid)

    thread = threading.Thread(target=sample, daemon=True)
    thread.start()
    try:
        with ThreadPoolExecutor(max_workers=maximum) as pool:
            records = list(pool.map(one_range, ranges))
    finally:
        stopping.set()
        thread.join(timeout=2)
    if active:
        raise AssertionError(f'native children leaked: {active}')
    if maxima['total_native_children'] > 2 * maximum:
        raise AssertionError(f'pair cap exceeded: {maxima}')
    return {'parallel_pairs': maximum, 'range_results': records, 'resource_maxima': maxima}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--output', required=True)
    args = parser.parse_args()
    ffmpeg = shutil.which('ffmpeg')
    if not ffmpeg:
        raise SystemExit('ffmpeg unavailable')
    ffmpeg = os.path.realpath(ffmpeg)
    output = Path(args.output).resolve()
    output.parent.mkdir(parents=True, exist_ok=True)
    source_pcm = generate_pcm()
    t0 = time.monotonic()
    with tempfile.TemporaryDirectory(prefix='tonepoet-ip03-native-') as tempdir:
        tempdir = Path(tempdir)
        raw = tempdir / 'source.s16le'
        raw.write_bytes(source_pcm)
        flac = tempdir / 'source.flac'
        run_checked([ffmpeg, '-v', 'error', '-nostdin', '-f', 's16le', '-ar', str(RATE),
                     '-ac', str(CHANNELS), '-i', str(raw), '-c:a', 'flac', '-y', str(flac)])
        reference = run_checked([ffmpeg, '-v', 'error', '-nostdin', '-i', str(flac),
                                 '-map', '0:a:0', '-f', 's32le', '-c:a', 'pcm_s32le', 'pipe:1'])
        assert len(reference) == SAMPLES * CHANNELS * 4
        starts = [0, 1, 587, 588, 589, RATE-1, RATE, RATE+1,
                  2*RATE+587, 3*RATE+589, 5*RATE+11, SAMPLES-589]
        ranges = [(i, start, min(SAMPLES, start + 588 + (i % 3)))
                  for i, start in enumerate(starts)]
        results = [probe(ffmpeg, flac, reference, ranges[:4], 1, tempdir),
                   probe(ffmpeg, flac, reference, ranges, 2, tempdir)]
        receipt = {
            'scope': 'INDEPENDENT FFmpeg pipe resource envelope; NOT Rust S3 supervisor/admission',
            'outcome': 'PASS', 'ffmpeg_path': ffmpeg,
            'ffmpeg_executable_sha256': sha(Path(ffmpeg).read_bytes()),
            'ffmpeg_version': run_checked([ffmpeg, '-version']).decode().splitlines()[0],
            'fixture': {'samples': SAMPLES, 'channels': CHANNELS, 'rate': RATE,
                        'source_pcm_sha256': sha(source_pcm), 'source_flac_sha256': sha(flac.read_bytes()),
                        'whole_decoded_s32le_sha256': sha(reference),
                        'source_flac_bytes': flac.stat().st_size},
            'results': results, 'elapsed_seconds': round(time.monotonic() - t0, 3),
        }
        output.write_text(json.dumps(receipt, indent=2) + '\n')
        print(json.dumps({'outcome': receipt['outcome'], 'ranges': sum(len(r['range_results']) for r in results),
                          'resources': [r['resource_maxima'] for r in results],
                          'elapsed_seconds': receipt['elapsed_seconds']}))


if __name__ == '__main__':
    main()
