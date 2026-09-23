# SoX-ng 14.8.0.1 policy-v17 source-lock proof

**Policy:** `sox_ng_14_8_0_1_v17`  
**Pinned revision:** `9ed22fb3d813d6c02f67c254e57d162cee014a30`  
**Pinned source-tree NAR hash:** `sha256-WxMirop+SH3RzUM57QBDjetp9Q4qhFjzcfo2OJHStcs=`  
**Predecessor:** `sox_ng_14_8_0_1_v16` at `324b8cf873fd7836e8848bd87f7a90d8faa6f849` / `sha256-LjGx+yaWi5EcZsXhTmdRaf9utFXcCXASMmjRtm6vUc8=`

## Why v17 exists

Policy v16 is immutable evidence. The SoX-ng source identity therefore cannot be rewritten in place when the qualified closure moves to a different revision. Policy v17 is the append-only source-lock successor. It preserves v16's DSP, analyzer, terminal, packaging, metadata, capacity, and exact-Wave64 contracts while binding the corrected SoX-ng source tree.

The v17 revision is not treated as a linear patch on the v16 revision; the two Git histories diverge. The relevant v17 source nevertheless contains the delayed/sparse output-finalization repair used by libsndfile-backed formats. In particular, `src/formats_i.c` accounts for the logical end of a pending sparse write when reporting file length, and the revision carries a dedicated sparse-file regression that exercises Wave64 header finalization.

## Terminal arithmetic continuity

The source-side premises of the existing Float64 terminal bound depend on `src/sox_ng.h` and `src/gain.c`, not on the file-length finalization code. An exact-content comparison of those two files between the v16 and v17 pins on 2026-09-21 found them byte-for-byte identical:

- `src/sox_ng.h`: identical at `324b8cf873fd7836e8848bd87f7a90d8faa6f849` and `9ed22fb3d813d6c02f67c254e57d162cee014a30`.
- `src/gain.c`: identical at the two pins.
- `src/dither.c`: identical at the two pins.
- `src/rate.c`: identical at the two pins.
- `src/dsdiff.c`: identical at the two pins.
- `src/dsf.c`: identical at the two pins.

`src/formats_i.c` and `src/sndfile.c` do differ at the v17 pin. Those differences are intentionally treated as part of the executing closure and are covered by the complete real-tool qualification rather than assumed equivalent.

Auditable v17 source locations:

- `https://github.com/barstoolbluz/sox_ng/blob/9ed22fb3d813d6c02f67c254e57d162cee014a30/src/sox_ng.h`
- `https://github.com/barstoolbluz/sox_ng/blob/9ed22fb3d813d6c02f67c254e57d162cee014a30/src/gain.c`
- `https://github.com/barstoolbluz/sox_ng/blob/9ed22fb3d813d6c02f67c254e57d162cee014a30/src/dither.c`
- `https://github.com/barstoolbluz/sox_ng/blob/9ed22fb3d813d6c02f67c254e57d162cee014a30/src/rate.c`
- `https://github.com/barstoolbluz/sox_ng/blob/9ed22fb3d813d6c02f67c254e57d162cee014a30/src/dsdiff.c`
- `https://github.com/barstoolbluz/sox_ng/blob/9ed22fb3d813d6c02f67c254e57d162cee014a30/src/dsf.c`
- `https://github.com/barstoolbluz/sox_ng/blob/9ed22fb3d813d6c02f67c254e57d162cee014a30/src/formats_i.c`
- `https://github.com/barstoolbluz/sox_ng/blob/9ed22fb3d813d6c02f67c254e57d162cee014a30/src/sndfile.c`

At the v17 pin, the terminal facts used by the v8 derivation therefore remain unchanged:

1. `sox_sample_t` is a signed 32-bit integer sample domain.
2. The Float64 carrier conversion maps that grid by the exact power-of-two scale `2^-31` and recovers the same integer grid before the effect.
3. The non-limiter `gain` path keeps its coefficient in binary64 and writes each sample through the same single `SOX_ROUND_CLIP_COUNT(*ibuf * mult, effp->clips)` site.
4. A non-clipping terminal conversion therefore contributes at most one half Q1.31 sample, `2^-32` full scale.
5. The independently retained binary64 arithmetic allowance remains `2^-51` full scale.

The v17 Float64 terminal authority is consequently unchanged:

```text
2^-32 + 2^-51
```

with the same upward-rounded Q1.63 representation:

```text
2147487744
```

## Wave64 scope

This source proof does **not** promote the new writer on source inspection alone. The source lock changes the executing closure, so production remains fail-closed until the existing gated Reference qualification runs under the new pin. That gate re-executes the exact 60-cell Wave64 integrity matrix, independent parser checks, FFmpeg full traversal, package/sample-identity checks, profile responses, throughput/deadline checks, and common-model release certification.

## Limits

This proof applies only to revision `9ed22fb3d813d6c02f67c254e57d162cee014a30` with NAR `sha256-WxMirop+SH3RzUM57QBDjetp9Q4qhFjzcfo2OJHStcs=`, the qualified non-limiter terminal gain path, and the existing Reference cell contract. Any later SoX-ng source identity requires another append-only policy generation and a fresh source-lock audit.
