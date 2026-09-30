# Brief R10 — SSRC should stay available when it cannot own the dither

Date: 2026-09-30
Base: the supplied `tonepoet-src.tar.gz` is authoritative. It is our local
`main` at gate 7210/0, with the SSRC true-peak registry commissioned.

## What the user hits

Choose a destination rate such as 176.4 kHz or 352.8 kHz, choose SSRC, and
choose a dither type. The conversion refuses:

```
ssrc_terminal_dither_unavailable
```

SSRC has no dither table at those destination rates. We confirmed that
directly against the pinned build — dither id 99, 16-bit, profile high:

| destination | result |
|---|---|
| 44.1 kHz | accepted |
| 96 kHz | accepted |
| 176.4 kHz | refused, exit 255 |
| 192 kHz | accepted |
| 352.8 kHz | refused, exit 255 |

So the gap is specific: the 44.1-family multiples above 96 kHz, not high rates
in general. That is true of SSRC and does not follow that SSRC is unavailable.

## The outcome we want

A user-selectable combination of resampler, destination rate, bit depth and
dither converts. SSRC does the rate conversion; whatever is qualified to apply
the selected dither at the destination applies it.

If a competent user could build that pipeline by hand — decode, SSRC to a
float intermediate, then quantize and dither with ffmpeg or sox — tonepoet
should build it too, rather than refusing the request.

The refusal should survive only where it states something true about the
request as a whole, not where it reports that one tool in a multi-tool
pipeline cannot perform one stage by itself.

## What must not be lost

The integrity property R6 was protecting is real and stays: when a user
explicitly demands SSRC-native dither, that ownership must not migrate to
another tool silently. Getting soxr's dither while believing you got SSRC's
noise shaping is worse than a refusal.

Whatever distinguishes those two situations should be explicit, and the user
should be able to tell from the output which tool owned the dither.

## History worth knowing

This behavior previously existed. Before the R6 WavPack Int24 corrective,
`resolve_ssrc_immediate_output` returned a Float64 nonterminal in exactly this
case, with the comment:

> This is a derived global-family fusion miss, not a whole-request settings
> error. A later admitted terminal may own the dither.

Commit `142192e` replaced that `Ok` with the present `Err`. Note that
`ssrc_terminal_dither_unavailable` is now raised from two places, in
`resolve_ssrc_immediate_output` and in `resolve_ssrc_terminal_realization`. R6's goal —
keeping ordinary SSRC dither ownership on SSRC — was sound; the refusal
appears to have been applied more broadly than that goal required. The code
already separates an explicit SSRC-native override from an ordinary dither
selection.

We are not asking for that specific code to be restored. It is offered as
evidence that the split terminal is a shape this pipeline already understood.

## Questions we could not answer from here

Which terminals are qualified to own the dither after an SSRC float
intermediate, across the rate, depth and dither combinations a user can
select, and whether any of those combinations should still refuse for a
genuine reason rather than this one.

## State

Gate 7210 passed / 0 failed, zero warnings. `tonepoet-true-peak` 160/0.
Reference requalified on the Nix rooting. SSRC true-peak registry
commissioned, 4,368 records. Issues #55, #57 and #58 closed and
field-verified.

## Build capability

`cargo test -p tonepoet-pipeline` builds in seconds and covers the planner.
The root `tonepoet` crate peaks at 6.24 GB in one `rustc` and will OOM under a
4 GB ceiling; write anything there uncompiled and say so.
