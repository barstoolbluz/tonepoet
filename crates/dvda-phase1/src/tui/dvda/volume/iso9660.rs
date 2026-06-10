#![forbid(unsafe_code)]

//! Read-only DVD-Audio ISO9660 bridge backend.
//!
//! Some DVD-Audio images include an ISO9660 bridge in addition to UDF. Detection
//! may therefore prove `/AUDIO_TS/AUDIO_TS.IFO` through ISO9660 even when the UDF
//! backend cannot mount the image. This volume backend keeps the detection and
//! materialization evidence path aligned by exposing the same AUDIO_TS files
//! through the `DvdaVolume` trait.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::tui::dvda::error::{DvdaError, Result};
use crate::tui::dvda::volume::{DvdaFile, DvdaVolume};

const DVD_SECTOR_SIZE: u64 = 2048;
const DVD_SECTOR_SIZE_USIZE: usize = 2048;
const FIRST_VOLUME_DESCRIPTOR_SECTOR: u64 = 16;
const MAX_VOLUME_DESCRIPTORS: u32 = 256;
const MAX_DIRECTORY_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct Iso9660DvdaVolume {
    iso_path: PathBuf,
    files: BTreeMap<String, Iso9660IndexedFile>,
}

impl Iso9660DvdaVolume {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let iso_path = path.into();
        let mut reader = Iso9660Reader::open(&iso_path)?;
        let files = reader.index_audio_ts_files()?;
        if !files.contains_key("AUDIO_TS.IFO") {
            return Err(DvdaError::MissingFile {
                candidates: vec![format!("{}:AUDIO_TS/AUDIO_TS.IFO", iso_path.display())],
            });
        }
        Ok(Self { iso_path, files })
    }

    pub fn iso_path(&self) -> &Path {
        &self.iso_path
    }

    pub fn audio_ts_file_names(&self) -> impl Iterator<Item = &str> {
        self.files.values().map(|file| file.name.as_str())
    }
}

impl DvdaVolume for Iso9660DvdaVolume {
    fn open_audio_ts_file(&self, name: &str) -> Result<Box<dyn DvdaFile>> {
        let key = sanitize_audio_ts_name(name)?;
        let Some(indexed) = self.files.get(&key) else {
            return Err(DvdaError::MissingFile {
                candidates: vec![format!("{}:AUDIO_TS/{key}", self.iso_path.display())],
            });
        };
        Ok(Box::new(Iso9660File::open(&self.iso_path, indexed)?))
    }

    fn file_len(&self, name: &str) -> Result<Option<u64>> {
        let key = sanitize_audio_ts_name(name)?;
        Ok(self.files.get(&key).map(|file| file.len))
    }
}

#[derive(Clone, Debug)]
struct Iso9660IndexedFile {
    name: String,
    extent_lba: u32,
    len: u64,
}

struct Iso9660File {
    file: File,
    start: u64,
    pos: u64,
    len: u64,
}

impl Iso9660File {
    fn open(iso_path: &Path, indexed: &Iso9660IndexedFile) -> Result<Self> {
        let file = File::open(iso_path).map_err(|source| DvdaError::io(iso_path.display().to_string(), source))?;
        Ok(Self {
            file,
            start: u64::from(indexed.extent_lba) * DVD_SECTOR_SIZE,
            pos: 0,
            len: indexed.len,
        })
    }
}

impl DvdaFile for Iso9660File {
    fn len(&self) -> u64 {
        self.len
    }
}

impl Read for Iso9660File {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || self.pos >= self.len {
            return Ok(0);
        }
        let count = (self.len - self.pos).min(buf.len() as u64) as usize;
        self.file.seek(SeekFrom::Start(self.start + self.pos))?;
        let read = self.file.read(&mut buf[..count])?;
        self.pos = self.pos.saturating_add(read as u64);
        Ok(read)
    }
}

impl Seek for Iso9660File {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let next = match pos {
            SeekFrom::Start(value) => value as i128,
            SeekFrom::End(value) => self.len as i128 + value as i128,
            SeekFrom::Current(value) => self.pos as i128 + value as i128,
        };
        if next < 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "negative seek in ISO9660 file"));
        }
        self.pos = (next as u64).min(self.len);
        Ok(self.pos)
    }
}

#[derive(Debug, Clone, Copy)]
struct Iso9660DirRecord {
    extent_lba: u32,
    data_len: u32,
    file_flags: u8,
}

