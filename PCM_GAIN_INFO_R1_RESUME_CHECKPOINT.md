# PCM Gain Information R1 — Persistent Resume Checkpoint

Date: 2026-10-18.  Status: SOURCE IMPLEMENTED, BUILD QUALIFICATION OPEN.

## Baseline

R2D source: `TonePoet_DC_Shift_R2D_FULL_SOURCE_2026-10-18.tar.gz`, verified SHA-256 `a270326a2811fbdefb047769203eb04d70ee98079595b65debeb6351c02eb172`.

## Implementation complete

- 4 touched Rust UI files only: `src/tui/button_map.rs`, `src/tui/draw_output.rs`, `src/tui/convert_screen.rs`, `src/tui/keybindings.rs`.
- Literal two-cell-guarded `ⓘ` at x=13/14 beside PCM gain label; all original pills still start at x=17 and have original widths.
- Mouse and focused-row keyboard **i** both open existing scrollable Notice. Esc/Enter close through established input dispatch.
- Four full mode explanations as plain-text section headings with `Choose this` guidance; automatic Guard claim qualified to match actual policy and lossy -0.10 dBTP ceiling clarified. No algorithm changes.
- `PCM_GAIN_INFO_R1_FROM_R2D.patch` captures code-only diff. Three focused new Rust tests exist but **have not been executed**.

## Executed checks

Source/geometry invariant audit 15/15 PASS; R2C static audit 13/13 PASS; audio smoke 13 PASS; channel-layout 14 PASS; DVD-A regression 6 PASS. Read `validation/pcm_gain_info_r1/` logs. Source content comparison against R2D confirms no non-UI Rust source changes.

## Remaining required work

Pinned `nix develop` shell, `cargo fmt --all -- --check`, `cargo check --workspace --all-targets`, focused `cargo test -p tonepoet pcm_gain_info_regressions` and `pcm_gain_info_glyph_tests`, workspace clippy, complete test gate, and release build. Resolve only actual errors; no gain-planner or DSP change requested. Review exact terminal behavior and popup scroll/dismissal on narrow screens. Toolchain absent on producing host.

See `PCM_GAIN_INFO_R1_ENGINEERING_HANDOFF.md` for source-verified copy details and exact commands. Verify `PCM_GAIN_INFO_R1_SHA256SUMS.txt` after extraction and the outer archive's `.sha256` sidecar.
