# DVD-Audio Materializer — Phase 1 Implementation Brief

## What I need from you

Design a detailed Phase 1 implementation plan for the DVD-Audio volume reader
and IFO navigation parsers in Rust. Phase 0 (corpus characterization) is
complete — we have golden IFO binary fixtures and a diagnostic Python parser
that validates our understanding of the format.

Specifically, I need you to evaluate and refine:

1. **Module layout**: Where should the dvda parser live? The SACD parser is at
   `src/tui/sacd/`. Should dvda follow that pattern or go elsewhere?

2. **The `DvdaVolume` trait**: Design the trait and its two implementations
   (ISO via `isomage` crate, extracted directory). What methods does it need?
   How should AOB file access work for the sector reader?

3. **IFO parser design**: How should we structure the AMG, ATSI, and SAMG
   parsers in Rust? One module per IFO type? Shared byte-order helpers?
   Error handling strategy?

4. **The typed data model**: The guidance doc proposes `DvdaDisc`, `DvdaGroup`,
   `TitleSet`, `AudioTitle`, `AudioChapter`, `AudioAttributes`,
   `ChannelGroupAttributes`. Validate this against what the real IFO structures
   contain (see foo_input_dvda reference below). Are we missing anything? Is
   anything over-specified?

5. **SAMG cross-referencing**: SAMG provides absolute sectors and a flat track
   list. ATSI provides relative sectors and the full hierarchy. How should
   these relate in the data model? Should the parser try to correlate them?

6. **Test strategy**: We have binary IFO fixtures from 7 discs. How should unit
   tests be structured? Parse-and-assert on known values?

7. **What NOT to build yet**: Phase 1 does not include the materializer,
   TrackSourceRef changes, pipeline wiring, or any audio extraction. Just
   parsing and the volume abstraction.

---

## Context: Project and existing patterns

Tonepoet is a Rust CLI/TUI audio conversion toolkit. It has an existing SACD
ISO materializer that follows this pattern:

- `src/tui/sacd/` — in-process SACD ISO parser (reads Master TOC, area TOCs,
  track entries from the ISO binary)
- `src/convert/pipeline/materializer_sacd.rs` — materializer that calls the
  parser, builds `PreparedSource` with `PreparedTrack` entries containing
  `TrackSourceRef::SacdTrack { iso, track_index, area }`
- `sacd-rs` crate — separate crate for DSD extraction (used by realize_track)

The DVD-Audio materializer will follow the same pattern: parse disc structure
in-process, represent selected tracks as typed source references.

Build system: Nix flake, Rust edition 2021, workspace with sub-crates.
Error handling: `thiserror` in library code, `anyhow` in main.

---

## Your prior guidance (summary)

You previously reviewed our research brief and provided guidance in
`docs/dvd-a-materializer-guidance.md`. Key decisions from that document:

- **Sector-range splitting**, not FFmpeg seeking. IFO/SAMG define track
  boundaries as first/last DVD sectors.
- **In-process MPEG-PS demuxing** from day one. AOB demuxing is small enough
  to own directly.
- **LPCM extraction in Rust** from day one. No FFmpeg for PCM.
- **FFmpeg only for MLP decode** initially.
- **Parse all three IFO types**: AMG, ATSI, SAMG.
- **`DvdaVolume` abstraction** with ISO and directory backends.
- **Rich data model**: groups > titles > chapters > sector ranges, not flat.
- **Directory support from the start**, not deferred.

---

## Phase 0 results: what we learned from the corpus

7 DVD-Audio ISOs characterized. Key findings:

**All test discs use MLP codec** — no LPCM in corpus.

**SAMG is incomplete** — omits multichannel content on MGLETSGETITON and
Talking Heads 77. ATSI parsing is mandatory.

**Title numbering is irregular** — HDAD2009 uses titles 129-130; Talking
Heads uses 129 and 1 (not 130); MGLETSGETITON uses 129-134.

**Multi-format title sets** — MGLETSGETITON ATS_01 carries both 96/24
multichannel (format 0) and 192/24 stereo (format 2). Different titles
within one ATS can use different audio formats.

**SAMG absolute vs ATSI relative sectors** — SAMG abs_first_sect includes
IFO/metadata sectors. ATSI sectors are relative to AOB start. Delta is
~855-951 sectors.

**All ISOs use UDF 1.02** — ISO reader must handle UDF.

**Spacer tracks** (1.0-1.2s) appear between content groups.

### Corpus summary

| Disc | ATS | Codec | Rate | Depth | Ch | CPPM | Groups | Titles | Tracks |
|------|-----|-------|------|-------|----|------|--------|--------|--------|
| HDAD2009 | 1 | MLP | 192k | 24 | 2 | No | 1 | 2 | 5 |
| AP I Robot | 1 | MLP | 192k | 24 | 2 | No | 1 | 2 | 10 |
| AP Friendly Card | 1 | MLP | 192k | 24 | 2 | No | 1 | 2 | 10 |
| AP Eye in the Sky | 1 | MLP | 192k | 24 | 2 | No | 1 | 2 | 10 |
| MGLETSGETITON | 1 | MLP | 96k/192k | 24 | 5 (4+1) / 2 | Yes | 4 | 6 | 29 |
| Hawks & Doves | 2 | MLP | 176.4k | 24 | 2 | Yes | 1 | 1+1v | 9+1v |
| Talking Heads 77 | 2 | MLP | 96k/48k | 24 | 6 (4+2) / 2 | Yes | 2 | 3+1v | 27+1v |

