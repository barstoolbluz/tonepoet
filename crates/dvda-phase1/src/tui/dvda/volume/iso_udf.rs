#![forbid(unsafe_code)]

//! Read-only DVD-Audio ISO/UDF backend.
//!
//! This backend parses enough UDF 1.02/ECMA-167 structure to expose files in
//! `AUDIO_TS` through the existing `DvdaVolume` trait. Phase 2 uses it to parse
//! IFO/BUP/MKB files and collect AOB byte sizes without copying AOB payloads
//! into staging. Phase 3 can reuse the same backend for bounded AOB range reads.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::tui::dvda::error::{DvdaError, Result};
use crate::tui::dvda::volume::{DvdaFile, DvdaVolume};

const DVD_SECTOR_SIZE: u64 = 2048;
const DVD_SECTOR_SIZE_USIZE: usize = 2048;
const MAX_DESCRIPTOR_SEQUENCE_SECTORS: u32 = 4096;
const MAX_DIRECTORY_BYTES: u64 = 16 * 1024 * 1024;
const MAX_FILE_ENTRY_BYTES: u64 = 64 * 1024;

const TAG_ANCHOR_VOLUME_DESCRIPTOR_POINTER: u16 = 2;
const TAG_PARTITION_DESCRIPTOR: u16 = 5;
const TAG_LOGICAL_VOLUME_DESCRIPTOR: u16 = 6;
const TAG_TERMINATING_DESCRIPTOR: u16 = 8;
const TAG_FILE_SET_DESCRIPTOR: u16 = 256;
const TAG_FILE_IDENTIFIER_DESCRIPTOR: u16 = 257;
const TAG_FILE_ENTRY: u16 = 261;
const TAG_EXTENDED_FILE_ENTRY: u16 = 266;

const FID_DELETED: u8 = 0x04;
const FID_PARENT: u8 = 0x08;

#[derive(Clone, Debug)]
pub struct IsoUdfDvdaVolume {
    iso_path: PathBuf,
    files: BTreeMap<String, IndexedFile>,
}

