# DVD-Audio Materializer — Phase 3 Implementation Brief

## What I need from you

Generate a downloadable code bundle implementing Phase 3: AOB sector
reading, MPEG-PS demuxing, and MLP extraction via ffmpeg. This combines
the original guidance doc's Phases 3 and 4 because all 7 test discs use
MLP — no LPCM discs exist in the corpus. LPCM unpacking can be added
later as spec coverage if an LPCM disc is found.

Phase 3 replaces the `realize_track` stub (`ConvertError::UnsupportedTrackSource`)
with working audio extraction that produces a PCM WAV file from DVD-Audio
MLP content.

Emit compilable Rust code, not a plan.

---

## What Phase 2 produced

`TrackSourceRef::DvdaTrack` carries everything Phase 3 needs:

```rust
DvdaTrack {
    volume_source: DvdaVolumeSourceRef,  // Directory{root} or Iso{path,backend}
    sector_address_space: DvdaSectorAddressSpace, // AtsAobRelative or SamgAbsolute
    first_pts: u32,          // track start in 90kHz PTS ticks
    len_in_pts: u32,         // track duration in PTS ticks
    track_type: Option<u8>,  // raw IFO byte
    index_start: Option<u8>,
    downmix_matrix: Option<u8>,
    audio_format_index: Option<u8>,  // only set for single-format ATS
    sector_ranges: Vec<DvdaSectorRangeRef>,  // first/last sector pairs
    aob_files: Vec<DvdaAobFileRef>,          // AOB inventory for ATS-relative reads
    title_set_nr: Option<u8>,
    group_nr: u8,
    group_track_ordinal: u32,
    // ... plus SAMG fields, title context, etc.
}
```

The `DvdaVolume` trait and `AobSectorReader` from Phase 1 already handle:
- Opening files from directory or ISO volumes
- Building the AOB concatenated block inventory (9 parts, block_first/block_last)
- Reading sectors by logical block number with AOB boundary crossing

`realize_track` currently returns `Err(ConvertError::UnsupportedTrackSource)`
for `DvdaTrack`. Phase 3 replaces that with real extraction.

---

## What Phase 3 must deliver

### 1. AOB MPEG-PS demuxer (Rust, in-process)

Read 2048-byte DVD sectors from the `AobSectorReader`, extract Private
Stream 1 payloads. This is the shared layer for both MLP and LPCM.

Each DVD sector is an MPEG-2 Program Stream pack:
- Pack header starts with `0x000001BA` (4 bytes)
- Pack header is 14 bytes + stuffing (length in low 3 bits of byte 13)
- PES packets follow: `0x000001` prefix + stream_id byte + 2-byte length
- Private Stream 1 has stream_id `0xBD`
- Within the PES packet: skip PES header extension (length at PES[8])
- The DVD-Audio sub-header follows the PES header

The sub-header identifies the stream:
```c
struct sub_header {
    stream_id: u8,      // 0xA0 = PCM, 0xA1 = MLP
    cyclic: u8,
    padding: u8,
    extra_header_length: u8,
    // For PCM (stream_id 0xA0):
    //   first_audio_frame: u16
    //   padding: u8
    //   group2_bits:4 | group1_bits:4
    //   group2_freq:4 | group1_freq:4
    //   padding: u8
    //   channel_assignment: u8
    //   padding: u8
    //   cci: u8
    // For MLP (stream_id 0xA1):
    //   4 bytes padding
    //   cci: u8
}
```

The audio payload begins after the sub-header.

### 2. MLP track extraction

For each track:
1. Open the `DvdaVolume` from `volume_source`
2. Read sectors in `sector_ranges` order via `AobSectorReader`
3. Demux each sector: extract Private Stream 1 payloads
4. Validate stream_id is `0xA1` (MLP)
5. Collect MLP payloads into a bounded temporary file
6. Invoke ffmpeg to decode MLP → PCM WAV:
   ```
   ffmpeg -f mlp -i <temp_mlp_file> -c:a pcm_s32le -f wav <output.wav>
   ```
7. Validate output sample count against PTS-derived expected duration
8. Return the WAV path as the realized track

### 3. PCM WAV output

