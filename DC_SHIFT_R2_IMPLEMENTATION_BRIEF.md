# Implementation Request — TonePoet: Pipeline-Native DC Measurement and High-Fidelity DC Shift (R2)

## Assignment and authority

Implement **measured (automatic) DC-offset subtraction** and **manual DC-offset subtraction** in TonePoet, alongside the existing **2 Hz high-pass DC-removal mode**. Include accessible, nontechnical help through an **ⓘ on the DC correction row**.

Work against the **fresh, complete TonePoet repository snapshot supplied with this request**. Prior work on channel layouts, DVD-Audio, stereo folding, DC high-pass filtering, HDCD, de-emphasis, true-peak processing, and logging has already been integrated. **Do not apply older patches or assume paths and types are unchanged.** Inspect the fresh tree and its project instructions before designing the change.

Build production-quality code and tests, then deliver a complete source archive. The standard is correct audio, fidelity, predictable performance, truthful evidence, and low integration risk—not architectural novelty.

**Two nonnegotiable behaviors:**

1. In measured mode, **DC analysis is a typed, scheduled part of conversion**. Its result resolves a downstream correction decision and, where appropriate, a physical correction operation. The user must not have to analyze or prepare the audio separately.
2. **Measurement is not proof of an audio defect.** A finite recording can have a nonzero average because of genuine musical content. Automatic correction must be appropriately conservative, and both the UI and logs must accurately distinguish an observed mean from a confidently identified recording bias.

## 1. Scope and constraints

Extend existing mechanisms rather than replace them:

- The Format pane's advanced DC-removal row and clickable-pill conventions.
- The existing typed conversion plan, registered effects, dependency/order rules, and execution realization.
- Existing decoded-PCM carriers, analysis, caching/fingerprints, staging, cancellation, and logging.
- The existing 2 Hz high-pass implementation and its established SoX-NG/FFmpeg behavior.
- Existing true-peak, terminal precision, dither, and output-publication contracts.

Do **not** introduce a second scheduler, generalized DSP framework, new cache database, ownership/journal system, or broad pipeline rewrite. Avoid modifying unrelated codecs and established fast paths unless the requested effect would otherwise be omitted.

Maintain the current performance of conversions with DC correction **Off**. Avoid extra analysis or decode passes in **High-pass** and **Manual** modes unless an independently selected effect already requires them.

## 2. Convert UI: clickable pills and a useful ⓘ

The existing DC row lives **below the fold** in the maximized Format pane. Keep it there. It must remain clearly separate from the **CD de-emphasis** row: *DC offset has nothing to do with CD pre-emphasis*.

Replace the existing On/Off DC-removal choice with four mutually exclusive **clickable pills**, respecting actual UI conventions:

| Pill label | Meaning |
|---|---|
| **Off** | Do not measure or correct DC for this setting. **Default.** |
| **Auto shift** | Analyze the conversion signal, then subtract a sufficiently well-supported steady offset if warranted. |
| **High-pass** | Retain the existing 2 Hz high-pass DC-removal operation. |
| **Manual** | Subtract a supplied fixed offset, without automatic measurement. |

Inspect pill drawing, focus/navigation, hitboxes, editing, and preset patterns in the **fresh** source. Keep labels compact enough for the current pane. Do not introduce radio buttons, a new modal configuration panel, or new precision parameters merely because DSP tools expose them.

### Required information glyph

Add the **literal Unicode ⓘ (U+24D8)** on the **DC correction row**, alongside the pills, following the established de-emphasis/HDCD/true-peak info controls:

