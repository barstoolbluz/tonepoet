# R19 HDCD Decode — source delivery and qualification status

Date: 2026-10-14. Governing documents: `BRIEF_R19_convert_hdcd_decode_planning_FINAL_R2_2026-10-03.md` and `ADDENDUM_HDCD_BRIEF_current_tree_2026-10-13.md`.

**Status: implementation delivered for build-host validation; NOT yet compile-tested, full-gate verified, Reference-qualified, or release-approved.** The supplied execution environment has FFmpeg 7.1.5 but no Cargo, rustc, rustfmt or Nix. R37's reported 7,459-pass gate is *upstream evidence*, not a result for these changes. The governing addendum specifies FFmpeg 7.1.3 on the build host; the physical smoke below used 7.1.5.

## Scope and behavior

- **Eligibility:** Known-lossless, integer (not float), PCM, exactly 16-bit/44,100 Hz, and one or two channels. Unknown/lossy/24-bit/48-kHz/multichannel/DSD sources do not launch an automatic decoder scan. CUE requires a recognized single-image layout and readable image with authoritative track sample boundaries. Unsupported early-source families remain unscanned.
- **Detection:** The existing whole-file FFmpeg `hdcd` detector is reused without a one-second cut-off. Typed facts include packet detection, Peak Extend (PE), nonzero Low-Level Gain Adjustment (LLE), Transient Filter (TF), and maximum gain adjustment; detection text never authorizes execution. PE/LLE is actionable. TF-only and passive HDCD signal presence above the fold, but decoding is Off/unavailable because current FFmpeg does not reconstruct TF and passive filter insertion would still change sample representation.
- **Format and defaults:** One `FormatState::pane_rows` authority supplies the HDCD row for display and input registration; an undetected eligible source sees Off below the fold when maximized. Detected streams promote the row with shared notice affordance. Actionable lossless integer 16/44.1 preservation stays Off; leaving that domain defaults On, including lossy outputs. Active decoding automatically widens a source-derived lossless 16-bit target to Int24. Explicit Int16 is refused instead of silently truncating the logical 20-bit decoded signal. Manual On/Off is sticky. The stronger potential-control-stream-loss warning is reserved for sample-changing conversions; pure wider-PCM 44.1 uses informational wording.
- **Batch/cache:** Scanning is sequential and off the UI thread. Every eligible ordinary file is probed and classified independently; valid structured cache hits use whole-file nanosecond-resolution modified time and byte count. The reducer checks generation, ordered source list, and identity before admitting results. The single-image CUE path scans sample-exact track boundaries independently and binds effectual source ordinals, so neighboring non-HDCD tracks are not decoded. CUE segment classifications are recomputed per selected source session rather than stored in a new database/cache table.
- **Source-first physical execution:** The typed planner reserves `SourceBitstreamDecode` as the first phase, ahead of CD de-emphasis, selected resampler, gain, dither, and terminal. FFmpeg runs `hdcd=bits_per_sample=16:disable_autoconvert=1` directly on the original 16-bit PCM (CUE: an `atrim` of exact source samples before HDCD), emits 44.1-kHz `pcm_s32le` WAV, and validates format, channels, rate and source identity before/after reading. That S32 carrier enters the existing Float64 effect/resampler/terminal machinery. No `-ar`, `-ac`, `-force_pe`, new HDCD backend, or hidden rate change is inserted into the HDCD stage.
- **Planner:** HDCD + CD de-emphasis is ordered correctly. The registered effect bridge preserves the existing FFmpeg/SoXR, SoX-ng, or SSRC resampling and terminal ownership choices, including the SSRC split/package-only variants. HDCD + certified true-peak Guard/Normalize fails closed: there is no qualification for observing the protected waveform after this decoder. The selected effect, source identity, track-scoped CUE authorization and ordering participate in execution fingerprinting. Passive/no-effect state does not install an HDCD effect.
- **Presets:** Save only explicit On/Off overrides, never HDCD source scan facts. Do not accidentally serialize an HDCD-derived Int24 promotion as a user-selected preset bit depth; source replacement reverts that automatic widening.
- **Excluded:** FFmpeg algorithm changes, TF reconstruction, SoX-ng HDCD decoding, HDCD re-encoding, new control-code metadata, general CUE extraction architecture, broad true-peak requalification.

## Verification actually performed

