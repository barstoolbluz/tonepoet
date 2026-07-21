# DSD Reference policy v14 qualification candidate

**Date:** 2026-07-21  
**Policy:** `sox_ng_14_8_0_1_v14`  
**Status:** source-controlled qualification candidate; not release authority

## F8 correction

Policy v14 replaces FFmpeg `loudnorm` as the production true-peak analyzer.
The pinned FFmpeg 7.1 route was empirically shown to collapse to sample peak at
192 kHz and to exhibit a different, unqualified response at 352.8 kHz. Those
behaviors make its `input_tp` output unsuitable as rate-independent dBTP
authority for the enabled Reference target-rate matrix.

The replacement is measurement-only. Render, gain binding, terminal
realization, packaging, metadata mutation, decoded-sample verification, and
all user-visible target contracts are inherited unchanged from policy v13.

## Qualified measurement shape

For Float64 and Int24 W64 carriers, the analyzer is one pinned SoX-ng command:

```text
sox -S -D {carrier_w64} -n rate -v -L -s {sample_rate_hz_x16} stats
```

For the Float32 W64 post-terminal carrier, policy v14 retains the already
qualified carrier-decode seam because SoX-ng 14.8.0.1 mis-scales that carrier.
Pinned FFmpeg decodes the path-backed W64 directly to headerless little-endian
Float64 PCM on stdout, and pinned SoX-ng consumes that stream without a shell or
disk intermediate:

```text
ffmpeg -nostdin -hide_banner -nostats -loglevel error -i {carrier_w64} \
  -map 0:a:0 -vn -sn -dn -c:a pcm_f64le -f f64le pipe:1

sox -S -D -t raw -e floating-point -b 64 -L \
  -r {sample_rate_hz} -c {channels} - -n \
  rate -v -L -s {sample_rate_hz_x16} stats
```

The strict parser accepts exactly one `Pk lev dB` row from SoX `stats` under
`LC_ALL=C`. It requires one value for mono or the exact Overall-plus-per-channel
shape for stereo, and uses only the Overall value. Negative infinity is accepted
only after the existing independent signed-zero scan proves silence.

## Error authority

The oversampling factor is fixed at 16. For a signal bandlimited to the input
Nyquist frequency, the worst sample-grid miss at 16x is bounded by:

```text
-20 log10(cos(pi / 32)) = 0.041925956379... dB
```

Policy v14 rounds that bound upward to `0.041925957 dB`. It fits inside the
unchanged `0.100000000 dB` analyzer residual. Reporting uncertainty remains
`0.010000000 dB`; the total one-sided Q+E authority remains exactly
`0.110000000 dB`. F8 is not resolved by widening the ceiling or uncertainty.

## Qualification matrix

The mandatory analyzer gate advances to
`tonepoet-reference-analyzer-qualification/v5` and 1,968 cases:

- 960 inherited normalized-frequency single-tone cases;
- 768 fixed-frequency single-tone cases at 1, 20, 48, and 70 kHz, restricted
  to frequencies below 0.49 of each target rate;
- 240 inherited phase-aligned multitone cases.

Every enabled target rate from 44.1 through 768 kHz is exercised. The matrix
retains both early and late peak positions, both phases, mono and stereo, and
all three analytic levels. It therefore covers the 192 kHz failure, the
352.8/384 kHz cells, and the requested tail position with fixed physical
frequencies rather than only rate-normalized tones.

## Append-only and activation status

Every policy-v13 derivation, JSON manifest, candidate, certification stub, and
report remains byte-identical. Policy v14 has a new policy identity, analyzer
carrier schema, qualification schema, semantic plan identity, candidate
manifest, certification stub, and deterministic derivation checker.

The source-controlled v14 manifest remains `qualification_candidate`. Runtime
activation continues to fail closed until the mandatory pinned real-tool gate
produces a passing schema-v14 certification report and binds its digest to the
byte-identical v14 candidate manifest. This assembly environment does not claim
that release qualification.
