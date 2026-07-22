# Current DSD Reference P0 Handoff

**Date:** 2026-07-22
**Current candidate policy:** `sox_ng_14_8_0_1_v16`
**Runtime exposure:** fail-closed until promotion; ordinary defaults remain exact legacy behavior
**Supersedes:** all earlier `handoff_dsd_reference_p0_*` snapshots for current-state claims

## Admission and routing authority

`tonepoet_pipeline::selects_reference_dsd_to_pcm()` remains the sole admission predicate for native-v2 Reference planning and Reference-only preflight work. It requires:

1. native-v2 DSD settings;
2. an authoritative DSD source classification;
3. a non-DSD target.

Native-v2 DSD-to-DSD requests remain on the ordinary DSD topology. Pre-promotion defaults remain the exact legacy flat settings origin and do not enter the Reference planner.

## F10 corrective re-resolution (policy v16)

Policy v16 supersedes the rejected v15 F10 response. It does not treat SoX
metadata probing as structural authority and never publishes a W64 carrier whose
declared extents disagree with its physical bytes.

Before any W64 terminal QPCM can reach metadata work or the final atomic rename,
production invokes the independent `validate_exact_w64_pcm()` parser. The parser
requires the root-declared extent to equal the physical file extent, traverses
every chunk to the exact end of file, validates 8-byte alignment and zero padding,
rejects duplicate or missing required chunks, requires canonical format/fact
payloads, checks rate/channels/depth/encoding/block alignment/byte rate, derives
the exact frame count from the structurally valid R64 data extent and requires
terminal QPCM to match it, and rejects undeclared
trailing bytes. Failure is fail-closed with stable diagnostic
`DSD-REF-P0-026` before publication. A structurally accepted carrier must then
complete an independent FFmpeg `-xerror` decode traversal. SoX `--i` is
supplemental metadata evidence only.

The release gate defines a 60-cell W64 characterization matrix covering Int24,
Float32, and Float64 at every enabled rate and mono/stereo. Each cell scans the
`2^-96` through `2^-1` power-of-two region, then brackets the observed transition
with 256 ordered amplitudes at `2^e / 510` resolution. It then tests all-zero,
immediately-below-power-boundary, at-boundary, leading-silence, and
trailing-silence fixtures. The gate inspects payload bytes,
requires exact declared extents and exact frame counts, and independently decodes
controls to prove that each intended nonzero sample remains nonzero. The policy
therefore makes the narrower claim that the defect is associated with an encoded
all-zero result after depth/effects quantization; any input-level threshold is
measured per cell rather than assumed.

For direct W64 delivery, QPCM and packaged output are intentionally the same
path. Their equal sample hashes are recorded only as identity continuity. The
execution evidence uses `direct_w64_qpcm_exact_delivery`, and manifest authority
rejects any v16 W64 evidence that claims an independent package decode
comparison. Non-W64 targets continue to require independent decoded-package
comparison.

Policy v15 remains byte-frozen as a rejected historical candidate. Policy v16
is the append-only successor and remains `not_run`, fail-closed, and unpromoted
until compilation, formatting, warning-denied lint, the complete workspace
suite, the exact pinned tool closure, live smoke, throughput qualification, the
60-cell W64 matrix, and release-report binding all pass.

## F9 corrective return

The append-only checker lineage is restored. Every v5-v15 checker validates its
own immutable artifacts and persistent policy identity without asserting the
mutable current-policy embed pointer, and the complete historical chain passes
together.

Measurement-contract validation now derives the expected carrier from the
trusted Reference summary and measurement purpose. Direct inputs and Float32
producer inputs must match that authority, and the producer transport shape is
validated independently. Crossed-carrier negative coverage spans W64, RIFF, and
RF64.

Verified-silence decoding now consumes an opaque decoded-carrier authority and
obeys the existing route table. Float64 W64 uses SoX to emit headerless f64le;
it is never opened directly by FFmpeg. Qualification records a matched
short-silence/short-nonzero probe plus a long-silence probe, classifies the
pinned FFmpeg open defect, and runtime certification validation requires the
canonical evidence and production disposition.

Static assembly and all historical derivation checkers were rerun. That F9
return left v15 fail-closed and unpromoted; v15 is now a frozen rejected
historical candidate and current v16 remains fail-closed pending commissioned
qualification.

