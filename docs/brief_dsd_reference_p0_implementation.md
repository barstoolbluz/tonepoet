# Implementation Brief: P0 Reference DSD→PCM

**Status:** implementation commission; design only — do not implement in this round  
**Date:** 2026-07-19  
**Authority:** `docs/brief_dsd_reference_p0_scope_and_commission.md` narrows and corrects `docs/brief_dsd_reference_design.md`; the accepted design remains authoritative where this brief does not explicitly narrow or correct it.  
**Implementation objective:** ship a qualified, fail-closed Auto/Reference DSD→PCM pathway for lossless singleton conversions, including qualified DST decode materialization, without implementing Manual workflows, lossy delivery, or programme-wide processing.

---

## 0. Non-negotiable scope boundary

Build only:

- Reference DSD→PCM reconstruction for DSD64, DSD128, and DSD256;
- native uncompressed DSF and DSDIFF/DSD sources;
- DSDIFF/DST and SACD DST sources through a qualified lossless decode-materialization front-end;
- mono and stereo policy cells only;
- singleton processing only;
- lossless targets `FlacNative`, `WavRiff`, `WavRf64`, `WavW64`, `AiffNative`, `WavPackNative`, and `AlacM4a`;
- terminal depths Int16, Int24, Float32, and Float64 only where the target/depth matrix in §2.5 permits them;
- gain modes `Reference`, `NativeLevel`, `Fixed`, and `NormalizePeak` with the semantics in §2.6;
- immutable policy `sox_ng_14_8_0_1_v1`;
- profile selection B1–B5, B4W as explicit Wideband, and B6 represented but unconditionally rejected;
- fixed-point level arithmetic, measurement/deferred binding, toolchain attestation, native-v2 fingerprints and manifest, migration, publication hardening, P0 UI/CLI/`:set`/preset exposure, and the complete qualification suite.

Do **not** build:

- Manual workflow files, schema, admission, object store, linter, executor, workflow discovery, workflow CLI flags, or the in-TUI workflow builder;
- Reference-front-end Opus, MP3, or AAC delivery;
- programme-wide gain authority for independent albums;
- process-before-split CUE/SACD programme conversion;
- multi-member publication gates;
- B6 execution;
- DSD512 or DSD1024 Reference handling;
- multichannel Reference output;
- PCM→DSD Reference policy or any redesign of the existing PCM→DSD DSP path.

A type reservation or neutral seam required by this brief is not permission to implement the deferred feature behind it.

---

## 1. Current-tree starting point

Every statement in this section describes the held source tree, not the desired implementation.

1. The held tree still pins `barstoolbluz/sox_ng` at revision `482801f768d5075fcef1ec81968fd57a85433627`, not the commissioned `324b8cf` revision (`flake.lock:142-160`). The implementation apply tree must be checked before work begins. P0 may not expose a qualified cell unless the resolved SoX-ng closure is the commissioned 14.8.0.1 build and passes the behavior probes in §9.
2. `PipelineSettings` has one flat `dsd` field, and the current `DsdSettings` mixes PCM→DSD filter/modulator controls with DSD→PCM lowpass and gain controls (`tonepoet-pipeline/src/settings.rs:20-60`, `tonepoet-pipeline/src/settings.rs:716-766`). Its shared sinc defaults are sized for PCM→DSD upsampling (`tonepoet-pipeline/src/settings.rs:781-812`).
3. All current DSD→PCM planning reaches one `PlanOperation::DsdToPcm`, either directly or through a WAV intermediate and final encode (`tonepoet-pipeline/src/plan.rs:613-621`, `tonepoet-pipeline/src/plan.rs:1161-1243`).
4. The current SoX builder produces one static argv that can combine rate, a single-cutoff sinc, gain/normalization, and generic dither (`tonepoet-pipeline/src/plugins.rs:1614-1688`, `tonepoet-pipeline/src/plugins.rs:1712-1738`). That shape cannot express a measured value binding a later gain command.
5. `ConversionPlan` currently carries static commands, and the executor runs them sequentially without a typed result-binding mechanism (`tonepoet-pipeline/src/plan.rs:298-327`, `src/convert/pipeline/track_executor.rs:256-292`, `src/convert/pipeline/track_executor.rs:342-424`). Command records already preserve resolved argv, output tails, status, and elapsed time (`src/convert/pipeline/track_executor.rs:491-569`).
6. `SourceInfo` carries format, rate, channels, and DSD representation but not the DSD-versus-DST encoding fact (`tonepoet-pipeline/src/source.rs:8-64`, `tonepoet-pipeline/src/source.rs:129-178`). The bridge already inspects DSD containers and keeps DSF, DSDIFF/DSD, and DSDIFF/DST in a companion record because the planner type cannot express that distinction (`src/convert/pipeline/plan_bridge.rs:426-458`, `src/convert/pipeline/plan_bridge.rs:501-533`).
7. The in-tree `sacd-rs` DST module is safe Rust, exposes DSD64/128/256 geometry, accepts legal channel counts 1–6, and documents byte-exact validation against `sacd_extract` for real stereo and six-channel SACD material (`crates/sacd-rs/src/dst/mod.rs:1-16`, `crates/sacd-rs/src/dst/decoder.rs:29-83`, `crates/sacd-rs/src/dst/decoder.rs:132-179`). Its public errors are typed rather than panic-based (`crates/sacd-rs/src/dst/mod.rs:47-119`).
8. The common DSD reader already converts DSF, DSDIFF/DSD, and DSDIFF/DST to canonical uncompressed DSD frames, checks `DSTC` when present, and can write decoded frames to uncompressed DSDIFF/DSD (`crates/sacd-rs/src/dsd_file/reader.rs:257-310`, `crates/sacd-rs/src/dsd_file/ops.rs:241-272`). The encoder verifies predictive candidates by decoding and comparing every source byte (`crates/sacd-rs/src/dst/encoder.rs:961-971`). These are reusable primitives; they are not yet the qualified planner/executor front-end.
9. Runtime custom tools are already rejected by the planner adapter; only registered built-ins map to runtime binaries (`src/convert/pipeline/planned_adapter.rs:35-46`). The runner resolves configured/PATH binaries and caches versions, but native-v2 attestation does not yet happen before rerun admission (`src/convert/pipeline/tool.rs:315-358`, `src/convert/pipeline/stages.rs:15627-15665`).
10. Output container identity is split between format, extension, and raw FFmpeg flags; WAV, RF64, and W64 are distinct product choices even though the planner format is WAV (`src/convert/pipeline/types.rs:543-548`, `src/convert/formats.rs:244-259`). P0 must resolve an exact typed target before planning.
11. The current manifest is version 1, stores the current settings fingerprint, and participates in rerun authority (`src/convert/pipeline/manifest.rs:12-54`, `src/convert/pipeline/rerun.rs:154-182`). Queue rows can persist full `PipelineSettings`, but older compatibility paths can omit them (`src/convert/queue.rs:198-203`, `src/convert/formats.rs:605-612`).
12. The orchestration layer already has descriptor-bound album publication authority and a sibling-directory assembly/rename path (`src/convert/pipeline/stages.rs:18776-18938`, `src/convert/pipeline/stages.rs:18974-19276`). Its whole-album and incremental manifest writes are currently nonfatal, which is not sufficient for native-v2 reuse authority (`src/convert/pipeline/stages.rs:19239-19253`, `src/convert/pipeline/stages.rs:20068-20079`).
13. The TUI currently exposes only DSD gain mode and fixed gain for DSD-source/PCM-target conversions and constructs a default `DsdSettings` before overriding those fields (`src/tui/draw_output.rs:164-179`, `src/tui/app.rs:3538-3542`, `src/tui/convert_actions.rs:371-400`). Presets are version 3 and do not persist DSD-source pathway/profile policy or selected container identity (`src/tui/presets.rs:29-75`, `src/tui/presets.rs:104-149`, `src/tui/app.rs:3235-3235`, `src/tui/app.rs:3522-3527`).
14. The production request path can carry full planner settings, while the legacy compatibility projection resets DSD settings to defaults and cannot represent the new controls (`src/convert/pipeline/unified_request.rs:27-81`, `src/convert/pipeline/unified_request.rs:499-580`). P0 controls must use the full-settings path.
15. Existing PCM→DSD behavior already has `DsdFilterPreset::{Auto, Sinc}` and an upsample/sinc/volume chain; P0 must preserve it under a direction-specific settings substructure rather than redesign it (`tonepoet-pipeline/src/plugins.rs:1559-1611`).

---

## 2. Exact P0 product contract

### 2.1 Qualified source front-ends

P0 admits these source facts only after stable source-content identity and probe facts are established from the same bytes:

| Source fact | P0 route |
|---|---|
| Uncompressed DSF | verified non-hard-linked source materialization, then native SoX-ng input |
| Uncompressed DSDIFF/DSD | verified non-hard-linked source materialization, then native SoX-ng input |
| DSDIFF/DST | qualified in-tree DST decode to canonical uncompressed DSDIFF/DSD, then native SoX-ng input |
| SACD area encoded as DSD | qualified per-track lossless extraction to canonical uncompressed DSDIFF/DSD, then native SoX-ng input |
| SACD area encoded as DST | qualified per-track DST extraction/decode to canonical uncompressed DSDIFF/DSD, then native SoX-ng input |

