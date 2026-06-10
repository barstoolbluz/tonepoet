#![forbid(unsafe_code)]

//! Read-only ISO9660 bridge backend for DVD-Audio images.
//!
//! Some DVD-Audio ISO images expose `AUDIO_TS` through the ISO9660 bridge even
//! when the UDF index is absent or unreadable. This backend indexes only the
//! `AUDIO_TS` directory and exposes files through the existing `DvdaVolume`
//! trait. It is intentionally read-only and bounded.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::tui::dvda::error::{DvdaError, Result};
use crate::tui::dvda::volume::{DvdaFile, DvdaVolume};

const ISO_SECTOR_SIZE: u64 = 2048;
const ISO_SECTOR_SIZE_USIZE: usize = 2048;
const VOLUME_DESCRIPTOR_START_SECTOR: u64 = 16;
const PRIMARY_VOLUME_DESCRIPTOR: u8 = 1;
const VOLUME_DESCRIPTOR_SET_TERMINATOR: u8 = 255;
const ISO9660_MAGIC: &[u8; 5] = b"CD001";
const ROOT_DIRECTORY_RECORD_OFFSET: usize = 156;
const MAX_VOLUME_DESCRIPTORS: u64 = 256;
const MAX_DIRECTORY_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct Iso9660DvdaVolume {
    iso_path: PathBuf,
    files: BTreeMap<String, Iso9660Entry>,
}

impl Iso9660DvdaVolume {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let iso_path = path.into();
        let mut file = File::open(&iso_path).map_err(|source| DvdaError::io(iso_path.display().to_string(), source))?;
        let root = read_primary_volume_descriptor(&mut file, &iso_path)?;
        let files = index_audio_ts_files(&mut file, &iso_path, root)?;
        Ok(Self { iso_path, files })
    }

    pub fn iso_path(&self) -> &Path {
        &self.iso_path
    }

    pub fn audio_ts_file_names(&self) -> impl Iterator<Item = &str> {
        self.files.keys().map(String::as_str)
    }
}

impl DvdaVolume for Iso9660DvdaVolume {
    fn open_audio_ts_file(&self, name: &str) -> Result<Box<dyn DvdaFile>> {
        let key = sanitize_audio_ts_name(name)?;
        let Some(entry) = self.files.get(&key) else {
            return Err(DvdaError::MissingFile {
                candidates: vec![format!("{}:AUDIO_TS/{key}", self.iso_path.display())],
            });
        };
        Ok(Box::new(Iso9660File::open(&self.iso_path, entry)?))
    }

    fn file_len(&self, name: &str) -> Result<Option<u64>> {
        let key = sanitize_audio_ts_name(name)?;
        Ok(self.files.get(&key).map(|entry| entry.len))
    }
}

#[derive(Clone, Debug)]
struct Iso9660Entry {
    name: String,
    extent_lba: u32,
    len: u64,
    is_dir: bool,
}

impl Iso9660Entry {
    fn offset(&self) -> u64 {
        u64::from(self.extent_lba) * ISO_SECTOR_SIZE
    }
}

struct Iso9660File {
    file: File,
    offset: u64,
    len: u64,
    pos: u64,
}

impl Iso9660File {
    fn open(path: &Path, entry: &Iso9660Entry) -> Result<Self> {
        let file = File::open(path).map_err(|source| DvdaError::io(path.display().to_string(), source))?;
        Ok(Self {
            file,
            offset: entry.offset(),
            len: entry.len,
            pos: 0,
        })
    }
}

impl DvdaFile for Iso9660File {
    fn len(&self) -> u64 {
        self.len
    }
}

impl Read for Iso9660File {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.len || out.is_empty() {
            return Ok(0);
        }
        let count = (self.len - self.pos).min(out.len() as u64) as usize;
        self.file.seek(SeekFrom::Start(self.offset + self.pos))?;
        let read = self.file.read(&mut out[..count])?;
        self.pos += read as u64;
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
        self.pos = next as u64;
        Ok(self.pos)
    }
}

