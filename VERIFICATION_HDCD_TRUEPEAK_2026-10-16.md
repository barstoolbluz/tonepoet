# Verification ledger — 2026-10-16

## Executed in this environment

* `python tests/hdcd_true_peak_composition_ffmpeg_smoke.py` — **PASS** (7 printed witnesses: original S16 -> HDCD S32 -> Float64, de-emphasis, Float64 SoXR 44.1 -> 88.2, processed-signal scalar Track/Album Guard/Normalize, exact independent CUE-slice equality, Float64 -> terminal Int16 TPDF, 44.1-kHz Float64 -> MP3 without a hidden sample-rate change).
* `python tests/hdcd_source_first_ffmpeg_smoke.py` — **PASS** (HDCD source-first FLAC/CUE sample evidence).
* `python tests/hdcd_lossy_precision_ffmpeg_smoke.py` — **PASS** for FFmpeg/SoXR/Opus/MP3; `libfdk_aac` was **NOT RUN** because that backend is unavailable.
* `python tests/verify_hdcd_analysis_cache_sql.py` — **PASS**.
* `python tests/verify_hdcd_v28_migration_sql.py` — **PASS** (14 modeled cases).
* Original-to-modified five-file source inventory — **PASS**; original archive member preservation check — **PASS**.
* SHA-256 manifest and deterministic full-repository archive internal verification — see packaging inventory/sidecar.

## Not executable here (explicitly not passed)

* `cargo fmt --all -- --check`: **NOT RUN** (`cargo`, `rustfmt` unavailable).
* Targeted Rust planner tests, compiler and integration tests: **NOT RUN** (`cargo`, `rustc` unavailable).
* Full workspace and Nix gates: **NOT RUN** (`cargo`, `nix` unavailable).
* Actual TonePoet job end-to-end with logged HDCD + TruePeak outcome: **NOT RUN** (no compiled binary or Rust test harness).
* Pinned Reference requalification: **NOT RUN** (qualified runtime/toolchain unavailable).

### Scope of physical witness

The FFmpeg/numpy/scipy test checks commands and sampled values but does not establish the Rust selected terminal proof, sample-exact decoded-lossy output, phase-aware continuous-frequency true-peak extrema, or authoritative downstream codec ceiling. The code is **provisional** until executable integration is verified.