The realize step should produce a `pcm_s32le` WAV file in the staging
directory, matching the pattern used by the CUE materializer's segment
carriers. This WAV then feeds into the existing encode pipeline
(ffmpeg/sox → target format).

### 4. realize_track wiring

Replace the `DvdaTrack` stub in `realize_track_with_tool_limits_and_stats`
with a call to `realize_dvda_track()`, following the pattern of
`realize_sacd_track()`:
- Open volume from `DvdaVolumeSourceRef`
- Read sectors
- Demux MPEG-PS
- Extract MLP payload to temp file
- ffmpeg decode to WAV
- Return `RealizedTrackInfo { path, ... }`

### 5. End-to-end tests

Test against the 4 unencrypted DVD-Audio ISOs (HDAD2009, 3x Alan Parsons):
- Extract track 1 from each disc
- Verify the output is a valid WAV file
- Verify sample rate and channel count match IFO attributes
- Verify duration is within tolerance of PTS-derived expected duration

---

## Realization strategy: MLP payload extraction

The key insight from foo_input_dvda: tracks are defined by **sector ranges**
from the IFO. You read exactly those sectors, demux each one, and collect
the MLP audio payload. No seeking, no PTS-based splitting — sector boundaries
ARE the track boundaries.

```
realize_dvda_track():
  1. Open DvdaVolume from volume_source
  2. For each sector_range in sector_ranges:
       Read sectors [first..last] via AobSectorReader
  3. For each 2048-byte sector:
       Parse MPEG-PS pack header
       Find Private Stream 1 PES packet (stream_id 0xBD)
       Strip PES header + DVD-Audio sub-header
       Collect audio payload bytes
  4. Write collected MLP payload to temp file
  5. ffmpeg -f mlp -i temp.mlp -c:a pcm_s32le output.wav
  6. Validate output (probe sample count, compare to PTS duration)
  7. Return output.wav path
```

### MLP container format for ffmpeg

ffmpeg's MLP demuxer (`-f mlp`) expects raw MLP frames concatenated.
The payload extracted from Private Stream 1 after stripping the sub-header
is exactly this: raw MLP frame data. No additional container wrapping
is needed.

### Duration validation

Expected samples = `len_in_pts * sample_rate / 90000`. But `sample_rate`
may be unknown for multi-format ATS (Phase 2 leaves it as `None`). When
unknown, skip sample-count validation and just verify the output file is
non-empty and probes as valid audio.

When `sample_rate` is known (single-format ATS), validate:
`|actual_samples - expected_samples| <= sample_rate` (1-second tolerance).

---

## foo_input_dvda reference: MPEG-PS demuxer

### `dvda_block.cpp` — complete sector demuxer (83 lines)

