# Findings: Reference-Admission Corrective Round

**Commission status:** this file preserves the admission evidence that
commissioned F1 and F2. The 2026-07-20 complete-tree candidate resolves both
findings in source under the new immutable policy identity
`sox_ng_14_8_0_1_v6`. The candidate remains fail-closed and unpromoted until
the full declared Rust, pinned-tool, live-smoke, warning-free, and release
certification gates pass unchanged. See
[`handoff_dsd_reference_p0_current.md`](handoff_dsd_reference_p0_current.md)
and the v6 qualification report for the current authority.

## F1 — qualification gate: pre/post measurements disagree by ~13.9 dB (carrier or binding mixup)

`complete_p0_reference_qualification_report` panics at
`tests/dsd_reference_qualification.rs:2281` on the
`44100-1ch-float32-wav_riff-default` cell:

```text
post-final true peak exceeds the Reference -1.000000000 dBTP ceiling
pre_reported             = -31.810000000 dBTP
pre_conservative_upper   = -31.700000000 dBTP
post_reported            =  +0.140000000 dBTP
post_conservative_upper  =  +0.250000000 dBTP
gain_policy = ReferenceCompensated { requested_gain: +18.020599913,
    ceiling: -1.0, terminal_bound: { safe_pre_terminal_ceiling: -1.010001164 } }
terminal_args = sox -S -D <stage-01.w64> -t w64 -e floating-point -b 32
    <stage-02.w64> gain +18.020599913
```

The arithmetic is the finding. If the render carrier's true peak were
really −31.81, post-final would be ≈ −13.79; it measured **+0.14**. The
post value is self-consistent with a **−6 dB fixture** under −12 dB
headroom (−17.88 + 18.02 = +0.14); the pre value is self-consistent with
a **−20 dB fixture** under −12 dB headroom (−32 ≈ −31.81). Your harness
does synthesize −20 dB carriers (`tests/dsd_reference_qualification.rs:1780`).
So the pre-final measurement and the post-final measurement appear to
have observed carriers derived from *different fixtures* — a stage-path,
measurement-id, or work-dir binding crossing between subcases, in either
the harness's per-cell loop or the production measurement binding.

What we verified is NOT the cause: the streamed producer argv is clean
(no stray `gain` token; `dsd_reference.rs:2357-2374`), and the render
command applies headroom exactly once. The gain authority and post-final
ceiling check are doing their jobs against inconsistent inputs.

Resolve by finding the crossed binding; do not widen the ceiling, soften
the post-final check, or adjust fixture levels to mask the disagreement.

## F2 — pre-promotion TUI hiding overshoots: the legacy DSD gain feature is disabled

Five TUI-layer tests fail. One is **pre-existing** and pins a
pre-project capability:

```text
tui::app::source_default_reset_tests::apply_source_defaults_preserves_source_sentinels_when_probe_is_unresolved
  panics: convert.format.dsd_gain_mode.select_value(&DsdGainMode::Fixed) == false
```

Selecting the Fixed (manual) DSD gain mode no longer works. That control
family (Disabled/Auto/Manual gain for DSD→PCM) predates the Reference
project and is part of the exact-legacy behavior your admission
corrective promises pre-promotion. Hiding the *new* Reference rows
(path/profile/reference-gain) pre-promotion is correct; disabling the
*old* gain pills is a regression against your own "ordinary defaults
remain exact legacy behavior" contract.

The other four are your own tests from earlier rounds, now inconsistent
with the pre-promotion hiding — adjudicate each as stale-pin or symptom
while fixing the above:

```text
tui::app::dsd_gain_format_state_tests::manual_dsd_gain_row_adjusts_value_and_selects_manual_mode
  (gain-row adjustment is a no-op: DbNano(0) vs expected DbNano(250000000))
tui::app::dsd_gain_format_state_tests::pre_promotion_reference_controls_remain_hidden_for_dsd_to_pcm
  (fails on `resampler.options.iter().any(|o| o.enabled)` — the hiding pass
   appears to disable resampler options too; check for over-broad disable)
tui::presets::companion_preset_tests::apply_to_pills_reports_values_refused_by_format_constraints_and_parsing
  (v4 preset fields output_target/dsd_path/dsd_profile/dsd_gain now refused)
tui::presets::companion_preset_tests::dsd_preset_refusal_is_independent_of_disabled_pcm_prestate
  (same refusal set, unexpected)
```

If v4 preset DSD fields are *intended* to be refused pre-promotion,
update those tests and say so in the report; if not, fix the refusal.
Either way the pre-existing legacy gain selection must work again, with
the pre-existing test passing unmodified.

## Resolution applied in the v6 candidate

### F1 — carrier-sensitive analyzer binding

The apparent fixture crossing was an analyzer-decoder crossing. An isolated
same-file reproduction established that FFmpeg reads SoX-written Float32 W64
at the correct level, while routing that same carrier through SoX's W64 reader
and an f64 WAV stream drives it near full scale. Float64 W64 has the opposite
requirement: direct FFmpeg decoding is wrong by `2^31`, while the SoX f64
stream is correct.

Policy v6 therefore binds the analyzer route to the measured carrier:

- R64 pre-final measurement: typed SoX f64-WAV stdout -> FFmpeg stdin;
- Float32 QPCM post-final measurement: direct FFmpeg W64 input;
- Int24/Float64 QPCM post-final measurement: typed SoX f64-WAV stdout ->
  FFmpeg stdin.