All admitted source routes require:

- DSD64, DSD128, or DSD256;
- known channel count of one or two;
- exact qualified front-end identity;
- exact source-content SHA-256 and probe digest;
- singleton scope under §2.2;
- a source/container/rate/channel cell present in the immutable qualification manifest.

A DST decoder that can technically handle six channels does not make multichannel Reference qualified. Channel counts 3–6 remain policy rejections.

### 2.2 Singleton authority

P0 supports only `ReferenceProgrammeScope::Singleton`.

An independent-file batch containing more than one member fails before rendering:

```text
DSD-REF-P0-012: Reference P0 supports singleton conversions only. Convert the
selected files one at a time as independent singletons with independent gain,
or wait for programme-wide Reference support.
```

A continuous CUE/SACD image that would be split before reconstruction fails before rendering:

```text
DSD-REF-P0-013: Reference P0 cannot split a continuous DSD programme before
reconstruction. This source must be processed as one programme before splitting;
wait for programme-wide Reference support. Already independent files may be
converted one at a time with independent gain.
```

The worker may not downgrade a dispatcher-authored multi-member context to singleton merely because it receives one member at a time. The existing batch context already carries a fresh attempt ID, expected count, grouping root, and source paths (`src/convert/pipeline/types.rs:120-176`); P0 uses those facts only to reject multi-member work, not to implement shared programme gain.

### 2.3 Reference profile matrix

Target rates are exactly:

```text
44100, 48000, 88200, 96000, 176400, 192000,
352800, 384000, 705600, 768000 Hz
```

#### Standard `profile = Reference`

| Source rate | 44.1k | 48k | 88.2k | 96k | 176.4k | 192k | 352.8k | 384k | 705.6k | 768k |
|---|---|---|---|---|---|---|---|---|---|---|
| DSD64 | B1 | B2 | B3 | B3 | B3 | B3 | B3 | B3 | B3 | B3 |
| DSD128 | B1 | B2 | error E88 | error E96 | B4 | B4 | B4 | B4 | B4 | B4 |
| DSD256 | B1 | B2 | error E88 | error E96 | B5 | B5 | B5 | B5 | B5 | B5 |
| DSD512 | error ERATE | error ERATE | error ERATE | error ERATE | error ERATE | error ERATE | error ERATE | error ERATE | error ERATE | error ERATE |
| DSD1024 | error ERATE | error ERATE | error ERATE | error ERATE | error ERATE | error ERATE | error ERATE | error ERATE | error ERATE | error ERATE |

#### Explicit `profile = Wideband`

| Source rate | 44.1k | 48k | 88.2k | 96k | 176.4k | 192k | 352.8k | 384k | 705.6k | 768k |
|---|---|---|---|---|---|---|---|---|---|---|
| DSD64 | error EW64 | error EW64 | error EW64 | error EW64 | error EW64 | error EW64 | error EW64 | error EW64 | error EW64 | error EW64 |
| DSD128 | error EWTARGET | error EWTARGET | error EWTARGET | error EWTARGET | B4W | B4W | B4W | B4W | B4W | B4W |
| DSD256 | error EWB6FIT | error EWB6FIT | error EWB6FIT | error EWB6FIT | error EWB6FIT | error EWB6FIT | error EB6 | error EB6 | error EB6 | error EB6 |
| DSD512 | error ERATE | error ERATE | error ERATE | error ERATE | error ERATE | error ERATE | error ERATE | error ERATE | error ERATE | error ERATE |
| DSD1024 | error ERATE | error ERATE | error ERATE | error ERATE | error ERATE | error ERATE | error ERATE | error ERATE | error ERATE | error ERATE |

The stable error texts are:

```text
E88 / DSD-REF-P0-006:
Reference policy sox_ng_14_8_0_1_v1 has no qualified target-limited profile
for {DSD128|DSD256} → 88.2 kHz. Choose 44.1/48 kHz, choose 176.4 kHz or
higher, or wait for a new policy.

E96 / DSD-REF-P0-007:
Reference policy sox_ng_14_8_0_1_v1 has no direct 96 kHz qualification for
{DSD128|DSD256}. Choose 48 kHz, choose 176.4 kHz or higher, or wait for a
new policy.

ERATE / DSD-REF-P0-003:
Reference policy sox_ng_14_8_0_1_v1 supports DSD64, DSD128, and DSD256 only.
Use a supported-rate source or wait for expanded-rate/Manual support.

EW64 / DSD-REF-P0-008:
No Wideband profile is defined for DSD64. Select the Reference profile.

EWTARGET / DSD-REF-P0-008:
DSD128 Wideband uses B4W and requires a target rate of at least 176.4 kHz.
Select the Reference profile or choose 176.4 kHz or higher.

EWB6FIT / DSD-REF-P0-008:
DSD256 Wideband uses B6, whose 140 kHz stopband edge cannot fit this target;
B6 is also unavailable under policy sox_ng_14_8_0_1_v1. Select Reference/B5.

EB6 / DSD-REF-P0-009:
B6 is represented but unqualified and unavailable under policy
sox_ng_14_8_0_1_v1. Select Reference/B5 or wait for a later immutable policy.
```

No local test flag, environment variable, downloaded table, or successful ad hoc command may turn an error cell into a supported cell under the same policy ID.

### 2.4 Corrected, frozen profile semantics

The SoX-ng `sinc` frequency argument is the transition's **−6 dB point**, not the passband edge. The commissioned 14.8.0.1 measurement closes this question.

For every explicit sinc profile:

```text
frequency_argument_hz = passband_edge_hz + transition_width_hz / 2
stopband_edge_hz      = passband_edge_hz + transition_width_hz
```

The frozen table is:

| Profile | Flat/passband edge | Transition width | `sinc` −6 dB frequency argument | Stopband begins | P0 status |
|---|---:|---:|---:|---:|---|
| B1 | integrated `rate -u` response | integrated | none | target Nyquist | supported candidate |
| B2 | integrated `rate -u` response | integrated | none | target Nyquist | supported candidate |
| B3 | 25,000 Hz | 10,000 Hz | 30,000 Hz | 35,000 Hz | supported candidate |
| B4 | 30,000 Hz | 15,000 Hz | 37,500 Hz | 45,000 Hz | supported candidate |
| B4W | 35,000 Hz | 15,000 Hz | 42,500 Hz | 50,000 Hz | explicit Wideband candidate |
| B5 | 48,000 Hz | 22,000 Hz | 59,000 Hz | 70,000 Hz | supported candidate |
| B6 | 88,200 Hz | 51,800 Hz | 114,100 Hz | 140,000 Hz | typed; rejected in v1 |

Qualification must measure the realized response, not merely compare argv. For B3, for example, it must prove unity through the 25 kHz passband, approximately −6.02 dB at 30 kHz, and the requested stopband behavior from 35 kHz. Equivalent profile assertions apply to B4, B4W, B5, and any future B6 policy.

### 2.5 Lossless target/depth matrix

The P0 policy intends to enable exactly these target/depth cells after tool-gated qualification:

| Target | Int16 | Int24 | Float32 | Float64 |
|---|---|---|---|---|
| `WavW64` | supported candidate | supported candidate | supported candidate | supported candidate |
| `WavRiff` | supported candidate | supported candidate | supported candidate | supported candidate |
| `WavRf64` | supported candidate | supported candidate | supported candidate | supported candidate |
| `FlacNative` | supported candidate | supported candidate | rejected | rejected |
| `AiffNative` | supported candidate | supported candidate | rejected | rejected |
| `WavPackNative` | supported candidate | supported candidate | rejected | rejected |
| `AlacM4a` | supported candidate | supported candidate | rejected | rejected |

A candidate becomes supported only when its exact policy cell is present in the generated immutable qualification manifest. P0 is not complete until every intended candidate either passes and is frozen supported or is reported as a release blocker; it may not silently disappear from UI/help.

Stable errors:

```text
DSD-REF-P0-010 (Int8):
Reference policy sox_ng_14_8_0_1_v1 has no qualified 8-bit terminal
realization. Choose 16-bit, 24-bit, Float32, or Float64 where supported.

DSD-REF-P0-010 (Int32):
Reference policy sox_ng_14_8_0_1_v1 has no qualified 32-bit integer terminal
realization. Choose 24-bit, Float32, or Float64 where supported.

DSD-REF-P0-011:
{target} does not support {depth} under Reference policy
sox_ng_14_8_0_1_v1. Choose a target/depth pair listed by the policy.
```

RIFF/WAV additionally requires a deterministic preflight proof that audio data plus all planned chunks remains within the policy's ordinary-RIFF limit. If that proof fails:

```text
DSD-REF-P0-018: The predicted RIFF/WAV output exceeds the qualified RIFF size
limit. Choose RF64, W64, or another supported lossless target.
```

