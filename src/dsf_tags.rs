//! DSF ID3v2 metadata support.
//!
//! All direct `id3` crate calls are isolated in `backend`. The crate supplies
//! generic ID3 stream parsing and serialization but no DSF container adapter,
//! so this module validates the DSF metadata pointer and performs same-directory
//! atomic container replacement itself.

use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DsfTagSnapshot {
    /// Canonical editor display key -> ordered, distinct values.
    pub fields: BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DsfReadOutcome {
    pub snapshot: DsfTagSnapshot,
    pub warnings: Vec<String>,
}

impl DsfTagSnapshot {
    pub fn first(&self, key: &str) -> Option<&str> {
        self.fields
            .get(key)
            .and_then(|values| values.iter().find(|value| !value.trim().is_empty()))
            .map(String::as_str)
    }

    /// Returns the scalar editor representation for a potentially multi-value
    /// key. This string is presentation-only unless that exact row is edited;
    /// unrelated writes do not emit a change for the key and therefore preserve
    /// every original ID3 frame independently.
    pub fn joined(&self, key: &str) -> Option<String> {
        let mut values = self
            .fields
            .get(key)?
            .iter()
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>();
        values.dedup();
        (!values.is_empty()).then(|| values.join("; "))
    }

    pub fn parsed_u32(&self, key: &str) -> Option<u32> {
        self.first(key)?.trim().parse().ok()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DsfTagChange {
    pub canonical_key: String,
    pub value: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DsfWriteProgressPhase {
    Preparing,
    Journaling,
    WritingTail,
    CopyingPrefix,
    Publishing,
    Recovering,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DsfWriteProgress {
    pub phase: DsfWriteProgressPhase,
    pub bytes_done: u64,
    pub bytes_total: u64,
}

/// Explicit resolution for a legacy full-file rollback marker. These markers
/// predate generation-bound journals, so tonepoet never guesses which copy is
/// authoritative when the marker and current target differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DsfLegacyBackupResolution {
    RestoreBackup,
    KeepCurrent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DsfLegacyBackupInspection {
    pub target: PathBuf,
    pub marker: PathBuf,
    pub target_bytes: u64,
    pub marker_bytes: u64,
    pub byte_identical: bool,
}

const COPY_CHUNK_BYTES: usize = 1024 * 1024;
const MAX_IN_PLACE_TAIL_BYTES: u64 = 64 * 1024 * 1024;
const TAIL_JOURNAL_MAGIC: &[u8; 8] = b"TPDSFJ02";
const TAIL_JOURNAL_PREPARED: u8 = 0;
const TAIL_JOURNAL_COMMITTED: u8 = 1;
const TAIL_JOURNAL_IDENTITY_LEN: usize = 32;
const TAIL_JOURNAL_HEADER_LEN: usize = 8 + 1 + 8 + 8 + TAIL_JOURNAL_IDENTITY_LEN;
const RECOVERY_IDENTITY_SAMPLE_BYTES: usize = 64 * 1024;
const TAIL_JOURNAL_STATE_OFFSET: u64 = 8;

#[cfg(test)]
thread_local! {
    static TEST_FAIL_TAIL_JOURNAL_COMMIT_SYNC: std::cell::Cell<bool> = const {
        std::cell::Cell::new(false)
    };
}

pub fn is_dsf(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("dsf"))
}

pub fn to_track_metadata(snapshot: &DsfTagSnapshot) -> crate::convert::pipeline::TrackMetadata {
    let mut extra = BTreeMap::new();
    for (key, values) in &snapshot.fields {
        if let Some(value) = values.iter().find(|value| !value.trim().is_empty()) {
            extra.insert(key.to_ascii_lowercase(), value.clone());
        }
    }
    crate::convert::pipeline::TrackMetadata {
        title: snapshot.first("TITLE").map(ToOwned::to_owned),
        artist: snapshot.first("ARTIST").map(ToOwned::to_owned),
        album_artist: snapshot.first("ALBUMARTIST").map(ToOwned::to_owned),
        composer: snapshot.first("COMPOSER").map(ToOwned::to_owned),
        performer: snapshot.first("PERFORMER").map(ToOwned::to_owned),
        genre: snapshot.first("GENRE").map(ToOwned::to_owned),
        date: snapshot.first("DATE").map(ToOwned::to_owned),
        track_number: snapshot.parsed_u32("TRACKNUMBER"),
        disc_number: snapshot.parsed_u32("DISCNUMBER"),
        isrc: snapshot.first("ISRC").map(ToOwned::to_owned),
        publisher: snapshot.first("LABEL").map(ToOwned::to_owned),
        copyright: snapshot.first("COPYRIGHT").map(ToOwned::to_owned),
        comment: snapshot.first("COMMENT").map(ToOwned::to_owned),
        extra,
        ..crate::convert::pipeline::TrackMetadata::default()
    }
}

pub fn read(path: &Path) -> Result<DsfTagSnapshot, String> {
    let outcome = read_with_warnings(path)?;
    for warning in &outcome.warnings {
        log::warn!("DSF metadata read degraded for '{}': {warning}", path.display());
    }
    Ok(outcome.snapshot)
}

/// Best-effort DSF metadata read. Container quirks that do not prevent safe
/// audio conversion are reported as warnings and yield either the readable ID3
/// snapshot or an empty snapshot. Mutation continues to use the strict
/// `inspect_dsf_metadata_location` gate.
pub fn read_with_warnings(path: &Path) -> Result<DsfReadOutcome, String> {
    if !is_dsf(path) {
        return Err(format!("'{}' is not a DSF file", path.display()));
    }
    let (location, mut warnings) = inspect_dsf_metadata_location_for_read(path)?;
    let snapshot = match location {
        Some(location) => match backend::read_at_location(path, location) {
            Ok(snapshot) => canonicalize_snapshot(snapshot),
            Err(error) => {
                warnings.push(format!(
                    "ID3 metadata could not be decoded and was ignored: {error}"
                ));
                DsfTagSnapshot::default()
            }
        },
        None => DsfTagSnapshot::default(),
    };
    Ok(DsfReadOutcome { snapshot, warnings })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DsfMetadataLocation {
    Untagged { file_size: u64 },
    Id3 {
        offset: u64,
        tag_end: u64,
        file_size: u64,
    },
}

/// Validate the DSF container facts needed for safe ID3 access and replacement.
/// A nonzero metadata pointer must identify a complete ID3v2 tag. Any bytes
/// following that tag are treated as an ambiguous layout and rejected by the
/// writer rather than silently discarded.
fn inspect_dsf_metadata_location(path: &Path) -> Result<DsfMetadataLocation, String> {
    let mut file = std::fs::File::open(path)
        .map_err(|error| format!("open DSF container '{}': {error}", path.display()))?;
    let actual_size = file
        .metadata()
        .map_err(|error| format!("stat DSF container '{}': {error}", path.display()))?
        .len();
    let mut header = [0u8; 28];
    file.read_exact(&mut header)
        .map_err(|error| format!("read 28-byte DSF header '{}': {error}", path.display()))?;
    if &header[0..4] != b"DSD " {
        return Err(format!(
            "invalid DSF header in '{}': expected DSD chunk marker",
            path.display()
        ));
    }
    let chunk_size = u64::from_le_bytes([
        header[4], header[5], header[6], header[7], header[8], header[9], header[10], header[11],
    ]);
    if chunk_size != 28 {
        return Err(format!(
            "invalid DSF header in '{}': DSD chunk size is {chunk_size}, expected 28",
            path.display()
        ));
    }
    let declared_size = u64::from_le_bytes([
        header[12], header[13], header[14], header[15], header[16], header[17], header[18],
        header[19],
    ]);
    if declared_size != actual_size {
        return Err(format!(
            "invalid DSF header in '{}': declared file size {declared_size} differs from actual size {actual_size}",
            path.display()
        ));
    }
    let metadata_offset = u64::from_le_bytes([
        header[20], header[21], header[22], header[23], header[24], header[25], header[26],
        header[27],
    ]);
    if metadata_offset != 0
        && (metadata_offset < chunk_size || metadata_offset.saturating_add(10) > actual_size)
    {
        return Err(format!(
            "invalid DSF metadata pointer in '{}': offset {metadata_offset} cannot contain an ID3 header within {actual_size} bytes",
            path.display()
        ));
    }
    let audio_boundary = if metadata_offset == 0 {
        actual_size
    } else {
        metadata_offset
    };
    validate_dsf_audio_chunks(&mut file, chunk_size, audio_boundary, path)?;
    if metadata_offset == 0 {
        return Ok(DsfMetadataLocation::Untagged {
            file_size: actual_size,
        });
    }

    file.seek(SeekFrom::Start(metadata_offset)).map_err(|error| {
        format!(
            "seek to DSF metadata offset {metadata_offset} in '{}': {error}",
            path.display()
        )
    })?;
    let mut id3_header = [0u8; 10];
    file.read_exact(&mut id3_header).map_err(|error| {
        format!(
            "read ID3 header at DSF metadata offset {metadata_offset} in '{}': {error}",
            path.display()
        )
    })?;
    if &id3_header[0..3] != b"ID3" {
        return Err(format!(
            "invalid DSF metadata area in '{}': header declares metadata at offset {metadata_offset}, but no ID3 marker is present",
            path.display()
        ));
    }
    if !matches!(id3_header[3], 2 | 3 | 4) {
        return Err(format!(
            "unsupported ID3 major version {} at DSF metadata offset {metadata_offset} in '{}'",
            id3_header[3],
            path.display()
        ));
    }
    if id3_header[6..10].iter().any(|byte| byte & 0x80 != 0) {
        return Err(format!(
            "invalid syncsafe ID3 size at DSF metadata offset {metadata_offset} in '{}'",
            path.display()
        ));
    }
    let payload_size = id3_header[6..10]
        .iter()
        .fold(0u64, |size, byte| (size << 7) | u64::from(*byte));
    let footer_size = if id3_header[3] == 4 && id3_header[5] & 0x10 != 0 {
        10
    } else {
        0
    };
    let tag_end = metadata_offset
        .checked_add(10)
        .and_then(|value| value.checked_add(payload_size))
        .and_then(|value| value.checked_add(footer_size))
        .ok_or_else(|| {
            format!(
                "ID3 size overflows the DSF container address space at offset {metadata_offset} in '{}'",
                path.display()
            )
        })?;
    if tag_end > actual_size {
        return Err(format!(
            "truncated ID3 tag in '{}': tag ending at {tag_end} exceeds DSF file size {actual_size}",
            path.display()
        ));
    }
    Ok(DsfMetadataLocation::Id3 {
        offset: metadata_offset,
        tag_end,
        file_size: actual_size,
    })
}

fn inspect_dsf_metadata_location_for_read(
    path: &Path,
) -> Result<(Option<DsfMetadataLocation>, Vec<String>), String> {
    let mut warnings = Vec::new();
    let mut file = std::fs::File::open(path)
        .map_err(|error| format!("open DSF container '{}': {error}", path.display()))?;
    let actual_size = file
        .metadata()
        .map_err(|error| format!("stat DSF container '{}': {error}", path.display()))?
        .len();
    let mut header = [0u8; 28];
    file.read_exact(&mut header)
        .map_err(|error| format!("read 28-byte DSF header '{}': {error}", path.display()))?;
    if &header[0..4] != b"DSD " {
        return Err(format!(
            "invalid DSF header in '{}': expected DSD chunk marker",
            path.display()
        ));
    }
    let chunk_size = u64::from_le_bytes(header[4..12].try_into().expect("8-byte DSF chunk size"));
    if chunk_size != 28 {
        return Err(format!(
            "invalid DSF header in '{}': DSD chunk size is {chunk_size}, expected 28",
            path.display()
        ));
    }
    let declared_size =
        u64::from_le_bytes(header[12..20].try_into().expect("8-byte DSF file size"));
    if declared_size != actual_size {
        warnings.push(format!(
            "declared file size {declared_size} differs from actual size {actual_size}"
        ));
    }
    let metadata_offset =
        u64::from_le_bytes(header[20..28].try_into().expect("8-byte DSF metadata pointer"));
    let pointer_valid = metadata_offset == 0
        || (metadata_offset >= chunk_size && metadata_offset.saturating_add(10) <= actual_size);
    let audio_boundary = if metadata_offset == 0 || !pointer_valid {
        actual_size
    } else {
        metadata_offset
    };
    if let Err(error) = validate_dsf_audio_chunks(&mut file, chunk_size, audio_boundary, path) {
        warnings.push(format!("noncanonical DSF audio-chunk layout: {error}"));
    }
    if metadata_offset == 0 {
        return Ok((Some(DsfMetadataLocation::Untagged { file_size: actual_size }), warnings));
    }
    if !pointer_valid {
        warnings.push(format!(
            "metadata pointer {metadata_offset} cannot contain an ID3 header within {actual_size} bytes; metadata was ignored"
        ));
        return Ok((None, warnings));
    }

    file.seek(SeekFrom::Start(metadata_offset)).map_err(|error| {
        format!(
            "seek to DSF metadata offset {metadata_offset} in '{}': {error}",
            path.display()
        )
    })?;
    let mut id3_header = [0u8; 10];
    if let Err(error) = file.read_exact(&mut id3_header) {
        warnings.push(format!(
            "ID3 header at metadata offset {metadata_offset} could not be read and was ignored: {error}"
        ));
        return Ok((None, warnings));
    }
    if &id3_header[0..3] != b"ID3" {
        warnings.push(format!(
            "metadata pointer {metadata_offset} does not identify an ID3 marker; metadata was ignored"
        ));
        return Ok((None, warnings));
    }
    if !matches!(id3_header[3], 2 | 3 | 4) {
        warnings.push(format!(
            "unsupported ID3 major version {} at metadata offset {metadata_offset}; metadata was ignored",
            id3_header[3]
        ));
        return Ok((None, warnings));
    }
    if id3_header[6..10].iter().any(|byte| byte & 0x80 != 0) {
        warnings.push(format!(
            "invalid syncsafe ID3 size at metadata offset {metadata_offset}; metadata was ignored"
        ));
        return Ok((None, warnings));
    }
    let payload_size = id3_header[6..10]
        .iter()
        .fold(0u64, |size, byte| (size << 7) | u64::from(*byte));
    let footer_size = if id3_header[3] == 4 && id3_header[5] & 0x10 != 0 {
        10
    } else {
        0
    };
    let Some(tag_end) = metadata_offset
        .checked_add(10)
        .and_then(|value| value.checked_add(payload_size))
        .and_then(|value| value.checked_add(footer_size))
    else {
        warnings.push("ID3 size overflows the DSF address space; metadata was ignored".to_string());
        return Ok((None, warnings));
    };
    if tag_end > actual_size {
        warnings.push(format!(
            "truncated ID3 tag ends at {tag_end}, beyond actual file size {actual_size}; metadata was ignored"
        ));
        return Ok((None, warnings));
    }
    Ok((
        Some(DsfMetadataLocation::Id3 {
            offset: metadata_offset,
            tag_end,
            file_size: actual_size,
        }),
        warnings,
    ))
}

fn validate_dsf_audio_chunks(
    file: &mut std::fs::File,
    first_chunk_offset: u64,
    audio_boundary: u64,
    path: &Path,
) -> Result<(), String> {
    if audio_boundary < first_chunk_offset {
        return Err(format!(
            "invalid DSF audio boundary in '{}': metadata begins at {audio_boundary}, before the first payload chunk at {first_chunk_offset}",
            path.display()
        ));
    }

    let mut cursor = first_chunk_offset;
    let mut saw_format = false;
    let mut saw_data = false;
    while cursor < audio_boundary {
        let remaining = audio_boundary - cursor;
        if remaining < 12 {
            return Err(format!(
                "invalid DSF chunk layout in '{}': {remaining} byte(s) remain before the metadata boundary at {audio_boundary}, fewer than the 12-byte chunk header",
                path.display()
            ));
        }
        file.seek(SeekFrom::Start(cursor)).map_err(|error| {
            format!(
                "seek to DSF chunk header at offset {cursor} in '{}': {error}",
                path.display()
            )
        })?;
        let mut chunk_header = [0u8; 12];
        file.read_exact(&mut chunk_header).map_err(|error| {
            format!(
                "read DSF chunk header at offset {cursor} in '{}': {error}",
                path.display()
            )
        })?;
        let chunk_size = u64::from_le_bytes([
            chunk_header[4],
            chunk_header[5],
            chunk_header[6],
            chunk_header[7],
            chunk_header[8],
            chunk_header[9],
            chunk_header[10],
            chunk_header[11],
        ]);
        if chunk_size < 12 {
            return Err(format!(
                "invalid DSF chunk size {chunk_size} at offset {cursor} in '{}': every chunk includes its 12-byte header",
                path.display()
            ));
        }
        let chunk_end = cursor.checked_add(chunk_size).ok_or_else(|| {
            format!(
                "DSF chunk size overflows the container address space at offset {cursor} in '{}'",
                path.display()
            )
        })?;
        if chunk_end > audio_boundary {
            return Err(format!(
                "invalid DSF chunk layout in '{}': chunk at offset {cursor} ends at {chunk_end}, beyond the metadata boundary at {audio_boundary}",
                path.display()
            ));
        }

        match &chunk_header[0..4] {
            b"fmt " => {
                if saw_format || saw_data {
                    return Err(format!(
                        "invalid DSF chunk order in '{}': fmt chunk at offset {cursor} is duplicated or follows audio data",
                        path.display()
                    ));
                }
                if chunk_size != 52 {
                    return Err(format!(
                        "invalid DSF fmt chunk size {chunk_size} at offset {cursor} in '{}': expected 52",
                        path.display()
                    ));
                }
                saw_format = true;
            }
            b"data" => {
                if !saw_format || saw_data {
                    return Err(format!(
                        "invalid DSF chunk order in '{}': data chunk at offset {cursor} must follow exactly one fmt chunk",
                        path.display()
                    ));
                }
                saw_data = true;
            }
            marker => {
                let marker = String::from_utf8_lossy(marker);
                return Err(format!(
                    "unsupported DSF chunk '{marker}' at offset {cursor} in '{}'; refusing to infer ownership across an unknown container layout",
                    path.display()
                ));
            }
        }
        cursor = chunk_end;
    }

    if !saw_format || !saw_data {
        return Err(format!(
            "invalid DSF chunk layout in '{}': expected one fmt chunk followed by one data chunk before metadata",
            path.display()
        ));
    }
    Ok(())
}

pub fn write_with_backup(path: &Path, changes: &[DsfTagChange]) -> Result<Option<String>, String> {
    write_with_control(path, changes, &|| false, &|_| {})
}

/// Save DSF metadata with cooperative cancellation and byte-level progress.
///
/// The historical name `write_with_backup` remains as the compatibility
/// wrapper, but ordinary DSF writes no longer allocate a full-file backup.
/// Existing-tail writes use a bounded, durable tail journal; growth/untagged
/// writes use same-directory temp+rename so the original inode remains the
/// rollback authority until publication.
pub fn write_with_control(
    path: &Path,
    changes: &[DsfTagChange],
    is_cancelled: &dyn Fn() -> bool,
    progress: &dyn Fn(DsfWriteProgress),
) -> Result<Option<String>, String> {
    if changes.is_empty() {
        return Ok(None);
    }
    progress(DsfWriteProgress {
        phase: DsfWriteProgressPhase::Preparing,
        bytes_done: 0,
        bytes_total: 0,
    });
    if is_cancelled() {
        return Err("metadata save cancelled before preparing DSF metadata".to_string());
    }
    reject_symlinked_write_path(path)?;
    let (_write_lock, target_path) = acquire_dsf_write_lock(path)?;
    let path = target_path.as_path();
    preflight_dsf_write_artifacts(path)?;
    let resolved = validate_and_resolve_write(path, changes)?;
    let (location, encoded) = backend::prepare(path, &resolved)
        .map_err(|error| format!("failed to save DSF ID3 tags to '{}': {error}", path.display()))?;
    write_prepared(path, location, &encoded, is_cancelled, progress)
}

fn validate_and_resolve_write(
    path: &Path,
    changes: &[DsfTagChange],
) -> Result<Vec<DsfTagChange>, String> {
    if !is_dsf(path) {
        return Err(format!("'{}' is not a DSF file", path.display()));
    }
    reject_symlinked_write_path(path)?;
    resolve_changes(changes)
}

fn reject_symlinked_write_path(path: &Path) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("inspect DSF write path '{}': {error}", path.display()))?;
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "refusing to mutate symlinked DSF path '{}'; edit the resolved target explicitly so write authority remains unambiguous",
            path.display()
        ));
    }
    Ok(())
}