impl IsoUdfDvdaVolume {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let iso_path = path.into();
        let mut reader = UdfReader::open(&iso_path)?;
        let files = reader.index_audio_ts_files()?;
        Ok(Self { iso_path, files })
    }

    pub fn iso_path(&self) -> &Path {
        &self.iso_path
    }

    pub fn audio_ts_file_names(&self) -> impl Iterator<Item = &str> {
        self.files.values().map(|file| file.name.as_str())
    }

    /// Return indexed UDF metadata for one file in `AUDIO_TS`, without reading
    /// its payload. This is used by fixture tests and Phase 3 planning code to
    /// prove that ISO/UDF file-size and extent facts are available before audio
    /// demux starts.
    pub fn audio_ts_file_info(&self, name: &str) -> Result<Option<UdfAudioTsFileInfo>> {
        let key = sanitize_audio_ts_name(name)?;
        Ok(self.files.get(&key).map(IndexedFile::to_public_info))
    }

    /// Return indexed UDF metadata for every file found under `AUDIO_TS`, without
    /// reading file payloads.
    pub fn audio_ts_files_info(&self) -> Vec<UdfAudioTsFileInfo> {
        self.files.values().map(IndexedFile::to_public_info).collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UdfAudioTsFileInfo {
    pub name: String,
    pub len: u64,
    pub storage: UdfFileStorageKind,
    pub extents: Vec<UdfFileExtent>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UdfFileStorageKind {
    Extents,
    Inline,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UdfFileExtent {
    /// Absolute byte offset in the ISO image.
    pub offset: u64,
    /// Byte count for this extent after truncation to the file information
    /// length reported by UDF.
    pub len: u64,
}

impl DvdaVolume for IsoUdfDvdaVolume {
    fn open_audio_ts_file(&self, name: &str) -> Result<Box<dyn DvdaFile>> {
        let key = sanitize_audio_ts_name(name)?;
        let Some(indexed) = self.files.get(&key) else {
            return Err(DvdaError::MissingFile {
                candidates: vec![format!("{}:AUDIO_TS/{key}", self.iso_path.display())],
            });
        };

        Ok(Box::new(IsoUdfFile::open(&self.iso_path, indexed)?))
    }

    fn file_len(&self, name: &str) -> Result<Option<u64>> {
        let key = sanitize_audio_ts_name(name)?;
        Ok(self.files.get(&key).map(|file| file.len))
    }
}

#[derive(Clone, Debug)]
struct IndexedFile {
    name: String,
    len: u64,
    data: NodeData,
}

impl IndexedFile {
    fn to_public_info(&self) -> UdfAudioTsFileInfo {
        match &self.data {
            NodeData::Extents(extents) => UdfAudioTsFileInfo {
                name: self.name.clone(),
                len: self.len,
                storage: UdfFileStorageKind::Extents,
                extents: extents
                    .iter()
                    .map(|extent| UdfFileExtent {
                        offset: extent.offset,
                        len: extent.len,
                    })
                    .collect(),
            },
            NodeData::Inline(data) => UdfAudioTsFileInfo {
                name: self.name.clone(),
                len: self.len,
                storage: UdfFileStorageKind::Inline,
                extents: vec![UdfFileExtent {
                    offset: 0,
                    len: data.len() as u64,
                }],
            },
        }
    }
}

#[derive(Clone, Debug)]
enum NodeData {
    Extents(Vec<IsoExtent>),
    Inline(Arc<Vec<u8>>),
}

#[derive(Clone, Debug)]
struct IsoExtent {
    offset: u64,
    len: u64,
}

struct IsoUdfFile {
    pos: u64,
    len: u64,
    storage: IsoFileStorage,
}

enum IsoFileStorage {
    Extents { file: File, extents: Vec<IsoExtent> },
    Inline { data: Arc<Vec<u8>> },
}

impl IsoUdfFile {
    fn open(iso_path: &Path, indexed: &IndexedFile) -> Result<Self> {
        let storage = match &indexed.data {
            NodeData::Inline(data) => IsoFileStorage::Inline { data: Arc::clone(data) },
            NodeData::Extents(extents) => IsoFileStorage::Extents {
                file: File::open(iso_path).map_err(|source| DvdaError::io(iso_path.display().to_string(), source))?,
                extents: extents.clone(),
            },
        };

        Ok(Self {
            pos: 0,
            len: indexed.len,
            storage,
        })
    }
}

impl DvdaFile for IsoUdfFile {
    fn len(&self) -> u64 {
        self.len
    }
}

impl Read for IsoUdfFile {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || self.pos >= self.len {
            return Ok(0);
        }

        let wanted = (self.len - self.pos).min(buf.len() as u64) as usize;
        let read = match &mut self.storage {
            IsoFileStorage::Inline { data } => {
                let start = self.pos as usize;
                let available = data.len().saturating_sub(start);
                let count = wanted.min(available);
                if count == 0 && wanted > 0 {
                    return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "UDF inline file ended early"));
                }
                buf[..count].copy_from_slice(&data[start..start + count]);
                count
            }
            IsoFileStorage::Extents { file, extents } => read_from_extents(file, extents, self.pos, &mut buf[..wanted])?,
        };
        self.pos = self.pos.saturating_add(read as u64);
        Ok(read)
    }
}

impl Seek for IsoUdfFile {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let next = match pos {
            SeekFrom::Start(value) => value as i128,
            SeekFrom::End(value) => self.len as i128 + value as i128,
            SeekFrom::Current(value) => self.pos as i128 + value as i128,
        };
        if next < 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "negative seek in UDF file"));
        }
        self.pos = next as u64;
        Ok(self.pos)
    }
}

fn read_from_extents(file: &mut File, extents: &[IsoExtent], mut pos: u64, mut out: &mut [u8]) -> io::Result<usize> {
    let mut total = 0usize;
    for extent in extents {
        if pos >= extent.len {
            pos -= extent.len;
            continue;
        }

        let in_extent = pos;
        let count = (extent.len - in_extent).min(out.len() as u64) as usize;
        file.seek(SeekFrom::Start(extent.offset + in_extent))?;
        file.read_exact(&mut out[..count])?;
        total += count;
        out = &mut out[count..];
        pos = 0;
        if out.is_empty() {
            return Ok(total);
        }
    }

    if total == 0 {
        Ok(0)
    } else {
        Ok(total)
    }
}