WavPack must be lossless and non-hybrid. Arbitrary or conflicting raw container flags fail before planning:

```text
DSD-REF-P0-019: The selected output container does not match the canonical
Reference target or contains unrecognized output flags. Re-select the target.
```

### 2.6 Gain modes

All modes render with explicit −12.000000000 dB processing headroom. The pre-final true-peak measurement is always performed.

#### `Reference`

```text
requested_gain = 12.000000000 + 6.020599913
               = 18.020599913 dB
applied_gain   = min(requested_gain, maximum_ceiling_safe_gain)
```

This is the only mode allowed to reduce its requested gain automatically. It receives the full Reference label only with `profile = Reference` and a fully qualified source/target/toolchain cell.

#### `NativeLevel`

```text
applied_gain = 12.000000000 dB
```

The value is exact. If the conservative terminal bound cannot keep post-final true peak at or below −1.000000000 dBTP, fail before terminal realization; do not reduce the gain. Label: `Reference reconstruction; modified native-level gain`.

#### `Fixed`

```text
applied_gain = 12.000000000 + fixed_gain_db
```

`fixed_gain_db` is mandatory only in this mode and must be within −24.000000000 to +24.000000000 dB. The value is exact. If unsafe under the conservative terminal bound, fail before terminal realization. Label: `Reference reconstruction; modified fixed gain`.

#### `NormalizePeak`

The terminal SoX command uses `norm <target_dbfs>` with the configured target in −12.000000000 to 0.000000000 dBFS. This mode still records pre-final and post-final true peak but is not governed by, accepted under, or relabeled as the Reference −1 dBTP contract. Label: `Reference reconstruction; modified/unqualified peak normalization`.

For `Reference`, `NativeLevel`, and `Fixed`, post-final conservative true peak must be at or below −1.000000000 dBTP. There is no limiter, compressor, soft clipper, retry/backoff loop, second gain, or second dither realization.

Unsafe exact-gain error:

```text
DSD-REF-P0-016: The requested {native-level|fixed} gain cannot satisfy the
Reference −1.000000000 dBTP ceiling for this measured source and terminal
format. Reduce the fixed gain, choose Reference gain, or choose NormalizePeak
with its modified/unqualified semantics.
```

### 2.7 Deferred product errors

Manual is represented in the persisted pathway enum but always rejected in P0:

```text
DSD-REF-P0-001: Manual DSD workflows are not available in this P0 build.
Use Reference with a supported lossless target, or wait for Manual workflow
support.
```

Every new DSD→lossy request is rejected before render:

```text
DSD-REF-P0-002: Reference DSD reconstruction currently supports lossless
delivery only. Choose FLAC, RIFF/WAV, RF64, W64, AIFF, WavPack, or ALAC/M4A,
or wait for Reference-front-end Opus/MP3/AAC delivery.
```

Unknown DSD container/encoding:

```text
DSD-REF-P0-004: The DSD container or compression mode could not be identified
as DSF/DSD, DSDIFF/DSD, DSDIFF/DST, or a supported SACD area. Reference will
not guess the decoder path.
```

Unsupported channels:

```text
DSD-REF-P0-005: Reference policy sox_ng_14_8_0_1_v1 supports qualified mono
and stereo cells only. Select a mono/stereo track or wait for multichannel
qualification.
```

---

## 3. Frozen policy and toolchain authority

### 3.1 Policy identity

Implement an append-only private registry keyed by:

```rust
pub enum DsdReferencePolicyVersion {
    SoxNg14801V1,
}
```

The stable serialized key is `sox_ng_14_8_0_1_v1`.

The v1 registry freezes:

- commissioned SoX-ng 14.8.0.1 closure and exact revision `324b8cf`;
- FFmpeg analyzer/package/verification closure;
- in-process DST decoder and SACD extraction build identities;
- source container/encoding/rate/channel cells;
- profile definitions and corrected `sinc` rendering;
- −12 dB headroom, +6.020599913 dB compensation, −1 dBTP ceiling;
- exact measurement parser and one-sided analyzer bounds;
- terminal realization bounds per rate/depth/toolchain cell;
- target/depth/package cells;
- complete qualification-manifest digest.

Changing any of those facts requires a new policy ID. The qualification manifest is build/package evidence, never a mutable runtime feature gate.

Before implementing behavior against a new apply tree, verify:

1. `flake.lock` resolves `barstoolbluz/sox_ng@324b8cf`;
2. the executable reports 14.8.0.1 or the package identity otherwise binds the exact commissioned build;
3. the complete store/package closure identity matches the policy manifest;
4. the D1 response probe reproduces the commissioned passband/−6 dB/stopband profile.

If the apply tree still has the held tree's `482801f...` pin, updating and locking the input is part of P0-A. Do not claim Reference qualification on the old closure.

### 3.2 Direction-neutral versus DSD-specific machinery

Use direction-neutral forms at no material cost for:

- `DbNano`;
- policy-manifest identity and append-only registry mechanics;
- measurement IDs, parser dispatch, and typed measurement values;
- deferred argument binding;
- semantic plan hashing and execution fingerprints;
- source-content materialization;
- manifest version dispatch;
- publication journaling/durability;
- qualification report format.

Keep these P0-specific:

- DSD source classification;
- reconstruction profile resolution;
- DST/SACD front-end routing;
- Reference gain rules;
- terminal dither policy;
- DSD→PCM qualification matrix and labels.

Do not spend P0 effort designing the future PCM→DSD Reference evidence policy.

---

## 4. Exact P0 command and execution contract

### 4.1 Canonical notation

```text
IN      verified planner-owned uncompressed DSD materialization
R64     planner-owned 64-bit-float W64 render carrier
QPCM    planner-owned terminal-PCM W64 carrier
FINAL   planner-owned final-container staging file
SR      target PCM sample rate in decimal Hz
G       resolved gain, mandatory sign and exactly nine fractional digits
TW      transition width in decimal Hz
FC      corrected −6 dB transition-center frequency in decimal Hz
```

All commands use `LC_ALL=C`, separate argv tokens, no shell, and policy-owned executable identities. Every SoX command includes global `-D` so implicit dither cannot be inserted. Reference never uses `-G` or `-R`.

### 4.2 Optional qualified DST/SACD front-end

Native DSF/DSDIFF/DSD routes materialize the admitted source bytes without hard links. DST/SACD routes execute a typed in-process front-end before SoX:

```rust
pub enum DsdInputFrontEnd {
    NativeUncompressed,
    DsdiffDst { decoder: QualifiedDstDecoderVersion },
    SacdDsd { extractor: QualifiedSacdExtractorVersion },
    SacdDst {
        extractor: QualifiedSacdExtractorVersion,
        decoder: QualifiedDstDecoderVersion,
    },
}
```

The front-end writes canonical untagged DSDIFF/DSD using the in-tree decoded-reader and DFF-writer primitives. It must preserve exact DSD bytes, sample rate, channel order, and per-channel sample count; it may not alter level, filter, resample, or convert bit order incorrectly.

Runtime verifies:

- source identity before and after reading;
- declared rate/channel geometry;
- every structured decode error;
- `DSTC` when present;
- exact decoded byte count;
- canonical materialization probe facts and SHA-256;
- no source mutation and no hard-link alias to the source.

An unattested front-end fails before decode:

```text
DSD-REF-P0-014: Reference requires the qualified DST/SACD decode front-end for
this source, but the decoder/extractor identity or qualification manifest does
not match. Install the qualified toolchain or use an uncompressed DSF/DSDIFF
source.
```

Provenance and UI wording append `with qualified DST decode front-end` when DST was decoded. The full Reference label remains available because the front-end is lossless and independently qualified; an unqualified FFmpeg fallback is forbidden.

### 4.3 Render argv

#### B1/B2

```text
sox -S -D IN -t w64 -e floating-point -b 64 R64
    gain -12.000000000
    rate -u SR
```

Exact tokens after executable:

```text
["-S", "-D", IN,
 "-t", "w64", "-e", "floating-point", "-b", "64", R64,
 "gain", "-12.000000000",
 "rate", "-u", SR]
```

#### B3

```text
... gain -12.000000000 rate -u SR sinc -a 180 -L -t 10000 -30000
```

#### B4

```text
... gain -12.000000000 rate -u SR sinc -a 180 -L -t 15000 -37500
```

#### B4W

```text
... gain -12.000000000 rate -u SR sinc -a 180 -L -t 15000 -42500
```

#### B5

```text
... gain -12.000000000 rate -u SR sinc -a 180 -L -t 22000 -59000
```

#### B6 — transcript fixture only; never executable under v1

```text
... gain -12.000000000 rate -u SR sinc -a 180 -L -t 51800 -114100
```

The generic token form for B3–B6 is:

```text
["-S", "-D", IN,
 "-t", "w64", "-e", "floating-point", "-b", "64", R64,
 "gain", "-12.000000000",
 "rate", "-u", SR,
 "sinc", "-a", "180", "-L", "-t", TW, concat("-", FC)]
```