impl Iso9660DirRecord {
    fn is_directory(self) -> bool {
        self.file_flags & 0x02 != 0
    }
}

#[derive(Debug, Clone, Copy)]
struct Iso9660Root {
    record: Iso9660DirRecord,
    joliet: bool,
}

struct Iso9660Reader {
    path: PathBuf,
    file: File,
}

impl Iso9660Reader {
    fn open(path: &Path) -> Result<Self> {
        let file = File::open(path).map_err(|source| DvdaError::io(path.display().to_string(), source))?;
        Ok(Self { path: path.to_path_buf(), file })
    }

    fn index_audio_ts_files(&mut self) -> Result<BTreeMap<String, Iso9660IndexedFile>> {
        for root in self.roots()? {
            if let Some(audio_ts) = self.find_child(root.record, root.joliet, "AUDIO_TS", true)? {
                let files = self.index_directory(audio_ts, root.joliet)?;
                if !files.is_empty() {
                    return Ok(files);
                }
            }
        }
        Err(DvdaError::MissingFile {
            candidates: vec![format!("{}:AUDIO_TS/AUDIO_TS.IFO", self.path.display())],
        })
    }

    fn roots(&mut self) -> Result<Vec<Iso9660Root>> {
        let mut roots = Vec::new();
        let mut sector = FIRST_VOLUME_DESCRIPTOR_SECTOR;
        for _ in 0..MAX_VOLUME_DESCRIPTORS {
            let bytes = match self.read_sector(sector) {
                Ok(bytes) => bytes,
                Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(err) => return Err(DvdaError::io(self.path.display().to_string(), err)),
            };
            if &bytes[1..6] != b"CD001" || bytes[6] != 1 {
                break;
            }
            match bytes[0] {
                1 => {
                    if let Some((record, _)) = parse_directory_record(&bytes[156..], false) {
                        roots.push(Iso9660Root { record, joliet: false });
                    }
                }
                2 => {
                    if let Some((record, _)) = parse_directory_record(&bytes[156..], true) {
                        roots.push(Iso9660Root { record, joliet: true });
                    }
                }
                255 => break,
                _ => {}
            }
            sector = sector.saturating_add(1);
        }
        Ok(roots)
    }

    fn find_child(
        &mut self,
        directory: Iso9660DirRecord,
        joliet: bool,
        wanted_name: &str,
        wanted_directory: bool,
    ) -> Result<Option<Iso9660DirRecord>> {
        if !directory.is_directory() {
            return Ok(None);
        }
        let bytes = self.read_record_bytes(directory)?;
        let mut offset = 0usize;
        while offset < bytes.len() {
            let record_len = bytes[offset] as usize;
            if record_len == 0 {
                let next_sector = ((offset / DVD_SECTOR_SIZE_USIZE) + 1) * DVD_SECTOR_SIZE_USIZE;
                if next_sector <= offset {
                    return Ok(None);
                }
                offset = next_sector;
                continue;
            }
            if offset + record_len > bytes.len() {
                return Ok(None);
            }
            let Some((record, name)) = parse_directory_record(&bytes[offset..offset + record_len], joliet) else {
                return Ok(None);
            };
            if iso9660_name_matches(&name, wanted_name) && record.is_directory() == wanted_directory {
                return Ok(Some(record));
            }
            offset += record_len;
        }
        Ok(None)
    }

    fn index_directory(&mut self, directory: Iso9660DirRecord, joliet: bool) -> Result<BTreeMap<String, Iso9660IndexedFile>> {
        if !directory.is_directory() {
            return Ok(BTreeMap::new());
        }
        let bytes = self.read_record_bytes(directory)?;
        let mut files = BTreeMap::new();
        let mut offset = 0usize;
        while offset < bytes.len() {
            let record_len = bytes[offset] as usize;
            if record_len == 0 {
                let next_sector = ((offset / DVD_SECTOR_SIZE_USIZE) + 1) * DVD_SECTOR_SIZE_USIZE;
                if next_sector <= offset {
                    break;
                }
                offset = next_sector;
                continue;
            }
            if offset + record_len > bytes.len() {
                break;
            }
            if let Some((record, name)) = parse_directory_record(&bytes[offset..offset + record_len], joliet) {
                if !record.is_directory() && name != "." && name != ".." {
                    let normalized = normalize_iso9660_file_name(&name);
                    if !normalized.is_empty() {
                        files.insert(
                            normalized.clone(),
                            Iso9660IndexedFile {
                                name: normalized,
                                extent_lba: record.extent_lba,
                                len: u64::from(record.data_len),
                            },
                        );
                    }
                }
            }
            offset += record_len;
        }
        Ok(files)
    }

