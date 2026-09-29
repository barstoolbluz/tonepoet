# TonePoet deterministic gate + portable Reference — source candidate complete

Date: 2026-09-28 EDT / 2026-09-29 UTC
Base supplied by brief: `apply/ssrc-wavpack-int24-r6 @ 3a8cb1a`
Problem 1 commit: `54f9e289f6fa0a51e8a0f60479baa03878435a75` (`bound persistent lease encoding deterministically`)

This file is the authoritative handoff for the source candidate in this bundle. The source work for the governing brief is complete. Release promotion is intentionally **not** complete because the final build-host qualification has not been run and must not be fabricated.

## Problem 1 — deterministic coordination descriptor

Committed in `54f9e28`.

- Reader safety limit remains exactly 1 MiB.
- Persistent writer uses compact schema 2 with lossless path interning.
- Schema 1 remains readable.
- Writer checks the complete encoded size before creating a staging pathname.
- Regressions cover the previous legitimate-growth case, schema-1 compatibility, and genuine schema-2 oversize rejection before publication.

Do not raise the limit, serialize the suite, retry the gate, or weaken fail-closed reading.

## Problem 2 — portable DSD Reference

### Portable external-runtime identity

Package mode is selected only when both are set:

- `TONEPOET_REFERENCE_RUNTIME_CLOSURE_ROOT`
- `TONEPOET_REFERENCE_RUNTIME_CLOSURE_MANIFEST_PATH`

The manifest schema is `tonepoet-reference-runtime-closure/v1`. It binds the package-private runtime tree by canonical relative path and exact member identity. Real directories, regular-file SHA-256 values, and exact relative symlink targets are all first-class members. Missing, changed, additional, malformed, unsupported-special-file, absolute-symlink, and escaping-symlink cases fail closed. The manifest itself must be a regular non-symlink file outside the hashed tree.

The helper is:

```bash
python3 tools/build_reference_runtime_closure_manifest.py \
  "$TONEPOET_REFERENCE_RUNTIME_CLOSURE_ROOT" \
  "$TONEPOET_REFERENCE_RUNTIME_CLOSURE_MANIFEST_PATH"
```

The builder scans before and after atomic manifest publication. Installation prefix is absent from the manifest digest.

When the portable manifest variables are absent, the existing immutable exact `/nix/store/...` binding remains the fail-closed Nix/dev fallback.

### Package activation contract

The repository contains no native `.deb` or `.rpm` recipe, so the package-builder integration point is deliberately a staging/activation contract rather than a new packaging framework.

A release package must stage a private Reference runtime tree containing every runtime member that can influence Reference output, including the selected external executables (including `ffprobe` beside the bound `ffmpeg`) and their package-private loader, libraries, plugins, and resources. Any `strip`, `patchelf`, interpreter, RPATH/RUNPATH, or equivalent mutation must occur **before** the manifest is built and before qualification.

The package activation wrapper/environment must set the closure root/manifest variables above plus exact package-private executable bindings:

- `TONEPOET_REFERENCE_SOX_PATH`
- `TONEPOET_REFERENCE_FFMPEG_PATH`
- `TONEPOET_REFERENCE_METAFLAC_PATH`
- `TONEPOET_REFERENCE_WVTAG_PATH`
- `TONEPOET_REFERENCE_ATOMIC_PARSLEY_PATH`

The runner's resolved executable must equal each package activation path. A random same-named executable from `$PATH` is not accepted. If the package uses PATH-based discovery internally, prepend only the package-private executable directory so runner resolution and activation authority converge on the same staged bytes.

The immutable Nix build/store strings remain embedded as build provenance; they are not used as the portable installation location. The promoted portable identity is relocation-invariant because package installation prefix/canonical runtime path is not hashed.

The active v18 qualification harness follows the same rule: it still records the Nix store strings as build provenance, but it does **not** require the staged executables to equal those store paths. Instead, every executable used by qualification (SoX-ng, FFmpeg, sibling `ffprobe`, `metaflac`, `wvtag`, and AtomicParsley) must resolve inside `TONEPOET_REFERENCE_RUNTIME_CLOSURE_ROOT`, whose complete manifest is attested before qualification begins.

### CPU-dispatch qualification

The promoted/common closure binds the admitted qualified set:

- loudness: `scalar`, `sse2`, `avx`
- certified peak: `scalar`, `avx`
- FFmpeg ceiling: `sse+sse2`
- libsoxr: `SOXR_USE_SIMD=0`

The per-run execution fingerprint still binds the actual production-selected loudness and certified-peak tier.

Qualification forces the real production selectors/kernels. The loudness qualification geometries are deliberately 1 channel for scalar, 2 for SSE2, and 4 for AVX; the 4-channel case is required because the production AVX loudness kernel otherwise performs no AVX arithmetic on a stereo block. Reference9 remains scalar-only; Standard/Fast exercise scalar and AVX using the production kernels.

### External dispatch/environment

Active Reference FFmpeg audio and decoded-sample/full-traversal authority paths prepend:

```text
-cpuflags sse+sse2
```

Reference audio subprocesses use a cleared environment containing:

```text
LC_ALL=C
SOXR_USE_SIMD=0
```

Metadata-only mutation/version/control probes use the separate locale-only environment and do not claim a libsoxr dispatch binding.
The release qualification environment-isolation probe explicitly verifies both `LC_ALL=C` and `SOXR_USE_SIMD=0`, while the locale-only probe paths remain separate.

