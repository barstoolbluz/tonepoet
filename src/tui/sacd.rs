// SPDX-License-Identifier: GPL-3.0-or-later
//
// Copyright (C) 2026 the tonepoet authors.
//
// SACD ISO support: detection, TOC parsing, metadata surfacing.
//
// SACD ISOs are NOT UDF — they're a flat sequence of 2048-byte sectors
// per the ScarletBook specification (Philips/Sony, 1999). The Master
// TOC sits at LSN 510 with redundant copies at LSN 520 and LSN 530;
// each is 10 blocks (20 KB). Per-area TOCs follow, pointed at by the
// Master TOC. Audio is DSD64 (1-bit, 2.8224 MHz), stored either raw
// or DST-compressed (lossless ~2.5:1).
//
// This module intentionally implements ScarletBook fresh against the
// public spec rather than vendoring the GPL-2.0 sacd-extract sources.
// sacd-extract is referenced as a parsing oracle for spec ambiguity
// only — no code is copied.
//
// v1 scope: detection + metadata surfacing only. Audio extraction
// (DST decoding, DSF/DFF output) is deferred — would require either a
// pure-Rust DST decoder port (~3 KLOC) or subprocess invocation of
// sacd_extract (subprocess aggregation is license-clean even with
// GPL-2.0 upstream).

use std::path::Path;

/// Sector size (bytes) per the ScarletBook spec. All SACD-ISO offsets
/// are in units of these sectors.
pub const SECTOR_SIZE: u64 = 2048;

/// Master TOC start sectors (LSNs). Three redundant copies; if the
/// first is corrupted, fall back to the next.
pub const MASTER_TOC_LSNS: [u64; 3] = [510, 520, 530];

/// Magic identifier at the start of each Master TOC sector. ASCII,
/// 8 bytes, big-endian per spec.
pub const MASTER_TOC_MAGIC: &[u8; 8] = b"SACDMTOC";

/// Number of contiguous sectors occupied by each Master TOC copy
/// (Master TOC sector 0 = `master_toc_t`, sector 1 = `SACDText`,
/// sector 2 = `SACD_Man`, sectors 3-9 = album text continuation /
/// reserved). 10 sectors × 2048 bytes = 20480 bytes per copy.
pub const MASTER_TOC_LEN_SECTORS: u64 = 10;

/// On-disc size of `master_toc_t` (ScarletBook spec). All fields fit
/// within the first sector (2048 bytes).
const MASTER_TOC_T_SIZE: usize = 0xa8;

/// Magic identifier for the multilingual text sector that follows the
/// Master TOC (sector at LSN 511 / 521 / 531 — i.e. master_toc_lsn+1).
pub const SACD_TEXT_MAGIC: &[u8; 8] = b"SACDText";

/// Magic identifier for a 2-channel area's TOC sector (first sector
/// at the LSN pointed at by master_toc.area_1_toc_1_start).
pub const TWOCH_TOC_MAGIC: &[u8; 8] = b"TWOCHTOC";

/// Magic identifier for a multi-channel area's TOC sector.
pub const MULCH_TOC_MAGIC: &[u8; 8] = b"MULCHTOC";

/// Magic identifier for the per-area track-LSN list (SACDTRL1).
/// Lives at one of the sectors following the area TOC header.
pub const SACD_TRL1_MAGIC: &[u8; 8] = b"SACDTRL1";

/// Magic identifier for the per-area track-time list (SACDTRL2).
pub const SACD_TRL2_MAGIC: &[u8; 8] = b"SACDTRL2";

/// Magic identifier for the per-area, per-track text sector
/// (track titles, performers, composers, ISRC-adjacent metadata).
/// One sector per locale; tonepoet only parses the primary one
/// (locale 0).
pub const SACD_T_TXT_MAGIC: &[u8; 8] = b"SACDTTxt";

/// Magic identifier for the per-area ISRC + per-track genre list.
/// Spans **two** consecutive sectors (4096 bytes total, 4092 used).
pub const SACD_IGL_MAGIC: &[u8; 8] = b"SACD_IGL";

/// Magic identifier for the access list. Spans 32 consecutive
/// sectors (64 KB) and contains data we don't currently surface;
/// the scan loop must skip past it lest 8-byte windows of access-
/// list data accidentally collide with another magic value.
pub const SACD_ACC_MAGIC: &[u8; 8] = b"SACD_ACC";

/// Sector span of the SACD_ACC access list. Per spec.
const SACD_ACC_SECTOR_SPAN: u64 = 32;

/// Sector span of the SACD_IGL ISRC/genre list (two sectors).
const SACD_IGL_SECTOR_SPAN: u64 = 2;

/// On-disc size of the `area_toc_t` *header* (the fields up to but
/// not including the trailing 1896-byte data buffer). The header
/// plus data sum to 2048 bytes (one sector).
pub const AREA_TOC_HEADER_SIZE: usize = 0x98;

/// SACD audio frame rate (frames per second). Used to convert the
/// (minutes, seconds, frames) timecodes in SACDTRL2 to seconds.
pub const SACD_FRAME_RATE: u32 = 75;

/// Single canonical sample frequency for SACD audio: 64 × 44.1 kHz.
/// The spec defines `sample_frequency = 0x04` to mean exactly this.
/// No other value has ever been pressed.
pub const SACD_SAMPLE_RATE_HZ: u32 = 2_822_400;

/// Errors surfaced by the SACD module.
#[derive(Debug, Clone)]
pub enum SacdError {
    /// File is too small to be a valid SACD ISO (smaller than the
    /// first Master TOC offset).
    TooSmall { size: u64, required: u64 },
    /// File I/O failure (couldn't open, couldn't seek, couldn't read).
    Io(String),
    /// None of the three redundant Master TOCs had the expected magic
    /// bytes. Either not an SACD ISO, or corrupted/encrypted.
    NotSacdIso,
    /// TOC parsed but contained an invalid value (e.g. negative
    /// offset, oversized field, out-of-bounds reference).
    Malformed(String),
}

impl std::fmt::Display for SacdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SacdError::TooSmall { size, required } => {
                write!(
                    f,
                    "SACD: file too small ({} bytes, need at least {})",
                    size, required
                )
            }
            SacdError::Io(msg) => write!(f, "SACD I/O: {}", msg),
            SacdError::NotSacdIso => write!(f, "SACD: not a valid SACD ISO (no Master TOC magic)"),
            SacdError::Malformed(msg) => write!(f, "SACD malformed: {}", msg),
        }
    }
}

impl std::error::Error for SacdError {}

/// Cheap detection: does this path look like an SACD ISO?
///
/// Reads a single 8-byte block from each of the three redundant
/// Master TOC offsets (LSN 510 / 520 / 530, each at sector_n * 2048
/// bytes) and checks for the `SACDMTOC` magic. Returns `true` as soon
/// as any copy matches; returns `false` on file-too-small, I/O
/// failure, or no magic at any of the three offsets.
///
/// Designed to be called during browse-screen directory scans, so the
/// I/O cost is bounded: at most 3 short seeks and 3 × 8-byte reads
/// per file (~negligible on SSD, sub-second on spinning disk per file).
/// Callers should still cache the result by `(path, mtime)` if they
/// scan the same directory repeatedly.
pub fn is_sacd_iso(path: &Path) -> bool {
    use std::fs::File;
    use std::io::{Read, Seek, SeekFrom};

    let Ok(mut f) = File::open(path) else {
        return false;
    };
    let Ok(meta) = f.metadata() else {
        return false;
    };
    let size = meta.len();

    // Even the first Master TOC requires 510*2048 + 8 bytes = 1,044,488.
    let min_size = MASTER_TOC_LSNS[0] * SECTOR_SIZE + MASTER_TOC_MAGIC.len() as u64;
    if size < min_size {
        return false;
    }

    let mut buf = [0u8; 8];
    for &lsn in MASTER_TOC_LSNS.iter() {
        let offset = lsn * SECTOR_SIZE;
        // Skip an offset that's past EOF (might happen on a
        // truncated rip or a non-SACD file just barely above min_size).
        if offset + buf.len() as u64 > size {
            continue;
        }
        if f.seek(SeekFrom::Start(offset)).is_err() {
            continue;
        }
        if f.read_exact(&mut buf).is_err() {
            continue;
        }
        if &buf == MASTER_TOC_MAGIC {
            return true;
        }
    }
    false
}

/// Detection result with diagnostic info — used by code paths that
/// want to distinguish "not SACD" from "looks like SACD but Master
/// TOC redundancy is partially corrupted." Most callers want the
/// `bool` form via `is_sacd_iso`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectionResult {
    /// All three Master TOC copies match. Healthy ISO.
    HealthyAllRedundant,
    /// At least one Master TOC copy matches but at least one is
    /// corrupted/missing. Still readable; flag for telemetry.
    HealthyPartialRedundant { good: u8 },
    /// No Master TOC copy matched. Not an SACD ISO (or fully corrupt).
    NotSacd,
    /// File could not be read (permission denied, vanished, etc.).
    IoFailure,
    /// File is below the minimum size to host a Master TOC.
    TooSmall,
}

/// Detect with redundancy diagnostics. See `is_sacd_iso` for the
/// simpler boolean form.
pub fn detect_sacd_iso(path: &Path) -> DetectionResult {
    use std::fs::File;
    use std::io::{Read, Seek, SeekFrom};

    let Ok(mut f) = File::open(path) else {
        return DetectionResult::IoFailure;
    };
    let Ok(meta) = f.metadata() else {
        return DetectionResult::IoFailure;
    };
    let size = meta.len();

    let min_size = MASTER_TOC_LSNS[0] * SECTOR_SIZE + MASTER_TOC_MAGIC.len() as u64;
    if size < min_size {
        return DetectionResult::TooSmall;
    }

    let mut good: u8 = 0;
    let mut buf = [0u8; 8];
    for &lsn in MASTER_TOC_LSNS.iter() {
        let offset = lsn * SECTOR_SIZE;
        if offset + buf.len() as u64 > size {
            continue;
        }
        if f.seek(SeekFrom::Start(offset)).is_err() {
            continue;
        }
        if f.read_exact(&mut buf).is_err() {
            continue;
        }
        if &buf == MASTER_TOC_MAGIC {
            good += 1;
        }
    }
    match good {
        0 => DetectionResult::NotSacd,
        3 => DetectionResult::HealthyAllRedundant,
        n => DetectionResult::HealthyPartialRedundant { good: n },
    }
}

// ---------------------------------------------------------------
// Master TOC parsing
// ---------------------------------------------------------------
//
// Layout of `master_toc_t` (ScarletBook spec, packed, big-endian):
//
//   off  size  field
//   ---  ----  ------------------------------------------------
//   0x00    8  id ("SACDMTOC")
//   0x08    1  spec_version.major
//   0x09    1  spec_version.minor
//   0x0a    6  reserved01
//   0x10    2  album_set_size                       (u16 BE)
//   0x12    2  album_sequence_number                (u16 BE)
//   0x14    4  reserved02
//   0x18   16  album_catalog_number ASCII (NUL/space padded)
//   0x28   16  album_genre[4]   (4 × genre_table_t = 4 × 4)
//   0x38    8  reserved03
//   0x40    4  area_1_toc_1_start LSN               (u32 BE)
//   0x44    4  area_1_toc_2_start LSN               (u32 BE)
//   0x48    4  area_2_toc_1_start LSN               (u32 BE)
//   0x4c    4  area_2_toc_2_start LSN               (u32 BE)
//   0x50    1  disc_type byte (bit 7 = hybrid in BE bitfield order)
//   0x51    3  reserved04
//   0x54    2  area_1_toc_size  (sectors)           (u16 BE)
//   0x56    2  area_2_toc_size  (sectors)           (u16 BE)
//   0x58   16  disc_catalog_number
//   0x68   16  disc_genre[4]
//   0x78    2  disc_date_year                       (u16 BE)
//   0x7a    1  disc_date_month
//   0x7b    1  disc_date_day
//   0x7c    4  reserved05
//   0x80    1  text_area_count
//   0x81    7  reserved06
//   0x88   32  locales[8]   (8 × locale_table_t = 8 × 4)
//
// Total: 0xa8 = 168 bytes. Anything beyond this within the first
// sector is reserved/padding. Sector 1 holds the multilingual
// album/disc text (`SACDText`); sector 2 holds `SACD_Man`. Both are
// parsed in C2b.
//
// Field semantics worth pinning:
//   - area_1_*: 2-channel area pointers ("TWOCHTOC")
//   - area_2_*: multi-channel area pointers ("MULCHTOC")
//   - A start LSN of 0 means "this area is absent". An ISO can have
//     stereo only, multichannel only, or both. At least one must be
//     non-zero on a healthy disc (we treat an all-zero pair as
//     malformed since detection-by-magic already passed).
//   - toc_2 is the redundant backup copy of the area's TOC; if toc_1
//     is unreadable the player falls back to toc_2. We surface both
//     pointers so C2b can implement the same fallback per area.

/// Genre slot from a Master TOC genre table. The spec allows up to
/// 4 album genres and 4 disc genres; unused slots have category 0
/// (NOT_USED) and are filtered out before this Vec is built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Genre {
    /// `category_t` per spec: 1=GENERAL, 2=JAPANESE.
    pub category: u8,
    /// `genre_t` per spec (0..=29). See `genre_name()` for label.
    pub genre: u8,
}

impl Genre {
    /// English label for the genre code, or `"unknown"` if outside
    /// the spec range.
    pub fn name(&self) -> &'static str {
        match self.genre {
            0 => "Not used",
            1 => "Not defined",
            2 => "Adult contemporary",
            3 => "Alternative rock",
            4 => "Children's music",
            5 => "Classical",
            6 => "Contemporary Christian",
            7 => "Country",
            8 => "Dance",
            9 => "Easy listening",
            10 => "Erotic",
            11 => "Folk",
            12 => "Gospel",
            13 => "Hip-hop",
            14 => "Jazz",
            15 => "Latin",
            16 => "Musical",
            17 => "New age",
            18 => "Opera",
            19 => "Operetta",
            20 => "Pop",
            21 => "Rap",
            22 => "Reggae",
            23 => "Rock",
            24 => "Rhythm and blues",
            25 => "Sound effects",
            26 => "Soundtrack",
            27 => "Spoken word",
            28 => "World music",
            29 => "Blues",
            _ => "unknown",
        }
    }
}

/// One area's pointer block as laid out in the Master TOC. Both
/// stereo and multi-channel areas share this shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AreaPointer {
    /// LSN of the area's primary TOC sector. 0 if the area is absent.
    pub toc_1_start: u32,
    /// LSN of the redundant backup TOC. The player falls back to
    /// this if toc_1 is unreadable.
    pub toc_2_start: u32,
    /// Size of the area's TOC in sectors (capped at 96 per spec).
    /// 0 if the area is absent.
    pub toc_size_sectors: u16,
}

