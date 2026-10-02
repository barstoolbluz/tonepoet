# Brief R17 — a conditional block in the folder template fails every track

Date: 2026-10-02
Base: the supplied `tonepoet-src.tar.gz` is our `main` at gate 7244/0.

## What the user sees

Five tracks, all failing instantly:

```
01 - Shrimp Dance.flac                                              FLAC Failed
└ PlanOutputs: output concurrency admission failed: output planning
  redirected planner-authoritative album namespace from
  '/home/daedalus/temp/Hiroshi Suzuki - Cat (1975) [FLAC]' to
  '/home/daedalus/temp/Hiroshi Suzuki - Cat (1975) [FLAC] {We Release Jazz
  WRJ010LTD Reissue LP  24-96kHz}'
```

The failed status persists in the queue, so re-running the conversion shows the
same five failures and writes nothing new to the log.

## Reproduction

Deterministic, in one process, with a private config, data dir, cache and
output root. No TUI. Nothing else running.

```bash
export XDG_CONFIG_HOME=/tmp/iso/cfg XDG_DATA_HOME=/tmp/iso/data XDG_CACHE_HOME=/tmp/iso/cache
tonepoet convert '~/torrents/Hiroshi Suzuki - Cat (1975, 2021, WRJ) [LP 24-96]'/*.flac \
  --format flac --output /tmp/iso/out \
  --folder-naming '%ALBUM_ARTIST% - %ALBUM% (%YEAR%) [%FORMAT%] {%TITLE_EXTRA%  %BITDEPTH%-%SAMPLERATE%}'
```

Every track fails with the message above.

The same command with a `--folder-naming` that has no `{...}` block converts
5 of 5 and writes `Cat (We Release Jazz WRJ010LTD Reissue LP 24-96)`.

The two paths in the message differ by exactly the contents of the conditional
block. The template above is the one the user's TUI is configured with.

## Observations

- The message says "output concurrency admission failed". Nothing concurrent is
  involved in the reproduction.
- Nothing is written to the output root: no album directory, no partial
  output, no staging.
- The field occurrence left a terminal failed status persisted in the queue, so
  re-running produced the same five failures and no new log output.

## The outcome we want

An album whose folder template contains a conditional block converts
successfully, and lands in the folder the template describes once all of its
variables are known.

Where a conversion genuinely cannot proceed, the user is told what is wrong
with their album in terms they can act on, once — not an internal message about
namespace redirection repeated per track.

## State

Gate 7244 passed / 0 failed across 63 targets, zero warnings.
`tonepoet-true-peak` is gated separately and was not run today; CLAUDE.md
records it at 160/0. Reference qualification current as of 2026-10-02.

## Build capability

`cargo test -p tonepoet-pipeline` builds in seconds. The root `tonepoet` crate
peaks at 6.24 GB in one `rustc` and will OOM under a 4 GB ceiling; write those
changes uncompiled and say so. Some files are Reference-source-locked
(`reference_source_lock.rs` lists them) and changing one requires
requalification on our build host — if your change touches any, say so in the
delivery notes and we will run it.
