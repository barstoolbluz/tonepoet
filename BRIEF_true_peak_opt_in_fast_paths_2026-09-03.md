# BRIEF — Opt-in faster true-peak scanning paths

**Date:** 2026-09-03
**Base:** `main` @ `143e672`
**Crate:** `crates/tonepoet-true-peak`
**Related:** `OUTSTANDING_ISSUES.md` #28 (mode selection), and the R10/R11 true-peak work

## What the user wants

**Two additional true-peak scanning paths, with `Headroom64x` remaining the gold standard, that
a user can deliberately opt into for speed.**

`Headroom64x` stays the default and stays the reference against which everything else is
judged. Nothing about its accuracy, its contract, or its role in the album hard-ceiling proof is
being reconsidered. What is missing is a choice: today a user who wants a faster scan has no
option at all.

## Why

From the user's chair, the current scan is slow, and the comparison that makes it feel slow is
concrete. Measured on this machine, scanning ten minutes of stereo audio:

| | 44.1 kHz | 176.4 kHz |
|---|---|---|
| `loudgain` (EBU R128 scan) | 121x realtime | 40x realtime |
| `Headroom64x` | 12.7x realtime | 3.1x realtime |

So a reference tool most users already know is roughly ten times faster. For a 40-minute album
at 176.4 kHz that is about a minute versus roughly **12.7 CPU-minutes** for `Headroom64x`
(measured single-thread throughput ~1,111k interleaved samples/second at that rate, essentially
constant across sample rates; scanning is per track and dispatched concurrently, so wall-clock
is a fraction of the CPU total on a many-core machine).

**This is explicitly not a request to adopt loudgain's accuracy.** Its 4x profile is unfit for
headroom decisions and that judgement stands. It is a statement that the gap is large enough
that many users will simply not opt into a scan that costs twelve minutes an album, and will
therefore get no true-peak protection at all. A path they *will* opt into is worth more than a
perfect path they decline.

## What is wanted

**Two additional paths**, exposed as a deliberate user choice, forming a three-rung ladder with
the existing mode. The operator has set the accuracy envelope for each rung explicitly:

| rung | declared one-sided bound | role |
|---|---|---|
| existing `Headroom64x` | 0.030 dB (`HEADROOM64X_MAX_UNDERREAD_DB`) | gold standard, default, unchanged |
| middle | **0.042 - 0.044 dB** | modest accuracy cost for a real speed gain |
| fastest | **0.082 - 0.084 dB** | largest speed gain the operator will accept |

These envelopes are deliberate, informed decisions and are not up for renegotiation on accuracy
grounds. They are ceilings, not targets: a path that lands well inside its envelope is better,
not worse.

Speed requirements:

- **Both new paths must be materially faster than `Headroom64x`.** As a floor, the *slower* of
  the two must scan a 40-minute 176.4 kHz stereo album in **well under 7.7 CPU-minutes**.
- **The two rungs must be meaningfully separated in speed.** A fastest path that is only
  marginally quicker than the middle one has not earned its place, and shipping two
  near-identical options is worse than shipping one. If the second rung cannot be made
  distinctly faster, say so and ship one path rather than padding the ladder.

- **Each exposed path carries its own declared, qualified bound**, held to the same standard as
  the existing one: a stated number, derived and defended, with tests that fail if the
  implementation stops meeting it. A fast path whose error is unknown is not acceptable at any
  speed.
- The speed need not come from accuracy. **If a design can deliver the required speed without
  giving up accuracy, that is strictly preferred** to spending the accuracy budget. The
  envelopes above are permission, not an instruction to use them.

## How this interacts with the hard ceiling

Album-scoped DSD hard-ceiling gain proves that a requested ceiling cannot be exceeded, and that
proof depends on a bound the meter can stand behind.

That guarantee must not weaken. Either the faster paths are excluded from the hard-ceiling
authority, or their bound is proved to the same standard and the reserve widens to match. Which
of those is right is the implementer's call, but a fast path must never silently reduce the
strength of the ceiling guarantee.

`OUTSTANDING_ISSUES.md` #28 concluded that mode selection is "a static property of each call
site, not of the material and not a user setting". That is worth reading precisely rather than
as a blanket prohibition. Its argument was that `Reporting4x` and `Headroom64x` "are not
fast/slow tiers of one measurement -- they answer different questions", so exposing the
reporting mode as a fast option would let a user answer the wrong question and call it speed.

The rungs asked for here are a different proposition: they answer the **same** question --
what is the true peak, for a headroom decision -- at different declared accuracies. That is a
genuine speed/accuracy trade a user can reason about, which is exactly what #28 said the
existing pair was not. #28's reasoning is therefore preserved, not overturned: `Reporting4x`
stays out of this ladder, and mode-versus-question remains a call-site decision even as
accuracy-within-a-question becomes a user choice.

## Choosing well

Deliberately not prescribed: how many paths, what they are called, what technique produces the
speed, and where the control lives. Those are design decisions.

What does matter:

- A user choosing a faster path should be able to tell what they are giving up, in a unit that
  means something — the declared bound, not a factor name.
- The default must remain the accurate path. Speed is opt-in, never inherited silently.
- The number of choices should stay small. Three rungs total, not a menu.

## Scope

**In scope:** the crate's public surface and internals, its qualification and tests, the
production caller's selection of a path, and however the choice reaches the user.

**Out of scope:** `Reporting4x`, whose libebur128-compatible profile is a separate contract;
the reserve and finite-stream proof recorded in #29; and the pipeline redesign.

## Working constraints

- `crates/tonepoet-true-peak` must keep an **empty `[dependencies]`**, import no Tonepoet type,
  and stay free of DSD, album-gain and pipeline concepts. It must survive the planned pipeline
  redesign unchanged. This is the crate's central constraint and is not negotiable.
- The streaming shape (`new` / `push_interleaved` / `finalize`) must hold; the meter must not
  buffer whole albums.
- The frozen first-stage coefficients and the checked-in real-material fixture are qualified
  artefacts. Changing either requires saying so explicitly and re-qualifying.
- The implementation container has no Rust toolchain, no Nix, and none of the audio tools.
  Running the gate is the operator's job; no delivery should assume it has been run. Report
  performance claims as designed-for, not measured, unless they were actually measured.
- No emoji or decorative unicode in user-visible text. No F-keys. Plain letters in Browse remain
  reserved for type-ahead.
- Tests that mutate process-global state have caused repeated flakes in this project.