impl AreaPointer {
    /// True if this pointer references an actual area (non-zero
    /// primary LSN AND non-zero size).
    pub fn is_present(&self) -> bool {
        self.toc_1_start != 0 && self.toc_size_sectors != 0
    }
}

/// Disc release date from the Master TOC. The spec does not require
/// all three components; a year-only date has month=day=0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiscDate {
    pub year: u16,
    pub month: u8,
    pub day: u8,
}

/// Decoded `master_toc_t`. Multilingual text (album title, disc
/// title, performer, etc.) lives in the `SACDText` sector and is
/// parsed separately in C2b.
#[derive(Debug, Clone)]
pub struct MasterToc {
    /// (major, minor) — e.g. (1, 20) on a 1.20 disc.
    pub spec_version: (u8, u8),
    /// 1-based set size for box-set discs. 1 for standalone albums.
    pub album_set_size: u16,
    /// 1-based sequence number within a set. 1 for standalone albums.
    pub album_sequence_number: u16,
    /// ASCII catalog number, trimmed of trailing NULs/spaces.
    /// Empty string if the field was all NUL or all spaces.
    pub album_catalog_number: String,
    /// Up to 4 album-level genres; NOT_USED slots filtered out.
    pub album_genres: Vec<Genre>,
    /// 2-channel area pointer. Check `is_present()` before following.
    pub two_channel: AreaPointer,
    /// Multi-channel area pointer. Check `is_present()` before
    /// following.
    pub multi_channel: AreaPointer,
    /// True if this is a hybrid SACD (CD-DA layer + DSD layer). Bit
    /// 7 of `disc_type` per the BE-packed bitfield in the spec.
    pub disc_type_hybrid: bool,
    /// Disc-level catalog number (often the SACD UPC), trimmed.
    pub disc_catalog_number: String,
    /// Up to 4 disc-level genres; NOT_USED slots filtered out.
    pub disc_genres: Vec<Genre>,
    /// Release date if present (year != 0).
    pub disc_date: Option<DiscDate>,
    /// Number of language entries used in the locale table (0..=8).
    /// C2b consults this when iterating `SACDText` per-language
    /// blocks.
    pub text_area_count: u8,
    /// Up to 8 (language_code, character_set) pairs from the locale
    /// table. Only the first `text_area_count` are populated; the
    /// rest are spec-zero entries.
    pub locales: Vec<Locale>,
}

/// One row of the Master TOC locale table — one (language,
/// charset) pair per supported text area in `SACDText`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Locale {
    /// ISO 639-2 two-letter code (e.g. b"en"). May be `[0,0]` if
    /// the slot is unused even though `text_area_count` claims it.
    pub language_code: [u8; 2],
    /// ScarletBook `character_set_t` value. 1=ISO 646, 2=ISO 8859-1,
    /// 3=Music Shift-JIS, 4=KSC5601, 5=GB2312, 6=Big5, 7=ISO 8859-1
    /// with escapes.
    pub character_set: u8,
}

/// Parse a `master_toc_t` from a buffer that holds at least the
/// first sector of a Master TOC copy. The buffer's first 8 bytes
/// MUST be the `SACDMTOC` magic — callers should have verified this
/// already (this function double-checks and returns `NotSacdIso`
/// otherwise).
///
/// Returns `Malformed` if the buffer is too short, if both area
/// pointers are zero, or if any sub-field decode fails.
pub fn parse_master_toc(buf: &[u8]) -> Result<MasterToc, SacdError> {
    if buf.len() < MASTER_TOC_T_SIZE {
        return Err(SacdError::Malformed(format!(
            "master TOC buffer too short: {} bytes, need {}",
            buf.len(),
            MASTER_TOC_T_SIZE,
        )));
    }
    if &buf[0..8] != MASTER_TOC_MAGIC {
        return Err(SacdError::NotSacdIso);
    }

    let spec_version = (buf[0x08], buf[0x09]);
    let album_set_size = read_be_u16(buf, 0x10);
    let album_sequence_number = read_be_u16(buf, 0x12);
    let album_catalog_number = read_fixed_ascii(&buf[0x18..0x28]);
    let album_genres = read_genre_table(&buf[0x28..0x38]);

    let two_channel = AreaPointer {
        toc_1_start: read_be_u32(buf, 0x40),
        toc_2_start: read_be_u32(buf, 0x44),
        toc_size_sectors: read_be_u16(buf, 0x54),
    };
    let multi_channel = AreaPointer {
        toc_1_start: read_be_u32(buf, 0x48),
        toc_2_start: read_be_u32(buf, 0x4c),
        toc_size_sectors: read_be_u16(buf, 0x56),
    };

    if !two_channel.is_present() && !multi_channel.is_present() {
        return Err(SacdError::Malformed(
            "master TOC has no playable areas (both 2-ch and multi-ch pointers are zero)".into(),
        ));
    }

    // Hybrid bit: in the spec the BE bitfield orders
    // `disc_type_hybrid : 1` BEFORE `disc_type_reserved : 7`, which
    // in MSB-first packing places it at bit 7 of the byte. (The LE
    // branch puts it at bit 0 in struct order, but the on-disc byte
    // is the same — packed-struct bitfields are byte-level, and the
    // disc itself is a fixed BE format.)
    let disc_type_hybrid = (buf[0x50] & 0x80) != 0;

    let disc_catalog_number = read_fixed_ascii(&buf[0x58..0x68]);
    let disc_genres = read_genre_table(&buf[0x68..0x78]);

    let disc_date = {
        let year = read_be_u16(buf, 0x78);
        let month = buf[0x7a];
        let day = buf[0x7b];
        if year == 0 {
            None
        } else {
            Some(DiscDate { year, month, day })
        }
    };

    let text_area_count = buf[0x80];
    if text_area_count > 8 {
        return Err(SacdError::Malformed(format!(
            "text_area_count = {} exceeds spec maximum of 8",
            text_area_count,
        )));
    }

    let mut locales = Vec::with_capacity(8);
    for i in 0..8 {
        let off = 0x88 + i * 4;
        locales.push(Locale {
            language_code: [buf[off], buf[off + 1]],
            character_set: buf[off + 2],
            // buf[off + 3] is reserved
        });
    }

    Ok(MasterToc {
        spec_version,
        album_set_size,
        album_sequence_number,
        album_catalog_number,
        album_genres,
        two_channel,
        multi_channel,
        disc_type_hybrid,
        disc_catalog_number,
        disc_genres,
        disc_date,
        text_area_count,
        locales,
    })
}

/// Open `path`, try each redundant Master TOC LSN in order, and
/// return the first one that parses cleanly. If every copy is either
/// missing magic or malformed, returns `NotSacdIso` for the
/// missing-magic case or the most informative `Malformed` for the
/// bad-parse case.
pub fn read_master_toc(path: &Path) -> Result<MasterToc, SacdError> {
    use std::fs::File;
    use std::io::{Read, Seek, SeekFrom};

    let mut f = File::open(path).map_err(|e| SacdError::Io(format!("open: {}", e)))?;
    let size = f
        .metadata()
        .map_err(|e| SacdError::Io(format!("metadata: {}", e)))?
        .len();

    let min_size = MASTER_TOC_LSNS[0] * SECTOR_SIZE + MASTER_TOC_T_SIZE as u64;
    if size < min_size {
        return Err(SacdError::TooSmall {
            size,
            required: min_size,
        });
    }

    let mut last_malformed: Option<SacdError> = None;
    let mut saw_any_magic = false;
    let mut buf = vec![0u8; MASTER_TOC_T_SIZE];

    for &lsn in MASTER_TOC_LSNS.iter() {
        let offset = lsn * SECTOR_SIZE;
        if offset + MASTER_TOC_T_SIZE as u64 > size {
            continue;
        }
        if f.seek(SeekFrom::Start(offset)).is_err() {
            continue;
        }
        if f.read_exact(&mut buf).is_err() {
            continue;
        }
        if &buf[0..8] != MASTER_TOC_MAGIC {
            continue;
        }
        saw_any_magic = true;
        match parse_master_toc(&buf) {
            Ok(toc) => return Ok(toc),
            Err(e) => last_malformed = Some(e),
        }
    }

    if let Some(e) = last_malformed {
        return Err(e);
    }
    if saw_any_magic {
        // Defensive: magic matched but we never produced an Ok or
        // Err above. Shouldn't be reachable but keeps the function
        // total.
        return Err(SacdError::Malformed(
            "master TOC: parse loop exited without result".into(),
        ));
    }
    Err(SacdError::NotSacdIso)
}

/// Read a big-endian u16 at `off`. Caller must ensure the slice has
/// at least `off + 2` bytes (we bounds-check via slice indexing,
/// which panics — acceptable since `parse_master_toc` validates the
/// total length up-front).
#[inline]
fn read_be_u16(buf: &[u8], off: usize) -> u16 {
    u16::from_be_bytes([buf[off], buf[off + 1]])
}

#[inline]
fn read_be_u32(buf: &[u8], off: usize) -> u32 {
    u32::from_be_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
}

/// Read a fixed-width ASCII field, trimming trailing NULs and spaces
/// (the spec pads catalog-number-style fields with either, depending
/// on the press). Non-ASCII bytes are passed through as-is (preserved
/// via `from_utf8_lossy`) so unusual presses don't lose information,
/// though the spec only sanctions ASCII here.
fn read_fixed_ascii(slice: &[u8]) -> String {
    let trimmed_end = slice
        .iter()
        .rposition(|&b| b != 0 && b != b' ')
        .map_or(0, |p| p + 1);
    String::from_utf8_lossy(&slice[..trimmed_end]).into_owned()
}

/// Parse the 16-byte genre table (4 × 4-byte `genre_table_t`).
/// Filters out entries with category=0 (NOT_USED).
fn read_genre_table(slice: &[u8]) -> Vec<Genre> {
    let mut out = Vec::with_capacity(4);
    for i in 0..4 {
        let off = i * 4;
        let category = slice[off];
        // bytes off+1 / off+2 are reserved (a u16 padding); off+3 is
        // the genre code.
        let genre = slice[off + 3];
        if category != 0 {
            out.push(Genre { category, genre });
        }
    }
    out
}

// ---------------------------------------------------------------
// Master SACDText (multilingual album / disc text)
// ---------------------------------------------------------------
//
// One `master_sacd_text_t` sector exists per declared locale, in
// LSN order starting at MasterTocLsn+1. Each is exactly 2048 bytes:
//
//   0x00   8   id ("SACDText")
//   0x08   8   reserved
//   0x10   2   album_title_position                    (BE u16)
//   0x12   2   album_artist_position                   (BE u16)
//   0x14   2   album_publisher_position                (BE u16)
//   0x16   2   album_copyright_position                (BE u16)
//   0x18   2   album_title_phonetic_position           (BE u16)
//   0x1a   2   album_artist_phonetic_position          (BE u16)
//   0x1c   2   album_publisher_phonetic_position       (BE u16)
//   0x1e   2   album_copyright_phonetic_position       (BE u16)
//   0x20   2   disc_title_position                     (BE u16)
//   0x22   2   disc_artist_position                    (BE u16)
//   0x24   2   disc_publisher_position                 (BE u16)
//   0x26   2   disc_copyright_position                 (BE u16)
//   0x28   2   disc_title_phonetic_position            (BE u16)
//   0x2a   2   disc_artist_phonetic_position           (BE u16)
//   0x2c   2   disc_publisher_phonetic_position        (BE u16)
//   0x2e   2   disc_copyright_phonetic_position        (BE u16)
//   0x30   2000  data (NUL-terminated strings)
//
// Each `*_position` is a byte offset from the START of the sector
// (NOT from `data`). 0 means "field absent". Strings are NUL-
// terminated and encoded per the locale's `character_set_t`.

/// Decoded `SACDText` for a single locale. `None` fields indicate
/// either a 0 position pointer in the spec or a string that decoded
/// to empty.
#[derive(Debug, Clone, Default)]
pub struct SacdText {
    pub album_title: Option<String>,
    pub album_title_phonetic: Option<String>,
    pub album_artist: Option<String>,
    pub album_artist_phonetic: Option<String>,
    pub album_publisher: Option<String>,
    pub album_publisher_phonetic: Option<String>,
    pub album_copyright: Option<String>,
    pub album_copyright_phonetic: Option<String>,
    pub disc_title: Option<String>,
    pub disc_title_phonetic: Option<String>,
    pub disc_artist: Option<String>,
    pub disc_artist_phonetic: Option<String>,
    pub disc_publisher: Option<String>,
    pub disc_publisher_phonetic: Option<String>,
    pub disc_copyright: Option<String>,
    pub disc_copyright_phonetic: Option<String>,
    /// Charset code (`character_set_t`) used to decode the strings.
    /// Captured so callers can know whether decode was high-fidelity
    /// (charsets 1, 2, 7 → trivially correct) or lossy (charsets 3-6
    /// — Asian double-byte sets that we currently bytes-thru).
    pub charset: u8,
}

/// Parse a 2048-byte `SACDText` sector for a single locale. Returns
/// `Malformed` if magic doesn't match or the buffer is short.
///
/// `charset` is the `character_set_t` value from the locale entry in
/// the Master TOC that this text sector corresponds to.
pub fn parse_sacd_text(buf: &[u8], charset: u8) -> Result<SacdText, SacdError> {
    if buf.len() < SECTOR_SIZE as usize {
        return Err(SacdError::Malformed(format!(
            "SACDText sector too short: {} bytes, need {}",
            buf.len(),
            SECTOR_SIZE,
        )));
    }
    if &buf[0..8] != SACD_TEXT_MAGIC {
        return Err(SacdError::Malformed(format!(
            "SACDText magic missing (got {:?})",
            &buf[0..8],
        )));
    }

    // Read all 16 position fields. NUL-terminated string at each
    // non-zero position.
    let read_at = |pos_off: usize| -> Option<String> {
        let pos = read_be_u16(buf, pos_off) as usize;
        if pos == 0 || pos >= buf.len() {
            return None;
        }
        let s = read_cstr_at(buf, pos, charset);
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    };

    Ok(SacdText {
        album_title: read_at(0x10),
        album_artist: read_at(0x12),
        album_publisher: read_at(0x14),
        album_copyright: read_at(0x16),
        album_title_phonetic: read_at(0x18),
        album_artist_phonetic: read_at(0x1a),
        album_publisher_phonetic: read_at(0x1c),
        album_copyright_phonetic: read_at(0x1e),
        disc_title: read_at(0x20),
        disc_artist: read_at(0x22),
        disc_publisher: read_at(0x24),
        disc_copyright: read_at(0x26),
        disc_title_phonetic: read_at(0x28),
        disc_artist_phonetic: read_at(0x2a),
        disc_publisher_phonetic: read_at(0x2c),
        disc_copyright_phonetic: read_at(0x2e),
        charset,
    })
}

