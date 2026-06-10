# DVD-Audio Materializer — Phase 2 Implementation Brief

## What I need from you

Design the Phase 2 implementation: wiring the DVD-Audio parser into
tonepoet's conversion pipeline as a materializer. Phase 1 delivered a
standalone IFO parser crate (`crates/dvda-phase1/`) that successfully
parses all 7 test ISOs. Phase 2 integrates it.

Specifically:

1. **Integration strategy**: Should we add `dvda-phase1` as a workspace
   dependency, or copy `src/tui/dvda/` into the main crate? The SACD parser
   lives at `src/tui/sacd/` (in the main crate). The Phase 1 crate mirrors
   that layout (`src/tui/dvda/`).

2. **`TrackSourceRef::DvdaTrack` design**: What fields should it carry?
   We discovered that `track_type` does NOT encode the audio format index
   on real discs (see "Known gap" below). The realize step needs enough
   info to find and read the right AOB sectors.

3. **Detection logic**: How should `is_dvda_candidate()` work? All test ISOs
   are UDF-only. The `isomage` ISO backend is unvalidated. Options: extract
   AUDIO_TS via 7z to temp dir, or probe ISO for `DVDAUDIO-AMG` magic at a
   known sector offset, or require explicit `--dvda` flag.

4. **Group selection UX**: How does `dvda_group` on `SourceOptions` map to
   the `DvdaGroup` model? Default behavior when no group is specified?

5. **CPPM handling**: Return `MaterializeError::Encrypted` when MKB detected?

6. **Metadata strategy**: IFO has no text metadata. What goes in
   `AlbumMetadata` and `TrackMetadata`? Sidecar support now or later?

7. **What to produce**: A downloadable code bundle containing the materializer,
   type changes, detection wiring, and tests. The bundle should compile against
   the current tonepoet codebase.

---

## Phase 1 output: what the parser produces

`parse_dvda_volume(&volume) -> Result<DvdaDisc>` returns:

```rust
pub struct DvdaDisc {
    pub amg: AmgInfo,                       // AUDIO_TS.IFO header + AOTT table
    pub title_sets: Vec<TitleSet>,          // one per ATS (audio or video)
    pub samg: Option<SamgInfo>,             // AUDIO_PP.IFO flat track list
    pub groups: Vec<DvdaGroup>,             // correlated from AOTT + SAMG
    pub copy_protection: CopyProtectionInfo,
    pub supplemental_video_ifo_present: bool,
    pub diagnostics: Vec<DvdaDiagnostic>,
}

pub struct TitleSet {
    pub number: u8,                         // 1-based ATS number
    pub source_file: String,                // "ATS_01_0.IFO" etc (diagnostics)
    pub kind: TitleSetKind,                 // Audio, Video, Unknown
    pub header: AtsiHeader,                 // sector pointers, etc.
    pub audio_pgcit_offset: usize,          // byte offset used for parsing
    pub audio_formats: Vec<AudioAttributes>,// 8 entries, some empty
    pub downmix_matrices: Vec<DownmixMatrix>,
    pub aobs: Vec<AobFileEntry>,            // 9 AOB parts with block ranges
    pub aobs_last_sector: Option<u32>,
    pub titles: Vec<AudioTitle>,
    pub diagnostics: Vec<DvdaDiagnostic>,
}

pub struct AudioTitle {
    pub title_set_nr: u8,
    pub title_nr: u8,                       // raw PGC ID (0x81, 0x82...)
    pub title_ordinal: u8,                  // 1-based ordinal (matches AOTT)
    pub audio_format_index: Option<u8>,     // uniform format, or None if mixed
    pub audio_format_indices: Vec<u8>,      // all distinct format indices
    pub track_count_declared: u8,
    pub index_count_declared: u8,
    pub len_in_pts: u32,
    pub chapters: Vec<AudioChapter>,
}

pub struct AudioChapter {                   // = AudioTrack
    pub track_nr: u8,                       // 1-based within title
    pub track_type: u8,                     // raw byte from IFO
    pub audio_format_index: Option<u8>,     // resolved from track_type (unreliable, see gap)
    pub downmix_matrix: Option<u8>,
    pub index_start: u8,
    pub first_pts: u32,
    pub len_in_pts: u32,
    pub sector_ranges: Vec<SectorRange>,
}

pub struct SectorRange {
    pub index_nr: u8,
    pub first: u32,                         // relative to AOB start
    pub last: u32,
}

pub struct AudioAttributes {
    pub format_index: u8,
    pub present: bool,
    pub audio_type_raw: u16,
    pub channel_format: ChannelFormat,       // group1/group2 rate, depth, assignment
    pub channel_assignment: Option<ChannelAssignment>,
    pub coding: AudioCoding,                 // Always Unknown in Phase 1
}

pub struct DvdaGroup {
    pub group_nr: u8,
    pub title_refs: Vec<TitleRef>,           // ATS + title_ordinal references
    pub samg_tracks: Vec<SamgTrackRef>,
    pub correlation: GroupCorrelation,        // FromAmgAott, FromAtsiFallback, etc.
}

pub struct AobFileEntry {
    pub title_set_nr: u8,
    pub part_nr: u8,
    pub file_name: String,
    pub exists: bool,
    pub byte_len: u64,
    pub block_first: u32,
    pub block_last: u32,
}
```

