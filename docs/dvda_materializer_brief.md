# DVD-Audio Materializer — Research Brief & Implementation Plan

## Purpose

This document presents research findings and a proposed implementation plan for a
`DvdaMaterializer` that extracts lossless audio (MLP and LPCM) from DVD-Audio disc
images. It follows the existing materializer pattern (`materializer_sacd.rs`,
`materializer_cue.rs`, `materializer_7z.rs`) and is intended for review by a
reasoning model before implementation begins.

---

## 1. DVD-Audio Disc Structure

### 1.1 AUDIO_TS Directory Layout

A DVD-Audio disc contains an `AUDIO_TS/` directory (analogous to `VIDEO_TS/` on
DVD-Video) with the following files:

| File                | Purpose |
|---------------------|---------|
| `AUDIO_TS.IFO`      | Audio Manager (AMG) — main navigation, pointers to all title sets |
| `AUDIO_TS.BUP`      | Backup of AUDIO_TS.IFO |
| `AUDIO_TS.VOB`      | Optional video menu (hybrid/universal discs only) |
| `AUDIO_PP.IFO`      | Simple Audio Manager (SAMG) — CD-like TOC for simple players. Always 128 KB. Max 314 track entries with absolute sector pointers |
| `ATS_XX_0.IFO`      | Audio Title Set Information for title set XX (01–99). Contains track structure, PTS timestamps, sector pointers, audio attributes |
| `ATS_XX_0.BUP`      | Backup of corresponding IFO |
| `ATS_XX_Y.AOB`      | Audio Object files. XX = title set (01–99), Y = part (1–9). Each file capped at 1 GB |
| `DVDAUDIO.MKB`      | CPPM Media Key Block (only on encrypted discs) |

### 1.2 Hierarchy: Album > Group > Title > Track

DVD-Audio uses a five-level hierarchy:

- **Album**: one per disc side
- **Group** (up to 9): essentially playlists. Typical layout:
  - Group 1 = stereo high-resolution mix
  - Group 2 = multichannel (5.1) mix
  - Group 3 = Dolby Digital bonus (optional)
- **Title** (Audio Only Title / AOTT): drawn from AOB data. All tracks within a
  title share identical audio attributes (codec, sample rate, bit depth, channels)
- **Track** (up to 99 per title/group): individual songs
- **Index**: sub-track markers (rarely used in practice)

### 1.3 Audio Title Set (ATS)

An ATS is a collection of AOB files sharing the same audio format parameters. Each
ATS has one IFO file and one or more AOB parts. Stereo and multichannel versions of
the same album typically occupy *separate* ATS's because they have different channel
counts.

### 1.4 AOB File Format

AOB files are **MPEG-2 Program Streams** (ISO 13818-1) with a fixed pack size of
2048 bytes (one DVD sector). Key properties:

- Each sector contains a Pack Header + PES packet of type `0xBD` (Private Stream 1)
- No navigation system packets (unlike VOBs)
- Single audio stream per ATS
- Tracks are **not** separated by file boundaries — the IFO contains PTS timestamps
  and sector offsets defining track start/end within the continuous AOB stream
- Multiple AOB parts (ATS_XX_1.AOB through ATS_XX_9.AOB) are logically concatenated

### 1.5 Audio Codecs in AOBs

| Codec | Notes |
|-------|-------|
| **LPCM** (uncompressed) | Up to 192 kHz / 24-bit stereo, or 96 kHz / 24-bit 5.1 |
| **MLP** (Meridian Lossless Packing) | Lossless compression (~1.5:1 ratio). Required when LPCM would exceed the 9.6 Mbps bitrate ceiling. Up to 192 kHz / 24-bit stereo, 96 kHz / 24-bit 6ch |

Dolby Digital / DTS may appear as DVD-Video-zone VOB content in bonus groups but
are not native to the AOB audio stream.

### 1.6 MLP Details

- **Relationship to TrueHD**: Dolby TrueHD (Blu-ray) is an extension of MLP. Same
  underlying bitstream, but TrueHD adds higher bitrates and Atmos metadata.
- **ffmpeg decoder**: Codec ID `mlp` (`AV_CODEC_ID_MLP`). Shares internals with
  `truehd` decoder in `libavcodec/mlp*.c`. Fully functional decoder.
