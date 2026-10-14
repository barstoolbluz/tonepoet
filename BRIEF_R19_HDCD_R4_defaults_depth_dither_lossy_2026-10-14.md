# BRIEF — R19 HDCD: delivery report, then a revised policy assignment

The R19 HDCD delivery is applied. It did not compile, and the gate found a
database migration defect that breaks every upgrade path. Both are below,
followed by the assignment, which supersedes parts of the original R19 brief.

## It did not compile — fourteen errors, five defects

Fixed here, all mechanical. Recorded so the pattern stays visible.

| file | defect |
| --- | --- |
| `src/tui/command.rs` | the lossless check matched `format.format.selected_value()` against `tonepoet_pipeline::AudioFormat`; the pill holds `crate::convert::formats::AudioFormat`. Five type mismatches plus `E0599`, since only the latter has `Lpcm`. |
| `src/convert/pipeline/stages.rs` | `ToolIdentifier` used unqualified and unimported (2). |
| `tonepoet-pipeline/src/semantic_plan.rs` | `PreferredTool` moved into `request.settings`, then matched on afterwards; not `Copy`. |
| `src/tui/draw_output.rs` | the HDCD row suffix is a `String` built inside the match arm; `pill_row` borrows it for as long as `lines` lives. |
| `src/convert/pipeline/stages.rs` | a test held `&mut effects[1].effect` across two immutable reads of `effects`. |

The delivery notes described finding and fixing "a compilation blocker" — the
missing `HdcdDecode` match arm in `execution_evidence.rs`. That one was real;
five more sat behind it.

## Gate: 7466 passed / 14 failed

One is the expected source-lock freshness failure: `semantic_plan.rs`,
`fingerprint.rs` and `stages.rs` all moved, so requalification is owed. Of the
remaining thirteen we ran ten in isolation and each failed 1 of 1; the three we
did not run alone are `db::` v23 migration tests in the same family as the
rest. None look like the known contention flake.

### Ten failures, one cause: the v28 migration assumes a table that does not exist yet

```
src/db.rs:7015
migrate v25 database through current schema:
  "v28 HDCD migration: no such table: analysis_cache"
```

`migrate_v28` runs `ALTER TABLE analysis_cache ADD COLUMN`, while that table's
`CREATE` lives in the schema-creation path, so a stepwise migration reaches the
ALTER first. Every upgrade path the tests cover — v21 through v27 — dies there,
taking eight `db::` migration tests, the v23-to-v24 startup protocol test and
`recent_files::remove_with_db_failure_preserves_memory_for_restart_consistency`
with it.

We did not open a real installed database, so the blast radius is inferred from
those tests rather than observed. On their evidence any database going through
stepwise migration fails to open; a fresh install should be unaffected.

### One more: the HDCD preset provenance test

```
tui::presets::companion_preset_tests::hdcd_auto_promoted_depth_does_not_become_preset_user_policy
src/tui/presets.rs:2858
  left  Source
  right Int24
```

This is the round's own test, and it covers the provenance rule the assignment
below depends on.

## One field observation, relevant to section 3 of the assignment

Reported from the field: turning HDCD Decode on does not widen the bit depth.
The widening does work — with an untouched depth we measured `Int16` become
`Int24` with `hdcd_promoted_from_depth = Some(Int16)`. It requires
`!self.bit_depth_overridden`, and applying a preset that carries a bit depth
calls `mark_bit_depth_user_policy()` (`src/tui/presets.rs:876`), which sets that
flag and so suppresses the promotion for the rest of the session. A preset written before HDCD existed therefore silences the promotion,
and the user learns only at conversion time, from the refusal in
`execute_commit_with_source_options_transform`.

The assignment changes this policy substantially. Worth confirming that its
section 3 promotion survives a preset-owned depth.

---

**Both defects above must be fixed alongside the assignment.** The migration
one is the more urgent: as delivered, an existing installation cannot open its
database.

# ASSIGNMENT

# TonePoet R19 — HDCD Defaults, Bit Depth, Dither and Lossy Encoding

## Assignment

You will receive a complete, integrated TonePoet code bundle containing the previously reviewed R19 HDCD implementation and its R3 corrections.

**Implement the following changes to HDCD conversion policy, Format-pane behavior, bit-depth selection, dithering and planner execution.**

The supplied source tree is authoritative. Inspect the actual implementation, including the existing HDCD detector, Format controls, automatic-default provenance, typed planner, registered effects, resampler selection, dither ownership, terminal encoders and tests before making changes.

