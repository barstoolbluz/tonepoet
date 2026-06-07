# DVD-Audio Corpus — Expected Properties

Golden reference for Phase 1 parser development. All values derived from
`scripts/dvda_corpus_probe.py` (IFO parsing) and ffprobe (AOB stream analysis)
run against fixtures in `tests/fixtures/dvda/`.

## Corpus Summary

| Disc | ISO Size | ATS Count | Codec | CPPM | SAMG Groups | ATSI Titles | ATSI Tracks | SAMG Entries |
|------|----------|-----------|-------|------|-------------|-------------|-------------|-------------|
| HDAD2009 | 1.7G | 1 | MLP | No | 1 | 2 | 5 | 5 |
| MGLETSGETITON | 3.7G | 1 | MLP | Yes | 4 | 6 | 29 | 21 |
| Hawks & Doves | 1.6G | 2 (1 audio, 1 video) | MLP | Yes | 1 | 1+1 video | 9+1 video | 10 |
| Talking Heads 77 | 4.2G | 2 (1 audio, 1 video) | MLP | Yes | 2 (1 audio, 1 VOB ref) | 3+1 video | 27+1 video | 15 |

Note: SAMG entries < ATSI tracks for MGLETSGETITON and Talking Heads because SAMG
omits multichannel content (see per-disc detail).

**All test discs use MLP codec.** No LPCM discs in the current corpus. LPCM
extraction (Phase 3) will need synthetic fixtures or an additional test disc.

## CPPM Detection

| Disc | DVDAUDIO.MKB | Size |
|------|-------------|------|
| HDAD2009 | Absent | — |
| MGLETSGETITON | Present | 3,145,728 bytes |
| Hawks & Doves | Present | 3,145,728 bytes |
| Talking Heads 77 | Present | 3,145,728 bytes |

MKB size is identical (3 MB) across all CPPM-protected discs.

## Magic Bytes

| IFO Type | File | Magic (12 bytes) |
|----------|------|-----------------|
| AMG | AUDIO_TS.IFO | `DVDAUDIO-AMG` |
| ATSI | ATS_XX_0.IFO | `DVDAUDIO-ATS` |
| SAMG | AUDIO_PP.IFO | `DVDAUDIOSAPP` |
| ASVS | AUDIO_SV.IFO | `DVDAUDIOASVS` |

All 4 discs produce the expected magic bytes.

## ISO Filesystem

All 4 ISOs use UDF 1.02 filesystem. `isoinfo` works on some (those with
ISO 9660 bridge), `7z` works on all. The `isomage` crate (or equivalent)
must support UDF to handle this corpus.

---

## Per-Disc Detail

### HDAD2009 (HD Audio Disc 2009 Sampler)

- **Source**: `/mnt/scratch/dev/dawdiolab/test-isos/HDAD2009.ISO`
- **AMG**: 1 audio title set, 0 video title sets, spec 0x10
- **CPPM**: No
- **AOB files**: ATS_01_1.AOB (1,073,741,824), ATS_01_2.AOB (696,569,856)
- **ffprobe**: MLP, 192 kHz, 24-bit, 2ch stereo

#### ATS_01 Audio Format
- Group 1: 192.0 kHz / 24-bit, stereo (L R), assignment 1

#### ATS_01 Titles and Tracks

**Title 129** (2 tracks, 1088.5s total):

| Track | Duration | PTS Start | PTS Length | First Sector | Last Sector |
|-------|----------|-----------|------------|-------------|------------|
| 1 | 661.000s | 75 | 59,490,000 | 0 | 240,308 |
| 2 | 427.500s | 59,490,075 | 38,475,000 | 240,309 | 396,962 |

**Title 130** (3 tracks, 1250.6s total):

| Track | Duration | PTS Start | PTS Length | First Sector | Last Sector |
|-------|----------|-----------|------------|-------------|------------|
| 1 | 496.000s | 75 | 44,640,000 | 396,963 | 585,455 |
| 2 | 506.000s | 44,640,075 | 45,540,000 | 585,456 | 776,069 |
| 3 | 248.623s | 90,180,075 | 22,376,099 | 776,070 | 864,409 |

#### SAMG Tracks (5 entries, all group 1)

| # | Group | Track | Duration | Sample Rate | Depth | Channels | Abs Sectors |
|---|-------|-------|----------|-------------|-------|----------|-------------|
| 1 | 1 | 1 | 661.000s | 192.0 kHz | 24 | L R | 855-241,163 |
| 2 | 1 | 2 | 427.500s | 192.0 kHz | 24 | L R | 241,164-397,817 |
| 3 | 1 | 3 | 496.000s | 192.0 kHz | 24 | L R | 397,818-586,310 |
| 4 | 1 | 4 | 506.000s | 192.0 kHz | 24 | L R | 586,311-776,924 |
| 5 | 1 | 5 | 248.623s | 192.0 kHz | 24 | L R | 776,925-865,264 |

