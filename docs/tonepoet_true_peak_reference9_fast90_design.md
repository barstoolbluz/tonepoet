# Tonepoet true peak: a faster reference and a 90-second high-accuracy scanner

## Decision

Build one **2x front end with hierarchical, certified peak search**, and give it two execution policies:

- **Reference9:** complete the search, including numerical rescoring where necessary. The 40-minute commissioning album must finish in no more than 540 seconds.
- **Fast90:** bound the additional search work, spend that work on the largest remaining upper bounds, and return the achieved peak interval. The same album must finish in no more than 90 seconds.

First run this engine against the **unchanged finite Headroom64 reconstruction**. That produces a faster, accuracy-preserving replacement for today's reference evaluator and an independent validation target for the new search. Then qualify a separate, substantially more accurate reconstruction, **HQ1024**, for the two new policies.

The important next optimization is not another shorter first filter. It is to stop constructing the 4x stream everywhere. Compute only the reference-quality 2x stream everywhere; prove that most groups of intervals cannot contain the maximum; evaluate finer phases only where that proof does not settle the question.

There is a second, independent improvement: the current 64x filters are not close enough to ideal interpolation for a much denser grid alone to deliver the desired accuracy. The concrete HQ candidate below changes the first filter, replaces the later Blackman half phases, and makes a 1024x target available **without scanning a 1024x stream**.

**Status:** this is an implementation design supported by source inspection, independently rerun existing numerical qualifiers, a new executable numerical probe, and candidate coefficients. It is not a completed Rust implementation or a measured 90-second/540-second release. No commissioning album or benchmark machine was included in the archive. Rust/Cargo were unavailable in this analysis environment.

## 1. Scope and acceptance targets

### 1.1 Workload and timing

The working commissioning workload is the previously discussed **40-minute, 192 kHz, stereo, retained Float64 PCM carrier**, on the same machine used for the user's successful implementation. Confirm that workload from the operator's baseline record rather than substituting a generic workstation specification. The source bundle does not supply an actual full-album benchmark record or identify the machine that produced the reported speedup.

For this workload, the arithmetic is:

```text
Programme length:        2,400 seconds
Original frames:         460,800,000
Channel samples:         921,600,000
Float64 input bytes:   7,372,800,000

Fast90 minimum rate:       5,120,000 original frames/second
Fast90 input throughput:      81.92 MB/second, before extra I/O costs
Reference9 minimum rate:      853,333.333 original frames/second
```

These are required rates, not predictions of implementation performance. No finite implementation can promise the same elapsed time for arbitrarily many channels, arbitrarily high sample rates, or arbitrarily slow storage. Report the measured workload, CPU, compiler, SIMD backend, storage/cache state, and concurrency beside every result.

The limits govern the **carrier scan**, including file reading, Float64 decoding, meter construction, processing, finalization, and any final candidate rescoring. They do not silently include or exclude a second pass. There is no second DSD reconstruction and no required second carrier scan. Measure end-to-end conversion separately when decoding or encoding is also of interest.

### 1.2 Deliverables and quality gates

The compatibility deliverable is an optimized legacy-64 evaluator that returns the same mathematical finite peak and preserves the legacy reporting calibration and ceiling contract. Its acceptance gate is <=540 seconds with no material loss of legacy numerical accuracy. Keep the original direct evaluator as the oracle until this passes.

The preferred new reconstruction is HQ1024. For it, use these initial release objectives:

```text
Reference9:
  wall time <= 540 seconds;
  finite-target interval width <= 0.0000001 dB on the commissioning corpus;
  prefer proof-complete search with only the qualified evaluation enclosure left;
  qualified-band analytical point-error objective <= 0.0001 dB.

Fast90:
  wall time <= 90 seconds;
  finite-target interval width <= 0.0001 dB on the commissioning corpus;
  continue toward proof-complete search when the fixed work allowance permits;
  measure actual point error separately from interval width.
```

The last two numbers are **design objectives**, not published guarantees. Fast90 must not add 0.0001 dB as a fixed safety reserve: it uses the upper endpoint it actually proved. Failing either the time or the width objective fails that combined release objective. A wide but valid interval is a safe degraded result, not a successful high-accuracy benchmark.

The point-error objective applies to properly constructed, in-band analytical tests and to separately described independent-reference comparisons. It is not an arbitrary-signal theorem derived from passband ripple. All finite-target containment tests, including out-of-band inputs, have a different and stronger obligation: the target's actual finite peak must lie in the returned interval.

For silence, report silence and a zero linear interval rather than fabricating a dB width. For very small nonzero signals, retain a meaningful absolute linear interval even when relative width is unhelpful.

## 2. What the supplied implementation actually does

The archive is `tonepoet_true_peak_green_b682e47_2026-09-08.tar.gz`. The commit-like token is part of its supplied filename; this review did not independently resolve it against a Git repository.

Its relevant components are:

- `crates/tonepoet-true-peak/src/lib.rs`: legacy reference, prefix implementations, public modes, numerical allowances, finite-reconstruction contract, and coefficient regression tests.
- `crates/tonepoet-true-peak/src/reference_fast.rs`: the implemented accelerated-reference scanner.
- `crates/tonepoet-true-peak/qualification/verify_reference_fast_scanner.py`: independent coarse-to-reference residual and tail-factorization checks.
- `crates/tonepoet-true-peak/examples/bench_ceiling_f64le.rs`: retained-carrier scan benchmark.
- `src/convert/pipeline/stages.rs`, particularly the PCM scanner and gain-authority selection near lines 32525-32610: production integration.

### 2.1 Reference is still the direct six-stage path

`HeadroomEngine::process_frame()` in `lib.rs` evaluates the complete cascade with nested per-phase processing. The original-rate half phase uses a 384-tap symmetric FIR with 192 products per original frame/channel. The later stages use 49, 25, 17, 13, and 9 nominal taps.

The documented reference arithmetic count is:

```text
192 + 48 + 48 + 64 + 96 + 128 = 576 coefficient products
per original frame/channel.
```

The code already removes redundant identity-phase multiplication and uses doubled rings. It deliberately preserves the original reference accumulation order. Therefore, merely suggesting ring buffers or identity-phase specialization would repeat work already present.

### 2.2 Fast computes a full reference-quality 4x prefix

`FftReference4Engine` already executes the 384-tap half phase with 2048-point, paired-channel overlap-save convolution. Its payload is 1665 original frames. It then runs the 49-tap stage with symmetric pair products to create every 4x sample.