`rate -u` must precede `sinc`. No generic resampler, generic sinc, generic dither, normalization, or user effect may enter this command.

### 4.4 True-peak measurement

Exact argv against `R64`, then repeated against `QPCM`:

```text
ffmpeg -nostdin -hide_banner -nostats -loglevel info
       -i INPUT
       -filter:a loudnorm=I=-23.0:LRA=7.0:TP=-1.0:print_format=json
       -f null -
```

Exact tokens after executable:

```text
["-nostdin", "-hide_banner", "-nostats", "-loglevel", "info",
 "-i", INPUT,
 "-filter:a", "loudnorm=I=-23.0:LRA=7.0:TP=-1.0:print_format=json",
 "-f", "null", "-"]
```

The strict parser:

- accepts exactly one final JSON object from the identified filter;
- reads exactly `input_tp`, never `output_tp`;
- accepts the frozen decimal grammar or exact `-inf` silence literal;
- parses directly to `DbNano`, never through binary floating point;
- rejects missing/duplicate reports, locale commas, NaN, positive infinity, unknown syntax, and values outside −1000 to +100 dBTP;
- accepts `-inf` only after an independent scan proves all finite samples are signed zero;
- records raw JSON, reported value, one-sided reporting uncertainty, analyzer residual bound, and conservative upper bound.

```text
TP_upper = TP_reported + Q + E
```

`Q` and `E` are immutable policy data for the exact analyzer closure. A cell without a defensible one-sided contract remains unavailable.

### 4.5 Terminal safety arithmetic

Use signed nanodecibels:

```rust
pub struct DbNano(pub i64);
```

Canonical serde and argv rendering use a mandatory sign where argv requires it and exactly nine fractional digits. Parsing rejects exponent notation, locale syntax, NaN/Inf, and excess precision.

Each target-rate/depth cell stores a conservative linear terminal error bound:

```rust
pub struct TerminalRealizationBound {
    pub max_added_peak_fs_q63_ceil: u64,
    pub safe_pre_terminal_ceiling_dbtp: DbNano,
    pub derivation_digest: Sha256Digest,
}
```

The policy generator, not runtime, derives the dB ceiling with high-precision deterministic arithmetic and conservative rounding. Runtime computes:

```text
maximum_ceiling_safe_gain = safe_pre_terminal_ceiling_dbtp - TP_upper
```

No fixed dB offset may stand in for additive quantization/dither/float-rounding error near silence.

### 4.6 Terminal argv

#### Int24

```text
sox -S -D R64 -t w64 -e signed-integer -b 24 QPCM gain G dither
```

#### Int16

```text
sox -S -D R64 -t w64 -e signed-integer -b 16 QPCM gain G dither -s
```

#### Float32

```text
sox -S -D R64 -t w64 -e floating-point -b 32 QPCM gain G
```

#### Float64

```text
sox -S -D R64 -t w64 -e floating-point -b 64 QPCM gain G
```

For `NormalizePeak`, replace `gain G` with `norm <canonical target>`, retaining the depth-specific dither tokens.

There is exactly one terminal integer dither effect: Shibata for Int16, TPDF for Int24, none for floating point. The current code already maps plain `dither` and `dither -s` to TPDF and Shibata (`tonepoet-pipeline/src/mapping.rs:91-100`); P0 must qualify the exact pinned SoX-ng behavior rather than rely on the label alone.

### 4.7 Lossless packaging argv

`QPCM` is the sole terminal sample-format boundary. Packaging must decode back sample-identically.

Common FFmpeg prefix:

```text
["-y", "-hide_banner", "-nostdin",
 "-i", QPCM,
 "-map", "0:a:0", "-map_metadata", "-1",
 "-vn", "-sn", "-dn"]
```

Let `PCM_CODEC` be `pcm_s16le`, `pcm_s24le`, `pcm_f32le`, or `pcm_f64le`.

| Target | Exact suffix |
|---|---|
| `WavW64` | no package command; `QPCM` is the staged final audio object |
| `WavRiff` | `-c:a PCM_CODEC -f wav FINAL` |
| `WavRf64` | `-c:a PCM_CODEC -f wav -rf64 always FINAL` |
| `FlacNative` Int16/24 | `-c:a flac -compression_level FLAC_LEVEL FINAL` |
| `AiffNative` Int16 | `-c:a pcm_s16be -f aiff FINAL` |
| `AiffNative` Int24 | `-c:a pcm_s24be -f aiff FINAL` |
| `WavPackNative` Int16/24 | `-c:a wavpack -compression_level WAVPACK_LEVEL FINAL` |
| `AlacM4a` Int16/24 | `-c:a alac -f ipod FINAL` |

`FLAC_LEVEL` is canonical 0–8. `WAVPACK_LEVEL` is canonical 0–3 and non-hybrid. Those byte-affecting values participate in behavior identity and qualification.

No package command may contain `-af`, `-ar`, `-sample_fmt`, gain, normalization, dither, or an additional stream. Metadata/artwork/tag mutation happens afterward and must be followed by decoded-sample verification.

### 4.8 Verification order

For every successful P0 conversion:

1. verify and materialize source;
2. if required, decode/extract DST/SACD to canonical uncompressed DSD;
3. render to `R64`;
4. verify `R64` contract and exact carrier bridge;
5. measure pre-final true peak;
6. bind one constant gain or normalize target;
7. realize `QPCM` once;
8. measure post-final true peak;
9. enforce the ceiling for Reference/NativeLevel/Fixed;
10. package losslessly when required;
11. mutate metadata/artwork without audio changes;
12. decode and compare every channel/sample to `QPCM`;
13. hash final output;
14. create mandatory manifest state;
15. publish atomically under retained authority.

Any failure before the commit primitive publishes nothing and cleans all planner-owned work.

---

## 5. P0 types and settings

### 5.1 Directional settings split

Build the directional split from the accepted design:

```rust
pub struct DsdSettings {
    pub pcm_to_dsd: PcmToDsdSettings,
    pub from_dsd: DsdSourceSettings,
    pub(crate) origin: DsdSettingsOrigin,
}

pub(crate) enum DsdSettingsOrigin {
    NativeV2,
    LegacyFlatV1(LegacyDsdSettingsWireV1),
}

pub struct PcmToDsdSettings {
    pub noise_shaper: DsdNoiseShaper,
    pub modulator_order: ModulatorOrder,
    pub trellis: Option<TrellisSettings>,
    pub filter: DsdFilterPreset,
    pub sinc: PcmToDsdSincSettings,
    pub gain_compensation: GainCompensation,
}

pub struct DsdSourceSettings {
    pub pathway: DsdSourcePathway,
    pub reference_policy: DsdReferencePolicyVersion,
    pub profile: DsdReconstructionSelection,
    pub gain_mode: DsdSourceGainMode,
    pub fixed_gain_db: Option<DbNano>,
    pub normalize_peak_target_dbfs: DbNano,
}
```

Exact enums:

```rust
pub enum DsdSourcePathway {
    Reference,
    Manual, // typed and persisted; P0 validation always rejects
}

pub enum DsdReconstructionSelection {
    Reference,
    Wideband,
}

pub enum DsdSourceGainMode {
    Reference,
    NativeLevel,
    Fixed,
    NormalizePeak,
}
```

`PcmToDsdSettings` receives the current PCM→DSD noise shaper, modulator, trellis, filter, sinc, and gain-compensation fields without behavior changes. Rename the shared `SincFilterSettings` to `PcmToDsdSincSettings`; do not allow Reference planning to read it.

New defaults:

```text
pathway                     = reference
reference_policy            = sox_ng_14_8_0_1_v1
profile                     = reference
gain_mode                   = reference
fixed_gain_db               = none
normalize_peak_target_dbfs  = -0.150000000
origin                      = native_v2 (private)
```

### 5.2 Deferred Manual choice

P0 **omits** `PipelineSettings.audio_workflow` and every workflow snapshot/stage/dependency type.

Forward-compatibility contract:

- `DsdSourcePathway::Manual` and its stable serialized key `manual` ship now;
- native-v2 settings containing `pathway = manual` parse successfully but fail deterministic validation with `DSD-REF-P0-001` before plan construction;
- queue admission, TUI conversion commit, CLI conversion, and preset application may never persist an executable Manual job in P0;
- a later release may add `PipelineSettings.audio_workflow: Option<AudioWorkflowSnapshot>` with serde default `None` and independent workflow schema versioning;
- the current native-v2 DSD wire remains valid after that additive top-level field appears;
- the generic `PlannedExecutionStep` vector and publication API may not assume every future command belongs to Reference, but P0 adds no workflow stage variant or workflow behavior.

### 5.3 Exact output target identity

Implement the exact `ResolvedOutputTarget` identity for every currently enabled `(AudioFormat, ContainerOption)` pair, because preset v4, native-v2 behavior identity, and fail-closed lossy rejection require an unambiguous product key. This is identity infrastructure only; it does not implement new encoders.