- `git diff --check`: **pass** (no whitespace errors).
- `python -m py_compile tests/hdcd_source_first_ffmpeg_smoke.py`: **pass**.
- `python tests/hdcd_source_first_ffmpeg_smoke.py`: **pass** on local FFmpeg 7.1.5. Encoded a synthetic 16/44.1 stereo WAV into FLAC, decoded an unaligned CUE segment using source-first `hdcd` to S32 PCM, and byte-compared the result against a non-seek decode of the identical exact sample range. Both yielded **44,159 exact stereo frames, 44,100 Hz, signed PCM S32**.
- In the prior reconstructed checkpoint, generated an HDCD packet after one second and observed FFmpeg detect PE with a full-file scan; this supports detector behavior but is not an executed Rust test on the final source.
- Added focused Rust tests (not executed here): HDCD classification/late packets, source eligibility, identity-safe structured cache, Format display/default/manual/source-reset, preset provenance, typed planner ordering/resampler/refusal, and CUE per-ordinal command/selection checks.

## Required build-host acceptance (not run here)

1. From this precise extracted tree with the pinned toolchain: `cargo test --locked -p tonepoet-pipeline hdcd` and `cargo test --locked --lib hdcd`; run the general relevant registered-effect, Convert, CUE, and database tests. Fix any Rust compilation or assertion defect before acceptance.
2. Run `python tests/hdcd_source_first_ffmpeg_smoke.py` with the *pinned FFmpeg 7.1.3*, then perform real-file Convert tests for lossless Int24 at 44.1, SSRC FLAC Int24/88.2, SoX-ng and FFmpeg/SoXR rate changes, mixed ordinary files, single-image CUE mixed PE/non-PE, both corrections, error cleanup, presets, and source replacements.
3. `cargo test --workspace --locked` and `cargo build --workspace --locked` on a capable build host. Do not overrun the memory-limited validator by attempting a full root build here.
4. Requalify the Reference source closure using `./scripts/requalify_reference_r18.sh /absolute/path/to/real-sacd.iso` in the configured/reference-equipped build environment, with `TONEPOET_DSD_REFERENCE_UNQUALIFIED_OVERRIDE` unset. Verify the freshness tests, a positive SACD delivery and an isolated stale-lock negative refusal before release. Do not hand-edit the installed certificate/lock hashes.

## Locked-source changes and remaining risk

Modified Reference-locked files: `tonepoet-pipeline/src/semantic_plan.rs`, `tonepoet-pipeline/src/fingerprint.rs`, `src/convert/pipeline/stages.rs`. The other three highlighted by the addendum (`tonepoet-pipeline/src/plan.rs`, `tonepoet-pipeline/src/plugins.rs`, `src/convert/pipeline/plan_bridge.rs`) are unchanged. The installed R37 qualification therefore **cannot bind this modified source**. No corresponding requalification sidecars were produced or claimed.

The principal remaining uncertainty is **compiled end-to-end behavior and complete regression testing**, not a known request to expand the design. The FFmpeg smoke validates a key carrier boundary, not the TUI, planner binding, SSRC ownership, CUE runner, or release qualification. Do not describe the delivered source as release-ready until the above gates pass.

See `HANDOFF_TO_NEXT_SESSION.md` for checkpoint commits and reproduction steps. Source hashes are in `R19_SOURCE_SHA256SUMS.txt`; the delivered archive is separately hashed in `SHA256SUMS` alongside the download.

## Continuation / corrective audit (2026-10-14)

This complete-tree corrective snapshot supersedes the R19 source snapshot above for handoff. Its local Git history preserves both the original R37 baseline and the intermediate R19 implementation.

**Source corrections (not compiled here):**

1. `src/convert/pipeline/execution_evidence.rs` now explicitly maps `RegisteredUnaryEffect::HdcdDecode` to the `hdcd_decode` execution receipt. The old exhaustive match lacked this newly introduced variant, a compilation blocker. A focused Rust receipt test was added (not executed).
2. `src/tui/presets.rs` now distinguishes a source-derived Int24 widening from an explicit user depth selection when serializing/restoring version-5 TUI presets. `hdcd_auto_depth` is optional/omitted when absent for compatibility with older presets. A JSON round-trip and user-override regression test was added (not executed).
3. `src/db.rs` rejects an HDCD cache write when either scan-time nanosecond modification timestamp **or byte length** differs from the current file identity. Its regression fixture includes same-second replacement and stale-write refusal (not executed).

