# DiscContents Unified Disc Model — Architectural Guidance

## Decision Summary

| # | Decision                    | Answer                                                                                                                                                        |
| - | --------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 1 | Thin vs curated             | Make `DiscContents.presentations` **curated-by-default**. Keep excluded parser artifacts separately as suppressed candidates or diagnostics.                  |
| 2 | Placeholder heuristic       | Classify DVD-Audio placeholders using **AOTT-derived track count and duration**, not SAMG track count or correlation type alone.                              |
| 3 | Model placement             | Put the model in the **main crate under `src/disc/`** now; do not put it in the conversion pipeline.                                                          |
| 4 | AOB probe timing            | Probe during the public DiscContents construction flow, before final mapping completes. Keep the inner mapper deterministic by passing probe results into it. |
| 5 | Label synthesis             | Generate canonical default labels centrally during mapping, from structured audio fields and shared formatting rules.                                         |
| 6 | Crate dependencies          | Keep core model types format-neutral. Let format-specific mapper modules depend on DVD-Audio and SACD parser types.                                           |
| 7 | CLI command                 | Add `disc-info` as the unified user-facing command. Keep `dvda-info` temporarily for compatibility and deeper DVD-Audio debugging.                            |
| 8 | PreparedSource relationship | Keep `DiscContents` and `PreparedSource` independent. Use `DiscContents` for browsing and selection, then use native materializers for conversion.            |
| 9 | Diagnostics                 | Carry normalized, user-facing diagnostics in `DiscContents`; keep full native diagnostics on parser-native types.                                             |

---

## 1. Thin vs Curated Model

### Decision

`DiscContents.presentations` should contain only meaningful, user-selectable disc presentations.

For DVD-Audio, placeholder groups should not appear in `presentations`.

The model should still preserve evidence of suppressed parser artifacts through one of these mechanisms:

* `suppressed_presentations`
* `presentation_candidates`
* normalized diagnostics with native references
* access to the original native parser model in debug paths

The important point: `presentations` should represent the browsable user-facing view, not every group the parser discovered.

### Rationale

`DiscContents` exists between format-specific parsers and consumers such as the TUI browser, CLI `disc-info`, and Convert screen stream picker. Those consumers need a shared browsing abstraction. They should not each rediscover which DVD-Audio groups represent real audio programs.

A thin model pushes the same policy decision into every caller. That creates drift:

* TUI may hide placeholders.
* CLI may show placeholders.
* Convert screen may expose menu/gap groups as selectable streams.
* Tests may validate different counts depending on the consumer.

A curated primary model makes the common path correct by default.

This does not require throwing away parser evidence. The model can still carry suppressed candidates or diagnostics. The key distinction is semantic: a placeholder group is a parser artifact, not a `DiscPresentation`.

### Concrete Guidance

Use this model meaning:

* `presentations`: meaningful, user-selectable presentations
* `suppressed_presentations` or equivalent: parser-discovered candidates excluded from normal browsing
* diagnostics: reasons for suppression, probe status, consistency findings, and native provenance

Default consumers should use `presentations`.

Debugging and raw inspection paths may show suppressed candidates.

---

## 2. Placeholder Heuristic

### Decision

For DVD-Audio only, classify a group as a placeholder when either condition holds:

```text
AOTT-derived track count == 0

OR

AOTT-derived track count == 1
AND AOTT-derived total duration < 5 seconds
```

The heuristic must use the AOTT-visible group shape:

```text
group title_refs -> titles -> chapters -> chapter durations
```

It must not use SAMG track count for placeholder detection.

### Rationale

The known placeholder groups share the same user-visible shape:

* one AOTT-derived track
* about one second of duration
* menu/gap-like behavior
* no meaningful presentation value

This correctly handles the current corpus:

| Disc                | Parser groups | User-facing presentations | Suppressed placeholders |
| ------------------- | ------------: | ------------------------: | ----------------------: |
| `hdad2009`          |             2 |                         2 |                       0 |
| `ap_i_robot`        |             2 |                         2 |                       0 |
| `ap_friendly_card`  |             2 |                         2 |                       0 |
| `ap_eye_in_the_sky` |             2 |                         2 |                       0 |
| `hawks_and_doves`   |             2 |                         1 |                       1 |
| `talking_heads_77`  |             3 |                         2 |                       1 |
| `mgletsgetiton`     |             6 |                         4 |                       2 |