fn acquire_dsf_write_lock(
    path: &Path,
) -> Result<(crate::config::StoreFileLock, PathBuf), String> {
    crate::config::StoreFileLock::acquire_for_path(path).map_err(|error| {
        format!(
            "acquire bounded DSF metadata-write lock for '{}': {error}",
            path.display()
        )
    })
}

fn write_prepared(
    path: &Path,
    location: DsfMetadataLocation,
    encoded_tag: &[u8],
    is_cancelled: &dyn Fn() -> bool,
    progress: &dyn Fn(DsfWriteProgress),
) -> Result<Option<String>, String> {
    if let DsfMetadataLocation::Id3 {
        offset,
        tag_end,
        file_size,
    } = location
    {
        if tag_end != file_size {
            return Err(format!(
                "refusing to replace DSF ID3 metadata in '{}': {} trailing byte(s) follow the declared tag, so their ownership is ambiguous",
                path.display(),
                file_size - tag_end
            ));
        }
        let allocation = file_size - offset;
        if encoded_tag.len() as u64 <= allocation && allocation <= MAX_IN_PLACE_TAIL_BYTES {
            let padded = pad_id3_to_allocation(encoded_tag, allocation)?;
            return write_tail_in_place(path, offset, &padded, is_cancelled, progress);
        }
    }
    rewrite_container(path, location, encoded_tag, is_cancelled, progress)
}

fn pad_id3_to_allocation(encoded: &[u8], allocation: u64) -> Result<Vec<u8>, String> {
    if encoded.len() < 10 || !encoded.starts_with(b"ID3") {
        return Err("ID3 backend produced an invalid tag header".to_string());
    }
    if encoded[5] & 0x10 != 0 {
        return Err("ID3 backend unexpectedly emitted a footer; bounded DSF tail padding cannot preserve it".to_string());
    }
    let allocation = usize::try_from(allocation)
        .map_err(|_| "existing DSF metadata allocation does not fit addressable memory".to_string())?;
    if allocation < encoded.len() || allocation < 10 {
        return Err("existing DSF metadata allocation is smaller than the encoded tag".to_string());
    }
    let payload_size = allocation - 10;
    if payload_size > 0x0fff_ffff {
        return Err(format!(
            "existing DSF metadata allocation of {allocation} bytes exceeds the ID3v2 syncsafe size limit"
        ));
    }
    let mut padded = vec![0u8; allocation];
    padded[..encoded.len()].copy_from_slice(encoded);
    padded[6] = ((payload_size >> 21) & 0x7f) as u8;
    padded[7] = ((payload_size >> 14) & 0x7f) as u8;
    padded[8] = ((payload_size >> 7) & 0x7f) as u8;
    padded[9] = (payload_size & 0x7f) as u8;
    Ok(padded)
}

fn tail_journal_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("audio.dsf");
    path.with_file_name(format!(".{name}.tonepoet-dsf-tail.journal"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TailJournalHeader {
    state: u8,
    offset: u64,
    original_len: u64,
    recovery_identity: [u8; TAIL_JOURNAL_IDENTITY_LEN],
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TailJournalCommitOutcome {
    /// The state byte and journal contents are durably COMMITTED.
    Durable,
    /// The state byte was not changed. The PREPARED journal remains
    /// rollback-authoritative, so immediate rollback is safe.
    StateUnchanged(String),
    /// The state byte was written, but its durability is unknown. The target
    /// tail was already fsynced; retain the journal and let recovery resolve
    /// whichever complete state reached stable storage.
    DurabilityUncertain(String),
}

fn encode_tail_journal_header(header: TailJournalHeader) -> [u8; TAIL_JOURNAL_HEADER_LEN] {
    let mut bytes = [0u8; TAIL_JOURNAL_HEADER_LEN];
    bytes[..8].copy_from_slice(TAIL_JOURNAL_MAGIC);
    bytes[8] = header.state;
    bytes[9..17].copy_from_slice(&header.offset.to_le_bytes());
    bytes[17..25].copy_from_slice(&header.original_len.to_le_bytes());
    bytes[25..].copy_from_slice(&header.recovery_identity);
    bytes
}

#[cfg(test)]
fn encode_tail_journal(
    offset: u64,
    original: &[u8],
    recovery_identity: &[u8; TAIL_JOURNAL_IDENTITY_LEN],
    state: u8,
) -> Vec<u8> {
    let header = encode_tail_journal_header(TailJournalHeader {
        state,
        offset,
        original_len: original.len() as u64,
        recovery_identity: *recovery_identity,
    });
    let mut bytes = Vec::with_capacity(TAIL_JOURNAL_HEADER_LEN + original.len());
    bytes.extend_from_slice(&header);
    bytes.extend_from_slice(original);
    bytes
}

fn read_tail_journal_header(
    journal: &Path,
    file: &mut std::fs::File,
) -> Result<TailJournalHeader, String> {
    let file_len = file
        .metadata()
        .map_err(|error| format!("stat DSF tail journal '{}': {error}", journal.display()))?
        .len();
    if file_len < TAIL_JOURNAL_HEADER_LEN as u64 {
        return Err(format!(
            "DSF tail journal '{}' has an invalid header",
            journal.display()
        ));
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|error| format!("seek DSF tail journal '{}': {error}", journal.display()))?;
    let mut bytes = [0u8; TAIL_JOURNAL_HEADER_LEN];
    file.read_exact(&mut bytes)
        .map_err(|error| format!("read DSF tail journal header '{}': {error}", journal.display()))?;
    if &bytes[..8] != TAIL_JOURNAL_MAGIC {
        return Err(format!(
            "DSF tail journal '{}' has an invalid header",
            journal.display()
        ));
    }
    let state = bytes[8];
    if !matches!(state, TAIL_JOURNAL_PREPARED | TAIL_JOURNAL_COMMITTED) {
        return Err(format!(
            "DSF tail journal '{}' has unknown state {state}",
            journal.display()
        ));
    }
    let offset = u64::from_le_bytes(bytes[9..17].try_into().expect("fixed journal offset slice"));
    let original_len =
        u64::from_le_bytes(bytes[17..25].try_into().expect("fixed journal length slice"));
    let recovery_identity = bytes[25..]
        .try_into()
        .expect("fixed DSF recovery identity slice");
    let expected_len = (TAIL_JOURNAL_HEADER_LEN as u64)
        .checked_add(original_len)
        .ok_or_else(|| format!("DSF tail journal '{}' length overflows", journal.display()))?;
    if file_len != expected_len {
        return Err(format!(
            "DSF tail journal '{}' declares {original_len} original byte(s), but contains {}",
            journal.display(),
            file_len.saturating_sub(TAIL_JOURNAL_HEADER_LEN as u64)
        ));
    }
    Ok(TailJournalHeader {
        state,
        offset,
        original_len,
        recovery_identity,
    })
}

fn open_tail_journal_for_read(journal: &Path) -> Result<Option<std::fs::File>, String> {
    let metadata = match std::fs::symlink_metadata(journal) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "inspect DSF tail journal '{}': {error}",
                journal.display()
            ))
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!(
            "refusing ambiguous DSF tail journal '{}': expected a regular file",
            journal.display()
        ));
    }
    let file = std::fs::File::open(journal)
        .map_err(|error| format!("open DSF tail journal '{}': {error}", journal.display()))?;
    if !file
        .metadata()
        .map_err(|error| format!("inspect opened DSF tail journal '{}': {error}", journal.display()))?
        .is_file()
    {
        return Err(format!(
            "refusing ambiguous DSF tail journal '{}': opened object is not a regular file",
            journal.display()
        ));
    }
    Ok(Some(file))
}

fn dsf_recovery_identity(
    file: &mut std::fs::File,
    metadata_offset: u64,
    file_size: u64,
) -> Result<[u8; TAIL_JOURNAL_IDENTITY_LEN], String> {
    use sha2::{Digest, Sha256};

    if metadata_offset > file_size {
        return Err(format!(
            "DSF recovery identity offset {metadata_offset} exceeds file size {file_size}"
        ));
    }
    let sample_len = metadata_offset.min(RECOVERY_IDENTITY_SAMPLE_BYTES as u64) as usize;
    let mut first = vec![0u8; sample_len];
    file.seek(SeekFrom::Start(0))
        .map_err(|error| format!("seek DSF recovery identity prefix: {error}"))?;
    file.read_exact(&mut first)
        .map_err(|error| format!("read DSF recovery identity prefix: {error}"))?;

    let mut last = vec![0u8; sample_len];
    file.seek(SeekFrom::Start(metadata_offset - sample_len as u64))
        .map_err(|error| format!("seek DSF recovery identity suffix: {error}"))?;
    file.read_exact(&mut last)
        .map_err(|error| format!("read DSF recovery identity suffix: {error}"))?;

    let mut digest = Sha256::new();
    digest.update(b"tonepoet-dsf-tail-recovery-v2\0");
    digest.update(file_size.to_le_bytes());
    digest.update(metadata_offset.to_le_bytes());
    digest.update((sample_len as u64).to_le_bytes());
    digest.update(&first);
    digest.update(&last);
    Ok(digest.finalize().into())
}

