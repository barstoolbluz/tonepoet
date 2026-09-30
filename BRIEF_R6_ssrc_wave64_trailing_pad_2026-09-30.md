# Brief R6 — the SSRC true-peak grid cannot be qualified

Date: 2026-09-30
Base: `main` @ `e90936e`

A field conversion refused because the SSRC true-peak terminal is not
commissioned. That refusal is correct; the registry ships empty by design. The
problem is that the commissioning run cannot be performed on this build.

---

## What happens

`qualify_ssrc_true_peak_terminal.py` dies in `discover_ssrc_dither_capabilities`,
before any cell is characterized:

```
ValueError: Wave64 failed exact parsing (truncated Wave64 chunk header) and narrow
SSRC two-byte trailing-pad parsing (truncated Wave64 chunk header)
```

44.1 kHz sorts first among the probe rates, so nothing runs at all.

## Minimal reproduction

48000 -> 44100 Hz, 64 frames, mono, 16-bit, `--dither 99 --pdf 1 --dstContainer
w64`, against the pinned `ssrc-2.4.2` build. The 228-byte output:

```
root size field: 228            (agrees with the physical file length)
  fmt  chunk @40  size 40  -> end 80,  aligned 80
  data chunk @80  size 142 -> end 222, aligned 224
stopped at 224; 4 bytes remain: 00000000
```

SSRC writes four zero bytes past the eight-aligned end of its final chunk.

Both validators admit exactly two. The harness checks `len(data) - end == 2`.
Tonepoet's production validator checks `declared_file_bytes - chunk_end == 2` in
`tonepoet-pipeline/src/w64.rs`, pinned by a test named
`ssrc_validator_accepts_only_its_exact_two_zero_byte_trailing_pad`. The harness
mirrors production; neither is the more permissive of the two.

It is frame-count dependent. A one-second fixture at every probe rate crossed
with all 22 admitted dither IDs — 132 combinations — parses without a single
failure. The harness's 64-frame probe does not. That is why this was never seen
in ordinary conversion.

## The shipped evidence cannot be promoted instead

`outcome_operator_2026-09-28` contains the exact cell the failing album needs,
`high:192000:44100:2:16:99:triangular`, marked `passed`. It is still
unpromotable: its identity file records `"outcome": "not_qualified"`, only 2,382
of 4,536 cells passed, and the promotion script refuses any report containing a
non-passing cell.

Its failure classes are dominated by this same defect — `w64_alignment_padding`
447 and `w64_truncated_chunk_header` 345, together 792 cells or 17% of the grid,
failing on Wave64 parsing rather than on anything numerical. The R6 handoff also
specifies a corrected grid of 4,368 cells against that run's 4,536, so it is
superseded on its own terms.

## Required

The SSRC true-peak terminal grid can be qualified on the build host, and a
192 kHz to 44.1 kHz Int16 TPDF album converts once its cell is commissioned.

Wave64 written by the pinned SSRC build and Wave64 accepted by tonepoet agree.
The rule is stated once and enforced identically by the production validator and
the qualification harness, so neither can drift from the other again.

Three owners are possible and the choice is yours: `ssrc` emits two trailing
bytes or none; both validators accept any run of trailing zeros up to the
declared root size; or the writer includes the pad in the data chunk's declared
size. `ssrc` is our own fork, so all three are reachable — but the exactly-two
rule is deliberate in production code with a test pinning it, and relaxing it is
a decision about what Wave64 tonepoet will accept as certified evidence. Say
which you chose and why.

Do not hand-author registry records. Do not promote a report containing
non-passing cells. Do not special-case the failing frame count.

## Cost is still unknown

The grid was never timed, because it never started. Whatever fixes this should
also make a single `--cell` run measurable, so the full grid can be estimated
before it is committed to.

## State

`main` @ `e90936e`. Gate 7204 passed / 0 failed, no warnings.
`tonepoet-true-peak` 160/0. Reference is requalified on the Nix rooting with
scalar, sse2 and avx admitted. Issue #55 is closed and field-verified. Filed as
issue #57.

## Build capability

`cargo test -p tonepoet-pipeline` and `cargo test -p tonepoet-true-peak` build
in seconds and need no audio tools. The root `tonepoet` crate peaks at 6.24 GB
in one `rustc` even at `-j 1` with debug info off and will OOM under a 4 GB
ceiling; write those changes uncompiled and say which ones they are.

Note that `ssrc` itself is required to reproduce this. A prebuilt `ssrc` and
`sox_ng` closure was supplied in an earlier bundle and can be supplied again on
request.