- **Substreams**: MLP can carry multiple substreams — substream 0 is typically a
  2-channel downmix, allowing simple decoders to get stereo without decoding the
  full multichannel stream.

---

## 2. IFO File Structure

### 2.1 ATS_XX_0.IFO Contents

IFO files contain **structural data only** — no text metadata. Specifically:

- Track count and durations (PTS timestamps, 1/90000th second resolution)
- Sector pointers to track boundaries within AOBs
- Audio attributes per title set (all titles within an ATS share the same attributes):
  - Codec type (PCM or MLP/PPCM)
  - Sample rate (44.1, 48, 88.2, 96, 176.4, 192 kHz)
  - Bit depth / quantization word length (16, 20, 24)
  - Channel count and channel assignment (21 possible layouts)
- Group-to-track assignment mappings
- Downmix coefficient tables

### 2.2 No Text Metadata

**IFO files never contain artist/album/track title text.** Some discs carry "Real
Time Text" within the AOB stream, but this is rare and inconsistent. Text metadata
must come from external sources:

- Sidecar files (analogous to SACD XML sidecars)
- MusicBrainz lookup (future feature)
- Filename/directory naming heuristics
- User-provided metadata

### 2.3 IFO files are never encrypted

Even on CPPM-protected discs, IFO files are readable. Only AOB audio data is
encrypted.

---

## 3. CPPM Protection

### 3.1 What It Is

CPPM (Content Protection for Prerecorded Media) encrypts AOB data. When present:
- `DVDAUDIO.MKB` (Media Key Block) appears in AUDIO_TS
- Decryption requires a device key to derive a media key, then a title key per ATS

### 3.2 Open Source Status

- Cracked circa 2005
- Key implementation: `libdvdcpxm` (from DVD-Audio Explorer codebase)
- `DVDAuth` (github.com/saramibreak/DVDAuth) wraps libdvdcpxm
- Legal status varies by jurisdiction (analogous to CSS/libdvdcss)
- No Rust implementation exists

### 3.3 Recommended Approach

For the initial implementation: **detect CPPM and return `MaterializeError::Encrypted`**.
This matches the SACD materializer's approach to encrypted discs. Users with
encrypted ISOs can pre-decrypt with external tools (DVD-Audio Explorer, DVD Audio
Extractor). CPPM decryption integration can be evaluated later.

---

## 4. Existing Tools & Crates

### 4.1 Extraction Tools

| Tool | Language | Capabilities |
|------|----------|-------------|
| **DVD-Audio Explorer** (dvdaexplorer) | C | IFO parsing, CPPM decryption, AOB demuxing, MLP decode, track extraction. Most complete OSS solution |
| **DVD Audio Extractor** | Proprietary | Commercial. Reliable track splitting to FLAC/WAV |
| **foobar2000 + foo_input_dvda** | C++ | Plays/rips DVD-Audio discs and ISOs |
| **atsifodump** | C | Dumps IFO structure, outputs CUE sheets. Good reference for IFO parsing (github.com/whitslack/atsifodump) |
| **dvda-author** | C | Authoring tool, can import/extract AUDIO_TS. GPL |

### 4.2 ffmpeg AOB Handling

**ffmpeg has no dedicated DVD-Audio demuxer.** AOB files can be read as raw MPEG-PS
(`-f mpeg`), but without IFO parsing there are no track boundaries — ffmpeg treats
the entire AOB stream as one continuous output. The DVD-Video demuxer
(`dvdvideo` / libdvdnav) does not handle AUDIO_TS.

**Practical approach**: Parse IFO ourselves for track structure, then use ffmpeg to
decode specific byte ranges of the AOB stream.

### 4.3 Rust Crates

| Crate | Purpose | UDF Support | Status |
|-------|---------|-------------|--------|
| **isomage** (v2.1.0) | ISO 9660 + UDF reader | Yes (ECMA-167) | Active (May 2026). Pure Rust, no unsafe. Best candidate (**unverified — needs hands-on evaluation before committing**) |
| **cdfs** (v0.2.3) | ISO 9660 reader | No | Stale (Oct 2023) |
| **iso9660** (v0.1.1) | ISO 9660 reader | No | Minimal/WIP |

