# Delivery notes — SSRC WavPack Int24 package-only corrective R6

Date: 2026-09-28
Base: `TonePoet_SSRC_Ordinary_Terminal_Ownership_Corrective_R5_2026-09-28.tar.gz`

## Defect closed

R5 correctly made ordinary SSRC own final integer samples and dither for direct WAV and the admitted FFmpeg package-only lossless cells, but deliberately excluded non-hybrid WavPack Int24 because FFmpeg silently stores that cell as true 32-bit WavPack. The exclusion caused a forced ordinary SSRC conversion to fall back to `SSRC Float64 -> SoX Int24 + dither`, transferring dither ownership away from SSRC.

## Correction

A distinct `PcmTerminalRealizationKind::SsrcPreterminalSoxPackage` is admitted only when all of these are true:

- target is lossless non-hybrid WavPack;
- final PCM depth is Int24;
- no later sample-changing processing is required;
- SSRC can execute the selected dither/PDF at the destination rate.

SSRC therefore emits the final Int24 samples and owns terminal dither/noise shaping. The physical package step is frozen to SoX with `apply_processing=false`; it must consume the exact SSRC artifact at the same rate/depth. The lowering validator rejects a SoX rate override, requires the output path to be the last argument (so no SoX effects can follow), rejects second sample-changing operations, and rejects duplicate/downstream dither ownership.

FFmpeg's existing WavPack Int24 refusal is unchanged.

## Focused regressions added

- 96 kHz -> 44.1 kHz lossless WavPack Int24 + forced SSRC + explicit TPDF selects `SsrcPreterminalSoxPackage`; SSRC owns `--bits 24 --dither 99 --pdf 1` and SoX packages only.
- 96 kHz -> 176.4 kHz WavPack Int24 + TPDF refuses with `ssrc_terminal_dither_unavailable` before command construction.
- The same 176.4-kHz request with dither `None` remains SSRC-owned and switchless, followed by SoX package-only output.
- The existing FFmpeg WavPack Int24 unsupported test now also freezes the SoX package-only command shape (`-b 24`, no rate/dither effect tokens, output path last).

## Local evidence

- R4/R5 SSRC qualification/promotion Python suite: 17/17 PASS.
- R6 focused static/adversarial audit: 51/51 PASS.
- SoX 14.4.2 package-only WavPack Int24 encode/decode: byte-identical PCM at compression levels 0, 1, 2, and 3; genuine 24-bit output at 44.1 kHz.
- Certified true-peak qualification scripts, commissioned registry, failed historical operator report, and long-name files remain byte-identical to R5.

See `EVIDENCE_sox_wavpack_int24_package_only_R6_2026-09-28.txt` and `AUDIT_ssrc_wavpack_int24_package_only_R6_2026-09-28.txt`.

## Environment limit

This container has SoX and FFmpeg but no `cargo`, `rustc`, `rustfmt`, `nix`, or `ssrc`. Rust/Nix/real-SSRC execution remains an operator gate. No registry row was fabricated or promoted.
