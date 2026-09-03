# BRIEF — Tonepoet quietly delivers something other than what was asked for

**Date:** 2026-09-03
**Base:** `main` @ `9562acd`
**Related:** `OUTSTANDING_ISSUES.md` #30 (the sample-rate defect), #29 (the headroom reserve),
#28 (mode selection belongs at the call site, not in a user toggle)

## What the user wants

**When Tonepoet cannot deliver exactly what was asked for, it should say so, rather than
quietly delivering something else.**

Two independent defects share that shape, and both are in scope:

1. **Sample rate.** A rate the target format cannot encode is offered anyway, then silently
   changed at encode time.
2. **Headroom.** A hard-ceiling album gain silently lands below the requested ceiling when
   aggressive noise shaping is in play.

They are unrelated in mechanism but identical in character: in each case the shortfall is
**knowable before the conversion starts**, from the chosen format and settings alone, and in
each case the user only finds out afterwards, if at all.

## The first defect: a rate the format cannot encode

Selecting AAC in the TUI today leaves 176.4 kHz and 192 kHz selectable. Neither can exist: the
MPEG-4 sampling-frequency index stops at 96 kHz, both encoders in Tonepoet's own flake
advertise a maximum of 96 kHz, and `ffmpeg -c:a aac -ar 192000` fails outright with
`Specified sample rate 192000 is not supported by the aac encoder`.

What happens instead is worse than a clean failure. The final encode step emits `-ar` only when
it is handed a concrete rate; when it is handed `None`, FFmpeg picks a supported rate itself and
downsamples without comment. Several routings pass `None` deliberately, because an earlier step
has already applied the rate.

One exception first, so the rest is not misread: for lossy targets `push_encode_final`
(`tonepoet-pipeline/src/plan.rs:1571`) **overrides** whatever rate it was handed with the
measured carrier rate whenever runtime DSD album gain is active, and refuses outright if the
encoder cannot accept it. That is the hard-ceiling guarantee added in `9562acd` and it is
working. Everything below describes the ordinary conversion path, where no such override
applies:

- **A DSD source with a lossy target passes `None`** (`plan_from_dsd`,
  `tonepoet-pipeline/src/plan.rs:1391`). The requested rate is consumed by the DSD-to-PCM step,
  which writes a WAV intermediate at that rate; the lossy encode then re-reads it with no rate
  pinned. A DSD source is the case this project cares most about, and it is silent whether the
  user chose "same as source" or explicitly picked 192 kHz.
- **A PCM source is silent whenever no resample is required.** `rate_change_for_pcm` returns
  `None` for `RateTarget::Source`, and also for `RateTarget::PcmHz(hz)` when the source is
  already at `hz` — which is exactly what happens when someone selects the rate their source
  already has.
- **A PCM source needing a resample can go either way.** If SSRC brick-wall resampling or SoX
  preprocessing performs it, that step carries the rate and the encode again receives `None`
  (call sites at `plan.rs:1495` and `plan.rs:1539`). Only the default path passes the rate
  through to the encoder (`plan.rs:1551`), and only there does FFmpeg reject the request — as a
  raw encoder error rather than as anything Tonepoet said in its own words.

So silence is the ordinary outcome and the loud failure is the narrow one. This list is what
was verified by reading the planner; it is not offered as an exhaustive routing analysis, and
confirming the full set is part of the work.

### What exists today

#### The wrong constant

`FormatState::apply_format_constraints` (`src/tui/app.rs:5247`) disables AAC rates above
`192_000`. The correct ceiling is `96_000`. Neighbouring arms are right — Opus is pinned to
48 kHz, MP3 capped at 48 kHz — so this is an isolated wrong number. No test asserts it.

#### The larger hole: "same as source" escapes the cap-style arms

The cascade has two shapes of arm. The **cap** arms exempt the sentinel:

```rust
if opt.value != SOURCE_SAMPLE_RATE_SENTINEL && opt.value > 192_000 { opt.enabled = false; }
```

while the **pin** arms disable it, because they compare against one exact rate — Opus uses
`opt.enabled = opt.value == 48_000`, and DTS/AC3 use `if opt.value != 48_000`.

So the sentinel escape applies to the cap arms only: among lossy targets that is **AAC and
MP3**. Opus, DTS and AC3 pin to 48 kHz and disable "same as source" outright, so they are not
exposed. Fixing the AAC constant does nothing for the AAC sentinel path, which is the likelier
one in practice because "same as source" is the natural choice for a high-rate source.

#### There is already a settled pattern for this