**No Rust crates exist for DVD-Audio IFO parsing or AOB demuxing.** We need to
write our own.

---

## 5. Integration Points (Existing Pipeline)

### 5.1 Materializer Trait

```rust
#[async_trait]
pub trait Materializer: Send + Sync {
    async fn materialize(
        &self,
        req: &PipelineRequest,
        staging: &StagingDir,
        runner: &dyn ToolRunner,
        reporter: Option<&dyn PipelineReporter>,
        tool_paths: &HashMap<String, PathBuf>,
        cancel: &CancellationToken,
    ) -> Result<PreparedSource, MaterializeError>;
}
```

The materializer parses the container structure and returns `PreparedSource` with
`PreparedTrack` entries. It does **not** decode audio — that happens later in
`realize_track()`.

### 5.2 Types That Need New Variants

**`SourceKind` enum** (types.rs):
```rust
pub enum SourceKind {
    SingleFile,
    SevenZip,
    CueImage,
    SacdIso,
    DvdAudio,   // <-- new
}
```

**`TrackSourceRef` enum** (types.rs):
```rust
TrackSourceRef::DvdaTrack {
    /// Path to the ISO image (or AUDIO_TS directory, future)
    source: PathBuf,
    /// Title set number (1-based, ATS_XX)
    title_set: u8,
    /// Track index within the title set (0-based)
    track_index: u32,
    // NOTE: `group` may be unnecessary here. The materializer resolves
    // group → title_set + track_index. The realize step only needs
    // title_set and track_index to locate the track in the IFO. The
    // SACD pattern stores `area` because realize re-parses the ISO —
    // `title_set` serves that same role here. Flagged for reasoning
    // model review.
},
```

**`SourceOptions` struct** (types.rs):
```rust
pub struct SourceOptions {
    pub archive_password: Option<SecretString>,
    pub sacd_area: Option<SacdArea>,
    pub dvda_group: Option<u8>,  // <-- new: 1-based group number
    pub cue_sidecar: CueSidecarPolicy,
    pub track_selection: TrackSelection,
}
```

### 5.3 Dispatch Chain Additions

**`detect_source_kind()`** in stages.rs — add DVD-Audio detection before the
generic `.iso` → SevenZip fallback:

```rust
if is_dvda_candidate(req)? {
    return Ok(SourceKind::DvdAudio);
}
```

**`materializer_for()`** in stages.rs:

```rust
SourceKind::DvdAudio => Ok(Box::new(DvdaMaterializer)),
```

**`realize_track()`** in stages.rs — new match arm:

```rust
TrackSourceRef::DvdaTrack { source, title_set, track_index }
    => realize_dvda_track(source, *title_set, *track_index, staging, runner, cancel).await,
```

The realize step re-parses the IFO from the ISO (same pattern as SACD's
`realize_sacd_track_blocking`) to get PTS boundaries and audio attributes for the
requested track.

---

## 6. Proposed Architecture

### 6.1 New Module: `dvda` (IFO/AOB parser)

Location: `src/tui/dvda/` or `src/dvda/` (paralleling `src/tui/sacd/`).

This is a pure-Rust, in-process parser for DVD-Audio disc structure. It reads IFO
binary data and produces a typed representation of the disc.

#### Key Types