#[derive(Clone, Debug)]
struct LongAd {
    len: u32,
    lba: u32,
    partition_ref: u16,
}

#[derive(Clone, Debug)]
struct Partition {
    number: u16,
    start_sector: u32,
    sector_count: u32,
}

#[derive(Clone, Debug)]
struct LogicalVolume {
    block_size: u32,
    file_set: LongAd,
    partition_maps: Vec<u16>,
}

#[derive(Clone, Debug)]
struct UdfNode {
    file_type: u8,
    len: u64,
    data: NodeData,
}

#[derive(Clone, Debug)]
struct DirectoryEntry {
    name: String,
    file_characteristics: u8,
    icb: LongAd,
}

struct UdfReader {
    iso_path: PathBuf,
    file: File,
    file_len: u64,
    logical: LogicalVolume,
    partitions: Vec<Partition>,
}

impl UdfReader {
    fn open(path: &Path) -> Result<Self> {
        let mut file = File::open(path).map_err(|source| DvdaError::io(path.display().to_string(), source))?;
        let file_len = file.metadata().map_err(|source| DvdaError::io(path.display().to_string(), source))?.len();
        let anchor = find_anchor(&mut file, file_len, path)?;
        let (logical, partitions) = read_volume_descriptor_sequence(&mut file, path, &anchor)?;

        Ok(Self {
            iso_path: path.to_path_buf(),
            file,
            file_len,
            logical,
            partitions,
        })
    }

    fn index_audio_ts_files(&mut self) -> Result<BTreeMap<String, IndexedFile>> {
        let file_set_ad = self.logical.file_set.clone();
        let fsd = self.read_file_set_descriptor(&file_set_ad)?;
        let root = self.read_node(&fsd.root_icb)?;
        let root_entries = self.read_directory(&root)?;

        let Some(audio_ts) = root_entries
            .iter()
            .find(|entry| entry.name.eq_ignore_ascii_case("AUDIO_TS") && entry.file_characteristics & FID_PARENT == 0)
        else {
            return Err(DvdaError::MissingFile {
                candidates: vec![format!("{}:AUDIO_TS", self.iso_path.display())],
            });
        };

        let audio_ts_node = self.read_node(&audio_ts.icb)?;
        let entries = self.read_directory(&audio_ts_node)?;
        let mut out = BTreeMap::new();
        for entry in entries {
            if entry.file_characteristics & (FID_PARENT | FID_DELETED) != 0 || entry.name.is_empty() {
                continue;
            }
            let node = self.read_node(&entry.icb)?;
            let key = entry.name.to_ascii_uppercase();
            out.insert(
                key,
                IndexedFile {
                    name: entry.name,
                    len: node.len,
                    data: node.data,
                },
            );
        }
        Ok(out)
    }

    fn read_file_set_descriptor(&mut self, ad: &LongAd) -> Result<FileSetDescriptor> {
        let bytes = self.read_long_ad_bytes(ad, DVD_SECTOR_SIZE as usize)?;
        let tag = tag_identifier(&bytes)?;
        if tag != TAG_FILE_SET_DESCRIPTOR {
            return Err(DvdaError::Iso {
                message: format!("expected UDF File Set Descriptor, got tag {tag}"),
            });
        }
        Ok(FileSetDescriptor {
            root_icb: parse_long_ad_at(&bytes, 400, "File Set Descriptor root ICB")?,
        })
    }

    fn read_node(&mut self, ad: &LongAd) -> Result<UdfNode> {
        let len = ad_extent_len(ad.len).max(DVD_SECTOR_SIZE as u32) as usize;
        let max_len = len.min(MAX_FILE_ENTRY_BYTES as usize);
        let bytes = self.read_long_ad_bytes(ad, max_len)?;
        let tag = tag_identifier(&bytes)?;
        match tag {
            TAG_FILE_ENTRY => parse_file_entry(&bytes, 56, 168, 172, 176, self),
            TAG_EXTENDED_FILE_ENTRY => parse_file_entry(&bytes, 56, 208, 212, 216, self),
            _ => Err(DvdaError::Iso {
                message: format!("expected UDF File Entry, got tag {tag}"),
            }),
        }
    }