/// Read a NUL-terminated byte string starting at `start` in `buf`,
/// then decode it with the given charset.
fn read_cstr_at(buf: &[u8], start: usize, charset: u8) -> String {
    let end = buf[start..]
        .iter()
        .position(|&b| b == 0)
        .map_or(buf.len(), |p| start + p);
    let bytes = &buf[start..end];
    decode_text(bytes, charset)
}

/// Decode a byte slice per ScarletBook `character_set_t`:
///   - 0 (UNKNOWN), 1 (ISO 646 / 7-bit ASCII), 2 (ISO 8859-1),
///     7 (ISO 8859-1 with escapes) → Latin-1, lossless.
///   - 3 (Music Shift-JIS), 4 (KSC 5601), 5 (GB 2312), 6 (Big5):
///     pass-through via UTF-8 lossy. Asian SACDs will surface
///     placeholder � replacement chars; full conversion needs a
///     dedicated encoding crate (deferred — adding `encoding_rs`
///     would be a separate dependency decision).
pub fn decode_text(bytes: &[u8], charset: u8) -> String {
    match charset {
        0 | 1 | 2 | 7 => latin1_to_utf8(bytes),
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// Latin-1 → UTF-8. Each input byte is a Unicode code point in 0..=255,
/// which encodes as 1 byte (ASCII) or 2 bytes (high-Latin-1) in UTF-8.
fn latin1_to_utf8(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect()
}

/// Read the first SACDText sector from disk. Convention: the primary
/// locale's text sector lives at LSN `master_toc_lsn + 1`, where
/// `master_toc_lsn` is the LSN of the Master TOC copy that produced
/// `master_toc`. The locale's character_set is taken from
/// `master_toc.locales[0]`.
///
/// Multilingual text (locales 1..text_area_count) is deferred — each
/// additional locale lives in a successive sector but tonepoet only
/// surfaces the primary one (matching sacd-extract's "we only use
/// the first SACDText entry" comment).
pub fn read_master_text(
    path: &Path,
    master_toc_lsn: u64,
    master_toc: &MasterToc,
) -> Result<Option<SacdText>, SacdError> {
    if master_toc.text_area_count == 0 {
        return Ok(None);
    }
    let charset = master_toc
        .locales
        .first()
        .map(|l| l.character_set)
        .unwrap_or(0);
    let sector_lsn = master_toc_lsn + 1;
    let buf = read_sector(path, sector_lsn)?;
    match parse_sacd_text(&buf, charset) {
        Ok(t) => Ok(Some(t)),
        Err(SacdError::Malformed(_)) => Ok(None), // missing-magic = no text, not fatal
        Err(e) => Err(e),
    }
}

// ---------------------------------------------------------------
// Area TOC header (TWOCHTOC / MULCHTOC, sector 0 of each area)
// ---------------------------------------------------------------
//
// Layout of `area_toc_t` (header portion, 152 bytes; full sector
// includes a trailing 1896-byte `data` region holding NUL-terminated
// area description / copyright strings).
//
//   off  size  field
//   ---  ----  ------------------------------------------------
//   0x00    8  id ("TWOCHTOC" or "MULCHTOC")
//   0x08    1  version.major
//   0x09    1  version.minor
//   0x0a    2  size (sectors, BE u16)
//   0x0c    4  reserved01
//   0x10    4  max_byte_rate (BE u32)
//   0x14    1  sample_frequency (0x04 = DSD64)
//   0x15    1  frame_format byte:
//                 BE bitfield: reserved:4, frame_format:4
//                 → low nibble = frame_format (0=DST, 2=DSD3in14, 3=DSD3in16)
//                 → high nibble = reserved
//   0x16   10  reserved03
//   0x20    1  channel_count
//   0x21    1  loudspeaker byte:
//                 BE bitfield: loudspeaker_config:5, extra_settings:3
//                 → high 5 bits = loudspeaker_config
//                 → low 3 bits  = extra_settings
//   0x22    1  max_available_channels
//   0x23    1  area_mute_flags
//   0x24   12  reserved04
//   0x30    1  track_attribute byte (BE: reserved:4, track_attribute:4 → low nibble)
//   0x31   15  reserved06
//   0x40    3  total_playtime (m, s, f)
//   0x43    1  reserved07
//   0x44    1  track_offset (offset into album)
//   0x45    1  track_count (1..=255)
//   0x46    2  reserved08
//   0x48    4  track_start LSN (BE u32) — first audio LSN of area
//   0x4c    4  track_end LSN (BE u32)
//   0x50    1  text_area_count
//   0x51    7  reserved09
//   0x58   40  languages[10] (locale_table_t × 10) — note: 10 here, not 8
//   0x80    2  track_text_offset (BE u16)
//   0x82    2  index_list_offset (BE u16)
//   0x84    2  access_list_offset (BE u16)
//   0x86   10  reserved10
//   0x90    2  area_description_offset           (BE u16)
//   0x92    2  copyright_offset                  (BE u16)
//   0x94    2  area_description_phonetic_offset  (BE u16)
//   0x96    2  copyright_phonetic_offset         (BE u16)
//   0x98 1896  data (NUL-terminated strings)
//
// Total = 0x98 + 1896 = 2048 bytes ✓

/// Frame format used by an SACD area. DSD-3-in-N variants are uncompressed;
/// DST is lossless-compressed (~2.5:1) and requires DST decoding to
/// reconstruct DSD samples for playback or transcoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameFormat {
    /// DST-compressed DSD (lossless). Audio extraction needs a DST
    /// decoder (deferred — out of scope for v1 metadata-only).
    Dst,
    /// Uncompressed DSD packed 3 frames in 14 bytes.
    Dsd3In14,
    /// Uncompressed DSD packed 3 frames in 16 bytes.
    Dsd3In16,
    /// Spec value not in {0, 2, 3}. Surfaced rather than rejected so
    /// detection still succeeds for unusual presses.
    Unknown(u8),
}

impl FrameFormat {
    fn from_nibble(n: u8) -> Self {
        match n & 0x0f {
            0 => FrameFormat::Dst,
            2 => FrameFormat::Dsd3In14,
            3 => FrameFormat::Dsd3In16,
            other => FrameFormat::Unknown(other),
        }
    }
    pub fn is_dst_encoded(&self) -> bool {
        matches!(self, FrameFormat::Dst)
    }
}

/// Total playtime for an area as a (minutes, seconds, frames@75)
/// triple. Use `total_seconds()` for a flat duration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PlayTime {
    pub minutes: u8,
    pub seconds: u8,
    pub frames: u8,
}

impl PlayTime {
    /// Total duration in seconds as f64 (frames are 1/75 sec each).
    pub fn total_seconds(&self) -> f64 {
        self.minutes as f64 * 60.0
            + self.seconds as f64
            + self.frames as f64 / SACD_FRAME_RATE as f64
    }
}

/// One area's per-track entry: timing from SACDTRL1+SACDTRL2, text
/// from SACDTTxt, ISRC + genre from SACD_IGL. All non-timing fields
/// are best-effort — a disc may carry only timing, only timing+title,
/// the full set, or any subset in between. (Copy is intentionally
/// not derived because TrackText holds owned Strings.)
#[derive(Debug, Clone, Default)]
pub struct TrackEntry {
    /// Absolute LSN where this track's audio begins.
    pub start_lsn: u32,
    /// Length of this track's audio in sectors.
    pub length_lsn: u32,
    /// Track start time relative to the album start (m, s, f).
    pub start_time: PlayTime,
    /// Track duration (m, s, f).
    pub duration: PlayTime,
    /// Per-track text (title, performer, composer, ...). All fields
    /// optional; on a disc with no SACDTTxt sector, all are None.
    pub text: TrackText,
    /// 12-character ISRC if the disc has SACD_IGL with a non-empty
    /// ISRC for this track.
    pub isrc: Option<String>,
    /// Per-track genre from SACD_IGL.
    pub genre: Option<Genre>,
}