The existing R19 brief and addendum remain relevant except where this assignment explicitly supersedes them.

**Important policy decisions:**

1. **Retain automatic HDCD decoding for lossy output** when effectual HDCD is confirmed, as originally specified in R19.
2. Automatically enable HDCD decoding for lossless output at 24-bit precision or higher.
3. Default HDCD decoding Off for 16-bit lossless output.
4. Allow users to enable HDCD decoding and subsequently select 16-bit lossless output, with TPDF dither enabled by default but independently controllable.
5. Preserve high-precision PCM when feeding lossy encoders. Do not force an unnecessary 16-bit intermediate representation or dither operation.

The fourth decision intentionally supersedes R19's previous refusal of HDCD-decoded 16-bit output.

Preserve all unrelated R19 behavior, especially source eligibility, effectual-versus-passive detection, source-first decoding, CUE track boundaries, source identity, existing resampler choices and Reference qualification contracts.

## 1. HDCD eligibility and decoding semantics

Retain the existing strict scan eligibility: known-lossless, integer PCM, 16-bit, 44.1 kHz, mono or stereo, subject to the supported source and CUE handling.

Distinguish:

- **Effectual HDCD:** Peak Extend and/or nonzero Low-Level Gain Adjustment is present. Decoding is actionable.
- **Transient Filter-only HDCD:** Signaling exists, but the current FFmpeg decoder cannot reconstruct the feature. Leave decoding unavailable.
- **Passive HDCD:** HDCD signaling exists without supported output-changing features. Leave decoding unavailable.
- **No HDCD:** Do not insert an HDCD decoder.

When enabled, invoke the existing FFmpeg decoder as the first sample-domain operation. Preserve the logical 20-bit decoded result in the existing high-precision carrier, using 24-bit-capable or higher representations as appropriate.

Do not run the decoder merely because valid but non-effectual HDCD packets were detected. Do not introduce a second detector, a different HDCD algorithm or speculative Transient Filter reconstruction.

## 2. Revised automatic HDCD defaults

Apply these rules **only to sources with confirmed effectual HDCD**.

### Lossless output at 16-bit precision

Default **HDCD Decode Off**, whether the target container is FLAC, WAV, WavPack or another supported lossless format.

This includes ordinary 16/44.1 → 16/44.1 lossless conversions.

For a 16-bit lossless target at a different sample rate, also default Off under this revised policy. Retain the existing warning that resampling or other sample-changing operations can destroy recoverable HDCD signaling if decoding is disabled.

### Lossless output at 24-bit precision or higher

Default **HDCD Decode On** when the resolved lossless output representation is:

- 24-bit integer PCM;
- 32-bit integer PCM;
- a supported high-precision floating-point output format.

For example, an effectual 16/44.1 FLAC source converted to FLAC 24/44.1 should automatically decode HDCD.

Do not automatically change a selected 32-bit or floating-point target down to 24 bits.

### Lossy output

Default **HDCD Decode On** whenever confirmed effectual HDCD is present.

This applies to MP3, AAC, Opus and other supported lossy output formats.

The purpose is to recover supported HDCD dynamics before lossy compression irreversibly discards the original HDCD control information.

The user must remain free to turn HDCD Decode Off. Respect that manual choice, including across unrelated Format changes and state recomputation.

Do not automatically decode passive HDCD or unsupported Transient Filter-only signaling.

**This preserves R19's original automatic-On policy for lossy output.**

### Default provenance

Use the resolved output representation, not an unresolved `Source` selector.

Retain the existing automatic-versus-manual provenance model. Explicit user choices remain authoritative across subsequent format changes, scan completions and other automatic recomputations.

In the absence of an explicit override, changing from lossy output to 16-bit lossless output should recompute the default from On to Off. Changing from 16-bit lossless output to lossy output should recompute the default from Off to On.

Changing the source resets source-derived HDCD state in the existing manner.

Do not allow automatic Int24 promotion to create a feedback loop that changes the HDCD default or causes repeated UI transitions.

## 3. HDCD pill and bit-depth behavior

When HDCD Decode becomes On, whether automatically or manually:

- If the selected output is **16-bit lossless**, automatically change the visible bit-depth selection to **24-bit** when the user manually enables HDCD Decode.
- If the lossless output is already 24-bit or higher, retain that selection.
- If the output uses an existing high-precision floating-point representation, retain it.
- If the output is lossy, retain its codec and sample-format semantics; do not present 24-bit PCM as a fictional lossy output depth.

