# Poll counters, and a correction to my CPU claim

## The counters

Both profiled runs, twelve tracks, 911 MiB of PCM each.

| Counter | Run 1 | Run 2 |
| --- | --- | --- |
| WouldBlock polls during transfer | 14,139 | 14,151 |
| no-writer polls | 81,578 | 81,852 |
| total poll sleep | 484.89 s | 486.60 s |
| observer PCM read | 71.85 s | 71.93 s |
| observer PCM decode | 0.27 s | 0.29 s |

The hypothesis holds, and the arithmetic closes.

911 MiB divided by the 64 KiB pipe capacity is 14,576 chunks, against 14,139 measured
WouldBlock polls. At 5 ms each that is 70.70 s of sleeping inside a 71.85 s read. The
actual reading is about **1.16 s**; the rest is the sleep. Both runs agree to two decimal
places.

That caps throughput at roughly one pipe-load per 5 ms, near 12.8 MB/s, and 911 MiB at
that rate is 74.7 s against the 74 s penalty measured end to end. I believe this accounts
for the whole of it.

The large no-writer count appears to be the other eleven observers waiting their turn
while one track transfers, since the group runs sequentially. It inflates the poll-sleep
total well past elapsed time, as you said it would.

## A correction

I previously reported that the tonepoet process burns 87 extra CPU seconds and is
therefore not idle-blocking. **I no longer trust that number and I should not have stated
it that firmly.**

Reading the process's own `utime` and `stime` directly until exit gives 1.5 s with
ReplayGain off and 14.8 s with it on — about 13 s more, not 87, and close to what the
meters alone cost. That is consistent with idle blocking, not against it.

The two methods disagree and I could not settle which is right with what is installed
here. There is no `perf` or `strace` on this host.

- The bundle's sampler, and a shell `time` of the whole job, both report about 114 s of
  CPU for the Both run against 26 s for Off, with the growth mostly in system time.
- Direct reads of the main process give 14.8 s against 1.5 s.
- Summing the sampler's per-process figures gives about 29 s, which matches neither.

The sampler can only be a lower bound, since the run spawns 52 short-lived tools that
periodic sampling will mostly miss, so I would expect it to *under*-report rather than
over-report. I cannot explain the system time.

Stack samples taken during a Both run do show `nanosleep` under the mirror observer, which
is consistent with the poll loop, but six samples is not a profile.

So your assessment stands unchanged: where the extra CPU goes is not proved. Only the
wall-time explanation is established, and it does not depend on the CPU question.

## On the proposed change

Replacing the fixed delay with readiness-driven waiting looks right to me, and the
measurement predicts what it should recover: about 70 of the 71.9 s the observer spends
reading.

I would rather you made that change than I did. It touches cancellation, EOF validation
and fallback semantics, and you can see the invariants around them better than I can.

## Third run, and the CPU question resolved

The counters were measured a third time on a fresh profiled run. All three agree.

| | run 1 | run 2 | run 3 |
| --- | --- | --- | --- |
| WouldBlock polls | 14,139 | 14,151 | 14,132 |
| observer PCM read | 71.85 s | 71.93 s | 71.82 s |
| of which 5 ms sleeps | 70.70 s | 70.75 s | 70.66 s |
| actual read work | 1.16 s | 1.17 s | 1.16 s |
| meters, loudness + true-peak | 9.84 s | 10.16 s | 9.86 s |

I also measured CPU directly rather than through the sampler, three repetitions per mode,
reading each process's own counters.

| | ReplayGain Off | ReplayGain Both |
| --- | --- | --- |
| convert process, own CPU | 1.6 s | 14.8 s |
| ffmpeg, sox, ffprobe, metaflac combined | 12.0 s | 13.7 s |
| total | 13.6 s | 28.5 s |
| wall | 15.0 s | 88.5 s |

The encoders do not spin: their CPU is essentially unchanged between modes. The convert
process gains about 13 s, which is close to what the meters cost, and the run spends
roughly 60 of its 88 s idle.

**So there is no large unexplained CPU, and my earlier claim was simply wrong.** The
behaviour is idle blocking, exactly as the 5 ms sleep predicts, and your hypothesis needs
no CPU mystery to support it.

One loose end I could not close. A shell `time` of the whole job reports 25.6 s and
113.8 s of CPU for the two modes, reproducibly, and the bundle's sampler agrees with it.
Direct per-process reads give 13.6 s and 28.5 s. The shell timer is accurate on control
workloads, so I cannot dismiss it, and I cannot reconcile it either. I trust the direct
reads because each one is a counter belonging to a named process I can point at, but I am
recording the disagreement rather than hiding it.

Unrelated, noticed while sampling: a `tonepoet tui` left running here has used an hour of
CPU over 2.2 days, about 2% of a core while idle and doing nothing. Probably its own
issue, and not something I have looked into.