/// Decoded header portion of one area TOC (TWOCHTOC or MULCHTOC).
/// Strings (description, copyright) are pulled from the trailing
/// data region using the area's primary locale's character_set.
#[derive(Debug, Clone)]
pub struct AreaTocHeader {
    /// `Stereo` for TWOCHTOC, `MultiChannel` for MULCHTOC.
    pub kind: AreaKind,
    /// (major, minor).
    pub spec_version: (u8, u8),
    /// Total size of this area's TOC in sectors.
    pub size_sectors: u16,
    /// Maximum multiplexed-frame byte rate in bytes/sec.
    pub max_byte_rate: u32,
    /// Always 0x04 in practice (= DSD64). Surfaced as raw u8 so
    /// non-spec values aren't silently coerced.
    pub sample_frequency: u8,
    /// DST vs uncompressed DSD.
    pub frame_format: FrameFormat,
    /// Number of audio channels for each frame in this area
    /// (typically 2 for stereo, 5 or 6 for multi-channel).
    pub channel_count: u8,
    /// 5-bit `loudspeaker_config` value (high bits of byte 0x21).
    /// 0 = stereo, 3 = MC-no-LFE, 4 = 5.0, 5 = 5.1, etc.
    pub loudspeaker_config: u8,
    /// 3-bit `extra_settings` value (low bits of byte 0x21).
    pub extra_settings: u8,
    /// Maximum number of channels available on this area.
    pub max_available_channels: u8,
    /// Bitmask of muted channels for the area.
    pub area_mute_flags: u8,
    /// Total playtime of this area (sum over all tracks).
    pub total_playtime: PlayTime,
    /// Track index offset within the album (for box-set numbering).
    pub track_offset: u8,
    /// Number of tracks in this area (1..=255).
    pub track_count: u8,
    /// First audio LSN of the area.
    pub track_start_lsn: u32,
    /// Last audio LSN + 1 of the area.
    pub track_end_lsn: u32,
    /// Number of locales for which SACDTTxt sectors exist.
    pub text_area_count: u8,
    /// Up to 10 (language, charset) pairs. Note: 10 here, vs. 8 in
    /// the Master TOC.
    pub locales: Vec<Locale>,
    /// Area description string (e.g. "5.1 Multi-channel"), from
    /// `area_description_offset` within the area TOC sector data.
    /// Decoded with `locales[0].character_set`.
    pub description: Option<String>,
    pub description_phonetic: Option<String>,
    pub copyright: Option<String>,
    pub copyright_phonetic: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AreaKind {
    Stereo,
    MultiChannel,
}

/// Parse an area TOC header from a single 2048-byte sector. The
/// trailing data region is used in-place for description/copyright
/// string lookups (no separate buffer needed).
pub fn parse_area_toc_header(buf: &[u8]) -> Result<AreaTocHeader, SacdError> {
    if buf.len() < SECTOR_SIZE as usize {
        return Err(SacdError::Malformed(format!(
            "area TOC sector too short: {} bytes, need {}",
            buf.len(),
            SECTOR_SIZE,
        )));
    }
    let kind = match &buf[0..8] {
        b if b == TWOCH_TOC_MAGIC => AreaKind::Stereo,
        b if b == MULCH_TOC_MAGIC => AreaKind::MultiChannel,
        other => {
            return Err(SacdError::Malformed(format!(
                "area TOC magic mismatch (got {:?}, want TWOCHTOC or MULCHTOC)",
                other,
            )));
        }
    };

    let spec_version = (buf[0x08], buf[0x09]);
    let size_sectors = read_be_u16(buf, 0x0a);
    let max_byte_rate = read_be_u32(buf, 0x10);
    let sample_frequency = buf[0x14];

    // Frame format: the spec's BE bitfield declares
    //     reserved02:4, frame_format:4
    // putting frame_format in the LOW nibble (when packed MSB-first
    // into a single byte, the LATER-declared bitfield occupies the
    // low bits).
    let frame_format = FrameFormat::from_nibble(buf[0x15] & 0x0f);

    let channel_count = buf[0x20];
    // Loudspeaker byte: BE bitfield order is
    //     loudspeaker_config:5, extra_settings:3
    // → high 5 bits = loudspeaker_config, low 3 bits = extra_settings.
    let loudspeaker_config = (buf[0x21] >> 3) & 0x1f;
    let extra_settings = buf[0x21] & 0x07;
    let max_available_channels = buf[0x22];
    let area_mute_flags = buf[0x23];

    let total_playtime = PlayTime {
        minutes: buf[0x40],
        seconds: buf[0x41],
        frames: buf[0x42],
    };
    let track_offset = buf[0x44];
    let track_count = buf[0x45];
    let track_start_lsn = read_be_u32(buf, 0x48);
    let track_end_lsn = read_be_u32(buf, 0x4c);

    let text_area_count = buf[0x50];
    if text_area_count > 10 {
        return Err(SacdError::Malformed(format!(
            "area TOC text_area_count = {} exceeds spec maximum of 10",
            text_area_count,
        )));
    }
    let mut locales = Vec::with_capacity(10);
    for i in 0..10 {
        let off = 0x58 + i * 4;
        locales.push(Locale {
            language_code: [buf[off], buf[off + 1]],
            character_set: buf[off + 2],
        });
    }

    let charset = locales.first().map(|l| l.character_set).unwrap_or(0);

    let description = read_optional_offset_str(buf, 0x90, charset);
    let copyright = read_optional_offset_str(buf, 0x92, charset);
    let description_phonetic = read_optional_offset_str(buf, 0x94, charset);
    let copyright_phonetic = read_optional_offset_str(buf, 0x96, charset);

    Ok(AreaTocHeader {
        kind,
        spec_version,
        size_sectors,
        max_byte_rate,
        sample_frequency,
        frame_format,
        channel_count,
        loudspeaker_config,
        extra_settings,
        max_available_channels,
        area_mute_flags,
        total_playtime,
        track_offset,
        track_count,
        track_start_lsn,
        track_end_lsn,
        text_area_count,
        locales,
        description,
        description_phonetic,
        copyright,
        copyright_phonetic,
    })
}

/// Read a u16 BE at `off_pos` from `buf`; if non-zero, treat as a
/// byte offset within `buf` and read a NUL-term string. Used for
/// the four `*_offset` fields in area_toc_t.
fn read_optional_offset_str(buf: &[u8], off_pos: usize, charset: u8) -> Option<String> {
    let off = read_be_u16(buf, off_pos) as usize;
    if off == 0 || off >= buf.len() {
        return None;
    }
    let s = read_cstr_at(buf, off, charset);
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

// ---------------------------------------------------------------
// SACDTRL1 (track LSNs) and SACDTRL2 (track times)
// ---------------------------------------------------------------

/// Parse a 2048-byte SACDTRL1 sector. The track_start_lsn and
/// track_length_lsn arrays each hold 255 BE u32s; only the first
/// `track_count` entries are meaningful.
pub fn parse_trl1(buf: &[u8], track_count: u8) -> Result<Vec<(u32, u32)>, SacdError> {
    if buf.len() < SECTOR_SIZE as usize {
        return Err(SacdError::Malformed("SACDTRL1 sector too short".into()));
    }
    if &buf[0..8] != SACD_TRL1_MAGIC {
        return Err(SacdError::Malformed(format!(
            "SACDTRL1 magic missing (got {:?})",
            &buf[0..8],
        )));
    }
    let mut out = Vec::with_capacity(track_count as usize);
    // start LSNs at offset 0x08 (8 + 0), lengths at offset 0x08 + 255*4 = 0x08 + 1020 = 0x404.
    let start_base = 8;
    let len_base = 8 + 255 * 4;
    for i in 0..track_count as usize {
        let s = read_be_u32(buf, start_base + i * 4);
        let l = read_be_u32(buf, len_base + i * 4);
        out.push((s, l));
    }
    Ok(out)
}

/// Parse a 2048-byte SACDTRL2 sector. Returns (start, duration)
/// pairs in track order, only `track_count` of them.
pub fn parse_trl2(buf: &[u8], track_count: u8) -> Result<Vec<(PlayTime, PlayTime)>, SacdError> {
    if buf.len() < SECTOR_SIZE as usize {
        return Err(SacdError::Malformed("SACDTRL2 sector too short".into()));
    }
    if &buf[0..8] != SACD_TRL2_MAGIC {
        return Err(SacdError::Malformed(format!(
            "SACDTRL2 magic missing (got {:?})",
            &buf[0..8],
        )));
    }
    let mut out = Vec::with_capacity(track_count as usize);
    // each area_tracklist_time_t = 4 bytes (m, s, f, flags).
    // start[255] at offset 0x08, duration[255] at offset 0x08 + 255*4 = 0x08 + 1020 = 0x404.
    let start_base = 8;
    let dur_base = 8 + 255 * 4;
    for i in 0..track_count as usize {
        let s = PlayTime {
            minutes: buf[start_base + i * 4],
            seconds: buf[start_base + i * 4 + 1],
            frames: buf[start_base + i * 4 + 2],
        };
        let d = PlayTime {
            minutes: buf[dur_base + i * 4],
            seconds: buf[dur_base + i * 4 + 1],
            frames: buf[dur_base + i * 4 + 2],
        };
        out.push((s, d));
    }
    Ok(out)
}

// ---------------------------------------------------------------
// SACDTTxt (per-track multilingual text)
// ---------------------------------------------------------------
//
// Layout of an SACDTTxt sector (2048 bytes, one per locale):
//
//   off  size  field
//   ---  ----  ------------------------------------------------
//   0x00    8  id ("SACDTTxt")
//   0x08    2*track_count  track_text_position[i] (BE u16 each)
//   ... rest: NUL-terminated text blocks pointed at by positions
//
// Each non-zero `track_text_position[i]` points within the same
// 2048-byte sector to a per-track text block:
//
//   [track_amount: u8][unknown: 3 bytes]
//     [track_type: u8][0x20: u8][string: NUL-term][NUL pad...]
//     [track_type: u8][0x20: u8][string: NUL-term][NUL pad...]
//     ...track_amount entries...
//
// `track_type` values (from `track_type_t` in the spec):
//   0x01 TITLE          0x81 TITLE_PHONETIC
//   0x02 PERFORMER      0x82 PERFORMER_PHONETIC
//   0x03 SONGWRITER     0x83 SONGWRITER_PHONETIC
//   0x04 COMPOSER       0x84 COMPOSER_PHONETIC
//   0x05 ARRANGER       0x85 ARRANGER_PHONETIC
//   0x06 MESSAGE        0x86 MESSAGE_PHONETIC
//   0x07 EXTRA_MESSAGE  0x87 EXTRA_MESSAGE_PHONETIC
//
// The 0x20 byte after each type byte is documented in sacd-extract
// only as "unknown 0x20" — it appears to be a separator.

/// Per-track text fields for a single track in a single area, all
/// optional (a track may carry just title, or title+performer, or
/// every field). Phonetic variants are stored separately for discs
/// that ship Latin-script romanizations (common on JP imports).
#[derive(Debug, Clone, Default)]
pub struct TrackText {
    pub title: Option<String>,
    pub performer: Option<String>,
    pub songwriter: Option<String>,
    pub composer: Option<String>,
    pub arranger: Option<String>,
    pub message: Option<String>,
    pub extra_message: Option<String>,
    pub title_phonetic: Option<String>,
    pub performer_phonetic: Option<String>,
    pub songwriter_phonetic: Option<String>,
    pub composer_phonetic: Option<String>,
    pub arranger_phonetic: Option<String>,
    pub message_phonetic: Option<String>,
    pub extra_message_phonetic: Option<String>,
}

/// Parse one SACDTTxt sector for the *primary* locale into a vector
/// of `TrackText`, one per track (zero-indexed). Tracks with all
/// position pointers set to 0 yield default (all-None) entries.
pub fn parse_sacd_t_txt(
    buf: &[u8],
    track_count: u8,
    charset: u8,
) -> Result<Vec<TrackText>, SacdError> {
    if buf.len() < SECTOR_SIZE as usize {
        return Err(SacdError::Malformed("SACDTTxt sector too short".into()));
    }
    if &buf[0..8] != SACD_T_TXT_MAGIC {
        return Err(SacdError::Malformed(format!(
            "SACDTTxt magic missing (got {:?})",
            &buf[0..8],
        )));
    }

    let mut out = vec![TrackText::default(); track_count as usize];
    for i in 0..track_count as usize {
        let pos_off = 8 + i * 2;
        if pos_off + 2 > buf.len() {
            break;
        }
        let pos = read_be_u16(buf, pos_off) as usize;
        // Position 0 = absent. Need at least 4 bytes (track_amount +
        // 3 unknown) at the pointed-at location.
        if pos == 0 || pos + 4 > buf.len() {
            continue;
        }

        let track_amount = buf[pos] as usize;
        // Skip track_amount byte + 3 unknown.
        let mut p = pos + 4;

        for j in 0..track_amount {
            // Need at least 2 more bytes (type + 0x20 separator).
            if p + 2 > buf.len() {
                break;
            }
            let track_type = buf[p];
            // Past type byte and the documented-but-mystery 0x20.
            p += 2;
            if p >= buf.len() {
                break;
            }

            // String at p; possibly empty (first byte is 0).
            if buf[p] != 0 {
                let str_end = buf[p..]
                    .iter()
                    .position(|&b| b == 0)
                    .map_or(buf.len(), |e| p + e);
                let bytes = &buf[p..str_end];
                let s = decode_text(bytes, charset);
                if !s.is_empty() {
                    set_track_text_field(&mut out[i], track_type, s);
                }
            }

            // Advance to next entry's type byte (only between entries).
            if j < track_amount.saturating_sub(1) {
                while p < buf.len() && buf[p] != 0 {
                    p += 1;
                }
                while p < buf.len() && buf[p] == 0 {
                    p += 1;
                }
            }
        }
    }

    Ok(out)
}

fn set_track_text_field(tt: &mut TrackText, ttype: u8, s: String) {
    match ttype {
        0x01 => tt.title = Some(s),
        0x02 => tt.performer = Some(s),
        0x03 => tt.songwriter = Some(s),
        0x04 => tt.composer = Some(s),
        0x05 => tt.arranger = Some(s),
        0x06 => tt.message = Some(s),
        0x07 => tt.extra_message = Some(s),
        0x81 => tt.title_phonetic = Some(s),
        0x82 => tt.performer_phonetic = Some(s),
        0x83 => tt.songwriter_phonetic = Some(s),
        0x84 => tt.composer_phonetic = Some(s),
        0x85 => tt.arranger_phonetic = Some(s),
        0x86 => tt.message_phonetic = Some(s),
        0x87 => tt.extra_message_phonetic = Some(s),
        // Unknown / future track types: silently skip rather than
        // refuse to parse (matches sacd-extract behaviour).
        _ => {}
    }
}

// ---------------------------------------------------------------
// SACD_IGL (per-track ISRC + per-track genre)
// ---------------------------------------------------------------
//
// Layout (4092 bytes, spans 2 sectors = 4096 bytes available):
//
//   off    size  field
//   ----  ----   ------------------------------------------------
//   0x000    8   id ("SACD_IGL")
//   0x008 3060   isrc[255]   (12 bytes each = 255 × 12)
//   0xc04    4   reserved (u32)
//   0xc08 1020   track_genre[255]  (4 bytes each = 255 × 4)
//
// ISRC layout per `isrc_t` (12 bytes): country[2] + owner[3] +
// year[2] + designation[5] = 12 ASCII characters. All 0x00 means
// "no ISRC". Some discs pad with spaces instead.

/// One row of SACD_IGL data for a single track.
#[derive(Debug, Clone, Default)]
pub struct TrackIsrcGenre {
    /// 12-character ISRC if non-empty, else None.
    pub isrc: Option<String>,
    /// Per-track genre if `category != 0`, else None.
    pub genre: Option<Genre>,
}

/// Parse a SACD_IGL block (must be at least 4092 bytes — i.e. you
/// need to have read both sectors and concatenated them before
/// calling). Returns one `TrackIsrcGenre` per track in disc order.
pub fn parse_sacd_igl(buf: &[u8], track_count: u8) -> Result<Vec<TrackIsrcGenre>, SacdError> {
    let need = 8 + 12 * 255 + 4 + 4 * 255; // = 4092
    if buf.len() < need {
        return Err(SacdError::Malformed(format!(
            "SACD_IGL buffer too short: {} bytes, need {}",
            buf.len(),
            need,
        )));
    }
    if &buf[0..8] != SACD_IGL_MAGIC {
        return Err(SacdError::Malformed(format!(
            "SACD_IGL magic missing (got {:?})",
            &buf[0..8],
        )));
    }

    let isrc_base = 8;
    let genre_base = 8 + 12 * 255 + 4;

    let mut out = Vec::with_capacity(track_count as usize);
    for i in 0..track_count as usize {
        // ISRC: 12 ASCII bytes. Some discs use NUL pad, some space.
        let isrc_off = isrc_base + i * 12;
        let isrc_str = read_fixed_ascii(&buf[isrc_off..isrc_off + 12]);
        let isrc = if isrc_str.is_empty() {
            None
        } else {
            Some(isrc_str)
        };

        // Genre: 4 bytes (category, reserved u16, genre_code).
        let g_off = genre_base + i * 4;
        let category = buf[g_off];
        let genre_code = buf[g_off + 3];
        let genre = if category != 0 {
            Some(Genre {
                category,
                genre: genre_code,
            })
        } else {
            None
        };

        out.push(TrackIsrcGenre { isrc, genre });
    }
    Ok(out)
}

// ---------------------------------------------------------------
// Top-level orchestrator
// ---------------------------------------------------------------

/// One parsed area (stereo or multi-channel) with its track list.
#[derive(Debug, Clone)]
pub struct AreaInfo {
    pub header: AreaTocHeader,
    /// Per-track LSN + time entries, in track order. Length matches
    /// `header.track_count`. Empty if neither SACDTRL1 nor SACDTRL2
    /// was found in the area's TOC sectors (rare but tolerated).
    pub tracks: Vec<TrackEntry>,
}

/// Top-level SACD metadata for an ISO. Either or both areas may be
/// present; at least one is guaranteed (master TOC validation
/// rejects the no-areas case).
#[derive(Debug, Clone)]
pub struct SacdMetadata {
    pub master_toc: MasterToc,
    pub master_text: Option<SacdText>,
    pub stereo: Option<AreaInfo>,
    pub multi_channel: Option<AreaInfo>,
}

impl SacdMetadata {
    /// Best-effort album title: prefer `master_text.album_title`,
    /// fall back to the stereo area's description, then None.
    pub fn album_title(&self) -> Option<&str> {
        self.master_text
            .as_ref()
            .and_then(|t| t.album_title.as_deref())
    }
    /// Album artist from the primary locale, if any.
    pub fn album_artist(&self) -> Option<&str> {
        self.master_text
            .as_ref()
            .and_then(|t| t.album_artist.as_deref())
    }
    /// True if either area is DST-encoded (audio extraction would
    /// need DST decoding).
    pub fn any_dst_encoded(&self) -> bool {
        self.stereo
            .as_ref()
            .is_some_and(|a| a.header.frame_format.is_dst_encoded())
            || self
                .multi_channel
                .as_ref()
                .is_some_and(|a| a.header.frame_format.is_dst_encoded())
    }
}

/// Open `path`, parse the Master TOC, then parse SACDText, both
/// area TOCs, and their tracklists. Returns a fully populated
/// `SacdMetadata` on success.
///
/// Areas are parsed independently — if the multi-channel area is
/// malformed but stereo is fine, you'll get `multi_channel = None`
/// and stereo populated. Set `strict` to true to error on any
/// per-area failure instead.
pub fn parse_sacd_iso(path: &Path) -> Result<SacdMetadata, SacdError> {
    parse_sacd_iso_with_strictness(path, false)
}

/// As `parse_sacd_iso` but lets the caller choose between best-effort
/// (non-strict; per-area failures yield None) and strict (any
/// per-area failure propagates).
pub fn parse_sacd_iso_with_strictness(
    path: &Path,
    strict: bool,
) -> Result<SacdMetadata, SacdError> {
    // Walk LSNs to find a parsing master TOC; remember which LSN it
    // came from so we can locate the SACDText sector immediately
    // after it.
    let (master_toc_lsn, master_toc) = read_master_toc_with_lsn(path)?;
    let master_text = read_master_text(path, master_toc_lsn, &master_toc)?;

    let stereo = parse_area_with_strictness(path, master_toc.two_channel, strict)?;
    let multi_channel = parse_area_with_strictness(path, master_toc.multi_channel, strict)?;

    Ok(SacdMetadata {
        master_toc,
        master_text,
        stereo,
        multi_channel,
    })
}

/// Like `read_master_toc` but also returns the LSN where the master
/// TOC was found. Needed because SACDText sits at master_toc_lsn+1,
/// and that LSN differs depending on which redundant copy parsed.
pub fn read_master_toc_with_lsn(path: &Path) -> Result<(u64, MasterToc), SacdError> {
    use std::fs::File;
    use std::io::{Read, Seek, SeekFrom};

    let mut f = File::open(path).map_err(|e| SacdError::Io(format!("open: {}", e)))?;
    let size = f
        .metadata()
        .map_err(|e| SacdError::Io(format!("metadata: {}", e)))?
        .len();

    let min_size = MASTER_TOC_LSNS[0] * SECTOR_SIZE + MASTER_TOC_T_SIZE as u64;
    if size < min_size {
        return Err(SacdError::TooSmall {
            size,
            required: min_size,
        });
    }

    let mut last_malformed: Option<SacdError> = None;
    let mut buf = vec![0u8; MASTER_TOC_T_SIZE];
    let mut saw_any_magic = false;

    for &lsn in MASTER_TOC_LSNS.iter() {
        let offset = lsn * SECTOR_SIZE;
        if offset + MASTER_TOC_T_SIZE as u64 > size {
            continue;
        }
        if f.seek(SeekFrom::Start(offset)).is_err() {
            continue;
        }
        if f.read_exact(&mut buf).is_err() {
            continue;
        }
        if &buf[0..8] != MASTER_TOC_MAGIC {
            continue;
        }
        saw_any_magic = true;
        match parse_master_toc(&buf) {
            Ok(toc) => return Ok((lsn, toc)),
            Err(e) => last_malformed = Some(e),
        }
    }

    if let Some(e) = last_malformed {
        return Err(e);
    }
    if saw_any_magic {
        return Err(SacdError::Malformed(
            "master TOC: parse loop exited without result".into(),
        ));
    }
    Err(SacdError::NotSacdIso)
}

fn parse_area_with_strictness(
    path: &Path,
    ptr: AreaPointer,
    strict: bool,
) -> Result<Option<AreaInfo>, SacdError> {
    if !ptr.is_present() {
        return Ok(None);
    }
    match parse_area(path, ptr) {
        Ok(a) => Ok(Some(a)),
        Err(e) if strict => Err(e),
        Err(_) => Ok(None),
    }
}

/// Parse one area: load its TOC sector(s), decode the header, then
/// scan the remaining sectors of the area TOC for SACDTRL1 (track
/// LSNs), SACDTRL2 (track times), SACDTTxt (per-track text), and
/// SACD_IGL (ISRC + per-track genre). SACD_ACC sectors are skipped
/// (32-sector span). Only the *first* SACDTTxt sector is parsed
/// (primary locale), matching sacd-extract behaviour.
///
/// Redundancy: if `ptr.toc_1_start` is unreadable or the parsed
/// header is malformed, falls back to `ptr.toc_2_start` (the backup
/// copy of the area TOC) when present. This mirrors what
/// sacd-extract does in `scarletbook_read.c` and is what real
/// players do when reading scratched discs.
pub fn parse_area(path: &Path, ptr: AreaPointer) -> Result<AreaInfo, SacdError> {
    // Try toc_1 first; on failure, fall back to toc_2 when set.
    let (header, header_start_lsn) = match try_area_header_at(path, ptr.toc_1_start as u64) {
        Ok(h) => (h, ptr.toc_1_start as u64),
        Err(primary_err) => {
            if ptr.toc_2_start == 0 {
                return Err(primary_err);
            }
            match try_area_header_at(path, ptr.toc_2_start as u64) {
                Ok(h) => (h, ptr.toc_2_start as u64),
                // Both copies failed — surface the *primary* error
                // since it's the one the user expected to work.
                Err(_) => return Err(primary_err),
            }
        }
    };
    let area_charset = header.locales.first().map(|l| l.character_set).unwrap_or(0);

    let mut starts: Option<Vec<(u32, u32)>> = None;
    let mut times: Option<Vec<(PlayTime, PlayTime)>> = None;
    let mut text_per_track: Vec<TrackText> =
        vec![TrackText::default(); header.track_count as usize];
    let mut isrc_genre: Vec<TrackIsrcGenre> =
        vec![TrackIsrcGenre::default(); header.track_count as usize];
    let mut got_text = false;

    // Walk sectors after the header. Cap at 96 (MAX_AREA_TOC_SIZE_LSN
    // per spec). `i` advances by 1, 2, or 32 depending on the magic
    // we just consumed (SACD_IGL spans 2 sectors; SACD_ACC spans 32).
    let max_scan = (header.size_sectors as u64).min(96);
    let mut i: u64 = 1;
    while i < max_scan {
        let lsn = header_start_lsn + i;
        let buf = match read_sector(path, lsn) {
            Ok(b) => b,
            Err(_) => break,
        };
        match &buf[0..8] {
            m if m == SACD_TRL1_MAGIC => {
                if let Ok(v) = parse_trl1(&buf, header.track_count) {
                    starts = Some(v);
                }
                i += 1;
            }
            m if m == SACD_TRL2_MAGIC => {
                if let Ok(v) = parse_trl2(&buf, header.track_count) {
                    times = Some(v);
                }
                i += 1;
            }
            m if m == SACD_T_TXT_MAGIC => {
                if !got_text {
                    if let Ok(v) = parse_sacd_t_txt(&buf, header.track_count, area_charset) {
                        text_per_track = v;
                        got_text = true;
                    }
                }
                i += 1;
            }
            m if m == SACD_IGL_MAGIC => {
                // SACD_IGL spans 2 sectors. Concatenate before parse.
                let next_lsn = lsn + 1;
                if let Ok(buf2) = read_sector(path, next_lsn) {
                    let mut full = Vec::with_capacity(2 * SECTOR_SIZE as usize);
                    full.extend_from_slice(&buf);
                    full.extend_from_slice(&buf2);
                    if let Ok(v) = parse_sacd_igl(&full, header.track_count) {
                        isrc_genre = v;
                    }
                }
                i += SACD_IGL_SECTOR_SPAN;
            }
            m if m == SACD_ACC_MAGIC => {
                // 32-sector span; skip past so subsequent sectors
                // are interpreted by magic, not as access-list data.
                i += SACD_ACC_SECTOR_SPAN;
            }
            _ => i += 1,
        }
    }

    let mut tracks = Vec::with_capacity(header.track_count as usize);
    for i in 0..header.track_count as usize {
        let (start_lsn, length_lsn) = starts
            .as_ref()
            .and_then(|v| v.get(i))
            .copied()
            .unwrap_or((0, 0));
        let (start_time, duration) = times
            .as_ref()
            .and_then(|v| v.get(i))
            .copied()
            .unwrap_or_default();
        let text = text_per_track.get(i).cloned().unwrap_or_default();
        let ig = isrc_genre.get(i).cloned().unwrap_or_default();
        tracks.push(TrackEntry {
            start_lsn,
            length_lsn,
            start_time,
            duration,
            text,
            isrc: ig.isrc,
            genre: ig.genre,
        });
    }

    Ok(AreaInfo { header, tracks })
}

/// Read a single 2048-byte sector at `lsn` from `path` into a fresh
/// Vec.
/// Read and parse an area TOC header from the given LSN. Returns
/// Err if the read fails or the magic / structure is malformed.
/// Used by `parse_area` with toc_1 then (on failure) toc_2.
fn try_area_header_at(path: &Path, lsn: u64) -> Result<AreaTocHeader, SacdError> {
    let buf = read_sector(path, lsn)?;
    parse_area_toc_header(&buf)
}

fn read_sector(path: &Path, lsn: u64) -> Result<Vec<u8>, SacdError> {
    use std::fs::File;
    use std::io::{Read, Seek, SeekFrom};

    let mut f = File::open(path).map_err(|e| SacdError::Io(format!("open: {}", e)))?;
    let offset = lsn * SECTOR_SIZE;
    f.seek(SeekFrom::Start(offset))
        .map_err(|e| SacdError::Io(format!("seek to LSN {}: {}", lsn, e)))?;
    let mut buf = vec![0u8; SECTOR_SIZE as usize];
    f.read_exact(&mut buf)
        .map_err(|e| SacdError::Io(format!("read LSN {}: {}", lsn, e)))?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Build a synthetic ISO file at `path` with `MASTER_TOC_MAGIC`
    /// placed at the given LSNs. Other LSNs get zero bytes.
    fn write_synthetic_iso(path: &std::path::Path, magic_at_lsns: &[u64]) -> std::io::Result<()> {
        // Build a file that covers up through the highest LSN that
        // any test might care about. We size it to cover LSN 530 + a
        // sector of slack.
        let max_lsn = MASTER_TOC_LSNS.iter().max().copied().unwrap_or(530);
        let total_size = (max_lsn + 1) * SECTOR_SIZE;
        let mut f = std::fs::File::create(path)?;
        // Write zeros up to the file size first by setting len.
        f.set_len(total_size)?;
        // Now write magic at each requested LSN.
        use std::io::Seek;
        for &lsn in magic_at_lsns {
            f.seek(std::io::SeekFrom::Start(lsn * SECTOR_SIZE))?;
            f.write_all(MASTER_TOC_MAGIC)?;
        }
        Ok(())
    }

    #[test]
    fn is_sacd_iso_returns_true_when_first_toc_has_magic() {
        let td = tempfile::tempdir().expect("tempdir");
        let path = td.path().join("a.iso");
        write_synthetic_iso(&path, &[510]).expect("write");
        assert!(is_sacd_iso(&path));
    }

    #[test]
    fn is_sacd_iso_falls_back_to_redundant_copies() {
        // First copy missing; second copy present.
        let td = tempfile::tempdir().expect("tempdir");
        let path = td.path().join("b.iso");
        write_synthetic_iso(&path, &[520]).expect("write");
        assert!(
            is_sacd_iso(&path),
            "should accept TOC at LSN 520 when 510 is bare"
        );

        // Only third copy present.
        let path = td.path().join("c.iso");
        write_synthetic_iso(&path, &[530]).expect("write");
        assert!(
            is_sacd_iso(&path),
            "should accept TOC at LSN 530 when 510/520 are bare"
        );
    }

    #[test]
    fn is_sacd_iso_returns_false_when_no_magic_at_any_lsn() {
        let td = tempfile::tempdir().expect("tempdir");
        let path = td.path().join("d.iso");
        write_synthetic_iso(&path, &[]).expect("write");
        assert!(!is_sacd_iso(&path));
    }

    #[test]
    fn is_sacd_iso_returns_false_for_too_small_file() {
        let td = tempfile::tempdir().expect("tempdir");
        let path = td.path().join("e.iso");
        let mut f = std::fs::File::create(&path).expect("create");
        f.write_all(MASTER_TOC_MAGIC).expect("write tiny file");
        assert!(
            !is_sacd_iso(&path),
            "8-byte file is too small to host Master TOC"
        );
    }

    #[test]
    fn is_sacd_iso_returns_false_for_nonexistent_path() {
        assert!(!is_sacd_iso(std::path::Path::new("/nonexistent/sacd.iso")));
    }

    #[test]
    fn detect_sacd_iso_distinguishes_full_vs_partial_redundancy() {
        let td = tempfile::tempdir().expect("tempdir");
        let path = td.path().join("full.iso");
        write_synthetic_iso(&path, &[510, 520, 530]).expect("write");
        assert_eq!(detect_sacd_iso(&path), DetectionResult::HealthyAllRedundant);

        let path = td.path().join("partial.iso");
        write_synthetic_iso(&path, &[510, 530]).expect("write");
        assert_eq!(
            detect_sacd_iso(&path),
            DetectionResult::HealthyPartialRedundant { good: 2 },
        );

        let path = td.path().join("none.iso");
        write_synthetic_iso(&path, &[]).expect("write");
        assert_eq!(detect_sacd_iso(&path), DetectionResult::NotSacd);

        let path = td.path().join("tiny.iso");
        std::fs::write(&path, b"x").expect("write");
        assert_eq!(detect_sacd_iso(&path), DetectionResult::TooSmall);

        assert_eq!(
            detect_sacd_iso(std::path::Path::new("/nonexistent/sacd.iso")),
            DetectionResult::IoFailure,
        );
    }

    /// Build a 168-byte master_toc_t buffer with sane defaults (magic
    /// + version 1.20 + 2-channel area present at LSN 540, size 1
    /// sector). Tests then mutate specific offsets to exercise edge
    /// cases without re-stating every field.
    fn baseline_master_toc_buf() -> Vec<u8> {
        let mut b = vec![0u8; MASTER_TOC_T_SIZE];
        b[0..8].copy_from_slice(MASTER_TOC_MAGIC);
        b[0x08] = 1; // major
        b[0x09] = 20; // minor
                      // album_set_size = 1, sequence = 1
        b[0x10..0x12].copy_from_slice(&1u16.to_be_bytes());
        b[0x12..0x14].copy_from_slice(&1u16.to_be_bytes());
        // 2-ch area at LSN 540, 1-sector TOC, backup at 541
        b[0x40..0x44].copy_from_slice(&540u32.to_be_bytes());
        b[0x44..0x48].copy_from_slice(&541u32.to_be_bytes());
        b[0x54..0x56].copy_from_slice(&1u16.to_be_bytes());
        b
    }

    #[test]
    fn parse_master_toc_minimal_valid() {
        let b = baseline_master_toc_buf();
        let toc = parse_master_toc(&b).expect("parse");
        assert_eq!(toc.spec_version, (1, 20));
        assert_eq!(toc.album_set_size, 1);
        assert_eq!(toc.album_sequence_number, 1);
        assert!(toc.two_channel.is_present());
        assert_eq!(toc.two_channel.toc_1_start, 540);
        assert_eq!(toc.two_channel.toc_2_start, 541);
        assert_eq!(toc.two_channel.toc_size_sectors, 1);
        assert!(!toc.multi_channel.is_present());
        assert!(!toc.disc_type_hybrid);
        assert!(toc.album_genres.is_empty());
        assert!(toc.disc_date.is_none());
        assert_eq!(toc.text_area_count, 0);
    }

    #[test]
    fn parse_master_toc_extracts_fields() {
        let mut b = baseline_master_toc_buf();
        // catalog numbers
        b[0x18..0x28].copy_from_slice(b"PROC-12345-D    ");
        b[0x58..0x68].copy_from_slice(b"DISCCAT-001\0\0\0\0\0");
        // multi-channel area: LSN 600, backup 601, 2-sector TOC
        b[0x48..0x4c].copy_from_slice(&600u32.to_be_bytes());
        b[0x4c..0x50].copy_from_slice(&601u32.to_be_bytes());
        b[0x56..0x58].copy_from_slice(&2u16.to_be_bytes());
        // hybrid disc (bit 7 of disc_type byte)
        b[0x50] = 0x80;
        // album genre slot 0: category=1 (GENERAL), genre=14 (JAZZ)
        b[0x28] = 1;
        b[0x2b] = 14;
        // disc genre slot 0: category=1, genre=23 (ROCK)
        b[0x68] = 1;
        b[0x6b] = 23;
        // date 2003-08-15
        b[0x78..0x7a].copy_from_slice(&2003u16.to_be_bytes());
        b[0x7a] = 8;
        b[0x7b] = 15;
        // text_area_count = 2, locales[0]="en"/2 ISO-8859-1, [1]="ja"/3 Shift-JIS
        b[0x80] = 2;
        b[0x88] = b'e';
        b[0x89] = b'n';
        b[0x8a] = 2;
        b[0x8c] = b'j';
        b[0x8d] = b'a';
        b[0x8e] = 3;

        let toc = parse_master_toc(&b).expect("parse");
        assert_eq!(toc.album_catalog_number, "PROC-12345-D");
        assert_eq!(toc.disc_catalog_number, "DISCCAT-001");
        assert!(toc.multi_channel.is_present());
        assert_eq!(toc.multi_channel.toc_1_start, 600);
        assert_eq!(toc.multi_channel.toc_size_sectors, 2);
        assert!(toc.disc_type_hybrid);
        assert_eq!(toc.album_genres.len(), 1);
        assert_eq!(toc.album_genres[0].genre, 14);
        assert_eq!(toc.album_genres[0].name(), "Jazz");
        assert_eq!(toc.disc_genres[0].name(), "Rock");
        assert_eq!(
            toc.disc_date,
            Some(DiscDate {
                year: 2003,
                month: 8,
                day: 15
            })
        );
        assert_eq!(toc.text_area_count, 2);
        assert_eq!(toc.locales[0].language_code, [b'e', b'n']);
        assert_eq!(toc.locales[0].character_set, 2);
        assert_eq!(toc.locales[1].language_code, [b'j', b'a']);
        assert_eq!(toc.locales[1].character_set, 3);
    }

    #[test]
    fn parse_master_toc_rejects_no_areas() {
        let mut b = baseline_master_toc_buf();
        b[0x40..0x44].copy_from_slice(&0u32.to_be_bytes());
        b[0x54..0x56].copy_from_slice(&0u16.to_be_bytes());
        match parse_master_toc(&b) {
            Err(SacdError::Malformed(m)) => assert!(m.contains("no playable areas")),
            other => panic!("expected Malformed, got {:?}", other),
        }
    }

    #[test]
    fn parse_master_toc_rejects_oversized_text_area_count() {
        let mut b = baseline_master_toc_buf();
        b[0x80] = 9;
        match parse_master_toc(&b) {
            Err(SacdError::Malformed(m)) => assert!(m.contains("text_area_count")),
            other => panic!("expected Malformed, got {:?}", other),
        }
    }

    #[test]
    fn parse_master_toc_rejects_short_buffer() {
        let b = vec![0u8; 50];
        match parse_master_toc(&b) {
            Err(SacdError::Malformed(m)) => assert!(m.contains("too short")),
            other => panic!("expected Malformed, got {:?}", other),
        }
    }

    #[test]
    fn parse_master_toc_rejects_wrong_magic() {
        let mut b = baseline_master_toc_buf();
        b[0..8].copy_from_slice(b"NOTSACD!");
        assert!(matches!(parse_master_toc(&b), Err(SacdError::NotSacdIso)));
    }

    #[test]
    fn read_master_toc_falls_back_through_redundant_copies() {
        use std::io::{Seek, SeekFrom, Write};
        let td = tempfile::tempdir().expect("tempdir");
        let path = td.path().join("redundant.iso");
        // File large enough to hold all 3 master TOC copies.
        let total = (MASTER_TOC_LSNS[2] + 1) * SECTOR_SIZE;
        let mut f = std::fs::File::create(&path).expect("create");
        f.set_len(total).expect("set_len");

        // Corrupt master TOC at LSN 510 (wrong magic).
        f.seek(SeekFrom::Start(MASTER_TOC_LSNS[0] * SECTOR_SIZE))
            .unwrap();
        f.write_all(b"NOTSACD!").unwrap();
        // Valid master TOC at LSN 520.
        f.seek(SeekFrom::Start(MASTER_TOC_LSNS[1] * SECTOR_SIZE))
            .unwrap();
        f.write_all(&baseline_master_toc_buf()).unwrap();
        drop(f);

        let toc = read_master_toc(&path).expect("read");
        assert_eq!(toc.spec_version, (1, 20));
        assert!(toc.two_channel.is_present());
    }

    #[test]
    fn read_master_toc_returns_not_sacd_when_no_magic() {
        let td = tempfile::tempdir().expect("tempdir");
        let path = td.path().join("blank.iso");
        let total = (MASTER_TOC_LSNS[2] + 1) * SECTOR_SIZE;
        let f = std::fs::File::create(&path).expect("create");
        f.set_len(total).expect("set_len");
        drop(f);
        assert!(matches!(read_master_toc(&path), Err(SacdError::NotSacdIso)));
    }

    #[test]
    fn read_fixed_ascii_trims_padding() {
        assert_eq!(read_fixed_ascii(b"ABC\0\0\0\0\0\0\0\0\0\0\0\0\0"), "ABC");
        assert_eq!(read_fixed_ascii(b"ABC             "), "ABC");
        assert_eq!(read_fixed_ascii(b"\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0"), "");
        assert_eq!(read_fixed_ascii(b"FULLWIDTH-CATALG"), "FULLWIDTH-CATALG");
    }

    #[test]
    fn sacd_error_display_messages() {
        let e = SacdError::TooSmall {
            size: 100,
            required: 1_044_488,
        };
        assert!(format!("{}", e).contains("100"));
        assert!(format!("{}", SacdError::NotSacdIso).contains("not a valid SACD"));
        assert!(format!("{}", SacdError::Io("test".into())).contains("test"));
        assert!(format!("{}", SacdError::Malformed("bad".into())).contains("bad"));
    }

    // ---------- C2b tests: SACDText / Area TOC / tracklists ----------

    /// Build a 2048-byte SACDText sector, place strings into the
    /// data region at known offsets, and return (buf, expected
    /// album_title_position).
    fn build_sacd_text_sector() -> (Vec<u8>, u16) {
        let mut b = vec![0u8; SECTOR_SIZE as usize];
        b[0..8].copy_from_slice(SACD_TEXT_MAGIC);
        // place "Hello" at offset 0x100, "World" at offset 0x110, NUL
        // terminated.
        let title_pos = 0x100u16;
        let artist_pos = 0x110u16;
        b[title_pos as usize..title_pos as usize + 5].copy_from_slice(b"Hello");
        b[artist_pos as usize..artist_pos as usize + 5].copy_from_slice(b"World");
        // album_title_position @ 0x10
        b[0x10..0x12].copy_from_slice(&title_pos.to_be_bytes());
        // album_artist_position @ 0x12
        b[0x12..0x14].copy_from_slice(&artist_pos.to_be_bytes());
        (b, title_pos)
    }

    #[test]
    fn parse_sacd_text_extracts_title_and_artist() {
        let (b, _pos) = build_sacd_text_sector();
        let t = parse_sacd_text(&b, 2).expect("parse"); // charset 2 = Latin-1
        assert_eq!(t.album_title.as_deref(), Some("Hello"));
        assert_eq!(t.album_artist.as_deref(), Some("World"));
        assert!(t.album_publisher.is_none());
        assert!(t.disc_title.is_none());
        assert_eq!(t.charset, 2);
    }

    #[test]
    fn parse_sacd_text_decodes_latin1_high_bytes() {
        let mut b = vec![0u8; SECTOR_SIZE as usize];
        b[0..8].copy_from_slice(SACD_TEXT_MAGIC);
        // "Café" in Latin-1: C=0x43 a=0x61 f=0x66 é=0xe9
        b[0x100..0x104].copy_from_slice(&[0x43, 0x61, 0x66, 0xe9]);
        b[0x10..0x12].copy_from_slice(&0x100u16.to_be_bytes());
        let t = parse_sacd_text(&b, 2).expect("parse");
        assert_eq!(t.album_title.as_deref(), Some("Café"));
    }

    #[test]
    fn parse_sacd_text_treats_zero_position_as_absent() {
        let mut b = vec![0u8; SECTOR_SIZE as usize];
        b[0..8].copy_from_slice(SACD_TEXT_MAGIC);
        // all positions left as 0
        let t = parse_sacd_text(&b, 1).expect("parse");
        assert!(t.album_title.is_none());
        assert!(t.disc_artist.is_none());
    }

    #[test]
    fn parse_sacd_text_rejects_wrong_magic() {
        let mut b = vec![0u8; SECTOR_SIZE as usize];
        b[0..8].copy_from_slice(b"NOTTEXT!");
        assert!(matches!(
            parse_sacd_text(&b, 1),
            Err(SacdError::Malformed(_))
        ));
    }

    #[test]
    fn decode_text_handles_known_charsets() {
        // ASCII (charset 1)
        assert_eq!(decode_text(b"abc", 1), "abc");
        // Latin-1 (charset 2): 0xe9 → é
        assert_eq!(decode_text(&[0xe9], 2), "é");
        // ISO 8859-1 with escapes (charset 7) → same as Latin-1 path
        assert_eq!(decode_text(&[0xe9], 7), "é");
        // Unknown charset (3, Shift-JIS): from_utf8_lossy fallback
        let s = decode_text(&[0xe9, 0xc1], 3);
        assert!(s.contains('\u{FFFD}') || !s.is_empty());
    }

    /// Build a minimal valid TWOCHTOC sector with `track_count`
    /// tracks and DSD64 / uncompressed-DSD format.
    fn build_area_toc_sector(magic: &[u8; 8], track_count: u8, frame_format_nibble: u8) -> Vec<u8> {
        let mut b = vec![0u8; SECTOR_SIZE as usize];
        b[0..8].copy_from_slice(magic);
        b[0x08] = 1;
        b[0x09] = 20; // version 1.20
        b[0x0a..0x0c].copy_from_slice(&3u16.to_be_bytes()); // size = 3 sectors (header + TRL1 + TRL2)
        b[0x10..0x14].copy_from_slice(&64_000u32.to_be_bytes()); // max_byte_rate
        b[0x14] = 0x04; // sample_frequency = DSD64
        b[0x15] = frame_format_nibble & 0x0f; // frame_format
        b[0x20] = if magic == MULCH_TOC_MAGIC { 6 } else { 2 }; // channel_count
        b[0x21] = (5u8 << 3) | 0; // loudspeaker_config = 5 (5.1), extra_settings = 0
        b[0x22] = b[0x20]; // max_available_channels
        b[0x40] = 45; // total_playtime: 45 minutes
        b[0x41] = 12; //                 12 seconds
        b[0x42] = 30; //                 30 frames
        b[0x44] = 0; // track_offset
        b[0x45] = track_count;
        b[0x48..0x4c].copy_from_slice(&540u32.to_be_bytes()); // track_start LSN
        b[0x4c..0x50].copy_from_slice(&100_000u32.to_be_bytes()); // track_end
        b[0x50] = 1; // text_area_count
                     // locale 0: "en", charset 2 (Latin-1)
        b[0x58] = b'e';
        b[0x59] = b'n';
        b[0x5a] = 2;
        // area description at offset 0x500 in the data region:
        // "5.1 Multi-channel"
        let desc_off = 0x500u16;
        let desc = b"5.1 Multi-channel";
        b[desc_off as usize..desc_off as usize + desc.len()].copy_from_slice(desc);
        b[0x90..0x92].copy_from_slice(&desc_off.to_be_bytes());
        b
    }

    #[test]
    fn parse_area_toc_header_extracts_two_channel() {
        let b = build_area_toc_sector(TWOCH_TOC_MAGIC, 12, 2 /* DSD3in14 */);
        let h = parse_area_toc_header(&b).expect("parse");
        assert_eq!(h.kind, AreaKind::Stereo);
        assert_eq!(h.spec_version, (1, 20));
        assert_eq!(h.size_sectors, 3);
        assert_eq!(h.max_byte_rate, 64_000);
        assert_eq!(h.sample_frequency, 0x04);
        assert_eq!(h.frame_format, FrameFormat::Dsd3In14);
        assert!(!h.frame_format.is_dst_encoded());
        assert_eq!(h.channel_count, 2);
        assert_eq!(h.loudspeaker_config, 5);
        assert_eq!(h.track_count, 12);
        assert_eq!(h.track_start_lsn, 540);
        assert_eq!(h.track_end_lsn, 100_000);
        assert_eq!(
            h.total_playtime,
            PlayTime {
                minutes: 45,
                seconds: 12,
                frames: 30
            }
        );
        assert!(
            (h.total_playtime.total_seconds() - (45.0 * 60.0 + 12.0 + 30.0 / 75.0)).abs() < 1e-9
        );
        assert_eq!(h.description.as_deref(), Some("5.1 Multi-channel"));
        assert_eq!(h.text_area_count, 1);
        assert_eq!(h.locales[0].language_code, [b'e', b'n']);
        assert_eq!(h.locales[0].character_set, 2);
    }

    #[test]
    fn parse_area_toc_header_extracts_multi_channel_dst() {
        let b = build_area_toc_sector(MULCH_TOC_MAGIC, 8, 0 /* DST */);
        let h = parse_area_toc_header(&b).expect("parse");
        assert_eq!(h.kind, AreaKind::MultiChannel);
        assert_eq!(h.frame_format, FrameFormat::Dst);
        assert!(h.frame_format.is_dst_encoded());
        assert_eq!(h.channel_count, 6);
    }

    #[test]
    fn parse_area_toc_header_unknown_frame_format() {
        let b = build_area_toc_sector(TWOCH_TOC_MAGIC, 1, 7 /* not in spec */);
        let h = parse_area_toc_header(&b).expect("parse");
        assert_eq!(h.frame_format, FrameFormat::Unknown(7));
    }

    #[test]
    fn parse_area_toc_header_rejects_wrong_magic() {
        let mut b = vec![0u8; SECTOR_SIZE as usize];
        b[0..8].copy_from_slice(b"NOTAREAS");
        assert!(matches!(
            parse_area_toc_header(&b),
            Err(SacdError::Malformed(_))
        ));
    }

    #[test]
    fn parse_area_toc_header_rejects_oversized_text_area_count() {
        let mut b = build_area_toc_sector(TWOCH_TOC_MAGIC, 1, 2);
        b[0x50] = 11;
        assert!(matches!(
            parse_area_toc_header(&b),
            Err(SacdError::Malformed(_))
        ));
    }

    fn build_trl1_sector(starts_lengths: &[(u32, u32)]) -> Vec<u8> {
        let mut b = vec![0u8; SECTOR_SIZE as usize];
        b[0..8].copy_from_slice(SACD_TRL1_MAGIC);
        let start_base = 8;
        let len_base = 8 + 255 * 4;
        for (i, &(s, l)) in starts_lengths.iter().enumerate() {
            b[start_base + i * 4..start_base + i * 4 + 4].copy_from_slice(&s.to_be_bytes());
            b[len_base + i * 4..len_base + i * 4 + 4].copy_from_slice(&l.to_be_bytes());
        }
        b
    }

    #[test]
    fn parse_trl1_decodes_track_lsns() {
        let b = build_trl1_sector(&[(540, 1000), (1540, 2000), (3540, 1500)]);
        let v = parse_trl1(&b, 3).expect("parse");
        assert_eq!(v, vec![(540, 1000), (1540, 2000), (3540, 1500)]);
    }

    #[test]
    fn parse_trl1_rejects_wrong_magic() {
        let mut b = vec![0u8; SECTOR_SIZE as usize];
        b[0..8].copy_from_slice(b"WRONG!!!");
        assert!(matches!(parse_trl1(&b, 1), Err(SacdError::Malformed(_))));
    }

    fn build_trl2_sector(times: &[(PlayTime, PlayTime)]) -> Vec<u8> {
        let mut b = vec![0u8; SECTOR_SIZE as usize];
        b[0..8].copy_from_slice(SACD_TRL2_MAGIC);
        let start_base = 8;
        let dur_base = 8 + 255 * 4;
        for (i, (s, d)) in times.iter().enumerate() {
            b[start_base + i * 4 + 0] = s.minutes;
            b[start_base + i * 4 + 1] = s.seconds;
            b[start_base + i * 4 + 2] = s.frames;
            b[dur_base + i * 4 + 0] = d.minutes;
            b[dur_base + i * 4 + 1] = d.seconds;
            b[dur_base + i * 4 + 2] = d.frames;
        }
        b
    }

    #[test]
    fn parse_trl2_decodes_track_times() {
        let times = vec![
            (
                PlayTime {
                    minutes: 0,
                    seconds: 0,
                    frames: 0,
                },
                PlayTime {
                    minutes: 3,
                    seconds: 45,
                    frames: 60,
                },
            ),
            (
                PlayTime {
                    minutes: 3,
                    seconds: 45,
                    frames: 60,
                },
                PlayTime {
                    minutes: 4,
                    seconds: 12,
                    frames: 0,
                },
            ),
        ];
        let b = build_trl2_sector(&times);
        let v = parse_trl2(&b, 2).expect("parse");
        assert_eq!(v.len(), 2);
        assert_eq!(
            v[0].0,
            PlayTime {
                minutes: 0,
                seconds: 0,
                frames: 0
            }
        );
        assert_eq!(
            v[0].1,
            PlayTime {
                minutes: 3,
                seconds: 45,
                frames: 60
            }
        );
        assert_eq!(v[1].1.total_seconds(), 4.0 * 60.0 + 12.0);
    }

    /// End-to-end test: write a synthetic SACD ISO with master TOC,
    /// SACDText, one stereo area + TRL1 + TRL2, and verify
    /// parse_sacd_iso assembles a correct SacdMetadata.
    #[test]
    fn parse_sacd_iso_full_roundtrip_stereo_only() {
        use std::io::{Seek, SeekFrom, Write};
        let td = tempfile::tempdir().expect("tempdir");
        let path = td.path().join("rt.iso");

        // File big enough to hold sectors up to LSN 600 + a few.
        let total_sectors = 700u64;
        let f = std::fs::File::create(&path).expect("create");
        f.set_len(total_sectors * SECTOR_SIZE).expect("set_len");
        drop(f);

        let mut f = std::fs::File::options()
            .write(true)
            .open(&path)
            .expect("reopen");

        // Master TOC at LSN 510 — area at LSN 540, size = 3 sectors.
        let mut mtoc = vec![0u8; MASTER_TOC_T_SIZE];
        mtoc[0..8].copy_from_slice(MASTER_TOC_MAGIC);
        mtoc[0x08] = 1;
        mtoc[0x09] = 20;
        mtoc[0x10..0x12].copy_from_slice(&1u16.to_be_bytes()); // album_set_size
        mtoc[0x12..0x14].copy_from_slice(&1u16.to_be_bytes()); // album_seq
        mtoc[0x40..0x44].copy_from_slice(&540u32.to_be_bytes()); // 2-ch toc_1
        mtoc[0x54..0x56].copy_from_slice(&3u16.to_be_bytes()); // 2-ch size
        mtoc[0x80] = 1; // text_area_count = 1
        mtoc[0x88] = b'e';
        mtoc[0x89] = b'n';
        mtoc[0x8a] = 2; // locale "en"/Latin-1
        f.seek(SeekFrom::Start(510 * SECTOR_SIZE)).unwrap();
        f.write_all(&mtoc).unwrap();

        // SACDText at LSN 511 (master_toc_lsn + 1).
        let (text_buf, _) = build_sacd_text_sector();
        f.seek(SeekFrom::Start(511 * SECTOR_SIZE)).unwrap();
        f.write_all(&text_buf).unwrap();

        // Area TOC at LSN 540 (2-ch, 3 tracks, DSD3in14).
        let area_buf = build_area_toc_sector(TWOCH_TOC_MAGIC, 3, 2);
        f.seek(SeekFrom::Start(540 * SECTOR_SIZE)).unwrap();
        f.write_all(&area_buf).unwrap();

        // SACDTRL1 at LSN 541 (next sector of area).
        let trl1 = build_trl1_sector(&[(600, 100), (700, 150), (850, 200)]);
        f.seek(SeekFrom::Start(541 * SECTOR_SIZE)).unwrap();
        f.write_all(&trl1).unwrap();

        // SACDTRL2 at LSN 542.
        let trl2 = build_trl2_sector(&[
            (
                PlayTime {
                    minutes: 0,
                    seconds: 0,
                    frames: 0,
                },
                PlayTime {
                    minutes: 1,
                    seconds: 30,
                    frames: 0,
                },
            ),
            (
                PlayTime {
                    minutes: 1,
                    seconds: 30,
                    frames: 0,
                },
                PlayTime {
                    minutes: 2,
                    seconds: 15,
                    frames: 0,
                },
            ),
            (
                PlayTime {
                    minutes: 3,
                    seconds: 45,
                    frames: 0,
                },
                PlayTime {
                    minutes: 1,
                    seconds: 0,
                    frames: 0,
                },
            ),
        ]);
        f.seek(SeekFrom::Start(542 * SECTOR_SIZE)).unwrap();
        f.write_all(&trl2).unwrap();
        drop(f);

        let md = parse_sacd_iso(&path).expect("parse_sacd_iso");

        // Master TOC
        assert_eq!(md.master_toc.spec_version, (1, 20));
        assert!(md.master_toc.two_channel.is_present());
        assert!(!md.master_toc.multi_channel.is_present());

        // Master text
        assert_eq!(md.album_title(), Some("Hello"));
        assert_eq!(md.album_artist(), Some("World"));

        // Stereo area
        let stereo = md.stereo.as_ref().expect("stereo present");
        assert_eq!(stereo.header.kind, AreaKind::Stereo);
        assert_eq!(stereo.header.track_count, 3);
        assert_eq!(stereo.header.frame_format, FrameFormat::Dsd3In14);
        assert!(!md.any_dst_encoded());
        assert_eq!(stereo.tracks.len(), 3);
        assert_eq!(stereo.tracks[0].start_lsn, 600);
        assert_eq!(stereo.tracks[0].length_lsn, 100);
        assert_eq!(stereo.tracks[1].start_lsn, 700);
        assert_eq!(stereo.tracks[1].duration.total_seconds(), 2.0 * 60.0 + 15.0);
        assert_eq!(stereo.tracks[2].start_time.minutes, 3);

        // No multi-channel area
        assert!(md.multi_channel.is_none());
    }

    // ---------- C2c tests: SACDTTxt + SACD_IGL + integrated parse_area ----------

    /// Build a single per-track text block in the format SACDTTxt
    /// uses inside its sector. `entries` is a slice of (track_type,
    /// optional string). Each entry is laid out as:
    ///   [type:u8][0x20:u8][string-bytes-or-nothing][NUL]
    /// A trailing NUL terminates the last string. Between entries
    /// sacd-extract walks through extra NULs as padding; we emit
    /// exactly one NUL between entries which is the minimum legal
    /// shape (the spec doesn't define a fixed inter-entry padding).
    fn build_track_text_block(entries: &[(u8, Option<&[u8]>)]) -> Vec<u8> {
        let mut out = Vec::new();
        // track_amount byte + 3 unknown
        out.push(entries.len() as u8);
        out.extend_from_slice(&[0u8; 3]);
        for &(ttype, s) in entries {
            out.push(ttype);
            out.push(0x20);
            if let Some(bytes) = s {
                out.extend_from_slice(bytes);
            }
            out.push(0); // NUL terminator (or marker for empty string)
        }
        out
    }

    #[test]
    fn parse_sacd_t_txt_extracts_title_and_performer() {
        let mut buf = vec![0u8; SECTOR_SIZE as usize];
        buf[0..8].copy_from_slice(SACD_T_TXT_MAGIC);
        // 2 tracks; positions at 0x100, 0x200.
        buf[0x08..0x0a].copy_from_slice(&0x100u16.to_be_bytes());
        buf[0x0a..0x0c].copy_from_slice(&0x200u16.to_be_bytes());
        // Track 0: TITLE="Hello", PERFORMER="World"
        let block0 = build_track_text_block(&[(0x01, Some(b"Hello")), (0x02, Some(b"World"))]);
        buf[0x100..0x100 + block0.len()].copy_from_slice(&block0);
        // Track 1: TITLE="Solo"
        let block1 = build_track_text_block(&[(0x01, Some(b"Solo"))]);
        buf[0x200..0x200 + block1.len()].copy_from_slice(&block1);

        let v = parse_sacd_t_txt(&buf, 2, 2 /* Latin-1 */).expect("parse");
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].title.as_deref(), Some("Hello"));
        assert_eq!(v[0].performer.as_deref(), Some("World"));
        assert!(v[0].composer.is_none());
        assert_eq!(v[1].title.as_deref(), Some("Solo"));
        assert!(v[1].performer.is_none());
    }