`ReferenceFastScanner::process_tile()` uses 4096-original-frame tiles, computes the cubic/Bernstein envelope for every 4x interval, and selects at most 64 intervals per tile. A selected interval evaluates all 15 missing phases of the composed 4x-to-64x tail, across the channels. Selection is already bounded using partial partitioning before sorting; replacing an imagined full sort is not the main opportunity.

The implementation's preferred finite-reference width is 0.010 dB. Unresolved uppers remain in the result when the cap is exhausted. Readiness is already checked per FFT block rather than per coarse knot.

### 2.3 Concrete remaining opportunities

There are four substantive opportunities:

1. Move the reference's expensive first stage to the already-established FFT execution strategy and process later stages in blocks.
2. Remove the unconditional 2x-to-4x stage from the fastest scanner.
3. Replace an unconditional per-4x-cell cubic calculation with a cheap group certificate on the 2x stream.
4. Refine a phase interval hierarchically instead of computing every missing phase of each candidate cell.

Reuse scratch storage and reduce repeated indexing, copying, and per-sample error storage as part of that change. Those are supporting optimizations, not substitutes for eliminating unnecessary reconstruction work.

### 2.4 Do not confuse the mode names

The ordinary PCM UI's Fast path is `new_accelerated_reference()`. The older `HeadroomScanMode::Fast` and `Fastest` still name direct 16x/8x paths used by existing DSD policy. The source default `HeadroomScanMode` is Standard. Preserve these distinctions during implementation; do not accidentally redirect a DSD policy by changing a similarly named PCM mode.

## 3. Accuracy has three different meanings here

### 3.1 Search accuracy

For a fixed finite reconstruction, let its actual peak be `P`. A valid search returns:

```text
L <= P <= U.
```

`20*log10(U/L)` is search/evaluation uncertainty, not the reconstruction's error relative to an ideal waveform. Once the legacy 64x finite maximum has been found and its arithmetic enclosed, more searching cannot make that waveform itself more faithful.

### 3.2 Reconstruction accuracy

A new filter or a denser grid can more closely approximate a separately specified continuous-time signal. That is a reconstruction change. Qualify it against analytical truth and independent references; do not call it an execution-only optimization.

The existing uncalibrated 64x cascade's sampled response audit in the new probe spans approximately -0.013463 to +0.013042 dB over `0..0.495*Fs`. The original first half-phase filter alone has approximately 0.01304 dB maximum passband lift. Several later Blackman phases contribute their own deficits. The existing scalar calibration does not remove frequency-dependent or phase-dependent error.

Consequently, running the existing filter on a 256x or 1024x grid alone is not the right high-accuracy upgrade.

### 3.3 Ceiling authority

The existing hard ceiling concerns a particular uncalibrated finite reconstruction with `RepeatEndpoints` and straight lines between its 64x knots. Its operator norm and terminal-error accounting belong to that reconstruction.

An HQ1024 result is not automatically an upper bound for that legacy waveform. An old 64x result is not automatically an upper bound for HQ1024. Neither is a theorem about an unspecified DAC or a lossy encoder's decoded output.

Keep the reconstruction identity attached to the certificate and to the terminal-error bound. No spectral-support declaration is inferred from a sample rate, file format, or DSD origin.

## 4. New numerical evidence included with this design

The executable probe is `research/tp_design_probe.py`; its output is `results/design_probe.json`. Candidate coefficients are in `results/hq1024_candidate_coefficients.json`.

### 4.1 A much cheaper search of the unchanged reference

For the bundled one-second, 48 kHz fixture, using its left channel, the new 2x search prototype:

```text
original frames:                      48,000
2x cells expanded to phase searches:      22
fine knots evaluated:                     74
nonzero tail coefficient products:      2,090
observed raw interval width:       0.000048594 dB
```

The full 64x waveform contains 3,071,937 nominal knots for that channel. This does not mean all computation fell by the ratio of those two counts: the first FIR, coarse stream, reductions, and screening still run. It does show that evaluating every missing phase is unnecessary on this fixture.

The prototype's raw lower and upper enclosed the independently exhaustively reconstructed finite peak within its stated floating-point comparison tolerance.

### 4.2 The HQ candidate did not make this fixture's search harder

For the same fixture and the HQ1024 target:

```text
2x cells expanded:                         18
fine knots evaluated:                      63
nonzero tail coefficient products:      1,822
observed raw interval width:       0.000091126 dB
```

The word "raw" matters. These are unbudgeted research searches using a directly computed coarse FIR stream, not the production FFT backend with its full numerical certificate. The count is a nonzero-coefficient arithmetic count, not an instruction count or benchmark.

### 4.3 The new reconstruction is meaningfully more accurate on analytical cases

Three supplied probe cases have a known underlying aligned peak of exactly 1.0, or 0 dB. They use a cosine-squared envelope and keep the complete modulated support below `0.495*Fs`.

For the low-band case, the existing calibrated reference reported about -0.002226272 dB; HQ1024 reported about +0.000000021 dB.

For the mid-band case, the existing reference reported about -0.006901306 dB; HQ1024 reported about -0.000002047 dB.

For the upper-band case, the existing reference reported about +0.001810888 dB; HQ1024 reported about -0.000003346 dB.

These three observations support qualification of the candidate. They are not a replacement for the existing much larger analytical suite or for a new adversarial search.

### 4.4 Independent real-fixture observation

The fixture README freezes a previous 256x libsoxr observation of -0.112265386 dBTP. This review did not rerun libsoxr.

The old calibrated reference, independently reconstructed in the probe, was about -0.109235448 dBTP: approximately +0.003029938 dB from that frozen observation. The HQ1024 finite peak was about -0.112284235 dBTP: approximately -0.000018848 dB from it.

This is a useful independent anomaly check, not a sub-0.00002 dB accuracy certificate. The references have different reconstruction and endpoint conventions, and the fixture is only one second of a duplicated-mono saxophone excerpt.

### 4.5 Adversarial cost remains real

An 8192-frame unwindowed upper-band multitone required 13,154 fine-knot evaluations and 354,766 nonzero tail products for legacy64. The HQ candidate required 13,003 evaluations and 358,671 products. Constant input required none.

Thus candidate sparsity is material-dependent. Fast90 needs a real work cap; Reference9 needs a dense-region execution path. The favourable fixture is not evidence that every possible input will be cheap.

The new longer reconstruction also changes out-of-band endpoint transients. For a finite alternating +/-0.7 sequence with repeated endpoints, the probe's legacy and HQ finite peaks were approximately 1.2470 and 1.4291. This is not a failure to reproduce either target: they are different finite waveforms. Do not advertise universal per-input dominance over the old reference.

## 5. Shared architecture

