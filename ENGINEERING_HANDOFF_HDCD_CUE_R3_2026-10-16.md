# TonePoet HDCD / CUE R3 corrective handoff

Date: 2026-10-16 (America/New_York). Status: **source-corrected; build-host UNQUALIFIED**.

## Basis and scope

Input archive: `TonePoet_LOGGING_HDCD_CUE_R3_2026-10-17.tar.gz`,
SHA-256 `23dc1f5ae370781e7813ff3de87a8e0607eded22e9d551486be96a573f408e5d`.
The archive contains the two compile corrections reported as already applied in the brief.
Its old R2 inventory no longer accounts for those two files. This delivery creates a
fresh, independently verified R3 full-source inventory; it does not silently
rewrite the historical R2 inventory.

Exactly **one existing production/test source file** changed relative to the
supplied archive: `tonepoet-pipeline/src/semantic_plan.rs`. No production DSP,
planner routing, dither selection, source identity or execution code changed.
`SOURCE_DELTA_HDCD_CUE_R3.patch` replays this one-file change against the exact
uploaded archive (not against an earlier R1 or pre-corrective R2 tree).

## Adjudication and correction

1. **The terminal failure was an invalid test assertion, not a missing
   converter terminal.** The test asked `selected_pcm_terminal()` for a terminal
   on both FLAC and MP3 conversions. MP3's legitimate terminal is
   `PlanOperation::EncodeLossy` with
   `SelectedTerminalRealization::LossyFfmpegEncoderInput`, not `EncodePcm`.
   The revised test preserves the four Track/Album x Guard/Normalize policies
   across FLAC and MP3 (eight cells), requires exactly one selected terminal,
   checks the certified observation reads Float64, checks its selected terminal
   consumes the post-gain Binary64/Float64 state with finite values, verifies a
   terminal proof, and distinguishes the correct FLAC vs MP3 realization. No
   spurious PCM intermediary is introduced to placate the test.
2. **Reported compiler errors already corrected in this input:**
   `src/tui/command.rs` defines `hdcd_decode_enabled` before the commit path
   uses it; `src/convert/processor.rs` includes `pre_resample: None` in its
   `CertifiedTruePeakExecutionContract` fixture. No duplicate edits were made.
3. **Five documentation warnings:** added `///` documentation to the two
   HDCD source identity fields and the three `HdcdCueSegment` fields. The
   one-based CUE ordinal, source sample start/count and nanosecond timestamp
   meanings were checked against the current source producers.

## Observed verification

Passed here: six FFmpeg/Python/SQLite smoke scripts; sample-exact continuous
HDCD decode then CUE cut (44,433 frames, zero mismatches), incorrect cut-first
route (390 mismatches), and unaffected ordinary member S16 identity. All
1,049 other original archive regular files were byte-identical after this
one-source-file change, and the original fixture symlink remained intact.
Patch application and byte-exact replay passed. Details and command transcripts
are in `VERIFICATION_HDCD_CUE_R3_2026-10-16.md` and
`R3_HDCD_CUE_SMOKE_LOG.txt`.

**Not proven here:** Rust compilation or warnings, the corrected Rust test
execution, full workspace tests, actual TonePoet end-to-end album conversion,
or pinned Nix/Reference source-lock requalification. There is no `rustc`,
`cargo`, `rustfmt` or `nix` in this execution environment, and network access
is unavailable. Do not equate the passing FFmpeg model smoke with an
end-to-end TonePoet run or production acceptance.

## Required build-host handoff gates

From the unpacked `tonepoet/` root, with Rust >=1.93 and the project backend
build requirements:

```sh
cargo fmt --all -- --check
cargo check --workspace
cargo test -p tonepoet-pipeline hdcd_certified_gain_four_policies_measure_the_decoded_float64_signal -- --nocapture
cargo test -p tonepoet-pipeline hdcd
cargo test --workspace
python -B tests/hdcd_cue_continuity_ffmpeg_smoke.py
python -B tests/hdcd_source_first_ffmpeg_smoke.py
python -B tests/hdcd_true_peak_composition_ffmpeg_smoke.py
python -B tests/hdcd_lossy_precision_ffmpeg_smoke.py
python -B tests/verify_hdcd_analysis_cache_sql.py
python -B tests/verify_hdcd_v28_migration_sql.py
sha256sum -c SHA256SUMS_HDCD_CUE_R3.txt
```

Read the existing Reference qualification procedures and requalify the changed
`semantic_plan.rs` source lock against the pinned toolchain only after the
workspace passes. Do **not** bless a new source-lock hash merely to suppress
an acceptance failure. Run production end-to-end CUE MP3 and FLAC conversions
with effectual/non-effectual members and certified Track/Album Guard/Normalize,
including failure-injection/evidence and runtime performance checks, before
asserting final shipping readiness.

The remaining items in the incoming brief (five R5 UI observations, #75 real
SACD tag-transfer round trip, and #62 2.19-GB RF64 real album) require the
specified user-side physical assets/inspection. No claim is made on them.