The executor validates measurement ID, purpose, programme scope, exact stage
path, producer presence, route, argv, environment, and parser against the
immutable plan summary before running a tool. QPCM remains W64 at every depth;
there is no on-disk RIFF analyzer or packaging intermediate, so W64/RF64 do not
inherit a 4 GiB RIFF ceiling. Because this changes route, argv, parser,
semantic identity, and evidence, the correction is append-only policy v6;
v1-v5 are unchanged historical identities.

### F2 — exact pre-promotion legacy gain behavior

Before promotion, DSD-to-PCM now visibly exposes and functionally applies the
frozen legacy family:

- Disabled -> exact legacy `Disabled` wire and no gain effect;
- Auto -> exact legacy `Auto` wire and `norm -<margin>`;
- Manual -> exact legacy `Manual` wire and `gain <signed dB>`.

Reference pathway/profile/gain, NativeLevel, and native NormalizePeak remain
promotion-gated. Generic resampler and dither controls remain available. The
settings builder validates and serializes the exact legacy wire rather than
accepting a UI value and discarding it.

V4 preset behavior is now explicit and tested: behaviorless default
Reference path/profile fields are accepted pre-promotion; legacy
Disabled/Auto/Manual values map to their exact legacy modes; historical
Normalize maps to legacy Auto through the existing compatibility mirror;
native-only pathway/profile/gain values remain reported refusals; DSD fields
are ignored when the destination is not DSD-to-PCM; and incompatible output
targets remain refused.

## Constraints and remaining gates

Complete-file delivery; do not expand scope. Required gates remain the full
workspace suite, both v5 and v6 deterministic generator checks, pinned-tool
qualification, the default-settings live smoke, Clippy with warnings denied,
and zero cold warnings. The pre-existing legacy-gain test remains unmodified.
The bundle-assembly environment lacked Cargo/rustc/rustfmt/Clippy and the
pinned SoX-ng closure, so those gates are not claimed here; the candidate stays
fail-closed pending execution in the declared toolchain.


## F3 (v7 round) — Float32 terminal-realization verification uses a defective decode route

With the v7 bundle applied and compile-clean, the tool-gated
`complete_p0_reference_qualification_report` progresses past all prior
blockers and now fails at `tests/dsd_reference_qualification.rs:2675`:

```text
terminal realization error 7.989553529916060270e-1 exceeded policy bound
1.192092895507812500e-7 for Float32
```

A ~0.799 full-scale sample discrepancy against a 2^-23-class bound means
the two sides of the comparison decoded different sample streams, not a
rounding excess. Your own v6 route matrix established that FFmpeg
mis-reads SoX Float32 W64 via the STREAMED route while reading it
correctly DIRECT (and the opposite for Float64). The Float32
terminal-realization verification appears to decode one side of the
comparison through the wrong route for its depth. Audit every decode in
the terminal-realization and sample-preservation checks against the
frozen per-depth route table, and pin each verification's route the same
way the measurement contract already pins argv/carriers.

State when F3 surfaced: workspace suite fully green (3733+), sentinel
green, live smoke passes, bounds --check green, zero cold warnings; the
qualification target is 3 passed / 1 failed at this checkpoint. Also
kept on our side: legacy-wire casing corrections in your new tests
(frozen v1 wire serializes capitalized variant names — "Auto"/"Disabled"
— byte compatibility pins it), and one real production fix your
exact-legacy test caught: the DbNano→f32 legacy gain conversion now
rounds through f64 so representable values (2.25) survive exactly.


## F4 (F3-corrective round) — Float64 terminal bound assumes f64 arithmetic; SoX's 32-bit effects boundary makes it unattainable

Your F3 corrective is applied and lands: the workspace suite is fully
green (4,587/0), the live smoke passes, warnings are zero, and the
typed sample-identity verify routes work — the qualification gate now
runs 53s of real tool work and progresses past every prior blocker.
Apply-side corrections kept on our side (review in the commit): the new
`reference_evidence_subprocesses_clear_and_set_exact_environment`
fixture passes `Some(0)` for the mandatory RIFF size bound like its
siblings; the harness package-collection now recognizes the Float64
RIFF/RF64 `Pipeline` step (producer SoX / consumer FFmpeg, filter flags
forbidden, `-ar`/`-ac` asserted as input-side raw-stream declarations
that must precede `-i`, output-side `-ar` still forbidden).

The one remaining failure is a policy-derivation finding, not a route
bug:

```text
tests/dsd_reference_qualification.rs:3034 (Float64 cell)
terminal realization error 2.328118253736022325e-10
exceeded policy bound     4.440892098500626162e-16
```