Volume access is through `DvdaVolume` trait with `DirectoryDvdaVolume`
(validated) and `IsoDvdaVolume` (feature-gated, unvalidated).

### Known gap: track_type format-index assumption is wrong

The bundle assumed `track_type & 0x07` selects one of the 8 audio format
entries. On real discs (MGLETSGETITON), ALL track_type low 3 bits are 0
even for titles that use format 2 (192/24 stereo). foo_input_dvda does
NOT use track_type for format selection — it determines the codec from
AOB packet sub-headers at read time.

For the materializer, this means:
- Single-format ATS: format is known (only one present entry).
- Multi-format ATS: format per title/track is NOT determinable from IFO.
  It will be resolved during Phase 3 (AOB demux) or by correlating AOTT
  entries with audio format entries (AOTT group number → format index is
  a plausible mapping but unverified).

---

## Pipeline types that need changes

### `SourceKind` (types.rs)

```rust
pub enum SourceKind {
    SingleFile,
    SevenZip,
    CueImage,
    SacdIso,
    // DvdAudio,  <-- new variant needed
}
```

Used in: `detect_source_kind()`, `materializer_for()`, `ExtractionProvenance`,
match arms throughout stages.rs and the orchestrator.

### `TrackSourceRef` (types.rs)

```rust
pub enum TrackSourceRef {
    StagedFile(PathBuf),
    CueSegmentCarrier { path, source_image, start_sample, samples, carrier },
    ImageSegment { image, start_sample, samples },
    SacdTrack { iso, track_index, area },
    // DvdaTrack { ... }  <-- new variant needed
}
```

Referenced in: `realize_track()` match, `source_ref_extension()`, manifest
builder, durable log source path extraction.

### `SourceOptions` (types.rs)

```rust
pub struct SourceOptions {
    pub archive_password: Option<SecretString>,
    pub sacd_area: Option<SacdArea>,
    pub cue_sidecar: CueSidecarPolicy,
    pub track_selection: TrackSelection,
    // pub dvda_group: Option<u8>,  <-- new field needed
}
```

Also needs a matching field in `RedactedSourceOptions` and the
`From<&PipelineRequest> for RedactedPipelineRequest` impl in types.rs
which copies SourceOptions fields to their redacted counterparts.

Note: CLI wiring for `--dvda-group` is Phase 5. In Phase 2, `dvda_group`
is always `None`. The materializer should default to group 1 (or all
audio groups) when no group is specified.

### `detect_source_kind()` (stages.rs)

Current order:
1. SACD ISO (magic byte check)
2. CUE image
3. Archive extensions (includes `.iso` → SevenZip)
4. Single audio file

DVD-Audio needs to slot in after SACD but before the `.iso` → SevenZip
fallback. This requires checking the ISO for AUDIO_TS content.

