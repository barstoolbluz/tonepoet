#!/usr/bin/env python3
"""Build/verify TonePoet's relocatable Reference external-runtime closure manifest.

The manifest names package-private files by canonical relative path and binds file
bytes or exact relative symlink text.  The installation prefix is intentionally
not part of identity.  The manifest MUST live outside the closure tree so it
cannot recursively describe itself.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import stat
import tempfile

SCHEMA = "tonepoet-reference-runtime-closure/v1"
MAX_ENTRIES = 100_000


def sha256_file_stable(path: Path) -> str:
    def once() -> str:
        h = hashlib.sha256()
        with path.open("rb", buffering=1024 * 1024) as f:
            while True:
                chunk = f.read(1024 * 1024)
                if not chunk:
                    break
                h.update(chunk)
        return h.hexdigest()
    first = once()
    second = once()
    if first != second:
        raise RuntimeError(f"file changed while hashing: {path}")
    return first


def canonical_rel(root: Path, path: Path) -> str:
    rel = path.relative_to(root)
    text = rel.as_posix()
    pure = PurePosixPath(text)
    if not text or text == "." or pure.is_absolute() or any(part in ("", ".", "..") for part in pure.parts):
        raise RuntimeError(f"non-canonical closure path: {text!r}")
    text.encode("utf-8")
    return text


def scan(root: Path) -> list[dict[str, str]]:
    root = root.resolve(strict=True)
    if not root.is_dir():
        raise RuntimeError(f"closure root is not a directory: {root}")
    entries: list[dict[str, str]] = []
    for parent, dirnames, filenames in os.walk(root, topdown=True, followlinks=False):
        dirnames.sort()
        filenames.sort()
        parent_path = Path(parent)
        # os.walk reports symlink-to-directory names in dirnames when followlinks=False;
        # move them into the materialized entry set instead of silently ignoring them.
        for name in list(dirnames):
            path = parent_path / name
            if path.is_symlink():
                dirnames.remove(name)
                filenames.append(name)
        # Directory existence is part of the strict closure identity.  Record
        # every real directory (except the implicit root) so added/removed empty
        # directories fail attestation just like files and symlinks.
        for name in dirnames:
            path = parent_path / name
            entries.append({"path": canonical_rel(root, path), "kind": "directory"})
            if len(entries) > MAX_ENTRIES:
                raise RuntimeError(f"closure exceeds {MAX_ENTRIES} entries")
        filenames.sort()
        for name in filenames:
            path = parent_path / name
            rel = canonical_rel(root, path)
            st = path.lstat()
            mode = st.st_mode
            if stat.S_ISREG(mode):
                entries.append({"path": rel, "kind": "file", "sha256": sha256_file_stable(path)})
            elif stat.S_ISLNK(mode):
                target = os.readlink(path)
                if os.path.isabs(target):
                    raise RuntimeError(f"absolute symlink target in closure: {rel} -> {target}")
                target.encode("utf-8")
                resolved = path.resolve(strict=True)
                try:
                    resolved.relative_to(root)
                except ValueError as exc:
                    raise RuntimeError(f"symlink escapes closure root: {rel} -> {target}") from exc
                entries.append({"path": rel, "kind": "symlink", "target": target})
            else:
                raise RuntimeError(f"unsupported special file in closure: {rel}")
            if len(entries) > MAX_ENTRIES:
                raise RuntimeError(f"closure exceeds {MAX_ENTRIES} entries")
    entries.sort(key=lambda item: item["path"])
    for prev, cur in zip(entries, entries[1:]):
        if prev["path"] >= cur["path"]:
            raise RuntimeError(f"duplicate/non-increasing closure path: {cur['path']}")
    return entries


def digest(entries: list[dict[str, str]]) -> str:
    h = hashlib.sha256()
    def field(data: bytes) -> None:
        h.update(len(data).to_bytes(8, "big"))
        h.update(data)
    field(SCHEMA.encode())
    for entry in entries:
        field(entry["path"].encode())
        field(entry["kind"].encode())
        if entry["kind"] == "file":
            field(bytes.fromhex(entry["sha256"]))
        elif entry["kind"] == "symlink":
            field(entry["target"].encode())
    return h.hexdigest()


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("closure_root", type=Path)
    parser.add_argument("manifest", type=Path)
    args = parser.parse_args()

    root = args.closure_root.resolve(strict=True)
    manifest_parent = args.manifest.parent.resolve(strict=True)
    manifest = manifest_parent / args.manifest.name
    try:
        manifest.relative_to(root)
    except ValueError:
        pass
    else:
        raise RuntimeError("manifest must live outside the closure tree")

    first = scan(root)
    second = scan(root)
    if first != second:
        raise RuntimeError("runtime closure changed while constructing its manifest")
    payload = {"schema": SCHEMA, "entries": first}
    encoded = (json.dumps(payload, indent=2, sort_keys=False) + "\n").encode()

    fd, temp_name = tempfile.mkstemp(prefix=f".{manifest.name}.", suffix=".tmp", dir=manifest_parent)
    temp = Path(temp_name)
    try:
        with os.fdopen(fd, "wb", closefd=True) as f:
            f.write(encoded)
            f.flush()
            os.fsync(f.fileno())
        os.replace(temp, manifest)
        dir_fd = os.open(manifest_parent, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
        try:
            os.fsync(dir_fd)
        finally:
            os.close(dir_fd)
    finally:
        temp.unlink(missing_ok=True)

    final = scan(root)
    if final != first:
        raise RuntimeError("runtime closure changed before manifest publication completed")
    print(f"schema={SCHEMA}")
    print(f"entries={len(first)}")
    print(f"digest={digest(first)}")
    print(f"manifest={manifest}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