    #[test]
    fn parse_sacd_t_txt_extracts_phonetic_variants() {
        let mut buf = vec![0u8; SECTOR_SIZE as usize];
        buf[0..8].copy_from_slice(SACD_T_TXT_MAGIC);
        buf[0x08..0x0a].copy_from_slice(&0x100u16.to_be_bytes());
        // Title + phonetic-title + performer + phonetic-performer
        let block = build_track_text_block(&[
            (0x01, Some(b"Title")),
            (0x81, Some(b"TitlePhon")),
            (0x02, Some(b"Performer")),
            (0x82, Some(b"PerfPhon")),
        ]);
        buf[0x100..0x100 + block.len()].copy_from_slice(&block);

        let v = parse_sacd_t_txt(&buf, 1, 2).expect("parse");
        assert_eq!(v[0].title.as_deref(), Some("Title"));
        assert_eq!(v[0].title_phonetic.as_deref(), Some("TitlePhon"));
        assert_eq!(v[0].performer.as_deref(), Some("Performer"));
        assert_eq!(v[0].performer_phonetic.as_deref(), Some("PerfPhon"));
    }

    #[test]
    fn parse_sacd_t_txt_skips_unknown_track_types() {
        let mut buf = vec![0u8; SECTOR_SIZE as usize];
        buf[0..8].copy_from_slice(SACD_T_TXT_MAGIC);
        buf[0x08..0x0a].copy_from_slice(&0x100u16.to_be_bytes());
        let block = build_track_text_block(&[
            (0x01, Some(b"Title")),
            (0x55, Some(b"GarbageType")), // unknown — must be ignored, not panic
            (0x02, Some(b"Perf")),
        ]);
        buf[0x100..0x100 + block.len()].copy_from_slice(&block);
        let v = parse_sacd_t_txt(&buf, 1, 2).expect("parse");
        assert_eq!(v[0].title.as_deref(), Some("Title"));
        assert_eq!(v[0].performer.as_deref(), Some("Perf"));
    }

