# TonePoet PCM Gain Information Control — R1 Engineering Handoff

**Date:** 2026-10-18  
**Authority:** User's *TonePoet: PCM Gain Information Popup* implementation request  
**Starting source:** `TonePoet_DC_Shift_R2D_FULL_SOURCE_2026-10-18.tar.gz` (SHA-256 `a270326a2811fbdefb047769203eb04d70ee98079595b65debeb6351c02eb172`)

## Delivered behavior

- The existing Convert/Format **PCM gain** row retains all four original clickable pills at their original x offsets and their prior mode behavior.
- A literal **ⓘ (U+24D8)** is visible beside the PCM gain label (before the pills), styled with the same foreground-only, bold contrasting color and adjacent same-style guard cell as the project's other glyphs.
- The two-cell glyph hitbox is registered at the displayed x coordinate, independent of all gain-pill targets, including narrow terminal widths. Placing the glyph *after* all four pills would make the help undiscoverable at common 80-column widths; using the unused label padding avoids moving any existing controls.
- Mouse activation opens the existing scrollable `ActiveOverlay::Notice` titled **PCM gain**. The existing **i** keyboard shortcut opens it when the PCM gain row is focused; Esc/Enter, scrolling, and overlay footer handling are inherited unchanged.
- Four plain-text, standalone section headings and paragraph breaks preserve the complete user guidance without introducing Markdown rendering or a new popup/styling framework.

## Source changes

Exactly four pre-existing Rust files differ from R2D:

1. `src/tui/button_map.rs`: adds `TuiButton::PcmGainInfo`, including its classification in the existing exhaustive button match.
2. `src/tui/draw_output.rs`: adds `pcm_gain_info_row`, which replaces a 16-cell label span with 12+2+2 cells, and adds a focused glyph-style regression test.
3. `src/tui/convert_screen.rs`: binds the rendered two-cell glyph hitbox, and adds ratatui `TestBackend` regressions for multiple widths/modes, actual mouse activation, keyboard activation, dismissal, rendering and scrolling.
4. `src/tui/keybindings.rs`: adds the content and keyboard/mouse routing to the existing Notice (no new modal state).

No PCM gain policies, audio-processing stages, true-peak implementations, planners, codecs, DSP, presets, or unrelated UI code were changed. `PCM_GAIN_INFO_R1_FROM_R2D.patch` captures the entire code change for review.

## Copy review against source

- **Guard:** `SampleGainPolicy::TruePeakGuard` does not permit boosting; `resolve_true_peak_gain_constraints` caps otherwise positive gain at unity (`tonepoet-pipeline/src/dsd_album_gain.rs`). It applies one scalar, not a dynamic limiter.
- **Normalize:** `SampleGainPolicy::TruePeakNormalize` permits either positive or negative gain, subject to the certified ceiling. The copy says *approaches* rather than *exactly reaches*, as a conservative upper bound and terminal error/headroom can leave the realized peak below the selected ceiling.
- **Automatic Guard:** `FormatState::apply_auto_gain_defaults()` in `src/tui/app.rs` selects PCM Guard for known floating-point to integer-PCM outputs or known lossless to lossy outputs, only while the mode is not user/preset overridden, not for DSD sources, and with the existing actionable HDCD lossy-decode exception. The source copy qualifies the original unconditional sentence accordingly.
- **Lossy target ceiling:** `effective_true_peak_target()` in `tonepoet-pipeline/src/settings.rs` caps higher requested lossy encoder-input ceilings at **−0.10 dBTP** (not a guarantee about post-decode lossy output). The copy explicitly distinguishes this.
- **Fixed gain:** `SampleGainPolicy::FixedGain` is not a certified true-peak policy and does not automatically add a limiter. The copy retains the clipping warning.

No gain algorithm or automatic policy was altered to make the copy true.

## Executed verification (this environment)

| Command | Actual result |
|---|---|
| `python validation/pcm_gain_info_r1/static_audit.py` | **15/15 PASS** (static source, copy and coordinate invariants only) |
| `python validation/dc_shift_r2c/static_regression_audit.py` | **13/13 PASS** (existing source assertions, not Rust) |
| `python validation/channel_dc_smoke.py` | **13 PASS** (existing Python/FFmpeg audio fixtures) |
| `python validation/channel_layout_binding_corrective.py` | **14 PASS** (existing decoded speaker-layout fixtures) |
| `python validation/dvda_stereo_fold_corrective.py` | **6 PASS** (existing DVD-A decoded-sample fixtures) |
| Diff audit versus R2D | Exactly **4 Rust UI files** modified; none removed |

The corresponding executed logs are in `validation/pcm_gain_info_r1/`.

**Not executed:** Rust compilation, `cargo fmt`, clippy, three new ratatui Rust tests, full workspace regressions, release build, and live interactive terminal rendering. This environment does not have the pinned Nix/Rust toolchain. The source audits are not substitutes for those gates. The result is a source-complete **implementation candidate**, not a build-qualified release.

## Build-host qualification (required)

In the repository root, with the pinned Nix environment installed:

```bash
nix develop --extra-experimental-features 'nix-command flakes'
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo test -p tonepoet pcm_gain_info_regressions -- --nocapture
cargo test -p tonepoet pcm_gain_info_glyph_tests -- --nocapture
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --no-fail-fast --exclude tonepoet-true-peak
cargo test -p tonepoet-true-peak
cargo build --release
```

The two new `pcm_gain_info_regressions` tests exercise ratatui `TestBackend`, not merely text assertions; the glyph test checks every shipped palette. Run them on the actual build host and correct **only demonstrated** compilation or rendering failures. Verify at widths 44, 64, 80, 100, and 120, all four PCM gain selections, two-cell glyph clicks, keyboard **i**, the long Notice's first/last visible paragraphs, Esc/Enter, and absence of gain or preset mutation. The original R2D Rust qualification also remains open.

## Integrity and interruption

The complete bundle includes `PCM_GAIN_INFO_R1_SHA256SUMS.txt` (SHA-256 for every delivered regular source file except the manifest itself) and this report. `PCM_GAIN_INFO_R1_RESUME_CHECKPOINT.md` records the state and the next verification steps. Compare against R2D if necessary; do not reimplement the feature from scratch.
