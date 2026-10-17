# HDCD + certified true-peak: engineering handoff (2026-10-16)

## Qualification status: SOURCE IMPLEMENTATION; BUILD-HOST ACCEPTANCE REQUIRED

This is the **entire fresh-cut 2026-10-16 TonePoet repository** with a focused modification and a compile-free FFmpeg witness. **Do not treat this archive as build-verified or production-qualified.** Neither Cargo nor rustc nor rustfmt nor the pinned Nix/Reference qualification runtime is available in this session. The integration must compile, pass the focused Rust tests and full workspace suite, and undergo applicable Reference requalification on the build host before accepting it.

## Objective and implemented source changes

* Removed the global HDCD-versus-certified-true-peak prohibition in the typed planner and TUI command path, **without weakening the source identity and eligible 16-bit/44.1-kHz guards**.
* An effectual HDCD member's certified Track or Album Guard/Normalize path now binds the **same typed pre-observation execution contract** as its actual registered-effect spine. HDCD runs source-first through FFmpeg on the original S16 source or exact CUE slice, producing S32; a private conversion raises it to Float64 for sample-domain effects. The selected, supported FFmpeg rate change operates once, before the certified Float64 observation. The already-commissioned true-peak scanner and gain resolver remain the only measurement and decision owners.
* Supports the admitted no-rate-change HDCD + CD de-emphasis path, along with registered effects the actual typed planner can place after an admitted resampling barrier. Does not apply automatic +6 dB gain or decode HDCD twice.
* True-peak preparation exposes a content-bound Float64 carrier to the existing Track- or Album-scoped terminal. For a Float64 WAV-only intermediate, recorded FFmpeg WAV-to-raw unwrapping is a container conversion, not an audio effect, dither, or resampler.
* Per-track Int16 auto-TPDF authority is shared by certified terminal-bound calculation and final encoder replan, including mixed CUE batches. Explicit dither-off remains authoritative. No global batch-wide TPDF toggle was added.
* Actual HDCD/registered-effect command records and operation-evidence invocation indices now follow the track into Track terminal processing, or the Album barrier into per-track final records. Scratch retries carry the prelude without rerunning it.
* Added typed-planner tests (Track/Album, Guard/Normalize, FLAC/MP3, FFmpeg rate change, ordering, precision), a source-bound execution contract test and member-local dither test, and a compile-free FFmpeg/numpy/scipy witness (7 assertions/groups) for the high-precision signal path, gain calculations, CUE range, lossy and Int16 terminals.

### Source changes

- `tonepoet-pipeline/src/semantic_plan.rs`
- `src/convert/pipeline/plan_bridge.rs`
- `src/convert/pipeline/stages.rs`
- `src/tui/command.rs`
- `src/tui/app.rs`
- `tests/hdcd_true_peak_composition_ffmpeg_smoke.py`

The archive root includes `SOURCE_DELTA_HDCD_TRUEPEAK.patch`, `SHA256SUMS_HDCD_TRUEPEAK.txt`, this handoff, verification results, and an interruption-safe resume checkpoint.

## Consciously unqualified routes

* HDCD-derived protected SSRC TruePeak prepass/replay is still refused: the existing commissioned strong-SSRC ingress/terminal contracts do **not** attest the newly decoded-HDCD entry path. No DSD/Reference/SSRC certificates or frozen hashes were altered.
* An HDCD + CD de-emphasis **before a rate change** is not yet admitted by the conservative certified pre-resample execution contract; same-rate HDCD + de-emphasis is available. Other arbitrary pre-resample filters without an existing preservation contract are refused. A backend-choice-specific rate-change refusal remains possible when the selected backend is not the admitted FFmpeg/Float64 route.
* Some target codecs or requested frequencies may be unqualified under the existing terminal proof policy; the correct behavior is a specific refusal, not a fallback transform.
* The FFmpeg witness uses an independently oversampled approximation for scalar checks; it is NOT the Rust certified true-peak implementation, and cannot authorize a claimed decoded-lossy-output true-peak ceiling.

## Next mandatory work on a capable build host

1. Run `cargo fmt --all -- --check`. If formatting changes are necessary, run `cargo fmt --all`; keep the resulting changes in the final patch. This workspace had no rustfmt.
2. Run `cargo test -p tonepoet-pipeline hdcd_certified -- --nocapture`, `cargo test hdcd_true_peak -- --nocapture`, and the existing `hdcd`, `pcm_true_peak`, `registered_effect`, CUE and dither test families. Fix **genuine** compiler and test defects and retain the smallest source delta.
3. Run all applicable workspace/build gates (`cargo test --workspace` and repository-specific CI/qualification). Use the repository's pinned Reference environment for Reference gates; do not infer certification from a different FFmpeg/SoX executable.
4. Execute real end-to-end TonePoet convert jobs with HDCD Track/Album Guard/Normalize, lossless and lossy, CUE member mixture, de-emphasis, 88.2-kHz and Int16, and check command records and observed bits. The smoke test exercises FFmpeg steps separately; it does not invoke the Rust job pipeline.
5. Audit any integration failure rather than removing contract checks. When all admission and output checks pass, update the handoff, checkpoint, and SHA inventory and regenerate the complete archive.

The prior snapshot's unrelated source, tests, frozen artifacts and qualification certificates are preserved. No speculative change to DSD, Reference, or SSRC production DSP was made.
