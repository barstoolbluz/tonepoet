# Cross-ATS AOB Probe — Design Brief

## Problem

Some DVD-Audio discs have a secondary title set (ATS 2) with IFO files
but no AOB files of its own. All audio lives in ATS 1's AOBs. The disc
browser's AOB probe reads sector 0 of a group's title set to identify
the codec and format. When ATS 2 has no AOBs, the current fallback
reads ATS 1's sector 0 instead, which returns the wrong presentation's
MLP major sync (e.g., 5.1 multichannel instead of stereo).

### Example: Dire Straits — Brothers in Arms DVD-Audio

```
ATS 1: ATS_01_1.AOB through ATS_01_4.AOB (5.1 multichannel MLP)
ATS 2: ATS_02_0.IFO only, NO AOB files (stereo MLP downmix)

AOTT[0]: group 1 → ATS 1, title 1 (9 tracks, 55:08) — 5.1
AOTT[1]: group 2 → ATS 1, title 2 (1 track, 0:02) — placeholder
AOTT[2]: group 3 → ATS 2, title 1 (9 tracks, 55:14) — stereo
```

Group 3's content physically lives somewhere within ATS 1's AOB data,
but ATS 2's sector addresses (0..483001) are ATS-2-relative. Sector 0
of ATS 2 does NOT correspond to sector 0 of ATS 1.

