# Resume checkpoint — TonePoet HDCD/TruePeak R1 (2026-10-16)

STATE: **Source implementation staged; compile, integration, and Reference gates still open.** The attached archive is a complete repository snapshot, not a qualified release.

INPUT SHA256: dbf02c1adca8d95ad4141de94ea8706ed0b16f7eef34ea68dc57ce36746ef9c9
OUTPUT repository root: `tonepoet/` inside this archive.

FILES CHANGED: `tonepoet-pipeline/src/semantic_plan.rs`, `src/convert/pipeline/plan_bridge.rs`, `src/convert/pipeline/stages.rs`, `src/tui/command.rs`, `src/tui/app.rs`.
NEW TEST: `tests/hdcd_true_peak_composition_ffmpeg_smoke.py`.
HANDOFF: `ENGINEERING_HANDOFF_HDCD_TRUEPEAK_2026-10-16.md`.
PATCH: `SOURCE_DELTA_HDCD_TRUEPEAK.patch` (relative to `TonePoet_SNAPSHOT_2026-10-16.tar.gz`).
VERIFICATION: `VERIFICATION_HDCD_TRUEPEAK_2026-10-16.md`.
MANIFEST: `SHA256SUMS_HDCD_TRUEPEAK.txt`; self-excludes to avoid recursive hash.

COMPLETED: Source-first HDCD + certified Track/Album waveform preparation, typed contract binding, Float64 retained scan carrier, per-track dither authority, effect command/evidence attribution including scratch retry and CUE, unit regression source, physical FFmpeg smoke. Original source member preservation checked; all five available Python HDCD tests pass (with one optional AAC skip).

NOT VERIFIED: Rust compilation, Rust tests, full workspace, real TonePoet pipeline conversion, Nix/Reference certification. FFmpeg smoke is not substitute evidence.

KNOWN REFUSALS: HDCD-derived protected SSRC/true-peak replay unqualified; pre-resample HDCD+de-emphasis with a rate change remains unqualified; not every preferred resampler has a protected/selected FFmpeg double-precision binding.

RESTART ORDER:
1. Verify archive external SHA256 and each file in `SHA256SUMS_HDCD_TRUEPEAK.txt`.
2. Read `ENGINEERING_HANDOFF_HDCD_TRUEPEAK_2026-10-16.md` and `VERIFICATION_HDCD_TRUEPEAK_2026-10-16.md`.
3. Inspect the **six** changed/new files and `SOURCE_DELTA_HDCD_TRUEPEAK.patch`.
4. Run Rust format/compile, focused Rust tests and workspace gates. Check CUE exact frames, mixed-member dither, Track vs Album provenance indices and retained scratch replay, and source/terminal proof agreement.
5. Run actual HDCD/TruePeak jobs and pinned Reference qualification; fix realistic failures only. Update handoff and regenerate checksum inventory/archive before release.

Do not assume these modifications are accepted merely because the current sandbox produced physical smoke witnesses.