The SAMG edge case makes this rule necessary. MGLETSGETITON groups 2 and 4 have mixed AMG/SAMG data. If the heuristic uses SAMG-derived track counts, those placeholder groups may appear real. The AOTT shape reflects the browsable group that users would see.

### Do Not Use as Primary Criteria

Do not classify a placeholder based only on:

```text
GroupCorrelation::MixedAmgAndSamg
```

Correlation describes provenance. It does not prove that a group is or is not a meaningful presentation.

Do not classify based only on codec or sample format. A placeholder can still carry enough data to resolve format details.

### Suppression Metadata

Each suppressed candidate should carry a reason such as:

```text
DVD-Audio placeholder: one AOTT-derived track, one-second duration.
```

Also preserve:

* original group number
* AOTT-derived track count
* AOTT-derived duration
* group correlation
* any SAMG presence
* probe result, if available

### Validation

Tests should assert:

* curated presentation count
* suppressed placeholder count
* exact suppressed group IDs
* reason code for each suppression
* MGLETSGETITON mixed AMG/SAMG placeholders remain suppressed despite SAMG data

The test should not only assert final counts. It should prove the edge case behavior.

---

## 3. Model Placement

### Decision

Place the model in the main crate under:

```text
src/disc/
```

Recommended structure:

```text
src/disc/
  mod.rs
  model.rs
  diagnostics.rs
  labels.rs
  dvda_mapper.rs
  sacd_mapper.rs
```

Do not put it under the conversion pipeline.

Do not create a new crate yet.

### Rationale

`DiscContents` serves application-level browsing, inspection, and stream selection. The main crate already owns those consumers.

The conversion pipeline has a different purpose: preparing exact extraction work. Placing `DiscContents` there would make the browsing model look like a conversion model.

A new crate may become useful later, but this model still needs to settle across DVD-Audio and SACD before extraction. Starting in the main crate lets the team move quickly while preserving a clean module boundary.

### Future Extraction Rule

Move format-neutral types into a crate later only after:

* DVD-Audio and SACD mappings stabilize
* `disc-info`, TUI, and Convert screen all consume the model
* at least one future format confirms the abstraction holds
* the core model can remain parser-neutral without awkward feature flags

A likely future crate would contain only the neutral model and shared label/diagnostic types. Parser-specific adapters can remain outside it.

---

## 4. AOB Probe Timing

### Decision

Run the DVD-Audio AOB probe as part of the public DiscContents construction flow, before final mapping completes.

Internally, keep this as two steps:

```text
parse DVD-Audio -> native DvdaDisc
probe AOBs -> group-level probe results
map native model + probe results -> DiscContents
```

The public API should make the probe hard to forget. The inner mapper should not perform I/O directly.

### Rationale

The AOB probe gives the model information the IFO alone cannot reliably provide, especially for multi-format title sets. Without it, `DiscContents` would show avoidable `Unknown` codecs and weaker labels.

However, putting I/O inside the mapper makes tests harder and hides failure modes. A deterministic mapper that accepts probe results gives better control.

This gives both benefits:

* consumers get accurate results through the normal construction path
* tests can inject empty, partial, or fixture probe results
* IFO-only fixtures remain valid
* probe failures become diagnostics instead of construction blockers

### Format Resolution Priority

For DVD-Audio presentation audio format, use this order:

1. AOB probe result
2. SAMG-derived format
3. IFO audio attributes
4. Unknown

The model should carry provenance for the chosen format source.

### Probe Failure Policy

Probe failure should not prevent building `DiscContents`.

Instead:

* keep the presentation if it otherwise passes curation
* use the best fallback format
* add a warning diagnostic
* mark audio format provenance as fallback or unknown

---

## 5. Label Synthesis

### Decision

Generate default human-readable labels centrally during mapping.

Consumers may choose shorter rendering, but they should not rebuild parser-specific audio summaries.

### Rationale

Labels such as these require shared interpretation:

```text
MLP 96kHz/24-bit 5.0
LPCM 192kHz/24-bit Stereo
DSD64 Stereo
DSD64 Multichannel
Unknown 48kHz/16-bit Stereo
```

If each consumer builds labels separately, the CLI, TUI, and Convert screen will eventually disagree.

The mapper has the needed context:

* codec
* sample rate
* bit depth
* channel count
* channel layout
* SACD area kind
* DVD-Audio probe provenance
* fallback status