---

## foo_input_dvda reference implementation

This is the key reference for how DVD-Audio IFO parsing actually works in
practice. Source is from `foo_input_dvda-0.8.2` (foobar2000 plugin, LGPL,
by Maxim V. Anisiutkin).

### IFO binary structures (`ifo.h`)

All multi-byte fields are big-endian on disc.

#### AMG (AUDIO_TS.IFO) — `amgi_mat_t`

```c
typedef struct {
  char     amg_identifier[12];                // 0x00: "DVDAUDIO-AMG"
  uint32_t amg_last_sector;                   // 0x0C
  uint8_t  zero_1[12];                        // 0x10
  uint32_t amgi_last_sector;                  // 0x1C
  uint8_t  zero_2;                            // 0x20
  uint8_t  specification_version;             // 0x21
  uint32_t amg_category;                      // 0x22
  uint16_t amg_nr_of_volumes;                 // 0x26
  uint16_t amg_this_volume_nr;                // 0x28
  uint8_t  disc_side;                         // 0x2A
  uint8_t  zero_3[5];                         // 0x2B
  uint32_t amg_asvs;                          // 0x30 (sector)
  uint8_t  zero_4[10];                        // 0x34
  uint8_t  amg_nr_of_video_title_sets;        // 0x3E
  uint8_t  amg_nr_of_audio_title_sets;        // 0x3F
  char     provider_identifier[32];           // 0x40
  uint64_t amg_pos_code;                      // 0x60
  uint8_t  zero_5[24];                        // 0x68
  uint32_t amgi_last_byte;                    // 0x80
  uint32_t first_play_pgc;                    // 0x84
  uint8_t  zero_6[56];                        // 0x88
  uint32_t amgm_vobs;                         // 0xC0 (sector)
  uint32_t att_srpt;                          // 0xC4 (sector)
  uint32_t aott_srpt;                         // 0xC8 (sector)
  uint32_t amgm_pgci_ut;                      // 0xCC (sector)
  uint32_t ats_atrt;                          // 0xD0 (sector)
  uint32_t txtdt_mgi;                         // 0xD4 (sector)
  uint32_t amgm_c_adt;                        // 0xD8 (sector)
  uint32_t amgm_vobu_admap;                   // 0xDC (sector)
  // ... followed by video/audio attr, subpicture attr
} amgi_mat_t;
```

#### AMG Audio-Only Title Table (`aott_srpt`)

The AMG sector pointer `aott_srpt` (at offset 0xC8) points to a table that
maps top-level audio titles to ATS title sets. This is the key structure for
group-to-title navigation. Our Phase 0 Python script does not yet parse this
table — it needs to be parsed in the Rust implementation.

```c
typedef struct {
  uint8_t title_set_nr : 4;   // which ATS (1-based)
  uint8_t type_ext     : 3;
  uint8_t is_audio     : 1;   // 1 = audio title, 0 = video
} audio_playback_type_t;

typedef struct {
  audio_playback_type_t pb_ty;
  uint8_t  nr_of_tracks;       // track count for this title
  uint8_t  zero_1[2];
  uint32_t len_in_pts;          // total duration in PTS
  uint8_t  title_set_nr;        // ATS number (redundant with pb_ty)
  uint8_t  title_nr;            // title number within the ATS
  uint32_t atsi_mat;            // sector offset to ATS IFO
} audio_title_info_t;  // 14 bytes

typedef struct {
  uint16_t nr_of_srpts;         // number of audio title entries
  uint16_t last_byte;
  audio_title_info_t* title;    // array of entries
} audio_tt_srpt_t;
```

This table lives in AUDIO_TS.IFO at the sector pointed to by `aott_srpt`.
It provides the disc-level view of all audio presentations — the reasoning
model's `DvdaGroup` entries should be derivable from this table combined
with the ATSI title data. Note: our Phase 0 diagnostic script does not
parse `aott_srpt` yet, so we don't have golden values for this table. The
Rust parser should parse it and the test suite should validate against
the fixture IFOs.

#### ATSI (ATS_XX_0.IFO) — `atsi_mat_t`

