# Host qualification — TonePoet CUE ReplayGain FIFO polling correction

Run on the host with the original Billie Holiday CUE/FLAC image, the source-to-target settings used in the October 24 instrumented A/B study, the exact original toolchain/audio dependencies, and a Rust toolchain compatible with Cargo.toml's `rust-version = "1.93"`. Do not substitute a synthetic album for fidelity qualification.

## 1. Authentication and build

Unpack the correction in a clean directory; from the archive's top level:

```bash
sha256sum -c SHA256SUMS
sha256sum -c SOURCE_SHA256SUMS
cd tonepoet
cargo test --locked -p tonepoet --lib fifo_readiness_tests -- --nocapture
cargo test --locked -p tonepoet --lib profiled_cue_meter_matches_unprofiled_samples_and_result_bits -- --nocapture
cargo test --locked --workspace
cargo build --locked --release
python3 -m unittest discover -s tooling/rg_diag -p 'test_*.py' -v
python3 ../handoff/verify_unix_fifo_kernel.py
```

The first Cargo command runs the **five** new Unix FIFO tests. They challenge no writer / cancellation, idle connected writer / no spin, odd-sized transfers and truncation, active cancellation and `POLLNVAL`, and EOF surplus + sample/meter equivalence. The workspace test gate may contain an independently known baseline Reference drift; report original and corrected outputs separately, never silently waive unrelated failures. If a test fails, record its exact output and reproduce on a pristine build.

For an independent baseline rebuild, unpack the original source archive alongside the corrected tree and build it with the same Cargo profile, release flags, dependencies and host. Keep build output directories separate. Verify `RG_FIFO_READINESS.patch` applies to the pristine baseline using `patch -p1 --dry-run < ../handoff/RG_FIFO_READINESS.patch` from the original `tonepoet/` directory.

## 2. Matched A/B performance trial

Use the *same twelve-track Billie Holiday album*, source filesystem/cache state, CUE selection, direct S1 route, sample rate / channels / depth, output format, encoder settings, ReplayGain target settings, external tools, CPU governor and host load used for the historical measurements. Execute **one conversion per invocation**, clear only the trial's output directory between runs, and allow the application to exit after terminal publication. Perform at least three successful repeated Off/Both runs on the pristine and corrected executables, interleaved in comparable cache conditions.

For each binary, invoke the existing instrumentation wrapper from that source tree; adapt the executable path only:

```bash
# ReplayGain Both, diagnostics enabled; choose exact original settings in the TUI.
python3 tooling/rg_diag/run_host.py --out "$PRIVATE_RG" --label fixed-both-profile-01 -- ./target/release/tonepoet

# ReplayGain Both, uninstrumented control.
python3 tooling/rg_diag/run_host.py --out "$PRIVATE_RG" --label fixed-both-control-01 --no-instrument -- ./target/release/tonepoet

# ReplayGain Off, uninstrumented control (switch only ReplayGain in the TUI).
python3 tooling/rg_diag/run_host.py --out "$PRIVATE_RG" --label fixed-off-control-01 --no-instrument -- ./target/release/tonepoet

# Reports for runs with host.json and cue.jsonl:
python3 tooling/rg_diag/analyze.py "$PRIVATE_RG/fixed-both-profile-01" --output "$PRIVATE_RG/fixed-both-profile-report.md"
```

Use unique labels for the remaining repetitions and for the baseline source. `run_host.py` includes TUI interaction/launch/exit in `host.json` wall time: **do not confuse this with conversion elapsed**. Prefer correlated `run_finished.elapsed_ms` or the actual conversion-work record where present, and separately measure TUI start/finish if it affects host wall. A failed/fallback run is excluded from source-pass speed comparison, not averaged into a successful cohort. Report median, range, both states, number of attempts and source-scan outcomes.

## 3. Readiness/CPU evidence to collect

With instrumented Both, require `rg_diag_manifest`, one complete `rg_diag_transport` and `rg_diag_observer` per track, exact transport/observer PCM byte parity, and exactly one authoritative `source_pass_replaygain_result` with `source_pass_observed=true`. There must not be an observer recovery retry, a hidden post-output ReplayGain fallback, or an incomplete/duplicated run. The updated analyzer accepts historical and corrected rows and reports *cumulative per-observer* readiness counters in corrected rows.

For every `rg_diag_observer`, examine:

- `would_block_poll_count`: **EAGAIN reads**, not the count of sleeps or `poll` syscalls.
- `readiness_wait_count` / `readiness_timeout_count`: kernel poll calls / timeouts; check for a wake count consistent with data delivery and no busy loop.
- `readiness_wait_call_ns`: cumulative wall time *inside poll*; not a serial album penalty.
- `poll_sleep_call_ns`: pre-first-writer backoff only in the corrected Unix code. It historically included active 5-ms waits: **do not compare old and new as identical metrics**.
- `pcm_read_call_ns`, `eof_wait_call_ns`, loudness + reporting-peak push/finalization, complete flag, and exact `pcm_bytes` per track.
- `fifo_wait_strategy`: must be `poll_50ms_plus_no_writer_backoff` for the corrected Unix observer.

