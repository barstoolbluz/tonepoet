# Metadata descriptor binding and unified native carrier authority — v27

## Scope

This corrective round addresses two remaining P0 defects in the metadata transaction design:

1. A read guard protected one opened carrier generation, but production parsers and subprocesses reopened the mutable user pathname after validation.
2. Native FLAC and DSF writers used format-specific mutation locks that did not conflict with the generic metadata reader/writer authority.

The correction keeps format-specific FLAC and DSF recovery journals, but moves their complete mutation windows under the same carrier-level read/write exclusion used by generic full-file transactions.

## Descriptor-bound reads

`MetadataReadAuthority` now owns:

- the canonical semantic carrier path;
- a stable parser path;
- the retained carrier descriptor;
- the shared carrier lock; and
- the private alias or snapshot directory that keeps the parser path valid.

See `src/db.rs:225-255`.

On Linux, the parser path is an extension-preserving private symlink to `/proc/<tonepoet-pid>/fd/<n>`. The descriptor is opened and checked against the pathname before the shared lock is established, checked again after lock acquisition, and retained until the authority is dropped. Reopening the parser path therefore duplicates the validated descriptor rather than resolving the user pathname. See `src/db.rs:6170-6301` and `src/db.rs:6510-6605`.

On platforms without Linux's cross-process `/proc/<pid>/fd` namespace, the parser path is a private point-in-time snapshot copied from the retained descriptor while shared authority is held. The API never silently returns the mutable original pathname. See `src/db.rs:6303-6402`.

The production reader inventory was changed to use `MetadataReadAuthority::read_path()` and keep the authority alive through the complete parse, decode, verification command, or asynchronous conversion pipeline. Representative paths include:

- in-process FFmpeg analysis: `src/tui/analyze.rs:52-66`;
- `flac`, `wvunpack`, and FFmpeg integrity verification: `src/tui/verify.rs:21-99`;
- bit comparison: `src/tui/bit_compare.rs:18-138`;
- Lofty metadata probes: `src/tui/probe.rs:599-632`;
- CUE image probes, metadata, artwork, and segment decoding: `src/convert/pipeline/materializer_cue.rs:94-137`;
- single-file conversion dispatch and retry: `src/convert/processor.rs:2536-2680` and `src/convert/processor.rs:3189-3221`.

Semantic source paths remain separate from parser paths. They continue to drive sidecar discovery, output naming, provenance, and user-visible diagnostics. Only byte-consuming operations receive the descriptor-bound path.

## Unified carrier exclusion

The carrier liveness sidecar now supports shared and exclusive lock modes. Process-local state mirrors those modes so a native writer cannot reenter against a live reader even on platforms whose advisory-lock behavior is process-scoped. See `src/db.rs:180-223` and `src/db.rs:3641-3756`.

`Database::acquire_native_metadata_write_authority()` acquires the exclusive form of the same identity-keyed lock used by readers and generic full-file transactions. It revalidates the opened carrier after lock acquisition and reconciles any abandoned generic transaction before returning. See `src/db.rs:6427-6520`.

Native FLAC common-write claims now own this exclusive carrier authority for the entire metadata or artwork mutation and release it only after the format-specific lock is retired. See `src/tui/probe.rs:2856-2945`.

Native DSF writes now acquire the exclusive carrier authority before the DSF `StoreFileLock`; both remain held through journal preparation, in-place publication or stream rewrite, durability, and recovery-artifact cleanup. See `src/dsf_tags.rs:803-834`.

Native recovery is run before the shared reader guard is acquired. If a writer starts in the gap, the shared acquisition fails closed; if a reader owns the shared lock, native recovery and mutation cannot enter. See `src/tui/probe.rs:9226-9243` and `src/dsf_tags.rs:246-268`.

External metadata mutators used by Tonepoet, including ReplayGain `loudgain` writes and `metaflac` copy operations, also retain the exclusive carrier authority through child-process completion.

## Regression coverage added

The new tests cover the reported races directly:

- a validated FLAC descriptor remains the parser input after the original pathname is renamed and replaced;
- a subprocess reads the retained descriptor path rather than the replacement pathname;
- a validated DSF descriptor remains the parser input after pathname replacement;
- a shared FLAC reader blocks a native FLAC mutation, and a second reader is permitted;
- a native FLAC writer paused immediately before the in-place metadata-region write blocks reader entry;
- a shared DSF reader blocks a native DSF mutation;
- a native DSF writer paused inside the journaled publication window blocks reader entry;
- the replacement inode can obtain an independent authority without changing what the retained reader consumes.

See `src/db.rs:9072-9155`, `src/tui/probe.rs:13528-13666`, and `src/dsf_tags.rs:3995-4114`.

## Static verification performed

- Audited every changed Rust file for delimiter balance, tabs, and trailing whitespace.
- Audited direct Lofty, FFmpeg, `flac`, `wvunpack`, `ffprobe`, `metaflac`, and `loudgain` read sites in production source.
- Audited native FLAC metadata and artwork entry points for common carrier authority ownership.
- Audited native DSF metadata, artwork, restore, and recovery entry points for common carrier authority ownership.
- Audited single-file and CUE conversion paths so descriptor aliases are used for byte reads without replacing semantic source paths.
- Ran `git diff --no-index --check` against the v26 source tree.

## Unexecuted gates

This environment does not contain `cargo`, `rustc`, or `rustfmt`. Consequently, the following required gates were not executed here:

- `cargo check --workspace --all-targets`;
- the full no-fail-fast Rust test suite;
- warning validation through the compiler;
- DSD qualification checks; and
- the live FLAC smoke test.

The implementation and tests are delivered for execution in the project toolchain environment. No claim is made that the unexecuted gates passed.
