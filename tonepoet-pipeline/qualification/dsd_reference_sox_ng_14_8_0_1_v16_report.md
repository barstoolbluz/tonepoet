# DSD Reference policy v16 qualification report

Policy v16 is an append-only successor to v15. It preserves the commissioned DSP, analyzer, terminal, packaging, metadata, and capacity contracts and adds the F10 Wave64 structural-integrity correction.

## Required correction

- Parse Wave64 independently of SoX and require the root-declared extent to equal the physical file extent.
- Traverse every chunk exactly, validate 8-byte alignment and zero padding, reject duplicate/missing required chunks, and reject undeclared trailing bytes.
- Derive the exact frame authority from the structurally valid R64 data extent and require terminal QPCM to match it exactly.
- Reject malformed QPCM with `DSD-REF-P0-026` before metadata probing, packaging, or atomic publication.
- Require a complete FFmpeg `-xerror` traversal after structural acceptance.
- Characterize Int24, Float32, and Float64 W64 at every enabled rate and mono/stereo shape around the empirically observed smallest reachable nonzero input.
- Prove each at-boundary, leading-silence, and trailing-silence control remains nonzero after independent decode.
- Treat same-path W64 QPCM/package hashes as identity continuity only, never as independent packaging evidence.

## Status

`not_run`. Promotion remains fail-closed until the complete workspace, formatting, lint, pinned-tool, live-smoke, throughput, qualification, and certification gates pass in the exact declared closure.
- The required 60-cell W64 matrix includes a 96-exponent scan and a 256-point boundary-neighborhood bracket at `2^e / 510` resolution for each enabled rate/channel/depth cell.