**FFmpeg detector finding corrected:** An early manual reproducer omitted `-y` and therefore caused FFmpeg to skip writing to `/dev/null`. Inspection proved that TonePoet's existing `hdcd_ffmpeg_args()` already passes `-y`; there was **no corresponding defect in the delivered detector**. The detector was not changed on the basis of that invalid reproducer.

**Physical regression checks completed on this host (FFmpeg 7.1.5):** `python3 tests/hdcd_source_first_ffmpeg_smoke.py` passed. The extended test uses the detector's actual argument shape (including `-y`) on a deliberately late HDCD Peak Extend packet: a one-second scan reports no HDCD, a whole-file scan detects it. It also encodes the packet-bearing source to FLAC and scans a sample-bounded two-member CUE partition; only the containing member reports effectual HDCD. The original independent 44,159-frame, 44.1-kHz stereo signed-32-bit comparison between coarse-seek and exact-trim FLAC source-first conversions still passes. `python3 -m py_compile tests/hdcd_source_first_ffmpeg_smoke.py` and `git diff --check` passed as well.

**Unchanged blockers:** No Rust compiler/Cargo/Nix was available in this container; Rust tests and workspace build were NOT run. The local FFmpeg is 7.1.5, not pinned 7.1.3. The changed source-locked files remain unqualified against the installed R37 Reference certificate. Do not treat the static corrections or FFmpeg smoke as release acceptance. Perform the original Required build-host acceptance above against the corrected bytes.

## R3 corrective patch — completed scan and structured analysis-cache reuse (2026-10-14)

**Supersedes R2 for source handoff. Remains uncompiled and unqualified.** The two scoped defects from the follow-up review were confirmed and corrected without changing the HDCD decoder, CUE track semantics, planner, resampler, terminal, or the three already-modified Reference-locked files.

1. `src/tui/app.rs`: `ensure_convert_hdcd_scan` now returns immediately for a matching source generation, whether pending or completed. A completed batch no longer clones all paths or invokes `identity_current`/filesystem metadata reads on each event-loop iteration. The generation changes on source reload; source replacement clears the prior scan state. Identity verification is still performed once at scan-result admission, at commit, and before/after physical decoding.
2. `src/db.rs`: `store_analysis` copies `hdcd_capabilities` and `hdcd_mtime_ns` from the previous cache row only when file path, whole-second modified time, byte length, and algorithm version agree. Its original full-analysis `INSERT OR REPLACE` semantics remain intact. A changed version or coarse identity clears both fields; the structured reader still requires current nanosecond identity.
3. `src/tui/event_loop.rs`: the Details completion handler now invokes a shared narrow persistence function. It writes **freshly scanned** typed HDCD capabilities using the original Details target's `SystemTime` and byte count, through `store_cached_hdcd_capabilities` and its physical-identity check. It then persists the merged narrow metadata facts, preserving human-readable HDCD details and pre-emphasis facts. Previously merged facts alone never authorize a new structured HDCD write.

**New Rust regressions (source added; not executed on this host):** completed 512-member inventory remains idle over 100 ticks; same-path generation reload and source replacement initiate new scans; a mutated source fails the unchanged commit identity predicate; full analysis retains matching typed facts and invalidates version/identity changes; Details completion populates the Convert cache and rejects stale identities.

**Checks actually executed here:** `git diff --check` passed; `python3 tests/verify_hdcd_analysis_cache_sql.py` passed using the exact `store_analysis` SQL extracted from the edited Rust source and an in-memory SQLite database; `python3 tests/hdcd_source_first_ffmpeg_smoke.py` passed on installed FFmpeg 7.1.5 (late HDCD packet, independent CUE classification, 44,159 signed-32 stereo frames at 44.1 kHz). The SQL check is an isolated statement-level test, **not** a compiled or end-to-end Rust database test.

**Outstanding build-host gates (unchanged):** Run the added Rust unit tests and focused existing HDCD suite, the full workspace build/test, real Convert paths with SSRC/SoX-ng/FFmpeg, the FFmpeg physical smoke on pinned 7.1.3, and Reference requalification on the actual modified source closure. Do not accept this archive as a release until those gates pass. There is no evidence from these two fixes that warrants a decoder or processing-architecture rewrite.
