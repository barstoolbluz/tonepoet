# BRIEF — R2 corrective: the lossy rate fallback discards bandwidth it did not need to

**Date:** 2026-09-03
**Base:** `main` @ `9562acd`, with the silent-request-divergences delivery applied
**Prior:** `BRIEF_silent_request_divergences_2026-09-03.md` and its implementation report

## Gate result

`cargo test --workspace --no-fail-fast`: **6700 passed, 1 failed**, 60 result lines.

The single failure is
`convert::pipeline::stages::chunk_2_1_3_postprocessing_gate_and_phase_tests::cue_stream_auto_alac_and_lossy_targets_remain_direct_ffmpeg`,
asserting `Auto Opus must remain directly streamable`. **It is not a test defect.** The test is
present unchanged at `9562acd` — this delivery neither added nor modified it — and it is
correctly detecting that Opus conversions now require a rate change that breaks the direct
FFmpeg streaming route. Do not adjust that test to pass.

Two integration fixes were applied to the delivery before gating, neither related to this
defect: `src/tui/app.rs` gained a function-local `use super::SOURCE_SAMPLE_RATE_SENTINEL;` in
`lossy_picker_concrete_rates_match_the_pipeline_encoder_authority` and in
`aac_192k_clamps_to_96k_and_dts_ac3_expose_their_valid_44k1_cell`, matching the idiom already
used by four sibling test functions in that file. Without them the lib test target did not
compile.

## The defect

`ffmpeg_lossy_encoder_rate_at_or_below` resolves a rate the encoder cannot take directly to the
**highest supported rate at or below** the request. For Opus, whose direct-input set is
`[8k, 12k, 16k, 24k, 48k]`, a 44.1 kHz source resolves to **24 kHz**.

44.1 kHz is the most common source rate there is.

The exposure is every path that resolves `RateTarget::Source`, which notably includes **the
CLI**: `convert` has no sample-rate flag, so `sample_rate_target_for_format` maps its absent
value to `RateTarget::Source`. `tonepoet convert <cd-rate-file> --format opus` therefore
band-limits to 12 kHz. Presets and the CUE-stream route reach it the same way; the failing test
above is the CUE-stream case.

The TUI pill path is **not** exposed: this delivery's own admission logic disables Opus's Source
sentinel and pins the selection to 48 kHz, so a TUI user still gets 48 kHz. That is worth
stating precisely so the fix is not mistaken for a UI problem — it is a planner-policy problem
that the UI happens to sidestep.

### Measured

A 15 kHz tone in a 44.1 kHz source, encoded with `libopus`, energy retained above 12 kHz, by
two independent measurement paths that agree to three decimals:

| encode | `volumedetect` mean | `astats` RMS |
|---|---:|---:|
| no `-ar` (behaviour before this delivery) | -21.9 dB | -21.864 dB |
| `-ar 24000` (this delivery) | **-78.0 dB** | **-78.123 dB** |
| `-ar 48000` | -21.9 dB | -21.864 dB |

The top octave is gone. `-ar 48000` reproduces the pre-delivery behaviour exactly.

### Confirmed end to end on the built binary

Not inferred from the planner. A 44.1 kHz FLAC converted with
`tonepoet convert cd.flac --format opus`:

```
WARN Opus cannot encode 44.1kHz directly with Tonepoet's configured encoder;
     this conversion will use 24kHz instead.
```

The resulting `01 - Track 01.opus` reports a container rate of **48000 Hz** and carries
**-79.9 dB** mean energy above 12 kHz, against -21.9 dB for the same source encoded without the
adjustment.

Note that the change **is** disclosed — the warning above is this delivery's own disclosure
working as designed. This is therefore not a silent-divergence defect. It is a correctly
announced wrong choice, which is why the fix belongs in the policy and not in the reporting.

### The loss buys nothing

Opus's bitstream is always 48 kHz. Every one of these produces a container reporting 48000 Hz:

```
-ar 8000  -> 48000 Hz      -ar 24000 -> 48000 Hz
-ar 12000 -> 48000 Hz      -ar 48000 -> 48000 Hz
-ar 16000 -> 48000 Hz
```

So resolving 44.1 kHz to 24 kHz does not produce a 24 kHz file, a smaller file, or a file that
matches the encoder's rate. It produces the same 48 kHz Opus stream with the top octave removed.
It is pure loss with no compensating benefit.

## Root cause: the table conflates two different meanings

The direct-rate tables treat every format's entries as the same kind of fact. They are not.

For MP3, AAC, DTS and AC-3, an entry is the **output sample rate** — the rate the finished file
will actually have. Verified:

```
AAC  -ar 96000 -> container 96000 Hz
MP3  -ar 48000 -> container 48000 Hz
AC3  -ar 44100 -> container 44100 Hz
```

For Opus, an entry is an **input band-limit**, and the output is always 48 kHz:

```
OPUS -ar 24000 -> container 48000 Hz
```

Choosing "the highest entry at or below the request" is a reasonable policy for an output rate
and a harmful one for an input band-limit. Applying one rule to both is what produced this.

## A second case in the same direction

`rate_at_or_below` returns `None` when the request is below a format's minimum, and the planner
turns that into a refusal:

> `{format} cannot encode the requested {rate} Hz directly, and no configured encoder rate
> exists at or below that request`

A 22.05 kHz source targeting AC-3 (minimum 32 kHz) therefore now fails. It **succeeded before
this delivery**, producing a 32 kHz file, because FFmpeg resamples upward. Opus at 22.05 kHz resolves downward to
16 kHz for the same reason. Both are the same error: the policy only ever looks down.

## What is wanted

**A lossy conversion should not discard bandwidth it did not have to discard, and should not
refuse work that is representable.**

Downward resolution is correct and unavoidable when the request genuinely exceeds what the
format can encode — 192 kHz to MP3 must become 48 kHz, and 192 kHz to AAC must become 96 kHz.
Those cases are correct in this delivery and must stay correct. The defect is confined to requests that fall
**below or between** supported rates, where the policy still looks down.

### Opus is not a rate-negotiation problem at all

`libopus` accepts **every** input rate and resamples internally; nothing fails. Verified:

| input | no `-ar` | with `-ar 48000` |
|---|---|---|
| 44.1 kHz | OK -> 48000 | - |
| 48 kHz | OK -> 48000 | - |
| 96 kHz | OK -> 48000 | OK -> 48000 |
| 176.4 kHz | OK -> 48000 | OK -> 48000 |
| 192 kHz | OK -> 48000 | OK -> 48000 |

So for Opus the correct resolved rate is **always 48 kHz**, for any source rate. There is no
case where a lower input rate serves full-bandwidth material better, because the output is
48 kHz regardless.

This is also the operator's original design, carried over from the converter Tonepoet is based
on: an Opus-specific resampling step that brings anything not already at 48 kHz up or down to
48 kHz using the user's chosen engine (SoX or SSRC), rather than letting the encoder's internal
resampler do it invisibly.

That design needs no new machinery, and in the Convert screen it is **already what happens**.

The Convert screen exposes a single **resampler** pill — `none` / `soxr` / `sox` / `ssrc`. There
is no brick-wall control anywhere in it: `nyquist_transition` is never set from `src/tui/command.rs`,
`src/config.rs` or the CLI, and its only other writer is `src/convert/wizard_integration.rs`, the
legacy wizard. **Do not carry the legacy wizard's model over to the Convert screen** — they
differ, and this brief describes the Convert screen. There, the transition is derived from that
one pill choice in `src/tui/convert_actions.rs`:

| resampler pill | `PreferredTool` | `NyquistTransition` |
|---|---|---|
| none | Auto | Gentle |
| sox | Sox | Gentle |
| ssrc | Ssrc | **BrickWall** |
| soxr | Ffmpeg | Gentle |