fn read_primary_volume_descriptor(file: &mut File, path: &Path) -> Result<Iso9660Entry> {
    let mut sector = [0_u8; ISO_SECTOR_SIZE_USIZE];
    for index in 0..MAX_VOLUME_DESCRIPTORS {
        let sector_nr = VOLUME_DESCRIPTOR_START_SECTOR + index;
        file.seek(SeekFrom::Start(sector_nr * ISO_SECTOR_SIZE))
            .map_err(|source| DvdaError::io(path.display().to_string(), source))?;
        let read = file.read(&mut sector).map_err(|source| DvdaError::io(path.display().to_string(), source))?;
        if read == 0 {
            break;
        }
        if read < ISO_SECTOR_SIZE_USIZE {
            return Err(DvdaError::ShortRead {
                context: format!("ISO9660 volume descriptor sector {sector_nr}"),
                needed: ISO_SECTOR_SIZE_USIZE,
                available: read,
            });
        }
        if &sector[1..6] != ISO9660_MAGIC || sector[6] != 1 {
            continue;
        }
        match sector[0] {
            PRIMARY_VOLUME_DESCRIPTOR => {
                let record = parse_directory_record(
                    &sector[ROOT_DIRECTORY_RECORD_OFFSET..],
                    "ISO9660 primary volume descriptor root directory",
                )?;
                if !record.is_dir {
                    return Err(DvdaError::parse("ISO9660 primary volume descriptor", "root record is not a directory"));
                }
                return Ok(record);
            }
            VOLUME_DESCRIPTOR_SET_TERMINATOR => break,
            _ => {}
        }
    }

    Err(DvdaError::MissingFile {
        candidates: vec![format!("{}:ISO9660 primary volume descriptor", path.display())],
    })
}

fn index_audio_ts_files(file: &mut File, path: &Path, root: Iso9660Entry) -> Result<BTreeMap<String, Iso9660Entry>> {
    let root_entries = read_directory(file, path, &root, "ISO9660 root directory")?;
    let audio_ts = root_entries
        .into_iter()
        .find(|entry| entry.is_dir && canonical_iso_name(&entry.name).eq_ignore_ascii_case("AUDIO_TS"))
        .ok_or_else(|| DvdaError::MissingFile {
            candidates: vec![format!("{}:AUDIO_TS", path.display())],
        })?;

    let mut files = BTreeMap::new();
    for mut entry in read_directory(file, path, &audio_ts, "ISO9660 AUDIO_TS directory")? {
        if entry.is_dir {
            continue;
        }
        let key = canonical_iso_name(&entry.name);
        if key.is_empty() {
            continue;
        }
        entry.name = key.clone();
        files.entry(key).or_insert(entry);
    }
    Ok(files)
}

fn read_directory(file: &mut File, path: &Path, dir: &Iso9660Entry, context: &str) -> Result<Vec<Iso9660Entry>> {
    if dir.len > MAX_DIRECTORY_BYTES {
        return Err(DvdaError::parse(
            context,
            format!("directory is too large to index safely: {} bytes", dir.len),
        ));
    }
    let mut data = vec![0_u8; dir.len as usize];
    file.seek(SeekFrom::Start(dir.offset()))
        .map_err(|source| DvdaError::io(path.display().to_string(), source))?;
    file.read_exact(&mut data).map_err(|source| DvdaError::io(path.display().to_string(), source))?;

    let mut entries = Vec::new();
    let mut offset = 0usize;
    while offset < data.len() {
        let record_len = data[offset] as usize;
        if record_len == 0 {
            let next_sector = ((offset / ISO_SECTOR_SIZE_USIZE) + 1) * ISO_SECTOR_SIZE_USIZE;
            if next_sector <= offset {
                break;
            }
            offset = next_sector;
            continue;
        }
        if offset + record_len > data.len() {
            return Err(DvdaError::ShortRead {
                context: format!("{context} directory record"),
                needed: offset + record_len,
                available: data.len(),
            });
        }
        let record = parse_directory_record(&data[offset..offset + record_len], context)?;
        if record.name != "\0" && record.name != "\x01" {
            entries.push(record);
        }
        offset += record_len;
    }
    Ok(entries)
}

fn parse_directory_record(bytes: &[u8], context: &str) -> Result<Iso9660Entry> {
    if bytes.len() < 34 {
        return Err(DvdaError::ShortRead {
            context: context.to_string(),
            needed: 34,
            available: bytes.len(),
        });
    }
    let record_len = bytes[0] as usize;
    if record_len != 0 && record_len > bytes.len() {
        return Err(DvdaError::ShortRead {
            context: context.to_string(),
            needed: record_len,
            available: bytes.len(),
        });
    }
    let name_len = bytes[32] as usize;
    let name_end = 33usize.checked_add(name_len).ok_or_else(|| DvdaError::parse(context, "directory-record name length overflow"))?;
    if name_end > bytes.len() {
        return Err(DvdaError::ShortRead {
            context: format!("{context} directory-record name"),
            needed: name_end,
            available: bytes.len(),
        });
    }

    let extent_lba = u32::from_le_bytes([bytes[2], bytes[3], bytes[4], bytes[5]]);
    let len = u32::from_le_bytes([bytes[10], bytes[11], bytes[12], bytes[13]]) as u64;
    let is_dir = bytes[25] & 0x02 != 0;
    let name_bytes = &bytes[33..name_end];
    let name = if name_bytes == [0] {
        "\0".to_string()
    } else if name_bytes == [1] {
        "\x01".to_string()
    } else {
        String::from_utf8_lossy(name_bytes).into_owned()
    };

    Ok(Iso9660Entry {
        name,
        extent_lba,
        len,
        is_dir,
    })
}