## V15/F8 operational hardening

Policy v15 retains v14's measurement-only 16x SoX true-peak architecture and
changes only its executor-liveness, analyzer-evidence, and timeout authority.
Historical v14 qualification and certification artifacts remain byte-identical;
the checker-only mutable-pointer assertion is corrected.

Every production Reference two-tool pipeline acquires a deduplicated family set
through one RAII guard before process launch. The frozen global order is SoX,
then FFmpeg, then SSRC; FFprobe shares the FFmpeg family. This order is
independent of producer-to-consumer direction, so the Float32 FFmpeg-to-SoX
analyzer cannot circular-wait with a SoX-to-FFmpeg Float64 package or
verification pipeline. Cancellation or semaphore closure while acquiring a
later family drops the complete partial set. Deterministic asynchronous tests
use barriers to force the former circular ownership, prove both opposite route
declarations complete under the new protocol, verify family deduplication, and
verify partial-acquisition release without sleep-based interleaving.

The analyzer authority is explicitly decomposed rather than inferred entirely
from the ideal grid calculation:

```text
ideal 16x grid component:              0.041925957 dB
pinned-resampler empirical component: 0.058074043 dB
analyzer residual E:                   0.100000000 dB
reporting quantization Q:              0.010000000 dB
one-sided total Q + E:                 0.110000000 dB
```

The analytic grid term is source-derived. The pinned-resampler term remains a
separate empirical authority requiring the exact SoX-ng 14.8.0.1 gate. Schema
v6 retains the 1,968 analytic/fixed-frequency cases and adds 200 impulse,
near-band-edge burst, alternating-sign, deterministic-broadband, and
boundary-transient cases at early and late positions across all enabled rates
and mono/stereo. Those cases compare the production 16x result with a 64x
pinned-tool qualification oracle. The 2,168-case matrix is not represented as
a coefficient-derived universal filter bound, and activation remains
fail-closed until the empirical component passes and is certified.

Analyzer timeouts are workload-derived. For guarded frames
`ceil(duration_ns * rate / 1e9) + 1`, workload is frames times channels times
16. The deadline is 120 seconds plus one second for each started block of one
million oversampled sample values. Existing admission bounds cap the workload
at 8,589,934,480 sample values and the deadline at 8,710 seconds. Both members
of the Float32 FFmpeg-to-SoX pipeline receive the same deadline. The plan
summary stores that exact value, the v15 semantic hash binds it, and runtime
validation requires exact command-to-summary equality. The release gate must
prove the pinned analyzer meets the one-million-sample-values/second throughput
floor.

## V14/F8 foundation: rate-independent true-peak measurement

FFmpeg 7.1 `loudnorm` is not a qualified true-peak authority across the enabled
high-rate matrix. The fixed-frequency admission evidence showed sample-peak-only
behavior at 192 kHz and a separate unqualified response at 352.8 kHz. Policy
v14 therefore removes `loudnorm` from production true-peak measurement rather
than widening the unchanged `Q + E = 0.110000000 dB` authority or redefining
dBTP as sample peak.

Every measurement now creates a measurement-only 16x view with pinned SoX-ng
`rate -v -L -s` and reads the resulting `Pk lev dB` from `stats`:

```text
Int24/Float64 W64:
  sox -S -D <carrier.w64> -n rate -v -L -s <sample_rate_x16> stats

Float32 W64:
  ffmpeg -nostdin -hide_banner -nostats -loglevel error -i <carrier.w64> \
    -map 0:a:0 -vn -sn -dn -c:a pcm_f64le -f f64le pipe:1
  | sox -S -D -t raw -e floating-point -b 64 -L \
    -r <sample_rate> -c <channels> - -n \
    rate -v -L -s <sample_rate_x16> stats
```

The Float32 producer retains the already-qualified FFmpeg decode seam because
SoX-ng 14.8.0.1 mis-scales its Float32 W64 on readback. The transport is
headerless f64le over a direct, shell-free stdout-to-stdin pipe; all other
depths use the direct path-backed SoX route. Neither route changes the render,
terminal realization, QPCM, packaging, metadata, or delivered audio.