**Note**: SAMG flattens titles 129+130 into one sequential track list.

---

### MGLETSGETITON (Marc Bolan / T.Rex — Get It On)

- **Source**: `MGLETSGETITON (DVD-A)/MGLETSGETITON.iso`
- **AMG**: 1 audio title set, 1 video title set, spec 0x12
- **CPPM**: Yes
- **AOB files**: ATS_01_1 through ATS_01_4 (3 x 1GB + 114MB)
- **ffprobe**: MLP, 96 kHz, 24-bit, 5ch (5.0)

#### ATS_01 Audio Formats
- Format [0]: 96.0 kHz / 24-bit (group 1+2), L R C Ls Rs + L R C (assignment 19, 5+3ch multichannel)
- Format [2]: 192.0 kHz / 24-bit, stereo (L R, assignment 1)

#### ATS_01 Titles (6 titles)

| Title | Tracks | Duration | Notes |
|-------|--------|----------|-------|
| 129 | 8 | 1910.7s | Main multichannel album |
| 130 | 1 | 1.2s | Spacer |
| 131 | 8 | 1908.9s | Stereo album (192 kHz) |
| 132 | 1 | 1.2s | Spacer |
| 133 | 2 | 149.9s | Bonus content |
| 134 | 9 | 261.0s | Short excerpts (~30s each) |

#### SAMG Groups (4 groups, 21 entries)

| Group | Tracks | Format | Content |
|-------|--------|--------|---------|
| 1 | 1 (spacer only) | 44.1/16 | Spacer track |
| 2 | 9 (8 + spacer) | 192.0/24 stereo | Main stereo album |
| 3 | 2 (1 + spacer) | 48.0/16 stereo | Bonus |
| 4 | 9 (8 + spacer) | 44.1/16 stereo | Short excerpts |

**Notable**: Multichannel content (title 129, 96/24 5.0) is not directly
represented in SAMG group 1 — SAMG only shows a spacer. The multichannel
tracks require ATSI navigation.

---

### Hawks & Doves (Neil Young)

- **Source**: `Neil Young - Hawks & Doves .../HAWKSANDDOVES.iso`
- **AMG**: 2 audio title sets (ATS_01 audio, ATS_02 video), 1 video TS, spec 0x12
- **CPPM**: Yes
- **AOB files**: ATS_01_1.AOB (1,008,666,624)
- **ffprobe**: MLP, 176.4 kHz, 24-bit, 2ch stereo

#### ATS_01 Audio Format
- Group 1: 176.4 kHz / 24-bit, stereo (L R), assignment 1

**176.4 kHz is notable** — this is a 44.1 kHz base rate x4 (rare).

#### ATS_01 Title 129 (9 tracks, 1833.2s)

| Track | Duration | First Sector | Last Sector |
|-------|----------|-------------|------------|
| 1 | 135.793s | 0 | 37,514 |
| 2 | 463.104s | 37,515 | 165,755 |
| 3 | 261.202s | 165,756 | 237,913 |
| 4 | 179.167s | 237,914 | 284,888 |
| 5 | 142.030s | 284,889 | 326,924 |
| 6 | 150.399s | 326,925 | 370,750 |
| 7 | 133.334s | 370,751 | 408,879 |
| 8 | 160.402s | 408,880 | 455,743 |
| 9 | 207.735s | 455,744 | 492,512 |

#### ATS_02 (Video title set)
- 1 track, 1.034s — likely a still image menu

#### SAMG (10 entries, group 1)
- 9 audio tracks (176.4/24) + 1 VOB track (48/16, video zone)
- Absolute sector ranges differ from ATSI relative sectors (SAMG uses
  absolute disc sectors, ATSI uses sectors relative to AOB start)

---

### Talking Heads 77

- **Source**: `Talking Heads .../77.iso`
- **AMG**: 2 audio title sets (ATS_01 audio, ATS_02 video), 3 video TS, spec 0x12
- **CPPM**: Yes
- **AOB files**: ATS_01_1 through ATS_01_4 (3 x 1GB + 334MB)
- **ffprobe**: MLP, 96 kHz, 24-bit, 6ch (5.1)