This automatic change must be visible in the Format pane, not merely an internal planner adjustment.

### Allow the user to return to 16-bit

After enabling HDCD Decode for lossless output, the user may explicitly select **16-bit** again.

This must be valid.

Do not turn HDCD Decode Off automatically. Do not reject the combination merely because the logical decoded signal exceeds 16-bit precision.

Instead, construct the correct processing path:

1. Decode supported HDCD features into the existing high-precision carrier.
2. Perform all selected signal processing and sample-rate conversion.
3. Apply selected dither, if enabled, immediately before final 16-bit integer quantization.
4. Write the 16-bit lossless output.

The UI may disclose that reducing decoded HDCD audio to 16 bits discards some recovered precision. This is informational, not a refusal.

### Automatic TPDF selection

When the user selects 16-bit lossless output while HDCD Decode is On, default dither to **TPDF**.

The user must remain free to turn dither Off or choose another supported dither option.

Respect manual dither overrides. Do not repeatedly re-enable TPDF in response to unrelated state recomputation after the user deliberately turns it Off.

When the output returns to 24-bit or higher, remove an HDCD-derived automatic TPDF selection where appropriate, without overwriting an independently selected manual dither preference.

Reuse existing dither-selection and provenance machinery rather than implementing a second HDCD-specific dither controller.

### Turning HDCD Off

If HDCD Decode automatically promoted the output from a source-derived 16-bit default to 24-bit, turning HDCD Off may restore the prior automatic bit-depth choice.

However, never overwrite a bit-depth choice the user explicitly made after that promotion.

Keep the existing preset behavior: serialize meaningful explicit user overrides, not source-derived detection facts or automatic bit-depth changes.

## 4. Lossy encoding: preserve precision

A lossy codec does not have an output PCM bit depth equivalent to a 16-bit or 24-bit lossless file.

**Do not automatically quantize HDCD-decoded audio to 16-bit merely because the selected output codec is lossy.**

When HDCD Decode is automatically or manually enabled for lossy output:

1. Restore supported HDCD features into high-precision PCM.
2. Perform required resampling and selected DSP operations.
3. Prefer the codec's supported floating-point input path.
4. Encode the lossy output without an unnecessary intermediate 16-bit conversion.

If a particular encoder path genuinely requires signed 16-bit integer input, use the existing terminal quantization mechanism and apply TPDF at that final integer conversion by default, subject to explicit user dither choices.

Do not impose this restriction on codecs that support floating-point input.

In particular, FFmpeg's libopus encoder supports floating-point PCM. The intended path should not unnecessarily reduce precision before Opus encoding.

Treat the internally recovered HDCD precision and the output codec's input sample format as separate properties.

### Performance requirement

The incremental decoding cost is acceptable. Do not sacrifice correct HDCD recovery solely to avoid the additional decoding pass.

However, preserve the existing fast paths:

- Reuse valid cached HDCD detection facts.
- Do not repeat a completed whole-file HDCD scan during conversion.
- Do not introduce redundant decoding or PCM materialization stages.
- Do not perform unnecessary integer quantization before lossy encoding.
- Do not introduce additional source-identity polling in the TUI.
- Preserve R3's completed-scan fast-path and structured analysis-cache fixes.

The expected additional work should be confined to sources that actually require HDCD decoding.

No performance threshold, new user setting or automatic bypass mechanism is necessary.

## 5. Opus and 48 kHz resampling

For a 16/44.1 source destined for Opus at 48 kHz, the expected high-quality path with HDCD Decode On is:

```
Original lossless PCM, 16/44.1
    |
    v
FFmpeg HDCD decode (effectual HDCD only)
    |
    v
High-precision decoded PCM, 44.1 kHz
    |
    v
44.1 -> 48 kHz resampling
(existing Ultra-quality setting)
    |
    v
Selected DSP/effects as correctly ordered
    |
    v
Floating-point encoder input
    |
    v
Opus encoding
```

The illustration establishes source-first decoding, required sample-rate conversion and high-precision lossy encoding. Other effects must retain their actual dependency-correct positions.

Resampling to an Opus-supported rate is required independently of whether HDCD Decode is enabled.

For automatic 44.1 → 48 kHz conversion, use the existing **Ultra-quality** resampling policy where supported. Honor a valid explicit user resampler or quality selection, rather than silently replacing it.

