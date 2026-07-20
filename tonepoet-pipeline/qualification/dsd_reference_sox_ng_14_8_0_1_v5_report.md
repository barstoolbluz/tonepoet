# DSD Reference Policy v5 Qualification Report

**Policy:** `sox_ng_14_8_0_1_v5`  
**Report schema:** `tonepoet-dsd-reference-policy-qualification-report/v1`  
**Policy evidence state:** QUALIFICATION CANDIDATE  
**Implementation certification:** not granted by this source-controlled report. Runtime activation remains disabled until the mandatory build and commissioned real-tool gates pass unchanged and their exact machine report is cryptographically bound into a promoted artifact.

## Corrective authority

Policy v5 is an append-only correction to the terminal gain boundary used by policy v4. Historical v1 through v4 identities and artifacts remain decodable and are not executable as the current policy. The v4 release was never promoted, so there is no persisted qualified-release authority to migrate. No migration is performed or required.

The v4 streamed analyzer carrier remains unchanged. The corrective defect is that gain binding consumed a conservative pre-terminal analyzer value and targeted the terminal epsilon boundary with no allowance for the independent post-final analyzer report to quantize one 0.01 dB reporting step upward. The real chain could therefore be safe in continuous amplitude yet honestly report a conservative post-final upper bound of -0.99 dBTP and fail the unchanged -1.00 dBTP gate.

Policy v5 reserves exactly one analyzer reporting quantum, `0.010000000` dB, before applying the existing terminal realization epsilon. The resulting safe pre-terminal ceilings are `-1.010002327` dBTP for Int24 TPDF, `-1.010001164` dBTP for Float32, and `-1.010000001` dBTP for Float64. The public ceiling and post-final acceptance predicate remain exactly `-1.000000000` dBTP.

The commissioned toolchain established that FFmpeg 7.1 scales SoX-ng 14.8.0.1 64-bit IEEE-float W64 samples by exactly `2^31` when FFmpeg demuxes that W64 directly. A −20 dBFS fixture therefore reports `+166.64 dBTP`. SoX-ng reads the same W64 correctly, and the render/finalize chain remains sample-exact. The defect is confined to the analyzer's direct W64 view.

## Pre-promotion runtime routing

Runtime defaults follow corrective choice (a). `DsdSettings::default()` serializes and executes as the exact frozen `LegacyFlatV1` compatibility view until a Reference policy is promoted. Native-v2 remains available only through an explicit origin migration (`DsdSettings::native_v2()`, `migrate_to_native_v2()`, a native-v2 serialized wire, or an explicit CLI DSD Reference flag). The Reference planner and runtime attestation remain fail-closed; no candidate-policy fallback or half-native hybrid is permitted. A permanent real-tool gate converts a deterministic DSD64 DSF fixture to 88.2-kHz 24-bit FLAC using otherwise-default settings, verifies the legacy route, executes the planner-emitted argv under its declared environment policy, publishes the work file atomically, and probes the resulting stream.

## SACD planning I/O policy

SACD Reference cells remain unavailable in P0. The plan bridge therefore rejects an explicitly native SACD request with `DSD-REF-P0-023` without opening or parsing the ISO. SACD TOC selection, original-source identity, materialization identity, and double-SHA mutation checks belong wholly to executor preflight once a future policy actually admits SACD cells. Plan construction must not perform TOC I/O merely to reach a deterministic unavailable-cell rejection. Legacy SACD planning is unchanged and performs no new Reference probe.

## Frozen analyzer carrier

Every true-peak measurement is a typed two-process step with no shell and no disk-backed analyzer copy:

```text
producer: sox_ng -S -D {carrier_w64} -t wav -e floating-point -b 64 -
transport: producer stdout connected directly to consumer stdin
consumer input: ffmpeg -f wav -i pipe:0 ...
```