```rust
/// Parsed DVD-Audio disc structure
pub struct DvdaDisc {
    pub title_sets: Vec<TitleSet>,
    pub groups: Vec<AudioGroup>,
}

/// One Audio Title Set (ATS_XX)
pub struct TitleSet {
    pub number: u8,                  // 1-based
    pub audio_attributes: AudioAttributes,
    pub tracks: Vec<DvdaTrackEntry>,
}

/// Audio attributes from IFO
pub struct AudioAttributes {
    pub codec: AudioCodec,           // Pcm or Mlp
    pub sample_rate: u32,            // Hz
    pub bit_depth: u32,              // 16, 20, or 24
    pub channels: u8,                // 1-6
    pub channel_assignment: u8,      // DVD-A layout code (0-20)
}

pub enum AudioCodec {
    Pcm,
    Mlp,
}

/// One track within a title set
pub struct DvdaTrackEntry {
    pub index: u32,                  // 0-based
    pub first_sector: u64,           // sector offset in AOB stream
    pub last_sector: u64,
    pub pts_start: u64,              // PTS timestamp (1/90000 s)
    pub pts_end: u64,
    pub duration_seconds: f64,       // derived from PTS
}

/// Audio group (playlist of tracks)
/// NOTE: This assumes a group draws from a single title set. Need to verify
/// whether a group can span multiple ATS's on real discs — the spec allows
/// groups to reference multiple titles, but all titles in a group likely share
/// the same ATS since they need compatible audio attributes.
pub struct AudioGroup {
    pub number: u8,                  // 1-based
    pub title_set: u8,               // which ATS this group draws from
    pub track_indices: Vec<u32>,     // indices into the title set's tracks
}
```

#### Parsing Strategy

1. **AUDIO_TS.IFO**: Parse the AMG header to get the number of Audio Title Sets
2. **ATS_XX_0.IFO**: Parse each ATSI to extract:
   - Audio attributes (codec, sample rate, bit depth, channels)
   - Track PTS timestamps and sector ranges
   - Group-to-track mappings

Reference implementations for IFO parsing:
- `atsifodump` (C, github.com/whitslack/atsifodump) — clean, well-documented
- `dvdaexplorer` (C) — comprehensive but complex
- DVD-Audio spec documentation at dvd-audio.sourceforge.io/spec/ats_ifo.shtml

### 6.2 New Module: `materializer_dvda.rs`

Location: `src/convert/pipeline/materializer_dvda.rs`

#### Detection: `is_dvda_candidate()`

```rust
pub(crate) fn is_dvda_candidate(req: &PipelineRequest) -> Result<bool, SourceDetectError> {
    // Only .iso files (for now)
    if !has_extension(&req.container, "iso") {
        return Ok(false);
    }
    // Check for explicit dvda_group request
    if req.source.dvda_group.is_some() {
        return Ok(true);
    }
    // Probe ISO for AUDIO_TS directory
    probe_iso_for_audio_ts(&req.container)
}
```

The probe function uses `isomage` to open the ISO and check for `AUDIO_TS/ATS_01_0.IFO`.
This must run *after* SACD detection (which is a fast magic-byte check) but *before*
the generic `.iso` → SevenZip fallback.

