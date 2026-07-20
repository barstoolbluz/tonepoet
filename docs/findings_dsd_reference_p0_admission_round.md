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
