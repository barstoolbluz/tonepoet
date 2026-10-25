#!/usr/bin/env python3
"""Deterministic IP-04 whole-source manifest + independent input delta.

Current manifest excludes itself and the SHA256SUMS list. SOURCE_DIFF is
excluded only while deriving the delta against the accepted IP-03 input.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import stat

ROOT = Path(__file__).resolve().parents[3]
BASE = Path('docs/cue_parallel_ip01/ip04_verification')
EXCLUDED = {str(BASE/'SOURCE_MANIFEST.json'), str(BASE/'SHA256SUMS')}
INPUT_NAME = 'TonePoet_SNAPSHOT_c1103be_2026-10-24.tar.gz'
INPUT_SHA = 'fc3ea07b6a8ffac0b21f366b50e01693ee6b0b626a89749f551f964bcda815d7'


def scan(root):
    entries = {}
    for path in sorted(root.rglob('*')):
        if path.is_dir() and not path.is_symlink():
            continue
        relative = path.relative_to(root).as_posix()
        if relative in EXCLUDED:
            continue
        mode = stat.S_IMODE(path.lstat().st_mode)
        if path.is_symlink():
            target = os.readlink(path)
            entries[relative] = {'type': 'symlink', 'target': target, 'mode': oct(mode),
                                 'sha256': hashlib.sha256(os.fsencode(target)).hexdigest()}
        elif path.is_file():
            entries[relative] = {'type': 'file', 'mode': oct(mode),
                                 'sha256': hashlib.sha256(path.read_bytes()).hexdigest()}
        else:
            raise ValueError(f'unsupported source file type: {relative}')
    return entries


def make_diff(old, new):
    earlier, current = set(old), set(new)
    return {'added': {p: new[p] for p in sorted(current-earlier)},
            'removed': {p: old[p] for p in sorted(earlier-current)},
            'changed': {p: {'old': old[p], 'new': new[p]}
                        for p in sorted(earlier & current) if old[p] != new[p]},
            'unchanged_count': sum(old[p] == new[p] for p in earlier & current)}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--baseline', type=Path, required=True)
    args = parser.parse_args()
    old = scan(args.baseline.resolve())
    current = scan(ROOT)
    self_diff = str(BASE / 'SOURCE_DIFF.json')
    old.pop(self_diff, None)
    current.pop(self_diff, None)
    diff = make_diff(old, current)
    (ROOT / self_diff).write_text(json.dumps(diff, indent=2, sort_keys=True)+'\n')
    entries = scan(ROOT)
    canonical = json.dumps(entries, sort_keys=True, separators=(',', ':')).encode()
    manifest = {
        'input_name': INPUT_NAME,
        'input_archive_sha256': INPUT_SHA,
        'input_commit_reported_by_owner': 'c1103be (gitless archive)',
        'entry_count': len(entries),
        'source_tree_sha256': hashlib.sha256(canonical).hexdigest(),
        'excluded_self_hashes': sorted(EXCLUDED),
        'entries': entries,
    }
    manifest_path = ROOT / BASE / 'SOURCE_MANIFEST.json'
    manifest_path.write_text(json.dumps(manifest, indent=2, sort_keys=True)+'\n')
    extra = str(BASE / 'SOURCE_MANIFEST.json')
    entries[extra] = {'type': 'file', 'sha256': hashlib.sha256(manifest_path.read_bytes()).hexdigest()}
    sums = ''.join(f"{entry['sha256']}  {p}\n" for p, entry in sorted(entries.items())
                   if entry['type'] == 'file')
    (ROOT / BASE / 'SHA256SUMS').write_text(sums)
    print(json.dumps({'entry_count': manifest['entry_count'],
                      'source_tree_sha256': manifest['source_tree_sha256'],
                      'regular_files_hashed': len(sums.splitlines()),
                      'added': len(diff['added']),
                      'removed': len(diff['removed']),
                      'changed': len(diff['changed'])}, indent=2))


if __name__ == '__main__':
    main()