    #[test]
    fn parse_sacd_t_txt_handles_zero_position() {
        let mut buf = vec![0u8; SECTOR_SIZE as usize];
        buf[0..8].copy_from_slice(SACD_T_TXT_MAGIC);
        // Both positions 0 — both tracks should yield default TrackText.
        let v = parse_sacd_t_txt(&buf, 2, 2).expect("parse");
        assert_eq!(v.len(), 2);
        assert!(v[0].title.is_none());
        assert!(v[1].title.is_none());
    }

    #[test]
    fn parse_sacd_t_txt_rejects_wrong_magic() {
        let mut buf = vec![0u8; SECTOR_SIZE as usize];
        buf[0..8].copy_from_slice(b"WRONGTXT");
        assert!(matches!(
            parse_sacd_t_txt(&buf, 1, 2),
            Err(SacdError::Malformed(_))
        ));
    }

    #[test]
    fn parse_sacd_t_txt_decodes_latin1() {
        let mut buf = vec![0u8; SECTOR_SIZE as usize];
        buf[0..8].copy_from_slice(SACD_T_TXT_MAGIC);
        buf[0x08..0x0a].copy_from_slice(&0x100u16.to_be_bytes());
        // "Beyoncé" = B(0x42) e(0x65) y(0x79) o(0x6f) n(0x6e) c(0x63) é(0xe9)
        let block =
            build_track_text_block(&[(0x02, Some(&[0x42, 0x65, 0x79, 0x6f, 0x6e, 0x63, 0xe9]))]);
        buf[0x100..0x100 + block.len()].copy_from_slice(&block);
        let v = parse_sacd_t_txt(&buf, 1, 2).expect("parse");
        assert_eq!(v[0].performer.as_deref(), Some("Beyoncé"));
    }

