// SPDX-License-Identifier: GPL-3.0-or-later
//
// Copyright (C) 2026 the tonepoet authors.
//
// SACD ISO support: detection, Master/Area TOC parsing, metadata
// surfacing, TOC consistency reporting, and metadata-to-extraction
// bridging. Audio extraction itself is delegated to crates/sacd-rs,
// with the parsed area frame-format nibble passed into the high-integrity
// extraction API.
//
// Formal spec audit index: docs/scarlet_book_audit_map.md. Grep
// SB-AUDIT anchors from that file to review implementation sites against
// the licensed Scarlet Book specification.
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
// SB-AUDIT: SB-DISC-001
pub const SECTOR_SIZE: u64 = 2048;

/// Master TOC start sectors (LSNs). Three redundant copies; if the
/// first is corrupted, fall back to the next.
// SB-AUDIT: SB-DISC-002
pub const MASTER_TOC_LSNS: [u64; 3] = [510, 520, 530];


/// Canonical ScarletBook structure signatures. The older *_MAGIC names
/// remain below for compatibility with existing call sites.
// SB-AUDIT: SB-SIG-001..SB-SIG-008
pub const MASTER_TOC_SIGNATURE: &[u8; 8] = b"SACDMTOC";
pub const MASTER_TEXT_SIGNATURE: &[u8; 8] = b"SACDText";
pub const AREA_TOC_SIGNATURE_STEREO: &[u8; 8] = b"TWOCHTOC";
pub const AREA_TOC_SIGNATURE_MCH: &[u8; 8] = b"MULCHTOC";
pub const AREA_TRACK_TEXT_SIGNATURE: &[u8; 8] = b"SACDTTxt";
pub const AREA_TRACK_LIST_1_SIGNATURE: &[u8; 8] = b"SACDTRL1";
pub const AREA_TRACK_LIST_2_SIGNATURE: &[u8; 8] = b"SACDTRL2";
pub const AREA_ISRC_GENRE_SIGNATURE: &[u8; 8] = b"SACD_IGL";

/// Magic identifier at the start of each Master TOC sector. ASCII,
/// 8 bytes, big-endian per spec.
pub const MASTER_TOC_MAGIC: &[u8; 8] = MASTER_TOC_SIGNATURE;

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
pub const SACD_TEXT_MAGIC: &[u8; 8] = MASTER_TEXT_SIGNATURE;

/// Magic identifier for a 2-channel area's TOC sector (first sector
/// at the LSN pointed at by master_toc.area_1_toc_1_start).
pub const TWOCH_TOC_MAGIC: &[u8; 8] = AREA_TOC_SIGNATURE_STEREO;

/// Magic identifier for a multi-channel area's TOC sector.
pub const MULCH_TOC_MAGIC: &[u8; 8] = AREA_TOC_SIGNATURE_MCH;

/// Magic identifier for the per-area track-LSN list (SACDTRL1).
/// Lives at one of the sectors following the area TOC header.
pub const SACD_TRL1_MAGIC: &[u8; 8] = AREA_TRACK_LIST_1_SIGNATURE;

/// Magic identifier for the per-area track-time list (SACDTRL2).
pub const SACD_TRL2_MAGIC: &[u8; 8] = AREA_TRACK_LIST_2_SIGNATURE;

/// Magic identifier for the per-area, per-track text sector
/// (track titles, performers, composers, ISRC-adjacent metadata).
/// One sector per locale; tonepoet only parses the primary one
/// (locale 0).
pub const SACD_T_TXT_MAGIC: &[u8; 8] = AREA_TRACK_TEXT_SIGNATURE;

/// Magic identifier for the per-area ISRC + per-track genre list.
/// Spans **two** consecutive sectors (4096 bytes total, 4092 used).
pub const SACD_IGL_MAGIC: &[u8; 8] = AREA_ISRC_GENRE_SIGNATURE;

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
    /// ScarletBook genre-table category label.
    pub fn category_name(&self) -> &'static str {
        match self.category {
            0 => "Not used",
            1 => "General",
            2 => "Japanese",
            _ => "unknown",
        }
    }

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
// SB-AUDIT: SB-MTOC-001..SB-MTOC-009
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

/// Reusable 2048-byte sector reader for TOC scans.
///
/// The previous helper opened, sought, read, and closed the ISO for every
/// sector. That is acceptable for one-off probes, but area TOC scans can touch
/// dozens of adjacent sectors. Keeping a single file descriptor and reusable
/// caller-provided buffers removes repeated open/close churn while preserving
/// the simple path-level public API.
struct SectorReader {
    file: std::fs::File,
}

impl SectorReader {
    fn open(path: &Path) -> Result<Self, SacdError> {
        let file = std::fs::File::open(path)
            .map_err(|e| SacdError::Io(format!("open: {}", e)))?;
        Ok(Self { file })
    }

    fn read_sector_into(&mut self, lsn: u64, buf: &mut [u8]) -> Result<(), SacdError> {
        use std::io::{Read, Seek, SeekFrom};

        if buf.len() < SECTOR_SIZE as usize {
            return Err(SacdError::Malformed(format!(
                "sector read buffer too small: {} < {}",
                buf.len(), SECTOR_SIZE
            )));
        }
        let offset = lsn.checked_mul(SECTOR_SIZE).ok_or_else(|| {
            SacdError::Malformed(format!("sector offset overflow for LSN {}", lsn))
        })?;
        self.file
            .seek(SeekFrom::Start(offset))
            .map_err(|e| SacdError::Io(format!("seek to LSN {}: {}", lsn, e)))?;
        self.file
            .read_exact(&mut buf[..SECTOR_SIZE as usize])
            .map_err(|e| SacdError::Io(format!("read LSN {}: {}", lsn, e)))
    }

    fn read_sector(&mut self, lsn: u64) -> Result<Vec<u8>, SacdError> {
        let mut buf = vec![0u8; SECTOR_SIZE as usize];
        self.read_sector_into(lsn, &mut buf)?;
        Ok(buf)
    }
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
// SB-AUDIT: SB-MTXT-001..SB-MTXT-006
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
    let mut reader = SectorReader::open(path)?;
    let buf = reader.read_sector(sector_lsn)?;
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

/// Frame format used by an SACD area. DSD variants are uncompressed;
/// DST is lossless-compressed and requires DST decoding to reconstruct
/// DSD samples for playback or transcoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
// SB-AUDIT: SB-ATOC-006, SB-AUDIO-012
pub enum FrameFormat {
    Dst,
    Reserved,
    Dsd3In14,
    Dsd3In16,
    Dsd4,
    Dsd5,
    Dsd6,
    Dsd7,
    Unknown(u8),
}

impl FrameFormat {
    fn from_nibble(n: u8) -> Self {
        match n & 0x0f {
            0 => FrameFormat::Dst,
            1 => FrameFormat::Reserved,
            2 => FrameFormat::Dsd3In14,
            3 => FrameFormat::Dsd3In16,
            4 => FrameFormat::Dsd4,
            5 => FrameFormat::Dsd5,
            6 => FrameFormat::Dsd6,
            7 => FrameFormat::Dsd7,
            other => FrameFormat::Unknown(other),
        }
    }

    pub fn is_dst_encoded(&self) -> bool {
        matches!(self, FrameFormat::Dst)
    }

    /// Low-nibble value as stored in the area TOC. Use this when plumbing
    /// the parsed TUI metadata into `sacd-rs` extraction options:
    /// `sacd_rs::FrameFormat::from_nibble(area.header.frame_format.as_nibble())`.
    pub fn as_nibble(&self) -> u8 {
        match self {
            FrameFormat::Dst => 0,
            FrameFormat::Reserved => 1,
            FrameFormat::Dsd3In14 => 2,
            FrameFormat::Dsd3In16 => 3,
            FrameFormat::Dsd4 => 4,
            FrameFormat::Dsd5 => 5,
            FrameFormat::Dsd6 => 6,
            FrameFormat::Dsd7 => 7,
            FrameFormat::Unknown(n) => n & 0x0f,
        }
    }

    pub fn sectors_per_frame(&self) -> Option<u32> {
        match self {
            FrameFormat::Dsd3In14
            | FrameFormat::Dsd4
            | FrameFormat::Dsd5
            | FrameFormat::Dsd6
            | FrameFormat::Dsd7 => Some(14),
            FrameFormat::Dsd3In16 => Some(16),
            FrameFormat::Dst | FrameFormat::Reserved | FrameFormat::Unknown(_) => None,
        }
    }
}

/// Total playtime for an area as a (minutes, seconds, frames@75)
/// triple. Use `total_seconds()` for a flat duration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
// SB-AUDIT: SB-TRL2-002..SB-TRL2-006
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

    /// Total SACD frame count at 75 fps.
    pub fn as_frame_count(&self) -> u32 {
        (self.minutes as u32) * 60 * SACD_FRAME_RATE
            + (self.seconds as u32) * SACD_FRAME_RATE
            + self.frames as u32
    }

    /// True when the sub-second frame component is within the 75 fps SACD clock.
    ///
    /// SACDTRL2 stores track times as raw frame-count fields.  The seconds
    /// byte can exceed 59 on real discs; sacd_extract still feeds it to
    /// TIME_FRAMECOUNT directly.  Only the frame byte is a sub-second index.
    pub fn is_normalized(&self) -> bool {
        self.frames < SACD_FRAME_RATE as u8
    }

