# Current DSD Reference P0 Handoff

**Date:** 2026-07-21
**Current candidate policy:** `sox_ng_14_8_0_1_v12`
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

The two carrier depths require opposite measurement routes. Policy v12 inherits v6's frozen analyzer contract:

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

The packaging transport is headerless little-endian Float64 PCM over a direct stdout-to-stdin pipe. It introduces no disk-backed RIFF intermediate and does not itself impose RIFF's 4 GiB ceiling on RF64 or W64 delivery. Policy v12's separate streamed-WAV analyzer cap still applies to every Reference plan. Ordinary RIFF output also retains its final-container capacity admission.

The planner refuses to construct a one-process FFmpeg package command for Float64. The executor independently revalidates the exact producer and consumer tools, argv, typed endpoints, rate, channel count, target-specific RF64 flags, and environment before spawning the pipeline.

## V7 correction: independent decoded-sample identity

The decoded-sample oracle is now carrier-sensitive:

- Float64 QPCM W64 source: SoX decodes to raw f64le; FFmpeg hashes the typed raw stream.
- Float64 final W64: the same qualified SoX-to-raw route.
- Float64 RIFF/RF64 delivered output: direct FFmpeg decode and hash.
- Other enabled carriers: direct FFmpeg decode and hash.

Consequently, Float64 RIFF/RF64 preservation compares a SoX-decoded W64 source identity with an FFmpeg-decoded delivered-file identity. The defective FFmpeg W64 decoder is not used on either side of that comparison and cannot produce a common-mode pass.

Post-metadata verification records the complete ordered command transcript. A new optional `executed_evidence_digest_v3` binds the v2 authority plus every post-metadata verification command's description, binary, sanitized argv, cwd, environment policy, explicit environment, environment keys, and exit status. Historical v1-v6 manifests retain their exact serialized shape because the zero v3 field is omitted; v7-and-later authority requires a nonzero v3 digest.

## V8 terminal correction: signed-32-bit effects boundary

The terminal `gain` effect is not f64 arithmetic merely because R64 and Float64 QPCM use f64 carriers. SoX-ng executes the effect in its signed-32-bit internal sample domain. Policy v8 therefore replaces the unattainable Float64 `2^-51`-only terminal bound with the summed authority `2^-32 + 2^-51` (`Q1.63 = 2^31 + 2^12`): the signed-32-bit round-to-nearest half-step plus the inherited Float64 arithmetic allowance. The safe pre-terminal ceiling is `-1.010000003 dBTP`.

The terminal audit retains Int24 at `2^-22` and Float32 at `2^-23`: both bounds already dominate the `2^-32` effects contribution with conservative margin. The per-depth decode audit retains the v7 typed route table unchanged. All enabled cells remain attainable; no enabled cell is left to fail a later qualification gate.


## V9/F5 correction: W64 metadata mutation admission

The pinned FFmpeg 7.1 W64 muxer has a third, independent W64 defect. When a
mono Int24 W64 data chunk is not divisible by W64's eight-byte alignment,
`ffmpeg -c:a copy -f w64` includes the alignment padding in the declared data
extent. The permanent qualification probe uses 8,820 samples at 88.2 kHz:
26,460 data bytes decode correctly before the rewrite, while the rewritten
file decodes to 8,821 samples with an identical 8,820-sample prefix and one
zero-valued phantom sample.

Policy v9 does not accept or hide that corruption. W64 remains a qualified
audio delivery target, but metadata mutation for W64 is unavailable by
construction:

- the Reference plan bridge rejects a W64 request with the metadata stage
  enabled before conversion begins;
- the metadata writer independently rejects the `w64` extension before
  creating a rewrite tempfile or selecting a tool;
- both paths use the stable, user-facing `DSD-REF-P0-024` authority;
- disabling the metadata stage preserves ordinary W64 delivery; and
- the commissioned gate requires all 60 W64 metadata cells to resolve to
  P0-024 rather than treating them as successful mutations.

The qualification matrix still contains 480 delivery cases. Post-metadata
decoded-sample identity remains mandatory for the 420 non-W64 cases. A
separate mono Int24 RIFF/WAV fixture uses 8,821 samples (26,463 odd data bytes)
and must remain sample-exact after the FFmpeg metadata rewrite, directly
qualifying the two-byte-alignment analog raised by F5.