```text
retained original-rate f64 frames
    |
    +-- validate; sample maxima; raw history needed for edges/rescoring
    |
    +-- fixed-block FFT half-delay FIR
    |       exact integer phase + enclosed half phase
    |       => a 2x coarse stream, with error metadata
    |
    +-- canonical source tiles
            |
            +-- coarse lower bound
            +-- group maxima and second-difference maxima
            +-- certified rejection of whole groups
            +-- smaller groups for survivors
            +-- phase-tree refinement for surviving cells
            +-- strict final rescoring or bounded deferred work
            |
            +-- point estimate + [L,U] + reconstruction identity + diagnostics
```

There are two fixed reconstruction descriptors, not a general plug-in framework:

```text
Legacy64:
  existing filters, existing edge semantics, existing reporting calibration.

HQ1024V1:
  the new frozen filters, 1024x target, no legacy calibration,
  its own support, norm, arithmetic enclosure, and point qualification.
```

Reference9 and Fast90 share the selected descriptor and most implementation. Their primary difference is search completion policy, not a different low-quality interpolation front end.

Do not allocate, materialize, or write an album-length oversampled signal. The coarse stream and raw input context remain bounded, and the final phase rows are evaluated only at selected positions.

## 6. A faster implementation of the existing 64x reference

This is the lowest-risk delivery and should precede the HQ reconstruction change.

### 6.1 FFT first stage

Extract the common overlap-save operation already duplicated between the Standard and accelerated-reference implementations into a private fixed-block half-delay executor. It accepts the frozen coefficient set and a fixed, qualified FFT size. Its output is an original-rate identity block plus a half-phase block, with timing alignment and error metadata.

For Legacy64, start with the already-used 2048/1665 geometry. Do not change the filter, calibration, endpoint policy, or target grid during this step.

Use the planner API already present. RustFFT 6.4.1 already selects supported SIMD implementations; "enable AVX in the FFT" is not a new optimization to claim. [E1]

### 6.2 Block execution of the remaining stages

Implement the later stages over contiguous blocks with a short halo, rather than invoking all six stage objects in a nested loop for every original frame. Use channel-planar scratch internally, or paired planar channels where measurements justify it. Retain the interleaved public API.

The mathematically symmetric later half phases can use the same pair-product strategy already employed by the existing fast implementations. With appropriate enclosure of coefficient pairing and rounding differences, this cuts their modeled products from 384 to 192 per original frame/channel. The first-stage 192 direct products disappear into the FFT operation.

This is a reduction from `576 direct products` to `192 later-stage products + FFT work`, not a claim of a threefold elapsed-time speedup. Memory traffic, max reductions, and code generation still matter.

Keep the uncalibrated peak reduction fused into block consumption. Do not store a complete 64x block longer than necessary merely to scan it again. Preserve the exact nominal time interval when clipping startup and finalization output.

### 6.3 Numerical compatibility is not bitwise identity by accident

FFT execution and paired summation change rounding. Distinguish three requirements:

- The mathematical finite waveform remains Legacy64.
- Reported peak differences must be within the independently qualified evaluation enclosure.
- A bitwise legacy report, when required, needs legacy-order local reevaluation rather than an assertion that FFT results happen to be bit-identical.

Retain raw neighborhoods for potentially winning knots. Reevaluate the small set whose enclosed values can still affect the maximum using the original stage accumulation order or an independently certified higher-accuracy direct evaluator. Reevaluate every unresolved competitor within the error margin, not only the largest approximate sample.

The final legacy upper must not simply inherit the approximately `1.1513e-6 * input_peak` coarse FFT allowance and call that identical to the original `1e-11 * input_peak` ceiling accuracy. Resolve or retain that uncertainty explicitly. Local rescoring is the route to recovering the original tight result.

### 6.4 Use this block engine as the dense fallback

The later phase-search engine should reuse this implementation when a region is dense with candidates. For Legacy64, its complete block cost is predictable. Determine the sparse-to-dense crossover from measured work, including support expansion and repeated neighboring knots, not from candidate count alone.

This first milestone can satisfy the user's reference-speed request at identical reconstruction accuracy even before the HQ candidate qualifies.

## 7. Concrete higher-accuracy reconstruction: HQ1024V1

### 7.1 First half phase

The included candidate uses a 1536-tap symmetric half-sample FIR:

```text
h[k] = sinc(k - 767.5) * KaiserWindow(k; length=1536, beta=14)
h     = h / sum(h)
```

The integer phase is exact. Generate and freeze the binary64 coefficients offline; do not build this filter with platform `sin`, `cos`, or Bessel functions at runtime.

Start qualification with an 8192-point FFT and `8192 - 1536 + 1 = 6657` new original frames per block. Benchmark 4096 and 8192 before freezing the shipping geometry.

The simple `N*log2(N)/payload` proxy is approximately 13.5303 for the old 2048/1665 front end and 15.9976 for 8192/6657, an 18.2% increase in that proxy, not a fourfold increase despite four times as many taps. This is only an FFT geometry comparison. It omits backend, cache, transform-pair, and memory costs.

The planned saving comes from removing the full-rate 4x construction and pervasive cubic screen, not from claiming that a longer FIR costs nothing.

### 7.2 Later half phases

The candidate has nine further 2x stages. Their **half-phase coefficient counts**, not nominal windowed-sinc tap counts, are:

```text
24, 12, 12, 8, 8, 6, 6, 6, 6
```

The first of these is a DC-normalized, symmetric Remez half-delay design on normalized-Nyquist band `0..0.5`. The second is the same type of design on `0..0.25`. The remaining seven are centered half-sample Lagrange FIRs of the stated even lengths, generated from exact rational interpolation weights before binary64 storage.

The included script specifies all construction arguments, and the included coefficient JSON preserves the generated candidate. SciPy's Remez routine is an offline minimax FIR designer; it is not a runtime dependency. [E2]

Do not copy the legacy -0.004 dB calibration into this target. HQ1024's candidate calibration is 1.0; its input sample peak remains an independent lower bound and report floor.

### 7.3 Measured candidate properties

The probe's sampled, all-phase audit over `0..0.495*Fs` found:

```text
Minimum magnitude deviation:     about -0.000001474 dB
Maximum magnitude deviation:     about +0.000001683 dB
Maximum complex response error:  about 1.93814e-7
1024x ideal-grid component:      about 0.000010016 dB
Full finite operator L-infinity: about 4.676026435151283
```

The complex-response audit is important: magnitude alone can conceal phase error. Nevertheless, these sampled response numbers are **not** a continuous frequency enclosure, and a small frequency-response deviation is not, by itself, a worst-case peak-relative bound for arbitrary multitone cancellation.