The observed maximum error is ≈ 2^-32 (2.3283e-10) — exactly one step
of SoX-ng's signed 32-bit internal effects representation. Your own v5
evidence records that boundary ("substantially more than 24 bits of
effective resolution inside the signed 32-bit effects representation").
The terminal `gain` for Float64 runs through that 32-bit chain, so its
realization error floor is 2^-32-class regardless of the f64 carrier;
the policy's Float64 bound was derived as if the arithmetic were f64
(≈ 2 × 2^-52). No decode route is wrong — the bound is unattainable by
the qualified toolchain.

Resolve in your domain under the append-only rules: re-derive the
Float64 terminal realization bound (and its safe pre-terminal ceiling)
from the 32-bit effects boundary, or restructure the Float64 terminal
operation to avoid the 32-bit quantization if a qualified route exists
— your choice with rationale. Float32 (bound 2^-23 ≫ 2^-32) and Int24
appear unaffected, but re-check all three cells' derivations against
the same boundary while you are in there. Do not widen any bound
without a derivation; do not soften the check.


## F5 (v8 terminal round) — third ffmpeg W64 defect: the MUXER folds alignment padding into the data chunk, appending a phantom sample

Your v8 terminal round otherwise lands: F4's summed 2^-32 + 2^-51 Float64
bound verifies (v8 checker green), the workspace suite is fully green
(4,587/0), the live smoke passes, warnings are zero, and the
qualification gate now runs its longest chain yet (259 s) with every
cell passing except one. Apply-side kept on our side: a crate-level
`#![recursion_limit = "512"]` for your large terminal-report `json!`
literal, and the failing assert was instrumented to preserve evidence
(kept — it names the cell and copies the pair to /tmp/qual_keep; review
and keep or replace with equivalent diagnostics).

The remaining failure is a NEW empirical toolchain defect your gate
caught honestly:

```text
cell: WavW64 Int24, mono, 88.2 kHz (8,820 samples = 26,460 data bytes,
      not 8-byte aligned)
packaged QPCM decodes to 8,820 samples; the ffmpeg -c:a copy -f w64
metadata rewrite of that same file decodes to 8,821 samples
(identical prefix, one phantom trailing sample; file sizes 26,564 vs
26,592)
```

Isolated evidence: sox reads both files and reports 8,820 vs 8,821
samples; ffmpeg hash-decodes disagree; byte comparison shows the
original 26,460 bytes identical with extra trailing data on the copy.
Stereo and 8-aligned fixtures round-trip cleanly (verified) — the
trigger is a data chunk whose byte length is not a multiple of the W64
8-byte alignment, where **ffmpeg's W64 muxer accounts the alignment
padding as data**, so the next demux yields a phantom frame. This is
distinct from the two decode-side W64 defects you already routed
around; it is mux-side.

Production impact, verified precisely: `metadata_tag_command`
(`src/convert/pipeline/stages.rs:4838-4853`) dispatches by extension —
`"wav"`/`"aiff"`/`"mp3"`/`"m4a"` route to the ffmpeg re-mux (temp-file
rewrite), while `"w64"` is ABSENT and fails loudly as
`UnsupportedTagFormat`. So there is no silent W64 corruption in
production today; the consequences are instead: (1) a Reference W64
delivery with the metadata stage enabled errors rather than tags —
decide and encode the intended behavior (skip-with-provenance, native
route, or documented rejection); (2) the muxer defect forecloses ever
qualifying an ffmpeg-based W64 tag route; and (3) the analogous hazard
for RIFF/WAV — which production DOES ffmpeg-remux — is untested: RIFF
pads chunks to 2-byte alignment, and 24-bit mono WAV with an odd sample
count produces an odd data size. Qualify or refute the 2-byte analog
with a fixture while you are in this code.

Resolve in one pass under your rules — candidate shapes, your choice
with rationale:

- route W64 metadata mutation away from the ffmpeg W64 muxer entirely
  (e.g. native RIFF/W64 chunk-level tag writing, or a qualified sox
  re-container with exact-size proof), with the post-metadata decode
  verification you already have as the acceptance gate;
- or forbid metadata mutation for W64 targets in this policy with a
  recorded reason and a user-facing message, if no qualified route
  exists;
- and in either case extend the qualification fixture set so at least
  one non-8-aligned W64 cell (mono Int24) is exercised permanently.

Do not accept the phantom sample; do not special-case the harness to
look away. Per the terminal directive: every enabled cell must pass by
construction in the returned bundle, and any cell you cannot make both
attainable and correct must be rejected with its reason.

### F5 resolution (policy v9 candidate, 2026-07-20)

Resolved without accepting or masking the phantom sample. The append-only
`sox_ng_14_8_0_1_v9` candidate retains W64 audio delivery but rejects W64
metadata mutation before conversion whenever the metadata stage is enabled.
The metadata writer independently rejects `.w64` with the same stable
`DSD-REF-P0-024` message before allocating a rewrite tempfile or selecting
FFmpeg, so alternate invocation paths cannot reach the defective muxer.

The commissioned gate now permanently reproduces the non-eight-aligned mono
Int24 W64 failure (8,820 samples / 26,460 bytes becoming 8,821 samples with an
identical prefix and one zero-valued phantom sample). It separately exercises
an odd-byte RIFF/WAV payload (8,821 mono Int24 samples / 26,463 bytes) and
requires byte-exact decoded-sample identity after metadata rewriting. The 480
qualified delivery cases remain enabled; post-metadata identity is required
for 420 non-W64 cases, and the 60 W64 metadata cases must resolve to
`DSD-REF-P0-024` by construction.


### F5 evidence completion (policy v10 candidate, 2026-07-20)

The v9 behavior correction remains valid, but its report described a surrogate
FFmpeg stream-copy probe as qualification of the production metadata mutator.
The append-only `sox_ng_14_8_0_1_v10` candidate corrects that evidentiary
overstatement without changing the 480-cell audio-delivery contract.

Qualification now invokes the shared per-file implementation used by production
`apply_metadata` for all 420 admitted non-W64 cells. It executes and reports
160 FFmpeg primary mutations, 180 `metaflac` mutations, 80 `wvtag` mutations,
and 20 AtomicParsley M4A freeform follow-ups. Each cell is re-probed for exact
container identity and decoded-sample identity after mutation. The report binds
the canonical path, executable SHA-256, and reported version of every mutator.
The machine claim is explicitly limited to authoritative tag mutation without
artwork embedding or ReplayGain, so the report does not imply evidence it did
not execute. The exact discovery and mutation commands use a closed environment
containing only `LC_ALL=C`, and the runtime validator binds that policy.

All 60 W64 cells traverse both production enforcement implementations: the
planner entry and the shared production metadata implementation. Both must
return `DSD-REF-P0-024`; merely calling the central policy helper is no longer
sufficient evidence.

Running the exact production FFmpeg path also exposed and closed RF64 container
drift: a source whose first four bytes are `RF64` now forces `-rf64 always` on
the same-extension rewrite, and the qualification matrix rechecks the RF64
container contract after metadata mutation. The runtime validator requires the
new structured evidence and rejects the former overbroad v9 claim.

### F5 runtime identity completion (policy v11 candidate, 2026-07-20)

The v10 route qualification remains valid, but the captured `metaflac`,
`wvtag`, and AtomicParsley identities were descriptive rather than an enforced
production execution boundary. The append-only `sox_ng_14_8_0_1_v11`
candidate closes that gap.

The policy-owned package/store paths for all three mutators are now compiled
into the production binary. Reference admission compares the report-certified
canonical path, executable SHA-256, and normalized version with the packaged
activation path, compiled store executable, and the path the active runner
would resolve. Any `ProcessorConfig.tool_paths` override or ambient `PATH`
substitution that resolves a different executable fails before conversion.

Immediately before metadata mutation, production rechecks the runner path,
activation path, compiled path, executable SHA-256, normalized version, and
closure digest. Every actual FFmpeg, `metaflac`, `wvtag`, and AtomicParsley
metadata command then executes through exact canonical-path-plus-SHA-256
authority rather than a second unconstrained lookup. The certified mutator
closure is serialized in each Reference track's toolchain evidence and included
in `execution_fingerprint_v1`, completing the per-output authority chain.

The schema-v11 machine report contains a required
`runtime_metadata_mutator_binding` object. Runtime validation rejects missing or
altered binding fields and non-canonical mutator identity objects, including a
store path that differs from the compiled package. The qualification source pins
all mutator executables to their corresponding store paths. Unit regressions
cover fail-closed alternate runners, exact bound execution, configured-path
rejection, executable-digest rejection, and per-output fingerprint sensitivity
to every mutator identity component.

No metadata scope was added: the v10 authoritative-tag route matrix and counts
remain unchanged, artwork and ReplayGain remain excluded from the F5 claim, and
W64 metadata mutation remains rejected under `DSD-REF-P0-024`.


## F6 (v11 round) — sox_ng's WAV writer wraps >4 GiB sizes on unseekable output instead of emitting streaming sentinels

Your v9/v10/v11 F5 resolution is applied and verified: suite 4,598/0,
all three deterministic checkers green, live smoke passes, zero cold
warnings after documenting the new fingerprint fields, and the W64
rejection + production-mutator-route qualification passes. Apply-side
kept: a catch-all `unreachable!` arm for the widened target enum in the
mutator match.

The one remaining failure is the >4 GiB streaming transport proof:

```text
tests/dsd_reference_qualification.rs:2381
SoX-ng did not emit the frozen large streaming-WAV size sentinels:
riff=0x0000003a, data=0x00000008
```

Isolated with a minimal fixture (sparse W64, f64 mono 48 kHz, data
chunk = 4 GiB + 8 bytes):

- sox_ng READS the file correctly: 536,870,913 samples — the W64
  reader is not implicated;
- sox_ng WRITES the streamed WAV header with sizes wrapped modulo
  2^32 (`RIFF 3a 00 00 00`, data size 8) instead of the streaming
  sentinels your contract froze. (4 GiB + 8) mod 2^32 = 8 — exact.

So the pinned SoX-ng 14.8.0.1 WAV writer truncates 64-bit sizes to
32 bits when the output is unseekable and the payload exceeds 4 GiB.
This affects the streamed analyzer/packaging routes only for carriers
past 4 GiB (long high-rate float programmes; the Round-2 continuous
albums are squarely in this class).

Resolution shapes, your choice with rationale under the append-only
rules:

1. Re-pin the streamed-WAV contract IF you can empirically qualify
   that the pinned FFmpeg consumer reads `pipe:0` to EOF and ignores
   the declared RIFF/data sizes for wrapped >4 GiB streams — with a
   permanent sparse-carrier fixture proving sample-exact consumption
   past the 4 GiB boundary. The sentinel check then becomes a
   consumption-completeness check.
2. Reject streamed cells whose carrier exceeds the provable bound
   (record a 4 GiB streamed-carrier capacity cap with its reason and a
   user-facing error), keeping smaller carriers qualified.
3. Note for the product owner (outside your scope): the defect lives
   in the user-owned sox_ng fork; an upstream one-line-class fix
   (emit 0xFFFFFFFF sentinels for unseekable >4 GiB output) plus a new
   toolchain pin and closure re-attestation would lift any cap later
   under a new policy ID. Design so that this later lift is a pure
   policy addition.

Do not accept a wrapped header as a sentinel; do not soften the
transport proof.

### F6 resolution (policy v12 candidate, 2026-07-21)

Resolved by choosing the fail-closed capacity boundary rather than asserting an
unproven downstream read-to-EOF behavior. The append-only
`sox_ng_14_8_0_1_v12` candidate reproduces the pinned SoX-ng defect with the
permanent sparse W64 fixture: 536,870,913 mono Float64 frames produce a
4,294,967,304-byte audio payload, while the unseekable WAV header contains RIFF
size `58` and data size `8`; the data field is the exact modulo-2^32 value,
while the RIFF field collapses to the header-only size. Those values are
recorded as a defect and are explicitly not accepted as sentinels or complete
transport evidence.

All Reference plans require the Float64 WAV stream used by the pre-terminal
analyzer. V12 therefore admits a plan only when the checked upper bound

```text
(ceil(duration_ns * target_rate_hz / 1e9) + 1 guard frame)
  * channels * 8 bytes
  <= 4,294,967,237 bytes
```

holds. The maximum is `u32::MAX - 58`, where 58 is the fixed RIFF-size
contribution of the pinned 66-byte streamed header. Missing duration, checked
arithmetic overflow, or a larger carrier fails before execution with the new
stable `DSD-REF-P0-025` error. Ordinary RIFF output keeps its existing
`DSD-REF-P0-018` preflight and precedence; RF64, W64, and every other delivery
container remain subject to the analyzer-stream cap.

The qualification report and runtime release validator now bind the exact
capacity constants, the one-frame guard, the negative sentinel/completeness
claims, and the user-facing error. The hardened v2 evidence also runs the pinned
producer at the largest frame-aligned admitted payload (4,294,967,232 bytes),
requires exact nonwrapped RIFF/data fields there, rejects the immediately
following frame with `DSD-REF-P0-025`, and scans every frame-aligned payload
through the frozen 4 GiB + 8 witness to locate the writer's actual first
RIFF-field wrap without assuming an unproved overflow formula. The qualification
harness serializes this evidence from typed structures; the runtime validator
deserializes it with `deny_unknown_fields` and validates arithmetic continuity
and all edge relationships before accepting the report.

The existing FFmpeg same-directory metadata rewrite now has an explicit
attribute contract. It snapshots and revalidates the original regular-file
identity, preserves permission state and access/modification timestamps,
preserves uid/gid on Unix, preserves the complete xattr set (including POSIX
ACL xattrs) on Linux, and verifies the published attributes after atomic
replacement and parent-directory sync. Target substitution detected before
publication, or any preservation failure, aborts closed.

Removing the cap later is a pure append-only policy addition requiring either a
corrected and repinned SoX-ng writer with renewed closure attestation or an
independently qualified sample-exact transport beyond 4 GiB.


## F7 (v12 round) — streamed-WAV header-size constant mis-derived: 66 vs the measured 58

Your v12 capacity resolution is otherwise sound and applied: suite
4,608/0, v12 checker green, live smoke passes, zero cold warnings.
Apply-side kept: `sync_parent_dir` re-exported from the new
`metadata_rewrite` module for the stages call sites, and one
double-reference comparison fix.

The single remaining qualification failure is one frozen constant
(`tests/dsd_reference_qualification.rs:2808`):

```text
observed_header_bytes = 58
policy stream_header_bytes = 66
```

Measured layout of SoX-ng 14.8.0.1's streamed float WAV header (hex
dump verified on this toolchain):

