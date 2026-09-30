# Delivery notes — R10 SSRC split terminal corrective R2

Date: 2026-09-30
Base: `TonePoet_R10_SSRC_Split_Terminal_Corrective_2026-09-30.tar.gz`

## Correction

R10's typed planner already selected the correct physical shape when SSRC cannot own the selected ordinary global dither at 176.4/352.8 kHz: SSRC emits a Float64 nonterminal and the qualified downstream terminal owns quantization/dither.

The retained command-lowering bridge lost one fact from that typed plan. Its downstream `EncodePcm` correctly carries `target_rate_hz: None` because SSRC already performed the rate conversion, but the FFmpeg and SoX command builders previously inferred fractional input only from a local rate/gain operation or from nominal source-depth reduction. Same-depth terminal landings therefore omitted dither even though their actual input was SSRC Float64.

Corrective R2 recovers exactly that missing fact in the existing PCM lowerers. A shared local predicate replays the typed SSRC immediate-output decision and identifies an integer PCM-lossless terminal consuming an SSRC Float64 continuation. Both FFmpeg and SoX OR that fact into their existing fractional/precision-reduction dither decision. The terminal rate remains `None`; no second sample-rate stage is introduced.

No terminal matrix, new carrier-state model, ownership architecture, or logging layer was added.

## Boundaries preserved

- Explicit SSRC-native `dither_id` / `pdf_type` overrides still fail closed and do not migrate.
- `DitherType::None` retains direct SSRC ownership where SSRC can own the integer terminal.
- Existing same-depth integer paths that are not consuming an SSRC Float64 split do not gain dither merely because processing is enabled.
- FFmpeg's dither path may use `aresample` as its quantize/dither filter, but the split terminal supplies no `out_sample_rate` and no `-ar`; SSRC remains the only rate converter.
- The SoX split terminal emits the selected dither effect and no `rate` / `-r` stage.

## Focused regressions

- WavPack Int24, 96 kHz -> 176.4 kHz, forced SSRC, explicit TPDF: SSRC `--bits -64`, no SSRC `--dither`/`--pdf`, SoX owns TPDF, no SoX rate stage.
- FLAC Int16, 96 kHz -> 176.4 kHz, forced SSRC, explicit TPDF: same-depth source/target, SSRC Float64 split, FFmpeg emits `dither_method=triangular`, no FFmpeg sample-rate change.
- Explicit SSRC-native override at an unavailable SSRC dither rate still refuses.
- WavPack Int24 + `DitherType::None` still stays SSRC-owned.
- The three R10 typed terminal assertions now require `realization.target_rate_hz == None`; a typed input-state assertion separately proves 176.4 kHz has already been reached.

## Verification in this environment

- Original R10 static audit: 20/20 PASS.
- R10 R2 focused static audit: 16/16 PASS.
- Adversarial mutation removing the FFmpeg continuation signal and restoring one stale `Some(176_400)` assertion: 14/16 PASS, nonzero as required.
- Edited Rust files have no trailing whitespace.
- No `cargo`, `rustc`, `rustfmt`, or `nix` executable is available in this sandbox, so Rust compilation was not claimed.

Build-host acceptance remains:

```bash
cargo test -p tonepoet-pipeline
```