Promote a rounded finite operator upper such as `4.68` only after independent coefficient/interval qualification. Do not make the numerical probe's last displayed digit an authority constant.

### 7.4 Geometry

The candidate's composed delay is 795354 target subframes, or 776.712890625 original frames. Derive the required input support and flush lengths from the complete nonzero kernels. A 777-original-frame mathematical support halo is the relevant starting bound; the research probe deliberately uses additional padding.

The 2x-to-1024x tail has 512 phases, but each phase uses only a short coarse neighborhood. The candidate's nonzero support lies within offsets `-16..17` of the 2x cell. That is 34 coefficient positions, not a 1536-tap dot product for every refined phase.

A complete 512-by-34 binary64 tail bank occupies 139,264 coefficient bytes. The few commonly visited dyadic rows should be laid out for locality. The table is shared immutable data, not duplicated per track. The implementation can use the same conservative 34-position window for both descriptors.

### 7.5 Why stop initially at this candidate?

1024x makes the ideal-grid contribution much smaller than the existing 64x grid contribution, while selective evaluation limits the extra search cost. Increasing the grid further only benefits Reference9 once its numerical and search uncertainty have been reduced below that grid error.

After the primary gates pass, benchmark exactly one refinement experiment: append two further short half phases for a 4096x target, and separately compare a 2048-tap/beta-16 first half phase. The 4096x ideal-grid contribution at the same band edge is about 0.000000626 dB. The 2048-tap alternative is not the included descriptor and needs its own reproducible coefficient design and qualification.

Adopt that extension only if it improves independently measured accuracy within the time budget. Give it a distinct reconstruction identity and rederive all contracts. Do not turn the first implementation into an arbitrary-factor filter designer. HQ1024 is the concrete, reproducible starting design, not a claim of a global optimum over all possible estimators.

## 8. The new certificate: curvature of the actual 2x stream

This section gives the algebra to implement, not a heuristic based on apparent audio bandwidth.

### 8.1 Tail representation

Let `y[i]` be the exact reference-quality 2x sequence, including the true reconstruction of the original-input extension. For a cell between `y[i]` and `y[i+1]`, write a target knot as:

```text
z[i,p] = sum_k h[p,k] * y[i+k]
```

There are 32 tail phases for Legacy64 and 512 for HQ1024. The cell's right endpoint is represented as `h[R,k] = 1` at `k=1` and zero elsewhere, where `R` is that tail phase count.

All these kernels are derived from the complete frozen later cascade. No band-limit assumption enters this representation.

### 8.2 Difference from a straight-line predictor

For `t = p/R`, define the residual coefficients:

```text
r[k] = h[p,k] - (1-t)*delta[k,0] - t*delta[k,1]
```

Its zeroth and first moments may be tiny rather than identically zero after coefficient rounding. Preserve them:

```text
m0 = sum_k r[k]
m1 = sum_k k*r[k]
a  = m0 - m1
b  = m1
r_tilde[k] = r[k] - a*delta[k,0] - b*delta[k,1]
```

The corrected residual has zero zeroth and first moments in exact arithmetic. For finite support `lo..hi`, define:

```text
Q[j] = sum_{k=lo..j} (j-k+1)*r_tilde[k],  j = lo..hi-2
Delta2 y[i+j] = y[i+j] - 2*y[i+j+1] + y[i+j+2]
```

Summation by parts gives the finite identity:

```text
sum_k r[k]*y[i+k]
  = a*y[i] + b*y[i+1] + sum_j Q[j]*Delta2 y[i+j].
```

This is why second differences are useful. The error is controlled by the local curvature of the actual discrete sequence, not by a guessed spectral cutoff.

### 8.3 Cell and group upper bounds

Let `D2` enclose the largest absolute second difference over the required support and `M` enclose the largest coarse magnitude. A valid cell bound is:

```text
P_cell <= U_cell
U_cell := max(abs(y[i]), abs(y[i+1])) + A*D2 + B*M
```

where `A` bounds `max_p sum_j abs(Q[p,j])` and `B` bounds `max_p (abs(a[p])+abs(b[p]))`, with all rounding and coefficient errors separately enclosed.

For a group of consecutive cells, use the maximum endpoint magnitude in the group and the maximum second difference over the union of the cells' true supports. The same constants apply. A quiet group can be rejected without visiting its individual cells.

Exploratory root constants are:

```text
Legacy64: A approximately 0.385054359363
HQ1024:   A approximately 0.329422342667
```

The candidate's denser target did not require a looser root curvature constant. For production, derive these with interval or exact-rational coefficient arithmetic and round the published constants upward. Keep the small affine-defect term; do not discard it because it appears close to machine epsilon.

### 8.4 Group sizes

Start with groups of 64 consecutive 2x cells. A surviving group is split into groups of 8; only survivors of those tests become individual cell candidates.

Within each canonical source tile, establish the coarse lower bound before screening its groups. This avoids doing early fine work against an unnecessarily low threshold. Compute second differences and block summaries once; do not rescan the entire halo separately for every small child.

A practical implementation stores summaries for fixed microblocks and combines the necessary prefix/suffix or halo contributions. A general range-query tree is unnecessary for the initial implementation.

The 64/8/1 choices are starting constants, not a public format or an auto-tuned runtime policy. Benchmark a small fixed alternative set if profiling justifies it.

### 8.5 Optional signed tightening

The initial implementation only needs `sum(abs(Q))*max(abs(Delta2 y))`. A later, inexpensive tightening may use positive and negative coefficient sums together with the minimum and maximum second differences.

That can improve monotone or concave regions, but it must not precede the simpler certificate's implementation and tests. Do not add a content classifier or sinusoid detector to the first version.

## 9. Hierarchical refinement inside a surviving cell

### 9.1 Bound a phase interval, not just an entire cell

Suppose knots at phase indices `a` and `b` have been evaluated. For each interior phase `p`, form:

```text
w = (p-a)/(b-a)
r[p] = h[p] - (1-w)*h[a] - w*h[b].
```

Apply exactly the same moment correction and second-difference factorization as above. Freeze `A[a,b]` and `B[a,b]` for each dyadic node. Then:

```text
P[a,b] <= U[a,b]
U[a,b] := max(abs(z[a]), abs(z[b])) + A[a,b]*D2 + B[a,b]*M
```

plus the endpoint, curvature, coefficient, and arithmetic enclosures.

If this upper cannot beat the current global lower, the whole phase interval is finished. Otherwise, evaluate its midpoint, update the lower, and split. A midpoint value is a short tail dot product, not another global filtering pass.

