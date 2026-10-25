# TonePoet ReplayGain source-pass performance investigation

Instrumented measurements are inclusive wall-clock call durations, not CPU times or pure FIFO blocked time.
The transport and observer execute concurrently. **Never add their call durations as an album critical path.**

| Run | TonePoet conversion wall (s) | Host process wall (s) | CPU lower bound (s) | CUE run IDs | Diagnostic status |
|---|---:|---:|---:|---:|---|
| both-profile-01 | N/A | 89.908 | 113.870 | 1 | 78f3587c:PASS |
| off-profile-01 | N/A | 15.433 | 23.920 | 1 | eb12df6b:NOT INSTRUMENTED |
| off-control-01 | N/A | 15.301 | 23.160 | 0 | NOT INSTRUMENTED |
| both-control-01 | N/A | 89.761 | 112.800 | 0 | NOT INSTRUMENTED |
| off-control-02 | N/A | 15.328 | 23.460 | 0 | NOT INSTRUMENTED |
| both-control-02 | N/A | 89.346 | 112.880 | 0 | NOT INSTRUMENTED |
| off-control-03 | N/A | 15.420 | 23.390 | 0 | NOT INSTRUMENTED |
| both-control-03 | N/A | 89.802 | 114.630 | 0 | NOT INSTRUMENTED |

## Per-run evidence

### both-profile-01

Host return code: `0`. Profiling enabled: `True`. Trace records: 206.

Inferred per-role process CPU lower bounds (CUE S1 command signature heuristics; not exclusive native ReplayGain time): cue_stream_decoder: 5.330 s, cue_stream_encoder: 7.730 s, other_ffmpeg_or_unclassified: 0.130 s, other_or_unclassified: 85.410 s, tonepoet_app_including_native_meter: 15.270 s

Sampled process-tree CPU lower bounds: ffmpeg: 5.460 s, ffprobe: 1.020 s, metaflac: 0.070 s, sox_ng: 7.730 s, tonepoet: 99.590 s

**CUE run `78f3587c24a7ba7dce3cce99` — PASS**

Source-pass RG observed: `True`. 

| Ordinal | PCM MiB | Read call (s) | Encoder write call (s) | Mirror write call (s) | Observer PCM read call (s) | Decode PCM call (s) | Loudness call (s) | Reporting peak call (s) |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 70.70 | 0.203 | 0.008 | 3.637 | 3.036 | 0.024 | 0.181 | 0.614 |
| 2 | 71.57 | 0.059 | 0.050 | 6.508 | 5.846 | 0.022 | 0.171 | 0.589 |
| 3 | 78.96 | 0.077 | 0.084 | 7.175 | 6.469 | 0.024 | 0.209 | 0.646 |
| 4 | 62.17 | 0.055 | 0.081 | 5.751 | 5.111 | 0.022 | 0.173 | 0.588 |
| 5 | 59.88 | 0.054 | 0.082 | 5.484 | 4.935 | 0.020 | 0.148 | 0.530 |
| 6 | 70.67 | 0.073 | 0.084 | 6.459 | 5.803 | 0.022 | 0.188 | 0.607 |
| 7 | 68.44 | 0.080 | 0.073 | 6.253 | 5.612 | 0.022 | 0.183 | 0.600 |
| 8 | 83.11 | 0.095 | 0.086 | 7.548 | 6.819 | 0.025 | 0.218 | 0.677 |
| 9 | 92.92 | 0.095 | 0.086 | 8.460 | 7.602 | 0.028 | 0.246 | 0.768 |
| 10 | 85.33 | 0.086 | 0.073 | 7.852 | 6.987 | 0.030 | 0.215 | 0.790 |
| 11 | 70.49 | 0.088 | 0.062 | 6.399 | 5.775 | 0.021 | 0.169 | 0.589 |
| 12 | 97.24 | 0.090 | 0.052 | 8.875 | 7.933 | 0.029 | 0.256 | 0.803 |

These track-level call-wall totals include scheduler delays; observer FIFO read includes intentional 5-ms polls. The last two columns are distinct meter calls on the same PCM, not exclusive CPU costs. A long mirror-write call is consistent with backpressure but is not definitive evidence of kernel blocking.

### off-profile-01

Host return code: `0`. Profiling enabled: `True`. Trace records: 166.

Inferred per-role process CPU lower bounds (CUE S1 command signature heuristics; not exclusive native ReplayGain time): cue_stream_decoder: 4.520 s, cue_stream_encoder: 6.820 s, other_ffmpeg_or_unclassified: 0.130 s, other_or_unclassified: 10.500 s, tonepoet_app_including_native_meter: 1.950 s

Sampled process-tree CPU lower bounds: ffmpeg: 4.650 s, ffprobe: 1.160 s, metaflac: 0.050 s, sox_ng: 6.820 s, tonepoet: 11.240 s

**CUE run `eb12df6becabc180b0ecbfbc` — NOT INSTRUMENTED**

Source-pass result event count: 0; do not infer observer success from eligibility.
Evidence warnings: No source-pass profiler records; RG Off, fallback, or disabled profiling

### off-control-01

Host return code: `0`. Profiling enabled: `False`. Trace records: 0.

Inferred per-role process CPU lower bounds (CUE S1 command signature heuristics; not exclusive native ReplayGain time): cue_stream_decoder: 4.160 s, cue_stream_encoder: 6.800 s, other_ffmpeg_or_unclassified: 0.140 s, other_or_unclassified: 10.260 s, tonepoet_app_including_native_meter: 1.800 s