    /// Build a 4096-byte SACD_IGL buffer (two sectors concatenated)
    /// with given (isrc, optional genre) pairs per track.
    fn build_sacd_igl_buf(rows: &[(Option<&str>, Option<(u8, u8)>)]) -> Vec<u8> {
        let mut buf = vec![0u8; 2 * SECTOR_SIZE as usize];
        buf[0..8].copy_from_slice(SACD_IGL_MAGIC);
        let isrc_base = 8;
        let genre_base = 8 + 12 * 255 + 4;
        for (i, (isrc, genre)) in rows.iter().enumerate() {
            if let Some(s) = isrc {
                let bytes = s.as_bytes();
                let n = bytes.len().min(12);
                buf[isrc_base + i * 12..isrc_base + i * 12 + n].copy_from_slice(&bytes[..n]);
            }
            if let Some((category, code)) = genre {
                buf[genre_base + i * 4] = *category;
                buf[genre_base + i * 4 + 3] = *code;
            }
        }
        buf
    }

    #[test]
    fn parse_sacd_igl_extracts_isrc_and_genre() {
        let buf = build_sacd_igl_buf(&[
            (Some("USAA10800001"), Some((1, 14 /* JAZZ */))),
            (None, Some((1, 23 /* ROCK */))),
            (Some("GBABC2200042"), None),
        ]);
        let v = parse_sacd_igl(&buf, 3).expect("parse");
        assert_eq!(v.len(), 3);
        assert_eq!(v[0].isrc.as_deref(), Some("USAA10800001"));
        assert_eq!(v[0].genre.unwrap().name(), "Jazz");
        assert!(v[1].isrc.is_none());
        assert_eq!(v[1].genre.unwrap().name(), "Rock");
        assert_eq!(v[2].isrc.as_deref(), Some("GBABC2200042"));
        assert!(v[2].genre.is_none());
    }

