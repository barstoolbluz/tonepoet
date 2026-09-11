# Fast true-peak scan: certify regions before reconstructing them

## Implementation specification for the API-frozen 2026-09-10 crate

**Scope:** `crates/tonepoet-true-peak` in `tonepoet-true-peak-api-frozen-2026-09-10.tar.gz`.

**Required wall gate:** at most **0.66 seconds per programme minute**, using the supplied uninstrumented benchmark on the operator's Xeon E5-1680 v2. **Stretch objective: 0.50 seconds per programme minute.** A reduction to 0.85 seconds per minute is useful progress, not completion.

**Accuracy policy:** preserve the frozen HQ1024V1 target and its truthful per-channel certificates. For ordinary decoded PCM, finish the Fast search to a **0.01 dB maximum certificate width**, with a separately tested **0.01 dB analytical point-error gate** on the qualified analytical corpus. Do not spend additional work merely to reproduce Reference's last digits.

**Public API:** unchanged, including the feature-gated surface and existing constant values. No new tier, reconstruction identity, public setting, dependency, feature, or migration mechanism.

**Delivery status:** this package contains a design, an exact-arithmetic coefficient derivation, executable tests of that derivation, and a workload-screening probe. It is not a replacement Rust implementation. The operator's two-minute 192 kHz recording and target processor were not available here. No target-machine wall-time result is claimed.

---

## 1. Decision and precedence

Replace Fast's unconditional whole-signal reconstruction with **native-input certified screening followed by selective HQ reconstruction**.

The cheap screen works on the original PCM samples and their second differences. It encloses *every* frozen HQ1024 knot in a region. When that upper bound cannot beat an already established same-channel HQ4 lower witness, the region needs no first-stage FFT, no 4x midpoint convolution, and no coarse-domain curvature scan.

Reconstruct only the surviving regions. Use direct, symmetric evaluation of the existing 1536-tap half-delay FIR for sparse requests and the existing qualified 8192-point FFT graph for dense requests. Build the existing HQ4 survey only where needed. Resolve remaining competitive HQ1024 intervals to the chosen accuracy, using the already-frozen dyadic tail bounds.

This changes where work occurs, not the waveform being certified. It does not substitute Reporting4x, a shorter filter, a guessed bandwidth, a polynomial waveform, or a downsampled input for HQ1024V1.

The user's latest instruction permits reduced point precision in exchange for speed. It therefore supersedes the brief's earlier objective that the Fast point match Reference exactly. The API freeze, finite-target containment, correct edge handling, and throughput measurement remain binding. The new accuracy floor is not optional and is not satisfied merely by returning a wide but safe interval.

The first implementation should omit the old optional nomination/proposal/finishing policy rather than port its scheduling apparatus into a sparse representation. HQ4 supplies the initial witnesses; mandatory tolerance resolution supplies any additional knots. Retain useful numerical helpers, including signed fitting only if later measurements justify using it as a witness proposal. Candidate refinement accounted for 0.3% of the baseline, so redesigning that heuristic is not the performance project.

## 2. What the attached implementation actually does

### 2.1 Binding observations

The supplied brief reports Fast at 2.38 s/min idle and 1.78 s/min under load on 24-bit, 192 kHz stereo programme. A rustc 1.80.1 comparison reports 1.68 s/min under load. The shipping target is 0.66 s/min. These are operator measurements, not measurements reproduced in this package. [S1, lines 37-63]

Its stage attribution is 45.2% qualified-prefix execution plus ingestion, 28.7% HQ4 midpoint survey, 23.0% flat envelope, 2.9% reading/decoding, and 0.3% candidate work. Those percentages describe attributed instrumented time; they are not an exact decomposition of uninstrumented total wall time. [S1, lines 78-98]

Even deleting candidate work entirely would leave almost all runtime intact. Deleting the midpoint and flat-envelope passes while retaining the current prefix would also leave little or no room for the 0.66 gate: 45.2% of 1.78 s/min is about 0.80 s/min, before input and other costs. This calculation is diagnostic, not a benchmark prediction.

### 2.2 Source-level causes

`FastPeakMeterImpl::feed_prefix` feeds every sample through `QualifiedHalfDelayFft`, then ingests all emitted coarse values. `FastState::process_tile` calls `build_survey` and `cover_groups` before asking whether the tile's optional work is dominated. The expensive work has therefore already happened when pruning takes effect. [S2, `src/fast_scan.rs:1508-1542,1742-1771`]

`build_survey` materializes the HQ4 stream and repeatedly maps absolute `i128` coordinates to slice positions. `flat_span_metrics` separately reduces coarse magnitudes, coarse second differences, and HQ4 magnitudes. These operations explain why a nominally short interpolation kernel does not account for the entire scan cost. [S2, `src/fast_scan.rs:803-1043`]

The first stage already packs two channels into one complex transform and already has explicit AVX execution for the same radix-2 graph. Do not propose stereo packing or adding AVX as though either were absent. [S2, `src/qualified_half_delay_fft.rs:249-330,839-973`]

Finally, Fast currently accumulates flat parent uppers into `channel_upper_peaks` and `channel_flat_upper_peaks` before optional refinement. Later point evaluations cannot retire those bounds. That is valid for the old fixed-work policy, but it cannot enforce a new maximum-width requirement. The new search must replace refined parent bounds with the bounds of their covering children. [S2, `src/fast_scan.rs:958-1015,1586-1690`]

## 3. Define the precision requirement without changing the target

BS.1770-5 does not provide one universal scalar true-peak error tolerance. Its Annex 2 describes a 4x implementation; the informative oversampling table gives 4x sinusoidal sampling under-reads of 0.554 dB at 0.45 Fs and 0.688 dB at 0.5 Fs. EBU Tech 3341's specified true-peak tests use +0.2/-0.4 dB tolerances. [E1, E2]

Use the following explicit engineering requirement instead of the ambiguous phrase "10x more precise":

- Every normal-production Fast certificate must enclose the frozen finite HQ1024 peak, overall and separately for each channel.
- Every non-silent ordinary-PCM channel must finish with `20*log10(U/L) <= 0.01 dB`. Its reported point must lie in that interval. A genuine channel silence has `[0,0]`, not a fabricated logarithmic width.
- On independently constructed, qualified-band analytical signals, the point's absolute error must be at most 0.01 dB against the analytical truth, not against a rounded displayed test value.

