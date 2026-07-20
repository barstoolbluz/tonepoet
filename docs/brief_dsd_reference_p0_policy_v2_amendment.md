# DSD Reference P0 Policy v2 Amendment

**Date:** 2026-07-19  
**Authority:** This amendment corrects the immutable command and qualification authority of `docs/brief_dsd_reference_p0_implementation.md`. It narrows only the predictive-compressed DST source-rate/channel cells whose required independent-oracle evidence is absent; all other P0 product boundaries remain unchanged.

## 1. New immutable policy identity

The corrected policy identifier is:

```text
sox_ng_14_8_0_1_v2
```

`sox_ng_14_8_0_1_v1` remains a historical identifier and must never be reinterpreted as this corrected contract. New native-v2 settings default to v2. Runtime planning does not execute v1 under this implementation because no complete v1 release qualification artifact was published.

The source-controlled v2 manifest is a `qualification_candidate`, not a release claim. Runtime attestation accepts only `qualified_release`. Promotion is permitted only after the complete build, workspace, clippy, and commissioned real-tool gates pass unchanged and produce the machine-readable qualification report. The candidate manifest contains an empty release-certification slot. Promotion must atomically replace the placeholder certification report, preserve the exact candidate manifest at the policy-owned snapshot path, bind both its SHA-256 recorded by the gate and the installed report SHA-256, and change status to `qualified_release`. Runtime validates those bindings before any other attestation. Changing candidate cells or command behavior after qualification requires another append-only policy identity.

## 2. WavPack Int24 terminal preservation

For `WavPackNative` with terminal `Int24`, append these exact FFmpeg arguments after `-c:a wavpack` and before `-compression_level`:

```text
-bits_per_raw_sample 24
```

This is required to preserve the terminal 24-bit sample contract with the qualified FFmpeg closure. The argument is behavior- and transcript-bearing and is qualified only under v2.

## 3. Compressed DST evidence boundary

The available independent-oracle compressed-DST corpus qualifies predictive decoding only for stereo DSD64. It contains no predictive-compressed mono oracle at DSD64 and no predictive-compressed mono or stereo oracle at DSD128 or DSD256. Standards-literal `DSTCoded=0` fixtures qualify container parsing, geometry, byte ordering, and raw-frame materialization, but do not qualify predictive compressed decoding.

Therefore v2 admits:

- native uncompressed DSF and DSDIFF/DSD at DSD64, DSD128, and DSD256, mono and stereo;
- DSDIFF/DST and SACD/DST only for stereo DSD64;
- SACD/DSD at DSD64, DSD128, and DSD256, mono and stereo.

Any predictive DSDIFF/DST or SACD/DST cell outside stereo DSD64 fails before decode with:

```text
DSD-REF-P0-021: Reference policy sox_ng_14_8_0_1_v2 qualifies predictive compressed DST only for stereo DSD64. Mono DSD64 and all DSD128/DSD256 predictive-DST cells remain unavailable because no matching independent-oracle corpus is present. Use an uncompressed DSF/DSDIFF source, decode with an independently verified tool outside Reference, or wait for a later immutable policy.
```

No `DSTCoded=0` fixture may be represented as evidence for predictive compressed decoding. `DSD-REF-P0-020` remains assigned to the existing managed-destination authority error; the new compressed-DST rejection is append-only as `DSD-REF-P0-021`.

## 4. Release qualification architecture

The mandatory real-tool qualification must execute the exact `PlannedExecutionStep` sequence returned by `plan_reference_dsd()`. It may create deterministic source/carrier fixtures, but it must not independently reconstruct render, measurement, deferred terminal, or package argv.

The strict loudnorm parser, conservative `Q + E` arithmetic, deferred gain binding, signed-zero scan, and post-final ceiling check used by qualification must be the same production functions used by the executor.
