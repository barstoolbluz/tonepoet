# Brief R24 — terminal dither after CD de-emphasis, with conservative automatic policy and optional advanced shaping

Date: 2026-10-04

Base: use the most recent supplied TonePoet full-repository snapshot. If this brief names a bundle differently from the attachment, the attached bundle is authoritative.

## Assignment

Implement the smallest coherent change that makes TonePoet handle the final requantization after digital CD de-emphasis correctly.

The core policy distinction is deliberate:

> **Plain TPDF is automatic quantization hygiene. Shibata is an optional mastering choice.**

TonePoet must therefore:

- automatically use **plain TPDF** when digitally de-emphasized audio ultimately lands on 16-bit integer PCM and the user has not explicitly overridden dither;
- never select Shibata automatically;
- optionally expose **Shibata** as an advanced user choice for final 16-bit output where the current terminal realization can support it correctly;
- preserve the high-precision Float64 de-emphasis path and apply dither only once, at the terminal Int16 quantization boundary.

This is a narrow DSP-policy and UI correction. It is not an invitation to redesign the planner, add a general mastering subsystem, or broaden dither policy across unrelated paths.

We are aiming for world-class engineering, but proportionality matters. Prefer the smallest integration-friendly correction that preserves existing performance, planner ownership, and qualified terminal behavior.

When finished, generate a downloadable complete repository snapshot/code bundle, concise delivery notes, verification evidence, and `HANDOFF_TO_NEXT_SESSION.md`.

---

# 1. Technical basis

Digital CD de-emphasis is an audio filter, not a metadata-only operation.

TonePoet's existing architecture already does the important high-precision part correctly:

- the registered SoX-ng de-emphasis stage uses `-D`, so SoX-ng does not dither there;
- the SoX-ng registered-effect path writes a 64-bit floating PCM carrier;
- the FFmpeg registered-effect path uses `aemphasis=mode=reproduction:type=cd`;
- the FFmpeg registered-effect path writes `pcm_f64le`;
- the semantic registered-effect contract treats the result as a floating-point / Float64 signal.

Preserve that architecture.

The external tools behave consistently with the intended policy:

- SoX/SoX-ng normally use plain TPDF automatically when reducing processed audio below 24-bit precision, unless automatic dither is disabled;
- a bare SoX `dither` effect is plain TPDF;
- FFmpeg `aemphasis` operates in double precision;
- libswresample does not enable dither by default;
- FFmpeg/libswresample supports triangular dither, which is the ordinary TPDF realization;
- SoX/SoX-ng and FFmpeg/libswresample also support shaped dither such as Shibata.

The important product distinction is:

- **TPDF** removes signal-correlated quantization error in the least opinionated conventional way;
- **Shibata** intentionally noise-shapes the quantization noise for perceptual benefit and is therefore a mastering choice rather than a neutral automatic correction.

TonePoet should make the former automatic and the latter deliberate.

---

# 2. Governing behavior

## 2.1 Automatic TPDF after active CD de-emphasis

When all of the following are true:

1. `RegisteredUnaryEffect::CdDeemphasis` is actually applied;
2. the final target is PCM lossless;
3. the final resolved PCM storage depth is `PcmBitDepth::Int16`;
4. the user has not explicitly overridden dither;

the effective automatic dither choice must be:

    TPDF

This applies even when the nominal source storage width is also Int16.

The reason is the intervening floating-point filtering, which produces sample values that no longer lie on the original 16-bit quantization grid.

This rule must also apply when `BitDepthTarget::Source` resolves to Int16.

## 2.2 Exactly one terminal quantization

The DSP order must remain:

    source/decode
      ->
    CD de-emphasis in high precision
      ->
    any later DSP, including sample-rate conversion
      ->
    one terminal dither + Int16 quantization
      ->
    lossless packaging

Never:

    de-emphasis
      ->
    dither to Int16
      ->
    later resampling or other DSP

The registered de-emphasis stage must therefore continue to emit Float64 without dither.

Do not restore SoX-ng automatic dither inside that stage.

Do not make the FFmpeg `aemphasis` stage emit Int16.

