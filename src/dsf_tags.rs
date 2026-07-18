//! DSF ID3v2 metadata support.
//!
//! All direct `id3` crate calls are isolated in `backend`. The crate supplies
//! generic ID3 stream parsing and serialization but no DSF container adapter,
//! so this module validates the DSF metadata pointer and owns the crash-safe
//! container mutation paths itself.

use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DsfTagSnapshot {
    /// Canonical editor display key -> ordered, distinct values.
    pub fields: BTreeMap<String, Vec<String>>,
    /// Canonical editor display key -> number of stored source frames/items.
    /// This can exceed `fields[key].len()` when duplicate frames carry the
    /// same scalar text; the editor needs the carrier count to warn before a
    /// scalar replacement collapses those frames.
    pub stored_value_counts: BTreeMap<String, usize>,
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

    pub fn stored_value_count(&self, key: &str) -> usize {
        self.stored_value_counts
            .get(key)
            .copied()
            .or_else(|| self.fields.get(key).map(Vec::len))
            .unwrap_or(0)
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DsfTailJournalInspection {
    pub target: PathBuf,
    pub journal: PathBuf,
    pub state: &'static str,
    pub operation: &'static str,
    pub original_file_size: u64,
    pub committed_file_size: u64,
}

const COPY_CHUNK_BYTES: usize = 1024 * 1024;
const REWRITE_PADDING_BYTES: u64 = 1024 * 1024;
const MAX_IN_PLACE_TAIL_BYTES: u64 = 64 * 1024 * 1024;
const TAIL_JOURNAL_MAGIC_V2: &[u8; 8] = b"TPDSFJ02";
const TAIL_JOURNAL_MAGIC_V3: &[u8; 8] = b"TPDSFJ03";
const TAIL_JOURNAL_PREPARED: u8 = 0;
const TAIL_JOURNAL_COMMITTED: u8 = 1;
const TAIL_JOURNAL_KIND_REPLACE_TAIL: u8 = 0;
const TAIL_JOURNAL_KIND_APPEND_TAG: u8 = 1;
const TAIL_JOURNAL_IDENTITY_LEN: usize = 32;
const TAIL_JOURNAL_V2_HEADER_LEN: usize = 8 + 1 + 8 + 8 + TAIL_JOURNAL_IDENTITY_LEN;
const TAIL_JOURNAL_V3_HEADER_LEN: usize =
    8 + 1 + 1 + 8 + 8 + 8 + 8 + TAIL_JOURNAL_IDENTITY_LEN;
const RECOVERY_IDENTITY_SAMPLE_BYTES: usize = 64 * 1024;
const TAIL_JOURNAL_STATE_OFFSET: u64 = 8;
const DSF_HEADER_PATCH_OFFSET: u64 = 12;
const DSF_HEADER_PATCH_LEN: u64 = 16;

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
            crate::convert::pipeline::insert_source_text_tag(&mut extra, key, value);
        }
    }
    let pre_emphasis =
        crate::convert::pipeline::source_text_tags_indicate_pre_emphasis(&extra);
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
        pre_emphasis,
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

#[derive(Debug, Clone)]
pub struct DsfArtworkSnapshot {
    original_location: DsfMetadataLocation,
    original_encoded_tag: Vec<u8>,
    original_header_patch: Option<[u8; DSF_HEADER_PATCH_LEN as usize]>,
}

/// Replace one APIC picture type through the same journaled DSF tail writer
/// used for text metadata. The returned snapshot supports batch rollback.
pub fn write_artwork_with_control(
    path: &Path,
    picture_type_code: u8,
    mime_type: &str,
    image_bytes: &[u8],
    is_cancelled: &dyn Fn() -> bool,
    progress: &dyn Fn(DsfWriteProgress),
) -> Result<(DsfArtworkSnapshot, Option<String>), String> {
    progress(DsfWriteProgress {
        phase: DsfWriteProgressPhase::Preparing,
        bytes_done: 0,
        bytes_total: 0,
    });
    if is_cancelled() {
        return Err("metadata save cancelled before preparing DSF artwork".to_string());
    }
    reject_symlinked_write_path(path)?;
    let (_write_lock, target_path) = acquire_dsf_write_lock(path)?;
    let path = target_path.as_path();
    preflight_dsf_write_artifacts(path)?;
    let (snapshot, encoded) = backend::prepare_artwork_replace(
        path,
        picture_type_code,
        mime_type,
        image_bytes,
    )?;
    let warning = write_prepared(path, snapshot.original_location, &encoded, is_cancelled, progress)?;
    Ok((snapshot, warning))
}

/// Remove one APIC picture type through the journaled DSF tail writer.
/// `Ok(None)` means the file had no matching picture and was not changed.
pub fn remove_artwork_with_control(
    path: &Path,
    picture_type_code: u8,
    is_cancelled: &dyn Fn() -> bool,
    progress: &dyn Fn(DsfWriteProgress),
) -> Result<Option<(DsfArtworkSnapshot, Option<String>)>, String> {
    progress(DsfWriteProgress {
        phase: DsfWriteProgressPhase::Preparing,
        bytes_done: 0,
        bytes_total: 0,
    });
    if is_cancelled() {
        return Err("metadata save cancelled before preparing DSF artwork removal".to_string());
    }
    reject_symlinked_write_path(path)?;
    let (_write_lock, target_path) = acquire_dsf_write_lock(path)?;
    let path = target_path.as_path();
    preflight_dsf_write_artifacts(path)?;
    let Some((snapshot, encoded)) = backend::prepare_artwork_remove(path, picture_type_code)? else {
        return Ok(None);
    };
    let warning = write_prepared(path, snapshot.original_location, &encoded, is_cancelled, progress)?;
    Ok(Some((snapshot, warning)))
}

/// Restore the exact pre-artwork metadata state through the journaled tail writer.
/// Existing ID3 tails are restored in place. An originally untagged DSF is
/// returned byte-for-byte to its original header and length through a PREPARED
/// append journal, so batch rollback never leaves a synthetic empty tag behind.
pub fn restore_artwork_snapshot(
    path: &Path,
    snapshot: &DsfArtworkSnapshot,
) -> Result<Option<String>, String> {
    reject_symlinked_write_path(path)?;
    let (_write_lock, target_path) = acquire_dsf_write_lock(path)?;
    let path = target_path.as_path();
    preflight_dsf_write_artifacts(path)?;
    match snapshot.original_location {
        DsfMetadataLocation::Id3 { .. } => {
            let current = inspect_dsf_metadata_location(path)?;
            write_prepared(
                path,
                current,
                &snapshot.original_encoded_tag,
                &|| false,
                &|_| {},
            )
        }
        DsfMetadataLocation::Untagged { file_size } => {
            let original_header_patch = snapshot.original_header_patch.ok_or_else(|| {
                "DSF artwork rollback snapshot is missing its original untagged header patch"
                    .to_string()
            })?;
            restore_untagged_artwork_snapshot(path, file_size, &original_header_patch)
        }
    }
}

pub fn write_with_backup(path: &Path, changes: &[DsfTagChange]) -> Result<Option<String>, String> {
    write_with_control(path, changes, &|| false, &|_| {})
}

/// Save DSF metadata with cooperative cancellation and byte-level progress.
///
/// The historical name `write_with_backup` remains as the compatibility
/// wrapper, but ordinary DSF writes no longer allocate a full-file backup.
/// Existing-tail writes use a bounded, durable tail journal. First tags append
/// under a header-patch journal. Only metadata growth beyond the available
/// tail allocation uses same-directory temp+rename.
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

fn read_dsf_header_patch(
    path: &Path,
) -> Result<[u8; DSF_HEADER_PATCH_LEN as usize], String> {
    let mut file = std::fs::File::open(path)
        .map_err(|error| format!("open DSF header source '{}': {error}", path.display()))?;
    file.seek(SeekFrom::Start(DSF_HEADER_PATCH_OFFSET))
        .map_err(|error| format!("seek DSF header patch '{}': {error}", path.display()))?;
    let mut patch = [0u8; DSF_HEADER_PATCH_LEN as usize];
    file.read_exact(&mut patch)
        .map_err(|error| format!("read DSF header patch '{}': {error}", path.display()))?;
    Ok(patch)
}

fn write_prepared(
    path: &Path,
    location: DsfMetadataLocation,
    encoded_tag: &[u8],
    is_cancelled: &dyn Fn() -> bool,
    progress: &dyn Fn(DsfWriteProgress),
) -> Result<Option<String>, String> {
    match location {
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
            let allocation = file_size - offset;
            if encoded_tag.len() as u64 <= allocation && allocation <= MAX_IN_PLACE_TAIL_BYTES {
                let padded = pad_id3_to_allocation(encoded_tag, allocation)?;
                return write_tail_in_place(path, offset, &padded, is_cancelled, progress);
            }
            let padded = pad_id3_with_rewrite_reserve(encoded_tag)?;
            rewrite_container(path, location, &padded, is_cancelled, progress)
        }
        DsfMetadataLocation::Untagged { file_size } => {
            let padded = pad_id3_with_rewrite_reserve(encoded_tag)?;
            append_tag_in_place(path, file_size, &padded, is_cancelled, progress)
        }
    }
}

fn pad_id3_with_rewrite_reserve(encoded: &[u8]) -> Result<Vec<u8>, String> {
    let allocation = (encoded.len() as u64)
        .checked_add(REWRITE_PADDING_BYTES)
        .ok_or_else(|| "DSF ID3 rewrite allocation overflows".to_string())?;
    pad_id3_to_allocation(encoded, allocation)
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
        .unwrap_or_else(|| std::ffi::OsStr::new("audio.dsf"));
    let digest = crate::config::native_os_str_sha256_hex(
        b"tonepoet-dsf-tail-journal-path-v1\0",
        name,
    );
    path.with_file_name(format!(".tonepoet-dsf-tail-{digest}.journal"))
}

fn legacy_tail_journal_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("audio.dsf");
    path.with_file_name(format!(".{name}.tonepoet-dsf-tail.journal"))
}