## V10/F5 evidence completion: exact production metadata routes

V9's product behavior was correct, but its certification phrase overstated the
mechanism exercised by the gate. The 420 admitted metadata cells had used a
representative FFmpeg stream-copy remux even though production dispatches among
FFmpeg, `metaflac`, `wvtag`, and an AtomicParsley M4A freeform follow-up. V10
removes that surrogate authority.

`apply_metadata` and the qualification seam now delegate to the same private
per-file implementation. The commissioned matrix executes that implementation
in place for every admitted cell and records the exact route counts:

- FFmpeg primary mutation: 160 cells;
- `metaflac`: 180 cells;
- `wvtag`: 80 cells;
- AtomicParsley freeform follow-up: 20 ALAC/M4A cells.

Every admitted cell is probed again for its exact target/container contract and
decoded-sample identity after mutation. The machine report also captures each
production mutator's canonical executable path, SHA-256, and reported version.
The runtime certification validator requires this structured evidence and no
longer accepts the v9 universal claim string.
The machine contract explicitly scopes this evidence to authoritative tag
mutation without artwork embedding or ReplayGain; neither excluded operation is
claimed by the F5 gate.

The 60 W64 cells now traverse both actual enforcement implementations: the
production planner entry (`plan_request_for_track`) and the shared production
metadata implementation used by `apply_metadata`. Both must return
`DSD-REF-P0-024` in every cell, before planned work or a mutator command is
created.

Exact-route qualification exposed one adjacent F5 container invariant. FFmpeg
will normally rewrite a small RF64 input as ordinary RIFF when the output is a
`.wav` temporary. The production command builder now detects the input `RF64`
magic and emits `-rf64 always`; the matrix reruns the target probe after
metadata mutation, so an RF64-to-RIFF downgrade fails certification.

## Evidence subprocess environment

All Reference evidence-producing commands now use `ClearAndSet` with exactly:

```text
LC_ALL=C
```

This includes carrier probes, direct decoded-sample hashes, both stages of Float64-W64 streamed hashes, both stages of Float64 RIFF/RF64 packaging, measurements, toolchain probes, the exact production metadata discovery/mutation commands, and post-metadata verification. Runtime tests reject inherited-environment or variable-set drift. The qualification harness also clears its environment before spawning tools.

## Capacity and topology invariants

`QPCM` remains W64 at every enabled terminal depth.

- W64 output uses QPCM directly.
- Float64 RIFF/RF64 uses the typed raw stream above.
- Other non-W64 targets package from W64 QPCM through their qualified route.
- Ordinary RIFF size admission applies to the final RIFF target.
- Every Reference plan is admitted only when its required Float64 analyzer stream is bounded to at most 4,294,967,237 audio bytes.
- The commissioned real-tool gate requires exact header fields at the largest frame-aligned admitted carrier, rejects the immediately following carrier, and scans the contiguous frame-aligned transition through the frozen 4 GiB + 8 witness to locate the actual first RIFF-field wrap.
- At 768 kHz stereo, a five-minute programme remains below the cap; a six-minute programme is rejected with `DSD-REF-P0-025`. RF64 and W64 do not bypass this analyzer-carrier bound.
- The public `-1.000000000 dBTP` ceiling is unchanged. Int24 and Float32 retain their v5 bounds; Float64 uses the corrected v8 signed-32-bit effects bound.


## FFmpeg rewrite attribute contract

The shared same-directory FFmpeg rewrite primitive is an atomic content-and-attribute replacement, not a disposable-file shortcut. It snapshots the original regular file before the mutator runs, rejects target substitution, restores and verifies permissions and access/modification timestamps, preserves uid/gid on Unix, preserves the complete Linux xattr set including POSIX ACL xattrs, syncs the replacement, atomically renames it, and syncs the parent directory. Any identity or governed-attribute drift detected by the two pre-publication checks fails closed before replacement.

## Immutable policy identity

Policy v12 is append-only. It inherits v11's runtime-bound metadata-mutator
contract and all earlier numerical terminal bounds, package topology, decode
routes, semantic plan normalization, and enabled audio-delivery cells. The new
identity exists solely because F6 adds a fail-closed capacity boundary to the
required unseekable Float64 WAV transport.

