# Issues #77 and #78: idle TUI CPU, and per-call tool supervision cost

**Source:** `TonePoet_SNAPSHOT_6ad6bb1_2026-10-27.tar.gz`
sha256 `e8cab5cf4e5066fc82e57b8a1bee0cd679c12360e235ae68d377b189a5cb8463`,
branch `issue78-evidence`, which is `main` plus a documentation-only update to #78.

## Outcomes wanted

**#78.** A supervised tool call costs about what the tool itself costs, and
supervising a running tool costs little CPU however many processes the host is
running. The containment guarantee is not weakened, and conversion output is
unchanged.

**#77.** An idle TUI costs approximately nothing, and stays as responsive to input,
resize and background progress as it is now. The 2.0% observation is either
explained, or the evidence that would explain it is named.

The diagnosis, and any fix, are yours to judge.

## Caveats

Everything below was measured by me on one host: AMD Threadripper 7970X, 64
threads, about 940 processes, Linux 6.12. I have already been wrong several times
in this investigation. strace distorted my first timings, I miscounted the ffprobe
calls, and I proposed a cause that did not survive testing. The corrected account
of those is in #78. Treat these as observations, not conclusions, and my reading
of the code as shallow next to yours.

## #78

The facts are in the #78 entry of `docs/OUTSTANDING_ISSUES.md`, under the
2026-10-27 update. In short:

- An S1 CUE conversion of 12 tracks makes 38 short ffprobe and metaflac calls, one
  at a time. Each costs about 100 ms more than the tool itself: about 55 ms before
  exec and about 40 ms after exit. That is 3.5–3.8 s per run. S3 makes the same 38
  calls.
- In isolation on this host, a `cgroup.procs` write costs 21–27 ms when writes are
  spaced 100 ms or more apart, and 0.08 ms back to back.
- With 3,000 idle processes added to the host, after-exit time per call rose from
  ~40 to 91–110 ms. Before-exec time and the tools' own runtimes did not change.
- Forked helper processes used 11.0 s of CPU per S1 run (20.8 s with the 3,000
  extra processes). The audio tools used 11.9 s.
- Roughly half of the before-exec time is unattributed.

## #77

The entry describes a session 2.2 days old, measured at 2.0% of a core in two
windows of 10 s and 20 s. It cites a lifetime total of one hour of CPU as
consistent with that rate. That total includes whatever else the session did,
including any conversions, so it says nothing about the idle rate.

Everything below comes from a fresh instance using empty XDG directories, the
user's `config.toml` and no database, idle on Browse:

| Terminal | Cells | Idle CPU |
| --- | --- | --- |
| 100×30 | 3,000 | 0.37% |
| 200×50 | 10,000 | 0.77% |
| 248×75 (the user's) | 18,600 | 1.1% |
| 400×110 | 44,000 | 2.27% |

- At 200×50, Browse is about 1.0% and the other tabs 0.5–0.8%. Directory size, a
  focused album with cover art, and the user's config made no measurable
  difference.
- Over the first 35 minutes of an idle soak the rate stayed at 1.10–1.19%, with
  threads (65), RSS (~100 MB) and open descriptors (17) all constant.
- strace of the main thread over 20 s: 196 `epoll_wait` calls with a 100 ms
  timeout, all timing out. After each one, the thread opens `/dev/tty`, makes a
  `TIOCGWINSZ` ioctl, writes 25 bytes (`ESC[39m ESC[49m ESC[59m ESC[0m ESC[?25l`)
  and closes the tty. Separately, about every 5 s it locks and checks a
  queue-scope lease file.
- perf, 60 s, frame-pointer build, 248×75, Browse, 2,145 samples. Shares of the
  process's samples, inclusive:

| Frame | Share |
| --- | --- |
| `Terminal::draw` | 98% |
| `draw_ui` | 68% |
| `draw_browse_screen` | 65% |
| `Paragraph::render` | 57% |
| `LineTruncator::next_line` | 38% |
| `draw_browse_list` | 32% |
| `Graphemes::next` | 29% |
| `Buffer::diff` | 22% |
| `draw_header` | 6% |

- The gap between ~1.1% here and the 2.0% observed is unexplained. The user's
  database today holds 40 Completed and 1 Interrupted queue items, in a scope whose
  lease names a process that no longer exists. Its state on 2026-10-24 is unknown.
  I did not run a TUI against it, because adopting the interrupted item could start
  a real conversion.

## Facts about this tree

- Files listed in `REFERENCE_COMMON_SOURCE_PATHS` invalidate the installed
  Reference qualification when they change. `src/convert/script_supervisor.rs`,
  `src/convert/pipeline/tool.rs` and `src/tui/` are not among them. `stages.rs` and
  `track_executor.rs` are.
- On rustc 1.93.1, `-C force-frame-pointers=yes` crashes LLVM's loop vectorizer
  while compiling `tonepoet-true-peak`.
