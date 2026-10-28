# Issues #77 and #78: host results for your delivery

**Delivery:** `TonePoet_ISSUES_77_78_FULL_SOURCE_2c05497ba6b4.tar.gz`. The
outcomes in the original brief are unchanged.

Everything here was measured by me on one host and is subject to the same
caveat as before. One example: I first reported that clone3 placement removed
the cgroup migration. It does, but the launcher fails right after, as described
below.

## Build and gate

- It compiled as received on rustc 1.93.1. Your focused tests pass.
- I made one local change. The fixture in
  `linux_descendant_snapshot_finds_grandchildren_without_host_scan` spawned
  `/bin/sh` without `.stdin(Stdio::null())`, which failed
  `subprocess_stdin_convention`. I added it.
- The gate came back 7,631 passed and 1 failed. The failure is
  `installed_reference_qualification_binds_the_current_locked_sources`, which
  was already failing before your delivery. No test binary aborted.

## #78

**Cgroup containment does not arm on this host, in your tree or the input
tree.** `clone3(CLONE_INTO_CGROUP)` succeeds. The launcher's
`unshare(CLONE_NEWCGROUP)` then returns EPERM, the launcher exits with status
70, and the supervisor falls back to the child subreaper. The outcome's warning
says "delegated cgroup launcher setup failed before script release". Neither
tree makes any other `unshare` call.

- **Host:** unprivileged user, a tmux scope under `user@1000.service`, Linux
  6.12.85, cgroup2 mounted with `nsdelegate`.
- **Tests:** the fixture in `tests/conversion_action_runscript_containment.rs`
  gets the same fallback under `Auto`. So neither the gate nor any run below
  exercised your change to cgroup-backed monitoring.

**Measurements.** All are untraced A/B runs, input tree then yours, on the same
12-track album.

| Measure | Input | Yours |
| --- | --- | --- |
| S1 wall time, two runs | 16.7 / 16.6 s | 14.6 / 14.5 s |
| S3 wall time | 10.2 s | 8.5 s |
| Per call, before exec (median) | ~57 ms | ~19 ms |
| Per call, after exit (median) | ~40 ms | ~38 ms |
| ffprobe runner time (process ~93 ms) | ~193 ms | ~151 ms |
| metaflac runner time (process ~14 ms) | ~103 ms | ~68 ms |
| S1 with ~6,950 host processes: wall | 21.3 s | 14.7 s |
| S1 with ~6,950 host processes: after exit | 99 ms ffprobe, 172 ms metaflac | 38–39 ms |
| Whole-tree CPU per S1 run (`getrusage`) | 72.0 s, of which system 34.3 s | 42.0 s, of which system 10.4 s |

The whole-tree CPU runs overlapped an unrelated workload at load average 30–40.

**Output.** The file set, the audio MD5 and tags of all 12 FLACs, and the CUE
bytes are identical between the trees, for S1 and S3.

**Containment behaviour.** I ran four fixtures, each under `Auto` and under
forced fallback, on both trees:

- a TERM-ignoring `setsid` child left after the leader exits;
- a child that would finish on its own;
- timeout with a double-fork;
- cancellation with a double-fork.

Outcomes and event sequences were identical between the trees, and no process
survived. On this host `Auto` was itself the fallback.

## #77

Idle on Browse, isolated XDG directories, CPU over 30 s:

| Terminal | Input | Yours |
| --- | --- | --- |
| 100×30 | 0.30% | 0.03% |
| 200×50 | 0.73% | 0.07% |
| 248×75 | 1.13% | 0.03% |
| 400×110 | 2.20% | 0.07% |

- **Longer windows.** Over 120 s yours measured 0.075% at 248×75, and 0.39%
  and 0.37% at 400×110. At 400×110 all of it was on the main thread, and draws
  ran at exactly one per 5 s.
- **Profile.** perf over 60 s at 400×110 took 378 samples. About half are in
  ratatui buffer and draw symbols. The rest include `ensure_convert_hdcd_scan`,
  `getenv`, `Instant::elapsed` and `try_recv`.
- **Interaction.**
  - Keypress to screen: 6–9 ms in both trees.
  - Async probe result shown: 318 ms in yours, 316 ms in the input tree.
  - A conversion queued from the TUI passed through the same sequence of Queue
    states in both trees, with each state within about 0.1 s of the input tree.
  - Not exercised: per-track progress rows (neither tree showed them on this
    route), resize, mouse and image previews.
- **The 2.0% observation** has not been investigated further.
