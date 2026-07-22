#!/usr/bin/env python3
"""Verify the complete handoff symlink set and literal targets."""

from __future__ import annotations

import os
from pathlib import Path, PurePosixPath


def main() -> None:
    root = Path(__file__).resolve().parents[2]
    ledger = root / "docs" / "handoff_symlinks.txt"
    expected: dict[str, str] = {}
    for line_number, raw in enumerate(ledger.read_text(encoding="utf-8").splitlines(), 1):
        if not raw or raw.startswith("#"):
            continue
        try:
            path_text, target = raw.split("\t")
        except ValueError as error:
            raise AssertionError(f"invalid symlink ledger line {line_number}") from error
        path = PurePosixPath(path_text)
        if path.is_absolute() or ".." in path.parts or path_text in expected:
            raise AssertionError(f"unsafe or duplicate symlink ledger path: {path_text}")
        expected[path_text] = target

    actual: dict[str, str] = {}
    for directory, directories, files in os.walk(root, followlinks=False):
        directory_path = Path(directory)
        for name in [*directories, *files]:
            entry = directory_path / name
            if entry.is_symlink():
                relative = entry.relative_to(root).as_posix()
                actual[relative] = os.readlink(entry)

    if actual != expected:
        raise AssertionError(f"symlink ledger mismatch: expected={expected!r}, actual={actual!r}")

    resolved_root = root.resolve()
    for path_text, target in actual.items():
        entry = root / path_text
        resolved = (entry.parent / target).resolve(strict=False)
        if resolved != resolved_root and resolved_root not in resolved.parents:
            raise AssertionError(f"symlink escapes handoff root: {path_text} -> {target}")
        if not resolved.exists():
            raise AssertionError(f"symlink target does not exist: {path_text} -> {target}")

    print(f"handoff symlink ledger verified: {len(actual)} entry")


if __name__ == "__main__":
    main()
