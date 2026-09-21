# DSD Reference policy v17 qualification report

Policy v17 is the append-only source-lock successor to v16. It preserves the v16 DSP, analyzer, terminal, packaging, metadata, capacity, and exact Wave64 structural-integrity contracts without editing any v16 evidence bytes.

## Source-lock correction

- Bind SoX-ng 14.8.0.1 to revision `9ed22fb3d813d6c02f67c254e57d162cee014a30` and NAR `sha256-WxMirop+SH3RzUM57QBDjetp9Q4qhFjzcfo2OJHStcs=`.
- Preserve the v16 manifest, candidate, certification skeleton, report, checker, and v8 terminal source proof byte-for-byte as historical evidence.
- Bind the Phase-5 common candidate to the active `sox_ng_14_8_0_1_v17` policy identity while retaining its explicit v16 inherited-evidence digest.
- Re-run the complete real-tool gate before production promotion; a source-lock update is not accepted from static source inspection alone.

## Source audit

The v17 source-lock proof is `tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v17_source_lock_proof.md` (`sha256:4746ec2e7764d15e31d14b95ea39e8c7c45698b45b8e83129df77bfea28217ac`). Exact-content comparison found the terminal-arithmetic authorities `src/sox_ng.h` and `src/gain.c` byte-for-byte identical between the v16 and v17 pins. The new revision changes output-finalization behavior, including the sparse-file length accounting used by the Wave64 repair, so the Wave64 and full qualification gates remain mandatory.

## Qualification contract

The gate remains the v16-established complete qualification surface: workspace tests, formatting, lint, pinned-tool attestation, live smoke, throughput/deadline qualification, complete Reference qualification, the 60-cell exact-Wave64 matrix, and Phase-5 common-model release certification. The existing exact parser must continue to reject malformed Wave64 and accept only files whose declared root/data extents, chunk traversal, alignment, PCM format, and frame count match their physical contents and upstream authority.

## Status

`not_run`. Production remains fail-closed until the operator runs the gated qualification in the exact declared closure and installs the completed report/certification output.
