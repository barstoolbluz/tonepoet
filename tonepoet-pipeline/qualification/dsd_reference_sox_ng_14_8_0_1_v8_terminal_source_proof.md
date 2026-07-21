# SoX-ng 14.8.0.1 terminal-effects source proof

**Policy:** `sox_ng_14_8_0_1_v8`  
**Pinned revision:** `324b8cf873fd7836e8848bd87f7a90d8faa6f849`  
**Pinned source-tree NAR hash:** `sha256-LjGx+yaWi5EcZsXhTmdRaf9utFXcCXASMmjRtm6vUc8=`  
**Scope:** the non-limiter terminal `gain` realization used by `ReferenceCompensated`, `NativeLevelExact`, and `FixedExact`

## Source authorities

The pinned source tree is selected by `flake.lock`; both the Git revision and NAR hash above are also frozen in the v8 qualification manifest.

The derivation depends on these exact pinned-source facts:

1. `src/sox_ng.h` defines `typedef sox_int32_t sox_sample_t`; `sox_int32_t` is a signed two's-complement 32-bit integer.
2. `SOX_SAMPLE_TO_FLOAT_64BIT` maps an internal sample to binary64 by multiplication by `1 / (SOX_SAMPLE_MAX + 1)`, i.e. by `2^-31`.
3. `SOX_FLOAT_64BIT_TO_SAMPLE` maps binary64 back by multiplication by `2^31` followed by signed round-to-nearest and clipping at the `sox_sample_t` endpoints.
4. `SOX_ROUND_CLIP_COUNT` rounds a non-clipping value to the nearest signed internal integer by adding or subtracting `0.5` before conversion.
5. In `src/gain.c`, `priv_t.fixed_gain` is a binary64 `double`; the parsed dB value is converted once with `dB_to_linear`, copied to the binary64 `mult`, and the non-limiter flow writes each output sample through `SOX_ROUND_CLIP_COUNT(*ibuf * mult, effp->clips)`.

Auditable source locations:

- `https://github.com/barstoolbluz/sox_ng/blob/324b8cf873fd7836e8848bd87f7a90d8faa6f849/src/sox_ng.h`
- `https://github.com/barstoolbluz/sox_ng/blob/324b8cf873fd7836e8848bd87f7a90d8faa6f849/src/gain.c`

## Derivation

Let the internal input sample be the signed integer `k`. Its full-scale value is `x = k / 2^31`.

Every permitted internal integer has magnitude below `2^31`. Therefore `k` is exactly representable in binary64, and division by the power of two `2^31` is exact. A SoX-written Float64 carrier produced from this grid contains an exact binary64 representation of `k / 2^31`; reading it through the pinned Float64 conversion multiplies by `2^31` and recovers `k` exactly before the gain effect. This round trip introduces no additional grid error.

For every finite gain multiplier `m`, the pinned non-limiter gain implementation computes `k * m` in binary64 and performs exactly one sample-domain conversion: the terminal round to `sox_sample_t`. The selected gain mode changes only the binary64 coefficient supplied to that same site; it does not add another integer-domain conversion or another Q1.31 rounding stage. Provided the value is not clipped, round-to-nearest contributes at most one half internal sample:

```text
0.5 / 2^31 = 2^-32 full scale
```

This bound does not depend on the sign of `k`, the sign or magnitude of `m`, or which qualified exact gain policy produced `m`. Tonepoet's `resolve_bound_gain` admits `ReferenceCompensated`, `NativeLevelExact`, and `FixedExact` only when their measured pre-terminal peak and requested gain fit below the compiled safe pre-terminal ceiling; exact modes are rejected rather than reduced when they do not fit. The post-final validator then rejects any result above the public `-1.000000000 dBTP` ceiling. The clipping branches are therefore outside the qualified cell contract.

The separate inherited binary64 coefficient/arithmetic allowance remains `2^-51` full scale. This v8 proof does not silently reassign coefficient construction or binary64 multiplication error to the Q1.31 term: those pre-existing uncertainties remain charged to that independently frozen allowance. The exact pinned source establishes that the previously omitted effects-domain contribution is one—and only one—terminal Q1.31 half-step for all three qualified exact gain modes. It is additive to the inherited allowance, yielding the v8 Float64 authority:

```text
2^-32 + 2^-51
```

Its exact upward-rounded Q1.63 representation is:

```text
2^31 + 2^12 = 2147487744
```

## Limits of this proof

This proof is valid only for the exact pinned source revision, the exact Nix source-tree identity, the non-limiter `gain` path, the qualified carrier routes, and the three exact gain policies named above. `NormalizePeak` and any limiter path are outside this authority. A SoX-ng source change, a different gain effect path, a floating-point internal sample type, an additional sample-domain conversion, or admission of clipping requires a new append-only policy generation and a new proof.