```text
RIFF header            12   ("RIFF" + u32 size + "WAVE")
fmt  chunk header       8
fmt  body              18   (16-byte WAVE_FORMAT_IEEE_FLOAT + u16 cbSize=0)
fact chunk header       8
fact body               4   (u32 sample count)
data chunk header       8
total                  58
```

The header size IS encoding-dependent, and both variants are now
measured on this exact toolchain (hex dumps verified):

```text
Float64 streamed WAV:  RIFF 12 + fmt 8+18 (IEEE_FLOAT + cbSize=0)
                       + fact 8+4 + data hdr 8            = 58 bytes
Int24 streamed WAV:    RIFF 12 + fmt 8+40 (EXTENSIBLE)
                       + fact 8+4 + data hdr 8            = 80 bytes
```

Note sox writes WAVE_FORMAT_EXTENSIBLE for streamed Int24 and emits a
`fact` chunk for BOTH encodings — derive nothing; use these measured
values. If your streamed-WAV contract only ever carries the f64
re-container (as the frozen analyzer routes indicate), then 58 is the
single normative value and the Int24 layout is recorded for
completeness only — do not add per-encoding constants the contract
cannot reach. Correct `stream_header_bytes` (and any dependent v12
constants such as the RIFF-size overhead) from the measured layout,
and mint the correction under your append-only rules.
Everything else in the v12 capacity contract verified against the real
binary.