    fn read_directory(&mut self, node: &UdfNode) -> Result<Vec<DirectoryEntry>> {
        if node.len > MAX_DIRECTORY_BYTES {
            return Err(DvdaError::Iso {
                message: format!("UDF directory is too large for Phase 2 indexing: {} bytes", node.len),
            });
        }
        let bytes = self.read_node_bytes(node)?;
        parse_directory_entries(&bytes)
    }

    fn read_node_bytes(&mut self, node: &UdfNode) -> Result<Vec<u8>> {
        match &node.data {
            NodeData::Inline(data) => Ok(data[..data.len().min(node.len as usize)].to_vec()),
            NodeData::Extents(extents) => self.read_extents_bytes(extents, node.len),
        }
    }

    fn read_extents_bytes(&mut self, extents: &[IsoExtent], len: u64) -> Result<Vec<u8>> {
        let wanted = usize::try_from(len).map_err(|_| DvdaError::Iso {
            message: format!("UDF extent length does not fit usize: {len}"),
        })?;
        let mut out = vec![0_u8; wanted];
        let read = read_from_extents(&mut self.file, extents, 0, &mut out)
            .map_err(|source| DvdaError::io(self.iso_path.display().to_string(), source))?;
        if read < wanted {
            return Err(DvdaError::ShortRead {
                context: "UDF extents".to_string(),
                needed: wanted,
                available: read,
            });
        }
        Ok(out)
    }

    fn read_long_ad_bytes(&mut self, ad: &LongAd, min_len: usize) -> Result<Vec<u8>> {
        let offset = self.long_ad_offset(ad)?;
        let requested = ad_extent_len(ad.len) as usize;
        let len = requested.max(min_len);
        read_at(&mut self.file, &self.iso_path, offset, len)
    }

    fn long_ad_offset(&self, ad: &LongAd) -> Result<u64> {
        let partition = self.partition_for_ref(ad.partition_ref)?;
        let rel = u64::from(ad.lba)
            .checked_mul(u64::from(self.logical.block_size))
            .ok_or_else(|| DvdaError::Iso { message: "UDF logical block offset overflow".to_string() })?;
        let base = u64::from(partition.start_sector)
            .checked_mul(DVD_SECTOR_SIZE)
            .ok_or_else(|| DvdaError::Iso { message: "UDF partition byte offset overflow".to_string() })?;
        let offset = base
            .checked_add(rel)
            .ok_or_else(|| DvdaError::Iso { message: "UDF absolute byte offset overflow".to_string() })?;
        if offset > self.file_len {
            return Err(DvdaError::Iso {
                message: format!("UDF long_ad points past end of ISO at byte {offset}"),
            });
        }
        Ok(offset)
    }

    fn extent_from_ad(&self, raw_len: u32, lba: u32, partition_ref: u16) -> Result<Option<IsoExtent>> {
        let len = u64::from(ad_extent_len(raw_len));
        if len == 0 {
            return Ok(None);
        }
        let extent_kind = raw_len >> 30;
        if extent_kind == 3 {
            return Err(DvdaError::Unsupported {
                feature: "UDF continuation allocation descriptors".to_string(),
            });
        }
        if extent_kind != 0 {
            return Ok(None);
        }
        let offset = self.long_ad_offset(&LongAd { len: raw_len, lba, partition_ref })?;
        Ok(Some(IsoExtent { offset, len }))
    }

    fn partition_for_ref(&self, partition_ref: u16) -> Result<&Partition> {
        let partition_number = self
            .logical
            .partition_maps
            .get(partition_ref as usize)
            .copied()
            .unwrap_or(partition_ref);
        self.partitions
            .iter()
            .find(|partition| partition.number == partition_number)
            .ok_or_else(|| DvdaError::Iso {
                message: format!("UDF partition reference {partition_ref} maps to missing partition {partition_number}"),
            })
    }
}

struct Anchor {
    main_sequence_location: u32,
    main_sequence_length: u32,
}