### `materializer_for()` (stages.rs)

```rust
pub fn materializer_for(kind: SourceKind) -> Result<Box<dyn Materializer>, SourceDispatchError> {
    match kind {
        SourceKind::SingleFile => Ok(Box::new(SingleFileMaterializer)),
        SourceKind::SevenZip => Ok(Box::new(SevenZipMaterializer)),
        SourceKind::CueImage => Ok(Box::new(CueImageMaterializer)),
        SourceKind::SacdIso => Ok(Box::new(SacdIsoMaterializer)),
        // SourceKind::DvdAudio => Ok(Box::new(DvdaMaterializer)),
    }
}
```

### `realize_track()` (stages.rs)

The realize_track match needs a `TrackSourceRef::DvdaTrack` arm. In Phase 2,
this should return `Err(ConvertError::UnsupportedTrackSource)` — actual AOB
extraction is Phase 3. The materializer produces PreparedTracks but they
can't be decoded yet.

### `source_ref_extension()` (stages.rs)

Needs a `TrackSourceRef::DvdaTrack { .. }` arm. Should return something
like `"dvda"` or `"mlp"` (TBD based on whether format is known).

### Other match sites

Every exhaustive match on `SourceKind` and `TrackSourceRef` needs a new arm.
These include the manifest builder, durable log writer, and source path
extraction.

---

## SACD materializer as pattern (materializer_sacd.rs)

The SACD materializer shows the pattern Phase 2 should follow:

```rust
pub struct SacdIsoMaterializer;

impl Materializer for SacdIsoMaterializer {
    async fn materialize(
        &self,
        req: &PipelineRequest,
        staging: &StagingDir,
        _runner: &dyn ToolRunner,
        _reporter: Option<&dyn PipelineReporter>,
        _tool_paths: &HashMap<String, PathBuf>,
        cancel: &CancellationToken,
    ) -> Result<PreparedSource, MaterializeError> {
        // 1. Parse container structure
        let metadata = parse_sacd_iso(&req.container)?;
        // 2. Select area (stereo vs multichannel)
        let area = sacd_area_info(&metadata, requested_area)?;
        // 3. Build PreparedTrack entries
        for (idx, entry) in area.tracks.iter().enumerate() {
            tracks.push(PreparedTrack {
                id: TrackId { source_ordinal, disc_number, track_number },
                source_ref: TrackSourceRef::SacdTrack { iso, track_index, area },
                metadata: track_metadata(...),
                expected_samples: None,
                sample_rate: SACD_SAMPLE_RATE_HZ,
                bit_depth: None,
            });
        }
        // 4. Apply track selection
        let tracks = apply_track_selection(tracks, &req.source.track_selection)?;
        // 5. Derive album metadata
        let album_metadata = album_metadata(...);
        // 6. Return PreparedSource
        Ok(PreparedSource { container, kind: SourceKind::SacdIso, tracks, album_metadata, provenance })
    }
}
```

The materializer also needs to construct a `DvdaVolume` from `req.container`:
- If container is an ISO: use `IsoDvdaVolume` (unvalidated), or extract
  AUDIO_TS to staging via 7z/ToolRunner and use `DirectoryDvdaVolume`
- If container is a directory: use `DirectoryDvdaVolume` directly
This is a design question the reasoning model should address.

Key parallels for DVD-Audio:
- Parse with `parse_dvda_volume()` instead of `parse_sacd_iso()`
- Select group instead of area
- Build PreparedTracks from `AudioChapter` entries
- TrackSourceRef::DvdaTrack instead of SacdTrack
- sample_rate and bit_depth come from AudioAttributes/ChannelFormat
- No text metadata from IFO (unlike SACD which has some TOC text)

Detection function parallel:
```rust
pub(crate) fn is_sacd_iso_candidate(req: &PipelineRequest) -> Result<bool, SourceDetectError> {
    let detection = detect_sacd_iso(&req.container);
    Ok(is_sacd_detection_positive(detection)
        || (explicit_sacd_requested(req) && has_extension(&req.container, "iso")))
}
```

