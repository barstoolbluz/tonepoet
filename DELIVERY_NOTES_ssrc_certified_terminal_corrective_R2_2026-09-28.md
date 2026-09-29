# TonePoet SSRC certified-terminal corrective R2

Date: 2026-09-28
Baseline: `TonePoet_SSRC_Terminal_and_Long_Names_R1_2026-09-27.tar(2).gz`
Scope: the two reported certified-SSRC true-peak defects only.

## Outcome

Both defects are corrected without changing ordinary/direct SSRC policy, the general true-peak architecture, or the long-name implementation.

### 1. Floating-point PCM certified terminal

The certified SSRC true-peak route now admits Float32 and Float64 physical cells.

- Qualification uses SSRC's native `--bits -32` / `--bits -64` modes.
- Float cells are generated/promoted only with dither and PDF disabled, matching the pre-existing direct SSRC behavior for float targets.
- Qualification decodes real Wave64 f32/f64 terminal samples and records executed realization error as an absolute linear-full-scale bound.
- Commissioned bindings now carry a depth-aware error authority:
  - integer cells: target-LSB bound;
  - float cells: exact f64-bit absolute-linear-FS bound.
- Runtime validates exact Wave64 integer/float encoding by target depth, decodes f32/f64 samples, and runs the same every-sample fail-closed conformance pass.
- The stored-sample error is lifted through the existing `HQ1024V1_RECONSTRUCTION_LINF_GAIN_UPPER` authority exactly as the integer terminal error was.
- Downstream planning no longer rejects a certified float carrier. Dither, gain, and resampling remain disabled after SSRC; the remaining work is packaging/sample-preserving encoding only.

The generated production registry remains intentionally empty. No float rows were fabricated; they must still come from operator-machine qualification and promotion.

### 2. Qualification gain corpus no longer imposes a hidden +/-24 dB product limit

The default qualification gain points remain representative execution evidence and continue to derive the conservative per-cell error bound. Their minimum/maximum are retained in the evidence record for auditability, but they are no longer used as runtime TruePeakNormalize admission limits.

Runtime admission now checks that the resolved finite gain can be rendered by the exact SSRC diagonal `--mixChannels` matrix. The actual SSRC terminal realization is still checked sample-by-sample against the commissioned error bound before output commit. An outlying gain whose realized error exceeds the bound still fails closed.

No new automatic-normalization cap was introduced. Existing FixedGain product validation remains unchanged.

## Focused regression coverage added

Python qualification/promotion tests now cover:
- Float32/64 cell parsing;
- no dither/PDF on float cells;
- Float32 Wave64 decoding;
- float production-grid cells;
- native float terminal command arguments with no `--dither`, `--seed`, or `--pdf`;
- absolute-linear float bound derivation;
- float promotion and generated depth-aware authority;
- promotion refusal of float+dither evidence.

Rust tests were added for:
- `--bits -32` / `--bits -64` certified terminal commands without dither switches;
- float terminal error lifting into the existing album hard-ceiling bound;
- per-sample Float32/64 conformance acceptance and a deliberately mutated out-of-bound sample rejection;
- packaging certified Float32/64 terminals to WAV and AIFF, plus lossless WavPack Float32, with sample-changing stages disabled;
- TruePeakNormalize resolving above +24 dB (about +29.9 dB for a -30 dBTP fixture) despite a representative -24..+24 dB qualification corpus;
- extreme/unrepresentable gain matrix construction remaining fail-closed.

## Verification performed here

PASS:
- Python syntax compilation for qualification, promotion, and their tests.
- Qualification/promotion unit suite: **12/12 PASS** (R1 baseline was 8/8).
- Synthetic float promotion replay against the current source-authority hashes: PASS.
- Focused source audit: **10/10 PASS**.
- Baseline containment diff: only the seven expected implementation/test files plus corrective documentation/checkpoint differ.
- `tonepoet-pipeline/src/settings.rs`: byte-identical to R1.
- `tonepoet-pipeline/src/plugins.rs`: byte-identical to R1.
- `tonepoet-pipeline/src/ssrc_true_peak_terminal_commissioned.rs`: byte-identical to R1 and still empty.

Not executable in this environment:
- Rust compile/workspace tests (`cargo`/`rustc` absent);
- Nix gates (`nix` absent);
- physical SSRC qualification/promotion and field conversions (`ssrc` absent).

Those are still required operator-side gates. This delivery does not claim them.

## Changed implementation/test files

- `tonepoet-pipeline/src/ssrc_true_peak_terminal.rs`
- `tonepoet-pipeline/qualification/ssrc_true_peak_terminal/qualify_ssrc_true_peak_terminal.py`
- `tonepoet-pipeline/qualification/ssrc_true_peak_terminal/promote_ssrc_true_peak_terminal.py`
- `tonepoet-pipeline/qualification/ssrc_true_peak_terminal/test_ssrc_true_peak_terminal_qualification.py`
- `src/convert/pipeline/stages.rs`
- `src/convert/pipeline/plan_bridge.rs`
- `src/convert/processor.rs`

No long-name implementation file changed.
