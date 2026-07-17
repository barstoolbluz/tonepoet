//! DSF ID3v2 metadata support.
//!
//! All direct `id3` crate calls are isolated in `backend`. The crate supplies
//! generic ID3 stream parsing and serialization but no DSF container adapter,
//! so this module validates the DSF metadata pointer and performs same-directory
//! atomic container replacement itself.

use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DsfTagSnapshot {
    /// Canonical editor display key -> ordered, distinct values.
    pub fields: BTreeMap<String, Vec<String>>,
}

impl DsfTagSnapshot {
    pub fn first(&self, key: &str) -> Option<&str> {
        self.fields
            .get(key)
            .and_then(|values| values.iter().find(|value| !value.trim().is_empty()))
            .map(String::as_str)
    }

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
    if !is_dsf(path) {
        return Err(format!("'{}' is not a DSF file", path.display()));
    }
    backend::read(path)
        .map(canonicalize_snapshot)
        .map_err(|error| format!("failed to read DSF ID3 tags from '{}': {error}", path.display()))
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
    if changes.is_empty() {
        return Ok(None);
    }
    let resolved = validate_and_resolve_write(path, changes)?;
    apply_with_backup(path, || apply_resolved(path, &resolved))
}

/// Apply DSF ID3 changes without allocating or retiring a full-file rollback
/// marker. This is intentionally crate-private: callers must already own an
/// external transaction whose backup and journal lifecycle encloses this call.
pub(crate) fn write_without_backup(path: &Path, changes: &[DsfTagChange]) -> Result<(), String> {
    if changes.is_empty() {
        return Ok(());
    }
    let resolved = validate_and_resolve_write(path, changes)?;
    apply_resolved(path, &resolved)
}

fn validate_and_resolve_write(
    path: &Path,
    changes: &[DsfTagChange],
) -> Result<Vec<DsfTagChange>, String> {
    if !is_dsf(path) {
        return Err(format!("'{}' is not a DSF file", path.display()));
    }
    reject_symlinked_write_path(path)?;
    let resolved = resolve_changes(changes)?;
    backend::validate_writable_layout(path)?;
    Ok(resolved)
}

fn reject_symlinked_write_path(path: &Path) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("inspect DSF write path '{}': {error}", path.display()))?;
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "refusing to mutate symlinked DSF path '{}'; edit the resolved target explicitly so backup and replacement authority remain unambiguous",
            path.display()
        ));
    }
    Ok(())
}

fn apply_resolved(path: &Path, changes: &[DsfTagChange]) -> Result<(), String> {
    backend::apply(path, changes)
        .map_err(|error| format!("failed to save DSF ID3 tags to '{}': {error}", path.display()))
}

