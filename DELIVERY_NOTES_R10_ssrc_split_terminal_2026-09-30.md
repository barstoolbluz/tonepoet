# Delivery notes — R10 SSRC split terminal

Date: 2026-09-30
Base: supplied `tonepoet-src.tar.gz` from `TonePoet_R10_SSRC_Split_Terminal_2026-09-30.tar.gz`

## Outcome

R10 removes the over-broad whole-request refusal for an ordinary global dither selection when the selected SSRC destination rate has no native dither table.

When SSRC is otherwise the admitted resampler and its ordinary derived global dither is unavailable at the destination rate, the planner now keeps SSRC for the rate conversion, writes a Float64 nonterminal carrier, and gives the final integer realization to the existing qualified terminal planner. No new terminal capability is invented: FFmpeg/SoXR is used only for cells it already admits, SoX is used where its existing realization is qualified, and the request still refuses if no downstream candidate can satisfy the terminal contract.

The integrity boundary from R6 remains intact. An explicit SSRC-native `dither_id` and/or `pdf_type` does not migrate to another tool. Native overrides still resolve and rate-validate as SSRC-owned settings and fail closed when the requested native cell is unavailable. The existing `ssrc_native_terminal_override_cannot_split` guard for later sample processing remains unchanged.

`None` is also unchanged: where SSRC can own the final integer samples, no-dither requests remain direct SSRC terminals with no `--dither`/`--pdf` switches.

## Small implementation change

The production behavior change is concentrated in `tonepoet-pipeline/src/semantic_plan.rs::resolve_ssrc_immediate_output`:

- `SsrcDitherAvailability::UnavailableForSsrcTerminal` from an ordinary derived global family now returns `Float64 + Nonterminal` rather than `ssrc_terminal_dither_unavailable`.
- Explicit native overrides never reach that migration path: `resolve_ssrc_dither_for_rate` rate-validates the native ID and returns an error if SSRC cannot execute it.
- The existing downstream terminal selection and structured `dither_owner` contract remain the authority for which tool performs quantization/dither.

The legacy command topology already calls the same immediate-output resolver, so it follows the same split. Its established nonterminal path enables terminal processing when dither is active.

The TUI no longer labels this ordinary condition `ssrc unavailable`; it labels it `split dither`. This is intentionally not a capability promise. The planner still decides whether a qualified terminal exists for the chosen format/depth/dither cell.

The conversion log already derives dither ownership from executed command evidence and names `SSRC`, `SoX`, or `ffmpeg aresample`; no logging architecture change was necessary.

## Existing downstream qualification boundary

R10 deliberately reuses the terminal matrix already present in the planner rather than declaring that every split is executable. Low integer depths can use the existing FFmpeg/SoXR or SoX terminal cells according to format and dither family; FFmpeg-only containers can use the established SoX-preterminal/FFmpeg-package shape where it is admitted. Lossless WavPack Int24 remains excluded from FFmpeg because FFmpeg stores that cell as true 32-bit WavPack, so the split falls to the existing SoX Int24 terminal instead.

Int32 remains narrower by design: ordinary SoX Int32 dither is not behavior-qualified, while FFmpeg Int32 dither requires an admitted FFmpeg/SoXR mapping and is not used for families that the planner marks SoX-owned. Therefore an Int32 request whose selected dither has no qualified downstream realization can still refuse. That is a genuine whole-request terminal-capability refusal, not the SSRC destination-table refusal R10 removes.

## Regression coverage added/updated

The pipeline tests now pin these boundaries:

- 176.4 kHz and 352.8 kHz ordinary global dither choose the Float64 SSRC split.
- WAV Int16 + TPDF at 176.4 kHz: SSRC remains the resampler; FFmpeg owns the terminal TPDF.
- WAV Int32 + explicitly selected global TPDF at 176.4 kHz: split to the qualified FFmpeg Int32 terminal rather than confusing a global explicit choice with an SSRC-native override.
- FLAC Int16 + TPDF at 176.4 kHz: SSRC Float64 then FFmpeg terminal dither.
- lossless WavPack Int24 + TPDF at 176.4 kHz: SSRC Float64 then SoX terminal dither; this reuses the pre-R6 split shape only when SSRC itself cannot own dither.
- the same WavPack Int24 request with `None` remains SSRC-owned and SoX package-only.
- explicit SSRC-native ID/PDF at 176.4 kHz still refuses instead of migrating ownership.

## Verification performed in this sandbox

- Outer input `SHA256SUMS`: PASS.
- Full-tree pre-document comparison against the supplied `tonepoet-src.tar.gz`: exactly five existing Rust files changed:
  - `tonepoet-pipeline/src/semantic_plan.rs`
  - `tonepoet-pipeline/src/plugins.rs`
  - `tonepoet-pipeline/src/plan.rs`
  - `tonepoet-pipeline/src/mapping.rs`
  - `src/tui/app.rs`
- Focused static/adversarial audit: 20/20 PASS.
- Mutation probe: deliberately replacing the unavailable-dither split with a terminal result causes the audit to fail (19/20), proving the key audit check is discriminating.
- No trailing whitespace in changed Rust files.

## Environment limitation

This sandbox does not contain `cargo`, `rustc`, `rustfmt`, or `nix`. Therefore this delivery does **not** claim Rust compilation or execution.

The brief says `cargo test -p tonepoet-pipeline` is the relevant fast planner gate; run it on the build host before handoff. The root `tonepoet` crate is also modified only for the TUI status wording/helper and, per the supplied brief, may exceed a 4 GB build ceiling. Run the root tests only on a host with sufficient memory rather than weakening build settings or redesigning the change to fit this sandbox.
