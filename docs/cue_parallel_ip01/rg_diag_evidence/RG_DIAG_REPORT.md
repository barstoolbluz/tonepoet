# TonePoet ReplayGain source-pass performance investigation

Instrumented measurements are inclusive wall-clock call durations, not CPU times or pure FIFO blocked time.
The transport and observer execute concurrently. **Never add their call durations as an album critical path.**

| Run | TonePoet conversion wall (s) | Host process wall (s) | CPU lower bound (s) | CUE run IDs | Diagnostic status |
|---|---:|---:|---:|---:|---|
| both-profile-01 | N/A | 89.292 | 112.160 | 1 | bcf1d85b:PASS |
| off-profile-01 | N/A | 15.431 | 23.460 | 1 | 85584f25:NOT INSTRUMENTED |
| off-control-01 | N/A | 15.493 | 23.390 | 0 | NOT INSTRUMENTED |
| both-control-01 | N/A | 89.582 | 112.800 | 0 | NOT INSTRUMENTED |
| off-control-02 | N/A | 15.515 | 23.680 | 0 | NOT INSTRUMENTED |
| both-control-02 | N/A | 89.377 | 112.560 | 0 | NOT INSTRUMENTED |
| off-control-03 | N/A | 15.684 | 23.720 | 0 | NOT INSTRUMENTED |
| both-control-03 | N/A | 89.563 | 112.730 | 0 | NOT INSTRUMENTED |

## Per-run evidence

### both-profile-01

Host return code: `0`. Profiling enabled: `True`. Trace records: 206.

Inferred per-role process CPU lower bounds (CUE S1 command signature heuristics; not exclusive native ReplayGain time): cue_stream_decoder: 5.300 s, cue_stream_encoder: 7.650 s, other_ffmpeg_or_unclassified: 0.130 s, other_or_unclassified: 84.380 s, tonepoet_app_including_native_meter: 14.700 s

Sampled process-tree CPU lower bounds: ffmpeg: 5.430 s, ffprobe: 1.030 s, metaflac: 0.040 s, sox_ng: 7.650 s, tonepoet: 98.010 s

**CUE run `bcf1d85b5c4a8b235ddf2e08` — PASS**

Source-pass RG observed: `True`. 

| Ordinal | PCM MiB | Read call (s) | Encoder write call (s) | Mirror write call (s) | Observer PCM read call (s) | Decode PCM call (s) | Loudness call (s) | Reporting peak call (s) |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 70.70 | 0.224 | 0.009 | 3.592 | 3.033 | 0.022 | 0.174 | 0.601 |
| 2 | 71.57 | 0.057 | 0.069 | 6.522 | 5.863 | 0.021 | 0.188 | 0.585 |
| 3 | 78.96 | 0.071 | 0.072 | 7.173 | 6.451 | 0.023 | 0.207 | 0.643 |
| 4 | 62.17 | 0.051 | 0.069 | 5.657 | 5.094 | 0.018 | 0.164 | 0.509 |
| 5 | 59.88 | 0.049 | 0.089 | 5.455 | 4.931 | 0.017 | 0.158 | 0.493 |
| 6 | 70.67 | 0.065 | 0.061 | 6.436 | 5.770 | 0.021 | 0.187 | 0.587 |
| 7 | 68.44 | 0.061 | 0.060 | 6.242 | 5.600 | 0.021 | 0.182 | 0.568 |
| 8 | 83.11 | 0.093 | 0.088 | 7.534 | 6.810 | 0.025 | 0.199 | 0.692 |
| 9 | 92.92 | 0.095 | 0.085 | 8.447 | 7.602 | 0.028 | 0.223 | 0.780 |
| 10 | 85.33 | 0.082 | 0.085 | 7.767 | 7.000 | 0.026 | 0.204 | 0.715 |
| 11 | 70.49 | 0.068 | 0.063 | 6.375 | 5.752 | 0.020 | 0.166 | 0.570 |
| 12 | 97.24 | 0.090 | 0.062 | 8.859 | 7.945 | 0.029 | 0.231 | 0.812 |

These track-level call-wall totals include scheduler delays; observer FIFO read includes intentional 5-ms polls. The last two columns are distinct meter calls on the same PCM, not exclusive CPU costs. A long mirror-write call is consistent with backpressure but is not definitive evidence of kernel blocking.

### off-profile-01

Host return code: `0`. Profiling enabled: `True`. Trace records: 166.

Inferred per-role process CPU lower bounds (CUE S1 command signature heuristics; not exclusive native ReplayGain time): cue_stream_decoder: 4.320 s, cue_stream_encoder: 6.880 s, other_ffmpeg_or_unclassified: 0.130 s, other_or_unclassified: 10.330 s, tonepoet_app_including_native_meter: 1.800 s

Sampled process-tree CPU lower bounds: ffmpeg: 4.450 s, ffprobe: 0.980 s, metaflac: 0.030 s, sox_ng: 6.880 s, tonepoet: 11.120 s