Historical v16 qualification/evidence remains untouched.

## Qualification evidence state — intentionally unpromoted

The source candidate and active v18 policy changed. The checked-in promoted v18 evidence/report/certification remain on the previous closure on purpose. Do **not** hand-edit their hashes or copy candidate values into promoted evidence.

Final qualification must be run against the exact final staged external runtime tree after all packaging mutations. The final package must then ship those qualified runtime-tree bytes unchanged.

A build-host sequence is:

```bash
# 1. Build/stage the final package-private runtime tree first.
#    Perform strip/patchelf/interpreter/RPATH changes before the next command.

export TONEPOET_REFERENCE_RUNTIME_CLOSURE_ROOT=/absolute/path/to/final/stage/reference-runtime
export TONEPOET_REFERENCE_RUNTIME_CLOSURE_MANIFEST_PATH=/absolute/path/to/final/stage/reference-runtime-closure.json

python3 tools/build_reference_runtime_closure_manifest.py \
  "$TONEPOET_REFERENCE_RUNTIME_CLOSURE_ROOT" \
  "$TONEPOET_REFERENCE_RUNTIME_CLOSURE_MANIFEST_PATH"

# 2. Bind the exact staged executables that the package will activate.
export TONEPOET_REFERENCE_SOX_PATH="$TONEPOET_REFERENCE_RUNTIME_CLOSURE_ROOT/bin/sox"
export TONEPOET_REFERENCE_FFMPEG_PATH="$TONEPOET_REFERENCE_RUNTIME_CLOSURE_ROOT/bin/ffmpeg"
export TONEPOET_REFERENCE_METAFLAC_PATH="$TONEPOET_REFERENCE_RUNTIME_CLOSURE_ROOT/bin/metaflac"
export TONEPOET_REFERENCE_WVTAG_PATH="$TONEPOET_REFERENCE_RUNTIME_CLOSURE_ROOT/bin/wvtag"
export TONEPOET_REFERENCE_ATOMIC_PARSLEY_PATH="$TONEPOET_REFERENCE_RUNTIME_CLOSURE_ROOT/bin/AtomicParsley"
export PATH="$TONEPOET_REFERENCE_RUNTIME_CLOSURE_ROOT/bin:$PATH"

# 3. First run the small source gates requested by the brief.
cargo test -p tonepoet-true-peak
cargo test -p tonepoet-pipeline

# 4. Run the mandatory real-tool qualification and write fresh promoted artifacts.
export TONEPOET_REQUIRE_TOOLS=1
export TONEPOET_DSD_REFERENCE_EVIDENCE_PATH="$PWD/tonepoet-pipeline/qualification/dsd_reference_common_v18_evidence.json"
export TONEPOET_DSD_REFERENCE_REPORT_PATH="$PWD/tonepoet-pipeline/qualification/dsd_reference_common_v18_report.json"
export TONEPOET_DSD_REFERENCE_CERTIFICATION_PATH="$PWD/tonepoet-pipeline/qualification/dsd_reference_common_v18_certification.json"

cargo test --release --test dsd_reference_qualification \
  complete_p0_reference_qualification_report -- --nocapture
```

Use the project-qualified build host/Nix environment so the compiled build-provenance bindings are the intended qualified ones. The qualification test is expected to take roughly 46 minutes.

After qualification, verify that the package copies the already-qualified staged runtime tree without byte mutation. If packaging changes any closure member afterward, discard the qualification and rebuild/requalify.

## Validation performed in this environment

This environment has no `rustc`, `cargo`, `nix`, or `rustfmt`; no Rust compilation or Rust test execution is claimed.

Performed against the final source candidate:

- `git diff --check`: PASS.
- active v18 JSON parse: PASS.
- manifest-builder AST/smoke: PASS.
- manifest relocation test with files, real/empty directories, and a relative symlink: byte-identical manifest/digest across two prefixes, PASS.
- escaping-symlink manifest build: rejected, PASS.
- host FFmpeg 7.1.5 smoke accepted `-cpuflags sse+sse2` under `LC_ALL=C SOXR_USE_SIMD=0`: PASS. This is only a control-mechanism smoke test, not qualification evidence for the pinned build.
- focused target-free corrective source/policy audit: `96/96 PASS`; see `AUDIT_deterministic_gate_portable_reference_2026-09-28.txt`.
- direct target-free Problem-1 descriptor assertions (1 MiB bound, schema 2, schema-1 compatibility, pre-publication size check, and all three regressions): PASS.
- existing current-tree coordination verifier `tools/verify_concurrency_corrective_round6_r1.py`: PASS (broader coordination authority/admission coverage; it is not the descriptor-specific proof).

Two older broad static-audit scripts are stale on unrelated current-tree inventory and were **not** repaired as part of this brief:

- `tools/audit_concurrent_mutation_entrypoints.py` expects an old `script_supervisor.rs::spawn_launcher` launch shape.
- `tools/audit_test_coordination_isolation.py` flags four pre-existing permanent-delete tests in `src/tui/keybindings.rs`.

Neither touched file is changed by this corrective. Do not widen this brief to repair those audits unless their owners intentionally update their inventory.

## Still outstanding outside this brief

The R6 handoff still separately owes the 4,368-cell physical SSRC grid and one real WavPack decode comparison before that registry is promoted. Do not conflate that work with portable Reference qualification.