- Use the existing **foreground-only contrasting glyph** styling, **not** a filled background/circle or warning badge. Reuse the established treatment of the ambiguous-width Unicode glyph and its same-style guard cell.
- Register the glyph's actual rendered location as a **clickable two-cell information hitbox**, separate from the pills, and open the existing **scrollable Notice** overlay. Reuse its close, scrolling, focus, and input patterns. Respect existing keyboard behavior; do not introduce an inaccessible mouse-only control if the current conventions provide a keyboard route.
- The ⓘ should be present for all four modes, not only when an offset is detected. It is explanatory, not an alarm.
- Keep it below the fold with the row. Preserve correct pill/glyph hitboxes and text bounds at representative narrow and wide terminal sizes; account for the four-pill layout when computing positions.
- Implement this using the current equivalent of the existing `DeemphasisInfo`/`HdcdInfo` button-to-Notice path, not a separate tooltip framework.

**Notice title:** `DC offset correction`

**Required help message** (this is literal plain-text Notice content; allow the existing overlay to wrap and scroll it):

```text
DC offset means a recording's audio waveform sits slightly above or below its normal centre line. It can occur when audio is recorded or digitized. A tiny offset is usually harmless, and removing it does not automatically improve the sound.

Off — Recommended unless you have a reason to correct an offset. Leaves the audio unchanged.

Auto shift — TonePoet measures the audio during conversion. When the result supports correcting a reasonably steady offset, it subtracts that amount without filtering the deep bass. If the offset is negligible or the measurement is uncertain, it leaves the audio unchanged and explains why. Measurements cannot always distinguish an unwanted offset from real low-frequency sound.

High-pass (2 Hz) — Uses a very-low-frequency filter to remove DC and reduce slow changes in the baseline. Consider this if the offset drifts instead of staying steady. Unlike a fixed subtraction, a filter also changes some extremely low-frequency sound and can produce a short settling transient.

Manual — Subtracts the value you enter. Choose this if you have a trustworthy measurement and know what you want removed. For example, entering +0.003 subtracts 0.003 from each sample in the affected channel(s).

This setting does not fix hum, clipping, or CD pre-emphasis.
```

The Notice UI is currently plain text. Preserve clear paragraphs and mode-name prefixes rather than relying on Markdown rendering or rich styling. Test the **actual displayed notice**, not just the string constant.

### Manual editor and state

- Reuse the project's normal focused inline/value editor rather than inventing new widgets. State and help must explain the sign convention: **a user-entered offset of +0.003 means subtract 0.003**.
- Use normalized PCM amplitude units and make that meaning discoverable. Do not expose the opposite sign of a backend's `dcshift` command to users.
- A **single value applied to each channel** is a valid initial Manual UI; separate per-channel editing is only warranted if the fresh TUI already provides a coherent, low-cost pattern. Measured mode, however, must measure and correct **each channel independently**.
- Reject malformed, nonfinite, or unrepresentable manual values through existing validation. Preserve the last valid manual entry across pill switching without silently applying it in another mode.
- Preserve existing preset and configuration semantics: a legacy **DC removal = On** must continue to mean **2 Hz High-pass**, not Auto shift or Manual. Legacy Off maps to Off. New presets should round-trip without unexpected mode changes.
- Never turn on DC processing automatically because Browse displayed a DC-bias warning.

## 3. Audio semantics: distinguish a sample mean from an unwanted offset

For channel `c`, the arithmetic sample mean of an observed PCM interval is

\[
\mu_c = \frac{1}{N}\sum_{n=0}^{N-1} x_c[n].
\]

For constant subtraction, when justified,

\[
y_c[n] = x_c[n] - \widehat{b}_c,
\]

where `b̂_c` is the selected offset estimate; it need **not** automatically equal a naive whole-track mean in every signal condition.

**Critical epistemic limit:** a finite recording's mean can reflect genuine programme asymmetry, an incomplete cycle of sub-bass, fades, silence distribution, or other musical content. No inexpensive general-purpose detector can prove that the mean is a hardware/recording defect. Do not call a nonzero mean a discovered defect or automatically promise an inaudible, reversible change.

Measured mode represents **user-authorized, conservative automatic processing**, not a blanket instruction to zero the arithmetic mean of every recording.