Prevent double resampling.

Do not put `-ar 48000`, a hidden sample-format conversion or any other rate conversion inside the HDCD decoding stage.

The planner must continue to own rate conversion, including its choice of FFmpeg/SoXR, SoX-ng or another existing supported backend.

When HDCD Decode is manually turned Off, retain the ordinary lossy encoding route. Do not insert an HDCD decoder.

## 6. General planner and dither ordering

This requirement extends beyond HDCD.

TonePoet will gain additional DSP effects. The implementation must compose correctly with both existing and future registered processing operations.

The governing invariant is:

**Integer-output dither belongs at the final precision-reducing quantization boundary, after all sample-domain operations that can change the final PCM values.**

In particular:

- HDCD decoding must precede resampling, gain, de-emphasis, ordinary DSP and terminal quantization.
- Existing effect dependencies and required ordering must remain intact.
- Resampling, gain adjustments, normalization and other applicable signal processing must complete before final integer quantization.
- TPDF noise is added to the high-precision signal immediately before rounding/quantization to the target integer representation.
- No later effect may modify samples after final dithering and integer quantization.
- Do not dither the signal twice.
- Do not introduce an intermediate 16-bit carrier where the existing Float64 or equivalent high-precision carrier can preserve the signal.
- Do not dither solely because a lossy codec is selected when its input path does not require integer quantization.

Reuse the existing typed plan, registered-effect ordering, resampler placement and terminal/dither ownership mechanisms.

Do not add a new generalized effect scheduler, new DSP framework or speculative abstractions for future effects.

If an existing planner invariant already enforces correct terminal quantization, extend or reuse it rather than duplicating the rule in HDCD-specific execution code.

### Existing specialized paths

Preserve the existing SSRC terminal, package-only and split-terminal behavior. Preserve dither ownership so that the selected terminal applies any required quantization exactly once.

Preserve existing certified true-peak restrictions and Reference qualification boundaries. Do not weaken those restrictions merely to make a new HDCD combination appear valid.

Where an existing certified route genuinely cannot accept the changed plan, retain its established refusal behavior unless a focused, properly qualified correction is within scope.

## 7. UI requirements

Keep the existing Format-pane row authority, dynamic layout, shared information notices, keyboard handling and mouse hitboxes.

No new HDCD submenu or separate processing settings screen is needed.

The visible behavior must make the user's choices understandable:

- When confirmed effectual HDCD is detected, surface the existing HDCD row.
- For lossy output, default HDCD Decode On, with an explicit Off override available.
- For lossless output at 24-bit precision or higher, default HDCD Decode On.
- For 16-bit lossless output, default HDCD Decode Off.
- When the user switches HDCD On for 16-bit lossless output, visibly select 24-bit.
- When the user subsequently selects 16-bit, leave HDCD On and default dither to TPDF.
- When the user switches dither Off, honor that choice.
- When the target is lossy, do not display a fictional 24-bit lossy output depth.
- When the user changes source or output format, preserve explicit overrides according to the existing provenance rules.

Retain warnings for HDCD Decode Off when subsequent lossy encoding, resampling or DSP can destroy HDCD signaling.

Do not incorrectly warn that sample-exact, bit-depth-only widening necessarily destroys the control stream.

Avoid additional blocking I/O on the TUI event thread.

## 8. Focused acceptance tests

Add or update tests for the changed behavior without rewriting unrelated test suites.

### Automatic policy

Verify that confirmed effectual HDCD produces:

- Lossless 16/44.1 → lossless Int16/44.1: **Off**.
- Lossless 16/44.1 → lossless Int16/48: **Off**, with the existing applicable warning.
- Lossless 16/44.1 → lossless Int24/44.1: **On**.
- Lossless 16/44.1 → lossless Int24/88.2: **On**.
- Lossless 16/44.1 → supported lossless Float32 or Int32: **On**.
- Lossless 16/44.1 → MP3/44.1: **On**.
- Lossless 16/44.1 → AAC/44.1 or 48: **On**.
- Lossless 16/44.1 → Opus/48: **On**.

Verify that undetected, passive and TF-only HDCD sources do not acquire actionable decoding under any of these policies.

Verify that explicitly switching HDCD Decode Off for a lossy target suppresses the decoder and remains authoritative.

### Manual interactions

Test these transitions:

