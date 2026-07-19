# Commission Brief: DSD→PCM Reference Pathway + Manual/Expert Workflow System

## What this document is

This is a **design commission**, not an implementation brief. Your deliverable for
this round is a **design brief** — the document that a subsequent round (you,
again) will implement. We are deliberately splitting design from implementation
because the Manual/Expert workflow system is an architecture problem, and the
designer should be the implementer.

Two evidence documents ship in this bundle under `docs/`:

- `tonepoet_dsd_to_pcm_guidance_evidence_based_v9.md` — the product policy
  (Reference chain, reconstruction profiles B1–B6, headroom rule, gain
  convention, qualification requirements).
- `sox_ng_dsd_decimation_test_report_v5.md` — the empirical qualification of
  SoX-ng 14.8.0.1's native DSD decimation path that the policy rests on.

Both were produced by controlled measurement against SoX-ng 14.8.0.1
(tag `sox_ng-14.8.0.1`, commit `266aa9e777829a1b60959a1250b4c42597558639`).
Treat their findings as authoritative for that release. Where this commission
and those documents conflict, flag the conflict in your design brief rather
than silently picking one.

### Process contract

1. **This round:** read both evidence documents and the current code, then
   author `docs/brief_dsd_reference_design.md` (name it that) meeting the
   deliverable specification in §7.
2. The design brief will be validated locally (compile-free audit against the
   tree and the evidence documents).
3. **Next round:** you implement your own validated design under the standing
   complete-file delivery contract.

You have no compiler and no network. Every claim you make about current code
must carry a `file:line` reference into this bundle. Everything below has been
mechanically verified against the tree at the commit named in the bundle
manifest.

---

## 1. Toolchain status

- The flake currently pins `github:barstoolbluz/sox_ng` rev `482801f`
  (SoX_ng **v14.6.1**) — see `flake.lock`, node `sox_ng`.
- The fork's pin is being advanced to **14.8.0.1** upstream; the implementation
  round will build and test against 14.8.0.1. Design for 14.8.0.1 semantics
  (the release the evidence qualifies).
- Consequence the design must own: 14.8.0.1's native DSF/DSDIFF handlers
  advertise **only DSD64/128/256**. DSD512/DSD1024 sources cannot use the
  native SoX path at all (v5 §"Native DSD format limits").

## 2. Current state — verified inventory

### 2.1 The one existing pathway

All DSD sources route unconditionally into `plan_from_dsd`
(`tonepoet-pipeline/src/plan.rs:613`, body at `plan.rs:1161-1244`). There is no
rate gate and no FFmpeg alternative: `PlanOperation::DsdToPcm` dispatches only
to `build_sox_dsd_to_pcm` (`tonepoet-pipeline/src/plugins.rs:296-308`,
`plugins.rs:1147-1170`). A DSD512/1024 DSF will therefore be handed to a SoX
build whose reader rejects it — a latent runtime failure under 14.8.0.1.

The effect chain is assembled by `add_sox_dsd_to_pcm_effects`
(`plugins.rs:1614-1689`). For `DsdLowpassMethod::Auto | SoxUltra` (the only
reachable arms — see §2.3):

```text
sox -S <in> <fmt/depth args> <out>
  rate -u <target_hz>
  [ sinc -a 180 -<cutoff>      # only when cutoff < target Nyquist ]
  [ norm -<margin> | gain ±X ] # per gain mode, see §2.2
  [ dither args ]              # when depth ≤ 24-bit and dither != None
```

- Cutoffs come from `DsdRate::default_pcm_lowpass_hz`
  (`tonepoet-pipeline/src/enums.rs:342-349`): DSD64→25 kHz, DSD128→48 kHz,
  DSD256→96 kHz, DSD512/1024→None.
- The Nyquist guard (`plugins.rs:1669`) skips the sinc when
  `cutoff ≥ target_hz/2`. Consequence: **DSD256 → 176.4/192 kHz runs with no
  noise strip at all** (96 kHz ≥ 88.2/96 kHz), and DSD128 → 88.2/96 kHz
  likewise.
- Default PCM target when the user selects rate "source":
  `DsdRate::default_pcm_target_hz` (`enums.rs:322-330`): 88.2/176.4/352.8/
  352.8/705.6 kHz for DSD64/128/256/512/1024.