fn canonical_iso_name(name: &str) -> String {
    let without_version = name.split_once(';').map(|(stem, _)| stem).unwrap_or(name);
    without_version.trim_end_matches('.').to_ascii_uppercase()
}

fn sanitize_audio_ts_name(name: &str) -> Result<String> {
    if name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
        || name == "."
        || name == ".."
    {
        return Err(DvdaError::parse("AUDIO_TS filename", format!("invalid filename {name:?}")));
    }
    Ok(canonical_iso_name(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DVDA_AMG_MAGIC: &[u8] = b"DVDAUDIO-AMG";

    #[test]
    fn reads_audio_ts_file_from_minimal_iso9660_image() {
        let path = temp_test_path("iso9660_volume_reads_audio_ts.iso");
        std::fs::write(&path, minimal_iso9660_dvda_image(true)).expect("write ISO fixture");

        let volume = Iso9660DvdaVolume::open(&path).expect("open ISO9660 bridge");
        assert_eq!(volume.file_len("AUDIO_TS.IFO").expect("file len"), Some(DVDA_AMG_MAGIC.len() as u64));
        assert_eq!(volume.read_audio_ts_file("AUDIO_TS.IFO").expect("read IFO"), DVDA_AMG_MAGIC);
        assert_eq!(volume.read_audio_ts_file("audio_ts.ifo").expect("case-insensitive read"), DVDA_AMG_MAGIC);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn rejects_iso9660_image_without_audio_ts_path() {
        let path = temp_test_path("iso9660_volume_rejects_missing_audio_ts.iso");
        std::fs::write(&path, minimal_iso9660_dvda_image(false)).expect("write ISO fixture");

        assert!(matches!(Iso9660DvdaVolume::open(&path), Err(DvdaError::MissingFile { .. })));

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
        let mut image = vec![0_u8; ISO_SECTOR_SIZE_USIZE * 24];

        let root_record = iso9660_test_record(&[0], 18, ISO_SECTOR_SIZE as u32, 0x02);
        let pvd = &mut image[ISO_SECTOR_SIZE_USIZE * 16..ISO_SECTOR_SIZE_USIZE * 17];
        pvd[0] = 1;
        pvd[1..6].copy_from_slice(b"CD001");
        pvd[6] = 1;
        pvd[156..156 + root_record.len()].copy_from_slice(&root_record);

        let vdst = &mut image[ISO_SECTOR_SIZE_USIZE * 17..ISO_SECTOR_SIZE_USIZE * 18];
        vdst[0] = 255;
        vdst[1..6].copy_from_slice(b"CD001");
        vdst[6] = 1;

        if include_path {
            let root_dir = &mut image[ISO_SECTOR_SIZE_USIZE * 18..ISO_SECTOR_SIZE_USIZE * 19];
            let mut offset = 0usize;
            for record in [
                iso9660_test_record(&[0], 18, ISO_SECTOR_SIZE as u32, 0x02),
                iso9660_test_record(&[1], 18, ISO_SECTOR_SIZE as u32, 0x02),
                iso9660_test_record(b"AUDIO_TS", 19, ISO_SECTOR_SIZE as u32, 0x02),
            ] {
                root_dir[offset..offset + record.len()].copy_from_slice(&record);
                offset += record.len();
            }

            let audio_dir = &mut image[ISO_SECTOR_SIZE_USIZE * 19..ISO_SECTOR_SIZE_USIZE * 20];
            let mut offset = 0usize;
            for record in [
                iso9660_test_record(&[0], 19, ISO_SECTOR_SIZE as u32, 0x02),
                iso9660_test_record(&[1], 18, ISO_SECTOR_SIZE as u32, 0x02),
                iso9660_test_record(b"AUDIO_TS.IFO;1", 20, DVDA_AMG_MAGIC.len() as u32, 0x00),
            ] {
                audio_dir[offset..offset + record.len()].copy_from_slice(&record);
                offset += record.len();
            }
        }

        let ifo = &mut image[ISO_SECTOR_SIZE_USIZE * 20..ISO_SECTOR_SIZE_USIZE * 20 + DVDA_AMG_MAGIC.len()];
        ifo.copy_from_slice(DVDA_AMG_MAGIC);
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