    fn read_record_bytes(&mut self, record: Iso9660DirRecord) -> Result<Vec<u8>> {
        let len = u64::from(record.data_len);
        if len > MAX_DIRECTORY_BYTES {
            return Err(DvdaError::Iso {
                message: format!("ISO9660 directory is too large: {len} bytes"),
            });
        }
        let mut bytes = vec![0_u8; len as usize];
        self.file
            .seek(SeekFrom::Start(u64::from(record.extent_lba) * DVD_SECTOR_SIZE))
            .map_err(|source| DvdaError::io(self.path.display().to_string(), source))?;
        self.file
            .read_exact(&mut bytes)
            .map_err(|source| DvdaError::io(self.path.display().to_string(), source))?;
        Ok(bytes)
    }

    fn read_sector(&mut self, sector: u64) -> io::Result<[u8; DVD_SECTOR_SIZE_USIZE]> {
        let mut bytes = [0_u8; DVD_SECTOR_SIZE_USIZE];
        self.file.seek(SeekFrom::Start(sector * DVD_SECTOR_SIZE))?;
        self.file.read_exact(&mut bytes)?;
        Ok(bytes)
    }
}

fn parse_directory_record(bytes: &[u8], joliet: bool) -> Option<(Iso9660DirRecord, String)> {
    let record_len = *bytes.first()? as usize;
    if record_len == 0 || record_len > bytes.len() || record_len < 34 {
        return None;
    }
    let name_len = *bytes.get(32)? as usize;
    let name_start = 33usize;
    let name_end = name_start.checked_add(name_len)?;
    if name_end > record_len {
        return None;
    }
    let extent_lba = u32::from_le_bytes(bytes.get(2..6)?.try_into().ok()?);
    let data_len = u32::from_le_bytes(bytes.get(10..14)?.try_into().ok()?);
    let file_flags = *bytes.get(25)?;
    let raw_name = &bytes[name_start..name_end];
    let name = decode_iso9660_name(raw_name, joliet);
    Some((Iso9660DirRecord { extent_lba, data_len, file_flags }, name))
}

fn decode_iso9660_name(raw_name: &[u8], joliet: bool) -> String {
    if raw_name == [0] {
        return ".".to_string();
    }
    if raw_name == [1] {
        return "..".to_string();
    }
    if joliet && raw_name.len() % 2 == 0 {
        let mut units = Vec::with_capacity(raw_name.len() / 2);
        for chunk in raw_name.chunks_exact(2) {
            units.push(u16::from_be_bytes([chunk[0], chunk[1]]));
        }
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(raw_name).to_string()
    }
}

fn iso9660_name_matches(actual: &str, wanted: &str) -> bool {
    normalize_iso9660_file_name(actual) == wanted.to_ascii_uppercase()
}

fn normalize_iso9660_file_name(name: &str) -> String {
    let mut actual = name.to_ascii_uppercase();
    if let Some((prefix, _version)) = actual.split_once(';') {
        actual = prefix.to_string();
    }
    while actual.ends_with('.') {
        actual.pop();
    }
    actual
}

