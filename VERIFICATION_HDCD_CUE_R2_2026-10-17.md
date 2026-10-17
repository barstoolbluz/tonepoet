# Verification ledger -- TonePoet HDCD / CUE R2

Date: 2026-10-17 UTC; repository basis: R1 unqualified full archive.

## Executed and passed here

| Command / check | Result | Scope |
| --- | --- | --- |
| `python -B tests/hdcd_cue_continuity_ffmpeg_smoke.py` | PASS | Recognized Peak Extend, 44,433 exact post-HDCD-cut frames, **0 sample differences** vs full decode, deliberately broken cut-first **390 differences**; untouched mixed member S16 preserved |
| `python -B tests/hdcd_source_first_ffmpeg_smoke.py` | PASS | Scan-time classification, actual source-first continuous HDCD, sample-exact post-cut S32 WAV |
| `python -B tests/hdcd_true_peak_composition_ffmpeg_smoke.py` | PASS | Seven physical witnesses: source-first Float64, de-emphasis, one rate change, Track/Album scalar calculations, correct post-HDCD CUE cut, terminal TPDF/Int16 and MP3 |
| `python -B tests/hdcd_lossy_precision_ffmpeg_smoke.py` | PASS for available backends | Opus, MP3 and TPDF/Int16 witnesses |
| `python -B tests/verify_hdcd_analysis_cache_sql.py` | PASS | HDCD analysis cache authority/identity |
| `python -B tests/verify_hdcd_v28_migration_sql.py` | PASS | 14 modeled migration cases |
| R1 original vs R2 file-level comparison | PASS | Only two production Rust files, two adjusted existing FFmpeg tests and one new physical test, aside from explicit R2 delivery artifacts |
| R1-to-R2 source patch dry run, patch replay and current SHA256 inventory | PASS | R1-to-R2 patch replay reconstructed all five changed/new source and test files byte-identically; current R2 SHA inventory verified |

The `libfdk_aac` optional FFmpeg cell is **NOT RUN** because the optional backend is not installed.

## Not run: do not infer PASS

* `cargo fmt --all -- --check`: **NOT RUN** (`cargo` and `rustfmt` absent).
* Rust build/check, targeted new and existing Rust tests, exhaustive CUE direct/gated cells, all integration tests and `cargo test --workspace`: **NOT RUN** (`rustc`, `cargo` absent).
* Actual TonePoet end-to-end HDCD CUE -> MP3, guarded/norm Track/Album, failure-injected command-evidence retention and live conversion logs: **NOT RUN** (no compiled TonePoet).
* Measured decoder invocation/scratch-peak comparisons for actual TonePoet paths: **NOT RUN** (new Rust stub test *source* is present, uncompiled; no measured live memory/disk profile).
* Pinned Nix/Reference route requalification: **NOT RUN** (`nix` and pinned host absent).

## Proof boundaries

FFmpeg directly proves sample semantics and audio representations for constructed fixtures. Source inspection supports route integration, but **only Rust and build-host system tests can prove the real planner/executor/album log integration**. A per-member continuous HDCD decode has possible image-length-proportional cost for multi-member groups; a shared correctly attributed HDCD decode remains an optimization candidate, not a claimed implementation.