    #[test]
    fn parse_sacd_igl_rejects_short_buffer() {
        let buf = vec![0u8; 100];
        assert!(matches!(
            parse_sacd_igl(&buf, 1),
            Err(SacdError::Malformed(_))
        ));
    }

    #[test]
    fn parse_sacd_igl_rejects_wrong_magic() {
        let mut buf = vec![0u8; 2 * SECTOR_SIZE as usize];
        buf[0..8].copy_from_slice(b"NOTIGL!!");
        assert!(matches!(
            parse_sacd_igl(&buf, 1),
            Err(SacdError::Malformed(_))
        ));
    }

    /// End-to-end test: ISO with master TOC + SACDText + area TOC +
    /// TRL1 + TRL2 + SACDTTxt + SACD_IGL all wired up. Verifies
    /// per-track titles, performers, ISRCs, and genres are surfaced.
    #[test]
    fn parse_sacd_iso_full_roundtrip_with_per_track_text_and_isrc() {
        use std::io::{Seek, SeekFrom, Write};
        let td = tempfile::tempdir().expect("tempdir");
        let path = td.path().join("rt2.iso");

        let total_sectors = 700u64;
        let f = std::fs::File::create(&path).expect("create");
        f.set_len(total_sectors * SECTOR_SIZE).expect("set_len");
        drop(f);

        let mut f = std::fs::File::options()
            .write(true)
            .open(&path)
            .expect("reopen");

        // Master TOC at LSN 510 — 2-ch area at LSN 540, size = 5 sectors
        // (header, TRL1, TRL2, TTxt, IGL[0]; IGL spans 2 so total 6 used,
        // but we set size to 6 and ensure scan covers all).
        let mut mtoc = vec![0u8; MASTER_TOC_T_SIZE];
        mtoc[0..8].copy_from_slice(MASTER_TOC_MAGIC);
        mtoc[0x08] = 1;
        mtoc[0x09] = 20;
        mtoc[0x10..0x12].copy_from_slice(&1u16.to_be_bytes());
        mtoc[0x12..0x14].copy_from_slice(&1u16.to_be_bytes());
        mtoc[0x40..0x44].copy_from_slice(&540u32.to_be_bytes());
        mtoc[0x54..0x56].copy_from_slice(&7u16.to_be_bytes()); // 7 sectors
        mtoc[0x80] = 1;
        mtoc[0x88] = b'e';
        mtoc[0x89] = b'n';
        mtoc[0x8a] = 2;
        f.seek(SeekFrom::Start(510 * SECTOR_SIZE)).unwrap();
        f.write_all(&mtoc).unwrap();

        let (text_buf, _) = build_sacd_text_sector();
        f.seek(SeekFrom::Start(511 * SECTOR_SIZE)).unwrap();
        f.write_all(&text_buf).unwrap();

        // Area TOC at LSN 540 (override size_sectors=7).
        let mut area_buf = build_area_toc_sector(TWOCH_TOC_MAGIC, 2, 2);
        area_buf[0x0a..0x0c].copy_from_slice(&7u16.to_be_bytes());
        f.seek(SeekFrom::Start(540 * SECTOR_SIZE)).unwrap();
        f.write_all(&area_buf).unwrap();

        // SACDTRL1 at 541, SACDTRL2 at 542 (2 tracks).
        f.seek(SeekFrom::Start(541 * SECTOR_SIZE)).unwrap();
        f.write_all(&build_trl1_sector(&[(600, 100), (700, 200)]))
            .unwrap();
        f.seek(SeekFrom::Start(542 * SECTOR_SIZE)).unwrap();
        f.write_all(&build_trl2_sector(&[
            (
                PlayTime {
                    minutes: 0,
                    seconds: 0,
                    frames: 0,
                },
                PlayTime {
                    minutes: 2,
                    seconds: 0,
                    frames: 0,
                },
            ),
            (
                PlayTime {
                    minutes: 2,
                    seconds: 0,
                    frames: 0,
                },
                PlayTime {
                    minutes: 3,
                    seconds: 30,
                    frames: 0,
                },
            ),
        ]))
        .unwrap();

        // SACDTTxt at 543 — track 0: "Track One"/"Artist A",
        // track 1: "Track Two"/"Artist B".
        let mut ttxt_buf = vec![0u8; SECTOR_SIZE as usize];
        ttxt_buf[0..8].copy_from_slice(SACD_T_TXT_MAGIC);
        ttxt_buf[0x08..0x0a].copy_from_slice(&0x100u16.to_be_bytes());
        ttxt_buf[0x0a..0x0c].copy_from_slice(&0x200u16.to_be_bytes());
        let b0 = build_track_text_block(&[(0x01, Some(b"Track One")), (0x02, Some(b"Artist A"))]);
        ttxt_buf[0x100..0x100 + b0.len()].copy_from_slice(&b0);
        let b1 = build_track_text_block(&[(0x01, Some(b"Track Two")), (0x02, Some(b"Artist B"))]);
        ttxt_buf[0x200..0x200 + b1.len()].copy_from_slice(&b1);
        f.seek(SeekFrom::Start(543 * SECTOR_SIZE)).unwrap();
        f.write_all(&ttxt_buf).unwrap();

        // SACD_IGL at 544 + 545 (2 sectors).
        let igl = build_sacd_igl_buf(&[
            (Some("USAA10800001"), Some((1, 14))),
            (Some("USAA10800002"), Some((1, 14))),
        ]);
        f.seek(SeekFrom::Start(544 * SECTOR_SIZE)).unwrap();
        f.write_all(&igl).unwrap(); // writes 4096 bytes spanning 544+545
        drop(f);

        let md = parse_sacd_iso(&path).expect("parse_sacd_iso");
        let stereo = md.stereo.as_ref().expect("stereo present");
        assert_eq!(stereo.tracks.len(), 2);

        // Track 0: timing + text + ISRC + genre
        assert_eq!(stereo.tracks[0].start_lsn, 600);
        assert_eq!(stereo.tracks[0].text.title.as_deref(), Some("Track One"));
        assert_eq!(stereo.tracks[0].text.performer.as_deref(), Some("Artist A"));
        assert_eq!(stereo.tracks[0].isrc.as_deref(), Some("USAA10800001"));
        assert_eq!(
            stereo.tracks[0].genre.as_ref().map(|g| g.name()),
            Some("Jazz")
        );

        // Track 1
        assert_eq!(stereo.tracks[1].start_lsn, 700);
        assert_eq!(stereo.tracks[1].text.title.as_deref(), Some("Track Two"));
        assert_eq!(stereo.tracks[1].text.performer.as_deref(), Some("Artist B"));
        assert_eq!(stereo.tracks[1].isrc.as_deref(), Some("USAA10800002"));
        assert_eq!(stereo.tracks[1].duration.total_seconds(), 3.0 * 60.0 + 30.0);
    }

    // ---------- C7 tests: redundancy fallback, malformed paths, real-ISO ----------

    #[test]
    fn parse_area_falls_back_to_toc_2_when_toc_1_corrupt() {
        use std::io::{Seek, SeekFrom, Write};
        let td = tempfile::tempdir().expect("tempdir");
        let path = td.path().join("redundant_area.iso");
        let total_sectors = 700u64;
        let f = std::fs::File::create(&path).unwrap();
        f.set_len(total_sectors * SECTOR_SIZE).unwrap();
        drop(f);

        // Master TOC at LSN 510: area_1 toc_1 at 540 (corrupt), toc_2 at 550 (good).
        let mut mtoc = vec![0u8; MASTER_TOC_T_SIZE];
        mtoc[0..8].copy_from_slice(MASTER_TOC_MAGIC);
        mtoc[0x08] = 1;
        mtoc[0x09] = 20;
        mtoc[0x10..0x12].copy_from_slice(&1u16.to_be_bytes());
        mtoc[0x12..0x14].copy_from_slice(&1u16.to_be_bytes());
        mtoc[0x40..0x44].copy_from_slice(&540u32.to_be_bytes()); // toc_1
        mtoc[0x44..0x48].copy_from_slice(&550u32.to_be_bytes()); // toc_2
        mtoc[0x54..0x56].copy_from_slice(&3u16.to_be_bytes()); // size
        let mut f = std::fs::File::options().write(true).open(&path).unwrap();
        f.seek(SeekFrom::Start(510 * SECTOR_SIZE)).unwrap();
        f.write_all(&mtoc).unwrap();

        // Sector 540 stays zero (corrupt: no TWOCHTOC magic).
        // Sector 550: write a valid TWOCHTOC.
        let mut area_buf = vec![0u8; SECTOR_SIZE as usize];
        area_buf[0..8].copy_from_slice(TWOCH_TOC_MAGIC);
        area_buf[0x08] = 1;
        area_buf[0x09] = 20;
        area_buf[0x0a..0x0c].copy_from_slice(&1u16.to_be_bytes());
        area_buf[0x14] = 0x04;
        area_buf[0x15] = 2; // DSD3in14
        area_buf[0x20] = 2; // channels
        area_buf[0x40] = 30; // playtime
        area_buf[0x45] = 1; // track_count = 1
        f.seek(SeekFrom::Start(550 * SECTOR_SIZE)).unwrap();
        f.write_all(&area_buf).unwrap();
        drop(f);

        let md = parse_sacd_iso(&path).expect("parse should succeed via toc_2 fallback");
        let stereo = md.stereo.as_ref().expect("stereo via toc_2");
        assert_eq!(stereo.header.channel_count, 2);
        assert_eq!(stereo.header.track_count, 1);
    }

    #[test]
    fn parse_area_returns_primary_error_when_both_copies_fail() {
        use std::io::{Seek, SeekFrom, Write};
        let td = tempfile::tempdir().expect("tempdir");
        let path = td.path().join("both_bad.iso");
        let total = 700u64;
        let f = std::fs::File::create(&path).unwrap();
        f.set_len(total * SECTOR_SIZE).unwrap();
        drop(f);

        let mut mtoc = vec![0u8; MASTER_TOC_T_SIZE];
        mtoc[0..8].copy_from_slice(MASTER_TOC_MAGIC);
        mtoc[0x08] = 1;
        mtoc[0x09] = 20;
        mtoc[0x40..0x44].copy_from_slice(&540u32.to_be_bytes());
        mtoc[0x44..0x48].copy_from_slice(&550u32.to_be_bytes());
        mtoc[0x54..0x56].copy_from_slice(&1u16.to_be_bytes());
        let mut f = std::fs::File::options().write(true).open(&path).unwrap();
        f.seek(SeekFrom::Start(510 * SECTOR_SIZE)).unwrap();
        f.write_all(&mtoc).unwrap();
        // Leave sectors 540 AND 550 as zeros (no magic).
        drop(f);

        // Non-strict parse_sacd_iso → stereo = None, no error at top level.
        let md = parse_sacd_iso(&path).expect("non-strict swallows per-area errors");
        assert!(md.stereo.is_none());
        // Strict path → propagates the per-area error.
        let strict = parse_sacd_iso_with_strictness(&path, true);
        assert!(strict.is_err());
    }

    #[test]
    fn parse_area_no_toc_2_returns_primary_error() {
        // When ptr.toc_2_start is 0 we mustn't try sector 0 (which
        // contains the file system area).
        use std::io::{Seek, SeekFrom, Write};
        let td = tempfile::tempdir().expect("tempdir");
        let path = td.path().join("no_backup.iso");
        let total = 700u64;
        let f = std::fs::File::create(&path).unwrap();
        f.set_len(total * SECTOR_SIZE).unwrap();
        drop(f);

        let mut mtoc = vec![0u8; MASTER_TOC_T_SIZE];
        mtoc[0..8].copy_from_slice(MASTER_TOC_MAGIC);
        mtoc[0x08] = 1;
        mtoc[0x09] = 20;
        mtoc[0x40..0x44].copy_from_slice(&540u32.to_be_bytes());
        // toc_2_start = 0
        mtoc[0x54..0x56].copy_from_slice(&1u16.to_be_bytes());
        let mut f = std::fs::File::options().write(true).open(&path).unwrap();
        f.seek(SeekFrom::Start(510 * SECTOR_SIZE)).unwrap();
        f.write_all(&mtoc).unwrap();
        drop(f);

        // Sector 540 has no magic; toc_2=0 means no fallback.
        let strict = parse_sacd_iso_with_strictness(&path, true);
        assert!(
            strict.is_err(),
            "should error when toc_1 fails and no toc_2"
        );
    }

    /// Real-world ISO fixture: only runs when
    /// `TONEPOET_SACD_FIXTURE_ISO` points at an existing SACD ISO.
    /// Exercises `parse_sacd_iso` end-to-end and asserts the basic
    /// shape invariants (spec version 1.x, at least one area present,
    /// non-zero track count, DSD64 sample frequency byte). CI leaves
    /// this unset; developers point it at their own library to
    /// validate parser correctness against real-world pressings.
    #[test]
    fn parse_real_sacd_iso_when_env_var_set() {
        let Ok(path) = std::env::var("TONEPOET_SACD_FIXTURE_ISO") else {
            return;
        };
        let p = std::path::Path::new(&path);
        if !p.exists() {
            eprintln!("TONEPOET_SACD_FIXTURE_ISO='{}' not found — skipping", path);
            return;
        }
        let md = parse_sacd_iso(p).unwrap_or_else(|e| {
            panic!("real ISO '{}' failed to parse: {}", path, e);
        });

        assert_eq!(md.master_toc.spec_version.0, 1, "spec major should be 1");
        assert!(
            md.stereo.is_some() || md.multi_channel.is_some(),
            "at least one area must parse",
        );
        if let Some(area) = md.stereo.as_ref().or(md.multi_channel.as_ref()) {
            assert!(area.header.track_count > 0, "track_count must be > 0");
            assert_eq!(area.header.sample_frequency, 0x04, "must be DSD64");
            assert_eq!(area.tracks.len(), area.header.track_count as usize);
        }
    }
}
