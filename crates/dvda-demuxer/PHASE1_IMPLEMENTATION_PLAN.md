# DVD-Audio Materializer — Phase 1 Implementation Plan

This bundle implements the Phase 1 boundary only: DVD-Audio volume access plus AMG, ATSI, and SAMG IFO navigation parsing. It intentionally does not build AOB demuxing, LPCM extraction, MLP decode invocation, materializer wiring, or `TrackSourceRef` changes.

## 1. Module layout

Place the module at:

```text
src/tui/dvda/
```

This mirrors the existing SACD parser at `src/tui/sacd/` and minimizes architectural churn. The module must not depend on TUI state, widgets, conversion planning, or `main`; it should be a standalone parser that can later serve both a DVD-Audio materializer and commands such as `--dvda-list-groups`.

Proposed layout:

```text
src/tui/dvda/
  mod.rs
  error.rs
  endian.rs
  model.rs
  parser.rs
  sector.rs
  volume/
    mod.rs
    dir.rs
    iso.rs
  ifo/
    mod.rs
    amg.rs
    atsi.rs
    samg.rs
```

Rationale:

* `ifo/` keeps the three binary formats separate: AMG, ATSI, SAMG.
* `volume/` isolates filesystem concerns from binary parsing.
* `sector.rs` owns logical AOB block inventory and range reads.
* `model.rs` is the typed contract consumed by future materializer code.
* The whole module is `#![forbid(unsafe_code)]`.

A later refactor to `src/media/dvda/` or a separate crate remains possible because this layout has no TUI dependency.

## 2. Volume abstraction

Phase 1 should use a small, read-only `DvdaVolume` trait:

```rust
pub trait DvdaVolume: Send + Sync {
    fn open_audio_ts_file(&self, name: &str) -> Result<Box<dyn DvdaFile>>;
    fn file_len(&self, name: &str) -> Result<Option<u64>>;
    fn exists_audio_ts_file(&self, name: &str) -> bool;
    fn read_audio_ts_file(&self, name: &str) -> Result<Vec<u8>>;
    fn read_with_backup(&self, primary: &str, backup: &str) -> Result<(String, Vec<u8>)>;
}
```

`DvdaFile` is a boxed `Read + Seek + Send` handle with a known length. This is enough for IFO parsing and future AOB sector reads without requiring memory-mapped files or platform-specific random-access APIs.

### Directory implementation

`DirectoryDvdaVolume` accepts either a disc root containing `AUDIO_TS/` or an extracted `AUDIO_TS` directory. It resolves uppercase DVD names first, then uses case-insensitive fallback for extracted filesystems that lowercase names.

It performs no writes and no path traversal. File names are restricted to single AUDIO_TS entries, not arbitrary paths.

### ISO implementation

`IsoDvdaVolume` is feature-gated behind `iso-isomage`. It uses `isomage` to parse ISO/UDF and streams individual nodes into in-memory cursors. This is deliberate: current `isomage` public API exposes tree lookup and `cat_node`, not a stable random-access file object for files inside the ISO. The adapter is not considered validated until `tests/dvda_demuxer_iso_validation.rs` passes against the seven original Phase 0 UDF 1.02 ISO images. Until then, extracted-directory parsing is the validated Phase 1 backend and ISO support is an explicitly marked adapter under validation.

If `isomage` proves inadequate on target DVD-Audio UDF images, the fallback should be an explicit temporary extraction adapter with the same `DvdaVolume` trait. Do not leak that fallback into the parser.

## 3. IFO parser design

### Shared parsing rules

* All multi-byte fields are big-endian.
* Never transmute C structs.
* Use offset-based reads with explicit bounds checks.
* Fail on invalid magic identifiers for required files.
* Use `.BUP` fallback only when the `.IFO` file is missing. Do not silently fall back after parsing a corrupt `.IFO`; that would hide fixture and disc defects.
* Store source file names in parsed structs for diagnostics.

### AMG parser

`AUDIO_TS.IFO` / `AUDIO_TS.BUP` parser extracts:

* AMG header fields from `amgi_mat_t`.
* Sector pointers, especially `aott_srpt`.
* Provider identifier and volume fields.
* `aott_srpt` audio-only title table entries.

The `aott_srpt` table is required for the disc-level navigation view. It maps top-level audio title entries to ATS number and title number. The parser treats these entries as navigation records, not as the sole source of track boundaries.

### ATSI parser

`ATS_XX_0.IFO` / `ATS_XX_0.BUP` parser extracts:

* ATSI header fields.
* Audio format entries at `0x100`.
* Raw downmix matrices at `0x180`.
* Audio title hierarchy from `audio_pgcit`.
* Track timestamps.
* Track sector/index tables.

ATSI is authoritative for hierarchy and relative sector ranges. The parser follows the foo_input_dvda sector assignment logic: a sector index belongs to a track when `index >= current_track.n` and either `index < next_track.n` or the current track is the last track.

The code uses `ats_pgcit` when present as a sector pointer, falling back to `0x800`. The reference implementation seeks to `0x800`; accepting the header pointer is a modest robustness improvement that does not change the normal path. `TitleSet.audio_pgcit_offset` records the effective byte offset, and the fixture tests assert that the Phase 0 corpus parses at the reference `0x800` location.

### SAMG parser

`AUDIO_PP.IFO` parser extracts:

* Declared SAMG track count.
* Flat track list.
* Group number, track number, PTS duration.
* AOB/VOB zone flag.
* Channel format.
* Absolute first/last sectors and duplicate first-sector field.

SAMG is optional and non-authoritative because the corpus shows it can omit multichannel content. The parser preserves SAMG data and emits diagnostics when SAMG track count is lower than ATSI hierarchy track count.

## 4. Typed data model

