# DSD Reference Policy v7 Qualification Report

**Policy:** `sox_ng_14_8_0_1_v7`
**Report schema:** `tonepoet-dsd-reference-policy-qualification-report/v1`
**Policy evidence state:** QUALIFICATION CANDIDATE
**Implementation certification:** not granted. Runtime activation remains fail-closed until the complete build, formatting, lint, workspace, pinned-tool qualification, live-smoke, and release-certification gates pass unchanged and bind this exact candidate.

## Corrective authority

Policy v7 is an append-only correction to policy v6. Policies v1-v6 and their evidence remain immutable and history-only. V7 inherits the v6 carrier-sensitive analyzer contract and the v5 terminal bounds, but changes Float64 RIFF/RF64 packaging and decoded-sample verification. Those changes alter executable topology, argv, environment authority, semantic plan identity, and verification evidence, so they receive the new immutable identity `sox_ng_14_8_0_1_v7`.

## Float64 RIFF/RF64 packaging

V6 correctly established that FFmpeg 7.1 directly decoding a SoX-ng Float64 W64 carrier scales samples by approximately `2^31`. V7 therefore forbids direct FFmpeg input from Float64 QPCM W64 for RIFF and RF64 packaging.

The only admitted Float64 RIFF/RF64 package topology is a typed, shell-free two-process pipeline:

```text
sox_ng -S -D QPCM.w64 -t raw -e floating-point -b 64 -L -
| ffmpeg -y -hide_banner -nostdin -f f64le \
    -ar SAMPLE_RATE_HZ -ac CHANNELS -i pipe:0 \
    -map 0:a:0 -map_metadata -1 -vn -sn -dn \
    -c:a pcm_f64le -f wav [ -rf64 always ] OUTPUT
```

Both producer and consumer use `ClearAndSet` with exactly `LC_ALL=C`. The transport is headerless, explicitly little-endian raw Float64 PCM. Sample rate and channel count are bound from the immutable terminal contract on the FFmpeg input side. The stream is direct stdout-to-stdin, read to EOF, and never creates a disk-backed or RIFF-framed intermediate. Ordinary RIFF remains subject to final-target capacity admission; RF64 does not inherit the RIFF 4 GiB ceiling.

The planner represents the route as a typed `PlannedCommandPipeline`. The semantic plan hash binds both commands, typed endpoints, exact argv, environment policy, and environment. The executor rejects a pipeline whose policy, target, depth, paths, endpoints, tools, argv, or environment differs from the immutable plan summary.

## Independent decoded-sample identity

V7 removes the v6 common-mode verification oracle for Float64 RIFF/RF64:

- Float64 W64 QPCM source identity is computed only through the qualified SoX little-endian raw-f64 stream into FFmpeg's sample-hash sink.
- Packaged Float64 RIFF/RF64 identity is computed by direct FFmpeg decode of the delivered WAV/RF64 object.
- Equality therefore compares independently routed source and destination decoders. FFmpeg's defective direct Float64-W64 decoder cannot validate output produced from its own defective decode.
- Float64 W64 final-output verification uses the same SoX raw-stream route; non-Float64 carriers retain their qualified direct routes.

All hash and carrier-probe subprocesses use `ClearAndSet` with exactly `LC_ALL=C`. The frozen v1 and v2 executed-evidence digests remain byte-contract compatible. V7 adds `executed_evidence_digest_v3`, which binds every ordered post-metadata producer and consumer command, its typed role, sanitized argv, environment policy, explicit environment, and exit outcome without incorporating nondeterministic elapsed time or output tails.

## Inherited policy surface

V7 leaves the v6 analyzer route, legacy pre-promotion DSD gain behavior, supported cell matrix, gain and terminal bounds, public `-1.000000000 dBTP` ceiling, profile selection, source admission, target/depth matrix, and no-disk-RIFF-QPCM invariant unchanged.

The expanded supported-cell count remains `13,248`; the canonical cell-contract digest remains `8655f32296e3ac0012357c321cae026eb0effbcb3e128d5a1fad673fe12927a3`.

## Required regressions

Qualification must prove:

- Float64 RIFF and RF64 plans contain the exact typed SoX-to-FFmpeg package pipeline and no direct `ffmpeg -i QPCM.w64` package command.
- Float32 and integer package routes remain the previously qualified direct paths.
- Float64 source hashes use the SoX-stream authority while RIFF/RF64 output hashes use direct FFmpeg decode.
- A deliberately substituted direct-FFmpeg Float64-W64 source hash cannot satisfy the v7 route validator.
- carrier probes, direct hashes, streamed hashes, analyzer commands, package commands, and package pipelines all clear the environment and set only `LC_ALL=C`.
- RF64 and W64 retain W64 QPCM and no disk-backed RIFF intermediate, including the high-rate capacity cases.
- semantic-plan and v7 executed-evidence digests change if either pipeline command, endpoint, argv, environment policy, or environment changes, while historical v1/v2 digest contracts remain unchanged.
- every inherited v6 qualification cell and legacy-gain regression remains green.

## Mandatory gates

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

The tool-gated report must be schema version 7 and bind this exact candidate. This source tree does not claim those gates have run.