```rust
pub enum ResolvedOutputTarget {
    FlacNative, FlacOgg, FlacMka, FlacMkv,
    WavRiff, WavRf64, WavW64, WavMka, WavMkv,
    AiffNative, AiffMka, AiffMkv,
    WavPackNative, WavPackMka, WavPackMkv,
    Mp3Native, Mp3Mka, Mp3Mkv,
    AacM4a, AacMp4, AacM4b, AacMka, AacMkv,
    OpusNative, OpusWebM, OpusWebA, OpusMka, OpusMkv,
    AlacM4a, AlacMp4,
    DsfNative, DsfAsDff, DffNative,
    DtsNative, DtsMka, DtsMkv, DtsMp4,
    Ac3Native, Ac3Mka, Ac3Mkv, Ac3Mp4,
    LpcmRiff, LpcmAiff,
}
```

The request bridge derives exactly one variant from format, selected container, canonical extension, and canonical flag sequence. Native DSD planning requires `Some(target)` and rejects conflicts. Reference accepts only the seven variants in §2.5.

### 5.4 Lossy forward seam

Do not add a user setting for delivery mode. Derive delivery class from `ResolvedOutputTarget`:

```rust
pub enum DsdDeliveryClass {
    LosslessReference,
    LossyReferenceFrontEnd, // typed reservation; P0 validation always rejects
}
```

Keep reconstruction and delivery authority separable:

- `DsdReferencePolicyVersion` owns source front-ends, reconstruction profiles, measurement, gain, and lossless terminal cells;
- P0 `FinalizeDelivery` accepts only `LosslessReference` and a `PackageLossless` operation;
- a future lossy path reuses the same qualified render/front-end policy but supplies a separate immutable lossy-delivery policy/qualification identity and a new finalizer variant;
- adding lossy delivery must not alter or re-mint the already qualified lossless cells.

### 5.5 Source facts

Extend planner input:

```rust
pub enum DsdSourceKind {
    DsfUncompressed,
    DsdiffUncompressed,
    DsdiffDst,
    SacdArea { frame_format: SacdFrameEncoding },
    UnknownDsdContainer,
}

pub enum SacdFrameEncoding { Dsd, Dst }
```

`SourceInfo` gains `Option<DsdSourceKind>` with backward-compatible serde defaulting. Reference rejects `None`/unknown. Preserve the original SACD area encoding even after per-track materialization so provenance and front-end qualification remain truthful.

### 5.6 Execution-step and operation subset

Replace static-only action commands with:

```rust
pub enum PlannedExecutionStep {
    Command(PlannedCommand),
    Measurement(PlannedMeasurement),
    DeferredCommand(PlannedDeferredCommand),
}
```

Supporting P0 policy types:

```rust
pub enum ResolvedDsdProfile {
    B1RateOnly,
    B2RateOnly,
    B3 { passband_hz: u32, transition_hz: u32, center_hz: u32 },
    B4 { passband_hz: u32, transition_hz: u32, center_hz: u32 },
    B4W { passband_hz: u32, transition_hz: u32, center_hz: u32 },
    B5 { passband_hz: u32, transition_hz: u32, center_hz: u32 },
    B6 { passband_hz: u32, transition_hz: u32, center_hz: u32 },
}

pub enum ResolvedGainPolicy {
    ReferenceCompensated {
        requested_gain: DbNano,
        ceiling: DbNano,
        terminal_bound: TerminalRealizationBound,
    },
    NativeLevelExact {
        gain: DbNano,
        ceiling: DbNano,
        terminal_bound: TerminalRealizationBound,
    },
    FixedExact {
        gain: DbNano,
        ceiling: DbNano,
        terminal_bound: TerminalRealizationBound,
    },
    NormalizePeak { target_dbfs: DbNano },
}

pub struct FinalPcmContract {
    pub sample_rate_hz: u32,
    pub channels: u16,
    pub sample_kind: SampleKind,
    pub bit_depth: PcmBitDepth,
    pub dither: ReferenceDither,
}

pub enum ReferenceDither { None, Tpdf, Shibata }
```

P0 operations:

```rust
DsdLosslessDecodeMaterialize {
    front_end: DsdInputFrontEnd,
    output_contract: CanonicalDsdContract,
}
DsdReferenceRender {
    target_rate_hz: u32,
    profile: ResolvedDsdProfile,
    policy: DsdReferencePolicyVersion,
}
MeasureTruePeak {
    measurement_id: MeasurementId,
    scope: MeasurementScope, // P0 requires Plan only
    purpose: TruePeakPurpose,
}
DsdReferenceFinalize {
    sample_contract: FinalPcmContract,
    gain_policy: ResolvedGainPolicy,
    measurement: Option<MeasurementBinding>,
}
PackageLossless {
    target: ResolvedOutputTarget,
    sample_contract: FinalPcmContract,
}
```

Do not add Manual workflow operations. `PlannedExecutionStep::Command` is already generic enough for a future stage without changing the executor's result-binding model.

### 5.7 Planner purity

The `tonepoet-pipeline` planner remains deterministic and performs no filesystem, process, executable-resolution, version, or qualification-manifest I/O. It consumes already-established source facts and emits:

- required built-in tool identifiers;
- typed path roles;
- exact semantic argv;
- measurements and parsers;
- deferred gain bindings;
- cleanup paths;
- finalization/publication intent;
- deterministic rejection before any runtime preflight.

The orchestrator performs tool resolution/attestation after pure planning and before rerun admission.

---

## 6. Serde, migration, fingerprints, and manifest

### 6.1 Native and legacy DSD settings wire

Use a hand-written map deserializer with duplicate/unknown-key rejection.

Native v2 is exact:

```rust
pub struct DsdSettingsWireV2 {
    pub schema_version: u32, // exactly 2
    pub pcm_to_dsd: PcmToDsdSettings,
    pub from_dsd: DsdSourceSettings,
}
```

Legacy flat v1 is the complete current ten-field wire:

```rust
pub struct LegacyDsdSettingsWireV1 {
    pub noise_shaper: DsdNoiseShaper,
    pub modulator_order: ModulatorOrder,
    pub trellis: Option<TrellisSettings>,
    pub pcm_to_dsd_filter: DsdFilterPreset,
    pub dsd_to_pcm_lowpass: DsdLowpassMethod,
    pub dsd_to_pcm_gain_mode: LegacyDsdToPcmGainMode,
    pub dsd_to_pcm_auto_gain_margin_db: f32,
    pub dsd_to_pcm_gain_db: Option<f32>,
    pub sinc: SincFilterSettings,
    pub gain_compensation: GainCompensation,
}
```

Preserve it privately and execute the old DSD→PCM semantics exactly:

| Legacy intent | Preserved behavior/label |
|---|---|
| lowpass Auto/SoxUltra | old post-rate single-cutoff path; `Legacy SoX lowpass (preserved)` |
| lowpass Sinc | old pre-rate custom sinc path; `Legacy custom sinc (preserved)` |
| gain Disabled, no stray dB | no gain; `Legacy native level` |
| gain Disabled, stray dB | compatibility fixed gain; `Legacy fixed gain` |
| gain Auto | old SoX `norm` with old margin; `Legacy normalize` |
| gain Manual | old fixed gain; `Legacy fixed gain` |

Do not silently migrate a legacy item to Reference. Unchanged legacy settings serialize back to the legacy shape. Editing any DSD field requires an explicit migration to native-v2 settings. If a mirrored legacy field has diverged, serialization fails until migration.

Old queue rows with full legacy settings remain executable under legacy identity. Old rows missing the full settings/request fail:

```text
QUEUE-SETTINGS-MIGRATION-REQUIRED: This queued conversion does not contain the
complete settings required to preserve its original DSD behavior. Recreate or
explicitly migrate the job; tonepoet will not apply current defaults.
```

The compatibility constructor must freeze historical defaults and never call the evolving `PipelineSettings::default()`.

### 6.2 Fixed-point serde

New dB fields serialize as canonical signed decimal strings with exactly nine fractional digits. Hashing, equality, arithmetic, and argv rendering use `DbNano`; policy runtime performs checked integer arithmetic only.

### 6.3 Preset v4 subset

Version-dispatch exact v2, v3, and v4 wire structs before interpretation. Do not use parse-failure fallback to another schema; the current loader's parse-then-fallback path must be replaced (`src/tui/presets.rs:576-595`). Save through same-directory temporary file, flush/`fsync`, atomic replace, and parent-directory sync rather than direct `fs::write` (`src/tui/presets.rs:599-612`).

P0 v4 additions:

```rust
pub struct PresetWireV4 {
    pub version: u32, // exactly 4
    // existing v3 fields
    pub output_target: String, // required canonical ResolvedOutputTarget key
    pub dsd_path: Option<String>,
    pub dsd_profile: Option<String>,
    pub dsd_gain: Option<String>,
    pub dsd_gain_db: Option<String>,
    pub dsd_normalize_target_dbfs: Option<String>,
}
```

P0 omits workflow name/digest fields. A future preset v5 may add them without changing v4 files. `dsd_path = manual` is a valid wire spelling but preset application refuses it with `DSD-REF-P0-001`.