These thresholds are substantially tighter than the cited standardized allowances. They do **not** mean that each particular input must show ten times less error than a particular BS.1770 implementation: an implementation that is already exact on an input cannot be improved by a nonzero ratio.

### 3.1 Two distinct accuracy statements

Let `P_HQ` be the maximum over the finite frozen HQ1024 target. A valid interval of width 0.01 dB bounds both the returned point's deviation from `P_HQ` and the upper endpoint's excess over `P_HQ`, provided the point is in the interval. This is the search/evaluation guarantee.

It does not establish the error of HQ1024 relative to every conceivable continuous-time reconstruction. Keep the existing frozen waveform and its separate analytical qualification. The attached design documentation explicitly warns that sampled frequency-response results do not prove an arbitrary-signal peak-relative theorem. [S3, sections 7.3 and 8]

The analytical gate above is an additional test obligation, not a conclusion obtained by relabeling a finite certificate. Do not add an unproved ideal-sinc reserve to `upper_level()`.

### 3.2 Normal PCM and exceptional binary64 values

The accuracy floor applies to ordinary decoded PCM, including positive gain, all supported channel counts, and both edge policies. Do not require a spectral classifier or a source-bit-depth parameter. Use the same algorithm at every sample rate.

The existing API also accepts binary64 inputs with subnormal magnitudes or extreme exponents. Preserve their honest containment and established error behavior. Near the least positive subnormal, adjacent representable endpoints can themselves be farther apart than 0.01 dB; an unconditional relative-width promise there is not meaningful. Such cases must remain explicitly visible as wide, truthful results or the existing applicable numerical error, never as successful 0.01 dB measurements. Do not reject ordinary PCM merely because an intermediate *bound* overflows.

### 3.3 Private tolerance, frozen public surface

Keep `PeakTier::Fast.interval_objective_db()` returning `None`, as required by the freeze. The new limit is an internal production/commissioning policy, not a new public knob. Keep `FAST_WALL_NANOS_PER_PROGRAMME_MINUTE` at 660,000,000 and retain the existing `FAST_ALGORITHM_REVISION` value. This work adds no public revision marker.

A suitable downward binary64 approximation of the 0.01 dB amplitude ratio is:

```rust
// Private. Strictly no greater than 10^(0.01/20).
const FAST_ACCEPT_RATIO_DOWN: f64 = f64::from_bits(0x3ff004b7e9b5ce5c);
```

For normal positive `L`, accept a tolerance-resolved node only when `U <= down(FAST_ACCEPT_RATIO_DOWN * L)`. Round the multiplication downward. An upward-rounded threshold would permit accepting a node just outside the promised tolerance. Use a zero/small-value path where needed; no logarithm belongs in the hot search loop.

## 4. The new native-input bound

### 4.1 Target coordinates and composition

Let `x[n]` denote the original samples extended according to the selected finite edge policy. For a native interval `n`, write an HQ coordinate as `q = 1024*n + p`, where `0 <= p <= 1024`.

The frozen first stage is:

```text
z[2*m]   = x[m]
z[2*m+1] = sum(t=0..1535) h[t] * x[m + 768 - t]
```

Here `h` is the full symmetric 1536-tap sequence reconstructed from `HQ1024_HALF_DELAY_COEFFICIENTS`; its stored coefficients are the authority.

For `p < 1024`, let `s = floor(p/512)` and `r = p mod 512`. When `r = 0`, the target is `z[2*n+s]`. Otherwise:

```text
y[n,p] = sum(j=-16..17) tail[r,j] * z[2*n+s+j]
```

The right endpoint is `y[n,1024] = x[n+1]`.

Compose those stored dyadic coefficients in exact arithmetic to obtain:

```text
y[n,p] = sum(k=-775..776) H[p,k] * x[n+k].
```

The all-phase native support is `[-775,776]`. Retain the existing 777-frame raw halo rather than introducing a new edge convention.

### 4.2 Residual identity

Set `t = p/1024` and subtract the straight line between the original samples:

```text
r[p,k] = H[p,k] - (1-t)*delta[k,0] - t*delta[k,1]
m0[p]  = sum_k r[p,k]
m1[p]  = sum_k k*r[p,k]
a[p]   = m0[p] - m1[p]
b[p]   = m1[p]
r0[p,k] = r[p,k] - a[p]*delta[k,0] - b[p]*delta[k,1]
```

Preserve the affine defects; frozen coefficient rounding makes them small, not identically zero.

For `j = -775..774`, define:

```text
Q[p,j] = sum(k=-775..j) (j-k+1)*r0[p,k]
Delta2 x[i] = x[i] - 2*x[i+1] + x[i+2]
```

The exact finite identity is:

```text
y[n,p] = (1-t+a[p])*x[n] + (t+b[p])*x[n+1]
         + sum(j=-775..774) Q[p,j]*Delta2 x[n+j].
```

This is the same summation-by-parts technique already used by the crate's coarse-domain bounds, applied *before* the costly first-stage reconstruction.

### 4.3 Group bound

A group owns native intervals `n = a..b-1` and includes its endpoint samples `x[a..b]`. Define:

```text
S_g = max(i=a..b) abs(x[i])
D_g = max(i=a-775..b+773) abs(Delta2 x[i])
A_raw >= max_p sum_j abs(Q[p,j])
B_raw >= max_p (abs(a[p]) + abs(b[p]))
```

Then every frozen HQ1024 knot in the group satisfies:

```text
P_g <= S_g + B_raw*S_g + A_raw*D_g.
```

The union of sample support required by `D_g` ends at `b+775`. The endpoint at `b` is included in `S_g`. These ranges matter: do not accidentally use only differences inside the owned group.

The bound makes **no audio-bandwidth assumption**. High-frequency, adversarial, or endpoint-dominated material simply causes more groups to survive.

### 4.4 Constants derived and checked in this package