For a signal bandlimited to the original Nyquist frequency, sampling the
reconstructed view at 16x has a worst-case sinusoidal grid miss of
`-20 log10(cos(pi / 32)) = 0.041925956... dB`. Policy v14 rounds this upward
to `0.041925957 dB`, which remains inside the inherited `0.100000000 dB`
analyzer residual. The strict `sox_stats_pk_lev_db_v1` parser accepts exactly one C-locale
`Pk lev dB` row, requires the mono or Overall-plus-per-channel shape bound to the
planned channel count, and uses the Overall value; `-inf` still requires an
independent signed-zero proof.

The historical v14/v5 analyzer gate expanded from 1,200 to 1,968 cases. It retains normalized
single-tone and phase-aligned multitone families, adds fixed 1, 20, 48, and
70 kHz fixtures where in band, covers early and tail positions, and exercises
every enabled target rate including 352.8 and 384 kHz.

## V7 correction: Float64 RIFF/RF64 packaging

V6 correctly avoided FFmpeg's defective direct Float64-W64 decoder for measurement, but still used that decoder when packaging Float64 QPCM to RIFF or RF64. The preservation check then decoded both sides with FFmpeg, creating a common-mode false oracle.

V7 removes that route completely. Float64 RIFF/RF64 packaging is one typed, shell-free two-process operation:

```text
SoX:    -S -D <qpcm.w64> -t raw -e floating-point -b 64 -L -
FFmpeg: -f f64le -ar <rate> -ac <channels> -i pipe:0 ... -c:a pcm_f64le -f wav [ -rf64 always ] <output>
```

The packaging transport is headerless little-endian Float64 PCM over a direct stdout-to-stdin pipe. It introduces no disk-backed RIFF intermediate and does not itself impose RIFF's 4 GiB ceiling on RF64 or W64 delivery. Current policy v16 conservatively retains v13's corrected streamed-WAV capacity guard for every Reference plan, although the v15 analyzer itself no longer uses that carrier. Ordinary RIFF output also retains its final-container capacity admission.

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
- Every Reference plan conservatively retains v13's Float64 streamed-WAV admission bound of at most 4,294,967,245 audio bytes; the largest whole-frame mono payload is 4,294,967,240 bytes. The current v15 analyzer is path-backed or headerless raw and does not consume this allowance.
- The commissioned real-tool gate requires exact header fields at the largest frame-aligned admitted carrier, rejects the immediately following carrier, and scans the contiguous frame-aligned transition through the frozen 4 GiB + 8 witness to locate the actual first RIFF-field wrap.
- At 768 kHz stereo, a five-minute programme remains below the retained cap; a six-minute programme is rejected with `DSD-REF-P0-025`. RF64 and W64 do not bypass this conservative inherited admission rule.
- The public `-1.000000000 dBTP` ceiling is unchanged. Int24 and Float32 retain their v5 bounds; Float64 uses the corrected v8 signed-32-bit effects bound.


## Executor cleanup ownership

The production per-track executor establishes shared RAII cleanup ownership
before resetting its deterministic work directory. Its complete fallible body
runs inside one nested async block; planner cleanup paths are registered as soon
as each authoritative plan exists, and normal completion performs checked
cleanup exactly once. A successful conversion is converted to an explicit error
if governed scratch cleanup fails while retaining the complete successful
command transcript for diagnostics and audit; a primary conversion error
retains its own identity and reports any additional cleanup failure.

Reference source materialization runs in `spawn_blocking`, so the executor gives
the worker a cleanup lease and a child cancellation token guarded by
`CancellationToken::drop_guard()`. Dropping or aborting the outer future requests
cancellation immediately, but work-root removal is deferred until the blocking
worker has exited and surrendered its lease. A process-local per-work-root
semaphore remains owned by that shared supervisor, so a retry cannot reset the
deterministic directory until the prior blocking worker has exited and deferred
cleanup has run. Cleanup-authority acquisition selects between that semaphore and
the job cancellation token, with cancellation taking precedence, so a cancelled
duplicate or retry does not remain parked behind a pathological blocking worker.
The worker therefore cannot race completed cleanup or a later attempt by
recreating or writing beneath the shared work root. Cleanup during task
destruction remains best-effort and emits a warning on failure because no caller
remains to receive an error.

