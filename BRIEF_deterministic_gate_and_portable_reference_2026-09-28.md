# Brief — deterministic gate, and a Reference that ships

Date: 2026-09-28
Base: `apply/ssrc-wavpack-int24-r6` @ `3a8cb1a` (R6 applied, its three test
expectations reconciled)

Three problems. The second is the important one; the third is the one a user hit today.

---

## Problem 1 — the workspace gate is not deterministic

Under the normal parallel test run, a few tests fail with:

```
malformed contended coordination descriptor (fail closed):
persistent lease descriptor exceeds 1048576 bytes:
<tmp>/claims/journal-operation/<uuid>--<uuid>.lease
```

Every failing test passes when run alone. Which tests fail varies between runs;
they exercise rename, undo/redo, inline edit, or metadata writes, and are
otherwise unrelated.

This predates R6. It reproduces on `main` @ `70e5e4f`, landing on a different
set of tests in the same families.

Observed: three consecutive gate runs gave 7183 passed / 2 failed, all with this
message; an earlier run gave 13 failed, 10 of them this message, every one
naming a single identical `.lease` path. Host is a 32-thread Threadripper 7970X,
125 GiB RAM, no swap. CLAUDE.md calls this one recurrent flake and treats it as
expected; that is no longer accurate.

### Required

Repeated runs of the gate on an unloaded host produce the same result every
time, and no test fails for a reason that disappears when it runs alone.

Fix it in the product — not by serializing the run, raising the size cap,
retrying, or ignoring tests. A coordination descriptor that grew past a sane
bound is a fact worth understanding before it is made to fit.

---

## Problem 2 — Reference cannot be shipped to anyone

tonepoet is to be packaged as `.deb` and `.rpm` and installed by users on their
own x86_64 Linux machines. The DSD Reference path must run there, and its output
must be genuine Reference output.

It cannot today. Reference re-derives its runtime closure fingerprint on the
user's machine and refuses when it does not match the installed qualification.
Two inputs vary per machine. They are not the same kind of problem.

### The tool paths are bound to this machine, and should not be

The external tool binding requires the runner path, the policy path, and the
build-time compiled path to be one identical absolute `/nix/store/...` path
baked in at build time. No end user has it.

A tool's identity is its bytes and its behavior, not where it was installed.
Decide what the package must carry so the qualified `sox_ng` and `ffmpeg` are
provably the ones being run, wherever they live, and say so plainly.

### The CPU dispatch binding is real — qualify every tier

This one is not overreach. `tonepoet-true-peak` dispatches on detected CPU
features inside its numerical core, so scalar, SSE2 and AVX paths can differ in
the last bits. Binding the dispatch is doing real work and must keep doing it.

The fix is not to stop hashing dispatch. It is to qualify more than one tier.

These are x86-64 instructions with architecturally defined semantics: an AVX
path on this machine and on a user's older machine produce identical bits for
identical inputs. Every tier can therefore be qualified here, on one host, and
the certification can admit the set instead of binding whichever tier this CPU
happens to reach.

**Required.** One build and one qualification on this machine produce artifacts
that install and run Reference correctly on any supported x86_64 Linux machine,
whatever dispatch tier its CPU reaches. A user who installs the package and
converts a DSD source gets Reference output, or a clear and true refusal — never
a refusal that only means "this is not the machine that qualified." Reference
still fails closed on a tier that was never qualified.

Two things in the way. Tiers cannot currently be forced: detection calls
`is_x86_feature_detected!` with no override, so on this host the shipped
dispatch is always the richest one and lower tiers are never exercised as the
real path. Closing that reaches `tonepoet-true-peak`, whose public API is frozen
to additions only. And the external tools dispatch too — FFmpeg selects SIMD at
runtime and exposes `-cpuflags` to constrain it; establish whether `sox_ng` and
`ssrc` dispatch and whether they can be constrained the same way.

`sox_ng` and `ssrc` are our own forks, already custom-modified for this project,
and their sources are in this bundle. If the cleanest answer is to add a
dispatch-tier pin to one or both, say so and scope it — that is an acceptable
outcome and will be commissioned as separate work. Do not contort tonepoet
around a missing flag in a tool we control.

---

## Problem 3 — Int32 + TPDF + album true-peak fails when the rate changes

A real album conversion fails every track with:

```
backend encode failed: qualified FFmpeg Int32 triangular-dither terminal is
unavailable (qualified FFmpeg terminal is missing its resolved out_sample_rate)
```

Source: one 2.1 GB WavPack image, float32, 192 kHz, stereo, with a sidecar CUE,
8 tracks. Settings: FLAC, Int32, TPDF dither, ReplayGain/true-peak in album
scope with a 0.1 dB margin.