The default is derived from source facts, and differs by source kind. Both apply only while the
user has not overridden the pill (`resampler_overridden`):

- `FormatState::apply_auto_resampler` (`src/tui/app.rs:5018`) — the ordinary PCM path — selects
  `none` when the target already equals the source rate, and **`soxr`** when a rate change is
  required.
- `FormatState::cascade_dsd_source_to_pcm_defaults` (`src/tui/app.rs:5159`) — the DSD path —
  selects `none` when preserving the source rate and **`sox`** otherwise.

Since `needs_ssrc` keys off `NyquistTransition::BrickWall`, SSRC engages exactly when the user
picks `ssrc`; it is not a setting reachable any other way. Note also that selecting `soxr` does
not spawn a separate resampler process — it maps to `PreferredTool::Ffmpeg` and is realized as
`aresample=resampler=soxr` with the configured precision and cutoff, which is still the user's
chosen engine, explicitly parameterized rather than left to an encoder default.

For Opus the sample-rate pill is pinned to 48 kHz and cannot be changed. So a 44.1 kHz source
targeting Opus in the Convert screen already implies a rate change, already activates the
resampler pill, and already performs 44.1 -> 48 kHz with the user's chosen engine before the
encoder sees it. **That is the operator's original design, working today.**

The defect is therefore not that the design is missing. It is that the paths which resolve
`RateTarget::Source` — the CLI above all — bypass it and let the new fallback pick 24 kHz
instead. The fix is to make those paths reach the same resolved 48 kHz the Convert screen
already reaches, so the same resampler machinery runs. A policy change, not an architectural
one.

## Outcomes wanted

- A 44.1 kHz source converted to Opus retains its full bandwidth.
- Opus resolves to 48 kHz from any source rate, and the rate conversion is performed by
  Tonepoet's own resampler under the user's chosen quality settings rather than silently inside
  the encoder — matching what the Convert screen already does today.
- A source below a format's minimum rate converts successfully rather than being refused,
  matching the behaviour before this delivery.
- Requests genuinely above a format's maximum still resolve downward, unchanged.
- Whatever rate is chosen, the existing disclosure still reports it. Disclosure is not a
  substitute for choosing correctly, but it must not regress.
- `cue_stream_auto_alac_and_lossy_targets_remain_direct_ffmpeg` passes without being modified.

## What must not regress

The rest of the delivery is sound and should be preserved:

- one encoder-rate authority in `tonepoet-pipeline/src/mapping.rs`, consulted by the TUI,
  planner, hard-ceiling refusal, SSRC dither validation and the FFmpeg command builder;
- the FFmpeg lossy command always pinning `-ar` and failing closed, so FFmpeg is never left to
  negotiate the rate;
- **album-scoped DSD hard-ceiling gain still refusing rather than adapting** — that asymmetry is
  the `9562acd` guarantee and must not be softened into a fallback;
- pre-conversion and durable disclosure of any rate divergence;
- headroom-reserve disclosure, with the reserve and its proof untouched;
- AAC's picker ceiling at 96 kHz, and DTS/AC-3 exposing their valid 44.1 kHz cell.

`crates/tonepoet-true-peak` must remain byte-identical and must not be touched.

## Working constraints

- The implementation container has no Rust toolchain, no Nix, and none of the audio tools.
  Running the gate is the operator's job; no delivery should assume it has been run.
- `tonepoet-pipeline` is a pure planner: no `std::process`, no `tokio::process`, no I/O, no
  interactive behaviour.
- Plain letters in Browse remain reserved for type-ahead. No F-keys. No emoji or decorative
  unicode in UI text.
- Tests that mutate process-global state have caused repeated flakes in this project.
- Two tests are known low-rate flakes and are not this work's responsibility:
  `cancel_abandons_a_wedged_helper_without_waiting_for_it` (#20) and
  `empty_dead_queue_scope_is_reclaimed_but_live_empty_scope_is_preserved` (#31).