`tools/generate_raw_screen_metadata.py` composes every native phase from the supplied coefficient source using Python integers over a common power-of-two denominator. It computes the moments and second antiderivatives exactly and rounds only the final constants outward.

The resulting runtime constants are:

```rust
// Private generated metadata; use bits, not retyped decimal approximations.
const RAW_A_UPPER: f64 = f64::from_bits(0x3ff02862ce0a81a1);
const RAW_B_UPPER: f64 = f64::from_bits(0x3cce9970cbf12e12);
```

Their decimal values are approximately:

```text
A_raw = 1.009859852645697
B_raw = 8.493033329468595e-16
```

The largest `A` occurs at native phase 512; the largest affine-defect bound occurs at phase 941. The independently composed maximum L1 gain is enclosed by 4.676026435151283, consistent with the public conservative norm 4.68.

The derivation checks 1,025 rows, including the right endpoint. See `evidence/raw_screen_metadata.json` for exact rational numerators, denominators, bit patterns, support ranges, and the coefficient-source hash. Do not copy constants from the preliminary floating-point experiment or regenerate the frozen waveform itself.

### 4.5 Runtime arithmetic enclosure

The constants above certify real arithmetic on the stored coefficients. They do not excuse ignoring runtime rounding.

Compute the maximum observed second difference once from the actual raw values. For an available tile-plus-halo magnitude bound `X`, reuse the crate's numerical helper:

```text
E_D = dot_rounding_upper(8, upper_mul(4, X))
D_g_upper = upper_add(D_g_observed, E_D)
U_g = upper_add(
        S_g,
        upper_add(upper_mul(B_raw, S_g),
                  upper_mul(A_raw, D_g_upper)))
```

`S_g` is an exact input-magnitude reduction, not a reconstructed estimate. Reuse the bit-aware magnitude helpers so a caller's DAZ setting cannot convert nonzero subnormal input into a false exact-silence witness.

A fully zero contributing raw support gets the existing exact-zero treatment. A nonzero halo around a zero owned region must not be classified as silence: the frozen reconstruction can ring into the region.

An infinite or otherwise unusable screening bound means "survives", not "the waveform overflowed". Resolve the region with the existing scale-aware numerical paths before deciding whether `NumericalOverflow` actually applies.

## 5. Streaming ownership and the mandatory cheap pass

### 5.1 Canonical tiles

Keep 4096 **native intervals** per full tile. A tile `[a,b)` owns `n=a..b-1`; its finite target points can be represented by phases `0..1023` in those intervals. The final original sample is covered separately. An empty input remains an error; a one-frame input needs no interpolation.

Buffer raw input plus the 777-frame halo. A tile becomes executable when its required actual right context exists, or at EOF after applying the selected global edge policy. Never extend a local candidate window by repeating its own endpoints.

Use absolute `i128` coordinates at tile, stream, and HQ-domain boundaries. Within validated contiguous slices, convert once to bounded `usize` offsets. The raw reductions, direct FIR batches, and local surveys should not perform `i128` division, remainder, or checked conversion per sample.

### 5.2 Validation and chunk invariance

Preserve the current complete validation of a caller push before committing measurement state. An incomplete or nonfinite push must leave the meter equivalent to its state before that push. Empty pushes remain no-ops.

Do **not** use the caller-wide `validation_peaks` reduction as the search witness for an earlier tile. A very large push would then reveal future peaks that a sequence of small pushes has not revealed, changing pruning and possibly the returned interval.

Instead, when a canonical tile is ready, update its same-channel witnesses from the tile's own finite endpoint samples and previously processed canonical tiles. Treat raw halo samples as bound inputs, not early finite-domain peak witnesses. Apply this rule even though the complete caller push has already been validated.

Caller chunk boundaries must not change tile ownership, pruning order, dense/sparse decisions, work counts, point results, or intervals for a fixed execution backend.

### 5.3 Fused summaries

The common path needs validation/copying plus an inexpensive native-input summary pass, not three reconstructed-stream passes.

Build native amplitude maxima and second differences from contiguous channel slices. Reduce second differences into fixed 32-start summary bins. For each 256-interval root, combine the bins intersecting `[a-775,b+773]`. Widening to a bin's available boundaries is conservative; clamp partial bins to the already available tile buffer.

A tile of 4096 intervals and its halo has only a few hundred summary bins. A straightforward scan over the relevant bins is sufficient. Do not build a generic segment tree, general range-query service, or per-sample heap.

For a surviving 256-interval root, test 32-interval children. A child initially inherits its parent's `D_upper` and numerical bound; only its endpoint-sample maximum is recomputed. This avoids rescanning a roughly 1550-start halo for every child. Tighter child curvature summaries are a later, measured optimization, not a first-version requirement.

Keep raw summaries and active-span records tile-local and reusable. Do not retain an album-sized sample array or a list of every rejected cell.

## 6. Rejection must also preserve the HQ4 diagnostic

Maintain two distinct same-channel lower witnesses:

```text
L4[c]   = proven lower witness from original samples and evaluated HQ4 knots
Lall[c] = proven lower witness from all evaluated HQ1024 knots
```

Initially update both from the current canonical tile's exact input samples. After reconstruction, update `L4` only from genuine HQ4 evaluations with their numerical errors subtracted. Any valid HQ1024 evaluation may improve `Lall`.

**Raw rejection uses `U_g <= L4[c]`, with no accuracy-ratio slack.**

This is intentional. `fast_hq4_peak_linear` is documented as the complete HQ4 maximum. Pruning against a fine-knot winner or against `ratio*Lall` could skip an HQ4 value larger than all observed HQ4 values. Pruning against a certified HQ4 lower witness proves that omitted HQ4 values cannot change that maximum, while simultaneously proving that the entire omitted HQ1024 region cannot beat the already known overall lower witness.

Track the HQ4 point estimate from evaluated values; the skipped regions are certified noncompetitive for that diagnostic. This preserves a complete maximum without computing every HQ4 value. It does not preserve the old physical-work count of eight knots per stereo input frame, which is precisely the work this design removes.

Screen roots, then children, against the current `L4`. Order surviving children by descending raw upper, with ascending native coordinate as the tie-breaker. Improvements to the lower witness may reject a previously admitted child before reconstruction. Such a child still needs a bound comparison; a top-K cutoff is not a certificate.

