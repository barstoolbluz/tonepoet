# Brief R4 — the album rate-change terminal still fails, plus three open items

Date: 2026-09-29
Base: `main` @ `191fc8b`

The Nix path is done: Reference is requalified, the qualification admits the
scalar/sse2/avx dispatch set from one host, and a clean gate is 7194 / 0.

Four things remain. The first is a live user-facing failure.

---

## 1. The R1 Problem 3 correction is incomplete

The defect it was written for still reproduces on this build, with every
corrective applied.

Boston, *Boston* — one 2.1 GB WavPack image, float32, 192 kHz, stereo, sidecar
CUE, 8 tracks. FLAC, Int32, TPDF, album true-peak, 0.1 dB margin, target
176.4 kHz. Exactly the case R1 named.

Track 3, and only track 3:

```
backend encode failed: qualified FFmpeg Int32 triangular-dither terminal is
unavailable (qualified FFmpeg terminal is missing its resolved out_sample_rate)
```

The other seven tracks then failed with `PCM true-peak scan cancelled` — that
failure cancelled the album while their scans were in flight. Track 4 had
measured successfully one second earlier.

### What is established

The album is decomposed into independent per-track artifacts. There is no
shared decoded intermediate: each track is cut straight out of the WavPack
image by its own ffmpeg `atrim` invocation into its own segment WAV, then into
its own Float64 true-peak carrier. Eight structurally identical pipelines
differing only in segment boundaries. Exactly one reaches the Int32 terminal
with an unnormalized rate.

Track 3 is the longest track on the album by a wide margin:

| track | duration | f64 carrier |
|---|---|---|
| 1 | 286.00 s | 0.88 GB |
| 2 | 302.00 s | 0.93 GB |
| **3** | **469.00 s** | **1.44 GB** |
| 4 | 180.00 s | 0.55 GB |
| 5 | 261.00 s | 0.80 GB |
| 6 | 251.67 s | 0.77 GB |
| 7 | 227.47 s | 0.70 GB |
| 8 | 285.37 s | 0.88 GB |

Durations are from the CUE index points, with track 8 derived from the image
duration of 2262.5 s. Carrier sizes are 192 kHz x 2 channels x 8 bytes.

Scratch admission for the job logged `estimated_bytes=1.0 GB`. Scratch is
`/dev/shm/tonepoet` with `scratch_memory_limit_percent = 50` on a 125 GiB host;
the admission log showed 60.6 GB of budget remaining, so this is not budget
exhaustion.

It is by a clear margin the largest single unit of work in the album.

R1's regression for this defect passes. Its normalization applies only to a
`PcmTruePeakCarrier` source.

### Hypothesis, unverified

Something duration- or size-dependent puts the longest track on a path where
that normalization does not apply. Nothing was instrumented to confirm it, and
no other per-track difference has been ruled out — the tracks also run
concurrently, so ordering effects are not excluded. Treat it as a lead.

### Required

That conversion succeeds, on every track, at a target rate that differs from
the source. The correction holds regardless of track length or carrier size.

### Note on reproduction

This configuration cannot be produced from the CLI: no command-line flag sets
the PCM true-peak policy, and no preset covers it. Only the TUI reaches it,
which is why this was field-reported rather than caught by a test. A CLI path,
or a regression that exercises the real album-carrier flow at a realistic track
length, would make it reachable.

---

## 2. One track's failure is reported as eight, and hides the cause

The status bar showed `PCM true-peak scan cancelled` on all eight tracks. The
real error appeared on one track, and only in the application log at
`~/.cache/tonepoet/tonepoet.log`. From the TUI the run looked like a true-peak
scanning problem, which it was not.

### Required

When one track's failure cancels the album, the reported reason names the
originating failure. Cancellation of collateral work is not presented as a peer
failure of equal standing.

---

## 3. A coordination-contention flake still reaches the gate

Four consecutive gate runs: three clean at 7194 / 0, one with two failures.
Both failing tests pass in isolation.

```
scanner probe must acquire the now-ownerless durable descriptor inode:
  Os { code: 11, kind: WouldBlock }

retire test recovery reservation:
  "persistent lease is live-owned: .../journal-operation/<uuid>--<uuid>.lease"
```

They landed in `concurrency` and `tui::keybindings::permanent_delete_tests` —
different subsystems, so this may wander rather than being specific to those
two. It is a different family from the descriptor-size flake that the schema-3
encoding fixed; that one is gone.

### Required

Repeated gate runs on an unloaded host produce the same result every time.
Fix it in the product — not by serializing the suite, relaxing a lock,
retrying, or ignoring tests.

---

## 4. Package staging cannot process the real closure

`.deb`/`.rpm` is still blocked. The stager refuses the actual closure of the
six bound executables:

```
stage_reference_runtime_closure: absolute symlink points outside Nix store:
  .../nix/store/...-bluez-5.84/etc/bluetooth/network.conf -> /etc/bluetooth/network.conf
```

Three such symlinks, all `bluez-5.84` `/etc/bluetooth/*.conf`, out of 374
closure paths. `bluez` arrives transitively through `ffmpeg-full`. Nothing
about Bluetooth configuration can influence Reference output.

The stager rewrites absolute symlinks pointing into the store but has no policy
for one pointing outside it, so it refuses. Refusing is defensible as
fail-closed behavior; the consequence is that no package can be built. The 13
staging unit tests pass because they run against synthetic trees.

### Required

The stager produces a relocatable tree from the real closure, and a
staged-package qualification runs against it. Whatever policy resolves symlinks
that escape the store is explicit about what it excludes or preserves, and
drops nothing that could influence Reference output.

---

## 5. Confirm one qualification geometry chosen here

The 4-channel AVX loudness tier could not construct a meter: the convenience
constructor resolves only unambiguous mono and stereo, so nothing had ever been
qualified at that tier.

It now names roles explicitly as Left / Right / LeftSurround / RightSurround.
Every role carries a non-zero BS.1770 weight, so all four lanes do real
arithmetic; an `Lfe` or `Unused` channel would be weighted 0.0 and leave the
AVX kernel unexercised, which is the reason the 4-channel case exists.

That choice was made here to unblock qualification. If the intended geometry
differs, the AVX loudness tier is qualified against the wrong one and must be
requalified. Confirm or correct it.

---

## Also fixed here, for the record

`run_planned_command` asserted the Reference audio command environment held
exactly one variable; R1 deliberately added `SOXR_USE_SIMD=0` beside
`LC_ALL=C`. Verified against the planner and tightened to check both rather
than loosening the count.

Requalification then passed in 12.7 minutes, not the ~46 quoted in earlier
handoffs: report passed with 3903 positive and 34 expected-negative cases,
certification passed/qualified, `tonepoet-reference-nix-runtime-attestation/v1`
recorded, dispatch backends scalar, sse2 and avx.

## Not in scope

Issue #56, MusicBrainz refusing a single-image APE album with a sidecar CUE,
is open and unrelated. The R6 handoff still owes the 4,368-cell physical SSRC
grid and one real WavPack decode comparison.

## Build capability

`cargo test -p tonepoet-pipeline` and `cargo test -p tonepoet-true-peak` build
in seconds, need no audio tools, and would have caught the loudness-meter and
command-environment defects. The root `tonepoet` crate peaks at 6.24 GB in one
`rustc` even at `-j 1` with debug info off and will OOM under a 4 GB ceiling;
write those changes uncompiled and say which ones they are.