Sampled process-tree CPU lower bounds: ffmpeg: 4.300 s, ffprobe: 0.930 s, metaflac: 0.050 s, sox_ng: 6.800 s, tonepoet: 11.080 s

No CUE runs observed. Route fallback, RG Off, diagnostic disabled, or an uninstrumented execution are possible; **not a zero-cost result**.

### both-control-01

Host return code: `0`. Profiling enabled: `False`. Trace records: 0.

Inferred per-role process CPU lower bounds (CUE S1 command signature heuristics; not exclusive native ReplayGain time): cue_stream_decoder: 5.310 s, cue_stream_encoder: 7.710 s, other_ffmpeg_or_unclassified: 0.150 s, other_or_unclassified: 84.440 s, tonepoet_app_including_native_meter: 15.190 s

Sampled process-tree CPU lower bounds: ffmpeg: 5.460 s, ffprobe: 1.060 s, metaflac: 0.060 s, sox_ng: 7.710 s, tonepoet: 98.510 s

No CUE runs observed. Route fallback, RG Off, diagnostic disabled, or an uninstrumented execution are possible; **not a zero-cost result**.

### off-control-02

Host return code: `0`. Profiling enabled: `False`. Trace records: 0.

Inferred per-role process CPU lower bounds (CUE S1 command signature heuristics; not exclusive native ReplayGain time): cue_stream_decoder: 4.310 s, cue_stream_encoder: 6.990 s, other_ffmpeg_or_unclassified: 0.120 s, other_or_unclassified: 10.350 s, tonepoet_app_including_native_meter: 1.690 s

Sampled process-tree CPU lower bounds: ffmpeg: 4.430 s, ffprobe: 1.050 s, metaflac: 0.050 s, sox_ng: 6.990 s, tonepoet: 10.940 s

No CUE runs observed. Route fallback, RG Off, diagnostic disabled, or an uninstrumented execution are possible; **not a zero-cost result**.

### both-control-02

Host return code: `0`. Profiling enabled: `False`. Trace records: 0.

Inferred per-role process CPU lower bounds (CUE S1 command signature heuristics; not exclusive native ReplayGain time): cue_stream_decoder: 5.510 s, cue_stream_encoder: 7.750 s, other_ffmpeg_or_unclassified: 0.180 s, other_or_unclassified: 84.160 s, tonepoet_app_including_native_meter: 15.280 s

Sampled process-tree CPU lower bounds: ffmpeg: 5.690 s, ffprobe: 1.150 s, metaflac: 0.030 s, sox_ng: 7.750 s, tonepoet: 98.260 s

No CUE runs observed. Route fallback, RG Off, diagnostic disabled, or an uninstrumented execution are possible; **not a zero-cost result**.

### off-control-03

Host return code: `0`. Profiling enabled: `False`. Trace records: 0.

Inferred per-role process CPU lower bounds (CUE S1 command signature heuristics; not exclusive native ReplayGain time): cue_stream_decoder: 4.270 s, cue_stream_encoder: 6.820 s, other_ffmpeg_or_unclassified: 0.150 s, other_or_unclassified: 10.370 s, tonepoet_app_including_native_meter: 1.780 s

Sampled process-tree CPU lower bounds: ffmpeg: 4.420 s, ffprobe: 1.210 s, metaflac: 0.040 s, sox_ng: 6.820 s, tonepoet: 10.900 s

No CUE runs observed. Route fallback, RG Off, diagnostic disabled, or an uninstrumented execution are possible; **not a zero-cost result**.

### both-control-03

Host return code: `0`. Profiling enabled: `False`. Trace records: 0.

Inferred per-role process CPU lower bounds (CUE S1 command signature heuristics; not exclusive native ReplayGain time): cue_stream_decoder: 5.200 s, cue_stream_encoder: 7.770 s, other_ffmpeg_or_unclassified: 0.100 s, other_or_unclassified: 86.770 s, tonepoet_app_including_native_meter: 14.790 s

Sampled process-tree CPU lower bounds: ffmpeg: 5.300 s, ffprobe: 1.220 s, metaflac: 0.060 s, sox_ng: 7.770 s, tonepoet: 100.280 s

No CUE runs observed. Route fallback, RG Off, diagnostic disabled, or an uninstrumented execution are possible; **not a zero-cost result**.

## Interpretation guardrails

- First compare repeated uninstrumented RG Off/Both end-to-end runs, on the same host, source, conversion settings and warm/cold cache class. Do not average failed or fallback attempts into a claimed source-pass result.
- Separately compare instrumented RG Both against uninstrumented RG Both: the difference bounds diagnostic overhead but is not itself correction for all run-to-run variability.
- Distinguish `source_pass_observed=true` from policy eligibility, and require transport and observer `complete=true` plus exact PCM byte parity.
- If native push calls dominate and the standalone benchmark is similarly slow, investigate computational kernels; if mirror writes are long while the meter is fast standalone, investigate scheduling/buffering with targeted host profiling.
- A long mirror-write wall interval does NOT alone prove the FIFO is full; tokio task wake-up, thread scheduling and kernel copy can contribute. For true block/wake evidence, use OS-level sched/syscall tracing on the exact run.
- Host-process wall includes CLI startup or TUI interaction; use TonePoet's single `run_finished.elapsed_ms` when available for actual conversion wall. Never substitute interactive host wall as conversion duration.
- Linux `/proc` process-tree CPU can miss exited workers and reports lower bounds; use `perf stat` or OS profiling for authoritative CPU attribution where available.
- Do not infer an upper bound on parallel CUE speedup by subtracting RG On from Off elapsed times. Parallelism and meter work can overlap differently after architecture changes.

