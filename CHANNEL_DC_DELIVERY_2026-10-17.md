# TonePoet: Channel layout and DC-bias correction

Implementation delta for `BRIEF_channel_layout_and_dc_offset_2026-10-17.md`, against the supplied `TonePoet_LOGGING_channel_dc_2026-10-17.tar.gz` source tree.

## Added controls

In the maximized Convert > Format pane, two advanced PCM-output rows now appear below the ordinary controls:

* `channels: source | stereo`. `source` preserves the probed source channel count. `stereo` produces two output channels, independently of the source reader, including SACD, DVD-A, optical-disc and file-based sources. No arbitrary surround upmix or mono downmix is claimed. The pre-existing DVD-A reader auto-downmix is disabled on Convert admission so two hidden matrices cannot stack; the legacy DVD-A policy remains available in its original API.
* `DC removal: off | on (2 Hz HPF)`. This is an explicit, configurable 2 Hz high-pass. SoX `highpass 2` and FFmpeg `highpass=f=2:p=2:precision=f64` are backend candidates, selected through the existing typed physical planner. This is a **filter**, not FFmpeg's `dcshift` with an invented value: a `dcshift` must be supplied with a measured offset. A 2 Hz high-pass can affect content near/below 2 Hz and has a settling transient.

Both settings are persisted in v5 TUI presets and are backward-compatible with presets saved before this addition. They participate in the registered-effect typed graph, request identity, and completed-operation evidence. Effect construction replaces old matching intents, including stale dependency edges, on repeated admission.

## Stereo matrix and headroom

The production expression is the single authoritative constant `STEREO_FOLD_FFMPEG_FILTER` in `tonepoet-pipeline/src/semantic_plan.rs`, referenced by the fingerprint and conversion evidence. The physical backend is FFmpeg's role-aware `pan` rather than a positional SoX `remix` that could silently treat 5.1(side) as 5.1(back).

The unnormalized coefficients for each output are:

| Contribution | Left output | Right output |
|:--|--:|--:|
| Front left / right (respective) | 1.0 FL | 1.0 FR |
| Front center | 0.7071067811865476 FC | 0.7071067811865476 FC |
| Back left / right (respective) | 0.7071067811865476 BL | 0.7071067811865476 BR |
| Side left / right (respective) | 0.7071067811865476 SL | 0.7071067811865476 SR |
| Front-left/right-of-center (respective) | 0.7071067811865476 FLC | 0.7071067811865476 FRC |
| Back center | 0.5 BC | 0.5 BC |
| LFE | 0 | 0 |

FFmpeg's `<` pan operator normalizes each output by the sum of the **present** contribution coefficients. This makes the row an individually convex combination and prevents a sample-domain gain increase for bounded inputs, including coherent multichannel inputs, without an arbitrary scalar attenuation selected per title. This does not imply a strict intersample true-peak guarantee: select the existing certified true-peak guard when a measured output ceiling is required. The guard is applied after folding; its retained Float64 carrier and typed channel count match the stereo output. The downmix intentionally omits LFE.

The registered-effect realizer now carries authoritative **input**, **per-effect output**, and **final** channel widths. It does not parse an FFmpeg filter string to guess signal geometry. It validates raw Float64 frame widths and hands the correct input width to each successive subprocess. Channel count is changed once in the typed effect state and retained in the carrier source reference, certified scan, and final output context. Dither remains at the existing terminal precision-reduction boundary.

## Browse DC classification

The displayed DC bias measurement is unchanged. Classification is relative to AC RMS instead of an absolute `0.001` full-scale amplitude threshold. Since Browse RMS is AES-17-scaled, the classifier divides its linear value by sqrt(2), subtracts the DC energy to obtain AC RMS, and flags DC when `abs(DC) / AC_RMS > 0.01` (1%, about -40 dB). It treats invalid/near-silent measurements as non-alarming. Tests cover uniform gain changes, invalid measurements, silence, and significant bias.

## Boundaries and deliberately retained refusals

* Stereo folding supports **known input widths 1–8 channels**. Wider or unknown source geometry is refused instead of guessed.
* A certified PCM true-peak path that depends on the commissioned **protected SSRC replay** refuses an extra post-resample channel/DC effect: the existing proof/replay contract cannot truthfully bind that new stage. Select SoX/FFmpeg resampling or remove the effect for that combination. The ordinary non-SSRC true-peak path uses the complete recorded registered-effect spine.
* A strict true-peak ceiling cannot be claimed for decoded lossy outputs beyond TonePoet's existing certified encoder-input domain.
* The existing DSD Reference pathway stays restricted rather than silently invalidating its qualification. The Custom pathway can apply the new PCM-domain effects.
* The chosen channel geometry derives from the probed source count and FFmpeg's speaker-role layout. Unlabelled, noncanonical mappings cannot be reliably inferred from a mere channel count; neither the implementation nor this delivery claims otherwise.

## Verification completed in this environment

**PASS**: `python3 validation/channel_dc_smoke.py` uses the exact production matrix/filter strings and executes FFmpeg and SoX against generated Float64 inputs. It checks mono through eight-channel downmixes, 5.1(back) and 5.1(side) role mapping, LFE exclusion, sample-peak boundedness, and DC suppression with a preserved 440 Hz signal (13 checks).

**PASS**: `git diff --check` for all changed Rust files and the new smoke test.

**NOT RUN**: Rust compilation, rustfmt, the 7,529-test baseline gate, and full workspace tests. The supplied execution container has no `rustc`, `cargo`, or `rustfmt`, and cannot fetch a toolchain. The baseline's reported `7529 passed / 0 failed` is a statement in the brief, **not** a measurement on this modified tree.

Accordingly this deliverable is an **implementation candidate requiring build-host qualification**. It must not be relabeled as compiled, accepted, reference-qualified, or regression-free on the basis of its static and subprocess smoke tests.

## Build-host acceptance

From the top of the delivered repository, using the pinned toolchain and FFmpeg/SoX versions:

```bash
cargo fmt --all -- --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
python3 validation/channel_dc_smoke.py
```

Then run an actual conversion matrix: 6-channel FLAC and DVD-A to stereo PCM/lossy, DSD64 multichannel Custom to stereo PCM, existing true-peak guard and no-guard cases, gain + DC + de-emphasis/HDCD combinations as applicable, and album-/track-scope true-peak paths. Inspect the actual stereo output width, content, final true-peak ceiling within the certified contract, ordered execution evidence, and status after failure or cancellation. Capture performance against the supplied no-effect baseline before acceptance.

## Resumption

See `CHANNEL_DC_RESUME_2026-10-17.md` for the exact work status, file list, validation commands, and unresolved qualification requirements. The archive includes complete supplied source, not only changed files. Its companion `.patch` is for change review, not a substitute for the whole repository.