```c
typedef struct {
  char     ats_identifier[12];                // 0x00: "DVDAUDIO-ATS"
  uint32_t ats_last_sector;                   // 0x0C
  uint8_t  zero_1[12];                        // 0x10
  uint32_t atsi_last_sector;                  // 0x1C
  uint8_t  zero_2;                            // 0x20
  uint8_t  specification_version;             // 0x21
  uint32_t ats_category;                      // 0x22
  // ... zeros through 0xBF ...
  uint32_t atsm_vobs;                         // 0xC0 (sector, 0 = audio TS)
  uint32_t atstt_vobs;                        // 0xC4 (sector, AOB start)
  uint32_t ats_ptt_srpt;                      // 0xC8
  uint32_t ats_pgcit;                         // 0xCC
  // ... more sector pointers ...
  uint32_t ats_c_adt;                         // 0xE0
  uint32_t ats_vobu_admap;                    // 0xE4
  uint8_t  zero_13[24];                       // 0xE8
  audio_format_t ats_audio_format[8];         // 0x100 (8 x 16 bytes)
  downmix_matrix_t ats_downmix_matrices[14];  // 0x180 (14 x 18 bytes)
} atsi_mat_t;
```

Audio format entry (`audio_format_t`, 16 bytes):
```c
typedef struct {
  uint16_t      audio_type;     // 0x0100 = present
  channel_fmt_t channel_fmt;    // 3 bytes
  uint8_t       zero_1[11];
} audio_format_t;
```

Channel format (`channel_fmt_t`, 3 bytes):
```c
typedef struct {
  uint8_t gr2_bits  : 4;    // low nibble: group2 bit depth code
  uint8_t gr1_bits  : 4;    // high nibble: group1 bit depth code
  uint8_t gr2_freq  : 4;    // low nibble: group2 sample rate code
  uint8_t gr1_freq  : 4;    // high nibble: group1 sample rate code
  uint8_t ch_gr_assgn;      // channel/group assignment (0-20)
} channel_fmt_t;
```

Bit depth codes: 0=16, 1=20, 2=24.
Sample rate codes: bit3 selects 44.1k family (0=48k family), bits 0-2:
0=base, 1=2x, 2=4x. So 0x00=48k, 0x01=96k, 0x02=192k, 0x08=44.1k,
0x09=88.2k, 0x0A=176.4k.

#### Title/track structures at offset 0x800 in ATSI

The `audio_pgcit_t` starts at byte 0x800 within the IFO file:

```c
typedef struct {
  uint16_t nr_of_titles;         // 0x00
  uint8_t  zero_1[2];            // 0x02
  uint32_t last_byte;            // 0x04
  // followed by ats_title_idx_t entries and ats_title_t data
} audio_pgcit_t;  // 8 bytes

typedef struct {
  uint8_t  title_nr;             // 0x00
  uint8_t  zero_1[3];            // 0x01
  uint32_t title_table_offset;   // 0x04 (relative to audio_pgcit_t start)
} ats_title_idx_t;  // 8 bytes

typedef struct {
  uint8_t  zero_1[2];                    // 0x00
  uint8_t  tracks;                       // 0x02
  uint8_t  indexes;                      // 0x03
  uint32_t len_in_pts;                   // 0x04
  uint8_t  zero_2[4];                    // 0x08
  uint16_t track_sector_table_offset;    // 0x0C (relative to title start)
  uint8_t  zero_3[2];                    // 0x0E
  // followed by ats_track_timestamp_t[tracks] then sector data
} ats_title_t;  // 16 bytes

typedef struct {
  uint8_t  track_type;           // 0x00
  uint8_t  downmix_matrix;       // 0x01
  uint8_t  zero_1[2];            // 0x02
  uint8_t  n;                    // 0x04 (index start, 1-based)
  uint8_t  zero_2;               // 0x05
  uint32_t first_pts;            // 0x06
  uint32_t len_in_pts;           // 0x0A
  uint8_t  zero_3[6];            // 0x0E
} ats_track_timestamp_t;  // 20 bytes

typedef struct {
  uint8_t  zero_1[4];            // 0x00
  uint32_t first;                // 0x04 (sector, relative to AOB start)
  uint32_t last;                 // 0x08 (sector)
} ats_track_sector_t;  // 12 bytes
```

#### Track-to-sector assignment logic (from `dvda_zone.cpp`)

Each track has an `n` (index_start) field. Sectors (indexes) are assigned to
tracks by matching: sector index >= track.n AND (sector index < next_track.n
OR next_track.n == 0 for last track).

```cpp
for (auto j = 0; j < p_ats_title->indexes; j++) {
    for (auto k = 0u; k < dvda_title.get_tracks().size(); k++) {
        track_curr_idx = dvda_track.get_index();
        track_next_idx = (k < size - 1) ? next_track.get_index() : 0;
        if (j + 1 >= track_curr_idx && (j + 1 < track_next_idx || track_next_idx == 0)) {
            dvda_track.get_sector_pointers().emplace_back(...);
        }
    }
}
```

#### SAMG (AUDIO_PP.IFO) — `samg_mat_t`