### Concrete Guidance

Each presentation should carry both:

```text
label
structured audio format fields
```

The label is for default display.

The structured fields are for sorting, filtering, testing, and alternate UI formatting.

### Disc Label Policy

Do not use an empty DVD-Audio `provider_identifier` as the displayed disc label.

Use this fallback order:

1. Non-empty sidecar or user-provided album/disc title
2. SACD master text album title
3. Non-empty DVD-Audio provider identifier
4. Volume label or source directory/file stem
5. Generic format label, such as `DVD-Audio Disc` or `SACD Disc`

This prevents blank headings in CLI and TUI output.

---

## 6. Crate Dependencies and Mapper Boundaries

### Decision

Keep the core model types format-neutral.

Let mapper modules depend on format-native parser types.

Recommended split:

```text
src/disc/model.rs
  DiscContents
  DiscPresentation
  DiscTrack
  DiscFormat
  DiscDiagnostic
  AudioPresentationFormat

src/disc/dvda_mapper.rs
  DvdaDisc + probe results -> DiscContents

src/disc/sacd_mapper.rs
  SacdMetadata -> DiscContents

src/disc/labels.rs
  shared label formatting
```

### Rationale

The model should not expose `DvdaDisc`, `SacdMetadata`, `GroupCorrelation`, or `TocConsistencyReport` as required knowledge for consumers.

Consumers should deal with the unified model.

Mappers may understand native structures deeply. That is their purpose.

### Dependency Rule

Use this direction:

```text
format-native parser model -> mapper -> DiscContents -> consumers
```

Avoid this direction:

```text
DiscContents core types -> parser-native internals
```

The model may carry native references as opaque strings or IDs for diagnostics, but it should not force consumers to import parser-specific types.

---

## 7. `disc-info` CLI Command

### Decision

Add `disc-info` as the unified user-facing command.

Keep `dvda-info` temporarily.

### Rationale

The user-facing command should match the unified browsing model. `disc-info` should auto-detect disc format and display the curated `DiscContents` view.

`dvda-info` still has value during transition because DVD-Audio has format-specific debugging needs: AOTT/SAMG correlation, AOB probing, and placeholder suppression validation.

### Recommended Behavior

Default:

```text
disc-info <path>
```

Shows curated presentations only.

Raw/debug:

```text
disc-info --raw <path>
```

Shows curated presentations plus suppressed candidates.

Diagnostics:

```text
disc-info --diagnostics <path>
```

Shows warnings and errors.

Verbose:

```text
disc-info --verbose <path>
```

Includes info-level provenance, including probe source and suppression reasons.

Format-specific:

```text
dvda-info <path>
```

During transition, keep this as either:

* the existing DVD-Audio-native diagnostic command, or
* an alias for `disc-info --format dvd-audio --raw --verbose`

Do not remove `dvda-info` until `disc-info` can show all information needed to debug the current DVD-Audio corpus.

---

## 8. Relationship to `PreparedSource`

### Decision

Keep `DiscContents` and `PreparedSource` independent.

Do not make `DiscContents` convertible into `PreparedSource` as the primary architecture.

### Rationale

They answer different questions.

`DiscContents` answers:

```text
What can the user browse, inspect, and select?
```

`PreparedSource` answers:

```text
What exact extraction work should the conversion pipeline perform?
```

A browsing model should not carry every sector range, source reference, and extraction detail. A conversion model should not become responsible for UI presentation and curation.

### Correct Relationship

Use `DiscContents` to create a user selection.

Then pass that selection, together with the native parser model, to the format-specific materializer.

Recommended flow:

```text
parse/probe -> native model
native model + probe results -> DiscContents
user selects presentation/tracks
selection + native model -> materializer -> PreparedSource
```

This keeps conversion precise without bloating the browsing model.

### Required Bridge

The model should provide stable IDs that materializers can resolve:

* DVD-Audio presentation ID should map back to group number.
* SACD presentation ID should map back to stereo or multichannel area.
* Track IDs should map back to native chapter or SACD track index.

That bridge should support selection, not full conversion.

---

## 9. Diagnostics

### Decision

`DiscContents` should carry normalized diagnostics intended for user-facing display.

Full native diagnostics should remain on the native parser types.

### Rationale

The unified model should expose warnings that affect browsing and selection:

* suppressed DVD-Audio placeholders
* probe failures or fallback format decisions
* copy protection status
* SACD TOC consistency warnings
* invalid or suspicious sector ranges
* mismatches between redundant metadata copies

But not every native diagnostic belongs in the unified display. Deep parser internals should remain available through debug commands or native parser structures.

### Recommended Diagnostic Fields

Each normalized diagnostic should include:

```text
severity
scope
code
message
source format
native reference
```

Suggested severity levels:

```text
Info
Warning
Error
```

Suggested scopes:

```text
Disc
Presentation
Track
SuppressedCandidate
```

### Example Diagnostics

```text
Info: DVD-Audio group 3 format determined by AOB probe.
Warning: DVD-Audio group 2 suppressed as placeholder: one AOTT-derived track, one-second duration.
Warning: DVD-Audio group 4 had mixed AMG/SAMG provenance but placeholder classification used AOTT-derived shape.
Warning: SACD stereo area TOC redundant copy mismatch.
Error: SACD track sector range is invalid.
```

### Display Policy

Default CLI and TUI views should show:

* errors
* warnings

Verbose views should also show:

* info-level probe provenance
* placeholder suppression reasons
* native references

Raw/debug views should show suppressed candidates alongside diagnostics.

---

# Edge Case Guidance

## SAMG-Mixed Placeholders

Treat SAMG-mixed placeholder groups as suppressed if their AOTT-derived shape matches the placeholder heuristic.

Do not let SAMG track data override placeholder classification.

Preserve SAMG presence in diagnostics or suppressed-candidate metadata.

## Empty DVD-Audio Provider Identifier

Never display an empty disc title.

Use the disc label fallback chain:

```text
sidecar/user title
SACD album title
non-empty provider identifier
volume label or file stem
generic format label
```

## Multi-Format Title Sets

Do not assume one title set equals one presentation format.

For DVD-Audio, group-level presentations need group-level format resolution.

Use AOB probe results when available. If unavailable, fall back to SAMG, then IFO attributes, then Unknown.

The model should allow different presentations from the same title set to show different codecs, sample rates, bit depths, or channel layouts.

---

# Final Recommended Data Flow

## DVD-Audio

```text
parse ISO or directory
build DvdaDisc
probe AOBs when available
classify group candidates
suppress placeholders
map meaningful groups to DiscContents.presentations
record suppressed candidates and diagnostics
```

## SACD

```text
parse ISO
build SacdMetadata
map stereo area if present
map multichannel area if present
carry TOC consistency diagnostics
```

## Consumers

```text
TUI browser:
  show DiscContents.presentations

CLI disc-info:
  show DiscContents.presentations

CLI disc-info --raw:
  show DiscContents.presentations plus suppressed candidates

Convert screen:
  show DiscContents.presentations for selection

Conversion pipeline:
  use native materializer with selected presentation/track IDs
```

---

# Final Answers to the 9 Questions

## 1. Thin vs curated

Use a curated primary model. `DiscContents.presentations` should contain meaningful user-selectable presentations only.

Preserve placeholder groups separately as suppressed candidates or diagnostics.

## 2. Placeholder heuristic

For DVD-Audio, suppress a group when its AOTT-derived track count is zero, or when it has one AOTT-derived track with total duration under five seconds.

Use AOTT-derived count and duration. Do not use SAMG track count for this decision.

## 3. Model placement

Place the model in the main crate under `src/disc/`.

Do not put it in the conversion pipeline. Do not extract a new crate yet.

## 4. AOB probe timing

Run AOB probing during public DiscContents construction, before final mapping completes.

Keep the internal mapper deterministic by passing probe results into it.

## 5. Label synthesis

Generate canonical default labels centrally during mapping, using shared label formatting and structured audio fields.

## 6. Crate dependencies

Keep core model types format-neutral.

Put parser-specific dependencies in DVD-Audio and SACD mapper modules.

## 7. `disc-info` CLI

Add `disc-info` as the unified command.

Keep `dvda-info` temporarily for compatibility and deeper DVD-Audio diagnostics.

## 8. Relationship to `PreparedSource`

Keep the models independent.

Use `DiscContents` for browsing and selection. Use selected IDs plus native parser data to build `PreparedSource` through existing materializers.

## 9. Diagnostics

Carry normalized user-facing diagnostics in `DiscContents`.

Keep full native diagnostics on native parser models for deep debugging.
