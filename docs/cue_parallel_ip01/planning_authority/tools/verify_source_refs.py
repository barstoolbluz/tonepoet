#!/usr/bin/env python3
"""Verify plan's source paths/line anchors against separately extracted TonePoet snapshot.

This does not establish that cited source *supports* the claim; that requires audit.
"""
import argparse
import re
from pathlib import Path

FULL = re.compile(r'(?<![\w/])(?P<file>(?:src|tonepoet-pipeline)/[\w./-]+\.rs)(?::(?P<start>\d+)(?:-(?P<end>\d+))?)?')
BARE = re.compile(r'(?<![\w/])(?P<file>[\w-]+\.rs):(?P<start>\d+)(?:-(?P<end>\d+))?')
SHORT = re.compile(r'(?<![\w/]):(?P<start>\d+)(?:-(?P<end>\d+))?')


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('plan_root', type=Path)
    ap.add_argument('source_root', type=Path)
    args = ap.parse_args()
    root = args.plan_root.resolve()
    src = args.source_root.resolve()
    assert (src/'src/convert/pipeline/materializer_cue.rs').is_file(), 'not a TonePoet source root'
    by_name = {}
    for p in src.rglob('*.rs'):
        by_name.setdefault(p.name, []).append(p.relative_to(src).as_posix())
    errors = []
    checked = 0
    counts = {}
    def nlines(path):
        if path not in counts:
            counts[path] = len((src/path).read_text(encoding='utf-8').splitlines())
        return counts[path]
    for doc in sorted(root.rglob('*.md')):
        current = None
        for lineno, line in enumerate(doc.read_text(encoding='utf-8').splitlines(), 1):
            tokens = []
            for pat, rank in ((FULL, 0), (BARE, 1), (SHORT, 2)):
                for match in pat.finditer(line):
                    tokens.append((match.start(), match.end(), rank, match))
            # Named path tokens take precedence over bare basename and numeric short references.
            tokens.sort(key=lambda x: (x[0], x[2], -x[1]))
            occupied = []
            named_on_line = []
            for left, right, rank, match in tokens:
                if any(a <= left and right <= b for a, b in occupied):
                    continue
                occupied.append((left,right))
                if rank == 0:
                    path = match.group('file')
                elif rank == 1:
                    candidates = by_name.get(match.group('file'), [])
                    path = (current if current and current.endswith('/'+match.group('file'))
                            else candidates[0] if len(candidates) == 1 else None)
                    if not path:
                        errors.append(f'{doc.relative_to(root)}:{lineno}: ambiguous/unresolved {match.group(0)}')
                        continue
                else:
                    # Short ranges require an explicit path in this line, otherwise not treated as
                    # a machine-verifiable reference. The last explicit file normally owns them.
                    if not named_on_line:
                        continue
                    path = current
                    lo = int(match.group('start'))
                    hi = int(match.group('end') or lo)
                    if path and hi > nlines(path):
                        eligible = [name for name in named_on_line if hi <= nlines(name)]
                        if len(set(eligible)) == 1:
                            path = eligible[0]
                if not path:
                    continue
                current = path
                target = (src/path).resolve()
                if not target.is_relative_to(src) or not target.is_file():
                    errors.append(f'{doc.relative_to(root)}:{lineno}: nonexistent {path}')
                    continue
                if rank != 2:
                    named_on_line.append(path)
                start = match.group('start')
                if start:
                    lo = int(start)
                    hi = int(match.group('end') or start)
                    if lo < 1 or hi < lo or hi > nlines(path):
                        errors.append(f'{doc.relative_to(root)}:{lineno}: invalid {path}:{lo}-{hi} (file has {nlines(path)} lines)')
                    else:
                        checked += 1
    for e in errors:
        print('ERROR:', e)
    print(f'Source-reference check: {checked} valid anchors, {len(errors)} errors')
    raise SystemExit(1 if errors else 0)


if __name__ == '__main__':
    main()