fn apply_with_backup<F>(path: &Path, apply: F) -> Result<Option<String>, String>
where
    F: FnOnce() -> Result<(), String>,
{
    let backup = crate::db::Database::backup_path_for(path);
    crate::db::Database::create_backup_for(path, &backup)
        .map_err(|error| format!("backup failed for '{}': {error}", path.display()))?;

    match apply() {
        Ok(()) => match std::fs::remove_file(&backup) {
            Ok(()) => Ok(None),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Some(format!(
                "DSF metadata write for '{}' committed, but rollback marker '{}' was already absent during cleanup",
                path.display(),
                backup.display()
            ))),
            Err(error) => Ok(Some(format!(
                "DSF metadata write for '{}' committed, but rollback marker '{}' could not be removed: {error}",
                path.display(),
                backup.display()
            ))),
        },
        Err(error) => match crate::db::Database::restore_backup_for(path, &backup) {
            Ok(()) => Err(error),
            Err(restore_error) => Err(format!(
                "{error}; rollback could not be completed for '{}' from '{}': {restore_error}",
                path.display(),
                backup.display()
            )),
        },
    }
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
    use std::io::{Read, Seek, SeekFrom, Write};
    use std::path::{Path, PathBuf};

    pub(super) fn read(path: &Path) -> Result<DsfTagSnapshot, String> {
        let location = super::inspect_dsf_metadata_location(path)?;
        let tag = read_tag(path, location)?;
        Ok(snapshot_from_tag(&tag))
    }

    pub(super) fn validate_writable_layout(path: &Path) -> Result<(), String> {
        let location = super::inspect_dsf_metadata_location(path)?;
        if let DsfMetadataLocation::Id3 {
            tag_end,
            file_size,
            ..
        } = location
        {
            if tag_end != file_size {
                return Err(format!(
                    "refusing to replace DSF ID3 metadata in '{}': {} trailing byte(s) follow the declared tag, so their ownership is ambiguous",
                    path.display(),
                    file_size - tag_end
                ));
            }
        }
        let _ = read_tag(path, location)?;
        Ok(())
    }

    pub(super) fn apply(path: &Path, changes: &[DsfTagChange]) -> Result<(), String> {
        let location = super::inspect_dsf_metadata_location(path)?;
        let mut tag = read_tag(path, location)?;
        apply_changes_to_tag(&mut tag, changes);

        let mut encoded = Vec::new();
        tag.write_to(&mut encoded, Version::Id3v24)
            .map_err(|error| format!("encode ID3v2.4 tag: {error}"))?;
        if !encoded.starts_with(b"ID3") {
            return Err("ID3 backend produced bytes without an ID3 marker".to_string());
        }
        rewrite_container(path, location, &encoded)
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

    fn rewrite_container(
        path: &Path,
        location: DsfMetadataLocation,
        encoded_tag: &[u8],
    ) -> Result<(), String> {
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
        let mut published = false;
        let result = (|| {
            let copied = std::io::copy(&mut (&mut source).take(metadata_offset), &mut temporary)
                .map_err(|error| format!("copy DSF audio prefix to temporary file: {error}"))?;
            if copied != metadata_offset {
                return Err(format!(
                    "DSF source '{}' ended after {copied} bytes while copying the required {metadata_offset}-byte audio prefix",
                    path.display()
                ));
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
            temporary
                .write_all(encoded_tag)
                .map_err(|error| format!("write encoded ID3 tag to DSF temporary file: {error}"))?;
            temporary
                .sync_all()
                .map_err(|error| format!("sync DSF temporary file: {error}"))?;
            drop(temporary);
            crate::config::replace_config_file(&temporary_path, path)
                .map_err(|error| format!("atomically replace DSF file: {error}"))?;
            published = true;
            crate::config::sync_parent_dir(parent)
                .map_err(|error| format!("sync DSF parent directory after replacement: {error}"))?;
            Ok(())
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
    fn metadata_pointer_requires_an_exact_valid_audio_chunk_boundary() {
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
        std::fs::write(&path, bytes).expect("publish corrupt data boundary");

        let error = read(&path).expect_err("corrupt chunk boundary must fail closed");
        assert_eq!(
            error,
            format!(
                "failed to read DSF ID3 tags from '{}': invalid DSF chunk layout in '{}': 1 byte(s) remain before the metadata boundary at 8284, fewer than the 12-byte chunk header",
                path.display(),
                path.display()
            )
        );
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
                "refusing to mutate symlinked DSF path '{}'; edit the resolved target explicitly so backup and replacement authority remain unambiguous",
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
    fn nonzero_metadata_pointer_without_id3_marker_fails_closed() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("corrupt-metadata.dsf");
        write_test_dsf_fixture(&path, Some(b"NOT-AN-ID3-TAG"))
            .expect("write DSF with corrupt metadata area");

        let error = read(&path).expect_err("declared corrupt metadata must not become empty tags");

        assert_eq!(
            error,
            format!(
                "failed to read DSF ID3 tags from '{}': invalid DSF metadata area in '{}': header declares metadata at offset 8284, but no ID3 marker is present",
                path.display(),
                path.display()
            )
        );
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
    fn backup_wrapper_restores_exact_bytes_after_partial_failure() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("album.dsf");
        std::fs::write(&path, b"original DSF bytes").expect("write original");

        let result = apply_with_backup(&path, || {
            std::fs::write(&path, b"partially rewritten DSF bytes")
                .map_err(|error| error.to_string())?;
            Err("synthetic ID3 failure".to_string())
        });

        assert_eq!(
            result.expect_err("synthetic failure must propagate"),
            "synthetic ID3 failure"
        );
        assert_eq!(std::fs::read(&path).expect("read restored DSF"), b"original DSF bytes");
        assert!(!crate::db::Database::backup_path_for(&path).exists());
    }

    #[test]
    fn backup_wrapper_commits_exact_bytes_and_removes_marker() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("album.dsf");
        std::fs::write(&path, b"original DSF bytes").expect("write original");

        let warning = apply_with_backup(&path, || {
            std::fs::write(&path, b"tagged DSF bytes").map_err(|error| error.to_string())
        })
        .expect("commit synthetic DSF write");

        assert_eq!(warning, None);
        assert_eq!(std::fs::read(&path).expect("read committed DSF"), b"tagged DSF bytes");
        assert!(!crate::db::Database::backup_path_for(&path).exists());
    }
}