### Minimum measurement and assessment

- Stream every relevant channel of the **logical PCM input to the eventual correction**, or use a mathematically equivalent, explicitly justified observation from an existing earlier processing stage.
- Collect sample count, numerically stable mean, relevant RMS/AC-RMS and peak information, and a **lightweight bounded-memory** indication of temporal stability; do not require retaining an entire file in RAM.
- Use accurate accumulation (e.g. compensated or comparably sound methods) appropriate for long Float64 inputs. Chunking should not materially change decisions or corrected audio; do not demand unnecessary bitwise identity across differing chunk sizes.
- Account for silence/near-silence, very short clips, sample-rate differences, quiet versus boosted copies of the same content, intentionally asymmetric or very-low-frequency signals, and known baseline drift.
- Avoid a **fixed absolute-only significance threshold** that arbitrarily changes classification when a recording is uniformly gain-scaled. Handle near-zero AC-RMS safely; a ratio alone is not meaningful for silence.
- Provide a small, deterministic, documented decision policy with meaningful outcomes such as **apply**, **skip as negligible**, **skip because unstable**, and **indeterminate**. Thresholds should serve a demonstrated fidelity purpose, not pretend to diagnose all recordings.
- Where the evidence does not support subtracting a constant, **do not apply one** and do not silently fall back to High-pass. A skipped or indeterminate Auto shift can complete the rest of the conversion, but must be clearly reported as *not corrected*, with its reason. Fail the job rather than report a successful correction if a required measurement or execution step actually errors.

Do not invent complex frequency classifiers, machine learning, large window histories, configurable detection sensitivities, or multiple competing estimates unless a specific realistic case proves a simple scheme inadequate. Tests must challenge false positives, not merely prove removal of synthetic constant bias.

## 4. Two-stage planning: analysis is part of conversion

The **initial typed plan** must express the unresolved dependence:

1. User intent: `Auto shift` is requested.
2. Prerequisite: an observation of the correction-input PCM is needed.
3. Observation: per-channel measured values and adequacy evidence are produced.
4. Decision: a typed application/skip/indeterminate result is resolved.
5. Realized processing: a **fixed-value subtraction** is scheduled if warranted, using exactly the values decided, otherwise no subtraction is scheduled.
6. Execution and evidence: reflect what actually ran or was skipped.

These are **semantic distinctions**, not six mandatory new type definitions, tasks, subprocesses, or files. Use the existing plan, registered-effect ordering, realization hooks, and execution-evidence contracts. **Do not create a placeholder zero shift and mutate it invisibly mid-execution.** Resolve the parameter dependency explicitly before correction, with stable identities for the realized step.

Implement Auto mode **inside the conversion pipeline**. Browse analysis, an external preflight command, or a user-performed preparatory scan must never be required. A previous Browse result may be displayed as advice, but it is not automatically authoritative for downstream modified audio.

### Fast paths and effect composition

Trace every supported optimization that could bypass registered effects (including direct-stream, single-command, CUE-derived, source-specific, and album routes). An active Auto or Manual shift must either be correctly realized on that route or cause it to select the existing effect-aware route; it must never disappear from execution while remaining in the approved plan or log.

Preserve the existing high-pass effect and the correctly mapped stereo-fold implementation without rewrites.

## 5. Ordering and signal identity

Derive actual dependencies from the fresh repository. The correction and measurement must agree on **which signal** they concern.

Relevant interactions include decoding and DSD-to-PCM, HDCD decoding, CD de-emphasis, resampling, stereo folding/channel reordering, fixed gain, ReplayGain, current registered effects, certified true-peak, and terminal encoding/dither.