Old v2/v3 presets acquire the new Reference defaults but have no exact container identity. Preserve the visible historical selection and require explicit target confirmation before native DSD admission; never guess RIFF/RF64/W64 from extension or stale index.

### 6.4 Fingerprint domains

Retain the current settings fingerprint byte-for-byte as `legacy_settings_fingerprint_v1` for legacy manifest/rerun compatibility. Add:

```rust
settings_snapshot_fingerprint_v2
conversion_behavior_fingerprint_v1
semantic_plan_hash_v1
execution_fingerprint_v1
```

Rules:

- snapshot v2 hashes canonical native-v2 settings fields declared fingerprint-bearing;
- behavior identity is source-aware and pathway-scoped;
- semantic plan hashing replaces ephemeral absolute paths with typed roles such as `{SOURCE}`, `{WORK}`, `{FINAL}`;
- execution identity binds behavior, plan, planner/build identity, policy ID, qualification digest, platform/ABI/runtime dispatch, exact tool closure identities, and in-process DST/SACD build identities;
- generic dither/resampler/sinc settings are excluded from Reference behavior identity because planner firewalls prevent them from affecting argv;
- exact `ResolvedOutputTarget` is included;
- DST/SACD front-end identity and canonical materialization contract are included;
- pathnames and random staging IDs do not create false behavior changes;
- executable bytes/version/closure/dispatch drift invalidates reuse.

### 6.5 Manifest v2 P0 shape

Bump the manifest envelope to version 2 while retaining a frozen v1 reader.

P0 route identity variants:

```rust
pub enum ManifestRouteIdentityV2 {
    LegacyPipelineV1 {
        settings_fingerprint_v1: LegacySettingsFingerprintV1,
    },
    DsdReferenceV2 {
        settings_snapshot_fingerprint_v2: SettingsSnapshotFingerprintV2,
        resolved_output_target: ResolvedOutputTarget,
        policy: DsdReferencePolicyVersion,
        qualification_manifest_digest: Sha256Digest,
    },
}
```

Per native DSD track require:

- source presented/resolved path for provenance;
- source size and full content SHA-256;
- source probe digest;
- original `DsdSourceKind` and selected front-end identity;
- canonical materialization SHA-256 when a front-end rewrites/decodes;
- behavior fingerprint;
- execution fingerprint;
- semantic plan hash;
- exact output path, size, and full output SHA-256;
- validation status and publication timestamp.

No P0 Reference manifest carries programme identity because P0 rejects every multi-member scope. A future manifest version or additive route variant may add Manual/programme identity; do not write placeholder data now.

A native skip requires exact route, target, policy, qualification, source-content, front-end, behavior, execution, plan, output hash/size, and validation matches. Never infer native equivalence from a v1 manifest.

---

## 7. Orchestrator, executor, and publication

### 7.1 Admission and source materialization

Admission opens the selected source under ordinary input semantics, records presented and resolved paths, requires a regular file, and hashes/probes from that handle with pre/post identity checks. A rerun hit needs no work copy.

On a miss, reopen/resolve, require the admitted identity again, then copy or copy-on-write clone from the verified handle into the planner work root. Never hard-link. Hash/probe the materialization and require exact agreement before any backend starts. All tools receive only the work path.

For DST/SACD, decode from the verified work input into a second planner-owned canonical DSDIFF/DSD materialization. Never decode directly into a final or shared persistent location.

### 7.2 Measurement and deferred binding

The executor owns a plan-local map from `MeasurementId` to typed values. P0 supports only plan scope. The exact binding subset is:

```rust
pub struct PlannedMeasurement {
    pub id: MeasurementId,
    pub scope: MeasurementScope,
    pub purpose: TruePeakPurpose,
    pub command: PlannedCommand,
    pub parser: MeasurementParser,
}

pub enum MeasurementScope { Plan }
pub enum TruePeakPurpose { GainAuthority, PostFinalAcceptance }
pub enum TruePeakValue { Finite(DbNano), VerifiedSilence }

pub enum PlannedArg {
    Literal(String),
    BoundGainDb {
        true_peak: MeasurementId,
        policy: ResolvedGainPolicy,
    },
}
```

A deferred command stores a typed `BoundGainDb` expression; it does not contain a format string or replan after measurement.

After measurement:

1. validate the measurement binding and unit;
2. compute gain with checked `DbNano` arithmetic;
3. render canonical signed nine-decimal gain text;
4. record the fully resolved argv;
5. launch once.

Missing, duplicate, stale, non-finite, wrong-purpose, or wrong-unit measurements abort before finalization.

### 7.3 Toolchain preflight

After pure planning and before rerun admission:

- resolve each built-in tool once;
- bind canonical executable path, executable SHA-256, reported version, closure digest, behavior-probe digest, platform ABI, runtime-dispatch digest, and policy resources;
- bind in-process DST/SACD build identities;
- compare every required identity to the policy qualification manifest;
- compute execution fingerprint;
- rerun only after that identity exists.

Revalidate identity immediately before launch to the extent supported by immutable package/store semantics. Any mismatch fails closed; Reference never falls back to another SoX, FFmpeg decoder, sample-peak analyzer, or unqualified in-process decoder.

```text
DSD-REF-P0-015: The installed Reference toolchain does not match policy
sox_ng_14_8_0_1_v1 or failed its behavior probes. Activate/install the
qualified toolchain; tonepoet will not substitute another decoder, analyzer,
resampler, or encoder.
```

### 7.4 Cancellation and cleanup

Cancellation checks occur:

- before and after source copy/decode;
- before and after each process;
- before and after each measurement;
- before resolving a deferred command;
- before metadata mutation and verification;
- before journal creation;
- immediately before the non-cancellable atomic commit primitive.

All materialization, render, measurement, terminal, package, metadata, verification, journal, and temporary manifest paths are planner-declared cleanup paths. A restarted pre-publication job discards the old work root and reruns; a file's existence is never reuse authority.

### 7.5 Publication hardening

P0 uses the existing descriptor-bound album publication authority; it does not publish through the low-level track executor's stale-destination remove/rename path (`src/convert/pipeline/track_executor.rs:701-714`).

#### Absent album directory

For a singleton whose final album directory is absent, reuse the sibling-directory assembly and one-rename publisher, hardened as follows:

1. final audio, metadata, decoded-sample comparison, post-final peak result, and hashes must all pass;
2. file and directory sync failures are fatal;
3. the complete manifest v2 is mandatory inside the staging directory;
4. destination absence is revalidated under retained authority;
5. publish by a non-replacing directory rename;
6. sync the destination parent;
7. any post-rename sync failure reports an explicit recoverable-published state rather than claiming no publication.

#### Existing album directory

For one singleton output into an existing directory, implement the narrow journaled file+manifest transaction from the accepted design:

- hold the descriptor-bound album publication lock for the entire decision;
- verify/sync a same-filesystem sibling staged file;
- revalidate destination identity and current complete manifest hash;
- write and sync a journal containing prior identity, staged hash, previous manifest hash, complete next manifest payload/hash, and monotonic state;
- install an absent destination with an atomic no-replace primitive, or an explicitly requested overwrite with a qualified atomic replace primitive; never pre-delete;
- persist directory, atomically replace the shared manifest, persist again, and durably retire the journal.

A v1, corrupt, mismatched-route, mismatched-target, or mismatched-settings manifest is not merged implicitly. Native-v2 Reference and legacy entries never share one album manifest.

Crash recovery must distinguish:

- crash before install: preserve prior file and manifest;
- crash after file install but before manifest: verify exact journal hash, complete the exact manifest or report `PUBLISHED-WITHOUT-RERUN-AUTHORITY`;
- crash after manifest replace but before journal retirement: verify both and retire idempotently;
- unknown state: fail closed; never rebuild execution identity by scanning files.

---

## 8. P0 exposure

### 8.1 TUI

For DSD-source to PCM/lossless-target conversions, add:

```text
DSD path       [reference | manual (not yet available)]
DSD profile    [reference | wideband]
DSD gain       [reference | native | fixed | normalize]
DSD gain dB    [fixed only]
normalize dBFS [normalize only]
```

Do not add a workflow row or builder.

Rules:

- Reference is the default.
- Manual is visible as disabled/not yet available; attempting to select or apply it returns `DSD-REF-P0-001`.
- Wideband is enabled only for DSD128 targets ≥176.4 kHz; DSD256 B6 remains visible as unavailable with `DSD-REF-P0-009`; DSD64 Wideband is unavailable.
- Fixed enables only fixed gain. Normalize enables only normalize target.
- unsupported cells show the exact planner-grade message and disable commit;
- dither displays locked `Shibata (Reference)` for Int16, `TPDF (Reference)` for Int24, and `none (float)` for Float32/64;
- resampler displays locked `SoX-ng Reference (rate -u)`;
- generic dither/resampler/sinc state is preserved but inactive and cannot enter plan or behavior identity;
- modified gain/profile labeling follows §2.6.