The brief's proposed model is mostly right, but it needs two refinements.

First, `AudioChapter` should preserve the reference implementation's track semantics. foo_input_dvda calls the parsed unit `dvda_track_t`; it has track number, index start, first PTS, duration, downmix matrix, and one or more sector pointers. The bundle keeps the public type name `AudioChapter` but aliases `AudioTrack = AudioChapter` so later code can choose a more precise public name.

Second, audio codec should not be overclaimed in Phase 1. The ATSI audio format table gives channel assignment, group sample rates, and bit depths, but this phase does not parse AOB packets. Therefore `AudioAttributes.coding` is `Unknown` for now. MLP/LPCM classification belongs in the AOB demux/materializer phase.

The active audio-format table index is still Phase 1 data. The ATS track timestamp `track_type` byte selects one of the eight `ats_audio_format` entries in its low three bits. The parser therefore exposes `AudioChapter.audio_format_index` and title-level `AudioTitle.audio_format_index` / `AudioTitle.audio_format_indices`. This prevents later materializer code from having to rediscover the mapping for multi-format title sets such as MGLETSGETITON, where ATS 01 carries both format 0 and format 2.

Core model:

```text
DvdaDisc
  AmgInfo
    AudioTitleTableEntry[]
  TitleSet[]
    AtsiHeader
    AudioAttributes[8]
    DownmixMatrix[14]        // raw bytes plus typed phase/coefficient helpers
    AobFileEntry[9]
    AudioTitle[]
      AudioChapter[]
        SectorRange[]
  SamgInfo?
    SamgTrack[]
  DvdaGroup[]
  CopyProtectionInfo
  diagnostics[]
```

The model includes `AobFileEntry` inventory because Phase 1 owns volume abstraction and sector-range navigation. It does not include packet streams, demuxed frames, prepared tracks, decode commands, or output formats.

## 5. SAMG cross-referencing policy

Do not collapse SAMG and ATSI into one synthetic truth.

Recommended policy:

1. Parse ATSI hierarchy fully. This is authoritative for title, track, index, and relative AOB sector ranges.
2. Parse AMG AOTT entries to form the disc-level title/navigation view.
3. Parse SAMG as optional supplemental absolute-sector metadata.
4. Build `DvdaGroup` conservatively:
   * If AOTT exists, groups are seeded from AOTT ordinal entries and title refs.
   * If AOTT is absent, fallback groups are seeded from ATSI title order.
   * SAMG tracks are attached by `group_nr` where possible, but SAMG cannot erase or hide ATSI content.
5. Emit diagnostics when SAMG is incomplete relative to ATSI, when SAMG duplicate absolute-sector fields disagree, or when the expected repeated-copy structure is missing or inconsistent.

Future correlation can match by duration, order, and sector delta, but Phase 1 should preserve source records rather than inventing exact correlations.

## 6. Test strategy

Use the seven fixture directories under `tests/fixtures/dvda/`.

### Unit tests

* `ifo::amg::parse_amg`: magic, title-set count, AOTT table count and entries.
* `ifo::atsi::parse_atsi`: title count, irregular title numbers, track counts, channel assignment, rate/depth groups, sector assignment.
* `ifo::samg::parse_samg`: declared count, group/track numbering, zone flag, absolute sector duplicate equality.
* `sector::build_aob_inventory`: present AOB file block ranges, missing AOB virtual ranges.
* `volume::DirectoryDvdaVolume`: root vs `AUDIO_TS` root, uppercase/lowercase resolution, missing file behavior.

### Fixture integration tests

For each fixture, parse the directory volume and assert stable corpus facts:

* number of ATS title sets,
* expected CPPM/MKB presence,
* title and track counts,
* known irregular title numbers,
* expected channel assignment/rate/depth where known,
* SAMG incomplete diagnostic on discs where SAMG omits multichannel content.
* SAMG 128 KiB / repeated-copy validation assertions against fixtures, plus synthetic mismatch tests.
* Effective ATSI `audio_pgcit_t` offset assertions, including the reference `0x800` fixture path and `ats_pgcit = 0` fallback unit path.

The corrected bundle includes self-contained synthetic unit tests that run without the corpus, plus a fixture integration test that skips only the real-fixture assertions when `tests/fixtures/dvda` is absent.

### Golden JSON

Use `corpus_probe_output.json` only as an oracle for facts already produced by Phase 0. Add new golden values for AOTT once the Rust parser extracts them, because the diagnostic Python script did not parse AOTT.

## 7. What not to build in Phase 1

Do not build:

* materializer code,
* `TrackSourceRef` changes,
* `PreparedSource` / `PreparedTrack` wiring,
* AOB MPEG-PS demuxing,
* LPCM extraction,
* MLP decode invocation,
* FFmpeg integration,
* CPPM decryption,
* AUDIO_SV.IFO parsing,
* output naming or selection UI.

Phase 1 may detect `DVDAUDIO.MKB` and mark likely CPPM presence. It must not attempt decryption.

## 8. Acceptance criteria

Phase 1 is complete when:

1. `DirectoryDvdaVolume` parses all seven extracted fixture directories.
2. `IsoDvdaVolume` passes `tests/dvda_demuxer_iso_validation.rs` against the seven original Phase 0 UDF 1.02 ISO images, or ISO support remains explicitly marked as under validation.
3. AMG, ATSI, and SAMG parsers use explicit bounds checks and return structured `thiserror` errors.
4. AOTT parsing is covered by exact synthetic binary tests and real-fixture AOTT reference/invariant assertions.
5. ATSI title/track/sector assignment matches the Phase 0 corpus probe and foo_input_dvda logic.
6. SAMG omissions do not drop ATSI content.
7. The module has no dependency on conversion pipeline or TUI state.
