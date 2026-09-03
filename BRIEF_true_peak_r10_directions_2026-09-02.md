# DIRECTIONS — R10: keep the meter, remove the commissioning apparatus

**Date:** 2026-09-02
**Base:** `main` @ `9254daa`
**Prior:** `BRIEF_true_peak_crate_2026-09-01.md`, and the R9 handoff
`tonepoet_true_peak_CHAIN_AUTHORITY_R9`

## Precedence

`BRIEF_true_peak_crate_2026-09-01.md` still describes what this crate is for and what it must
be. It is included for that context and its constraints still hold — particularly that the
crate must import no Tonepoet types and must survive the coming pipeline redesign unchanged.

Where this document differs from it, **this document governs**.

To be clear about how the commissioning apparatus came to exist: it was **asked for**. You
identified those pieces as missing and they were added at the operator's direction. This is
not a case of scope invented without a request, and the reversal is not a criticism of the
work — the pieces do what they were asked to do.

What changed is that their practical cost is now understood: a fresh build cannot perform
album DSD auto-gain at all until an operator completes a pinned qualification run against a
real-DSD corpus, and the fingerprint that keeps the stamp honest is bound to pipeline source
that a planned redesign will churn. With that visible, the operator no longer wants them.

The prior brief's requirement that correctness be *demonstrated* — that a meter unchecked
against independent references "is a guess with a confident interface" — still stands
unchanged. What changes is only where that demonstration lives: in tests a compiler and a test
runner execute, rather than in runtime gating and build-time stamps.

## The short version

The crate is good and is being kept. The verification apparatus built around it is being
removed. Nothing about the DSP, the coefficients, the API, or the accuracy work is in
question.

## What was assessed and is staying

R9 was applied to a clean tree, built, and inspected. The crate met the brief's hard
constraints exactly:

- `crates/tonepoet-true-peak/Cargo.toml` has an **empty** `[dependencies]` — no ffmpeg, no
  soxr, no external crate.
- **Zero** references to any Tonepoet crate or type anywhere in the crate, including its tests
  and examples.
- **Zero** DSD or album-gain concepts in its API. Its vocabulary is sample rate, channel count,
  interleaved `f64` frames, interpolation mode, edge policy, level.
- Streaming shape — `TruePeakMeter::new` / `push_interleaved` / `finalize` — so it does not
  buffer whole albums.
- Both modes, with `Reporting4x` reproducing libebur128's rate-dependent profile (4x, 2x at
  96–192 kHz, sample peak at ≥192 kHz) rather than only its nominal factor.
- `PeakLevel::Finite { linear, dbtp }` documented as unclamped, so values above full scale
  produce positive dBTP, with `Silence` a distinct variant.
- `HEADROOM64X_GRID_MAX_UNDERREAD_DB = 0.002616421594233`, which was independently derived as
  `20·log10(cos(π/128))` and matches to fifteen decimals. Total bound 0.030 dB against the
  existing chain's 0.100 dB residual.

All of that is kept. This round should not revisit the filter, the coefficients, the modes,
the API shape, or the accuracy targets.

## What is being removed, and why

The runtime commissioning gate and the chain-contract fingerprint are to be removed.

`validate_dsd_album_gain_authority_sox(...)?` currently propagates a failure whenever a build
carries no valid commissioning stamp, and no stamp ships. The practical effect is that album
DSD auto-gain, which works today, hard-fails on a fresh build until an operator runs
`scripts/qualify_dsd_album_true_peak_authority.py` with a pinned SoX executable hash, a
qualified ffmpeg, and an operator-authored real-DSD corpus covering six rate configurations
(B1 44.1, B2 48, B3 88.2, B4/B4W/B5 176.4), then rebuilds.

Two reasons this shape does not fit what Tonepoet is, both of which only became
visible once the machinery existed and could be evaluated against real use.

**It protects against a risk this meter does not have.** That kind of gate earns its cost when
accuracy depends on an external binary whose build can vary — which is why the DSD reference
path pins SoX-ng 14.8.0.1 by hash. This meter has no dependencies at all. Same source, same
input, same result, on every machine. There are no runtime degrees of freedom for a runtime
check to catch, so it can only ever fail for reasons unrelated to the measurement.