### F7 resolution (policy v13 candidate, 2026-07-21)

Resolved without reinterpreting policy v12. The reachable SoX-ng 14.8.0.1
streamed Float64 WAV header is frozen at the measured 58 bytes. Because the
RIFF size field excludes the leading eight bytes, policy v13 uses a 50-byte
RIFF-size contribution and an unaligned maximum audio payload of
4,294,967,245 bytes. The largest whole mono Float64 frame payload is
4,294,967,240 bytes; the immediately following 4,294,967,248-byte payload is
rejected with `DSD-REF-P0-025`. The contiguous boundary scan now contains nine
frame-aligned observations through the unchanged 4 GiB + 8 byte wrap witness.

The correction is append-only. Every v12 derivation, JSON manifest, candidate,
certification stub, and report remains byte-identical. The historical typed v2
capacity-evidence contract retains v12's 66-byte-header/58-byte-overhead values;
v13 introduces a separate `ReferenceStreamedWavCapacityEvidenceV3` contract
that binds the measured 58-byte header, 50-byte overhead, corrected arithmetic,
and nine-point scan. Current policy/default/runtime bindings advance to v13,
while release activation remains fail-closed until a passing schema-v13 pinned
real-tool report is bound to the exact candidate manifest.

No decoder, analyzer, packaging, terminal, metadata, source-admission, or
product-exposure scope changed.


