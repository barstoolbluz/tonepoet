# Brief R8 — the other half of the Wave64 defect

Date: 2026-09-30
Base: the supplied `tonepoet-src.tar.gz` is authoritative. It is our local
`main` with your R6 and R7 correctives applied and the gate at 7204/0.

R6 and R7 are accepted and green. The SSRC grid now runs. It still cannot be
promoted, because there are two Wave64 defects and R6 addressed one of them.

---

## What the grid run found

Trialling eight representative cells, two failed. Both report:

```
output validation failed: production SSRC Wave64 validator rejected carrier:
  alignment after chunk at offset 80 extends beyond the declared file
```

Every gain point in those cells failed, and `observed_maximum_abs_error_lsb` is
`inf` — no measurement was possible at all.

This is the mirror of the case R6 fixed. R6 handled a carrier that runs *over*
the eight-byte aligned end of its final data chunk by four bytes. These
carriers fall *short* of that aligned end, so the rule R6 added — that a file
must reach at least the ordinary aligned end before its zero tail can be
excused — rejects them.

The rejection comes from production, not the harness. Your single-owner design
is working exactly as intended; it is simply reporting a case the policy does
not yet cover.

## The two defects are confirmed, not inferred

We re-ran cells sampled from each historical failure class against the current
build:

| historical class | sampled | result now |
|---|---|---|
| `w64_truncated_chunk_header` | 4 | 4 pass — R6 fixed this class |
| `w64_alignment_padding` | 4 requested, 3 returned | 3 fail, all with the short-alignment message |

The fourth alignment cell, `high:44100:48000:1:16:99:triangular`, produced no
result, most likely a rate-unsupported dither the harness skips.

So the class names in the superseded run map onto real, current behavior rather
than onto a guess about naming. The 4,536-cell run separates them cleanly:

| class | cells | shape | status |
|---|---|---|---|
| `w64_truncated_chunk_header` | 345 | runs over the aligned end | fixed by R6 |
| `w64_alignment_padding` | 447 | falls short of the aligned end | still failing |

The 447 is the historical count for that class, not a measured count under the
current build. It is the expected scale of the remaining work, given that every
sampled cell from that class still fails the same way.

It is not a single bit depth. The 447 break down as 234 at 8-bit, 129 at
24-bit, 84 at 16-bit — whichever payload sizes leave the data chunk end off the
eight-byte boundary, which depends on frames, channels and bytes per sample.

Our two trial failures were `high:192000:44100:2:24:99:triangular` and
`insane:192000:44100:1:24:99:triangular`, while `long:96000:44100:2:24:99:triangular`
passed, consistent with that.

## Why we did not run the full grid

Roughly 447 cells would fail, and `promote_ssrc_true_peak_terminal.py` refuses
any report containing a non-passing cell. The run would take an hour and
produce evidence that cannot be promoted. Say if you would rather have a
complete current failure census anyway.

## Required

The SSRC true-peak grid qualifies without Wave64 carrier rejections, so the
registry can be promoted and a 192 kHz to 44.1 kHz Int16 TPDF album converts.

Whatever admits a short-of-alignment carrier keeps the properties R6
established: one owner in `w64.rs`, the harness reaching it through the
compiled helper rather than reimplementing it, the generic exact validator
still strict, and zero-only tails still verified byte by byte. A carrier that
is genuinely truncated — as opposed to merely unpadded — must still be refused.

## Grid cost, now measured

`--jobs` scaling on eight cells:

| jobs | wall per cell |
|---|---|
| 1 | 2.657 s |
| 8 | 0.736 s |
| 16 | 0.758 s |

Eight is the sweet spot; sixteen is no better, as each SSRC process is itself
multithreaded. At 0.74 s per cell the full 4,536-cell grid is about 56 minutes
on this host.

## State

Gate 7204 passed / 0 failed, zero warnings. `w64` suite 20/20. Harness
self-tests 18/18. The harness clears dither discovery and characterizes cells.
`high:192000:44100:2:16:99:triangular` — the cell the field conversion needs —
qualifies and passes. The registry remains empty; nothing has been promoted.

## Build capability

`cargo test -p tonepoet-pipeline` builds in seconds. Reproducing the carrier
side needs the `ssrc` binary; a prebuilt `ssrc` and `sox_ng` closure can be
supplied on request.