fn sanitize_audio_ts_name(name: &str) -> Result<String> {
    let raw = name.replace('\\', "/");
    if raw.contains("..") {
        return Err(DvdaError::Iso { message: format!("invalid ISO9660 AUDIO_TS path: {name}") });
    }
    let Some(file_name) = raw.rsplit('/').next().filter(|part| !part.is_empty()) else {
        return Err(DvdaError::Iso { message: format!("invalid ISO9660 AUDIO_TS path: {name}") });
    };
    Ok(normalize_iso9660_file_name(file_name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::path::PathBuf;

    const DVDA_AMG_MAGIC: &[u8] = b"DVDAUDIO-AMG";

    #[test]
    fn opens_audio_ts_ifo_through_iso9660_bridge() {
        let path = temp_test_path("dvda_iso9660_volume.iso");
        std::fs::write(&path, minimal_iso9660_dvda_image(true)).expect("write ISO fixture");

        let volume = Iso9660DvdaVolume::open(&path).expect("ISO9660 DVD-Audio volume");
        let mut ifo = volume.open_audio_ts_file("AUDIO_TS.IFO").expect("AUDIO_TS.IFO");
        let mut magic = vec![0_u8; DVDA_AMG_MAGIC.len()];
        ifo.read_exact(&mut magic).expect("read magic");
        assert_eq!(magic, DVDA_AMG_MAGIC);
        assert_eq!(volume.file_len("AUDIO_TS.IFO").expect("len"), Some(DVDA_AMG_MAGIC.len() as u64));

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn rejects_stray_magic_without_audio_ts_path() {
        let path = temp_test_path("dvda_iso9660_volume_stray.iso");
        let mut bytes = vec![0_u8; DVD_SECTOR_SIZE_USIZE * 24];
        bytes[DVD_SECTOR_SIZE_USIZE * 20..DVD_SECTOR_SIZE_USIZE * 20 + DVDA_AMG_MAGIC.len()]
            .copy_from_slice(DVDA_AMG_MAGIC);
        std::fs::write(&path, bytes).expect("write ISO fixture");

        assert!(Iso9660DvdaVolume::open(&path).is_err());

        let _ = std::fs::remove_file(path);
    }

    fn temp_test_path(name: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        std::env::temp_dir().join(format!("tonepoet_{nonce}_{name}"))
    }

    fn minimal_iso9660_dvda_image(include_path: bool) -> Vec<u8> {
        let mut image = vec![0_u8; DVD_SECTOR_SIZE_USIZE * 24];

        let root_record = iso9660_test_record(&[0], 18, DVD_SECTOR_SIZE as u32, 0x02);
        let pvd = &mut image[DVD_SECTOR_SIZE_USIZE * 16..DVD_SECTOR_SIZE_USIZE * 17];
        pvd[0] = 1;
        pvd[1..6].copy_from_slice(b"CD001");
        pvd[6] = 1;
        pvd[156..156 + root_record.len()].copy_from_slice(&root_record);

        let vdst = &mut image[DVD_SECTOR_SIZE_USIZE * 17..DVD_SECTOR_SIZE_USIZE * 18];
        vdst[0] = 255;
        vdst[1..6].copy_from_slice(b"CD001");
        vdst[6] = 1;

        if include_path {
            let root_dir = &mut image[DVD_SECTOR_SIZE_USIZE * 18..DVD_SECTOR_SIZE_USIZE * 19];
            let mut offset = 0usize;
            for record in [
                iso9660_test_record(&[0], 18, DVD_SECTOR_SIZE as u32, 0x02),
                iso9660_test_record(&[1], 18, DVD_SECTOR_SIZE as u32, 0x02),
                iso9660_test_record(b"AUDIO_TS", 19, DVD_SECTOR_SIZE as u32, 0x02),
            ] {
                root_dir[offset..offset + record.len()].copy_from_slice(&record);
                offset += record.len();
            }

            let audio_dir = &mut image[DVD_SECTOR_SIZE_USIZE * 19..DVD_SECTOR_SIZE_USIZE * 20];
            let mut offset = 0usize;
            for record in [
                iso9660_test_record(&[0], 19, DVD_SECTOR_SIZE as u32, 0x02),
                iso9660_test_record(&[1], 18, DVD_SECTOR_SIZE as u32, 0x02),
                iso9660_test_record(b"AUDIO_TS.IFO;1", 20, DVDA_AMG_MAGIC.len() as u32, 0x00),
            ] {
                audio_dir[offset..offset + record.len()].copy_from_slice(&record);
                offset += record.len();
            }
        }

        image[DVD_SECTOR_SIZE_USIZE * 20..DVD_SECTOR_SIZE_USIZE * 20 + DVDA_AMG_MAGIC.len()]
            .copy_from_slice(DVDA_AMG_MAGIC);
        image
    }

    fn iso9660_test_record(name: &[u8], extent_lba: u32, data_len: u32, file_flags: u8) -> Vec<u8> {
        let len_without_padding = 33 + name.len();
        let record_len = len_without_padding + (len_without_padding % 2);
        let mut record = vec![0_u8; record_len];
        record[0] = record_len as u8;
        record[2..6].copy_from_slice(&extent_lba.to_le_bytes());
        record[6..10].copy_from_slice(&extent_lba.to_be_bytes());
        record[10..14].copy_from_slice(&data_len.to_le_bytes());
        record[14..18].copy_from_slice(&data_len.to_be_bytes());
        record[25] = file_flags;
        record[28..30].copy_from_slice(&1_u16.to_le_bytes());
        record[30..32].copy_from_slice(&1_u16.to_be_bytes());
        record[32] = name.len() as u8;
        record[33..33 + name.len()].copy_from_slice(name);
        record
    }
}
