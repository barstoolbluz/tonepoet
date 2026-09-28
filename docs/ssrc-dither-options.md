# SSRC dither and terminal semantics

This document describes the behavior implemented by the current Tonepoet planner and SSRC command lowering. It is not a recommendation table for hypothetical SSRC modes.

## SSRC controls

Tonepoet exposes these SSRC settings:

- `force`: require SSRC to own an actual PCM sample-rate conversion. If source and target PCM rates are equal, explicit SSRC is refused; SSRC is not inserted merely to perform dither.
- `insane_mode`: force the `Insane` profile.
- `profile`: one of `Insane`, `High`, `Long`, `Standard`, `Short`, `Fast`, or `Lightning`. `insane_mode` overrides `profile`; otherwise the planner derives a profile from the global resample quality when `profile` is unset.
- `attenuation_db`: optional SSRC `--att` value.
- `min_phase`: emit `--minPhase` when enabled.
- `dither_id`: optional native SSRC `--dither` override.
- `pdf_type`: optional native SSRC `--pdf` override. The typed surface supports only `Rectangular` (`0`) and `Triangular` (`1`).

There are no Tonepoet SSRC `Two-pass`, `Normalize`, or `Prevent Clipping` settings.

## Dither belongs to the final integer terminal

Tonepoet does not decide dither by comparing source and target nominal bit depths. Dither is a property of the final integer quantization stage.

Consequences:

- Float32 and Float64 SSRC outputs do not emit `--dither` or `--pdf`.
- Same-depth integer resampling may still dither when the user explicitly requests dither, because the resampler still performs a new integer quantization.
- Global `None` means no SSRC dither: with no native override, Tonepoet emits neither `--dither` nor `--pdf`.
- SSRC-native overrides apply only when SSRC owns the integer terminal. If later processing requires a Float64 split terminal, an active native override cannot be silently moved and the planner refuses that cell.

## Global dither mapping

The planner records whether a global dither family maps exactly to SSRC or is an approximation.

Exact mappings:

- `None`: inactive; no `--dither` or `--pdf`.
- `Tpdf`: `--dither 99 --pdf 1`.

Documented approximations:

- `SlopedTpdf`: SSRC has no sloped TPDF; use `--dither 99 --pdf 1`.
- `LowShibata`: ATH Curve A intensity 0 with triangular PDF.
- `Shibata`: ATH Curve A intensity 2 with triangular PDF.
- `HighShibata`: ATH Curve A intensity 6 with triangular PDF, clamped to the strongest Curve A intensity available at the destination rate.
- `Lipshitz`, `FWeighted`, `ModifiedEWeighted`, `ImprovedEWeighted`, and `Gesemann`: ATH Curve A intensity 0 with triangular PDF.

For ATH Curve A mappings, the maximum admitted intensity is 6 at 44.1/48 kHz, 2 at 88.2/96/192 kHz, and 1 at 8/11.025/22.05 kHz. The pinned SSRC 2.4.2 build has no dither table at 176.4, 352.8, or 384 kHz; this includes IDs 98 and 99. Any active dither selection is therefore refused before command construction at those rates. Global `None` remains valid because Tonepoet emits no `--dither`/`--pdf` switches. Other explicit native IDs are likewise validated against the destination rate and fail closed when unavailable.

Native `dither_id` and/or `pdf_type` settings override the derived global pair. The resolved plan records the native pair and its origin instead of pretending a native override is the original global family.

## Int32 ownership

Ordinary Int32 is resolved through the same destination-rate-valid native SSRC dither/PDF mapping as Int16 and Int24.

- At supported destination rates, an explicit global Int32 dither request remains on SSRC. TPDF at 44.1 kHz, for example, lowers as `--bits 32 --dither 99 --pdf 1`.
- A supported explicit native `dither_id`/`pdf_type` override likewise remains owned by SSRC.
- At destination rates with no SSRC dither table, any active Int32 dither request fails before command construction. `None` remains valid and emits no dither switches.
- Int32 ownership is not silently transferred to FFmpeg merely because the destination container is lossless and non-WAV.

## Terminal fusion versus Float64 split

When SSRC can own the final integer sample realization, the planner fuses resampling, integer quantization, and the resolved SSRC dither/PDF pair into that operation. Direct WAV output consumes the samples directly. An admitted non-WAV lossless target may follow with sample-preserving package-only encoding; FFmpeg owns the ordinary admitted cells, while non-hybrid WavPack Int24 uses SoX because FFmpeg cannot faithfully store that depth. The package step must consume the SSRC artifact at the same rate/depth and may not resample, filter, requantize, or dither.

When later sample-changing gain/effects or an unadmitted terminal must follow the resampler, SSRC instead emits a nonterminal Float64 carrier. An active SSRC-native dither/PDF override cannot be reassigned across that split. Ordinary Float64 SSRC output is `PcmFloating`; Float64 storage width or double-computation profiles do not by themselves establish `Binary64` preservation authority.

## Binary64 preservation authority

The stronger `TonepoetBinary64OverloadPreservingResampleV1` contract is separate from ordinary SSRC capability.

Production registry state is currently:

- `Standard`, `Short`, `Fast`, and `Lightning`: `Refuted` for the strong Binary64 contract because the audited source uses the single-precision pipeline for those profiles.
- `High`, `Long`, and `Insane`: `PendingEvidence` under Outcome C because no exact executable evidence cell has been commissioned.

`High`, `Long`, and `Insane` use double computation, but double computation alone is not a Binary64 preservation claim.

A future `Established` strong cell must also have an independently admitted protected Float64 ingress. The retained ordinary-PCM ingress is bounded classic RIFF/WAV `pcm_f64le`; only authoritative exact or bounded frame extents may prove that the carrier fits the protected RIFF cell. A duration estimate is not capacity proof. DSD-produced or ordinary pre-effect-produced Float64 boundaries do not inherit this ingress authority.

The latent protected SSRC execution route writes Float64 Wave64, validates its exact structure/geometry, and copies the PCM payload byte-for-byte into the retained raw carrier before certified observation. This route exists so a future exact evidence cell can be commissioned without another planner redesign; it does not mean any SSRC strong cell is commissioned today.

## Protected-route exclusions

- Qualified Reference delivery is sealed. Explicit SSRC does not override Reference routing or qualification.
- Certified PCM true-peak hard-ceiling operation requires an admitted overload-preserving path. With the current Outcome-C registry, Auto/FFmpeg uses the established FFmpeg/soxr protected resampler rather than treating Pending SSRC evidence as established.
- An ordinary `BeforePcmResample` registered effect is not admitted into that certified hard-ceiling path because current registered effects do not carry a protected Binary64/overload-preservation contract for their output boundary.
- Strong SSRC likewise cannot treat an ordinary pre-effect carrier as protected ingress, and SSRC evidence does not qualify a later sample-changing effect.