    pub fn is_zero(&self) -> bool {
        self.minutes == 0 && self.seconds == 0 && self.frames == 0
    }
}

/// One area's per-track entry: timing from SACDTRL1+SACDTRL2, text
/// from SACDTTxt, ISRC + genre from SACD_IGL. All non-timing fields
/// are best-effort — a disc may carry only timing, only timing+title,
/// the full set, or any subset in between. (Copy is intentionally
/// not derived because TrackText holds owned Strings.)
#[derive(Debug, Clone, Default)]
// SB-AUDIT: SB-TRL1-002..SB-TRL1-006, SB-TRL2-002..SB-TRL2-006, SB-TTXT-001..SB-TTXT-016, SB-IGL-002..SB-IGL-007
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
    /// Structured ISRC parsed from SACD_IGL, when present.
    pub structured_isrc: Option<Isrc>,
    /// Per-track genre from SACD_IGL.
    pub genre: Option<Genre>,
}

/// Decoded header portion of one area TOC (TWOCHTOC or MULCHTOC).
/// Strings (description, copyright) are pulled from the trailing
/// data region using the area's primary locale's character_set.
#[derive(Debug, Clone)]
// SB-AUDIT: SB-ATOC-001..SB-ATOC-014
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

/// Severity of a TOC consistency finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TocConsistencySeverity {
    Warning,
    Error,
}

/// Named consistency checks performed after the TOC structures are parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
// SB-AUDIT: SB-CHECK-001..SB-CHECK-012
pub enum TocConsistencyCheck {
    MasterAreaPointer,
    AreaPointerKind,
    AreaSizeBounds,
    TrackCount,
    TrackSectorRange,
    TrackList1Length,
    TrackList2Duration,
    AreaDuration,
    /// Exact-sector read/seek failure observed while scanning TOC sectors.
    /// This prevents damaged-disc reports from collapsing I/O loss into
    /// later "missing list" consistency errors.
    TocSectorRead,
}

/// One auditable consistency finding. `track_index` is one-based when set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TocConsistencyIssue {
    pub severity: TocConsistencySeverity,
    pub check: TocConsistencyCheck,
    pub area: Option<AreaKind>,
    pub track_index: Option<u8>,
    pub message: String,
}

/// A precise TOC-sector I/O diagnostic captured during metadata scanning.
///
/// Audio-sector extraction uses `RecoveryEvent`; TOC scanning gets a parallel
/// report path so a forensic caller can see the exact LSN and operation that
/// failed instead of inferring damage from a later missing-list error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TocReadEvent {
    pub severity: TocConsistencySeverity,
    pub area: Option<AreaKind>,
    pub lsn: u64,
    pub context: String,
    pub error: String,
}

impl std::fmt::Display for TocReadEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:?} TOC read event at LSN {}",
            self.severity, self.lsn
        )?;
        if let Some(area) = self.area {
            write!(f, " ({:?})", area)?;
        }
        write!(f, ": {}: {}", self.context, self.error)
    }
}

/// Result of the explicit TOC consistency pass. Non-strict parsing keeps
/// metadata and exposes this report; strict parsing treats any `Error`
/// finding as malformed input.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TocConsistencyReport {
    pub issues: Vec<TocConsistencyIssue>,
    pub read_events: Vec<TocReadEvent>,
}

impl TocConsistencyReport {
    pub fn is_clean(&self) -> bool {
        self.issues.is_empty() && self.read_events.is_empty()
    }

    pub fn has_errors(&self) -> bool {
        self.issues
            .iter()
            .any(|issue| issue.severity == TocConsistencySeverity::Error)
            || self
                .read_events
                .iter()
                .any(|event| event.severity == TocConsistencySeverity::Error)
    }

    pub fn extend(&mut self, mut other: TocConsistencyReport) {
        self.issues.append(&mut other.issues);
        self.read_events.append(&mut other.read_events);
    }

    pub fn push_error(
        &mut self,
        check: TocConsistencyCheck,
        area: Option<AreaKind>,
        track_index: Option<u8>,
        message: impl Into<String>,
    ) {
        self.push(TocConsistencySeverity::Error, check, area, track_index, message);
    }

    pub fn push_warning(
        &mut self,
        check: TocConsistencyCheck,
        area: Option<AreaKind>,
        track_index: Option<u8>,
        message: impl Into<String>,
    ) {
        self.push(TocConsistencySeverity::Warning, check, area, track_index, message);
    }

    fn push(
        &mut self,
        severity: TocConsistencySeverity,
        check: TocConsistencyCheck,
        area: Option<AreaKind>,
        track_index: Option<u8>,
        message: impl Into<String>,
    ) {
        self.issues.push(TocConsistencyIssue {
            severity,
            check,
            area,
            track_index,
            message: message.into(),
        });
    }

    pub fn push_read_error(
        &mut self,
        area: Option<AreaKind>,
        lsn: u64,
        context: impl Into<String>,
        error: impl Into<String>,
    ) {
        self.push_read_event(TocConsistencySeverity::Error, area, lsn, context, error);
    }

    pub fn push_read_warning(
        &mut self,
        area: Option<AreaKind>,
        lsn: u64,
        context: impl Into<String>,
        error: impl Into<String>,
    ) {
        self.push_read_event(TocConsistencySeverity::Warning, area, lsn, context, error);
    }

    fn push_read_event(
        &mut self,
        severity: TocConsistencySeverity,
        area: Option<AreaKind>,
        lsn: u64,
        context: impl Into<String>,
        error: impl Into<String>,
    ) {
        let context = context.into();
        let error = error.into();
        self.read_events.push(TocReadEvent {
            severity,
            area,
            lsn,
            context: context.clone(),
            error: error.clone(),
        });
        self.push(
            severity,
            TocConsistencyCheck::TocSectorRead,
            area,
            None,
            format!("TOC sector read failed at LSN {} while {}: {}", lsn, context, error),
        );
    }

    pub fn error_summary(&self) -> String {
        self.issues
            .iter()
            .filter(|issue| issue.severity == TocConsistencySeverity::Error)
            .map(|issue| issue.message.as_str())
            .collect::<Vec<_>>()
            .join("; ")
    }
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
    // Each on-disc SACDTRL2 entry is the same layout sacd_extract maps
    // directly onto `area_tracklist_time_t`: bytes 0, 1 and 2 are
    // minutes, seconds and 75-fps frame index; byte 3 carries flags /
    // reserved bits. Do not skip byte 0 here. For example, an entry
    // containing `45 2a 22 00` denotes 69:42:34, matching
    // sacd_extract's TIME_FRAMECOUNT interpretation.
    let start_base = 8;
    let dur_base = 8 + 255 * 4;
    for i in 0..track_count as usize {
        let start = start_base + i * 4;
        let dur = dur_base + i * 4;
        let s = PlayTime {
            minutes: buf[start],
            seconds: buf[start + 1],
            frames: buf[start + 2],
        };
        let d = PlayTime {
            minutes: buf[dur],
            seconds: buf[dur + 1],
            frames: buf[dur + 2],
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

/// ScarletBook per-track text type identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
// SB-AUDIT: SB-TTXT-001..SB-TTXT-016
pub enum TrackTextType {
    Title,
    Performer,
    Songwriter,
    Composer,
    Arranger,
    Message,
    ExtraMessage,
    Copyright,
    TitlePhonetic,
    PerformerPhonetic,
    SongwriterPhonetic,
    ComposerPhonetic,
    ArrangerPhonetic,
    MessagePhonetic,
    ExtraMessagePhonetic,
    CopyrightPhonetic,
    Unknown(u8),
}

impl From<u8> for TrackTextType {
    fn from(value: u8) -> Self {
        match value {
            0x01 => TrackTextType::Title,
            0x02 => TrackTextType::Performer,
            0x03 => TrackTextType::Songwriter,
            0x04 => TrackTextType::Composer,
            0x05 => TrackTextType::Arranger,
            0x06 => TrackTextType::Message,
            0x07 => TrackTextType::ExtraMessage,
            0x08 => TrackTextType::Copyright,
            0x81 => TrackTextType::TitlePhonetic,
            0x82 => TrackTextType::PerformerPhonetic,
            0x83 => TrackTextType::SongwriterPhonetic,
            0x84 => TrackTextType::ComposerPhonetic,
            0x85 => TrackTextType::ArrangerPhonetic,
            0x86 => TrackTextType::MessagePhonetic,
            0x87 => TrackTextType::ExtraMessagePhonetic,
            0x88 => TrackTextType::CopyrightPhonetic,
            other => TrackTextType::Unknown(other),
        }
    }
}

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
    pub copyright: Option<String>,
    pub title_phonetic: Option<String>,
    pub performer_phonetic: Option<String>,
    pub songwriter_phonetic: Option<String>,
    pub composer_phonetic: Option<String>,
    pub arranger_phonetic: Option<String>,
    pub message_phonetic: Option<String>,
    pub extra_message_phonetic: Option<String>,
    pub copyright_phonetic: Option<String>,
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
    match TrackTextType::from(ttype) {
        TrackTextType::Title => tt.title = Some(s),
        TrackTextType::Performer => tt.performer = Some(s),
        TrackTextType::Songwriter => tt.songwriter = Some(s),
        TrackTextType::Composer => tt.composer = Some(s),
        TrackTextType::Arranger => tt.arranger = Some(s),
        TrackTextType::Message => tt.message = Some(s),
        TrackTextType::ExtraMessage => tt.extra_message = Some(s),
        TrackTextType::Copyright => tt.copyright = Some(s),
        TrackTextType::TitlePhonetic => tt.title_phonetic = Some(s),
        TrackTextType::PerformerPhonetic => tt.performer_phonetic = Some(s),
        TrackTextType::SongwriterPhonetic => tt.songwriter_phonetic = Some(s),
        TrackTextType::ComposerPhonetic => tt.composer_phonetic = Some(s),
        TrackTextType::ArrangerPhonetic => tt.arranger_phonetic = Some(s),
        TrackTextType::MessagePhonetic => tt.message_phonetic = Some(s),
        TrackTextType::ExtraMessagePhonetic => tt.extra_message_phonetic = Some(s),
        TrackTextType::CopyrightPhonetic => tt.copyright_phonetic = Some(s),
        TrackTextType::Unknown(_) => {}
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

/// Structured International Standard Recording Code from SACD_IGL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
// SB-AUDIT: SB-IGL-002..SB-IGL-005
pub struct Isrc {
    pub country_code: [u8; 2],
    pub owner_code: [u8; 3],
    pub recording_year: [u8; 2],
    pub designation_code: [u8; 5],
}

impl Isrc {
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 12 {
            return None;
        }
        let raw = &bytes[..12];
        if raw.iter().all(|&b| b == 0 || b == b' ') {
            return None;
        }
        let mut country_code = [0u8; 2];
        let mut owner_code = [0u8; 3];
        let mut recording_year = [0u8; 2];
        let mut designation_code = [0u8; 5];
        country_code.copy_from_slice(&raw[0..2]);
        owner_code.copy_from_slice(&raw[2..5]);
        recording_year.copy_from_slice(&raw[5..7]);
        designation_code.copy_from_slice(&raw[7..12]);
        Some(Self { country_code, owner_code, recording_year, designation_code })
    }

    /// Structural ISRC validation: CC is A-Z, owner is alphanumeric,
    /// year and designation are decimal digits.
    pub fn is_valid(&self) -> bool {
        self.country_code.iter().all(|b| b.is_ascii_uppercase())
            && self.owner_code.iter().all(|b| b.is_ascii_alphanumeric())
            && self.recording_year.iter().all(|b| b.is_ascii_digit())
            && self.designation_code.iter().all(|b| b.is_ascii_digit())
    }
}

impl std::fmt::Display for Isrc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&String::from_utf8_lossy(&self.country_code))?;
        f.write_str(&String::from_utf8_lossy(&self.owner_code))?;
        f.write_str(&String::from_utf8_lossy(&self.recording_year))?;
        f.write_str(&String::from_utf8_lossy(&self.designation_code))
    }
}

