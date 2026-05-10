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

    #[test]
    fn sacd_error_display_messages() {
        let e = SacdError::TooSmall { size: 100, required: 1_044_488 };
        assert!(format!("{}", e).contains("100"));
        assert!(format!("{}", SacdError::NotSacdIso).contains("not a valid SACD"));
        assert!(format!("{}", SacdError::Io("test".into())).contains("test"));
        assert!(format!("{}", SacdError::Malformed("bad".into())).contains("bad"));
    }
}
