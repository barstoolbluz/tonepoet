# TonePoet CUE ReplayGain bottleneck — host investigation runbook

**Date:** 2026-10-24  
**Authority:** Source snapshot `TonePoet_SNAPSHOT_da8702a_2026-10-24.tar.gz` (SHA-256 `63e4e4ee4a0ae880ffd0126c6c52350ae374031c9995fb909155f3c4b47a93c5`)  
**Scope:** DIAGNOSTIC ONLY. No ReplayGain numerical-policy change, transport rearchitecture, new parallel execution, or conversion-default change.

## 1. What this instrumentation actually measures

- **Transport:** elapsed wall time spent inside each Tokio decoder read, encoder write, and ReplayGain mirror FIFO write, including syscall, blocking, executor scheduling and wake-up; header write and FIFO opening are also tracked. A long FIFO write is *consistent with* backpressure; it is not proof of how long the FIFO was full.
- **Observer:** elapsed wall time spent on raw FIFO reads, PCM-to-f64 decoding, native loudness `push_interleaved`, reporting true-peak `push_interleaved`, and their two finalizers. Counts of `WouldBlock` polls and measured 5-ms sleeps are exposed separately. A source-pass manifest is emitted before workers launch; the report refuses to mark a partial set of track traces successful. These sleep times also occur **inside** read/EOF times; do not add them twice.
- **Host:** best-effort Linux `/proc` sampling of the process tree's accumulated CPU ticks, grouped by executable basename and inferred decoder/encoder roles for the exact known S1 FFmpeg/SoX commands, without retaining command-line arguments. A process may exit before it is sampled. Thus these are **lower bounds**, not authoritative exclusive CPU accounting. **TonePoet main-process CPU includes the native meter and unrelated application threads**; a separate exclusive native meter CPU figure is not available from process accounting alone.
- **Microbenchmark:** the same native `NativeEbu2023 + IntegratedOnly` loudness and `ReportingPeakMeter` kernels, the same mono/stereo roles and 16,384-frame block size as the CUE source-pass implementation. It reads a regular raw PCM file rather than a FIFO, allowing the intrinsic arithmetic cost to be measured without the producer/encoder dependency.

**Absolutely no summing of concurrent observer and transport times.** Off-versus-Both elapsed difference is the net cost of changing the whole pipeline, not independently measurable ReplayGain CPU work. The transport stages within a single measured segment are sequential, but their wall times may include scheduler delays. The host runner's process wall includes startup and any time an interactive TUI remains open; use one-shot CLI for comparable wall timings.

## 2. Authenticate and compile on the real host

Use the normal `nix develop` environment described in `README.md`. Build and tests must run with the production features/codegen; do not benchmark debug binaries or under instrumentation-heavy tracing.

```bash
sha256sum TonePoet_SNAPSHOT_da8702a_2026-10-24.tar.gz   # if you retained the input archive
cd tonepoet
sha256sum -c RG_DIAG_CHANGED_SHA256SUMS.txt
python3 -m unittest discover -s tooling/rg_diag -p 'test_*.py' -v
cargo test --locked -p tonepoet profiled_cue_meter_matches_unprofiled_samples_and_result_bits
cargo test --locked -p tonepoet rg_profile
cargo test --locked -p tonepoet segmented_pipeline_
cargo test --locked -p tonepoet-true-peak
cargo build --locked --release -p tonepoet
cargo build --locked --release -p tonepoet-true-peak --example rg_native_bench
```

These Rust commands have **NOT RUN** in the preparation environment: `rustc`, `cargo`, and the application's system dependencies are unavailable there. Do not treat this bundle as compile-qualified until the host verifies it. If a build or test fails, retain the stdout/stderr and identify the exact failing command; do not silently modify production DSP or skip the failing test.

## 3. Exact-album experiment (predeclare the matrix)

