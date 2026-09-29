# Reference runtime rooting and package staging

The DSD Reference qualification has two deliberately different deployment rootings. They do not share an identity token.

## Nix rooting

A Nix deployment executes the policy-owned binaries directly from `/nix/store`. The store path is stable across binary-cache consumers, so final qualification does **not** require a package staging manifest.

Run the build-host qualification with both package-rooting variables absent:

```bash
unset TONEPOET_REFERENCE_RUNTIME_CLOSURE_ROOT
unset TONEPOET_REFERENCE_RUNTIME_CLOSURE_MANIFEST_PATH

export TONEPOET_REQUIRE_TOOLS=1
cargo test --release --test dsd_reference_qualification \
  complete_p0_reference_qualification_report -- --nocapture
```

The report records `tonepoet-reference-nix-runtime-attestation/v1`. That identity is domain-separated from the package manifest identity and binds the immutable store rooting, the existing common/metadata closure fingerprints, and the exact six executables exercised by qualification: SoX-ng, FFmpeg, FFprobe, metaflac, wvtag, and AtomicParsley.

A half-configured package environment still fails closed: the runtime-closure root and manifest variables must either both be absent (Nix) or both be present (staged package).

## Staged package rooting

`.deb`/`.rpm` users do not have the qualified `/nix/store` paths. Build the package-private runtime tree **before** qualification and before the manifest is generated.

From the flake development shell, where the five policy-owned activation variables already name the qualified Nix tools:

```bash
nix run .#stage-reference-runtime -- \
  "$PWD/package-stage/reference-runtime" \
  "$PWD/package-stage/reference-runtime-closure.json"
```

The stager:

1. queries the Nix requisites of the six executable entry points (FFprobe is the sibling of the qualified FFmpeg);
2. copies those store objects under the private tree's `nix/store/` subtree without changing the source store;
3. rewrites absolute Nix-store symlinks to relative links inside the private tree;
4. rewrites ELF RPATH/RUNPATH and absolute `DT_NEEDED` Nix paths to `$ORIGIN`-relative private paths while preserving RPATH-vs-RUNPATH semantics;
5. marks dynamic objects `NODEFLIB` so the host's default library directories cannot satisfy a missing dependency;
6. installs static public launchers in `bin/`. Each launcher invokes the copied Nix glibc loader explicitly with `--inhibit-cache`, so the payload's original absolute `PT_INTERP` is inert and the host `ld.so.cache` is not authority;
7. asks each private loader to list its dependency resolution and rejects any resolved library outside the staged tree, then runs each tool's version probe through the same launcher;
8. only after those mutations and checks, runs `build_reference_runtime_closure_manifest.py` over the final bytes.

The tool publishes via a same-filesystem temporary tree. A failure before publication leaves the previous tree untouched and publishes no manifest for partial bytes. `--replace` is required to replace an existing tree.

The adjacent `*.staging.json` file is review/provenance metadata; it is not runtime identity. The manifest remains the package qualification authority.

### Package qualification

Bind qualification to the staged entry points, not the original Nix paths:

```bash
root="$PWD/package-stage/reference-runtime"
manifest="$PWD/package-stage/reference-runtime-closure.json"

export TONEPOET_REFERENCE_RUNTIME_CLOSURE_ROOT="$root"
export TONEPOET_REFERENCE_RUNTIME_CLOSURE_MANIFEST_PATH="$manifest"
export TONEPOET_REFERENCE_SOX_PATH="$root/bin/sox"
export TONEPOET_REFERENCE_FFMPEG_PATH="$root/bin/ffmpeg"
export TONEPOET_REFERENCE_METAFLAC_PATH="$root/bin/metaflac"
export TONEPOET_REFERENCE_WVTAG_PATH="$root/bin/wvtag"
export TONEPOET_REFERENCE_ATOMIC_PARSLEY_PATH="$root/bin/AtomicParsley"
export PATH="$root/bin:$PATH"

export TONEPOET_REQUIRE_TOOLS=1
cargo test --release --test dsd_reference_qualification \
  complete_p0_reference_qualification_report -- --nocapture
```

Qualification re-attests the full staged manifest before Reference execution and records `tonepoet-reference-runtime-closure/v1` with its digest and entry count. The package must ship those qualified bytes unchanged. Any later `strip`, `patchelf`, RPATH/interpreter edit, symlink rewrite, or other closure mutation invalidates the qualification and requires rebuild + requalification.

### Packaging contract

The runtime tree is prefix-independent: relocation changes neither the manifest bytes nor the rewritten `$ORIGIN` relationships. A package may install the complete tree at a package-private prefix of its choice, but its TonePoet activation wrapper must point the five `TONEPOET_REFERENCE_*_PATH` variables and the two runtime-closure variables at that installed tree/manifest.

Preserve regular-file bytes, executable modes, and symlink text when transferring the staged tree into the final `.deb` or `.rpm`. Do not invoke the private `.tonepoet-runtime/payload/*` files directly; `bin/*` is the supported executable surface.

The stager's loader-list check proves the normal dynamic-link dependency graph resolves inside the private tree. The mandatory real-tool Reference qualification remains the final proof for runtime-loaded codecs/resources and behavior exercised by TonePoet.