fn write_tail_in_place(
    path: &Path,
    offset: u64,
    replacement: &[u8],
    is_cancelled: &dyn Fn() -> bool,
    progress: &dyn Fn(DsfWriteProgress),
) -> Result<Option<String>, String> {
    if is_cancelled() {
        return Err("metadata save cancelled before DSF tail journaling".to_string());
    }
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|error| format!("open DSF tail for update '{}': {error}", path.display()))?;
    let expected_size = offset
        .checked_add(replacement.len() as u64)
        .ok_or_else(|| format!("DSF tail range overflows for '{}'", path.display()))?;
    let actual_size = file
        .metadata()
        .map_err(|error| format!("stat DSF tail source '{}': {error}", path.display()))?
        .len();
    if actual_size != expected_size {
        return Err(format!(
            "DSF source '{}' changed during metadata preparation: expected {expected_size} bytes, found {actual_size}",
            path.display()
        ));
    }
    let recovery_identity = dsf_recovery_identity(&mut file, offset, actual_size)
        .map_err(|error| format!("bind DSF tail recovery journal for '{}': {error}", path.display()))?;
    let journal = tail_journal_path(path);
    publish_tail_journal_streaming(
        path,
        &journal,
        &mut file,
        TailJournalHeader {
            state: TAIL_JOURNAL_PREPARED,
            offset,
            original_len: replacement.len() as u64,
            recovery_identity,
        },
        is_cancelled,
        progress,
    )?;

    if let Err(error) = file.seek(SeekFrom::Start(offset)) {
        let primary = format!(
            "seek to DSF metadata write offset in '{}': {error}",
            path.display()
        );
        return match remove_journal_durably(&journal) {
            Ok(()) => Err(primary),
            Err(cleanup) => Err(format!(
                "{primary}; target bytes were not changed, but journal cleanup failed: {cleanup}"
            )),
        };
    }
    let mut written = 0usize;
    while written < replacement.len() {
        if is_cancelled() {
            return rollback_cancelled_tail(path, &journal, &mut file, progress);
        }
        let end = (written + COPY_CHUNK_BYTES).min(replacement.len());
        if let Err(error) = file.write_all(&replacement[written..end]) {
            let primary = format!("write DSF metadata tail '{}': {error}", path.display());
            return match restore_tail_from_journal(path, &journal, &mut file, progress) {
                Ok(()) => match remove_journal_durably(&journal) {
                    Ok(()) => Err(primary),
                    Err(cleanup) => Err(format!("{primary}; rollback succeeded, but journal cleanup failed: {cleanup}")),
                },
                Err(rollback) => Err(format!("{primary}; rollback failed and journal '{}' was retained: {rollback}", journal.display())),
            };
        }
        written = end;
        progress(DsfWriteProgress {
            phase: DsfWriteProgressPhase::WritingTail,
            bytes_done: written as u64,
            bytes_total: replacement.len() as u64,
        });
    }
    if is_cancelled() {
        return rollback_cancelled_tail(path, &journal, &mut file, progress);
    }
    if let Err(error) = file.sync_all() {
        let primary = format!("fsync DSF metadata tail '{}': {error}", path.display());
        return match restore_tail_from_journal(path, &journal, &mut file, progress) {
            Ok(()) => match remove_journal_durably(&journal) {
                Ok(()) => Err(primary),
                Err(cleanup) => Err(format!("{primary}; rollback succeeded, but journal cleanup failed: {cleanup}")),
            },
            Err(rollback) => Err(format!("{primary}; rollback failed and journal '{}' was retained: {rollback}", journal.display())),
        };
    }

    if is_cancelled() {
        return rollback_cancelled_tail(path, &journal, &mut file, progress);
    }
    match mark_tail_journal_committed(&journal) {
        TailJournalCommitOutcome::Durable => {}
        TailJournalCommitOutcome::StateUnchanged(error) => {
            let primary = format!(
                "could not commit DSF tail journal for '{}': {error}",
                path.display()
            );
            return match restore_tail_from_journal(path, &journal, &mut file, progress) {
                Ok(()) => match remove_journal_durably(&journal) {
                    Ok(()) => Err(primary),
                    Err(cleanup) => Err(format!(
                        "{primary}; rollback succeeded, but journal cleanup failed: {cleanup}"
                    )),
                },
                Err(rollback) => Err(format!(
                    "{primary}; rollback failed and PREPARED journal '{}' was retained for startup recovery: {rollback}",
                    journal.display()
                )),
            };
        }
        TailJournalCommitOutcome::DurabilityUncertain(error) => {
            progress(DsfWriteProgress {
                phase: DsfWriteProgressPhase::Publishing,
                bytes_done: replacement.len() as u64,
                bytes_total: replacement.len() as u64,
            });
            return Ok(Some(format!(
                "DSF metadata tail for '{}' was written and fsynced, but journal commit durability is uncertain: {error}. Journal '{}' was retained; recovery will keep the new tail if COMMITTED is durable or restore the old tail if PREPARED is durable",
                path.display(),
                journal.display()
            )));
        }
    }
    progress(DsfWriteProgress {
        phase: DsfWriteProgressPhase::Publishing,
        bytes_done: replacement.len() as u64,
        bytes_total: replacement.len() as u64,
    });
    match remove_journal_durably(&journal) {
        Ok(()) => Ok(None),
        Err(error) => Ok(Some(format!(
            "DSF metadata write for '{}' committed, but its committed tail journal could not be retired durably: {error}",
            path.display()
        ))),
    }
}

fn publish_tail_journal_streaming(
    path: &Path,
    journal: &Path,
    source: &mut std::fs::File,
    header: TailJournalHeader,
    is_cancelled: &dyn Fn() -> bool,
    progress: &dyn Fn(DsfWriteProgress),
) -> Result<(), String> {
    match std::fs::symlink_metadata(journal) {
        Ok(_) => {
            return Err(format!(
                "refusing to replace unresolved DSF tail journal '{}'",
                journal.display()
            ))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "inspect DSF tail journal path '{}': {error}",
                journal.display()
            ))
        }
    }
    let parent = journal.parent().unwrap_or_else(|| Path::new("."));
    let file_name = journal
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("tonepoet-dsf-tail.journal");
    let (temporary_path, mut temporary) = allocate_private_temp(parent, file_name)?;
    let mut published = false;
    let result = (|| {
        temporary
            .write_all(&encode_tail_journal_header(header))
            .map_err(|error| format!("write DSF tail journal header '{}': {error}", temporary_path.display()))?;
        source
            .seek(SeekFrom::Start(header.offset))
            .map_err(|error| format!("seek original DSF metadata tail '{}': {error}", path.display()))?;
        progress(DsfWriteProgress {
            phase: DsfWriteProgressPhase::Journaling,
            bytes_done: 0,
            bytes_total: header.original_len,
        });
        let mut copied = 0u64;
        let mut buffer = vec![0u8; COPY_CHUNK_BYTES];
        while copied < header.original_len {
            if is_cancelled() {
                return Err("metadata save cancelled during DSF tail journal copy".to_string());
            }
            let wanted = usize::try_from((header.original_len - copied).min(buffer.len() as u64))
                .expect("bounded DSF journal copy chunk");
            let read = source
                .read(&mut buffer[..wanted])
                .map_err(|error| format!("read original DSF metadata tail '{}': {error}", path.display()))?;
            if read == 0 {
                return Err(format!(
                    "DSF source '{}' ended after {copied} tail byte(s); expected {}",
                    path.display(),
                    header.original_len
                ));
            }
            temporary
                .write_all(&buffer[..read])
                .map_err(|error| format!("write DSF tail journal '{}': {error}", temporary_path.display()))?;
            copied += read as u64;
            progress(DsfWriteProgress {
                phase: DsfWriteProgressPhase::Journaling,
                bytes_done: copied,
                bytes_total: header.original_len,
            });
        }
        if is_cancelled() {
            return Err("metadata save cancelled before syncing DSF tail journal".to_string());
        }
        temporary
            .sync_all()
            .map_err(|error| format!("sync DSF tail journal temporary '{}': {error}", temporary_path.display()))?;
        if is_cancelled() {
            return Err("metadata save cancelled before publishing DSF tail journal".to_string());
        }
        drop(temporary);
        std::fs::rename(&temporary_path, journal).map_err(|error| {
            format!(
                "publish DSF tail journal '{}' from '{}': {error}",
                journal.display(),
                temporary_path.display()
            )
        })?;
        published = true;
        crate::config::sync_parent_dir(parent).map_err(|error| {
            format!(
                "sync parent after publishing DSF tail journal '{}': {error}",
                journal.display()
            )
        })?;
        Ok(())
    })();
    match result {
        Ok(()) => Ok(()),
        Err(primary) if published => Err(primary),
        Err(primary) => match std::fs::remove_file(&temporary_path) {
            Ok(()) => match crate::config::sync_parent_dir(parent) {
                Ok(()) => Err(primary),
                Err(error) => Err(format!(
                    "{primary}; unpublished DSF tail-journal temp '{}' was removed, but parent-directory durability could not be confirmed: {error}",
                    temporary_path.display()
                )),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(primary),
            Err(error) => Err(format!(
                "{primary}; additionally could not remove unpublished DSF tail-journal temp '{}': {error}",
                temporary_path.display()
            )),
        },
    }
}

fn allocate_private_temp(parent: &Path, file_name: &str) -> Result<(PathBuf, std::fs::File), String> {
    for _ in 0..128 {
        let private_name = if file_name.starts_with('.') {
            format!("{file_name}.{}.tmp", uuid::Uuid::new_v4())
        } else {
            format!(".{file_name}.{}.tmp", uuid::Uuid::new_v4())
        };
        let path = parent.join(private_name);
        #[cfg(unix)]
        let opened = {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
        };
        #[cfg(not(unix))]
        let opened = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path);
        match opened {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "allocate DSF tail-journal temporary in '{}': {error}",
                    parent.display()
                ))
            }
        }
    }
    Err(format!(
        "could not allocate a unique DSF tail-journal temporary in '{}'",
        parent.display()
    ))
}

fn rollback_cancelled_tail(
    path: &Path,
    journal: &Path,
    file: &mut std::fs::File,
    progress: &dyn Fn(DsfWriteProgress),
) -> Result<Option<String>, String> {
    match restore_tail_from_journal(path, journal, file, progress) {
        Ok(()) => match remove_journal_durably(journal) {
            Ok(()) => Err(format!(
                "metadata save cancelled while writing DSF metadata tail for '{}' and the original tail was restored",
                path.display()
            )),
            Err(cleanup) => Err(format!(
                "metadata save cancelled while writing DSF metadata tail for '{}'; the original tail was restored, but journal cleanup failed: {cleanup}",
                path.display()
            )),
        },
        Err(rollback) => Err(format!(
            "metadata save cancelled while writing DSF metadata tail for '{}'; rollback failed and journal '{}' was retained for startup recovery: {rollback}",
            path.display(),
            journal.display()
        )),
    }
}

fn verify_tail_journal_target(
    path: &Path,
    file: &mut std::fs::File,
    header: TailJournalHeader,
) -> Result<(), String> {
    let expected = header
        .offset
        .checked_add(header.original_len)
        .ok_or_else(|| format!("DSF tail journal range overflows for '{}'", path.display()))?;
    let actual = file
        .metadata()
        .map_err(|error| format!("stat DSF target during journal recovery '{}': {error}", path.display()))?
        .len();
    if actual != expected {
        return Err(format!(
            "refusing DSF tail-journal recovery for '{}': journal expects {expected} bytes, target has {actual}",
            path.display()
        ));
    }
    let actual_identity = dsf_recovery_identity(file, header.offset, actual).map_err(|error| {
        format!(
            "verify DSF target identity during journal recovery '{}': {error}",
            path.display()
        )
    })?;
    if actual_identity != header.recovery_identity {
        return Err(format!(
            "refusing DSF tail-journal recovery for '{}': the bounded audio-prefix identity no longer matches the journal authority",
            path.display()
        ));
    }
    Ok(())
}

fn restore_tail_from_journal(
    path: &Path,
    journal: &Path,
    file: &mut std::fs::File,
    progress: &dyn Fn(DsfWriteProgress),
) -> Result<(), String> {
    let mut journal_file = open_tail_journal_for_read(journal)?.ok_or_else(|| {
        format!(
            "open DSF tail journal '{}': file disappeared before rollback",
            journal.display()
        )
    })?;
    let header = read_tail_journal_header(journal, &mut journal_file)?;
    if header.state != TAIL_JOURNAL_PREPARED {
        return Err(format!(
            "refusing immediate DSF rollback from journal '{}': state {} is not PREPARED",
            journal.display(),
            header.state
        ));
    }
    verify_tail_journal_target(path, file, header)?;
    journal_file
        .seek(SeekFrom::Start(TAIL_JOURNAL_HEADER_LEN as u64))
        .map_err(|error| format!("seek DSF tail journal payload '{}': {error}", journal.display()))?;
    file.seek(SeekFrom::Start(header.offset))
        .map_err(|error| format!("seek for DSF tail rollback: {error}"))?;
    let mut copied = 0u64;
    let mut buffer = vec![0u8; COPY_CHUNK_BYTES];
    while copied < header.original_len {
        let wanted = usize::try_from((header.original_len - copied).min(buffer.len() as u64))
            .expect("bounded DSF rollback copy chunk");
        journal_file
            .read_exact(&mut buffer[..wanted])
            .map_err(|error| format!("read original DSF metadata tail from journal: {error}"))?;
        file.write_all(&buffer[..wanted])
            .map_err(|error| format!("restore original DSF metadata tail: {error}"))?;
        copied += wanted as u64;
        progress(DsfWriteProgress {
            phase: DsfWriteProgressPhase::Recovering,
            bytes_done: copied,
            bytes_total: header.original_len,
        });
    }
    file.sync_all()
        .map_err(|error| format!("fsync restored DSF metadata tail: {error}"))
}

fn mark_tail_journal_committed(journal: &Path) -> TailJournalCommitOutcome {
    let mut file = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(journal)
    {
        Ok(file) => file,
        Err(error) => {
            return TailJournalCommitOutcome::StateUnchanged(format!(
                "open DSF tail journal '{}' to commit: {error}",
                journal.display()
            ))
        }
    };
    if let Err(error) = file.seek(SeekFrom::Start(TAIL_JOURNAL_STATE_OFFSET)) {
        return TailJournalCommitOutcome::StateUnchanged(format!(
            "seek DSF tail journal state '{}': {error}",
            journal.display()
        ));
    }
    if let Err(error) = file.write_all(&[TAIL_JOURNAL_COMMITTED]) {
        // `write_all` does not prove that zero bytes reached the file when it
        // returns an error. A one-byte state transition may therefore already
        // be visible. Treat the journal as uncertain and never roll back from
        // it until recovery observes the durable PREPARED/COMMITTED byte.
        return TailJournalCommitOutcome::DurabilityUncertain(format!(
            "mark DSF tail journal committed '{}': {error}",
            journal.display()
        ));
    }
    #[cfg(test)]
    if TEST_FAIL_TAIL_JOURNAL_COMMIT_SYNC.with(|flag| flag.replace(false)) {
        return TailJournalCommitOutcome::DurabilityUncertain(format!(
            "fsync committed DSF tail journal '{}': synthetic commit-sync failure",
            journal.display()
        ));
    }
    match file.sync_all() {
        Ok(()) => TailJournalCommitOutcome::Durable,
        Err(error) => TailJournalCommitOutcome::DurabilityUncertain(format!(
            "fsync committed DSF tail journal '{}': {error}",
            journal.display()
        )),
    }
}

