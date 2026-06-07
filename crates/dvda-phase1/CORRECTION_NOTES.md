# Correction notes

This revision addresses the test-coverage feedback against the original Phase 1 bundle.

## Added self-contained tests

`tests/dvda_phase1_unit.rs` now includes deterministic synthetic binary fixtures for:

* AMG header parsing,
* direct AOTT_SRPT parsing,
* invalid AMG identifier and short AOTT bounds errors,
* ATSI audio format/rate/depth/channel assignment parsing,
* ATSI title/track/index sector assignment,
* SAMG flat-track parsing, AOB/VOB zone flag, channel format, and absolute sectors,
* channel-assignment/rate/depth lookup tables,
* AOB inventory construction,
* AOB sector-reader boundary crossing across AOB parts,
* directory volume fallback to `.BUP`, case-insensitive file lookup, and path-escape rejection,
* SAMG-incomplete diagnostics relative to ATSI hierarchy.

These tests do not require `tests/fixtures/dvda` and therefore still exercise the parser in a standalone bundle checkout.

## Strengthened fixture assertions

`tests/dvda_phase1_fixtures.rs` still skips when the real seven-disc corpus is absent, but when the fixtures exist it now asserts AOTT-specific behavior:

* minimum AOTT entry count per fixture family,
* playback type is marked audio,
* playback-type ATS nibble agrees with title-set reference,
* nonzero AOTT track count,
* nonzero AOTT PTS duration,
* nonzero `atsi_mat_sector`,
* no duplicate AOTT title references,
* each AOTT title-set/title reference resolves into parsed ATSI titles.

## Parser cleanup

* Removed a duplicate match arm in the channel-assignment table.
* Corrected the minimum ATSI matrix size to cover all 14 downmix matrices.
* Changed ATSI PGCIT sector-offset overflow from silent fallback to a structured parse error.

## Multi-format title-set model fix

This revision adds decoded audio-format index exposure instead of leaving later code to infer it from raw `track_type`:

* `AudioChapter.audio_format_index: Option<u8>` resolves the low-three-bit ATS audio-format table selector and verifies that the referenced `AudioAttributes` entry is present.
* `AudioTitle.audio_format_index: Option<u8>` is populated when all chapters in a title use the same format.
* `AudioTitle.audio_format_indices: Vec<u8>` records all distinct active format indices in first-seen order, so a future materializer can handle a mixed title without reparsing raw fields.
* Synthetic ATSI tests now cover one ATS carrying both format 0 and format 2 titles.
* Fixture tests now assert that MGLETSGETITON ATS 01 exposes both active format indices 0 and 2 when the seven-disc corpus is present.

## ISO/UDF validation status

The ISO backend is now treated as feature-gated and under validation, not as proven by scaffold code alone. `docs/ISO_UDF_VALIDATION.md` and `tests/dvda_phase1_iso_validation.rs` define the real-corpus validation path for the seven UDF 1.02 ISO images. Until that test passes on the real ISO corpus, the extracted-directory backend is the validated Phase 1 backend.

## 2026-06-07 forensic/modeling correction

This revision addresses the downmix/SAMG/PGCIT feedback:

* `DownmixMatrix` now keeps the original `[u8; 18]` and exposes typed phase masks plus eight per-source-channel L/R coefficient records. The helpers decode the foo_input_dvda attenuation law for inspection, while Phase 1 still performs no DSP.
* `parse_samg()` now records raw length, expected 128 KiB length, 16 KiB copy size, copy count, per-copy validation results, and diagnostics for unexpected size or mismatched repeated copies. Short synthetic fixtures remain valid parser inputs and produce diagnostics instead of hard failures.
* `ATSI_MAT_PARSED_SIZE` is now computed from the end of the parsed downmix matrix area, rather than being a loose magic constant.
* `TitleSet.audio_pgcit_offset` records the effective byte offset used for `audio_pgcit_t`; unit tests cover both explicit `ats_pgcit = 1` and `ats_pgcit = 0` fallback, and fixture tests assert that the corpus parses at the reference foobar offset `0x800`.
