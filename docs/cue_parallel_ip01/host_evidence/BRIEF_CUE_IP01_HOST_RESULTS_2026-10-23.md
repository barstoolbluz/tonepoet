# IP-01 host results, and a measured finding that changes the plan's premise

## IP-01 qualifies

Applied to the stated baseline and compiled as received, with no corrections needed.

- Workspace gate: 7586 passed, 1 failed, zero aborted binaries, 62 targets. The one
  failure is the Reference qualification freshness test, expected because two of the three
  changed modules are source-locked. Requalification is deliberately deferred until this
  series lands.
- All twelve new `cue_ip01_` tests executed and passed, including the six named as
  must-run. Offline Python suite 15/15.
- `cargo fmt --all -- --check` fails, but it already failed on roughly 250 files at the
  baseline, these three included. This repository has never been rustfmt-clean, so that
  command cannot distinguish a good delivery from a bad one. I suggest dropping it from
  the remaining packets.

The instrumentation does what it claims. It emitted a complete, correlated record of a
real conversion on the first attempt, and the summarizer read it without adjustment.

## The measured baseline

One real single-image album: 12 tracks, 45 minutes, 16-bit 44.1 kHz stereo FLAC with a
single-FILE CUE, decoded to FLAC at source rate and depth, no DSP. Release build,
32-core host, source and destination both on local NVMe. Five repetitions per cell,
counterbalanced order. Spread was under 2% in every cell.

| ReplayGain | Whole run | S1 transport |
| --- | --- | --- |
| Off | 15.4 s | 8.7 s |
| Both | 89.3 s | 82.6 s |

Every run took the direct stream route, admitted, one physical group, no fallback. Under
`Both` the source-pass observer was selected, applied, and settled as `applied`, and no
post-output scan ran.

## The finding

I believe the plan's central performance assumption is inverted.

The plan treats S1's source-pass ReplayGain as the cheap path, something S3 cannot match
and must compensate for with an extra output scan. On this host the source-pass observer
is instead the dominant cost of a default conversion: about 74 of the 89 seconds, with
roughly 83% of elapsed time under the UI default attributable to it.

It does not read as extra work. The per-child figures are the same shape in both cells —
same tool counts, same process structure — but every process runs about ten times
slower when the mirror is active. The decoder goes from 8.7 to 82.6 seconds and each
encoder from about 0.9 to about 9 seconds. Same work, one tenth the throughput.

My reading of the transport is that the mirrored transfer loop awaits the encoder write
and then the mirror write for each 64 KiB chunk, in sequence, and the mirror is a FIFO
whose reader is the analyzer. If the analyzer consumes more slowly than the encoder, every
chunk waits on it and the whole pipeline is paced by the analyzer. That is inference from
reading the loop, not something I have proved by experiment, and you can see more than I
can here.

## What I would like

Two consequences, if the finding holds.

Parallel encoding addresses the 15-second component while 74 seconds sits elsewhere. An
S3 that made encoding entirely free would remove roughly 15% of default-settings elapsed
time, and the 1.25x gate would be unreachable with `Both` on for reasons unrelated to
ranged decoding. I believe that would read as a refutation of S3 when it is not one.

It also seems possible that this is a large part of the original 2-5x complaint, since the
UI default is `Both` and this is the default route for an ordinary lossless CUE album.

So I would like the mirror's cost investigated and, if it is a defect rather than an
inherent cost, corrected before ranged decoding is qualified. If that reordering is wrong,
or if there is a reason the serialization must stay, I would rather be told than guess.

Raw per-trial traces, summaries, environment and fixture hashes are retained on this host.