struct FileSetDescriptor {
    root_icb: LongAd,
}

fn find_anchor(file: &mut File, file_len: u64, path: &Path) -> Result<Anchor> {
    let sectors = file_len / DVD_SECTOR_SIZE;
    let mut candidates = Vec::new();
    candidates.push(256_u64);
    if sectors > 0 {
        candidates.push(sectors - 1);
    }
    if sectors > 257 {
        candidates.push(sectors - 257);
    }

    candidates.sort_unstable();
    candidates.dedup();

    for sector in candidates {
        let Ok(bytes) = read_sector(file, path, sector) else {
            continue;
        };
        if tag_identifier(&bytes).ok() == Some(TAG_ANCHOR_VOLUME_DESCRIPTOR_POINTER) {
            return Ok(Anchor {
                main_sequence_length: read_u32(&bytes, 16, "AVDP main sequence length")?,
                main_sequence_location: read_u32(&bytes, 20, "AVDP main sequence location")?,
            });
        }
    }

    Err(DvdaError::Iso {
        message: "could not find UDF Anchor Volume Descriptor Pointer".to_string(),
    })
}

fn read_volume_descriptor_sequence(file: &mut File, path: &Path, anchor: &Anchor) -> Result<(LogicalVolume, Vec<Partition>)> {
    let sectors = ceil_div_u32(anchor.main_sequence_length, DVD_SECTOR_SIZE as u32)
        .min(MAX_DESCRIPTOR_SEQUENCE_SECTORS);
    let mut logical = None;
    let mut partitions = Vec::new();

    for i in 0..sectors {
        let sector = u64::from(anchor.main_sequence_location) + u64::from(i);
        let bytes = read_sector(file, path, sector)?;
        let tag = match tag_identifier(&bytes) {
            Ok(tag) => tag,
            Err(_) => continue,
        };
        match tag {
            TAG_LOGICAL_VOLUME_DESCRIPTOR => logical = Some(parse_logical_volume_descriptor(&bytes)?),
            TAG_PARTITION_DESCRIPTOR => partitions.push(parse_partition_descriptor(&bytes)?),
            TAG_TERMINATING_DESCRIPTOR => break,
            _ => {}
        }
    }

    let logical = logical.ok_or_else(|| DvdaError::Iso {
        message: "UDF Logical Volume Descriptor not found".to_string(),
    })?;
    if partitions.is_empty() {
        return Err(DvdaError::Iso {
            message: "UDF Partition Descriptor not found".to_string(),
        });
    }
    Ok((logical, partitions))
}

fn parse_logical_volume_descriptor(bytes: &[u8]) -> Result<LogicalVolume> {
    let block_size = read_u32(bytes, 212, "LVD logical block size")?;
    if block_size == 0 || u64::from(block_size) % DVD_SECTOR_SIZE != 0 {
        return Err(DvdaError::Unsupported {
            feature: format!("UDF logical block size {block_size}"),
        });
    }
    let file_set = parse_long_ad_at(bytes, 248, "LVD file set descriptor extent")?;
    let map_len = read_u32(bytes, 264, "LVD partition map length")? as usize;
    let maps_start = 440usize;
    let maps_end = maps_start.checked_add(map_len).ok_or_else(|| DvdaError::Iso {
        message: "UDF partition map length overflow".to_string(),
    })?;
    if maps_end > bytes.len() {
        return Err(DvdaError::ShortRead {
            context: "LVD partition maps".to_string(),
            needed: maps_end,
            available: bytes.len(),
        });
    }
    let partition_maps = parse_partition_maps(&bytes[maps_start..maps_end])?;

    Ok(LogicalVolume {
        block_size,
        file_set,
        partition_maps,
    })
}

fn parse_partition_descriptor(bytes: &[u8]) -> Result<Partition> {
    Ok(Partition {
        number: read_u16(bytes, 22, "PD partition number")?,
        start_sector: read_u32(bytes, 188, "PD starting location")?,
        sector_count: read_u32(bytes, 192, "PD partition length")?,
    })
}

