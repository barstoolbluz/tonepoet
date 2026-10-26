# IP-06 host measurement — paired S1 vs S3 matrix

2026-10-25. Reference host: AMD Threadripper 7970X, 63 threads, 128 GB, local NVMe ext4
for source, staging and destination. ffmpeg 7.1.3, flac 1.5.0, sox_ng 14.8.0.1, release build.
Each cell five counterbalanced paired repetitions, fresh destination per run, 140 runs, zero
nonzero exits. Wall clock is process start to exit.

| workload | RG | S1 median | S3 median | speedup | S1 spread | S3 spread |
| --- | --- | --- | --- | --- | --- | --- |
| 1 track, 16/44.1 | off | 9.49 s | 9.43 s | **1.01x** | 0.28 s | 0.27 s |
| 1 track, 16/44.1 | both | 16.50 s | 16.78 s | **0.98x** | 0.38 s | 0.16 s |
| 3 tracks, 16/44.1 | off | 10.73 s | 8.18 s | **1.31x** | 0.20 s | 0.13 s |
| 3 tracks, 16/44.1 | both | 17.93 s | 12.40 s | **1.45x** | 0.37 s | 0.27 s |
| 12 tracks, 16/44.1 | off | 15.52 s | 10.74 s | **1.44x** | 0.30 s | 0.29 s |
| 12 tracks, 16/44.1 | both | 22.87 s | 12.83 s | **1.78x** | 0.42 s | 0.23 s |
| 30 tracks, 16/44.1 | off | 25.39 s | 19.11 s | **1.33x** | 0.29 s | 0.25 s |
| 30 tracks, 16/44.1 | both | 33.13 s | 19.44 s | **1.70x** | 0.46 s | 0.18 s |
| 12 tracks, 24/44.1 | off | 18.49 s | 12.74 s | **1.45x** | 0.51 s | 0.75 s |
| 12 tracks, 24/44.1 | both | 23.89 s | 14.01 s | **1.71x** | 0.75 s | 0.54 s |
| 12 tracks, 24/96 | off | 30.55 s | 19.36 s | **1.58x** | 0.17 s | 0.14 s |
| 12 tracks, 24/96 | both | 36.13 s | 20.22 s | **1.79x** | 1.19 s | 0.51 s |
| real album, 12 tracks 16/44.1 | off | 15.57 s | 11.18 s | **1.39x** | 0.28 s | 0.21 s |
| real album, 12 tracks 16/44.1 | both | 19.45 s | 11.47 s | **1.70x** | 0.22 s | 0.15 s |

## The single-track cell is not a loss

At one track S3 shows 0.98x, but the pilot never ran. Its preflight refuses with
`not_one_complete_multi_track_cue_album` because `source.tracks.len() < 2`, and the trace
confirms `route=s1` with the gate enabled. That row therefore compares two S1 runs and the
difference is noise.

Consequently there is no crossover to find in the track-count dimension. Every workload
the pilot actually admits is faster, the smallest margin being 1.31x at three tracks with
ReplayGain off and 1.45x with it on.

## Correctness

For every cell, in both ReplayGain modes, S3 output was compared against S1 output:
decoded audio MD5 per track and the complete tag set, not only the ReplayGain tags.
All identical, 82 tracks per mode across the seven cells.

Destination file sets are identical to S1's in every cell, and no dotfile, manifest,
temporary file, lock, staging directory or evidence file appeared beneath any of the 140
destinations.

## Adverse cases

| case | result |
| --- | --- |
| SIGINT mid-conversion, 30-track pilot run | exit 130, nothing published, destination empty, no orphaned child processes |
| corrupt source frame, pilot route | exit 1, nothing published |

An earlier count of six surviving `ffmpeg`/`sox` processes after cancellation was a false
positive in the probe: the pattern matched an unrelated `sox_ng` test suite running
elsewhere on the host and the session's own processes. No tonepoet child was orphaned.

## ReplayGain path actually taken

Traced rather than assumed. On a 12-track album with the gate on, `route=s3`,
`route_reason=exact_flac_pilot_opt_in`, `source_pass_selected=True` and
`post_output_worker_observed=False`: the concurrent source-range scan is in use, not a
post-output rescan.

## Not covered

No spinning disk was available, so the HDD seek-contention cell is **NOT RUN**. Every
figure here is NVMe. The plan treats storage class as a reason to withhold auto-admission,
and nothing measured here speaks to it.

Only one host, so core count, memory bandwidth and FFmpeg build are a single point.

The admission policy itself is not implemented here. This document is evidence for that
decision, not the decision.
