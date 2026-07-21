# DSD Reference policy v11 runtime-bound metadata-mutator qualification report

## Status

`sox_ng_14_8_0_1_v11` is an **unpromoted qualification candidate**. Runtime activation remains fail-closed until the mandatory pinned real-tool gate emits a passing schema-v11 report and release certification binds that exact report and candidate manifest.

## Runtime identity correction

Policy v10 qualified the exact production metadata routes and recorded canonical executable paths, SHA-256 digests, and reported versions for `metaflac`, `wvtag`, and AtomicParsley. It did not require production runtime to prove that those certified executables were the binaries the runner would resolve and execute.

Policy v11 completes that identity chain. A passing commissioned report must contain `runtime_metadata_mutator_binding` with the canonical contract below:

- certified identities come from `toolchain.production_metadata_mutators`;
- each mutator is bound to its policy-owned package/store path at build time;
- the packaged activation path, compiled store path, runner-resolved path, and certified canonical path must be identical;
- `ProcessorConfig.tool_paths` and ambient `PATH` may not resolve any executable other than the certified canonical path;
- execution authority is the exact canonical path plus executable SHA-256;
- path, executable digest, reported version, and closure identity are reverified immediately before metadata mutation; and
- the three mutator identities are serialized in `ReferenceToolchainEvidence` and included in `execution_fingerprint_v1` for every Reference output whose metadata stage is enabled.

The `RealToolRunner` bound-execution path resolves the configured executable, rejects path drift, re-hashes the certified canonical path, rejects content drift, and spawns that exact path. The `ToolRunner` default fails closed for bound execution, so an alternate production runner must explicitly implement the same authority rather than silently falling back to ordinary execution. The production metadata stage wraps the ordinary runner with this authority for FFmpeg, `metaflac`, `wvtag`, and AtomicParsley. Therefore a configured override or changed ambient `PATH` fails before mutation, and a binary replacement at the certified path fails before execution.

## Qualification requirements

The commissioned v11 gate must execute the complete v10 production metadata matrix and additionally prove that:

1. the qualified mutator canonical paths equal the corresponding policy-owned store executables;
2. the runtime binding contract is emitted as `passed` in the machine report;
3. the embedded release validator rejects missing or altered runtime-binding fields;
4. production attestation compares the certified report identities with packaged activation, compiled store, and runner resolution;
5. production mutation revalidates path, digest, version, and closure immediately before work; and
6. per-output execution authority changes when any certified mutator identity changes.

## Scope

The v11 correction changes only runtime authority for the v10-qualified authoritative tag mutation routes. It does not broaden the qualified metadata surface. Artwork embedding and ReplayGain remain outside the F5 qualification claim. W64 metadata mutation remains rejected under `DSD-REF-P0-024`.

## Inherited authority

All v10 production-route, container-preservation, sample-identity, W64-rejection, analyzer, terminal-bound, packaging, and source-front-end authority is inherited unchanged. V11 adds the certified-binary-to-runtime-execution binding and its per-output authority only.