### 9.2 The bounds tighten rapidly

For Legacy64, the probe's maximum curvature constants at phase widths 32, 16, 8, 4, and 2 were approximately:

```text
0.38505436, 0.08729887, 0.02503616, 0.00654673, 0.00040681.
```

For HQ1024, the corresponding first three widths 512, 256, and 128 gave approximately:

```text
0.32942234, 0.07666879, 0.02169705.
```

At width 2, the HQ constant was approximately 0.000005503. These are measurements of finite coefficient identities; they are not frequency-response surrogates.

### 9.3 Ordering and reuse

Process the largest unresolved upper first within the tile. Break ties by source position, channel, and phase interval. Reuse midpoint values; a split's children share the evaluated parent midpoint.

A bounded array plus a small heap is sufficient. Do not allocate a heap node for every coarse cell before screening. Do not expand all 511 phases of an HQ cell merely because the cell survived its root test.

For the overall channel maximum, one channel may establish a lower that safely rejects another channel's candidates. Per-channel uppers remain valid, but do not claim that every channel has the same relative-width guarantee unless a separate per-channel search was requested.

## 10. Reference9 policy

Reference9 has no accuracy-limiting candidate cap. It uses group rejection, dyadic phase refinement, and local dense execution to establish the requested finite-target interval.

The work proceeds in this order:

1. Evaluate the 2x prefix and group certificates for the canonical tile.
2. Refine the highest remaining upper until the tile cannot change the global result beyond the selected numerical/accuracy requirement.
3. Use a dense block implementation where sparse refinement is more expensive.
4. Recompute numerically ambiguous winning neighborhoods with a tighter direct evaluator.
5. Carry only certified bounds or the small necessary unresolved neighborhoods beyond the tile.

A strict legacy run should recover the legacy evaluation accuracy, and, when explicitly required, the legacy point computation order. An HQ run should complete to its target interval; it must not silently spend an inherited legacy reserve.

### 10.1 Direct winner rescoring

The FFT front end is allowed to have a conservative error envelope much wider than the final reference result. Most losing intervals can still be dismissed with that envelope. Near the winner, use the saved raw samples to reevaluate the necessary prefix values or complete target knot with compensated or extended-precision accumulation and a separately derived bound.

Merely reevaluating the approximate winner is insufficient. Every competing node whose upper can exceed the improved lower remains live. Repeat until no unexamined competitor can invalidate the claimed interval.

The direct evaluator must respect the target coefficients and the real original-input edge extension. Repeatedly padding a local candidate block's own endpoints would change the waveform.

### 10.2 Dense regions

For Legacy64, use the optimized full block cascade from Section 6.

For HQ1024, do not jump directly to a full 1024x stream when a region is difficult. Batch an intermediate level, initially 8x or 16x, over the dense region plus its required halo. Reestablish local lower bounds and continue certified selective refinement above that level. The upper proof must correspond to the remaining HQ tail, not to a newly invented polynomial approximation.

A fully exhaustive HQ block is an oracle and last execution option, not the normal path. Benchmark the difficult upper-band families explicitly. If Reference9's accuracy-preserving run exceeds 540 seconds, it has failed the time gate; do not disguise a candidate cap as reference completion.

## 11. Fast90 policy

Fast90 uses the same high-quality target and the same safety equations. It differs by assigning a deterministic amount of discretionary work to each canonical input tile.

### 11.1 Account for actual categories of work

Charge work for phase dot products, node processing, dense-region processing, and direct rescoring. A count of "candidate cells" is inadequate because one HQ cell might need one new knot and another hundreds.

An initial commissioning setting is 32,768 tail-product-equivalent credits per 4096-original-frame tile per channel, with a bounded carry of at most eight tile allowances. This is a starting experiment, not a performance guarantee or an authority constant. Measure the node-processing charge and dense-block charge in the shipping build before freezing the policy.

The original accelerated scanner also spends approximately 98,304 unconditional second-stage pair products per tile/channel, before any candidate refinements. The new scanner removes that unconditional stage. The extra HQ FFT work, additional reductions, and all control flow still have to fit in the saved time.

Process high-upper candidates before low-upper candidates. Skip only when the certificate permits it. After reaching the preferred width, continue toward a tighter result if budget remains and the result is not already limited by its numerical enclosure. "Most accurate within the budget" should not mean stopping at an arbitrary coarse tolerance while cheap, useful refinement remains.

### 11.2 Preserve unresolved authority

At any work limit, retain every unresolved region's upper in the final maximum. Never report only the largest value that happened to be evaluated.

The result includes at least:

```text
reconstruction identity
point estimate
lower_linear
upper_linear
relative interval width, when meaningful
search-completion / budget-limited status
unresolved maximum
work and refinement diagnostics
```

A time limit is not implemented by abandoning the tail of the audio file. Every input frame participates in the mandatory scan and in a valid certificate.

### 11.3 Avoid permanently wasting the early budget

The current per-tile cap can leave an early unresolved upper that never receives more work. Add a small bounded deferred-candidate cache only after the basic search is correct.

Start with at most 64 unresolved peak neighborhoods, shared across the track's channels. Retain the coarse support and, where necessary for tight rescoring, the corresponding raw-input support. Prefer the largest uppers. When two retained neighborhoods overlap substantially, share the buffered region rather than copying it repeatedly.

Use carried credits and the explicit finalization allowance to refine those retained contenders. If a candidate must be evicted while still unresolved, its upper remains in a separate discarded-upper maximum. Eviction is not resolution. New larger lower bounds may later make that discarded upper irrelevant; until then it still limits the certificate.

This cache has bounded memory and does not require a second file read. Its final work is part of the 90-second budget. It is an accuracy improvement, not a prerequisite for the first correct implementation.

### 11.4 Timing is still a release measurement

A deterministic work cap bounds discretionary work, not I/O stalls or all possible host scheduling delays. It also does not prove that the mandatory front end fits 90 seconds.

Measure these components separately:

```text
input reading/decoding
FFT prefix and alignment
coarse reductions and mandatory screening
candidate processing
deferred work / rescoring / finalization
```

If the mandatory scan consumes the budget, increasing or rearranging candidate credits cannot solve the problem. Optimize that measured bottleneck or choose the best already-qualified descriptor that actually passes. A Legacy64 Fast90 fallback remains useful, but report it as that reconstruction, not as a successful HQ1024 release.

## 12. Floating-point obligations

### 12.1 The current qualification does not settle the new FFT proof