```c
typedef struct {
  char     samg_identifier[12];    // "DVDAUDIOSAPP"
  uint16_t samg_nr_of_tracks;      // 0x0C
  uint8_t  zero_1;                 // 0x0E
  uint8_t  specification_version;  // 0x0F
  samg_track_t track[314];         // 0x10, each 52 bytes
  uint8_t  zero_2[40];
} samg_mat_t;  // always 128 KB (8 copies of 16 KB)

typedef struct {
  uint8_t       zero_1[2];           // 0x00
  uint8_t       group_nr;            // 0x02
  uint8_t       track_nr;            // 0x03
  uint32_t      first_pts;           // 0x04
  uint32_t      len_in_pts;          // 0x08
  uint8_t       zero_2[4];           // 0x0C
  uint8_t       flags;               // 0x10 (bit 5 = zone: 0=AOB, 1=VOB)
  channel_fmt_t channel_fmt;         // 0x11 (3 bytes)
  uint8_t       zero_3[20];          // 0x14
  uint32_t      abs_first_sect;      // 0x28
  uint32_t      abs_first_sect_dup;  // 0x2C
  uint32_t      abs_last_sect;       // 0x30
} samg_track_t;  // 52 bytes
```

### Channel assignment table (21 entries)

From `audio_stream_info.cpp`, the `mlppcm_table[21]` maps assignment codes
to group1/group2 channel configurations:

| Code | Group 1 | Group 2 | G1 Ch | G2 Ch |
|------|---------|---------|-------|-------|
| 0 | C | — | 1 | 0 |
| 1 | L R | — | 2 | 0 |
| 2 | L R | S | 2 | 1 |
| 3 | L R | Ls Rs | 2 | 2 |
| 4 | L R | LFE | 2 | 1 |
| 5 | L R | LFE S | 2 | 2 |
| 6 | L R | LFE Ls Rs | 2 | 3 |
| 7 | L R | C | 2 | 1 |
| 8 | L R | C S | 2 | 2 |
| 9 | L R | C Ls Rs | 2 | 3 |
| 10 | L R | C LFE | 2 | 2 |
| 11 | L R | C LFE S | 2 | 3 |
| 12 | L R | C LFE Ls Rs | 2 | 4 |
| 13 | L R C | S | 3 | 1 |
| 14 | L R C | Ls Rs | 3 | 2 |
| 15 | L R C | LFE | 3 | 1 |
| 16 | L R C | LFE S | 3 | 2 |
| 17 | L R C | LFE Ls Rs | 3 | 3 |
| 18 | L R Ls Rs | LFE | 4 | 1 |
| 19 | L R Ls Rs | C | 4 | 1 |
| 20 | L R Ls Rs | C LFE | 4 | 2 |

Group 1 and group 2 can have different sample rates and bit depths.

### Filesystem abstraction (`dvda_filesystem.h`)

foo_input_dvda uses a `dvda_filesystem_t` that wraps UDF access:

```cpp
class dvda_filesystem_t {
    bool mount(const char* path);
    bool mount(dvda_media_t* media);
    void unmount();
    dvda_fileobject_ptr open(const char* file_name);
};
```

`dvda_fileobject_t` provides `read()`, `seek()`, `get_size()` on files
within the AUDIO_TS directory. The filesystem handles both physical discs
and ISO images through a `dvda_media_t` abstraction.

### Type hierarchy (`dvda_zone.h`)

```
dvda_zone_t              (disc level, owns titlesets)
  dvda_titleset_t        (one per ATS, owns titles + AOB files)
    dvda_title_t         (one per title within ATS)
      dvda_track_t       (one per track, has PTS + sector pointers)
        dvda_sector_pointer_t  (one per index, has first/last sector)
    dvda_aob_t[9]        (AOB file parts, block_first/block_last)
    dvda_downmix_matrix_t[14]
```

### Complete parsing implementation (`dvda_zone.cpp`)

This is the full IFO parsing logic from foo_input_dvda. It shows how
titlesets are opened, IFO data is read and byte-swapped, titles/tracks/
sector-pointers are populated, and AOB file inventories are built. This
is the reference for Phase 1's Rust parser.