Historical v1-v11 policy JSON, candidate, certification, and report artifacts
remain byte-identical. The historical deterministic checkers are extended only
to recognize v12 as the active successor while retaining their inherited
policy assertions. New v12 current/candidate/report/certification artifacts and
a deterministic v12 checker are present. The current and candidate v12
manifests are byte-identical. V12 remains `qualification_candidate`; no
promotion or release certification is claimed.

## F2 legacy behavior inherited from the prior correction

Before promotion, a DSD source targeting PCM visibly exposes and executes the frozen legacy gain family:

- `disabled` -> exact legacy `Disabled` wire;
- `auto` plus 0..6 dB margin -> exact legacy `Auto` wire and SoX `norm -<margin>`;
- `manual` plus -24..+24 dB -> exact legacy `Manual` wire and SoX `gain <signed dB>`.

Reference pathway/profile, Reference gain, NativeLevel, and native NormalizePeak remain hidden and disabled. Generic resampler and dither options remain available on the legacy route. Native-only selections are rejected rather than silently discarded. V4 preset acceptance/refusal behavior remains explicitly adjudicated and tested.

## Source-level regressions retained through v12

- Float64 RIFF/RF64 plans contain the exact typed SoX-to-FFmpeg package pipeline.
- Direct FFmpeg decoding of Float64 QPCM W64 is structurally forbidden.
- Package validation rejects tool, endpoint, argv, rate, channel, RF64-flag, and environment drift.
- High-rate Float64 W64/RF64 planning proves there is no disk-backed RIFF intermediate.
- Float64 QPCM/final-W64 hashes use the streamed SoX route; RIFF/RF64 output hashes use direct FFmpeg.
- Carrier probes and every decoded-sample hash constructor assert exact `ClearAndSet` / `LC_ALL=C` semantics.
- V10 manifest authority requires the complete ordered post-metadata transcript
  digest while historical manifest serialization remains compatible.
- The v3 digest regression changes on command order, route, environment policy, or explicit environment drift, while ignoring diagnostic output tails and elapsed time.
- The deterministic v7 checker continues to bind the historical package route, sample-identity contract, environment policy, and v3 evidence markers to source.
- The deterministic v8 checker retains the signed-32-bit effects derivation and source proof.
- The deterministic v9 checker retains the historical P0-024 admission proof.
- The deterministic v10 checker binds the exact production implementation,
  160/180/80 primary-route counts, 20 AtomicParsley follow-ups, both 60-cell
  W64 production boundaries, RF64 preservation, tool identities, and the
  narrowed structured certification fields.
- The deterministic v11 checker retains exact runtime binding from certified
  mutator identity to compiled store path, runner resolution, and bound execution.
- The deterministic v12 checker binds the 32-bit streamed-WAV arithmetic,
  permanent modulo-wrap fixture, planner guard frame, `DSD-REF-P0-025`, and
  runtime report validation while forbidding the former sentinel/read-to-EOF claim.

## Required verification commands

```text
python3 tonepoet-pipeline/qualification/derive_dsd_reference_v5_terminal_bounds.py --check
python3 tonepoet-pipeline/qualification/derive_dsd_reference_v6_terminal_bounds.py --check
python3 tonepoet-pipeline/qualification/derive_dsd_reference_v7_terminal_bounds.py --check
python3 tonepoet-pipeline/qualification/derive_dsd_reference_v8_terminal_bounds.py --check
python3 tonepoet-pipeline/qualification/derive_dsd_reference_v9_metadata_admission.py --check
python3 tonepoet-pipeline/qualification/derive_dsd_reference_v10_production_metadata.py --check
python3 tonepoet-pipeline/qualification/derive_dsd_reference_v11_runtime_mutator_binding.py --check
python3 tonepoet-pipeline/qualification/derive_dsd_reference_v12_streamed_wav_capacity.py --check
cargo fmt -p tonepoet -p tonepoet-pipeline -p sacd-rs -- --check
cargo test -p tonepoet-pipeline
cargo test -p sacd-rs
cargo test -p tonepoet --test settings_sentinel
cargo test --workspace
cargo clippy -p tonepoet -p tonepoet-pipeline -p sacd-rs \
  --all-targets --all-features -- -D warnings
TONEPOET_REQUIRE_TOOLS=1 \
TONEPOET_DSD_REFERENCE_REPORT_PATH="$PWD/tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v12_certification.json" \
  cargo test -p tonepoet --test dsd_reference_qualification -- --nocapture
```