Do not introduce a second temporary conversion merely to add dither.

## 2.3 Automatic policy is always plain TPDF

Automatic behavior for this feature must always choose **plain TPDF**.

Do not automatically choose:

- Shibata;
- high-pass triangular dither;
- Lipshitz;
- F-weighted curves;
- Gesemann;
- any other noise-shaped or psychoacoustically weighted mode.

Automatic CD de-emphasis should remain suitable for archival/conversion use and should not silently make a mastering decision.

---

# 3. Advanced dither choice

## 3.1 Shibata may be exposed, but only as an explicit advanced choice

TonePoet may expose **Shibata** for users intentionally producing a final 16-bit listening master.

It must be:

- behind an Advanced disclosure or equivalent low-prominence control;
- explicitly user-selected;
- never selected by target changes, de-emphasis changes, or automatic policy;
- clearly associated with the final 16-bit output quantization, not with the de-emphasis filter itself.

A suitable conceptual label is:

    16-bit output dither

not:

    de-emphasis dither

because the dither belongs to the terminal quantization boundary.

## 3.2 Reuse the existing Dither setting

Do not invent a second independent dither subsystem or a separate de-emphasis-only dither state.

If TonePoet already has a Dither field and explicit override provenance, the Advanced control on the De-emphasis row should reuse, proxy, or reveal that existing state.

Preserve the existing mechanisms such as:

- `dither_explicit`;
- `dither_overridden`;
- the existing dither enum/type;
- the existing terminal realization machinery.

There must remain one authoritative dither setting.

## 3.3 Recommended advanced choices

Prefer a deliberately small user-facing set.

If compatible with the existing Dither model, the advanced UI should conceptually offer:

- `Auto (TPDF)` — recommended/default behavior for qualifying Int16 de-emphasis output;
- `TPDF` — explicit plain triangular dither;
- `Shibata` — explicit noise-shaped mastering choice;
- `None` — expert override.

Do not expose every shaping curve merely because the backend supports it.

If the existing TonePoet UI already exposes a broader dither list globally, do not duplicate or contradict it. Reuse that existing list and preserve its semantics, but the De-emphasis row should not increase the visible complexity of the normal workflow.

Do not add rectangular dither merely for completeness.

Do not add high-pass TPDF unless it is already an established TonePoet option with existing semantics.

---

# 4. Backend realization

Reuse existing qualified terminal ownership.

## 4.1 SoX / SoX-ng terminal

For plain TPDF:

    dither

must remain the existing plain TPDF realization.

For explicit Shibata, use the existing SoX/SoX-ng shaped-dither form if TonePoet already supports or can add it locally without changing terminal ownership.

The registered de-emphasis subprocess must retain `-D` and Float64 output.

## 4.2 FFmpeg / SoXR terminal

For plain TPDF, reuse the existing libswresample mapping:

    dither_method=triangular

For explicit Shibata, use the existing libswresample Shibata dither mode if the current qualified terminal can realize it cleanly.

The registered `aemphasis` stage must remain Float64.

## 4.3 SSRC terminal ownership

Do not break existing SSRC ownership merely to expose Shibata.

If an existing qualified SSRC path owns the final Int16 landing and its current supported dither realization is plain TPDF:

- automatic behavior must continue to use that existing TPDF realization;
- do not append a second downstream dither/quantization stage merely to force Shibata;
- do not duplicate quantization;
- package-only continuations must remain package-only.

For an explicit Shibata request:

- use Shibata only if an already-qualified terminal realization can own the final quantization without violating current planner invariants;
- otherwise mark Shibata unavailable for that realization, or preserve the user's current compatible backend choice according to existing UI/planner conventions;
- do not broaden this task into a planner redesign solely to make every backend combination support Shibata.

A user-selected optional mastering mode does not justify breaking a stable terminal-ownership model.

---

# 5. User authority

The automatic rule is a default, not an unconditional override.

If the user explicitly selects a Dither setting, preserve it.

That includes:

- explicit TPDF;
- explicit Shibata;
- any existing supported explicit alternative;
- explicit None.