```cpp
// Extracts Private Stream 1 payload from one 2048-byte DVD sector.
void dvda_block_t::get_ps1(uint8_t* p_block, uint8_t* p_ps1_buffer,
                           int* p_ps1_offset, sub_header_t* p_ps1_info) {
    uint8_t* p_curr = p_block;
    // Check pack header magic: 0x000001BA
    if (*(uint32_t*)p_curr == 0xba010000) {
        // Skip 14-byte pack header + stuffing bytes (low 3 bits of byte 13)
        p_curr += 14 + (p_curr[13] & 0x07);
        while (p_curr < p_block + DVD_BLOCK_SIZE - 6) {
            int pes_length = (p_curr[4] << 8) + p_curr[5];
            if ((*(uint32_t*)p_curr & 0x00ffffff) == 0x00010000) {
                if (p_curr[3] == 0xbd) {  // Private Stream 1
                    if (p_curr < p_block + DVD_BLOCK_SIZE - 9) {
                        // PES header extension length at p_curr[8]
                        uint8_t* p_ps1_header = p_curr + 9 + p_curr[8];
                        uint8_t* p_ps1_end = p_curr + 6 + pes_length;
                        if (p_ps1_header < p_ps1_end && p_ps1_end <= p_block + DVD_BLOCK_SIZE) {
                            int ps1_header_length = get_ps1_info_length(p_ps1_header,
                                (int)(p_ps1_end - p_ps1_header));
                            // Capture sub-header info on first packet
                            if (p_ps1_info && p_ps1_info->header.stream_id == UNK_STREAM_ID
                                && ps1_header_length > 0)
                                memcpy(p_ps1_info, p_ps1_header,
                                    ps1_header_length < sizeof(sub_header_t)
                                        ? ps1_header_length : sizeof(sub_header_t));
                            // Audio payload starts after sub-header
                            uint8_t* p_ps1_body = p_ps1_header + ps1_header_length;
                            int ps1_body_length = (int)(p_ps1_end - p_ps1_body);
                            if (ps1_body_length > 0) {
                                memcpy(p_ps1_buffer + *p_ps1_offset,
                                       p_ps1_body, ps1_body_length);
                                *p_ps1_offset += ps1_body_length;
                            }
                        }
                    }
                }
                p_curr += 6 + pes_length;
            } else {
                break;
            }
        }
    }
}

// Multi-sector version: processes N consecutive sectors
void dvda_block_t::get_ps1(uint8_t* p_block, int blocks,
                           uint8_t* p_ps1_buffer, int* p_ps1_offset,
                           sub_header_t* p_ps1_info) {
    if (p_ps1_info)
        p_ps1_info->header.stream_id = UNK_STREAM_ID;
    for (int i = 0; i < blocks; i++)
        get_ps1(p_block + i * DVD_BLOCK_SIZE,
                p_ps1_buffer, p_ps1_offset, p_ps1_info);
}

// Sub-header length determination
int dvda_block_t::get_ps1_info_length(uint8_t* p_substream_buffer,
                                       int substream_length) {
    int header_length = 0;
    sub_header_t* sub_header = (sub_header_t*)p_substream_buffer;
    if (substream_length > 4) {
        switch (sub_header->header.stream_id) {
        case PCM_STREAM_ID:  // 0xA0
        case MLP_STREAM_ID:  // 0xA1
            header_length = sizeof(sub_header->header)
                          + sub_header->header.extra_header_length;
            break;
        }
    }
    return header_length;
}
```

### `dvda_block.h` — sub-header structure

```cpp
constexpr int DVD_BLOCK_SIZE = 2048;

typedef struct {
    struct {
        uint8_t stream_id;           // 0xA0 = PCM, 0xA1 = MLP
        uint8_t cyclic;
        uint8_t padding1;
        uint8_t extra_header_length; // bytes of extra header after this 4-byte header
    } header;
    union {
        struct {  // PCM (stream_id 0xA0)
            uint16_t first_audio_frame;
            uint8_t padding1;
            uint8_t group2_bits : 4;
            uint8_t group1_bits : 4;
            uint8_t group2_samplerate : 4;
            uint8_t group1_samplerate : 4;
            uint8_t padding2;
            uint8_t channel_arrangement;
            uint8_t padding3;
            uint8_t cci;
        } pcm;
        struct {  // MLP (stream_id 0xA1)
            uint8_t padding1;
            uint8_t padding2;
            uint8_t padding3;
            uint8_t padding4;
            uint8_t cci;
        } mlp;
    } extra_header;
} sub_header_t;

enum { UNK_STREAM_ID = 0, PCM_STREAM_ID = 0xA0, MLP_STREAM_ID = 0xA1 };
```

### Block reading pattern from `dvda_zone.cpp`

foo_input_dvda reads blocks through the titleset, which resolves logical
sector numbers to AOB file offsets:

```cpp
// Single block read: find which AOB file contains this sector
DVDAERROR dvda_titleset_t::get_block(uint32_t block, uint8_t* buf_ptr) {
    for (auto i = 0; i < 9; i++) {
        if (aobs[i].dvda_fileobject && block >= aobs[i].block_first
            && block <= aobs[i].block_last) {
            aobs[i].dvda_fileobject->seek(
                (block - aobs[i].block_first) * DVD_BLOCK_SIZE);
            aobs[i].dvda_fileobject->read((char*)buf_ptr, DVD_BLOCK_SIZE);
            // CPPM decryption hook (not needed for unencrypted discs)
            if (dvda_zone.get_dvdcpxm() && dvda_zone.get_dvdcpxm()->get_media_type() > 0)
                dvda_zone.get_dvdcpxm()->decrypt(buf_ptr, 1, DVDCPXM_PRESERVE_CCI);
            return DVDAERR_OK;
        }
    }
    return DVDAERR_AOB_BLOCK_NOT_FOUND;
}
```

