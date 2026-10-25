#!/usr/bin/env python3
"""Independent native FFmpeg FLAC S3 feasibility gate, not a Rust runner test.

No installed TonePoet binary is invoked. PCM reference is independently decoded
from the complete FLAC input; streaming ranges use the accepted IP-02 command.
"""
import argparse
import concurrent.futures
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import time

RATE = 44100
CHANNELS = 2
FRAME = CHANNELS * 4

def call(args):
    r = subprocess.run(args, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False)
    if r.returncode:
        raise RuntimeError(f"command exit {r.returncode}: {args} / {r.stderr[-2000:]!r}")
    return r.stdout

def sha(b):
    return hashlib.sha256(b).hexdigest()

def native_test(bits, tracks, samples_per_track, root):
    image = root / f'source_{bits}_{tracks}.flac'
    samples = tracks * samples_per_track
    args = ['ffmpeg','-v','error','-nostdin','-y','-f','lavfi', '-i',
            f'sine=frequency=440:sample_rate={RATE}:duration={samples / RATE}',
            '-ac','2','-c:a','flac','-sample_fmt','s16' if bits==16 else 's32']
    if bits == 24:
        args += ['-bits_per_raw_sample','24']
    call(args + [str(image)])
    image_sha = sha(image.read_bytes())
    full = call(['ffmpeg','-v','error','-nostdin','-i',str(image),'-map','0:a:0',
                 '-f','s32le','-c:a','pcm_s32le','pipe:1'])
    if len(full) != samples * FRAME:
        raise AssertionError(f'full image {bits}-bit extent drift: {len(full)}, {samples*FRAME}')
    metric_lock = threading.Lock()
    state = {'live_children': 0,'peak_live_children':0,'peak_parent_fds':0}
    def capture():
        with metric_lock:
            state['peak_live_children'] = max(state['peak_live_children'], state['live_children'])
            if os.path.exists('/proc/self/fd'):
                state['peak_parent_fds'] = max(state['peak_parent_fds'], len(os.listdir('/proc/self/fd')))

    def one(index):
        start = index * samples_per_track
        end = start + samples_per_track
        anchor = start // RATE
        trim_start = start % RATE
        trim_end = trim_start + samples_per_track
        dest = root / f'track-{bits}-{tracks}-{index:02}.flac'
        producer = ['ffmpeg','-v','error','-hide_banner','-nostdin','-xerror','-accurate_seek',
                    '-ss',str(anchor),'-err_detect','crccheck+explode','-i',str(image),
                    '-map','0:a:0','-vn','-sn','-dn','-af',
                    f'atrim=start_sample={trim_start}:end_sample={trim_end},asetpts=N/SR/TB',
                    '-f','s32le','-c:a','pcm_s32le','pipe:1']
        consumer = ['ffmpeg','-v','error','-hide_banner','-nostdin','-y',
                    '-f','s32le','-ar',str(RATE),'-ac',str(CHANNELS),'-i','pipe:0',
                    '-map','0:a:0','-c:a','flac','-sample_fmt','s16' if bits==16 else 's32']
        if bits == 24:
            consumer += ['-bits_per_raw_sample','24']
        consumer += [str(dest)]
        p = subprocess.Popen(producer, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        c = subprocess.Popen(consumer, stdin=subprocess.PIPE, stderr=subprocess.PIPE)
        with metric_lock:
            state['live_children'] += 2
        capture()
        count = 0
        digest = hashlib.sha256()
        failure = None
        try:
            while True:
                chunk = p.stdout.read(65536)
                if not chunk:
                    break
                count += len(chunk)
                if count > samples_per_track * FRAME:
                    raise ValueError(f'surplus PCM track {index}: {count}')
                digest.update(chunk)
                c.stdin.write(chunk)
            if count != samples_per_track * FRAME:
                raise ValueError(f'short PCM track {index}: {count}')
        except Exception as error:
            failure = error
            p.terminate(); c.terminate()
        finally:
            p.stdout.close()
            c.stdin.close()
        pe = p.wait(timeout=15)
        ce = c.wait(timeout=15)
        p_err = p.stderr.read()[-1024:]
        c_err = c.stderr.read()[-1024:]
        with metric_lock:
            state['live_children'] -= 2
        if failure or pe or ce:
            raise RuntimeError(f'track={index}: {failure}; producer={pe} {p_err!r}; consumer={ce} {c_err!r}')
        actual = call(['ffmpeg','-v','error','-nostdin','-i',str(dest),
                       '-f','s32le','-c:a','pcm_s32le','pipe:1'])
        expected = full[start*FRAME:end*FRAME]
        if actual != expected:
            raise AssertionError(f'PCM mismatch for bits={bits} track={index}')
        if digest.hexdigest() != sha(expected):
            raise AssertionError(f'raw range mismatch for bits={bits} track={index}')
        return {'index':index,'start':start,'end':end,'decoded_bytes':count,
                'pcm_sha256':sha(actual),'artifact_sha256':sha(dest.read_bytes())}
    started = time.monotonic()
    with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
        results = list(pool.map(one, range(tracks)))
    return {'source_sha256':image_sha,'source_full_pcm_sha256':sha(full),
            'bit_depth':bits,'tracks':tracks,'sample_rate':RATE,'image_samples':samples,
            'elapsed_s':round(time.monotonic()-started,3),
            'max_native_children':state['peak_live_children'],
            'max_parent_fds':state['peak_parent_fds'],
            'all_pcm_matched': True, 'ranges':results}

if __name__=='__main__':
    parser=argparse.ArgumentParser()
    parser.add_argument('--quick',action='store_true')
    parser.add_argument('--output',type=Path,required=True)
    args=parser.parse_args()
    results=[]
    with tempfile.TemporaryDirectory(prefix='tonepoet-ip04-native-') as folder:
        for bits, tracks, count in ([(16,3,44100),(24,3,44100)] if args.quick
                                    else [(16,12,4410),(24,30,4410)]):
            results.append(native_test(bits,tracks,count,Path(folder)))
    report={'scope':'independent native FFmpeg feasibility only; not Rust ToolRunner',
            'outcome':'PASS','cases':results}
    args.output.write_text(json.dumps(report,indent=2,sort_keys=True)+'\n')
    print(f"PASS: {[(x['bit_depth'],x['tracks']) for x in results]},"
          f" {sum(len(x['ranges']) for x in results)} exact PCM ranges")
