# TonePoet HDCD / CUE / true-peak R2 -- focused corrective handoff

**Date:** 2026-10-17 UTC (2026-10-16 US Eastern)
**Basis:** `TonePoet_HDCD_TRUEPEAK_R1_UNQUALIFIED_FULL_REPO_2026-10-16.tar.gz`
**Status:** source correction and executable FFmpeg witnesses completed; **Rust build-host acceptance and Reference qualification NOT RUN**.

This is a complete, independent repository snapshot, not an overlay. Preserve the admitted R1 effect-planner model, registered-effect carriers, certified Track/Album gain system, dither ownership and protected SSRC restrictions. R2 changes only `src/convert/pipeline/stages.rs` and `src/convert/pipeline/track_executor.rs` in production. It adds one physical regression and adjusts two existing FFmpeg test expectations; it does not modify production DSP, certified coefficient sets, frozen identities or dependency versions.

## Four source corrections

1. **Effect-aware direct CUE admission.** `cue_stream_phase1_direct_plan_for_paths` specializes the request's registered effects for the current CUE ordinal before considering the ordinary `PlanRequest`. Any remaining registered effect or true-peak policy disables one-command direct streaming *for that member*, directing it to the existing per-track worker; members without effects preserve the direct MP3/other admitted path. Existing true-peak preparation barriers are not replaced. An invalid CUE authorization remains an explicit error.

2. **Continuous HDCD state across the CUE cut.** `hdcd_source_first_command` no longer seeks or applies `atrim` ahead of the HDCD filter. It reads original S16 44.1-kHz programme continuously and applies `atrim=start_sample=...:end_sample=...,asetpts=PTS-STARTPTS` **after** HDCD, retaining the original sample clock and control history at non-second-aligned boundaries. Overflow and zero length are refused. Non-HDCD ordinals never acquire this decode. No +6 dB gain compensation, premature resampling or quantization was introduced.

3. **No unused ordinary S32 carriers for authorized HDCD tracks.** The existing grouped-CUE ordinary fallback accepts the request and excludes only HDCD-authorized ordinals before probing/staging fallback WAVs. It batches ordinary members identically to R1. HDCD workers continue to read original encoded source and never touch ordinary S32 fallback carriers. No cache, new executor, or staging abstraction was added.

4. **Retain completed prepass evidence on failure.** PCM true-peak preparation writes CommandRecords and TrackExecutionEvidence into caller-owned buffers throughout execution, including after successful HDCD decode and Float64 WAV unwrapping. HDCD's semantic effect record, source/artifact refs and actual invocation index are recorded immediately on successful decoder completion; final success projection suppresses the duplicate semantic record. Track-scope failures retain the completed prelude on their existing failure-record path. Album-scope futures return partial preludes even on error/cancellation; a blocked album attaches per-member commands/evidence with discarded-attempt lineage before publishing its failure log, without publishing any partial audio album. Command indices remain member-local and correlate with the recorded command list.

## Focused regression coverage (new/modified)

* Rust: CUE direct admission with effectual member vs unaffected member; Track Guard and Normalize vs no-effect MP3; continuous-decoder command layout and invalid spans; grouped fallback output list excludes one HDCD ordinal while retaining two ordinary ordinals; Album blocked prepass attaches independent successful and partial decoder receipts without losing invocation indices.
* Physical FFmpeg: `tests/hdcd_cue_continuity_ffmpeg_smoke.py` writes actual recognizable Peak Extend packets before and after a sample-nonaligned CUE boundary. It compares **continuous decode, then exact cut** to the sample range of full continuous decode: 44,433 frames, **zero mismatches**. It also verifies the old cut-then-decode route differs at **390 samples**, making the test non-vacuous. A neighboring ordinary member is sample-preserving.
* The prior `hdcd_source_first_ffmpeg_smoke.py` and `hdcd_true_peak_composition_ffmpeg_smoke.py` now check the corrected decode-then-cut order, not two variants of cut-before-decode. Existing high-precision/MP3/Int16 and SQL tests remain runnable.

## Deliberate limits and engineering tradeoffs

* Correctly reconstructing a CUE member's state requires decoding from the beginning of its programme. R2 does so per authorized member. **A multi-member image is not yet shared as one physical HDCD decode**. Implementing a shared certified HDCD carrier and cross-member command/evidence identity would expand staging and provenance contracts; that was deliberately not invented without executable qualification. This is a potential throughput cost for many selected HDCD tracks, not a sample-correctness short-cut. Measure it on the real build host before undertaking a coordinated grouped optimization; do not reintroduce pre-decoder seeking/trimming.
* HDCD-derived protected SSRC true-peak replay and HDCD + source-rate CD de-emphasis before a rate-changing certified boundary remain specific R1 refusals. Do not force them through incompatible execution contracts.
* The physical regression uses FFmpeg and concrete sample comparisons. It does **not** exercise TonePoet's Rust command dispatcher, Album publication, log writer, or pinned Reference qualification; Rust test sources are present but could not be compiled in this runtime. No end-to-end acceptance is claimed.
* Failed command records are included when the existing `run_recorded_planned_command` obtains a ToolRunner error record. Preexisting cancellation, cleanup, failure policy and publication mechanisms remain in control.

## Build-host handoff -- required before production handoff

1. Verify the outer archive `.sha256`, extract, and run `sha256sum -c SHA256SUMS_HDCD_CUE_R2.txt` from the repository root. Historical R1 and prior SHA inventories are provenance only; use the R2 inventory for this snapshot.
2. `cargo fmt --all -- --check` (format source if needed without unrelated edits); `cargo check --workspace`; `cargo test hdcd_source_first_tests -- --nocapture`; `cargo test hdcd_true_peak_partial_prelude_tests -- --nocapture`; run `cargo test cue_stream_admission_is_per_member -- --nocapture` and `cargo test grouped_cue_fallback_never_stages -- --nocapture`. Run existing R1 `hdcd`, `hdcd_true_peak`, `pcm_true_peak`, CUE/SSRC/Reference, and full `cargo test --workspace` gates.
3. With actual HDCD CUE audio and TonePoet binary, assert MP3 with no other effects executes HDCD, Guard/Normalize Track and Album meter decoded waveforms, mixed ordinary members stay direct where eligible, discarded failed preparations still log actual commands, and no unused S32 carrier is produced for HDCD tracks. Exercise scratch-failure injection after S32 decode and after downstream Float64 conversion; inspect per-track invocation indices and album publication atomicity.
4. Measure decoder invocations, throughput and peak scratch usage for multi-member images vs the R1 grouped fallback baseline. Only add shared HDCD grouping if warranted and identity/provenance and qualified execution remain exact.
5. Execute pinned Nix/Reference requalification and all workspace-specific acceptance gates. If any fails, correct the real defect rather than bypass the proof or weakening tests; regenerate the archive, manifest and status documents with accurate receipts.

**Verdict:** R2 addresses the four source-level corrections and has passing independent physical regression evidence. **It is NOT Rust-build verified or production-qualified** in this environment.