## 7. Selective first-stage reconstruction

### 7.1 Required coarse support

For a surviving child `[a,b)`, retain coarse values on:

```text
[2*a-16, 2*b+16], inclusive.
```

The odd coarse knots in this support have native half centers `m = a-8..b+7`. A 32-interval child therefore requests 48 half knots before merging overlaps. The even coarse knots are exact original samples.

Merge overlapping requested half-center intervals before executing them. Adjacent survivors should not repeatedly compute the same 1536-tap dot product. A fixed list of at most 128 child records per full tile is enough; there is no need for a map keyed by every absolute HQ coordinate.

### 7.2 Sparse direct executor

Evaluate the *existing* symmetric first-stage FIR:

```text
z[2*m+1] = sum(j=0..767)
             h[j] * (x[m+768-j] + x[m-767+j]).
```

For ordinary amplitudes, implement a scalar authority graph and explicit AVX batches across four consecutive half centers. Each AVX lane is an independent output; both input loads for a given tap advance with the output center, so the hot loop does not need gathers or cross-lane reversal. Use multiple independent output batches/accumulators when measurements show a dependency-chain bottleneck. No FMA or AVX2 may be required.

The symmetry halves coefficient multiplications; it does not halve input support. A source window must contain the full support of every requested output.

Derive the direct error using the current `dot_rounding_upper` machinery and the first-stage L1 norm. For the ordinary single-chain pair graph, an operation count of 3072 is a conservative starting account for 768 pair-add/multiply/accumulate steps. The precise accepted count must be attached to the implemented graph and tested. Do not borrow the FFT envelope for a different direct arithmetic graph.

One conservative form is:

```text
E_half = dot_rounding_upper(3072, upper_mul(H_l1_upper, X_support))
```

Use the existing scale-aware approach for extreme inputs. Scaling, additional reductions, or a different graph require their own accounted operations; 3072 is not permission to ignore those operations. Direct coefficients remain the frozen binary64 values.

Dispatch AVX once per meter/backend, not per output or tap. Scalar results need not be bitwise identical to AVX results if graphs differ, but each must satisfy its declared enclosure and the final accuracy gate.

### 7.3 Dense executor and crossover

Use the existing qualified 8192-point transform when the union of requested half knots in a channel pair is sufficiently large. Tune one private, deterministic count threshold on the target processor. Measure candidate thresholds 32, 64, 128, 256, and 512; select a fixed value from those measurements. Do not introduce runtime timing, adaptive autotuning, CPU-model databases, or public configuration.

The count must represent unique requested positions after merging, not the number of child records. Pairing channels is already present in the FFT implementation. When only one channel needs dense execution, the other packed component may be zero; do not create a quiet-channel error dominated by an unrelated loud channel unnecessarily.

For dense execution of a full 4096-interval tile, the available input window `[a-777,b+777]` has 5651 frames. It fits in one existing overlap-save block, whose useful-input capacity is `8192-1536+1 = 6657`.

A minimal safe adaptation is a crate-private finite-window wrapper over the current qualified executor. Supply that contiguous raw window at its real absolute start index, use zero internal history only before the supplied window, flush the partial block, and retain only the requested coarse range. For this retained range, every actual FIR support lies inside the supplied window; discarded startup outputs may depend on the artificial history, retained outputs do not. Add a test of that fact against the direct evaluator.

Reuse scratch allocations and the shared plan across active tiles. Do not construct or initialize the expensive FFT plan merely to process an all-rejected tile or a one-frame input.

The current FFT's per-packed-component maximum and numerical envelope remain applicable only if the arithmetic graph, geometry, scaling, and spectrum construction remain unchanged. Preserve those conditions. A new FFT factorization or a float32 FFT is outside the initial implementation.

### 7.4 Do not pay both costs

Make the initial sparse/dense choice from the admitted span mask before executing first-stage dots. Later witness improvements may discard work, but must not trigger repeated direct-to-FFT-to-direct execution of the same values. One deterministic escalation to the dense path is acceptable only if a measured implementation cannot collect the needed mask first; the preferred design collects it first.

## 8. Local HQ4 survey and accuracy resolution

### 8.1 Survey only active spans

Reuse the frozen 24-nonzero-coefficient midpoint row, the existing symmetric midpoint kernel, and its numerical accounting. Generate quarter-sample values only for surviving spans, including the endpoint values needed by their bounds.

Fuse local magnitude reduction into survey production where practical. Keep a small contiguous local survey for refinement; do not allocate or zero-fill a full-tile survey with placeholders for rejected regions. A placeholder is not a reconstructed zero.

Update `L4`, `Lall`, the HQ4 point diagnostic, and the reported point witness from actual evaluations. Their numerical errors remain attached to the source support and channel that produced them.

### 8.2 First try the existing flat bound locally

For each active span, compute the local coarse magnitude and second-difference enclosures. Apply the existing HQ4 bound using the frozen `A4` and `B4` metadata. This is the existing algebra, not a newly designed filter.

When that upper satisfies the 0.01 dB threshold against `Lall[c]`, retire it as a terminal bound. No candidate polishing is needed solely to make the point match Reference.

A tighter global lower obtained later only makes a previously accepted terminal bound safer. Therefore it is unnecessary to reopen old tiles merely because the winner improved.

### 8.3 Resolve only the remaining phase intervals

For a span that does not meet the width criterion, expose its quarter-cell roots to the existing dyadic tail hierarchy. Within a coarse cell, the two quarter-cell roots correspond to metadata nodes 2 and 3, covering tail phases `[0,256]` and `[256,512]`.

For a node with already evaluated endpoints, compute:

```text
U_node = max(endpoint_left_upper, endpoint_right_upper)
         + A_node * D2_coarse_upper
         + B_node * M_coarse_upper
```

Use outward arithmetic. `D2_coarse_upper` includes both the arithmetic error of the observed differences and propagation of the coarse-value errors. `M_coarse_upper` likewise includes coarse uncertainty. Reuse the existing frozen node metadata and 34-position tail support.