fn remove_journal_durably(journal: &Path) -> Result<(), String> {
    match std::fs::remove_file(journal) {
        Ok(()) => crate::config::sync_parent_dir(
            journal.parent().unwrap_or_else(|| Path::new(".")),
        )
        .map_err(|error| format!("sync parent after removing '{}': {error}", journal.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("remove '{}': {error}", journal.display())),
    }
}

fn rewrite_container(
    path: &Path,
    location: DsfMetadataLocation,
    encoded_tag: &[u8],
    is_cancelled: &dyn Fn() -> bool,
    progress: &dyn Fn(DsfWriteProgress),
) -> Result<Option<String>, String> {
    let (metadata_offset, old_file_size) = match location {
        DsfMetadataLocation::Untagged { file_size } => (file_size, file_size),
        DsfMetadataLocation::Id3 {
            offset,
            tag_end,
            file_size,
        } => {
            if tag_end != file_size {
                return Err(format!(
                    "refusing to replace DSF ID3 metadata in '{}': {} trailing byte(s) follow the declared tag, so their ownership is ambiguous",
                    path.display(),
                    file_size - tag_end
                ));
            }
            (offset, file_size)
        }
    };
    if is_cancelled() {
        return Err("metadata save cancelled before starting DSF full-file rewrite".to_string());
    }
    let new_file_size = metadata_offset
        .checked_add(encoded_tag.len() as u64)
        .ok_or_else(|| format!("rewritten DSF file size overflows for '{}'", path.display()))?;
    let mut source = std::fs::File::open(path)
        .map_err(|error| format!("open DSF source '{}': {error}", path.display()))?;
    let source_metadata = source
        .metadata()
        .map_err(|error| format!("stat DSF source '{}': {error}", path.display()))?;
    if source_metadata.len() != old_file_size {
        return Err(format!(
            "DSF source '{}' changed during metadata preparation: expected {old_file_size} bytes, found {}",
            path.display(),
            source_metadata.len()
        ));
    }

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("audio.dsf");
    let (temporary_path, mut temporary) = allocate_temp(parent, file_name, &source_metadata)?;
    let total = metadata_offset.saturating_add(encoded_tag.len() as u64);
    let mut published = false;
    let result = (|| {
        let mut copied = 0u64;
        let mut buffer = vec![0u8; COPY_CHUNK_BYTES];
        while copied < metadata_offset {
            if is_cancelled() {
                return Err("metadata save cancelled during DSF audio-prefix copy".to_string());
            }
            let wanted = usize::try_from((metadata_offset - copied).min(buffer.len() as u64))
                .expect("bounded DSF copy chunk");
            let read = source
                .read(&mut buffer[..wanted])
                .map_err(|error| format!("read DSF audio prefix: {error}"))?;
            if read == 0 {
                return Err(format!(
                    "DSF source '{}' ended after {copied} bytes while copying the required {metadata_offset}-byte audio prefix",
                    path.display()
                ));
            }
            temporary
                .write_all(&buffer[..read])
                .map_err(|error| format!("copy DSF audio prefix to temporary file: {error}"))?;
            copied += read as u64;
            progress(DsfWriteProgress {
                phase: DsfWriteProgressPhase::CopyingPrefix,
                bytes_done: copied,
                bytes_total: total,
            });
        }
        temporary
            .seek(SeekFrom::Start(12))
            .map_err(|error| format!("seek to DSF file-size field in temporary file: {error}"))?;
        temporary
            .write_all(&new_file_size.to_le_bytes())
            .map_err(|error| format!("write DSF declared file size: {error}"))?;
        temporary
            .write_all(&metadata_offset.to_le_bytes())
            .map_err(|error| format!("write DSF metadata pointer: {error}"))?;
        temporary
            .seek(SeekFrom::Start(metadata_offset))
            .map_err(|error| format!("seek to DSF metadata publication offset: {error}"))?;
        let mut tag_written = 0usize;
        while tag_written < encoded_tag.len() {
            if is_cancelled() {
                return Err("metadata save cancelled during DSF tag copy".to_string());
            }
            let end = (tag_written + COPY_CHUNK_BYTES).min(encoded_tag.len());
            temporary
                .write_all(&encoded_tag[tag_written..end])
                .map_err(|error| format!("write encoded ID3 tag to DSF temporary file: {error}"))?;
            tag_written = end;
            progress(DsfWriteProgress {
                phase: DsfWriteProgressPhase::CopyingPrefix,
                bytes_done: metadata_offset + tag_written as u64,
                bytes_total: total,
            });
        }
        temporary
            .sync_all()
            .map_err(|error| format!("sync DSF temporary file: {error}"))?;
        if is_cancelled() {
            return Err("metadata save cancelled before committing DSF full-file rewrite".to_string());
        }
        drop(temporary);
        crate::config::replace_config_file(&temporary_path, path)
            .map_err(|error| format!("atomically replace DSF file: {error}"))?;
        published = true;
        progress(DsfWriteProgress {
            phase: DsfWriteProgressPhase::Publishing,
            bytes_done: total,
            bytes_total: total,
        });
        match crate::config::sync_parent_dir(parent) {
            Ok(()) => Ok(None),
            Err(error) => Ok(Some(format!(
                "DSF metadata replacement for '{}' completed, but parent-directory durability could not be confirmed: {error}",
                path.display()
            ))),
        }
    })();
    if result.is_err() && !published {
        let _ = std::fs::remove_file(&temporary_path);
    }
    result
}

fn allocate_temp(
    parent: &Path,
    file_name: &str,
    source_metadata: &std::fs::Metadata,
) -> Result<(PathBuf, std::fs::File), String> {
    for _ in 0..128 {
        let path = parent.join(format!(
            ".{file_name}.tonepoet-id3-{}.tmp",
            uuid::Uuid::new_v4()
        ));
        #[cfg(unix)]
        let opened = {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            let mode = source_metadata.permissions().mode() & 0o777;
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(mode)
                .open(&path)
        };
        #[cfg(not(unix))]
        let opened = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path);
        match opened {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "allocate DSF temporary file in '{}': {error}",
                    parent.display()
                ))
            }
        }
    }
    Err(format!(
        "could not allocate a unique DSF temporary file in '{}'",
        parent.display()
    ))
}

fn recover_tail_journal(path: &Path) -> Result<bool, String> {
    let journal = tail_journal_path(path);
    let Some(mut journal_file) = open_tail_journal_for_read(&journal)? else {
        return Ok(false);
    };
    let header = read_tail_journal_header(&journal, &mut journal_file)?;
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(header.state == TAIL_JOURNAL_PREPARED)
        .open(path)
        .map_err(|error| format!("open DSF target for journal recovery '{}': {error}", path.display()))?;
    verify_tail_journal_target(path, &mut file, header)?;
    if header.state == TAIL_JOURNAL_PREPARED {
        journal_file
            .seek(SeekFrom::Start(TAIL_JOURNAL_HEADER_LEN as u64))
            .map_err(|error| format!("seek DSF tail journal payload '{}': {error}", journal.display()))?;
        file.seek(SeekFrom::Start(header.offset))
            .map_err(|error| format!("seek for DSF tail rollback: {error}"))?;
        let mut copied = 0u64;
        let mut buffer = vec![0u8; COPY_CHUNK_BYTES];
        while copied < header.original_len {
            let wanted = usize::try_from((header.original_len - copied).min(buffer.len() as u64))
                .expect("bounded DSF recovery copy chunk");
            journal_file
                .read_exact(&mut buffer[..wanted])
                .map_err(|error| format!("read original DSF metadata tail from journal: {error}"))?;
            file.write_all(&buffer[..wanted])
                .map_err(|error| format!("restore original DSF metadata tail: {error}"))?;
            copied += wanted as u64;
        }
        file.sync_all()
            .map_err(|error| format!("fsync restored DSF metadata tail: {error}"))?;
    }
    remove_journal_durably(&journal)?;
    Ok(true)
}

fn preflight_dsf_write_artifacts(path: &Path) -> Result<(), String> {
    recover_tail_journal(path)?;
    remove_orphaned_dsf_temps_for_target(path)?;
    let legacy_backup = crate::db::Database::backup_path_for(path);
    match std::fs::symlink_metadata(&legacy_backup) {
        Ok(_) => {
            let inspection = inspect_legacy_backup_locked(path, &legacy_backup)?;
            if inspection.byte_identical {
                remove_legacy_backup_marker_durably(&legacy_backup)?;
            } else {
                return Err(format!(
                    "refusing to save DSF metadata for '{}': unversioned legacy rollback marker '{}' differs from the current file. Inspect it with `tonepoet dsf-recover status '{}'`, then choose `restore-backup` or `keep-current`; tonepoet will not guess which generation is authoritative",
                    path.display(),
                    legacy_backup.display(),
                    path.display()
                ));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "inspect legacy DSF rollback marker '{}': {error}",
                legacy_backup.display()
            ))
        }
    }
    Ok(())
}

/// Inspect a legacy `.tonepoet-bak` marker under the same bounded per-target
/// lock used by DSF writes. Comparison is streaming and uses fixed memory.
pub fn inspect_legacy_backup(path: &Path) -> Result<DsfLegacyBackupInspection, String> {
    let (_lock, target) = acquire_dsf_write_lock(path)?;
    let marker = crate::db::Database::backup_path_for(&target);
    inspect_legacy_backup_locked(&target, &marker)
}

/// Resolve an ambiguous legacy marker explicitly. `RestoreBackup` publishes
/// the marker atomically through the database recovery helper; `KeepCurrent`
/// durably retires only the marker. Both operations hold the DSF target lock.
pub fn resolve_legacy_backup(
    path: &Path,
    resolution: DsfLegacyBackupResolution,
) -> Result<DsfLegacyBackupInspection, String> {
    let (_lock, target) = acquire_dsf_write_lock(path)?;
    let marker = crate::db::Database::backup_path_for(&target);
    let inspection = inspect_legacy_backup_locked(&target, &marker)?;
    match resolution {
        DsfLegacyBackupResolution::RestoreBackup => {
            crate::db::Database::restore_backup_for(&target, &marker)?;
        }
        DsfLegacyBackupResolution::KeepCurrent => {
            remove_legacy_backup_marker_durably(&marker)?;
        }
    }
    Ok(inspection)
}

fn inspect_legacy_backup_locked(
    target: &Path,
    marker: &Path,
) -> Result<DsfLegacyBackupInspection, String> {
    let target_meta = std::fs::metadata(target)
        .map_err(|error| format!("inspect current DSF '{}': {error}", target.display()))?;
    if !target_meta.is_file() {
        return Err(format!("current DSF '{}' is not a regular file", target.display()));
    }
    let marker_path_meta = std::fs::symlink_metadata(marker)
        .map_err(|error| format!("inspect legacy DSF rollback marker '{}': {error}", marker.display()))?;
    if marker_path_meta.file_type().is_symlink() || !marker_path_meta.is_file() {
        return Err(format!(
            "legacy DSF rollback marker '{}' is not a regular, non-symlink file",
            marker.display()
        ));
    }
    let marker_meta = std::fs::metadata(marker)
        .map_err(|error| format!("stat legacy DSF rollback marker '{}': {error}", marker.display()))?;
    let target_bytes = target_meta.len();
    let marker_bytes = marker_meta.len();
    let byte_identical = if target_bytes != marker_bytes {
        false
    } else {
        files_equal_streaming(target, marker, target_bytes)?
    };
    Ok(DsfLegacyBackupInspection {
        target: target.to_path_buf(),
        marker: marker.to_path_buf(),
        target_bytes,
        marker_bytes,
        byte_identical,
    })
}

fn files_equal_streaming(left: &Path, right: &Path, expected_len: u64) -> Result<bool, String> {
    let mut left_file = std::fs::File::open(left)
        .map_err(|error| format!("open current DSF '{}' for comparison: {error}", left.display()))?;
    let mut right_file = std::fs::File::open(right)
        .map_err(|error| format!("open legacy DSF rollback marker '{}' for comparison: {error}", right.display()))?;
    let mut left_buffer = vec![0u8; COPY_CHUNK_BYTES];
    let mut right_buffer = vec![0u8; COPY_CHUNK_BYTES];
    let mut compared = 0u64;
    while compared < expected_len {
        let wanted = usize::try_from((expected_len - compared).min(COPY_CHUNK_BYTES as u64))
            .expect("bounded DSF legacy-marker comparison chunk");
        left_file
            .read_exact(&mut left_buffer[..wanted])
            .map_err(|error| format!("read current DSF '{}' during comparison: {error}", left.display()))?;
        right_file
            .read_exact(&mut right_buffer[..wanted])
            .map_err(|error| format!("read legacy DSF rollback marker '{}' during comparison: {error}", right.display()))?;
        if left_buffer[..wanted] != right_buffer[..wanted] {
            return Ok(false);
        }
        compared += wanted as u64;
    }
    Ok(true)
}

fn remove_legacy_backup_marker_durably(marker: &Path) -> Result<(), String> {
    match std::fs::remove_file(marker) {
        Ok(()) => crate::config::sync_parent_dir(marker.parent().unwrap_or_else(|| Path::new(".")))
            .map_err(|error| format!(
                "legacy DSF rollback marker '{}' was removed, but parent-directory durability could not be confirmed: {error}",
                marker.display()
            )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("remove legacy DSF rollback marker '{}': {error}", marker.display())),
    }
}

fn remove_orphaned_dsf_temps_for_target(path: &Path) -> Result<usize, String> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let target_name = path.file_name().and_then(|name| name.to_str());
    let entries = std::fs::read_dir(parent)
        .map_err(|error| format!("scan DSF rewrite temporaries in '{}': {error}", parent.display()))?;
    let mut removed = 0usize;
    for entry in entries {
        let entry = entry
            .map_err(|error| format!("read DSF rewrite-temporary entry in '{}': {error}", parent.display()))?;
        let artifact = entry.path();
        let Some(name) = artifact.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(original_name) = dsf_temp_original_name(name)
            .or_else(|| tail_journal_temp_original_name(name))
        else {
            continue;
        };
        match target_name {
            Some(target_name) if original_name != target_name => continue,
            Some(_) => {}
            None if original_name == "audio.dsf" => {
                return Err(format!(
                    "cannot safely attribute orphan DSF temporary artifact '{}' to non-UTF-8 target '{}'; explicit inspection is required",
                    artifact.display(),
                    path.display()
                ));
            }
            None => continue,
        }
        let metadata = std::fs::symlink_metadata(&artifact)
            .map_err(|error| format!("inspect DSF rewrite temp '{}': {error}", artifact.display()))?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(format!(
                "refusing to remove ambiguous DSF temporary artifact '{}': expected a regular file",
                artifact.display()
            ));
        }
        std::fs::remove_file(&artifact)
            .map_err(|error| format!("remove stale DSF temporary artifact '{}': {error}", artifact.display()))?;
        removed += 1;
    }
    if removed > 0 {
        crate::config::sync_parent_dir(parent).map_err(|error| {
            format!(
                "sync parent after removing {removed} stale DSF temporary artifact(s) for '{}': {error}",
                path.display()
            )
        })?;
    }
    Ok(removed)
}

