#!/usr/bin/env python3
"""Verify IP-02 extracted cumulative source inventory (no network or cargo)."""
import hashlib
import json
import os
from pathlib import Path
import sys


def sha(path):
    h = hashlib.sha256()
    with path.open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            h.update(chunk)
    return h.hexdigest()


def main():
    root = Path(sys.argv[1] if len(sys.argv) > 1 else '.').resolve()
    manifest = root / 'IP02_SOURCE_MANIFEST.json'
    if not manifest.is_file():
        raise SystemExit('missing IP02_SOURCE_MANIFEST.json')
    inventory = json.loads(manifest.read_text())
    expected = set(inventory['files'])
    expected.update(['IP02_SOURCE_MANIFEST.json', 'IP02_SOURCE_SHA256SUMS'])
    actual = set()
    for p in root.rglob('*'):
        if p.is_file() or p.is_symlink():
            actual.add(p.relative_to(root).as_posix())
    if actual != expected:
        raise SystemExit(f'file inventory changed: extra={sorted(actual-expected)}, missing={sorted(expected-actual)}')
    for name, metadata in inventory['files'].items():
        path = root / name
        if metadata['type'] == 'symlink':
            if not path.is_symlink() or os.readlink(path) != metadata['target']:
                raise SystemExit(f'symlink identity drift: {name}')
            if not path.resolve().is_relative_to(root):
                raise SystemExit(f'unsafe symlink target: {name}')
        elif metadata['type'] == 'file':
            if path.is_symlink() or not path.is_file():
                raise SystemExit(f'not a regular file: {name}')
            if sha(path) != metadata['sha256'] or path.stat().st_size != metadata['size']:
                raise SystemExit(f'file content drift: {name}')
            if (path.stat().st_mode & 0o777) != metadata['mode']:
                raise SystemExit(f'file mode drift: {name}')
        else:
            raise SystemExit(f'unknown member type: {name}')
    checksums = (root / 'IP02_SOURCE_SHA256SUMS').read_text().splitlines()
    if len(checksums) != len([n for n in inventory['files'].values() if n['type']=='file']) + 1:
        raise SystemExit('checksum inventory length drift')
    for entry in checksums:
        claimed, _, name = entry.partition('  ')
        if not name or sha(root / name) != claimed:
            raise SystemExit(f'checksum mismatch: {name}')
    print(f"PASS: {len(inventory['files'])} manifest entries, {len(checksums)} checksum entries, all source bytes/modes and symlinks verified")


if __name__ == '__main__':
    main()