---

## Detection challenge

All 7 test ISOs use UDF 1.02. Some (e.g., HDAD2009) also have ISO 9660
bridge partitions, but UDF is the common filesystem. Detection options:

1. **Extract AUDIO_TS.IFO via 7z to temp, check magic** — reliable but slow
   (spawns a subprocess for every .iso file). Not suitable for batch scanning.

2. **Read ISO with isomage, look for AUDIO_TS.IFO** — fast, in-process, but
   isomage is unvalidated on these UDF ISOs.

3. **Probe raw ISO for DVDAUDIO-AMG magic at known sector offsets** — fastest
   but fragile (sector offset varies by disc authoring).

4. **Require explicit `--dvda` or `--dvda-group` flag** — no auto-detection,
   user must opt in. Similar to how `--area` can force SACD handling.

5. **Hybrid**: auto-detect when isomage works, fall back to explicit flag.

The SACD materializer's pattern: auto-detect via fast magic check, but also
accept explicit `--area` to force SACD handling on ambiguous ISOs.

---

## Metadata gap

DVD-Audio IFO files contain NO text metadata (no title, artist, album, genre,
date). The materializer can populate:

From IFO:
- `track_number` (from chapter ordinal or SAMG track_nr)
- `sample_rate`, `bit_depth` (from ChannelFormat, when format is known)
- `extra` map: dvda_group, dvda_title_set, dvda_title_nr, dvda_codec, channel_layout

NOT from IFO:
- title, artist, album_artist, album, genre, date, ISRC, publisher, copyright

Options for text metadata:
- **Sidecar file** alongside the ISO (like SACD's XML sidecar)
- **Directory name heuristics** ("Artist - Album (Year)")
- **MusicBrainz lookup** (future)
- **Leave blank** — user applies via naming templates or post-conversion tagging

Recommendation for Phase 2: leave text metadata blank. Populate structural
fields from IFO. Sidecar support can follow later.

---

## Constraints

- All new variants must be `Serialize`/`Deserialize` (pipeline types use serde)
- `TrackSourceRef::DvdaTrack` must be exhaustively handled in every match
- Detection must not break existing SACD/CUE/7z/single-file paths
- The materializer produces `PreparedSource` but realize_track returns an
  error — no audio extraction in Phase 2
- `#![forbid(unsafe_code)]` for new materializer code
- Tests should verify materialization against the 7-disc fixture corpus
  (structure only, not audio extraction)

---

## Test corpus

Same 7 fixture directories from Phase 0/1 at `tests/fixtures/dvda/`.
Phase 2 tests should:
- Parse each fixture through the materializer
- Assert PreparedTrack count matches corpus expectations
- Assert PreparedTrack sector ranges are populated
- Assert group selection works (default group, explicit group)
- Assert CPPM detection produces MaterializeError::Encrypted for 3 of 7 discs
  (MGLETSGETITON, Hawks & Doves, Talking Heads 77)
- Assert track selection filters work

---

## Phase 1 code bundle

The full Phase 1 crate is at `crates/dvda-phase1/` in the tonepoet repo.
Key files for the reasoning model:

```
crates/dvda-phase1/src/tui/dvda/
  mod.rs          — public API: parse_dvda_volume, DirectoryDvdaVolume, types
  model.rs        — DvdaDisc, TitleSet, AudioTitle, AudioChapter, SectorRange, etc.
  parser.rs       — parse_dvda_volume() orchestrator
  sector.rs       — AobFileEntry, AobSectorReader, build_aob_inventory()
  error.rs        — DvdaError enum
  endian.rs       — big-endian read helpers
  volume/mod.rs   — DvdaVolume trait, DvdaFile trait
  volume/dir.rs   — DirectoryDvdaVolume
  volume/iso.rs   — IsoDvdaVolume (feature-gated)
  ifo/amg.rs      — parse_amg() including AOTT
  ifo/atsi.rs     — parse_atsi() including titles/tracks/sectors
  ifo/samg.rs     — parse_samg()
```
