#!/usr/bin/env python3
from __future__ import annotations

import importlib.util
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest import mock

MODULE_PATH = Path(__file__).with_name("stage_reference_runtime_closure.py")
spec = importlib.util.spec_from_file_location("stage_reference_runtime_closure", MODULE_PATH)
assert spec is not None and spec.loader is not None
stage = importlib.util.module_from_spec(spec)
spec.loader.exec_module(stage)


class StagingPureTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        root = Path(self.tmp.name)
        self.store = root / "source" / "nix" / "store"
        self.stage = root / "stage"
        self.store.mkdir(parents=True)
        self.stage.mkdir()

        (self.store / "aaaa-tool" / "bin").mkdir(parents=True)
        (self.store / "aaaa-tool" / "lib").mkdir()
        (self.store / "aaaa-tool" / "lib" / "libx.so").write_bytes(b"lib")
        (self.store / "aaaa-tool" / "bin" / "tool").write_bytes(b"elf")

        staged = self.stage / "nix" / "store" / "aaaa-tool"
        (staged / "bin").mkdir(parents=True)
        (staged / "lib").mkdir()
        (staged / "lib" / "libx.so").write_bytes(b"lib")
        (staged / "bin" / "tool").write_bytes(b"elf")
        self.elf = staged / "bin" / "tool"

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def test_nix_rpath_becomes_origin_relative(self) -> None:
        source_lib = self.store / "aaaa-tool" / "lib"
        rewritten = stage.rewrite_search_entry(
            str(source_lib), elf=self.elf, stage_root=self.stage, store_root=self.store
        )
        self.assertEqual(rewritten, "$ORIGIN/../lib")

    def test_non_nix_absolute_rpath_is_rejected(self) -> None:
        with self.assertRaises(stage.StageError):
            stage.rewrite_search_entry(
                "/usr/lib", elf=self.elf, stage_root=self.stage, store_root=self.store
            )

    def test_existing_origin_escape_is_rejected(self) -> None:
        with self.assertRaises(stage.StageError):
            stage.rewrite_search_entry(
                "$ORIGIN/../../../../../../usr/lib",
                elf=self.elf,
                stage_root=self.stage,
                store_root=self.store,
            )

    def test_absolute_symlink_is_rewritten_inside_private_store(self) -> None:
        link = self.stage / "nix" / "store" / "aaaa-tool" / "bin" / "libx"
        link.symlink_to(self.store / "aaaa-tool" / "lib" / "libx.so")
        count = stage.rewrite_absolute_symlinks(self.stage, self.store)
        self.assertEqual(count, 1)
        target = os.readlink(link)
        self.assertFalse(os.path.isabs(target))
        self.assertEqual(link.resolve(), self.stage / "nix" / "store" / "aaaa-tool" / "lib" / "libx.so")

    def test_absolute_symlink_outside_store_is_rejected(self) -> None:
        link = self.stage / "nix" / "store" / "aaaa-tool" / "bin" / "escape"
        link.symlink_to("/etc/passwd")
        with self.assertRaises(stage.StageError):
            stage.rewrite_absolute_symlinks(self.stage, self.store)

    def test_copy_requisites_breaks_source_hardlinks_before_path_dependent_patching(self) -> None:
        source_object = self.store / "bbbb-hardlinks"
        (source_object / "lib" / "nested").mkdir(parents=True)
        first = source_object / "lib" / "libsame.so"
        second = source_object / "lib" / "nested" / "libsame.so"
        first.write_bytes(b"same-elf-bytes")
        os.link(first, second)
        self.assertEqual(first.stat().st_ino, second.stat().st_ino)

        copied_stage = Path(self.tmp.name) / "hardlink-stage"
        stage.copy_requisites(
            [source_object],
            stage_root=copied_stage,
            store_root=self.store,
            cp="cp",
        )
        copied_first = copied_stage / "nix" / "store" / source_object.name / "lib" / "libsame.so"
        copied_second = copied_stage / "nix" / "store" / source_object.name / "lib" / "nested" / "libsame.so"
        self.assertEqual(copied_first.read_bytes(), copied_second.read_bytes())
        self.assertNotEqual(copied_first.stat().st_ino, copied_second.stat().st_ino)

    def test_loader_list_parser_rejects_missing_dependency(self) -> None:
        with self.assertRaises(stage.StageError):
            stage.parse_loader_list("libmissing.so => not found\n")

    def test_loader_list_parser_extracts_absolute_resolution(self) -> None:
        parsed = stage.parse_loader_list(
            "linux-vdso.so.1 (0x1)\n"
            "libx.so => /tmp/runtime/libx.so (0x2)\n"
            "/tmp/runtime/ld-linux.so.2 (0x3)\n"
        )
        self.assertEqual(parsed, [Path("/tmp/runtime/libx.so"), Path("/tmp/runtime/ld-linux.so.2")])

    def test_existing_manifest_builder_accepts_relocation_with_identical_identity(self) -> None:
        link = self.stage / "nix" / "store" / "aaaa-tool" / "bin" / "libx"
        link.symlink_to("../lib/libx.so")
        first_manifest = Path(self.tmp.name) / "first-manifest.json"
        first_digest, first_entries = stage.build_manifest(
            self.stage, first_manifest, stage.MANIFEST_BUILDER
        )

        relocated = Path(self.tmp.name) / "different-prefix" / "runtime"
        relocated.parent.mkdir()
        shutil.copytree(self.stage, relocated, symlinks=True)
        second_manifest = Path(self.tmp.name) / "second-manifest.json"
        second_digest, second_entries = stage.build_manifest(
            relocated, second_manifest, stage.MANIFEST_BUILDER
        )
        self.assertEqual(first_digest, second_digest)
        self.assertEqual(first_entries, second_entries)
        self.assertEqual(first_manifest.read_bytes(), second_manifest.read_bytes())

    def test_generation_publication_requires_explicit_replace(self) -> None:
        parent = Path(self.tmp.name) / "publish"
        parent.mkdir()
        output = parent / "runtime"
        output.mkdir()
        (output / "old").write_text("old")
        manifest = parent / "manifest.json"
        metadata = parent / "manifest.json.staging.json"
        manifest.write_text("old-manifest")
        metadata.write_text("old-metadata")
        staged = parent / "staged"
        staged.mkdir()
        staged_manifest = parent / "staged-manifest"
        staged_metadata = parent / "staged-metadata"
        staged_manifest.write_text("new-manifest")
        staged_metadata.write_text("new-metadata")
        with self.assertRaises(stage.StageError):
            stage.publish_generation(
                staged_root=staged, output_root=output,
                staged_manifest=staged_manifest, manifest=manifest,
                staged_metadata=staged_metadata, metadata=metadata,
                replace=False,
            )
        self.assertEqual((output / "old").read_text(), "old")
        self.assertEqual(manifest.read_text(), "old-manifest")

    def test_generation_publication_rolls_back_if_manifest_publish_fails(self) -> None:
        parent = Path(self.tmp.name) / "rollback"
        parent.mkdir()
        output = parent / "runtime"
        output.mkdir()
        (output / "marker").write_text("old")
        manifest = parent / "manifest.json"
        metadata = parent / "manifest.json.staging.json"
        manifest.write_text("old-manifest")
        metadata.write_text("old-metadata")
        staged = parent / "staged"
        staged.mkdir()
        (staged / "marker").write_text("new")
        staged_manifest = parent / "staged-manifest"
        staged_metadata = parent / "staged-metadata"
        staged_manifest.write_text("new-manifest")
        staged_metadata.write_text("new-metadata")

        real_replace = os.replace

        def fail_manifest(source: os.PathLike[str] | str, destination: os.PathLike[str] | str) -> None:
            if Path(source) == staged_manifest and Path(destination) == manifest:
                raise OSError("injected manifest publication failure")
            real_replace(source, destination)

        with mock.patch.object(stage.os, "replace", side_effect=fail_manifest):
            with self.assertRaises(OSError):
                stage.publish_generation(
                    staged_root=staged, output_root=output,
                    staged_manifest=staged_manifest, manifest=manifest,
                    staged_metadata=staged_metadata, metadata=metadata,
                    replace=True,
                )
        self.assertEqual((output / "marker").read_text(), "old")
        self.assertEqual(manifest.read_text(), "old-manifest")
        self.assertEqual(metadata.read_text(), "old-metadata")
        self.assertFalse(any(parent.glob(".*.previous.*")))

    def test_generation_publication_replaces_root_manifest_and_metadata_together(self) -> None:
        parent = Path(self.tmp.name) / "replace"
        parent.mkdir()
        output = parent / "runtime"
        output.mkdir()
        (output / "marker").write_text("old")
        manifest = parent / "manifest.json"
        metadata = parent / "manifest.json.staging.json"
        manifest.write_text("old-manifest")
        metadata.write_text("old-metadata")
        staged = parent / "staged"
        staged.mkdir()
        (staged / "marker").write_text("new")
        staged_manifest = parent / "staged-manifest"
        staged_metadata = parent / "staged-metadata"
        staged_manifest.write_text("new-manifest")
        staged_metadata.write_text("new-metadata")
        stage.publish_generation(
            staged_root=staged, output_root=output,
            staged_manifest=staged_manifest, manifest=manifest,
            staged_metadata=staged_metadata, metadata=metadata,
            replace=True,
        )
        self.assertEqual((output / "marker").read_text(), "new")
        self.assertEqual(manifest.read_text(), "new-manifest")
        self.assertEqual(metadata.read_text(), "new-metadata")
        self.assertFalse(any(parent.glob(".*.previous.*")))