`validate_ssrc_dither_id_for_target_rate` (`src/tui/app.rs:16646`) hits the same problem and
resolves it deliberately, in a comment worth reading before designing anything:

> The concrete rate is unavailable at the TUI boundary. The pipeline performs the same
> validation after resolving `RateTarget::Source`, so rejecting here would make every shaped ID
> unusable in a source-coupled preset without adding safety.

That is, concrete rates are validated in the TUI; sentinel-resolved rates are validated in the
pipeline after `RateTarget::Source` resolves. A solution that follows this split will sit
naturally in the codebase; one that tries to resolve the sentinel early will fight it.

#### The constraint data already has a single source of truth

`mapping::ffmpeg_lossy_encoder_accepts_rate_directly` (`tonepoet-pipeline/src/mapping.rs:546`)
encodes per-encoder rate tables for `Mp3`, `Aac`, `Opus`, `Dts` and `Ac3`, returning `None` for
anything else. It is `pub(crate)`, but `mapping` is already a `pub mod`. It was added for the
hard-ceiling album-gain path and is correct; it is simply not consulted by the UI.

#### Why the duplication exists

There are two distinct `AudioFormat` enums:

- `crate::convert::formats::AudioFormat` — the TUI's, which includes `Ogg`;
- `tonepoet_pipeline::enums::AudioFormat` — the planner's, which does not.

They are bridged by `map_audio_format` (`src/tui/convert_actions.rs:562`). Any UI-side use of
the planner's tables has to cross that boundary. This is presumably why the caps were
hardcoded, and it is the thing to solve rather than route around.

#### Measured blast radius of deriving the caps from the planner

The rate picker offers: source, 44.1, 48, 88.2, 96, 176.4, 192, 352.8, 384, 705.6, 768, plus
DSD rates. Comparing today's hardcoded caps against the planner tables **over the rates the
picker actually offers**:

| format | enabled today | enabled if derived | change |
|---|---|---|---|
| AAC | 44.1, 48, 88.2, 96, 176.4, 192 | 44.1, 48, 88.2, 96 | **the fix** |
| MP3 | 44.1, 48 | 44.1, 48 | none |
| Opus | 48 | 48 | none |
| DTS | 48 | 44.1, 48 | 44.1 becomes selectable |
| AC3 | 48 | 44.1, 48 | 44.1 becomes selectable |

So deriving is far less disruptive than it first appears: MP3 and Opus are unaffected, and the
only side effects are DTS and AC3 gaining 44.1 kHz — which both encoders genuinely accept, so
arguably a second latent defect rather than a regression. This is a correction to an earlier
estimate that claimed wider fallout.

`Ogg` appears in the cascade's `AudioFormat::Mp3 | AudioFormat::Ogg` arm but is **not** an
argument against deriving, because that arm is unreachable for `Ogg`: it is decode-only and
appears in `input_decodable()` alone, not in `all()`, `output_encodable()`, `advanced_output()`
or `common_output()`, so it can never be selected as an output format. (`map_audio_format` would
send it to `pipeline_enums::AudioFormat::Flac` if it ever arrived.) Treat the `Ogg` half of that
arm as dead with respect to output selection rather than as behaviour to preserve or to fix.

## The second defect: a hard ceiling that quietly lands low

Album auto-gain with a hard ceiling holds back headroom so the ceiling provably cannot be
exceeded. Noise shaping injects a deliberate shaped error sequence, so the terminal signal can
peak above the audio alone, and proving the ceiling means budgeting that sequence's worst case.
For most settings the cost is invisible; for 16-bit with aggressive shaping it is not:

| terminal case | reserve |
|---|---:|
| Int16, no dither | 0.000542 dB |
| Int24, no dither | 0.000002 dB |
| Int16, TPDF | 0.001626 dB |
| Int16, Gesemann @ 44.1 kHz | 0.023884 dB |
| Int16, Shibata @ 44.1 kHz | 0.091549 dB |
| Int16, High-Shibata @ 44.1 kHz | 0.169689 dB |
| Int24, High-Shibata @ 44.1 kHz | 0.000656 dB |

Ask for a 0.0 dBTP ceiling on a CD-format file with High-Shibata and the result lands near
-0.17 dB. That is about 2% in amplitude and is not audible, but it is not what was asked for,
and the user currently has no way to know it will happen.

The reserve itself is correct and is **not** being changed here. It is the deterministic support
needed to prove the ceiling, and shrinking it requires a finite-stream endpoint proof recorded
as #29. This brief is about disclosure only.

