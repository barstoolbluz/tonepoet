# DSD Reference policy v10 production-metadata qualification report

## Status

`sox_ng_14_8_0_1_v10` is an **unpromoted qualification candidate**. Runtime activation remains fail-closed until the mandatory pinned real-tool gate emits a passing schema-v10 report and release certification binds that exact report and candidate manifest.

## F5 evidence correction

Policy v9 correctly rejected unsafe W64 metadata mutation, but its certification language was broader than its exercised mechanism: a representative FFmpeg stream-copy remux was described as qualification of the production metadata mutator. Policy v10 replaces that surrogate authority with the shared production per-file implementation used by `apply_metadata`.

The commissioned matrix must now execute the exact production metadata route for every admitted non-W64 delivery cell:

- FFmpeg primary mutation: 160 cells (`wav_riff`, `wav_rf64`, `aiff_native`, `alac_m4a`)
- `metaflac` primary mutation: 180 FLAC cells
- `wvtag` primary mutation: 80 WavPack cells
- AtomicParsley freeform follow-up: all 20 ALAC/M4A cells

For each of the 420 admitted cells, qualification rechecks the target container contract after mutation and proves decoded-sample identity against the QPCM authority. The exact production discovery and mutation commands run under a closed environment containing only `LC_ALL=C`; tool paths, executable SHA-256 values, and reported versions are captured in the machine report.

The machine-readable scope is deliberately narrower than the whole metadata stage: it qualifies authoritative tag mutation with no artwork sidecar and no ReplayGain pass. Artwork embedding and ReplayGain are not claimed by this F5 evidence.

The 60 W64 cells traverse both production rejection implementations, not only the central predicate:

1. `plan_request_for_track`, proving rejection before work is planned; and
2. the shared production per-file metadata implementation used by `apply_metadata`, proving rejection before any mutator command or temporary rewrite is created.

Both boundaries must return `DSD-REF-P0-024` for every W64 matrix cell.

## RF64 preservation

Executing the exact production FFmpeg rewrite also makes container identity observable. A small RF64 input would otherwise be rewritten as ordinary RIFF when FFmpeg infers the muxer from a `.wav` temporary. The production builder now detects an RF64 source and emits `-rf64 always`. Qualification reruns the exact package probe after metadata mutation, so an RF64-to-RIFF downgrade fails the gate.

## Inherited authority

All v9 audio-delivery, analyzer, terminal-bound, packaging, W64-defect, and odd-byte RIFF evidence is inherited unchanged. V10 changes only the metadata-mutation evidence authority, the exact production-route matrix, and RF64 container-preservation enforcement.