**Detection ordering** in `detect_source_kind()`:
1. SACD ISO (checks for SACD magic bytes — fast, doesn't need filesystem parse)
2. DVD-Audio ISO (checks for AUDIO_TS directory via `isomage` — heavier, requires filesystem parse)
3. CUE image
4. 7z/archive (generic .iso fallback)
5. Single audio file

#### Materialization Flow

```
materialize()
  1. Open ISO with isomage
  2. Check for DVDAUDIO.MKB → return MaterializeError::Encrypted if present
  3. Read AUDIO_TS/AUDIO_TS.IFO → get title set count
  4. For each title set: read ATS_XX_0.IFO → parse track structure
  5. Select group (default: group 1, or req.source.dvda_group)
  6. Build PreparedTrack entries with TrackSourceRef::DvdaTrack
  7. Apply track selection filter
  8. Derive album metadata (empty titles — IFO has no text)
  9. Return PreparedSource
```

### 6.3 Track Realization: `realize_dvda_track()`

Location: added to `stages.rs` (paralleling `realize_sacd_track`)

This is where audio is actually extracted from AOBs. Two codec paths:

#### LPCM Path
LPCM audio in AOBs is raw PCM packed into MPEG-PS Private Stream 1 packets. The
packing uses a DVD-Audio-specific header within each PES packet that specifies
sample format. Two options:

**Option A — ffmpeg subprocess**:
```
ffmpeg -f mpeg -i concat:ATS_01_1.AOB|ATS_01_2.AOB \
       -ss <start_time> -t <duration> \
       -c:a pcm_s32le -f wav output.wav
```
ffmpeg has no dedicated DVD-Audio demuxer, but `-f mpeg` treats AOBs as raw
MPEG-PS. This works for decoding but PTS-based seeking (`-ss`) is not
sample-accurate — see "Seeking precision" below.

**Option B — In-process MPEG-PS demuxing + PCM extraction** (more robust):
Parse MPEG-PS pack headers, extract Private Stream 1 payloads, strip the
DVD-Audio-specific audio frame headers, and write raw PCM to a WAV file. This
gives us precise sector-level control for track splitting and avoids the seeking
precision problem entirely.

#### MLP Path
MLP packets in AOBs are also in Private Stream 1. Options:

**Option A — ffmpeg subprocess** (recommended):
```
ffmpeg -f mpeg -i concat:ATS_01_1.AOB|ATS_01_2.AOB \
       -ss <start_pts> -t <duration> \
       -c:a pcm_s32le output.wav
```
Use ffmpeg's MLP decoder to transcode to PCM WAV. The start/duration come from
IFO-parsed PTS timestamps.

**Option B — ffmpeg-next in-process** (more integrated):
Use the project's existing `ffmpeg-next` bindings to open the AOB stream, seek to
the right PTS, decode MLP frames, and write PCM. This avoids subprocess overhead
and gives tighter PTS control.

#### Recommended Realization Strategy

**Phase 1 (initial)**: Use ffmpeg subprocess to decode AOBs to WAV with track
splitting via `-ss`/`-t` from IFO-derived PTS timestamps. How the AOB data reaches
ffmpeg depends on the ISO access strategy (open question #1 in section 7):

- If extracting to staging: ffmpeg reads AOB files directly from disk
- If streaming from ISO: extract AOBs to staging first, then feed to ffmpeg

```
realize_dvda_track():
  1. Ensure AOB files are accessible (extract from ISO to staging if needed)
  2. Re-parse ATS_XX_0.IFO to get track PTS boundaries and audio attributes
  3. Concatenate AOB parts: ATS_XX_1.AOB + ATS_XX_2.AOB + ...
  4. ffmpeg -f mpeg -i concat:<aobs> -ss <start> -t <dur> -c:a pcm_s32le out.wav
  5. Return path to decoded WAV
```

#### Seeking Precision Concern

ffmpeg's MPEG-PS demuxer performs PTS-based seeking, which is **not sample-accurate**.
When splitting tracks via `-ss`/`-t`, boundaries may land on the nearest keyframe
or PES packet boundary rather than the exact PTS. For lossless extraction this is a
real problem: tracks could have extra or missing samples at boundaries.

Mitigations:
- Validate output sample counts against IFO-derived durations
- Over-extract (pad start/end), then trim to exact sample boundaries in a second pass
- Fall back to in-process sector-level extraction for precise splits

This is the strongest argument for eventually moving to in-process MPEG-PS demuxing,
especially for LPCM where no codec step is needed.

**Phase 2 (future)**: In-process MPEG-PS demuxing for precise sector-level
extraction without ffmpeg subprocess overhead. Particularly beneficial for LPCM
where no codec is needed.

### 6.4 Sidecar / Metadata Strategy

Since IFO files contain no text metadata, we need external metadata sources.
Proposed priority cascade (analogous to SACD sidecar):

1. **Sidecar XML/JSON**: Look for a `<iso_stem>.dvda.xml` or similar file alongside
   the ISO, containing track titles, artist, album. Format TBD — could reuse the
   SACD sidecar XML format or define a simpler one.
2. **Directory name parsing**: Extract artist/album from the parent directory name
   using existing heuristics (e.g., "Artist - Album (Year) [ISO]" patterns).
3. **Fallback**: Track metadata left empty; user applies via naming templates or
   post-conversion tagging.

---

## 7. Open Questions for Reasoning Model Review

### 7.1 Architecture

1. **ISO access strategy**: Should we use `isomage` to read files directly from the
   ISO in-process, or extract AUDIO_TS to staging first (simpler, uses disk space)?
   The SACD materializer reads directly from ISO via `sacd-rs::IsoReader`. The 7z
   materializer extracts to staging. DVD-Audio AOBs can be large (up to 9 GB for
   multi-part AOBs), so extraction to staging may not be ideal.

2. **IFO parser scope**: Should the IFO parser live in a separate crate (like
   `sacd-rs`) or as a module within tonepoet? Given the complexity is moderate
   (ATS IFO is typically 4096 bytes / 2 sectors), an internal module seems
   appropriate unless there's a reuse argument.

3. **AUDIO_PP.IFO vs ATS_XX_0.IFO**: AUDIO_PP.IFO (Simple Audio Manager) provides
   a flat, CD-like track list with absolute sector pointers. ATS_XX_0.IFO provides
   the full hierarchical structure. Should we parse both, or just ATS IFOs?
   AUDIO_PP.IFO might be simpler for basic track extraction but loses group
   structure.

### 7.2 Track Realization

4. **ffmpeg subprocess vs in-process for MLP**: ffmpeg-next is already a dependency
   and used for probing. Using it in-process for MLP decoding avoids subprocess
   overhead and gives tighter PTS control. But the AOB MPEG-PS container isn't well
   supported by ffmpeg's demuxers. Is it worth writing a custom demuxer adapter, or
   is `ffmpeg -f mpeg -i` good enough with PTS-based seeking?

5. **LPCM extraction**: For LPCM AOBs, the audio is uncompressed PCM in a known
   packing format. In-process extraction (demux MPEG-PS, strip headers, write WAV)
   would be more efficient than ffmpeg subprocess, and the MPEG-PS demuxing is
   straightforward. Should we implement in-process LPCM from the start?

6. **AOB concatenation**: When an ATS spans multiple AOB files (ATS_01_1.AOB through
   ATS_01_9.AOB), we need to treat them as one continuous stream. ffmpeg supports
   `concat:` protocol for this. For in-process reading, we'd need a concatenating
   reader. Is there a preference?

### 7.3 Detection & Dispatch

7. **Detection ordering**: The current `detect_source_kind()` checks SACD first
   (fast magic-byte check), then CUE, then archive extensions. DVD-Audio detection
   requires opening the ISO and checking for AUDIO_TS, which is heavier than SACD's
   magic-byte check. Proposed order: SACD (magic) → DVD-A (filesystem probe) → CUE
   → archive → single file. Is this reasonable?

8. **Should `.iso` require explicit `--dvda-group` flag?** An alternative to
   auto-probing every ISO for AUDIO_TS is to require the user to pass `--dvda-group`
   (or `--dvda`) to trigger DVD-Audio handling. This avoids the overhead of ISO
   filesystem probing for non-DVD-A ISOs. The SACD materializer has a similar
   pattern: explicit `--area` triggers SACD even without magic bytes.

### 7.4 Metadata & UX

9. **Group selection UX**: DVD-Audio groups map roughly to SACD areas (stereo vs
   multichannel). Should we mirror the `--area stereo|multichannel` pattern, or
   expose raw group numbers? Groups can contain arbitrary content (not just
   stereo/multichannel), so raw numbers with a `--dvda-list-groups` probe command
   might be more appropriate.

10. **Sidecar format**: Should we define a new sidecar format for DVD-Audio, reuse
    the SACD sidecar XML format, or use a more generic format (e.g., CUE sheet,
    MusicBrainz disc ID lookup)?

---

## 8. Implementation Phases

### Phase 1: IFO Parser + Detection (no audio extraction yet)

**Deliverables:**
- `src/tui/dvda/mod.rs` — DVD-Audio disc structure types
- `src/tui/dvda/ifo_parser.rs` — ATS_XX_0.IFO and AUDIO_TS.IFO binary parser
- `src/tui/dvda/detect.rs` — ISO probing for AUDIO_TS presence
- Unit tests with binary IFO fixtures

**Dependencies:** `isomage` crate for ISO9660/UDF filesystem access

### Phase 2: Materializer (structure only, no audio decode)

**Deliverables:**
- `src/convert/pipeline/materializer_dvda.rs` — `DvdaMaterializer` struct
- New `SourceKind::DvdAudio` variant
- New `TrackSourceRef::DvdaTrack` variant
- `dvda_group` field on `SourceOptions`
- CPPM detection (`DVDAUDIO.MKB` check → `MaterializeError::Encrypted`)
- Detection and dispatch wiring in `stages.rs`
- Integration tests

### Phase 3: Track Realization (audio extraction)

**Deliverables:**
- `realize_dvda_track()` in `stages.rs`
- AOB extraction from ISO to staging
- ffmpeg-based MLP → PCM decode (subprocess)
- ffmpeg-based LPCM → PCM decode (subprocess)
- Track splitting via PTS timestamps
- End-to-end tests with real DVD-Audio ISOs

### Phase 4: Polish & Metadata

**Deliverables:**
- Sidecar metadata support
- CLI flags: `--dvda-group`, `--dvda` (force DVD-Audio mode)
- TUI integration (group selection in source pane)
- Documentation

### Future Phases (not in initial scope):
- In-process LPCM extraction (skip ffmpeg for PCM content)
- In-process MLP decode via ffmpeg-next bindings
- CPPM decryption integration
- MusicBrainz disc ID lookup
- AUDIO_TS directory support (not just ISOs)

---

## 9. Risk Assessment

| Risk | Severity | Mitigation |
|------|----------|------------|
| IFO format edge cases across disc manufacturers | Medium | Reference `atsifodump` + `dvdaexplorer` source; test against diverse ISOs |
| ffmpeg MPEG-PS seeking imprecision with PTS | Medium | Validate output sample counts against IFO durations; fall back to over-extraction + trim |
| `isomage` crate bugs with specific DVD ISO layouts | Low-Medium | UDF bridge disc variants may need testing; fallback to 7z extraction of AUDIO_TS |
| CPPM-encrypted discs silently corrupt | Low | Detect DVDAUDIO.MKB presence early in materialization |
| Large AOB files (up to 9 GB) staging overhead | Medium | Read directly from ISO via `isomage` streaming rather than extracting to staging |
| No text metadata available | Low | Expected limitation; sidecar system addresses this |

---

## 10. Test Corpus

Available DVD-Audio ISOs at `/mnt/scratch/dev/dawdiolab/test-isos/`:

| Source | Format | Notes |
|--------|--------|-------|
| `HDAD2009.ISO` (1.7G) | DVD-A ISO | Multi-artist HD Audio sampler — good for multi-group testing |
| `Miles_Davis_Kind_of_Blue/` | DVD-A (extracted dir) | Classic stereo album. **Requires future AUDIO_TS directory support** |
| `Miles_Davis_The_Man_with_the_Horn/` | DVD-A (extracted dir) | **Requires future AUDIO_TS directory support** |
| `MGLETSGETITON (DVD-A)/` | DVD-A (extracted dir) | **Requires future AUDIO_TS directory support** |
| `Neil Young - Hawks & Doves/` | DVD-A ISO (24/176.4) | High sample rate — tests 176.4 kHz path |
| `Neil Young & Crazy Horse - Live at Fillmore East/` | DVD-A ISO (LPCM 24/96) | LPCM codec path |
| `Talking Heads - Talking Heads 77/` | DVD-A ISO (Multichannel & Stereo 24/96) | Tests multi-group (stereo + multichannel) |

Not DVD-Audio (exclude from DVD-A testing):
- `Yes - Close To The Edge.iso` (22G) — likely Blu-ray
- `PRINCEANDTHEREVOLUTIONLIVE.iso` (38G) — Blu-ray
- Chicago directories — DTS / Blu-ray Pure Audio

---

## 11. Reference Materials

- DVD-Audio specification docs: https://dvd-audio.sourceforge.io/spec/
  - ATS IFO structure: https://dvd-audio.sourceforge.io/spec/ats_ifo.shtml
  - AOB format: https://dvd-audio.sourceforge.io/spec/aob.shtml
  - AUDIO_TS.IFO: https://dvd-audio.sourceforge.io/spec/audio_ts.shtml
- `atsifodump` source (IFO parser reference): https://github.com/whitslack/atsifodump
- `dvdaexplorer` source: https://offog.org/git/dvdaexplorer
- `DVDAuth` (CPPM): https://github.com/saramibreak/DVDAuth
- `isomage` crate: https://lib.rs/crates/isomage
- ffmpeg MLP decoder: `libavcodec/mlp*.c`
- Hydrogenaudio MLP wiki: https://wiki.hydrogenaudio.org/index.php?title=Meridian_Lossless_Packing