1. Start with an effectual HDCD source and lossless Int16 output.
2. Confirm HDCD Decode defaults Off.
3. Turn HDCD Decode On.
4. Confirm the output automatically becomes Int24.
5. Manually select Int16.
6. Confirm HDCD remains On and TPDF becomes the default.
7. Manually disable dither.
8. Confirm HDCD remains On and dither remains Off.
9. Change an unrelated Format setting.
10. Confirm manual choices remain intact.
11. Turn HDCD Off and verify that automatic depth promotion is reverted only when the user has not replaced it with an explicit choice.

Also test the lossy default transitions:

1. Select a lossy target with confirmed effectual HDCD.
2. Confirm HDCD Decode defaults On.
3. Manually turn HDCD Decode Off.
4. Change lossy codecs and unrelated settings.
5. Confirm the manual Off choice persists.
6. Verify that source replacement resets source-derived state and computes the correct new default.
7. Verify that switching between lossless and lossy targets recomputes automatic defaults when no manual override exists.

Test scan completion, preset save/restore and source-derived versus manual bit-depth provenance.

### Planner and physical execution

Verify the actual stage ordering for:

- HDCD → lossless Int24 without resampling.
- HDCD → selected effects → final TPDF/Int16 lossless output.
- HDCD → 44.1-to-48 Ultra-quality resampling → Opus encoding with high-precision input.
- HDCD Decode manually Off → ordinary 44.1-to-48 Opus conversion.
- HDCD → MP3 and AAC encoding using their supported high-precision input paths.
- HDCD → existing SoX-ng or FFmpeg/SoXR resampling → final integer quantization.
- Existing de-emphasis and true-peak combinations, subject to their current qualification restrictions.
- Existing SSRC package-only and split-terminal variants where supported.

Tests must establish that:

- HDCD runs first when selected.
- Only supported effectual tracks receive decoding.
- Automatic HDCD selection for lossy output produces an actual decoding stage.
- Manual HDCD Off suppresses that stage.
- The 44.1 → 48 kHz conversion is performed once.
- Dither occurs only at the required final integer-quantization boundary.
- Explicit dither Off works.
- Float-input lossy encoding does not incur an unnecessary Int16 conversion.
- No PCM processing occurs after terminal quantization.
- Previous unmodified conversion paths retain their behavior.

Prefer focused synthetic fixtures and existing test harnesses. Use actual encoder and decoder execution tests where practical rather than relying only on planner-string assertions.

## 9. Engineering constraints

This is a focused policy and planner correction to an existing implementation, not a redesign.

Prioritize correctness, high audio fidelity, deterministic planning, responsiveness, performance, and compatibility with the existing repository.

In particular:

- Preserve existing working code and planner contracts wherever possible.
- Do not create parallel state, scanning, caching, dither or terminal-ownership systems.
- Do not introduce new crash-recovery or defensive infrastructure unrelated to this assignment.
- Avoid touching Reference-locked files unless the implementation genuinely requires it.
- Keep changes to the smallest coherent set of components and tests.
- Avoid performance regressions in ordinary conversion paths and the TUI.
- Confine additional HDCD processing cost to conversions that actually require decoding.
- Distinguish actual test evidence from source inspection or inferred correctness.

Do not treat the previous R19 specifications as infallible. Where they conflict with this assignment, this assignment wins. Where they do not conflict, preserve their established contracts.

## 10. Verification and delivery

Produce a complete, authoritative, downloadable updated source bundle suitable for integration and handoff, not an isolated collection of patches.

Run the focused tests and available build checks. On a suitable build host, run the relevant HDCD, Format, database, planner and conversion suites, then the full workspace build and tests.

If Reference-locked source files change, perform the required requalification using the repository's established procedure. Never fabricate or hand-edit qualification artifacts.

If the execution environment lacks the necessary Rust toolchain, pinned FFmpeg version, resources or qualification fixtures, state what could not be run. Do not claim release qualification based on static inspection or an isolated FFmpeg smoke test.

Include a concise delivery report identifying:

- The policy changes implemented.
- The affected source files.
- The verified UI transitions and planner routes.
- The tests executed and their actual results.
- Any remaining build-host or Reference qualification requirements.
- Any genuine unresolved defects, if present.

**Success means TonePoet automatically recovers effectual HDCD for lossy and high-precision lossless output, supports user-controlled 16-bit lossless output with correct terminal dithering, and preserves the existing implementation's performance, integration contracts, conversion fidelity and unrelated functionality.**

Do not add more machinery than the requirements need.
