# DVD-Video ISO Audio Extraction — Full Integration Brief

## Overview

Add support for extracting audio from DVD-Video ISOs. The vendored
`dvdvideo` crate (Phase 0-1, already committed) provides IFO parsing,
title/chapter structure, cell addressing, VOB demuxing, and audio
stream attribute detection. This brief covers the full pipeline and
TUI integration.

All audio codecs are supported (LPCM, AC-3, DTS) — not just LPCM.
Concert film ISOs with AC-3 or DTS audio are valid extraction targets.

## Architecture

Follow the existing SACD pattern: add new enum variants, a materializer,
detection functions, disc mapper, TUI classification, disc browser
integration, and realize dispatch. The dvdvideo crate handles IFO
parsing; our existing DVD-Video LPCM demuxer handles LPCM extraction;
ffmpeg handles AC-3/DTS decode.

## Part 1: Enum Variants

### 1.1 SourceKind (types.rs:812)

Add `DvdVideo` variant:
```rust
pub enum SourceKind {
    SingleFile,
    SevenZip,
    CueImage,
    SacdIso,
    DvdAudio,
    DvdVideo,  // NEW
}
```

**4 exhaustive matches to update:**
- `materializer_for()` at stages.rs:202 — add `SourceKind::DvdVideo => Ok(Box::new(DvdVideoMaterializer))`
- `source_kind_label()` at stages.rs:6845 — add `SourceKind::DvdVideo => "DvdVideo"`
- Work kind match at processor.rs:955 — add `Some(SourceKind::DvdVideo) => WorkKind::MaterializeItem`
- Unit prefix match at processor.rs:963 — add `Some(SourceKind::DvdVideo) => "dvdv-materialize"`

### 1.2 EntryKind (browse.rs:51)

Add `DvdVideoIso` and `DvdVideoDir` variants:
```rust
pub enum EntryKind {
    ParentDir,
    Directory,
    AudioFile(AudioFormat),
    Archive,
    SacdIso,
    DvdAudioIso,
    DvdAudioDir,
    DvdVideoIso,   // NEW
    DvdVideoDir,   // NEW
    OtherFile,
}
```

**5 exhaustive matches to update:**
- Name style at draw_browse.rs:630 — add `EntryKind::DvdVideoIso | EntryKind::DvdVideoDir` arm (purple style, same as DVD-Audio)
- Info pane at draw_browse.rs:915 — add `EntryKind::DvdVideoIso | EntryKind::DvdVideoDir` arm with format display
- `type_label()` at browse.rs:463 — add `EntryKind::DvdVideoIso => "dvdv"` and `EntryKind::DvdVideoDir => "dvdv-dir"`
- `entry_type_rank()` at browse.rs:2202 — add to disc group (rank 25, same as SACD/DVD-Audio)
- `build_browse_entry_menu()` at context_menu.rs:424 — add DVD-Video context menu block (Convert, Browse Audio Streams, Edit metadata, etc.)