- Operations upstream of measurement that change gain, offsets, or channel mixtures can change the required subtraction. Measure the resulting signal, or propagate an earlier observation only if the transformation is provably equivalent and its identity is bound to the decision.
- Do not confuse channel **count** with channel **speaker roles**. Verify channel order at the actual correction carrier; measured offsets belong to those channels, even after folding or source-specific reordering.
- Do not allow post-correction gain or peak decisions to reuse **stale pre-correction** observations. When true-peak is selected, its approved input and ceiling logic must account for DC correction in the appropriate order.
- Preserve truthful behavior for workflows **without** a user-selected true-peak operation: do not silently enable a limiter, change gain, clip or normalize to accommodate a shift. If the final output cannot be represented under the user's choices, use existing explicit refusal or correction/ceiling policy rather than corrupting samples.
- Intermediate floating-point PCM may legitimately exceed ±1 before later planned gain; do not clamp it prematurely merely because a named external `dcshift` effect does.
- DC correction happens before the final precision-reducing quantization boundary. Maintain terminal dither exactly once, with its current ownership and format-dependent policy.

Do not impose one hardcoded global effect order. Resolve dependencies using existing typed planner and execution rules, and test the actual physical command/operation path.

## 6. Efficient measurement and carrier reuse

The full observation interval's result ordinarily becomes known only after observing that interval; therefore Auto shift requires a **dependency boundary**, but not necessarily another expensive decode.

Prefer to accumulate DC statistics while the pipeline **already materializes a high-precision carrier**, then resolve the decision and correct the carrier in a bounded-memory traversal. Reuse existing PCM transport and analysis work when doing so preserves the exact logical input semantics. Avoid independently decoding DSD, DVD-Audio, Blu-ray, or compressed sources twice just to calculate the mean.

Off, High-pass, and Manual modes must not be forced through an analysis pass for this feature. The extra work in Auto must be confined to that mode. Fuse measurement with an eligible existing traversal where integration is simple and safe; avoid introducing complicated fusion architecture for marginal gains. Explain unavoidable extra I/O or subprocesses and benchmark their cost.

Use existing carrier readers/writers and mature upstream I/O facilities; do not write a new general WAV/RF64 parser. Respect large recordings, cancellation points, and current publication/staging mechanisms.

## 7. Backends, precision, and sign correctness

Inspect **the pinned versions and their source/codecs** in the fresh snapshot before selecting a physical subtraction implementation.

Do not assume a tool preserves floating-point precision because it accepts a Float64 WAV file:

- Standard **FFmpeg 8.0 `dcshift`** is implemented for planar **signed 32-bit integer** samples (`S32P`). That can introduce unwanted sample-format conversion or clamping in a high-precision carrier path. Confirm the behavior of the actual pinned build.
- Traditional **SoX/SoX-NG** effects may use a signed 32-bit internal sample representation even when file I/O supports floating-point WAV. Verify the *pinned* SoX-NG processing core and `dcshift` path rather than assuming Float64 preservation.
- A small **native Float64 per-channel subtraction** integrated with TonePoet's existing carrier handling is justified if it is the lowest-risk way to preserve precision and eliminate unintended limiting or sample-format narrowing. This is a bounded operation, **not** permission to create a new DSP engine.

Choose the simplest realization that is demonstrably sound for TonePoet's supported audio. Preserve normal clipping, nonfinite-value, format, and cancellation contracts. No automatic limiter, dither, or integer narrowing may be smuggled into the subtraction stage.

**Backend sign trap:** external `dcshift` commands conventionally take an *amount to add*. TonePoet's Manual UI instead accepts the *offset to remove*. A displayed **+0.003 must produce `sample − 0.003`**, not `sample + 0.003`, regardless of backend. Test the physical output.

Manual common-channel subtraction is acceptable initially. Auto must calculate distinct values per channel and apply them to the correct carrier channel positions.

## 8. Track, album, and gapless semantics

Inspect TonePoet's actual track/album observation and gain grouping before selecting the measurement interval.