If the bound is noncompetitive or meets the same-channel tolerance threshold, retire it. Otherwise evaluate its dyadic midpoint with `tail_dot`, update the lower witness, and replace the parent with its two children. Never retain the old parent's upper as an additional unresolved region after replacement.

A depth-first stack, visiting the child with the higher upper first and breaking ties by lower HQ coordinate, is sufficient. Resolve one coarse cell or small active span at a time. Stack depth is bounded by the nine remaining tail subdivisions; a heap for the entire recording is unnecessary.

At width one there is no unevaluated interior knot. Its upper is the maximum of its endpoint uppers; the endpoint at tail phase 512 is the adjacent coarse value, not `bank[512]`.

### 8.4 Accuracy is mandatory; heuristic quotas are not

Do not reapply the old 64-nominee, eight-finisher, or 104-fine-knot caps to this mandatory resolver. Those caps can return a valid but insufficiently accurate result. Optional witness proposals may be capped; unresolved domains may not be discarded when a proposal cap is reached.

No clock is consulted. Work remains deterministic and finite. Difficult material can force substantially more work, up to exhaustive finite target evaluation within a tile. The release wall gate applies to the operator's specified programme benchmark, not an invented worst-case constant-time promise over all arbitrary binary64 arrays.

This distinction must remain explicit: a pathological input is not allowed to invalidate containment or quietly waive the accuracy floor, and success on ordinary programme is not proof of a universal 0.50 s/min bound.

### 8.5 Numerical ambiguity

If FFT error, especially in an imbalanced channel pair, prevents satisfying the tolerance, recompute the still-competitive coarse support directly and rebuild the affected evaluations/bounds. Tighten all remaining competitive nodes, not only the currently estimated winner.

The direct path already exists for sparse work; this is reuse, not a second reconstruction. Replace stale uncertainty in the active coverage records. The maximum historical evaluation error may remain in diagnostics, but must not remain an irrevocable lower limit on the returned certificate width.

## 9. Certificate reduction and public behavior

Maintain a coverage partition: every owned finite target region is either certified dominated, or represented by terminal bounds that cover it. Refining a node replaces it with a covering partition of children.

For each channel, retain the maximum terminal upper from completed work and the best proven lower witness. Raw-rejected regions can be retired against `L4`; their maxima cannot exceed `Lall`. Tolerance-resolved terminal bounds may exceed the lower witness, but only within the accepted ratio at their time of retirement.

At finalization:

```text
L_channel = maximum valid same-channel lower witness
U_channel = maximum of the terminal coverage uppers and required exact endpoints
U_channel = min(U_channel, outward(4.68 * exact_channel_input_peak))
```

Both candidate uppers must be independently valid before taking their minimum. Do not force a false certificate by clipping an upper to the desired width. Do not inflate a point or lower witness to make the interval appear narrow.

The overall interval is the maximum of channel lowers and the maximum of channel uppers. Passing the overall width alone is insufficient: a loud channel must not conceal a poor interval for a quiet channel.

Keep the reported point numerically supported and inside the returned interval. Preserve the established treatment of any last-bit reconciliation; a new test must not mistake a rounded point for an exact lower witness.

Return `Complete` only when unresolved coverage cannot exceed the applicable proven lower winner under the established semantics. Meeting 0.01 dB alone does not imply `Complete`; `WorkLimited` can still describe a valid, adequately narrow result. Never return `TimeLimited` for this implementation.

Preserve `Debug`, `Clone`, error variants, finite-domain frame counts, both edge policies, and the separate Reporting/Standard/Reference behavior. The new backend must not alter those other tiers' search policies or coefficient identities.

## 10. Diagnostics without breaking the freeze

The struct fields, method signatures, enum variants, features, and public constants remain unchanged. Diagnostic *work counts* naturally change when work is avoided; maintaining a fictional full-scan count would misrepresent the optimization.

Use `groups_rejected`, `groups_expanded`, `strict_coarse_evaluations`, `phase_evaluations`, and related generic work counters for their actual operations. Count directly computed half knots as strict coarse evaluations. Count HQ tail evaluations actually performed, not phases represented by a rejected bound.

`fast_survey_knots` should count unique HQ4 knots actually reduced as values. `fast_hq4_peak_linear` remains the complete maximum established by values plus dominance proofs, as described in section 6. `fast_input_sample_peak_linear` remains the exact input maximum. Remove the old test that equates physical survey work to `channels*(4*(frames-1)+1)`; replace it with a coverage invariant and truthful-work checks.

Do not relabel raw-screen regions as V2 nominees. Old optional-policy counters remain present and zero when that policy is not executed. Use generic phase counters for mandatory dyadic resolution. Do not assign new meanings to retired clock-limit counters; they remain zero.

Preserve the feature-gated `FastCommissioningMode` variants and `new_fast_commissioning` signature. `Production` runs the new full production search. The two earlier ablation names, `SurveyBoundsOnly` and `NominationOnly`, both stop after screening, selective HQ4 survey, and flat bounds when the old nomination stage is absent; document that equivalence rather than keeping an otherwise unused nomination implementation solely to differentiate them. These diagnostic ablations may return a wider truthful `WorkLimited` certificate, are never selected by the normal constructor, and cannot establish the production accuracy or wall gate. Their original declarations remain intact; update only explanatory comments and benchmark labels.

Under the existing `fast-stage-timing` feature, attribute direct/FFT coarse execution to the existing prefix stage, actual local HQ4 work to the survey stage, and raw/coarse bound construction to the envelope stage. Attribute the mandatory resolution work coherently to the existing refinement aggregate; update explanatory comments and the example's accounting so it is neither omitted nor double-counted. Nonexecuted nomination/proposal sub-stages remain zero. No public timing field is added.

Temporary occupancy and direct/FFT crossover detail can be collected in crate-private test/commissioning code or the supplied offline probe. It must not create another feature or new public diagnostics surface. Normal builds must contain no stage clock reads.

## 11. Implementation map and order

### 11.1 Check the proposed work reduction before the main rewrite

Run `tools/screening_probe.py` on the actual two-minute 192 kHz input. It uses only same-channel canonical-tile sample witnesses, so it does not receive an unfair global-peak oracle. Its survivor mask is conservative relative to a production implementation that can improve `L4` from reconstruction.