```cpp
// dvda_zone.cpp — DVD-Audio Decoder plugin
// Copyright (c) 2009-2025 Maxim V.Anisiutkin (LGPL 2.1)

#include "b2n.h"       // B2N_16, B2N_32, B2N_64 — big-to-native byte swap
#include "dvda_zone.h"
#include <algorithm>
#include <cmath>
#include <string>

auto PTS_TO_SEC = [](auto pts) {
    return pts / 90000.0;
};

dvda_sector_pointer_t::dvda_sector_pointer_t(dvda_track_t& track, ats_track_sector_t& p_ats_track_sector, int sp_index) : dvda_track(track) {
    index = sp_index;
    first = p_ats_track_sector.first;
    last  = p_ats_track_sector.last;
}

double dvda_sector_pointer_t::get_time() {
    return PTS_TO_SEC(get_length_pts());
}

uint32_t dvda_sector_pointer_t::get_length_pts() {
    auto denom = dvda_track.get_last() - dvda_track.get_first() + 1u;
    if (denom) {
        auto pts = (double)dvda_track.get_length_pts() * (double)(last - first + 1u) / (double)denom;
        return (uint32_t)pts;
    }
    return 0u;
}

dvda_track_t::dvda_track_t(ats_track_timestamp_t& ats_track_timestamp, int track_no) {
    track          = track_no;
    index          = ats_track_timestamp.n;
    first_pts      = ats_track_timestamp.first_pts;
    length_pts     = ats_track_timestamp.len_in_pts;
    downmix_matrix = ats_track_timestamp.downmix_matrix < DOWNMIX_MATRICES ? ats_track_timestamp.downmix_matrix : -1;
}

double dvda_track_t::get_time() const {
    return PTS_TO_SEC(length_pts);
}

uint32_t dvda_track_t::get_first() {
    auto sector = (get_sector_pointers().size() > 0) ? get_sector_pointer(0).get_first() : 0u;
    for (auto i = 1u; i < get_sector_pointers().size(); i++) {
        sector = std::min(sector, get_sector_pointer(i).get_first());
    }
    return sector;
};

uint32_t dvda_track_t::get_last() {
    auto sector = (get_sector_pointers().size() > 0) ? get_sector_pointer(0).get_last() : 0u;
    for (auto i = 1u; i < get_sector_pointers().size(); i++) {
        sector = std::max(sector, get_sector_pointer(i).get_last());
    }
    return sector;
};

dvda_title_t::dvda_title_t(ats_title_t* p_ats_title, ats_title_idx_t* p_ats_title_idx) {
    title    = p_ats_title_idx->title_nr;
    indexes  = p_ats_title->indexes;
    tracks   = p_ats_title->tracks;
    length_pts = p_ats_title->len_in_pts;
}

double dvda_title_t::get_time() const {
    return PTS_TO_SEC(length_pts);
}

// --- Downmix coefficient decoding (0.2007 dB per step) ---

double dvda_downmix_matrix_t::get_downmix_coef(int channel, int dmx_channel) {
    auto dmx_coef{ 0.0 };
    dvda_downmix_channel_t* p_dmx_channel = get_downmix_channel(channel, dmx_channel);
    if (p_dmx_channel) {
        auto coef = p_dmx_channel->coef;
        if (coef < 200) {
            auto L_db = -0.2007 * coef;
            dmx_coef = std::pow(10.0, L_db / 20.0);
            if (p_dmx_channel->inv_phase) dmx_coef = -dmx_coef;
        } else if (coef < 255) {
            auto L_db = -(2.0 * 0.2007 * (coef - 200) + 0.2007 * 200);
            dmx_coef = std::pow(10.0, L_db / 20.0);
            if (p_dmx_channel->inv_phase) dmx_coef = -dmx_coef;
        }
    }
    return dmx_coef;
}

// --- Titleset open: reads ATS_XX_0.IFO and populates titles/tracks/sectors ---

bool dvda_titleset_t::open(size_t titleset) {
    dvda_titleset = titleset;
    dvda_titleset_type = dvda_titleset_e::DVDTitlesetUnknown;
    char file_name[13];
    snprintf(file_name, sizeof(file_name), "ATS_%02d_0.IFO", (int)dvda_titleset);
    auto atsi_file = dvda_zone.get_filesystem().open(file_name);
    if (!atsi_file) {
        snprintf(file_name, sizeof(file_name), "ATS_%02d_0.BUP", (int)dvda_titleset);
        atsi_file = dvda_zone.get_filesystem().open(file_name);
        if (!atsi_file) return is_open;
    }
    auto atsi_size = atsi_file->get_size();
    if (atsi_size >= 0x0800) {
        atsi_mat_t atsi_mat;
        if (atsi_file->read((char*)&atsi_mat, sizeof(atsi_mat_t)) == sizeof(atsi_mat_t)) {
            if (memcmp("DVDAUDIO-ATS", atsi_mat.ats_identifier, 12) == 0) {
                // --- Build AOB file inventory ---
                uint32_t aob_offset{ 0 };
                for (auto i = 0; i < 9; i++) {
                    snprintf(aobs[i].file_name, sizeof(aobs[i].file_name), "ATS_%02d_%01d.AOB", (int)dvda_titleset, i + 1);
                    aobs[i].dvda_fileobject = dvda_zone.get_filesystem().open(aobs[i].file_name);
                    if (aobs[i].dvda_fileobject) {
                        auto aob_size = aobs[i].dvda_fileobject->get_size();
                        aobs[i].block_first = aob_offset;
                        aobs[i].block_last = (uint32_t)(aobs[i].block_first + aob_size / DVD_BLOCK_SIZE + (aob_size % DVD_BLOCK_SIZE > 0 ? 1 : 0) - 1);
                    } else {
                        aobs[i].block_first = aob_offset;
                        aobs[i].block_last = aobs[i].block_first + (1024 * 1024 - 32) * 1024 / DVD_BLOCK_SIZE - 1;
                    }
                    aob_offset = aobs[i].block_last + 1;
                }

                // --- Byte-swap header fields ---
                B2N_32(atsi_mat.ats_last_sector);
                B2N_32(atsi_mat.atsi_last_sector);
                B2N_32(atsi_mat.ats_category);
                B2N_32(atsi_mat.atsi_last_byte);
                B2N_32(atsi_mat.atsm_vobs);
                B2N_32(atsi_mat.atstt_vobs);
                B2N_32(atsi_mat.ats_ptt_srpt);
                B2N_32(atsi_mat.ats_pgcit);
                B2N_32(atsi_mat.atsm_pgci_ut);
                B2N_32(atsi_mat.ats_tmapt);
                B2N_32(atsi_mat.atsm_c_adt);
                B2N_32(atsi_mat.atsm_vobu_admap);
                B2N_32(atsi_mat.ats_c_adt);
                B2N_32(atsi_mat.ats_vobu_admap);
                for (auto i = 0; i < 8; i++) {
                    B2N_16(atsi_mat.ats_audio_format[i].audio_type);
                }

                // --- Parse downmix matrices ---
                for (auto m = 0; m < DOWNMIX_MATRICES; m++) {
                    for (auto ch = 0; ch < DOWNMIX_CHANNELS; ch++) {
                        downmix_matrices[m].get_downmix_channel(ch, 0)->inv_phase = ((atsi_mat.ats_downmix_matrices[m].phase.L >> (DOWNMIX_CHANNELS - ch - 1)) & 1) == 1;
                        downmix_matrices[m].get_downmix_channel(ch, 0)->coef = atsi_mat.ats_downmix_matrices[m].coef[ch].L;
                        downmix_matrices[m].get_downmix_channel(ch, 1)->inv_phase = ((atsi_mat.ats_downmix_matrices[m].phase.R >> (DOWNMIX_CHANNELS - ch - 1)) & 1) == 1;
                        downmix_matrices[m].get_downmix_channel(ch, 1)->coef = atsi_mat.ats_downmix_matrices[m].coef[ch].R;
                    }
                }

                // --- Detect audio vs video titleset ---
                if (atsi_mat.atsm_vobs == 0) {
                    dvda_titleset_type = dvda_titleset_e::DVDTitlesetAudio;
                } else {
                    dvda_titleset_type = dvda_titleset_e::DVDTitlesetVideo;
                }
                aobs_last_sector = atsi_mat.ats_last_sector - 2 * (atsi_mat.atsi_last_sector + 1);

                // --- Parse title/track/sector data at offset 0x800 ---
                uint32_t ats_len = (uint32_t)atsi_size - 0x0800;
                atsi_file->seek(0x0800);
                std::vector<uint8_t> ats_buf(ats_len, 0);
                uint8_t* ats_end = ats_buf.data() + ats_len;
                atsi_file->read((char*)ats_buf.data(), ats_len);
                audio_pgcit_t* p_audio_pgcit = (audio_pgcit_t*)ats_buf.data();
                ats_title_idx_t* p_ats_title_idx = nullptr;

                if ((uint8_t*)p_audio_pgcit + AUDIO_PGCIT_SIZE > ats_end) goto error_exit;
                B2N_16(p_audio_pgcit->nr_of_titles);
                B2N_32(p_audio_pgcit->last_byte);
                ats_end = ats_buf.data() + ((ats_len < p_audio_pgcit->last_byte + 1) ? ats_len : p_audio_pgcit->last_byte + 1);
                p_ats_title_idx = (ats_title_idx_t*)((uint8_t*)p_audio_pgcit + AUDIO_PGCIT_SIZE);

                for (auto i = 0u; i < p_audio_pgcit->nr_of_titles; i++) {
                    if ((uint8_t*)&p_ats_title_idx[i] + ATS_TITLE_IDX_SIZE > ats_end) goto error_exit;
                    B2N_32(p_ats_title_idx[i].title_table_offset);
                    ats_title_t* p_ats_title = (ats_title_t*)((uint8_t*)p_audio_pgcit + p_ats_title_idx[i].title_table_offset);

                    if ((uint8_t*)p_ats_title + ATS_TITLE_SIZE > ats_end) goto error_exit;
                    B2N_32(p_ats_title->len_in_pts);
                    B2N_16(p_ats_title->track_sector_table_offset);
                    auto p_ats_track_timestamp = (ats_track_timestamp_t*)((uint8_t*)p_ats_title + ATS_TITLE_SIZE);
                    auto p_ats_track_sector = (ats_track_sector_t*)((uint8_t*)p_ats_title + p_ats_title->track_sector_table_offset);

                    auto&& dvda_title = get_titles().emplace_back(p_ats_title, &p_ats_title_idx[i]);

                    // --- Parse track timestamps ---
                    for (auto j = 0; j < p_ats_title->tracks; j++) {
                        if ((uint8_t*)&p_ats_track_timestamp[j] + ATS_TRACK_TIMESTAMP_SIZE > ats_end) goto error_exit;
                        B2N_32(p_ats_track_timestamp[j].first_pts);
                        B2N_32(p_ats_track_timestamp[j].len_in_pts);
                        dvda_title.get_tracks().emplace_back(p_ats_track_timestamp[j], j + 1);
                    }

                    // --- Parse sector pointers and assign to tracks ---
                    for (auto j = 0; j < p_ats_title->indexes; j++) {
                        if ((uint8_t*)&p_ats_track_sector[j] + ATS_TRACK_SECTOR_SIZE > ats_end) goto error_exit;
                        B2N_32(p_ats_track_sector[j].first);
                        B2N_32(p_ats_track_sector[j].last);
                        for (auto k = 0u; k < dvda_title.get_tracks().size(); k++) {
                            int track_curr_idx, track_next_idx;
                            auto&& dvda_track = dvda_title.get_track(k);
                            track_curr_idx = dvda_track.get_index();
                            track_next_idx = (k < dvda_title.get_tracks().size() - 1) ? dvda_title.get_track(k + 1).get_index() : 0;
                            if (j + 1 >= track_curr_idx && (j + 1 < track_next_idx || track_next_idx == 0)) {
                                dvda_track.get_sector_pointers().emplace_back(dvda_track, p_ats_track_sector[j], j + 1);
                            }
                        }
                    }
                }
                is_open = true;
            error_exit:
                ats_buf.clear();
            }
        }
    }
    return is_open;
}

// --- Block reading: resolves logical sector to AOB file + offset ---

DVDAERROR dvda_titleset_t::get_block(uint32_t block, uint8_t* buf_ptr) {
    for (auto i = 0; i < 9; i++) {
        if (aobs[i].dvda_fileobject && block >= aobs[i].block_first && block <= aobs[i].block_last) {
            if (!aobs[i].dvda_fileobject->seek((block - aobs[i].block_first) * DVD_BLOCK_SIZE)) {
                return DVDAERR_CANNOT_SEEK_ATS_XX_X_AOB;
            }
            if (aobs[i].dvda_fileobject->read((char*)buf_ptr, DVD_BLOCK_SIZE) != DVD_BLOCK_SIZE) {
                return DVDAERR_CANNOT_READ_ATS_XX_X_AOB;
            }
            // CPPM decryption hook
            if (dvda_zone.get_dvdcpxm() && dvda_zone.get_dvdcpxm()->get_media_type() > 0) {
                dvda_zone.get_dvdcpxm()->decrypt(buf_ptr, 1, DVDCPXM_PRESERVE_CCI);
            }
            return DVDAERR_OK;
        }
    }
    return DVDAERR_AOB_BLOCK_NOT_FOUND;
}

// --- Multi-block read with AOB boundary crossing ---

size_t dvda_titleset_t::get_blocks(uint32_t block_first, uint32_t block_last, uint8_t* buf_ptr) {
    auto blocks_read{ 0 };
    auto aob_index{ -1 };
    for (auto i = 0; i < 9; i++) {
        if (block_first >= aobs[i].block_first && block_first <= aobs[i].block_last) {
            aob_index = i;
            break;
        }
    }
    if (aob_index >= 0) {
        if (aobs[aob_index].dvda_fileobject) {
            if (aobs[aob_index].dvda_fileobject->seek((block_first - aobs[aob_index].block_first) * DVD_BLOCK_SIZE)) {
                if (block_last <= aobs[aob_index].block_last) {
                    int bytes_to_read = (block_last + 1 - block_first) * DVD_BLOCK_SIZE;
                    int bytes_read = (int)aobs[aob_index].dvda_fileobject->read((char*)buf_ptr, bytes_to_read);
                    blocks_read += bytes_read / DVD_BLOCK_SIZE;
                } else {
                    // Read spans AOB boundary — read remainder of current AOB then start of next
                    int bytes_to_read_1 = (aobs[aob_index].block_last + 1 - block_first) * DVD_BLOCK_SIZE;
                    int bytes_read = (int)aobs[aob_index].dvda_fileobject->read((char*)buf_ptr, bytes_to_read_1);
                    blocks_read += bytes_read / DVD_BLOCK_SIZE;
                    if (aob_index + 1 < 9) {
                        if (aobs[aob_index + 1].dvda_fileobject) {
                            if (aobs[aob_index + 1].dvda_fileobject->seek(0)) {
                                int bytes_to_read_2 = (block_last + 1 - aobs[aob_index + 1].block_first) * DVD_BLOCK_SIZE;
                                int bytes_read = (int)aobs[aob_index + 1].dvda_fileobject->read((char*)buf_ptr + blocks_read * DVD_BLOCK_SIZE, bytes_to_read_2);
                                blocks_read += bytes_read / DVD_BLOCK_SIZE;
                            }
                        }
                    }
                }
            }
        }
    }
    if (dvda_zone.get_dvdcpxm() && dvda_zone.get_dvdcpxm()->get_media_type() > 0) {
        dvda_zone.get_dvdcpxm()->decrypt(buf_ptr, blocks_read, DVDCPXM_PRESERVE_CCI);
    }
    return blocks_read;
}

// --- Zone open: reads AUDIO_TS.IFO and opens all titlesets ---

bool dvda_zone_t::open() {
    close();
    auto is_open{ false };
    audio_titlesets = 99;
    video_titlesets = 99;
    auto amgi_file = dvda_filesystem.open("AUDIO_TS.IFO");
    if (!amgi_file) {
        auto amgi_file = dvda_filesystem.open("AUDIO_TS.BUP");
    }
    if (amgi_file) {
        amgi_mat_t amgi_mat;
        if (amgi_file->read((char*)&amgi_mat, sizeof(amgi_mat_t)) == sizeof(amgi_mat_t)) {
            if (memcmp("DVDAUDIO-AMG", amgi_mat.amg_identifier, 12) == 0) {
                B2N_32(amgi_mat.amg_last_sector);
                B2N_32(amgi_mat.amgi_last_sector);
                B2N_32(amgi_mat.amg_category);
                B2N_16(amgi_mat.amg_nr_of_volumes);
                B2N_16(amgi_mat.amg_this_volume_nr);
                B2N_32(amgi_mat.amg_asvs);
                B2N_64(amgi_mat.amg_pos_code);
                B2N_32(amgi_mat.amgi_last_byte);
                B2N_32(amgi_mat.first_play_pgc);
                B2N_32(amgi_mat.amgm_vobs);
                B2N_32(amgi_mat.att_srpt);
                B2N_32(amgi_mat.aott_srpt);
                B2N_32(amgi_mat.amgm_pgci_ut);
                B2N_32(amgi_mat.ats_atrt);
                B2N_32(amgi_mat.txtdt_mgi);
                B2N_32(amgi_mat.amgm_c_adt);
                B2N_32(amgi_mat.amgm_vobu_admap);
                B2N_16(amgi_mat.amgm_audio_attr.lang_code);
                B2N_16(amgi_mat.amgm_subp_attr.lang_code);

                audio_titlesets = (audio_titlesets < amgi_mat.amg_nr_of_audio_title_sets) ? audio_titlesets : amgi_mat.amg_nr_of_audio_title_sets;
                video_titlesets = (video_titlesets < amgi_mat.amg_nr_of_video_title_sets) ? video_titlesets : amgi_mat.amg_nr_of_video_title_sets;

                // Open each audio titleset
                for (auto i = 0u; i < audio_titlesets; i++) {
                    auto& dvda_titleset = get_titlesets().emplace_back(*this);
                    if (!dvda_titleset.open(i + 1)) {
                        get_titlesets().pop_back();
                    }
                }
                is_open = true;
            }
        }
    }
    return is_open;
}

// --- Block read dispatch ---

DVDAERROR dvda_zone_t::get_block(size_t titleset, uint32_t block_no, uint8_t* buf_ptr) {
    return get_titleset(titleset).get_block(block_no, buf_ptr);
}

size_t dvda_zone_t::get_blocks(size_t titleset, uint32_t block_no, size_t blocks, uint8_t* buf_ptr) {
    return get_titleset(titleset).get_blocks(block_no, (int)(block_no + blocks - 1), buf_ptr);
}
```

