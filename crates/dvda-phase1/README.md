# DVD-Audio Phase 1 Bundle

This is a drop-in Rust scaffold for Tonepoet's DVD-Audio Phase 1 parser work.
It contains:

* a detailed implementation plan,
* standalone `src/tui/dvda/` Rust module files,
* directory and optional ISO/UDF volume backends,
* AMG, ATSI, and SAMG parsers, including typed downmix metadata, active audio-format indices, and SAMG repeated-copy diagnostics,
* typed data model,
* AOB inventory and logical-sector reader,
* self-contained parser/unit tests,
* fixture integration tests with AOTT and multi-format ATS assertions,
* optional ISO/UDF validation tests for the real ISO corpus,
* an example group-listing command.

## Integrating into Tonepoet

1. Copy `src/tui/dvda/` into the Tonepoet tree.
2. Add `pub mod dvda;` beside the existing SACD module.
3. Add dependencies:

```toml
thiserror = "1"
isomage = { version = "2.1", optional = true }
```

4. Add a Cargo feature if ISO support should be compiled immediately:

```toml
[features]
iso-isomage = ["dep:isomage"]
```

5. Copy `tests/dvda_phase1_unit.rs`, `tests/dvda_phase1_fixtures.rs`, and, if ISO support is enabled, `tests/dvda_phase1_iso_validation.rs` into the workspace tests area.
6. Run the directory fixture tests against `tests/fixtures/dvda`.
7. Run `docs/ISO_UDF_VALIDATION.md` once the original Phase 0 ISO images are available.
8. Keep the parser independent of materializer and pipeline modules until Phase 2.

## Scope boundary

This bundle does not demux AOB files or decode audio. It parses navigation data and prepares the volume/sector abstractions that later phases will use.

## Multi-format ATS handling

Phase 1 now exposes the active audio-format table index directly. `AudioChapter.audio_format_index` resolves the index from `track_type`, and `AudioTitle.audio_format_index` is populated for uniform-format titles. `AudioTitle.audio_format_indices` preserves every distinct active index in the title. This is required for discs such as MGLETSGETITON, where one ATS carries both 96/24 multichannel and 192/24 stereo presentations.

## ISO/UDF status

`IsoDvdaVolume` is compiled only with `--features iso-isomage`. It uses `isomage` tree lookup and `cat_node` streaming, but the adapter should not be called validated until `tests/dvda_phase1_iso_validation.rs` has passed against the seven original Phase 0 ISO images. See `docs/ISO_UDF_VALIDATION.md`.
