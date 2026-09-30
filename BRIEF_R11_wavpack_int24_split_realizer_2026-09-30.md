# Brief R11 — one cell of the SSRC split does not realize

Date: 2026-09-30
Base: the supplied `tonepoet-src.tar.gz` is authoritative. It is our local
`main` with your R10 corrective R2 applied.

R10 works. SSRC no longer becomes unavailable when it cannot own the selected
dither, the explicit-native-override refusal still holds, and `DitherType::None`
stays SSRC-owned. Eleven of the twelve valid cells plan and lower correctly.
One does not.

---

## The failing cell

Your own regression fails:

```
plan::stage_a_lowering_selection_diagnostics::
  wavpack_int24_ssrc_unavailable_dither_splits_to_sox_but_none_stays_ssrc_owned

176.4 kHz WavPack Int24 TPDF must split after SSRC instead of refusing:
CapabilityUnavailable { capability: "common_realizer",
  reason: "the typed plan is valid but requires the Phase-3 common realization path" }
```

The typed plan is valid — the planner half of R10 did its job. Lowering cannot
realize it.

## The boundary, measured

Planning across formats and depths, all 96 kHz to 176.4 kHz, forced SSRC,
explicit TPDF, metadata transfer off. 352.8 kHz, the other rate the original
report named, was not measured:

| target | Int16 | Int24 | Int32 | Float32 | Float64 |
|---|---|---|---|---|---|
| FLAC | ffmpeg→ssrc→ffmpeg | ffmpeg→ssrc→ffmpeg | ffmpeg→ssrc→ffmpeg | format-rejected | format-rejected |
| WavPack | ffmpeg→ssrc→ffmpeg | **common_realizer** | ffmpeg→ssrc→ffmpeg | ffmpeg→ssrc→ffmpeg | format-rejected |
| WAV | ffmpeg→ssrc→ffmpeg | ffmpeg→ssrc→ffmpeg | ffmpeg→ssrc→ffmpeg | ffmpeg→ssrc | ffmpeg→ssrc |

Fifteen cells, three format-rejected by pre-existing rules — FLAC/ALAC refuse
float depths, WavPack refuses Float64 — leaving twelve valid, of which eleven
realize. The two WAV
float cells correctly plan no terminal, because there is nothing to quantize.

Every cell whose terminal is FFmpeg realizes. The single failing cell is the
one whose terminal is SoX.

## What that suggests, unverified

WavPack Int24 is the cell R6 gave its own `SsrcPreterminalSoxPackage` path,
routing packaging through SoX rather than FFmpeg. The split variant of that
shape appears to have no realizer, while the FFmpeg-terminal split does. We
did not read the realizer to confirm this, and the neighbouring evidence is
circumstantial rather than a diagnosis.

## Required

A user-selectable rate, format and depth combination that the planner accepts
produces a runnable pipeline. Specifically, WavPack Int24 at a destination
rate where SSRC cannot own the dither converts, with SSRC doing the rate
conversion and SoX owning the quantization and dither.

The boundaries R10 established stay: no second rate stage after SSRC, explicit
SSRC-native overrides still fail closed, `DitherType::None` still SSRC-owned.

If some combination genuinely cannot be realized, the refusal should say that
at planning time rather than producing a valid plan that cannot be executed.

## State

Gate 7207 passed / 5 failed. Four of those five pass in isolation and are the
known coordination-contention flake, which has now hit ten different tests
across runs, in `concurrency`, `tui::keybindings`, `tui::probe` and
`tui::rename_plan`. The fifth is the regression above.

Your static audits pass here: R10 base 20/20, R10 R2 16/16.

## Build capability

`cargo test -p tonepoet-pipeline` builds in seconds and covers the planner and
both lowerers; it is where this failure surfaces. The root `tonepoet` crate
peaks at 6.24 GB in one `rustc` and will OOM under a 4 GB ceiling.
