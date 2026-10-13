# Brief R19 — Convert-screen HDCD detection and planner-correct decoding

Revision: FINAL R2 — final source-level audit correction.

Date: 2026-10-03

Base: **the fresh TonePoet repository snapshot supplied with this implementation task.** Treat that snapshot as the sole code authority. Do not apply, reconstruct, or assume any earlier corrective bundle by name. The current repository is expected to include the already-integrated Convert de-emphasis work; inspect what is actually present and reuse its generalized UI/provenance/planner/effect machinery rather than recreating it.

## Read this first

Implement the HDCD equivalent of TonePoet's current Convert de-emphasis feature using the capabilities that exist **today**.

This is not a transient-filter reverse-engineering project. Do not modify FFmpeg's HDCD algorithm, recover Pacific Microsonics filter coefficients, emulate unsupported hardware, or expand scope into future HDCD research.

Current capability boundary to verify against the FFmpeg actually pinned by the fresh repository:

- FFmpeg's `hdcd` filter decodes **Peak Extend** and **Low-Level Gain Adjustment**;
- it detects the **Transient Filter** flag but does not reconstruct the transient-filter path;
- its decoded signal is logically 20-bit and must not be collapsed back to 16-bit if TonePoet claims to have decoded HDCD;
- SoX/SoX-ng does **not** currently provide an HDCD decoder. SoX-ng remains available for resampling, dither/quantization and terminal/container work where the existing planner selects it.

The feature therefore has two distinct responsibilities:

1. **Detection/UI policy** — automatically determine whether eligible 16/44.1 material contains HDCD and surface the correct user choice without touching ineligible content.
2. **Planner/execution policy** — when HDCD Decode is On, make FFmpeg HDCD decoding the first sample-domain operation, then let TonePoet's planner compose whatever valid FFmpeg / SoX-ng / SSRC route satisfies the user's remaining rate, depth, dither, codec, true-peak and container choices.

The planner must remain a planner. Do not implement fixed recipes such as "HDCD always means FFmpeg does the whole conversion" or "HDCD + SSRC is a special pipeline." SSRC is an important regression case, not the governing design.

World-class correctness is required, but keep the implementation focused. Prefer existing typed planner contracts, effect carriers, dynamic Format rows, async generation guards and analysis caches over parallel mechanisms.

---

# 1. User-visible outcome

Add an **HDCD Decode** control to the Convert screen's **Format** pane for eligible CD-derived PCM.

For an eligible source with no HDCD detected, the control is normally **below the fold**, visible when the Format pane is maximized, and Off.

When TonePoet detects HDCD, promote the HDCD row **above the fold** so the user cannot miss it. Use the same durable `ⓘ` information affordance established by the current de-emphasis feature.

TonePoet must distinguish:

1. **HDCD presence/capabilities** — a fact discovered by analysis of the source PCM;
2. **HDCD Decode On/Off** — the conversion policy selected automatically or manually.

A preservation conversion may correctly show:

> **HDCD detected — Peak Extend**
> **HDCD Decode: Off  ⓘ**

That is intentional: lossless 16/44.1 can preserve the original HDCD-coded PCM for later compliant playback/decoding.

---

# 2. Automatic scanning eligibility is strict

TonePoet should automatically scan for HDCD only after the source/member is positively established as:

- lossless;
- PCM;
- integer PCM, not float;
- 16-bit;
- exactly 44,100 Hz;
- mono or stereo, consistent with the supported HDCD decoder path.

Do not scan when any required fact is unknown.

Do not run automatic HDCD analysis for:

- lossy sources;
- DSD;
- floating-point PCM;
- bit depths other than 16;
- sample rates other than 44.1 kHz;
- multichannel material outside the decoder's supported mono/stereo domain;
- a source whose technical facts have not yet been established.

This Convert predicate is intentionally stricter than any historical Details-pane applicability helper. If the current Details implementation still gates HDCD only by bit depth, do not reuse that weaker predicate blindly. Prefer one shared, exact predicate if a small refactor can make Details and Convert agree without broad churn.

Do not extract archives or perform unrelated materialization solely to make an early HDCD scan possible. Reuse existing preview/materialized carriers where the fresh snapshot already exposes them. Keep unsupported early-source families fail-closed rather than expanding this task into new preview architecture.

---

# 3. Reuse the existing HDCD detector; do not invent another scanner

TonePoet already has a whole-file FFmpeg HDCD detector in/near `src/tui/analyze.rs` and existing Details/analysis plumbing around it.

Inspect the fresh snapshot and reuse that detector.

Important existing behavior to preserve:

- whole-file scanning is deliberate because HDCD packets may first occur well after the first second;
- the detector can scan bounded CUE track segments when a single-image CUE supplies authoritative track boundaries;
- the current analysis cache can retain HDCD facts keyed to file identity;
- stale async results are already rejected by session/generation/path identity in existing analysis paths.

Do **not** regress to a short prefix scan.

Do **not** run spectral analysis or unrelated DR/loudness analysis merely to answer HDCD presence for Convert.

