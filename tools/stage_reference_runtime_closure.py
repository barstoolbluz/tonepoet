#!/usr/bin/env python3
"""Build TonePoet's relocatable package-private Reference runtime from Nix outputs.

This tool intentionally performs *all* package mutations before invoking the
existing runtime-closure manifest builder.  The resulting public entry points
are static launchers; each launcher explicitly executes the copied Nix glibc
loader and a private payload.  Dynamic search paths inside the copied closure
are rewritten from absolute Nix paths to $ORIGIN-relative paths, and dynamic
objects are marked NODEFLIB so host /lib and /usr/lib are not valid fallbacks.

The source Nix store is never modified.  A failed build leaves the previous
published tree untouched and never publishes a manifest for partial bytes.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import stat
import subprocess
import sys
import tempfile
from typing import Iterable

TOOLS = ("sox", "ffmpeg", "ffprobe", "metaflac", "wvtag", "AtomicParsley")
ACTIVATION_ENV = {
    "sox": "TONEPOET_REFERENCE_SOX_PATH",
    "ffmpeg": "TONEPOET_REFERENCE_FFMPEG_PATH",
    "metaflac": "TONEPOET_REFERENCE_METAFLAC_PATH",
    "wvtag": "TONEPOET_REFERENCE_WVTAG_PATH",
    "AtomicParsley": "TONEPOET_REFERENCE_ATOMIC_PARSLEY_PATH",
}
PRIVATE_STORE = PurePosixPath("nix/store")
PRIVATE_META = PurePosixPath(".tonepoet-runtime")
MANIFEST_BUILDER = Path(__file__).with_name("build_reference_runtime_closure_manifest.py")
MAX_METADATA_BYTES = 8 * 1024 * 1024


class StageError(RuntimeError):
    pass


def run_checked(argv: list[str], *, env: dict[str, str] | None = None) -> subprocess.CompletedProcess[str]:
    try:
        return subprocess.run(
            argv,
            check=True,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            env=env,
        )
    except FileNotFoundError as exc:
        raise StageError(f"required command is unavailable: {argv[0]}") from exc
    except subprocess.CalledProcessError as exc:
        raise StageError(
            f"command failed ({exc.returncode}): {' '.join(argv)}\n"
            f"stdout:\n{exc.stdout}\nstderr:\n{exc.stderr}"
        ) from exc


def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb", buffering=1024 * 1024) as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()


def is_elf(path: Path) -> bool:
    if not path.is_file() or path.is_symlink():
        return False
    with path.open("rb") as f:
        return f.read(4) == b"\x7fELF"


def within(path: Path, root: Path) -> bool:
    try:
        path.relative_to(root)
        return True
    except ValueError:
        return False


def store_object_for(path: Path, store_root: Path) -> Path:
    path = path.resolve(strict=True)
    store_root = store_root.resolve(strict=True)
    if not within(path, store_root):
        raise StageError(f"qualified executable is outside {store_root}: {path}")
    rel = path.relative_to(store_root)
    if not rel.parts:
        raise StageError(f"qualified executable names the store root itself: {path}")
    return store_root / rel.parts[0]


def staged_store_path(stage_root: Path, store_root: Path, source: Path) -> Path:
    source_abs = source if source.is_absolute() else source.absolute()
    try:
        rel = source_abs.relative_to(store_root)
    except ValueError as exc:
        raise StageError(f"path is not under Nix store {store_root}: {source_abs}") from exc
    if not rel.parts:
        raise StageError(f"cannot map Nix store root itself into package tree: {source_abs}")
    return stage_root / Path(PRIVATE_STORE) / rel


def origin_relative(elf: Path, target: Path) -> str:
    rel = os.path.relpath(target, start=elf.parent)
    rel_posix = PurePosixPath(rel).as_posix()
    return "$ORIGIN" if rel_posix == "." else f"$ORIGIN/{rel_posix}"


def rewrite_search_entry(entry: str, *, elf: Path, stage_root: Path, store_root: Path) -> str:
    if not entry:
        raise StageError(f"empty RPATH/RUNPATH entry is forbidden: {elf}")
    if entry == "$ORIGIN" or entry == "${ORIGIN}" or entry.startswith("$ORIGIN/") or entry.startswith("${ORIGIN}/"):
        token = "${ORIGIN}" if entry.startswith("${ORIGIN}") else "$ORIGIN"
        suffix = entry[len(token):].lstrip("/")
        target = elf.parent if not suffix else (elf.parent / suffix)
        resolved = target.resolve(strict=False)
        if not within(resolved, stage_root.resolve()):
            raise StageError(f"existing ORIGIN search path escapes staged runtime: {elf}: {entry}")
        return entry
    candidate = Path(entry)
    if not candidate.is_absolute():
        raise StageError(f"relative non-ORIGIN RPATH/RUNPATH is not relocatable: {elf}: {entry}")
    if not within(candidate, store_root):
        raise StageError(f"absolute non-Nix RPATH/RUNPATH is not package-private: {elf}: {entry}")
    staged = staged_store_path(stage_root, store_root, candidate)
    if not staged.exists():
        raise StageError(f"RPATH/RUNPATH target is missing from copied Nix closure: {entry}")
    return origin_relative(elf, staged)


def rewrite_needed_entry(entry: str, *, elf: Path, stage_root: Path, store_root: Path) -> str:
    candidate = Path(entry)
    if not candidate.is_absolute():
        return entry
    if not within(candidate, store_root):
        raise StageError(f"absolute non-Nix DT_NEEDED entry is not package-private: {elf}: {entry}")
    staged = staged_store_path(stage_root, store_root, candidate)
    if not staged.exists():
        raise StageError(f"absolute DT_NEEDED target is missing from copied Nix closure: {entry}")
    return origin_relative(elf, staged)


def dynamic_table(path: Path, readelf: str) -> str | None:
    if not is_elf(path):
        return None
    result = run_checked([readelf, "-dW", str(path)])
    if "There is no dynamic section" in result.stdout:
        return None
    return result.stdout


def patch_dynamic_elf(path: Path, *, stage_root: Path, store_root: Path, patchelf: str, readelf: str) -> dict[str, int]:
    table = dynamic_table(path, readelf)
    if table is None:
        return {"dynamic_elf": 0, "rpath_entries_rewritten": 0, "needed_entries_rewritten": 0}
    has_rpath = "(RPATH)" in table
    has_runpath = "(RUNPATH)" in table
    if has_rpath and has_runpath:
        raise StageError(f"ELF carries both DT_RPATH and DT_RUNPATH; refusing to change lookup semantics: {path}")

    rpath = run_checked([patchelf, "--print-rpath", str(path)]).stdout.strip()
    rewritten_rpath: list[str] = []
    rpath_rewrites = 0
    if rpath:
        for entry in rpath.split(":"):
            new = rewrite_search_entry(entry, elf=path, stage_root=stage_root, store_root=store_root)
            rewritten_rpath.append(new)
            rpath_rewrites += int(new != entry)

    needed = [line.strip() for line in run_checked([patchelf, "--print-needed", str(path)]).stdout.splitlines() if line.strip()]
    replacements: list[tuple[str, str]] = []
    for entry in needed:
        new = rewrite_needed_entry(entry, elf=path, stage_root=stage_root, store_root=store_root)
        if new != entry:
            replacements.append((entry, new))

    original_mode = stat.S_IMODE(path.stat().st_mode)
    parent_mode = stat.S_IMODE(path.parent.stat().st_mode)
    try:
        path.chmod(original_mode | stat.S_IWUSR)
        path.parent.chmod(parent_mode | stat.S_IWUSR | stat.S_IXUSR)
        for old, new in replacements:
            run_checked([patchelf, "--replace-needed", old, new, str(path)])
        if rpath:
            set_rpath = [patchelf]
            if has_rpath:
                # patchelf otherwise converts DT_RPATH to DT_RUNPATH, which can
                # change transitive lookup semantics. Preserve the source tag.
                set_rpath.append("--force-rpath")
            set_rpath.extend(["--set-rpath", ":".join(rewritten_rpath), str(path)])
            run_checked(set_rpath)
        # The public launcher passes --inhibit-cache. NODEFLIB closes the other
        # default host-library fallback, so an omitted private dependency fails.
        run_checked([patchelf, "--no-default-lib", str(path)])
    finally:
        try:
            path.chmod(original_mode)
        finally:
            path.parent.chmod(parent_mode)

    final_table = dynamic_table(path, readelf)
    if final_table is None:
        raise StageError(f"dynamic ELF lost its dynamic table during relocation: {path}")
    if has_rpath != ("(RPATH)" in final_table) or has_runpath != ("(RUNPATH)" in final_table):
        raise StageError(f"RPATH/RUNPATH lookup semantics changed during relocation: {path}")
    if "NODEFLIB" not in final_table:
        raise StageError(f"DF_1_NODEFLIB was not recorded after relocation: {path}")

    final_rpath = run_checked([patchelf, "--print-rpath", str(path)]).stdout.strip()
    if final_rpath:
        for entry in final_rpath.split(":"):
            rewrite_search_entry(entry, elf=path, stage_root=stage_root, store_root=store_root)
    final_needed = [line.strip() for line in run_checked([patchelf, "--print-needed", str(path)]).stdout.splitlines() if line.strip()]
    for entry in final_needed:
        if Path(entry).is_absolute():
            raise StageError(f"absolute DT_NEEDED survived relocation: {path}: {entry}")

    return {
        "dynamic_elf": 1,
        "rpath_entries_rewritten": rpath_rewrites,
        "needed_entries_rewritten": len(replacements),
    }


def rewrite_absolute_symlinks(stage_root: Path, store_root: Path) -> int:
    count = 0
    # Snapshot the paths first because each rewrite replaces the directory entry.
    links = [p for p in stage_root.rglob("*") if p.is_symlink()]
    for link in links:
        target_text = os.readlink(link)
        if not os.path.isabs(target_text):
            continue
        source_target = Path(target_text)
        if not within(source_target, store_root):
            raise StageError(f"absolute symlink points outside Nix store: {link} -> {target_text}")
        staged_target = staged_store_path(stage_root, store_root, source_target)
        if not staged_target.exists() and not staged_target.is_symlink():
            raise StageError(f"absolute symlink target is absent from copied closure: {link} -> {target_text}")
        relative = os.path.relpath(staged_target, start=link.parent)
        parent_mode = stat.S_IMODE(link.parent.stat().st_mode)
        try:
            link.parent.chmod(parent_mode | stat.S_IWUSR | stat.S_IXUSR)
            link.unlink()
            link.symlink_to(relative)
        finally:
            link.parent.chmod(parent_mode)
        count += 1
    return count


def query_requisites(nix_store: str, roots: Iterable[Path], store_root: Path) -> list[Path]:
    argv = [nix_store, "--query", "--requisites", *[str(root) for root in roots]]
    output = run_checked(argv).stdout.splitlines()
    paths: set[Path] = set()
    for line in output:
        if not line.strip():
            continue
        path = Path(line.strip())
        if not path.is_absolute() or not within(path, store_root):
            raise StageError(f"nix-store returned a non-store requisite: {path}")
        paths.add(path)
    if not paths:
        raise StageError("nix-store returned an empty runtime closure")
    return sorted(paths, key=lambda p: p.as_posix())


def copy_requisites(requisites: list[Path], *, stage_root: Path, store_root: Path, cp: str) -> None:
    destination_store = stage_root / Path(PRIVATE_STORE)
    destination_store.mkdir(parents=True)
    for source in requisites:
        rel = source.relative_to(store_root)
        if len(rel.parts) != 1:
            raise StageError(f"nix-store requisite is not a store object root: {source}")
        destination = destination_store / rel.name
        if destination.exists() or destination.is_symlink():
            raise StageError(f"duplicate staged store object: {destination}")
        # Nix may deduplicate identical files as hardlinks.  Path-dependent ELF
        # rewrites must never mutate two staged names through the same inode, so
        # deliberately break hardlink identity while retaining all other archive
        # metadata.  Reflinks keep this cheap on filesystems that support CoW.
        run_checked([
            cp,
            "--archive",
            "--reflink=auto",
            "--no-preserve=links",
            "--",
            str(source),
            str(destination),
        ])


def resolve_public_tools(args: argparse.Namespace, store_root: Path) -> dict[str, Path]:
    explicit = {
        "sox": args.sox,
        "ffmpeg": args.ffmpeg,
        "metaflac": args.metaflac,
        "wvtag": args.wvtag,
        "AtomicParsley": args.atomic_parsley,
    }
    tools: dict[str, Path] = {}
    for name, raw in explicit.items():
        value = raw or os.environ.get(ACTIVATION_ENV[name])
        if not value:
            raise StageError(f"missing {name} path; pass --{name.lower().replace('atomicparsley', 'atomic-parsley')} or set {ACTIVATION_ENV[name]}")
        path = Path(value).resolve(strict=True)
        store_object_for(path, store_root)
        tools[name] = path
    ffprobe = (tools["ffmpeg"].parent / "ffprobe").resolve(strict=True)
    store_object_for(ffprobe, store_root)
    tools["ffprobe"] = ffprobe
    return tools


def validate_launcher(launcher: Path, *, readelf: str) -> None:
    launcher = launcher.resolve(strict=True)
    if not launcher.is_file() or launcher.is_symlink():
        raise StageError(f"launcher must be a regular non-symlink file: {launcher}")
    if not os.access(launcher, os.X_OK):
        raise StageError(f"launcher is not executable: {launcher}")
    if not is_elf(launcher):
        raise StageError(f"launcher is not ELF: {launcher}")
    program_headers = run_checked([readelf, "-lW", str(launcher)]).stdout
    if " INTERP " in program_headers or re.search(r"^\s*INTERP\s", program_headers, re.MULTILINE):
        raise StageError("Reference runtime launcher must be statically linked (PT_INTERP is present)")
    dynamic = run_checked([readelf, "-dW", str(launcher)]).stdout
    if "(NEEDED)" in dynamic:
        raise StageError("Reference runtime launcher must not have dynamic DT_NEEDED dependencies")


def make_runtime_entrypoints(
    *,
    stage_root: Path,
    source_tools: dict[str, Path],
    store_root: Path,
    launcher: Path,
    patchelf: str,
) -> dict[str, dict[str, str]]:
    bindir = stage_root / "bin"
    payload_dir = stage_root / Path(PRIVATE_META) / "payload"
    loader_dir = stage_root / Path(PRIVATE_META) / "loaders"
    bindir.mkdir(parents=True)
    payload_dir.mkdir(parents=True)
    loader_dir.mkdir(parents=True)
    result: dict[str, dict[str, str]] = {}

    launcher_bytes = launcher.read_bytes()
    launcher_mode = stat.S_IMODE(launcher.stat().st_mode) | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH

    for name in TOOLS:
        source = source_tools[name]
        staged_payload = staged_store_path(stage_root, store_root, source)
        if not staged_payload.is_file():
            raise StageError(f"staged public payload is not a regular file: {staged_payload}")
        interpreter = Path(run_checked([patchelf, "--print-interpreter", str(staged_payload)]).stdout.strip())
        if not interpreter.is_absolute() or not within(interpreter, store_root):
            raise StageError(f"qualified {name} does not use a Nix-store ELF interpreter: {interpreter}")
        staged_loader = staged_store_path(stage_root, store_root, interpreter)
        if not staged_loader.is_file():
            raise StageError(f"private loader for {name} is missing from closure: {staged_loader}")

        public = bindir / name
        public.write_bytes(launcher_bytes)
        public.chmod(launcher_mode)

        payload_link = payload_dir / name
        loader_link = loader_dir / name
        payload_link.symlink_to(os.path.relpath(staged_payload, start=payload_dir))
        loader_link.symlink_to(os.path.relpath(staged_loader, start=loader_dir))

        result[name] = {
            "source": str(source),
            "payload": staged_payload.relative_to(stage_root).as_posix(),
            "loader": staged_loader.relative_to(stage_root).as_posix(),
            "public": public.relative_to(stage_root).as_posix(),
            "source_sha256": sha256_file(source),
            "launcher_sha256": sha256_file(public),
        }
    return result


def parse_loader_list(text: str) -> list[Path]:
    paths: list[Path] = []
    for line in text.splitlines():
        stripped = line.strip()
        if not stripped or stripped.startswith("linux-vdso"):
            continue
        if "=> not found" in stripped:
            raise StageError(f"private loader reported a missing dependency: {stripped}")
        candidate: str | None = None
        if "=>" in stripped:
            rhs = stripped.split("=>", 1)[1].strip()
            if rhs.startswith("/"):
                candidate = rhs.split(" (", 1)[0].strip()
        elif stripped.startswith("/"):
            candidate = stripped.split(" (", 1)[0].strip()
        if candidate:
            paths.append(Path(candidate))
    return paths


def validate_private_resolution(stage_root: Path) -> dict[str, list[str]]:
    root = stage_root.resolve(strict=True)
    resolved: dict[str, list[str]] = {}
    base_env = {"LC_ALL": "C", "SOXR_USE_SIMD": "0"}
    probes = {
        "sox": ["--version"],
        "ffmpeg": ["-version"],
        "ffprobe": ["-version"],
        "metaflac": ["--version"],
        "wvtag": ["--version"],
        "AtomicParsley": [],
    }
    for name in TOOLS:
        public = root / "bin" / name
        list_env = dict(base_env)
        list_env["TONEPOET_REFERENCE_LAUNCHER_LIST"] = "1"
        listed = run_checked([str(public)], env=list_env)
        dependency_paths = parse_loader_list(listed.stdout + "\n" + listed.stderr)
        if not dependency_paths:
            raise StageError(f"private loader listed no dependencies for {name}")
        private: list[str] = []
        for path in dependency_paths:
            real = path.resolve(strict=True)
            if not within(real, root):
                raise StageError(f"{name} resolved a dynamic dependency outside staged runtime: {path} -> {real}")
            private.append(real.relative_to(root).as_posix())
        run_checked([str(public), *probes[name]], env=base_env)
        resolved[name] = sorted(set(private))
    return resolved


def build_manifest(stage_root: Path, manifest_path: Path, manifest_builder: Path) -> tuple[str, int]:
    manifest_path.parent.mkdir(parents=True, exist_ok=True)
    result = run_checked([sys.executable, str(manifest_builder), str(stage_root), str(manifest_path)])
    digest = None
    entries = None
    for line in result.stdout.splitlines():
        if line.startswith("digest="):
            digest = line.split("=", 1)[1]
        elif line.startswith("entries="):
            entries = int(line.split("=", 1)[1])
    if digest is None or entries is None:
        raise StageError(f"manifest builder did not report digest and entry count:\n{result.stdout}")
    if manifest_path.stat().st_size > MAX_METADATA_BYTES:
        raise StageError(f"generated manifest exceeds runtime reader limit: {manifest_path.stat().st_size} bytes")
    return digest, entries


def remove_path(path: Path) -> None:
    if path.is_dir() and not path.is_symlink():
        shutil.rmtree(path)
    else:
        path.unlink(missing_ok=True)


def publish_generation(
    *,
    staged_root: Path,
    output_root: Path,
    staged_manifest: Path,
    manifest: Path,
    staged_metadata: Path,
    metadata: Path,
    replace: bool,
) -> None:
    destinations = (output_root, manifest, metadata)
    existing = [path for path in destinations if path.exists() or path.is_symlink()]
    if existing and not replace:
        raise StageError(
            "published Reference runtime state already exists (use --replace): "
            + ", ".join(str(path) for path in existing)
        )

    backups: dict[Path, Path] = {}
    for destination in existing:
        backup = destination.with_name(f".{destination.name}.previous.{os.getpid()}")
        if backup.exists() or backup.is_symlink():
            raise StageError(f"stale publication backup exists: {backup}")
        os.replace(destination, backup)
        backups[destination] = backup

    published: list[Path] = []
    try:
        for source, destination in (
            (staged_root, output_root),
            (staged_manifest, manifest),
            (staged_metadata, metadata),
        ):
            os.replace(source, destination)
            published.append(destination)
    except Exception:
        for destination in reversed(published):
            if destination.exists() or destination.is_symlink():
                remove_path(destination)
        for destination, backup in backups.items():
            os.replace(backup, destination)
        raise
    else:
        for backup in backups.values():
            remove_path(backup)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output_root", type=Path, help="final package-private Reference runtime directory")
    parser.add_argument("manifest", type=Path, help="final runtime-closure manifest path; must be outside output_root")
    parser.add_argument("--launcher", type=Path, required=True, help="statically linked reference_runtime_launcher binary")
    parser.add_argument("--manifest-builder", type=Path, default=MANIFEST_BUILDER, help="runtime-closure manifest builder")
    parser.add_argument("--store-root", type=Path, default=Path("/nix/store"))
    parser.add_argument("--nix-store", default="nix-store")
    parser.add_argument("--patchelf", default="patchelf")
    parser.add_argument("--readelf", default="readelf")
    parser.add_argument("--cp", default="cp")
    parser.add_argument("--replace", action="store_true", help="replace an existing complete staged tree")
    parser.add_argument("--sox", type=Path)
    parser.add_argument("--ffmpeg", type=Path)
    parser.add_argument("--metaflac", type=Path)
    parser.add_argument("--wvtag", type=Path)
    parser.add_argument("--atomic-parsley", dest="atomic_parsley", type=Path)
    args = parser.parse_args()

    output_root = args.output_root.absolute()
    manifest = args.manifest.absolute()
    output_root.parent.mkdir(parents=True, exist_ok=True)
    manifest.parent.mkdir(parents=True, exist_ok=True)
    try:
        manifest.relative_to(output_root)
    except ValueError:
        pass
    else:
        raise StageError("manifest must live outside the runtime closure tree")

    store_root = args.store_root.resolve(strict=True)
    launcher = args.launcher.resolve(strict=True)
    manifest_builder = args.manifest_builder.resolve(strict=True)
    validate_launcher(launcher, readelf=args.readelf)
    source_tools = resolve_public_tools(args, store_root)
    roots = sorted({store_object_for(path, store_root) for path in source_tools.values()}, key=lambda p: p.as_posix())
    requisites = query_requisites(args.nix_store, roots, store_root)

    temp_root = Path(tempfile.mkdtemp(prefix=f".{output_root.name}.stage.", dir=output_root.parent))
    temp_manifest = Path(tempfile.mkstemp(prefix=f".{manifest.name}.stage.", dir=manifest.parent)[1])
    temp_manifest.unlink(missing_ok=True)
    try:
        copy_requisites(requisites, stage_root=temp_root, store_root=store_root, cp=args.cp)
        absolute_symlinks_rewritten = rewrite_absolute_symlinks(temp_root, store_root)

        patch_counts = {"dynamic_elf": 0, "rpath_entries_rewritten": 0, "needed_entries_rewritten": 0}
        for path in sorted((p for p in temp_root.rglob("*") if p.is_file() and not p.is_symlink()), key=lambda p: p.as_posix()):
            if not is_elf(path):
                continue
            counts = patch_dynamic_elf(
                path,
                stage_root=temp_root,
                store_root=store_root,
                patchelf=args.patchelf,
                readelf=args.readelf,
            )
            for key, value in counts.items():
                patch_counts[key] += value

        entrypoints = make_runtime_entrypoints(
            stage_root=temp_root,
            source_tools=source_tools,
            store_root=store_root,
            launcher=launcher,
            patchelf=args.patchelf,
        )
        private_resolution = validate_private_resolution(temp_root)
        manifest_digest, manifest_entries = build_manifest(temp_root, temp_manifest, manifest_builder)

        staging_metadata = {
            "schema": "tonepoet-reference-runtime-staging/v1",
            "store_root": str(store_root),
            "requisite_store_path_count": len(requisites),
            "requisite_store_paths": [str(path) for path in requisites],
            "absolute_symlinks_rewritten": absolute_symlinks_rewritten,
            "elf_patch_counts": patch_counts,
            "entrypoints": entrypoints,
            "private_dependency_resolution": private_resolution,
            "manifest": {
                "schema": "tonepoet-reference-runtime-closure/v1",
                "digest": manifest_digest,
                "entry_count": manifest_entries,
                "sha256": sha256_file(temp_manifest),
            },
        }
        metadata_bytes = (json.dumps(staging_metadata, indent=2, sort_keys=True) + "\n").encode()
        temp_metadata = temp_manifest.with_name(temp_manifest.name + ".staging.json")
        temp_metadata.write_bytes(metadata_bytes)

        final_metadata = manifest.with_name(manifest.name + ".staging.json")
        publish_generation(
            staged_root=temp_root,
            output_root=output_root,
            staged_manifest=temp_manifest,
            manifest=manifest,
            staged_metadata=temp_metadata,
            metadata=final_metadata,
            replace=args.replace,
        )

        print("schema=tonepoet-reference-runtime-staging/v1")
        print(f"root={output_root}")
        print(f"manifest={manifest}")
        print(f"manifest_digest={manifest_digest}")
        print(f"manifest_entries={manifest_entries}")
        print(f"requisite_store_paths={len(requisites)}")
        print(f"dynamic_elf={patch_counts['dynamic_elf']}")
        print(f"absolute_symlinks_rewritten={absolute_symlinks_rewritten}")
        return 0
    finally:
        if temp_root.exists():
            shutil.rmtree(temp_root, ignore_errors=True)
        temp_manifest.unlink(missing_ok=True)
        temp_manifest.with_name(temp_manifest.name + ".staging.json").unlink(missing_ok=True)


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except StageError as exc:
        print(f"stage_reference_runtime_closure: {exc}", file=sys.stderr)
        raise SystemExit(2)