Key observations for the Rust port:

1. **Fallback to .BUP**: Both AMG and ATSI parsing try the .BUP backup if
   .IFO fails to open.
2. **AOB inventory built eagerly**: All 9 possible AOB parts are probed at
   titleset open time, building block_first/block_last ranges for the
   concatenated logical sector space.
3. **Bounds checking throughout**: Every pointer advancement checks against
   `ats_end` before dereferencing.
4. **aobs_last_sector calculation**: `ats_last_sector - 2*(atsi_last_sector+1)`
   — subtracts the IFO+BUP overhead to get the AOB-only sector count.
5. **get_blocks handles AOB boundary crossing**: When a read spans two AOB
   files, it reads the tail of the first and head of the second.
6. **CPPM decrypt hook**: Applied after every block read, before returning
   data to the caller. In Phase 1 we just detect CPPM; the hook architecture
   is relevant for future phases.

---

## Available test fixtures

`tests/fixtures/dvda/` contains extracted IFO files from 7 DVD-Audio ISOs:

```
hdad2009/         — 192/24 stereo, 1 ATS, no CPPM
ap_i_robot/       — 192/24 stereo, 1 ATS, no CPPM
ap_friendly_card/ — 192/24 stereo, 1 ATS, no CPPM
ap_eye_in_the_sky/— 192/24 stereo, 1 ATS, no CPPM
mgletsgetiton/    — 96/24 5.0 + 192/24 stereo, 1 ATS, CPPM
hawks_and_doves/  — 176.4/24 stereo, 2 ATS, CPPM
talking_heads_77/ — 96/24 5.1 + 96/24 stereo, 2 ATS, CPPM
```

Each directory contains AUDIO_TS.IFO, AUDIO_PP.IFO, ATS_XX_0.IFO,
AUDIO_SV.IFO, and DVDAUDIO.MKB where applicable.

A diagnostic Python parser (`scripts/dvda_corpus_probe.py`) produces
verified output against these fixtures. JSON output is at
`tests/fixtures/dvda/corpus_probe_output.json`.

---

## Constraints and preferences

- Module should be `#![forbid(unsafe_code)]` (same as the pipeline module)
- Error types via `thiserror`
- No external C dependencies for IFO parsing — pure Rust
- ISO/UDF reading via `isomage` crate (unverified — needs evaluation; if
  inadequate, 7z extraction to temp dir is the fallback)
- AUDIO_SV.IFO (still video set) — extract but don't parse in Phase 1
- The parser should be usable standalone (not coupled to the pipeline) so
  it can serve a future `--dvda-list-groups` CLI command
