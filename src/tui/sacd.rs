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
                write!(f, "SACD: file too small ({} bytes, need at least {})", size, required)
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

    let Ok(mut f) = File::open(path) else { return false; };
    let Ok(meta) = f.metadata() else { return false; };
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
        if f.seek(SeekFrom::Start(offset)).is_err() { continue; }
        if f.read_exact(&mut buf).is_err() { continue; }
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

    let Ok(mut f) = File::open(path) else { return DetectionResult::IoFailure; };
    let Ok(meta) = f.metadata() else { return DetectionResult::IoFailure; };
    let size = meta.len();

    let min_size = MASTER_TOC_LSNS[0] * SECTOR_SIZE + MASTER_TOC_MAGIC.len() as u64;
    if size < min_size {
        return DetectionResult::TooSmall;
    }

    let mut good: u8 = 0;
    let mut buf = [0u8; 8];
    for &lsn in MASTER_TOC_LSNS.iter() {
        let offset = lsn * SECTOR_SIZE;
        if offset + buf.len() as u64 > size { continue; }
        if f.seek(SeekFrom::Start(offset)).is_err() { continue; }
        if f.read_exact(&mut buf).is_err() { continue; }
        if &buf == MASTER_TOC_MAGIC { good += 1; }
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
        if year == 0 { None } else { Some(DiscDate { year, month, day }) }
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
        return Err(SacdError::TooSmall { size, required: min_size });
    }

    let mut last_malformed: Option<SacdError> = None;
    let mut saw_any_magic = false;
    let mut buf = vec![0u8; MASTER_TOC_T_SIZE];

    for &lsn in MASTER_TOC_LSNS.iter() {
        let offset = lsn * SECTOR_SIZE;
        if offset + MASTER_TOC_T_SIZE as u64 > size {
            continue;
        }
        if f.seek(SeekFrom::Start(offset)).is_err() { continue; }
        if f.read_exact(&mut buf).is_err() { continue; }
        if &buf[0..8] != MASTER_TOC_MAGIC { continue; }
        saw_any_magic = true;
        match parse_master_toc(&buf) {
            Ok(toc) => return Ok(toc),
            Err(e) => last_malformed = Some(e),
        }
    }

    if let Some(e) = last_malformed { return Err(e); }
    if saw_any_magic {
        // Defensive: magic matched but we never produced an Ok or
        // Err above. Shouldn't be reachable but keeps the function
        // total.
        return Err(SacdError::Malformed("master TOC: parse loop exited without result".into()));
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Build a synthetic ISO file at `path` with `MASTER_TOC_MAGIC`
    /// placed at the given LSNs. Other LSNs get zero bytes.
    fn write_synthetic_iso(
        path: &std::path::Path,
        magic_at_lsns: &[u64],
    ) -> std::io::Result<()> {
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
        assert!(is_sacd_iso(&path), "should accept TOC at LSN 520 when 510 is bare");

        // Only third copy present.
        let path = td.path().join("c.iso");
        write_synthetic_iso(&path, &[530]).expect("write");
        assert!(is_sacd_iso(&path), "should accept TOC at LSN 530 when 510/520 are bare");
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
        assert!(!is_sacd_iso(&path), "8-byte file is too small to host Master TOC");
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
        b[0x08] = 1;     // major
        b[0x09] = 20;    // minor
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
        b[0x88] = b'e'; b[0x89] = b'n'; b[0x8a] = 2;
        b[0x8c] = b'j'; b[0x8d] = b'a'; b[0x8e] = 3;

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
        assert_eq!(toc.disc_date, Some(DiscDate { year: 2003, month: 8, day: 15 }));
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
        f.seek(SeekFrom::Start(MASTER_TOC_LSNS[0] * SECTOR_SIZE)).unwrap();
        f.write_all(b"NOTSACD!").unwrap();
        // Valid master TOC at LSN 520.
        f.seek(SeekFrom::Start(MASTER_TOC_LSNS[1] * SECTOR_SIZE)).unwrap();
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
        let e = SacdError::TooSmall { size: 100, required: 1_044_488 };
        assert!(format!("{}", e).contains("100"));
        assert!(format!("{}", SacdError::NotSacdIso).contains("not a valid SACD"));
        assert!(format!("{}", SacdError::Io("test".into())).contains("test"));
        assert!(format!("{}", SacdError::Malformed("bad".into())).contains("bad"));
    }
}