**CUE run `85584f250d3638e1b9aefe85` — NOT INSTRUMENTED**

Source-pass result event count: 0; do not infer observer success from eligibility.
Evidence warnings: No source-pass profiler records; RG Off, fallback, or disabled profiling

### off-control-01

Host return code: `0`. Profiling enabled: `False`. Trace records: 0.

Inferred per-role process CPU lower bounds (CUE S1 command signature heuristics; not exclusive native ReplayGain time): cue_stream_decoder: 4.310 s, cue_stream_encoder: 6.890 s, other_ffmpeg_or_unclassified: 0.090 s, other_or_unclassified: 10.330 s, tonepoet_app_including_native_meter: 1.770 s

Sampled process-tree CPU lower bounds: ffmpeg: 4.400 s, ffprobe: 1.170 s, metaflac: 0.060 s, sox_ng: 6.890 s, tonepoet: 10.870 s

No CUE runs observed. Route fallback, RG Off, diagnostic disabled, or an uninstrumented execution are possible; **not a zero-cost result**.

### both-control-01

Host return code: `0`. Profiling enabled: `False`. Trace records: 0.

Inferred per-role process CPU lower bounds (CUE S1 command signature heuristics; not exclusive native ReplayGain time): cue_stream_decoder: 5.310 s, cue_stream_encoder: 7.780 s, other_ffmpeg_or_unclassified: 0.100 s, other_or_unclassified: 84.670 s, tonepoet_app_including_native_meter: 14.940 s

Sampled process-tree CPU lower bounds: ffmpeg: 5.410 s, ffprobe: 0.990 s, metaflac: 0.090 s, sox_ng: 7.780 s, tonepoet: 98.530 s

No CUE runs observed. Route fallback, RG Off, diagnostic disabled, or an uninstrumented execution are possible; **not a zero-cost result**.

### off-control-02

Host return code: `0`. Profiling enabled: `False`. Trace records: 0.

Inferred per-role process CPU lower bounds (CUE S1 command signature heuristics; not exclusive native ReplayGain time): cue_stream_decoder: 4.460 s, cue_stream_encoder: 6.830 s, other_ffmpeg_or_unclassified: 0.150 s, other_or_unclassified: 10.420 s, tonepoet_app_including_native_meter: 1.820 s

Sampled process-tree CPU lower bounds: ffmpeg: 4.610 s, ffprobe: 1.050 s, metaflac: 0.010 s, sox_ng: 6.830 s, tonepoet: 11.180 s

No CUE runs observed. Route fallback, RG Off, diagnostic disabled, or an uninstrumented execution are possible; **not a zero-cost result**.

### both-control-02

Host return code: `0`. Profiling enabled: `False`. Trace records: 0.

Inferred per-role process CPU lower bounds (CUE S1 command signature heuristics; not exclusive native ReplayGain time): cue_stream_decoder: 5.300 s, cue_stream_encoder: 7.800 s, other_ffmpeg_or_unclassified: 0.140 s, other_or_unclassified: 84.140 s, tonepoet_app_including_native_meter: 15.180 s

Sampled process-tree CPU lower bounds: ffmpeg: 5.440 s, ffprobe: 0.930 s, metaflac: 0.030 s, sox_ng: 7.800 s, tonepoet: 98.360 s

No CUE runs observed. Route fallback, RG Off, diagnostic disabled, or an uninstrumented execution are possible; **not a zero-cost result**.

### off-control-03

Host return code: `0`. Profiling enabled: `False`. Trace records: 0.

Inferred per-role process CPU lower bounds (CUE S1 command signature heuristics; not exclusive native ReplayGain time): cue_stream_decoder: 4.560 s, cue_stream_encoder: 6.780 s, other_ffmpeg_or_unclassified: 0.140 s, other_or_unclassified: 10.520 s, tonepoet_app_including_native_meter: 1.720 s

Sampled process-tree CPU lower bounds: ffmpeg: 4.700 s, ffprobe: 1.120 s, metaflac: 0.050 s, sox_ng: 6.780 s, tonepoet: 11.070 s

No CUE runs observed. Route fallback, RG Off, diagnostic disabled, or an uninstrumented execution are possible; **not a zero-cost result**.

### both-control-03

Host return code: `0`. Profiling enabled: `False`. Trace records: 0.

Inferred per-role process CPU lower bounds (CUE S1 command signature heuristics; not exclusive native ReplayGain time): cue_stream_decoder: 5.250 s, cue_stream_encoder: 7.720 s, other_ffmpeg_or_unclassified: 0.130 s, other_or_unclassified: 84.620 s, tonepoet_app_including_native_meter: 15.010 s

Sampled process-tree CPU lower bounds: ffmpeg: 5.380 s, ffprobe: 1.010 s, metaflac: 0.060 s, sox_ng: 7.720 s, tonepoet: 98.560 s

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