**Its fingerprint is coupled to the code we are about to rewrite.**
`assets/true_peak/dsd_album_gain_chain_contract_v1.txt` hashes source ranges inside
`tonepoet-pipeline`, including `DsdReconstructionSelection`, `ResolvedDsdProfile`,
`resolve_reference_target_rate`, `default_pcm_target_hz`, `DsdRate::from_hz`,
`source.dsd_rate()`, `is_native_v2()` and `album_auto_gain_selected()`. A pipeline redesign is
planned — user-composed conversion steps, PCM to DSD, DSD to and from lossy PCM — and it will
touch that code. Every such change would stale the stamp and require another pinned
commissioning run with real DSD material, or leave album auto-gain failing.

The crate itself is not the problem. The crate knows nothing about the pipeline; the contract
knows about both, and it is the contract that breaks.

## What should replace it

**Test-time verification only. No runtime gate, and no soft runtime warning either.**

The accuracy claims should be enforced as assertions that run in the ordinary workspace test
suite, so that a change degrading the meter fails `cargo test --workspace` the way any other
regression does. The qualification work already done — adversarial search, under-read bounds,
edge and silence and above-full-scale and short-input behaviour, streaming-versus-single-shot
equivalence — is the right content; it belongs where the existing gate already looks.

A soft runtime warning is explicitly not wanted. It would add runtime code and noise while
preventing nothing.

One narrow piece of the fingerprint idea is worth keeping in cheap form: a test asserting the
coefficient table's integrity, so an accidental edit or corruption fails loudly. A checksum
assertion in a `#[test]` is the scale intended — not a build-time contract spanning twenty
source ranges across two crates.

## Python

Python must not become a build or runtime dependency, and today it is not one: `build.rs`
invokes it zero times and `flake.nix` was unchanged by R9. That property must hold.

Beyond that, the operator-facing Python should go as far as is reasonable:

- The commissioning script goes with the gate it serves.
- Qualification logic that proves the meter's accuracy should be Rust tests in the crate,
  runnable through the normal gate, rather than a separate manual invocation.
- Offline filter *design* is a different case. Its output — the coefficient table — is already
  checked in as generated Rust. If keeping a design script is genuinely useful for
  regenerating coefficients later, say so and keep it clearly marked as offline tooling that
  nothing builds or tests against. Do not keep scripts merely because they exist.
- Do not ship `__pycache__` or other build artefacts in a delivery.

## One defect found while assessing R9

`crates/tonepoet-true-peak/tests/meter.rs` did not compile against its own library: two
references to `result.per_channel[...]` survived a rename to `channel_linear_peaks`, which the
same file uses correctly in six other places. The two stale uses were translated to the
established idiom to get a build.

Noted not as a rebuke but because it points at the same conclusion the rest of this document
reaches: the implementation environment has no Rust toolchain, so static checks, manifest
hashing and Python self-tests were the only verification available — and none of them can
catch a field rename. That is an argument for verification a compiler and test runner perform,
which the operator can actually run.

## Scope

**In scope:** removing the runtime commissioning gate and the chain-contract fingerprint;
moving accuracy verification into the workspace test suite; reducing the Python surface; and
keeping album DSD auto-gain working on a plain `cargo build` with no commissioning step.

**Out of scope:** the meter's DSP, its coefficients, its API, its accuracy targets, and its
mode set. Those were assessed and are being kept.

Also out of scope: the "preserve original peaks" gain policy from `OUTSTANDING_ISSUES.md` #19,
which is a product decision that follows once the measurement is in place.

## Working constraints

- The implementation container has no Rust toolchain, no Nix, and none of the audio tools.
  Running the gate is the operator's job; no delivery should assume it has been run.
- The crate must remain free of Tonepoet types and of any dependency on the current pipeline,
  so it survives the coming redesign unchanged. That constraint is unchanged from the previous
  brief and is not negotiable.
- Tests that mutate process-global state have caused repeated flakes in this project.
- No emoji or decorative unicode in user-visible text.