#### ATS_01 Audio Formats (3 formats)
- Format [0]: 96.0 kHz / 24-bit (group 1+2), L R C LFE Ls Rs + L R C (assignment 20, 6+3ch)
- Format [1]: 96.0 kHz / 24-bit, stereo (L R), assignment 1
- Format [2]: 48.0 kHz / 24-bit, stereo (L R), assignment 1

#### ATS_01 Titles

**Title 129** — Multichannel (13 tracks, 2782.4s):

| Track | Duration | First Sector | Last Sector |
|-------|----------|-------------|------------|
| 1 | 170.927s | 0 | 78,455 |
| 2 | 189.100s | 78,456 | 162,389 |
| 3 | 188.700s | 162,390 | 246,691 |
| 4 | 235.473s | 246,692 | 350,933 |
| 5 | 103.900s | 350,934 | 396,283 |
| 6 | 289.500s | 396,284 | 524,985 |
| 7 | 247.100s | 524,986 | 634,628 |
| 8 | 181.627s | 634,629 | 715,315 |
| 9 | 211.240s | 715,316 | 811,761 |
| 10 | 259.160s | 811,762 | 923,870 |
| 11 | 280.340s | 923,871 | 1,043,837 |
| 12 | 254.960s | 1,043,838 | 1,157,408 |
| 13 | 170.373s | 1,157,409 | 1,236,230 |

**Title 1** — Stereo (13 tracks, 2785.9s):

| Track | Duration | First Sector | Last Sector |
|-------|----------|-------------|------------|
| 1 | 169.467s | 1,236,231 | 1,271,976 |
| 2 | 189.733s | 1,271,977 | 1,311,675 |
| 3 | 188.727s | 1,311,676 | 1,341,934 |
| 4 | 235.973s | 1,341,935 | 1,392,025 |
| 5 | 104.267s | 1,392,026 | 1,414,590 |
| 6 | 289.500s | 1,414,591 | 1,463,796 |
| 7 | 251.300s | 1,463,797 | 1,505,644 |
| 8 | 182.133s | 1,505,645 | 1,535,341 |
| 9 | 201.567s | 1,535,342 | 1,578,832 |
| 10 | 261.400s | 1,578,833 | 1,621,276 |
| 11 | 271.360s | 1,621,277 | 1,666,678 |
| 12 | 265.173s | 1,666,679 | 1,715,604 |
| 13 | 175.266s | 1,715,605 | 1,735,984 |

**Title 130** — Spacer (1 track, 1.0s)

#### SAMG (15 entries)

SAMG presents 2 groups:
- **Group 1**: 13 tracks (96/24 stereo) + 1 spacer (48/24) — the stereo version
- **Group 2**: 1 VOB track (48/16) — video zone reference

**Notable**: The multichannel version (title 129, 5.1) is **not in SAMG**.
SAMG only presents the stereo downmix. Multichannel access requires ATSI
title/group navigation.

---

## Observations for Parser Development

1. **All test discs use MLP** — no LPCM in corpus. Phase 3 LPCM tests need
   synthetic data or an additional test disc.

2. **SAMG is incomplete** — it doesn't expose multichannel content on
   MGLETSGETITON or Talking Heads 77. The parser must use ATSI for full
   group/title navigation as the reasoning model guidance recommends.

3. **SAMG absolute sectors vs ATSI relative sectors** — SAMG uses absolute
   disc sectors; ATSI sector pointers are relative to the AOB start within
   the title set. The parser needs both coordinate systems.
   Spot check (HDAD2009): ATSI track 1 first_sector=0, SAMG abs_first_sector=855.
   The delta (855 sectors = 1,751,040 bytes) represents the IFO/metadata area
   before AOB content on the disc.

4. **Spacer tracks** (1.0-1.2s) appear between content groups. The parser
   should flag these for filtering (foo_input_dvda has "do not load short
   tracks" option).

5. **Title numbering is irregular** — HDAD2009 uses titles 129-130; Talking
   Heads uses 129 and 1 (not 130); MGLETSGETITON uses 129-134. Title
   numbers are not sequential indices — they're the `title_nr` field from
   `ats_title_idx_t`.

6. **Multi-format title sets** — MGLETSGETITON ATS_01 carries both 96/24
   multichannel (format 0) and 192/24 stereo (format 2) in the same title
   set. Different titles within one ATS can use different audio formats.

7. **176.4 kHz** — Hawks & Doves uses 176.4 kHz (44.1 kHz base x4). The
   parser must support 44.1k-family rates, not just 48k-family.

8. **UDF 1.02** — all ISOs use UDF. ISO filesystem reader must support UDF.
