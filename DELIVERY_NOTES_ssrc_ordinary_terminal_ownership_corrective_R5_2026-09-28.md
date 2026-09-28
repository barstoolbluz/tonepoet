# Delivery notes: ordinary SSRC terminal ownership corrective R5

Date: 2026-09-28
Base: `TonePoet_SSRC_Terminal_Qualification_Bounce_Corrective_R4_2026-09-28.tar.gz`

## Purpose

R5 closes the two remaining ordinary/non-true-peak SSRC ownership defects identified after R4. It does not reopen the certified true-peak architecture, qualification design, long-name implementation, output lifecycle, or commissioned registry.

## C1 - lossless non-WAV packaging no longer transfers SSRC dither ownership

For an ordinary SSRC rate conversion with no later sample-changing work, an admitted lossless non-WAV target can now use the existing `PcmTerminalRealizationKind::SsrcPreterminalFfmpegPackage` shape:

1. SSRC resolves destination-rate dither availability before any Float64 split decision.
2. SSRC writes the final integer samples at the final sample rate and final integer depth.
3. FFmpeg consumes that exact SSRC artifact only to encode/package the lossless container.
4. The package step is `apply_processing=false` and is validated to contain no rate/filter/dither processing.
5. The lowering validator requires SSRC output -> package input continuity, the same final rate/depth, exactly one SSRC step, exactly one package step, no later sample-changing step, and no downstream/duplicate dither owner.

The admission is deliberately integer-only and format-specific:

- FLAC: integer depths accepted by the existing format policy;
- AIFF: integer depths;
- lossless WavPack: FFmpeg-supported integer cells only; Int24 remains excluded because the existing plugin marks FFmpeg WavPack Int24 as a silent true-width substitution; hybrid mode remains excluded;
- ALAC: Int16/Int24.

Ordinary floating-point non-WAV SSRC behavior remains the R4 Float64 split. A regression pins that scope boundary.

At 176.4/352.8/384 kHz, where the pinned SSRC exposes no dither table, active dither refuses as `ssrc_terminal_dither_unavailable` before command construction. `None` remains valid and switchless.

## C2 - ordinary direct WAV Int32 dither remains on SSRC

The ordinary-only Int32 dither gate was removed from the SSRC resolver. Ordinary and certified SSRC terminals now use the same native destination-rate-valid SSRC dither/PDF mapping.

At a supported rate, e.g. 44.1 kHz:

- WAV Int32 + TPDF lowers to SSRC `--bits 32 --dither 99 --pdf 1`;
- a supported explicit native ID/PDF remains owned by SSRC;
- no later quantizer/dither owner is introduced.

At an unsupported dither-table rate such as 176.4 kHz:

- active dither fails clearly before command construction;
- Int32 + `None` remains a direct SSRC terminal with no `--dither`/`--pdf` switches.

## UI and conversion-log consistency

The TUI no longer applies the non-SSRC Int32 dither clamp when SSRC is selected. It preserves the user selection and uses the pipeline's rate-aware mapping as its source of truth. Unsupported selections are labeled `ssrc unavailable` rather than being described as a split.

Conversion logs now treat executed SSRC command evidence as authoritative for ordinary Int32 dither. A command carrying `--dither 99 --pdf 1` is reported as TPDF via SSRC, including the native ID/PDF. A missing dither stage remains explicitly reported as not applied.

## Local verification completed

- R4 SSRC qualification/promotion Python suite: **17/17 PASS**.
- Focused static/adversarial audit: **50/50 PASS**.
- FFmpeg 7.1.5 package-only execution, decoded PCM byte comparison:
  - FLAC Int8/16/24/32: PASS;
  - AIFF Int8/16/24/32: PASS;
  - lossless WavPack Int16/32: PASS;
  - ALAC Int16/24: PASS.
- FLAC Int32 probe reports `sample_fmt=s32`, `bits_per_raw_sample=32`.
- Commissioned SSRC true-peak registry remains unchanged and empty, SHA-256 `9aa87c0beb7ba7e8595c133c768e903d7538ac18190bd6214a2a7ff4e01b9d8d`.
- Historical failed operator report remains unchanged, SHA-256 `10965530548724afdb6edc53e9319d3c2a7b59b45800e24e9f8e534e9054a784`.
- Qualification scripts/tests and the checked long-name implementation files are byte-identical to R4.

Detailed local evidence is in `AUDIT_ssrc_ordinary_terminal_ownership_R5_2026-09-28.txt`.

## Environment limitation

This container has no `cargo`, `rustc`, `rustfmt`, `nix`, or `ssrc`. Therefore this delivery does **not** claim Rust workspace execution, Nix gates, a fresh physical SSRC qualification run, registry promotion, Reference requalification, or Journey/Bach field conversion acceptance.

Those remain operator-side gates. Use `HANDOFF_TO_OPERATOR_ssrc_ordinary_terminal_ownership_R5_2026-09-28.md`. Do not promote the certified registry unless the fresh production qualification passes every requested physical cell.