This is already implemented in Rust as `AobSectorReader::read_blocks_into()`
in the Phase 1 crate.

---

## Existing infrastructure to reuse

### Phase 1 crate (`crates/dvda-demuxer/`)

- `AobSectorReader` — reads logical sectors from concatenated AOBs
- `DvdaVolume` trait — opens files from directory or ISO
- `DirectoryDvdaVolume`, `IsoUdfDvdaVolume` — volume backends
- `build_aob_inventory()` — builds block_first/block_last for 9 AOB parts

### Pipeline infrastructure

- `ToolRunner` / `ToolCommand` — for invoking ffmpeg subprocess
- `ToolBinary::Ffmpeg` — ffmpeg binary resolution
- `realize_sacd_track()` — pattern for async realization with progress tracking
- `RealizedTrackInfo` — return type for realize_track
- Staging directory (`StagingDir`) for temp files
- `CancellationToken` for cooperative cancellation

### ffmpeg

Available in the nix dev shell as `ffmpeg_7-full` with MLP decoder confirmed
(`ffmpeg -codecs` shows `mlp`). The project already invokes ffmpeg for many
conversion tasks via `ToolRunner`.

---

## Constraints

- `#![forbid(unsafe_code)]` — the MPEG-PS parser must be safe Rust
- Use `ToolRunner` for ffmpeg invocation (not `std::process::Command`)
- Follow `realize_sacd_track` pattern: `spawn_blocking` for I/O-heavy work,
  progress heartbeat, cancellation checks
- Write MLP payload to staging dir temp file, not to memory (tracks can be
  hundreds of MB)
- Handle `DvdaSectorAddressSpace::AtsAobRelative` (the common case). Defer
  `SamgAbsolute` sector reads to a later phase.
- The realize step produces a PCM WAV file that feeds the existing encode pipeline
- LPCM extraction (stream_id 0xA0) is deferred — emit a clear error if
  encountered ("LPCM DVD-Audio extraction not yet implemented")

---

## Test corpus for Phase 3

4 unencrypted discs (the others have CPPM and can't be decoded):

| Disc | ATS | Rate | Depth | Channels | Tracks |
|------|-----|------|-------|----------|--------|
| HDAD2009 | 1 | 192 kHz | 24 | 2 (stereo) | 5 |
| AP I Robot | 1 | 192 kHz | 24 | 2 (stereo) | 10 |
| AP Friendly Card | 1 | 192 kHz | 24 | 2 (stereo) | 10 |
| AP Eye in the Sky | 1 | 192 kHz | 24 | 2 (stereo) | 10 |

All are MLP, 192 kHz/24-bit stereo. The ISOs are at
`/mnt/scratch/dev/dawdiolab/test-isos/`.

Phase 3 tests should:
- Extract at least 1 track per disc to WAV
- Verify output probes as valid audio (ffprobe)
- Verify sample rate = 192000, channels = 2, bit depth = 24
- Verify duration is within 1-second tolerance of PTS-derived expected

---

## Phase 1 code: AobSectorReader

```rust
pub struct AobSectorReader<'a, V: DvdaVolume + ?Sized> {
    volume: &'a V,
    aobs: &'a [AobFileEntry],
}

impl<'a, V: DvdaVolume + ?Sized> AobSectorReader<'a, V> {
    pub fn new(volume: &'a V, aobs: &'a [AobFileEntry]) -> Self { ... }

    /// Read `block_count` consecutive 2048-byte sectors starting at `block_first`.
    /// Handles AOB file boundary crossing transparently.
    pub fn read_blocks(&self, block_first: u32, block_count: u32) -> Result<Vec<u8>> { ... }

    /// Read into a pre-allocated buffer. Returns bytes written.
    pub fn read_blocks_into(&self, block_first: u32, block_count: u32, out: &mut [u8]) -> Result<usize> { ... }
}
```

---

## What to produce

A code bundle containing:
1. `src/convert/pipeline/dvda_demux.rs` (or similar) — MPEG-PS sector demuxer
2. Updated `realize_track` in `stages.rs` — `DvdaTrack` arm calls `realize_dvda_track()`
3. `realize_dvda_track()` implementation — sector read → demux → ffmpeg → WAV
4. Tests