- Target 176.4 kHz: fails, all 8 tracks.
- Target 192 kHz: succeeds, all 8 tracks, correct s32/192k output with
  ReplayGain tags written.

The only difference is whether the target rate equals the source rate.

### What is established

The check is at `src/convert/pipeline/track_executor.rs:1674`. When the
selected terminal realization carries a resolved target rate, the emitted
`aresample` filter must contain `out_sample_rate`, equal to both the input rate
and the target rate. When the realization carries no rate, the filter must not
contain the key at all. Both modes exist deliberately.

The succeeding 192 kHz run used this terminal, from its conversion log:

```
ffmpeg ... -f f64le -ar 192000 -ac 2 -i pipe:0 ...
  -af aresample=resampler=soxr:precision=33:cutoff=0.950:dither_method=triangular:out_sample_fmt=s32
```

No `out_sample_rate`, and it passed. The album true-peak flow stages each track
to a Float64 carrier first, which is what makes this the qualified Int32
triangular terminal (`matches_ffmpeg_int32_triangular_terminal_model` requires
Float64 input).

Planning alone looks self-consistent. Asking the planner directly for
WavPack-f32-192k to FLAC-Int32 under album true-peak gives, for a 176.4 kHz
target, `target_rate_hz=Some(176400)` and a filter that does contain
`out_sample_rate=176400`; for a 192 kHz target, `target_rate_hz=None` and a
filter without it. Neither would trip the check.

The filter builder omits the key whenever no rate change is requested of that
step (`tonepoet-pipeline/src/plugins.rs:1832`). That behavior is unchanged by
R6 — pre-bundle `main` @ `70e5e4f` emits a byte-identical filter.

### Hypothesis, possibly wrong

Something about the album carrier flow — which planning-only inspection does
not exercise — produces a terminal whose realization reports a resolved rate
while its filter reports none. A carrier already resampled to the target rate
before the terminal step would have that shape. This was not verified, and the
real 176.4 kHz command was never captured. Treat it as a lead, not a finding.

Which side is wrong is likewise unsettled. The terminal could be required to
pin its rate explicitly even when same-rate, or the realization could be
required to report no rate once the carrier is already at rate. Decide it.

### Required

That conversion succeeds. More generally, Int32 with TPDF and album true-peak
produces correct output at any supported target rate, whether or not it differs
from the source rate, and the qualified terminal's self-consistency check
either passes or refuses for a true reason. A regression covers the rate-change
case specifically; it is the case with no coverage today.

---

## What you can and cannot verify here

Build and test these directly; they are small and need no audio tools:

- `cargo test -p tonepoet-true-peak` — where the dispatch work lives. One
  dependency, ~3 s from clean.
- `cargo test -p tonepoet-pipeline` — planning and argv. Three dependencies,
  ~9 s from clean. Problem 3's planning side is inspectable here; its carrier
  flow is not.

You cannot build the root `tonepoet` crate. A single `rustc` invocation for its
lib peaks at 6.24 GB even at `-j 1` with debug info off, above the 4 GB ceiling.
`src/concurrency.rs` (problem 1), `src/convert/pipeline/track_executor.rs`
(problem 2's tool binding, and problem 3's failing check) live there. Write those changes; we compile and gate
them. Say plainly which of your changes you could not compile.

The R6 delivery shipped three test expectations that did not match its own
model, because it was authored without a compiler. Two of the three were in
`tonepoet-pipeline`, which you *can* build. Build what you can.

Prebuilt `sox_ng`, `ssrc` and `ffmpeg` are in this bundle if you need to observe
real tool behavior.

## Sequencing

Fix problem 1, then problem 2, then requalify once at the end.

`src/concurrency.rs` is not in `REFERENCE_COMMON_SOURCE_PATHS`, so a fix
confined there does not itself invalidate a qualification. Problem 2 almost
certainly reaches the hashed closure, and R6 already modified nine files in it.
Requalifying earlier wastes the run.

Requalification is `complete_p0_reference_qualification_report`
(`tests/dsd_reference_qualification.rs`), output directed into
`tonepoet-pipeline/qualification/` via
`TONEPOET_DSD_REFERENCE_{EVIDENCE,REPORT,CERTIFICATION}_PATH`. Roughly 46
minutes, and it runs on our build host, not here. Do not promote hand-authored
or partial evidence.

## Also outstanding, not in scope

The R6 handoff still owes the full 4,368-cell physical SSRC grid before registry
promotion, and one real conversion decoding a WavPack result against the SSRC
Int24 preterminal bytes. The commissioned registry ships empty.

## Environment

We build inside `nix develop`; the system Rust toolchain is below the workspace
MSRV of 1.93. `cargo fmt --check` fails across ~250 files and never has been
green — running `cargo fmt` would rewrite files in the hashed source closure.
Do not run it.
