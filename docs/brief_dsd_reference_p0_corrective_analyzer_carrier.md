# Corrective Brief: P0 Reference Analyzer Carrier (post-apply, post-audit)

**Status:** your corrected-v6 P0 implementation is applied at `385a914`,
compiled, and gated: full workspace suite green, zero cold warnings, 13
mechanical apply-side compile fixes (recorded in the commit message; none
touch design logic). The three-lane adversarial audit returned **zero
confirmed HIGH/MEDIUM findings** — legacy argv byte-identity, manifest
route segregation, the promotion gate, serde rejection surfaces, DbNano
arithmetic, policy immutability, and plan-hash stability all survived
hostile review. One tool-gated qualification test fails on a **real
toolchain defect in your frozen analyzer contract**. This brief carries the
empirical evidence and commissions the correction.

## The defect (measured, reproducible)

`complete_p0_reference_qualification_report` fails at the true-peak parse:
`Reference input_tp is outside -1000 to +100 dBTP`. Root cause, isolated on
the pinned toolchain (sox_ng 14.8.0.1 @324b8cf, ffmpeg 7.1 from the flake):

- **ffmpeg's W64 demuxer mis-decodes sox-written 64-bit-float W64 by
  exactly 2^31** (+186.64 dB): a −20 dBFS sine measures
  `input_tp = 166.64` and astats peak 166.64 dB. The scaling factor is
  exact: 20·log10(2^31) = 186.64.
- Header trigger: sox writes the fmt chunk with the plain
  `WAVE_FORMAT_IEEE_FLOAT (0x0003)` tag at 64 bits; ffmpeg's own W64 muxer
  writes `WAVE_FORMAT_EXTENSIBLE (0xFFFE)` with the float subformat GUID,
  and ffmpeg reads its own file correctly. ffprobe identifies both as
  `pcm_f64le`; the corruption is in decode scaling, not codec detection.
- **Your strict parser caught the garbage and failed closed** — the range
  check worked exactly as designed. This is the §3.2 contingency your
  design brief anticipated: "If that pin fails, the policy must select a
  different analyzer-readable exact carrier."

Empirical carrier matrix (−20 dBFS sine fixtures, this exact toolchain):

```text
sox f64 → W64  → ffmpeg loudnorm:  input_tp 166.64   BROKEN (+2^31 scale)
sox f32 → W64  → ffmpeg loudnorm:  input_tp -20.00   correct
sox f64 → WAV  → ffmpeg loudnorm:  input_tp -20.00   correct (RIFF 4 GiB ceiling)
sox f64 → AIFF → ffmpeg loudnorm:  input_tp -20.00   correct (4 GiB-class ceiling)
sox f64 → CAF  → ffmpeg loudnorm:  input_tp 166.64   BROKEN
sox f64 → RF64:                    sox_ng cannot write RF64
sox f64 W64 → sox stats readback:  Pk -20.00 exact   render chain UNAFFECTED
```

The last line matters: sox round-trips its own f64 W64 exactly, so the
R64 → finalize chain keeps its sample-exactness pin. Only the *analyzer
view* of R64/QPCM needs a new qualified path.

## Directive

Select and specify the corrected analyzer contract under your own
append-only policy rules (this changes measurement argv, the analyzer
identity, and qualification evidence → new policy ID per your §1.2 rules;
the release was never promoted, so no persisted-authority migration is
needed — state that explicitly). Candidate resolutions, decide with
rationale:

1. **Streamed exact re-container for measurement**: pipe the f64 W64
   carrier through sox as f64 WAV to ffmpeg's stdin
   (`sox R64.w64 -t wav - | ffmpeg -f wav -i pipe:0 …`). **Verified on
   this exact toolchain:** the −20 dBFS f64 W64 fixture measures
   `input_tp = -20.00` through the pipe. Streaming WAV avoids the
   RIFF on-disk size ceiling (unknown-size header, ffmpeg reads to EOF —
   the >4 GiB streamed case still needs its own qualification fixture);
   sox f64→f64 WAV is a container re-wrap, sample-exact. Cost: one extra
   decode-side pass, no disk. Requires: pinning the pipe semantics
   (unknown-size RIFF acceptance) in qualification, and deciding how the
   two-process measurement step fits your PlannedMeasurement single-command
   shape (a typed two-stage measurement, not a shell pipeline — no shell).
2. **f32 measurement view with a policy error bound**: render an f32 W64
   measurement copy (sox f64→f32, correctly read by ffmpeg). f32 mantissa
   (24-bit) cannot represent the f64 samples exactly, but the added
   true-peak error is boundable (≤ 2^-24 relative ≈ 0.0000005 dB-scale
   near full scale) and can be folded into the certified one-sided
   analyzer uncertainty `Q`/`E` arithmetic. Cost: one extra render-side
   pass + a new bound derivation; keeps single-command measurement.
3. Any alternative you can qualify (e.g. teaching the policy to accept an
   ffmpeg-side `-f w64`-with-forced-decoder workaround only if you can
   demonstrate correct decode — our probing found none).

Requirements regardless of choice: update the frozen measurement argv and
parser contract; re-derive the qualification manifest and certification
JSON; update `tests/dsd_reference_qualification.rs` so
`complete_p0_reference_qualification_report` passes against real tools;
record the ffmpeg W64 defect in provenance/docs so a future ffmpeg fix
does not silently change the measurement path without a policy bump.

## Audit LOWs (optional hardening, fold in if cheap)

- Strict loudnorm report parse: serde_json accepts duplicate keys
  (last-wins). The one-report-object check narrows exposure; a duplicate
  `input_tp` key inside the single report would still parse. A manual
  duplicate-key scan of the extracted report object closes it.
- `crates/sacd-rs/build.rs`: additionally verify fixtures against
  `P0_SHA256SUMS` at build time (content-hash corpus ID already fails
  loudly at test time; this just fails earlier).
- Manifest serde tags (`route`, snake_case variant names) are load-bearing
  wire format: add a frozen-string fixture test if not already pinned.
- SACD ISO preflight double-SHA race: system-level, accepted; document as
  out-of-threat-model in the module docs.

## Out of scope

Everything else. The P0 surface, deferrals (Manual workflows, in-TUI
builder, lossy delivery), and the promotion process are unchanged. Do not
expand scope. Complete-file delivery contract applies.