Do not silently replace an explicit dither choice merely because De-emphasis is On.

Toggling De-emphasis must not clear `dither_overridden` or equivalent provenance.

Target changes must not clear it either.

The Advanced control must operate through the same explicit-authority mechanism as the existing Dither field.

---

# 6. TUI behavior

The ordinary UI should remain simple.

For an eligible pre-emphasized 16-bit / 44.1 kHz source:

### De-emphasis Off

Use the existing ordinary automatic dither rules.

A normal 16 -> 16 lossless path with no other DSP may therefore remain `None`.

### De-emphasis On, final Int16 lossless PCM, dither automatic

The effective dither becomes:

    TPDF

The normal row should not force the user to choose anything else.

### Advanced disclosure

The De-emphasis row may expose an Advanced control containing the existing terminal Dither setting or a proxy to it.

Its copy should make the distinction clear.

A suitable conceptual presentation is:

    Advanced
      16-bit output dither: Auto (TPDF)

If the user opens the choice:

    Auto (TPDF)
    TPDF
    Shibata
    None

subject to the existing TonePoet dither model and backend capabilities described above.

Do not describe Shibata as "better".

If explanatory copy is needed, prefer language equivalent to:

- `TPDF — neutral default for requantizing to 16-bit`
- `Shibata — noise-shaped option for a final 16-bit listening master`
- `None — disable dither`

Keep that copy short.

### Applicability

The advanced 16-bit dither control should be inactive, hidden, or clearly irrelevant according to existing UI conventions when the final target does not have a terminal Int16 quantization.

Examples:

- Int24 PCM;
- Int32 PCM;
- Float32;
- Float64;
- DSD;
- AAC;
- MP3;
- Opus;
- other lossy targets.

Do not quantize a lossy encode path to Int16 merely to make this option applicable.

---

# 7. Recalculation rules

R23 made De-emphasis itself target-dependent.

Automatic Dither must therefore observe the **final current De-emphasis state** after target constraints have settled.

When no explicit dither override exists:

- manually turning De-emphasis On must recompute automatic dither;
- manually turning De-emphasis Off must recompute ordinary dither policy rather than blindly forcing None;
- an R23 target-driven automatic change in De-emphasis must also recompute dither;
- sample-rate and bit-depth changes that alter the final Int16 applicability must recompute dither.

Be careful about ordering.

Do not allow this sequence:

    target changes
      ->
    dither computed from stale De-emphasis
      ->
    De-emphasis automatically changes afterward

Prefer extending or reusing the current `apply_auto_dither()` / constraint cascade instead of adding a second UI policy engine.

---

# 8. Planner correctness must not rely only on the TUI

A CLI request, restored state, unit test, future caller, or non-TUI frontend can construct a `PipelineRequest` without passing through UI default logic.

Therefore the planning/lowering boundary itself must guarantee the same effective policy.

Where all of these are true:

- `CdDeemphasis` is actually registered;
- final target resolves to lossless Int16 PCM;
- dither is not explicit;

the planner must derive effective TPDF even if the incoming request's nominal dither field still contains an ordinary default value.

Reuse existing helpers such as `request_applies_cd_deemphasis()` and existing target-depth resolution.

A narrow derived-policy correction is preferred.

Do not generalize this task into:

> every Float64 intermediate automatically forces TPDF

unless the current architecture demonstrably requires that formulation.

Scope the new automatic policy to CD de-emphasis landing on Int16.

---

# 9. Negative scope

Do not add a new de-emphasis-specific automatic dither requirement for:

- Int24 output;
- Int32 output;
- Float32 output;
- Float64 output;
- DSD;
- AAC;
- MP3;
- Opus;
- other lossy codec output;
- WavPack hybrid merely because de-emphasis occurred.

For lossy targets, preserve the high-precision filtered signal into the encoder.

Do not noise-shape automatically.

Do not treat Shibata as a required correctness feature.

Do not add a mastering preview system.

Do not add loudness compensation, gain changes, headroom management, clipping protection, or unrelated DSP under this brief unless an existing required path demonstrably breaks without a local correction.

---

# 10. Focused verification