For ordinary file batches, scan each technically eligible member independently. Do not assume track 1 represents the album.

For single-image CUE material where authoritative track boundaries already exist and the existing detector can seek reliably, retain per-track HDCD facts rather than treating the entire image as uniformly HDCD.

---

# 4. Replace boolean-only reasoning with a structured HDCD capability result

A simple `hdcd_detected: bool` is insufficient for Convert defaults.

FFmpeg distinguishes valid HDCD packets that actually request output-changing features from packets whose control codes are effectively NOP. TonePoet also needs to know whether the current decoder can do something useful.

Extend/reuse the existing structured scan result so downstream Convert logic can distinguish at least:

```text
NotDetected
Effectual {
    peak_extend: bool,
    low_level_gain_adjustment: bool,
    transient_filter: bool,
    max_gain_adjustment_db: ...,
}
TransientOnly
PassiveNoSupportedFeatures
```

Exact type names are yours to choose.

The underlying raw facts should remain available rather than being flattened prematurely into display strings.

At minimum retain:

- HDCD packets detected yes/no;
- Peak Extend present yes/no;
- nonzero Low-Level Gain Adjustment present yes/no / maximum adjustment;
- Transient Filter flag present yes/no;
- any existing detector error summary that is already available and useful for diagnostics.

Do **not** parse TonePoet's human-readable `hdcd_detail` string to drive planner behavior. If the current persistent analysis cache stores only `hdcd_detected` + `hdcd_detail`, extend the smallest authoritative cache representation needed to retain the structured capability facts, with proper identity/version handling. Reuse the existing analysis-cache migration/version mechanism rather than building a second HDCD cache.

Do not persist UI-only wording.

---

# 5. Important correction: passive / TF-only HDCD is not an ordinary decode case

Do **not** assume that running FFmpeg's `hdcd` filter is a harmless no-op whenever HDCD packets are detected but Peak Extend / Low-Level Gain Adjustment are unused.

FFmpeg's implementation distinguishes:

- HDCD packets with output-changing PE/LLE controls (`HDCD_EFFECTUAL`); and
- HDCD packets whose supported control codes do not change the decoded output (`HDCD_NO_EFFECT`).

That distinction does **not** mean the filter is sample-representation-transparent in the no-effect case. The current FFmpeg implementation still expands/maps the 16-bit source into its wider HDCD output domain even when PE is off and target gain is zero. For a normal 16-bit input, the non-PE path shifts the samples into the 32-bit carrier used for the logical 20-bit decode domain. Therefore invoking `hdcd` on passive or TF-only material is not equivalent to leaving the original 16-bit preservation-domain PCM untouched.

TonePoet should consequently avoid inserting the decoder when the scan found no currently supported output-changing feature to restore.

Policy:

## Effectual HDCD

If **Peak Extend and/or Low-Level Gain Adjustment** is present, `HDCD Decode` is actionable and follows the automatic-default policy below.

If the Transient Filter flag is also present, decode the supported PE/LLE features and use `ⓘ` to disclose that the current FFmpeg path detects but does not reconstruct Transient Filter behavior.

## Transient-filter-only HDCD

If HDCD packets are present and Transient Filter signaling is present, but there is no Peak Extend and no Low-Level Gain Adjustment:

- promote the HDCD status above the fold;
- leave `HDCD Decode` Off;
- make the decode control disabled/unavailable for the current implementation rather than pretending FFmpeg can reconstruct TF;
- show `ⓘ` text such as:

> HDCD signaling is present and includes the Transient Filter flag, but no Peak Extend or Low-Level Gain Adjustment was detected. TonePoet's current FFmpeg decoder detects Transient Filter signaling but does not reconstruct that feature, so there is no supported HDCD transformation to apply here.

Do not characterize this as corruption or a conversion error.

## Passive HDCD / no supported active features

If valid HDCD packets are present but no Peak Extend, Low-Level Gain Adjustment or actionable supported feature is present:

- promote the status above the fold;
- keep decode Off and disabled;
- explain with `ⓘ` that HDCD codes were detected but the current decoder found no supported output-changing features to restore.

This distinction is deliberate and prevents a pointless sample transformation on "HDCD present, no effect" material.

---

# 6. Automatic default policy for effectual HDCD

For an eligible member with effectual HDCD (Peak Extend and/or Low-Level Gain Adjustment), define the preservation domain as:

- lossless output;
- integer PCM;
- 16-bit;
- exactly 44.1 kHz.

## Preservation-domain target

For lossless integer 16/44.1 output:

- surface HDCD Decode above the fold because HDCD was detected;
- default **Off**;
- preserve the original HDCD-coded waveform when the user leaves it Off.

This preserves the relationship to the original CD-domain PCM and permits later HDCD-aware playback/decoding.

## Target leaves the preservation domain

Default HDCD Decode **On** when any of the following is true:

1. output is lossy, at any final sample rate;
2. final lossless sample rate differs from 44.1 kHz;
3. final lossless PCM representation is not integer 16-bit, even if the rate remains 44.1 kHz.