If a job treats adjoining tracks as a **continuous programme**, do not create discontinuities by applying unrelated per-track DC shifts at artificial boundaries. Reuse the existing continuous/albumscope machinery to obtain a common, properly defined correction scope when available and appropriate. Where processing is genuinely per-track, document and report that scope and do not claim album-wide continuity or measurement.

Do not build a separate album coordination service to implement this. Include one realistic contiguous-track test and one independent-track test using the available architecture.

## 9. Presets, identities, caching, and interrupted runs

Bind the observation to the **source and effective upstream-processed PCM identity**, channel layout/order, rate, selected scope, and measurement policy. Distinguish that from the user intent and from the realized subtraction parameters. Relevant changes must invalidate a stale measurement or correction carrier.

Reuse existing fingerprint, cache, manifest, execution record, and publication contracts. Reuse a prior valid observation only when existing mechanisms can prove it describes the same logical PCM. A filename or channel count alone is insufficient evidence.

A canceled or failed measurement must not publish an output claiming DC correction. A successful measurement followed by failed correction must not be logged as successful conversion. Use existing cancellation and cleanup paths, not a new transaction/journal system.

## 10. Truthful conversion evidence and user feedback

Reuse the present **concise conversion log plus structured execution record** architecture. Maintain the distinctions among:

- **Intent:** mode, manual input or automatic policy, and scope.
- **Observation:** actual measured channel offsets, statistical context, input-carrier identity, measurement backend/invocation, and success/failure.
- **Decision:** apply/skip/indeterminate and plain-language reason; never equate a nonzero mean with proven recording error.
- **Execution:** exact per-channel subtraction values and sign, backend and processing format, carrier/command or native invocation identity, and result.
- **Verification:** the evidence genuinely available (including cheaply accumulated output statistics, if suitable), without claiming unperformed verification or imposing a gratuitous third scan.

The user-facing result must distinguish **"analyzed, no change"**, **"offset subtracted"**, **"high-pass applied"**, **"manual value applied"**, and a real execution error. It must never say a correction was applied merely because it was requested or measured.

Preserve current recipe-reconstruction and terminal ownership semantics. Do not add a third log or a duplicate provenance system.

## 11. Focused acceptance tests

Test *decoded sample behavior* and actual route execution, not just strings, enums, and command arguments.

**Measurement and decisions**

- Positive/negative known biases; different biases per stereo channel; a representative multichannel carrier.
- Level-scaled copies (consistent classification), near-silence, short clips, long streams, zero AC-RMS, and extremely low-frequency or deliberately asymmetric programme content.
- Stable bias versus obvious drift; indeterminate/no-op behavior with honest reporting.
- Bounded memory and sufficiently stable numeric results across chunk sizes.

**Physical subtraction**

- Verify input/output samples and the UI-to-backend sign convention (user **+0.003** means subtract **0.003**).
- Per-channel corrections do not swap speaker roles after an intermediate or stereo fold.
- No surprise integer narrowing, limiting, clipping, peak normalization, or dither; correct handling of terminal limits and peak protection when expressly selected.
- Manual executes **without** measurement; Off does no new work; legacy High-pass retains its existing acoustic effect and backend policy.

**Planner integration**

- Auto observations correctly bind a later typed decision and realized effect. No false no-op placeholder or unapplied registered-effect receipt.
- Representative combinations with stereo folding, DSD-to-PCM, resampling, HDCD, de-emphasis, true peak, terminal dither, and lossy/lossless outputs (select distinct execution paths, not a combinatorial codec matrix).
- A previously optimized/direct/CUE path cannot bypass the requested correction.
- Album continuity and independent-track scope behave as claimed.

**TUI and lifecycle**

