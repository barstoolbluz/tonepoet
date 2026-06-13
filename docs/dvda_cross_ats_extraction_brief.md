# DVD-Audio Cross-ATS Extraction Failure — Investigation Brief

## Problem

Extracting a cross-ATS group (ATS 2 with no AOB files, audio shared
with ATS 1) fails with LPCM channel-assignment errors. The disc is
MLP, not LPCM. The materializer correctly computes disc-absolute
sector addresses and the realize path supports `DiscAbsolute`
addressing, but the data read from the ISO at those addresses
produces invalid MPEG-PS parsing.

## Test case: Brothers in Arms DVD-Audio, Group 3 (stereo)

Group 1 (ATS 1, multichannel): extracts successfully — ATS-relative
sector addressing, AOB files present.

Group 3 (ATS 2, stereo): fails instantly — disc-absolute sector
addressing, no AOB files.

## Error log

```
track realization failed: DVD-Audio LPCM sector-local commit failed
at logical sector 1970983: DVD-Audio Private Stream 1 packet handler
failed: DVD-Audio LPCM channel-assignment code 85 is unsupported;
expected 0 through 20
```

Other tracks show channel-assignment codes 137, 223, 84, 242 — all
nonsense values indicating the demuxer is parsing the wrong data.

## Sector address analysis

```
ATS 2 atsi_mat_sector: 1683053
ATS 2 atstt_vobs: 77
Computed disc-absolute AOB start: 1683053 + 77 = 1683130
ATS 2 chapter sector ranges: 0..483001 (ATS-2-relative)

Error sectors from log → ATS-2-relative offsets:
  1728411 - 1683130 = 45281   (within 0..483001 ✓)
  1839061 - 1683130 = 155931  (within 0..483001 ✓)
  1896479 - 1683130 = 213349  (within 0..483001 ✓)
  1970983 - 1683130 = 287853  (within 0..483001 ✓)
  2072773 - 1683130 = 389643  (within 0..483001 ✓)
```

The ATS-2-relative offsets are all within the valid range, suggesting
the disc-absolute base is at least in the right ballpark. But the
data at those addresses parses as LPCM (sub-stream ID 0xA0) with
invalid channel codes, not as MLP.

## What exists

### Materializer cross-ATS support (working)

`materializer_dvda.rs`:
- `title_set_has_existing_aobs()` detects AOB-less title sets
- `title_disc_absolute_sector_base()` computes atsi_mat_sector +
  atstt_vobs (line 840-863)
- `sector_ranges_for_address_space()` adds the base to chapter
  sector ranges (line 866-893)
- Sets `DvdaSectorAddressSpace::DiscAbsolute` on the track source ref
- Empty `aob_files` vec for AOB-less title sets

### Realize cross-ATS support (exists but may be buggy)

`dvda_realize.rs`:
- `TrackSectorReader::DiscAbsoluteIso` variant (line 860)
- `open_disc_absolute_iso()` opens the ISO file handle (line 888)
- `read_disc_absolute_blocks_from_iso()` reads sectors at byte offset
  `block_first * 2048` (line 939)
- Dispatch at line 882 routes `DiscAbsolute` to the ISO reader

### Probe cross-ATS support (working)

`dvda_utils.rs` line 130-160: the probe successfully reads one sector
from the ISO at disc-absolute addresses and correctly identifies MLP.
The probe uses the same `atsi_mat_sector + atstt_vobs + first_sector`
formula and reads via `std::fs::File::open` + seek.

## What to investigate

### 1. Byte offset computation

The probe computes: `disc_lba * 2048` as the byte offset.
Does `read_disc_absolute_blocks_from_iso` use the same formula?
Is `block_first` the disc-absolute sector number, or has it been
transformed somewhere?

### 2. Volume source resolution

`open_disc_absolute_iso` extracts the ISO path from
`DvdaVolumeSourceRef`. For an ISO opened via UDF, what path does
it get? Is it the original ISO path, or something else?

### 3. MPEG-PS pack header alignment

DVD sectors are 2048 bytes. MPEG-PS packs start with `00 00 01 BA`.
If the ISO read is off by even 1 byte, the pack header check would
fail or the stream ID parsing would misalign. Could there be a
byte alignment issue in the disc-absolute read path?

### 4. Sector address correctness

The probe uses `atsi_mat_sector + atstt_vobs + first_sector` and
works. The materializer uses `title_disc_absolute_sector_base()`
which also computes `atsi_mat_sector + atstt_vobs`. Are these
computing the same value? Could `atstt_vobs` differ between the
probe path and the materializer path?

### 5. Content at the read addresses

The probe reads sector 0 of the cross-ATS stream and finds valid
MLP data (MPEG-PS pack start `00 00 01 BA`, sub-stream ID 0xA1).
The extraction reads later sectors and finds what it thinks is LPCM
(sub-stream ID 0xA0) with invalid channel codes. Could the actual
audio data at those offsets be something other than audio (e.g.,
navigation or video data interleaved in the VOB space)?

### 6. Compare with the probe path

The probe path that works:
```rust
let disc_lba = u64::from(aott_entry.atsi_mat_sector)
    + u64::from(title_set.header.atstt_vobs)
    + u64::from(first_sector);
let byte_offset = disc_lba * 2048;
file.seek(SeekFrom::Start(byte_offset))?;
file.read_exact(&mut buf)?;  // 2048 bytes
```

Compare this byte-for-byte with how `read_disc_absolute_blocks_from_iso`
computes its seek offset and reads data.

## Code to read

```
Materializer disc-absolute setup:
  src/convert/pipeline/materializer_dvda.rs
    title_disc_absolute_sector_base()     — line 840
    sector_ranges_for_address_space()     — line 866
    append_title_tracks()                 — line 1100

Realize disc-absolute read:
  src/convert/pipeline/dvda_realize.rs
    TrackSectorReader enum                — line 860
    open_disc_absolute_iso()              — line 888
    read_disc_absolute_blocks_from_iso()  — line 939
    extract_track_audio_payload()         — line 948

Probe disc-absolute read (working reference):
  src/disc/dvda_utils.rs
    probe_group_aob_format_with_path()    — line 130

Demuxer:
  src/convert/pipeline/dvda_demux.rs
    parse_private_stream_1_packets()      — sector parser
```

## What the reasoning model should produce

1. Root cause: why does the disc-absolute ISO read produce garbage
   for extraction when the same offset formula works for the probe?
2. Fix: make cross-ATS extraction work for Brothers in Arms group 3
3. Verify: the extracted audio should be MLP, and with the auto
   downmix policy, should produce stereo output