Examples for effectual HDCD:

- FLAC 16/44.1 -> FLAC 16/44.1: **Off**
- FLAC 16/44.1 -> FLAC 24/44.1: **On**
- FLAC 16/44.1 -> FLAC 24/88.2: **On**
- FLAC 16/44.1 -> WAV Int32/44.1: **On**
- FLAC 16/44.1 -> Float32/44.1 WAV: **On**
- FLAC 16/44.1 -> MP3 44.1: **On**
- FLAC 16/44.1 -> Opus 48: **On**

Use the **resolved final output representation**, not merely an unresolved UI enum such as `Source`.

Passive/TF-only detection does not auto-enable decoding under any target because the current decoder has no supported feature restoration to perform.

---

# 7. HDCD Decode changes the precision contract

FFmpeg's HDCD filter expands the HDCD-coded 16-bit stream into a logical 20-bit decoded signal. TonePoet must preserve that information when HDCD Decode is On.

For **lossless** output with HDCD Decode On, the final representation must be capable of retaining the decoded precision:

- Int24 or Int32 are valid;
- Float32 / Float64 are valid if already supported by the selected container/path;
- Int16 is not a valid final decoded representation.

Do not silently decode to 20-bit precision and then truncate back to 16-bit while claiming HDCD Decode succeeded.

## Automatic/source-derived 16-bit target

If HDCD Decode becomes automatically On and the output is lossless but the bit-depth choice is still source-derived/automatic at 16-bit, promote the automatic effective target to **Int24**, using the existing auto-default/provenance machinery.

Make the UI change visible; do not hide it inside the planner.

## Explicit user-selected Int16 target

If the user explicitly selected a lossless Int16 target and HDCD Decode is On:

- do not silently override the explicit bit-depth choice;
- mark/refuse the combination with an actionable message such as:

> HDCD decoding produces more than 16 bits of signal precision. Choose a 24-bit, 32-bit or floating-point lossless output, or turn HDCD Decode off.

Do not add a 20-bit output mode merely for this feature unless the fresh repository already supports a real 20-bit target.

For lossy output, keep the decoded signal at high precision internally and let the existing encoder path own its normal input realization; do not quantize to 16-bit first.

---

# 8. The Format-pane UX should mirror the current de-emphasis pattern, not fork it

Inspect the current post-R18 Format-pane implementation and reuse its dynamic-row, promotion, info-button and override-provenance patterns.

`FormatState::pane_rows(maximized)` (or the fresh equivalent) must remain the single authority for:

- rendering;
- keyboard navigation;
- mouse hit registration.

Do not duplicate placement logic across draw/input paths.

## Eligible, scan complete, no HDCD

- HDCD Decode appears only below the fold when Format is maximized;
- Off;
- no warning/promotion.

## Scan pending

Represent the state explicitly enough that automatic policy cannot race the scan. A disabled `Scanning for HDCD…` state below the fold is acceptable.

Do not let an eligible source be silently submitted under an unresolved automatic HDCD policy if the user's eventual default could change from Off to On. Use the smallest existing pending-validation pattern: either submission waits/refuses until the scan resolves, or the user explicitly overrides the automatic HDCD policy. Do not block the render/event thread.

## Effectual HDCD detected

- promote above fold;
- show `HDCD detected` plus concise capability information where space allows;
- show `HDCD Decode: On/Off` according to the target policy;
- show `ⓘ`.

## TF-only / passive HDCD detected

- promote above fold;
- show the detected status;
- Decode remains Off/disabled;
- show `ⓘ` explaining why current TonePoet has no supported decode transformation to apply.

For batches, an aggregate label such as:

> HDCD detected — 3 of 12 tracks (2 decode-capable)

is acceptable, provided the per-item facts remain authoritative for execution.

---

# 9. Automatic choice provenance must become sticky after user interaction

Follow the current de-emphasis / Format auto-default convention already present in the fresh snapshot.

Keep separate state for:

- scan state / per-source HDCD capabilities;
- current HDCD Decode On/Off selection;
- whether the user explicitly overrode the automatic choice;
- whether a source-derived bit-depth promotion to Int24 occurred because HDCD Decode became On.

Before user override, recompute the automatic choice when relevant facts change:

- HDCD scan completes;
- output codec changes;
- resolved output sample rate changes;
- resolved output bit depth/sample representation changes.

After explicit user interaction, later probe/scan completion or target changes must not silently undo the user's HDCD Decode choice.

Do not let HDCD's automatic Int24 promotion erase a user's later explicit bit-depth selection.

On source replacement, clear source-derived HDCD capability state and return the HDCD choice to the new source's automatic-policy provenance.

Extend the existing Convert probe/async baseline snapshots only as much as necessary so late HDCD results cannot overwrite newer user choices.

---

# 10. Batch scanning: technically gate first, then scan HDCD only where useful

Do not assume the representative/first path describes the batch.