- Four real clickable pills; keyboard navigation; Manual editor; Off default; legacy On → High-pass preset migration; new preset round-trip.
- **Literal ⓘ rendered with the existing contrasting foreground and two-cell guard/hitbox**; clicking it opens the **DC offset correction** Notice. The popup text is readable and scrollable, closes normally, and remains accessible with different pill selections and terminal widths.
- Browse does not need to be visited; cancellation during analysis; failed measurement; failed subtraction; reused valid observations; changed upstream audio invalidating old results.
- Conversion log and structured evidence accurately distinguish measure/decision/execution/verification.

Preserve all existing source-specific channel-layout and DC high-pass regressions, particularly the recent DVD-Audio mapping fixes. Do not change unrelated tests merely to satisfy a new design.

## 12. Performance and proportionality

Measure the four modes (**Off, High-pass, Auto shift, Manual**) on representative short and long conversions. Report number of expensive source decodes, carrier passes, external processes, intermediate bytes written, elapsed time, and memory where measurable. Use existing benchmark harnesses or focused reproducible fixtures.

Normal **Off** must retain its prior fast path and memory profile. Auto may need one additional carrier traversal for a valid dependency; if so, confine that expense to Auto and explain the necessity. Do not add speculative parallel reduction, SIMD, cross-file orchestration, file-ownership machinery, or redundant safety checks without a demonstrated payoff.

Prefer a coherent correction over generalized abstractions. A design can be somewhat less elegant and still be the **right** design if it preserves correctness and repository stability.

## 13. Working procedure and deliverables

1. Read the fresh source tree's README, project instructions, implementation status, and relevant conversion and UI code.
2. Establish baseline test results and locate the existing high-pass DC feature end to end.
3. Locate existing typed dependency/observation machinery and reusable carriers; write a concise implementation plan that **identifies exact integration points**.
4. Implement the UI/preset migration and the two-stage measured-mode dependency using current patterns.
5. Choose and implement the physically qualified correction operation; validate samples and sign.
6. Run focused tests as you proceed, then the normal formatting, lint, Rust compile, and workspace regression gates.
7. Review the final diff for unrequested changes, hidden precision conversion, direct-route bypasses, unused abstractions, and evidence gaps. Remove unnecessary complexity.
8. Deliver a **complete downloadable full-source archive** with focused tests, SHA-256 manifest, exact verification commands/results, benchmark findings or explicit limitations, and a concise engineering handoff.

**Interruption policy:** Save a current workspace checkpoint with implementation decisions, completed changes, command outcomes, remaining tests, and hashes. Maintain it throughout substantial work so another session can resume without relying on this conversation. Do not claim an unexecuted test passed or a bundle is qualified without its gate evidence.

If something in this prompt conflicts with a demonstrated constraint of the fresh repository, make the smallest correct adjustment and document it. Do not ask for or implement a broad redesign merely to satisfy an aesthetic preference.

## 14. Acceptance conditions

The feature is done when all of these are true:

1. Off remains the default and preserves current performance/behavior; legacy On continues to mean High-pass.
2. The **DC correction ⓘ** row uses real clickable pills and established info/Notice conventions, with clear layperson copy that explains when to use each mode.
3. Manual subtracts the user-entered amount in the promised sign, without requiring analysis.
4. Auto's measurement is **part of conversion's typed dependency plan** and resolves a truthful apply/skip decision and subsequent physical correction when warranted.
5. Observed and corrected PCM refer to the correct logical signal and channel order.
6. Precision, true-peak semantics, terminal encoding/dither, and album continuity are preserved; no silent clipping, limiting, or filter substitution occurs.
7. Fingerprints, caching, presets, cancellation, and two-file logging truthfully represent intent, observation, decision, execution, and evidence.
8. Fast paths cannot omit an activated correction; unrelated conversion paths and tests remain stable.
9. Focused and applicable repository gates pass, or unexecuted gates are explicitly identified as qualifications still required on the build host.
10. There is no unnecessary scheduling, DSP, or resilience framework introduced solely for this feature.

**Deliver the code and verified full-source bundle.** Build the smallest complete implementation that meets this contract, not a proposal for another implementation session.