The producer performs an f64-to-f64 container re-wrap. For streams whose length cannot be represented in ordinary RIFF, SoX emits the streaming size sentinel and FFmpeg must read to EOF. This avoids the on-disk RIFF 4 GiB ceiling while preserving the f64 sample representation. The producer identity, complete producer argv, direct-pipe transport, consumer input argv, parser identity, analyzer argv, environment policy, and explicit environment are all part of the immutable plan and semantic hash.

All Reference external commands use `ClearAndSet`: the process launcher clears the inherited environment and installs only `LC_ALL=C`. Qualification applies the same policy and includes an adversarial child probe that preloads an ambient poison variable before clearing it. The machine report must contain the manifest environment contract, the probe result, and sanitized command evidence. Any change to environment inheritance or the allowlist requires a new append-only policy identity and fresh evidence.

The qualification gate must prove all of the following on the pinned closure:

1. Direct FFmpeg analysis of a SoX-written f64 W64 reproduces the known `2^31` scaling defect.
2. The frozen typed SoX-to-FFmpeg path measures the same −20 dBFS fixture as −20 dBTP within the existing analyzer tolerance.
3. The producer emits a valid f64 RIFF/WAVE stream. For the greater-than-4-GiB case, qualification captures the initial stream header, requires both RIFF and data size fields to use the frozen large-size sentinel class (`>= 0x7fff0000`), and proves that FFmpeg reads through EOF.
4. A sparse, valid f64 W64 carrier with more than 4 GiB of audio payload traverses the same producer and FFmpeg input contract successfully; the proof consumer may stream-copy to the null muxer so this capacity proof does not conflate transport with loudnorm CPU cost.
5. The existing 1,200-case analyzer matrix passes through the production planner, typed pipeline, strict report extractor, and strict parser.
6. The real-tool gate enforces a 20-minute deadline for individual commands, a 60-minute deadline for producer-consumer pipelines, and a 10-second terminate/reap deadline. Every failure path retains bounded stage diagnostics and must obtain terminal statuses for all started children or fail explicitly as a supervision failure.
7. Concurrent report writers are rejected by a same-directory lock; the winning writer synchronizes a unique same-directory temporary and atomically installs it before synchronizing the parent directory.

A future FFmpeg W64 fix does not authorize direct W64 measurement under policy v5. Changing or removing this carrier requires another append-only policy ID and fresh evidence.

## Unchanged authority

All policy-v4 source admission, profile selection, analyzer-carrier behavior, package/decode-back rules, cell contract, rejection precedence, and deferred surfaces remain unchanged. Enabled source cells are native DSF and DSDIFF/DSD at DSD64/128/256 mono/stereo, plus DSDIFF/DST DSD64 stereo. SACD, Int16, Manual workflows, the in-TUI builder, and lossy delivery remain unavailable.

The expanded supported-cell count remains `13,248`; the canonical cell-contract digest remains `8655f32296e3ac0012357c321cae026eb0effbcb3e128d5a1fad673fe12927a3`.

## Mandatory gates

```text
cargo fmt -p tonepoet -p tonepoet-pipeline -p sacd-rs -- --check
cargo test -p tonepoet-pipeline
cargo test -p sacd-rs
cargo test -p tonepoet --test settings_sentinel
cargo test --workspace
cargo clippy -p tonepoet -p tonepoet-pipeline -p sacd-rs \
  --all-targets --all-features -- -D warnings
TONEPOET_REQUIRE_TOOLS=1 \
  cargo test -p tonepoet --test dsd_reference_qualification -- --nocapture
```

The final gate must emit schema-version-5 machine evidence, including the default-settings DSD64 DSF-to-FLAC live smoke, exact tool identities, the clear-and-set subprocess environment contract and adversarial isolation probe, qualification deadline/supervision policy, the analyzer-carrier defect/correction/capacity proofs, D1 responses, the 1,200-case analyzer digest, production source-front-end results, gain/terminal-chain results, primitive DST corpus results, all 480 planner-derived package cases, all 60 enabled terminal-bound cells, the unchanged v4 cell count/digest, and a pass outcome.
