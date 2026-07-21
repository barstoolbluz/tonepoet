# DSD Reference policy v12 bounded streamed-WAV qualification report

## Status

`sox_ng_14_8_0_1_v12` is an **unpromoted qualification candidate**. Runtime activation remains fail-closed until the mandatory pinned real-tool gate emits a passing schema-v12 report and release certification binds that exact report and candidate manifest.

## F6 correction

The pinned SoX-ng 14.8.0.1 WAV writer does not emit a streaming sentinel when an unseekable Float64 WAV payload crosses the 32-bit RIFF-size boundary. For the permanent sparse fixture with a 4 GiB + 8 byte audio payload, the writer emits RIFF size `58` and data size `8`; the data field is the exact modulo-2^32 truncation, while the RIFF field collapses to the header-only value. Those wrapped fields are not treated as sentinels and are not accepted as proof that a downstream consumer reads the complete stream.

Policy v12 therefore admits only streams whose complete predicted Float64 payload keeps both structural RIFF size and data size representable. The SoX-ng streamed header is 66 bytes and RIFF size excludes the first 8 bytes, so the controlling capacity is:

```text
max_audio_payload_bytes = 2^32 - 1 - 58 = 4,294,967,237
```

Planner admission computes `ceil(duration_ns * target_rate_hz / 1e9)`, reserves one additional output frame for duration quantization and resampler endpoint rounding, and multiplies by channels and 8 bytes per Float64 sample. Missing duration, arithmetic overflow, or a result above 4,294,967,237 bytes fails closed with `DSD-REF-P0-025`. The cap applies to every Reference cell because every Reference conversion uses this Float64 WAV transport for pre-terminal analyzer authority; Float64 RIFF/RF64 packaging uses a headerless raw stream and does not weaken the analyzer requirement.

Ordinary RIFF output retains its existing `DSD-REF-P0-018` preflight and error precedence. RF64, W64, and all other output targets are not allowed to bypass the analyzer-carrier cap.

## Accepted-edge and transition evidence

The v2 capacity-evidence contract permanently exercises the pinned producer at the arithmetic boundary rather than proving planner arithmetic and writer overflow in separate fixtures.

The largest frame-aligned admitted mono Float64 payload is 4,294,967,232 bytes (536,870,904 frames). Its structural RIFF size is 4,294,967,290, which is representable. The gate requires the pinned writer to emit that exact RIFF field and the exact 4,294,967,232-byte data field; otherwise policy v12 cannot be promoted.

The immediately following frame-aligned payload is 4,294,967,240 bytes (536,870,905 frames). Its structural RIFF size is 4,294,967,298 and therefore cannot be represented in a 32-bit RIFF field. The gate requires admission to reject it with `DSD-REF-P0-025` and records the writer's actual defective header. It does not assume that this first rejected carrier is necessarily the first numerical field decrease. Instead, a contiguous ten-point frame-aligned scan from the accepted edge through the existing 4 GiB + 8 data-wrap witness locates and records the pinned writer's actual first RIFF-field wrap. This avoids encoding an inferred writer formula as evidence.

The complete boundary evidence is emitted through strongly typed serialization in the qualification harness and consumed through a `deny_unknown_fields` typed schema in the runtime release validator. Runtime validation checks the scan's arithmetic continuity, exact accepted edge, first rejected edge, planner outcomes, first observed RIFF-field decrease, and frozen data-wrap witness. The report hash still binds the exact observed fields.

## Qualification requirements

The commissioned v12 gate must execute the complete v11 matrix and additionally prove that:

1. the largest frame-aligned admitted carrier has exact, nonwrapped RIFF and data fields under the pinned SoX producer;
2. the immediately following frame-aligned carrier is rejected by admission with `DSD-REF-P0-025` and has an unrepresentable structural RIFF size;
3. a contiguous frame-aligned real-tool scan locates the pinned writer's first observed RIFF-field wrap without assuming its position;
4. the permanent 4 GiB + 8 sparse W64 fixture declares 536,870,913 mono Float64 frames, the pinned reader reports that exact count, and the producer emits frozen RIFF size `58` and modulo data size `8`;
5. wrapped fields are recorded as a reproduced defect, not a sentinel or successful transport claim;
6. compiled planner constants equal the manifest capacity contract;
7. missing duration and checked-arithmetic overflow fail closed; and
8. the embedded release validator rejects missing, malformed, unknown, discontinuous, or altered streamed-capacity evidence.

## Rewritten-file attribute contract

FFmpeg metadata and artwork rewrites use a same-directory temporary file followed by atomic replacement. The shared replacement primitive now snapshots the original regular file before the external mutator runs, rejects target identity or governed-attribute substitution, reapplies attributes to the rewritten file before publication, syncs the rewritten file, atomically replaces the target, syncs the parent directory, and verifies the published attributes.

The portable contract preserves permission state and access/modification timestamps. Unix builds additionally preserve uid/gid. Linux builds preserve the complete extended-attribute set, including POSIX ACL xattrs. Failure to preserve or verify any governed attribute aborts replacement. A substitution detected by either pre-publication identity check leaves the substituted target untouched.

## Future lift

The cap may be removed only by an append-only policy with a corrected, repinned SoX-ng toolchain and renewed closure and behavior attestation, or by a separately qualified transport whose completeness is sample-exact beyond the 4 GiB boundary. No v12 field may be reinterpreted in place.

## Inherited authority

All v11 runtime-bound metadata-mutator, production-route, container-preservation, sample-identity, W64-rejection, analyzer, terminal-bound, packaging, and source-front-end authority is inherited unchanged. V12 adds only the fail-closed streamed-WAV capacity boundary, its permanent boundary/defect evidence, and explicit attribute preservation for the existing FFmpeg rewrite primitive.