## F8 (v13 round) — loudnorm performs NO inter-sample oversampling at high input rates: dBTP degrades to sample peak at ≥192 kHz

Your v13 header-authority correction is applied and verified (constant
matches the measured 58; v13 checker green; suite 4,613/0; smoke green;
zero cold warnings; qualification 4/5 at 1,159 s). The remaining sweep
failure (`tests/dsd_reference_qualification.rs:4277`, under-report
3.007 dB at rate=192000, normalized_frequency=0.25, phase=π/4,
position=early) has been mechanistically isolated on this toolchain —
and the first hypothesis (filter warm-up) was tested and REFUTED, so do
not pursue it:

```text
fs/4 sine, phase π/4, amplitude 0.5 (analytic true peak −6.02 dBFS,
sample maximum −9.03 dBFS = A·sin(π/4)):

fed to loudnorm at 48 kHz:   input_tp = −5.42   (inter-sample detected)
fed to loudnorm at 96 kHz:   input_tp = −5.42   (inter-sample detected)
fed to loudnorm at 192 kHz:  input_tp = −9.03   (EXACTLY the sample max)

isolated −6.02 dBFS single-sample peak at stream position 5, 192 kHz:
input_tp = −6.02 — head position alone measures fine (warm-up refuted).
```

The behavior is rate-dependent and does NOT follow a simple threshold —
the same normalized-fs/4 raw stream measured at every rate (analytic
true peak −6.02, sample max −9.03):

```text
 48,000 Hz: −5.42   inter-sample reconstruction working
 96,000 Hz: −5.42   working
176,400 Hz: −5.94   working (mild under-read, near truth)
192,000 Hz: −9.03   EXACTLY the sample peak — no reconstruction at all
352,800 Hz: −3.58   OVER-read by 2.4 dB (content at 88.2 kHz here rides
                    the analyzer's internal-resampling edge; over-read
                    is at least ceiling-conservative, but unqualified)
```

So the measured degenerate case is exactly-192 kHz input (sample peak
only — 3.007 dB blind spot for B5-passband content, matching your
sweep), 176.4 kHz measured acceptable on this fixture, and 352.8 kHz
exhibits a third behavior needing its own characterization. Do not
trust any internal-target threshold story, including this note's:
qualify each enabled target rate empirically with fixed-frequency
in-band fixtures (the matrix above varies frequency with rate since it
reuses one normalized-fs/4 stream). Low rates (44.1–96 kHz) measured
unaffected.

Resolution shapes, your choice with rationale under append-only rules:

1. **Qualified pre-oversampling producer:** extend the typed producer
   stage to upsample the measurement view (e.g. SoX `rate` ×4, a
   qualified resampler you already pin) before loudnorm, so the
   consumer's sample peak approximates true peak with a derivable
   residual bound (bandlimited content at 4× headroom); fold that
   residual into Q/E with a derivation, and extend the sweep to prove
   the high-rate cells within authority. Measurement-only: the audio
   chain is untouched.
2. **Per-cell analytic bound:** after the Reference profile sinc, the
   signal is bandlimited to the profile stopband; where
   stopband/Nyquist is small (e.g. 70 kHz/176.4 kHz), the maximum
   inter-sample overshoot above sample peak is analytically boundable
   and could be folded into the per-cell authority — but at B5's
   48 kHz passband on a 192 kHz target the ratio permits the full
   ~3 dB, so this shape alone cannot rescue those cells; justify any
   hybrid precisely.
3. A different qualified true-peak analyzer route with rate-independent
   oversampling, if you can attest one within the pinned closure.

Do not widen Q+E; do not let dBTP silently mean sample peak at any
enabled cell. Extend the sweep to cover the tail position and the
352.8/384 kHz cells under whatever shape you choose.

### F8 resolution (policy v14 candidate, 2026-07-21)

Policy v14 removes FFmpeg `loudnorm` from production true-peak authority. Every
measurement uses pinned SoX-ng to create a measurement-only 16x view and reads
its peak through the strict, channel-count-bound `sox_stats_pk_lev_db_v1` parser. Int24 and Float64
W64 carriers use direct path-backed SoX. Float32 W64 retains the previously
qualified direct FFmpeg decode seam, emitting headerless f64le over a direct,
shell-free pipe into the same SoX 16x measurement and `stats` stage.

The unchanged one-sided authority remains `Q + E = 0.010000000 + 0.100000000 =
0.110000000 dB`. For content bandlimited to the original Nyquist frequency, the
16x sample grid has a worst-case sinusoidal miss of
`-20 log10(cos(pi / 32)) = 0.041925956... dB`; policy rounds this upward to
`0.041925957 dB`, which is contained within the existing analyzer residual.
No ceiling or uncertainty budget is widened, and dBTP is not redefined as
sample peak.

The analyzer qualification schema advances append-only to v5 and expands from
1,200 to 1,968 cases. It retains the normalized single-tone and phase-aligned
multitone corpus, adds fixed 1, 20, 48, and 70 kHz in-band fixtures, covers
both early and tail positions, and exercises every enabled target rate,
including 352.8 and 384 kHz. Release activation remains fail-closed until the
exact pinned SoX-ng 14.8.0.1/FFmpeg 7.1 gate passes and its report is bound.

No render, terminal, QPCM, packaging, metadata, source-admission, enabled-cell,
or delivered-audio scope changed. The v13 streamed-WAV capacity guard remains
as a conservative inherited admission rule even though the v14 analyzer no
longer uses that carrier.