Use the smallest asynchronous preflight that establishes per-member facts correctly.

For every batch member:

1. obtain/reuse technical facts;
2. reject it from HDCD scanning unless it is confirmed lossless integer mono/stereo 16/44.1 PCM;
3. for eligible members, consult the valid HDCD analysis cache first;
4. run the existing whole-file/per-track FFmpeg detector only on cache misses;
5. retain the structured HDCD classification per member.

HDCD analysis is substantially more expensive than reading a tag. Use bounded concurrency or the repository's existing sequential/bounded analysis convention so staging a large album does not launch an unbounded fleet of FFmpeg processes.

Do not run unrelated DR/loudness/pre-emphasis analysis as a side effect of this Convert HDCD inventory.

Source replacement/generation invalidation must make stale scan completions harmless.

Automatic On applies only to members with **effectual** HDCD. Untagged/non-HDCD/passive members must not receive the HDCD decoder merely because another track in the batch did.

A user-forced On must still be bounded to members for which an actionable effectual HDCD scan exists; do not run the FFmpeg HDCD decoder against arbitrary eligible-but-not-HDCD tracks or passive/TF-only tracks just because a batch-level toggle is On.

---

# 11. Planner semantic: HDCD decode is a source-bitstream-sensitive operation

HDCD decode is not just another generic source-rate EQ.

The decoder must inspect the original HDCD control stream embedded in the source PCM. Therefore, when enabled, **HDCD Decode must be the first sample-domain transform** on that source/member.

It must precede:

- CD de-emphasis;
- resampling;
- gain/normalization;
- dither/quantization;
- true-peak observation that is intended to describe the converted signal;
- any other effect that can alter the source samples/control-code stream.

Decode/materialize container bytes as needed to expose source PCM, but do not perform a sample-changing DSP operation before HDCD decoding.

Model this as an explicit semantic ordering/phase in the typed plan rather than relying on incidental vector order.

If the current de-emphasis implementation introduced a general concept equivalent to `SourceRate` effects, inspect whether HDCD needs a stronger phase/dependency such as `SourceBitstreamDecode` / `BeforeOtherPcmEffects`. Prefer the smallest extension that makes the ordering invariant explicit.

Do not weaken the existing de-emphasis planner semantics to make room for HDCD.

---

# 12. Mandatory HDCD + pre-emphasis interaction

The fresh repository already contains the de-emphasis feature. Test their coexistence.

If an eligible source is both:

- effectual HDCD; and
- pre-emphasized under the current authoritative de-emphasis policy;

and both corrections are On, the semantic order must be:

```text
source 16/44.1 PCM
    -> FFmpeg HDCD decode
    -> CD de-emphasis at 44.1 kHz
    -> selected resampler, if any
    -> remaining terminal/dither/encode work
```

Never:

```text
de-emphasis -> HDCD decode
```

and never:

```text
resample -> HDCD decode
```

because the earlier sample-domain operation can destroy the HDCD control stream.

If HDCD Decode is manually forced Off while another enabled operation (for example de-emphasis or sample-rate conversion) will irreversibly alter the HDCD-coded PCM, preserve the user's override but surface a durable warning for **effectual** HDCD:

> HDCD Decode is Off. This conversion changes the source PCM and can destroy the HDCD control stream before Peak Extend / Low-Level Gain Adjustment are restored.

Do not show this warning for passive/TF-only HDCD as though supported PE/LLE information were being lost.

---

# 13. Current physical decoder backend: FFmpeg

Today the semantic `HDCD Decode` operation has one supported physical implementation: FFmpeg's `hdcd` filter.

Do not invent SoX-ng HDCD support.

Do not expose an HDCD backend selector in the UI.

The typed planner should select/freeze FFmpeg as the physical HDCD decoder candidate, then continue planning the rest of the route independently.

The execution contract must guarantee:

- source HDCD PCM enters the FFmpeg HDCD filter before any sample-changing transform;
- decoded output remains at the source 44.1-kHz rate unless a separate selected resampler later changes it;
- the decoded logical 20-bit information is carried losslessly into the next planned stage;
- there is no hidden second resample in the HDCD stage.

For the admitted TonePoet source domain, lower the decoder explicitly as a 16-bit HDCD operation with FFmpeg autoconversion disabled (the current filter defaults are `bits_per_sample=16` and `disable_autoconvert=1`, but encode those assumptions deliberately in the typed lowering/contract rather than relying on an ambient default that could drift). The HDCD stage must fail rather than silently inserting a format/rate conversion needed only to make its own filter graph connect.

Validate the actual lowered command/graph and resulting carrier facts. The HDCD stage must remain 44.1 kHz and must not contain a hidden resampler.

Do not use `force_pe` for ordinary HDCD decoding.

---

# 14. High-precision carrier after HDCD decode

FFmpeg's HDCD filter outputs the decoded signal in a wider integer domain. Do not write it into an intermediate Int16 carrier.

Reuse the high-precision typed carrier machinery already present after R18 where possible.

Acceptable implementations include, depending on current architecture:

- exact/widened S32 PCM with a typed 20-bit-valid-signal contract; or
- a Float64 carrier that can exactly represent the decoded integer values before subsequent processing.

Do not choose a carrier merely because it is easy to encode. The carrier contract must preserve the decoded signal exactly enough for downstream SSRC / SoX-ng / FFmpeg processing and must not introduce dither, quantization or rate conversion before the planner assigns those operations.

Record the realized sample rate, sample representation and ownership explicitly enough that downstream lowering cannot accidentally quantize/resample twice.

---

# 15. Multi-tool planner requirement

After HDCD decoding, the user still owns the ordinary conversion settings.

The planner must compose the valid physical route that satisfies them.

Examples are conceptual, not fixed recipes:

```text
FFmpeg HDCD decode
    -> SSRC resample
    -> FFmpeg package/encode
```

```text
FFmpeg HDCD decode
    -> SSRC resample
    -> SoX-ng package-only terminal
```

```text
FFmpeg HDCD decode
    -> SoX-ng resample
    -> FFmpeg terminal encode
```

```text
FFmpeg HDCD decode
    -> FFmpeg/SoXR resample
    -> FFmpeg terminal
```

```text
FFmpeg HDCD decode
    -> terminal encode at 44.1 kHz
```

The current planner may legitimately fuse adjacent FFmpeg operations if the fresh architecture already supports a proven fusion without changing semantics, but fusion is optional. Do not redesign the planner merely to save a process launch.

The invariants are mandatory:

- HDCD decode happens first;
- the user's selected resampler remains the selected resampler;
- there is exactly one sample-rate conversion when a rate change is requested;
- terminal ownership/dither rules remain those of the existing typed plan;
- no stage collapses the decoded precision back to Int16 before terminal policy explicitly permits it (and decoded lossless Int16 is otherwise refused under section 7).

---

# 16. Mandatory SSRC regression case — example, not architecture

The following scenario must work because it exposes incorrect fixed-recipe planning:

- source: lossless integer stereo 16/44.1;
- scan: effectual HDCD detected;
- HDCD Decode: On;
- target: FLAC;
- target: Int24 / 88.2 kHz;
- selected resampler: SSRC.

Conceptually:

```text
source 16/44.1
    -> FFmpeg HDCD decode at 44.1
    -> high-precision carrier
    -> SSRC 44.1 -> 88.2
    -> existing qualified terminal realization
```

The terminal may be an existing SSRC-owned final-sample + package-only route or an SSRC Float64 split-terminal route, according to the fresh planner's current dither/terminal policy.

It must **not** become:

```text
FFmpeg HDCD -> FFmpeg resample -> ...
```

when the user selected SSRC.

It must **not** become:

```text
FFmpeg HDCD -> SSRC -> FFmpeg resample again -> ...
```

It must **not** lose the existing package-only / dither ownership guarantees introduced by prior planner work.

Reuse the current generalized effect/resampler/terminal bridge rather than adding an HDCD-specific SSRC executor.

---

# 17. Mandatory non-SSRC planner cells

Add focused coverage for at least:

## A. No rate change, lossless decoded output

Effectual HDCD 16/44.1 -> FLAC 24/44.1, Decode On.

Expected semantic shape:

```text
FFmpeg HDCD decode -> 24-bit-capable terminal encode
```

No resampler.

## B. Lossy 44.1

Effectual HDCD 16/44.1 -> lossy 44.1, Decode automatically On.

Expected:

```text
FFmpeg HDCD decode -> lossy encoder
```

No gratuitous resampler and no intermediate Int16 truncation.

## C. SoX-ng resampler

Effectual HDCD -> target rate change with SoX-ng selected.

Expected:

```text
FFmpeg HDCD decode -> SoX-ng rate conversion -> terminal
```

## D. FFmpeg/SoXR resampler

Effectual HDCD -> target rate change with the FFmpeg/SoXR family selected.

Expected semantic order:

```text
HDCD decode -> selected FFmpeg/SoXR rate conversion -> terminal
```

Fusion into one FFmpeg graph is acceptable only if the typed plan still proves that order and no hidden resampling/quantization occurs around the HDCD filter.

## E. Explicit Int16 contradiction

Effectual HDCD, Decode On, lossless output explicitly pinned to Int16.

Expected: focused refusal/actionable UI, not silent truncation.

## F. Passive HDCD

HDCD packets detected, no PE/LLE.

Expected: above-fold HDCD status, Decode Off/disabled, no FFmpeg HDCD transform inserted.

## G. TF-only

Transient Filter signaling detected, no PE/LLE.

Expected: above-fold status + `ⓘ`, Decode Off/disabled, no claim that current TonePoet reconstructs TF.

---

# 18. True-peak / normalization interaction

HDCD decoding can materially change level/dynamics. Any true-peak Guard/Normalize or equivalent sample observation that governs the converted output must see the **decoded** waveform when HDCD Decode is On.

Never measure true peak before HDCD decode and then use that result as authority for the decoded conversion.