Record admitted interval fraction, unique half-knot requests, and pair-tile counts at the candidate dense thresholds. These are work-density observations, not speed claims. If nearly every tile remains dense, the intended savings are not established for that input; do not claim the target is likely merely because the one-second bundled fixture prunes well.

### 11.2 Land the native screen and its proof boundary

Add one private raw-screen module or an equivalent private section in `fast_scan.rs`. Add generated private `A_raw`/`B_raw` metadata and its source hash. Port the range algebra and runtime rounding enclosure. Test this screen independently before connecting rejection to skipped reconstruction.

Retain the original source archive untouched as the comparison baseline. Do not regenerate from an earlier Fast bundle.

### 11.3 Add a bounded raw tile buffer and direct prefix batches

Refactor Fast's input ingestion so raw screening happens before first-stage execution. Keep validation transactional and use canonical tile witnesses. Add the symmetric direct half-delay scalar and AVX kernels, interval merging, and reusable scratch storage.

Keep raw buffering local to Fast. Do not replace the Standard/Reference raw buffer or refactor every scanner into a common framework.

### 11.4 Add lazy dense execution and local surveys

Expose only the minimal crate-private finite-window capability needed from `qualified_half_delay_fft.rs`. Preserve its existing public-to-the-crate streaming interface for the other tiers. Keep its arithmetic graph and coefficient/error artifacts unchanged.

Replace full-tile survey materialization with contiguous active-span surveys. Implement the strict `L4` rejection rule and prove coverage before removing unconditional work.

### 11.5 Add tolerance resolution and final reduction

Reuse frozen dyadic metadata and existing numerical tail helpers. Implement terminal coverage replacement, per-channel stopping, and direct tightening of numerically ambiguous active supports. Remove the old optional-policy machinery that is no longer called; do not retain a second production Fast backend as a permanent switch.

Update tests whose purpose was to enforce the superseded nomination quota or unconditional full survey. Preserve tests of the waveform, numerical enclosures, edge behavior, transactional errors, and API. Retire obsolete policy tests deliberately rather than weakening unrelated assertions.

### 11.6 Fix the one known incorrect test

The supplied source's `v2_signed_fits_round_only_bounded_local_offsets_and_respect_ties` asserts a positive offset for `(0.8,1.0,0.6)`. The correct signed offset is negative, approximately -1/6; transposing the neighbors gives +1/6. Correct the test, not the production sign. Exercise the assertions that previously followed the failing assertion if the helper remains. [S1, lines 118-129; S2, `src/fast_scan.rs:420-437,2413-2428`]

The three defects from earlier deliveries are already fixed in this baseline. Do not reintroduce or redundantly patch the missing import, orphaned caller, or stale midpoint metadata literal. [S1, lines 113-116]

### 11.7 Measure and finish

First obtain correct containment and the required width. Then select the direct/FFT crossover and remove proven hot-loop overhead. Stop adding machinery once the accuracy and wall gates pass. The 0.50 stretch is worth measuring, but is not permission to relax containment or to ship unqualified arithmetic.

The expected edit footprint is Fast's internals, a minimal private prefix adapter, generated raw metadata, affected tests, and the benchmark/qualification documentation. `src/lib.rs` may change private wiring and explanatory comments but not the frozen public surface. `Cargo.toml` retains the sole dependency, feature set, MSRV, and release profile.

## 12. Performance acceptance

### 12.1 What success means

For the operator's 120-second input:

```text
0.66 s/min gate: total_wall_seconds <= 1.32
0.50 s/min stretch: total_wall_seconds <= 1.00
```

For a 40-minute album, those rates correspond to 26.4 and 20 seconds respectively, subject to the actual end-to-end album measurement. Do not substitute a repeated short fixture and claim an album result.

Use `bench_ceiling_f64le`'s total elapsed time, including meter construction, file read, Float64 decoding, pushes, and finalization. The benchmark's timer boundaries remain unchanged. Compilation and the separate offline oracle are not inside that measurement.

The shipping measurement is uninstrumented. A feature-enabled attribution build cannot set a release-success flag. Keep the benchmark's existing failure behavior when the wall target is missed, and add an example-local accuracy gate derived from the returned per-channel intervals. No API addition is needed for that check.

### 12.2 Machine and build discipline

The binding processor is AVX-capable Ivy Bridge-EP without AVX2 or FMA. Do not make a result from the development container or a newer processor the gate. Keep single-threaded crate execution; there is no hidden worker pool.

Use the declared rustc 1.93-or-newer environment and the brief's vectorizer workaround where needed:

```sh
RUSTFLAGS='-C llvm-args=--vectorize-loops=false'
```

The operator reports that the current source crashes rustc 1.93.1's loop vectorizer and that auto-vectorization contributes no measurable speed on the baseline. Explicit AVX kernels are independent of that loop-vectorizer setting. Do not lower the MSRV or patch for 1.80 as part of this task. [S1, lines 93-98,136-148]

Build baseline and candidate with the same compiler, flags, release profile, and target configuration. Use separate target directories for instrumented and uninstrumented binaries so a stale feature build cannot be mistaken for the shipping executable.

### 12.3 Paired runs

Record the input hash, sample rate, frame count, channels, compiler version, flags, governor/clock condition, and raw JSON for each run. Compare baseline and candidate in paired, alternating order rather than always warming the CPU with the baseline first.

Take five measured runs in each of the brief's two representative clock conditions. Report the individual results and median; do not hide the idle condition or select only the fastest clock-ramped run. Treat a median miss in either representative condition as a miss of the binding gate, and show outliers rather than disguising them as an accuracy trade.

Run the actual album once after the two-minute gate passes, including finalization and its real boundaries. Run the separate 48 kHz fixture and a small set of 44.1/48 and 176.4/192 kHz material to detect a narrow benchmark-only improvement. Those are regression evidence, not newly invented mandatory wall gates for every sample rate and adversarial signal.

### 12.4 Diagnose misses by actual work