### F8 operational hardening (policy v15 candidate, 2026-07-21)

The v14 analyzer architecture is retained, but its executor and evidence contract
advance append-only to policy v15. No v14 artifact is reinterpreted or modified.

All three production Reference pipelines now acquire their complete tool-family
permit set through one composite RAII guard before either subprocess starts. The
frozen global rank is SoX, then FFmpeg, then SSRC; FFprobe shares the FFmpeg
family. The helper deduplicates families, acquires only in that rank, and drops
every already-acquired permit if cancellation or semaphore closure prevents a
later acquisition. A barrier-forced asynchronous regression first reconstructs
the former FFmpeg-to-SoX versus SoX-to-FFmpeg circular wait with one permit per
family, then proves both opposite-direction declarations complete under the
composite protocol without sleep-based scheduling assumptions. A second test
binds duplicate-family collapse and partial-acquisition release on cancellation.

The unchanged one-sided authority is now decomposed explicitly:

```text
ideal 16x grid component:             0.041925957 dB
pinned-resampler empirical component: 0.058074043 dB
analyzer residual E:                  0.100000000 dB
reporting quantization Q:              0.010000000 dB
one-sided total Q + E:                0.110000000 dB
```

The first component is analytic. The second is not claimed from the grid
derivation; it is a separate pinned-tool empirical authority that remains
`requires_pinned_real_tool_qualification`. Analyzer schema v6 adds 200
adversarial cases—impulse, near-band-edge burst, alternating-sign, deterministic
broadband, and boundary-transient fixtures at both boundaries, all enabled
rates, and mono/stereo—to the inherited 1,968 cases. The production 16x result
is compared with a 64x pinned-SoX qualification oracle, for 2,168 total cases.
This is deliberately recorded as empirical pinned-resampler evidence, not as a
coefficient-derived universal filter proof. Activation remains fail-closed until
the exact pinned matrix passes and is bound into certification.

Analyzer deadlines no longer inherit the generic one-hour command timeout. The
planner derives one deadline from guarded source frames, channel count, and the
16x factor:

```text
workload = (ceil(duration_ns * rate / 1e9) + 1) * channels * 16
deadline = 120 s + ceil(workload / 1,000,000 sample-values/s)
```

The existing streamed-WAV admission cap bounds the maximum workload at
8,589,934,480 oversampled sample values and the maximum deadline at 8,710
seconds. The same deadline is bound to both processes in the Float32 pipeline, stored
in the plan summary, and included in the v15 semantic identity; runtime accepts
only an exact command-to-summary match. The pinned release gate must demonstrate
at least the frozen throughput floor and bind the maximum-admission arithmetic
before promotion.

No render, terminal, QPCM, packaging, metadata, source-admission, enabled-cell,
or delivered-audio behavior changed. The assembly environment still lacks the
Rust toolchain and pinned SoX-ng 14.8.0.1 closure, so policy v15 remains an
unpromoted, fail-closed qualification candidate.


## F9 (v15 round) — three regressions in the v14/v15 return; assembly quality dropped

Applied state: suite 4,616/0 except item 2's test, v15 checker green,
smoke green, zero cold warnings. Apply-side kept (review and keep): the
`exact_string_array` helper you referenced but never shipped is
implemented in `track_executor.rs`; two `ToolIdentifier` clones. This
round also requires you to re-run your own static assembly checks
before returning — a missing function should not reach us.

### F9.1 — v14 checker breaks the append-only lineage