fn parse_partition_maps(mut bytes: &[u8]) -> Result<Vec<u16>> {
    let mut maps = Vec::new();
    while !bytes.is_empty() {
        if bytes.len() < 2 {
            return Err(DvdaError::ShortRead {
                context: "UDF partition map".to_string(),
                needed: 2,
                available: bytes.len(),
            });
        }
        let map_type = bytes[0];
        let map_len = bytes[1] as usize;
        if map_len == 0 || map_len > bytes.len() {
            return Err(DvdaError::Iso {
                message: format!("invalid UDF partition map length {map_len}"),
            });
        }
        let map = &bytes[..map_len];
        match map_type {
            1 if map_len >= 6 => maps.push(read_u16(map, 4, "type 1 partition map number")?),
            // UDF type-2 maps used for sparable/metadata partitions carry the
            // physical partition number at byte 38 in the common UDF layout.
            2 if map_len >= 40 => maps.push(read_u16(map, 38, "type 2 partition map number")?),
            _ => {}
        }
        bytes = &bytes[map_len..];
    }
    Ok(maps)
}

fn parse_file_entry(
    bytes: &[u8],
    info_len_offset: usize,
    extended_attr_len_offset: usize,
    allocation_len_offset: usize,
    data_base: usize,
    reader: &UdfReader,
) -> Result<UdfNode> {
    if bytes.len() < data_base {
        return Err(DvdaError::ShortRead {
            context: "UDF File Entry".to_string(),
            needed: data_base,
            available: bytes.len(),
        });
    }

    let file_type = *bytes.get(27).ok_or_else(|| DvdaError::ShortRead {
        context: "UDF ICB tag".to_string(),
        needed: 28,
        available: bytes.len(),
    })?;
    let flags = read_u16(bytes, 34, "UDF ICB flags")?;
    let allocation_type = flags & 0x0007;
    let info_len = read_u64(bytes, info_len_offset, "UDF information length")?;
    let extended_attr_len = read_u32(bytes, extended_attr_len_offset, "UDF extended attribute length")? as usize;
    let allocation_len = read_u32(bytes, allocation_len_offset, "UDF allocation descriptor length")? as usize;
    let allocation_start = data_base.checked_add(extended_attr_len).ok_or_else(|| DvdaError::Iso {
        message: "UDF allocation descriptor offset overflow".to_string(),
    })?;
    let allocation_end = allocation_start.checked_add(allocation_len).ok_or_else(|| DvdaError::Iso {
        message: "UDF allocation descriptor length overflow".to_string(),
    })?;
    if allocation_end > bytes.len() {
        return Err(DvdaError::ShortRead {
            context: "UDF allocation descriptors".to_string(),
            needed: allocation_end,
            available: bytes.len(),
        });
    }
    let allocation_bytes = &bytes[allocation_start..allocation_end];

    let data = match allocation_type {
        0 => NodeData::Extents(truncate_extents(parse_short_ads(allocation_bytes, reader)?, info_len)),
        1 => NodeData::Extents(truncate_extents(parse_long_ads(allocation_bytes, reader)?, info_len)),
        2 => NodeData::Extents(truncate_extents(parse_extended_ads(allocation_bytes, reader)?, info_len)),
        3 => {
            let wanted = (info_len as usize).min(allocation_bytes.len());
            NodeData::Inline(Arc::new(allocation_bytes[..wanted].to_vec()))
        }
        _ => {
            return Err(DvdaError::Unsupported {
                feature: format!("UDF allocation descriptor type {allocation_type}"),
            });
        }
    };

    Ok(UdfNode { file_type, len: info_len, data })
}

fn parse_short_ads(bytes: &[u8], reader: &UdfReader) -> Result<Vec<IsoExtent>> {
    let mut out = Vec::new();
    for chunk in bytes.chunks_exact(8) {
        let raw_len = read_u32(chunk, 0, "short_ad length")?;
        let lba = read_u32(chunk, 4, "short_ad location")?;
        if let Some(extent) = reader.extent_from_ad(raw_len, lba, 0)? {
            out.push(extent);
        }
    }
    Ok(out)
}