Current behavior: group 3 shows "MLP 96kHz/24-bit 5.1" (wrong channel
layout — reads ATS 1's sector 0).

Correct behavior: group 3 should show "MLP 96kHz/24-bit Stereo".

### foo_input_dvda does not handle this either

foo_input_dvda's `get_block()` checks `aobs[i].dvda_fileobject` before
reading, and null file objects (missing AOBs) are skipped. It returns
`DVDAERR_AOB_BLOCK_NOT_FOUND` for any block in a missing AOB. There is
no cross-ATS fallback in the reference implementation.

---

## Solution

### The sector offset computation

Each AOTT entry has `atsi_mat_sector` — the disc-absolute LBA of that
title set's IFO file (e.g., `ATS_02_0.IFO`).

Each ATSI MAT has `atstt_vobs` at offset `0x40` — the sector count from
the start of the ATS to where the title VOBs (AOBs) begin.

The disc-absolute LBA of ATS N's first AOB sector is:

```
ats_n_aob_start = aott_entry.atsi_mat_sector + atstt_vobs
```

A track's chapter sector range `first` is ATS-relative (offset from the
start of the ATS's AOB space). So the disc-absolute sector of a
specific track position is:

```
disc_lba = ats_n_aob_start + chapter.first_sector
```

Reading 2048 bytes from the ISO at `disc_lba * 2048` gives the raw
sector for demuxing and probe.

### What exists today

**Parsed and available:**
- `AudioTitleTableEntry.atsi_mat_sector: u32` — disc-absolute sector
  of each ATS's IFO (model.rs line 120, parsed in amg.rs)
- `TitleSet.aobs_last_sector: Option<u32>` — AOB-only sector count
- The `DvdaVolume` trait — can read files from ISO/directory
- `IsoUdfDvdaVolume` — opens ISO files for reading

**NOT parsed (needs adding):**
- `atstt_vobs` at ATSI MAT offset `0x40` — sector offset from ATS
  start to AOB data. Currently not read by the ATSI parser.

**Verified from foo_input_dvda's `ifo.h` (`atsi_mat_t` struct):**
```
offset 0x0C: ats_last_sector    (parsed)
offset 0x1C: atsi_last_sector   (parsed)
offset 0x3C: atsm_vobs          (menu VOBs, not needed)
offset 0x40: atstt_vobs         (title VOBs — THIS IS NEEDED)
```

---

## Implementation

### Step 1: Parse `atstt_vobs` in the ATSI parser

**File:** `crates/dvda-phase1/src/tui/dvda/ifo/atsi.rs`

Add reading of offset `0x40` from the ATSI MAT header:
```rust
atstt_vobs: be_u32(bytes, 0x40, "atstt_vobs")?,
```

**File:** `crates/dvda-phase1/src/tui/dvda/model.rs`

Add field to `AtsiHeader` (line ~157):
```rust
pub atstt_vobs: u32,
```

### Step 2: Store `atsi_mat_sector` on groups or make it accessible

The AOTT entry already has `atsi_mat_sector`. Groups are built from
AOTT entries via `groups_from_disc_parts()`. Either:
- Store `atsi_mat_sector` on `DvdaGroup` (new field), or
- Look it up from `disc.amg.audio_title_table` by matching
  `entry.ordinal == group.group_nr`

The lookup approach avoids changing the group model.

### Step 3: Fix the AOB probe for AOB-less title sets

**File:** `src/disc/dvda_utils.rs` — `probe_group_aob_format()`

When `AobSectorReader` fails (no AOBs for this title set):

1. Look up the AOTT entry for this group:
   ```rust
   let aott_entry = disc.amg.audio_title_table.iter()
       .find(|e| e.title_set_nr == title_ref.title_set_nr)?;
   ```
2. Get `atstt_vobs` from the title set's header:
   ```rust
   let atstt_vobs = title_set.header.atstt_vobs;
   ```
3. Compute disc-absolute sector:
   ```rust
   let disc_lba = aott_entry.atsi_mat_sector + atstt_vobs + first_sector;
   ```
4. Read 2048 bytes from the ISO at byte offset `disc_lba * 2048`.
   This requires raw ISO read access. The `DvdaVolume` trait doesn't
   expose raw sector reads, but `IsoUdfDvdaVolume` wraps a file handle.
   Options:
   a. Add a `read_raw_sector(lba: u32) -> Result<Vec<u8>>` method to
      the `DvdaVolume` trait
   b. Re-open the ISO file directly from the path (the probe function
      already has the path available via `disc` context or can receive
      it as a parameter)
   c. Add an optional `raw_reader: Option<&dyn Fn(u32) -> ...>` callback

   Option (a) is cleanest but changes the trait. Option (b) is simplest
   — just `std::fs::File::open(path)` and seek to `disc_lba * 2048`.

5. Demux and probe the sector normally.

### Step 4: Handle directory-based DVD-Audio sources

For directory-based DVD-Audio (not ISO), the `atsi_mat_sector` is
relative to the disc image, which doesn't exist as a single file.
However, directory-based rips typically have AOB files for all title
sets (they're extracted files, not a disc image). The cross-ATS problem
only affects ISO images where ATS 2 was mastered without separate AOBs.

The raw ISO read path should check if the source is an ISO before
attempting disc-absolute sector reads. For directories, fall back to
the current behavior (ATS 1 fallback or Unknown).

---

## Scope

| File | Change | Lines |
|------|--------|-------|
| `crates/dvda-phase1/src/tui/dvda/ifo/atsi.rs` | Parse `atstt_vobs` at offset 0x40 | ~2 |
| `crates/dvda-phase1/src/tui/dvda/model.rs` | Add `atstt_vobs` to `AtsiHeader` | ~1 |
| `src/disc/dvda_utils.rs` | Cross-ATS raw ISO sector read in probe fallback | ~25 |
| **Total** | | **~28** |

---

## Test corpus

| Disc | ATS count | AOB-less ATS | Expected fix |
|------|-----------|-------------|--------------|
| Brothers in Arms | 2 | ATS 2 (stereo) | Group 3: MLP 96kHz/24-bit Stereo |
| HDAD2009 | 1 | none | No change |
| MGLETSGETITON | 1 | none | No change |
| Hawks & Doves | 2 | ATS 2 may have AOBs | Check |
| Talking Heads 77 | 2 | ATS 2 may have AOBs | Check |

---

## Validation

After implementing, verify:
1. `disc-info` on Brothers in Arms shows group 3 as "MLP 96kHz/24-bit Stereo"
2. All existing discs with AOBs still probe correctly
3. `cargo test -p dvda-phase1` passes (ATSI parser changes)
4. `cargo test --bin tonepoet` passes (probe changes)

---

## What the reasoning model should produce

1. Modified `crates/dvda-phase1/src/tui/dvda/ifo/atsi.rs` with
   `atstt_vobs` parsing
2. Modified `crates/dvda-phase1/src/tui/dvda/model.rs` with the new
   field on `AtsiHeader`
3. Modified `src/disc/dvda_utils.rs` with the cross-ATS probe fallback
   using disc-absolute sector reads
4. Guidance on whether to add `read_raw_sector` to `DvdaVolume` or use
   direct file I/O for the ISO read