Use the **same Billie Holiday CUE+image album** that measured ~15 s (RG Off) versus ~89 s (RG default/Both). Match the actual original codec, destination, bit depth, backend, metadata, and other processing flags. Do not invent or change parameters to make the test faster. Run this on an otherwise idle host, on the same filesystem, with separate **fresh** output directories and no destination reuse or cache-hit behavior. The CLI supports `--replaygain off|both` explicitly. Verify the selected route is S1 and `source_pass_observed=true` in the Both trial before interpreting profiling as source-pass evidence.

The following `flac` flags are placeholders **only if the original run used FLAC with those settings**. Substitute the real options and input paths:

```bash
# Edit these paths and flags. Never publish private source filenames in a public report.
SRC='/private/path/Billie-Holiday.cue'
ROOT='/private/rg-experiment'
mkdir -p "$ROOT/outputs"

# Three uninstrumented paired trials, alternated to expose warm/cold/cache drift.
python3 tooling/rg_diag/run_host.py --out "$ROOT/runs" --label off-control-01 --no-instrument -- \
    ./target/release/tonepoet convert "$SRC" --format flac --replaygain off --output "$ROOT/outputs/off-01"
python3 tooling/rg_diag/run_host.py --out "$ROOT/runs" --label both-control-01 --no-instrument -- \
    ./target/release/tonepoet convert "$SRC" --format flac --replaygain both --output "$ROOT/outputs/both-01"
# Repeat OFF/BOTH in counterbalanced order for trials 02 and 03 with fresh output roots.

# Both instrumented and its RG Off control (do not reuse the control output directory).
python3 tooling/rg_diag/run_host.py --out "$ROOT/runs" --label both-profile-01 -- \
    ./target/release/tonepoet convert "$SRC" --format flac --replaygain both --output "$ROOT/outputs/both-profile-01"
python3 tooling/rg_diag/run_host.py --out "$ROOT/runs" --label off-profile-01 -- \
    ./target/release/tonepoet convert "$SRC" --format flac --replaygain off --output "$ROOT/outputs/off-profile-01"

python3 tooling/rg_diag/analyze.py \
    "$ROOT/runs/both-profile-01" "$ROOT/runs/off-profile-01" \
    "$ROOT/runs/off-control-01" "$ROOT/runs/both-control-01" \
    --output "$ROOT/RG_DIAG_REPORT.md"
```

If profiling substantially changes the Both run's runtime, record the bias and do not make numerical claims about the native path from a single profiled trial. Repeat at least three trials per relevant condition, report individual times, medians and spread, and keep samples with distinguishable cache states separate. For CPU attribution, the host process tree and its S1 role inference are lower bounds and heuristics; use `perf stat` / kernel scheduling evidence when possible, without contaminating the clean throughput benchmark. Neither the host root-process CPU nor native meter push-call wall time is exclusive ReplayGain CPU.

## 4. Independent native-meter microbenchmark

Extract raw PCM with the **same FFmpeg decode/carrier geometry** as `cue_stream_decoder_command` in `src/convert/pipeline/stages.rs`. Examples, choosing exactly ONE appropriate carrier:

```bash
# The production CUE streaming route uses one of s32le/f32le/f64le.
ffmpeg -v error -hide_banner -nostdin -i "$SRC_IMAGE" -map 0:a:0 -vn -sn -dn \
    -f s32le -c:a pcm_s32le "$ROOT/album.s32le"
# Or match a float carrier with -f f32le -c:a pcm_f32le, or f64le/pcm_f64le.
# Use the actual sample rate and channel count (e.g., 44100 and 2) from the route.

./target/release/examples/rg_native_bench "$ROOT/album.s32le" 44100 2 s32le both 5 \
    > "$ROOT/native-both.jsonl"
./target/release/examples/rg_native_bench "$ROOT/album.s32le" 44100 2 s32le loudness 5 \
    > "$ROOT/native-loudness.jsonl"
./target/release/examples/rg_native_bench "$ROOT/album.s32le" 44100 2 s32le peak 5 \
    > "$ROOT/native-peak.jsonl"
./target/release/examples/rg_native_bench "$ROOT/album.s32le" 44100 2 s32le decode 5 \
    > "$ROOT/native-decode.jsonl"

python3 tooling/rg_diag/analyze.py "$ROOT/runs/both-profile-01" \
    --bench "$ROOT/native-both.jsonl" \
    --bench "$ROOT/native-loudness.jsonl" \
    --bench "$ROOT/native-peak.jsonl" \
    --bench "$ROOT/native-decode.jsonl" \
    --output "$ROOT/RG_DIAG_WITH_BENCH.md"
```

