# R3 qualification rooting and staging — delivery notes

Date: 2026-09-29  
Supplied base: `main @ e8095f3` (the supplied source archive contains no `.git` directory)

## Outcome 1 — Nix qualification

`complete_p0_reference_qualification_report` no longer treats absence of the staged-package manifest as an unconditional error. The production manifest attestation remains strict: a root without a manifest or a manifest without a root still fails.

When both package variables are absent, qualification must prove the `nix-store` rooting. The report emits a domain-separated `tonepoet-reference-nix-runtime-attestation/v1` digest built from:

- canonical `/nix/store` rooting;
- the existing common runtime closure fingerprint;
- the existing metadata-mutation closure fingerprint; and
- store path, executable SHA-256, and reported version for all six executable bytes exercised by qualification (SoX-ng, FFmpeg, FFprobe, metaflac, wvtag, AtomicParsley).

Staged-package qualification still emits `tonepoet-reference-runtime-closure/v1`, and candidate execution must report the exact same manifest digest observed by qualification preflight. Evidence from one rooting therefore cannot silently satisfy the other.

No promoted v18 evidence/report/certification was hand-edited. A real build-host requalification must produce it.

## Outcome 2 — package-private runtime staging

Added:

- `tools/reference_runtime_launcher.c` — static Linux entry-point launcher;
- `tools/stage_reference_runtime_closure.py` — repeatable/fail-closed Nix closure staging tool;
- `tools/test_stage_reference_runtime_closure.py` — target-free transformation/publication tests;
- `nix run .#stage-reference-runtime` — pinned Linux staging entry point;
- `docs/packaging/reference-runtime-staging.md` — build-host/package integration contract.

The stager copies the Nix requisites under a package-private `nix/store/` subtree, deliberately breaks source hardlinks before path-dependent mutations, rewrites absolute store symlinks and ELF lookup authority into the private tree, disables default host-library lookup, verifies the post-patch RPATH/RUNPATH tag semantics and `DF_1_NODEFLIB`, validates private-loader dependency resolution and public version probes, and only then invokes the existing manifest builder. The source Nix store is never modified.

The payloads retain their original Nix `PT_INTERP` bytes, but those bytes are not execution authority in the package: only static public launchers are invoked, and they explicitly execute copied private loaders with `--inhibit-cache`. This avoids inventing a non-existent relocatable `PT_INTERP` convention while keeping the supported public runtime prefix-independent.

Publication is temp-first. Replacing an existing generation requires `--replace`; ordinary publication failures roll back root, manifest, and staging metadata to the prior generation.

## Validation performed here

This environment does not contain Rust/Cargo, Nix, or patchelf, so the root integration test, flake evaluation, real 374-store-path closure staging, and Reference requalification cannot be executed here. Those are build-host gates, not claims in this bundle.

Performed locally:

- supplied outer `SHA256SUMS`: PASS for brief, blocked-evidence note, and source archive;
- `python3 -m py_compile tools/stage_reference_runtime_closure.py tools/test_stage_reference_runtime_closure.py`: PASS;
- `python3 tools/test_stage_reference_runtime_closure.py`: **12/12 PASS**, including hardlink separation, relocation-stable manifest identity, and rollback after injected publication failure;
- `tools/reference_runtime_launcher.c` compiled with the flake's hardening/static-link flags: PASS;
- `readelf` on that launcher: no `PT_INTERP`; stricter stager launcher validation also finds no dynamic `DT_NEEDED`: PASS;
- disposable explicit-loader smoke (`launcher -> private loader -> payload`): PASS;
- GCC `-fanalyzer` on the launcher: PASS;
- changed-file trailing-whitespace audit: PASS;
- existing manifest builder remains unchanged and fail-closed on absolute/escaping symlinks.

The uncompiled Rust change is confined to `tests/dsd_reference_qualification.rs`; no production Rust source was changed. The build host must run the commands in `docs/packaging/reference-runtime-staging.md`, then run the normal workspace/gate suite and final real-tool qualification. Promoted evidence stays stale until that succeeds.