Use the existing `FormatField`/visible-row model rather than a separate modal navigation path (`src/tui/app.rs:2938-2993`). Build full settings from TUI state instead of defaulting the DSD struct and overriding two gain fields.

### 8.2 CLI

Add:

```text
--dsd-path <reference|manual>
--dsd-profile <reference|wideband>
--dsd-gain <reference|native|fixed|normalize>
--dsd-gain-db <DB>
--dsd-normalize-target-dbfs <DBFS>
```

Do not add workflow flags.

Rules:

- defaults are Reference/Reference/Reference;
- `--dsd-gain-db` requires fixed;
- normalize target requires normalize;
- profile/gain flags conflict with `--dsd-path manual`, which then returns the not-yet-available error;
- every DSD flag errors when conversion direction is not DSD→PCM;
- fixed-point text uses the strict decimal grammar;
- CLI attaches complete settings and exact resolved target; it never expands the legacy projection.

### 8.3 `:set`

Add:

```text
:set dsd-path reference|manual
:set dsd-profile reference|wideband
:set dsd-gain reference|native|fixed|normalize
:set dsd-gain-db <DB>
:set dsd-normalize-target <DBFS>
```

Do not add `dsd-workflow`. All paths use the same state transition and validation functions as keyboard/mouse interaction.

### 8.4 Provenance wording

Every new Reference record includes:

- policy and qualification digest;
- source kind/rate/channels and admitted content identity;
- front-end identity and decoded materialization identity when applicable;
- exact tool paths, hashes, versions, closure/probe/dispatch identities;
- exact target/profile/passband/transition/−6 dB frequency;
- headroom and requested attenuation;
- pre/post reported and conservative true peak;
- requested/applied gain and ceiling outcome;
- terminal error bound/dither policy;
- package/decoded-sample verification;
- resolved transcript hash.

Approved claim:

> The native SoX-ng Reference path uses a 180 dB requested reconstruction-filter attenuation and is qualified for approximately 180 dB coherent composite stopband rejection.

Do not claim a −180 dBFS noise floor or present order-null figures as broadband rejection.

DST label suffix:

> with qualified DST decode front-end

---

## 9. DST qualification contract

### 9.1 Chosen evidence method

Use a **pinned independent-oracle fixture corpus** as the release authority.

The corpus must contain:

- original DSDIFF/DST and SACD DST inputs or frame-exact extracts;
- DSD64, DSD128, and DSD256 coverage;
- mono and stereo P0 cells;
- additional six-channel decoder-only fixtures proving decoder geometry while policy admission still rejects multichannel Reference;
- canonical uncompressed interleaved DSD SHA-256 values generated independently:
  - `sacd_extract` for SACD DSD64 material;
  - the pinned qualified FFmpeg DST decoder or another independent decoder for DSDIFF/DST DSD64/128/256 fixtures;
- fixture provenance and hashes in the qualification manifest.

The production in-tree decoder must reproduce every expected canonical DSD byte. The independent oracle, not an in-tree roundtrip alone, establishes bit-exact decoder qualification.

Also run the secondary invariant:

```text
production decode
  → in-tree encode as a valid uncompressed DST frame
  → production decode
  → exact DSD byte comparison
```

This catches geometry/materialization integration defects but does not replace the independent oracle.

### 9.2 Runtime and negative qualification

Tests must cover:

- DSD64/128/256 frame geometry;
- mono/stereo;
- six-channel decoder success plus Reference policy rejection;
- compressed and `DSTCoded=0` frames;
- DSDIFF container parsing by `CMPR`, not extension;
- SACD DSD and DST area provenance;
- `DSTC` pass, mismatch, malformed, and absent reporting;
- truncated frames, invalid mappings/segments/probability tables, bad channel counts/rates, output-size mismatch, and cancellation;
- decoded materialization header/rate/channel/sample-count correctness;
- bit-exact decoded bytes and materialization readback;
- no partial materialization after failure;
- unattested decoder/extractor rejection before work;
- execution-fingerprint invalidation when the in-process decoder build or fixture/qualification digest changes.

---

## 10. Complete P0 test plan

### 10.1 Pure policy/planner tests

Use declarative expected tables, not representative cases.

Generate and pin:

1. every supported standard profile source-rate × target-rate cell;
2. every supported B4W source-rate × target-rate cell;
3. every E88, E96, ERATE, EW64, EWTARGET, EWB6FIT, and EB6 matrix rejection;
4. every supported target × depth cell;
5. every rejected target × depth cell;
6. all four gain modes;
7. mono and stereo;
8. native uncompressed and qualified-DST front-end semantic plans;
9. exact RIFF/RF64/W64 target distinction;
10. exact full argv vector after deterministic measurement binding.

For every supported Cartesian case assert:

- exact render argv;
- `gain -12.000000000` before `rate -u`;
- `rate -u` before corrected `sinc`;
- exact corrected `TW` and `FC` tokens;
- exact measurement argv twice;
- exact gain resolution and nine-decimal rendering;
- exact final argv and exactly one terminal dither sequence;
- exact package argv or direct-W64 finalization;
- absence of `-G`, `norm` in Reference gain, generic resampler/sinc/dither tokens, shell tokens, or extra stages;
- exact semantic path-role hash independent of random work root;
- exact cleanup and publication intent.

For B6, assert its typed profile and hypothetical corrected argv fixture, then assert v1 rejection before command execution.

### 10.2 D1 frequency-response qualification

On the pinned SoX-ng 14.8.0.1 closure, measure each explicit profile with deterministic Float64 fixtures and a steady-state window. Assert:

| Profile | Flat response through | −6 dB point | stopband begins |
|---|---:|---:|---:|
| B3 | 25,000 Hz | 30,000 Hz | 35,000 Hz |
| B4 | 30,000 Hz | 37,500 Hz | 45,000 Hz |
| B4W | 35,000 Hz | 42,500 Hz | 50,000 Hz |
| B5 | 48,000 Hz | 59,000 Hz | 70,000 Hz |
| B6 fixture only | 88,200 Hz | 114,100 Hz | 140,000 Hz |

Also qualify B1/B2 integrated `rate -u` responses, passband ripple, attenuation, alias rejection, duration, channel preservation, and the W64 bridge. Comparing argv alone is not acceptance.

### 10.3 Measurement/gain/terminal tests

Test:

- strict final JSON selection;
- `input_tp`, never `output_tp`;
- missing, duplicate, malformed, locale, NaN/Inf, out-of-range, and wrong-unit failures;
- verified silence and nonzero near-silence;
- analyzer round-to-nearest/truncation contract and conservative Q/E arithmetic;
- known inter-sample peak above sample peak;
- monotonic controlled sweeps;
- exact requested gains;
- Reference ceiling-constrained reduction;
- NativeLevel/Fixed exact success and exact refusal;
- NormalizePeak labeling and non-Reference ceiling treatment;
- no retry/backoff or second realization;
- Q1.63 terminal error upward rounding and safe dB ceiling downward rounding;
- Int16 Shibata, Int24 TPDF, Float32, and Float64 bounds for every supported rate/depth cell;
- post-final measurement and acceptance;
- no supported float cell inherits an implicit zero terminal bound.

### 10.4 Packaging and verification tests

For every supported target/depth/encoder-setting cell:

- probe exact container/codec/depth/rate/channels;
- decode back and compare every sample/channel to `QPCM`;
- distinguish ordinary RIFF, RF64, and W64;
- prove RIFF deterministic size refusal;
- exercise W64 >4 GiB behavior where practical through sparse/generated fixtures;
- prove FLAC/WavPack compression-level ranges do not alter decoded samples;
- reject WavPack hybrid/correction mode;
- prove ALAC and AIFF sample preservation;
- reject package filters/resampling/additional streams;
- repeat decoded-sample comparison after metadata/artwork mutation;
- verify output hash and manifest binding.

### 10.5 Source and DST tests

Carry every requirement in §9, plus:

- stable source read and source mutation races;
- symlink presented/resolved-path provenance;
- clone/copy materialization, never hard link;
- source and canonical-DST materialization hash/probe agreement;
- DSF versus DSDIFF/DSD versus DSDIFF/DST classification;
- SACD original area encoding preserved after realization;
- DSD512/1024, unknown encoding, channels >2, and unattested front-end errors;
- independent batch and continuous-image rejection messages exactly pinned.

### 10.6 Serde, migration, preset, and sentinel tests

Required:

- exact legacy flat JSON/TOML fixtures and migration table;
- legacy Auto/Disabled/Manual/Sinc execution remains old behavior, never Reference;
- native-v2 roundtrip;
- mixed keys, duplicate keys, missing/wrong version, unknown keys, and user-authored private origin rejection;
- manual pathway parses then deterministically rejects;
- canonical `DbNano` roundtrip and invalid-number rejection;
- unchanged legacy roundtrip and mutate-then-refuse behavior;
- old full-settings queue preservation;
- missing-settings queue block and frozen historical defaults;
- all currently enabled format/container pairs map to one `ResolvedOutputTarget` key;
- Reference accepts only seven P0 targets and rejects every lossy/native-DSD/non-P0 target with exact text;
- exact v2/v3/v4 preset dispatch;
- malformed/future TUI presets never fall through to wizard parsing;
- v2/v3 container confirmation requirement;
- v4 exact target/DSD-field roundtrip and atomic save crash points;
- every new field appears in sentinel inventory, propagation, conflict, and fingerprint tests;
- every new field remains unrepresentable through the legacy `ConversionOptions` projection.

