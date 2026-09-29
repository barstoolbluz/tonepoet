# R3 qualification rooting and staging — Corrective R2 delivery notes

Date: 2026-09-29  
Parent: `TonePoet_R3_Qualification_Rooting_And_Staging_Corrective_R1_2026-09-29.tar.gz`  
Parent SHA-256: `77ddb0b0dbe3fb9d5287b5f495aac003909d24d76a006f2b125a3286cfcbf10d`

## Correction

Corrective R1 passed the manifest-visible payload symlink pathname directly to the copied glibc
loader. glibc therefore derived `$ORIGIN` for the main executable from
`.tonepoet-runtime/payload`, not from the executable's real copied location under
`nix/store/<object>/bin`. A payload whose rewritten search path was correctly
`$ORIGIN/../lib` could consequently fail to resolve its private libraries.

Corrective R2 changes only the launcher behavior required to fix that error:

- construct the same `.tonepoet-runtime/loaders/<tool>` and `.tonepoet-runtime/payload/<tool>`
  mapping paths;
- resolve each mapping with `realpath(..., NULL)`;
- pass the canonical copied payload pathname to the private loader in both normal and `--list`
  modes;
- execute the canonical copied loader pathname;
- retain `--argv0 <tool>` so the child observes the intended public executable name.

The symlinks remain in the staged tree and remain manifest-visible. `origin_relative()` and all
ELF relocation logic are unchanged.

## Focused regression

The existing twelve staging tests were left unchanged. One test was added that builds a small
`ffmpeg` payload at `runtime/nix/store/test-tool/bin/ffmpeg`, a required `libx.so` at
`runtime/nix/store/test-tool/lib/libx.so`, and links the payload with
`RUNPATH=$ORIGIN/../lib`. It installs the same payload/loader symlink layout as the production
stager and requires both:

- `runtime/bin/ffmpeg` to execute successfully and print `x=42`; and
- `TONEPOET_REFERENCE_LAUNCHER_LIST=1 runtime/bin/ffmpeg` to succeed and resolve `libx.so`
  through the staged private path.

A scratch discrimination run against the unmodified R1 launcher reproduces exit 127 in both
modes. The same tree with R2 exits 0 in both modes.

## Local validation

- Python byte-compilation: PASS.
- Staging suite: **13/13 PASS**.
- Launcher compiled with the exact production flags from `flake.nix`: PASS.
- `-Wall -Wextra -Werror`: PASS.
- No launcher `PT_INTERP`: PASS.
- No launcher `DT_NEEDED`: PASS.
- GCC `-fanalyzer`: PASS.
- R1 -> R2 changed source paths: exactly two.
- Minimal patch `git apply --check` against untouched R1: PASS.
- Re-extracted full source candidate reruns the staging suite: **13/13 PASS**.

This environment still has no Cargo/Rust, Nix, `nix-store`, or patchelf. The real closure build,
six-entry-point private-resolution validation, staged-package Reference qualification, normal
workspace/gate suite, and promoted-evidence refresh are therefore intentionally not claimed.

## Scope

Outcome 1's Nix-rooting qualification is unchanged. The staged-package manifest contract is
unchanged. The manifest builder is unchanged. Publication/rollback is unchanged. No production
Rust source changed in this corrective pass.
