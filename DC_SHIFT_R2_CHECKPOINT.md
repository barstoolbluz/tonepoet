# DC Shift R2 persisted engineering checkpoint (2026-10-18)

State: **FULL SOURCE MODIFIED AND SAVED; BUILD/TEST QUALIFICATION PENDING.**

Original uploaded repository archive: `TonePoet_SNAPSHOT_2026-10-18.tar.gz`, SHA256 `ba4e5c6d7b73fd387886e9cde818fc630c019e89f778041952e7b8c2006120a0`.

Source tree is this archive's top-level `tonepoet/`. The original implementation brief is `DC_SHIFT_R2_IMPLEMENTATION_BRIEF.md`; handoff/verification is `DC_SHIFT_R2_ENGINEERING_HANDOFF.md`, change inventory is `DC_SHIFT_R2_SOURCE_CHANGE_INVENTORY.txt`, code-integrity digest list is `DC_SHIFT_R2_SHA256SUMS.txt`.

## Completed changes

- TUI four modes, Manual signed normalized value editor, literal U+24D8 two-cell info hitbox, original Notice dialog and keyboard route, below-fold advanced row.
- Legacy On preset migrates to 2 Hz High-pass, Off default, new modes and Manual offset round-trip.
- Effect intent/semantic plan wired to typed DC observation -> decision -> native registered correction, with fingerprinted policy.
- Native Float64 bounded-memory Auto measurement and per-channel subtraction; no silent clipping in native arithmetic, per-channel reason/output statistics, cancellation and source digest verification across Auto passes.
- Existing effect-chain executor wired for ordinary and DSD/certified true-peak carrier routes; terminal peak overload refusal; conservative continuous CUE per-track Auto skip.
- Structured execution projector extended to bind physical DC receipts; focused new tests included in source.

## Verified results

- Original source archive SHA256 checked.
- Per-file original/new digest inventory produced (12 modified Rust files, one added Rust module; no removed files).
- Independent host FFmpeg 7.1.5 Float64 `anull` raw/WAV/raw transport: 480,000 frames x 2ch bit identical, including >1 amplitude. Repro/test output bundled.

## Not executed / unresolved

- `nix`, `rustc`, `cargo`, `rustfmt` absent; full cargo fmt/check/clippy/workspace tests impossible in this environment. No unrun gate is a pass.
- Compilation/typechecking of added planner variants/call sites not verified; there may be compile or integration defects.
- No pinned FFmpeg 8 transport test or realistic four-mode TonePoet performance measurements.
- No actual end-to-end decoded PCM verification of source-specific, DSD, DVD-Audio, CUE direct-stream, true-peak, HDCD, dither routes. These require qualification on a build host.
- Album-wide common estimator not supplied for adjoining CUE members: Auto deliberately records skip instead of risking per-member clicks. Determine whether reuse of existing album machinery is necessary and feasible before approving full R2 acceptance.

## Resume instructions

1. Extract bundled full source. Verify `DC_SHIFT_R2_SHA256SUMS.txt` (from root: `sha256sum -c DC_SHIFT_R2_SHA256SUMS.txt`).
2. Read project `CLAUDE.md` and `DC_SHIFT_R2_ENGINEERING_HANDOFF.md`; inspect changed files from `DC_SHIFT_R2_SOURCE_CHANGE_INVENTORY.txt`.
3. On project-pinned Nix host run format, cargo check/clippy, focused tests, complete workspace gate, and audio route fixtures; resolve concrete findings minimally.
4. Repeat original-vs-updated per-file source hash verification, update this checkpoint and handoff, regenerate digest manifest, and rearchive entire tree.

Do not claim success for any test not actually executed. Do not replay earlier construction scripts against this source tree.
