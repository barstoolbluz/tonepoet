# BRIEF — the two-decimal ceiling regressed in R29C; and `custom` needs to be a pill

R29C is applied, gated and committed as `572063d`. All four R29 asks are
present and R29C's own tests pass. One regression came with it, and two asks
sit on the same surface.

What follows is how this reads to us from the outside. We misapprehend this
codebase routinely, and the causes we name are guesses; the one claim we
tested directly is the renderer comparison in item 1. Where we are wrong,
trust your own reading.

## What is verified

Gate: **7427 passed / 2 failed** — `r29c_` 2/2, `r29_` 6/6,
`settings_sentinel` 14/14.

```
r27_reference_music_gain_excludes_only_internal_protection_without_exposing_it
installed_reference_qualification_binds_the_current_locked_sources
```

The second is expected; `stages.rs` and `tonepoet-pipeline/src/settings.rs`
both moved. The first is item 1, and it is not the contention flake — 3 of 3
failures in isolation.

## 1. A selected ceiling lost its two-decimal display

The log reads `ceiling -0.1 dBTP` where it read `ceiling -0.10 dBTP`.
`human_log_ceiling` changed from padding to two decimals to trimming trailing
zeros, and the comment gives the reason as avoiding rounding away finer values.

We ran both versions over the same inputs. They differ only where padding
applies:

| exact | before | after |
| --- | --- | --- |
| `-0.100000000` | `-0.10` | `-0.1` |
| `-12.000000000` | `-12.00` | `-12` |
| `-0.000100000` | `-0.0001` | `-0.0001` |
| `-0.000000001` | `-0.000000001` | `-0.000000001` |

So the two-decimal floor and fine precision may not be in tension, and the
hazard the comment describes may not have been present. `r27_…` asserts the
two-decimal form and is byte-identical to its prior state, so the delivery
contradicts its own assertion. The gate on the tree R29C was built from was
7412 / 1, freshness only.

**What we want:** a selected ceiling shown with at least two decimals, `-0.10`
by default, with anything finer shown in full and never rounded away.

Also in this build: 7 new `missing_docs` warnings in `tonepoet-pipeline` for
`TruePeakTargetOrigin` and its members.

## 2. `custom` is hint text where it should be a control

The row renders as:

```
TP target  < -1 dBTP >  0.25 dB steps; custom: :set dsd-true-peak-target <dBTP>
```

`custom` reads as an option but cannot be clicked; the only route to an
arbitrary value is the command.

**What we want:** `custom` is a pill. Activating it opens an editable entry in
place, and input is validated so an invalid value cannot be committed. The
`:set` keys stay as an alternative rather than the only route.

Open, as design rather than defect: whether validation refuses the keystroke as
typed, so the field never holds an invalid value, or accepts the text and
reports why on commit.

## 3. The sign surprised a user

`:set dsd-true-peak-target 0.0001` is refused as out of range. Prepending a
minus sign works.

If a target above 0 dBTP is never meaningful, then every valid value in that
field is zero or negative, and a bare `0.0001` can only mean headroom below
full scale — so refusing it on range reads as a trap rather than a correction.
We may have the semantics wrong, and would like that confirmed.

**What we want:** entering a magnitude is not a trap. How is open — reading
unsigned input as headroom below 0 is closest to how the ask was phrased,
*"right up to 0.0001 of 0 dBFS"*; the sign could instead stay mandatory with a
refusal that says the value was positive.

Also open: `0` is currently accepted, which is a ceiling with no headroom at
all. Whether it should be refused is undecided.

## Requalification

`stages.rs` and `tonepoet-pipeline/src/settings.rs` are both source-locked and
both moved, so the freshness gate is failing as designed. Any fix to item 1
touches `stages.rs` again.

`scripts/requalify_reference_r20.sh` is reusable as-is. Its last full green pass
was `24ce2cd`; the most recent attempt stopped on the staging lease-ownership
flake, so on this tree the two black-box smokes and the real-SACD positive
conversion are unverified.