pub(crate) fn write_authority_paths(path: &Path) -> Result<Vec<PathBuf>, String> {
    let lock = crate::config::store_lock_authority_path(path).map_err(|error| {
        format!(
            "derive metadata-write lock authority for '{}': {error}",
            path.display()
        )
    })?;
    let mut authorities = vec![tail_journal_path(path), lock];
    let legacy = legacy_tail_journal_path(path);
    if legacy.exists() {
        authorities.push(legacy);
    }
    Ok(authorities)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TailJournalKind {
    ReplaceTail,
    AppendTag,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TailJournalHeader {
    state: u8,
    kind: TailJournalKind,
    offset: u64,
    original_len: u64,
    original_file_size: u64,
    committed_file_size: u64,
    recovery_identity: [u8; TAIL_JOURNAL_IDENTITY_LEN],
    payload_offset: u64,
}

impl TailJournalHeader {
    fn replace_tail(
        state: u8,
        offset: u64,
        original_len: u64,
        file_size: u64,
        recovery_identity: [u8; TAIL_JOURNAL_IDENTITY_LEN],
    ) -> Self {
        Self {
            state,
            kind: TailJournalKind::ReplaceTail,
            offset,
            original_len,
            original_file_size: file_size,
            committed_file_size: file_size,
            recovery_identity,
            payload_offset: TAIL_JOURNAL_V3_HEADER_LEN as u64,
        }
    }

    fn append_tag(
        state: u8,
        original_file_size: u64,
        committed_file_size: u64,
        recovery_identity: [u8; TAIL_JOURNAL_IDENTITY_LEN],
    ) -> Self {
        Self {
            state,
            kind: TailJournalKind::AppendTag,
            offset: DSF_HEADER_PATCH_OFFSET,
            original_len: DSF_HEADER_PATCH_LEN,
            original_file_size,
            committed_file_size,
            recovery_identity,
            payload_offset: TAIL_JOURNAL_V3_HEADER_LEN as u64,
        }
    }

    fn identity_boundary(self) -> u64 {
        match self.kind {
            TailJournalKind::ReplaceTail => self.offset,
            TailJournalKind::AppendTag => self.original_file_size,
        }
    }
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

fn encode_tail_journal_header(
    header: TailJournalHeader,
) -> [u8; TAIL_JOURNAL_V3_HEADER_LEN] {
    let mut bytes = [0u8; TAIL_JOURNAL_V3_HEADER_LEN];
    bytes[..8].copy_from_slice(TAIL_JOURNAL_MAGIC_V3);
    bytes[8] = header.state;
    bytes[9] = match header.kind {
        TailJournalKind::ReplaceTail => TAIL_JOURNAL_KIND_REPLACE_TAIL,
        TailJournalKind::AppendTag => TAIL_JOURNAL_KIND_APPEND_TAG,
    };
    bytes[10..18].copy_from_slice(&header.offset.to_le_bytes());
    bytes[18..26].copy_from_slice(&header.original_len.to_le_bytes());
    bytes[26..34].copy_from_slice(&header.original_file_size.to_le_bytes());
    bytes[34..42].copy_from_slice(&header.committed_file_size.to_le_bytes());
    bytes[42..].copy_from_slice(&header.recovery_identity);
    bytes
}

#[cfg(test)]
fn encode_tail_journal(
    offset: u64,
    original: &[u8],
    recovery_identity: &[u8; TAIL_JOURNAL_IDENTITY_LEN],
    state: u8,
) -> Vec<u8> {
    let file_size = offset + original.len() as u64;
    let header = encode_tail_journal_header(TailJournalHeader::replace_tail(
        state,
        offset,
        original.len() as u64,
        file_size,
        *recovery_identity,
    ));
    let mut bytes = Vec::with_capacity(TAIL_JOURNAL_V3_HEADER_LEN + original.len());
    bytes.extend_from_slice(&header);
    bytes.extend_from_slice(original);
    bytes
}

#[cfg(test)]
fn encode_legacy_tail_journal(
    offset: u64,
    original: &[u8],
    recovery_identity: &[u8; TAIL_JOURNAL_IDENTITY_LEN],
    state: u8,
) -> Vec<u8> {
    let mut header = [0u8; TAIL_JOURNAL_V2_HEADER_LEN];
    header[..8].copy_from_slice(TAIL_JOURNAL_MAGIC_V2);
    header[8] = state;
    header[9..17].copy_from_slice(&offset.to_le_bytes());
    header[17..25].copy_from_slice(&(original.len() as u64).to_le_bytes());
    header[25..].copy_from_slice(recovery_identity);
    let mut bytes = Vec::with_capacity(TAIL_JOURNAL_V2_HEADER_LEN + original.len());
    bytes.extend_from_slice(&header);
    bytes.extend_from_slice(original);
    bytes
}

#[cfg(test)]
fn encode_append_journal(
    original_header_patch: &[u8; DSF_HEADER_PATCH_LEN as usize],
    original_file_size: u64,
    committed_file_size: u64,
    recovery_identity: &[u8; TAIL_JOURNAL_IDENTITY_LEN],
    state: u8,
) -> Vec<u8> {
    let header = encode_tail_journal_header(TailJournalHeader::append_tag(
        state,
        original_file_size,
        committed_file_size,
        *recovery_identity,
    ));
    let mut bytes = Vec::with_capacity(
        TAIL_JOURNAL_V3_HEADER_LEN + original_header_patch.len(),
    );
    bytes.extend_from_slice(&header);
    bytes.extend_from_slice(original_header_patch);
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
    if file_len < TAIL_JOURNAL_V2_HEADER_LEN as u64 {
        return Err(format!(
            "DSF tail journal '{}' has an invalid header",
            journal.display()
        ));
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|error| format!("seek DSF tail journal '{}': {error}", journal.display()))?;
    let mut magic = [0u8; 8];
    file.read_exact(&mut magic)
        .map_err(|error| format!("read DSF tail journal header '{}': {error}", journal.display()))?;

    let header = if &magic == TAIL_JOURNAL_MAGIC_V2 {
        let mut rest = vec![0u8; TAIL_JOURNAL_V2_HEADER_LEN - magic.len()];
        file.read_exact(&mut rest).map_err(|error| {
            format!("read DSF tail journal header '{}': {error}", journal.display())
        })?;
        let state = rest[0];
        let offset = u64::from_le_bytes(
            rest[1..9]
                .try_into()
                .expect("fixed legacy journal offset slice"),
        );
        let original_len = u64::from_le_bytes(
            rest[9..17]
                .try_into()
                .expect("fixed legacy journal length slice"),
        );
        let recovery_identity = rest[17..]
            .try_into()
            .expect("fixed legacy DSF recovery identity slice");
        let file_size = offset.checked_add(original_len).ok_or_else(|| {
            format!("DSF tail journal '{}' range overflows", journal.display())
        })?;
        TailJournalHeader {
            state,
            kind: TailJournalKind::ReplaceTail,
            offset,
            original_len,
            original_file_size: file_size,
            committed_file_size: file_size,
            recovery_identity,
            payload_offset: TAIL_JOURNAL_V2_HEADER_LEN as u64,
        }
    } else if &magic == TAIL_JOURNAL_MAGIC_V3 {
        if file_len < TAIL_JOURNAL_V3_HEADER_LEN as u64 {
            return Err(format!(
                "DSF tail journal '{}' has an invalid v3 header",
                journal.display()
            ));
        }
        let mut rest = vec![0u8; TAIL_JOURNAL_V3_HEADER_LEN - magic.len()];
        file.read_exact(&mut rest).map_err(|error| {
            format!("read DSF tail journal header '{}': {error}", journal.display())
        })?;
        let state = rest[0];
        let kind = match rest[1] {
            TAIL_JOURNAL_KIND_REPLACE_TAIL => TailJournalKind::ReplaceTail,
            TAIL_JOURNAL_KIND_APPEND_TAG => TailJournalKind::AppendTag,
            other => {
                return Err(format!(
                    "DSF tail journal '{}' has unknown operation kind {other}",
                    journal.display()
                ))
            }
        };
        let offset = u64::from_le_bytes(
            rest[2..10]
                .try_into()
                .expect("fixed journal offset slice"),
        );
        let original_len = u64::from_le_bytes(
            rest[10..18]
                .try_into()
                .expect("fixed journal length slice"),
        );
        let original_file_size = u64::from_le_bytes(
            rest[18..26]
                .try_into()
                .expect("fixed journal original-size slice"),
        );
        let committed_file_size = u64::from_le_bytes(
            rest[26..34]
                .try_into()
                .expect("fixed journal committed-size slice"),
        );
        let recovery_identity = rest[34..]
            .try_into()
            .expect("fixed DSF recovery identity slice");
        TailJournalHeader {
            state,
            kind,
            offset,
            original_len,
            original_file_size,
            committed_file_size,
            recovery_identity,
            payload_offset: TAIL_JOURNAL_V3_HEADER_LEN as u64,
        }
    } else {
        return Err(format!(
            "DSF tail journal '{}' has an invalid header",
            journal.display()
        ));
    };

    if !matches!(header.state, TAIL_JOURNAL_PREPARED | TAIL_JOURNAL_COMMITTED) {
        return Err(format!(
            "DSF tail journal '{}' has unknown state {}",
            journal.display(),
            header.state
        ));
    }
    match header.kind {
        TailJournalKind::ReplaceTail => {
            let expected_size = header.offset.checked_add(header.original_len).ok_or_else(|| {
                format!("DSF tail journal '{}' range overflows", journal.display())
            })?;
            if header.original_file_size != expected_size
                || header.committed_file_size != expected_size
            {
                return Err(format!(
                    "DSF tail journal '{}' has inconsistent replacement bounds",
                    journal.display()
                ));
            }
        }
        TailJournalKind::AppendTag => {
            if header.offset != DSF_HEADER_PATCH_OFFSET
                || header.original_len != DSF_HEADER_PATCH_LEN
                || header.original_file_size
                    < DSF_HEADER_PATCH_OFFSET + DSF_HEADER_PATCH_LEN
                || header.committed_file_size <= header.original_file_size
            {
                return Err(format!(
                    "DSF tail journal '{}' has inconsistent append bounds",
                    journal.display()
                ));
            }
        }
    }
    let expected_len = header
        .payload_offset
        .checked_add(header.original_len)
        .ok_or_else(|| format!("DSF tail journal '{}' length overflows", journal.display()))?;
    if file_len != expected_len {
        return Err(format!(
            "DSF tail journal '{}' declares {} original byte(s), but contains {}",
            journal.display(),
            header.original_len,
            file_len.saturating_sub(header.payload_offset)
        ));
    }
    Ok(header)
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
    dsf_recovery_identity_with_domain(
        file,
        metadata_offset,
        file_size,
        b"tonepoet-dsf-tail-recovery-v2\0",
        false,
    )
}

fn dsf_append_recovery_identity(
    file: &mut std::fs::File,
    original_file_size: u64,
) -> Result<[u8; TAIL_JOURNAL_IDENTITY_LEN], String> {
    dsf_recovery_identity_with_domain(
        file,
        original_file_size,
        original_file_size,
        b"tonepoet-dsf-append-recovery-v3\0",
        true,
    )
}

fn dsf_recovery_identity_with_domain(
    file: &mut std::fs::File,
    metadata_offset: u64,
    file_size: u64,
    domain: &[u8],
    mask_mutable_header_patch: bool,
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
    if mask_mutable_header_patch {
        mask_recovery_identity_header_patch(&mut first, 0);
    }

    let last_offset = metadata_offset - sample_len as u64;
    let mut last = vec![0u8; sample_len];
    file.seek(SeekFrom::Start(last_offset))
        .map_err(|error| format!("seek DSF recovery identity suffix: {error}"))?;
    file.read_exact(&mut last)
        .map_err(|error| format!("read DSF recovery identity suffix: {error}"))?;
    if mask_mutable_header_patch {
        mask_recovery_identity_header_patch(&mut last, last_offset);
    }

    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update(file_size.to_le_bytes());
    digest.update(metadata_offset.to_le_bytes());
    digest.update((sample_len as u64).to_le_bytes());
    digest.update(&first);
    digest.update(&last);
    Ok(digest.finalize().into())
}

fn mask_recovery_identity_header_patch(sample: &mut [u8], sample_offset: u64) {
    let sample_end = sample_offset.saturating_add(sample.len() as u64);
    let patch_end = DSF_HEADER_PATCH_OFFSET + DSF_HEADER_PATCH_LEN;
    let overlap_start = sample_offset.max(DSF_HEADER_PATCH_OFFSET);
    let overlap_end = sample_end.min(patch_end);
    if overlap_start < overlap_end {
        let start = (overlap_start - sample_offset) as usize;
        let end = (overlap_end - sample_offset) as usize;
        sample[start..end].fill(0);
    }
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
        TailJournalHeader::replace_tail(
            TAIL_JOURNAL_PREPARED,
            offset,
            replacement.len() as u64,
            actual_size,
            recovery_identity,
        ),
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

fn append_tag_in_place(
    path: &Path,
    original_file_size: u64,
    replacement: &[u8],
    is_cancelled: &dyn Fn() -> bool,
    progress: &dyn Fn(DsfWriteProgress),
) -> Result<Option<String>, String> {
    if is_cancelled() {
        return Err("metadata save cancelled before DSF append journaling".to_string());
    }
    let committed_file_size = original_file_size
        .checked_add(replacement.len() as u64)
        .ok_or_else(|| format!("appended DSF size overflows for '{}'", path.display()))?;
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|error| format!("open untagged DSF for append '{}': {error}", path.display()))?;
    let actual_size = file
        .metadata()
        .map_err(|error| format!("stat untagged DSF source '{}': {error}", path.display()))?
        .len();
    if actual_size != original_file_size {
        return Err(format!(
            "DSF source '{}' changed during metadata preparation: expected {original_file_size} bytes, found {actual_size}",
            path.display()
        ));
    }
    let recovery_identity = dsf_append_recovery_identity(&mut file, original_file_size)
        .map_err(|error| {
            format!(
                "bind DSF append recovery journal for '{}': {error}",
                path.display()
            )
        })?;
    let journal = tail_journal_path(path);
    publish_tail_journal_streaming(
        path,
        &journal,
        &mut file,
        TailJournalHeader::append_tag(
            TAIL_JOURNAL_PREPARED,
            original_file_size,
            committed_file_size,
            recovery_identity,
        ),
        is_cancelled,
        progress,
    )?;

    let rollback_after_error = |primary: String,
                                file: &mut std::fs::File|
     -> Result<Option<String>, String> {
        match restore_tail_from_journal(path, &journal, file, progress) {
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
        }
    };

    if let Err(error) = file.seek(SeekFrom::Start(original_file_size)) {
        let primary = format!("seek to DSF append offset in '{}': {error}", path.display());
        return rollback_after_error(primary, &mut file);
    }
    let mut written = 0usize;
    while written < replacement.len() {
        if is_cancelled() {
            return rollback_cancelled_tail(path, &journal, &mut file, progress);
        }
        let end = (written + COPY_CHUNK_BYTES).min(replacement.len());
        if let Err(error) = file.write_all(&replacement[written..end]) {
            let primary = format!("append DSF metadata tag '{}': {error}", path.display());
            return rollback_after_error(primary, &mut file);
        }
        written = end;
        progress(DsfWriteProgress {
            phase: DsfWriteProgressPhase::WritingTail,
            bytes_done: written as u64,
            bytes_total: replacement.len() as u64,
        });
    }
    if let Err(error) = file.sync_all() {
        let primary = format!("fsync appended DSF metadata '{}': {error}", path.display());
        return rollback_after_error(primary, &mut file);
    }
    if is_cancelled() {
        return rollback_cancelled_tail(path, &journal, &mut file, progress);
    }

    let mut header_patch = [0u8; DSF_HEADER_PATCH_LEN as usize];
    header_patch[..8].copy_from_slice(&committed_file_size.to_le_bytes());
    header_patch[8..].copy_from_slice(&original_file_size.to_le_bytes());
    if let Err(error) = file.seek(SeekFrom::Start(DSF_HEADER_PATCH_OFFSET)) {
        let primary = format!("seek to DSF header patch in '{}': {error}", path.display());
        return rollback_after_error(primary, &mut file);
    }
    if let Err(error) = file.write_all(&header_patch) {
        let primary = format!("patch DSF size and metadata pointer '{}': {error}", path.display());
        return rollback_after_error(primary, &mut file);
    }
    if let Err(error) = file.sync_all() {
        let primary = format!("fsync DSF header patch '{}': {error}", path.display());
        return rollback_after_error(primary, &mut file);
    }
    if is_cancelled() {
        return rollback_cancelled_tail(path, &journal, &mut file, progress);
    }

    match mark_tail_journal_committed(&journal) {
        TailJournalCommitOutcome::Durable => {}
        TailJournalCommitOutcome::StateUnchanged(error) => {
            let primary = format!(
                "could not commit DSF append journal for '{}': {error}",
                path.display()
            );
            return rollback_after_error(primary, &mut file);
        }
        TailJournalCommitOutcome::DurabilityUncertain(error) => {
            progress(DsfWriteProgress {
                phase: DsfWriteProgressPhase::Publishing,
                bytes_done: replacement.len() as u64,
                bytes_total: replacement.len() as u64,
            });
            return Ok(Some(format!(
                "DSF metadata for '{}' was appended and its header was fsynced, but journal commit durability is uncertain: {error}. Journal '{}' was retained; recovery will keep the append if COMMITTED is durable or restore the original untagged file if PREPARED is durable",
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
            "DSF metadata append for '{}' committed, but its committed journal could not be retired durably: {error}",
            path.display()
        ))),
    }
}

fn publish_file_noreplace(source: &Path, destination: &Path) -> std::io::Result<()> {
    // The source and destination are in the same directory. Creating a hard
    // link is an atomic create-if-absent operation on the supported local
    // filesystems: an existing journal is never replaced. The private source
    // name may survive a failed unlink, but both names then identify the same
    // fully fsynced journal and startup cleanup can retire the extra name.
    std::fs::hard_link(source, destination)?;
    let _ = std::fs::remove_file(source);
    Ok(())
}

fn publish_tail_journal_bytes(
    journal: &Path,
    header: TailJournalHeader,
    payload: &[u8],
) -> Result<(), String> {
    if payload.len() as u64 != header.original_len {
        return Err(format!(
            "DSF tail journal payload has {} byte(s), expected {}",
            payload.len(),
            header.original_len
        ));
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
            .and_then(|()| temporary.write_all(payload))
            .map_err(|error| {
                format!(
                    "write DSF tail journal temporary '{}': {error}",
                    temporary_path.display()
                )
            })?;
        temporary.sync_all().map_err(|error| {
            format!(
                "sync DSF tail journal temporary '{}': {error}",
                temporary_path.display()
            )
        })?;
        drop(temporary);
        publish_file_noreplace(&temporary_path, journal).map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                format!(
                    "refusing to replace unresolved DSF tail journal '{}' while publishing from '{}'",
                    journal.display(),
                    temporary_path.display()
                )
            } else {
                format!(
                    "atomically publish DSF tail journal '{}' from '{}' without replacement: {error}",
                    journal.display(),
                    temporary_path.display()
                )
            }
        })?;
        published = true;
        crate::config::sync_parent_dir(parent).map_err(|error| {
            format!(
                "sync parent after publishing DSF tail journal '{}': {error}",
                journal.display()
            )
        })
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

fn publish_tail_journal_streaming(
    path: &Path,
    journal: &Path,
    source: &mut std::fs::File,
    header: TailJournalHeader,
    is_cancelled: &dyn Fn() -> bool,
    progress: &dyn Fn(DsfWriteProgress),
) -> Result<(), String> {
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
        publish_file_noreplace(&temporary_path, journal).map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                format!(
                    "refusing to replace unresolved DSF tail journal '{}' while publishing from '{}'",
                    journal.display(),
                    temporary_path.display()
                )
            } else {
                format!(
                    "atomically publish DSF tail journal '{}' from '{}' without replacement: {error}",
                    journal.display(),
                    temporary_path.display()
                )
            }
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

fn restore_untagged_artwork_snapshot(
    path: &Path,
    original_file_size: u64,
    original_header_patch: &[u8; DSF_HEADER_PATCH_LEN as usize],
) -> Result<Option<String>, String> {
    let current = inspect_dsf_metadata_location(path)?;
    let DsfMetadataLocation::Id3 {
        offset,
        tag_end,
        file_size,
    } = current
    else {
        return match current {
            DsfMetadataLocation::Untagged { file_size } if file_size == original_file_size => Ok(None),
            DsfMetadataLocation::Untagged { file_size } => Err(format!(
                "refusing DSF artwork rollback for '{}': expected original size {original_file_size}, found untagged size {file_size}",
                path.display()
            )),
            DsfMetadataLocation::Id3 { .. } => unreachable!("matched above"),
        };
    };
    if offset != original_file_size || tag_end != file_size {
        return Err(format!(
            "refusing DSF artwork rollback for '{}': current tag occupies {offset}..{tag_end} in a {file_size}-byte file, expected an appended tag beginning at {original_file_size}",
            path.display()
        ));
    }
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|error| format!("open DSF artwork rollback target '{}': {error}", path.display()))?;
    let recovery_identity = dsf_append_recovery_identity(&mut file, original_file_size)
        .map_err(|error| {
            format!(
                "bind DSF artwork rollback journal for '{}': {error}",
                path.display()
            )
        })?;
    let journal = tail_journal_path(path);
    publish_tail_journal_bytes(
        &journal,
        TailJournalHeader::append_tag(
            TAIL_JOURNAL_PREPARED,
            original_file_size,
            file_size,
            recovery_identity,
        ),
        original_header_patch,
    )?;
    match restore_tail_from_journal(path, &journal, &mut file, &|_| {}) {
        Ok(()) => match remove_journal_durably(&journal) {
            Ok(()) => Ok(None),
            Err(error) => Ok(Some(format!(
                "DSF artwork rollback for '{}' restored the exact untagged file, but its journal could not be retired durably: {error}",
                path.display()
            ))),
        },
        Err(error) => Err(format!(
            "DSF artwork rollback failed for '{}' and journal '{}' was retained for startup recovery: {error}",
            path.display(),
            journal.display()
        )),
    }
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

fn tail_journal_target_mismatch(
    path: &Path,
    file: &mut std::fs::File,
    header: TailJournalHeader,
) -> Result<Option<String>, String> {
    let actual = file
        .metadata()
        .map_err(|error| {
            format!(
                "stat DSF target during journal recovery '{}': {error}",
                path.display()
            )
        })?
        .len();
    match header.kind {
        TailJournalKind::ReplaceTail if actual != header.original_file_size => {
            return Ok(Some(format!(
                "journal expects {} bytes, target has {actual}",
                header.original_file_size
            )));
        }
        TailJournalKind::AppendTag => {
            let size_matches = if header.state == TAIL_JOURNAL_COMMITTED {
                actual == header.committed_file_size
            } else {
                actual >= header.original_file_size && actual <= header.committed_file_size
            };
            if !size_matches {
                return Ok(Some(format!(
                    "append journal permits {}..={} bytes in PREPARED state and requires {} bytes in COMMITTED state, target has {actual}",
                    header.original_file_size,
                    header.committed_file_size,
                    header.committed_file_size
                )));
            }
        }
        _ => {}
    }

    let actual_identity = match header.kind {
        TailJournalKind::ReplaceTail => dsf_recovery_identity(
            file,
            header.identity_boundary(),
            header.original_file_size,
        ),
        TailJournalKind::AppendTag => dsf_append_recovery_identity(file, header.original_file_size),
    }
    .map_err(|error| {
        format!(
            "verify DSF target identity during journal recovery '{}': {error}",
            path.display()
        )
    })?;
    if actual_identity != header.recovery_identity {
        return Ok(Some(
            "the bounded audio-prefix identity no longer matches the journal authority".to_string(),
        ));
    }

    if header.kind == TailJournalKind::AppendTag && header.state == TAIL_JOURNAL_COMMITTED {
        let facts_match = matches!(
            inspect_dsf_metadata_location(path)?,
            DsfMetadataLocation::Id3 { offset, tag_end, file_size }
                if offset == header.original_file_size
                    && tag_end == header.committed_file_size
                    && file_size == header.committed_file_size
        );
        if !facts_match {
            return Ok(Some(
                "published container facts do not match the committed append journal".to_string(),
            ));
        }
    }
    Ok(None)
}

fn verify_tail_journal_target(
    path: &Path,
    file: &mut std::fs::File,
    header: TailJournalHeader,
) -> Result<(), String> {
    if let Some(reason) = tail_journal_target_mismatch(path, file, header)? {
        return Err(format!(
            "refusing DSF tail-journal recovery for '{}': {reason}",
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
        .seek(SeekFrom::Start(header.payload_offset))
        .map_err(|error| {
            format!(
                "seek DSF tail journal payload '{}': {error}",
                journal.display()
            )
        })?;
    file.seek(SeekFrom::Start(header.offset))
        .map_err(|error| format!("seek for DSF tail rollback: {error}"))?;
    let mut copied = 0u64;
    let mut buffer = vec![0u8; COPY_CHUNK_BYTES];
    while copied < header.original_len {
        let wanted = usize::try_from((header.original_len - copied).min(buffer.len() as u64))
            .expect("bounded DSF rollback copy chunk");
        journal_file
            .read_exact(&mut buffer[..wanted])
            .map_err(|error| format!("read original DSF metadata bytes from journal: {error}"))?;
        file.write_all(&buffer[..wanted])
            .map_err(|error| format!("restore original DSF metadata bytes: {error}"))?;
        copied += wanted as u64;
        progress(DsfWriteProgress {
            phase: DsfWriteProgressPhase::Recovering,
            bytes_done: copied,
            bytes_total: header.original_len,
        });
    }
    if header.kind == TailJournalKind::AppendTag {
        file.set_len(header.original_file_size)
            .map_err(|error| format!("truncate appended DSF metadata during rollback: {error}"))?;
    }
    file.sync_all()
        .map_err(|error| format!("fsync restored DSF metadata state: {error}"))
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
        .unwrap_or_else(|| std::ffi::OsStr::new("audio.dsf"));
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
    file_name: &std::ffi::OsStr,
    source_metadata: &std::fs::Metadata,
) -> Result<(PathBuf, std::fs::File), String> {
    let digest = crate::config::native_os_str_sha256_hex(
        b"tonepoet-dsf-rewrite-temp-path-v1\0",
        file_name,
    );
    for _ in 0..128 {
        let path = parent.join(format!(
            ".tonepoet-dsf-rewrite-{digest}-{}.tmp",
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

fn recover_tail_journal_at(path: &Path, journal: &Path) -> Result<bool, String> {
    let Some(mut journal_file) = open_tail_journal_for_read(journal)? else {
        return Ok(false);
    };
    let header = read_tail_journal_header(journal, &mut journal_file)?;
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(header.state == TAIL_JOURNAL_PREPARED)
        .open(path)
        .map_err(|error| {
            format!(
                "open DSF target for journal recovery '{}': {error}",
                path.display()
            )
        })?;
    if header.state == TAIL_JOURNAL_PREPARED {
        restore_tail_from_journal(path, journal, &mut file, &|_| {})?;
    } else {
        verify_tail_journal_target(path, &mut file, header)?;
    }
    remove_journal_durably(journal)?;
    Ok(true)
}

fn tail_journal_matches_target(journal: &Path, path: &Path) -> Result<bool, String> {
    let Some(mut journal_file) = open_tail_journal_for_read(journal)? else {
        return Ok(false);
    };
    let header = read_tail_journal_header(journal, &mut journal_file)?;
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(format!(
                "open candidate DSF target '{}' while attributing journal '{}': {error}",
                path.display(),
                journal.display()
            ));
        }
    };
    Ok(tail_journal_target_mismatch(path, &mut file, header)?.is_none())
}

fn dsf_targets_in_directory(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let entries = std::fs::read_dir(dir)
        .map_err(|error| format!("scan DSF recovery directory '{}': {error}", dir.display()))?;
    let mut targets = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            format!("read DSF recovery directory entry in '{}': {error}", dir.display())
        })?;
        let path = entry.path();
        if !is_dsf(&path) {
            continue;
        }
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        if metadata.is_file() && !metadata.file_type().is_symlink() {
            targets.push(path);
        }
    }
    Ok(targets)
}

fn hashed_tail_journal_name(name: &str) -> bool {
    let Some(digest) = name
        .strip_prefix(".tonepoet-dsf-tail-")
        .and_then(|name| name.strip_suffix(".journal"))
    else {
        return false;
    };
    digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn legacy_tail_journal_name(name: &str) -> bool {
    name.starts_with('.') && name.ends_with(".tonepoet-dsf-tail.journal")
}

fn journal_authority_name(path: &Path) -> Option<&str> {
    let name = path.file_name()?.to_str()?;
    if hashed_tail_journal_name(name) || legacy_tail_journal_name(name) {
        Some(name)
    } else {
        tail_journal_temp_authority_name(name)
    }
}

fn resolve_tail_journal_target(
    journal: &Path,
    candidates: &[PathBuf],
) -> Result<Option<PathBuf>, String> {
    let authority_name = journal_authority_name(journal).ok_or_else(|| {
        format!(
            "refusing unrecognized DSF tail-journal artifact '{}'",
            journal.display()
        )
    })?;

    if hashed_tail_journal_name(authority_name) {
        let mut path_matches = candidates.iter().filter(|candidate| {
            tail_journal_path(candidate)
                .file_name()
                .and_then(|name| name.to_str())
                == Some(authority_name)
        });
        let Some(target) = path_matches.next() else {
            return Ok(None);
        };
        if path_matches.next().is_some() {
            return Err(format!(
                "refusing ambiguous DSF tail-journal authority '{}': multiple current DSF targets derive the same SHA-256 authority pathname",
                journal.display()
            ));
        }
        return if tail_journal_matches_target(journal, target)? {
            Ok(Some(target.clone()))
        } else {
            Ok(None)
        };
    }

    let mut identity_matches = Vec::new();
    for candidate in candidates {
        if tail_journal_matches_target(journal, candidate)? {
            identity_matches.push(candidate.clone());
        }
    }
    match identity_matches.len() {
        0 => Ok(None),
        1 => Ok(identity_matches.pop()),
        _ => Err(format!(
            "refusing ambiguous legacy DSF tail journal '{}': its embedded generation identity matches multiple current DSF targets",
            journal.display()
        )),
    }
}

fn recover_tail_journal(path: &Path) -> Result<bool, String> {
    let journal = tail_journal_path(path);
    if journal.exists() {
        return recover_tail_journal_at(path, &journal);
    }

    let legacy = legacy_tail_journal_path(path);
    if !legacy.exists() {
        return Ok(false);
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let candidates = dsf_targets_in_directory(parent)?;
    match resolve_tail_journal_target(&legacy, &candidates)? {
        Some(target) if target == path => recover_tail_journal_at(path, &legacy),
        Some(_) => Ok(false),
        None => Err(format!(
            "unresolved legacy DSF tail journal '{}' cannot be attributed to a current DSF target; inspect the directory at startup or run `tonepoet dsf-recover status '{}'` before retrying the write",
            legacy.display(),
            path.display()
        )),
    }
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

/// Inspect a generation-bound DSF tail journal for one target without changing
/// either artifact. The same per-target lock used by writes prevents recovery
/// status from racing a live mutation.
pub fn inspect_tail_journal(path: &Path) -> Result<Option<DsfTailJournalInspection>, String> {
    let (_lock, target) = acquire_dsf_write_lock(path)?;
    inspect_tail_journal_locked(&target)
}

fn inspect_tail_journal_locked(
    target: &Path,
) -> Result<Option<DsfTailJournalInspection>, String> {
    let hashed = tail_journal_path(target);
    let journal = if hashed.exists() {
        hashed
    } else {
        let legacy = legacy_tail_journal_path(target);
        if !legacy.exists() {
            return Ok(None);
        }
        let parent = target.parent().unwrap_or_else(|| Path::new("."));
        let candidates = dsf_targets_in_directory(parent)?;
        match resolve_tail_journal_target(&legacy, &candidates)? {
            Some(candidate) if candidate == target => legacy,
            Some(candidate) => {
                return Err(format!(
                    "DSF tail journal '{}' belongs to '{}', not '{}'",
                    legacy.display(),
                    candidate.display(),
                    target.display()
                ))
            }
            None => {
                return Err(format!(
                    "DSF tail journal '{}' cannot be attributed to '{}'",
                    legacy.display(),
                    target.display()
                ))
            }
        }
    };

    let mut journal_file = open_tail_journal_for_read(&journal)?.ok_or_else(|| {
        format!(
            "DSF tail journal '{}' disappeared during inspection",
            journal.display()
        )
    })?;
    let header = read_tail_journal_header(&journal, &mut journal_file)?;
    let mut target_file = std::fs::File::open(target).map_err(|error| {
        format!(
            "open DSF target for tail-journal inspection '{}': {error}",
            target.display()
        )
    })?;
    verify_tail_journal_target(target, &mut target_file, header)?;

    Ok(Some(DsfTailJournalInspection {
        target: target.to_path_buf(),
        journal,
        state: if header.state == TAIL_JOURNAL_PREPARED {
            "prepared"
        } else {
            "committed"
        },
        operation: match header.kind {
            TailJournalKind::ReplaceTail => "replace-tail",
            TailJournalKind::AppendTag => "append-tag",
        },
        original_file_size: header.original_file_size,
        committed_file_size: header.committed_file_size,
    }))
}

/// Recover or retire the generation-bound tail journal for one DSF target. A
/// PREPARED journal restores the exact pre-write state; a COMMITTED journal is
/// verified against the published container and then retired. Inspection and
/// recovery remain under one per-target lock, so the returned description is
/// the exact journal generation that was resolved.
pub fn recover_tail_journal_for_target(
    path: &Path,
) -> Result<Option<DsfTailJournalInspection>, String> {
    let (_lock, target) = acquire_dsf_write_lock(path)?;
    let Some(inspection) = inspect_tail_journal_locked(&target)? else {
        return Ok(None);
    };
    if !recover_tail_journal_at(&target, &inspection.journal)? {
        return Err(format!(
            "DSF tail journal '{}' disappeared while its target lock was held",
            inspection.journal.display()
        ));
    }
    Ok(Some(inspection))
}

/// Inspect a legacy `.tonepoet-bak` marker if present. Absence is not an error,
/// which lets the recovery CLI report tail and legacy authority independently.
pub fn inspect_legacy_backup_if_present(
    path: &Path,
) -> Result<Option<DsfLegacyBackupInspection>, String> {
    let (_lock, target) = acquire_dsf_write_lock(path)?;
    let marker = crate::db::Database::backup_path_for(&target);
    match std::fs::symlink_metadata(&marker) {
        Ok(_) => inspect_legacy_backup_locked(&target, &marker).map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!(
            "inspect legacy DSF rollback marker '{}': {error}",
            marker.display()
        )),
    }
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
            require_dsf_header(&marker, "legacy DSF rollback marker")?;
            crate::db::Database::restore_backup_for(&target, &marker)?;
        }
        DsfLegacyBackupResolution::KeepCurrent => {
            remove_legacy_backup_marker_durably(&marker)?;
        }
    }
    Ok(inspection)
}

fn require_dsf_header(path: &Path, label: &str) -> Result<(), String> {
    let mut file = std::fs::File::open(path)
        .map_err(|error| format!("open {label} '{}': {error}", path.display()))?;
    let mut marker = [0u8; 4];
    file.read_exact(&mut marker)
        .map_err(|error| format!("read {label} header '{}': {error}", path.display()))?;
    if &marker != b"DSD " {
        return Err(format!(
            "refusing to restore {label} '{}': expected DSF DSD chunk marker",
            path.display()
        ));
    }
    Ok(())
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
    let target_name = path.file_name();
    let rewrite_digest = crate::config::native_os_str_sha256_hex(
        b"tonepoet-dsf-rewrite-temp-path-v1\0",
        target_name.unwrap_or_else(|| std::ffi::OsStr::new("audio.dsf")),
    );
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
        let belongs_to_target = if rewrite_temp_digest(name).is_some_and(|value| value == rewrite_digest) {
            true
        } else if let Some(original_name) = legacy_dsf_temp_original_name(name) {
            target_name
                .and_then(|name| name.to_str())
                .is_some_and(|target| target == original_name)
        } else if let Some(journal_name) = tail_journal_temp_authority_name(name) {
            // Unpublished journal temps attribute by NAME: publication is an
            // atomic hard-link of a complete, fsynced journal, so a surviving
            // temp is torn by contract and must not be content-parsed.
            tail_journal_path(path)
                .file_name()
                .and_then(|name| name.to_str())
                == Some(journal_name)
                || legacy_tail_journal_path(path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    == Some(journal_name)
        } else {
            false
        };
        if !belongs_to_target {
            continue;
        }
        let metadata = std::fs::symlink_metadata(&artifact)
            .map_err(|error| format!("inspect DSF temporary '{}': {error}", artifact.display()))?;
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

fn rewrite_temp_digest(name: &str) -> Option<&str> {
    let stem = name
        .strip_prefix(".tonepoet-dsf-rewrite-")?
        .strip_suffix(".tmp")?;
    let digest = stem.get(..64)?;
    let uuid = stem.get(64..)?.strip_prefix('-')?;
    uuid::Uuid::parse_str(uuid).ok()?;
    if digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Some(digest)
    } else {
        None
    }
}

fn legacy_dsf_temp_original_name(name: &str) -> Option<&str> {
    let (prefix, suffix) = name.rsplit_once(".tonepoet-id3-")?;
    let original = prefix.strip_prefix('.')?;
    if !original.to_ascii_lowercase().ends_with(".dsf") {
        return None;
    }
    let uuid = suffix.strip_suffix(".tmp")?;
    uuid::Uuid::parse_str(uuid).ok()?;
    Some(original)
}

/// Attribute an UNPUBLISHED tail-journal temp to a current target by NAME
/// alone. Publication is an atomic hard-link of a complete, fsynced journal,
/// so a surviving `.tmp` is dead weight by contract — its content may be
/// arbitrarily torn and must not be parsed for attribution. Removal still
/// acquires the target's write lock (cleanup_stale_dsf_temp), so this can
/// never race a live publication.
fn resolve_tail_journal_temp_target(
    journal_name: &str,
    candidates: &[PathBuf],
) -> Result<Option<PathBuf>, String> {
    let mut matches = candidates.iter().filter(|candidate| {
        tail_journal_path(candidate)
            .file_name()
            .and_then(|name| name.to_str())
            == Some(journal_name)
            || legacy_tail_journal_path(candidate)
                .file_name()
                .and_then(|name| name.to_str())
                == Some(journal_name)
    });
    let Some(target) = matches.next() else {
        return Ok(None);
    };
    if matches.next().is_some() {
        return Err(format!(
            "refusing ambiguous DSF tail-journal temp authority '{journal_name}': multiple current DSF targets derive the same journal name"
        ));
    }
    Ok(Some(target.clone()))
}

fn tail_journal_temp_authority_name(name: &str) -> Option<&str> {
    let stem = name.strip_suffix(".tmp")?;
    let (journal_name, uuid) = stem.rsplit_once('.')?;
    uuid::Uuid::parse_str(uuid).ok()?;
    if hashed_tail_journal_name(journal_name) || legacy_tail_journal_name(journal_name) {
        Some(journal_name)
    } else {
        None
    }
}


#[cfg(test)]
fn is_dsf_temp_name(name: &str) -> bool {
    rewrite_temp_digest(name).is_some() || legacy_dsf_temp_original_name(name).is_some()
}

fn cleanup_stale_dsf_temp(
    artifact: &Path,
    target: &Path,
    label: &str,
    dir: &Path,
) -> String {
    match acquire_dsf_write_lock(target) {
        Ok((_lock, _target)) => match std::fs::remove_file(artifact) {
            Ok(()) => match crate::config::sync_parent_dir(dir) {
                Ok(()) => format!("Removed stale {label} {}", artifact.display()),
                Err(error) => format!(
                    "Removed stale {label} {}, but directory durability is unconfirmed: {error}",
                    artifact.display()
                ),
            },
            Err(error) => format!("{label} cleanup failed for {}: {error}", artifact.display()),
        },
        Err(error) => format!("{label} cleanup deferred for {}: {error}", artifact.display()),
    }
}

/// Recover generation-bound DSF tail journals, report unversioned legacy
/// full-file backup markers without applying them, and remove orphaned temp
/// rewrites in one directory. Messages are suitable for the startup status
/// surface; failures remain visible and leave their authority artifacts intact.
pub fn recover_stale_writes_in_directory(dir: &Path) -> Vec<String> {
    let mut messages = Vec::new();
    let candidates = match dsf_targets_in_directory(dir) {
        Ok(candidates) => candidates,
        Err(error) => {
            messages.push(error);
            return messages;
        }
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return messages;
    };
    for entry in entries.flatten() {
        let artifact = entry.path();
        let Some(name) = artifact.file_name().and_then(|name| name.to_str()) else {
            continue;
        };

        if hashed_tail_journal_name(name) || legacy_tail_journal_name(name) {
            match resolve_tail_journal_target(&artifact, &candidates) {
                Ok(Some(original)) => match acquire_dsf_write_lock(&original) {
                    Ok((_lock, target)) => match recover_tail_journal_at(&target, &artifact) {
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
                },
                Ok(None) => messages.push(format!(
                    "DSF tail journal '{}' was retained because its embedded generation identity does not match any current DSF target in '{}'",
                    artifact.display(),
                    dir.display()
                )),
                Err(error) => messages.push(error),
            }
            continue;
        }

        if let Some(journal_name) = tail_journal_temp_authority_name(name) {
            match resolve_tail_journal_temp_target(journal_name, &candidates) {
                Ok(Some(original)) => messages.push(cleanup_stale_dsf_temp(
                    &artifact,
                    &original,
                    "DSF tail-journal temp",
                    dir,
                )),
                Ok(None) => messages.push(format!(
                    "DSF tail-journal temp '{}' was retained because it cannot be attributed to a current target",
                    artifact.display()
                )),
                Err(error) => messages.push(error),
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

        let rewrite_target = if let Some(digest) = rewrite_temp_digest(name) {
            let mut matches = candidates.iter().filter(|candidate| {
                candidate.file_name().is_some_and(|file_name| {
                    crate::config::native_os_str_sha256_hex(
                        b"tonepoet-dsf-rewrite-temp-path-v1\0",
                        file_name,
                    ) == digest
                })
            });
            let first = matches.next().cloned();
            if matches.next().is_some() {
                messages.push(format!(
                    "DSF rewrite temp '{}' was retained because its target digest is ambiguous",
                    artifact.display()
                ));
                None
            } else {
                first
            }
        } else if let Some(original_name) = legacy_dsf_temp_original_name(name) {
            let original = artifact.with_file_name(original_name);
            original.exists().then_some(original)
        } else {
            None
        };
        if let Some(original) = rewrite_target {
            messages.push(cleanup_stale_dsf_temp(
                &artifact,
                &original,
                "DSF rewrite temp",
                dir,
            ));
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
    let mut stored_value_counts = BTreeMap::<String, usize>::new();
    let raw_counts = raw.stored_value_counts;
    let entries = raw.fields.into_iter().collect::<Vec<_>>();

    for (key, values) in &entries {
        let count = raw_counts
            .get(key)
            .copied()
            .unwrap_or_else(|| values.iter().filter(|value| !value.trim().is_empty()).count());
        *stored_value_counts.entry(canonicalize_key(key)).or_default() += count;
    }

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
    DsfTagSnapshot {
        fields,
        stored_value_counts,
    }
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
    use id3::frame::{Comment, Content, ExtendedText, Picture, PictureType};
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

    pub(super) fn prepare_artwork_replace(
        path: &Path,
        picture_type_code: u8,
        mime_type: &str,
        image_bytes: &[u8],
    ) -> Result<(super::DsfArtworkSnapshot, Vec<u8>), String> {
        let location = super::inspect_dsf_metadata_location(path)?;
        let mut tag = read_tag(path, location)?;
        let original_encoded_tag = encode_tag(&tag)?;
        let picture_type = picture_type_from_code(picture_type_code);
        tag.remove_picture_by_type(picture_type);
        tag.add_frame(Picture {
            mime_type: mime_type.to_string(),
            picture_type,
            description: String::new(),
            data: image_bytes.to_vec(),
        });
        let encoded = encode_tag(&tag)?;
        Ok((
            super::DsfArtworkSnapshot {
                original_location: location,
                original_encoded_tag,
                original_header_patch: matches!(location, DsfMetadataLocation::Untagged { .. })
                    .then(|| super::read_dsf_header_patch(path))
                    .transpose()?,
            },
            encoded,
        ))
    }

    pub(super) fn prepare_artwork_remove(
        path: &Path,
        picture_type_code: u8,
    ) -> Result<Option<(super::DsfArtworkSnapshot, Vec<u8>)>, String> {
        let location = super::inspect_dsf_metadata_location(path)?;
        let mut tag = read_tag(path, location)?;
        let picture_type = picture_type_from_code(picture_type_code);
        if !tag.pictures().any(|picture| picture.picture_type == picture_type) {
            return Ok(None);
        }
        let original_encoded_tag = encode_tag(&tag)?;
        tag.remove_picture_by_type(picture_type);
        let encoded = encode_tag(&tag)?;
        Ok(Some((
            super::DsfArtworkSnapshot {
                original_location: location,
                original_encoded_tag,
                original_header_patch: matches!(location, DsfMetadataLocation::Untagged { .. })
                    .then(|| super::read_dsf_header_patch(path))
                    .transpose()?,
            },
            encoded,
        )))
    }

    fn encode_tag(tag: &Tag) -> Result<Vec<u8>, String> {
        let mut encoded = Vec::new();
        tag.write_to(&mut encoded, Version::Id3v24)
            .map_err(|error| format!("encode ID3v2.4 tag: {error}"))?;
        if !encoded.starts_with(b"ID3") {
            return Err("ID3 backend produced bytes without an ID3 marker".to_string());
        }
        Ok(encoded)
    }

    /// Convert the ID3v2 APIC picture-type byte without relying on a reverse
    /// `From<u8>` implementation that id3 1.17.0 does not provide. Keep the
    /// registry mapping explicit so every standardized code and every unknown
    /// extension value has deterministic round-trip behavior.
    pub(super) fn picture_type_from_code(code: u8) -> PictureType {
        match code {
            0x00 => PictureType::Other,
            0x01 => PictureType::Icon,
            0x02 => PictureType::OtherIcon,
            0x03 => PictureType::CoverFront,
            0x04 => PictureType::CoverBack,
            0x05 => PictureType::Leaflet,
            0x06 => PictureType::Media,
            0x07 => PictureType::LeadArtist,
            0x08 => PictureType::Artist,
            0x09 => PictureType::Conductor,
            0x0a => PictureType::Band,
            0x0b => PictureType::Composer,
            0x0c => PictureType::Lyricist,
            0x0d => PictureType::RecordingLocation,
            0x0e => PictureType::DuringRecording,
            0x0f => PictureType::DuringPerformance,
            0x10 => PictureType::ScreenCapture,
            0x11 => PictureType::BrightFish,
            0x12 => PictureType::Illustration,
            0x13 => PictureType::BandLogo,
            0x14 => PictureType::PublisherLogo,
            unknown => PictureType::Undefined(unknown),
        }
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
        let stored_value_counts = fields
            .iter()
            .map(|(key, values)| (key.clone(), values.len()))
            .collect();
        DsfTagSnapshot {
            fields,
            stored_value_counts,
        }
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
    fn dsf_snapshot_promotes_pre_emphasis_and_preserves_custom_tag_provenance() {
        let snapshot = DsfTagSnapshot {
            fields: BTreeMap::from([
                ("PRE_EMPHASIS".to_string(), vec!["1".to_string()]),
                ("MY_NOTE".to_string(), vec!["keep me".to_string()]),
            ]),
            ..DsfTagSnapshot::default()
        };

        let metadata = to_track_metadata(&snapshot);
        assert!(metadata.pre_emphasis);
        assert_eq!(metadata.extra.get("my_note").map(String::as_str), Some("keep me"));
        assert_eq!(
            metadata
                .extra
                .get(&format!(
                    "{}my_note",
                    crate::convert::pipeline::SOURCE_TEXT_TAG_EXTRA_PREFIX
                ))
                .map(String::as_str),
            Some("keep me")
        );
    }

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
    fn compact_tag_growth_rewrites_once_then_reuses_seeded_padding_in_place() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("compact.dsf");
        let mut tag = id3::Tag::new();
        tag.add_frame(id3::Frame::text("TIT2", "compact"));
        let mut metadata = Vec::new();
        tag.write_to(&mut metadata, id3::Version::Id3v24)
            .expect("serialize compact fixture tag");
        write_test_dsf_fixture(&path, Some(&metadata)).expect("write compact tagged fixture");
        let original = std::fs::read(&path).expect("read compact fixture");
        let metadata_offset = u64::from_le_bytes(
            original[20..28].try_into().expect("metadata pointer"),
        );
        assert_eq!(original.len() as u64 - metadata_offset, metadata.len() as u64);

        let first_progress = std::sync::Mutex::new(Vec::new());
        write_with_control(
            &path,
            &[DsfTagChange {
                canonical_key: "TITLE".into(),
                value: Some("a".repeat(64 * 1024)),
            }],
            &|| false,
            &|update| first_progress.lock().expect("first progress lock").push(update.phase),
        )
        .expect("first growing edit rewrites once");
        let after_first = std::fs::read(&path).expect("read first rewrite");
        // The header's declared-file-size field (bytes 12..20) MUST change —
        // the rewrite grows the file by the new tag + seeded reserve. Every
        // other prefix byte (magic/header, metadata pointer, fmt/data chunks,
        // audio) must be preserved.
        assert_eq!(&after_first[..12], &original[..12]);
        assert_eq!(
            u64::from_le_bytes(after_first[12..20].try_into().expect("size field")),
            after_first.len() as u64,
            "header must declare the grown file size",
        );
        assert_eq!(
            &after_first[20..metadata_offset as usize],
            &original[20..metadata_offset as usize],
            "the one-time rewrite must preserve the metadata pointer and every audio byte",
        );
        let first_allocation = after_first.len() as u64 - metadata_offset;
        assert!(
            first_allocation >= REWRITE_PADDING_BYTES + 64 * 1024,
            "rewrite must seed at least 1 MiB of reusable metadata allocation"
        );
        let first_phases = first_progress.into_inner().expect("first progress values");
        assert!(first_phases.contains(&DsfWriteProgressPhase::CopyingPrefix));
        assert!(!first_phases.contains(&DsfWriteProgressPhase::Journaling));

        let second_progress = std::sync::Mutex::new(Vec::new());
        write_with_control(
            &path,
            &[DsfTagChange {
                canonical_key: "TITLE".into(),
                value: Some("b".repeat(96 * 1024)),
            }],
            &|| false,
            &|update| second_progress.lock().expect("second progress lock").push(update.phase),
        )
        .expect("second growing edit fits seeded allocation");
        let after_second = std::fs::read(&path).expect("read in-place update");
        assert_eq!(
            &after_second[..metadata_offset as usize],
            &after_first[..metadata_offset as usize],
            "the follow-up edit must not rewrite container/audio bytes",
        );
        assert_eq!(after_second.len(), after_first.len());
        let expected_title = "b".repeat(96 * 1024);
        assert_eq!(
            read(&path).expect("read second title").first("TITLE"),
            Some(expected_title.as_str())
        );
        let second_phases = second_progress.into_inner().expect("second progress values");
        assert!(second_phases.contains(&DsfWriteProgressPhase::Journaling));
        assert!(second_phases.contains(&DsfWriteProgressPhase::WritingTail));
        assert!(!second_phases.contains(&DsfWriteProgressPhase::CopyingPrefix));
    }

    #[test]
    fn untagged_first_tag_appends_padded_id3_without_moving_audio_bytes() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("untagged.dsf");
        write_test_dsf_fixture(&path, None).expect("write untagged fixture");
        let original = std::fs::read(&path).expect("read untagged fixture");
        let original_size = original.len() as u64;
        let progress = std::sync::Mutex::new(Vec::new());

        write_with_control(
            &path,
            &[DsfTagChange {
                canonical_key: "MY_NOTE".into(),
                value: Some("first tag".into()),
            }],
            &|| false,
            &|update| progress.lock().expect("progress lock").push(update.phase),
        )
        .expect("append first DSF tag");

        let after = std::fs::read(&path).expect("read appended fixture");
        assert_eq!(
            u64::from_le_bytes(after[20..28].try_into().expect("metadata pointer")),
            original_size
        );
        assert_eq!(
            u64::from_le_bytes(after[12..20].try_into().expect("declared size")),
            after.len() as u64
        );
        assert_eq!(&after[28..original.len()], &original[28..]);
        assert_eq!(&after[original.len()..original.len() + 3], b"ID3");
        assert!(after.len() as u64 >= original_size + REWRITE_PADDING_BYTES);
        assert_eq!(
            read(&path).expect("read appended tag").first("MY_NOTE"),
            Some("first tag")
        );
        let phases = progress.into_inner().expect("progress values");
        assert!(phases.contains(&DsfWriteProgressPhase::Journaling));
        assert!(phases.contains(&DsfWriteProgressPhase::WritingTail));
        assert!(phases.contains(&DsfWriteProgressPhase::Publishing));
        assert!(!phases.contains(&DsfWriteProgressPhase::CopyingPrefix));
        assert!(!tail_journal_path(&path).exists());
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
    fn cancelling_untagged_append_restores_exact_original_without_prefix_copy() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("untagged.dsf");
        write_test_dsf_fixture(&path, None).expect("write untagged fixture");
        let original = std::fs::read(&path).expect("read original fixture");
        let cancelled = std::sync::atomic::AtomicBool::new(false);
        let phases = std::sync::Mutex::new(Vec::new());

        let error = write_with_control(
            &path,
            &[DsfTagChange {
                canonical_key: "TITLE".into(),
                value: Some("x".repeat(2 * 1024 * 1024)),
            }],
            &|| cancelled.load(std::sync::atomic::Ordering::SeqCst),
            &|update| {
                phases.lock().expect("phase lock").push(update.phase);
                if update.phase == DsfWriteProgressPhase::WritingTail
                    && update.bytes_done >= COPY_CHUNK_BYTES as u64
                {
                    cancelled.store(true, std::sync::atomic::Ordering::SeqCst);
                }
            },
        )
        .expect_err("mid-append cancellation must abort");

        assert!(error.contains("metadata save cancelled while writing DSF metadata tail"));
        assert!(error.contains("original tail was restored"));
        assert_eq!(std::fs::read(&path).expect("read unchanged fixture"), original);
        let phases = phases.into_inner().expect("phase values");
        assert!(phases.contains(&DsfWriteProgressPhase::Journaling));
        assert!(phases.contains(&DsfWriteProgressPhase::WritingTail));
        assert!(phases.contains(&DsfWriteProgressPhase::Recovering));
        assert!(!phases.contains(&DsfWriteProgressPhase::CopyingPrefix));
        assert!(!tail_journal_path(&path).exists());
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
            encode_legacy_tail_journal(
                offset,
                &original_tail,
                &recovery_identity,
                TAIL_JOURNAL_PREPARED,
            ),
        )
        .expect("write prepared journal");
        let inspection = inspect_tail_journal(&path)
            .expect("inspect prepared journal")
            .expect("prepared journal present");
        assert_eq!(inspection.state, "prepared");
        assert_eq!(inspection.operation, "replace-tail");
        assert_eq!(inspection.original_file_size, original.len() as u64);
        assert_eq!(inspection.committed_file_size, original.len() as u64);
        assert!(recover_tail_journal_for_target(&path)
            .expect("recover prepared journal")
            .is_some());
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
        let inspection = inspect_tail_journal(&path)
            .expect("inspect committed journal")
            .expect("committed journal present");
        assert_eq!(inspection.state, "committed");
        assert_eq!(inspection.operation, "replace-tail");
        assert!(recover_tail_journal_for_target(&path)
            .expect("clean committed journal")
            .is_some());
        assert_eq!(std::fs::read(&path).expect("read committed target"), committed);
        assert!(!journal.exists());
    }

    #[test]
    fn prepared_append_journal_restores_header_and_truncates_while_committed_keeps_tag() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("append-recovery.dsf");
        write_test_dsf_fixture(&path, None).expect("write untagged fixture");
        let original = std::fs::read(&path).expect("read original fixture");
        let original_size = original.len() as u64;
        let original_header_patch: [u8; DSF_HEADER_PATCH_LEN as usize] = original
            [DSF_HEADER_PATCH_OFFSET as usize
                ..(DSF_HEADER_PATCH_OFFSET + DSF_HEADER_PATCH_LEN) as usize]
            .try_into()
            .expect("original DSF header patch");
        let mut tag = id3::Tag::new();
        tag.add_frame(id3::Frame::text("TIT2", "Recovered append"));
        let mut encoded = Vec::new();
        tag.write_to(&mut encoded, id3::Version::Id3v24)
            .expect("serialize append tag");
        let padded = pad_id3_with_rewrite_reserve(&encoded).expect("pad append tag");
        let committed_size = original_size + padded.len() as u64;
        let mut identity_file = std::fs::File::open(&path).expect("open identity fixture");
        let recovery_identity = dsf_append_recovery_identity(
            &mut identity_file,
            original_size,
        )
        .expect("compute append recovery identity");
        let journal = tail_journal_path(&path);

        std::fs::write(
            &journal,
            encode_append_journal(
                &original_header_patch,
                original_size,
                committed_size,
                &recovery_identity,
                TAIL_JOURNAL_PREPARED,
            ),
        )
        .expect("write prepared append journal");
        {
            let mut file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .expect("open prepared append target");
            file.seek(SeekFrom::Start(original_size))
                .expect("seek partial append");
            file.write_all(&padded[..padded.len() / 2])
                .expect("write partial append");
            file.seek(SeekFrom::Start(DSF_HEADER_PATCH_OFFSET))
                .expect("seek partial header patch");
            file.write_all(&committed_size.to_le_bytes())
                .expect("write only declared size");
        }
        let inspection = inspect_tail_journal(&path)
            .expect("inspect prepared append journal")
            .expect("prepared append journal present");
        assert_eq!(inspection.state, "prepared");
        assert_eq!(inspection.operation, "append-tag");
        assert_eq!(inspection.original_file_size, original_size);
        assert_eq!(inspection.committed_file_size, committed_size);
        assert!(recover_tail_journal_for_target(&path)
            .expect("recover prepared append")
            .is_some());
        assert_eq!(std::fs::read(&path).expect("read restored target"), original);
        assert!(!journal.exists());

        {
            let mut file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .expect("open committed append target");
            file.seek(SeekFrom::Start(original_size))
                .expect("seek full append");
            file.write_all(&padded).expect("write full append");
            file.seek(SeekFrom::Start(DSF_HEADER_PATCH_OFFSET))
                .expect("seek header patch");
            file.write_all(&committed_size.to_le_bytes())
                .expect("write declared size");
            file.write_all(&original_size.to_le_bytes())
                .expect("write metadata pointer");
        }
        std::fs::write(
            &journal,
            encode_append_journal(
                &original_header_patch,
                original_size,
                committed_size,
                &recovery_identity,
                TAIL_JOURNAL_COMMITTED,
            ),
        )
        .expect("write committed append journal");
        let committed = std::fs::read(&path).expect("read committed append");
        let inspection = inspect_tail_journal(&path)
            .expect("inspect committed append journal")
            .expect("committed append journal present");
        assert_eq!(inspection.state, "committed");
        assert_eq!(inspection.operation, "append-tag");
        assert!(recover_tail_journal_for_target(&path)
            .expect("retire committed append journal")
            .is_some());
        assert_eq!(std::fs::read(&path).expect("read retained append"), committed);
        assert_eq!(
            read(&path).expect("read retained append tag").first("TITLE"),
            Some("Recovered append")
        );
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
        let backup = crate::db::Database::backup_path_for(&path);

        let mut current_tag = id3::Tag::new();
        current_tag.add_frame(id3::Frame::text("TIT2", "Current generation"));
        let mut current_metadata = Vec::new();
        current_tag
            .write_to(&mut current_metadata, id3::Version::Id3v24)
            .expect("encode current tag");
        write_test_dsf_fixture(&path, Some(&current_metadata)).expect("write current DSF");
        let current = std::fs::read(&path).expect("read current DSF");

        let mut older_tag = id3::Tag::new();
        older_tag.add_frame(id3::Frame::text("TIT2", "Older generation"));
        let mut older_metadata = Vec::new();
        older_tag
            .write_to(&mut older_metadata, id3::Version::Id3v24)
            .expect("encode older tag");
        write_test_dsf_fixture(&backup, Some(&older_metadata)).expect("write older backup DSF");
        let older = std::fs::read(&backup).expect("read older backup DSF");

        let inspection = inspect_legacy_backup(&path).expect("inspect marker");
        assert_eq!(inspection.target_bytes, current.len() as u64);
        assert_eq!(inspection.marker_bytes, older.len() as u64);
        assert!(!inspection.byte_identical);
        resolve_legacy_backup(&path, DsfLegacyBackupResolution::KeepCurrent)
            .expect("keep current");
        assert_eq!(std::fs::read(&path).expect("read current"), current);
        assert!(!backup.exists());

        let mut restored_tag = id3::Tag::new();
        restored_tag.add_frame(id3::Frame::text("TIT2", "Restored generation"));
        let mut restored_metadata = Vec::new();
        restored_tag
            .write_to(&mut restored_metadata, id3::Version::Id3v24)
            .expect("encode restored tag");
        write_test_dsf_fixture(&backup, Some(&restored_metadata))
            .expect("write replacement backup DSF");
        let restored = std::fs::read(&backup).expect("read replacement backup DSF");
        resolve_legacy_backup(&path, DsfLegacyBackupResolution::RestoreBackup)
            .expect("restore backup");
        assert_eq!(std::fs::read(&path).expect("read restored"), restored);
        assert!(!backup.exists());
    }

    #[test]
    fn restore_backup_refuses_non_dsf_marker_without_mutating_either_generation() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("album.dsf");
        write_test_dsf_fixture(&path, None).expect("write current DSF");
        let current = std::fs::read(&path).expect("read current DSF");
        let backup = crate::db::Database::backup_path_for(&path);
        let invalid = b"not a DSF rollback generation";
        std::fs::write(&backup, invalid).expect("write invalid backup");

        let error = resolve_legacy_backup(&path, DsfLegacyBackupResolution::RestoreBackup)
            .expect_err("non-DSF backup must be refused");

        assert_eq!(
            error,
            format!(
                "refusing to restore legacy DSF rollback marker '{}': expected DSF DSD chunk marker",
                backup.display()
            )
        );
        assert_eq!(std::fs::read(&path).expect("read untouched target"), current);
        assert_eq!(std::fs::read(&backup).expect("read retained marker"), invalid);
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


    #[cfg(unix)]
    #[test]
    fn native_filename_authority_paths_are_collision_free() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let first = temp.path().join(OsString::from_vec(b"first-\xff.dsf".to_vec()));
        let second = temp.path().join(OsString::from_vec(b"second-\xfe.dsf".to_vec()));
        let literal = temp.path().join("audio.dsf");
        for path in [&first, &second, &literal] {
            write_test_dsf_fixture(path, None).expect("write DSF fixture");
        }

        assert_ne!(tail_journal_path(&first), tail_journal_path(&second));
        assert_ne!(tail_journal_path(&first), tail_journal_path(&literal));
        assert_ne!(tail_journal_path(&second), tail_journal_path(&literal));
        assert_ne!(
            crate::config::store_lock_authority_path(&first).expect("first lock"),
            crate::config::store_lock_authority_path(&second).expect("second lock")
        );
        assert_ne!(
            crate::config::store_lock_authority_path(&first).expect("first lock"),
            crate::config::store_lock_authority_path(&literal).expect("literal lock")
        );
        assert_eq!(
            legacy_tail_journal_path(&first),
            legacy_tail_journal_path(&literal),
            "the compatibility path collision is intentional and must never be used for new journals"
        );
    }

    #[cfg(unix)]
    #[test]
    fn hashed_journal_resolves_by_native_filename_before_generation_identity() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let first = temp.path().join(OsString::from_vec(b"one-\xff.dsf".to_vec()));
        let second = temp.path().join(OsString::from_vec(b"two-\xfe.dsf".to_vec()));
        write_test_dsf_fixture(&first, None).expect("write first DSF");
        std::fs::copy(&first, &second).expect("make byte-identical second DSF");

        let original = std::fs::read(&first).expect("read original DSF");
        let mut identity_file = std::fs::File::open(&first).expect("open identity target");
        let identity = dsf_append_recovery_identity(&mut identity_file, original.len() as u64)
            .expect("compute append identity");
        let journal = tail_journal_path(&first);
        let committed_size = original.len() as u64 + 10;
        let header = TailJournalHeader::append_tag(
            TAIL_JOURNAL_PREPARED,
            original.len() as u64,
            committed_size,
            identity,
        );
        // An AppendTag journal carries its 16-byte original header patch as
        // payload; a header-only journal is (correctly) rejected as torn.
        let mut journal_bytes = encode_tail_journal_header(header).to_vec();
        journal_bytes.extend_from_slice(&original[12..28]);
        std::fs::write(&journal, journal_bytes).expect("write hashed append journal");

        let candidates = dsf_targets_in_directory(temp.path()).expect("enumerate targets");
        assert_eq!(
            resolve_tail_journal_target(&journal, &candidates).expect("resolve hashed journal"),
            Some(first),
            "byte-identical targets must not make a filename-bound hashed authority ambiguous",
        );
        assert_ne!(journal, tail_journal_path(&second));
    }

    #[cfg(unix)]
    #[test]
    fn journal_publication_is_atomic_and_never_replaces_an_existing_authority() {
        let temp = tempfile::tempdir().expect("tempdir");
        let destination = temp.path().join(".tonepoet-dsf-tail-authority.journal");
        let first = temp.path().join("first.tmp");
        let second = temp.path().join("second.tmp");
        std::fs::write(&first, b"first authority").expect("write first source");
        std::fs::write(&second, b"second authority").expect("write second source");

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let spawn = |source: PathBuf| {
            let barrier = std::sync::Arc::clone(&barrier);
            let destination = destination.clone();
            std::thread::spawn(move || {
                barrier.wait();
                publish_file_noreplace(&source, &destination)
            })
        };
        let first_thread = spawn(first.clone());
        let second_thread = spawn(second.clone());
        barrier.wait();
        let first_result = first_thread.join().expect("first publisher");
        let second_result = second_thread.join().expect("second publisher");

        assert_eq!(usize::from(first_result.is_ok()) + usize::from(second_result.is_ok()), 1);
        let loser = if let Err(error) = first_result {
            error
        } else {
            second_result.expect_err("second publisher must lose")
        };
        assert_eq!(loser.kind(), std::io::ErrorKind::AlreadyExists);
        let published = std::fs::read(&destination).expect("read published authority");
        assert!(published == b"first authority" || published == b"second authority");
    }

    #[cfg(unix)]
    #[test]
    fn concurrent_non_utf8_and_literal_dsf_writes_keep_independent_authority() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let non_utf = temp.path().join(OsString::from_vec(b"track-\xff.dsf".to_vec()));
        let literal = temp.path().join("audio.dsf");
        write_test_dsf_fixture(&non_utf, None).expect("write non-UTF DSF");
        write_test_dsf_fixture(&literal, None).expect("write literal DSF");

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let spawn = |path: PathBuf, title: &'static str| {
            let barrier = std::sync::Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                write_with_backup(
                    &path,
                    &[DsfTagChange {
                        canonical_key: "TITLE".to_string(),
                        value: Some(title.to_string()),
                    }],
                )
                .map(|warning| (path, warning))
            })
        };
        let first = spawn(non_utf.clone(), "Non-UTF title");
        let second = spawn(literal.clone(), "Literal title");
        barrier.wait();
        let (first_path, first_warning) = first.join().expect("non-UTF writer").expect("non-UTF save");
        let (second_path, second_warning) = second.join().expect("literal writer").expect("literal save");

        assert_eq!(first_path, non_utf);
        assert_eq!(second_path, literal);
        assert!(first_warning.is_none());
        assert!(second_warning.is_none());
        assert_eq!(read(&non_utf).expect("read non-UTF result").first("TITLE"), Some("Non-UTF title"));
        assert_eq!(read(&literal).expect("read literal result").first("TITLE"), Some("Literal title"));
        assert!(!tail_journal_path(&non_utf).exists());
        assert!(!tail_journal_path(&literal).exists());
        assert!(!legacy_tail_journal_path(&non_utf).exists());
    }

    #[cfg(unix)]
    #[test]
    fn legacy_fallback_journal_is_attributed_by_embedded_identity_not_filename() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let target = temp.path().join(OsString::from_vec(b"nonutf-\xff.dsf".to_vec()));
        let literal = temp.path().join("audio.dsf");
        let mut target_tag = id3::Tag::new();
        target_tag.add_frame(id3::Frame::text("TIT2", "non-UTF target"));
        let mut target_metadata = Vec::new();
        target_tag
            .write_to(&mut target_metadata, id3::Version::Id3v24)
            .expect("encode target tag");
        let mut literal_tag = id3::Tag::new();
        literal_tag.add_frame(id3::Frame::text("TIT2", "literal audio"));
        let mut literal_metadata = Vec::new();
        literal_tag
            .write_to(&mut literal_metadata, id3::Version::Id3v24)
            .expect("encode literal tag");
        write_test_dsf_fixture(&target, Some(&target_metadata)).expect("write target");
        write_test_dsf_fixture(&literal, Some(&literal_metadata)).expect("write literal");
        let original_target = std::fs::read(&target).expect("read target");
        let original_literal = std::fs::read(&literal).expect("read literal");
        let offset = u64::from_le_bytes(original_target[20..28].try_into().expect("metadata pointer"));
        let original_tail = original_target[offset as usize..].to_vec();
        let mut identity_file = std::fs::File::open(&target).expect("open identity target");
        let identity = dsf_recovery_identity(&mut identity_file, offset, original_target.len() as u64)
            .expect("compute target identity");
        let legacy = legacy_tail_journal_path(&target);
        assert_eq!(legacy, legacy_tail_journal_path(&literal));
        std::fs::write(
            &legacy,
            encode_tail_journal(offset, &original_tail, &identity, TAIL_JOURNAL_PREPARED),
        )
        .expect("write fallback journal");
        let mut torn = original_target.clone();
        torn[offset as usize..].fill(0xa5);
        std::fs::write(&target, torn).expect("tear target tail");

        let messages = recover_stale_writes_in_directory(temp.path());

        assert_eq!(std::fs::read(&target).expect("read recovered target"), original_target);
        assert_eq!(std::fs::read(&literal).expect("read untouched literal"), original_literal);
        assert!(!legacy.exists());
        assert!(messages.iter().any(|message| message.contains("Recovered DSF metadata tail journal")));
    }

    #[test]
    fn id3_picture_type_byte_mapping_round_trips_every_value() {
        for code in u8::MIN..=u8::MAX {
            assert_eq!(
                u8::from(backend::picture_type_from_code(code)),
                code,
                "APIC picture-type code {code:#04x} must round-trip"
            );
        }
    }

    #[test]
    fn dsf_artwork_uses_tail_journal_and_rolls_back_without_full_file_backup() {
        use std::io::{Seek, SeekFrom};

        fn picture_count(path: &Path, code: u8) -> usize {
            let DsfMetadataLocation::Id3 { offset, .. } = inspect_dsf_metadata_location(path)
                .expect("inspect DSF metadata")
            else {
                return 0;
            };
            let mut file = std::fs::File::open(path).expect("open DSF tag");
            file.seek(SeekFrom::Start(offset)).expect("seek DSF tag");
            let tag = id3::Tag::read_from2(&mut file).expect("read DSF tag");
            tag.pictures()
                .filter(|picture| picture.picture_type == backend::picture_type_from_code(code))
                .count()
        }

        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("artwork.dsf");
        write_test_dsf_fixture(&path, None).expect("write untagged fixture");
        let original = std::fs::read(&path).expect("read original fixture");
        let audio_end = original.len();
        let phases = std::sync::Mutex::new(Vec::new());
        let (snapshot, warning) = write_artwork_with_control(
            &path,
            3,
            "image/png",
            b"synthetic-png-payload",
            &|| false,
            &|update| phases.lock().expect("phase lock").push(update.phase),
        )
        .expect("write DSF artwork");

        assert!(warning.is_none());
        // The append patches header bytes 12..28 (declared size + metadata
        // pointer) by design; every other original byte must be preserved.
        let tagged = std::fs::read(&path).expect("read artwork fixture");
        assert_eq!(&tagged[..12], &original[..12]);
        assert_eq!(&tagged[28..audio_end], &original[28..]);
        assert_eq!(
            u64::from_le_bytes(tagged[12..20].try_into().expect("size field")),
            tagged.len() as u64,
        );
        assert_eq!(
            u64::from_le_bytes(tagged[20..28].try_into().expect("pointer field")),
            audio_end as u64,
        );
        assert_eq!(picture_count(&path, 3), 1);
        assert!(!crate::db::Database::backup_path_for(&path).exists());
        assert!(!tail_journal_path(&path).exists());
        let phases = phases.into_inner().expect("phase values");
        assert!(phases.contains(&DsfWriteProgressPhase::Journaling));
        assert!(phases.contains(&DsfWriteProgressPhase::WritingTail));
        assert!(!phases.contains(&DsfWriteProgressPhase::CopyingPrefix));

        restore_artwork_snapshot(&path, &snapshot).expect("rollback artwork snapshot");
        assert_eq!(std::fs::read(&path).expect("read rolled-back artwork fixture"), original);
        assert_eq!(picture_count(&path, 3), 0);
        assert!(!tail_journal_path(&path).exists());
        assert!(!crate::db::Database::backup_path_for(&path).exists());
    }

    #[test]
    fn cancelling_dsf_artwork_append_restores_exact_original() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("cancel-artwork.dsf");
        write_test_dsf_fixture(&path, None).expect("write untagged fixture");
        let original = std::fs::read(&path).expect("read original fixture");
        let cancelled = std::sync::atomic::AtomicBool::new(false);
        let image = vec![0x5a; 2 * 1024 * 1024];

        let error = write_artwork_with_control(
            &path,
            3,
            "image/jpeg",
            &image,
            &|| cancelled.load(std::sync::atomic::Ordering::SeqCst),
            &|update| {
                if update.phase == DsfWriteProgressPhase::WritingTail
                    && update.bytes_done >= COPY_CHUNK_BYTES as u64
                {
                    cancelled.store(true, std::sync::atomic::Ordering::SeqCst);
                }
            },
        )
        .expect_err("mid-write cancellation must roll back artwork append");

        assert!(error.contains("metadata save cancelled while writing DSF metadata tail"));
        assert_eq!(std::fs::read(&path).expect("read restored fixture"), original);
        assert!(!tail_journal_path(&path).exists());
        assert!(!crate::db::Database::backup_path_for(&path).exists());
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