The pure planner declares the admitted source, admitted-source temporary,
canonical DST output and temporary, SACD extraction output and temporary, and
signed-zero raw stream in the authoritative cleanup vector. Runtime derives no
PID- or clock-named Reference materialization files and validates the complete
scratch set against both the admitted and rematerialized plans before use. The
signed-zero verifier retains its incremental scan and gives the planned raw
stream a dedicated RAII guard.

Production-path tests invoke `execute_planned_track_conversion()` itself for the
ordinary execution boundaries and for the three admitted Reference barriers:
before scratch-path creation, during source copying, and during DST decoding.
Because the supplied v15 artifact remains deliberately unpromoted, those three
tests use a `cfg(test)` task-local seam to skip release attestation only; they
still enter the real planner, cleanup supervisor, `spawn_blocking` materializer,
and outer production-executor lifetime. A fourth harness exercises the same
blocking-worker lease directly at the intentionally unadmitted SACD extraction
seam, so later SACD admission cannot regress lifetime ownership. Each test
aborts the outer task, proves cleanup is not allowed to race ahead of the live
worker, proves a retry cannot acquire the deterministic work root, releases the
worker, and proves that the work root and every governed scratch path remain
absent. Signed-zero tests cover decoder failure, decoder cancellation, and
success, a cancellation test proves a retry blocked behind retained work-root
authority wakes promptly without disturbing the active worker, and cleanup-error
tests prove both that an unremovable governed path is reported and that a
successful command transcript is retained on that terminal error. These tests
are included in the workspace but were not executed in the assembly environment
described below.

## FFmpeg rewrite attribute contract

The shared same-directory FFmpeg rewrite primitive is an atomic content-and-attribute replacement, not a disposable-file shortcut. It snapshots the original regular file before the mutator runs, rejects target substitution, restores and verifies permissions and access/modification timestamps, preserves uid/gid on Unix, preserves the complete Linux xattr set including POSIX ACL xattrs, syncs the replacement, atomically renames it, and syncs the parent directory. Any identity or governed-attribute drift detected by the two pre-publication checks fails closed before replacement.

## Immutable policy identity

Policy v15 is append-only. It inherits v14's render, analyzer commands and
parser, terminal, packaging, source-front-end, capacity-admission,
metadata-mutator, sample-identity, and rewritten-file attribute authority. The
new identity exists solely because the F8 follow-up adds canonical composite
tool-family acquisition, separates analytic and empirical analyzer residual
authority, expands the adversarial gate, and binds workload-derived analyzer
deadlines.

Historical v1-v14 policy JSON, candidate, certification, and report artifacts
remain byte-identical. Historical checker source is not claimed byte-identical:
the affected v5-v14 derivation checkers were intentionally corrected to stop
asserting mutable current-policy and successor-sensitive runtime pointers, while
retaining their immutable historical artifact and policy-identity checks. New
v15 current/candidate/report/certification artifacts and a deterministic v15
checker are present. The current and candidate v15 manifests are byte-identical.
V15 remains `qualification_candidate`; no promotion or release certification is
claimed.

## F2 legacy behavior inherited from the prior correction

Before promotion, a DSD source targeting PCM visibly exposes and executes the frozen legacy gain family:

- `disabled` -> exact legacy `Disabled` wire;
- `auto` plus 0..6 dB margin -> exact legacy `Auto` wire and SoX `norm -<margin>`;
- `manual` plus -24..+24 dB -> exact legacy `Manual` wire and SoX `gain <signed dB>`.

Reference pathway/profile, Reference gain, NativeLevel, and native NormalizePeak remain hidden and disabled. Generic resampler and dither options remain available on the legacy route. Native-only selections are rejected rather than silently discarded. V4 preset acceptance/refusal behavior remains explicitly adjudicated and tested.

## Source-level regressions retained through v15

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
- The deterministic v13 checker freezes every v12 artifact, binds the measured
  58-byte Float64 header, 50-byte RIFF-size contribution, corrected 4,294,967,245-byte
  ceiling, nine-point boundary scan, typed v3 evidence, and current runtime identity.
- The deterministic v14 checker freezes every v13 artifact, binds the 16x
  measurement-only SoX route and Float32 FFmpeg-to-raw seam, verifies the
  conservative `0.041925957 dB` grid bound inside the unchanged `0.110000000 dB`
  authority, and binds the 1,968-case matrix with fixed-frequency and tail cells.
