//! Sector-aligned ISO sector reader. SACDs use a fixed 2048-byte
//! sector size; this module exposes a thin wrapper over `File` that
//! lets callers read by Logical Sector Number (LSN).

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

/// SACD logical sector size (ScarletBook spec). Mirrors
/// `tui::sacd::SECTOR_SIZE` in tonepoet's parser.
pub const SECTOR_SIZE: u64 = 2048;

/// Read SACD sectors from an ISO file by LSN.
pub struct IsoReader {
    file: File,
}

impl IsoReader {
    pub fn open(path: &Path) -> io::Result<Self> {
        Ok(Self {
            file: File::open(path)?,
        })
    }

    /// Read a single sector at `lsn` into `buf`. `buf` must be at
    /// least `SECTOR_SIZE` bytes; only the first `SECTOR_SIZE` are
    /// written. Returns `Err` on short read.
    pub fn read_sector(&mut self, lsn: u64, buf: &mut [u8]) -> io::Result<()> {
        assert!(
            buf.len() >= SECTOR_SIZE as usize,
            "read_sector: buffer too small ({} < {})",
            buf.len(),
            SECTOR_SIZE,
        );
        self.file.seek(SeekFrom::Start(lsn * SECTOR_SIZE))?;
        self.file.read_exact(&mut buf[..SECTOR_SIZE as usize])?;
        Ok(())
    }

    /// Read `count` consecutive sectors starting at `lsn` into `buf`.
    /// `buf` must be at least `count * SECTOR_SIZE` bytes.
    pub fn read_sectors(&mut self, lsn: u64, count: usize, buf: &mut [u8]) -> io::Result<()> {
        let need = count * SECTOR_SIZE as usize;
        assert!(buf.len() >= need, "read_sectors: buffer too small");
        self.file.seek(SeekFrom::Start(lsn * SECTOR_SIZE))?;
        self.file.read_exact(&mut buf[..need])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn read_sector_returns_2048_bytes_at_lsn() {
        let td = tempfile::tempdir().unwrap();
        let path = td.path().join("test.iso");
        let mut f = File::create(&path).unwrap();
        // Sector 0: all 0xAA
        f.write_all(&[0xAA; SECTOR_SIZE as usize]).unwrap();
        // Sector 1: all 0xBB
        f.write_all(&[0xBB; SECTOR_SIZE as usize]).unwrap();
        // Sector 2: all 0xCC
        f.write_all(&[0xCC; SECTOR_SIZE as usize]).unwrap();
        drop(f);

        let mut reader = IsoReader::open(&path).unwrap();
        let mut buf = [0u8; SECTOR_SIZE as usize];
        reader.read_sector(0, &mut buf).unwrap();
        assert!(buf.iter().all(|&b| b == 0xAA));
        reader.read_sector(2, &mut buf).unwrap();
        assert!(buf.iter().all(|&b| b == 0xCC));
        reader.read_sector(1, &mut buf).unwrap();
        assert!(buf.iter().all(|&b| b == 0xBB));
    }

    #[test]
    fn read_sectors_reads_contiguous_range() {
        let td = tempfile::tempdir().unwrap();
        let path = td.path().join("test.iso");
        let mut f = File::create(&path).unwrap();
        for v in [0x11u8, 0x22, 0x33] {
            f.write_all(&vec![v; SECTOR_SIZE as usize]).unwrap();
        }
        drop(f);

        let mut reader = IsoReader::open(&path).unwrap();
        let mut buf = vec![0u8; 3 * SECTOR_SIZE as usize];
        reader.read_sectors(0, 3, &mut buf).unwrap();
        assert!(buf[..SECTOR_SIZE as usize].iter().all(|&b| b == 0x11));
        assert!(buf[SECTOR_SIZE as usize..2 * SECTOR_SIZE as usize]
            .iter()
            .all(|&b| b == 0x22));
        assert!(buf[2 * SECTOR_SIZE as usize..].iter().all(|&b| b == 0x33));
    }

    #[test]
    fn read_sector_errors_on_eof() {
        let td = tempfile::tempdir().unwrap();
        let path = td.path().join("short.iso");
        let mut f = File::create(&path).unwrap();
        f.write_all(&[0x42; 100]).unwrap();
        drop(f);
        let mut reader = IsoReader::open(&path).unwrap();
        let mut buf = [0u8; SECTOR_SIZE as usize];
        assert!(reader.read_sector(0, &mut buf).is_err());
    }
}