fn parse_long_ads(bytes: &[u8], reader: &UdfReader) -> Result<Vec<IsoExtent>> {
    let mut out = Vec::new();
    for chunk in bytes.chunks_exact(16) {
        let raw_len = read_u32(chunk, 0, "long_ad length")?;
        let lba = read_u32(chunk, 4, "long_ad location")?;
        let partition_ref = read_u16(chunk, 8, "long_ad partition reference")?;
        if let Some(extent) = reader.extent_from_ad(raw_len, lba, partition_ref)? {
            out.push(extent);
        }
    }
    Ok(out)
}

fn parse_extended_ads(bytes: &[u8], reader: &UdfReader) -> Result<Vec<IsoExtent>> {
    let mut out = Vec::new();
    for chunk in bytes.chunks_exact(20) {
        let raw_len = read_u32(chunk, 0, "ext_ad length")?;
        let lba = read_u32(chunk, 12, "ext_ad location")?;
        let partition_ref = read_u16(chunk, 16, "ext_ad partition reference")?;
        if let Some(extent) = reader.extent_from_ad(raw_len, lba, partition_ref)? {
            out.push(extent);
        }
    }
    Ok(out)
}

fn truncate_extents(mut extents: Vec<IsoExtent>, mut len: u64) -> Vec<IsoExtent> {
    let mut out = Vec::with_capacity(extents.len());
    for mut extent in extents.drain(..) {
        if len == 0 {
            break;
        }
        if extent.len > len {
            extent.len = len;
        }
        len -= extent.len;
        out.push(extent);
    }
    out
}

fn parse_directory_entries(bytes: &[u8]) -> Result<Vec<DirectoryEntry>> {
    let mut out = Vec::new();
    let mut offset = 0usize;
    while offset + 38 <= bytes.len() {
        let tag = match tag_identifier(&bytes[offset..]) {
            Ok(tag) => tag,
            Err(_) => break,
        };
        if tag != TAG_FILE_IDENTIFIER_DESCRIPTOR {
            break;
        }

        let file_characteristics = bytes[offset + 18];
        let identifier_len = bytes[offset + 19] as usize;
        let icb = parse_long_ad_at(bytes, offset + 20, "FID ICB")?;
        let implementation_use_len = read_u16(bytes, offset + 36, "FID implementation use length")? as usize;
        let identifier_start = offset
            .checked_add(38)
            .and_then(|value| value.checked_add(implementation_use_len))
            .ok_or_else(|| DvdaError::Iso { message: "FID identifier offset overflow".to_string() })?;
        let identifier_end = identifier_start.checked_add(identifier_len).ok_or_else(|| DvdaError::Iso {
            message: "FID identifier length overflow".to_string(),
        })?;
        if identifier_end > bytes.len() {
            return Err(DvdaError::ShortRead {
                context: "FID identifier".to_string(),
                needed: identifier_end,
                available: bytes.len(),
            });
        }

        let name = decode_osta_compressed_unicode(&bytes[identifier_start..identifier_end])?;
        out.push(DirectoryEntry { name, file_characteristics, icb });

        let entry_len = align4(38 + implementation_use_len + identifier_len);
        if entry_len == 0 {
            break;
        }
        offset = offset.saturating_add(entry_len);
    }
    Ok(out)
}

fn decode_osta_compressed_unicode(bytes: &[u8]) -> Result<String> {
    if bytes.is_empty() {
        return Ok(String::new());
    }
    match bytes[0] {
        8 => Ok(String::from_utf8_lossy(&bytes[1..]).into_owned()),
        16 => {
            if (bytes.len() - 1) % 2 != 0 {
                return Err(DvdaError::Iso {
                    message: "odd-length UDF CS0 UTF-16 identifier".to_string(),
                });
            }
            let mut units = Vec::with_capacity((bytes.len() - 1) / 2);
            for pair in bytes[1..].chunks_exact(2) {
                units.push(u16::from_be_bytes([pair[0], pair[1]]));
            }
            String::from_utf16(&units).map_err(|err| DvdaError::Iso {
                message: format!("invalid UDF CS0 UTF-16 identifier: {err}"),
            })
        }
        _ => Ok(String::from_utf8_lossy(bytes).into_owned()),
    }
}