### What is already disclosed, and what is missing

Do not rebuild what exists. Two things already report:

- `processor.rs` logs the full picture at album-gain resolution — signal ceiling upper bound,
  pre- and post-gain terminal reconstruction errors, target and resolved fixed gain. It is
  accurate but engineer-facing, in scientific notation.
- The conversion log's transform list records `submitted-batch DSD album gain X dB (... loudest
  true peak Y dBTP; true-peak target Z dBTP)`.

So the numbers exist. What is missing is the plain statement that the delivered peak will sit
below the requested ceiling, by how much, and why — and, more useful still, saying it **before**
the conversion runs. The reserve is a pure function of target format, bit depth and dither
selection, so it is fully determined the moment those pills are set, exactly like the
sample-rate ceiling. That is the shared opportunity between the two defects in this brief.

## Decisions this work has to make

**What should happen when a resolved rate is impossible.** Two defensible answers, and the
choice is a product decision rather than a mechanical one:

- **Refuse**, consistent with what the hard-ceiling album-gain path already does. Safe, but
  turns a conversion that currently succeeds (downsampled) into a hard failure.
- **Select the highest supported rate at or below the request, and say so** in the log and in
  the UI. Keeps conversions working while ending the silence.

The user's stated preference is to be told and offered the choice, which points at the second
for the ordinary path, possibly with a prompt. Note the asymmetry: **the hard-ceiling refusal
must stay a refusal.** Under album `NormalizePeak` an unannounced post-gain resample voids the
proved ceiling, so silently picking another rate there would reintroduce exactly the defect
`9562acd` closed.

**Where the ceiling is enforced.** The TUI can only see concrete selections. The pipeline is
the only place that sees a resolved `RateTarget::Source`. Both need to hold, and the existing
SSRC precedent suggests how they divide.

**Whether to keep hardcoded caps at all.** The `192_000` error exists because the UI duplicates
knowledge the planner already owns. Fixing the number without addressing the duplication invites
the next drift, but crossing the two-enum boundary is real work; that trade is the
implementer's to weigh.

## Outcomes wanted

- No sample rate is selectable for a target format that cannot encode it.
- "Same as source" cannot deliver an impossible rate; it is caught once the concrete rate is
  known.
- A conversion never silently produces a different sample rate than the one in effect. If the
  rate changes, the user learns it — before the conversion where practical, in the log always.
- An impossible request is not surfaced as a raw FFmpeg encoder error. Whatever the chosen
  behaviour, Tonepoet explains the problem in its own words, because the third row of the table
  above already fails today and fails badly.
- The hard-ceiling album-gain path keeps refusing rather than adapting.
- A user choosing a bit depth and dither that will cost measurable headroom learns the size of
  the shortfall from that choice, ideally before converting, without needing to read a debug log
  or know what a noise shaper is. The reserve is not reduced to achieve this.
- The constraint has one source of truth. A future encoder or rate change should not require
  editing two places, and should not be able to drift.
- Presets that carry a now-impossible rate degrade sensibly rather than failing obscurely or
  silently changing meaning.

## Scope

**In scope:** sample-rate admission for lossy targets across the TUI, presets, and the planner;
disclosure of the hard-ceiling headroom shortfall; the reporting of any divergence between what
was requested and what will be delivered; and whatever is needed to give each constraint a
single home.

**Out of scope:** **tightening** the headroom reserve,
which is a separate proof obligation recorded in #29 and belongs with the pipeline redesign;
the interactive prompt design if it turns out to need new UI infrastructure — a clear
non-silent fallback is enough for this round; and the pipeline redesign itself.

## Working constraints

- The implementation container has no Rust toolchain, no Nix, and none of the audio tools.
  Running the gate is the operator's job; no delivery should assume it has been run.
- `tonepoet-pipeline` is a pure planner: no `std::process`, no `tokio::process`, no I/O, and no
  interactive behaviour. It may report a constraint; it must not prompt.
- `crates/tonepoet-true-peak` must not be touched by this work and must keep its empty
  `[dependencies]` and zero Tonepoet references.
- Plain letters in Browse remain reserved for type-ahead. No F-keys. No emoji or decorative
  unicode in UI text.
- Tests that mutate process-global state have caused repeated flakes in this project.
- Two tests are known low-rate flakes and are not this work's responsibility:
  `cancel_abandons_a_wedged_helper_without_waiting_for_it` (#20) and
  `empty_dead_queue_scope_is_reclaimed_but_live_empty_scope_is_preserved` (#31).