- The deterministic v15 checker freezes every v14 artifact, binds the frozen
  SoX-before-FFmpeg-before-SSRC composite permit rank, cancellation-safe RAII
  ownership, the 2,168-case analyzer schema, explicit grid/resampler/reporting
  decomposition, and the workload-derived deadline constants and release gates.
- Barrier-forced asynchronous regressions reconstruct the historical circular
  wait and prove opposite-direction pipelines complete; a companion regression
  verifies duplicate-family collapse and partial-permit release on cancellation.

## Required verification commands

The v16 derivation checker freezes the complete v15 checker/artifact set and
validates inherited manifest fields before checking the exact W64 integrity
source and evidence bindings. Every historical checker is also run against its
own immutable artifacts so the append-only lineage remains green.

```text
python3 tonepoet-pipeline/qualification/derive_dsd_reference_v16_w64_integrity.py --check
cargo fmt -p tonepoet -p tonepoet-pipeline -p sacd-rs -- --check
cargo test -p tonepoet-pipeline
cargo test -p sacd-rs
cargo test -p tonepoet --test settings_sentinel
cargo test --workspace
cargo clippy -p tonepoet -p tonepoet-pipeline -p sacd-rs \
  --all-targets --all-features -- -D warnings
TONEPOET_REQUIRE_TOOLS=1 \
TONEPOET_DSD_REFERENCE_REPORT_PATH="$PWD/tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v16_certification.json" \
  cargo test -p tonepoet --test dsd_reference_qualification -- --nocapture
```

## Assembly-environment verification and limitation

The assembly environment provides system SoX 14.4.2 and FFmpeg 7.1.3. An unpinned local route smoke reproduced the direct Float64-W64 FFmpeg hash mismatch and showed that:

1. SoX-decoded raw-f64le source identity;
2. the typed SoX-to-FFmpeg RIFF package; and
3. direct FFmpeg decode of the packaged RIFF

produce matching source/delivered identities while direct FFmpeg decode of the W64 source differs. The same unpinned environment also executed the v15 16x SoX `stats` route at 192, 352.8, 384, and 768 kHz, plus the Float32 FFmpeg-to-f64le-to-SoX route at 192 kHz; every process exited successfully and produced one parseable `Pk lev dB` result. These are route smokes only, not pinned policy qualification.

The environment does not provide Cargo, rustc, rustfmt, Clippy, Nix, Flox, or
the pinned SoX-ng 14.8.0.1 closure. Therefore Rust compilation, formatting, workspace tests, Clippy, pinned-tool
qualification, live smoke, and release certification could not be executed here.
V16 remains fail-closed and unpromoted.

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

## V13/F7 completion: measured Float64 streamed-WAV header

F7 established that the reachable streamed Float64 WAV carrier has a 58-byte
header: 12 bytes of RIFF/WAVE framing, an 8+18-byte IEEE-float `fmt` chunk, an
8+4-byte `fact` chunk, and an 8-byte `data` header. Because the RIFF size field
excludes the leading eight bytes, the fixed RIFF-size contribution is 50 bytes,
not 58. The separately measured 80-byte Int24 streamed header is unreachable in
this contract and is not introduced into policy.

Policy v13 therefore admits an unaligned payload up to
`u32::MAX - 50 = 4,294,967,245` bytes. The largest whole mono Float64 frame
payload is 4,294,967,240 bytes (536,870,905 frames); the next payload,
4,294,967,248 bytes, is rejected with `DSD-REF-P0-025`. The contiguous real-tool
scan contains nine frame-aligned observations through the existing 4 GiB + 8
byte witness.

V12 and its checker, manifests, certification stub, and report remain
byte-identical. Its v2 evidence type retains the historical 66-byte header and
58-byte RIFF contribution. V13 uses a separate v3 evidence type and current
runtime binding. No route, tool, metadata, analyzer, terminal, admission-scope,
or enabled-cell behavior changed.

- v16 threshold characterization brackets each enabled W64 cell with a 96-exponent scan and a 256-point boundary-neighborhood at `2^e / 510` resolution; the first bracketed nonzero must remain nonzero after FFmpeg decoding.
