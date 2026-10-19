# TonePoet — PCM gain / DC three-findings corrective R3

**Date:** 2026-10-19  
**Input archive:** `TonePoet_LOGGING_pcm_gain_dc_2026-10-18.tar.gz`  
**Input archive SHA-256:** `6d0385b68f1ed6bb3dfb8bce2c8fd4e1915fe7683d9b49603c5a1b1fa676cb5e`  
**Brief:** `BRIEF_pcm_gain_dc_three_findings_2026-10-18.md`  
**Status:** Corrected **source candidate** for build-host qualification, **not** a release-certified build. Rust compilation and the full suite have **not** run here.

## Scope and decisions

Only **three existing Rust source files** differ from the supplied archive:

- `tonepoet-pipeline/src/semantic_plan.rs`: strengthen the stereo-fold/legacy regression without asserting an unverified Auto-backend preference. The new raw-layout condition requires **both** an active true-peak policy and a DC native effect **and** more than two channels in the *terminal input state*. For no DC, a legacy 2-Hz high-pass, or stereo fold before native DC, this condition is false. Seeing FFmpeg in one of those cases is therefore **not evidence of multichannel-constraint leakage**: the registry and candidate-proof selection may legitimately choose FFmpeg. The new test checks that **no terminal candidate** receives the layout obligation in those cases, retains terminal proof, and confirms an explicitly preferred SoX terminal remains admitted after the stereo fold. The production planner predicate and selection logic are unchanged.
- `src/tui/convert_screen.rs`: register the PCM gain information glyph as the normal suffix **after all four pills**, with a two-cell mouse hitbox covering the icon and guard cell. Register and render from the same responsive label table. Update mouse regression with the pill-to-icon boundary and several compact/full-width transitions. Repair the DC notice test: at 96×50, show the ending directly when the text fits (or scroll to it if help text is later extended); at 96×24, test a genuinely scrollable notice by moving to the bottom and checking the final paragraph.
- `src/tui/draw_output.rs`: remove the special-case splice that rewrote the PCM gain label. Use `pill_row(..., "PCM gain   ", "ⓘ", ...)` exactly as other info controls do. To retain all four interactive pills and a readable/help-accessible suffix on narrow screens, abbreviate only pill **display** labels progressively (full at width ≥84, intermediate at ≥64/≥52, and `off`/`G`/`N`/`F` at the narrowest widths). The underlying `PcmGainMode` and four selectable values are unchanged. Preserve common selected/focused/disabled styles, and test foreground-only contrast and the two-cell Unicode-width guard.

**One additive validation file**: `validation/pcm_gain_dc_20261019/static_review.py` performs independent geometry assertions, source wiring checks and scope checks. This is not a substitute for compilation or a live rendered UI.

## Checks executed in the current offline environment

- `python3 validation/pcm_gain_dc_20261019/static_review.py`: **16/16 PASS** (source and derived geometry, including every width 42–199).
- `python3 validation/dc_shift_r2c/static_regression_audit.py`: **13/13 PASS** (pre-existing typed-DC source consistency).
- `python3 validation/dc_shift_r2c/ffmpeg_speaker_reproducer.py`: **3/3 PASS** (actual host FFmpeg verifies 3.0, quad, 5-channel `0x3b` masks and bitwise-identical payloads; does not qualify pinned FFmpeg).
- Complete bytewise comparison against the input archive confirms **only the three listed existing Rust files changed**. The repository's remaining pre-existing files are preserved.

Not available here: `nix`, `rustc`, `cargo`, `rustfmt`, Cargo dependency/build closure, Rust regression execution, real terminal round-trips, pinned FFmpeg 8, or production end-to-end conversions. **Do not describe any of these as passed.**

## Reference source-lock gate: intentionally **not** requalified

The supplied tree already has a stale `tonepoet-pipeline/qualification/dsd_reference_common_v18_source_lock.json` relative to the changed locked sources. The `semantic_plan.rs` test edit changes its bytes again. **Do not update that sidecar by copying SHA-256 values or disable the freshness gate.** The existing release qualification must be rerun with the real reference tools, and all four generated report/certification/evidence/source_lock records must be regenerated together. Until then, the locked-source-freshness test is expected to fail and Reference production promotion remains inactive (fail-closed). This is a deliberate **remaining release blocker**, not a fixed issue.

## Build-host checklist (exact follow-up)

From the archive root, run `sha256sum -c BUNDLE_SHA256SUMS.txt` before entering `tonepoet/` (the delivery manifest is one directory above the source tree). From `tonepoet/`, using the pinned development environment in `CLAUDE.md`:

```sh
nix develop --extra-experimental-features 'nix-command flakes'
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo test -p tonepoet-pipeline multichannel_dc_constraint_does_not_perturb_stereo_fold_or_legacy_paths -- --nocapture
cargo test -p tonepoet dc_info_hitbox_four_pills_and_readable_notice_at_terminal_widths -- --nocapture
cargo test -p tonepoet pcm_gain_info_is_visible_clickable_and_does_not_overlap_pills_at_narrow_widths -- --nocapture
cargo test -p tonepoet pcm_gain_info_follows_all_four_pills_with_foreground_only_unicode_guard -- --nocapture
cargo test --workspace --no-fail-fast
python3 validation/pcm_gain_dc_20261019/static_review.py
python3 validation/dc_shift_r2c/static_regression_audit.py
python3 validation/dc_shift_r2c/ffmpeg_speaker_reproducer.py
```

In addition to the named regressions, manually confirm the Format pane and mouse map at widths 44, 51, 52, 63, 64, 83, 84, and 120, for every PCM gain mode. Verify exact sample route/identity for native DC before/after fold with actual binary64 carriers; under Auto and explicit SoX/FFmpeg choices check that **only** genuinely multichannel native DC at certified terminals requires the verified layout. If the explicit SoX post-fold regression fails on the pinned build, inspect the **specific admission mismatch** before changing either the test or the planner; never drop a real proof obligation to force an expected backend. Check the originally reported 7572-pass/3-fail gate afresh.

Then run genuine `dsd_reference_qualification` with the pinned SoX-ng, FFmpeg and required tools as described by `tests/reference_qualification_freshness.rs`; copy the generated four v18 records into `tonepoet-pipeline/qualification/` as a consistent set; rerun all gates and produce a new source-lock identity. Do not promote Reference from static review alone.

## Resume / interrupted-session contract

**Use this source tree as the R3 baseline.** Do not replay the earlier PCM_GAIN_INFO_R2 patch or older DC correction scripts: they are historical artifacts. The current source changes are deliberately limited to three Rust files, and the current review was non-compiling. If a build host reports additional Rust failures, fix only demonstrable regressions and rerun the named and workspace gates. Record changes in a new, versioned handoff; preserve the old input archive SHA and this R3 chain of custody. An optional portable code delta accompanies the full bundle as `PCM_GAIN_DC_R3_CODE_DELTA_2026-10-19.patch`.

**Delivery verification:** `BUNDLE_SHA256SUMS.txt` lists the complete post-correction source file set and supporting handoff (excluding the manifest itself); verify it before use. Old `R*SHA256SUMS*` files inside the source tree belong to previous deliveries and may legitimately be stale.