Add only the tests needed to prove the new policy and preserve existing terminal ownership.

## 10.1 Core automatic case

Input:

- 16-bit / 44.1 kHz source;
- CD De-emphasis On;
- final lossless Int16 target;
- no explicit dither override.

Prove:

- de-emphasis still emits Float64;
- SoX registered-effect lowering still contains `-D`;
- FFmpeg registered-effect lowering still emits `pcm_f64le`;
- effective automatic dither is TPDF;
- exactly one TPDF realization exists;
- the TPDF realization occurs at the final Int16 landing.

## 10.2 Resampling composition

Exercise:

    CD de-emphasis
      ->
    sample-rate conversion
      ->
    final Int16 lossless output

Prove:

    deemphasis < resample < dither/quantize

and never:

    deemphasis < dither < resample

## 10.3 SoX terminal

Prove:

- one bare `dither` terminal effect for automatic TPDF;
- no early dither inside the registered de-emphasis command;
- no duplicate dither.

If Shibata is supported through this terminal, add one focused explicit-Shibata test proving the shaped mode is selected only when requested.

## 10.4 FFmpeg/SoXR terminal

Prove automatic TPDF produces the existing:

    dither_method=triangular

at the terminal Int16 realization.

If Shibata is supported through this terminal, add one focused explicit-Shibata test proving the appropriate existing libswresample shaped mode is selected only when requested.

Do not expand this into exhaustive testing of every libswresample dither curve.

## 10.5 SSRC composition

Use an already-qualified SSRC cell.

Prove:

- de-emphasis precedes SSRC;
- automatic TPDF is realized exactly once by the existing authoritative Int16 landing;
- package-only downstream stages do not dither again.

If Shibata is unavailable under this ownership model, test the UI/planner behavior for that unavailable explicit choice only if TonePoet already has a convention for capability-dependent settings.

Do not redesign SSRC terminal ownership merely to make Shibata available.

## 10.6 `BitDepthTarget::Source`

Where authoritative source depth resolves to Int16:

- De-emphasis On;
- automatic dither;

must resolve to TPDF exactly as explicit Int16 does.

## 10.7 Negative controls

Verify the new automatic CD-deemphasis rule does not inject TPDF for:

- Int24 output;
- floating-point output;
- lossy codec output;
- De-emphasis Off.

## 10.8 Explicit authority

Verify:

- automatic Int16 + De-emphasis On -> TPDF;
- explicit None remains None;
- explicit TPDF remains TPDF;
- explicit Shibata remains Shibata on a supported terminal;
- an existing other supported explicit dither remains unchanged;
- De-emphasis toggles do not clear dither override provenance;
- target changes do not clear explicit dither provenance.

## 10.9 TUI recomputation

Focused state tests should prove:

1. eligible lossless Int16 target + De-emphasis Off -> ordinary automatic dither result;
2. manually turn De-emphasis On -> automatic TPDF;
3. turn De-emphasis Off -> ordinary automatic rule restored;
4. explicitly select Shibata -> subsequent De-emphasis/target changes preserve the explicit choice where applicable;
5. explicitly select None -> subsequent De-emphasis/target changes preserve None;
6. an R23 target-driven De-emphasis transition recomputes dither from the resulting De-emphasis state;
7. Advanced UI never makes Shibata the automatic choice.

## 10.10 No duplication

For representative SoX, FFmpeg, and SSRC plans, count the dither realization or equivalent command fragment and prove there is exactly one.

This is more valuable than adding broad generic tests.

---

# 11. Preserve all prior behavior

Preserve R23 behavior:

- De-emphasis default follows the target;
- a manual De-emphasis choice remains sticky;
- CUE/catalog evidence remains advisory according to the current authority model;
- R23 wording and popup text remain unchanged;
- pre-emphasis signaling/tag handling remains correct;
- lossless 16/44.1 with De-emphasis Off retains signaling where supported;
- De-emphasis On suppresses/strips stale pre-emphasis signaling as already implemented.

Preserve existing:

- true-peak behavior;
- terminal realization ownership;
- SSRC qualified cells;
- package-only continuations;
- source-depth semantics;
- WavPack semantics;
- rate-conversion behavior;
- DSD paths;
- codec paths;
- performance characteristics.

Do not change the command topology of unrelated conversions.

---

# 12. Performance and integration constraints

The policy decision itself should be effectively free.

Do not add:

- an additional audio pass;
- extra file scanning;
- another temporary carrier;
- a new DSP subprocess solely for policy;
- duplicate planner abstractions;
- a generalized precision framework solely for this requirement.

The only additional audio work in the automatic case should be the terminal TPDF operation already required by the Int16 landing.

Optional Shibata must reuse an existing qualified terminal realization.

If supporting Shibata for some backend combination would require disproportionate planner changes, do less: keep automatic TPDF correct and constrain Shibata availability to already-compatible terminals.

---

# 13. Build and validation

Run the narrowest relevant tests first.

At minimum:

    cargo test -p tonepoet-pipeline

plus focused tests covering the bridge/planner and TUI changes.

Then run the normal feasible repository checks.

The prior R23 brief records that the root `tonepoet` crate can exceed a 4 GB build ceiling. If the current environment cannot compile that crate:

- do not compromise the implementation merely to force the build;
- run all feasible pipeline/static tests;
- inspect root changes carefully;
- disclose exactly what was not compiled.

Inspect `reference_source_lock.rs` before modifying files.

If any modified file is Reference-source-locked, state that explicitly in delivery notes so qualification can be rerun on the proper host.

Never claim a test ran when it did not.

---

# 14. Final self-audit before packaging

Audit specifically for:

- duplicate dither;
- dither before later DSP;
- accidental Int16 materialization immediately after de-emphasis;
- automatic Shibata;
- Shibata silently replacing TPDF;
- loss of explicit user dither authority;
- stale automatic dither after R23 target-driven De-emphasis changes;
- planner/TUI disagreement;
- SSRC terminal-ownership regressions;
- package-only stages becoming audio-processing stages;
- accidental dither on lossy or >16-bit targets;
- a second de-emphasis-specific dither state;
- unnecessary abstraction or scope expansion.

Fix any real issue before handoff.

---


---

# Also in scope — the information pop-up and its glyph

Three field-reported defects on the `ⓘ` pop-up that the De-emphasis row opens.
They are independent of the dither policy above and can be done alongside it.

## 1. It is a fixed size and looks cramped

`draw_notice` renders through the shared scrollable popup with
`preferred_width: 66` and `min_height: 9`.

Ratio-based sizing already exists elsewhere in the TUI —
`centered_rect(percent_x, percent_y, area)` in `src/tui/disc_browser.rs` and
`src/tui/conversion_actions_ui.rs`.

The pop-up sizes itself to the terminal the way those prompts do, and its text
is not cramped.

## 2. The `Esc` pill is not clickable

The footer renders `footer_pill("Esc close", theme.purple, theme)`.
`draw_scrollable_message_popup` receives no button map, so nothing registers a
hitbox for it. `draw_file_input`, `draw_text_edit` and `draw_command_input`
each receive one.

The `Esc` pill closes the pop-up when clicked, as it appears to offer.

## 3. The `ⓘ` does not read as an affordance

It renders as plain text in the row suffix — `pill_row` takes `suffix: &str` —
so it carries no colour of its own and sits on the default ground.

The glyph reads as something to click: a solid background and a contrasting
foreground, both drawn from the active theme, consistent across themes.

## The outcome we want

The information pop-up is sized like the TUI's other prompts, its `Esc` pill
works when clicked, and the `ⓘ` that opens it looks like a control.

# Desired outcome

The completed behavior should be accurately summarized by this statement:

> TonePoet performs CD de-emphasis at high precision, preserves that precision through any later DSP, and when the final signal automatically lands on 16-bit integer PCM, applies one neutral TPDF dither at the terminal quantization boundary. Users who deliberately want a noise-shaped final 16-bit listening master may select Shibata through an advanced explicit setting, but TonePoet never chooses Shibata automatically.

Do not do more than is needed to make that statement reliably true.
