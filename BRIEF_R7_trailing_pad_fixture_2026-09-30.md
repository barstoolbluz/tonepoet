# Brief R7 — one fixture in your corrective never reaches its assertion

Date: 2026-09-30
Base: `main` @ `03a62eb` with the R6 corrective applied

The corrective works. The production validator now accepts the exact 228-byte
repro it previously rejected, the qualification harness gets past dither
discovery, and the cell the field conversion needs qualifies and passes. One of
your own tests fails.

---

## The failure

`tonepoet-pipeline::w64::tests::ssrc_trailing_zero_pad_does_not_weaken_exact_frame_extent`

It asserts the error message contains `expected 36`. The message is:

```
alignment after chunk at offset 80 extends beyond the declared file
```

## Measured geometry

The fixture is 24-bit mono, 11 real frames, validated against an expectation of
12. Its base length is 137 bytes; the data chunk sits at offset 80 and its
ordinary eight-byte aligned end is 144. The helper appends the requested
trailing bytes and rewrites the root size.

Running the same fixture across pad sizes:

| trailing bytes | file length | result |
|---|---|---|
| 0 | 137 | `data chunk declares 33 payload bytes, expected 36` |
| 6 | 143 | `alignment after chunk at offset 80 extends beyond the declared file` |
| 7 | 144 | `data chunk declares 33 payload bytes, expected 36` |
| 8 | 145 | SSRC path `expected 36`; exact path `1 undeclared/truncated bytes remain at offset 144` |

The test requests 6, which lands one byte short of alignment. Your new rule
requires the file to reach at least the aligned end of the final data chunk, so
validation stops on alignment and never reaches the frame-extent comparison the
test is named for.

Two further facts from the same measurement. A pad of 7 is the smallest value
where both the exact and SSRC paths agree on the frame-extent error. A pad of 0
already produces the asserted message, so the trailing pad is not currently
load-bearing for this assertion at all — whatever value you choose should be
large enough that the pad is actually what is being tested.

## Required

The test exercises what its name claims: a trailing zero pad admitted by the
SSRC rule does not weaken exact frame-extent checking. Choose the pad value, or
change the assertion, as you judge correct — the numbers above are measurements,
not a recommendation, and this test pins certified-path behavior so it was left
alone here.

---

## Everything else in the corrective verified

- `cargo check --workspace --all-targets` clean, no warnings.
- The production validator now accepts the shipped 228-byte repro. Confirmed by
  running `validate_ssrc_w64_pcm` against that exact file before and after:
  previously `4 undeclared/truncated bytes remain at offset 224`, now accepted.
- Harness self-tests 18/18.
- `w64` unit tests 19/20, the one failure being the fixture above.
- The harness now clears dither-capability discovery and characterizes cells.
- `high:192000:44100:2:16:99:triangular`, the cell the field conversion needs,
  qualifies and **passes**.

## The grid is now measurable

This was unanswerable before, because the harness never started.

```
execution_wall_seconds      2.095
fixed_overhead_wall_seconds 1.167
wall_seconds_per_cell       2.095
```

4,536 cells at 2.1 s is about 2.6 hours single-threaded. The harness accepts
`--jobs`, and the build host has 32 threads, but each SSRC process is itself
multithreaded so it will not scale linearly. We will trial `--jobs` on a small
subset before committing to a full run.

## Your single-owner design holds

`w64.rs` is the only authority for whether an SSRC tail is admissible, and the
Python harness reaches it through the compiled `--inspect-ssrc-w64` helper
rather than reimplementing the rule. That is what the previous brief asked for,
and it is why the production validator and the harness agreed in testing here
rather than having to be checked separately.

## A correction on our side

You noted that our brief named base `main @ e90936e` while our evidence file
named `ae66104`. The evidence header was generated from `HEAD` at packaging
time, after two of our own commits; the brief's base was correct. Both now
resolve to the same tree.

## Build capability

`cargo test -p tonepoet-pipeline` builds in seconds and would have caught this
fixture. Reproducing the SSRC side additionally needs the `ssrc` binary; a
prebuilt `ssrc` and `sox_ng` closure can be supplied on request.