Use `host.json` process-tree CPU as a **lower bound**, not exact native-meter CPU. If available, use system CPU profiling for more authoritative attribution and compare profiler-enabled vs control wall times to bound diagnostic overhead. Sequential observers waiting for later tracks overlap; do not sum their pre-writer delays or inclusive read times into end-to-end conversion time.

## 4. Fidelity (FLAC target example)

Preserve two independent output directories (pristine baseline and corrected) with the *identical* source and conversion settings. Confirm same published track count, order, filenames, duration, channel count and sample rate. Compare the **ReplayGain tag values exactly**, and compare other expected metadata and cover art. Do not require encoded container byte identity.

For FLAC outputs, use the same installed `metaflac` and `ffmpeg` versions against each pair:

```bash
# Run for each paired track (identical basename); substitute actual files.
metaflac --show-tag=REPLAYGAIN_TRACK_GAIN --show-tag=REPLAYGAIN_TRACK_PEAK \
         --show-tag=REPLAYGAIN_ALBUM_GAIN --show-tag=REPLAYGAIN_ALBUM_PEAK \
         "$BASELINE_TRACK" > "$PRIVATE_RG/baseline-tags.txt"
metaflac --show-tag=REPLAYGAIN_TRACK_GAIN --show-tag=REPLAYGAIN_TRACK_PEAK \
         --show-tag=REPLAYGAIN_ALBUM_GAIN --show-tag=REPLAYGAIN_ALBUM_PEAK \
         "$FIXED_TRACK" > "$PRIVATE_RG/fixed-tags.txt"
cmp "$PRIVATE_RG/baseline-tags.txt" "$PRIVATE_RG/fixed-tags.txt"

# Compare decoded sample streams, not container bytes (same decoder and options).
ffmpeg -v error -i "$BASELINE_TRACK" -map 0:a:0 -f s32le -acodec pcm_s32le \
    "$PRIVATE_RG/baseline-decoded.s32le"
ffmpeg -v error -i "$FIXED_TRACK" -map 0:a:0 -f s32le -acodec pcm_s32le \
    "$PRIVATE_RG/fixed-decoded.s32le"
cmp "$PRIVATE_RG/baseline-decoded.s32le" "$PRIVATE_RG/fixed-decoded.s32le"
```

For other formats, or where floating-point/sample-identity comparison requires a different format, select a canonical common PCM representation that does not introduce a lossy transformation. Compare embedded album/track ReplayGain values at the actual metadata backend, including untouched metadata, independent of any container-level timestamps or ordering.

If the original host's exact decoded CUE PCM file is still retained, independently compare the native meter's **raw floating-point result bits** from the unchanged in-tree benchmark against the pristine snapshot and corrected tree. Run from each source tree, with the **same** PCM bytes and geometry:

```bash
cargo run --locked -p tonepoet-true-peak --release --example rg_native_bench -- \
    "$ORIGINAL_CUE_PCM_RAW" 44100 2 s32le both 3 > "$PRIVATE_RG/native-both.jsonl"
```

Compare the `lufs_bits` and `peak_linear_bits` for every repeat against the pristine run, not just rounded tags. This corroborates unchanged native math, but does **not** by itself prove the entire FIFO transport preserved samples; the matched decoded-output and metadata comparisons above are still required.

## 5. Failure, cancel, and cleanup gates

With a qualified host test fixture, exercise one late-connected producer, an intentionally idle/paused connected producer, cancellation while a future track has no writer, cancellation during an active stream and at EOF wait, early close/short payload, wrong prefix, and surplus byte. Rust unit tests cover core descriptor behavior; host integration must confirm cancellation reaches a terminal outcome, no orphan observer/task/process/FD, correct directory cleanup, and no partial publication. Force an observer-side failure without independently failing the encoder/decoder and confirm **existing one-time non-mirrored recovery plus output-measured ReplayGain** rather than incorrectly accepting a partial source-pass result. Preserve the default RG Off path; do not introduce new fault injection hooks in production solely for qualification.

## 6. Decision criteria

Report end-to-end Off/Both elapsed, analyzer status, source-pass observed vs fallback, PCM identity, tag identity, cancellation/failure outcome, and readiness-wait counters. **No arbitrary 20–30 second speed requirement.** Accept this repair if it demonstrably removes the artificial 5-ms delay from EAGAIN and introduces no comparable latency, correctness, cancellation, or fallback defect. A strong improvement without fidelity/cancellation qualification is not sufficient for final handoff. A correct patch that is slower than hoped is not an automatic reason for a larger redesign; investigate concrete residual costs first.
