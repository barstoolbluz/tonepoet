# TonePoet R31 -- analysis over-range and custom-pill default (2026-10-11)

## Delivery status

**Complete, minimal source correction; compiled/build-host acceptance NOT verified here.**

The supplied archive is an R30C-based full source tree. R31 modifies exactly four existing Rust files and adds three focused regressions. No DSP, output quantization, true-peak measurement algorithm, conversion planner, dependencies, locked Reference sources, or qualification machinery was changed.

## Corrections

1. **True-peak `custom` pill acceptance.** `src/tui/convert_screen.rs` now expects `-0.10 dBTP` for both PCM and DSD default rows. The test still verifies click target, editor appearance, pill persistence, and committing `0.0001` as negative user-selected headroom. No product default or user interaction was changed.

2. **Sample-domain over-range semantics.** In `src/tui/analyze.rs`, the existing per-sample condition is now `abs_val > 1.0`, rather than `abs_val >= 0.9999695`. Signed integer full-scale endpoints and floating-point samples exactly at ±1.0 are **not** over-range. Finite floating-point samples beyond ±1.0 **are** counted, regardless of bit depth. The already present per-sample loop is unchanged apart from its comparison; no new pass, interpolation, resampling, or allocations are introduced. The existing `AnalysisResult.clipping_count`/SQLite column names are retained for compatibility; the UI now displays **Samples > 0 dBFS** instead of asserting historic clipping.

   Digital PCM samples alone cannot conclusively identify clipping that occurred earlier in mastering or conversion. A run-of-rail-values heuristic could misclassify legitimate full-scale programme material and was deliberately not added. An integer PCM stream cannot contain samples above its representable full-scale bounds. The native true-peak observation is a separate measurement and is not interpreted as a count of clipped samples.

3. **Stale analysis cache.** `ANALYSIS_ALGO_VERSION` moves from 27 to 28, causing one-time cache misses and recalculation for old PCM analyses. Additionally, the metadata-only `analysis_cache` UPSERT does **not** carry over an obsolete `clipping_count` when the `algo_version` changes. That counter becomes `NULL`, so the full analysis cache reader refuses the partially promoted row. It continues to preserve the count for unchanged file identity **and** unchanged algorithm version. No DB schema migration is required.

4. **Regression coverage.** New `r31_` tests exercise native WAV decoding at signed 16/24/32-bit endpoints, float32/float64 exactly at full scale and at the next representable value beyond it, whole-file vs bounded/seeked analysis, and metadata-only cache refresh across the algorithm version boundary and after rescan.

## Existing true-peak path (no new scan needed)

`analyze_file()` returns `true_peak_dbtp: None` by design: it is the sample-domain/DR scan. In ordinary :analyze and extracted-CUE-track Analyze workflows, `src/tui/command.rs` separately invokes `measure_loudness()`, then writes its native ReplayGain reporting peak into `result.true_peak_dbtp`. Seek-based CUE track analysis intentionally omits the programme loudness path and leaves true peak unavailable. The source brief's observation that `analyze_file()` itself sets `None` is accurate; its inference that the user-facing analysis never populates true peak is not. No redundant native scan has been added.

## QA performed in this environment

- Source archive extracted intact; before editing, the inherited R30 full-source SHA inventory verified **919 unchanged files**, and only the four intentionally edited paths subsequently differed.
- `git diff --check`: PASS.
- New R31 patch applied cleanly to independently restored originals of the four modified files; byte-for-byte comparison: PASS.
- Shell syntax checks (`bash -n`) on `R31_ACCEPTANCE.sh`, inherited `R30_ACCEPTANCE.sh` and `scripts/requalify_reference_r20.sh`: PASS.
- Independent FFmpeg CLI decoder boundary checks on generated WAVs: PASS for 16/24/32-bit signed integer (0 over-range samples), float32/float64 (22,050 strict over-range samples in test fixtures), including exact full-scale samples.
- SQLite verification executing the **production metadata-only UPSERT SQL** extracted from `src/db.rs`: PASS for old-version carryover refusal, current-version preservation, and changed-file identity.
- No Rust/Cargo/Nix toolchain is available in the environment. `./R31_ACCEPTANCE.sh` exits **2** with `Cargo unavailable`. **No compiled Rust tests, workspace release build, performance benchmark, Reference qualification, or positive SACD conversion were run here.** The 7,434/2 gate numbers in the supplied brief are reported historical context, not results of this run.

## Build-host acceptance and handoff

On a properly configured audio/Rust 1.93+ host with access to a real SACD ISO:

```bash
cargo test --locked --lib r31_
cargo test --locked --lib r30_
./R31_ACCEPTANCE.sh /absolute/path/to/real-SACD.iso
```

`R31_ACCEPTANCE.sh` runs the three `r31_` regressions, then invokes the unchanged `R30_ACCEPTANCE.sh`, which executes the inherited focused regression suites, full workspace gates, release builds, and `scripts/requalify_reference_r20.sh`. Perform the previously documented **negative runtime-closure mismatch smoke** and visually verify the analysis overlay. Do not waive Reference source-lock freshness failures or use the unqualified override. Although R31 itself does **not** edit the locked `stages.rs` and `settings.rs`, their accepted R30 revisions still require fresh build-host evidence relative to the installed Reference sidecars.

The **complete R31 source archive** includes this note, `R31_ACCEPTANCE.sh`, the `R31_FROM_INPUT.patch` minimal code delta, `R31_RESUME_CHECKPOINT_2026-10-11.md` and `R31_SOURCE_SHA256SUMS.txt`. For an already-unpacked *identical supplied input archive*, applying `R31_FROM_INPUT.patch` changes only the four Rust files; use the full archive for the additional acceptance and handoff artifacts. The older R30 source manifests and delivery notes are historical records, superseded by the R31 inventory for current source-integrity checking.
