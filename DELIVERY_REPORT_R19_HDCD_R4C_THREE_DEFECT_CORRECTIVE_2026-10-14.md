# TonePoet R19 HDCD R4C - three-defect corrective delivery

Date: 2026-10-14 America/New_York (2026-10-15 UTC).

## Authority and status

- Authoritative input: complete `TonePoet_R19_HDCD_R4_POLICY_CORRECTIVE_2026-10-14.tar.gz`, SHA-256 `f934758022830a9cfdd13e4d604cc6cfdbbf30ece2783045e8b157fbdf4da06f`.
- Governing policy: `BRIEF_R19_HDCD_R4_defaults_depth_dither_lossy_2026-10-14.md`, plus the user's three-defect validation.
- Result: narrow source correction and new Rust regression tests DELIVERED. Compiled build-host acceptance and Reference requalification OPEN.
- Exactly three existing Rust files changed. No new scheduler, scanner, dither controller, database migration or runtime dependency.

## Corrections

1. **User Int16 selection preserves Decode On.** `src/tui/app.rs` applies the existing manual HDCD override when an actionable, already-enabled HDCD conversion is explicitly switched to 16-bit lossless, from either the `select_bit_depth` API or a selected bit-depth pill routed through `after_user_selection`. This is not the Decode-On handler: it does not widen the newly chosen Int16 back to Int24. Initial automatic defaults, format transitions without an explicit bit-depth pick, manual Decode Off and source replacement keep their existing behavior. Automatic TPDF follows the existing dither machinery; an explicit dither Off is not overwritten.

2. **Automatic HDCD TPDF is member-local, not batch policy.** `src/tui/convert_actions.rs` now projects the ordinary representative-source automatic dither into shared settings when the visible TPDF selection was derived from HDCD. That keeps an ordinary 24->16 member on automatic Shibata, without modifying explicit batch-wide dither choices. `src/convert/pipeline/plan_bridge.rs` extends the existing CD de-emphasis Float64 terminal bridge to restore non-explicit TPDF for the concrete HDCD-effect member, restricting CUE authorization by source ordinal and excluding already-quantized SSRC terminal carriers and hybrid WavPack.

3. **Manual AAC terminal dither choice survives disabled UI controls.** `src/tui/convert_actions.rs` retains explicit dither type/provenance, including Off, for the PCM-to-libfdk AAC signed-16-bit terminal. The existing FFmpeg/SoXR plugin then lowers explicit Off to `dither_method=none`, implicit default to `triangular`, and supported explicit methods to their existing mappings. Float-input MP3/Opus and DSD controls continue projecting dither inactive.

The R4 SQL migration correction, source-first HDCD/CUE execution, stage/encoder implementation and registered-effect ordering were left untouched.

## Exact source delta

- `src/tui/app.rs`
- `src/tui/convert_actions.rs`
- `src/convert/pipeline/plan_bridge.rs`

The included `R19_HDCD_R4C_SOURCE_DELTA.patch` applies to the authoritative R4 source snapshot. This archive includes the FULL source tree, not just a patch. The historical `R19_HDCD_R4_PACKAGE_SHA256SUMS.txt` is deliberately retained unchanged for R4 provenance and will identify exactly three modified files as mismatches. Use `R19_HDCD_R4C_PACKAGE_SHA256SUMS.txt` for current R4C integrity.

## Rust regression tests added or updated (written; NOT run here)

- `r4c_explicit_int16_after_automatic_hdcd_on_keeps_decode_on_in_both_entry_points` - auto Int24 HDCD On -> explicit Int16 by direct and pill paths; automatic TPDF; manual dither Off; unrelated format and source replacement controls.
- `r4c_hdcd_mixed_batch_auto_tpdf_is_not_projected_to_ordinary_members` - mixed effectual HDCD plus 24-bit PCM; visible HDCD TPDF but common automatic Shibata; explicit dither remains globally authoritative.
- `r4c_aac_preserves_manual_dither_off_and_terminal_ffmpeg_lowering` - lossless Int16 + HDCD + dither Off -> AAC; confirms projected provenance and explicit none, explicit TPDF, and implicit default FFmpeg dither methods against the existing planner.
- `r4c_hdcd_float64_carrier_reasserts_one_terminal_tpdf` - final Int16/Source terminal contains one SoX TPDF and retains non-explicit provenance.
- `r4c_hdcd_cue_dither_bridge_is_effectual_member_only` - only authorized CUE ordinal inherits HDCD TPDF.
- `r4c_hdcd_carrier_dither_respects_explicit_off_and_terminal_ownership` - manual Off, Float64Wav bridge, already-quantized SSRC carrier, hybrid WavPack.
- One prior lossy-dither-inactive control was changed from AAC to MP3 because the former assertion contradicted the now-authoritative AAC terminal requirement.

## Checks actually executed (compile-free)

- PASS: baseline R4 archive extraction and 988-file SHA-256 inventory before patching.
- PASS: source diff applies with `git apply --check --whitespace=error` to exact baseline; all three changed files reproduced byte-for-byte in a separate checkout.
- PASS: `python3 tests/verify_hdcd_analysis_cache_sql.py`.
- PASS: `python3 tests/verify_hdcd_v28_migration_sql.py`, 14 modeled legacy/replay SQL cases.
- PASS: `python3 tests/hdcd_source_first_ffmpeg_smoke.py`, real FFmpeg FLAC/CUE sample-bounded HDCD decoding.
- PASS: `python3 tests/hdcd_lossy_precision_ffmpeg_smoke.py`, Float-input Opus/MP3 and terminal TPDF/Int16.
- PASS: Python probe syntax (`python3 -m py_compile`).

The new Rust test bodies were source-reviewed, not executed. Do not claim compilation, UI runtime validation, libfdk AAC execution, full workspace gate or Reference qualification. `cargo`, `rustc`, `rustfmt` and `nix` are absent here; system FFmpeg lacks `libfdk_aac`. See `R19_HDCD_R4C_COMPILE_FREE_TEST_LOG.txt` for actual commands and results.

## Required acceptance on the pinned build host

1. Enter the repository's `nix develop` environment (Rust >=1.93; pinned FFmpeg/libfdk AAC).
2. `cargo check --workspace --all-targets`.
3. `cargo test r4c_ -- --nocapture`, then `cargo test hdcd_ -- --nocapture`, and run related Format, dither, registered-effect, AAC and migration focused tests.
4. Execute the documented complete routine gate: `cargo test --workspace --no-fail-fast --exclude tonepoet-true-peak`; separately `cargo test -p tonepoet-true-peak` as appropriate for the host. Run the release build and real per-track/CUE conversions with validated inputs.
5. Execute pinned `libfdk_aac` cases: automatic triangular, explicit Off (no dither), supported explicit dither method and unsupported method refusal. Confirm no double dither/quantization and unchanged float-input MP3/Opus and SSRC variants.
6. `src/convert/pipeline/plan_bridge.rs` is Reference source-locked. Execute the repository's established `scripts/requalify_reference_r20.sh` procedure on an authorized host with its fixture, then check freshness and Reference tests; DO NOT hand-edit source-lock JSON or attest unrun gates.
7. Only after those gates pass should this snapshot be designated release-qualified. Any failures should be addressed locally; no broad rewrite is implied.

## Why this is proportional

The modifications use only existing UI override state, existing ordinary auto-dither derivation and the established Float64 effect terminal bridge. No runtime overhead was added for ordinary files other than in-memory predicates at planning boundaries; no new materialization stage was introduced. The unresolved build-host gates are verification obligations, not evidence that extra infrastructure is needed.
