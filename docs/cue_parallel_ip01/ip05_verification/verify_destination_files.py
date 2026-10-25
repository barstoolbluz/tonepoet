#!/usr/bin/env python3
"""Host-side IP-05 visible destination-set gate (exact files, no private state)."""
import argparse
import json
from pathlib import Path, PurePosixPath
import tempfile


def verify(root: Path, expected: list[str]) -> list[str]:
    if not root.is_dir() or root.is_symlink():
        return ["destination absent, symlink, or not a directory"]
    wanted = set()
    errors = []
    for item in expected:
        path = PurePosixPath(item)
        if path.is_absolute() or any(p in (".", "..", "") or p.startswith(".") for p in path.parts) or not path.parts:
            errors.append(f"unsafe expected path: {item!r}")
        else:
            if item in wanted:
                errors.append(f"duplicate expected path: {item!r}")
            wanted.add(item)
    if not wanted:
        errors.append("empty expected file set")
    observed = set()
    for path in root.rglob("*"):
        rel = path.relative_to(root).as_posix()
        if any(part.startswith(".") for part in path.relative_to(root).parts):
            errors.append(f"hidden destination state: {rel}")
        if path.is_symlink():
            errors.append(f"destination symlink: {rel}")
        elif path.is_dir():
            if not any(name.startswith(rel + "/") for name in wanted):
                errors.append(f"unrequested directory: {rel}")
            continue
        elif path.is_file():
            observed.add(rel)
        else:
            errors.append(f"nonregular destination node: {rel}")
    for rel in sorted(observed - wanted):
        errors.append(f"unrequested file: {rel}")
    for rel in sorted(wanted - observed):
        errors.append(f"missing requested file: {rel}")
    return errors


def self_test() -> None:
    with tempfile.TemporaryDirectory(prefix="ip05-dest-") as directory:
        root = Path(directory)
        (root / "disc").mkdir()
        (root / "disc" / "01.flac").write_bytes(b"FLAC")
        assert not verify(root, ["disc/01.flac"])
        (root / "disc" / ".cue-lock").write_text("not allowed")
        assert any("hidden destination state" in item for item in verify(root, ["disc/01.flac"]))
        (root / "disc" / ".cue-lock").unlink()
        (root / "disc" / "marker.json").write_text("{}")
        assert any("unrequested file" in item for item in verify(root, ["disc/01.flac"]))
        (root / "disc" / "marker.json").unlink()
        (root / "disc" / "staging").mkdir()
        assert any("unrequested directory" in item for item in verify(root, ["disc/01.flac"]))
        (root / "disc" / "staging").rmdir()
        assert verify(root, ["disc/01.flac", "disc/02.flac"])
        assert verify(root, ["../01.flac"])
    print("PASS: exact destination-set self-test (allowed, hidden, extra file, extra directory, missing, traversal)")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--destination", type=Path)
    parser.add_argument("--expected-json", type=Path,
                        help="JSON array of exactly the requested relative output paths")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    if args.self_test:
        self_test()
        return 0
    if args.destination is None or args.expected_json is None:
        parser.error("--destination and --expected-json are required without --self-test")
    expected = json.loads(args.expected_json.read_text())
    if not isinstance(expected, list) or not all(isinstance(x, str) for x in expected):
        parser.error("expected JSON must be an array of relative pathname strings")
    errors = verify(args.destination, expected)
    if errors:
        print("FAIL: destination contains missing/unrequested/hidden paths:")
        for item in errors:
            print(" -", item)
        return 1
    print(f"PASS: {len(expected)} requested files, no hidden/unrequested destination state")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
