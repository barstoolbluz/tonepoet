# Findings: Reference-Admission Corrective Round (applied at HEAD)

**Status:** your admission corrective is applied and mostly lands: the
default-settings live smoke passes (D1 resolved — DSD64 DSF → FLAC works
again), all four SACD plan tests pass with no TOC I/O (D2 resolved), the
v5 terminal-bound generator `--check` passes, cold warnings are zero, and
the workspace suite is green outside the two findings below. One
apply-side mechanical fix on our side: a type annotation in the
qualification test's diagnostic closure (`tests/dsd_reference_qualification.rs`
`pre.map(|value: &TruePeakMeasurement| …)`); review and keep.

These two findings are yours to resolve. Evidence only; nothing was
patched around them.

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

## Constraints

Complete-file delivery; do not expand scope. Gates: full workspace suite,
`derive_dsd_reference_v5_terminal_bounds.py --check`, tool-gated
qualification green, the default-settings live smoke, zero cold
warnings. The pre-existing legacy-gain test must pass **unmodified**.