The existing sentinel explicitly lists current DSD fields and their propagation/fingerprint treatment (`tests/settings_sentinel.rs:414-432`, `tests/settings_sentinel.rs:634-649`); replace those rows with the directional model rather than suppressing drift.

### 10.7 Fingerprint, manifest, and rerun tests

Test:

- frozen legacy v1 hash fixtures;
- native snapshot versus behavior domain inclusion/exclusion;
- exact target distinction;
- source kind/front-end/materialization identity;
- path-role normalization;
- planner, policy, qualification, executable, closure, version, ABI, dispatch, and in-process backend drift;
- manifest v1/v2 exact version dispatch;
- route/track variant mismatch;
- missing source/output SHA-256;
- source/output content drift;
- Reference/legacy cross-match refusal;
- corrupt/missing manifest refusal;
- successful native rerun skip only on complete identity match.

### 10.8 Publication and crash tests

Inject failures at every transition:

- before/after final file sync;
- before/after manifest creation;
- before/after journal sync;
- before/after no-replace or replace install;
- before/after directory sync;
- before/after manifest replace;
- before/after journal retirement;
- cancellation at every cancellable boundary;
- destination and manifest races;
- unexpected existing destination;
- unsupported filesystem primitive;
- crash recovery repeated twice to prove idempotency;
- whole-directory publication with mandatory manifest;
- no partial publication on any precommit failure;
- `PUBLISHED-WITHOUT-RERUN-AUTHORITY` only for the precisely verified post-install/pre-manifest state.

### 10.9 Exposure and wording tests

Snapshot/interaction tests assert:

- defaults and visible rows;
- Manual disabled/not-yet-available behavior;
- Wideband availability and B6 rejection;
- gain-field enablement;
- locked dither/resampler display without destructive state changes;
- CLI conflicts/direction guards/full-settings handoff;
- `:set` uses the same state transition path;
- preset refusal is atomic;
- qualified, modified-gain, Wideband, NormalizePeak, and DST-front-end labels;
- approved attenuation/rejection wording;
- absence of “−180 dBFS noise floor” and equivalent claims.

### 10.10 Tool-gated qualification selection

Create one deterministic integration-test target for the release qualification report, for example:

```text
TONEPOET_REQUIRE_TOOLS=1 \
  cargo test -p tonepoet --test dsd_reference_qualification -- --nocapture
```

It must record:

- exact tool and in-process backend identities;
- qualification-manifest digest;
- enabled/rejected matrix cells;
- fixture hashes;
- D1 response results;
- DST independent-oracle results;
- analyzer/terminal bounds;
- package decode-back results;
- pass/fail outcome.

No unsupported cell is enabled because an unrelated smoke command happens to exit successfully.

---

## 11. Two independently gated implementation sub-rounds

### P0-A — foundation, wire compatibility, immutable plan

Implement only:

1. apply-tree SoX-ng pin/closure preflight;
2. `DbNano` and canonical parsing/serde;
3. directional DSD settings split and exact legacy compatibility wire;
4. `DsdSourcePathway::Manual` typed reservation and P0 rejection;
5. exact `ResolvedOutputTarget` identity/resolver;
6. source-kind and front-end facts;
7. immutable policy/profile/target tables with D1-corrected frequency centers;
8. generic execution-step/measurement/deferred types;
9. pure Reference planner and all deterministic rejections;
10. semantic/snapshot/behavior identity types;
11. manifest-v2 structs/version dispatch;
12. preset-v4 wire/version dispatch and sentinel updates;
13. pure unit/transcript/migration/fingerprint tests.

Production behavior seam:

- do not route user conversions through the new executor yet;
- the new planner may be invoked by tests/internal preflight only;
- existing legacy conversions remain behaviorally unchanged;
- no partially qualified Reference option is exposed in CLI/TUI.

P0-A gate:

```text
cargo fmt -p tonepoet -p tonepoet-pipeline -p sacd-rs -- --check
cargo test -p tonepoet-pipeline
cargo test -p sacd-rs
cargo test -p tonepoet --test settings_sentinel
cargo test --workspace
cargo clippy -p tonepoet -p tonepoet-pipeline -p sacd-rs \
  --all-targets --all-features -- -D warnings
```

Add any additionally touched crate explicitly. Do not substitute unconditional workspace fmt/clippy if unrelated existing failures remain.

P0-A is complete only when all pure planner transcripts and rejections are frozen and the old path has zero behavior regressions.

### P0-B — execution, qualification, publication, exposure

Implement only after P0-A is accepted:

1. source materialization;
2. qualified DSDIFF/DST and SACD DSD/DST front-end;
3. toolchain attestation and execution fingerprints;
4. measurement/parser/deferred binding;
5. render/finalize/package execution;
6. post-final peak and decoded-sample verification;
7. manifest-v2 rerun authority;
8. absent-directory and journaled existing-directory publication hardening;
9. CLI/TUI/`:set`/preset exposure;
10. full real-tool and DST qualification suite/report.

P0-B gate:

```text
cargo fmt -p tonepoet -p tonepoet-pipeline -p sacd-rs -- --check
cargo test --workspace
cargo clippy -p tonepoet -p tonepoet-pipeline -p sacd-rs \
  --all-targets --all-features -- -D warnings
TONEPOET_REQUIRE_TOOLS=1 \
  cargo test -p tonepoet --test dsd_reference_qualification -- --nocapture
```

Again, add every additionally touched crate explicitly. P0-B is complete only when the generated qualification manifest and report agree with the compiled policy table and no candidate/rejected-cell mismatch exists.

---

## 12. PCM→DSD reuse notes — no design expansion

P0 must preserve the existing custom PCM→DSD settings and behavior under `PcmToDsdSettings`. Do not route it through the new DSD→PCM profile matrix.

Free future reuse is limited to:

- `DbNano`;
- append-only policy registry mechanics;
- qualification-manifest format;
- source/tool/execution identity primitives;
- measurement/deferred-step executor if a future PCM→DSD evidence policy needs them;
- manifest domains;
- publication durability.

A future Auto/Reference PCM→DSD policy requires its own evidence commission. Do not invent its filters, levels, modulators, or qualification cells now.

---

## 13. Definition of done

P0 is handoff-ready only when all are true:

- new DSD→PCM conversions default to Reference/Reference/Reference;
- legacy queued conversions preserve old semantics or fail with the explicit migration-required error;
- the apply tree resolves the commissioned SoX-ng 14.8.0.1 closure;
- every enabled source/front-end/channel/rate/profile/target/depth/gain cell is frozen in an immutable qualification manifest;
- every unsupported cell fails before render with the exact actionable message;
- D1-corrected sinc centers realize the intended flat/−6 dB/stopband profile;
- DSDIFF/DST and SACD DST use the qualified lossless front-end and independent-oracle corpus;
- Reference uses explicit headroom, `rate -u`, optional corrected profile sinc, true-peak measurement, one constant gain, one terminal realization, post-final measurement, sample-preserving package, verification, and atomic publication;
- generic dither/resampler/sinc settings cannot affect Reference argv or behavior identity;
- Manual and lossy delivery remain unimplemented and fail with the P0 messages;
- every multi-member and continuous programme fails before render;
- manifest v2 and execution identity prevent stale reuse after any source/output/tool/policy/plan drift;
- crash tests prove no silent partial publication or fabricated rerun authority;
- all scoped fmt/clippy gates, `cargo test --workspace`, and the real-tool qualification selection pass;
- the generated qualification report names exact versions, identities, fixture hashes, enabled/rejected cells, and results.

## 14. Complete-file delivery contract for the implementation rounds

Each implementation round that follows must:

1. begin by verifying the supplied tree against `docs/handoff_manifest.txt` and report the exact pass count;
2. deliver complete changed files, not patch fragments or omitted sections;
3. preserve every untouched file byte-for-byte;
4. include all new/updated tests and generated qualification artifacts required by that sub-round;
5. run and report the sub-round gates exactly, including every skipped tool gate and reason;
6. update `docs/handoff_manifest.txt` only after the tree is final;
7. package the complete source tree without `.git`, `target`, temporary work roots, editor caches, or nested prior archives;
8. extract the produced archive into a fresh directory and re-run the final manifest check;
9. report changed files, test results, unresolved `NEEDS-VERIFICATION` seams, archive SHA-256, and whether implementation behavior is exposed;
10. make no Manual, lossy, programme-wide, B6, multichannel, DSD512/1024, or PCM→DSD Reference implementation changes unless a later commission explicitly authorizes them.

This brief authorizes planning for the two P0 implementation rounds only. It does not authorize implementation in the present round.
