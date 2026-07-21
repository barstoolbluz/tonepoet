# DSD Reference policy v13 corrected streamed-WAV header qualification report

## Status

`sox_ng_14_8_0_1_v13` is an **unpromoted qualification candidate**. Runtime activation remains fail-closed until the mandatory pinned real-tool gate emits a passing schema-v13 report and release certification binds that exact report and candidate manifest.

## F7 correction

Policy v12 correctly rejected unseekable Float64 WAV carriers before the pinned SoX-ng writer's 32-bit RIFF fields could wrap, but it misidentified the streamed header as 66 bytes. The measured SoX-ng 14.8.0.1 Float64 WAV layout is:

```text
RIFF header            12 bytes
fmt chunk header        8 bytes
fmt body               18 bytes
fact chunk header       8 bytes
fact body               4 bytes
data chunk header       8 bytes
total                   58 bytes
```

The RIFF size field excludes the leading eight bytes, so the fixed non-audio contribution is 50 bytes, not 58. Policy v13 therefore freezes:

```text
stream_header_bytes      = 58
riff_size_overhead_bytes = 50
max_audio_payload_bytes  = 2^32 - 1 - 50
                         = 4,294,967,245
```

These are measured Float64-stream values. The separately observed 80-byte streamed Int24 header is outside the reachable Reference transport and is not introduced into the policy.

## Corrected boundary evidence

The largest whole mono Float64 frame payload admitted by v13 is 4,294,967,240 bytes, or 536,870,905 frames. Its structural RIFF size is 4,294,967,290, which remains representable in the 32-bit RIFF field.

The immediately following payload is 4,294,967,248 bytes, or 536,870,906 frames. Its structural RIFF size is 4,294,967,298 and is therefore rejected with `DSD-REF-P0-025` before execution.

The real-tool evidence contract scans nine contiguous frame-aligned payloads from the accepted edge through the existing 4 GiB + 8 byte data-wrap witness. At that witness, the sparse W64 source declares 536,870,913 mono Float64 frames and the pinned writer emits RIFF size `58` and data size `8`. Those wrapped fields remain defect evidence, not sentinels or proof of complete downstream consumption.

Planner admission still computes `ceil(duration_ns * target_rate_hz / 1e9)`, reserves one additional output frame for duration quantization and resampler endpoint rounding, and multiplies by channels and 8 bytes per Float64 sample. Missing duration, checked-arithmetic overflow, or a payload above 4,294,967,245 bytes fails closed with `DSD-REF-P0-025`.

## Append-only identity

Policy v12 and all of its JSON, candidate, certification, report, and derivation artifacts remain byte-identical. Its historical v2 evidence schema continues to encode the frozen 66-byte-header/58-byte-overhead contract. Policy v13 introduces a separate v3 evidence schema for the measured 58-byte header and 50-byte RIFF-size overhead; the active runtime validator accepts only the v13 schema and constants.

## Qualification requirements

The commissioned v13 gate must execute the complete v12 matrix and additionally prove that:

1. the streamed Float64 WAV header is exactly 58 bytes;
2. the compiled RIFF-size overhead is exactly 50 bytes;
3. the unaligned payload ceiling is exactly 4,294,967,245 bytes;
4. the largest frame-aligned admitted payload is 4,294,967,240 bytes and has exact RIFF/data fields;
5. the immediately following payload is rejected with `DSD-REF-P0-025`;
6. the contiguous nine-point scan reaches the 4 GiB + 8 witness and records the first observed RIFF-field decrease;
7. the witness remains RIFF size `58`, data size `8`, with no sentinel or consumer-completeness claim; and
8. the embedded release validator rejects v12 constants, malformed evidence, unknown fields, discontinuities, or altered edge relationships.

## Inherited authority

All v12 runtime-bound metadata-mutator, production-route, container-preservation, sample-identity, W64-rejection, analyzer, terminal-bound, packaging, source-front-end, fail-closed capacity-admission, and rewritten-file attribute authority is inherited unchanged. V13 changes only the measured streamed Float64 WAV header length, its RIFF-size contribution, the resulting capacity edge, and the evidence and policy identity that bind those values.
