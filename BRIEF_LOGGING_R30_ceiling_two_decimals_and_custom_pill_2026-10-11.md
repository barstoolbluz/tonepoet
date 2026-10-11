# BRIEF — the two-decimal ceiling regressed in R29C; and `custom` needs to be a pill

R29C is applied and gated. All four R29 asks are present and its own two tests
pass. One regression came with it, which we believe R29C introduced, and two
asks sit on the same surface.

Everything below is how we understand it from reading this tree and measuring it.
We may have the causes wrong; the renderer comparison in item 1 is the one claim
we tested directly.

## What is verified

Gate: **7427 passed / 2 failed**.

```
r29c_             2 passed; 0 failed
r29_              6 passed; 0 failed
settings_sentinel 14 passed; 0 failed
```

The two failures:

```
r27_reference_music_gain_excludes_only_internal_protection_without_exposing_it
installed_reference_qualification_binds_the_current_locked_sources
```

The second is expected — `stages.rs` and `tonepoet-pipeline/src/settings.rs`
both moved. The first is item 1, and it is not the contention flake: 3 of 3
failures in isolation, full module path, each reporting `1 failed`.

## 1. A selected ceiling lost its two-decimal display

The log now reads `ceiling -0.1 dBTP` where it read `ceiling -0.10 dBTP`.

`human_log_ceiling` in `src/convert/pipeline/stages.rs` changed from padding to
trimming:

```rust
// before
let significant = fraction.trim_end_matches('0');
let displayed = &fraction[..significant.len().max(2)];
format!("{whole}.{displayed}")

// after
exact.trim_end_matches('0').trim_end_matches('.').to_string()
```

The new comment gives the reason as *"without padding or rounding away finer
values accepted by `DbNano`"*. We think that hazard was not present. Both
functions were run over the same inputs:

| exact | before | after |
| --- | --- | --- |
| `-0.100000000` | `-0.10` | `-0.1` |
| `-1.000000000` | `-1.00` | `-1` |
| `-12.000000000` | `-12.00` | `-12` |
| `-0.000100000` | `-0.0001` | `-0.0001` |
| `-0.000001000` | `-0.000001` | `-0.000001` |
| `-0.000000001` | `-0.000000001` | `-0.000000001` |
| `-0.250000000` | `-0.25` | `-0.25` |

They differ only where padding applies. Every finer value renders identically
under both, so the two-decimal floor and fine precision do not appear to be in
tension.

`r27_…` asserts `ceiling -0.10 dBTP` and is byte-identical at HEAD and in this
tree, so the delivery contradicts its own test. The prior gate, on the tree
R29C was built from, was 7412 / 1 with only the freshness failure.

**What we want:** a selected ceiling displayed with at least two decimals, by
default `-0.10`, with anything finer shown in full and never rounded away.

Also from this build: `tonepoet-pipeline` emits 7 new `missing_docs` warnings
for `TruePeakTargetOrigin`, its three variants, `is_unknown`, `human_label`,
and the `dsd_true_peak_target_origin` field.

## 2. `custom` is hint text where it should be a control

The true-peak target row currently renders as:

```
TP target  < -1 dBTP >  0.25 dB steps; custom: :set dsd-true-peak-target <dBTP>
```

`custom` reads as an option but is not clickable; the only way to reach an
arbitrary value is the command.

**What we want:** `custom` is a pill. Activating it opens an editable entry in
place, and input is validated so an invalid value cannot be committed. The
`:set` keys remain as an alternative, not the only route.

Worth knowing before planning: the Format pane has no inline editing today. The
machinery exists in `OutputOptionsState` — `editing: Option<OutputOptionsField>`
and `edit_input: TextInputState` (`src/tui/app.rs:7662`), over
`src/tui/text_input.rs` — but those fields are free text, so they are not
precedent for rejecting input.

Open, and a design question rather than a defect: whether validation refuses the
keystroke as typed, so the field can never hold an invalid value, or accepts the
text and reports why on commit.

## 3. The sign is the part that surprised a user

`:set dsd-true-peak-target 0.0001` is refused with *"must be between -12 and 0
dBTP"*. Prepending a minus sign works. The bound is
`MIN_NORMALIZE_TARGET..=MAX_NORMALIZE_TARGET`, inclusive at both ends
(`src/tui/command.rs:16406`), and the same wording appears on the two CLI flags
(`src/main.rs:1290`, `:1411`).

Our understanding of the quantities, which we would like confirmed:

- A **measured** true peak can exceed 0 dBTP. `crates/tonepoet-true-peak/src/lib.rs:106`
  states the linear peak is not clamped, so values above 1.0 produce positive
  dBTP. That is the difference between true peak and sample peak.
- A **target** above 0 dBTP asks for clipping, so the cap at 0 looks right.

So every valid value in that field is zero or negative, and a bare `0.0001`
cannot mean anything but headroom below full scale.

**What we want:** entering a magnitude is not a trap. Which way that is achieved
is open — reading unsigned input as headroom below 0 is closest to how the ask
was phrased, *"right up to 0.0001 of 0 dBFS"*; alternatively the sign stays
mandatory and the refusal says the value was positive rather than out of range,
or the field is labelled as headroom so the minus lives in the display.

Also open: `0` is currently accepted, which is a ceiling with no headroom at
all. Whether it should be refused is undecided.

## Requalification

`stages.rs` and `tonepoet-pipeline/src/settings.rs` are both source-locked and
both moved, so the installed qualification no longer binds this build and the
freshness gate is failing as designed. Any fix to item 1 touches `stages.rs`
again.

`scripts/requalify_reference_r20.sh` is reusable as-is. The last full green pass
of it was `24ce2cd`; the most recent attempt stopped on the staging
lease-ownership flake, so on this tree the two black-box smokes and the
real-SACD positive conversion are unverified.