`derive_dsd_reference_v14_*.py --check` fails post-v15:
`planner: missing '"qualification/dsd_reference_sox_ng_14_8_0_1_v14.json"'`.
Every previous generation kept ALL historical checkers green
simultaneously (v9/v10/v11 verified together earlier). Diagnosed
precisely: every other v14 marker still passes (V14_KEY, SoxNg14801V14,
the 16x oversample constants, the sox-stats parser identifiers) — the
ONLY failing marker is the embedded-artifact path
`"qualification/dsd_reference_sox_ng_14_8_0_1_v14.json"`, which every
successor policy must necessarily repoint (the planner now embeds
v15's). A historical checker asserting the CURRENT embed path is an
inherently non-append-only assertion. Fix: historical checkers assert
their own persistent artifacts/constants, never the current embed
pointer (and audit v15's checker so it does not repeat the pattern);
state the lineage contract explicitly: every historical checker must
pass forever.

### F9.2 — crossed-carrier contract validation no longer rejects a crossed producer input (REAL protection gap)

Your own negative test fails:
`track_executor.rs:8247`:
`assert!(validate_reference_measurement_contract(f32_summary, &crossed_path).is_err())`
— the crossed path is ACCEPTED. Root cause shape: the F8/v14 analyzer
correction added an upsampling producer stage to the Float32 post-final
route; the test crosses `input_stage.input` and `args[6]` to the R64
carrier, and the contract validator apparently does not validate the
NEW producer stage's input path against the summary carrier. This is
precisely the crossed-carrier protection class F1/F3 established.
Restore full producer-stage path validation (binding correctly rejects;
contract must too) and extend the negative coverage to every route that
gained a producer in v14.

### F9.3 — verified-silence scan uses the forbidden direct-ffmpeg f64-W64 route, and ffmpeg cannot even open the fixture

`tests/dsd_reference_qualification.rs:451` (run helper), captured argv:

```text
ffmpeg -y -nostdin -hide_banner -loglevel error
  -i <...>/verified_silence/work/.final-wav_w64-176400-2ch...stage-01.w64
  -map 0:a:0 -f f64le -acodec pcm_f64le <...>/silence-1.f64le
failed: Error opening input: Invalid data found when processing input
```

Two defects in one: (a) the silence-scan helper decodes a SoX-written
f64 W64 via DIRECT FFmpeg — the route your own table forbids for f64
carriers (the 2^31 defect class); route it through the qualified SoX
raw-stream mechanism like every other f64 decode; (b) FFmpeg refuses to
open this particular silence QPCM at all — characterize whether the
zero-content/short data chunk is the trigger and record it with the
other FFmpeg W64 defects if so.

Terminal rules restated: all historical checkers green, the negative
test passes unmodified in intent, the silence scan obeys the route
table, no cell left predicted-failing, and re-run your static assembly
verification before returning.

### F9 resolution (v15 corrective return)

1. **Append-only checker lineage.** Every v5-v15 derivation checker now validates
   only its own immutable artifacts and persistent policy identity. Historical
   checkers no longer assert the mutable current-policy embed pointer. The
   lineage contract is stated in each checker: once shipped, a checker must
   remain valid against every successor policy. The complete v5-v15 checker
   chain passes together after the correction.
2. **Producer-stage carrier binding.** Measurement-contract validation derives
   the required carrier from the trusted plan summary and purpose, then binds
   both direct commands and producer stages to that path. It also independently
   requires the Float32 post-final producer shape. The existing crossed-carrier
   negative test retains its intent and now exercises W64, RIFF, and RF64 routes.
3. **Route-authorized silence decoding.** Silence scans accept only an opaque
   decoded-carrier authority and dispatch through the same route table as other
   decoded-audio operations. Float64 W64 therefore uses the qualified SoX
   f64le-raw route; direct FFmpeg remains unavailable. Qualification now pairs
   the known short-silence failure with a duration-matched nonzero control and a
   long-silence probe, classifies the trigger as zero-content or a short
   zero-content interaction, records the result beside the existing W64
   defects, and binds that evidence in runtime certification validation.

Static assembly verification was rerun over the corrected tree, including the
complete historical checker chain, source-marker audits, delimiter structure,
archive path safety, and the regenerated handoff manifest. This assembly
container does not provide the Rust toolchain or the pinned SoX-ng/FFmpeg
closure, so no compiled or pinned-real-tool execution is claimed here. Policy
v15 remains fail-closed and unpromoted pending its commissioned qualification.


## F10 (F9-resolution round) — sox_ng writes a bogus header-only W64 size field for silent content; ffmpeg correctly refuses the file

Your F9 resolution lands fully: all 11 historical checkers green under
the new lineage contract, the crossed-carrier negative test passes,
suite 4,633/0, smoke green, zero cold warnings after an import cleanup
on our side (the loudnorm-parser imports v15 removed from production
use are now test-scoped). The single remaining qualification failure
(`tests/dsd_reference_qualification.rs:462`, ffprobe refusing the
`dsf_uncompressed-2822400-1ch` QPCM) is a NEW toolchain defect —
initially misattributed to ffmpeg, then fully isolated:

```text
sox -D -r 88200 -n -e floating-point -b 64 -c 1 tone.w64  synth 0.1 sine 1000 gain -6
sox -D -r 88200 -n -e floating-point -b 64 -c 1 zeros.w64 synth 0.1 sine 1000 vol 0

both files: 70,696 bytes; sox reads both correctly (8,820 samples)

tone.w64  RIFF-GUID size field: 0x00011428 = 70,696  (correct)
zeros.w64 RIFF-GUID size field: 0x00000088 = 136     (HEADER-ONLY — bogus)
zeros.w64 data-chunk size field: 0x18 = 24           (EMPTY payload declared;
                                  correct field value would be 0x113b8 =
                                  70,584 (W64 sizes include the 24-byte
                                  chunk header); 70,560 bytes of zero
                                  samples are present on disk)

ffprobe tone.w64   -> opens fine
ffprobe zeros.w64  -> "Invalid data" (correctly honoring a size field
                       that excludes the entire data payload)
ffprobe -f w64 …   -> forcing the demuxer does NOT bypass (verified)
```

So: **sox_ng 14.8.0.1's W64 writer finalizes BOTH size fields as an
empty file when the written content is all-zero** (as if header
patch-up never ran), while the full zero-sample payload is present on
disk — a sibling of the F6 streamed-WAV size-wrap defect (its W64/WAV
size accounting has multiple bugs). FFmpeg is exonerated for this
failure class: it honors the declared sizes. sox reports the full
8,820 samples despite both fields declaring empty, so its reader
evidently reads to EOF rather than trusting the size fields — which is
why sox round-trips its own broken files while FFmpeg refuses them. This explains both gate hits:
the verified_silence QPCM (F9.3(b)) and any fixture whose QPCM renders
all-zero.

Resolution shapes under the terminal rules:

1. Route every open/probe of possibly-silent W64 carriers through the
   qualified SoX mechanism (SoX reads its own files correctly), and add
   a permanent all-zero-content W64 fixture to the gate;
2. and/or have the harness/production detect-and-refuse the bogus size
   field explicitly (a 0x88 header-only size on a larger file is
   mechanically detectable) with a diagnostic naming this defect;
3. characterize the exact trigger (all-zero vs threshold vs first-block
   silence) with fixtures while you are in there.

Product owner note (outside your scope): this is the second sox_ng
writer defect for the upstream ledger — one-line-class fix expected in
the fork's W64 size finalization, then pin bump + policy lift.
