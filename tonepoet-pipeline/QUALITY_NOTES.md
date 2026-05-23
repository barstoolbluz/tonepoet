# Quality notes

This v9 revision hardens the v8 release candidate around plugin extensibility and conservative built-in metadata behavior. Settings validation now checks intrinsic value relationships only; target-specific metadata and ReplayGain feasibility is decided by the selected registry plugins. This allows caller-provided tools to support custom formats, DSF/DFF metadata, or non-built-in ReplayGain targets without fighting built-in validation.

Key invariants:

- The crate remains pure: no probing, spawning, config reads, filesystem writes, or ambient state.
- Same request plus same registry yields the same topology, commands, work paths, cleanup paths, and finalization.
- Passthrough requires proven audio-content equality, copy-safe metadata policy, no post-processing, and copy-safe encoder settings.
- Metadata-strip, ReplayGain-only, source-MD5-only, and verify-only requests use deterministic stream-copy/post-processing paths where possible.
- Metadata pruning runs after registry plugin selection; a metadata step is skipped only when the selected plugin reports `MetadataDisposition::WritesRequestedPolicy`.
- Built-in FFmpeg metadata handling is target-aware. It does not claim DSF/DFF artwork/tag support or artwork support for containers that the command builder cannot write safely.
- Built-in loudgain support is target-aware. Unsupported targets fail through plugin selection rather than settings validation, so custom plugins can support additional targets.
- Passthrough and execute plans carry deterministic work paths plus cleanup paths for interruption-safe executors.
- FLAC source-MD5 tagging uses `metaflac --set-tag=SOURCE_AUDIO_MD5=...`; no ID3v2 write path exists for FLAC.
- FLAC verification uses real decode testing through `flac -t -s`; generic verification uses FFmpeg decode-to-null.

Compiler-backed checks still need to run in a Rust-equipped environment; see `CHECKS.md`.

## v10 hardening notes

- `StoreSourceAudioMd5` now models the completed work file as both the logical input and in-place output, so executor dependency graphs do not imply that `metaflac` mutates the original source file.
- Strip-only metadata rewrites no longer open the original source as an unnecessary second FFmpeg input; this keeps metadata-only rewrite plans smaller and avoids needless I/O.
- The crate version is bumped to `0.10.0` for this corrected bundle.

