# Current DSD Reference P0 Handoff

**Date:** 2026-07-20
**Current candidate policy:** `sox_ng_14_8_0_1_v7`
**Runtime exposure:** fail-closed until promotion; ordinary defaults remain exact legacy behavior
**Supersedes:** all earlier `handoff_dsd_reference_p0_*` snapshots for current-state claims

## Admission and routing authority

`tonepoet_pipeline::selects_reference_dsd_to_pcm()` remains the sole admission predicate for native-v2 Reference planning and Reference-only preflight work. It requires:

1. native-v2 DSD settings;
2. an authoritative DSD source classification;
3. a non-DSD target.

Native-v2 DSD-to-DSD requests remain on the ordinary DSD topology. Pre-promotion defaults remain the exact legacy flat settings origin and do not enter the Reference planner.

## F1 analyzer correction inherited from v6

The failing Float32 qualification cell was not caused by fixture amplitude, per-cell directory reuse, or a crossed work path. An isolated same-file reproduction established a carrier-sensitive decoder crossing:

```text
SoX-written Float32 W64, analytic peak -20 dBFS
FFmpeg direct decode                         input_tp -20.00
SoX W64 decode -> f64 WAV stream -> FFmpeg input_tp  -0.00

SoX-written Float64 W64, analytic peak -20 dBFS
FFmpeg direct decode                         input_tp 166.64
SoX W64 decode -> f64 WAV stream -> FFmpeg input_tp -20.00
```

The two carrier depths require opposite measurement routes. Policy v7 inherits v6's frozen analyzer contract:

- R64 pre-final measurement: SoX f64-WAV stdout directly into FFmpeg stdin;
- Float32 QPCM post-final measurement: direct path-backed W64 input to FFmpeg;
- Int24 and Float64 QPCM post-final measurement: SoX f64-WAV stdout into FFmpeg stdin;
- parser: `ffmpeg_loudnorm_input_tp_v3`.

The executor validates every measurement against the immutable `DsdReferencePlanSummary`: exact measurement ID, scope, purpose, carrier path, parser, environment, and carrier-sensitive decoder route. Canonical argv pointed at the wrong carrier or using the wrong decoder is rejected before execution.

## V7 correction: Float64 RIFF/RF64 packaging

V6 correctly avoided FFmpeg's defective direct Float64-W64 decoder for measurement, but still used that decoder when packaging Float64 QPCM to RIFF or RF64. The preservation check then decoded both sides with FFmpeg, creating a common-mode false oracle.

V7 removes that route completely. Float64 RIFF/RF64 packaging is one typed, shell-free two-process operation:

```text
SoX:    -S -D <qpcm.w64> -t raw -e floating-point -b 64 -L -
FFmpeg: -f f64le -ar <rate> -ac <channels> -i pipe:0 ... -c:a pcm_f64le -f wav [ -rf64 always ] <output>
```

The transport is headerless little-endian Float64 PCM over a direct stdout-to-stdin pipe. It introduces no disk-backed RIFF intermediate and therefore does not impose RIFF's 4 GiB ceiling on RF64 or W64 programmes. Ordinary RIFF output retains its final-container capacity admission.

The planner refuses to construct a one-process FFmpeg package command for Float64. The executor independently revalidates the exact producer and consumer tools, argv, typed endpoints, rate, channel count, target-specific RF64 flags, and environment before spawning the pipeline.

## V7 correction: independent decoded-sample identity

The decoded-sample oracle is now carrier-sensitive:

- Float64 QPCM W64 source: SoX decodes to raw f64le; FFmpeg hashes the typed raw stream.
- Float64 final W64: the same qualified SoX-to-raw route.
- Float64 RIFF/RF64 delivered output: direct FFmpeg decode and hash.
- Other enabled carriers: direct FFmpeg decode and hash.

Consequently, Float64 RIFF/RF64 preservation compares a SoX-decoded W64 source identity with an FFmpeg-decoded delivered-file identity. The defective FFmpeg W64 decoder is not used on either side of that comparison and cannot produce a common-mode pass.

Post-metadata verification records the complete ordered command transcript. A new optional `executed_evidence_digest_v3` binds the v2 authority plus every post-metadata verification command's description, binary, sanitized argv, cwd, environment policy, explicit environment, environment keys, and exit status. Historical v1-v6 manifests retain their exact serialized shape because the zero v3 field is omitted; v7 authority requires a nonzero v3 digest.

## Evidence subprocess environment

All Reference evidence-producing commands now use `ClearAndSet` with exactly:

```text
LC_ALL=C
```

This includes carrier probes, direct decoded-sample hashes, both stages of Float64-W64 streamed hashes, both stages of Float64 RIFF/RF64 packaging, measurements, toolchain probes, and post-metadata verification. Runtime tests reject inherited-environment or variable-set drift. The qualification harness also clears its environment before spawning tools.

