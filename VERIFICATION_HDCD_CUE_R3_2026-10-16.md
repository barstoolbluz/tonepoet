# HDCD / CUE R3 verification ledger

Verification environment: 2026-10-16, Python and FFmpeg installed;
`rustc`, `cargo`, `rustfmt`, `nix` unavailable and outbound network DNS blocked.

## PASS: executed

| Check | Result |
| --- | --- |
| `python -B tests/hdcd_cue_continuity_ffmpeg_smoke.py` | PASS: 44,433 post-HDCD-cut frames identical; intentionally bad ordering differs in 390 sample values; neighboring S16 member unchanged |
| `python -B tests/hdcd_source_first_ffmpeg_smoke.py` | PASS: source-first FLAC/CUE classification and 44,159-frame S32 carrier |
| `python -B tests/hdcd_true_peak_composition_ffmpeg_smoke.py` | PASS: Float64, source-first, preemphasis, one resample, Track/Album gain witnesses, CUE boundaries, terminal TPDF and MP3 |
| `python -B tests/hdcd_lossy_precision_ffmpeg_smoke.py` | PASS: Opus and MP3 input precision and Int16 TPDF witness, optional `libfdk_aac` NOT RUN |
| `python -B tests/verify_hdcd_analysis_cache_sql.py` | PASS: HDCD analysis cache identity/invalidations |
| `python -B tests/verify_hdcd_v28_migration_sql.py` | PASS: 14 modeled migration cases |
| Complete input tar member SHA-256 comparison after source patch | PASS: 1049 unchanged regular files, exactly one source file changed (`tonepoet-pipeline/src/semantic_plan.rs`), no missing members, one symlink preserved |
| R3 patch application / byte-exact replay against input `semantic_plan.rs` | PASS |

The raw stdout/stderr from the six smoke scripts is preserved in
`R3_HDCD_CUE_SMOKE_LOG.txt`.

## Static source findings

- The legacy `selected_pcm_terminal` helper intentionally locates
  `PlanOperation::EncodePcm` only. The failing HDCD test invoked it even for
  `AudioFormat::Mp3`. Production's lossy terminal is `EncodeLossy` with
  `LossyFfmpegEncoderInput`, for which the selected terminal proof is valid.
- The updated HDCD regression test distinguishes the two real terminal kinds,
  keeps one selected terminal per requested conversion, checks its Float64
  input state and proof, and checks the certified observation subject.
- The incoming `src/tui/command.rs` already binds `hdcd_decode_enabled`; the
  incoming `src/convert/processor.rs` already includes `pre_resample: None`.
- The five documented fields under `RegisteredUnaryEffect::HdcdDecode` and
  `HdcdCueSegment` now have Rust doc comments.
- The prior `SHA256SUMS_HDCD_CUE_R2.txt` was already stale in the original
  input archive for `src/tui/command.rs` and `src/convert/processor.rs`.
  Current R3 source and delivery checksums are governed exclusively by the
  independently verified new `SHA256SUMS_HDCD_CUE_R3.txt`.

## OPEN: cannot be claimed passed here

- Revised Rust test execution and full Rust workspace build/check/test.
- Rust lint/no-warning verification and `cargo fmt` verification.
- Pinned Reference/source-lock requalification after changing
  `tonepoet-pipeline/src/semantic_plan.rs`.
- End-to-end native TonePoet HDCD/CUE and Track/Album certified-gain routes,
  failure evidence/logging and measured performance.
- Optional `libfdk_aac` runtime witness and the brief's real-media/UI checks.

These are qualification gates, not evidence of a newly observed runtime defect.
The R3 code delta is limited to one corrected test and five field docs.