/// One row of SACD_IGL data for a single track.
#[derive(Debug, Clone, Default)]
pub struct TrackIsrcGenre {
    /// 12-character ISRC if non-empty, else None. Kept for compatibility
    /// with existing metadata callers.
    pub isrc: Option<String>,
    /// Structured ISRC with validation helpers.
    pub structured_isrc: Option<Isrc>,
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
        let isrc_bytes = &buf[isrc_off..isrc_off + 12];
        let structured_isrc = Isrc::parse(isrc_bytes);
        let isrc_str = structured_isrc.as_ref().map(|isrc| isrc.to_string()).unwrap_or_default();
        let isrc = if isrc_str.is_empty() { None } else { Some(isrc_str) };

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

        out.push(TrackIsrcGenre { isrc, structured_isrc, genre });
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
    /// Per-area TOC consistency diagnostics. A parsed area can be usable
    /// while still carrying warnings or recovery-relevant errors.
    pub consistency: TocConsistencyReport,
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
    /// Master + area TOC consistency diagnostics accumulated during parse.
    pub consistency: TocConsistencyReport,
}


/// Production extraction mode for SACD audio materialization.
///
/// The strict mode is intended for validation and normal high-integrity
/// conversion. Salvage mode is explicit and returns a non-clean
/// `sacd_rs::extract::ExtractReport` whenever damaged sectors or partial
/// frames had to be skipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SacdExtractionMode {
    Strict,
    Salvage,
}

impl SacdExtractionMode {
    fn integrity_options(self, area: &AreaInfo) -> sacd_rs::extract::ExtractIntegrityOptions {
        let frame_format = sacd_rs::FrameFormat::from_nibble(area.header.frame_format.as_nibble());
        match self {
            SacdExtractionMode::Strict => {
                sacd_rs::extract::ExtractIntegrityOptions::strict()
                    .with_frame_format(frame_format)
            }
            SacdExtractionMode::Salvage => {
                sacd_rs::extract::ExtractIntegrityOptions::salvage()
                    .with_frame_format(frame_format)
            }
        }
    }
}

/// Errors from the SACD metadata-to-extraction bridge.
#[derive(Debug)]
pub enum SacdExtractionError {
    /// Caller requested a track index outside the parsed area track list.
    TrackIndexOutOfRange { requested: usize, track_count: usize },
    /// The parsed track does not have a usable sector range. This usually
    /// means SACDTRL1 was missing or internally inconsistent.
    MissingTrackSectorRange { track_index: usize },
    /// Caller requested an area that was not present or failed TOC parsing.
    AreaMissing { area: AreaKind },
    /// Failed to open the ISO for sector-aligned extraction.
    IsoOpen(String),
    /// High-integrity extractor failure.
    Extract(sacd_rs::extract::ExtractError),
}

impl std::fmt::Display for SacdExtractionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TrackIndexOutOfRange { requested, track_count } => write!(
                f,
                "SACD extraction: track index {} out of range for {} tracks",
                requested, track_count
            ),
            Self::MissingTrackSectorRange { track_index } => write!(
                f,
                "SACD extraction: track {} has no usable sector range",
                track_index + 1
            ),
            Self::AreaMissing { area } => write!(
                f,
                "SACD extraction: requested {:?} area is not available",
                area
            ),
            Self::IsoOpen(msg) => write!(f, "SACD extraction: open ISO: {}", msg),
            Self::Extract(err) => write!(f, "SACD extraction: {}", err),
        }
    }
}

impl std::error::Error for SacdExtractionError {}

impl From<sacd_rs::extract::ExtractError> for SacdExtractionError {
    fn from(err: sacd_rs::extract::ExtractError) -> Self {
        Self::Extract(err)
    }
}

impl AreaInfo {
    /// Convert the parsed area TOC frame-format nibble into the extraction
    /// crate's authoritative `FrameFormat` type.
    ///
    /// This is the production handoff point requested by the extraction brief:
    /// extraction must not rediscover DST/plain-DSD solely from per-sector
    /// bits. The area TOC is authoritative for frame classification and
    /// decoder routing.
    pub fn extraction_frame_format(&self) -> sacd_rs::FrameFormat {
        sacd_rs::FrameFormat::from_nibble(self.header.frame_format.as_nibble())
    }

    /// Build source-compatible extraction options from this area's parsed TOC
    /// and one selected track, using sacd_extract queueing semantics.
    ///
    /// Track 0 starts at the area TOC `track_start`. Later tracks start at
    /// their SACDTRL1 `track_start_lsn`. Non-final tracks end at the next
    /// track start plus one sector. The final track ends at
    /// `area_toc.track_end + 1`, matching sacd_extract's queued
    /// `length_lsn = area_toc->track_end - start_lsn + 1` and its
    /// `[start_lsn, start_lsn + length_lsn)` processing loop.
    ///
    /// The wider scan window is paired with sacd_extract-style default
    /// audio-frame trimming: only frames whose frame-info timecode lands in
    /// `[TrackEntry.start_time, TrackEntry.start_time + TrackEntry.duration)`
    /// are emitted.
    pub fn track_extract_options(
        &self,
        track_index: usize,
        output_format: sacd_rs::extract::OutputFormat,
    ) -> Result<sacd_rs::extract::ExtractOptions, SacdExtractionError> {
        let entry = self.tracks.get(track_index).ok_or(
            SacdExtractionError::TrackIndexOutOfRange {
                requested: track_index,
                track_count: self.tracks.len(),
            },
        )?;

        let start_lsn = if track_index == 0 {
            self.header.track_start_lsn as u64
        } else {
            let start = self.tracks[track_index].start_lsn as u64;
            if start == 0 {
                return Err(SacdExtractionError::MissingTrackSectorRange { track_index });
            }
            start
        };

        let end_lsn = if let Some(next) = self.tracks.get(track_index + 1) {
            let next_start = next.start_lsn as u64;
            if next_start == 0 {
                return Err(SacdExtractionError::MissingTrackSectorRange { track_index });
            }
            next_start.checked_add(1).ok_or(
                SacdExtractionError::MissingTrackSectorRange { track_index },
            )?
        } else {
            (self.header.track_end_lsn as u64).checked_add(1).ok_or(
                SacdExtractionError::MissingTrackSectorRange { track_index },
            )?
        };

        if start_lsn == 0 || end_lsn <= start_lsn {
            return Err(SacdExtractionError::MissingTrackSectorRange { track_index });
        }

        let filter_start = entry.start_time.as_frame_count();
        let duration_end = filter_start.saturating_add(entry.duration.as_frame_count());
        let mut filter_end = duration_end;

        // sacd_extract trims completed output frames by absolute frame
        // timecode while its sector queue overlaps adjacent tracks and can
        // extend into lead-out garbage. On some discs the TRL2 duration table
        // overshoots the next absolute start / area total; bound the emit
        // window by those absolute times so tail or next-track frames are not
        // treated as in-track integrity loss.
        if let Some(next) = self.tracks.get(track_index + 1) {
            let next_start = next.start_time.as_frame_count();
            if next_start > filter_start {
                filter_end = filter_end.min(next_start);
            }
        } else {
            let area_end = self.header.total_playtime.as_frame_count();
            if area_end > filter_start {
                filter_end = filter_end.min(area_end);
            }
        }

        let filter_duration = filter_end.saturating_sub(filter_start);

        Ok(sacd_rs::extract::ExtractOptions::new(
            start_lsn,
            end_lsn,
            self.header.channel_count,
            output_format,
        )
        .with_time_filter(sacd_rs::extract::TimeFilter::new(
            filter_start,
            filter_duration,
        )))
    }