fn dsf_temp_original_name(name: &str) -> Option<&str> {
    let (prefix, suffix) = name.rsplit_once(".tonepoet-id3-")?;
    let original = prefix.strip_prefix('.')?;
    if !original.to_ascii_lowercase().ends_with(".dsf") {
        return None;
    }
    let uuid = suffix.strip_suffix(".tmp")?;
    uuid::Uuid::parse_str(uuid).ok()?;
    Some(original)
}

fn tail_journal_temp_original_name(name: &str) -> Option<&str> {
    let stem = name.strip_suffix(".tmp")?;
    let (journal_name, uuid) = stem.rsplit_once('.')?;
    uuid::Uuid::parse_str(uuid).ok()?;
    let original = journal_name
        .strip_prefix('.')?
        .strip_suffix(".tonepoet-dsf-tail.journal")?;
    if !original.to_ascii_lowercase().ends_with(".dsf") {
        return None;
    }
    Some(original)
}

#[cfg(test)]
fn is_dsf_temp_name(name: &str) -> bool {
    dsf_temp_original_name(name).is_some()
}

/// Recover generation-bound DSF tail journals, report unversioned legacy
/// full-file backup markers without applying them, and remove orphaned temp
/// rewrites in one directory. Messages are suitable for the startup status
/// surface; failures remain visible and leave their authority artifacts intact.
pub fn recover_stale_writes_in_directory(dir: &Path) -> Vec<String> {
    let mut messages = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return messages;
    };
    for entry in entries.flatten() {
        let artifact = entry.path();
        let Some(name) = artifact.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if let Some(original_name) = name
            .strip_prefix('.')
            .and_then(|name| name.strip_suffix(".tonepoet-dsf-tail.journal"))
            .filter(|name| name.to_ascii_lowercase().ends_with(".dsf"))
        {
            let original = artifact.with_file_name(original_name);
            match acquire_dsf_write_lock(&original) {
                Ok((_lock, target)) => match recover_tail_journal(&target) {
                    Ok(true) => messages.push(format!(
                        "Recovered DSF metadata tail journal for {}",
                        target.display()
                    )),
                    Ok(false) => {}
                    Err(error) => messages.push(format!(
                        "DSF metadata tail-journal recovery failed for {}: {error}",
                        target.display()
                    )),
                },
                Err(error) => messages.push(format!(
                    "DSF metadata tail-journal recovery deferred for {}: {error}",
                    original.display()
                )),
            }
            continue;
        }
        if let Some(original_name) = tail_journal_temp_original_name(name) {
            let original = artifact.with_file_name(original_name);
            if original_name == "audio.dsf" && !original.exists() {
                messages.push(format!(
                    "DSF tail-journal temp '{}' was retained because its fallback name cannot be safely attributed to a non-UTF-8 or removed target",
                    artifact.display()
                ));
                continue;
            }
            if !original.exists() {
                messages.push(format!(
                    "DSF tail-journal temp '{}' was retained because its target '{}' is missing",
                    artifact.display(),
                    original.display()
                ));
                continue;
            }
            match acquire_dsf_write_lock(&original) {
                Ok((_lock, _target)) => match std::fs::remove_file(&artifact) {
                    Ok(()) => match crate::config::sync_parent_dir(dir) {
                        Ok(()) => messages.push(format!(
                            "Removed stale DSF tail-journal temp {}",
                            artifact.display()
                        )),
                        Err(error) => messages.push(format!(
                            "Removed stale DSF tail-journal temp {}, but directory durability is unconfirmed: {error}",
                            artifact.display()
                        )),
                    },
                    Err(error) => messages.push(format!(
                        "DSF tail-journal temp cleanup failed for {}: {error}",
                        artifact.display()
                    )),
                },
                Err(error) => messages.push(format!(
                    "DSF tail-journal temp cleanup deferred for {}: {error}",
                    artifact.display()
                )),
            }
            continue;
        }
        if let Some(original_name) = name
            .strip_suffix(".tonepoet-bak")
            .filter(|name| name.to_ascii_lowercase().ends_with(".dsf"))
        {
            let original = artifact.with_file_name(original_name);
            match acquire_dsf_write_lock(&original) {
                Ok((_lock, target)) => match inspect_legacy_backup_locked(&target, &artifact) {
                    Ok(inspection) if inspection.byte_identical => {
                        match remove_legacy_backup_marker_durably(&artifact) {
                            Ok(()) => messages.push(format!(
                                "Retired byte-identical legacy DSF rollback marker {}",
                                artifact.display()
                            )),
                            Err(error) => messages.push(format!(
                                "Byte-identical legacy DSF rollback marker retirement failed for {}: {error}",
                                artifact.display()
                            )),
                        }
                    }
                    Ok(_) => messages.push(format!(
                        "Legacy DSF rollback marker '{}' differs from '{}'; inspect with `tonepoet dsf-recover status '{}'`, then choose `restore-backup` or `keep-current`",
                        artifact.display(),
                        target.display(),
                        target.display()
                    )),
                    Err(error) => messages.push(format!(
                        "Legacy DSF rollback-marker inspection failed for {}: {error}",
                        artifact.display()
                    )),
                },
                Err(error) => messages.push(format!(
                    "Legacy DSF rollback-marker recovery deferred for {}: {error}",
                    original.display()
                )),
            }
            continue;
        }
        if let Some(original_name) = dsf_temp_original_name(name) {
            let original = artifact.with_file_name(original_name);
            if original_name == "audio.dsf" && !original.exists() {
                messages.push(format!(
                    "DSF rewrite temp '{}' was retained because its fallback name cannot be safely attributed to a non-UTF-8 or removed target",
                    artifact.display()
                ));
                continue;
            }
            if !original.exists() {
                messages.push(format!(
                    "DSF rewrite temp '{}' was retained because its target '{}' is missing",
                    artifact.display(),
                    original.display()
                ));
                continue;
            }
            match acquire_dsf_write_lock(&original) {
                Ok((_lock, _target)) => match std::fs::remove_file(&artifact) {
                    Ok(()) => match crate::config::sync_parent_dir(dir) {
                        Ok(()) => messages.push(format!(
                            "Removed stale DSF rewrite temp {}",
                            artifact.display()
                        )),
                        Err(error) => messages.push(format!(
                            "Removed stale DSF rewrite temp {}, but directory durability is unconfirmed: {error}",
                            artifact.display()
                        )),
                    },
                    Err(error) => messages.push(format!(
                        "DSF rewrite temp cleanup failed for {}: {error}",
                        artifact.display()
                    )),
                },
                Err(error) => messages.push(format!(
                    "DSF rewrite temp cleanup deferred for {}: {error}",
                    artifact.display()
                )),
            }
        }
    }
    messages
}

fn canonicalize_key(key: &str) -> String {
    let squashed = key
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .map(|character| character.to_ascii_uppercase())
        .collect::<String>();
    match squashed.as_str() {
        "YEAR" => "DATE".to_string(),
        "ALBUMARTIST" | "ALBUMARTISTS" | "ALBUMARTISTCREDIT" => "ALBUMARTIST".to_string(),
        "TOTALTRACKS" => "TRACKTOTAL".to_string(),
        "TOTALDISCS" => "DISCTOTAL".to_string(),
        "DESCRIPTION" => "COMMENT".to_string(),
        _ => key.trim().to_ascii_uppercase(),
    }
}

fn append_distinct(values: &mut Vec<String>, value: String) {
    let value = value.trim().to_string();
    if !value.is_empty() && !values.iter().any(|existing| existing == &value) {
        values.push(value);
    }
}

fn canonicalize_snapshot(raw: DsfTagSnapshot) -> DsfTagSnapshot {
    let mut fields = BTreeMap::<String, Vec<String>>::new();
    let entries = raw.fields.into_iter().collect::<Vec<_>>();

    // Canonical spellings win deterministically even though the raw snapshot is
    // key-sorted. A second pass appends distinct legacy-alias values only after
    // the canonical values for the group.
    for canonical_pass in [true, false] {
        for (key, values) in &entries {
            let canonical_key = canonicalize_key(key);
            let normalized_key = key.trim().to_ascii_uppercase();
            if (normalized_key == canonical_key) != canonical_pass {
                continue;
            }
            let target = fields.entry(canonical_key).or_default();
            for value in values {
                append_distinct(target, value.clone());
            }
        }
    }
    DsfTagSnapshot { fields }
}

fn resolve_changes(changes: &[DsfTagChange]) -> Result<Vec<DsfTagChange>, String> {
    let mut resolved = BTreeMap::<String, Option<String>>::new();
    for change in changes {
        let key = canonicalize_key(&change.canonical_key);
        let value = change
            .value
            .as_ref()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        if matches!(
            key.as_str(),
            "TRACKNUMBER" | "TRACKTOTAL" | "DISCNUMBER" | "DISCTOTAL" | "BPM"
        ) {
            if let Some(raw) = value.as_deref() {
                let parsed = raw.parse::<u32>().map_err(|_| {
                    format!("invalid DSF metadata value for {key}: expected an unsigned integer, got {raw:?}")
                })?;
                if matches!(key.as_str(), "TRACKTOTAL" | "DISCTOTAL") && parsed == 0 {
                    return Err(format!(
                        "invalid DSF metadata value for {key}: totals must be greater than zero"
                    ));
                }
                if key == "BPM" && parsed == 0 {
                    return Err(
                        "invalid DSF metadata value for BPM: tempo must be greater than zero"
                            .to_string(),
                    );
                }
            }
        }
        if let Some(previous) = resolved.get(&key) {
            if previous != &value {
                return Err(format!(
                    "conflicting DSF metadata changes target canonical key {key}: {previous:?} versus {value:?}"
                ));
            }
        } else {
            resolved.insert(key, value);
        }
    }
    Ok(resolved
        .into_iter()
        .map(|(canonical_key, value)| DsfTagChange { canonical_key, value })
        .collect())
}

/// Direct dependency seam. Keep every crate-specific type and method here.
mod backend {
    use super::{DsfMetadataLocation, DsfTagChange, DsfTagSnapshot};
    use id3::frame::{Comment, Content, ExtendedText};
    use id3::{Tag, TagLike, Version};
    use std::collections::BTreeMap;
    use std::io::{Seek, SeekFrom};
    use std::path::Path;

    pub(super) fn read_at_location(
        path: &Path,
        location: DsfMetadataLocation,
    ) -> Result<DsfTagSnapshot, String> {
        let tag = read_tag(path, location)?;
        Ok(snapshot_from_tag(&tag))
    }

    pub(super) fn prepare(
        path: &Path,
        changes: &[DsfTagChange],
    ) -> Result<(DsfMetadataLocation, Vec<u8>), String> {
        let location = super::inspect_dsf_metadata_location(path)?;
        let mut tag = read_tag(path, location)?;
        apply_changes_to_tag(&mut tag, changes);

        let mut encoded = Vec::new();
        tag.write_to(&mut encoded, Version::Id3v24)
            .map_err(|error| format!("encode ID3v2.4 tag: {error}"))?;
        if !encoded.starts_with(b"ID3") {
            return Err("ID3 backend produced bytes without an ID3 marker".to_string());
        }
        Ok((location, encoded))
    }

    fn read_tag(path: &Path, location: DsfMetadataLocation) -> Result<Tag, String> {
        let DsfMetadataLocation::Id3 { offset, .. } = location else {
            return Ok(Tag::new());
        };
        let mut file = std::fs::File::open(path)
            .map_err(|error| format!("open DSF ID3 source '{}': {error}", path.display()))?;
        file.seek(SeekFrom::Start(offset)).map_err(|error| {
            format!(
                "seek to DSF ID3 metadata offset {offset} in '{}': {error}",
                path.display()
            )
        })?;
        Tag::read_from2(&mut file).map_err(|error| {
            format!(
                "DSF header declares ID3 metadata at offset {offset}, but the ID3 backend could not read it: {error}"
            )
        })
    }


    pub(super) fn snapshot_from_tag(tag: &Tag) -> DsfTagSnapshot {
        let mut fields = BTreeMap::<String, Vec<String>>::new();

        push(&mut fields, "TITLE", tag.title());
        push(&mut fields, "ARTIST", tag.artist());
        push(&mut fields, "ALBUM", tag.album());
        push(&mut fields, "ALBUMARTIST", tag.album_artist());
        push(&mut fields, "GENRE", tag.genre());
        if let Some(date) = tag
            .frames()
            .find(|frame| frame.id() == "TDRC")
            .and_then(|frame| frame.content().text())
        {
            push_owned(&mut fields, "DATE", date.to_string());
        } else if let Some(year) = tag.year() {
            push_owned(&mut fields, "DATE", year.to_string());
        }
        if let Some(track) = tag.track() {
            push_owned(&mut fields, "TRACKNUMBER", track.to_string());
        }
        if let Some(total) = tag.total_tracks() {
            push_owned(&mut fields, "TRACKTOTAL", total.to_string());
        }
        if let Some(disc) = tag.disc() {
            push_owned(&mut fields, "DISCNUMBER", disc.to_string());
        }
        if let Some(total) = tag.total_discs() {
            push_owned(&mut fields, "DISCTOTAL", total.to_string());
        }

        for comment in tag.comments() {
            push_owned(&mut fields, "COMMENT", comment.text.clone());
        }
        for extended in tag.extended_texts() {
            push_owned(
                &mut fields,
                extended.description.trim().to_ascii_uppercase(),
                extended.value.clone(),
            );
        }
        for frame in tag.frames() {
            let key = match frame.id() {
                "TCOM" => Some("COMPOSER"),
                "TPE3" => Some("CONDUCTOR"),
                "TSRC" => Some("ISRC"),
                "TPUB" => Some("LABEL"),
                "TCOP" => Some("COPYRIGHT"),
                "TENC" => Some("ENCODER"),
                "TBPM" => Some("BPM"),
                "TDOR" => Some("ORIGINALDATE"),
                _ => None,
            };
            if let (Some(key), Some(value)) = (key, frame.content().text()) {
                push_owned(&mut fields, key, value.to_string());
            }
        }
        DsfTagSnapshot { fields }
    }

    pub(super) fn apply_changes_to_tag(tag: &mut Tag, changes: &[DsfTagChange]) {
        for change in changes {
            apply_one(tag, change);
        }
    }

    #[cfg(test)]
    pub(super) fn multi_value_fixture_after(changes: &[DsfTagChange]) -> DsfTagSnapshot {
        let mut tag = Tag::new();
        tag.add_frame(id3::Frame::text("TIT2", "Original title"));
        tag.add_frame(Comment {
            lang: "eng".to_string(),
            description: "first".to_string(),
            text: "first comment".to_string(),
        });
        tag.add_frame(Comment {
            lang: "eng".to_string(),
            description: "second".to_string(),
            text: "second comment".to_string(),
        });
        apply_changes_to_tag(&mut tag, changes);
        snapshot_from_tag(&tag)
    }

