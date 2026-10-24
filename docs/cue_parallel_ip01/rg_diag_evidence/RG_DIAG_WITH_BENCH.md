# TonePoet ReplayGain source-pass performance investigation

Instrumented measurements are inclusive wall-clock call durations, not CPU times or pure FIFO blocked time.
The transport and observer execute concurrently. **Never add their call durations as an album critical path.**

| Run | TonePoet conversion wall (s) | Host process wall (s) | CPU lower bound (s) | CUE run IDs | Diagnostic status |
|---|---:|---:|---:|---:|---|
| both-profile-01 | N/A | 89.292 | 112.160 | 1 | bcf1d85b:PASS |

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

## Standalone native-meter benchmark

| Mode | Repeats | Median wall (s) | Loudness call (s) | Reporting peak call (s) | Wall / programme |
|---|---:|---:|---:|---:|---:|
| both (s32le, 44100 Hz, 2ch) | 5 | 10.291 | 2.380 | 7.394 | 0.004× |

Standalone PCM file reads, decoding, OS caching, finalization and meter costs are not equivalent to pipelined FIFO backpressure.

## Standalone native-meter benchmark

| Mode | Repeats | Median wall (s) | Loudness call (s) | Reporting peak call (s) | Wall / programme |
|---|---:|---:|---:|---:|---:|
| loudness (s32le, 44100 Hz, 2ch) | 5 | 2.882 | 2.365 | 0.000 | 0.001× |

Standalone PCM file reads, decoding, OS caching, finalization and meter costs are not equivalent to pipelined FIFO backpressure.

## Standalone native-meter benchmark

| Mode | Repeats | Median wall (s) | Loudness call (s) | Reporting peak call (s) | Wall / programme |
|---|---:|---:|---:|---:|---:|
| peak (s32le, 44100 Hz, 2ch) | 5 | 7.923 | 0.000 | 7.405 | 0.003× |

Standalone PCM file reads, decoding, OS caching, finalization and meter costs are not equivalent to pipelined FIFO backpressure.

## Standalone native-meter benchmark

| Mode | Repeats | Median wall (s) | Loudness call (s) | Reporting peak call (s) | Wall / programme |
|---|---:|---:|---:|---:|---:|
| decode (s32le, 44100 Hz, 2ch) | 5 | 0.514 | 0.000 | 0.000 | 0.000× |

Standalone PCM file reads, decoding, OS caching, finalization and meter costs are not equivalent to pipelined FIFO backpressure.

## Interpretation guardrails

- First compare repeated uninstrumented RG Off/Both end-to-end runs, on the same host, source, conversion settings and warm/cold cache class. Do not average failed or fallback attempts into a claimed source-pass result.
- Separately compare instrumented RG Both against uninstrumented RG Both: the difference bounds diagnostic overhead but is not itself correction for all run-to-run variability.
- Distinguish `source_pass_observed=true` from policy eligibility, and require transport and observer `complete=true` plus exact PCM byte parity.
- If native push calls dominate and the standalone benchmark is similarly slow, investigate computational kernels; if mirror writes are long while the meter is fast standalone, investigate scheduling/buffering with targeted host profiling.
- A long mirror-write wall interval does NOT alone prove the FIFO is full; tokio task wake-up, thread scheduling and kernel copy can contribute. For true block/wake evidence, use OS-level sched/syscall tracing on the exact run.
- Host-process wall includes CLI startup or TUI interaction; use TonePoet's single `run_finished.elapsed_ms` when available for actual conversion wall. Never substitute interactive host wall as conversion duration.
- Linux `/proc` process-tree CPU can miss exited workers and reports lower bounds; use `perf stat` or OS profiling for authoritative CPU attribution where available.
- Do not infer an upper bound on parallel CUE speedup by subtracting RG On from Off elapsed times. Parallelism and meter work can overlap differently after architecture changes.

