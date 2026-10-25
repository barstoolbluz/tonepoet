#!/usr/bin/env python3
"""Deterministically package plan-only files with SHA256SUMS and an inventory.

Do not bundle the original large source; accept the full-source archive only in
implementation sessions. This script never touches source or accepts external
paths as members of the planning bundle.
"""
import argparse
import gzip
import hashlib
import json
import subprocess
import sys
import tarfile
from datetime import datetime, timezone
from pathlib import Path

BASELINE_SHA = '0740a9c8cbd8e59648981207daf62e6373f4b544ad3a40c7872b3890c9d76d26'
NAMES = ('observe_baseline', 'qualify_flac_ranges', 'global_resource_admission',
         'parallel_worker_private', 'integrate_pilot', 'qualify_and_promote')
EPOCH = int(datetime(2026, 10, 22, tzinfo=timezone.utc).timestamp())


def sha(path):
    h = hashlib.sha256()
    with path.open('rb') as f:
        for b in iter(lambda: f.read(1024*1024), b''):
            h.update(b)
    return h.hexdigest()


def write_atomic(path, data):
    tmp = path.with_suffix(path.suffix + '.tmp')
    tmp.write_bytes(data)
    tmp.replace(path)


def package(root, output):
    root = root.resolve()
    output = output.resolve()
    if output.is_relative_to(root):
        raise ValueError('output must be outside the input package tree')
    files = sorted(p for p in root.rglob('*') if p.is_file()
                   and p.name not in {'SHA256SUMS', 'manifest.json'}
                   and not p.name.endswith('.pyc'))
    inventory = {}
    for p in files:
        name = p.relative_to(root).as_posix()
        if p.is_symlink() or p.resolve().is_relative_to(root) is False:
            raise ValueError(f'unsafe symlink/path: {name}')
        inventory[name] = {'size': p.stat().st_size, 'sha256': sha(p)}
    packets = []
    for i, slug in enumerate(NAMES, 1):
        pid = f'IP-{i:02d}'
        prompt = f'packets/{pid}_{slug}.md'
        if prompt not in inventory:
            raise FileNotFoundError(f'missing committed prompt: {prompt}')
        packets.append({'id': pid, 'prompt': prompt,
                        'depends_on': [] if i == 1 else [f'IP-{i-1:02d}'],
                        'initial_state': 'planned'})
    manifest = {'schema': 'tonepoet-cue-planning-inventory-v1',
                'date': '2026-10-22',
                'planning_release': 'R2_CORRECTIVE',
                'source_archive': 'TonePoet_SNAPSHOT_98aae2b_2026-10-22.tar.gz',
                'source_archive_sha256': BASELINE_SHA,
                'source_commit_reported': '98aae2b',
                'source_present_in_this_bundle': False,
                'production_modified': False,
                'packets': packets,
                'files': inventory}
    write_atomic(root/'manifest.json', (json.dumps(manifest, sort_keys=True, indent=2)+'\n').encode())
    sums = {**{k: v['sha256'] for k, v in inventory.items()},
            'manifest.json': sha(root/'manifest.json')}
    write_atomic(root/'SHA256SUMS',
                 (''.join(f'{sums[k]}  {k}\n' for k in sorted(sums))).encode())
    check = subprocess.run([sys.executable, '-B', str(root/'tools/verify_bundle.py'), str(root)],
                           capture_output=True, text=True)
    print(check.stdout.strip())
    if check.returncode:
        raise RuntimeError('bundle validation failed: '+check.stderr+check.stdout)
    members = [root/'manifest.json', root/'SHA256SUMS', *files]
    members = sorted(members, key=lambda p: p.relative_to(root).as_posix())
    prefix = 'TonePoet_CUE_Parallel_Plan_R2_CORRECTIVE_98aae2b_2026-10-22'
    output.parent.mkdir(parents=True, exist_ok=True)
    with output.open('wb') as base:
        with gzip.GzipFile(fileobj=base, filename='', mode='wb', mtime=0, compresslevel=9) as gz:
            with tarfile.open(fileobj=gz, mode='w', format=tarfile.GNU_FORMAT) as tf:
                dirinfo = tarfile.TarInfo(prefix+'/')
                dirinfo.type = tarfile.DIRTYPE
                dirinfo.mode = 0o755
                dirinfo.uid = dirinfo.gid = 0
                dirinfo.uname = dirinfo.gname = 'root'
                dirinfo.mtime = EPOCH
                tf.addfile(dirinfo)
                for f in members:
                    name = prefix+'/'+f.relative_to(root).as_posix()
                    info = tarfile.TarInfo(name)
                    info.size = f.stat().st_size
                    info.mode = 0o644
                    info.uid = info.gid = 0
                    info.uname = info.gname = 'root'
                    info.mtime = EPOCH
                    with f.open('rb') as fh:
                        tf.addfile(info, fileobj=fh)
    print('OUTPUT', output)
    print('BYTES', output.stat().st_size)
    print('SHA256', sha(output))


if __name__ == '__main__':
    ap = argparse.ArgumentParser()
    ap.add_argument('plan_root', type=Path)
    ap.add_argument('output_archive', type=Path)
    a = ap.parse_args()
    package(a.plan_root, a.output_archive)
