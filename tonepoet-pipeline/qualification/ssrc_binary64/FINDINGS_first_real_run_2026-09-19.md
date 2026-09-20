# SSRC Binary64 qualification: first run against the real executable — 2026-09-19

Operator run on the pinned executable. Everything the harness could verify about identity
passed; everything it tried to verify about output failed, for two reasons that are
properties of the harness and of tonepoet's Wave64 contract, not of SSRC's arithmetic.

## Identity: closed

- Executable `/nix/store/070wdb4cv4qjscmz4s6lzzl0gv3770yz-ssrc-2.4.2/bin/ssrc`,
  sha256 `dc401841d580a7b322038b38551f309fc63fac2af36bf3647f031177c19bef19`, built by
  `/nix/store/839i5bjn28x8p4pcfcy0ps990g5z7s8n-ssrc-2.4.2.drv` from source rev
  `b769add0…` (matches the harness pin), with gcc 15.2.0 and the cmake flags recorded
  in `static_audit_rev4.json`.
- SLEEF: the derivation copies fixed-output store path `swkggz2l…-source` into
  `submodules/sleef` before configure; that path's NAR hash equals the
  `sha256-BM88tQAI…` the ssrc flake declares for shibatch/sleef rev `0c063a8f…`, which is
  the harness pin. The executable links SLEEF statically (no dynamic sleef; only glibc);
  the nixpkgs sleef-4.0.0 in buildInputs is not what it links.
- SLEEF DFT type path audited (ObjectCache.hpp, DFTFilter.hpp, PartDFTFilter*.hpp,
  cli.cpp): double pipelines construct only double DFTs. Recorded in the static audit.
- `audit_identity_consistency.passed = true` in `outcome_2026-09-19.json`.

## Output: 0 of 30 cells, two causes

1. **No fact chunk.** SSRC's writer (dr_wav) emits only `fmt ` and `data` for IEEE
   float output, in `riff`, `w64`, and `rf64` containers alike. The harness's
   `parse_w64` and tonepoet's production validator (`tonepoet-pipeline/src/w64.rs:560`,
   reached from `bridge_validated_ssrc_w64_payload` in stages.rs) both refuse
   floating-point Wave64 without a fact chunk. So the production strong-SSRC path can
   never accept this executable's output as built, independent of qualification.

2. **Uncompensated filter delay.** SSRC emits the full linear-phase pre-roll and
   post-roll. For `high` at 44.1 kHz to 48 kHz, a 12,544-frame input produced 35,499
   output frames: 10,923 leading and 11,331 trailing near-zero frames around a
   13,245-frame active span (nominal 13,653). The harness's settled-DC windows assume
   zero latency and land in the pre-roll, reading gain 0. Whether production must trim
   this delay, and by how much for each profile and rate pair, is a design question the
   strong contract does not currently answer; the exact-copy ingress cannot trim.

## Numerics: fine where measured by hand

Locating the active span and measuring the central half of each overload plateau on that
one cell gives gain 1.0 within 1.7e-6 for +1.5, -1.5, +2.0, and -2.0: the Binary64
overload-preserving claim holds there. Peak 3.36 in the payload is Gibbs ringing on the
final Nyquist-rate ±2.0 segment, as expected of a brick-wall filter. Only one cell was
measured this way; the harness has to do the other 29.

## What would let the run go green

- Harness: tolerate a missing fact chunk (or require one only when present and wrong),
  and align the DC windows to the measured delay rather than to frame zero.
- Production: the same fact-chunk decision in `w64.rs`, and a decision on delay
  trimming for the strong SSRC path.

Neither is the operator's to make. The evidence files are committed as `pending`.

## Resolved 2026-09-20

The fork's `vendor/tonepoet-finite-stream-patch` (merged to master as 6b0bbfe) fixes both
causes: floating output carries a fact chunk equal to the data frames, and linear-phase
conversion publishes the finite timeline round(N*Fd/Fs). tonepoet's flake was re-pinned,
the executable it builds (sha256 `502af766…`) characterized 30/30 with the audits closed:
`outcome_2026-09-20.json`, evidence `sha256:860be9d1…`, outcome `bounded_established`.
The registry (`tonepoet-pipeline/src/ssrc_binary64.rs`) now returns Established for the
five characterized rate pairs on x86_64 at linear phase and 0.0 dB; everything else stays
pending.

Later the same day the rate scope was widened to the 42-pair grid (every ordered pair among
44.1, 48, 88.2, 96, 176.4, 192 kHz, plus 352.8 and 384 kHz down to each): 252 cells, all
passed, `outcome_grid42_2026-09-20.json`, evidence `sha256:6fd0d95e…`. The registry now
carries that grid.