**Track-boundary qualification:** The commands above meter the whole image as ONE continuous programme, whereas TonePoet actually instantiates a separate meter per CUE track. Continuous-image timing is useful for identifying arithmetic throughput, but is not an exact estimate of the cumulative native source-pass work. For a strict matched-workload check, use the source CUE plan's frame-exact `start_byte`/`byte_len` ranges to extract one raw PCM file for **each** selected track, benchmark them separately with this same example, and add their times. Do not derive rounded per-track extents from printed seconds or CD-sector approximations, discard index-00/pregap material ad hoc, or mix PCM sample encodings. Record verified frame counts for the entire matched track cohort and confirm they equal the run's transport/observer manifest. Compare per-track benchmark results with per-track observer work rather than their album-continuous metering values.

Standalone result-bit consistency across repeats is checked by the analyzer. Input raw PCM is **not** included in the code bundle: use the actual album. Source PCM must be a regular, frame-aligned file, stable for the entire benchmark; the benchmark refuses empty or mutated input. Record its SHA-256 privately. The standalone example runs normal production SIMD dispatch; it does not select a faster or lower-accuracy approximate meter.

## 5. Qualification and failure evidence

The agent must report separate **PASS / FAIL / NOT RUN / NOT INSTRUMENTED** states for:

1. Exact source manifest, base archive hash, lockfile unchanged and changed-file hash verification.
2. Rust compile, focused tests, all relevant real segmented-pipeline tests, workspace tests and production release build.
3. Uninstrumented numerical and metadata parity versus the original source (decoded PCM hashes, sample counts, ReplayGain track/album gain/peak values or exact permitted representation tolerance, output files, error and cancellation behavior). No audio output is changed by this patch intentionally, but that assertion still requires host verification.
4. Predeclared Off/Both control runs with *identical* conversion settings and fresh outputs, and confirmation of the real S1 route. Explicitly document source-pass success or fallback.
5. Instrumented expected-track manifest, complete per-track transport/observer coverage, exact byte correlation, and any failed/cancelled attempts. Failed/incomplete traces must not be represented as performance successes.
6. Native-meter benchmark of the same geometry, input sample content and optimized release build, including separate loudness and true-peak costs and consistency of numerical outputs.
7. Diagnostic observer perturbation: instrumented Both versus uninstrumented Both; record the difference rather than correcting it by guesswork.
8. CPU attribution: distinguish sampled process-tree lower bounds from exact CPU measurements; identify unresolved scheduling or kernel backpressure questions honestly.

**Do not change S1 or S3 execution architecture, remove/skip ReplayGain, lower true-peak precision, increase unbounded buffers, or implement the conditional ReplayGain observer under this investigation.** The result of this diagnostic effort is an evidence-based recommendation for the *next* phase, not an unreviewed optimization patch.

## 6. Privacy and repeatability

`TONEPOET_CUE_RG_PROFILE=1` alone does nothing. It requires `TONEPOET_PIPELINE_BASELINE_JSONL` to name a writable file. The host runner creates the JSONL file with mode `0600`; it never modifies the conversion process umask. `RG_DIAG` events carry a process-salted run ID, track ordinal, integers and booleans, not source paths. However **the pre-existing baseline stream contains general pipeline events that may contain paths or item IDs**. Keep trace directories private and redact sensitive information before sharing them. The analyzer intentionally omits full paths from its generated report.

Each run label uses a new directory and refuses overwrite. A dry run, failed run, partial output, fallback, or a conversion that reused old outputs must never be counted toward accepted throughput results. Preserve original trace JSONL, host JSON, compiled binary identity, source SHA-256, test logs, output-audio comparison manifests and report together in a private handoff archive.