    #[cfg(test)]
    pub(super) fn mapping_fixture_after(changes: &[DsfTagChange]) -> DsfTagSnapshot {
        let mut tag = Tag::new();
        tag.add_frame(id3::Frame::text("TIT2", "Original title"));
        tag.add_frame(id3::Frame::text("TRCK", "2/10"));
        tag.add_frame(id3::Frame::text("TPOS", "1/2"));
        tag.add_frame(Comment {
            lang: "eng".to_string(),
            description: String::new(),
            text: "Original comment".to_string(),
        });
        apply_changes_to_tag(&mut tag, changes);
        snapshot_from_tag(&tag)
    }

    fn apply_one(tag: &mut Tag, change: &DsfTagChange) {
        let value = change.value.as_deref();
        remove_extended_text_aliases(tag, &change.canonical_key);
        match change.canonical_key.as_str() {
            "TITLE" => set_text(tag, "TIT2", value),
            "ARTIST" => set_text(tag, "TPE1", value),
            "ALBUM" => set_text(tag, "TALB", value),
            "ALBUMARTIST" => set_text(tag, "TPE2", value),
            "GENRE" => set_text(tag, "TCON", value),
            "DATE" => set_text(tag, "TDRC", value),
            "COMPOSER" => set_text(tag, "TCOM", value),
            "CONDUCTOR" => set_text(tag, "TPE3", value),
            "ISRC" => set_text(tag, "TSRC", value),
            "LABEL" => set_text(tag, "TPUB", value),
            "COPYRIGHT" => set_text(tag, "TCOP", value),
            "ENCODER" => set_text(tag, "TENC", value),
            "BPM" => set_text(tag, "TBPM", value),
            "ORIGINALDATE" => set_text(tag, "TDOR", value),
            "TRACKNUMBER" | "TRACKTOTAL" => set_number_pair(tag, true, change),
            "DISCNUMBER" | "DISCTOTAL" => set_number_pair(tag, false, change),
            "COMMENT" => {
                tag.remove("COMM");
                if let Some(value) = value {
                    tag.add_frame(Comment {
                        lang: "eng".to_string(),
                        description: String::new(),
                        text: value.to_string(),
                    });
                }
            }
            key => {
                if let Some(value) = value {
                    tag.add_frame(ExtendedText {
                        description: key.to_string(),
                        value: value.to_string(),
                    });
                }
            }
        }
    }

    fn remove_extended_text_aliases(tag: &mut Tag, canonical_key: &str) {
        let retained = tag
            .frames()
            .filter(|frame| match frame.content() {
                Content::ExtendedText(text) => {
                    super::canonicalize_key(&text.description) != canonical_key
                }
                _ => true,
            })
            .cloned()
            .collect::<Vec<_>>();
        tag.remove("TXXX");
        tag.extend(retained.into_iter().filter(|frame| frame.id() == "TXXX"));
    }

    fn set_text(tag: &mut Tag, frame_id: &str, value: Option<&str>) {
        tag.remove(frame_id);
        if let Some(value) = value {
            tag.add_frame(id3::Frame::text(frame_id, value));
        }
    }

    fn set_number_pair(tag: &mut Tag, track_pair: bool, change: &DsfTagChange) {
        let (number, total) = if track_pair {
            (tag.track(), tag.total_tracks())
        } else {
            (tag.disc(), tag.total_discs())
        };
        let parsed = change.value.as_deref().and_then(|value| value.parse::<u32>().ok());
        let (number, total) = match change.canonical_key.as_str() {
            "TRACKNUMBER" | "DISCNUMBER" => (parsed, total),
            _ => (number, parsed),
        };
        let frame_id = if track_pair { "TRCK" } else { "TPOS" };
        tag.remove(frame_id);
        if number.is_some() || total.is_some() {
            let value = match (number, total) {
                (Some(number), Some(total)) => format!("{number}/{total}"),
                (Some(number), None) => number.to_string(),
                (None, Some(total)) => format!("/{total}"),
                (None, None) => return,
            };
            tag.add_frame(id3::Frame::text(frame_id, value));
        }
    }

    fn push(fields: &mut BTreeMap<String, Vec<String>>, key: &str, value: Option<&str>) {
        if let Some(value) = value {
            push_owned(fields, key, value.to_string());
        }
    }

    fn push_owned(fields: &mut BTreeMap<String, Vec<String>>, key: impl Into<String>, value: String) {
        if !value.trim().is_empty() {
            fields.entry(key.into()).or_default().push(value);
        }
    }
}