class LauncherOriginRegressionTests(unittest.TestCase):
    def test_launcher_resolves_payload_symlink_before_glibc_origin_expansion(self) -> None:
        cc = shutil.which("cc") or shutil.which("gcc")
        readelf = shutil.which("readelf")
        self.assertIsNotNone(cc, "focused launcher regression requires a C compiler")
        self.assertIsNotNone(readelf, "focused launcher regression requires readelf")
        assert cc is not None and readelf is not None

        with tempfile.TemporaryDirectory() as tmpdir:
            tmp = Path(tmpdir)
            runtime = tmp / "runtime"
            payload = runtime / "nix" / "store" / "test-tool" / "bin" / "ffmpeg"
            private_lib = runtime / "nix" / "store" / "test-tool" / "lib" / "libx.so"
            payload.parent.mkdir(parents=True)
            private_lib.parent.mkdir(parents=True)

            lib_source = tmp / "libx.c"
            lib_source.write_text("int x(void) { return 42; }\n")
            payload_source = tmp / "ffmpeg.c"
            payload_source.write_text(
                "#include <stdio.h>\n"
                "extern int x(void);\n"
                "int main(void) { int value = x(); printf(\"x=%d\\n\", value); return value == 42 ? 0 : 1; }\n"
            )

            subprocess.run(
                [cc, "-shared", "-fPIC", "-Wl,-soname,libx.so", "-o", str(private_lib), str(lib_source)],
                check=True,
                capture_output=True,
                text=True,
            )
            subprocess.run(
                [
                    cc,
                    "-o",
                    str(payload),
                    str(payload_source),
                    f"-L{private_lib.parent}",
                    "-Wl,--enable-new-dtags",
                    "-Wl,-rpath,$ORIGIN/../lib",
                    "-lx",
                ],
                check=True,
                capture_output=True,
                text=True,
            )

            program_headers = subprocess.run(
                [readelf, "-lW", str(payload)], check=True, capture_output=True, text=True
            ).stdout
            marker = "Requesting program interpreter: "
            interpreter_lines = [line for line in program_headers.splitlines() if marker in line]
            self.assertEqual(len(interpreter_lines), 1, program_headers)
            interpreter = Path(interpreter_lines[0].split(marker, 1)[1].rstrip("] "))
            self.assertTrue(interpreter.is_absolute(), interpreter)

            staged_loader = runtime / "nix" / "store" / "test-loader" / "lib" / interpreter.name
            staged_loader.parent.mkdir(parents=True)
            shutil.copy2(interpreter, staged_loader)

            launcher = tmp / "reference-runtime-launcher"
            subprocess.run(
                [
                    cc,
                    "-std=c11",
                    "-Os",
                    "-Wall",
                    "-Wextra",
                    "-Werror",
                    "-static",
                    "-D_FORTIFY_SOURCE=2",
                    "-fstack-protector-strong",
                    "-Wl,--build-id=sha1",
                    "-o",
                    str(launcher),
                    str(Path(__file__).with_name("reference_runtime_launcher.c")),
                ],
                check=True,
                capture_output=True,
                text=True,
            )

            public = runtime / "bin" / "ffmpeg"
            public.parent.mkdir(parents=True)
            shutil.copy2(launcher, public)
            public.chmod(0o755)

            payload_dir = runtime / ".tonepoet-runtime" / "payload"
            loader_dir = runtime / ".tonepoet-runtime" / "loaders"
            payload_dir.mkdir(parents=True)
            loader_dir.mkdir(parents=True)
            (payload_dir / "ffmpeg").symlink_to(os.path.relpath(payload, start=payload_dir))
            (loader_dir / "ffmpeg").symlink_to(os.path.relpath(staged_loader, start=loader_dir))

            normal = subprocess.run([str(public)], capture_output=True, text=True)
            self.assertEqual(normal.returncode, 0, normal.stdout + normal.stderr)
            self.assertEqual(normal.stdout, "x=42\n")

            list_env = os.environ.copy()
            list_env["TONEPOET_REFERENCE_LAUNCHER_LIST"] = "1"
            listed = subprocess.run([str(public)], env=list_env, capture_output=True, text=True)
            self.assertEqual(listed.returncode, 0, listed.stdout + listed.stderr)
            listing = listed.stdout + listed.stderr
            self.assertIn("libx.so", listing)
            listed_paths = stage.parse_loader_list(listing)
            self.assertTrue(
                any(path.resolve() == private_lib.resolve() for path in listed_paths),
                listing,
            )


if __name__ == "__main__":
    unittest.main()
