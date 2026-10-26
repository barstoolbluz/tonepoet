# Review request: I promoted the S3 route to default on

## The ask

Review this change adversarially and say whether it should stand, be amended, or be
reverted. It is a change to default behaviour for every user, it was written on the build
host rather than in an implementation session, and the evidence behind it comes from one
machine. Treat it as a candidate, not a decision already taken.

If it should stand, I would also like IP-06's formal record: whatever the packet requires
as its decision artifact, written against this evidence.

## What changed

Two functions in `stages.rs`, and assertions added to an existing test. The full patch is
attached as `S3_PROMOTION.patch`, and the tree is the snapshot named below.

`cue_s3_pilot_requested` previously returned true only for the exact value `1`. It now
returns false only for the exact trimmed value `0`. The parsing moved into
`cue_s3_pilot_enabled_for(Option<&str>)` so the opt-out spelling could be asserted without
mutating the process environment, which this suite's known contention makes unsafe.

Nothing else moved. The preflight is untouched.

## Why I judged it safe

The gate decides nothing by itself. Every eligibility fact stays in
`cue_s3_pilot_preflight`, which refuses, by its own named reasons: single-track albums,
non-identity PCM or registered processing, multiple or non-contiguous physical files,
non-exact CUE stream sources, a decode authority differing from the image, platforms
without the supervised two-process runner, and hosts that cannot seat two live children.
A refused album keeps S1 silently.

So promotion widens *which* albums reach the preflight, not what the preflight permits.

## Evidence

140 paired runs, seven workloads, five counterbalanced repetitions each, matched
ReplayGain Off and Both, fresh destination per run, zero nonzero exits. Full table and
per-run CSV are in the snapshot under
`docs/cue_parallel_ip01/ip06_host_measurement/`.

| workload | RG Off | RG Both |
| --- | --- | --- |
| 1 track | 1.01x | 0.98x |
| 3 tracks | 1.31x | 1.45x |
| 12 tracks | 1.44x | 1.78x |
| 30 tracks | 1.33x | 1.70x |
| 12 tracks, 24/44.1 | 1.45x | 1.71x |
| 12 tracks, 24/96 | 1.58x | 1.79x |
| real album, 12 tracks | 1.38x | 1.69x |

The single-track row did not clear the threshold, and the preflight already refuses that
case with `not_one_complete_multi_track_cue_album`; a trace with the gate on shows
`route=s1`. That row therefore compares two S1 runs. On this host there is no admitted
workload that loses.

Correctness, every cell and both modes: per-track decoded audio MD5 and the complete tag
set identical to S1, destination file sets identical, and no dotfile, manifest, temporary,
lock, staging directory or evidence file beneath any of the 140 destinations. SIGINT
mid-run publishes nothing and orphans nothing; a corrupt source frame exits nonzero and
publishes nothing.

After the change: unset routes s3, `0` and `" 0 "` route s1, `1` and other values route
s3. Default-route output matches explicit-S1 output at 3, 12, 30 and 12x24/96 tracks on
audio, tags and file set. Gate 7627 passed, 1 failed, 0 aborted binaries, the failure
being the deferred Reference drift.

## What I am least sure of

**Storage class is now measured, and my earlier NOT RUN was wrong.** I reported no
spinning disk was available; in fact the user's library is a spinning disk reached over
`fuse.sshfs`, and I had been converting from a staged NVMe copy of it. Re-measured
against the real source, cold, on four commercial albums of 6 to 13 tracks, alternating
which route touches the file first: neither route shows a measurable cold penalty, S3
shows no disproportionate one, and S3 is faster on every album at 1.58x to 1.67x. Table
in `docs/cue_parallel_ip01/ip06_host_measurement/COLD_SSHFS.md`.

What this still does not isolate is raw seek behaviour on a directly attached drive under
memory pressure, since sshfs readahead and the remote host's page cache sit in between.

**One host.** 32 cores, 63 threads, NVMe, ffmpeg 7.1.3. Core count and FFmpeg build are a
single point. The capacity refusal covers hosts too small to seat two children, but I have
not tested a genuinely small machine.

**The opt-out is an environment variable**, which is a developer affordance rather than a
user-facing setting. If a user on unsuitable hardware needs S1, they currently need to
know a variable name. Whether that is acceptable, or whether this needs a config key, is
a product decision I did not make.

**I may have misjudged the division of labour.** The series convention is that production
behaviour changes come from the implementation session and I fix mechanical breakage.
I wrote this one because the policy turned out to be a default flip with the real
decisions already encoded in the preflight. If that reasoning is wrong, say so and I will
revert it.

## Input

`TonePoet_SNAPSHOT_4d6b04c_s3promote_2026-10-25.tar.gz`, SHA-256
`1f1fcf261ac35aa585580489a64989541a71916af58f88bc592f0a91aa196d2e`. Branch `s3-promote`, two commits
ahead of `main` at `379bdcc` (v0.5.6), not merged.