The supplied accelerated-reference qualification report explicitly separates coefficient factorization from `runtime_fft_rounding_proof`, which is false. This review reran that script successfully. It verifies the residual and factorization construction; it does not independently prove the complete FFT arithmetic backend.

Do not copy an old dB-derived allowance into a new FFT size and call the numerical work finished. A formal finite-target certificate requires a justified execution enclosure. Where qualification remains empirical, describe it as empirical rather than promoting it to a theorem.

### 12.2 Bound the actual transform computation

The offline numeric qualification must include input packing, forward transform, filter-spectrum construction, pointwise multiplication, inverse transform, normalization, and coefficient rounding. Derive a conservative bound for the fixed transform plan and supported backend, including twiddle error, rather than relying solely on random roundtrip tests.

Packed stereo deserves an explicit test. The current implementation packs two real channels into one complex transform while tracking running input peaks per channel. For the new proof, use a bound based on the maximum magnitude across the packed pair and the full contributing history/block unless a backend-specific argument establishes independent channel bounds. Do not assume a quiet channel's numerical error is zero because its own input peak is zero.

Test a hot channel beside silence and beside a very quiet channel. This is a proof obligation for the new backend, not a claim here that the current test suite has demonstrated a particular failing input.

### 12.3 Propagate error into curvature and endpoints

For coarse samples with a common absolute enclosure `epsilon`:

```text
abs(Delta2 y - Delta2 y_hat) <= 4*epsilon.
```

With individual errors, use `epsilon[j] + 2*epsilon[j+1] + epsilon[j+2]`. For a computed target knot:

```text
knot_error <= sum_k abs(h[p,k])*epsilon[i+k]
              + coefficient-composition error
              + dot-product rounding error.
```

A group upper therefore uses enclosed endpoint magnitudes, enclosed curvature, affine-defect allowances, and directed arithmetic. A refined node uses its endpoints' own enclosures, not a guessed fraction of the root error.

Store error metadata per FFT block or coarse microblock, not necessarily per sample. A support spanning multiple blocks must combine their correct bounds. Replacing the current per-knot error array by metadata is an optimization only if this dependency is retained.

### 12.4 Lower bounds round downward

For an evaluated value `v` with error `e`, use a downward-enclosed `max(0, abs(v)-e)` for its lower and an upward-enclosed `abs(v)+e` for its upper. Preserve the exact input sample maximum as a valid lower for these identity-preserving reconstructions.

Coarse approximations are never used as certified lower bounds without their error. Comparator tolerances are not proof margins. Every non-finite intermediate either produces a safe unresolved infinity that is subsequently handled or fails the scan before a gain is authorized; it must not disappear through a max reduction.

### 12.5 Numeric floors and rescaling

Fast90's preferred 0.0001 dB interval leaves room for a conservative FFT enclosure, but that must be demonstrated at the actual amplitude scale. Reference9's much narrower result generally needs tighter evaluation or local direct rescoring.

Include underflow terms or an explicitly justified input range in the arithmetic analysis. Preserve the existing treatment of above-full-scale finite samples and numerical overflow; do not clamp inputs or introduce a music-only amplitude assumption to make the proof easier.

## 13. Streaming, edges, and memory

Keep decoder chunking separate from algorithmic geometry. FFT blocks, source tiles, group boundaries, tie-breaking, and credit grants derive from absolute frame positions. The same frames in different whole-frame push sizes must produce identical results for a fixed backend and policy.

Extend the **original input** using the selected edge policy and then reconstruct the required coarse halo. Repeating the first or last computed 2x value is generally not the same operation. Internal tile and candidate boundaries never become synthetic file boundaries.

The nominal target interval remains:

```text
0 .. (input_frames-1)*target_factor, inclusive.
```

Handle a single frame without creating an inter-frame search cell. Account for the final original sample exactly once in the peak semantics, even if overlapping work windows visit it more than once internally.

The state consists of fixed transform buffers, a raw history/future window, a bounded 2x tile with halo, reusable summaries and candidate arrays, and the optional bounded deferred cache. Do not retain the album's entire 2x signal.

For the initial implementation, a few megabytes per active scan is a reasonable budget to verify, not an excuse for an album-sized allocation. Large immutable kernel banks and FFT plans can be shared. Caller concurrency remains under the existing pipeline's controls; do not add a nested unbounded worker pool.

## 14. API and gain integration

### 14.1 Separate reconstruction from search policy

Use a small explicit internal representation such as:

```text
ReconstructionId:
  LegacyHeadroom64
  Hq1024V1

SearchPolicy:
  ReferenceComplete
  Fast90
```

These names are illustrative API design, not supplied compiling Rust. Existing public entry points remain available. Avoid an open-ended public filter-description API for this change.

A returned certificate binds the selected reconstruction, its operator-norm upper, its numerical envelope, and the achieved interval. It must not allow a caller to pair an HQ upper with a legacy terminal-error norm by forgetting which engine produced it.

### 14.2 Legacy compatibility

Retain `Reporting4x`, the calibrated legacy `Headroom64x` reporting result, the old authority helpers, and the DSD 16x/8x policy mappings. The new optimized legacy evaluator may replace execution only after the compatibility gates pass.

Keep the original direct implementation available to tests and the comparison benchmark. It need not become another permanent UI choice.

### 14.3 HQ ceiling migration

A new HQ hard-ceiling option requires an explicit reconstruction contract. Its continuous finite waveform can remain straight-line interpolation between adjacent HQ target knots, so its peak is again the maximum knot magnitude plus the arithmetic enclosure.

For HQ1024, independently qualify its full operator norm; `4.68` is the proposed widened candidate, replacing `4.09` **only for outputs governed by HQ1024**. Recompute every affected terminal-error test. Do not update the global legacy constant in place.

The existing album solver remains conceptually unchanged:

```text
G * (P_signal + E_pre) + E_post <= C.
```

`P_signal` receives the certificate's `upper_linear`. `E_pre` and `E_post` use the corresponding reconstruction norm. Preserve directional gain conversion, output-rate identity, terminal dither/shaper policy, cancellation, and resolved-gain retry semantics.

The current PCM Standard/Reference integration takes the maximum of a reserved point and a finite upper. Do not send the new HQ point through the old 0.030 dB reserve by reusing an enum token: that would conceal much of the accuracy improvement in the gain result. Give the HQ integration an explicit authority policy. No new small ideal-sinc reserve is published until independently justified.

### 14.4 DSD and lossy boundaries

Do not silently migrate the separately certified DSD Reference contract or existing album DSD ceiling. They continue to use their declared reconstruction until a separate migration is authorized and qualified.