**Additional non-exhaustive locations that SHOULD be updated** (won't break but will be functionally incomplete):
- browse.rs:191 — AudioOnly filter: add `DvdVideoIso | DvdVideoDir`
- browse.rs:206 — has_audio_files check: add `DvdVideoIso | DvdVideoDir`
- browse.rs:436 — `is_dvd_audio_iso()`: do NOT add DVD-Video here (it's DVD-Audio specific)
- browse.rs:444 — `is_disc_source()`: add `DvdVideoIso | DvdVideoDir`
- browse.rs:2220 — rank grouping: already covered by rank 25 arm
- browse.rs:2826 — materializable check: add `DvdVideoIso | DvdVideoDir`
- draw_browse.rs:644 — style (already covered by combined arm if added)
- keybindings.rs:791-795 — open entry catch-all: add `DvdVideoIso | DvdVideoDir`

### 1.3 DiscFormat (model.rs:7)

Add `DvdVideo` variant:
```rust
pub enum DiscFormat {
    DvdAudio,
    Sacd,
    DvdVideo,  // NEW
}
```

**1 exhaustive match to update:**
- `name()` at model.rs:14 — add `Self::DvdVideo => "DVD-Video"`

### 1.4 PresentationId (model.rs:55)

Add `DvdVideoTitle(u8)` variant:
```rust
pub enum PresentationId {
    DvdAudioGroup(u8),
    SacdArea(SacdAreaId),
    DvdVideoTitle(u8),  // NEW — title number within VTS
}
```

**4 exhaustive matches to update:**
- `presentation_id_label()` at disc_browser.rs:582 — add `PresentationId::DvdVideoTitle(n) => format!("DVD-Video title {n}")`
- `apply_presentation_to_source_options()` at disc_browser.rs:609 — add DVD-Video arm that sets the VTS/title selection on SourceOptions
- disc-info presentation at main.rs:1333 — add `PresentationId::DvdVideoTitle(n) => format!("Title {}", n)`
- disc-info suppressed at main.rs:1351 — same

### 1.5 TrackSourceRef (types.rs:409)

Add `DvdVideoTrack` variant:
```rust
DvdVideoTrack {
    /// Path to the DVD-Video ISO image.
    iso: PathBuf,
    /// VTS number (1-based).
    vts_number: u8,
    /// Title number within the VTS (1-based).
    title_number: u8,
    /// Chapter number (1-based).
    chapter_number: u16,
    /// Audio stream index (0-7) — corresponds to Private Stream 1
    /// sub-stream IDs 0xA0-0xA7 (LPCM) or 0x80-0x87 (AC-3).
    audio_stream_index: u8,
    /// Audio coding mode from IFO.
    audio_coding: DvdVideoAudioCoding,
    /// Cell sector ranges for this chapter from VTS_C_ADT.
    cell_sectors: Vec<(u32, u32)>,
    /// Title VOB start sector from VTSI_MAT.
    title_vob_start_sector: u32,
    /// Sample rate in Hz (from IFO or stream probe).
    sample_rate: Option<u32>,
    /// Bit depth (LPCM only).
    bit_depth: Option<u32>,
    /// Channel count.
    channels: Option<u8>,
}
```

Add `DvdVideoAudioCoding` enum:
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DvdVideoAudioCoding {
    Lpcm,
    Ac3,
    Dts,
    Mpeg,
}
```

**6 exhaustive matches to update:**
- Central dispatch at stages.rs:275 — add `TrackSourceRef::DvdVideoTrack { .. }` arm that calls `realize_dvdv_track()`
- Source format detection at stages.rs:6276 — add `TrackSourceRef::DvdVideoTrack { .. }` arm
- Track source ref label at stages.rs:6856 — add logging arm
- Source path extraction at stages.rs:10561 — add `TrackSourceRef::DvdVideoTrack { iso, .. } => iso.clone()`
- Work unit routing at processor.rs:1060 — add to realize group (same as SACD/DvdaTrack)
- Work kind at processor.rs:1124 — add `TrackSourceRef::DvdVideoTrack { .. } => WorkKind::MaterializeItem`

## Part 2: Detection Functions

### 2.1 New functions in disc/dvdv_utils.rs (NEW FILE)

```rust
/// Check if an ISO file contains a DVD-Video disc.
pub fn is_dvdv_iso(path: &Path) -> bool

/// Check if a directory contains a DVD-Video disc (VIDEO_TS/VIDEO_TS.IFO).
pub fn is_dvdv_directory(path: &Path) -> bool

/// Check if a path is any kind of DVD-Video source.
pub fn is_dvdv_source(path: &Path) -> bool
```

Detection logic:
- ISO: open with `dvdvideo::DvdDisc::open()`, check success
- Directory: check `VIDEO_TS/VIDEO_TS.IFO` exists with `DVDVIDEO-VMG` magic
- No codec filtering — all DVD-Video ISOs with audio are valid

**Important:** A hybrid DVD-Audio/DVD-Video disc (has both AUDIO_TS and
VIDEO_TS) should be classified as DVD-Audio, not DVD-Video. The DVD-Audio
pipeline already handles these. DVD-Video classification should only
apply when AUDIO_TS is absent or empty.

### 2.2 is_dvdv_candidate() in materializer_dvdv.rs

```rust
pub(crate) fn is_dvdv_candidate(req: &PipelineRequest) -> Result<bool, SourceDetectError>
```

Called from `detect_source_kind()` in stages.rs AFTER the DVD-Audio
check. If `is_dvda_candidate()` returned false, try DVD-Video.

### 2.3 Integration in stages.rs detect_source_kind()

At stages.rs:170, after the DVD-Audio check and before the CUE check:
```rust
if is_dvda_candidate(req)? {
    return Ok(SourceKind::DvdAudio);
}
// NEW: DVD-Video check (after DVD-Audio, before CUE)
if is_dvdv_candidate(req)? {
    return Ok(SourceKind::DvdVideo);
}
```

### 2.4 Classification in browse.rs

Add `classify_dvdv_iso_entries()` and `classify_dvdv_directory_entry()`
following the same cache pattern as DVD-Audio (browse.rs:1007-1026
and 2796-2823). Add cache fields to BrowseState:
```rust
pub dvdv_iso_classify_cache: HashMap<PathBuf, (ClassificationFingerprint, bool)>,
pub dvdv_dir_classify_cache: HashMap<PathBuf, (ClassificationFingerprint, bool)>,
```

## Part 3: Disc Model Mapper

### 3.1 New file: disc/dvdv_mapper.rs

Map a dvdvideo::DvdDisc + parsed VtsIfo(s) to our DiscContents model.

```rust
pub fn map_dvdv_disc(
    disc: &dvdvideo::DvdDisc,
    vts_ifos: &[(u8, dvdvideo::VtsIfo)],
    source_path: &Path,
) -> DiscContents
```

For each VTS that has audio streams:
- Create a `DiscPresentation` per title within the VTS
- Set `PresentationId::DvdVideoTitle(title_number)`
- Build `AudioPresentationFormat` from the VTS audio stream attributes
  (codec label, sample rate, bit depth, channels)
- Build `DiscTrack` from chapters with durations from PGC playback time
- Set `DiscFormat::DvdVideo`

### 3.2 Probe integration in disc_browser.rs

In `probe_disc_contents()` at disc_browser.rs:372, add DVD-Video branch:
```rust
pub fn probe_disc_contents(path: &Path) -> Result<DiscContents, String> {
    if crate::tui::sacd::is_sacd_iso(path) {
        return probe_sacd_contents(path);
    }
    if crate::disc::dvda_utils::is_dvda_source(path) {
        return crate::disc::dvda_utils::map_dvda_source(path);
    }
    // NEW
    if crate::disc::dvdv_utils::is_dvdv_source(path) {
        return crate::disc::dvdv_utils::map_dvdv_source(path);
    }
    Err(format!("Not a supported browsable disc source: {}", path.display()))
}
```

## Part 4: Materializer

### 4.1 New file: src/convert/pipeline/materializer_dvdv.rs

Follow the SACD materializer pattern (materializer_sacd.rs:24-138):

```rust
pub struct DvdVideoMaterializer;

#[async_trait]
impl Materializer for DvdVideoMaterializer {
    async fn materialize(
        &self,
        req: &PipelineRequest,
        staging: &StagingDir,
        _runner: &dyn ToolRunner,
        _reporter: Option<&dyn PipelineReporter>,
        _tool_paths: &HashMap<String, PathBuf>,
        cancel: &CancellationToken,
    ) -> Result<PreparedSource, MaterializeError> {
        // 1. Open ISO with dvdvideo::DvdDisc::open()
        // 2. Parse VTS IFOs (disc.parse_vts())
        // 3. Find requested title/audio stream
        // 4. Build PreparedTrack per chapter with cell sector ranges
        // 5. Return PreparedSource with SourceKind::DvdVideo
    }
}
```

Key fields on PreparedTrack:
- `source_ref: TrackSourceRef::DvdVideoTrack { iso, vts_number, ... }`
- `sample_rate` from IFO audio attributes (or None, let stream self-describe)
- `source_audio` with appropriate coding (Lpcm, Ac3, Dts)

### 4.2 Source options

Add DVD-Video selection fields to SourceOptions (types.rs):
```rust
pub dvdv_vts: Option<u8>,          // VTS number selection
pub dvdv_title: Option<u8>,        // Title within VTS
pub dvdv_audio_stream: Option<u8>, // Audio stream index
```

## Part 5: Track Realization

### 5.1 realize_dvdv_track() in stages.rs or new file

For LPCM tracks: use our existing DVD-Video LPCM demuxer
(`parse_private_stream_1_packets_with_mode(sector, DvdaSubHeaderMode::DvdVideo)`)
to extract LPCM from VOB sectors, same as cross-ATS extraction.

For AC-3/DTS tracks: extract the elementary stream from VOB sectors,
write to a temp file, then decode with ffmpeg to pcm_s32le WAV.

Cell sector addressing:
- `VtsIfo.cell_adt.lookup(vob_id, cell_id)` returns `(start_sector, end_sector)`
- Sectors are relative to the title VOB start (`VtsiMat.title_vob_sector`)
- Read from the ISO at `(title_vob_start_lba + cell_start_sector) * 2048`

### 5.2 VOB file mapping

DVD-Video VOB files are split at 1GB boundaries (VTS_01_1.VOB through
VTS_01_N.VOB). Cell sectors are relative to the concatenated VOB space.
The materializer needs to map cell sectors through the VOB file inventory,
similar to how DVD-Audio maps sectors through AOB files.

The dvdvideo crate's `DvdDisc.video_ts_files` provides the VOB file
inventory with LBA and size for each file.

## Part 6: TUI Integration

### 6.1 Context menu (context_menu.rs:424)

Add `EntryKind::DvdVideoIso | EntryKind::DvdVideoDir` arm with:
- "Convert (default stream)"
- "Browse Audio Streams..."
- "Convert Stream >" submenu (from cached probe)
- Separator
- "Edit metadata" (future — can omit initially)
- "Analyze"

### 6.2 Disc browser probe

Already handled by Part 3.2 — `probe_disc_contents()` dispatches to
the DVD-Video mapper. The disc browser overlay, stream pill, and
presentation switching are all format-agnostic via DiscContents.

### 6.3 apply_presentation_to_source_options()

At disc_browser.rs:609, add:
```rust
PresentationId::DvdVideoTitle(title_nr) => {
    options.dvdv_title = Some(*title_nr);
}
```

### 6.4 MusicBrainz TOC

In command.rs, add DVD-Video TOC computation similar to
`dvda_source_to_cd_sectors()`. Use chapter durations from PGC
playback time to build CD-frame sectors. Prefer stereo audio
stream (same logic as DVD-Audio stereo preference).

## Part 7: CLI disc-info

In main.rs `run_disc_info()`, add DVD-Video detection after DVD-Audio:
```rust
if crate::disc::dvdv_utils::is_dvdv_source(path) {
    // Open disc, parse VTS IFOs, map to DiscContents
    // Display presentations with audio format labels
}
```

## Compilation-Breaking Match Arms — Complete Inventory

All 20 exhaustive matches that MUST be updated for compilation:

**SourceKind (4):**
1. stages.rs:202 — materializer_for()
2. stages.rs:6845 — source_kind_label()
3. processor.rs:955 — work kind
4. processor.rs:963 — unit prefix

**TrackSourceRef (6):**
5. stages.rs:275 — central realize dispatch
6. stages.rs:6276 — source format detection
7. stages.rs:6856 — track source ref label
8. stages.rs:10561 — source path extraction
9. processor.rs:1060 — work unit routing
10. processor.rs:1124 — work kind classification

**EntryKind (5):**
11. draw_browse.rs:630 — name style
12. draw_browse.rs:915 — info pane
13. browse.rs:463 — type_label()
14. browse.rs:2202 — entry_type_rank()
15. context_menu.rs:424 — build_browse_entry_menu()

**DiscFormat (1):**
16. model.rs:14 — name()

**PresentationId (4):**
17. disc_browser.rs:582 — presentation_id_label()
18. disc_browser.rs:609 — apply_presentation_to_source_options()
19. main.rs:1333 — disc-info presentation
20. main.rs:1351 — disc-info suppressed

## New Files

1. `src/disc/dvdv_utils.rs` — detection + mapping functions
2. `src/disc/dvdv_mapper.rs` — DiscContents builder (or combined into dvdv_utils.rs)
3. `src/convert/pipeline/materializer_dvdv.rs` — materializer

## Modified Files (17)

4. `src/convert/pipeline/types.rs` — SourceKind, TrackSourceRef, DvdVideoAudioCoding, SourceOptions
5. `src/convert/pipeline/stages.rs` — detection, dispatch, realize, labels
6. `src/convert/pipeline/mod.rs` — module declaration + imports
7. `src/convert/processor.rs` — work kind routing
8. `src/convert/pipeline/plan_bridge.rs` — metadata obligation (add DvdVideo to whitelist)
9. `src/disc/model.rs` — DiscFormat, PresentationId
10. `src/disc/mod.rs` — module declaration
11. `src/disc/labels.rs` — disc_label() DiscFormat arm
12. `src/tui/browse.rs` — EntryKind, classification, caches
13. `src/tui/draw_browse.rs` — display styles, info pane
14. `src/tui/context_menu.rs` — right-click menu
15. `src/tui/keybindings.rs` — keyboard dispatch
16. `src/tui/command.rs` — :tags-mb DVD-V path
17. `src/tui/disc_browser.rs` — probe_disc_contents() branch
18. `src/tui/disc_browser_actions.rs` — apply_presentation_to_source_options()
19. `src/main.rs` — disc-info display
20. `Cargo.toml` (main crate) — add dvdvideo dependency

## Code to read (included in bundle)

```
crates/dvdvideo/src/ifo.rs — DvdDisc, VtsIfo, DvdTitle, DvdChapter, AudioStreamAttr, VtsCAdt
crates/dvdvideo/src/disc.rs — DvdDisc::open(), parse_vts(), DvdFile
crates/dvdvideo/src/lib.rs — re-exports

src/convert/pipeline/materializer_sacd.rs — TEMPLATE: complete materializer pattern
src/convert/pipeline/types.rs — SourceKind, TrackSourceRef, PreparedSource, PreparedTrack
src/convert/pipeline/stages.rs — detect_source_kind, materializer_for, realize dispatch
src/convert/processor.rs — work kind routing

src/disc/model.rs — DiscFormat, PresentationId, DiscContents, DiscPresentation
src/disc/dvda_utils.rs — detection function pattern (is_dvda_iso, etc.)
src/disc/sacd_mapper.rs — DiscContents mapper pattern
src/disc/labels.rs — disc_label helper

src/tui/browse.rs — EntryKind, classification caches, classify functions
src/tui/draw_browse.rs — display match arms
src/tui/context_menu.rs — right-click menu match arms
src/tui/disc_browser.rs — probe_disc_contents, presentation_id_label, apply_to_options
src/tui/disc_browser_actions.rs — source_mode_for_presentation
src/main.rs — disc-info

src/convert/pipeline/mod.rs — module declarations
src/disc/mod.rs — module declarations
```

## Constraints

- The dvdvideo crate is `#![forbid(unsafe_code)]` and has zero dependencies
- DVD-Video detection must NOT trigger on hybrid DVD-Audio/DVD-Video discs
  (those are handled by the DVD-Audio pipeline). Check: if `is_dvda_source()`
  returns true, skip DVD-Video detection.
- All 20 exhaustive match arms must be updated or compilation fails
- No behavior change for existing SACD, DVD-Audio, CUE, 7z, or single-file sources
- The dvdvideo crate uses `DvdDisc::open(path)` which reads via `std::fs::File`.
  For ISO files this is fine. For directory sources, the crate would need
  adaptation or we only support ISO initially.
- AC-3/DTS extraction requires ffmpeg decode to PCM — same pattern as
  existing ffmpeg-based conversion in the pipeline

## Test case

Neil Young & Crazy Horse — Live at the Fillmore East, 1970:
- Pure DVD-Video ISO (no AUDIO_TS)
- VTS_01 has LPCM 96kHz/24-bit/2ch audio
- VTS_02 has secondary content
- Expected: detect as DVD-Video, show LPCM presentation, extract to FLAC