## Assembly-environment verification and limitation

The assembly environment provides system SoX 14.4.2 and FFmpeg 7.1.3. An unpinned local route smoke reproduced the direct Float64-W64 FFmpeg hash mismatch and showed that:

1. SoX-decoded raw-f64le source identity;
2. the typed SoX-to-FFmpeg RIFF package; and
3. direct FFmpeg decode of the packaged RIFF

produce matching source/delivered identities while direct FFmpeg decode of the W64 source differs. This is route evidence only, not pinned policy qualification.

The environment does not provide Cargo, rustc, rustfmt, Clippy, Nix, Flox, or
the pinned SoX-ng 14.8.0.1 closure. Therefore Rust compilation, formatting, workspace tests, Clippy, pinned-tool
qualification, live smoke, and release certification could not be executed here.
V12 remains fail-closed and unpromoted.

## V11/F5 runtime completion: certified mutator identity is executed

V10 qualified the exact production metadata routes and captured the identities
of `metaflac`, `wvtag`, and AtomicParsley, but those report values were not yet
a runtime execution boundary. V11 closes that final gap without changing any
qualified audio cell or metadata-route count.

For every Reference conversion with the metadata stage enabled:

- the three mutator package/store paths are compiled into the Tonepoet binary;
- the packaged activation path, compiled store executable, runner-resolved
  executable, and report-certified canonical path must be identical;
- custom `ProcessorConfig.tool_paths` entries or ambient `PATH` substitutions
  that resolve any executable other than the certified canonical path are
  rejected;
- the certified executable SHA-256 and normalized reported version are checked
  during Reference toolchain admission;
- path, SHA-256, version, and closure identity are checked again immediately
  before metadata mutation;
- the production metadata runner executes FFmpeg, `metaflac`, `wvtag`, and
  AtomicParsley through exact canonical-path-plus-SHA-256 authority;
- alternate `ToolRunner` implementations fail closed unless they explicitly
  implement the same bound-execution contract; and
- the mutator identities are persisted in `ReferenceToolchainEvidence` and
  included in `execution_fingerprint_v1`, binding them into per-output
  execution authority.

The v11 commissioned report must emit the structured
`runtime_metadata_mutator_binding` contract. Runtime certification rejects a
missing or altered contract, a report path/hash/version that differs from the
compiled package, or an execution runner that resolves another binary.

V11 remains an unpromoted append-only qualification candidate. Historical
v1-v10 policy JSON, candidate, certification, and report artifacts remain
byte-identical. Historical derivation checkers were updated only to recognize
v11 as the active append-only successor while continuing to pin the exact
historical artifact hashes. The current and candidate v11 manifests are
byte-identical, and runtime activation remains fail-closed until a passing
schema-v11 report is bound into a promoted manifest.

## V12/F6 completion: bounded unseekable Float64 WAV transport

The pinned SoX-ng 14.8.0.1 writer truncates unseekable WAV RIFF and data sizes
to 32 bits. The permanent sparse fixture declares 536,870,913 mono Float64
frames (4 GiB + 8 audio bytes), and the pinned reader reports that exact frame
count from the W64 authority. The writer emits RIFF size `58` and data size `8`;
only the data field is the exact modulo-2^32 value, while the RIFF field collapses
to the header-only size. V12 records this as a
reproduced defect. It does not reinterpret either field as a streaming sentinel
and makes no downstream read-to-EOF completeness claim.

Every Reference conversion uses a Float64 WAV stream for pre-terminal analyzer
authority. Planner admission therefore computes a checked upper bound using
`ceil(duration_ns * target_rate_hz / 1e9)`, adds one output frame for duration
quantization and resampler endpoint rounding, and multiplies by channel count
and eight bytes per sample. The predicted audio payload must not exceed
`u32::MAX - 58 = 4,294,967,237` bytes. Missing duration, arithmetic overflow,
or excess capacity fails before execution with `DSD-REF-P0-025`.

Ordinary RIFF output retains its existing `DSD-REF-P0-018` preflight and error
precedence. RF64, W64, and other containers remain subject to the analyzer
stream cap. A later lift requires a new append-only policy backed by a corrected
SoX-ng pin and renewed closure/behavior qualification, or by an independently
qualified sample-exact transport beyond 4 GiB.
