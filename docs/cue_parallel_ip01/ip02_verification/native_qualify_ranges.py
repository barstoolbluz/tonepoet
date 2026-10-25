#!/usr/bin/env python3
"""IP-02 independent *native decoder* qualification; no TonePoet route is activated.

Generates deterministic signed PCM signatures (WAVE), encodes actual FLAC
(with/without SEEKTABLE), compares every ranged FFmpeg s32le byte to a full
S1-argument decoder's PCM, and tests lifecycle/EOF and seek scaling. Audio
files are created in --workdir, NOT shipped as giant archive members.

This script exercises the command shape authored in cue_flac_range.rs. It is
not a substitute for compiling that Rust source or accepting IP-02 on a build
host. It does not use floating seconds to calculate sample boundaries.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import random
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import wave

CHUNK = 64 * 1024


def sha(path):
    h = hashlib.sha256()
    with path.open('rb') as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b''):
            h.update(chunk)
    return h.hexdigest()


def checked(cmd, *, timeout=120):
    p = subprocess.run(cmd, capture_output=True, timeout=timeout)
    if p.returncode:
        raise RuntimeError(f'command exit {p.returncode}: {cmd!r}, stderr={p.stderr[-2000:]!r}')
    return p.stdout.decode('utf-8', errors='replace')


def fixture(path, rate, channels, bits, count):
    """Write signed little-endian noise, a deterministic signature per sample."""
    rng = random.Random((rate << 20) ^ (channels << 16) ^ (bits << 8) ^ count)
    mask = (1 << bits) - 1
    with wave.open(str(path), 'wb') as f:
        f.setnchannels(channels)
        f.setsampwidth(bits // 8)
        f.setframerate(rate)
        for start in range(0, count, 4096):
            data = bytearray()
            for index in range(start, min(count, start + 4096)):
                for channel in range(channels):
                    sample = rng.getrandbits(bits) ^ (index * 0x9e3779b1) ^ (channel * 0x71c675)
                    data.extend((sample & mask).to_bytes(bits // 8, 'little'))
            f.writeframesraw(data)


def reference_cmd(source):
    return ['ffmpeg', '-v', 'error', '-hide_banner', '-nostdin',
            '-i', str(source), '-map', '0:a:0', '-vn', '-sn', '-dn',
            '-f', 's32le', '-c:a', 'pcm_s32le', 'pipe:1']


def ranged_cmd(source, rate, start, end):
    seek_sec = start // rate
    trim_start = start % rate
    trim_end = trim_start + (end - start)
    return ['ffmpeg', '-v', 'error', '-hide_banner', '-nostdin',
            '-xerror', '-accurate_seek', '-ss', str(seek_sec),
            '-err_detect', 'crccheck+explode', '-i', str(source),
            '-map', '0:a:0', '-vn', '-sn', '-dn',
            '-af', f'atrim=start_sample={trim_start}:end_sample={trim_end},asetpts=N/SR/TB',
            '-f', 's32le', '-c:a', 'pcm_s32le', 'pipe:1']


def decode_to_path(cmd, path, expected, *, slow=False, allow_failure=False):
    """Drain stdout in bounded chunks; stderr goes to disk (never pipe-deadlocks).

    Fail on short or excess extent even if FFmpeg returns 0. Always reap the
    child. The source is never read wholesale into memory.
    """
    started = time.monotonic()
    with tempfile.TemporaryFile() as err, path.open('wb') as out:
        child = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=err, stdin=subprocess.DEVNULL,
                                 start_new_session=True)
        received = 0
        problem = None
        try:
            while True:
                chunk = child.stdout.read(CHUNK if not slow else 4096)
                if not chunk:
                    break
                received += len(chunk)
                if received > expected:
                    problem = f'surplus: {received} > {expected}'
                    break
                out.write(chunk)
                if slow:
                    time.sleep(0.002)
            if problem:
                os.killpg(child.pid, signal.SIGKILL)
            try:
                exitcode = child.wait(timeout=30)
            except subprocess.TimeoutExpired:
                os.killpg(child.pid, signal.SIGKILL)
                child.wait(timeout=10)
                problem = 'child wait timeout'
                exitcode = child.returncode
        finally:
            child.stdout.close()
            if child.poll() is None:
                os.killpg(child.pid, signal.SIGKILL)
                child.wait(timeout=10)
        err.seek(0)
        error_tail = err.read(8192).decode(errors='replace')
    if not problem and exitcode:
        problem = f'nonzero exit {exitcode}: {error_tail[-400:]}'
    if not problem and received != expected:
        problem = f'short: {received} != {expected}'
    if problem and not allow_failure:
        raise AssertionError(f'{problem}\n{cmd}')
    if not problem and allow_failure:
        raise AssertionError(f'invalid source unexpectedly decoded to full output: {cmd}')
    return {'bytes': received, 'elapsed_seconds': round(time.monotonic()-started, 6),
            'exit_code': exitcode, 'failure': problem, 'sha256': sha(path),
            'stderr_prefix': error_tail[:500]}


def expected_equal(ref, out, offset, length):
    with ref.open('rb') as a, out.open('rb') as b:
        a.seek(offset)
        remaining = length
        while remaining:
            size = min(CHUNK, remaining)
            first, second = a.read(size), b.read(size)
            if len(first) != size or first != second:
                return False
            remaining -= size
        return not b.read(1)


def ranges_for(rate, count, seed, quick):
    # Include both sides of CD frame and codec block boundaries, a sample
    # beyond a full second, middle, and the final sample.
    starts = {0, 1, 2, 587, 588, 589, 4095, 4096, 4097, rate-1,
              rate, rate+1, count//2, count//2+1, count-2, count-1}
    rng = random.Random(seed)
    for _ in range(4 if quick else 16):
        starts.add(rng.randrange(count))
    ranges = []
    for start in sorted(s for s in starts if 0 <= s < count):
        length = [1, 2, 17, 587, 4096, rate//5][(start + seed) % 6]
        ranges.append((start, min(count, start + length)))
    # Long segments from a sample-unaligned start and near EOF.
    ranges.extend([(min(589, count-1), count), (count//2, count), (0, count)])
    return list(dict.fromkeys(ranges))


def seektable_presence(path):
    listing = checked(['metaflac', '--list', '--block-type=SEEKTABLE', str(path)])
    return 'type: 3 (SEEKTABLE)' in listing


def verify_widening(wav_path, pcm_path, bits, channels, n):
    with wave.open(str(wav_path), 'rb') as w, pcm_path.open('rb') as p:
        assert w.getsampwidth() == bits // 8 and w.getnchannels() == channels
        for _ in range(min(n, 4096)):
            raw = w.readframes(1)
            wide = p.read(channels * 4)
            for c in range(channels):
                source = int.from_bytes(raw[c*(bits//8):(c+1)*(bits//8)], 'little', signed=True)
                target = int.from_bytes(wide[c*4:(c+1)*4], 'little', signed=True)
                assert target == source << (32 - bits), ('incorrect s32 widening', bits, c)


def cell(work, rate, channels, bits, with_seektable, quick):
    count = rate * (2 if quick else 4) + 47
    name = f'r{rate}_c{channels}_b{bits}_' + ('seek' if with_seektable else 'noseek')
    wav_path = work / (name+'.wav')
    flac_path = work / (name+'.flac')
    ref_path = work / (name+'.baseline.s32le')
    temp_path = work / (name+'.range.s32le')
    fixture(wav_path, rate, channels, bits, count)
    checked(['ffmpeg','-v','error','-nostdin','-y','-i',str(wav_path),'-c:a','flac',str(flac_path)])
    # Independently assert metadata block state, not merely the command.
    if with_seektable:
        checked(['metaflac', '--add-seekpoint=1s', str(flac_path)])
    assert seektable_presence(flac_path) == with_seektable, ('seektable state wrong', flac_path)
    expected = count*channels*4
    full = decode_to_path(reference_cmd(flac_path), ref_path, expected)
    verify_widening(wav_path, ref_path, bits, channels, count)
    mismatches = []
    all_ranges = ranges_for(rate, count, rate ^ bits ^ channels, quick)
    for start, end in all_ranges:
        length = (end-start)*channels*4
        got = decode_to_path(ranged_cmd(flac_path,rate,start,end), temp_path, length)
        if not expected_equal(ref_path,temp_path,start*channels*4,length):
            mismatches.append({'start':start,'end':end,'expected':length,'actual_sha':got['sha256']})
    if mismatches:
        raise AssertionError(f'{name}: sample mismatches {mismatches[:5]}')
    # Exact half-open tiling: may not omit or duplicate even one sample.
    # Use full SHA parity, not a file-size or first-sample-only comparison.
    edges = sorted(set([0,1,587,588,589,4096,rate,rate+1,count//3,count//2,count-1,count]))
    edges = [e for e in edges if 0 <= e <= count]
    digest = hashlib.sha256()
    for start, end in zip(edges, edges[1:]):
        piece = decode_to_path(ranged_cmd(flac_path,rate,start,end),temp_path,(end-start)*channels*4)
        with temp_path.open('rb') as f:
            for chunk in iter(lambda:f.read(CHUNK), b''):
                digest.update(chunk)
    if digest.hexdigest() != full['sha256']:
        raise AssertionError(f'{name}: tiling mismatch')
    result = {'cell':name,'samples':count,'file_sha256':sha(flac_path),
              'wav_sha256':sha(wav_path),'reference_pcm_sha256':full['sha256'],
              'ranges_passed':len(all_ranges),'tiles_passed':len(edges)-1,
              'seektable':with_seektable,'rate':rate,'channels':channels,'bits':bits,
              'reference_command':reference_cmd(flac_path),
              'range_command_example':ranged_cmd(flac_path,rate,589,590)}
    for x in (wav_path, flac_path, ref_path, temp_path):
        x.unlink()
    return result


def corruption_and_backpressure(work, quick):
    rate,ch,bits = 44100,2,24
    count = rate*(15 if quick else 75)+17
    wav_path=work/'long.wav'; source=work/'long.flac'; ref=work/'long.reference'
    piece=work/'long.piece'; bad=work/'long.corrupt.flac'; truncated=work/'long.truncated.flac'
    fixture(wav_path,rate,ch,bits,count)
    checked(['ffmpeg','-v','error','-nostdin','-y','-i',str(wav_path),'-c:a','flac',str(source)],timeout=180)
    full = decode_to_path(reference_cmd(source), ref,count*ch*4)
    measurements=[]
    for label,start in [('begin',rate//2),('mid',count//2),('late',count-rate*2)]:
        end=min(count,start+4096)
        times=[]
        for _ in range(4 if quick else 8):
            got=decode_to_path(ranged_cmd(source,rate,start,end),piece,(end-start)*ch*4)
            assert expected_equal(ref,piece,start*ch*4,(end-start)*ch*4)
            times.append(got['elapsed_seconds'])
        measurements.append({'offset':label,'sample':start,'elapsed_seconds':times,
                             'median_seconds':sorted(times)[len(times)//2]})
    no_seek_late_over_begin = round(measurements[-1]['median_seconds'] / max(0.0001,measurements[0]['median_seconds']),3)
    slow_start=count//4+7
    slow_end=min(count,slow_start+rate*3)
    slow=decode_to_path(ranged_cmd(source,rate,slow_start,slow_end),piece,
                        (slow_end-slow_start)*ch*4,slow=True)
    assert expected_equal(ref,piece,slow_start*ch*4,(slow_end-slow_start)*ch*4)
    # Cancellation while a reader is stalled by a deliberately slow consumer.
    cmd=ranged_cmd(source,rate,0,count)
    with tempfile.TemporaryFile() as err:
        proc=subprocess.Popen(cmd,stdin=subprocess.DEVNULL,stdout=subprocess.PIPE,stderr=err,start_new_session=True)
        prefix=proc.stdout.read(4096)
        assert prefix and proc.poll() is None, 'cancel probe child exited before cancellation'
        os.killpg(proc.pid, signal.SIGTERM)
        escalated = False
        try:
            proc.wait(timeout=1)
        except subprocess.TimeoutExpired:
            # FFmpeg can remain blocked in pipe write with a stalled reader.
            # A real supervisor must escalate and reap, not wait forever.
            escalated = True
            os.killpg(proc.pid, signal.SIGKILL)
            proc.wait(timeout=5)
        proc.stdout.close()
        assert proc.returncode != 0
    # Corrupt an audio frame byte; choose a range that needs to read this frame.
    # A failure or a short extent is required, never silently accept bad audio.
    shutil.copyfile(source,bad)
    with bad.open('r+b') as f:
        f.seek(bad.stat().st_size//2)
        frame = f.read(32)
        f.seek(bad.stat().st_size//2)
        f.write(bytes(b ^ 0xff for b in frame))
    corrupt_result=decode_to_path(ranged_cmd(bad,rate,0,count),piece,count*ch*4,allow_failure=True)
    shutil.copyfile(source,truncated)
    with truncated.open('r+b') as f:
        f.truncate(max(1024,truncated.stat().st_size//2))
    truncated_result=decode_to_path(ranged_cmd(truncated,rate,count//2,count),piece,
                                    (count-count//2)*ch*4,allow_failure=True)
    result={'samples':count,'flac_sha256':sha(source),'reference_sha256':full['sha256'],
            'seek_scaling_without_seektable':measurements,
            'late_to_begin_median_ratio':no_seek_late_over_begin,
            'slow_consumer':{'bytes':slow['bytes'],'seconds':slow['elapsed_seconds']},
            'cancel':{'prefix_bytes':len(prefix),'child_exit_code':proc.returncode,'reaped':True, 'sigkill_escalation':escalated},
            'corruption':{'file_sha256':sha(bad),'detected':corrupt_result['failure']},
            'truncation':{'file_sha256':sha(truncated),'detected':truncated_result['failure']}}
    for x in [wav_path,source,ref,piece,bad,truncated]:x.unlink()
    return result


def main():
    ap=argparse.ArgumentParser()
    ap.add_argument('--workdir',type=Path,required=True)
    ap.add_argument('--output',type=Path,required=True)
    ap.add_argument('--quick',action='store_true')
    a=ap.parse_args()
    a.workdir.mkdir(parents=True,exist_ok=True)
    a.output.parent.mkdir(parents=True,exist_ok=True)
    results={'schema':'tonepoet-ip02-native-v1','quick':a.quick,'timestamp_utc':time.strftime('%Y-%m-%dT%H:%M:%SZ',time.gmtime()),
             'platform':{'uname':list(os.uname()),'python':sys.version.split()[0]},
             'tools':{n:{'path':shutil.which(n),'path_sha256':sha(Path(shutil.which(n)).resolve()),
                         'version':checked([n,'-version' if n=='ffmpeg' else '--version']).splitlines()[0]}
                      for n in ['ffmpeg','flac','metaflac']},'cells':[], 'long_offset':None,
             'errors':[]}
    # Durable restart checkpoint: each completed cell is written atomically.
    if a.output.exists():
        old = json.loads(a.output.read_text())
        if old.get('schema') != results['schema'] or old.get('quick') != a.quick or old.get('tools') != results['tools']:
            raise SystemExit('incompatible prior verification checkpoint')
        results['cells'] = old.get('cells', [])
        results['long_offset'] = old.get('long_offset')
    def checkpoint():
        results['cell_count'] = len(results['cells'])
        results['range_count'] = sum(c['ranges_passed'] for c in results['cells'])
        results['tile_count'] = sum(c['tiles_passed'] for c in results['cells'])
        results['outcome'] = 'PASS' if results['long_offset'] and len(results['cells']) == 24 and not results['errors'] else 'IN PROGRESS'
        tmp = a.output.with_suffix('.tmp')
        tmp.write_text(json.dumps(results, indent=2, sort_keys=True) + '\n')
        tmp.replace(a.output)
    checkpoint()
    try:
        for rate in [44100,48000,96000]:
            for channels in [1,2]:
                for bits in [16,24]:
                    for seektable in [False,True]:
                        key = f'r{rate}_c{channels}_b{bits}_' + ('seek' if seektable else 'noseek')
                        if any(old['cell'] == key for old in results['cells']):
                            print('SKIP checkpoint', key, flush=True)
                            continue
                        r=cell(a.workdir,rate,channels,bits,seektable,a.quick)
                        results['cells'].append(r)
                        checkpoint()
                        print('PASS',r['cell'],r['ranges_passed'],'ranges',r['tiles_passed'],'tiles',flush=True)
        if results['long_offset'] is None:
            results['long_offset']=corruption_and_backpressure(a.workdir,a.quick)
            checkpoint()
        print('PASS long/corrupt/truncated/cancellation/slow',flush=True)
    except Exception as exc:
        results['errors'].append(f'{type(exc).__name__}: {exc}')
        print('FAIL',str(exc),file=sys.stderr,flush=True)
    finally:
        results['cell_count']=len(results['cells'])
        results['range_count']=sum(c['ranges_passed'] for c in results['cells'])
        results['tile_count']=sum(c['tiles_passed'] for c in results['cells'])
        results['outcome']='PASS' if not results['errors'] and len(results['cells']) == 24 and results['long_offset'] else 'FAIL'
        a.output.write_text(json.dumps(results,indent=2,sort_keys=True)+'\n')
    return 0 if not results['errors'] else 1

if __name__=='__main__':
    raise SystemExit(main())
