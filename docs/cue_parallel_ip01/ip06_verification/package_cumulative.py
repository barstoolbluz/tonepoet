#!/usr/bin/env python3
"""Build deterministic owner-approved default-on cumulative source with checksums.

Usage: from extracted tree, python3 -B docs/cue_parallel_ip01/ip06_verification/package_cumulative.py OUTPUT.tar.gz
The external archive SHA must be distributed separately from the archive.
"""
import argparse
import gzip
import hashlib
import json
import os
import stat
import tarfile
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
DOCS = 'docs/cue_parallel_ip01/'
MANIFEST = ROOT / DOCS / 'ip06_verification/SOURCE_MANIFEST.json'
SUMS = ROOT / 'SHA256SUMS'
INPUT_SHA = '9722a3e9551fa2f2e257a3d690ef9c6e19945072c61375d01fb95875d736d921'
EPOCH = int(datetime(2026, 10, 26, tzinfo=timezone.utc).timestamp())


def sha(path):
    h = hashlib.sha256()
    with path.open('rb') as f:
        for part in iter(lambda: f.read(1024 * 1024), b''):
            h.update(part)
    return h.hexdigest()


def entries():
    result = []
    for p in ROOT.rglob('*'):
        if '.git' in p.relative_to(ROOT).parts or '__pycache__' in p.parts:
            raise ValueError(f'build cache/Git data present: {p}')
        if p.is_file() or p.is_symlink():
            result.append(p)
    return sorted(result, key=lambda p: p.relative_to(ROOT).as_posix())


def code_hash(files):
    h = hashlib.sha256()
    count = 0
    for p in files:
        rel = p.relative_to(ROOT).as_posix()
        if rel == 'SHA256SUMS' or rel.startswith(DOCS) or p.is_symlink():
            continue
        h.update(rel.encode() + b'\0' + sha(p).encode() + b'\n')
        count += 1
    return h.hexdigest(), count


def write_package(output):
    if output.resolve().is_relative_to(ROOT.resolve()):
        raise ValueError('package output must be outside source tree')
    files = entries()
    origin, origin_count = code_hash(files)
    if output.name != f'TonePoet_CUE_IP06_DEFAULT_ON_FULL_SOURCE_{origin[:12]}.tar.gz':
        raise ValueError(f'expected output name TonePoet_CUE_IP06_DEFAULT_ON_FULL_SOURCE_{origin[:12]}.tar.gz')
    inventory = {}
    for path in files:
        rel = path.relative_to(ROOT).as_posix()
        if path == SUMS or path == MANIFEST:
            continue
        if path.is_symlink():
            target = path.resolve(strict=False)
            if not target.is_relative_to(ROOT.resolve()):
                raise ValueError(f'symlink escapes tree: {rel}')
            inventory[rel] = {'type': 'symlink', 'target': os.readlink(path)}
        else:
            inventory[rel] = {'type': 'file', 'bytes': path.stat().st_size, 'sha256': sha(path)}
    document = {
        'schema': 'tonepoet-ip06-cumulative-v2',
        'decision': 'owner-accepted-default-on; performance-cohort-exception; native-qualification-pending',
        'original_promoted_snapshot_sha256': '1f1fcf261ac35aa585580489a64989541a71916af58f88bc592f0a91aa196d2e',
        'input_archive_sha256': INPUT_SHA,
        'reported_commits_verifiable': False,
        'code_inventory_sha256': origin,
        'code_inventory_files_excluding_cue_parallel_docs': origin_count,
        'path_hashes_exclude': ['SHA256SUMS', str(MANIFEST.relative_to(ROOT))],
        'files_and_symlinks': inventory,
    }
    MANIFEST.write_text(json.dumps(document, indent=2, sort_keys=True) + '\n')
    files = entries()
    sums = [f'{sha(p)}  {p.relative_to(ROOT).as_posix()}\n' for p in files
            if not p.is_symlink() and p != SUMS]
    SUMS.write_text(''.join(sums))
    # Include directories to preserve the source filesystem shape, including
    # otherwise empty dirs. Canonicalize uid/gid/times and carry executability.
    paths = [p for p in ROOT.rglob('*') if '__pycache__' not in p.parts]
    paths.sort(key=lambda p: p.relative_to(ROOT).as_posix())
    output.parent.mkdir(parents=True, exist_ok=True)
    with output.open('wb') as stream:
        with gzip.GzipFile(filename='', mode='wb', fileobj=stream, compresslevel=9, mtime=0) as gz:
            with tarfile.open(fileobj=gz, mode='w', format=tarfile.GNU_FORMAT) as tf:
                top = tarfile.TarInfo('tonepoet/')
                top.type, top.mode, top.mtime = tarfile.DIRTYPE, 0o755, EPOCH
                tf.addfile(top)
                for p in paths:
                    name = 'tonepoet/' + p.relative_to(ROOT).as_posix()
                    info = tarfile.TarInfo(name)
                    info.mtime = EPOCH
                    if p.is_symlink():
                        info.type = tarfile.SYMTYPE
                        info.mode = 0o777
                        info.linkname = os.readlink(p)
                        tf.addfile(info)
                    elif p.is_dir():
                        info.type, info.mode = tarfile.DIRTYPE, 0o755
                        tf.addfile(info)
                    elif p.is_file():
                        info.size = p.stat().st_size
                        info.mode = 0o755 if p.stat().st_mode & (stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH) else 0o644
                        with p.open('rb') as f:
                            tf.addfile(info, f)
                    else:
                        raise ValueError(f'unsupported filesystem type: {p}')
    return {'archive': str(output), 'archive_sha256': sha(output),
            'archive_size': output.stat().st_size, 'source_code_inventory_sha256': origin,
            'regular_file_count': len(sums), 'symlink_count': sum(p.is_symlink() for p in files),
            'root_sha256sums_sha256': sha(SUMS)}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('output', type=Path)
    args = parser.parse_args()
    print(json.dumps(write_package(args.output), indent=2, sort_keys=True))
