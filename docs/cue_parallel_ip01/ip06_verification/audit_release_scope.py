#!/usr/bin/env python3
"""Compare this cumulative source tree against the uploaded S3-promotion tar.

Only the promotion rollback, IP-06 documents and root checksum may change.
Archives do not include Git history; do not infer branch or commit identity.
"""
import argparse
import hashlib
import json
import tarfile
from pathlib import Path

VERIFICATION = 'docs/cue_parallel_ip01/ip06_verification/'
# Source tar contained these two generated CPython files, excluded from release.
BYTECODE_EXCLUSIONS = {
    'tonepoet-pipeline/qualification/ssrc_binary64/__pycache__/qualify_ssrc_binary64.cpython-313.pyc',
    'tonepoet-pipeline/qualification/ssrc_binary64/__pycache__/test_qualify_ssrc_binary64.cpython-313.pyc',
}
CHANGED_ALLOWED = {
    'src/convert/pipeline/stages.rs', 'SHA256SUMS',
    'docs/cue_parallel_ip01/IMPLEMENTATION_STATUS.md',
    'docs/cue_parallel_ip01/ARCHITECTURE_DECISIONS.md',
    'docs/cue_parallel_ip01/CONFORMANCE_MATRIX.md',
    'docs/cue_parallel_ip01/VERIFICATION_RESULTS.md',
    'docs/cue_parallel_ip01/NEXT_SESSION_HANDOFF.md',
}


def sha_stream(source):
    h = hashlib.sha256()
    for data in iter(lambda: source.read(1024 * 1024), b''):
        h.update(data)
    return h.hexdigest()


def check(source, root):
    original = {}
    with tarfile.open(source, 'r:gz') as t:
        for info in t:
            if info.isdir():
                continue
            if not info.name.startswith('tonepoet/') or '..' in info.name.split('/'):
                raise ValueError(f'unsafe archive entry {info.name!r}')
            name = info.name.removeprefix('tonepoet/')
            if info.isfile():
                with t.extractfile(info) as f:
                    original[name] = ('file', sha_stream(f))
            elif info.issym():
                original[name] = ('link', info.linkname)
            else:
                raise ValueError(f'unexpected source entry type {info.name!r}')
    actual = {}
    for path in sorted(root.rglob('*')):
        if path.is_dir() and not path.is_symlink():
            continue
        name = path.relative_to(root).as_posix()
        if path.is_symlink():
            actual[name] = ('link', path.readlink().as_posix())
        elif path.is_file():
            with path.open('rb') as f:
                actual[name] = ('file', sha_stream(f))
        else:
            raise ValueError(f'unexpected source entry type {name!r}')
    removed = sorted(set(original) - set(actual))
    changed = sorted(x for x in original.keys() & actual.keys() if original[x] != actual[x])
    added = sorted(set(actual) - set(original))
    unwanted_changes = [x for x in changed if x not in CHANGED_ALLOWED and not x.startswith(VERIFICATION)]
    unwanted_additions = [x for x in added if not x.startswith(VERIFICATION)]
    unexpected_removed = [x for x in removed if x not in BYTECODE_EXCLUSIONS]
    if unexpected_removed or unwanted_changes or unwanted_additions:
        raise ValueError(f'unexpected scope: removed={unexpected_removed}, changed={unwanted_changes}, added={unwanted_additions}')
    if 'src/convert/pipeline/stages.rs' not in changed:
        raise ValueError('selector rollback not detected')
    return {'status': 'PASS', 'source_archive_sha256': sha_stream(source.open('rb')),
            'source_file_entries': len(original), 'output_file_entries': len(actual),
            'changed': changed, 'added': added, 'removed': removed,
            'git_provenance': 'not present in input tarball'}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('source_archive', type=Path)
    ap.add_argument('tree', type=Path)
    ap.add_argument('--json', type=Path)
    args = ap.parse_args()
    result = check(args.source_archive, args.tree)
    if args.json:
        args.json.write_text(json.dumps(result, indent=2, sort_keys=True) + '\n')
    print(f'{result["status"]}: {len(result["changed"])} changed; {len(result["added"])} added; {len(result["removed"])} bytecode caches excluded; only approved paths')
    print('CHANGED:', ', '.join(result['changed']))


if __name__ == '__main__':
    main()