The new PCM-input ceiling still does not promise the same peak in decoded AAC, MP3, Opus, or another lossy output. Keep that distinction in the user-facing description.

### 14.5 Album semantics

A collection of tracks is not automatically one concatenated finite stream. Preserve the current per-track endpoint treatment and album-level gain aggregation. A benchmark manifest should scan the actual tracks rather than changing their peaks by concatenating them to simplify timing.

## 15. Implementation sequence

### Change 1: capture the baseline and protect the oracle

Run the current shipping build on the actual commissioning carrier and album track set. Record all existing modes, actual elapsed times, point values, finite uppers, refinement statistics, compiler flags, and host conditions. Preserve the original direct reference behind the test/benchmark surface.

Acceptance: the archive's current tests pass and the user's reported result is reproduced. A filename, arithmetic-count model, or 120-second extrapolation is not the baseline measurement.

### Change 2: descriptor and offline coefficient contracts

Add the two fixed internal reconstruction descriptors, but keep HQ experimental. Port the probe's tail composition, moment correction, and curvature constants into independent qualification tooling. Freeze coefficients and node metadata in generated source files checked into the repository.

Tests recompute small identities independently rather than merely hashing a generated table. Include total support, timing delay, identity phases, DC/moment defects, full norm, and conservative coefficient-composition error.

Acceptance: impulse and random-window tests agree with the direct cascade; intentionally changing a tail coefficient or a support endpoint fails ordinary Rust tests.

### Change 3: shared 2x FFT block executor and numeric enclosure

Extract the existing first-stage FFT operation into private reusable code. Preserve its whole-frame input contract and paired-channel optimization. Produce planar coarse blocks with explicit absolute positions and error metadata.

Qualify the fixed sizes and arithmetic backend. Exercise final partial blocks, history, channel pairing, odd channel counts, silence, huge channel imbalance, and chunk invariance.

Acceptance: direct-prefix comparisons and the numerical proof obligations pass. Do not proceed by assuming the old scalar allowance covers a different transform.

### Change 4: optimized complete Legacy64 path

Add blockwise later-stage execution and local strict rescoring. Reuse it as the dense fallback. Extend `bench_ceiling_f64le` to distinguish the original direct reference from the optimized legacy reference without changing the meaning of existing benchmark records.

Acceptance: <=540 seconds on the commissioning workload, with legacy reconstruction and numerical compatibility. This is the first independently shippable result.

### Change 5: 2x group and phase-tree search

Implement 64/8/1 grouping, moment-corrected curvature bounds, dyadic phase refinement, deterministic ordering, and full certificate reduction. Begin without a work cap so the math and coverage can be checked against exhaustive Legacy64.

Acceptance: every directed and randomized finite input is contained; different chunk sizes are identical; deliberately omitted final cells, phase endpoints, and halo values are caught by mutations.

### Change 6: Reference9 HQ integration

Add HQ1024 to the same engine, use strict completion and tighter rescoring, and qualify analytical accuracy independently. Run the dense-region tests as well as real programme material.

Acceptance: <=540 seconds, required finite interval, analytical accuracy objective, and independent-reference sanity checks. The optimized legacy result remains available even if this new reconstruction has not yet qualified.

### Change 7: Fast90 budget and useful deferred work

Add deterministic work charging, bounded carry, truthful budget-limited results, and then the small deferred-neighborhood cache if measurements show it improves the achieved interval. Include all finalization work in timing.

Acceptance: <=90 seconds and <=0.0001 dB finite-target width together on the commissioning corpus. Adversarial work-cap tests must remain safe even when they return a wider interval.

### Change 8: explicit product integration and measured tuning

Update PCM settings and persistence deliberately, including the serde/default inventory tests. Bind terminal error to reconstruction identity. Leave DSD mappings unchanged. Tune only measured bottlenecks and choose the highest-accuracy qualified descriptor that passes both target gates.

Keep generated coefficient tools outside Cargo builds and runtime, consistent with the repository's existing approach. No commissioning stamps, executable fingerprints, runtime profile gates, or mandatory external tools are introduced.

## 16. Regression and numerical qualification plan

### 16.1 Finite reconstruction containment

Test both descriptors against independent exhaustive reconstruction on short and medium signals. Cover impulses at every position around startup, FFT boundaries, tile boundaries, and finalization; steps; alternating samples; arbitrary signs; constant and silent input; near-silence; single-frame and very short streams; above-full-scale input; both edge policies; odd and even channel counts; and channel maxima occurring at different times.

Generate random short vectors, then explicitly synthesize sign vectors that maximize individual residual and curvature operators. Random programme audio alone is not adequate coverage for an induced-norm bound.

Verify node bounds for every dyadic interval, not only the root. In particular, test both the left/right endpoint identities and moment-defect corrections.

### 16.2 Search correctness

Test strict pruning, tolerance-limited diagnostics, credit exhaustion at every node depth, zero discretionary credit, dense fallback, candidate ties, retained/evicted deferred candidates, and a larger peak appearing late in the track.

A mutation that returns only the observed point must fail. A mutation that drops an evicted upper must fail. A mutation that omits the last nominal interval must fail. A mutation that repeats tile endpoints instead of original-file endpoints must fail.

A proof-complete status must mean that no unexamined region can beat the certified winner, not that a counter reached its configured budget.

### 16.3 Arithmetic

Compare scalar and shipping SIMD backends against high-precision or compensated direct calculations. Test packed channels with very different scales, cancellation-heavy inputs, final FFT padding, subnormal handling under the declared numeric contract, overflow, and coefficient perturbation.

The offline floating-point probe is not the interval qualifier. The production qualification must bound generated coefficients and node constants outward and include FFT and dot-product arithmetic.

### 16.4 Point accuracy

Reuse the existing aligned multitone family, but extend the new target's qualification substantially: more frequency/phase combinations, negative peaks, upper-band emphasis, envelopes with correctly accounted spectral support, and independent dense/continuous truth where applicable.

Separate interior analytical truth from finite endpoint behaviour. An abruptly truncated sinusoid with repeated endpoints is not globally the same waveform as the untruncated analytical sinusoid.

Report signed error, maximum absolute error, worst under-read, worst over-read, and the input that produced each. Do not compare only medians and do not infer a universal multitone bound from the minimum gain of one polyphase branch.

### 16.5 Pipeline authority

Verify that the certificate reaches the existing linear gain solver without a stale point reserve; that the terminal norm matches the reconstruction identity; that a zero requested ceiling stays valid; and that album gain remains deterministic across scratch retry/rerun.

