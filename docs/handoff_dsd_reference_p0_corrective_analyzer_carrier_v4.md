# DSD Reference P0 Analyzer-Carrier Corrective Handoff

> **Superseded historical snapshot.** This document records an earlier corrective round and does not describe the current policy or handoff state. The current authority is [`docs/handoff_dsd_reference_p0_current.md`](handoff_dsd_reference_p0_current.md), with candidate policy `sox_ng_14_8_0_1_v5`.

**Date:** 2026-07-19
**Historical candidate policy at this snapshot:** `sox_ng_14_8_0_1_v4`
**Runtime exposure:** disabled until promotion; the checked-in artifact remains `qualification_candidate`

## Corrective decision

Policy v4 uses an exact, shell-free, two-process analyzer carrier:

```text
sox_ng -S -D {carrier_w64} -t wav -e floating-point -b 64 -
    stdout -> stdin
ffmpeg -nostdin -hide_banner -nostats -loglevel info \
    -f wav -i pipe:0 \
    -filter:a loudnorm=I=-23.0:LRA=7.0:TP=-1.0:print_format=json \
    -f null -
```

This retains the existing f64 W64 render/finalize authority and changes only the analyzer view. SoX-ng performs an f64-to-f64 container re-wrap; no f32 approximation, shell, temporary analyzer file, or disk-backed RIFF file is introduced.

The producer command, consumer command, direct-pipe transport, input/output modes, and `FfmpegLoudnormInputTpV2` parser identity are frozen in the plan, semantic hash, runtime contract validation, qualification manifest, and release-certification checks. Reference subprocesses additionally carry an explicit `ClearAndSet` environment policy: the launcher calls `env_clear()` and installs only the qualified allowlist (`LC_ALL=C`). Both the policy and sanitized effective environment are included in command evidence and semantic identity. Generic non-Reference commands retain inherited-environment compatibility.

Policy v3 was never promoted. No persisted qualified-release authority exists, so no migration is performed or required.

## Qualification and failure closure

The schema-version-4 commissioned gate now requires all prior P0 evidence plus these analyzer-carrier proofs:

1. Direct FFmpeg analysis of a SoX-written f64 W64 reproduces the pinned `2^31` scaling defect.
2. The typed producer-consumer path measures the same analytic -20 dBFS fixture within the existing analyzer tolerance.
3. Decoded f64 samples from the streamed WAV are byte-identical to the decoded f64 W64 source samples.
4. A valid sparse W64 fixture contains more than 4 GiB of audio payload.
5. The producer's initial WAV header uses the frozen large streaming-size sentinel class for both RIFF and data size fields.
6. FFmpeg consumes that direct stream through EOF using stream copy to the null muxer, separating transport capacity from loudnorm CPU cost.

Runtime execution rejects historical parsers, absent producer stages, argv drift, input/output-mode drift, environment-policy drift, environment drift, and any substitution of a path-backed FFmpeg input for `pipe:0`. Timeout, cancellation, stage failure, wait failure, and consumer-spawn failure enter a bounded supervisor that must obtain terminal statuses for both started children. The runner reports `Timeout` or `Cancelled` only after verified reaping; inability to establish termination becomes an explicit termination failure. Pipeline command records retain bounded diagnostics and effective environment evidence for both stages.

The commissioned gate uses explicit 20-minute command, 60-minute pipeline, and 10-second termination/reap deadlines with a 10 ms poll interval. Its ordinary commands and both analyzer pipelines use the same clear-and-set environment contract. An adversarial probe preloads an ambient poison variable and proves that the child observes only `LC_ALL=C`. Qualification report publication is serialized by a same-directory writer lock and uses a unique, synchronized same-directory temporary before atomic installation.

The release artifact remains fail-closed. Promotion still requires the exact candidate manifest and exact schema-version-4 machine report to be cryptographically bound into the release-certification descriptor.

## Additional in-scope hardening

- The strict loudnorm struct parse rejects duplicate `input_tp` members.
- Reference-pipeline concurrency permits do not inject `OMP_NUM_THREADS` or otherwise mutate the frozen producer/consumer environment.
- The `ToolRunner` trait no longer supplies a sequential pseudo-pipeline default; runners must implement real transport or return explicit unsupported-pipeline failure.
- Real-runner tests cover producer and consumer timeout, cancellation, producer and consumer nonzero exit, consumer spawn failure, closed-environment execution, terminal status capture, and Linux child disappearance.
- SACD double-SHA preflight comments state the reopen/path-race threat boundary; the checks detect ordinary mutation but are not represented as a capability guarantee.
- `sacd-rs` verifies `P0_SHA256SUMS` content and exact binary-corpus coverage during the build.
- Manifest route and execution serde tags have frozen-string tests.
- Historical v1, v2, and v3 policy values remain append-only decodable; only v4 is current.

## Changed files

```text
docs/handoff_dsd_reference_p0_corrective_analyzer_carrier_v4.md
docs/handoff_manifest.txt
src/convert/pipeline/bluray_realize.rs
src/convert/pipeline/dvda_realize.rs
src/convert/pipeline/dvdv_realize.rs
src/convert/pipeline/errors.rs
src/convert/pipeline/materializer_archive.rs
src/convert/pipeline/materializer_cue.rs
src/convert/pipeline/materializer_single.rs
src/convert/pipeline/mod.rs
src/convert/pipeline/planned_adapter.rs
src/convert/pipeline/progress/streaming.rs
src/convert/pipeline/scheduler.rs
src/convert/pipeline/stages.rs
src/convert/pipeline/tool.rs
src/convert/pipeline/track_executor.rs
src/tui/keybindings.rs
tests/dsd_reference_qualification.rs
tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v4.json
tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v4_candidate.json
tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v4_report.md
tonepoet-pipeline/src/dsd_reference.rs
tonepoet-pipeline/src/lib.rs
tonepoet-pipeline/src/plan.rs
```

## Mandatory commissioned gates

```text
cargo fmt -p tonepoet -p tonepoet-pipeline -p sacd-rs -- --check
cargo test -p tonepoet-pipeline
cargo test -p sacd-rs
cargo test -p tonepoet --test settings_sentinel
cargo test --workspace
cargo clippy -p tonepoet -p tonepoet-pipeline -p sacd-rs \
  --all-targets --all-features -- -D warnings
TONEPOET_REQUIRE_TOOLS=1 \
TONEPOET_DSD_REFERENCE_REPORT_PATH="$PWD/tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v4_certification.json" \
  cargo test -p tonepoet --test dsd_reference_qualification -- --nocapture
```

## Assembly validation and limitation

The incoming archive's self-excluding handoff manifest passed in full before modification. Rust grammar parsing, JSON parsing, manifest/corpus hash checks, policy-artifact consistency checks, source-level contract audits, and archive-level manifest verification were performed during assembly.

The assembly environment did not contain Cargo, rustc, rustfmt, Clippy, Nix, Flox, the pinned SoX-ng 14.8.0.1 closure, or a sparse-file-capable filesystem. Therefore the Rust build/test suite and mandatory commissioned greater-than-4-GiB gate could not be executed here, and no release qualification is claimed. A non-certifying host smoke test with system SoX 14.4.2 and FFmpeg 7.1.3 reproduced the direct-W64 `166.64 dBTP` defect and the streamed f64-WAV `-20.00 dBTP` correction.
