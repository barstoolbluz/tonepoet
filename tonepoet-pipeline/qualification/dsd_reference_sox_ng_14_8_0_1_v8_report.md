# DSD Reference policy v8 qualification report

**Policy:** `sox_ng_14_8_0_1_v8`  
**Status:** qualification candidate; unpromoted  
**Correction:** terminal F4 — SoX-ng signed-32-bit effects-boundary authority

## Append-only correction

Policy v8 is an append-only correction to v7. Policies v1-v7 and their artifacts remain immutable and history-only. V8 inherits the complete v7 carrier-sensitive decode table, Float64 RIFF/RF64 SoX-to-FFmpeg package pipeline, sample-identity oracle, environment contract, supported-cell matrix, analyzer authority, and public `-1.000000000 dBTP` ceiling.

The only normative cell value changed is the Float64 terminal-realization error authority. The terminal `gain` effect executes in SoX-ng's signed-32-bit internal effects domain even when both the reconstruction and terminal carriers are Float64. A signed Q1.31 sample grid has spacing `2^-31`; round-to-nearest contributes at most one half-step, `2^-32`. The previous `2^-51` allowance still covers Float64 coefficient/arithmetic error and is additive rather than discarded.

The v8 Float64 bound is therefore:

```text
max_added_peak_fs = 2^-32 + 2^-51
max_added_peak_fs_q63_ceil = 2^31 + 2^12 = 2147487744
safe_pre_terminal_ceiling_dbtp = floor_nano_db(20 log10(A - (2^-32 + 2^-51)))
A = 10^((-1.000000000 - 0.010000000) / 20)
S = -1.010000003 dBTP
```

The previous v7 bound, `2^-51`, modeled only f64 arithmetic and is unattainable by itself through this effects chain.

## Pinned-source proof

The derivation is now bound to `dsd_reference_sox_ng_14_8_0_1_v8_terminal_source_proof.md`. That proof identifies the exact pinned revision and NAR hash, the signed-32-bit `sox_sample_t` authority, the exact Float64 carrier conversion macros, and the non-limiter `gain.c` rounding site. It proves that every SoX internal grid value survives the Float64 carrier round trip exactly and that the terminal gain contributes at most one non-clipping half-step, independently of whether the resolved policy is `ReferenceCompensated`, `NativeLevelExact`, or `FixedExact`.

The qualification machine report records the proof path and SHA-256. Runtime release certification recomputes that digest and rejects any missing, substituted, or semantically altered proof object.

## Terminal-bound audit of every enabled depth

- **Int24 TPDF:** retained at `2^-22` (`2` output LSB). The terminal effects half-step is `2^-32`, exactly ten bits below the existing bound; the TPDF/quantizer authority remains the limiting term and its existing conservative margin contains the effects-domain rounding.
- **Float32:** retained at `2^-23`. The Float32 near-full-scale spacing is `2^-23`; its existing one-ULP bound exceeds the `2^-32` effects half-step by nine bits and remains conservative for the combined effects and carrier-rounding path.
- **Float64:** corrected to `2^-32 + 2^-51`: the limiting effects-domain half-step plus the inherited Float64 arithmetic allowance. No enabled Float64 cell is left with the unattainable f64-only bound.
- **Int16:** remains rejected. The commissioned Shibata realization still lacks a conservative worst-case peak bound.

The bound is rate-invariant, while each derivation digest remains rate-specific and binds policy v8, target rate, depth, realization label, Q1.63 ceiling, analyzer reserve, and safe pre-terminal ceiling.

## Audit of the remaining enabled-cell assertions

The two systemic causes were applied to every assertion exercised by the four-test qualification target:

- **Analyzer authority:** the carrier-sensitive v6 route remains depth-correct: Float32 W64 is decoded directly by FFmpeg; Float64 and Int24 W64 are decoded by SoX into the qualified f64 stream. The `0.010000000 dB` reporting quantum and `0.100000000 dB` residual bounds are many orders of magnitude wider than a `2^-32` full-scale sample perturbation and do not assume f64-only effects arithmetic.
- **Profile-response cells:** passband, transition, and stopband assertions measure the actual pinned SoX-ng output after its internal effects path. They are empirical response limits, not sub-`2^-32` arithmetic-error claims. The F3 real-tool run passed them before reaching F4; no profile cell is predicted to regress from the corrected terminal authority.
- **Terminal realization:** every enabled rate/channel/depth cell uses the typed R64 and QPCM carrier bindings. Int24 and Float32 retain conservative bounds; Float64 uses `2^-32 + 2^-51`. The check remains strict and records the maximum observed error by depth in the machine report.
- **Packaging and preservation:** decoded-sample equality remains exact, not tolerance-based. Every QPCM, packaged, and post-metadata carrier is selected from the closed role/target/depth route table. Direct FFmpeg decode of Float64 W64 and SoX decode of Float32 W64 are structurally rejected before command construction.
- **DST/source-front-end, container capacity, metadata, environment, and supervision assertions:** these do not derive sample-domain precision from the effects accumulator and are unaffected by the `2^-32` boundary. Their route-bearing subprocesses retain the same typed carrier and `ClearAndSet` authority that passed the F3 corrective run.

No enabled assertion remains coupled to the defective per-depth decoder route or to an unattainable f64-only sub-`2^-32` SoX terminal-error model.

## Per-depth decode-route audit

Every enabled carrier role is bound through the closed typed route table:

- Reconstruction R64 Float64 W64: SoX raw-f64 stream.
- Terminal QPCM Int24 W64: direct FFmpeg.
- Terminal QPCM Float32 W64: direct FFmpeg.
- Terminal QPCM Float64 W64: SoX raw-f64 stream.
- Packaged/post-metadata W64: direct FFmpeg for Int24/Float32; SoX raw-f64 stream for Float64.
- Packaged/post-metadata non-W64: direct FFmpeg at the depth-native codec.

The terminal-realization check consumes only the plan-bound R64 and QPCM carrier authorities. Package and post-metadata preservation checks consume only their plan-bound authorities. Direct FFmpeg decode of Float64 W64 remains forbidden; SoX readback of Float32 W64 is never used.

## Enabled-cell disposition

All 13,248 previously enabled cells remain attainable under the corrected v8 bound and the inherited v7 route table. No enabled cell requires rejection. Existing rejected source/rate/profile/target/depth cells remain rejected with their prior append-only reasons.

## Mandatory gates

```text
python3 tonepoet-pipeline/qualification/derive_dsd_reference_v5_terminal_bounds.py --check
python3 tonepoet-pipeline/qualification/derive_dsd_reference_v6_terminal_bounds.py --check
python3 tonepoet-pipeline/qualification/derive_dsd_reference_v7_terminal_bounds.py --check
python3 tonepoet-pipeline/qualification/derive_dsd_reference_v8_terminal_bounds.py --check
cargo fmt -p tonepoet -p tonepoet-pipeline -p sacd-rs -- --check
cargo test -p tonepoet-pipeline
cargo test -p sacd-rs
cargo test -p tonepoet --test settings_sentinel
cargo test --workspace
cargo clippy -p tonepoet -p tonepoet-pipeline -p sacd-rs --all-targets --all-features -- -D warnings
TONEPOET_REQUIRE_TOOLS=1 \
TONEPOET_DSD_REFERENCE_REPORT_PATH="$PWD/tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v8_certification.json" \
  cargo test -p tonepoet --test dsd_reference_qualification -- --nocapture
```

The real-tool report must be schema version 8 and bind the exact v8 candidate manifest. Promotion is not claimed by the source-controlled candidate.