Exercise all terminal formats already covered by the source tests. A more accurate point meter can still yield a conservative final gain because deterministic quantization or dither support dominates the remaining allowance. Report those terms separately rather than blaming the scanner's interval.

## 17. Benchmark design and pass/fail rules

### 17.1 Correct the timing boundary

The existing benchmark starts its timer after meter construction, file open, and buffer setup. Preserve that historical metric for old comparisons, but add a total-scan metric starting before construction/open and ending after finalization. The new 90/540-second promises use the latter.

Collect both single-carrier and album-manifest results. Keep each track's actual edge policy. Include peak values and certificate widths in every benchmark record so a speed improvement cannot hide a quality downgrade.

### 17.2 Measurements

Record total wall time, scan-loop wall time, user/system CPU time, peak memory, bytes read, source frames and channels, FFT time, mandatory screening time, refinement/rescore time, group rejection counts, refined phase counts, dense regions, work consumed, deferred-cache use, discarded-upper maximum, final interval, and completion status.

Report the actual SIMD backend and shipping code generation. RustFFT already makes backend choices; a microbenchmark of another FFT library or Python/SciPy is not the production result. [E1]

### 17.3 Corpus

The acceptance corpus includes the actual 40-minute commissioning material, the actual album as separate tracks, and additional representative material at 44.1/48 and 176.4/192 kHz. Keep explicit stress cases for sustained upper-band energy, flat high-level signals, dense transients, noise, silence, channel imbalance, and endpoint-dominated peaks.

Do not downsample high-rate material to make the benchmark pass. Do not reduce the target factor at high sample rates. This design's finite reconstruction remains explicit at every accepted rate.

### 17.4 Release decisions

Use at least five complete measured runs for the principal warm-cache comparison and record a cold-cache run separately. A practical engineering target is a median <=80 seconds for Fast90 and <=480 seconds for Reference9, leaving room below the binding maxima of 90 and 540 seconds. These internal margins are proposed acceptance policy, not measured results.

Do not publish an unconditional storage-independent claim when only warm-cache runs pass. If cold input fails because storage cannot supply the required rate, state the benchmark conditions and the end-to-end limitation.

For each primary release profile, require the time and quality gates on the same runs. No multiplication-count extrapolation, short-excerpt projection, or average across a fast failure and a slow success substitutes for that test.

## 18. Decisions not to take

Do not merely raise the current 64-candidate cap while keeping all mandatory 4x work and call it the 90-second design.

Do not shorten the first filter until a large fixed under-read reserve consumes the accuracy gained by better search.

Do not create all 1024x samples or run a fresh long original-rate FIR for every fine phase.

Do not reuse Legacy64's calibration, 0.030 dB point reserve, `4.09` terminal norm, or 201-frame support for HQ1024.

Do not treat dense frequency sampling as continuous frequency certification, or single-tone gain error as an arbitrary-signal true-peak theorem.

Do not add a GPU dependency, new unbounded pool, second DSD reconstruction, or mandatory second carrier read to the first implementation. None is necessary to test the primary architecture.

## 19. Evidence, reproducibility, and remaining work

### Supplied-source references

[S1] `crates/tonepoet-true-peak/src/lib.rs`, especially lines 34-71 (accelerated constants), 76-99 (legacy filters/calibration), 154-175 (FFT allowance), 761-1056 (direct and paired stages), and 1057-1184 (complete reference engine).

[S2] `crates/tonepoet-true-peak/src/reference_fast.rs`, especially `FftReference4Engine`, `ReferenceFastScanner::process_tile`, finalization, and the finite-stream wrapper. This file establishes the implemented 4x prefix, tile screen, cap, and unresolved-upper behaviour.

[S3] `crates/tonepoet-true-peak/qualification/verify_reference_fast_scanner.py` and `verify_ceiling_contract.py`. Both were rerun successfully; their output is included in `results/`.

[S4] `crates/tonepoet-true-peak/tests/fixtures/README.md`. This supplies the real fixture's provenance and frozen libsoxr observation. The audio itself is not redistributed in this design bundle; the probe reads it from the user's source checkout.

[S5] `crates/tonepoet-true-peak/examples/bench_ceiling_f64le.rs`. This establishes the current benchmark's timing boundary, modes, and metrics.

[S6] `src/convert/pipeline/stages.rs`, PCM scanning/authority selection near lines 32525-32610, and `docs/dsd_album_gain_true_peak_authority.md`. These establish the gain integration and legacy ceiling boundary that must not silently change.

### External implementation references

[E1] RustFFT 6.4.1 official documentation and source README, inspected for the pinned implementation's planner and SIMD behaviour:

`https://docs.rs/crate/rustfft/6.4.1/source/README.md`

`https://docs.rs/rustfft/6.4.1/rustfft/`

[E2] SciPy official `signal.remez` documentation, inspected for the offline minimax FIR design interface:

`https://docs.scipy.org/doc/scipy/reference/generated/scipy.signal.remez.html`

These references support implementation details. The new curvature certificate is derived in this document from finite coefficient identities; the external references are not presented as a preexisting proof of this particular scanner.

### Reproduce the included calculations

From a Python environment with the supplied research requirements installed:

```sh
python research/tp_design_probe.py \
  --crate /path/to/tonepoet/crates/tonepoet-true-peak \
  --output results/reproduced_design_probe.json \
  --coefficients-output results/reproduced_hq1024_coefficients.json
```

The script does not mutate the source checkout. It performs sampled response audits, coefficient-identity probes, unbudgeted mono hierarchical scans, exhaustive finite reconstruction comparisons, and three analytical peak checks. It contains no Rust benchmark, no production FFT backend, no outward interval arithmetic, and no production deadline mechanism. Those remain implementation and qualification work.

The archive SHA-256 inspected for this design was:

```text
0a5c49c993eb566863fb77d1aa92747d42046ded24e9cd708bbe7811bec5b3ce
```

That hash is provenance for this review, not a proposed runtime fingerprint or commissioning gate.

## Final recommendation

Deliver the optimized, contract-preserving Legacy64 evaluator first. It is the direct answer to obtaining the current reference result more cheaply and provides the oracle/fallback needed by the next changes.

Then implement the 2x curvature/group search and dyadic phase refinement. Run the same engine against the included HQ1024 candidate. Use strict completion and local high-accuracy rescoring for Reference9, and deterministic work allocation with truthful remaining uppers for Fast90.

The numerical evidence supports both parts of the proposal: the new search can discard nearly all fine evaluation on the bundled real fixture, and the new filters materially improve the analytical point estimates. The implementation must now establish the two things this environment could not: the complete production arithmetic enclosure and the full-length shipping-build performance gates.
