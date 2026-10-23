#!/usr/bin/env python3
"""Validate packet DAG, Markdown links, package inventory and SHA256SUMS."""
import argparse
import hashlib
import json
import re
from pathlib import Path

REQUIRED = ('00_START_HERE.md', '01_SOURCE_AUDIT.md', '02_ARCHITECTURE_DECISION.md',
            '03_IMPLEMENTATION_PLAN.md', '04_FORMAT_CAPABILITY_MATRIX.md',
            '05_CONFORMANCE_AND_PERFORMANCE.md', 'IMPLEMENTATION_STATUS.md',
            'ARCHITECTURE_DECISIONS.md', 'CONFORMANCE_MATRIX.md',
            'VERIFICATION_RESULTS.md', 'NEXT_SESSION_HANDOFF.md',
            'SOURCE_MANIFEST.json', '06_INDEPENDENT_ADVERSARIAL_AUDIT.md',
            '07_CHANGE_SUMMARY.md', 'manifest.json', 'SHA256SUMS')
LINK = re.compile(r'\[[^\]]*\]\(([^)#]+)(?:#[^)]*)?\)')


def sha(path):
    h = hashlib.sha256()
    with path.open('rb') as f:
        for b in iter(lambda: f.read(1024 * 1024), b''):
            h.update(b)
    return h.hexdigest()


def validate(root):
    errors = []
    for name in REQUIRED:
        if not (root/name).is_file():
            errors.append(f'missing {name}')
    if errors:
        return errors
    data = json.loads((root/'manifest.json').read_text())
    if data.get('planning_release') != 'R2_CORRECTIVE':
        errors.append('wrong planning release')
    listed = data['files']
    found = {p.relative_to(root).as_posix() for p in root.rglob('*') if p.is_file()}
    expected = set(listed) | {'manifest.json', 'SHA256SUMS'}
    if found != expected:
        errors.append(f'inventory mismatch missing={sorted(expected-found)} extra={sorted(found-expected)}')
    for name, meta in listed.items():
        p = root/name
        if p.is_file():
            if p.stat().st_size != meta['size'] or sha(p) != meta['sha256']:
                errors.append(f'inventory hash/size mismatch {name}')
        else:
            errors.append(f'missing listed file {name}')
    sums = {}
    for line in (root/'SHA256SUMS').read_text().splitlines():
        match = re.fullmatch(r'([0-9a-f]{64})  (.+)', line)
        if not match:
            errors.append('bad SHA256SUMS line ' + repr(line))
        elif match.group(2) in sums:
            errors.append('duplicate checksum line ' + match.group(2))
        else:
            sums[match.group(2)] = match.group(1)
    if set(sums) != expected - {'SHA256SUMS'}:
        errors.append('SHA256SUMS coverage mismatch')
    for name, digest in sums.items():
        p = root/name
        if p.is_file() and sha(p) != digest:
            errors.append(f'SHA256SUMS mismatch {name}')
    for doc in root.rglob('*.md'):
        for match in LINK.finditer(doc.read_text()):
            dest = match.group(1)
            if '://' in dest or dest.startswith(('mailto:', '#')):
                continue
            candidate = (doc.parent/dest).resolve()
            if not candidate.is_relative_to(root) or not candidate.exists():
                errors.append(f'broken link {doc.relative_to(root)} -> {dest}')
    nodes = {p['id']: p for p in data['packets']}
    if len(nodes) != len(data['packets']) or sorted(nodes) != ['IP-01','IP-02','IP-03','IP-04','IP-05','IP-06']:
        errors.append('wrong/duplicate committed packet IDs')
    visiting = set()
    visited = set()
    def walk(n):
        if n in visiting:
            errors.append(f'packet DAG cycle at {n}')
            return
        if n in visited:
            return
        visiting.add(n)
        for parent in nodes[n]['depends_on']:
            if parent not in nodes:
                errors.append(f'{n}: undefined dependency {parent}')
            else:
                walk(parent)
        visiting.remove(n)
        visited.add(n)
    for n in nodes:
        walk(n)
    for p in data['packets']:
        filename = p['prompt']
        if not (root/filename).is_file():
            errors.append(f'{p["id"]} missing prompt {filename}')
            continue
        src = (root/filename).read_text()
        required_sections = ('## Objective', '## Source', '## Required verification', 'STOP/SPLIT')
        if not all(fragment in src for fragment in required_sections):
            errors.append(f'{p["id"]}: missing standalone prompt sections')
        for key in ('rollback', 'cancel', 'replaygain', 'sha-256', 'NOT RUN'):
            if key.lower() not in src.lower():
                errors.append(f'{p["id"]}: lacks required {key}')
    return errors


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('plan_root', type=Path, nargs='?', default=Path('.'))
    root = ap.parse_args().plan_root.resolve()
    errors = validate(root)
    for e in errors:
        print('ERROR:', e)
    if errors:
        raise SystemExit(f'FAILED: {len(errors)} validation issues')
    data = json.loads((root/'manifest.json').read_text())
    print(f'PASS: {len(data["files"])} hashed payload files, {len(data["packets"])} prompts, acyclic DAG, links and SHA256SUMS')


if __name__ == '__main__':
    main()