    /// Build the high-integrity extraction controls for this area.
    ///
    /// This always passes
    /// `sacd_rs::FrameFormat::from_nibble(area.header.frame_format.as_nibble())`
    /// into `ExtractIntegrityOptions`, so production extraction routes
    /// DSD/DST by the area TOC end to end.
    pub fn track_integrity_options(
        &self,
        mode: SacdExtractionMode,
    ) -> sacd_rs::extract::ExtractIntegrityOptions {
        mode.integrity_options(self)
    }

    /// Extract one parsed area track into a caller-owned output writer using
    /// the high-integrity API.
    ///
    /// Call sites should use this instead of the legacy `extract_track()` path
    /// whenever they are starting from parsed `AreaInfo`; it carries the area
    /// TOC's frame format into the frame reader and returns the surfaced
    /// integrity report.
    pub fn extract_track_to_writer<W: std::io::Write + std::io::Seek>(
        &self,
        iso: &mut sacd_rs::iso_reader::IsoReader,
        output: &mut W,
        track_index: usize,
        output_format: sacd_rs::extract::OutputFormat,
        mode: SacdExtractionMode,
    ) -> Result<sacd_rs::extract::ExtractReport, SacdExtractionError> {
        let opts = self.track_extract_options(track_index, output_format)?;
        let integrity_options = self.track_integrity_options(mode);
        sacd_rs::extract::extract_track_with_integrity_options(
            iso,
            output,
            opts,
            integrity_options,
        )
        .map_err(SacdExtractionError::from)
    }

    /// Convenience wrapper that opens the ISO and then delegates to
    /// [`AreaInfo::extract_track_to_writer`]. This is suitable for production
    /// materializers that already have parsed `SacdMetadata` and an output
    /// file/temporary writer.
    pub fn extract_track_from_path<W: std::io::Write + std::io::Seek>(
        &self,
        iso_path: &Path,
        output: &mut W,
        track_index: usize,
        output_format: sacd_rs::extract::OutputFormat,
        mode: SacdExtractionMode,
    ) -> Result<sacd_rs::extract::ExtractReport, SacdExtractionError> {
        let mut iso = sacd_rs::iso_reader::IsoReader::open(iso_path)
            .map_err(|e| SacdExtractionError::IsoOpen(e.to_string()))?;
        self.extract_track_to_writer(&mut iso, output, track_index, output_format, mode)
    }
}

impl SacdMetadata {
    /// Return the parsed area metadata for a stereo or multi-channel request.
    pub fn area(&self, area: AreaKind) -> Option<&AreaInfo> {
        match area {
            AreaKind::Stereo => self.stereo.as_ref(),
            AreaKind::MultiChannel => self.multi_channel.as_ref(),
        }
    }

    /// Production SACD extraction entry point from parsed metadata.
    ///
    /// This deliberately delegates through [`AreaInfo::extract_track_from_path`]
    /// rather than the legacy `sacd_rs::extract::extract_track()` function, so
    /// every production extraction carries the area TOC frame format into
    /// `extract_track_with_integrity_options()` and returns the full integrity
    /// report.
    pub fn extract_track_from_path<W: std::io::Write + std::io::Seek>(
        &self,
        iso_path: &Path,
        area: AreaKind,
        track_index: usize,
        output: &mut W,
        output_format: sacd_rs::extract::OutputFormat,
        mode: SacdExtractionMode,
    ) -> Result<sacd_rs::extract::ExtractReport, SacdExtractionError> {
        let area_info = self
            .area(area)
            .ok_or(SacdExtractionError::AreaMissing { area })?;
        area_info.extract_track_from_path(
            iso_path,
            output,
            track_index,
            output_format,
            mode,
        )
    }

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
    let total_sectors = file_complete_sector_count(path)?;
    let (master_toc_lsn, master_toc) = read_master_toc_with_lsn(path)?;
    let master_text = read_master_text(path, master_toc_lsn, &master_toc)?;

    let mut consistency = validate_master_area_pointers(&master_toc, total_sectors);
    if strict && consistency.has_errors() {
        return Err(SacdError::Malformed(format!(
            "master TOC consistency failure: {}",
            consistency.error_summary()
        )));
    }

    let stereo = parse_area_with_strictness(
        path,
        master_toc.two_channel,
        AreaKind::Stereo,
        total_sectors,
        strict,
        &mut consistency,
    )?;
    let multi_channel = parse_area_with_strictness(
        path,
        master_toc.multi_channel,
        AreaKind::MultiChannel,
        total_sectors,
        strict,
        &mut consistency,
    )?;

    if strict && consistency.has_errors() {
        return Err(SacdError::Malformed(format!(
            "SACD TOC consistency failure: {}",
            consistency.error_summary()
        )));
    }

