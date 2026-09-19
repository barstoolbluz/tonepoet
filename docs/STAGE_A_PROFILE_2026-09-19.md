# Stage A pipeline profile — 2026-09-19

Zero-effect baseline profile of real conversions with the opt-in observer
(`TONEPOET_PIPELINE_BASELINE_JSONL`), on main at f05b38a (v0.5.2), release build, on the
operator's 16-thread Xeon E5-1680 v2. Sources were copies of library albums in a scratch
directory; outputs went to a second scratch directory; the library was not written to.
All runs: four workers, `--replaygain album`, `--write-log`. Raw records (gzipped) and
the driver script are in `tonepoet-pipeline/qualification/stage_a_2026-09-19/`; the
summariser is `scripts/stage_a_summary.py`.

## Runs

| Run | Source | Items | Wall |
|---|---|---:|---:|
| flac3_to_flac | Kiss *Rock and Roll Over*, Journey *Evolution*, Yes *Fragile* (30 tracks, 738 MiB) | 30 | 30.2 s |
| flac3_to_opus | same | 30 | 45.5 s |
| flac3_to_aac | same | 30 | 46.6 s |
| cue_to_flac | Steve Reich *Music for 18 Musicians*, single FLAC image + CUE (265 MiB) | 1 | 116.2 s |
| dsd_to_flac | Art Farmer & Jim Hall *Big Blues*, 4 DSF, DSD64 (1.4 GiB), album true-peak gain, standard scan | 4 | 154.2 s |

All 95 items completed; selected-vs-emitted realization: 94 records, 0 mismatches.

## Stage time summed over items (percent of summed stage time)

| Run | Convert | ReplayGain | Materialize | Publish | Metadata |
|---|---:|---:|---:|---:|---:|
| flac3_to_flac | 62.0 s (32%) | 62.2 s (32%) | 37.4 s (20%) | 24.1 s (13%) | 0 |
| flac3_to_opus | 124.1 s (41%) | 75.4 s (25%) | 35.7 s (12%) | 15.7 s (5%) | 46.5 s (15%) |
| flac3_to_aac | 144.5 s (47%) | 54.4 s (18%) | 35.8 s (12%) | 14.9 s (5%) | 55.1 s (18%) |
| dsd_to_flac | 0 (in Materialize) | 31.9 s (63%) | 4.1 s | 1.4 s | 12.6 s (25%) |

## Tool runs

| Run | Tool | Runs | Median | Direct timing of the same tool on the same file |
|---|---|---:|---:|---:|
| flac3_to_flac | ffprobe | 60 (2 per track) | 955 ms | 284 ms |
| flac3_to_flac | ffmpeg | 30 | 1.1 s | — |
| flac3_to_opus | opustags | 60 (2 per track) | 759 ms | 7 ms |
| flac3_to_aac | AtomicParsley | 30 | 775 ms | 6 ms |
| cue_to_flac | metaflac | 2 | 1.2 s | 5 ms |
| dsd_to_flac | metaflac | 8 | 723 ms | 5 ms |
| dsd_to_flac | sox | 4 | 49.7 s | — |
| cue_to_flac | ffmpeg + sox (paired) | 1 + 1 | 108.5 s | — |

Concurrency high-water: 8 tool runs and 8 ReplayGain decoders in flight with four
workers on the per-track runs; 2 certified scans on the DSD run.

Certified scan (DSD album-gain carrier, standard tier): 4 scans, 2808 MiB, 55.8 MiB/s.

ReplayGain observation: decodes every output in full at Float64 (4.5 GiB of PCM for the
738 MiB FLAC set; 1.6 s median per track).

## Findings

1. **External tool launch costs 0.7 to 1.2 s per call, regardless of the tool.** metaflac,
   opustags, and AtomicParsley take 5 to 7 ms when run directly and 720 to 1200 ms under
   tonepoet; ffprobe takes 284 ms directly and 955 ms under tonepoet. tonepoet's own
   process start is 318 ms, and each tool run goes through the script-supervisor path,
   which re-execs tonepoet as `__action-script-supervisor` and again as
   `__action-script-launcher`. With three to four tool calls per track, that is 2 to 3 s
   of overhead on a 5 to 8 s item, and it is the whole of the Metadata stage on Opus and
   AAC. This is the largest addressable cost in the profile and is not one of the audit's
   C-items.

2. **Album ReplayGain on per-track albums is not album-scoped.** Every per-track item
   measured one path in Album mode; output tags carry `REPLAYGAIN_ALBUM_GAIN` equal to
   each track's own gain (ten different values on one album). The request carries an
   `album_batch` with the expected track count, but the ReplayGain stage consumes only
   the item's own artifact paths and nothing reduces across items. The TUI dispatches
   through the same `process_queue_with_progress`. Single-image CUE albums are one item
   and are unaffected. This is what the missing test in OUTSTANDING #35a would catch.

3. **ffprobe runs twice per per-track item** (materialize and later), each a full probe.

4. **CUE image conversion is single-stream**: one ffmpeg and one sox paired for 108 s on
   a 68-minute image; the other workers idle. Expected for one item, noted for scale.

## Decisions on the audit's C-items

| Item | Evidence | Decision |
|---|---|---|
| C6 scanner high-water | DSD run: scan high-water 2 of 4 items, scans 50 s vs sox 209 s | No change; the scanner is not the bottleneck |
| C1 carrier reread | RG re-decodes each output in full; RG ≈ Convert on FLAC→FLAC | Real but second-order behind finding 1; revisit after tool-launch fix |
| C3 ReplayGain tail | No album tail exists because album RG is per-item (finding 2) | Blocked on the album-RG defect; measure after it is fixed |
| C4 metadata/remux bytes | Metadata stage is entirely tool-launch overhead | Moot until finding 1 is addressed |
| C2 effect fusion | No effect workloads exist; effects are not user-reachable | Still blocked; this profile is its zero-effect baseline |

## Recommended order

Fix finding 2 (correctness). Then measure and reduce the tool-launch path (finding 1),
which likely halves per-track wall time on lossless conversions. Then re-run this profile
and revisit C1 and C3.
