#!/usr/bin/env python3
"""Produce IP-05 full-source inventory, unchanged-input delta, and dual SHA lists."""
import argparse
import difflib
import hashlib
import json
import os
from pathlib import Path
import stat

BASE = Path('docs/cue_parallel_ip01')
EVIDENCE = BASE / 'ip05_verification'
EXCLUDED = {
    str(EVIDENCE / 'SOURCE_MANIFEST.json'),
    str(EVIDENCE / 'SHA256SUMS'),
    str(BASE / 'SOURCE_MANIFEST.json'),
    str(BASE / 'SHA256SUMS'),
}
SELF_DIFF = {str(EVIDENCE / 'SOURCE_DIFF.json'), str(EVIDENCE / 'PRODUCTION_DIFF.patch')}
INPUT_SHA = 'de88377467699875b265231893623b49ee1cfd6a68c20b284a6bf361444defa0'
INPUT_NAME = 'TonePoet_SNAPSHOT_9c06f0f_2026-10-25.tar.gz'
PRODUCTION = ('src/convert/pipeline/stages.rs','src/convert/pipeline/track_executor.rs')


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def scan(root: Path):
    entries = {}
    for file in sorted(root.rglob('*')):
        if file.is_dir() and not file.is_symlink():
            continue
        rel = file.relative_to(root).as_posix()
        if rel in EXCLUDED:
            continue
        mode = stat.S_IMODE(file.lstat().st_mode)
        if file.is_symlink():
            target = os.readlink(file)
            entries[rel] = {'type':'symlink', 'target':target, 'mode':oct(mode), 'sha256':sha256(os.fsencode(target))}
        elif file.is_file():
            entries[rel] = {'type':'file', 'mode':oct(mode), 'sha256':sha256(file.read_bytes())}
        else:
            raise ValueError(f'unsupported file type: {rel}')
    return entries


def make_diff(before, after):
    old = set(before); new=set(after)
    return {
        'added': {p:after[p] for p in sorted(new-old)},
        'removed': {p:before[p] for p in sorted(old-new)},
        'changed': {p:{'old':before[p],'new':after[p]} for p in sorted(old&new) if before[p]!=after[p]},
        'unchanged_count':sum(before[p] == after[p] for p in old&new),
    }


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--baseline', required=True, type=Path)
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[3]
    baseline=args.baseline.resolve()
    if not (baseline/'Cargo.toml').is_file():
        raise SystemExit('baseline must point to extracted tonepoet root')
    prior=scan(baseline)
    for name in PRODUCTION:
        original=(baseline/name).read_text()
        updated=(root/name).read_text()
        assert original!=updated, f'missing IP05 production change: {name}'
    for name in sorted(set(prior) & set(scan(root))):
        if name.startswith('src/') and prior[name]['sha256']!=scan(root)[name]['sha256'] and name not in PRODUCTION:
            raise AssertionError(f'unrelated src edit: {name}')
    production_patch=[]
    for name in PRODUCTION:
        production_patch += list(difflib.unified_diff(
            (baseline/name).read_text().splitlines(keepends=True),
            (root/name).read_text().splitlines(keepends=True),
            fromfile='a/'+name, tofile='b/'+name, n=5))
    (root/EVIDENCE/'PRODUCTION_DIFF.patch').write_text(''.join(production_patch))
    old=scan(baseline); new=scan(root)
    for p in SELF_DIFF:
        old.pop(p,None); new.pop(p,None)
    diff=make_diff(old,new)
    (root/EVIDENCE/'SOURCE_DIFF.json').write_text(json.dumps(diff,indent=2,sort_keys=True)+'\n')
    entries=scan(root)
    canonical=json.dumps(entries,sort_keys=True,separators=(',',':')).encode()
    manifest={
        'packet': 'IP-05',
        'input_archive': INPUT_NAME,
        'input_archive_sha256': INPUT_SHA,
        'input_git_commit_owner_reported': '9c06f0f (archive has no .git)',
        'entry_count': len(entries),
        'source_tree_sha256': sha256(canonical),
        'excluded_self_hashes': sorted(EXCLUDED),
        'entries': entries,
    }
    encoded=json.dumps(manifest,indent=2,sort_keys=True)+'\n'
    for rel in (EVIDENCE/'SOURCE_MANIFEST.json', BASE/'SOURCE_MANIFEST.json'):
        (root/rel).write_text(encoded)
    sums={name:record['sha256'] for name,record in entries.items() if record['type']=='file'}
    for rel in (EVIDENCE/'SOURCE_MANIFEST.json', BASE/'SOURCE_MANIFEST.json'):
        sums[rel.as_posix()]=sha256(encoded.encode())
    checksum_data=''.join(f'{digest}  {name}\n' for name,digest in sorted(sums.items()))
    for rel in (EVIDENCE/'SHA256SUMS', BASE/'SHA256SUMS'):
        (root/rel).write_text(checksum_data)
    print(json.dumps({
        'source_tree_sha256': manifest['source_tree_sha256'],
        'entry_count_excluding_self_hashes':len(entries),
        'file_hashes':len(sums),
        'added':len(diff['added']),'removed':len(diff['removed']),
        'changed':len(diff['changed']),
        'changed_source_files':[name for name in diff['changed'] if name.startswith('src/')],
    },indent=2))


if __name__=='__main__':
    main()