    Ok(SacdMetadata {
        master_toc,
        master_text,
        stereo,
        multi_channel,
        consistency,
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
    expected_kind: AreaKind,
    total_sectors: u64,
    strict: bool,
    global_consistency: &mut TocConsistencyReport,
) -> Result<Option<AreaInfo>, SacdError> {
    if !ptr.is_present() {
        return Ok(None);
    }
    match parse_area_checked_impl(path, ptr, Some(expected_kind), total_sectors) {
        Ok(area) => {
            global_consistency.extend(area.consistency.clone());
            if strict && area.consistency.has_errors() {
                return Err(SacdError::Malformed(format!(
                    "{:?} area TOC consistency failure: {}",
                    expected_kind,
                    area.consistency.error_summary()
                )));
            }
            Ok(Some(area))
        }
        Err(failure) if strict => Err(failure.error),
        Err(failure) => {
            let AreaParseFailure {
                error,
                consistency,
            } = failure;
            global_consistency.extend(consistency);
            global_consistency.push_error(
                TocConsistencyCheck::MasterAreaPointer,
                Some(expected_kind),
                None,
                format!("{:?} area failed to parse: {}", expected_kind, error),
            );
            Ok(None)
        }
    }
}

#[derive(Debug, Clone)]
struct AreaParseFailure {
    error: SacdError,
    consistency: TocConsistencyReport,
}

fn area_parse_failure(error: SacdError, consistency: TocConsistencyReport) -> AreaParseFailure {
    AreaParseFailure { error, consistency }
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
    let total_sectors = file_complete_sector_count(path)?;
    parse_area_checked_impl(path, ptr, None, total_sectors).map_err(|failure| failure.error)
}

fn parse_area_checked_impl(
    path: &Path,
    ptr: AreaPointer,
    expected_kind: Option<AreaKind>,
    total_sectors: u64,
) -> Result<AreaInfo, AreaParseFailure> {
    let mut consistency = TocConsistencyReport::default();
    let mut sector_reader = SectorReader::open(path)
        .map_err(|error| area_parse_failure(error, TocConsistencyReport::default()))?;

    // Try toc_1 first; on failure, fall back to toc_2 when set.
    let (header, header_start_lsn) = match try_area_header_at(&mut sector_reader, ptr.toc_1_start as u64) {
        Ok(h) => (h, ptr.toc_1_start as u64),
        Err(primary_err) => {
            let primary_msg = primary_err.to_string();
            let primary_was_io = matches!(&primary_err, SacdError::Io(_));
            if ptr.toc_2_start == 0 {
                if primary_was_io {
                    consistency.push_read_error(
                        expected_kind,
                        ptr.toc_1_start as u64,
                        "reading primary area TOC header",
                        primary_msg.clone(),
                    );
                }
                return Err(area_parse_failure(primary_err, consistency));
            }
            match try_area_header_at(&mut sector_reader, ptr.toc_2_start as u64) {
                Ok(h) => {
                    if primary_was_io {
                        consistency.push_read_warning(
                            expected_kind,
                            ptr.toc_1_start as u64,
                            "reading primary area TOC header",
                            primary_msg.clone(),
                        );
                    }
                    consistency.push_warning(
                        TocConsistencyCheck::MasterAreaPointer,
                        expected_kind,
                        None,
                        format!(
                            "primary area TOC at LSN {} failed; using redundant copy at LSN {}: {}",
                            ptr.toc_1_start, ptr.toc_2_start, primary_msg
                        ),
                    );
                    (h, ptr.toc_2_start as u64)
                }
                Err(backup_err) => {
                    let backup_msg = backup_err.to_string();
                    let backup_was_io = matches!(&backup_err, SacdError::Io(_));
                    if primary_was_io {
                        consistency.push_read_error(
                            expected_kind,
                            ptr.toc_1_start as u64,
                            "reading primary area TOC header",
                            primary_msg.clone(),
                        );
                    }
                    if backup_was_io {
                        consistency.push_read_error(
                            expected_kind,
                            ptr.toc_2_start as u64,
                            "reading redundant area TOC header",
                            backup_msg.clone(),
                        );
                    }
                    let msg = format!(
                        "both area TOC copies failed: primary LSN {}: {}; redundant LSN {}: {}",
                        ptr.toc_1_start, primary_msg, ptr.toc_2_start, backup_msg
                    );
                    let error = if primary_was_io && backup_was_io {
                        SacdError::Io(msg)
                    } else {
                        SacdError::Malformed(msg)
                    };
                    return Err(area_parse_failure(error, consistency));
                }
            }
        }
    };
    let area_kind = expected_kind.unwrap_or(header.kind);
    validate_area_header_consistency(
        ptr,
        expected_kind,
        header_start_lsn,
        total_sectors,
        &header,
        &mut consistency,
    );

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
    let mut scan_buf = vec![0u8; SECTOR_SIZE as usize];
    let mut i: u64 = 1;
    while i < max_scan {
        let lsn = header_start_lsn + i;
        if let Err(e) = sector_reader.read_sector_into(lsn, &mut scan_buf) {
            consistency.push_read_error(
                Some(area_kind),
                lsn,
                format!("scanning area TOC sector {} of {}", i, max_scan),
                e.to_string(),
            );
            break;
        }
        match &scan_buf[0..8] {
            m if m == SACD_TRL1_MAGIC => {
                match parse_trl1(&scan_buf, header.track_count) {
                    Ok(v) => starts = Some(v),
                    Err(e) => consistency.push_error(
                        TocConsistencyCheck::TrackList1Length,
                        Some(area_kind),
                        None,
                        format!("SACDTRL1 parse failed at LSN {}: {}", lsn, e),
                    ),
                }
                i += 1;
            }
            m if m == SACD_TRL2_MAGIC => {
                match parse_trl2(&scan_buf, header.track_count) {
                    Ok(v) => times = Some(v),
                    Err(e) => consistency.push_error(
                        TocConsistencyCheck::TrackList2Duration,
                        Some(area_kind),
                        None,
                        format!("SACDTRL2 parse failed at LSN {}: {}", lsn, e),
                    ),
                }
                i += 1;
            }
            m if m == SACD_T_TXT_MAGIC => {
                if !got_text {
                    if let Ok(v) = parse_sacd_t_txt(&scan_buf, header.track_count, area_charset) {
                        text_per_track = v;
                        got_text = true;
                    }
                }
                i += 1;
            }
            m if m == SACD_IGL_MAGIC => {
                // SACD_IGL spans 2 sectors. Concatenate before parse.
                let next_lsn = lsn + 1;
                let mut buf2 = vec![0u8; SECTOR_SIZE as usize];
                match sector_reader.read_sector_into(next_lsn, &mut buf2) {
                    Ok(()) => {
                        let mut full = Vec::with_capacity(2 * SECTOR_SIZE as usize);
                        full.extend_from_slice(&scan_buf);
                        full.extend_from_slice(&buf2);
                        if let Ok(v) = parse_sacd_igl(&full, header.track_count) {
                            isrc_genre = v;
                        }
                    }
                    Err(e) => {
                        consistency.push_read_warning(
                            Some(area_kind),
                            next_lsn,
                            "reading SACD_IGL continuation sector",
                            e.to_string(),
                        );
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
            structured_isrc: ig.structured_isrc,
            genre: ig.genre,
        });
    }

    validate_track_list_consistency(area_kind, &header, starts.as_deref(), times.as_deref(), &mut consistency);

    Ok(AreaInfo { header, tracks, consistency })
}

// SB-AUDIT: SB-CHECK-001
fn validate_master_area_pointers(master_toc: &MasterToc, total_sectors: u64) -> TocConsistencyReport {
    let mut report = TocConsistencyReport::default();
    validate_area_pointer(AreaKind::Stereo, master_toc.two_channel, total_sectors, &mut report);
    validate_area_pointer(
        AreaKind::MultiChannel,
        master_toc.multi_channel,
        total_sectors,
        &mut report,
    );
    report
}

fn validate_area_pointer(
    kind: AreaKind,
    ptr: AreaPointer,
    total_sectors: u64,
    report: &mut TocConsistencyReport,
) {
    let partially_present = ptr.toc_1_start != 0 || ptr.toc_2_start != 0 || ptr.toc_size_sectors != 0;
    if !ptr.is_present() {
        if partially_present {
            report.push_error(
                TocConsistencyCheck::MasterAreaPointer,
                Some(kind),
                None,
                format!(
                    "{:?} area pointer is partial: toc_1_start={}, toc_2_start={}, toc_size_sectors={}",
                    kind, ptr.toc_1_start, ptr.toc_2_start, ptr.toc_size_sectors
                ),
            );
        }
        return;
    }

    if ptr.toc_size_sectors > 96 {
        report.push_error(
            TocConsistencyCheck::AreaSizeBounds,
            Some(kind),
            None,
            format!(
                "{:?} area pointer declares {} TOC sectors; spec maximum is 96",
                kind, ptr.toc_size_sectors
            ),
        );
    }

    validate_lsn_span(
        TocConsistencyCheck::MasterAreaPointer,
        Some(kind),
        None,
        ptr.toc_1_start as u64,
        ptr.toc_size_sectors as u64,
        total_sectors,
        "primary area TOC pointer",
        report,
    );
    if ptr.toc_2_start != 0 {
        validate_lsn_span(
            TocConsistencyCheck::MasterAreaPointer,
            Some(kind),
            None,
            ptr.toc_2_start as u64,
            ptr.toc_size_sectors as u64,
            total_sectors,
            "redundant area TOC pointer",
            report,
        );
    }
}

// SB-AUDIT: SB-CHECK-002..SB-CHECK-004
fn validate_area_header_consistency(
    ptr: AreaPointer,
    expected_kind: Option<AreaKind>,
    header_start_lsn: u64,
    total_sectors: u64,
    header: &AreaTocHeader,
    report: &mut TocConsistencyReport,
) {
    let area = Some(expected_kind.unwrap_or(header.kind));

    if let Some(expected) = expected_kind {
        if header.kind != expected {
            report.push_error(
                TocConsistencyCheck::AreaPointerKind,
                area,
                None,
                format!(
                    "area pointer expected {:?} TOC but header at LSN {} is {:?}",
                    expected, header_start_lsn, header.kind
                ),
            );
        }
    }

    if header.size_sectors == 0 || header.size_sectors > 96 {
        report.push_error(
            TocConsistencyCheck::AreaSizeBounds,
            area,
            None,
            format!(
                "area TOC header declares {} sectors; valid range is 1..=96",
                header.size_sectors
            ),
        );
    }
    if ptr.toc_size_sectors != 0 && header.size_sectors != ptr.toc_size_sectors {
        report.push_error(
            TocConsistencyCheck::AreaSizeBounds,
            area,
            None,
            format!(
                "master pointer TOC size {} disagrees with area header TOC size {}",
                ptr.toc_size_sectors, header.size_sectors
            ),
        );
    }
    validate_lsn_span(
        TocConsistencyCheck::AreaSizeBounds,
        area,
        None,
        header_start_lsn,
        header.size_sectors as u64,
        total_sectors,
        "area TOC header span",
        report,
    );

    if header.track_count == 0 {
        report.push_error(
            TocConsistencyCheck::TrackCount,
            area,
            None,
            "area TOC declares zero tracks",
        );
    }

    if header.track_start_lsn == 0 || header.track_end_lsn == 0 || header.track_start_lsn >= header.track_end_lsn {
        report.push_error(
            TocConsistencyCheck::TrackSectorRange,
            area,
            None,
            format!(
                "invalid area audio bounds: track_start_lsn={}, track_end_lsn={}",
                header.track_start_lsn, header.track_end_lsn
            ),
        );
    } else {
        validate_lsn_span(
            TocConsistencyCheck::TrackSectorRange,
            area,
            None,
            header.track_start_lsn as u64,
            (header.track_end_lsn - header.track_start_lsn) as u64,
            total_sectors,
            "area audio span",
            report,
        );
    }
}

fn validate_track_list_consistency(
    area_kind: AreaKind,
    header: &AreaTocHeader,
    starts: Option<&[(u32, u32)]>,
    times: Option<&[(PlayTime, PlayTime)]>,
    report: &mut TocConsistencyReport,
) {
    let area = Some(area_kind);
    let track_count = header.track_count as usize;

    match starts {
        Some(v) if v.len() == track_count => {
            let mut previous_start: Option<u32> = None;
            let mut previous_end: Option<u64> = None;
            for (idx, &(start, len)) in v.iter().enumerate() {
                let track_index = Some((idx + 1) as u8);
                if start == 0 {
                    report.push_error(
                        TocConsistencyCheck::TrackList1Length,
                        area,
                        track_index,
                        "SACDTRL1 track start LSN is zero",
                    );
                }
                if len == 0 {
                    report.push_error(
                        TocConsistencyCheck::TrackList1Length,
                        area,
                        track_index,
                        "SACDTRL1 track length is zero sectors",
                    );
                }
                let end = match (start as u64).checked_add(len as u64) {
                    Some(end) => end,
                    None => {
                        report.push_error(
                            TocConsistencyCheck::TrackList1Length,
                            area,
                            track_index,
                            format!("SACDTRL1 track range overflows: start={}, length={}", start, len),
                        );
                        continue;
                    }
                };
                if start < header.track_start_lsn || end > header.track_end_lsn as u64 {
                    report.push_error(
                        TocConsistencyCheck::TrackSectorRange,
                        area,
                        track_index,
                        format!(
                            "track LSN range [{}..{}) falls outside area audio bounds [{}..{})",
                            start, end, header.track_start_lsn, header.track_end_lsn
                        ),
                    );
                }
                if let Some(prev_start) = previous_start {
                    if start < prev_start {
                        report.push_error(
                            TocConsistencyCheck::TrackSectorRange,
                            area,
                            track_index,
                            format!(
                                "track start LSN {} is before previous track start LSN {}",
                                start, prev_start
                            ),
                        );
                    }
                }
                if let Some(prev_end) = previous_end {
                    if (start as u64) < prev_end {
                        report.push_error(
                            TocConsistencyCheck::TrackSectorRange,
                            area,
                            track_index,
                            format!(
                                "track starts at LSN {} before previous track ends at LSN {}",
                                start, prev_end
                            ),
                        );
                    } else if (start as u64) > prev_end {
                        report.push_warning(
                            TocConsistencyCheck::TrackSectorRange,
                            area,
                            track_index,
                            format!(
                                "gap of {} sectors before track {}",
                                (start as u64) - prev_end,
                                idx + 1
                            ),
                        );
                    }
                }
                previous_start = Some(start);
                previous_end = Some(end);
            }
            if let Some(&(first, _)) = v.first() {
                if first != header.track_start_lsn {
                    report.push_warning(
                        TocConsistencyCheck::TrackSectorRange,
                        area,
                        Some(1),
                        format!(
                            "first SACDTRL1 start LSN {} differs from area track_start_lsn {}",
                            first, header.track_start_lsn
                        ),
                    );
                }
            }
            if let Some(last_end) = previous_end {
                if last_end != header.track_end_lsn as u64 {
                    report.push_warning(
                        TocConsistencyCheck::TrackSectorRange,
                        area,
                        Some(header.track_count),
                        format!(
                            "last SACDTRL1 end LSN {} differs from area track_end_lsn {}",
                            last_end, header.track_end_lsn
                        ),
                    );
                }
            }
        }
        Some(v) => report.push_error(
            TocConsistencyCheck::TrackCount,
            area,
            None,
            format!(
                "SACDTRL1 decoded {} tracks but area header declares {}",
                v.len(), track_count
            ),
        ),
        None if header.track_count != 0 => report.push_error(
            TocConsistencyCheck::TrackList1Length,
            area,
            None,
            "SACDTRL1 track LSN/length list is missing",
        ),
        None => {}
    }

    match times {
        Some(v) if v.len() == track_count => {
            let mut previous_end: Option<u32> = None;
            let mut last_end: Option<u32> = None;
            let mut duration_sum: u32 = 0;
            for (idx, &(start, duration)) in v.iter().enumerate() {
                let track_index = Some((idx + 1) as u8);
                if !start.is_normalized() {
                    report.push_error(
                        TocConsistencyCheck::TrackList2Duration,
                        area,
                        track_index,
                        format!(
                            "SACDTRL2 start time is not normalized: {:02}:{:02}:{:02}",
                            start.minutes, start.seconds, start.frames
                        ),
                    );
                }
                if !duration.is_normalized() {
                    report.push_error(
                        TocConsistencyCheck::TrackList2Duration,
                        area,
                        track_index,
                        format!(
                            "SACDTRL2 duration is not normalized: {:02}:{:02}:{:02}",
                            duration.minutes, duration.seconds, duration.frames
                        ),
                    );
                }
                if duration.is_zero() {
                    report.push_error(
                        TocConsistencyCheck::TrackList2Duration,
                        area,
                        track_index,
                        "SACDTRL2 duration is zero",
                    );
                }
                let start_frames = start.as_frame_count();
                let duration_frames = duration.as_frame_count();
                duration_sum = duration_sum.saturating_add(duration_frames);
                let end_frames = start_frames.saturating_add(duration_frames);
                if let Some(prev_end) = previous_end {
                    if start_frames < prev_end {
                        report.push_error(
                            TocConsistencyCheck::TrackList2Duration,
                            area,
                            track_index,
                            format!(
                                "track starts at frame {} before previous track ends at frame {}",
                                start_frames, prev_end
                            ),
                        );
                    } else if start_frames > prev_end {
                        report.push_warning(
                            TocConsistencyCheck::TrackList2Duration,
                            area,
                            track_index,
                            format!(
                                "timecode gap of {} frames before track {}",
                                start_frames - prev_end,
                                idx + 1
                            ),
                        );
                    }
                }
                previous_end = Some(end_frames);
                last_end = Some(end_frames);
            }

            let area_total = header.total_playtime.as_frame_count();
            if area_total != 0 {
                if let Some(last_end) = last_end {
                    let delta = frame_delta(last_end, area_total);
                    if delta > SACD_FRAME_RATE {
                        report.push_warning(
                            TocConsistencyCheck::AreaDuration,
                            area,
                            None,
                            format!(
                                "last SACDTRL2 end frame {} differs from area total_playtime {} by {} frames",
                                last_end, area_total, delta
                            ),
                        );
                    }
                }
                let sum_delta = frame_delta(duration_sum, area_total);
                if sum_delta > SACD_FRAME_RATE {
                    report.push_warning(
                        TocConsistencyCheck::AreaDuration,
                        area,
                        None,
                        format!(
                            "sum of SACDTRL2 durations {} differs from area total_playtime {} by {} frames",
                            duration_sum, area_total, sum_delta
                        ),
                    );
                }
            }
        }
        Some(v) => report.push_error(
            TocConsistencyCheck::TrackCount,
            area,
            None,
            format!(
                "SACDTRL2 decoded {} tracks but area header declares {}",
                v.len(), track_count
            ),
        ),
        None if header.track_count != 0 => report.push_error(
            TocConsistencyCheck::TrackList2Duration,
            area,
            None,
            "SACDTRL2 track time/duration list is missing",
        ),
        None => {}
    }
}

fn validate_lsn_span(
    check: TocConsistencyCheck,
    area: Option<AreaKind>,
    track_index: Option<u8>,
    start_lsn: u64,
    sectors: u64,
    total_sectors: u64,
    label: &str,
    report: &mut TocConsistencyReport,
) {
    if sectors == 0 {
        report.push_error(
            check,
            area,
            track_index,
            format!("{} has zero length at LSN {}", label, start_lsn),
        );
        return;
    }
    let Some(end_lsn) = start_lsn.checked_add(sectors) else {
        report.push_error(
            check,
            area,
            track_index,
            format!("{} overflows: start={}, sectors={}", label, start_lsn, sectors),
        );
        return;
    };
    if end_lsn > total_sectors {
        report.push_error(
            check,
            area,
            track_index,
            format!(
                "{} [{}..{}) exceeds ISO complete-sector count {}",
                label, start_lsn, end_lsn, total_sectors
            ),
        );
    }
}

fn frame_delta(a: u32, b: u32) -> u32 {
    if a >= b { a - b } else { b - a }
}

/// Read and parse an area TOC header from the given LSN. Returns
/// Err if the read fails or the magic / structure is malformed.
/// Used by `parse_area` with toc_1 then (on failure) toc_2.
fn try_area_header_at(reader: &mut SectorReader, lsn: u64) -> Result<AreaTocHeader, SacdError> {
    let buf = reader.read_sector(lsn)?;
    parse_area_toc_header(&buf)
}

fn file_complete_sector_count(path: &Path) -> Result<u64, SacdError> {
    let size = std::fs::metadata(path)
        .map_err(|e| SacdError::Io(format!("metadata: {}", e)))?
        .len();
    Ok(size / SECTOR_SIZE)
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
        let b = build_area_toc_sector(TWOCH_TOC_MAGIC, 1, 7 /* undocumented but known */);
        let h = parse_area_toc_header(&b).expect("parse");
        assert_eq!(h.frame_format, FrameFormat::Dsd7);
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
            b[start_base + i * 4 + 3] = 0;
            b[dur_base + i * 4 + 0] = d.minutes;
            b[dur_base + i * 4 + 1] = d.seconds;
            b[dur_base + i * 4 + 2] = d.frames;
            b[dur_base + i * 4 + 3] = 0;
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

    #[test]
    fn parse_trl2_matches_sacd_extract_time_entry_layout() {
        let mut b = vec![0u8; SECTOR_SIZE as usize];
        b[0..8].copy_from_slice(SACD_TRL2_MAGIC);
        let start_base = 8;
        let dur_base = 8 + 255 * 4;
        b[start_base..start_base + 4].copy_from_slice(&[0x45, 0x2a, 0x22, 0x00]);
        b[dur_base..dur_base + 4].copy_from_slice(&[0x0c, 0x3b, 0x1f, 0x00]);

        let v = parse_trl2(&b, 1).expect("parse");
        assert_eq!(
            v[0].0,
            PlayTime {
                minutes: 0x45,
                seconds: 0x2a,
                frames: 0x22,
            }
        );
        assert_eq!(
            v[0].1,
            PlayTime {
                minutes: 0x0c,
                seconds: 0x3b,
                frames: 0x1f,
            }
        );
        assert_eq!(v[0].0.as_frame_count(), 313_684);
        assert_eq!(v[0].1.as_frame_count(), 58_456);
    }

    #[test]
    fn play_time_counts_seconds_above_59() {
        let t = PlayTime {
            minutes: 40,
            seconds: 61,
            frames: 0,
        };
        assert_eq!(t.as_frame_count(), 40 * 60 * SACD_FRAME_RATE + 61 * SACD_FRAME_RATE);
        assert!(t.is_normalized());
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
        assert_eq!(v[0].structured_isrc.unwrap().to_string(), "USAA10800001");
        assert!(v[0].structured_isrc.unwrap().is_valid());
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



    #[test]
    fn parse_sacd_iso_reports_clean_toc_consistency_for_matching_lists() {
        use std::io::{Seek, SeekFrom, Write};
        let td = tempfile::tempdir().expect("tempdir");
        let path = td.path().join("toc_clean.iso");
        let total_sectors = 900u64;
        let f = std::fs::File::create(&path).unwrap();
        f.set_len(total_sectors * SECTOR_SIZE).unwrap();
        drop(f);

        let mut f = std::fs::File::options().write(true).open(&path).unwrap();
        let mut mtoc = vec![0u8; MASTER_TOC_T_SIZE];
        mtoc[0..8].copy_from_slice(MASTER_TOC_MAGIC);
        mtoc[0x08] = 1;
        mtoc[0x09] = 20;
        mtoc[0x10..0x12].copy_from_slice(&1u16.to_be_bytes());
        mtoc[0x12..0x14].copy_from_slice(&1u16.to_be_bytes());
        mtoc[0x40..0x44].copy_from_slice(&540u32.to_be_bytes());
        mtoc[0x54..0x56].copy_from_slice(&3u16.to_be_bytes());
        f.seek(SeekFrom::Start(510 * SECTOR_SIZE)).unwrap();
        f.write_all(&mtoc).unwrap();

        let mut area = build_area_toc_sector(TWOCH_TOC_MAGIC, 2, 2);
        area[0x40] = 3;
        area[0x41] = 0;
        area[0x42] = 0;
        area[0x48..0x4c].copy_from_slice(&600u32.to_be_bytes());
        area[0x4c..0x50].copy_from_slice(&850u32.to_be_bytes());
        f.seek(SeekFrom::Start(540 * SECTOR_SIZE)).unwrap();
        f.write_all(&area).unwrap();

        let trl1 = build_trl1_sector(&[(600, 100), (700, 150)]);
        f.seek(SeekFrom::Start(541 * SECTOR_SIZE)).unwrap();
        f.write_all(&trl1).unwrap();

        let trl2 = build_trl2_sector(&[
            (
                PlayTime { minutes: 0, seconds: 0, frames: 0 },
                PlayTime { minutes: 1, seconds: 0, frames: 0 },
            ),
            (
                PlayTime { minutes: 1, seconds: 0, frames: 0 },
                PlayTime { minutes: 2, seconds: 0, frames: 0 },
            ),
        ]);
        f.seek(SeekFrom::Start(542 * SECTOR_SIZE)).unwrap();
        f.write_all(&trl2).unwrap();
        drop(f);

        let md = parse_sacd_iso_with_strictness(&path, true).expect("strict clean TOC");
        assert!(md.consistency.is_clean(), "{:?}", md.consistency.issues);
        assert!(md.stereo.as_ref().unwrap().consistency.is_clean());
    }

    #[test]
    fn parse_sacd_iso_records_precise_toc_read_event_for_truncated_area_scan() {
        use std::io::{Seek, SeekFrom, Write};
        let td = tempfile::tempdir().expect("tempdir");
        let path = td.path().join("toc_truncated_scan.iso");

        // File contains sectors 0..541. The area header declares a 3-sector
        // TOC at 540, so the scan of LSN 542 must produce an exact read event
        // rather than only a later "missing SACDTRL2" consistency error.
        let total_sectors = 542u64;
        let f = std::fs::File::create(&path).unwrap();
        f.set_len(total_sectors * SECTOR_SIZE).unwrap();
        drop(f);

        let mut f = std::fs::File::options().write(true).open(&path).unwrap();
        let mut mtoc = vec![0u8; MASTER_TOC_T_SIZE];
        mtoc[0..8].copy_from_slice(MASTER_TOC_MAGIC);
        mtoc[0x08] = 1;
        mtoc[0x09] = 20;
        mtoc[0x10..0x12].copy_from_slice(&1u16.to_be_bytes());
        mtoc[0x12..0x14].copy_from_slice(&1u16.to_be_bytes());
        mtoc[0x40..0x44].copy_from_slice(&540u32.to_be_bytes());
        mtoc[0x54..0x56].copy_from_slice(&3u16.to_be_bytes());
        f.seek(SeekFrom::Start(510 * SECTOR_SIZE)).unwrap();
        f.write_all(&mtoc).unwrap();

        let mut area = build_area_toc_sector(TWOCH_TOC_MAGIC, 1, 2);
        area[0x0a..0x0c].copy_from_slice(&3u16.to_be_bytes());
        area[0x48..0x4c].copy_from_slice(&600u32.to_be_bytes());
        area[0x4c..0x50].copy_from_slice(&700u32.to_be_bytes());
        f.seek(SeekFrom::Start(540 * SECTOR_SIZE)).unwrap();
        f.write_all(&area).unwrap();

        f.seek(SeekFrom::Start(541 * SECTOR_SIZE)).unwrap();
        f.write_all(&build_trl1_sector(&[(600, 100)])).unwrap();
        drop(f);

        let md = parse_sacd_iso(&path).expect("non-strict parse keeps report");
        let stereo = md.stereo.as_ref().expect("stereo area present");
        assert!(stereo.consistency.read_events.iter().any(|event| {
            event.lsn == 542
                && event.area == Some(AreaKind::Stereo)
                && event.context.contains("scanning area TOC sector")
                && event.error.contains("read LSN 542")
        }));
        assert!(md.consistency.read_events.iter().any(|event| event.lsn == 542));
        assert!(md
            .consistency
            .issues
            .iter()
            .any(|issue| issue.check == TocConsistencyCheck::TocSectorRead
                && issue.message.contains("LSN 542")));
        assert!(parse_sacd_iso_with_strictness(&path, true).is_err());
    }

    #[test]
    fn parse_sacd_iso_reports_toc_consistency_failures_non_strict() {
        use std::io::{Seek, SeekFrom, Write};
        let td = tempfile::tempdir().expect("tempdir");
        let path = td.path().join("toc_bad.iso");
        let total_sectors = 700u64;
        let f = std::fs::File::create(&path).unwrap();
        f.set_len(total_sectors * SECTOR_SIZE).unwrap();
        drop(f);

        let mut f = std::fs::File::options().write(true).open(&path).unwrap();
        let mut mtoc = vec![0u8; MASTER_TOC_T_SIZE];
        mtoc[0..8].copy_from_slice(MASTER_TOC_MAGIC);
        mtoc[0x08] = 1;
        mtoc[0x09] = 20;
        mtoc[0x10..0x12].copy_from_slice(&1u16.to_be_bytes());
        mtoc[0x12..0x14].copy_from_slice(&1u16.to_be_bytes());
        mtoc[0x40..0x44].copy_from_slice(&540u32.to_be_bytes());
        mtoc[0x54..0x56].copy_from_slice(&120u16.to_be_bytes()); // invalid: >96 and mismatches header
        f.seek(SeekFrom::Start(510 * SECTOR_SIZE)).unwrap();
        f.write_all(&mtoc).unwrap();

        let mut area = build_area_toc_sector(TWOCH_TOC_MAGIC, 2, 2);
        area[0x40] = 1;
        area[0x41] = 0;
        area[0x42] = 0;
        area[0x48..0x4c].copy_from_slice(&600u32.to_be_bytes());
        area[0x4c..0x50].copy_from_slice(&650u32.to_be_bytes());
        f.seek(SeekFrom::Start(540 * SECTOR_SIZE)).unwrap();
        f.write_all(&area).unwrap();

        let trl1 = build_trl1_sector(&[(600, 100), (650, 0)]);
        f.seek(SeekFrom::Start(541 * SECTOR_SIZE)).unwrap();
        f.write_all(&trl1).unwrap();

        let trl2 = build_trl2_sector(&[
            (
                PlayTime { minutes: 0, seconds: 0, frames: 0 },
                PlayTime { minutes: 1, seconds: 0, frames: 0 },
            ),
            (
                PlayTime { minutes: 0, seconds: 30, frames: 0 },
                PlayTime { minutes: 0, seconds: 60, frames: 0 },
            ),
        ]);
        f.seek(SeekFrom::Start(542 * SECTOR_SIZE)).unwrap();
        f.write_all(&trl2).unwrap();
        drop(f);

        let md = parse_sacd_iso(&path).expect("non-strict keeps metadata and report");
        assert!(md.consistency.has_errors());
        assert!(md.consistency.issues.iter().any(|issue| issue.check == TocConsistencyCheck::AreaSizeBounds));
        assert!(md.consistency.issues.iter().any(|issue| issue.check == TocConsistencyCheck::TrackSectorRange));
        assert!(md.consistency.issues.iter().any(|issue| issue.check == TocConsistencyCheck::TrackList1Length));
        assert!(md.consistency.issues.iter().any(|issue| issue.check == TocConsistencyCheck::TrackList2Duration));
        assert!(parse_sacd_iso_with_strictness(&path, true).is_err());
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
    fn parse_sacd_iso_preserves_both_unreadable_area_toc_events() {
        use std::io::{Seek, SeekFrom, Write};
        let td = tempfile::tempdir().expect("tempdir");
        let path = td.path().join("both_unreadable_area_tocs.iso");
        let total = 700u64;
        let f = std::fs::File::create(&path).unwrap();
        f.set_len(total * SECTOR_SIZE).unwrap();
        drop(f);

        let mut mtoc = vec![0u8; MASTER_TOC_T_SIZE];
        mtoc[0..8].copy_from_slice(MASTER_TOC_MAGIC);
        mtoc[0x08] = 1;
        mtoc[0x09] = 20;
        mtoc[0x40..0x44].copy_from_slice(&700u32.to_be_bytes());
        mtoc[0x44..0x48].copy_from_slice(&701u32.to_be_bytes());
        mtoc[0x54..0x56].copy_from_slice(&1u16.to_be_bytes());
        let mut f = std::fs::File::options().write(true).open(&path).unwrap();
        f.seek(SeekFrom::Start(510 * SECTOR_SIZE)).unwrap();
        f.write_all(&mtoc).unwrap();
        drop(f);

        let md = parse_sacd_iso(&path).expect("non-strict keeps forensic report");
        assert!(md.stereo.is_none());

        let primary = md.consistency.read_events.iter().find(|event| {
            event.area == Some(AreaKind::Stereo)
                && event.lsn == 700
                && event.context.contains("primary area TOC header")
        });
        let redundant = md.consistency.read_events.iter().find(|event| {
            event.area == Some(AreaKind::Stereo)
                && event.lsn == 701
                && event.context.contains("redundant area TOC header")
        });

        assert!(primary.is_some(), "missing primary event: {:?}", md.consistency.read_events);
        assert!(redundant.is_some(), "missing redundant event: {:?}", md.consistency.read_events);
        assert_eq!(primary.unwrap().severity, TocConsistencySeverity::Error);
        assert_eq!(redundant.unwrap().severity, TocConsistencySeverity::Error);
        assert!(parse_sacd_iso_with_strictness(&path, true).is_err());
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


    fn build_extraction_test_area(frame_format_nibble: u8) -> AreaInfo {
        let header = parse_area_toc_header(&build_area_toc_sector(
            TWOCH_TOC_MAGIC,
            1,
            frame_format_nibble,
        ))
        .expect("area header");
        AreaInfo {
            header,
            tracks: vec![TrackEntry {
                start_lsn: 600,
                length_lsn: 14,
                start_time: PlayTime::default(),
                duration: PlayTime {
                    minutes: 0,
                    seconds: 0,
                    frames: 1,
                },
                ..TrackEntry::default()
            }],
            consistency: TocConsistencyReport::default(),
        }
    }

    #[test]
    fn area_integrity_options_plumb_area_frame_format() {
        let area = build_extraction_test_area(0); // DST
        let strict = area.track_integrity_options(SacdExtractionMode::Strict);
        assert_eq!(strict.frame_format, Some(sacd_rs::FrameFormat::Dst));
        assert!(!strict.recover_sector_errors);
        assert!(strict.strict_channel_count);

        let salvage = area.track_integrity_options(SacdExtractionMode::Salvage);
        assert_eq!(salvage.frame_format, Some(sacd_rs::FrameFormat::Dst));
        assert!(salvage.recover_sector_errors);
        assert!(!salvage.strict_channel_count);
    }

    #[test]
    fn area_track_extract_options_use_toc_start_and_area_end_for_single_track() {
        let area = build_extraction_test_area(2); // DSD3-in-14
        let opts = area
            .track_extract_options(0, sacd_rs::extract::OutputFormat::Dff)
            .expect("track options");
        assert_eq!(opts.start_lsn, area.header.track_start_lsn as u64);
        assert_eq!(opts.end_lsn, area.header.track_end_lsn as u64 + 1);
        assert_eq!(opts.channel_count, area.header.channel_count);
        assert_eq!(
            opts.time_filter,
            Some(sacd_rs::extract::TimeFilter::new(
                area.tracks[0].start_time.as_frame_count(),
                area.tracks[0].duration.as_frame_count(),
            ))
        );
    }

    #[test]
    fn non_final_track_extract_options_use_next_track_start_plus_one() {
        let mut area = build_extraction_test_area(2); // DSD3-in-14
        area.header.track_start_lsn = 540;
        area.header.track_end_lsn = 900;
        area.tracks = vec![
            TrackEntry {
                start_lsn: 540,
                length_lsn: 50,
                start_time: PlayTime { minutes: 0, seconds: 0, frames: 0 },
                duration: PlayTime { minutes: 0, seconds: 8, frames: 0 },
                ..TrackEntry::default()
            },
            TrackEntry {
                start_lsn: 600,
                length_lsn: 100,
                start_time: PlayTime { minutes: 0, seconds: 8, frames: 0 },
                duration: PlayTime { minutes: 0, seconds: 4, frames: 0 },
                ..TrackEntry::default()
            },
        ];

        let opts = area
            .track_extract_options(0, sacd_rs::extract::OutputFormat::Dff)
            .expect("track options");

        assert_eq!(opts.start_lsn, 540);
        assert_eq!(opts.end_lsn, 601);
        assert_eq!(
            opts.time_filter,
            Some(sacd_rs::extract::TimeFilter::new(
                area.tracks[0].start_time.as_frame_count(),
                area.tracks[0].duration.as_frame_count(),
            ))
        );
    }

    #[test]
    fn final_track_extract_options_use_area_track_end_plus_one_not_trl1_length() {
        let mut area = build_extraction_test_area(2); // DSD3-in-14
        area.header.track_start_lsn = 540;
        area.header.track_end_lsn = 700;
        area.tracks = vec![
            TrackEntry {
                start_lsn: 540,
                length_lsn: 50,
                start_time: PlayTime { minutes: 0, seconds: 0, frames: 0 },
                duration: PlayTime { minutes: 0, seconds: 8, frames: 0 },
                ..TrackEntry::default()
            },
            TrackEntry {
                start_lsn: 600,
                length_lsn: 1_000,
                start_time: PlayTime { minutes: 0, seconds: 8, frames: 0 },
                duration: PlayTime { minutes: 0, seconds: 4, frames: 0 },
                ..TrackEntry::default()
            },
        ];

        let opts = area
            .track_extract_options(1, sacd_rs::extract::OutputFormat::Dff)
            .expect("track options");

        assert_eq!(opts.start_lsn, 600);
        assert_eq!(opts.end_lsn, 701);
        assert_eq!(
            opts.time_filter,
            Some(sacd_rs::extract::TimeFilter::new(
                area.tracks[1].start_time.as_frame_count(),
                area.tracks[1].duration.as_frame_count(),
            ))
        );
    }

    #[test]
    fn metadata_extraction_entry_reports_missing_area_before_opening_iso() {
        let md = SacdMetadata {
            master_toc: parse_master_toc(&{
                let mut b = vec![0u8; MASTER_TOC_T_SIZE];
                b[0..8].copy_from_slice(MASTER_TOC_MAGIC);
                b[0x08] = 1;
                b[0x09] = 20;
                b[0x40..0x44].copy_from_slice(&540u32.to_be_bytes());
                b[0x54..0x56].copy_from_slice(&1u16.to_be_bytes());
                b
            })
            .expect("master toc"),
            master_text: None,
            stereo: Some(build_extraction_test_area(2)),
            multi_channel: None,
            consistency: TocConsistencyReport::default(),
        };
        let mut output = std::io::Cursor::new(Vec::<u8>::new());
        let err = md
            .extract_track_from_path(
                std::path::Path::new("/definitely/not/present.iso"),
                AreaKind::MultiChannel,
                0,
                &mut output,
                sacd_rs::extract::OutputFormat::Dff,
                SacdExtractionMode::Strict,
            )
            .expect_err("missing area should fail before ISO open");
        assert!(matches!(err, SacdExtractionError::AreaMissing { area: AreaKind::MultiChannel }));
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