Measure raw-screen cost, admitted interval fraction, direct half-knot work, dense FFT work, local survey work, and mandatory refinement. A sparse-path miss calls for hot-loop and allocation work; a dense-path miss calls for examining the native bound's selectivity or the measured crossover. A large unexplained remainder calls for validation/copying and coordinate-indexing inspection.

Do not respond to a miss by truncating filters, skipping unbounded regions, reducing sample rate, disabling numerical envelopes, returning only a guessed peak, or using wall-clock cutoffs.

The primary expected gain is removing unconditional first-stage and survey execution. The design does not depend on an asserted 3x improvement to an FFT that has not been measured.

## 13. Required executable validation

### 13.1 Public freeze and unchanged tiers

Compare the complete public declarations in `src/lib.rs` under default features and `fast-stage-timing`, including public constants, derives, associated methods, fields, and enum variants. Compare the feature and dependency declarations. A private-module change is allowed; an exported addition is not.

Use the existing public integration tests and example builds as compile checks. Run Standard, Reference, and Reporting regressions without loosening their tolerances to accommodate Fast. The existing coefficient checksums must remain identical.

### 13.2 Raw-screen proof and falsification

Regenerate the exact metadata and compare it byte-for-byte to the checked-in artifact. Check the stored binary64 bounds against the exact fractions, not a rounded decimal literal.

In Rust, directly enumerate the frozen finite target for short signals, using the existing independent dense oracle rather than the new screen or Reference's reported point. For every raw-rejected group, verify that the target maximum is below the recorded same-channel `L4` witness. Test both native parities, negative source coordinates in the halo, all phases, and the final endpoint.

Add tests with arbitrary coarse/raw arrays, alternating signs, impulses at both support limits, constants, ramps, short transients, cancellation, and exact zeros. An impulse just outside a requested support is especially useful for catching an overly narrow or accidentally local edge extension.

The supplied exact Python tests validate the coefficient composition, summation-by-parts identity, moment corrections, support-bin ranges, and outward constants. They are strong evidence for the new algebra, but do not test Rust buffer indexing, AVX behavior, or certificate state transitions.

### 13.3 First-stage direct and dense equivalence

Compare the new direct half-delay evaluations with an independently ordered, compensated or exact-dyadic scalar oracle and the declared numerical enclosure. Test scalar and explicit AVX paths separately. Include sign alternation and cancellation; agreeing on a sine wave is insufficient.

Verify that a retained finite-window FFT output depends only on its real supplied support and matches the frozen direct FIR within the qualified error. Exercise the first and last retained output, partial windows, mono, odd channel counts, and one active channel in a pair.

Force sparse and dense execution of the same requested spans. Their numerical results need not be bitwise equal, but both must satisfy independent containment and accuracy. Neither may depend on a local endpoint-padding shortcut.

### 13.4 Coverage and tolerance completion

Test that refining a loose parent removes its old upper from the active coverage. Construct a case in which keeping the parent would leave width above 0.01 dB even though its children resolve tightly.

Disable optional proposals and still require the floor. Force an input that would exceed the old 104-fine-knot cap; it must continue resolving competitive coverage instead of returning a wide production result. A dominated node may stop early; an unproved node may not disappear.

Use asymmetric stereo and multichannel inputs. Verify each channel against its own dense target and width. Include a loud channel paired with a much quieter nonzero channel to exercise direct tightening of FFT uncertainty.

Verify the HQ4 diagnostic separately: compare the bound-pruned scan's HQ4 maximum against a full independent HQ4 scan, accounting for the declared evaluation errors. This is completeness of the mathematical maximum, not a requirement for bitwise identity between different direct/FFT arithmetic graphs. A larger fine-knot winner must never justify suppressing a potentially larger HQ4 diagnostic value.

### 13.5 Streaming and exceptional inputs

Exercise lengths 1 and 2; short lengths around 32 and 256; and finite boundaries around 4096, 4097, and 8193 frames. Cover the existing 6657-frame FFT useful-input geometry in dense-adapter tests. Use one-shot input, single-frame pushes, prime-sized chunks, and chunks ending on every tested ownership boundary.

Compare point, intervals, status, and non-timing diagnostics for a fixed backend. Do not only compare rounded dB values. Clone a partially filled meter and complete the clones with different chunking. Reject a bad push between two valid pushes and compare against the clean stream.

Retain DAZ/FTZ tests for the new raw-difference and direct FIR graphs. Use a rounding-mode/MXCSR restoration guard in each test. Keep nonfinite input rejection, exact silence, huge finite values, and subnormal containment tests. Do not demand the ordinary-PCM relative-width gate where binary64 representation cannot express it.

### 13.6 Analytical and real-material accuracy

Reuse and extend the existing correctly band-limited analytical families with known phase/peak truth. Include positive and negative extrema and higher-band content within the qualified range. Compare against the actual analytical amplitude, not a rounded -6.0 dB label for a 0.5-amplitude tone.

For finite windowed or endpoint-affected signals, use the correctly extended finite oracle; a window changes the analytical waveform. Keep the bundled independent real-material reference as an anomaly check, not a universal proof of ideal reconstruction.

On the operator's actual benchmark, report per-channel finite widths and compare the point to the independently checked HQ target/Reference result. A wall pass with a failed width gate is a failed combined release. A width pass with a wall miss is also a failed combined release.

### 13.7 Test execution cost

Use optimized test builds for repeated dense numerical qualification; the brief explains why unoptimized Reference priming makes the full debug suite slow. Do not redesign Reference or reduce oracle coverage just to make the Fast development loop faster. Run the normal existing suite as a final compatibility check, with its known bad signed-fit expectation corrected or its genuinely obsolete policy test deliberately retired.

## 14. Evidence produced for this design

The exact metadata generator completed against coefficient-source SHA-256:

```text
7070c2e9abc255062dd30aaa516c0827d969d238759e14a61c5d1da94a67de9d
```

Four exact-arithmetic test cases passed. The main case checks every native phase against a separately summed first-stage-plus-tail cascade and the residual identity on exact integer input. It also verifies enclosure by the derived bound. Constant/affine, summary-range, and outward-rounding checks passed. See `evidence/exact_test_log.txt`.

The screening-only pilot on the attached one-second 48 kHz stereo fixture produced:

```text
native intervals across channels:       95,998
256-interval roots:                         376
roots rejected before reconstruction:      362
32-interval children surviving:              26
native intervals in surviving children:     832
surviving interval fraction:           0.866685%
unique requested half knots:              1,120
```

The pilot deliberately uses only sample-based canonical-tile lower witnesses. It does not use a Reference answer, a whole-file peak known in advance, tolerance slack, or future halo samples as witnesses. Reconstruction can improve those witnesses further.

This fixture is a one-second, duplicated-mono saxophone excerpt, as documented in the attached crate. It is not the operator's 192 kHz programme. The pilot does not include first-stage execution, tail resolution, input decoding performance, or Rust throughput. Therefore **0.866685% is a work-density observation, not a claimed wall-time speedup**.

No Rust compiler was available in this environment. The supplied Rust crate's full tests and the proposed Rust implementation were not built or run here. The evidence establishes the new bound and one input's selectivity; the target-machine implementation and end-to-end gates remain executable handoff work.

## 15. Commands for the implementing session

Set the input crate path to the extracted, attached baseline, not an earlier bundle:

```sh
CRATE=/path/to/tonepoet-true-peak-api-frozen-2026-09-10/crates/tonepoet-true-peak
DESIGN=/path/to/fast-input-screen-design

python "$DESIGN/tools/generate_raw_screen_metadata.py" \
  --crate "$CRATE" \
  --output /tmp/raw_screen_metadata.json
cmp /tmp/raw_screen_metadata.json "$DESIGN/evidence/raw_screen_metadata.json"

python "$DESIGN/tools/test_raw_screen.py" --crate "$CRATE"
```

The generator and exact tests use only Python's standard library. The screening pilot additionally needs NumPy:

```sh
python "$DESIGN/tools/screening_probe.py" \
  --input /path/to/the-actual-120-second-stereo-192k.f64le \
  --channels 2 --sample-rate 192000 --edge repeat \
  --output /tmp/actual_192k_screening.json
```

After implementing the design, build from the standalone crate. Keep attribution and shipping target directories separate:

```sh
cd "$CRATE"

RUSTFLAGS='-C llvm-args=--vectorize-loops=false' \
CARGO_TARGET_DIR=target/fast-shipping \
cargo test --release --no-fail-fast

RUSTFLAGS='-C llvm-args=--vectorize-loops=false' \
CARGO_TARGET_DIR=target/fast-shipping \
cargo build --release --example bench_ceiling_f64le

RUSTFLAGS='-C llvm-args=--vectorize-loops=false' \
CARGO_TARGET_DIR=target/fast-attribution \
cargo build --release --features fast-stage-timing --example bench_ceiling_f64le
```

The existing argument order is `<path> <sample-rate-hz> <channels> <tier>`. Run the same input against both builds:

```sh
INPUT=/path/to/the-actual-120-second-stereo-192k.f64le

target/fast-shipping/release/examples/bench_ceiling_f64le \
  "$INPUT" 192000 2 fast > fast-shipping.json

target/fast-attribution/release/examples/bench_ceiling_f64le \
  "$INPUT" 192000 2 fast > fast-attribution.json
```

The uninstrumented example prints JSON and then exits unsuccessfully when the existing wall gate fails; retain both that JSON and the exit status. Save both builds' records, but only the shipping build may establish the wall gate. Extend the example's JSON with per-channel widths and its private accuracy-pass result; do not change the positional command-line contract.

## 16. Completion criteria and exclusions

The handoff is complete only when the frozen public surface is unchanged; numerical/finite-target containment tests pass; ordinary-PCM per-channel widths and analytical point errors pass; the supplied fixture remains correct; and the operator's uninstrumented benchmark meets 0.66 s/min in the reported representative clock conditions. Report 0.50 s/min separately as achieved or not achieved.

Retain raw evidence for the baseline comparison, selected direct/FFT threshold, and final result. Do not present estimated operation counts or an instrumented run as a measured shipping pass.

Do not add a new FFT dependency, new public tier, new feature, float32 target, approximate replacement waveform, source-rate-dependent downsampling rule, audio classifier, worker pool, generic scheduling framework, migration layer, persistent benchmark switch, or compatibility facade for the retired internal candidate policy. Do not optimize Standard, Reference, Reporting, or tonepoet's conversion pipeline in this change.

The central implementation rule is simple: **prove that a region matters before paying to reconstruct it; once it matters, resolve it far enough to meet the stated accuracy.**

## Source references

**[S1]** Attached `BRIEF_true_peak_api_frozen_2026-09-10.md`. The public freeze is at lines 12-28; scope and standalone dependency at lines 30-35; operator measurements at lines 37-98; known test state at lines 100-129; build constraints at lines 131-148; earlier outcome at lines 151-159.

**[S2]** Attached `tonepoet-true-peak-api-frozen-2026-09-10.tar.gz`, SHA-256 `c0f2fd8647c6bd1bc67474bfff6053826fd19531a92d310b6538deaf952a7d07`. Paths and line numbers in this document refer to its unmodified crate. Especially `src/lib.rs`, `src/fast_scan.rs`, `src/qualified_half_delay_fft.rs`, `src/certified_scan.rs`, `src/hq1024_coefficients.rs`, and `src/qualified_prefix_coefficients.rs`.

**[S3]** Within the same archive, `docs/tonepoet_true_peak_reference9_fast90_design.md`, sections 7-10; and `qualification/generate_hq1024.py`, especially `exact_node_bound`. These provide the existing target definition, summation-by-parts construction, and limitations of analytical claims. New native-input constants were independently derived from the frozen stored coefficients, not assumed from the design prose.

**[E1]** ITU-R BS.1770-5, Annex 2 and its Attachment 1, printed pages 18-22; PDF page indices 19-23. Official publication: <https://www.itu.int/rec/R-REC-BS.1770-5-202311-I>. The oversampling table on printed page 21 was visually checked.

**[E2]** EBU Tech 3341 (2023), true-peak tests 15-23, printed page 9. Official PDF: <https://tech.ebu.ch/docs/tech/tech3341.pdf>. The tolerance table was visually checked.