fn parse_long_ad_at(bytes: &[u8], offset: usize, context: &'static str) -> Result<LongAd> {
    let end = offset.checked_add(16).ok_or_else(|| DvdaError::Iso {
        message: format!("{context} offset overflow"),
    })?;
    if end > bytes.len() {
        return Err(DvdaError::ShortRead {
            context: context.to_string(),
            needed: end,
            available: bytes.len(),
        });
    }
    Ok(LongAd {
        len: read_u32(bytes, offset, context)?,
        lba: read_u32(bytes, offset + 4, context)?,
        partition_ref: read_u16(bytes, offset + 8, context)?,
    })
}

fn tag_identifier(bytes: &[u8]) -> Result<u16> {
    if bytes.len() < 16 {
        return Err(DvdaError::ShortRead {
            context: "UDF descriptor tag".to_string(),
            needed: 16,
            available: bytes.len(),
        });
    }
    let checksum = bytes[..16]
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != 4)
        .fold(0_u8, |sum, (_, value)| sum.wrapping_add(*value));
    if checksum != bytes[4] {
        return Err(DvdaError::Iso {
            message: "UDF descriptor tag checksum mismatch".to_string(),
        });
    }
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn read_sector(file: &mut File, path: &Path, sector: u64) -> Result<Vec<u8>> {
    let offset = sector.checked_mul(DVD_SECTOR_SIZE).ok_or_else(|| DvdaError::Iso {
        message: "ISO sector offset overflow".to_string(),
    })?;
    read_at(file, path, offset, DVD_SECTOR_SIZE_USIZE)
}

fn read_at(file: &mut File, path: &Path, offset: u64, len: usize) -> Result<Vec<u8>> {
    let mut out = vec![0_u8; len];
    file.seek(SeekFrom::Start(offset)).map_err(|source| DvdaError::io(path.display().to_string(), source))?;
    file.read_exact(&mut out).map_err(|source| DvdaError::io(path.display().to_string(), source))?;
    Ok(out)
}

fn read_u16(bytes: &[u8], offset: usize, context: &'static str) -> Result<u16> {
    let end = offset + 2;
    if end > bytes.len() {
        return Err(DvdaError::ShortRead { context: context.to_string(), needed: end, available: bytes.len() });
    }
    Ok(u16::from_le_bytes([bytes[offset], bytes[offset + 1]]))
}

fn read_u32(bytes: &[u8], offset: usize, context: &'static str) -> Result<u32> {
    let end = offset + 4;
    if end > bytes.len() {
        return Err(DvdaError::ShortRead { context: context.to_string(), needed: end, available: bytes.len() });
    }
    Ok(u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ]))
}

fn read_u64(bytes: &[u8], offset: usize, context: &'static str) -> Result<u64> {
    let end = offset + 8;
    if end > bytes.len() {
        return Err(DvdaError::ShortRead { context: context.to_string(), needed: end, available: bytes.len() });
    }
    Ok(u64::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
        bytes[offset + 4],
        bytes[offset + 5],
        bytes[offset + 6],
        bytes[offset + 7],
    ]))
}

fn ad_extent_len(raw_len: u32) -> u32 {
    raw_len & 0x3fff_ffff
}

fn align4(value: usize) -> usize {
    (value + 3) & !3
}

fn ceil_div_u32(n: u32, d: u32) -> u32 {
    if n == 0 { 0 } else { 1 + (n - 1) / d }
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
    Ok(name.to_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_osta_cs0_8_bit_names() {
        assert_eq!(decode_osta_compressed_unicode(b"\x08AUDIO_TS.IFO").unwrap(), "AUDIO_TS.IFO");
    }

    #[test]
    fn decodes_osta_cs0_16_bit_names() {
        let name = [0x10, 0x00, b'A', 0x00, b'B'];
        assert_eq!(decode_osta_compressed_unicode(&name).unwrap(), "AB");
    }

    #[test]
    fn masks_extent_type_bits_from_allocation_length() {
        assert_eq!(ad_extent_len(0x4000_0800), 0x800);
        assert_eq!(ad_extent_len(0x8000_1234), 0x1234);
    }
}
