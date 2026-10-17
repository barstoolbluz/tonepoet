# Persistent resume checkpoint -- HDCD / CUE R2 (2026-10-17)

**STATE:** Four user-reported source-level defects corrected in full R1-derived repository. FFmpeg samples and SQL regression scripts pass. **DO NOT CLAIM COMPLETED BUILD-HOST ACCEPTANCE**; Rust toolchain, full gates, pinned Reference qualification and executable TonePoet integration tests are unavailable here.

**INPUT:** `TonePoet_HDCD_TRUEPEAK_R1_UNQUALIFIED_FULL_REPO_2026-10-16.tar.gz`, SHA256 `d5ffc013b7b668d55b7792629955203dbdc9ebcc218089c4a84772f3ebc568ca`.

**OUTPUT:** full archive `TonePoet_HDCD_CUE_R2_UNQUALIFIED_FULL_REPO_2026-10-17.tar.gz` and `.sha256` sidecar. Inside: `tonepoet/` complete repo, `SOURCE_DELTA_HDCD_CUE_R2.patch` relative to exact R1, and `SHA256SUMS_HDCD_CUE_R2.txt` (self-excluding manifest).

**TWO MODIFIED PRODUCTION FILES:**
- `src/convert/pipeline/track_executor.rs`: member-local effect-aware direct CUE stream admission; true-peak barrier guard.
- `src/convert/pipeline/stages.rs`: continuous HDCD before sample-exact CUE cut, skip unused grouped S32 fallbacks for effectual member, fail-safe PCM true-peak prelude evidence for Track and Album, early completed HDCD effect record, focused Rust regression tests.

**TEST FILES:** `tests/hdcd_cue_continuity_ffmpeg_smoke.py` (new valid Peak Extend fixture; independent exact waveform comparison), `tests/hdcd_source_first_ffmpeg_smoke.py` and `tests/hdcd_true_peak_composition_ffmpeg_smoke.py` (corrected CUE decode/cut assertions).

**COMPLETED:** physical continuous decode then cut equals full-programme reference for 44,433 frames at non-second boundary, old cut-first path differs in 390 sample observations, neighboring ordinary member retains source S16 bytes; source-first + true-peak FFmpeg smoke passes; prior lossy/dither/SQL smoke passes; R2 patch and archive/checksum integrity verified separately in `VERIFICATION_HDCD_CUE_R2_2026-10-17.md`.

**OPEN/UNQUALIFIED:** No Rust compiler, no targeted Rust tests, no actual TonePoet CUE MP3 or Guard/Normalize execution, no injected track/album error logs, no full workspace tests, no pinned Reference requalification. Multi-track HDCD presently decodes original continuous image per selected member; shared one-pass grouping has not been implemented or proven and should only be considered after measuring real build-host impact. Retain R1's documented protected SSRC and pre-resample de-emphasis/rate-change refusals.

**RESUME PROCEDURE:**
1. Verify external tar sha256 and internal R2 inventory. Read this checkpoint, handoff and verification ledger. Baseline is exact R1, **not** the earlier 2026-10-16 fresh-cut pre-R1 snapshot.
2. Run `cargo fmt --all -- --check`, `cargo check --workspace` and targeted Rust tests newly named `cue_stream_admission_is_per_member_and_rejects_hdcd_and_true_peak`, `grouped_cue_fallback_never_stages_unusable_hdcd_member_carriers`, `hdcd_true_peak_partial_prelude_tests`, `hdcd_source_first_tests`; run all existing HDCD and CUE gates. Address only genuine compiler/test defects.
3. Run full `cargo test --workspace`; actual 16/44.1 effectual HDCD CUE -> 44.1 MP3 with Track/Album Guard and Normalize and mixed non-HDCD tracks, including source sample and command-record equality; inject a failure after successful HDCD S32 and after successful Float64 conversion, inspect discarded operation/effect invocation indices and publication atomicity.
4. Run pinned Reference verification and production performance (decoder calls, scratch peak) before production handoff. Regenerate R2 (or R3) complete archive, focused delta and all SHA manifests to represent changes. Never report unrun gates as passed.

**CHANGE DISCIPLINE:** No generalized processing graph, cached control-state heuristics, double dither, altered DSP/SSRC/DSD/Reference certificates or speculative logging framework. Preserve existing semantics and corrections already accepted in R1.