#[cfg(test)]
pub(crate) fn write_test_dsf_fixture(
    path: &Path,
    metadata_bytes: Option<&[u8]>,
) -> std::io::Result<()> {
    const CHANNELS: u32 = 2;
    const BLOCK_SIZE_PER_CHANNEL: u32 = 4096;
    const SAMPLE_COUNT_PER_CHANNEL: u64 = 32_768;
    let audio_bytes = vec![0u8; (CHANNELS * BLOCK_SIZE_PER_CHANNEL) as usize];
    let audio_end = 28u64 + 52u64 + 12u64 + audio_bytes.len() as u64;
    let metadata_len = metadata_bytes.map_or(0u64, |bytes| bytes.len() as u64);
    let file_size = audio_end + metadata_len;
    let metadata_offset = metadata_bytes.map_or(0u64, |_| audio_end);

    let mut bytes = Vec::with_capacity(file_size as usize);
    bytes.extend_from_slice(b"DSD ");
    bytes.extend_from_slice(&28u64.to_le_bytes());
    bytes.extend_from_slice(&file_size.to_le_bytes());
    bytes.extend_from_slice(&metadata_offset.to_le_bytes());

    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&52u64.to_le_bytes());
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&2u32.to_le_bytes());
    bytes.extend_from_slice(&CHANNELS.to_le_bytes());
    bytes.extend_from_slice(&2_822_400u32.to_le_bytes());
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(&SAMPLE_COUNT_PER_CHANNEL.to_le_bytes());
    bytes.extend_from_slice(&BLOCK_SIZE_PER_CHANNEL.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());

    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&(12u64 + audio_bytes.len() as u64).to_le_bytes());
    bytes.extend_from_slice(&audio_bytes);
    if let Some(metadata_bytes) = metadata_bytes {
        bytes.extend_from_slice(metadata_bytes);
    }
    std::fs::write(path, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use id3::TagLike;

    #[test]
    fn id3_mapping_seam_round_trips_number_pairs_comments_and_custom_text() {
        let snapshot = canonicalize_snapshot(backend::mapping_fixture_after(&[
            DsfTagChange {
                canonical_key: "TRACKTOTAL".into(),
                value: Some("12".into()),
            },
            DsfTagChange {
                canonical_key: "DISCTOTAL".into(),
                value: Some("3".into()),
            },
            DsfTagChange {
                canonical_key: "COMMENT".into(),
                value: Some("Updated comment".into()),
            },
            DsfTagChange {
                canonical_key: "CATALOGNUMBER".into(),
                value: Some("ABC-123".into()),
            },
        ]));

        assert_eq!(snapshot.first("TITLE"), Some("Original title"));
        assert_eq!(snapshot.first("TRACKNUMBER"), Some("2"));
        assert_eq!(snapshot.first("TRACKTOTAL"), Some("12"));
        assert_eq!(snapshot.first("DISCNUMBER"), Some("1"));
        assert_eq!(snapshot.first("DISCTOTAL"), Some("3"));
        assert_eq!(snapshot.first("COMMENT"), Some("Updated comment"));
        assert_eq!(snapshot.first("CATALOGNUMBER"), Some("ABC-123"));
    }

    #[test]
    fn unrelated_dsf_edit_preserves_distinct_comment_frames() {
        let snapshot = backend::multi_value_fixture_after(&[DsfTagChange {
            canonical_key: "TITLE".into(),
            value: Some("New title".into()),
        }]);
        assert_eq!(snapshot.first("TITLE"), Some("New title"));
        assert_eq!(
            snapshot.fields["COMMENT"],
            vec!["first comment".to_string(), "second comment".to_string()],
            "an unedited scalar editor row must not collapse distinct COMM frames",
        );
    }

    #[test]
    fn noncanonical_audio_boundary_is_readable_but_write_blocked() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("corrupt-boundary.dsf");
        let mut tag = id3::Tag::new();
        tag.add_frame(id3::Frame::text("TIT2", "Original title"));
        let mut metadata = Vec::new();
        tag.write_to(&mut metadata, id3::Version::Id3v24)
            .expect("serialize fixture tag");
        write_test_dsf_fixture(&path, Some(&metadata)).expect("write tagged fixture");

        let mut bytes = std::fs::read(&path).expect("read fixture bytes");
        let data_chunk_size = u64::from_le_bytes(
            bytes[84..92]
                .try_into()
                .expect("data chunk size field"),
        );
        bytes[84..92].copy_from_slice(&(data_chunk_size - 1).to_le_bytes());
        std::fs::write(&path, &bytes).expect("publish corrupt data boundary");

        let outcome = read_with_warnings(&path)
            .expect("benignly noncanonical DSF metadata must remain readable");
        assert_eq!(outcome.snapshot.first("TITLE"), Some("Original title"));
        assert_eq!(outcome.warnings.len(), 1);
        assert!(outcome.warnings[0].contains(
            "noncanonical DSF audio-chunk layout: invalid DSF chunk layout"
        ));

        let error = write_with_backup(
            &path,
            &[DsfTagChange {
                canonical_key: "TITLE".into(),
                value: Some("Replacement".into()),
            }],
        )
        .expect_err("strict DSF write validation must reject the noncanonical boundary");
        assert!(error.contains("invalid DSF chunk layout"));
        assert_eq!(std::fs::read(&path).expect("read unchanged fixture"), bytes);
    }

    #[test]
    fn declared_size_mismatch_is_readable_but_write_blocked() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("declared-size-mismatch.dsf");
        let mut tag = id3::Tag::new();
        tag.add_frame(id3::Frame::text("TIT2", "Readable title"));
        let mut metadata = Vec::new();
        tag.write_to(&mut metadata, id3::Version::Id3v24)
            .expect("serialize fixture tag");
        write_test_dsf_fixture(&path, Some(&metadata)).expect("write tagged fixture");

        let mut bytes = std::fs::read(&path).expect("read fixture bytes");
        let actual_size = bytes.len() as u64;
        bytes[12..20].copy_from_slice(&(actual_size + 17).to_le_bytes());
        std::fs::write(&path, &bytes).expect("publish declared-size mismatch");

        let outcome = read_with_warnings(&path)
            .expect("declared-size mismatch must not hide otherwise readable ID3 metadata");
        assert_eq!(outcome.snapshot.first("TITLE"), Some("Readable title"));
        assert_eq!(
            outcome.warnings,
            vec![format!(
                "declared file size {} differs from actual size {actual_size}",
                actual_size + 17
            )]
        );

        let error = write_with_backup(
            &path,
            &[DsfTagChange {
                canonical_key: "TITLE".into(),
                value: Some("Replacement".into()),
            }],
        )
        .expect_err("strict DSF write validation must reject a declared-size mismatch");
        assert_eq!(
            error,
            format!(
                "failed to save DSF ID3 tags to '{}': invalid DSF header in '{}': declared file size {} differs from actual size {actual_size}",
                path.display(),
                path.display(),
                actual_size + 17
            )
        );
        assert_eq!(std::fs::read(&path).expect("read unchanged fixture"), bytes);
    }

    #[test]
    fn real_dsf_boundary_reads_untagged_file_then_writes_and_rewrites_id3() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("album.dsf");
        write_test_dsf_fixture(&path, None).expect("write minimal untagged DSF fixture");
        let untagged_bytes = std::fs::read(&path).expect("read untagged fixture bytes");
        let audio_end = untagged_bytes.len() as u64;

        let untagged = read(&path).expect("valid untagged DSF must be readable");
        assert_eq!(untagged, DsfTagSnapshot::default());

        let first_warning = write_with_backup(
            &path,
            &[
                DsfTagChange { canonical_key: "TITLE".into(), value: Some("Boundary title".into()) },
                DsfTagChange { canonical_key: "ARTIST".into(), value: Some("Boundary artist".into()) },
                DsfTagChange { canonical_key: "ALBUM".into(), value: Some("Boundary album".into()) },
                DsfTagChange { canonical_key: "ALBUMARTIST".into(), value: Some("Boundary album artist".into()) },
                DsfTagChange { canonical_key: "TRACKNUMBER".into(), value: Some("2".into()) },
                DsfTagChange { canonical_key: "TRACKTOTAL".into(), value: Some("10".into()) },
                DsfTagChange { canonical_key: "DISCNUMBER".into(), value: Some("1".into()) },
                DsfTagChange { canonical_key: "DISCTOTAL".into(), value: Some("2".into()) },
                DsfTagChange { canonical_key: "COMMENT".into(), value: Some("Boundary comment".into()) },
                DsfTagChange { canonical_key: "CATALOGNUMBER".into(), value: Some("ABC-123".into()) },
            ],
        )
        .expect("write ID3 to real DSF fixture");
        assert_eq!(first_warning, None);

        let first_bytes = std::fs::read(&path).expect("read first tagged DSF bytes");
        assert_eq!(
            u64::from_le_bytes(first_bytes[12..20].try_into().expect("declared size field")),
            first_bytes.len() as u64
        );
        assert_eq!(
            u64::from_le_bytes(first_bytes[20..28].try_into().expect("metadata pointer field")),
            audio_end
        );
        assert_eq!(&first_bytes[..12], &untagged_bytes[..12]);
        assert_eq!(&first_bytes[28..audio_end as usize], &untagged_bytes[28..]);
        assert_eq!(&first_bytes[audio_end as usize..audio_end as usize + 3], b"ID3");

        let first = read(&path).expect("read written DSF ID3");
        assert_eq!(first.first("TITLE"), Some("Boundary title"));
        assert_eq!(first.first("ARTIST"), Some("Boundary artist"));
        assert_eq!(first.first("ALBUM"), Some("Boundary album"));
        assert_eq!(first.first("ALBUMARTIST"), Some("Boundary album artist"));
        assert_eq!(first.first("TRACKNUMBER"), Some("2"));
        assert_eq!(first.first("TRACKTOTAL"), Some("10"));
        assert_eq!(first.first("DISCNUMBER"), Some("1"));
        assert_eq!(first.first("DISCTOTAL"), Some("2"));
        assert_eq!(first.first("COMMENT"), Some("Boundary comment"));
        assert_eq!(first.first("CATALOGNUMBER"), Some("ABC-123"));

        let second_warning = write_with_backup(
            &path,
            &[
                DsfTagChange { canonical_key: "TITLE".into(), value: None },
                DsfTagChange { canonical_key: "TRACKTOTAL".into(), value: Some("12".into()) },
                DsfTagChange { canonical_key: "DISCTOTAL".into(), value: Some("3".into()) },
                DsfTagChange { canonical_key: "COMMENT".into(), value: Some("Rewritten comment".into()) },
            ],
        )
        .expect("rewrite ID3 in real DSF fixture");
        assert_eq!(second_warning, None);

        let second_bytes = std::fs::read(&path).expect("read rewritten DSF bytes");
        assert_eq!(
            u64::from_le_bytes(second_bytes[12..20].try_into().expect("rewritten size field")),
            second_bytes.len() as u64
        );
        assert_eq!(
            u64::from_le_bytes(second_bytes[20..28].try_into().expect("rewritten pointer field")),
            audio_end
        );
        assert_eq!(&second_bytes[..12], &untagged_bytes[..12]);
        assert_eq!(&second_bytes[28..audio_end as usize], &untagged_bytes[28..]);
        assert_eq!(&second_bytes[audio_end as usize..audio_end as usize + 3], b"ID3");

        let second = read(&path).expect("read rewritten DSF ID3");
        assert_eq!(second.first("TITLE"), None);
        assert_eq!(second.first("TRACKNUMBER"), Some("2"));
        assert_eq!(second.first("TRACKTOTAL"), Some("12"));
        assert_eq!(second.first("DISCNUMBER"), Some("1"));
        assert_eq!(second.first("DISCTOTAL"), Some("3"));
        assert_eq!(second.first("COMMENT"), Some("Rewritten comment"));
        assert_eq!(second.first("CATALOGNUMBER"), Some("ABC-123"));
        assert!(!crate::db::Database::backup_path_for(&path).exists());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_dsf_write_is_rejected_without_changing_link_or_target() {
        let temp = tempfile::tempdir().expect("tempdir");
        let target = temp.path().join("target.dsf");
        let link = temp.path().join("linked.dsf");
        write_test_dsf_fixture(&target, None).expect("write target fixture");
        std::os::unix::fs::symlink(&target, &link).expect("create DSF symlink");
        let original_target = std::fs::read(&target).expect("read original target");

        let error = write_with_backup(
            &link,
            &[DsfTagChange {
                canonical_key: "TITLE".into(),
                value: Some("Must not be written".into()),
            }],
        )
        .expect_err("symlinked DSF mutation must fail closed");

        assert_eq!(
            error,
            format!(
                "refusing to mutate symlinked DSF path '{}'; edit the resolved target explicitly so write authority remains unambiguous",
                link.display()
            )
        );
        assert!(
            std::fs::symlink_metadata(&link)
                .expect("link metadata")
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read_link(&link).expect("link target"), target);
        assert_eq!(
            std::fs::read(&target).expect("target remains readable"),
            original_target
        );
        assert!(!crate::db::Database::backup_path_for(&link).exists());
    }

    #[test]
    fn tagged_dsf_with_unowned_trailing_bytes_is_not_rewritten() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("tag-with-tail.dsf");
        let mut tag = id3::Tag::new();
        tag.add_frame(id3::Frame::text("TIT2", "Original title"));
        let mut metadata = Vec::new();
        tag.write_to(&mut metadata, id3::Version::Id3v24)
            .expect("serialize fixture tag");
        metadata.extend_from_slice(b"TAIL");
        write_test_dsf_fixture(&path, Some(&metadata)).expect("write tagged DSF with tail");
        let original = std::fs::read(&path).expect("read original fixture");

        let error = write_with_backup(
            &path,
            &[DsfTagChange {
                canonical_key: "TITLE".into(),
                value: Some("Replacement title".into()),
            }],
        )
        .expect_err("ambiguous trailing bytes must block replacement");

        assert_eq!(
            error,
            format!(
                "refusing to replace DSF ID3 metadata in '{}': 4 trailing byte(s) follow the declared tag, so their ownership is ambiguous",
                path.display()
            )
        );
        assert_eq!(std::fs::read(&path).expect("read unchanged fixture"), original);
        assert!(!crate::db::Database::backup_path_for(&path).exists());
    }

    #[test]
    fn nonzero_metadata_pointer_without_id3_marker_is_readable_but_write_blocked() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("corrupt-metadata.dsf");
        write_test_dsf_fixture(&path, Some(b"NOT-AN-ID3-TAG"))
            .expect("write DSF with corrupt metadata area");
        let original = std::fs::read(&path).expect("read original fixture");

        let outcome = read_with_warnings(&path)
            .expect("invalid ID3 marker must degrade metadata without blocking audio reads");
        assert!(outcome.snapshot.fields.is_empty());
        assert_eq!(
            outcome.warnings,
            vec!["metadata pointer 8284 does not identify an ID3 marker; metadata was ignored".to_string()]
        );

        let error = write_with_backup(
            &path,
            &[DsfTagChange {
                canonical_key: "TITLE".into(),
                value: Some("Replacement".into()),
            }],
        )
        .expect_err("strict writes must reject an invalid metadata marker");
        assert_eq!(
            error,
            format!(
                "failed to save DSF ID3 tags to '{0}': invalid DSF metadata area in '{0}': header declares metadata at offset 8284, but no ID3 marker is present",
                path.display()
            )
        );
        assert_eq!(std::fs::read(&path).expect("read unchanged fixture"), original);
    }

    #[test]
    fn canonicalization_merges_id3_aliases_and_prefers_canonical_identity() {
        let mut raw = DsfTagSnapshot::default();
        raw.fields.insert("TOTALTRACKS".into(), vec!["9".into()]);
        raw.fields.insert("TRACKTOTAL".into(), vec!["10".into()]);
        raw.fields.insert("DESCRIPTION".into(), vec!["legacy".into()]);
        raw.fields.insert("COMMENT".into(), vec!["canonical".into()]);
        let snapshot = canonicalize_snapshot(raw);
        assert_eq!(snapshot.fields["TRACKTOTAL"], vec!["10", "9"]);
        assert_eq!(snapshot.fields["COMMENT"], vec!["canonical", "legacy"]);
    }

    #[test]
    fn conflicting_same_group_changes_fail_before_io() {
        let error = resolve_changes(&[
            DsfTagChange { canonical_key: "COMMENT".into(), value: Some("a".into()) },
            DsfTagChange { canonical_key: "DESCRIPTION".into(), value: Some("b".into()) },
        ])
        .expect_err("conflict must fail closed");
        assert!(error.contains("canonical key COMMENT"));
    }

    #[test]
    fn invalid_numeric_changes_fail_before_backup_or_tag_io() {
        let invalid_number = resolve_changes(&[DsfTagChange {
            canonical_key: "TRACKNUMBER".into(),
            value: Some("side-a".into()),
        }])
        .expect_err("non-numeric track numbers must fail closed");
        assert_eq!(
            invalid_number,
            "invalid DSF metadata value for TRACKNUMBER: expected an unsigned integer, got \"side-a\""
        );

        let zero_total = resolve_changes(&[DsfTagChange {
            canonical_key: "TOTALDISCS".into(),
            value: Some("0".into()),
        }])
        .expect_err("zero totals must fail closed");
        assert_eq!(
            zero_total,
            "invalid DSF metadata value for DISCTOTAL: totals must be greater than zero"
        );

        let non_numeric_bpm = resolve_changes(&[DsfTagChange {
            canonical_key: "BPM".into(),
            value: Some("fast".into()),
        }])
        .expect_err("non-numeric BPM must fail closed");
        assert_eq!(
            non_numeric_bpm,
            r#"invalid DSF metadata value for BPM: expected an unsigned integer, got "fast""#
        );

        let zero_bpm = resolve_changes(&[DsfTagChange {
            canonical_key: "BPM".into(),
            value: Some("0".into()),
        }])
        .expect_err("zero BPM must fail closed");
        assert_eq!(
            zero_bpm,
            "invalid DSF metadata value for BPM: tempo must be greater than zero"
        );
    }

    #[test]
    fn bounded_tail_write_reuses_allocation_and_reports_byte_progress() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("album.dsf");
        let mut tag = id3::Tag::new();
        tag.add_frame(id3::Frame::text("TIT2", "x".repeat(16 * 1024)));
        let mut metadata = Vec::new();
        tag.write_to(&mut metadata, id3::Version::Id3v24)
            .expect("serialize allocated fixture tag");
        write_test_dsf_fixture(&path, Some(&metadata)).expect("write tagged fixture");
        let before = std::fs::read(&path).expect("read original fixture");
        let metadata_offset = u64::from_le_bytes(
            before[20..28].try_into().expect("metadata pointer"),
        ) as usize;
        let progress = std::sync::Mutex::new(Vec::new());

        let warning = write_with_control(
            &path,
            &[DsfTagChange {
                canonical_key: "TITLE".into(),
                value: Some("Short replacement".into()),
            }],
            &|| false,
            &|update| progress.lock().expect("progress lock").push(update),
        )
        .expect("bounded tail rewrite");

        assert_eq!(warning, None);
        let after = std::fs::read(&path).expect("read updated fixture");
        assert_eq!(after.len(), before.len());
        assert_eq!(&after[..metadata_offset], &before[..metadata_offset]);
        assert_eq!(read(&path).expect("read updated tags").first("TITLE"), Some("Short replacement"));
        let phases = progress
            .into_inner()
            .expect("progress values")
            .into_iter()
            .map(|update| update.phase)
            .collect::<Vec<_>>();
        assert!(phases.contains(&DsfWriteProgressPhase::Journaling));
        assert!(phases.contains(&DsfWriteProgressPhase::WritingTail));
        assert!(phases.contains(&DsfWriteProgressPhase::Publishing));
        assert!(!phases.contains(&DsfWriteProgressPhase::CopyingPrefix));
        assert!(!tail_journal_path(&path).exists());
        assert!(!crate::db::Database::backup_path_for(&path).exists());
        assert!(
            std::fs::read_dir(temp.path())
                .expect("read tempdir")
                .flatten()
                .all(|entry| !is_dsf_temp_name(&entry.file_name().to_string_lossy()))
        );
    }

    #[test]
    fn cancelling_bounded_tail_write_restores_exact_original_bytes() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("album.dsf");
        let mut tag = id3::Tag::new();
        tag.add_frame(id3::Frame::text("TIT2", "x".repeat(2 * 1024 * 1024)));
        let mut metadata = Vec::new();
        tag.write_to(&mut metadata, id3::Version::Id3v24)
            .expect("serialize large fixture tag");
        write_test_dsf_fixture(&path, Some(&metadata)).expect("write tagged fixture");
        let original = std::fs::read(&path).expect("read original fixture");
        let cancelled = std::sync::atomic::AtomicBool::new(false);

        let error = write_with_control(
            &path,
            &[DsfTagChange {
                canonical_key: "TITLE".into(),
                value: Some("Replacement".into()),
            }],
            &|| cancelled.load(std::sync::atomic::Ordering::SeqCst),
            &|update| {
                if update.phase == DsfWriteProgressPhase::WritingTail
                    && update.bytes_done >= COPY_CHUNK_BYTES as u64
                {
                    cancelled.store(true, std::sync::atomic::Ordering::SeqCst);
                }
            },
        )
        .expect_err("mid-tail cancellation must abort");

        assert!(error.contains("metadata save cancelled while writing DSF metadata tail"));
        assert!(error.contains("original tail was restored"));
        assert_eq!(std::fs::read(&path).expect("read restored fixture"), original);
        assert!(!tail_journal_path(&path).exists());
        assert!(!crate::db::Database::backup_path_for(&path).exists());
    }

    #[test]
    fn cancellation_after_final_tail_progress_rolls_back_before_commit() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("album.dsf");
        let mut tag = id3::Tag::new();
        tag.add_frame(id3::Frame::text("TIT2", "x".repeat(1024)));
        let mut metadata = Vec::new();
        tag.write_to(&mut metadata, id3::Version::Id3v24)
            .expect("serialize fixture tag");
        write_test_dsf_fixture(&path, Some(&metadata)).expect("write tagged fixture");
        let original = std::fs::read(&path).expect("read original fixture");
        let cancelled = std::sync::atomic::AtomicBool::new(false);

        let error = write_with_control(
            &path,
            &[DsfTagChange {
                canonical_key: "TITLE".into(),
                value: Some("Replacement".into()),
            }],
            &|| cancelled.load(std::sync::atomic::Ordering::SeqCst),
            &|update| {
                if update.phase == DsfWriteProgressPhase::WritingTail
                    && update.bytes_done == update.bytes_total
                {
                    cancelled.store(true, std::sync::atomic::Ordering::SeqCst);
                }
            },
        )
        .expect_err("cancellation after the final copy chunk must still roll back");

        assert_eq!(
            error,
            format!(
                "metadata save cancelled while writing DSF metadata tail for '{}' and the original tail was restored",
                path.display()
            )
        );
        assert_eq!(std::fs::read(&path).expect("read restored fixture"), original);
        assert!(!tail_journal_path(&path).exists());
    }

    #[test]
    fn commit_state_sync_failure_retains_journal_without_attempting_rollback() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("album.dsf");
        let mut tag = id3::Tag::new();
        tag.add_frame(id3::Frame::text("TIT2", "x".repeat(1024)));
        let mut metadata = Vec::new();
        tag.write_to(&mut metadata, id3::Version::Id3v24)
            .expect("serialize fixture tag");
        write_test_dsf_fixture(&path, Some(&metadata)).expect("write tagged fixture");
        let original = std::fs::read(&path).expect("read original fixture");
        TEST_FAIL_TAIL_JOURNAL_COMMIT_SYNC.with(|flag| flag.set(true));
        let progress = std::sync::Mutex::new(Vec::new());

        let warning = write_with_control(
            &path,
            &[DsfTagChange {
                canonical_key: "TITLE".into(),
                value: Some("Replacement".into()),
            }],
            &|| false,
            &|update| progress.lock().expect("progress lock").push(update.phase),
        )
        .expect("commit-state durability uncertainty preserves the atomically written target");
        TEST_FAIL_TAIL_JOURNAL_COMMIT_SYNC.with(|flag| flag.set(false));

        assert!(
            !progress
                .lock()
                .expect("progress lock")
                .iter()
                .any(|phase| *phase == DsfWriteProgressPhase::Recovering),
            "durability uncertainty must never initiate rollback from an uncertain state byte",
        );
        assert!(warning.as_deref().is_some_and(|warning| {
            warning.contains("journal commit durability is uncertain")
                && warning.contains("recovery will keep the new tail if COMMITTED is durable")
        }));
        let committed = std::fs::read(&path).expect("read committed target");
        assert_ne!(committed, original);
        let journal = tail_journal_path(&path);
        assert!(journal.exists(), "uncertain journal authority must be retained");
        let journal_bytes = std::fs::read(&journal).expect("read retained journal");
        assert_eq!(journal_bytes[TAIL_JOURNAL_STATE_OFFSET as usize], TAIL_JOURNAL_COMMITTED);

        assert!(recover_tail_journal(&path).expect("resolve retained journal"));
        assert_eq!(std::fs::read(&path).expect("read recovered target"), committed);
        assert!(!journal.exists());
    }

    #[test]
    fn cancelling_full_rewrite_preserves_original_and_removes_temp() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("untagged.dsf");
        write_test_dsf_fixture(&path, None).expect("write untagged fixture");
        let original = std::fs::read(&path).expect("read original fixture");
        let cancelled = std::sync::atomic::AtomicBool::new(false);

        let error = write_with_control(
            &path,
            &[DsfTagChange {
                canonical_key: "TITLE".into(),
                value: Some("x".repeat(2 * 1024 * 1024)),
            }],
            &|| cancelled.load(std::sync::atomic::Ordering::SeqCst),
            &|update| {
                if update.phase == DsfWriteProgressPhase::CopyingPrefix
                    && update.bytes_done > 0
                {
                    cancelled.store(true, std::sync::atomic::Ordering::SeqCst);
                }
            },
        )
        .expect_err("mid-rewrite cancellation must abort");

        assert_eq!(error, "metadata save cancelled during DSF tag copy");
        assert_eq!(std::fs::read(&path).expect("read unchanged fixture"), original);
        assert!(
            std::fs::read_dir(temp.path())
                .expect("read tempdir")
                .flatten()
                .all(|entry| !is_dsf_temp_name(&entry.file_name().to_string_lossy()))
        );
    }

    #[test]
    fn prepared_tail_journal_restores_and_committed_journal_only_cleans_up() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("album.dsf");
        let mut tag = id3::Tag::new();
        tag.add_frame(id3::Frame::text("TIT2", "Original"));
        let mut metadata = Vec::new();
        tag.write_to(&mut metadata, id3::Version::Id3v24)
            .expect("serialize fixture tag");
        write_test_dsf_fixture(&path, Some(&metadata)).expect("write tagged fixture");
        let original = std::fs::read(&path).expect("read original fixture");
        let offset = u64::from_le_bytes(original[20..28].try_into().expect("metadata pointer"));
        let original_tail = original[offset as usize..].to_vec();
        let mut identity_file = std::fs::File::open(&path).expect("open identity fixture");
        let recovery_identity = dsf_recovery_identity(
            &mut identity_file,
            offset,
            original.len() as u64,
        )
        .expect("compute recovery identity");
        let journal = tail_journal_path(&path);

        let mut torn = original.clone();
        torn[offset as usize..].fill(0xa5);
        std::fs::write(&path, &torn).expect("write torn target");
        std::fs::write(
            &journal,
            encode_tail_journal(
                offset,
                &original_tail,
                &recovery_identity,
                TAIL_JOURNAL_PREPARED,
            ),
        )
        .expect("write prepared journal");
        assert!(recover_tail_journal(&path).expect("recover prepared journal"));
        assert_eq!(std::fs::read(&path).expect("read recovered target"), original);
        assert!(!journal.exists());

        let mut committed = original.clone();
        committed[offset as usize..].fill(0x5a);
        std::fs::write(&path, &committed).expect("write committed target");
        std::fs::write(
            &journal,
            encode_tail_journal(
                offset,
                &original_tail,
                &recovery_identity,
                TAIL_JOURNAL_COMMITTED,
            ),
        )
        .expect("write committed journal");
        assert!(recover_tail_journal(&path).expect("clean committed journal"));
        assert_eq!(std::fs::read(&path).expect("read committed target"), committed);
        assert!(!journal.exists());
    }

    #[test]
    fn tail_journal_recovery_refuses_same_sized_different_target_and_retains_authority() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("album.dsf");
        let mut tag = id3::Tag::new();
        tag.add_frame(id3::Frame::text("TIT2", "Original"));
        let mut metadata = Vec::new();
        tag.write_to(&mut metadata, id3::Version::Id3v24)
            .expect("serialize fixture tag");
        write_test_dsf_fixture(&path, Some(&metadata)).expect("write tagged fixture");
        let original = std::fs::read(&path).expect("read original fixture");
        let offset = u64::from_le_bytes(original[20..28].try_into().expect("metadata pointer"));
        let original_tail = original[offset as usize..].to_vec();
        let mut identity_file = std::fs::File::open(&path).expect("open identity fixture");
        let recovery_identity = dsf_recovery_identity(
            &mut identity_file,
            offset,
            original.len() as u64,
        )
        .expect("compute recovery identity");
        let journal = tail_journal_path(&path);
        std::fs::write(
            &journal,
            encode_tail_journal(
                offset,
                &original_tail,
                &recovery_identity,
                TAIL_JOURNAL_PREPARED,
            ),
        )
        .expect("write prepared journal");

        let mut replacement = original.clone();
        replacement[0] ^= 0xff;
        std::fs::write(&path, &replacement).expect("replace target with same-sized file");

        let error = recover_tail_journal(&path)
            .expect_err("journal must not restore bytes into a different same-sized target");

        assert_eq!(
            error,
            format!(
                "refusing DSF tail-journal recovery for '{}': the bounded audio-prefix identity no longer matches the journal authority",
                path.display()
            )
        );
        assert_eq!(
            std::fs::read(&path).expect("read replacement target"),
            replacement
        );
        assert!(journal.exists(), "unconsumed recovery authority must be retained");
    }

    #[test]
    fn directory_recovery_refuses_unversioned_legacy_backup_and_removes_orphan_temp() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("album.dsf");
        let backup = crate::db::Database::backup_path_for(&path);
        let orphan = temp
            .path()
            .join(format!(".album.dsf.tonepoet-id3-{}.tmp", uuid::Uuid::new_v4()));
        let journal_temp = temp.path().join(format!(
            ".album.dsf.tonepoet-dsf-tail.journal.{}.tmp",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&path, b"new bytes").expect("write target");
        std::fs::write(&backup, b"old bytes").expect("write legacy backup");
        std::fs::write(&orphan, b"temporary bytes").expect("write orphan temp");
        std::fs::write(&journal_temp, b"partial journal bytes")
            .expect("write orphan journal temp");

        let messages = recover_stale_writes_in_directory(temp.path());

        assert_eq!(std::fs::read(&path).expect("read untouched target"), b"new bytes");
        assert!(backup.exists(), "unbound rollback authority must remain for explicit handling");
        assert!(!orphan.exists());
        assert!(!journal_temp.exists());
        assert!(messages.iter().any(|message| {
            message
                == &format!(
                    "Legacy DSF rollback marker '{}' differs from '{}'; inspect with `tonepoet dsf-recover status '{}'`, then choose `restore-backup` or `keep-current`",
                    backup.display(),
                    path.display(),
                    path.display()
                )
        }));
        assert!(messages.iter().any(|message| {
            message == &format!("Removed stale DSF rewrite temp {}", orphan.display())
        }));
        assert!(messages.iter().any(|message| {
            message == &format!("Removed stale DSF tail-journal temp {}", journal_temp.display())
        }));
    }

    #[test]
    fn prewrite_refuses_legacy_marker_before_new_generation_can_commit() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("album.dsf");
        let mut tag = id3::Tag::new();
        tag.add_frame(id3::Frame::text("TIT2", "Current generation"));
        let mut metadata = Vec::new();
        tag.write_to(&mut metadata, id3::Version::Id3v24)
            .expect("serialize current tag");
        write_test_dsf_fixture(&path, Some(&metadata)).expect("write current fixture");
        let current = std::fs::read(&path).expect("read current generation");
        let backup = crate::db::Database::backup_path_for(&path);
        std::fs::write(&backup, b"unbound older generation").expect("write legacy marker");

        let error = write_with_backup(
            &path,
            &[DsfTagChange {
                canonical_key: "TITLE".into(),
                value: Some("New title".into()),
            }],
        )
        .expect_err("legacy marker must block a later save");

        assert_eq!(
            error,
            format!(
                "refusing to save DSF metadata for '{}': unversioned legacy rollback marker '{}' differs from the current file. Inspect it with `tonepoet dsf-recover status '{}'`, then choose `restore-backup` or `keep-current`; tonepoet will not guess which generation is authoritative",
                path.display(),
                backup.display(),
                path.display()
            )
        );
        assert_eq!(std::fs::read(&path).expect("read blocked target"), current);
        assert_eq!(
            std::fs::read(&backup).expect("read retained marker"),
            b"unbound older generation"
        );
    }

    #[test]
    fn prewrite_handles_tail_journal_and_orphan_temp_before_refusing_legacy_marker() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("album.dsf");
        let mut tag = id3::Tag::new();
        tag.add_frame(id3::Frame::text("TIT2", "Original"));
        let mut metadata = Vec::new();
        tag.write_to(&mut metadata, id3::Version::Id3v24)
            .expect("serialize fixture tag");
        write_test_dsf_fixture(&path, Some(&metadata)).expect("write tagged fixture");
        let original = std::fs::read(&path).expect("read original fixture");
        let offset = u64::from_le_bytes(original[20..28].try_into().expect("metadata pointer"));
        let original_tail = original[offset as usize..].to_vec();
        let mut identity_file = std::fs::File::open(&path).expect("open identity fixture");
        let recovery_identity = dsf_recovery_identity(
            &mut identity_file,
            offset,
            original.len() as u64,
        )
        .expect("compute recovery identity");
        let journal = tail_journal_path(&path);
        std::fs::write(
            &journal,
            encode_tail_journal(
                offset,
                &original_tail,
                &recovery_identity,
                TAIL_JOURNAL_PREPARED,
            ),
        )
        .expect("write prepared journal");
        let mut torn = original.clone();
        torn[offset as usize..].fill(0xa5);
        std::fs::write(&path, torn).expect("write torn target");
        let orphan = temp
            .path()
            .join(format!(".album.dsf.tonepoet-id3-{}.tmp", uuid::Uuid::new_v4()));
        std::fs::write(&orphan, b"orphan rewrite bytes").expect("write orphan temp");
        let journal_temp = temp.path().join(format!(
            ".album.dsf.tonepoet-dsf-tail.journal.{}.tmp",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&journal_temp, b"partial journal bytes")
            .expect("write orphan journal temp");
        let backup = crate::db::Database::backup_path_for(&path);
        std::fs::write(&backup, b"unbound legacy bytes").expect("write legacy marker");

        let error = write_with_backup(
            &path,
            &[DsfTagChange {
                canonical_key: "TITLE".into(),
                value: Some("Replacement".into()),
            }],
        )
        .expect_err("legacy marker must remain the terminal preflight refusal");

        assert_eq!(
            error,
            format!(
                "refusing to save DSF metadata for '{}': unversioned legacy rollback marker '{}' differs from the current file. Inspect it with `tonepoet dsf-recover status '{}'`, then choose `restore-backup` or `keep-current`; tonepoet will not guess which generation is authoritative",
                path.display(),
                backup.display(),
                path.display()
            )
        );
        assert_eq!(std::fs::read(&path).expect("read recovered target"), original);
        assert!(!journal.exists(), "generation-bound tail journal is consumed");
        assert!(!orphan.exists(), "target-matched orphan temp is retired");
        assert!(!journal_temp.exists(), "unpublished journal temp is retired");
        assert!(backup.exists(), "unversioned marker remains for explicit handling");
    }

    #[test]
    fn prewrite_retires_byte_identical_legacy_marker_and_continues() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("album.dsf");
        write_test_dsf_fixture(&path, None).expect("write fixture");
        let backup = crate::db::Database::backup_path_for(&path);
        std::fs::copy(&path, &backup).expect("copy identical marker");

        write_with_backup(
            &path,
            &[DsfTagChange {
                canonical_key: "TITLE".into(),
                value: Some("Saved after retirement".into()),
            }],
        )
        .expect("byte-identical marker is safely retired");

        assert!(!backup.exists());
        assert_eq!(
            read(&path).expect("read saved tag").first("TITLE"),
            Some("Saved after retirement")
        );
    }

    #[test]
    fn explicit_legacy_resolution_supports_keep_current_and_restore_backup() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("album.dsf");
        std::fs::write(&path, b"current generation").expect("write current");
        let backup = crate::db::Database::backup_path_for(&path);
        std::fs::write(&backup, b"older generation").expect("write backup");

        let inspection = inspect_legacy_backup(&path).expect("inspect marker");
        assert_eq!(inspection.target_bytes, 18);
        assert_eq!(inspection.marker_bytes, 16);
        assert!(!inspection.byte_identical);
        resolve_legacy_backup(&path, DsfLegacyBackupResolution::KeepCurrent)
            .expect("keep current");
        assert_eq!(std::fs::read(&path).expect("read current"), b"current generation");
        assert!(!backup.exists());

        std::fs::write(&backup, b"restored generation").expect("write replacement backup");
        resolve_legacy_backup(&path, DsfLegacyBackupResolution::RestoreBackup)
            .expect("restore backup");
        assert_eq!(std::fs::read(&path).expect("read restored"), b"restored generation");
        assert!(!backup.exists());
    }

    #[test]
    fn prewrite_removes_target_matched_orphan_temp_before_saving() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("album.dsf");
        let mut tag = id3::Tag::new();
        tag.add_frame(id3::Frame::text("TIT2", "Original"));
        let mut metadata = Vec::new();
        tag.write_to(&mut metadata, id3::Version::Id3v24)
            .expect("serialize fixture tag");
        write_test_dsf_fixture(&path, Some(&metadata)).expect("write tagged fixture");
        let orphan = temp
            .path()
            .join(format!(".album.dsf.tonepoet-id3-{}.tmp", uuid::Uuid::new_v4()));
        std::fs::write(&orphan, b"orphan rewrite bytes").expect("write orphan temp");

        write_with_backup(
            &path,
            &[DsfTagChange {
                canonical_key: "TITLE".into(),
                value: Some("Replacement".into()),
            }],
        )
        .expect("save after orphan cleanup");

        assert!(!orphan.exists());
        assert_eq!(
            read(&path)
                .expect("read saved DSF")
                .first("TITLE"),
            Some("Replacement")
        );
    }

    #[test]
    fn cancelling_tail_journal_copy_reports_byte_progress_and_preserves_target() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("album.dsf");
        let mut tag = id3::Tag::new();
        tag.add_frame(id3::Frame::text("TIT2", "x".repeat(3 * 1024 * 1024)));
        let mut metadata = Vec::new();
        tag.write_to(&mut metadata, id3::Version::Id3v24)
            .expect("serialize large fixture tag");
        write_test_dsf_fixture(&path, Some(&metadata)).expect("write large tagged fixture");
        let original = std::fs::read(&path).expect("read original fixture");
        let cancelled = std::sync::atomic::AtomicBool::new(false);
        let updates = std::sync::Mutex::new(Vec::new());

        let error = write_with_control(
            &path,
            &[DsfTagChange {
                canonical_key: "TITLE".into(),
                value: Some("Replacement".into()),
            }],
            &|| cancelled.load(std::sync::atomic::Ordering::SeqCst),
            &|update| {
                updates.lock().expect("progress lock").push(update);
                if update.phase == DsfWriteProgressPhase::Journaling
                    && update.bytes_done >= COPY_CHUNK_BYTES as u64
                {
                    cancelled.store(true, std::sync::atomic::Ordering::SeqCst);
                }
            },
        )
        .expect_err("journal-copy cancellation must abort before target mutation");

        assert_eq!(error, "metadata save cancelled during DSF tail journal copy");
        assert_eq!(std::fs::read(&path).expect("read unchanged target"), original);
        assert!(!tail_journal_path(&path).exists());
        let progress = updates.into_inner().expect("progress updates");
        assert!(progress.iter().any(|update| {
            update.phase == DsfWriteProgressPhase::Journaling
                && update.bytes_done >= COPY_CHUNK_BYTES as u64
                && update.bytes_total > update.bytes_done
        }));
        assert!(std::fs::read_dir(temp.path())
            .expect("read tempdir")
            .flatten()
            .all(|entry| !entry.file_name().to_string_lossy().contains("tonepoet-dsf-tail.journal")));
    }
}
