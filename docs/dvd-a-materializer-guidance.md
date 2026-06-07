# Revised DVD-Audio Materializer Guidance

## Executive guidance

The DVD-Audio materializer should follow the SACD materializer pattern: parse disc structure in-process, represent selected tracks as typed source references, and realize each track from the original source using deterministic range reads.

The foo_input_dvda findings change the extraction design. Track realization should be sector-range based, not FFmpeg time-seek based. The IFO/SAMG structures define track or chapter boundaries as first/last DVD sectors. The materializer should read those sectors from the logical AOB stream, demux Private Stream 1 packets in Rust, unpack LPCM in Rust, and invoke FFmpeg only for MLP decode.

The initial implementation should not treat FFmpeg as the DVD-A demuxer. FFmpeg can decode MLP, but tonepoet should own DVD-A navigation, sector selection, AOB concatenation, PES extraction, PCM unpacking, and output validation.

## Core decisions

### 1. Write the IFO/AOB parser in Rust

Use foo_input_dvda, atsifodump, and the DVD-Audio structure notes as references, but do not shell out to atsifodump or depend on ffprobe for runtime behavior.

Reasons:

* The project already has an SACD materializer that parses disc structure in-process.
* DVD-Audio track boundaries live in IFO/SAMG sector tables, not in the MPEG-PS stream.
* atsifodump is useful as a fixture oracle and regression comparator, but a runtime shell dependency would add platform and output-format risk.
* ffprobe/FFmpeg can identify media streams, but it does not provide DVD-Audio group/title/chapter navigation.

### 2. Treat sector ranges as the source of truth for splitting

Drop the FFmpeg `-ss` / `-t` splitting plan. Track realization should start from IFO/SAMG sector ranges.

Preferred realization flow:

```text
IFO/SAMG track or chapter entry
  -> title set + first_sector + last_sector
  -> logical AOB sector reader
  -> MPEG-PS pack/PES parser
  -> Private Stream 1 payload extraction
  -> PCM unpack or MLP decode
  -> WAV/PCM output
  -> sample-count and duration validation
```

PTS values should remain in the model, but use them for duration metadata and validation. Do not use PTS as the primary split mechanism.

### 3. Implement MPEG-PS/AOB demuxing in-process from the start

AOB demuxing is small enough to own directly. The reader should process fixed 2048-byte DVD sectors, recognize MPEG-PS pack headers, extract Private Stream 1 PES packets, and expose the DVD-Audio substream payload.

Initial types should look roughly like this:

```rust
pub struct AobSectorRange {
    pub title_set: u8,
    pub first_sector: u64,
    pub last_sector: u64,
}

pub struct AobPacket {
    pub sector: u64,
    pub stream_id: u8,      // 0xa0 = PCM, 0xa1 = MLP
    pub pts: Option<u64>,
    pub dts: Option<u64>,
    pub payload: Vec<u8>,
}
```

The AOB reader should treat `ATS_XX_1.AOB` through `ATS_XX_9.AOB` as one logical stream. Sector pointers should resolve against that concatenated stream.

### 4. Implement LPCM extraction in Rust from day one

Do not invoke FFmpeg for LPCM unless temporarily needed for bring-up comparison.

The PCM path should:

* Demux AOB sectors in Rust.
* Extract Private Stream 1 payloads.
* Validate substream ID `0xa0`.
* Parse the DVD-Audio LPCM header.
* Handle group 1 / group 2 channel layouts.
* Handle big-endian sample packing.
* Support 16-bit, 20-bit, and 24-bit PCM.
* Support group-specific sample rates and bit depths where present.
* Write WAV/PCM output directly.
* Validate output sample count against sector payload and PTS-derived duration.

This path gives the project a deterministic baseline and avoids routing simple byte-unpacking through a media framework.

### 5. Use FFmpeg only for MLP decode in the first implementation

MLP still needs a decoder. Start with FFmpeg for that codec, but do not ask FFmpeg to seek through full AOB streams.

Preferred MLP flow:

```text
Rust IFO parser
  -> Rust sector-range reader
  -> Rust AOB/PES extraction
  -> selected MLP payload/range
  -> FFmpeg MLP decode
  -> PCM output
  -> validation
```

Two viable initial bridges:

1. Write a bounded temporary stream for the selected track/chapter and invoke FFmpeg on that file.
2. Pipe selected MLP packets or frames into FFmpeg.

The first option is easier to debug. The second option cuts staging I/O once the packet iterator is stable.

Do not start with ffmpeg-next unless subprocess behavior becomes a blocker. Once tonepoet has an `AobPacket` or `MlpFrame` iterator, an in-process FFmpeg binding becomes easier because FFmpeg no longer has to understand DVD-Audio navigation.

