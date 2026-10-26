#!/usr/bin/env python3
"""Fail-closed SHA256 source scope audit relative to reviewed default-OFF archive.

Compare all regular files and symlinks, including deletions, excluding only
package-updated checksum/manifest artifacts. No claim about Git ancestry.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import tarfile
from pathlib import Path

BASE_SHA = '9722a3e9551fa2f2e257a3d690ef9c6e19945072c61375d01fb95875d736d921'
ROOT = Path(__file__).resolve().parents[3]
DOCS = 'docs/cue_parallel_ip01/'
CURRENT_LEDGERS = {
    'ARCHITECTURE_DECISIONS.md', 'CONFORMANCE_MATRIX.md',
    'VERIFICATION_RESULTS.md', 'IMPLEMENTATION_STATUS.md',
    'NEXT_SESSION_HANDOFF.md', 'CHANGED_FILE_SUMMARY.md',
}
CHANGING = {
    'src/convert/pipeline/stages.rs', 'SHA256SUMS',
    DOCS + 'ip06_verification/SOURCE_MANIFEST.json',
    DOCS + 'ip06_verification/README.md',
    DOCS + 'ip06_verification/CHECKPOINT.md',
    DOCS + 'ip06_verification/CHANGED_FILES.md',
    DOCS + 'ip06_verification/DECISION_RECORD.md',
    DOCS + 'ip06_verification/package_cumulative.py',
} | {DOCS + n for n in CURRENT_LEDGERS}
ADDED = {
    DOCS + 'ip06_verification/INDEPENDENT_REVIEW_2026-10-25.md',
    DOCS + 'ip06_verification/REVIEW_ROLLBACK_CHECKPOINT_2026-10-25.md',
    DOCS + 'ip06_verification/PRODUCTION_ENABLE_DEFAULT_ON.patch',
    DOCS + 'ip06_verification/PRODUCTION_DISABLE_DEFAULT_ON.patch',
    DOCS + 'ip06_verification/audit_default_on_delta.py',
    DOCS + 'ip06_verification/DEFAULT_ON_SCOPE_AUDIT.json',
}


def sha(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def compare(source: Path, root: Path) -> dict:
    if sha(source.read_bytes()) != BASE_SHA:
        raise ValueError('reviewed input archive SHA-256 mismatch')
    original = {}
    with tarfile.open(source, 'r:gz') as tar:
        for m in tar:
            if m.isdir():
                continue
            if not m.name.startswith('tonepoet/') or '..' in m.name.split('/'):
                raise ValueError('unsafe archive path: ' + m.name)
            rel = m.name[len('tonepoet/'):]
            if m.isfile():
                with tar.extractfile(m) as stream:
                    digest = hashlib.file_digest(stream, 'sha256').hexdigest()
                original[rel] = ('file', digest)
            elif m.issym():
                original[rel] = ('symlink', m.linkname)
            else:
                raise ValueError('unsupported entry: ' + m.name)
    current = {}
    for p in root.rglob('*'):
        if p.is_symlink():
            current[p.relative_to(root).as_posix()] = ('symlink', p.readlink().as_posix())
        elif p.is_file():
            with p.open('rb') as stream:
                current[p.relative_to(root).as_posix()] = ('file', hashlib.file_digest(stream, 'sha256').hexdigest())
    # This report references itself; only its own content is excluded from
    # unchanged-file verification. It must be present at packaging time.
    changed = sorted(k for k in original.keys() & current.keys() if original[k] != current[k])
    added = sorted(current.keys() - original.keys())
    removed = sorted(original.keys() - current.keys())
    problems = []
    if removed:
        problems.append(f'unexpected removals: {removed}')
    if set(changed) != CHANGING:
        problems.append(f'changed mismatch: expected={sorted(CHANGING)} found={changed}')
    if set(added) != ADDED:
        problems.append(f'added mismatch: expected={sorted(ADDED)} found={added}')
    for k in original.keys() & current.keys():
        if k not in CHANGING and original[k] != current[k]:
            problems.append(f'unexpected change: {k}')
    result = {
        'status': 'PASS' if not problems else 'FAIL',
        'input_archive_sha256': BASE_SHA,
        'changed': changed,
        'added': added,
        'removed': removed,
        'unchanged': sum(1 for x in original.keys() & current.keys() if original[x] == current[x]),
        'other_production_changes': [k for k in changed if not k.startswith(DOCS) and k not in {'src/convert/pipeline/stages.rs', 'SHA256SUMS'}],
        'errors': problems,
        'git_history_verified': False,
    }
    if problems:
        raise ValueError(json.dumps(result, indent=2))
    return result


if __name__ == '__main__':
    ap = argparse.ArgumentParser()
    ap.add_argument('reviewed_input_tar', type=Path)
    args = ap.parse_args()
    print(json.dumps(compare(args.reviewed_input_tar, ROOT), indent=2))