## Capacity and topology invariants

`QPCM` remains W64 at every enabled terminal depth.

- W64 output uses QPCM directly.
- Float64 RIFF/RF64 uses the typed raw stream above.
- Other non-W64 targets package from W64 QPCM through their qualified route.
- Ordinary RIFF size admission applies to the final RIFF target.
- A 15-minute, 768-kHz, stereo Float64 W64/RF64 plan remains admissible without a RIFF intermediate.
- The public `-1.000000000 dBTP` ceiling and inherited v5 terminal bounds are unchanged.

## Immutable policy identity

The Float64 package topology, argv, typed pipeline step, sample-identity routes, semantic plan hash, runtime validation, and executed evidence changed. They are represented by the append-only identity `sox_ng_14_8_0_1_v7`.

Historical v1-v6 policy artifacts and hash-bound documents are unchanged. New v7 current/candidate/report/certification artifacts and a deterministic v7 checker are present. The current and candidate v7 manifests are byte-identical. V7 remains `qualification_candidate`; no promotion or release certification is claimed.

## F2 legacy behavior inherited from the prior correction

Before promotion, a DSD source targeting PCM visibly exposes and executes the frozen legacy gain family:

- `disabled` -> exact legacy `Disabled` wire;
- `auto` plus 0..6 dB margin -> exact legacy `Auto` wire and SoX `norm -<margin>`;
- `manual` plus -24..+24 dB -> exact legacy `Manual` wire and SoX `gain <signed dB>`.

Reference pathway/profile, Reference gain, NativeLevel, and native NormalizePeak remain hidden and disabled. Generic resampler and dither options remain available on the legacy route. Native-only selections are rejected rather than silently discarded. V4 preset acceptance/refusal behavior remains explicitly adjudicated and tested.

## Source-level regressions added for v7

- Float64 RIFF/RF64 plans contain the exact typed SoX-to-FFmpeg package pipeline.
- Direct FFmpeg decoding of Float64 QPCM W64 is structurally forbidden.
- Package validation rejects tool, endpoint, argv, rate, channel, RF64-flag, and environment drift.
- High-rate Float64 W64/RF64 planning proves there is no disk-backed RIFF intermediate.
- Float64 QPCM/final-W64 hashes use the streamed SoX route; RIFF/RF64 output hashes use direct FFmpeg.
- Carrier probes and every decoded-sample hash constructor assert exact `ClearAndSet` / `LC_ALL=C` semantics.
- V7 manifest authority requires the complete ordered post-metadata transcript digest while historical manifest serialization remains compatible.
- The v3 digest regression changes on command order, route, environment policy, or explicit environment drift, while ignoring diagnostic output tails and elapsed time.
- The deterministic v7 checker binds the package route, sample-identity contract, environment policy, and v3 evidence markers to source.

## Required verification commands

```text
python3 tonepoet-pipeline/qualification/derive_dsd_reference_v5_terminal_bounds.py --check
python3 tonepoet-pipeline/qualification/derive_dsd_reference_v6_terminal_bounds.py --check
python3 tonepoet-pipeline/qualification/derive_dsd_reference_v7_terminal_bounds.py --check
cargo fmt -p tonepoet -p tonepoet-pipeline -p sacd-rs -- --check
cargo test -p tonepoet-pipeline
cargo test -p sacd-rs
cargo test -p tonepoet --test settings_sentinel
cargo test --workspace
cargo clippy -p tonepoet -p tonepoet-pipeline -p sacd-rs \
  --all-targets --all-features -- -D warnings
TONEPOET_REQUIRE_TOOLS=1 \
TONEPOET_DSD_REFERENCE_REPORT_PATH="$PWD/tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v7_certification.json" \
  cargo test -p tonepoet --test dsd_reference_qualification -- --nocapture
```

## Assembly-environment verification and limitation

The assembly environment provides system SoX 14.4.2 and FFmpeg 7.1.3. An unpinned local route smoke reproduced the direct Float64-W64 FFmpeg hash mismatch and showed that:

1. SoX-decoded raw-f64le source identity;
2. the typed SoX-to-FFmpeg RIFF package; and
3. direct FFmpeg decode of the packaged RIFF

produce matching source/delivered identities while direct FFmpeg decode of the W64 source differs. This is route evidence only, not pinned policy qualification.

The environment does not provide Cargo, rustc, rustfmt, Clippy, Nix, Flox, or the pinned SoX-ng 14.8.0.1 closure. Therefore Rust compilation, formatting, workspace tests, Clippy, pinned-tool qualification, live smoke, and release certification could not be executed here. V7 remains fail-closed and unpromoted.