## IFO parsing guidance

Parse all three navigation sources:

### AUDIO_TS.IFO / AMG

Use this for disc-level structure, group navigation, title-set references, and top-level validation.

### ATS_XX_0.IFO / ATSI

Use this for title-set internals:

* Audio title data.
* Chapter/program records.
* Timestamps.
* Relative first/last sector pointers.
* Audio attributes.
* Channel assignment and channel-group metadata.

Do not collapse the model to `TitleSet { tracks: Vec<_> }`. Commercial authoring can represent logical songs as chapters or programs within larger title structures, especially for gapless content.

### AUDIO_PP.IFO / SAMG

Parse this earlier than originally planned. It provides a flat, CD-like chapter/track table with absolute sector pointers. Use it as:

* A fast simple-TOC path.
* A cross-check against AMG/ATSI interpretation.
* A fallback for discs where hierarchy parsing hits an authoring oddity.
* A useful data source for group/chapter listing.

Do not use SAMG as the only source of truth because it loses hierarchy and may not capture every UX requirement. The materializer still needs group/title/chapter structure for selection, diagnostics, and future TUI behavior.

## Data model changes

The proposed `TitleSet { tracks }` and `AudioGroup { title_set, track_indices }` model is too flat. Replace it with a model that can express groups, titles, chapters/programs, and sector ranges independently.

Recommended shape:

```rust
pub struct DvdaDisc {
    pub groups: Vec<DvdaGroup>,
    pub title_sets: Vec<TitleSet>,
}

pub struct DvdaGroup {
    pub number: u8,
    pub entries: Vec<GroupEntry>,
}

pub struct GroupEntry {
    pub title_set: u8,
    pub title: u16,
    pub chapter_or_program: u16,
    pub logical_track_number: u16,
}

pub struct TitleSet {
    pub number: u8,
    pub titles: Vec<AudioTitle>,
    pub aob_parts: Vec<AobPart>,
}

pub struct AudioTitle {
    pub title_number: u16,
    pub audio_attributes: AudioAttributes,
    pub chapters: Vec<AudioChapter>,
}

pub struct AudioChapter {
    pub number: u16,
    pub first_sector: u64,
    pub last_sector: u64,
    pub pts_start: u64,
    pub pts_len: u64,
}
```

The `AudioAttributes` type should support channel groups instead of assuming one sample rate and bit depth for the whole stream:

```rust
pub struct AudioAttributes {
    pub codec: AudioCodec,
    pub channel_assignment: u8,
    pub channel_groups: Vec<ChannelGroupAttributes>,
}

pub struct ChannelGroupAttributes {
    pub channels: Vec<DvdaChannel>,
    pub sample_rate: u32,
    pub bit_depth: u8,
}
```

## TrackSourceRef guidance

The proposed `TrackSourceRef::DvdaTrack { source, title_set, track_index }` is not enough. Store enough identity and validation data to re-find the same logical item after reparsing.

Recommended shape:

```rust
TrackSourceRef::DvdaTrack {
    source: PathBuf,
    group: u8,
    title_set: u8,
    title: u16,
    chapter_or_program: u16,
    first_sector: u64,
    last_sector: u64,
    pts_start: u64,
    pts_len: u64,
    attrs_fingerprint: DvdaAudioAttrsFingerprint,
}
```

The realization step can still re-parse the IFO, but the stored reference should catch parser drift, authoring oddities, and mismatched track selections.

## ISO and directory access strategy

Use a `DvdaVolume` abstraction instead of binding the parser directly to ISO access.

Recommended trait shape:

```rust
pub trait DvdaVolume {
    fn read_file(&self, path: &str) -> Result<Vec<u8>>;
    fn open_file(&self, path: &str) -> Result<Box<dyn ReadAt + Send + Sync>>;
    fn exists(&self, path: &str) -> bool;
    fn list_audio_ts(&self) -> Result<Vec<DvdaDirEntry>>;
}
```

Implement at least:

* `IsoDvdaVolume` for ISO/UDF images.
* `DirectoryDvdaVolume` for extracted `AUDIO_TS` directories.

Directory support should not wait for a late future phase. Many decrypted DVD-Audio workflows produce folders, and the available test corpus already includes extracted DVD-Audio directories.

For MLP subprocess decoding, stage only bounded selected ranges or selected title-set AOBs. Do not extract full `AUDIO_TS` by default.

## Detection guidance

Detection order should remain:

1. SACD ISO detection.
2. DVD-Audio detection.
3. CUE.
4. Archive fallback.
5. Single audio file.

DVD-Audio detection should check for `AUDIO_TS` and known IFO magic values:

* `DVDAUDIO-AMG` for `AUDIO_TS.IFO`.
* `DVDAUDIO-ATS` for `ATS_XX_0.IFO`.
* `DVDAUDIOSAPP` or equivalent SAMG signature for `AUDIO_PP.IFO`, after verifying against fixtures.

Keep `--dvda` as a force mode for odd images. Do not require it for normal ISO detection.

## CPPM guidance

Default behavior should remain: detect likely CPPM protection and fail with an encrypted-source error before attempting extraction.

Add an override such as:

```text
--dvda-assume-decrypted
```

Some user-supplied decrypted folders or rebuilt ISOs may still contain `DVDAUDIO.MKB`. The override should allow advanced users to try extraction, while normal behavior protects users from confusing encrypted payload failures.

## Revised implementation phases

### Phase 0: corpus characterization

Before implementation, build a small diagnostic harness that runs against the available DVD-Audio corpus.

Deliver:

* `AUDIO_TS` file listing.
* IFO sizes and magic bytes.
* Parsed group/title/chapter counts where available.
* `atsifodump` or foo_input_dvda comparison output.
* Expected codec, channel layout, sample rate, bit depth, and duration for each test source.
* Known-good output sample counts from a trusted extractor when available.

This phase creates golden fixtures and prevents the parser model from drifting away from real commercial discs.

### Phase 1: volume reader + navigation parsers

Deliver:

* `DvdaVolume` abstraction.
* ISO volume reader.
* Extracted directory volume reader.
* `AUDIO_TS.IFO` parser.
* `ATS_XX_0.IFO` parser.
* Minimal `AUDIO_PP.IFO` parser.
* Typed group/title/chapter/sector model.
* CPPM detection.
* Group/chapter listing command.
* Unit tests with binary IFO fixtures.
* Fixture comparisons against foo_input_dvda and/or atsifodump.

### Phase 2: materializer structure

Deliver:

* `SourceKind::DvdAudio`.
* Rich `TrackSourceRef::DvdaTrack`.
* `dvda_group` source option.
* DVD-Audio detection and dispatch.
* `DvdaMaterializer`.
* Prepared tracks with blank or sidecar-derived metadata.
* Track selection support.
* Integration tests that materialize structure without decoding audio.

### Phase 3: AOB reader + LPCM realization

Deliver:

* Logical AOB concatenating reader.
* 2048-byte sector reader.
* MPEG-PS pack parser.
* Private Stream 1 PES parser.
* `0xa0` PCM and `0xa1` MLP substream validation.
* DVD-Audio LPCM unpacker.
* WAV writer.
* End-to-end LPCM extraction tests.
* Sample-count and duration validation.

This phase should produce working output for LPCM DVD-Audio discs without FFmpeg.

### Phase 4: MLP realization

Deliver:

* Sector-range based MLP extraction.
* Bounded temporary stream or pipe bridge to FFmpeg.
* FFmpeg MLP decode to PCM/WAV.
* Output validation against parsed timing and expected sample count.
* Trim pass only if real test output shows leading or trailing decoded samples.
* End-to-end MLP extraction tests.

### Phase 5: UX, metadata, and hardening

Deliver:

* Sidecar metadata support.
* `--dvda-group`.
* `--dvda-list-groups`.
* `--dvda`.
* `--dvda-assume-decrypted`.
* TUI group picker.
* Better diagnostics when AMG/ATSI/SAMG disagree.
* Directory input polish.
* Optional ffmpeg-next MLP path if subprocess behavior becomes a real limitation.

## Main changes from the original brief

Replace the original track realization plan:

```text
ffmpeg -f mpeg -i concat:ATS_01_1.AOB|ATS_01_2.AOB \
       -ss <start_time> -t <duration> \
       -c:a pcm_s32le output.wav
```

with:

```text
read IFO/SAMG sector range
  -> read exact sectors from logical AOB stream
  -> parse MPEG-PS packs and PES packets in Rust
  -> validate substream ID
  -> unpack LPCM in Rust or pass MLP payload to FFmpeg
  -> write WAV/PCM
  -> validate sample count and duration
```

The original “seeking concern” should become a short note explaining that tonepoet avoids FFmpeg time seeking entirely for DVD-Audio. Sector ranges are the split authority. PTS is validation and metadata.

## Bottom line

The materializer should own DVD-Audio navigation and AOB demuxing from day one. FFmpeg should not drive track splitting, and it should not handle LPCM. The correct split point is the IFO/SAMG sector range. The correct initial architecture is:

```text
Rust IFO/SAMG parser
  + Rust logical AOB sector reader
  + Rust MPEG-PS/PES extractor
  + Rust LPCM unpacker
  + FFmpeg only for MLP decode
```

This design better matches the existing SACD materializer pattern and gives tonepoet deterministic behavior across commercial DVD-Audio authoring variants.