- **There is no processing headroom.** No `-G`, no pre-chain attenuation, no
  restoration. The gain stage (when enabled) runs *after* rate+sinc, so the
  intermediate effect boundaries are exposed exactly as v5's stress fixtures
  clipped ("Headroom result").
- Dither is appended by the same function (`plugins.rs:1681-1687`) via
  `target_depth_needs_dither` (Int8/16/24 only).

### 2.2 Gain modes

`add_sox_dsd_to_pcm_gain` (`plugins.rs:1712-1741`),
`DsdToPcmGainMode` (`enums.rs:709-722`, default `Disabled`):

- `Auto` → `norm -<margin>` — **programme-dependent peak normalization**, with
  margin default 0.15 dB (`tonepoet-pipeline/src/settings.rs:748-750`). This is
  the policy v9 §3 explicitly rejects for the Reference path ("Do not use …
  track-by-track gain changes"; normalization ≠ DSD level compensation).
- `Manual` → `gain ±X` from `dsd_to_pcm_gain_db` (fails loudly when None).
- `Disabled` → nothing, except a compat path honoring a stray
  `dsd_to_pcm_gain_db`.

### 2.3 What users can actually reach

Verified surface map (agent-swept, then spot-verified):

| Field | CLI | TUI | `:set` | Presets | config.toml |
|---|---|---|---|---|---|
| `dsd_to_pcm_lowpass` | ✗ | ✗ | ✗ | ✗ | ✗ |
| `dsd_to_pcm_gain_mode` / `gain_db` | ✗ | ✓ pills | ✗ | ✗ | ✗ |
| `dsd_to_pcm_auto_gain_margin_db` | ✗ | ✗ (no control) | ✗ | ✗ | ✗ |
| `dsd.sinc.*` (SincFilterSettings) | ✗ | ✗ | ✗ | ✗ | ✗ |
| profile/cutoff selection | ✗ | ✗ | ✗ | ✗ | ✗ |

- CLI: `run_convert` builds legacy `ConversionOptions`; the bridge hardcodes
  `settings.dsd = DsdSettings::default()`
  (`src/convert/pipeline/unified_request.rs:531`) — so CLI DSD→PCM is always
  Auto lowpass + Disabled gain. The settings sentinel records
  `dsd.dsd_to_pcm_lowpass` as `LegacyProjectionStatus::Unrepresentable`
  (`tests/settings_sentinel.rs:876`).
- TUI: the only DSD→PCM controls are the gain-mode pill and manual-dB row,
  shown when `dsd_to_pcm_gain_available()` (source DSD, target PCM) —
  `src/tui/draw_output.rs:164-179`, `src/tui/app.rs:3538-3542`. The TUI builds
  `DsdSettings::default()` and overrides *only* the gain fields
  (`src/tui/convert_actions.rs:375-400`). The margin has no UI control.
- Therefore `DsdLowpassMethod::Sinc` — which *is* implemented
  (`plugins.rs:1622-1639`: user FIR from `dsd.sinc` pre-rate, then `rate -I`)
  — is **dead in practice**: nothing in the program ever sets
  `dsd_to_pcm_lowpass` away from `Auto`. Note this contradicts any assumption
  that users "can change the pathway": today they cannot, except gain.
- `SincFilterSettings` (`settings.rs:784-810`) defaults are sized for PCM→DSD
  upsampling (262,144 taps, 8× oversample) — unusable as-is for a DSD→PCM
  design; its dual use by the dead Sinc arm is a design smell to resolve.
- The *separate* `sox_resampler.sinc_*` fields (`settings.rs:661-673`) are
  PCM-resampling FIR pre-filter knobs, TUI-advanced-only
  (`convert_actions.rs:451-456`), nulled by the legacy bridge
  (`unified_request.rs:562-573`). Your design must state how these relate to
  (or are firewalled from) the DSD profile system.

### 2.4 Missing infrastructure

- **No pre-quantization measurement step exists.** True peak only appears
  post-encode via loudgain for ReplayGain tagging. The v9 gain convention
  (constant +6.0206 dB capped to −1 dBTP) requires measure-then-apply on the
  *converted, still-headroomed* audio — the plan/step architecture
  (`PlanOperation` → `PlannedCommand`, executed in
  `src/convert/pipeline/stages.rs`) has no step whose output parameterizes a
  later step's arguments. This is a real architectural addition your design
  must specify.
- **No user-composable pipeline mechanism exists anywhere.** No raw-args
  passthrough, no effect-chain schema, no workflow files (swept: no
  `custom_effects`/`extra_args`/`raw_args` surfaces).

## 3. Product requirement R1 — Auto/Reference pathway

Implement v9's Reference path as tonepoet's default DSD→PCM behavior for
DSD64/128/256 DSF/DFF via native SoX-ng:

```text
native DSD reader
  → explicit fixed processing headroom (provisional −12 dB; see v9 §2)
  → rate -u directly to the requested PCM rate
  → profile sinc at the PCM rate, -a 180, when the profile fits (≥88.2 kHz targets)
  → PCM-domain effects
  → programme measurement (true peak)
  → one constant restoration + DSD level compensation, ceiling −1 dBTP
  → one final quantization: TPDF at 24-bit, qualified Shibata-family at 16-bit,
    none for float
  → lossless encode
```

Profile table (passband/stopband pairs replace today's single cutoffs):

| Source | Auto/Reference | Optional Wideband (explicit only) |
|---|---|---|
| DSD64 | B3: 25–35 kHz | — |
| DSD128 | B4: 30–45 kHz | B4W: 35–50 kHz |
| DSD256 | B5: 48–70 kHz | B6: 88.2–140 kHz (provisional) |

with the effective-profile rule (v9 §4): `narrower-of(requested/Auto profile,
source ceiling, target ceiling)`; 44.1/48 kHz targets use `rate -u` alone (B1/
B2). A higher target rate never auto-widens the profile. This fixes both
current defects: DSD128's deprecated 48 kHz cutoff and DSD256's skipped sinc at
176.4/192 kHz.

Specific decisions your design must make and justify:

- **Gain semantics.** The Reference constant-gain-with-ceiling policy becomes
  the new Auto. Decide the fate of the current `norm`-based Auto (e.g. a
  distinct `Normalize` mode) and of `Disabled`/`Manual`, including serde/
  fingerprint compatibility (`tonepoet-pipeline/src/fingerprint.rs:340-360`
  hashes the gain mode; changed semantics must change fingerprints honestly).
- **Headroom qualification.** −12 dB is provisional (v5 measured ~5.5–6.3 dB
  FIR + ~3 dB rate reserve). Specify the reserve per profile, where it is
  applied/restored, and the test that pins it.
- **Measurement step architecture** for the −1 dBTP ceiling: which tool
  (sox stats? ffmpeg astats/ebur128 — note "true peak" strictly needs
  oversampled measurement), where in the plan it runs, and how its result
  parameterizes the gain command (deferred-arg resolution at stage execution in
  `stages.rs` is acceptable; design the contract).
- **Single-quantization guarantee.** Today depth/dither are applied inside the
  same sox invocation — fine — but multi-step plans (WAV intermediate →
  `push_encode_final`, `plan.rs:1221-1243`) must provably not re-quantize.
  State the invariant and the pin.
- **DSD512/1024 and DST routing.** Fail closed with a clear planner error, or
  qualify an FFmpeg decode path (v9 §10) — your choice, but silent handoff to
  a rejecting SoX binary is not acceptable. If FFmpeg: define its source
  ceiling and Auto profile from measured decoder behavior, or explicitly defer
  with a fail-closed stub.
- **Continuous programmes** (v9 §8): state how the CUE/SACD image pipeline
  (materialize → convert → split) interacts with "process before track split",
  or scope it out explicitly with rationale.
- **Log/documentation wording** (v9 "Stopband-rejection wording"): the
  conversion log currently prints the lowpass method
  (`src/convert/pipeline/stages.rs:16883-16887`); extend it to record profile,
  headroom, measured peak, applied gain, and the qualified-rejection phrasing —
  never a "−180 dBFS noise floor" claim.

## 4. Product requirement R2 — Manual/Expert workflow system

A structured way for a user to define their own DSD→PCM (and generally
audio→audio) pipeline using sox_ng, ffmpeg, and/or ssrc stages, per v9 §9:

- Arbitrary ordered stages; each stage names a backend and its args/effects.
- **No silent Auto injection**: no implicit Reference sinc, `rate -u`, gain,
  headroom, dither, or compensation added to a manual workflow.
- Tonepoet may lint/warn (likely aliasing, duplicate resample, premature
  quantization, missing headroom, multiple dither) but must not rewrite.
- Workflows must be persistable (design the schema — TOML in the existing
  preset/config ecosystem is the natural fit) and selectable per conversion.
- Define the trust boundary: workflows execute user-specified args against
  tools tonepoet already invokes; state how paths/outputs are still owned by
  the pipeline (staging, atomic publish, logging) even when the middle of the
  chain is user-authored.
- Decide the fate of the dead `DsdLowpassMethod::Sinc` arm and the dual-use
  `SincFilterSettings`: fold into the workflow system, repurpose as a built-in
  expert profile, or delete — no dead reachable-looking surfaces left behind.

## 5. Exposure requirements

The design must make the Auto/Reference vs Manual choice, profile selection
(Reference vs explicit Wideband), gain mode, and workflow selection reachable
from:

- **TUI**: extend the format pane / advanced overlays; respect the existing
  pill + `FormatField` navigation architecture (`src/tui/app.rs:2938-2993`)
  and the two-pass draw/button-map convention.
- **CLI**: new flags on `convert` (design them; the legacy
  `ConversionOptions` bridge is `Unrepresentable` for these — decide whether
  to extend the bridge or require the full-`PipelineRequest` path, and keep
  `tests/settings_sentinel.rs` honest either way).
- **`:set` command mode** where it fits the existing grammar
  (`src/tui/command.rs`).
- **Presets**: persist the new fields in `TuiPreset`
  (`src/tui/presets.rs`) with graceful deserialization of old preset files.

## 6. Constraints

- Workspace conventions per `CLAUDE.md` (edition 2021, thiserror in libs,
  ratatui 0.26 pill architecture, module re-exports).
- `cargo test --workspace` green; zero cold-build warnings; sub-crate tests
  included.
- `fingerprint.rs` must hash every new behavior-affecting field.
- `StubToolRunner` transcript pins are the established pattern for asserting
  exact sox/ffmpeg argv — use them for the Reference chain and the
  no-injection guarantee of manual workflows.
- Runtime qualification tests that need real sox_ng go behind the existing
  `TONEPOET_REQUIRE_TOOLS` gate.
- v9 §11's 17 qualification requirements: your design brief must map each to
  {automated test | tool-gated test | documented manual qualification |
  explicitly out of scope}, with rationale. Order-null results must never be
  presented as stopband rejection anywhere (tests, logs, docs).
- Serde compatibility: existing queue persistence and preset files must load.

## 7. Deliverable specification for your design brief

`docs/brief_dsd_reference_design.md` must contain:

1. **Settings & type design** — new/changed enums and structs with exact field
   lists, defaults, serde strategy, fingerprint additions.
2. **Planner design** — new/changed `PlanOperation` variants, routing rules
   (including DSD512+/DST), the measurement/deferred-gain step contract.
3. **Command assembly** — the exact argv the Reference chain emits per
   (source rate × target rate × depth) cell, including headroom and
   restoration; and the manual-workflow argv assembly rules.
4. **Workflow schema** — the persistable manual-pipeline format with examples
   (the v9 §9 FFmpeg→SoX→SSRC→FFmpeg chain as a worked example).
5. **Exposure design** — TUI/CLI/`:set`/preset surfaces, with interaction
   rules against existing pills (dither pill vs Reference quantization policy,
   resampler pill vs DSD path, `sox_resampler.sinc_*` firewall).
6. **Test plan** — the §6 qualification mapping, transcript pins, sentinel
   updates, migration tests.
7. **Phasing** — an ordered implementation plan sized for one apply round, or
   an explicit two-round split with a stable seam.
8. **Compatibility & migration** — what happens to existing presets, queues,
   TUI state, and the current gain modes; what fingerprints change and why.
9. **Open questions** — anything genuinely requiring a product decision,
   stated as a decidable question with your recommendation.

Ground every reference to current code in `file:line` against this bundle.
Do not include implementation code in the design brief beyond short
illustrative signatures/argv examples.
