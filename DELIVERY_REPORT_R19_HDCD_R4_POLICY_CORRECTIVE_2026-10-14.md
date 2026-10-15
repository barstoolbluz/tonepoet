# TonePoet R19 HDCD R4 — corrective engineering handoff

**Date:** 2026-10-14 (UTC)  
**Authority:** `BRIEF_R19_HDCD_R4_defaults_depth_dither_lossy_2026-10-14.md`, SHA-256 `608c1ea4fda02fd0c3270423561460bf64acd231dce6160669d670cc57d11f1d`.  
**Base archive:** `TonePoet_LOGGING_R19_HDCD_R4_2026-10-14.tar.gz`, SHA-256 `35d228f329fd3aa766f23985ab5aec28ae1d92360012d39c83e163ee7b40c6aa`.

## Status

**Complete source snapshot delivered; release qualification remains OPEN.** This execution environment lacks `cargo`, `rustc`, `rustfmt`, `nix`, the pinned FFmpeg with `libfdk_aac`, and the real Reference qualification fixture. No Rust compilation, Rust test, installed-database upgrade or Reference requalification is claimed. Do not interpret passing independent Python/FFmpeg checks as a green release gate.

## Corrections

1. **Database v28:** `src/db.rs` creates the missing `analysis_cache` before attempting the two HDCD identity/capability columns, preserving already-present rows; replay remains idempotent. Two Rust tests cover missing-table and populated-table cases.
2. **Automatic Decode policy:** `src/tui/app.rs` defaults Off for 16-bit lossless at every rate; On for supported high-precision lossless and lossy outputs, for effectual HDCD only. Undetected, passive and TF-only sources remain ineligible. Manual Off stays authoritative through unrelated changes. A source replacement clears source-derived HDCD policy.
3. **Visible depth and preset provenance:** A manual Decode-On over a 16-bit lossless target visibly promotes the format bit depth to Int24, even when a preset originally selected Int16. A later manual Int16 choice stays Int16 with decoding On. Prior automatic depth is restored only if no later user depth selection replaced it. Preset serialization distinguishes automatic promotion/TPDF from deliberate user settings. Existing unmarked presets deserialize unchanged.
4. **Sixteen-bit HDCD output:** `src/tui/command.rs` and `tonepoet-pipeline/src/semantic_plan.rs` remove the obsolete refusal. The existing registered-effect terminal computes one TPDF owner after the Float64 processing carrier, unless an explicit dither selection (including Off) overrides it. Existing certified true-peak refusal is retained.
5. **Lossy codec precision:** `tonepoet-pipeline/src/plugins.rs` explicitly chooses floating encoder input for wider-than-Int16 PCM feeding libopus (`flt`) and libmp3lame (`fltp`). For libfdk AAC, whose input requires Int16, the FFmpeg filter performs one final TPDF/sample-format reduction if the carrier is wider or the signal changed; explicit supported dither choices override the default. Source-Int16 unchanged AAC takes the old fast path. Certified DSD and PCM true-peak routes retain their previously qualified terminal behavior; this patch does not add unqualified dithering to them.
6. **Existing rate/backend policy:** The HDCD source-first invocation and its source-identity guard remain unchanged. Existing selected resamplers, explicit Ultra quality, 44.1-to-48 Opus rate pinning, SSRC terminal variants, de-emphasis, per-CUE-member authorization and source cache fast paths remain in their original code. No independent HDCD scanner or scheduler was added.

## Changed files (relative to supplied archive)

- `src/db.rs`
- `src/tui/app.rs`
- `src/tui/command.rs`
- `src/tui/presets.rs`
- `tonepoet-pipeline/src/semantic_plan.rs`
- `tonepoet-pipeline/src/plugins.rs`

Added compile-free focused probes:

- `tests/verify_hdcd_v28_migration_sql.py`
- `tests/hdcd_lossy_precision_ffmpeg_smoke.py`

Original tests and other project files remain in place. A reconstructable six-file source diff is shipped as `R19_HDCD_R4_SOURCE_DELTA.patch` and full-file inventory as `R19_HDCD_R4_PACKAGE_SHA256SUMS.txt`.

## Tests actually executed in this environment

| Check | Observation |
| --- | --- |
| `python3 tests/verify_hdcd_analysis_cache_sql.py` | PASS: original cache SQL semantics |
| `python3 tests/hdcd_source_first_ffmpeg_smoke.py` | PASS: genuine FFmpeg HDCD decode, late detection, FLAC/CUE sample counts |
| `python3 tests/verify_hdcd_v28_migration_sql.py` | PASS: 14 modeled v21–v27 cache-present/cache-missing/replayed SQLite scenarios, using SQL extracted from new migration |
| `python3 tests/hdcd_lossy_precision_ffmpeg_smoke.py` | PASS: S32 to Float-input Opus, S32 to planar-Float-input MP3, and one format-only TPDF/Int16 conversion |
| `python3 -m py_compile` (four smoke tests) | PASS |
| Actual `libfdk_aac` encoding | NOT RUN: encoder unavailable in system FFmpeg |
| Rust compile/check/test suite | NOT RUN: Rust toolchain unavailable |
| Pinned-FFmpeg HDCD/Format/queue execution | NOT RUN: pinned toolchain unavailable |
| Reference source lock and real-fixture requalification | NOT RUN: requires configured build host and fixture |
| Actual installed SQLite migration/startup (v21–v27) | NOT RUN: no installed database; supplied Rust tests cover path when compiled |
| Performance regression benchmark | NOT RUN: executable unavailable |

These PASS results describe the **compile-free instruments only**. The newly added Rust tests were inspected but could not be executed.

## Build-host acceptance gates

Run in the repo root on the authorized toolchain/configuration, with FFmpeg including HDCD, SoXR and libfdk AAC:

```bash
cargo check --workspace --all-targets
cargo test hdcd_ -- --nocapture
cargo test migrate_v -- --nocapture
cargo test v28_ -- --nocapture
cargo test ffmpeg_lossy -- --nocapture
cargo test --workspace --all-targets
python3 tests/verify_hdcd_analysis_cache_sql.py
python3 tests/verify_hdcd_v28_migration_sql.py
python3 tests/hdcd_source_first_ffmpeg_smoke.py
python3 tests/hdcd_lossy_precision_ffmpeg_smoke.py
```

Validate the format-pane and preset regression tests in the complete Cargo gate. Exercise actual effectual 16/44.1 sources into Int24 FLAC, manual TPDF/Int16 FLAC, explicit dither Off/Int16, and 48-kHz Opus plus AAC/MP3. Confirm one planned rate-conversion owner and no additional quantization after terminal dither, and recheck installed SQLite upgrades with backed-up databases.

**Reference qualification is mandatory:** `tonepoet-pipeline/src/semantic_plan.rs` and `tonepoet-pipeline/src/plugins.rs` are listed in `reference_source_lock.rs`. Do not hand-edit qualification JSON or accept stale source locks. Run the established `scripts/requalify_reference_r20.sh` gate with the configured real SACD fixture/tool runtime, inspect its newly generated report/certification/evidence/source lock, and then run the normal Reference freshness and workspace tests. The old qualification sidecars remain untouched in this snapshot.

## Focus and limitations

This is a six-source-file correction, not a planner redesign. It reuses the current typed terminal and TUI provenance machinery. Real Rust compilation is the remaining decisive gate; no unverified claim of a zero-failure full test suite, identical decoded samples, pinned AAC performance or refreshed Reference certification is made. The complete source archive is the handoff; this report is not a substitute for source and tests.