Inspect the current post-R18 protected true-peak/effect qualification system.

If the existing generalized source-transform path can admit the FFmpeg HDCD decoder under the protected contract with a small, demonstrable qualification, add that qualification and focused evidence.

If doing so would require a broad new numerical qualification campaign, fail closed for the unsupported HDCD Decode + certified true-peak combination with an actionable message. Do not bypass the protected path, silently move HDCD later, or silently disable true peak.

If both HDCD decode and de-emphasis are enabled, any such observation must occur after **both** source corrections.

---

# 19. Detection and execution are separate

The automatic detector may classify a source as effectual HDCD, but execution must not depend on a stale scan detached from source identity.

At request construction / execution admission:

- carry the source identity and structured HDCD capability fact through the same generation/path identity discipline used by the fresh Convert implementation;
- apply HDCD Decode automatically only to the specific members whose current scan says effectual HDCD;
- do not turn a batch-level UI `On` into unconditional `hdcd` filtering of every 16/44.1 member.

If the file identity changes after the scan, invalidate/rescan rather than trusting stale capability data.

---

# 20. Manual override semantics

For an effectual HDCD source the user may explicitly choose:

- **Off** even when automatic policy would choose On;
- **On** even in the lossless 16/44.1 preservation case.

Respect that choice.

If the user chooses On in the preservation case, the conversion is no longer a preservation-domain 16-bit transcode; section 7 applies and the decoded lossless output must become at least 24-bit-capable.

If the user chooses Off while another planned operation will actually change sample values — for example resampling, lossy encoding, de-emphasis, gain/normalization, or other DSP — show the warning described in section 12 because effectual HDCD codes/features may become unrecoverable before PE/LLE are restored.

Do **not** describe a pure lossless 16-bit -> wider-integer 44.1-kHz representation change as though it necessarily destroys the embedded code bits. A bit-exact widening/padding path can preserve the source sample values/control-bit information. However, because ordinary HDCD playback/decoder expectations are tied to the 16-bit CD-domain representation, TonePoet must not promise that a widened 24/32-bit file will remain automatically recognizable as HDCD by downstream software. If Decode is manually Off for such a bit-depth-only widening, use informational wording about loss of the standard 16-bit playback contract rather than the stronger "control stream destroyed" warning.

Do not add a blocking confirmation unless the fresh UI already uses one for a directly comparable override.

Passive/TF-only detection is not manually force-decoded through this UI in the current implementation; the control is unavailable because the supported decoder has no PE/LLE work to perform.

---

# 21. No "HDCD tag stripping" requirement

Unlike pre-emphasis, HDCD is fundamentally encoded in the PCM control stream rather than represented by a standard playback tag that TonePoet must strip from output metadata.

Do not invent an `HDCD` metadata tag or new stripping policy as part of this task.

When Decode is Off and the output remains lossless integer 16/44.1 with sample values preserved, the original HDCD control stream should naturally remain in the PCM.

When Decode is On, or when the user forces Decode Off but performs sample-changing DSP/resampling/lossy encoding, the resulting PCM may no longer carry a valid HDCD code stream. The planner/UI warning covers that consequence; do not attempt to synthesize replacement HDCD control codes.

---

# 22. Preserve AccurateRip / CUEToolsDB-oriented preservation behavior

The reason lossless integer 16/44.1 -> lossless integer 16/44.1 defaults HDCD Decode Off is preservation-oriented.

Do not introduce an automatic HDCD transform merely because codes are detected in this case.

The user may deliberately want the CD-derived waveform preserved for AccurateRip/CUEToolsDB relationships or later HDCD-aware playback/processing.

The `ⓘ` help should make this distinction understandable without overexplaining:

> HDCD is encoded in the 16-bit CD-domain PCM. Leaving Decode off preserves those source samples. Turning Decode on restores supported HDCD features and changes the waveform.

---

# 23. Reuse the current de-emphasis architecture where it is general, not where it is specific

Because the fresh snapshot already contains R18-era de-emphasis work, inspect it first.

Strongly prefer reusing:

- source-evidence / effect-selection provenance;
- dynamic below-fold / promoted-above-fold Format rows;
- `ⓘ` notice plumbing;
- per-member batch effect application;
- source-domain effect planning;
- high-precision carriers;
- selected-resampler freezing;
- SSRC package-only/split-terminal continuation;
- execution fingerprinting;
- true-peak qualification boundaries.

Do **not** blindly model HDCD as the same kind of effect as de-emphasis if that would allow another PCM effect to run before HDCD. HDCD needs a stronger source-first ordering because its decoder consumes the embedded control stream.

Do not regress or rewrite the accepted de-emphasis feature while adding HDCD.

---

# 24. Fingerprints and deterministic identity

HDCD Decode On/Off and its semantic placement are execution-significant.

Use the current typed settings/effects/plan fingerprint machinery so that:

- enabling HDCD Decode changes execution identity;
- disabling it preserves the existing no-HDCD identity;
- the selected physical FFmpeg HDCD operation is represented deterministically;
- UI-only scan wording/capability detail does not enter execution identity unless it changes the actual plan;
- passive/TF-only detection with Decode unavailable does not alter the conversion execution fingerprint.

Do not hash mutable human-readable `hdcd_detail` strings into execution identity.

---

# 25. Presets

Never serialize source-derived HDCD detection/capability facts into a preset.

Inspect the fresh preset handling for the current de-emphasis control.

If source-sensitive DSP controls persist only when explicitly user-overridden, follow that convention for HDCD Decode.

Restoring a preset must not pretend that a new source has HDCD before its scan completes.

Do not redesign the preset schema.

---

# 26. Relevant code to inspect before editing

The fresh snapshot is authoritative; names may have moved after R18.

At minimum inspect current equivalents of:

## Existing HDCD analysis

- `src/tui/analyze.rs`
  - `HdcdResult`
  - `detect_hdcd`
  - `parse_hdcd_output`
  - whole-file / bounded-segment tests
- `src/tui/metadata_view_models.rs`
  - HDCD applicability
  - Details display
- `src/tui/keybindings.rs`
  - Details HDCD analysis target selection
  - async worker / per-file analysis
- `src/db.rs`
  - analysis-cache schema/version
  - narrow metadata-analysis-fact cache get/store
- `src/tui/app.rs`
  - `MetadataAnalysisFacts`
  - file identity / merge behavior

## Convert / de-emphasis UI to reuse

- `FormatField`
- `FormatPaneRow`
- `FormatState`
- `FormatState::pane_rows`
- current de-emphasis state/evidence/override fields
- current source replacement/reset logic
- current Convert batch evidence worker
- current probe baseline/snapshot/generation guards
- current `ⓘ` / notice overlay button plumbing
- request construction / per-item effect injection

## Planner / execution

Inspect the **fresh post-R18 architecture**, especially current equivalents of:

- `tonepoet-pipeline/src/semantic_plan.rs`
  - source-domain effects
  - physical effect candidates
  - effect ordering/dependencies
  - selected resampler contracts
- `tonepoet-pipeline/src/plan.rs`
- `tonepoet-pipeline/src/plugins.rs`
- `tonepoet-pipeline/src/fingerprint.rs`
- `src/convert/pipeline/plan_bridge.rs`
- `src/convert/pipeline/stages.rs`
- registered-effect/source-transform carrier representation
- terminal realization / package-only paths
- protected true-peak planning

Do not code from historical line numbers or from the old R17/R18 bundle structure. Inspect the supplied repository as it exists now.

---

# 27. Focused verification — detection

Add focused tests for the existing detector and new structured classification.

At minimum:

- ineligible 24/44.1 does not launch automatic HDCD scan;
- ineligible 16/48 does not launch automatic HDCD scan;
- lossy 16/44.1 does not launch automatic HDCD scan;
- float 16/44.1-like PCM does not launch automatic HDCD scan;
- multichannel 16/44.1 does not launch unsupported HDCD scan;
- unknown depth/rate does not launch scan;
- eligible lossless integer mono/stereo 16/44.1 does;
- a late HDCD packet beyond the first second is still detected;
- Peak Extend classifies as effectual;
- nonzero LLE with no PE classifies as effectual;
- PE + TF classifies as effectual-with-TF;
- TF only classifies as transient-only/unavailable;
- valid packets with no PE/LLE/TF classify as passive/no-supported-features;
- no HDCD classifies NotDetected;
- structured cached facts round-trip without parsing display text;
- stale cache identity does not arm HDCD on changed bytes.

---

# 28. Focused verification — UI/default policy

For **effectual** HDCD:

- lossless integer 16/44.1 target -> above-fold Decode Off;
- lossless Int24/44.1 -> above-fold Decode On;
- lossless Int24/88.2 -> On;
- lossless Int32/44.1 -> On;
- floating-point lossless output -> On;
- lossy 44.1 -> On;
- lossy 48 -> On.

For no detection:

- ordinary control remains below fold and Off.

For TF-only/passive:

- above-fold HDCD status;
- Decode Off/disabled;
- `ⓘ` explains current capability boundary accurately.

Verify:

- user override survives later target changes;
- manual Decode Off + lossless bit-depth-only 16->24/44.1 does not falsely claim the HDCD control stream was destroyed; any warning is limited to downstream playback-recognition/contract caveats;
- source replacement clears old HDCD capability state;
- late scan completion cannot overwrite a newer user choice;
- automatic Decode On promotes an unoverridden lossless Source/Int16 bit-depth target to Int24;
- explicit Int16 override is not silently changed and creates an actionable invalid combination;
- keyboard/mouse/rendering consume the same dynamic row authority.

---

# 29. Focused verification — batch behavior

Cover:

- track 1 no HDCD, later track effectual HDCD -> aggregate warning still appears;
- one effectual track does not cause non-HDCD tracks to run through `hdcd`;
- passive/TF-only members are counted/described but do not receive decoder stages;
- mixed effectual/passive batch applies decode only where actionable;
- stale source-generation scan results are ignored;
- cached negative results avoid rerunning FFmpeg;
- cache misses use bounded/sequential async scanning rather than unbounded process spawning;
- single-image CUE tracks use authoritative segment boundaries where the existing detector supports them.

---

# 30. Focused verification — planner/tool composition

Validate semantic plans **and actual lowered commands/contracts**, not just UI state.

Mandatory cells:

1. effectual HDCD -> FLAC 24/44.1, no resample;
2. effectual HDCD -> lossy 44.1;
3. effectual HDCD -> FLAC 24/88.2 with SSRC selected;
4. effectual HDCD -> admitted target with SoX-ng resampler selected;
5. effectual HDCD -> admitted target with FFmpeg/SoXR resampler selected;
6. HDCD + de-emphasis both On, proving `HDCD -> deemphasis -> resampler`;
7. HDCD Decode On + explicit lossless Int16 target -> focused refusal;
8. TF-only/passive -> no HDCD decoder node/command;
9. selected resampler remains selected after the FFmpeg HDCD stage;
10. exactly one sample-rate conversion;
11. decoded carrier never becomes Int16 before terminal realization;
12. SSRC package-only / split-terminal dither ownership remains unchanged;
13. no hidden FFmpeg resample is introduced in the HDCD decode stage;
14. the lowered HDCD stage pins the intended 16-bit input interpretation and disables FFmpeg filter-graph autoconversion rather than relying on implicit defaults.

Where current planner tests already provide helpers for physical-candidate ownership, extend them rather than creating an HDCD-only test framework.

---

# 31. Reference-source lock / qualification

Inspect the fresh snapshot's `tests/reference_source_lock.rs` before editing.

Because HDCD affects the typed planner/bridge and possibly protected observation paths, Reference-locked files may need to change. Do not contort the implementation to avoid an honestly required locked-file edit.

State exactly which locked files changed and follow the repository's current requalification workflow.

Do not modify unrelated locked files.

---

# 32. Performance and robustness constraints

Preserve the fast path for all ineligible sources: they should not pay for HDCD scanning.

For eligible material:

- use existing identity-aware cache results where valid;
- scan asynchronously;
- bound concurrent FFmpeg detector processes;
- do not block render/event handling;
- do not run broad analysis when HDCD-only analysis suffices;
- do not create a second media probe/cache subsystem;
- do not add retries, journals or lifecycle machinery without a concrete normal-user failure.

Correctness takes precedence over avoiding the necessary whole-file scan; the existing late-packet regression is evidence that a prefix scan is not an acceptable optimization.

---

# 33. Explicitly out of scope

Do **not** implement in this packet:

- Transient Filter reconstruction;
- changes to FFmpeg's HDCD algorithm;
- Pacific Microsonics filter-bank reverse engineering;
- WMP/PMD100/PMD200 archaeology;
- HDCD encoding;
- synthesis/reinsertion of HDCD control codes;
- a SoX-ng HDCD decoder;
- a generalized new DSP framework when the post-R18 planner can express this operation;
- broad redesign of the analysis database;
- new support for non-16/44.1 HDCD-like material.

Future TF work must be able to extend the structured HDCD capability model without invalidating this feature, but do not build speculative TF execution machinery now.

---

# 34. Build/test constraints

Use the capabilities of the implementation environment honestly.

Run all focused planner/pipeline tests that fit the environment.

Run the root tests relevant to:

- HDCD detector parsing/classification;
- analysis cache;
- Convert Format UI state/provenance;
- async stale-result guards;
- request construction.

If the root crate still exceeds the available memory ceiling documented by the fresh repository, do not deliberately OOM the environment. Run target-free/static audits and smaller crates where possible and state exactly what was not compiled.

Run:

- diff/whitespace checks;
- relevant existing static audits;
- Reference qualification when locked files change and the environment supports it;
- no unrelated source changes.

---

# 35. Delivery

Produce a downloadable code bundle consistent with the repository's current handoff conventions.

Include:

- the complete modified repository snapshot or repository-standard corrective bundle requested by the task;
- integrity hashes / `SHA256SUMS`;
- `DELIVERY_NOTES_...md`;
- `HANDOFF_TO_NEXT_SESSION.md`.

Delivery notes must state:

- exact HDCD scan eligibility;
- structured detection states;
- why PE/LLE is actionable but TF-only/passive is not decoded today;
- automatic On/Off policy;
- 20-bit decoded precision / Int24 promotion policy;
- planner ordering, especially HDCD before de-emphasis;
- FFmpeg's role as current HDCD decoder;
- SSRC / SoX-ng / FFmpeg resampler/terminal composition verified;
- true-peak interaction/qualification status;
- batch/cache behavior;
- Reference-lock changes;
- tests/audits actually run;
- anything not run and why.

The desired implementation is the **smallest complete HDCD Convert feature that tells the truth about current decoder capabilities and lets the existing planner compose the user's requested conversion correctly.**
