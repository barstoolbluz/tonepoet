//! Authoritative metadata-writer routing and numbering-value capabilities.
//!
//! This module is deliberately UI-neutral. The metadata persistence layer uses
//! [`metadata_persistence_route_for_path`] to select its write route. Feature
//! layers query [`metadata_numbering_capability_for_path`] so they evaluate
//! the actual backend selected by that route. Unknown routes and unclassified Lofty tag
//! types fail closed: adding support requires an explicit backend declaration
//! here rather than inheriting an unsafe default.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use lofty::file::TaggedFileExt;
use lofty::tag::{ItemKey, TagType};

const FLAC_MAGIC: &[u8; 4] = b"fLaC";
const ID3V2_HEADER_LEN: u64 = 10;
const ID3V2_FOOTER_LEN: u64 = 10;
/// A prepended ID3v2 tag larger than this is treated as malformed rather than
/// allowing attacker-controlled offsets or unbounded allocation/seek behavior.
pub(crate) const MAX_ID3V2_FLAC_PREFIX_LEN: u64 = 16 * 1024 * 1024;

// --- Shared tolerant APEv2 reader -------------------------------------------------
//
// This parser is intentionally UI-neutral. It is the single bounded read seam used
// by both metadata-editor wrappers and conversion materializers when Lofty rejects
// an otherwise readable APEv2 tag (for example, because one physical key is outside
// the APEv2 key grammar). Invalid physical keys are retained in `NativeApeTag` for
// the recovery writer, but omitted from neutral rows and disclosed structurally.

pub(crate) const APE_SIGNATURE: &[u8; 8] = b"APETAGEX";
pub(crate) const APE_DESCRIPTOR_LEN: usize = 32;
pub(crate) const APE_VERSION_2: u32 = 2_000;
pub(crate) const APE_FLAG_HEADER_PRESENT: u32 = 1 << 31;
pub(crate) const APE_FLAG_IS_HEADER: u32 = 1 << 29;
pub(crate) const APE_ITEM_READ_ONLY: u32 = 1;
pub(crate) const APE_ITEM_TYPE_MASK: u32 = 0b110;
pub(crate) const APE_ITEM_TYPE_TEXT: u32 = 0;
pub(crate) const APE_ITEM_TYPE_BINARY: u32 = 0b010;
pub(crate) const APE_ITEM_TYPE_LOCATOR: u32 = 0b100;
pub(crate) const MAX_NATIVE_APE_TAG_BYTES: u64 = 64 * 1024 * 1024;
const MAX_NATIVE_APE_ITEMS: u32 = 1_000_000;
const MIN_NATIVE_APE_ITEM_BYTES: usize = 9;

#[derive(Debug, Clone)]
pub(crate) struct NativeApeItem {
    pub(crate) raw: Vec<u8>,
    pub(crate) key_bytes: Vec<u8>,
    pub(crate) key: Option<String>,
    pub(crate) flags: u32,
    pub(crate) value: Vec<u8>,
}

impl NativeApeItem {
    pub(crate) fn item_type(&self) -> u32 {
        self.flags & APE_ITEM_TYPE_MASK
    }

    pub(crate) fn is_read_only(&self) -> bool {
        self.flags & APE_ITEM_READ_ONLY != 0
    }
}

#[derive(Debug, Clone)]
pub(crate) struct NativeApeTag {
    pub(crate) replace_start: u64,
    pub(crate) footer_end: u64,
    pub(crate) had_header: bool,
    pub(crate) items: Vec<NativeApeItem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NeutralApeRow {
    pub(crate) raw_key: String,
    pub(crate) canonical_key: String,
    pub(crate) item_key: ItemKey,
    pub(crate) value: String,
    pub(crate) is_binary: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NeutralApeWarning {
    pub(crate) path: std::path::PathBuf,
    pub(crate) escaped_keys: Vec<String>,
}

impl NeutralApeWarning {
    pub(crate) fn message(&self) -> String {
        format!(
            "{} invalid APE key{} skipped in '{}': {}",
            self.escaped_keys.len(),
            if self.escaped_keys.len() == 1 { "" } else { "s" },
            self.path.display(),
            self.escaped_keys
                .iter()
                .map(|key| format!("'{key}'"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

#[derive(Debug, Clone)]
pub(crate) struct NeutralApeReadOutcome {
    pub(crate) rows: Vec<NeutralApeRow>,
    pub(crate) warning: Option<NeutralApeWarning>,
}

fn u32_le_at(bytes: &[u8], offset: usize) -> Result<u32, String> {
    let value = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| "truncated APEv2 integer field".to_string())?;
    Ok(u32::from_le_bytes([value[0], value[1], value[2], value[3]]))
}

pub(crate) fn native_ape_error_is_eligible(err: &lofty::error::LoftyError) -> bool {
    use lofty::error::ErrorKind;

    matches!(
        err.kind(),
        ErrorKind::FileDecoding(decoding)
            if decoding.format() == Some(lofty::file::FileType::Ape)
    )
}

pub(crate) fn ape_key_is_valid(key: &[u8]) -> bool {
    if !(2..=255).contains(&key.len()) || !key.iter().all(|byte| (0x20..=0x7e).contains(byte)) {
        return false;
    }
    ![b"ID3".as_slice(), b"TAG".as_slice(), b"OGGS".as_slice(), b"MP+".as_slice()]
        .iter()
        .any(|reserved| key.eq_ignore_ascii_case(reserved))
}

pub(crate) fn display_escaped_ape_key(key: &[u8]) -> String {
    if let Ok(text) = std::str::from_utf8(key) {
        let mut escaped = String::new();
        for ch in text.chars() {
            match ch {
                '\\' => escaped.push_str("\\\\"),
                '\'' => escaped.push_str("\\'"),
                ch if ch.is_control() => escaped.extend(ch.escape_default()),
                ch => escaped.push(ch),
            }
        }
        return escaped;
    }

    key.iter()
        .map(|byte| format!("\\x{byte:02X}"))
        .collect::<String>()
}

pub(crate) fn optional_id3v1_start(
    file: &mut std::fs::File,
    file_len: u64,
) -> Result<Option<u64>, String> {
    if file_len < 128 {
        return Ok(None);
    }
    file.seek(SeekFrom::Start(file_len - 128))
        .map_err(|error| format!("seek trailing ID3v1 probe: {error}"))?;
    let mut signature = [0u8; 3];
    file.read_exact(&mut signature)
        .map_err(|error| format!("read trailing ID3v1 probe: {error}"))?;
    Ok((&signature == b"TAG").then_some(file_len - 128))
}


fn native_ape_item_capacity(item_count: u32, region_len: usize) -> usize {
    usize::try_from(item_count)
        .unwrap_or(usize::MAX)
        .min(region_len / MIN_NATIVE_APE_ITEM_BYTES)
}

pub(crate) fn read_native_ape_tag(path: &Path) -> Result<Option<NativeApeTag>, String> {
    let mut file = std::fs::File::open(path)
        .map_err(|error| format!("open '{}': {error}", path.display()))?;
    let file_len = file
        .metadata()
        .map_err(|error| format!("stat '{}': {error}", path.display()))?
        .len();
    let footer_end = optional_id3v1_start(&mut file, file_len)?.unwrap_or(file_len);
    if footer_end < APE_DESCRIPTOR_LEN as u64 {
        return Ok(None);
    }

    file.seek(SeekFrom::Start(footer_end - APE_DESCRIPTOR_LEN as u64))
        .map_err(|error| format!("seek APEv2 footer in '{}': {error}", path.display()))?;
    let mut footer = [0u8; APE_DESCRIPTOR_LEN];
    file.read_exact(&mut footer)
        .map_err(|error| format!("read APEv2 footer in '{}': {error}", path.display()))?;
    if &footer[..8] != APE_SIGNATURE {
        return Ok(None);
    }

    let version = u32_le_at(&footer, 8)?;
    if version != APE_VERSION_2 {
        return Err(format!(
            "unsupported APE tag version {version} in '{}'; native fallback accepts APEv2 only",
            path.display()
        ));
    }
    let tag_size = u64::from(u32_le_at(&footer, 12)?);
    let item_count = u32_le_at(&footer, 16)?;
    let footer_flags = u32_le_at(&footer, 20)?;
    if tag_size < APE_DESCRIPTOR_LEN as u64
        || tag_size > MAX_NATIVE_APE_TAG_BYTES
        || tag_size > footer_end
    {
        return Err(format!(
            "invalid APEv2 tag size {tag_size} in '{}'",
            path.display()
        ));
    }
    if item_count > MAX_NATIVE_APE_ITEMS {
        return Err(format!(
            "APEv2 item count {item_count} exceeds the native safety limit in '{}'",
            path.display()
        ));
    }

    let items_start = footer_end - tag_size;
    let items_len = tag_size - APE_DESCRIPTOR_LEN as u64;
    let had_header = footer_flags & APE_FLAG_HEADER_PRESENT != 0;
    let replace_start = if had_header {
        if items_start < APE_DESCRIPTOR_LEN as u64 {
            return Err(format!("APEv2 header underflows file start in '{}'", path.display()));
        }
        let header_start = items_start - APE_DESCRIPTOR_LEN as u64;
        file.seek(SeekFrom::Start(header_start))
            .map_err(|error| format!("seek APEv2 header in '{}': {error}", path.display()))?;
        let mut header = [0u8; APE_DESCRIPTOR_LEN];
        file.read_exact(&mut header)
            .map_err(|error| format!("read APEv2 header in '{}': {error}", path.display()))?;
        if &header[..8] != APE_SIGNATURE
            || u32_le_at(&header, 8)? != version
            || u32_le_at(&header, 12)? != tag_size as u32
            || u32_le_at(&header, 16)? != item_count
            || u32_le_at(&header, 20)? & APE_FLAG_IS_HEADER == 0
        {
            return Err(format!(
                "APEv2 footer claims a header but the matching header is absent or inconsistent in '{}'",
                path.display()
            ));
        }
        header_start
    } else {
        items_start
    };

    let items_len_usize = usize::try_from(items_len)
        .map_err(|_| format!("APEv2 item region is too large in '{}'", path.display()))?;
    file.seek(SeekFrom::Start(items_start))
        .map_err(|error| format!("seek APEv2 items in '{}': {error}", path.display()))?;
    let mut bytes = vec![0u8; items_len_usize];
    file.read_exact(&mut bytes)
        .map_err(|error| format!("read APEv2 items in '{}': {error}", path.display()))?;

    // A physical item requires at least an 8-byte item header and a one-byte
    // key terminator. Clamp attacker-controlled footer counts to the maximum
    // number that could fit in the bounded region before allocating.
    let mut items = Vec::with_capacity(native_ape_item_capacity(item_count, bytes.len()));
    let mut cursor = 0usize;
    for item_index in 0..item_count {
        let item_start = cursor;
        if cursor.checked_add(8).is_none_or(|end| end > bytes.len()) {
            return Err(format!(
                "truncated APEv2 item {item_index} header in '{}'",
                path.display()
            ));
        }
        let value_len = u32_le_at(&bytes, cursor)? as usize;
        let flags = u32_le_at(&bytes, cursor + 4)?;
        cursor += 8;
        let key_end = bytes[cursor..]
            .iter()
            .position(|byte| *byte == 0)
            .map(|offset| cursor + offset)
            .ok_or_else(|| {
                format!(
                    "unterminated APEv2 item {item_index} key in '{}'",
                    path.display()
                )
            })?;
        let key_bytes = bytes[cursor..key_end].to_vec();
        cursor = key_end + 1;
        let value_end = cursor.checked_add(value_len).ok_or_else(|| {
            format!("APEv2 item {item_index} length overflow in '{}'", path.display())
        })?;
        if value_end > bytes.len() {
            return Err(format!(
                "truncated APEv2 item {item_index} value in '{}'",
                path.display()
            ));
        }
        let value = bytes[cursor..value_end].to_vec();
        cursor = value_end;
        let key = if ape_key_is_valid(&key_bytes) {
            Some(String::from_utf8(key_bytes.clone()).map_err(|_| {
                format!("ASCII APEv2 key decoded as invalid UTF-8 in '{}'", path.display())
            })?)
        } else {
            None
        };
        items.push(NativeApeItem {
            raw: bytes[item_start..cursor].to_vec(),
            key_bytes,
            key,
            flags,
            value,
        });
    }
    if cursor != bytes.len() {
        return Err(format!(
            "APEv2 item count/size mismatch in '{}': {} unclaimed byte(s)",
            path.display(),
            bytes.len() - cursor
        ));
    }

    Ok(Some(NativeApeTag {
        replace_start,
        footer_end,
        had_header,
        items,
    }))
}

pub(crate) fn native_ape_canonical_key(raw_key: &str) -> String {
    let normalized = raw_key.trim().to_ascii_uppercase();
    match normalized.as_str() {
        "TRACK" | "TRACKNUMBER" => "TRACKNUMBER".to_string(),
        "TRACKTOTAL" | "TOTALTRACKS" => "TRACKTOTAL".to_string(),
        "DISK" | "DISC" | "DISKNUMBER" | "DISCNUMBER" => "DISCNUMBER".to_string(),
        "DISKTOTAL" | "DISCTOTAL" | "TOTALDISCS" => "DISCTOTAL".to_string(),
        "ALBUM ARTIST" | "ALBUMARTIST" => "ALBUMARTIST".to_string(),
        "YEAR" | "DATE" => "DATE".to_string(),
        "DESCRIPTION" | "COMMENT" => "COMMENT".to_string(),
        "ENCODING TOOL" | "ENCODINGTOOL" => "ENCODINGTOOL".to_string(),
        "MUSICBRAINZ ALBUM ID" | "MUSICBRAINZ RELEASE ID" | "MUSICBRAINZ_ALBUMID" => {
            "MUSICBRAINZ_ALBUMID".to_string()
        }
        "MUSICBRAINZ ALBUM ARTIST ID" | "MUSICBRAINZ RELEASE ARTIST ID"
        | "MUSICBRAINZ_ALBUMARTISTID" => "MUSICBRAINZ_ALBUMARTISTID".to_string(),
        "MUSICBRAINZ RELEASE GROUP ID" | "MUSICBRAINZ_RELEASEGROUPID" => {
            "MUSICBRAINZ_RELEASEGROUPID".to_string()
        }
        "MUSICBRAINZ TRACK ID" | "MUSICBRAINZ RECORDING ID" | "MUSICBRAINZ_TRACKID" => {
            "MUSICBRAINZ_TRACKID".to_string()
        }
        "MUSICBRAINZ RELEASE TRACK ID" | "MUSICBRAINZ_RELEASETRACKID" => {
            "MUSICBRAINZ_RELEASETRACKID".to_string()
        }
        "MUSICBRAINZ ARTIST ID" | "MUSICBRAINZ_ARTISTID" => {
            "MUSICBRAINZ_ARTISTID".to_string()
        }
        other => other.to_string(),
    }
}

fn item_key_for_neutral_ape_row(canonical_key: &str) -> ItemKey {
    match canonical_key {
        "TITLE" => ItemKey::TrackTitle,
        "ARTIST" => ItemKey::TrackArtist,
        "ALBUM" => ItemKey::AlbumTitle,
        "ALBUMARTIST" => ItemKey::AlbumArtist,
        "TRACKNUMBER" => ItemKey::TrackNumber,
        "TRACKTOTAL" => ItemKey::TrackTotal,
        "DISCNUMBER" => ItemKey::DiscNumber,
        "DISCTOTAL" => ItemKey::DiscTotal,
        "DATE" => ItemKey::Year,
        "GENRE" => ItemKey::Genre,
        "COMMENT" => ItemKey::Comment,
        "COMPOSER" => ItemKey::Composer,
        "LYRICIST" => ItemKey::Lyricist,
        "ARRANGER" => ItemKey::Arranger,
        "PERFORMER" => ItemKey::Performer,
        "ISRC" => ItemKey::Isrc,
        "CATALOGNUMBER" => ItemKey::CatalogNumber,
        "PUBLISHER" => ItemKey::Publisher,
        "COPYRIGHT" => ItemKey::CopyrightMessage,
        "ORIGINALDATE" => ItemKey::OriginalReleaseDate,
        "MUSICBRAINZ_ALBUMID" => ItemKey::MusicBrainzReleaseId,
        "MUSICBRAINZ_ALBUMARTISTID" => ItemKey::MusicBrainzReleaseArtistId,
        "MUSICBRAINZ_RELEASEGROUPID" => ItemKey::MusicBrainzReleaseGroupId,
        "MUSICBRAINZ_TRACKID" => ItemKey::MusicBrainzRecordingId,
        "MUSICBRAINZ_RELEASETRACKID" => ItemKey::MusicBrainzTrackId,
        "MUSICBRAINZ_ARTISTID" => ItemKey::MusicBrainzArtistId,
        "REPLAYGAIN_TRACK_GAIN" => ItemKey::ReplayGainTrackGain,
        "REPLAYGAIN_TRACK_PEAK" => ItemKey::ReplayGainTrackPeak,
        "REPLAYGAIN_ALBUM_GAIN" => ItemKey::ReplayGainAlbumGain,
        "REPLAYGAIN_ALBUM_PEAK" => ItemKey::ReplayGainAlbumPeak,
        _ => ItemKey::Unknown(canonical_key.to_string()),
    }
}

pub(crate) fn native_ape_numbering_rows(
    raw_key: &str,
    value: &str,
) -> Option<Vec<(String, ItemKey, String)>> {
    let normalized = raw_key.trim().to_ascii_uppercase();
    let (number_key, total_key, number_item, total_item) = match normalized.as_str() {
        "TRACK" => (
            "TRACKNUMBER",
            "TRACKTOTAL",
            ItemKey::TrackNumber,
            ItemKey::TrackTotal,
        ),
        "DISK" | "DISC" => (
            "DISCNUMBER",
            "DISCTOTAL",
            ItemKey::DiscNumber,
            ItemKey::DiscTotal,
        ),
        _ => return None,
    };
    let mut parts = value.split('/');
    let number = parts.next().unwrap_or("").trim();
    if number.parse::<u32>().is_err() {
        return None;
    }
    let mut rows = vec![(number_key.to_string(), number_item, number.to_string())];
    if let Some(total) = parts.next().map(str::trim).filter(|value| !value.is_empty()) {
        if total.parse::<u32>().is_err() {
            return None;
        }
        rows.push((total_key.to_string(), total_item, total.to_string()));
    }
    if parts.next().is_some() {
        return None;
    }
    Some(rows)
}

pub(crate) fn native_ape_rows(
    tag: &NativeApeTag,
    path: &Path,
) -> Result<Vec<NeutralApeRow>, String> {
    let mut rows = Vec::new();
    for item in tag.items.iter().filter(|item| item.key.is_some()) {
        let raw_key = item.key.as_deref().expect("filtered valid APE key");
        let (value, is_binary) = match item.item_type() {
            APE_ITEM_TYPE_TEXT | APE_ITEM_TYPE_LOCATOR => {
                let value = std::str::from_utf8(&item.value).map_err(|_| {
                    format!(
                        "APEv2 item '{}' has invalid UTF-8 text in '{}'",
                        raw_key,
                        path.display()
                    )
                })?;
                (value.split('\0').collect::<Vec<_>>().join("; "), false)
            }
            APE_ITEM_TYPE_BINARY => (format!("<binary, {} bytes>", item.value.len()), true),
            _ => {
                return Err(format!(
                    "APEv2 item '{}' has reserved value type in '{}'",
                    raw_key,
                    path.display()
                ));
            }
        };
        if !is_binary {
            if let Some(numbering_rows) = native_ape_numbering_rows(raw_key, &value) {
                rows.extend(numbering_rows.into_iter().map(
                    |(canonical_key, item_key, value)| NeutralApeRow {
                        raw_key: raw_key.to_string(),
                        canonical_key,
                        item_key,
                        value,
                        is_binary: false,
                    },
                ));
                continue;
            }
        }
        let canonical_key = native_ape_canonical_key(raw_key);
        rows.push(NeutralApeRow {
            raw_key: raw_key.to_string(),
            item_key: item_key_for_neutral_ape_row(&canonical_key),
            canonical_key,
            value,
            is_binary,
        });
    }
    Ok(rows)
}

pub(crate) fn read_native_ape_fallback(path: &Path) -> Result<NeutralApeReadOutcome, String> {
    let tag = read_native_ape_tag(path)?.ok_or_else(|| {
        format!(
            "Lofty reported an APEv2 decoding failure for '{}', but no bounded trailing APEv2 footer was found",
            path.display()
        )
    })?;
    let rows = native_ape_rows(&tag, path)?;
    let escaped_keys = tag
        .items
        .iter()
        .filter(|item| item.key.is_none())
        .map(|item| display_escaped_ape_key(&item.key_bytes))
        .collect::<Vec<_>>();
    let warning = (!escaped_keys.is_empty()).then(|| NeutralApeWarning {
        path: path.to_path_buf(),
        escaped_keys,
    });
    Ok(NeutralApeReadOutcome { rows, warning })
}

pub(crate) fn invalid_native_ape_keys(path: &Path) -> Result<Vec<String>, String> {
    Ok(read_native_ape_tag(path)?
        .into_iter()
        .flat_map(|tag| tag.items)
        .filter(|item| item.key.is_none())
        .map(|item| display_escaped_ape_key(&item.key_bytes))
        .collect())
}

/// Detect a native FLAC stream at byte zero or immediately after one bounded,
/// well-formed ID3v2 tag. Returns `Ok(None)` for non-FLAC input and an error for
/// a file that claims an ID3v2 prefix but cannot be parsed safely or is not
/// followed immediately by `fLaC`.
pub(crate) fn detect_flac_stream_offset<R: Read + Seek>(reader: &mut R) -> Result<Option<u64>, String> {
    reader
        .seek(SeekFrom::Start(0))
        .map_err(|err| format!("seek file start while detecting FLAC: {err}"))?;
    let mut header = [0u8; 10];
    let mut read = 0usize;
    while read < 4 {
        match reader.read(&mut header[read..4]) {
            Ok(0) => return Ok(None),
            Ok(count) => read += count,
            Err(err) => return Err(format!("read FLAC/ID3 signature: {err}")),
        }
    }
    if &header[..4] == FLAC_MAGIC {
        return Ok(Some(0));
    }
    if &header[..3] != b"ID3" {
        return Ok(None);
    }

    while read < header.len() {
        match reader.read(&mut header[read..]) {
            Ok(0) => return Err("truncated ID3v2 header before FLAC stream".to_string()),
            Ok(count) => read += count,
            Err(err) => return Err(format!("read ID3v2 header before FLAC stream: {err}")),
        }
    }
    let major = header[3];
    if !matches!(major, 2 | 3 | 4) {
        return Err(format!("unsupported ID3v2.{major} prefix before FLAC stream"));
    }
    if header[6..10].iter().any(|byte| byte & 0x80 != 0) {
        return Err("malformed ID3v2 syncsafe size before FLAC stream".to_string());
    }
    let payload_len = header[6..10]
        .iter()
        .fold(0u64, |value, byte| (value << 7) | u64::from(*byte));
    let footer_len = if major == 4 && header[5] & 0x10 != 0 {
        ID3V2_FOOTER_LEN
    } else {
        0
    };
    let stream_offset = ID3V2_HEADER_LEN
        .checked_add(payload_len)
        .and_then(|value| value.checked_add(footer_len))
        .ok_or_else(|| "ID3v2 prefix length overflow before FLAC stream".to_string())?;
    if stream_offset > MAX_ID3V2_FLAC_PREFIX_LEN {
        return Err(format!(
            "ID3v2 prefix before FLAC stream is too large ({stream_offset} bytes; maximum {MAX_ID3V2_FLAC_PREFIX_LEN})"
        ));
    }

    reader
        .seek(SeekFrom::Start(stream_offset))
        .map_err(|err| format!("seek past ID3v2 prefix before FLAC stream: {err}"))?;
    let mut magic = [0u8; 4];
    reader
        .read_exact(&mut magic)
        .map_err(|err| format!("read FLAC magic after ID3v2 prefix: {err}"))?;
    if &magic != FLAC_MAGIC {
        return Err("ID3v2 prefix is not followed immediately by a FLAC stream".to_string());
    }
    Ok(Some(stream_offset))
}

/// Path-oriented wrapper shared by routing and the native writer.
pub(crate) fn flac_stream_offset(path: &Path) -> Result<Option<u64>, String> {
    let mut file = std::fs::File::open(path)
        .map_err(|err| format!("open '{}': {err}", path.display()))?;
    detect_flac_stream_offset(&mut file)
        .map_err(|err| format!("inspect '{}': {err}", path.display()))
}

/// Top-level persistence route used by the metadata writer.
///
/// Format-owned routes centralize policy without necessarily forcing one
/// serializer. `WavPackApeDispatch` keeps healthy files on the established
/// Lofty path and activates the native APEv2 recovery writer only after a
/// typed APE decoding failure. `Lofty` probes the carrier and writes its
/// actual primary tag type through Lofty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataPersistenceRoute {
    NativeFlacVorbis,
    NativeDsfId3,
    WavPackApeDispatch,
    ReadOnlyApeFamily,
    Lofty,
    UnsupportedDff,
}

/// Concrete metadata backend that owns numbering-field serialization.
///
/// Every supported backend appears as an explicit variant so its numbering
/// semantics must be declared exhaustively in [`Self::numbering_capabilities`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataPersistenceBackend {
    NativeFlacVorbis,
    NativeDsfId3,
    NativeWavPackApe,
    LoftyVorbisComments,
    LoftyId3v2,
    LoftyApe,
    LoftyMp4Ilst,
    ReadOnlyApeFamily,
    UnsupportedDff,
    UnclassifiedLofty,
}

/// Neutral persistence capabilities for numbering-family metadata fields.
///
/// These flags describe representations that the backend accepts and
/// round-trips faithfully through the writer/read path. They intentionally do
/// not encode any TUI scheme names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetadataNumberingCapabilities {
    /// Canonical positive unsigned decimal values such as `1` or `17`.
    pub plain_unsigned: bool,
    /// Zero-padded positive unsigned values such as `01` or `017`.
    pub padded_unsigned: bool,
    /// Fraction representations such as `1/17` and padded variants.
    pub numeric_fraction: bool,
    /// Non-numeric lexical representations whose spelling matters, such as
    /// side-prefixed values like `A01`.
    pub lexical: bool,
}

impl MetadataNumberingCapabilities {
    pub const NONE: Self = Self {
        plain_unsigned: false,
        padded_unsigned: false,
        numeric_fraction: false,
        lexical: false,
    };

    pub const TEXTUAL: Self = Self {
        plain_unsigned: true,
        padded_unsigned: true,
        numeric_fraction: true,
        lexical: true,
    };

    pub const PLAIN_UNSIGNED_ONLY: Self = Self {
        plain_unsigned: true,
        padded_unsigned: false,
        numeric_fraction: false,
        lexical: false,
    };

    pub const APE_NUMERIC: Self = Self {
        plain_unsigned: true,
        padded_unsigned: true,
        numeric_fraction: true,
        lexical: false,
    };

    pub const fn intersection(self, other: Self) -> Self {
        Self {
            plain_unsigned: self.plain_unsigned && other.plain_unsigned,
            padded_unsigned: self.padded_unsigned && other.padded_unsigned,
            numeric_fraction: self.numeric_fraction && other.numeric_fraction,
            lexical: self.lexical && other.lexical,
        }
    }

    pub const fn supports(self, representation: MetadataNumberingRepresentation) -> bool {
        match representation {
            MetadataNumberingRepresentation::PlainUnsigned => self.plain_unsigned,
            MetadataNumberingRepresentation::PaddedUnsigned => self.padded_unsigned,
            MetadataNumberingRepresentation::NumericFraction => self.numeric_fraction,
            MetadataNumberingRepresentation::Lexical => self.lexical,
        }
    }
}

/// Representation requirements understood by persistence and feature layers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataNumberingRepresentation {
    PlainUnsigned,
    PaddedUnsigned,
    NumericFraction,
    Lexical,
}

impl MetadataPersistenceBackend {
    /// Capabilities are exhaustive by backend. New backend variants cannot
    /// compile without making an explicit safe declaration here.
    pub const fn numbering_capabilities(self) -> MetadataNumberingCapabilities {
        match self {
            Self::NativeFlacVorbis | Self::LoftyVorbisComments => {
                MetadataNumberingCapabilities::TEXTUAL
            },
            Self::NativeWavPackApe | Self::LoftyApe => MetadataNumberingCapabilities::APE_NUMERIC,
            Self::NativeDsfId3 | Self::LoftyId3v2 | Self::LoftyMp4Ilst => {
                MetadataNumberingCapabilities::PLAIN_UNSIGNED_ONLY
            },
            Self::ReadOnlyApeFamily | Self::UnsupportedDff | Self::UnclassifiedLofty => {
                MetadataNumberingCapabilities::NONE
            }
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::NativeFlacVorbis => "native FLAC/Vorbis comments",
            Self::NativeDsfId3 => "native DSF/ID3",
            Self::NativeWavPackApe => "native WavPack/APEv2",
            Self::LoftyVorbisComments => "Lofty Vorbis comments",
            Self::LoftyId3v2 => "Lofty ID3v2",
            Self::LoftyApe => "Lofty APE",
            Self::LoftyMp4Ilst => "Lofty MP4 ilst",
            Self::ReadOnlyApeFamily => "read-only APE/Musepack metadata",
            Self::UnsupportedDff => "unsupported DFF metadata",
            Self::UnclassifiedLofty => "unclassified Lofty tag type",
        }
    }
}

fn extension_is(path: &Path, expected: &str) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case(expected))
}

fn has_flac_magic(path: &Path) -> bool {
    matches!(flac_stream_offset(path), Ok(Some(_)))
}

/// Resolve the same top-level route used by the metadata writer.
///
/// Dispatch order is intentional: DSF is extension-owned native; WavPack is
/// extension-owned policy dispatch (healthy tags use Lofty, typed malformed
/// APE tags use the native recovery writer); APE/Musepack remain
/// read-fallback-only; FLAC uses either its
/// extension or file magic; DFF is explicitly unsupported; every other path is
/// delegated to Lofty's content probe.
pub fn metadata_persistence_route_for_path(path: &Path) -> MetadataPersistenceRoute {
    if extension_is(path, "dsf") {
        MetadataPersistenceRoute::NativeDsfId3
    } else if extension_is(path, "wv") {
        MetadataPersistenceRoute::WavPackApeDispatch
    } else if extension_is(path, "ape") || extension_is(path, "mpc") {
        MetadataPersistenceRoute::ReadOnlyApeFamily
    } else if extension_is(path, "flac") || has_flac_magic(path) {
        MetadataPersistenceRoute::NativeFlacVorbis
    } else if extension_is(path, "dff") {
        MetadataPersistenceRoute::UnsupportedDff
    } else {
        MetadataPersistenceRoute::Lofty
    }
}

/// Map the actual primary Lofty tag type to the backend that serializes
/// numbering fields. Non-primary and future tag types remain fail-closed.
pub fn metadata_backend_for_lofty_tag_type(tag_type: TagType) -> MetadataPersistenceBackend {
    match tag_type {
        TagType::VorbisComments => MetadataPersistenceBackend::LoftyVorbisComments,
        TagType::Id3v2 => MetadataPersistenceBackend::LoftyId3v2,
        TagType::Ape => MetadataPersistenceBackend::LoftyApe,
        TagType::Mp4Ilst => MetadataPersistenceBackend::LoftyMp4Ilst,
        _ => MetadataPersistenceBackend::UnclassifiedLofty,
    }
}

/// Canonical numbering-field identity shared by capability validation and
/// serializer key normalization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MetadataNumberingField {
    TrackNumber,
    TrackTotal,
    DiscNumber,
    DiscTotal,
}

impl MetadataNumberingField {
    fn trimmed_unknown_name(key: &ItemKey) -> Option<&str> {
        let ItemKey::Unknown(name) = key else {
            return None;
        };
        let name = name.trim();
        (!name.is_empty()).then_some(name)
    }

    fn from_logical_name(name: &str) -> Option<Self> {
        [
            Self::TrackNumber,
            Self::TrackTotal,
            Self::DiscNumber,
            Self::DiscTotal,
        ]
        .into_iter()
        .find(|field| {
            field
                .logical_aliases()
                .iter()
                .any(|alias| name.eq_ignore_ascii_case(alias))
        })
    }

    fn from_item_key(key: &ItemKey) -> Option<Self> {
        match key {
            ItemKey::TrackNumber => Some(Self::TrackNumber),
            ItemKey::TrackTotal => Some(Self::TrackTotal),
            ItemKey::DiscNumber => Some(Self::DiscNumber),
            ItemKey::DiscTotal => Some(Self::DiscTotal),
            ItemKey::Unknown(_) => Self::from_logical_name(Self::trimmed_unknown_name(key)?),
            _ => None,
        }
    }

    fn from_item_key_for_backend(
        backend: MetadataPersistenceBackend,
        key: &ItemKey,
    ) -> Option<Self> {
        if let Some(field) = Self::from_item_key(key) {
            return Some(field);
        }
        let name = Self::trimmed_unknown_name(key)?;
        match backend {
            MetadataPersistenceBackend::NativeDsfId3
            | MetadataPersistenceBackend::LoftyId3v2
                if name.eq_ignore_ascii_case("TRCK") =>
            {
                Some(Self::TrackNumber)
            }
            MetadataPersistenceBackend::NativeDsfId3
            | MetadataPersistenceBackend::LoftyId3v2
                if name.eq_ignore_ascii_case("TPOS") =>
            {
                Some(Self::DiscNumber)
            }
            MetadataPersistenceBackend::NativeWavPackApe
            | MetadataPersistenceBackend::LoftyApe
                if name.eq_ignore_ascii_case("TRACK") =>
            {
                Some(Self::TrackNumber)
            }
            MetadataPersistenceBackend::NativeWavPackApe
            | MetadataPersistenceBackend::LoftyApe
                if name.eq_ignore_ascii_case("DISC") =>
            {
                Some(Self::DiscNumber)
            }
            MetadataPersistenceBackend::LoftyMp4Ilst
                if name.eq_ignore_ascii_case("TRKN") =>
            {
                Some(Self::TrackNumber)
            }
            MetadataPersistenceBackend::LoftyMp4Ilst
                if name.eq_ignore_ascii_case("DISK") =>
            {
                Some(Self::DiscNumber)
            }
            _ => None,
        }
    }

    const fn display_key(self) -> &'static str {
        match self {
            Self::TrackNumber => "TRACKNUMBER",
            Self::TrackTotal => "TRACKTOTAL",
            Self::DiscNumber => "DISCNUMBER",
            Self::DiscTotal => "DISCTOTAL",
        }
    }

    fn typed_item_key(self) -> ItemKey {
        match self {
            Self::TrackNumber => ItemKey::TrackNumber,
            Self::TrackTotal => ItemKey::TrackTotal,
            Self::DiscNumber => ItemKey::DiscNumber,
            Self::DiscTotal => ItemKey::DiscTotal,
        }
    }

    /// Exact logical spellings for this field. The canonical persistence and
    /// editor spelling is always first.
    const fn logical_aliases(self) -> &'static [&'static str] {
        match self {
            Self::TrackNumber => &["TRACKNUMBER"],
            Self::TrackTotal => &["TRACKTOTAL", "TOTALTRACKS"],
            Self::DiscNumber => &["DISCNUMBER", "DISKNUMBER"],
            Self::DiscTotal => &["DISCTOTAL", "DISKTOTAL", "TOTALDISCS"],
        }
    }

    const fn backend_aliases(
        self,
        backend: MetadataPersistenceBackend,
    ) -> &'static [&'static str] {
        match (backend, self) {
            (MetadataPersistenceBackend::LoftyId3v2, Self::TrackNumber) => &["TRCK"],
            (MetadataPersistenceBackend::LoftyId3v2, Self::DiscNumber) => &["TPOS"],
            (MetadataPersistenceBackend::NativeWavPackApe, Self::TrackNumber)
            | (MetadataPersistenceBackend::LoftyApe, Self::TrackNumber) => &["TRACK"],
            (MetadataPersistenceBackend::NativeWavPackApe, Self::DiscNumber)
            | (MetadataPersistenceBackend::LoftyApe, Self::DiscNumber) => &["DISC"],
            (MetadataPersistenceBackend::LoftyMp4Ilst, Self::TrackNumber) => &["TRKN"],
            (MetadataPersistenceBackend::LoftyMp4Ilst, Self::DiscNumber) => &["DISK"],
            _ => &[],
        }
    }
}

/// Return the canonical editor key for a typed, logical, or backend-specific
/// numbering item. Backend-specific aliases are interpreted only in their own
/// tag type so an unrelated custom field cannot be collapsed accidentally.
pub(crate) fn canonical_numbering_display_key_for_backend_item(
    backend: MetadataPersistenceBackend,
    key: &ItemKey,
) -> Option<&'static str> {
    MetadataNumberingField::from_item_key_for_backend(backend, key)
        .map(MetadataNumberingField::display_key)
}

pub(crate) fn canonical_numbering_display_key_for_tag_item(
    key: &ItemKey,
    tag_type: TagType,
) -> Option<&'static str> {
    canonical_numbering_display_key_for_backend_item(
        metadata_backend_for_lofty_tag_type(tag_type),
        key,
    )
}

/// Resolve an exact logical numbering alias to its canonical display key and
/// complete alias group. Matching ignores only surrounding whitespace and
/// ASCII case; punctuation remains significant, so custom fields such as
/// `DISK-NUMBER` never acquire numbering semantics.
pub(crate) fn logical_numbering_alias_group(
    name: &str,
) -> Option<(&'static str, &'static [&'static str])> {
    let field = MetadataNumberingField::from_logical_name(name.trim())?;
    Some((field.display_key(), field.logical_aliases()))
}

/// Normalize logical editor numbering keys to typed keys for serializers whose
/// standard numbering structures require them.
pub fn normalize_numbering_item_key_for_backend(
    backend: MetadataPersistenceBackend,
    key: &ItemKey,
) -> ItemKey {
    if !matches!(
        backend,
        MetadataPersistenceBackend::LoftyId3v2
            | MetadataPersistenceBackend::NativeWavPackApe
            | MetadataPersistenceBackend::LoftyApe
            | MetadataPersistenceBackend::LoftyMp4Ilst
    ) {
        return key.clone();
    }

    MetadataNumberingField::from_item_key_for_backend(backend, key)
        .map(MetadataNumberingField::typed_item_key)
        .unwrap_or_else(|| key.clone())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NormalizedTypedLoftyChange {
    pub persistence_key: ItemKey,
    pub value: Option<String>,
    pub removal_keys: Vec<ItemKey>,
}

/// Normalize a complete ID3v2/APE/MP4 change set before mutation.
///
/// Logical and backend-native aliases collapse onto their typed carrier key.
/// Equal operations coalesce. Any value/value or value/deletion disagreement
/// fails closed so the result cannot depend on caller ordering.
pub(crate) fn normalized_typed_lofty_changes(
    backend: MetadataPersistenceBackend,
    changes: &[(ItemKey, Option<String>)],
) -> Result<Vec<NormalizedTypedLoftyChange>, String> {
    if !matches!(
        backend,
        MetadataPersistenceBackend::LoftyId3v2
            | MetadataPersistenceBackend::NativeWavPackApe
            | MetadataPersistenceBackend::LoftyApe
            | MetadataPersistenceBackend::LoftyMp4Ilst
    ) {
        return Err(format!(
            "{} is not a typed metadata backend",
            backend.label()
        ));
    }

    #[derive(Debug)]
    struct PendingChange {
        persistence_key: ItemKey,
        values: Vec<Option<String>>,
        removal_keys: Vec<ItemKey>,
    }

    fn item_key_sort_key(key: &ItemKey) -> String {
        match key {
            ItemKey::Unknown(name) => format!("Unknown({name})"),
            _ => format!("{key:?}"),
        }
    }

    let mut pending = Vec::<PendingChange>::new();
    for (key, value) in changes {
        let persistence_key = normalize_numbering_item_key_for_backend(backend, key);
        let normalized_value = value
            .as_ref()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());

        let index = if let Some(index) = pending
            .iter()
            .position(|change| change.persistence_key == persistence_key)
        {
            index
        } else {
            pending.push(PendingChange {
                persistence_key: persistence_key.clone(),
                values: Vec::new(),
                removal_keys: Vec::new(),
            });
            pending.len() - 1
        };
        let change = &mut pending[index];
        if !change.values.iter().any(|existing| existing == &normalized_value) {
            change.values.push(normalized_value);
        }
        if !change.removal_keys.iter().any(|candidate| candidate == key) {
            change.removal_keys.push(key.clone());
        }
        if let Some(field) = MetadataNumberingField::from_item_key_for_backend(backend, key) {
            for alias in field
                .logical_aliases()
                .iter()
                .chain(field.backend_aliases(backend).iter())
            {
                let alias = ItemKey::Unknown((*alias).to_string());
                if !change
                    .removal_keys
                    .iter()
                    .any(|candidate| candidate == &alias)
                {
                    change.removal_keys.push(alias);
                }
            }
        }
        if !change
            .removal_keys
            .iter()
            .any(|candidate| candidate == &persistence_key)
        {
            change.removal_keys.push(persistence_key);
        }
    }

    pending.sort_by(|left, right| {
        item_key_sort_key(&left.persistence_key).cmp(&item_key_sort_key(&right.persistence_key))
    });

    let mut resolved = Vec::with_capacity(pending.len());
    for mut change in pending {
        change.values.sort();
        if change.values.len() > 1 {
            return Err(format!(
                "conflicting metadata changes target the same {} field {:?}: {:?}",
                backend.label(),
                change.persistence_key,
                change.values,
            ));
        }
        change
            .removal_keys
            .sort_by_key(item_key_sort_key);
        resolved.push(NormalizedTypedLoftyChange {
            persistence_key: change.persistence_key,
            value: change.values.pop().flatten(),
            removal_keys: change.removal_keys,
        });
    }
    Ok(resolved)
}

fn is_canonical_positive_unsigned(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && value
            .parse::<u32>()
            .ok()
            .is_some_and(|parsed| parsed > 0 && parsed.to_string() == value)
}

fn numbering_value_representation(value: &str) -> MetadataNumberingRepresentation {
    if is_canonical_positive_unsigned(value) {
        return MetadataNumberingRepresentation::PlainUnsigned;
    }
    if value.len() > 1
        && value.starts_with('0')
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && value.parse::<u32>().ok().is_some_and(|parsed| parsed > 0)
    {
        return MetadataNumberingRepresentation::PaddedUnsigned;
    }
    if let Some((number, total)) = value.split_once('/') {
        if !total.contains('/')
            && !number.is_empty()
            && !total.is_empty()
            && number.bytes().all(|byte| byte.is_ascii_digit())
            && total.bytes().all(|byte| byte.is_ascii_digit())
            && number.parse::<u32>().ok().is_some_and(|number| number > 0)
            && total.parse::<u32>().ok().is_some_and(|total| total > 0)
        {
            return MetadataNumberingRepresentation::NumericFraction;
        }
    }
    MetadataNumberingRepresentation::Lexical
}

/// Enforce backend numbering capabilities at the persistence boundary before
/// any carrier bytes, rollback markers, or journals can be changed.
pub(crate) fn validate_numbering_changes_for_backend(
    backend: MetadataPersistenceBackend,
    changes: &[(ItemKey, Option<String>)],
) -> Result<(), String> {
    let capabilities = backend.numbering_capabilities();
    for (key, value) in changes {
        let Some(field) = MetadataNumberingField::from_item_key_for_backend(backend, key) else {
            continue;
        };
        let Some(value) = value.as_deref() else {
            continue;
        };
        if value.trim().is_empty() {
            continue;
        }
        // Classify the supplied spelling, not a trimmed surrogate. Typed
        // numeric carriers must reject values such as ` 7 ` rather than
        // silently normalizing them to a representation the caller did not
        // request.
        let representation = numbering_value_representation(value);
        if capabilities.supports(representation) {
            continue;
        }
        let requirement = match representation {
            MetadataNumberingRepresentation::PlainUnsigned => "plain unsigned",
            MetadataNumberingRepresentation::PaddedUnsigned => "padded unsigned",
            MetadataNumberingRepresentation::NumericFraction => "numeric fraction",
            MetadataNumberingRepresentation::Lexical => "lexical",
        };
        let supported = if capabilities == MetadataNumberingCapabilities::PLAIN_UNSIGNED_ONLY {
            "supported representation: canonical positive unsigned numbering values"
        } else if capabilities == MetadataNumberingCapabilities::APE_NUMERIC {
            "supported representations: plain unsigned, padded unsigned, and numeric fraction numbering values"
        } else if capabilities == MetadataNumberingCapabilities::NONE {
            "this backend has no declared numbering capability"
        } else {
            "the backend's declared numbering representations do not include this value"
        };
        return Err(format!(
            "{} cannot persist {} value {:?} losslessly: {requirement} numbering is unsupported; {supported}",
            backend.label(),
            field.display_key(),
            value,
        ));
    }
    Ok(())
}

/// Resolve the concrete backend whose writer will persist numbering fields.
///
/// Native routes are known without parsing. Generic routes are classified from
/// the same primary tag type that `write_all_tags_lofty_in_place` edits or
/// creates. Failure to probe a generic carrier is a capability failure rather
/// than an extension-based guess.
pub fn metadata_backend_for_path(path: &Path) -> Result<MetadataPersistenceBackend, String> {
    match metadata_persistence_route_for_path(path) {
        MetadataPersistenceRoute::NativeFlacVorbis => {
            Ok(MetadataPersistenceBackend::NativeFlacVorbis)
        }
        MetadataPersistenceRoute::NativeDsfId3 => Ok(MetadataPersistenceBackend::NativeDsfId3),
        MetadataPersistenceRoute::WavPackApeDispatch => {
            Ok(MetadataPersistenceBackend::NativeWavPackApe)
        }
        MetadataPersistenceRoute::ReadOnlyApeFamily => {
            Ok(MetadataPersistenceBackend::ReadOnlyApeFamily)
        }
        MetadataPersistenceRoute::UnsupportedDff => {
            Ok(MetadataPersistenceBackend::UnsupportedDff)
        }
        MetadataPersistenceRoute::Lofty => {
            let tagged = lofty::read_from_path(path).map_err(|error| {
                format!(
                    "cannot determine metadata numbering capabilities for '{}': {error}",
                    path.display()
                )
            })?;
            let tag_type = tagged
                .primary_tag()
                .map(|tag| tag.tag_type())
                .unwrap_or_else(|| tagged.primary_tag_type());
            Ok(metadata_backend_for_lofty_tag_type(tag_type))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetadataNumberingCapability {
    pub backend: MetadataPersistenceBackend,
    pub capabilities: MetadataNumberingCapabilities,
}

/// Resolve the concrete persistence backend and its numbering capabilities in
/// one probe so presentation and execution callers cannot classify separately.
pub fn metadata_numbering_capability_for_path(
    path: &Path,
) -> Result<MetadataNumberingCapability, String> {
    let backend = metadata_backend_for_path(path)?;
    Ok(MetadataNumberingCapability {
        backend,
        capabilities: backend.numbering_capabilities(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn syncsafe(value: u32) -> [u8; 4] {
        assert!(value < (1 << 28));
        [
            ((value >> 21) & 0x7f) as u8,
            ((value >> 14) & 0x7f) as u8,
            ((value >> 7) & 0x7f) as u8,
            (value & 0x7f) as u8,
        ]
    }

    #[test]
    fn native_ape_item_capacity_clamps_untrusted_footer_counts() {
        assert_eq!(native_ape_item_capacity(100_000_000, 90), 10);
        assert_eq!(native_ape_item_capacity(3, 90), 3);
        assert_eq!(native_ape_item_capacity(u32::MAX, 8), 0);
    }

    #[test]
    fn flac_stream_detection_accepts_direct_and_bounded_id3v2_prefixes() {
        let mut direct = std::io::Cursor::new(b"fLaC".to_vec());
        assert_eq!(detect_flac_stream_offset(&mut direct).unwrap(), Some(0));

        let payload_len = 4_633u32;
        let mut prefixed = Vec::new();
        prefixed.extend_from_slice(&[
            b'I', b'D', b'3', 3, 0, 0x80,
            syncsafe(payload_len)[0], syncsafe(payload_len)[1],
            syncsafe(payload_len)[2], syncsafe(payload_len)[3],
        ]);
        prefixed.extend(std::iter::repeat(0x55).take(payload_len as usize));
        prefixed.extend_from_slice(b"fLaC");
        let mut prefixed = std::io::Cursor::new(prefixed);
        assert_eq!(
            detect_flac_stream_offset(&mut prefixed).unwrap(),
            Some(ID3V2_HEADER_LEN + u64::from(payload_len)),
        );

        let mut v24_with_footer = Vec::new();
        v24_with_footer.extend_from_slice(&[
            b'I', b'D', b'3', 4, 0, 0x10, 0, 0, 0, 3,
        ]);
        v24_with_footer.extend_from_slice(b"tag");
        v24_with_footer.extend_from_slice(&[b'3', b'D', b'I', 4, 0, 0x10, 0, 0, 0, 3]);
        v24_with_footer.extend_from_slice(b"fLaC");
        let mut v24_with_footer = std::io::Cursor::new(v24_with_footer);
        assert_eq!(
            detect_flac_stream_offset(&mut v24_with_footer).unwrap(),
            Some(ID3V2_HEADER_LEN + 3 + ID3V2_FOOTER_LEN),
        );
    }

    #[test]
    fn flac_stream_detection_rejects_malformed_or_unbounded_id3v2_prefixes() {
        let mut malformed_syncsafe = std::io::Cursor::new(vec![
            b'I', b'D', b'3', 3, 0, 0, 0x80, 0, 0, 0,
        ]);
        assert!(detect_flac_stream_offset(&mut malformed_syncsafe)
            .unwrap_err()
            .contains("syncsafe"));

        let oversized = (MAX_ID3V2_FLAC_PREFIX_LEN + 1 - ID3V2_HEADER_LEN) as u32;
        let size = syncsafe(oversized);
        let mut oversized = std::io::Cursor::new(vec![
            b'I', b'D', b'3', 3, 0, 0, size[0], size[1], size[2], size[3],
        ]);
        assert!(detect_flac_stream_offset(&mut oversized)
            .unwrap_err()
            .contains("too large"));

        let mut missing_flac = std::io::Cursor::new(
            [&[b'I', b'D', b'3', 3, 0, 0, 0, 0, 0, 1][..], b"xNOPE".as_slice()].concat(),
        );
        assert!(detect_flac_stream_offset(&mut missing_flac)
            .unwrap_err()
            .contains("not followed immediately"));
    }

    #[test]
    fn id3v2_prefixed_flac_magic_routes_without_a_flac_extension() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("extensionless-prefixed-audio");
        let mut bytes = vec![b'I', b'D', b'3', 3, 0, 0x80, 0, 0, 0, 2];
        bytes.extend_from_slice(b"xxfLaC");
        std::fs::write(&path, bytes).expect("write prefixed FLAC signature fixture");
        assert_eq!(
            metadata_persistence_route_for_path(&path),
            MetadataPersistenceRoute::NativeFlacVorbis,
        );
    }

    #[test]
    fn every_declared_backend_reports_explicit_capabilities() {
        for backend in [
            MetadataPersistenceBackend::NativeFlacVorbis,
            MetadataPersistenceBackend::LoftyVorbisComments,
        ] {
            assert_eq!(
                backend.numbering_capabilities(),
                MetadataNumberingCapabilities::TEXTUAL,
                "unexpected textual capabilities for {backend:?}"
            );
        }
        for backend in [
            MetadataPersistenceBackend::NativeWavPackApe,
            MetadataPersistenceBackend::LoftyApe,
        ] {
            assert_eq!(
                backend.numbering_capabilities(),
                MetadataNumberingCapabilities::APE_NUMERIC,
                "unexpected APE numbering capabilities for {backend:?}"
            );
        }
        for backend in [
            MetadataPersistenceBackend::NativeDsfId3,
            MetadataPersistenceBackend::LoftyId3v2,
            MetadataPersistenceBackend::LoftyMp4Ilst,
        ] {
            assert_eq!(
                backend.numbering_capabilities(),
                MetadataNumberingCapabilities::PLAIN_UNSIGNED_ONLY,
                "unexpected plain-only capabilities for {backend:?}"
            );
        }
        for backend in [
            MetadataPersistenceBackend::ReadOnlyApeFamily,
            MetadataPersistenceBackend::UnsupportedDff,
            MetadataPersistenceBackend::UnclassifiedLofty,
        ] {
            assert_eq!(
                backend.numbering_capabilities(),
                MetadataNumberingCapabilities::NONE,
                "unexpected fail-closed capabilities for {backend:?}"
            );
        }
    }

    #[test]
    fn textual_and_numeric_backends_enforce_representation_boundaries() {
        for backend in [
            MetadataPersistenceBackend::NativeFlacVorbis,
            MetadataPersistenceBackend::LoftyVorbisComments,
        ] {
            let capabilities = backend.numbering_capabilities();
            assert!(capabilities.supports(MetadataNumberingRepresentation::PlainUnsigned));
            assert!(capabilities.supports(MetadataNumberingRepresentation::PaddedUnsigned));
            assert!(capabilities.supports(MetadataNumberingRepresentation::NumericFraction));
            assert!(capabilities.supports(MetadataNumberingRepresentation::Lexical));
        }

        for backend in [
            MetadataPersistenceBackend::NativeWavPackApe,
            MetadataPersistenceBackend::LoftyApe,
        ] {
            let capabilities = backend.numbering_capabilities();
            assert!(capabilities.supports(MetadataNumberingRepresentation::PlainUnsigned));
            assert!(capabilities.supports(MetadataNumberingRepresentation::PaddedUnsigned));
            assert!(capabilities.supports(MetadataNumberingRepresentation::NumericFraction));
            assert!(!capabilities.supports(MetadataNumberingRepresentation::Lexical));
        }

        for backend in [
            MetadataPersistenceBackend::NativeDsfId3,
            MetadataPersistenceBackend::LoftyId3v2,
            MetadataPersistenceBackend::LoftyMp4Ilst,
        ] {
            let capabilities = backend.numbering_capabilities();
            assert!(capabilities.supports(MetadataNumberingRepresentation::PlainUnsigned));
            assert!(!capabilities.supports(MetadataNumberingRepresentation::PaddedUnsigned));
            assert!(!capabilities.supports(MetadataNumberingRepresentation::NumericFraction));
            assert!(!capabilities.supports(MetadataNumberingRepresentation::Lexical));
        }
    }

    #[test]
    fn reader_aliases_are_backend_scoped() {
        assert_eq!(
            canonical_numbering_display_key_for_tag_item(
                &ItemKey::Unknown("TRCK".to_string()),
                TagType::Id3v2,
            ),
            Some("TRACKNUMBER")
        );
        assert_eq!(
            canonical_numbering_display_key_for_tag_item(
                &ItemKey::Unknown("Track".to_string()),
                TagType::Ape,
            ),
            Some("TRACKNUMBER")
        );
        assert_eq!(
            canonical_numbering_display_key_for_tag_item(
                &ItemKey::Unknown("trkn".to_string()),
                TagType::Mp4Ilst,
            ),
            Some("TRACKNUMBER")
        );
        for (key, tag_type) in [
            ("TRACK", TagType::Id3v2),
            ("TRCK", TagType::Ape),
            ("TRKN", TagType::Id3v2),
            ("TRACK", TagType::VorbisComments),
        ] {
            assert_eq!(
                canonical_numbering_display_key_for_tag_item(
                    &ItemKey::Unknown(key.to_string()),
                    tag_type,
                ),
                None,
                "backend-native alias {key:?} must remain distinct on {tag_type:?}"
            );
        }
    }

    #[test]
    fn punctuation_bearing_custom_fields_never_acquire_numbering_semantics() {
        for (backend, tag_type, custom) in [
            (MetadataPersistenceBackend::LoftyId3v2, TagType::Id3v2, "T-R-C-K"),
            (MetadataPersistenceBackend::LoftyId3v2, TagType::Id3v2, "TRACK-NUMBER"),
            (MetadataPersistenceBackend::LoftyApe, TagType::Ape, "T-R-A-C-K"),
            (MetadataPersistenceBackend::LoftyApe, TagType::Ape, "TRACK-NUMBER"),
            (MetadataPersistenceBackend::LoftyMp4Ilst, TagType::Mp4Ilst, "T-R-K-N"),
            (MetadataPersistenceBackend::LoftyMp4Ilst, TagType::Mp4Ilst, "TRACK-NUMBER"),
        ] {
            let key = ItemKey::Unknown(custom.to_string());
            assert_eq!(
                canonical_numbering_display_key_for_tag_item(&key, tag_type),
                None,
                "punctuation-bearing custom key {custom:?} must remain independent"
            );
            assert_eq!(
                normalize_numbering_item_key_for_backend(backend, &key),
                key,
                "punctuation-bearing custom key {custom:?} must not normalize"
            );
            assert!(validate_numbering_changes_for_backend(
                backend,
                &[(
                    ItemKey::Unknown(custom.to_string()),
                    Some("A01".to_string()),
                )],
            )
            .is_ok());
        }

        let logical = ItemKey::Unknown("  tracknumber  ".to_string());
        assert_eq!(
            normalize_numbering_item_key_for_backend(
                MetadataPersistenceBackend::LoftyId3v2,
                &logical,
            ),
            ItemKey::TrackNumber,
            "surrounding whitespace may be ignored for an exact logical alias"
        );
    }

    #[test]
    fn logical_numbering_alias_groups_are_exact_complete_and_punctuation_safe() {
        for (alias, canonical, expected_aliases) in [
            ("TRACKNUMBER", "TRACKNUMBER", &["TRACKNUMBER"][..]),
            (
                "totaltracks",
                "TRACKTOTAL",
                &["TRACKTOTAL", "TOTALTRACKS"][..],
            ),
            ("DISCNUMBER", "DISCNUMBER", &["DISCNUMBER", "DISKNUMBER"][..]),
            ("disknumber", "DISCNUMBER", &["DISCNUMBER", "DISKNUMBER"][..]),
            (
                "DISCTOTAL",
                "DISCTOTAL",
                &["DISCTOTAL", "DISKTOTAL", "TOTALDISCS"][..],
            ),
            (
                " disktotal ",
                "DISCTOTAL",
                &["DISCTOTAL", "DISKTOTAL", "TOTALDISCS"][..],
            ),
            (
                "totaldiscs",
                "DISCTOTAL",
                &["DISCTOTAL", "DISKTOTAL", "TOTALDISCS"][..],
            ),
        ] {
            let (actual_canonical, actual_aliases) =
                logical_numbering_alias_group(alias).expect("known numbering alias");
            assert_eq!(actual_canonical, canonical);
            assert_eq!(actual_aliases, expected_aliases);
            assert_eq!(actual_aliases.first().copied(), Some(actual_canonical));
        }

        for custom in ["DISK-NUMBER", "DISK-TOTAL", "DISK NUMBER", "DISK_TOTAL"] {
            assert_eq!(
                logical_numbering_alias_group(custom),
                None,
                "punctuation-bearing custom field {custom:?} must remain unrelated"
            );
        }
    }

    #[test]
    fn typed_lofty_change_normalization_is_order_independent_and_conflict_closed() {
        let backend = MetadataPersistenceBackend::LoftyId3v2;
        let equal = normalized_typed_lofty_changes(
            backend,
            &[
                (
                    ItemKey::Unknown("TRCK".to_string()),
                    Some("7".to_string()),
                ),
                (
                    ItemKey::Unknown("TRACKNUMBER".to_string()),
                    Some("7".to_string()),
                ),
            ],
        )
        .expect("equal aliases must coalesce");
        assert_eq!(equal.len(), 1);
        assert_eq!(equal[0].persistence_key, ItemKey::TrackNumber);
        assert_eq!(equal[0].value.as_deref(), Some("7"));
        assert!(equal[0]
            .removal_keys
            .iter()
            .any(|key| key == &ItemKey::Unknown("TRCK".to_string())));
        assert!(equal[0]
            .removal_keys
            .iter()
            .any(|key| key == &ItemKey::Unknown("TRACKNUMBER".to_string())));
        let equal_reversed = normalized_typed_lofty_changes(
            backend,
            &[
                (
                    ItemKey::Unknown("TRACKNUMBER".to_string()),
                    Some("7".to_string()),
                ),
                (
                    ItemKey::Unknown("TRCK".to_string()),
                    Some("7".to_string()),
                ),
            ],
        )
        .expect("reversed equal aliases must coalesce");
        assert_eq!(equal_reversed, equal);

        let conflicting_orders = [
            vec![
                (
                    ItemKey::Unknown("TRACKNUMBER".to_string()),
                    Some("7".to_string()),
                ),
                (
                    ItemKey::Unknown("TRCK".to_string()),
                    Some("8".to_string()),
                ),
            ],
            vec![
                (
                    ItemKey::Unknown("TRCK".to_string()),
                    Some("8".to_string()),
                ),
                (
                    ItemKey::Unknown("TRACKNUMBER".to_string()),
                    Some("7".to_string()),
                ),
            ],
        ];
        let conflict_errors = conflicting_orders.map(|changes| {
            normalized_typed_lofty_changes(backend, &changes)
                .expect_err("conflicting aliases must fail closed")
        });
        assert_eq!(conflict_errors[0], conflict_errors[1]);
        assert!(conflict_errors[0].contains("conflicting metadata changes"));

        let three_way_orders = [
            vec![
                (
                    ItemKey::Unknown("TRACKNUMBER".to_string()),
                    Some("9".to_string()),
                ),
                (
                    ItemKey::Unknown("TRCK".to_string()),
                    Some("7".to_string()),
                ),
                (ItemKey::TrackNumber, Some("8".to_string())),
            ],
            vec![
                (ItemKey::TrackNumber, Some("8".to_string())),
                (
                    ItemKey::Unknown("TRACKNUMBER".to_string()),
                    Some("9".to_string()),
                ),
                (
                    ItemKey::Unknown("TRCK".to_string()),
                    Some("7".to_string()),
                ),
            ],
        ];
        let three_way_errors = three_way_orders.map(|changes| {
            normalized_typed_lofty_changes(backend, &changes)
                .expect_err("three-way conflicts must fail closed")
        });
        assert_eq!(three_way_errors[0], three_way_errors[1]);
        assert!(three_way_errors[0].contains("Some(\"7\")"));
        assert!(three_way_errors[0].contains("Some(\"8\")"));
        assert!(three_way_errors[0].contains("Some(\"9\")"));

        let multi_field_orders = [
            vec![
                (ItemKey::TrackNumber, Some("7".to_string())),
                (
                    ItemKey::Unknown("TRCK".to_string()),
                    Some("8".to_string()),
                ),
                (ItemKey::DiscNumber, Some("2".to_string())),
                (
                    ItemKey::Unknown("TPOS".to_string()),
                    Some("3".to_string()),
                ),
            ],
            vec![
                (
                    ItemKey::Unknown("TPOS".to_string()),
                    Some("3".to_string()),
                ),
                (ItemKey::DiscNumber, Some("2".to_string())),
                (
                    ItemKey::Unknown("TRCK".to_string()),
                    Some("8".to_string()),
                ),
                (ItemKey::TrackNumber, Some("7".to_string())),
            ],
        ];
        let multi_field_errors = multi_field_orders.map(|changes| {
            normalized_typed_lofty_changes(backend, &changes)
                .expect_err("multiple conflicting fields must fail deterministically")
        });
        assert_eq!(multi_field_errors[0], multi_field_errors[1]);

        for changes in [
            vec![
                (
                    ItemKey::Unknown("TRACKTOTAL".to_string()),
                    Some("17".to_string()),
                ),
                (ItemKey::Unknown("TOTALTRACKS".to_string()), None),
            ],
            vec![
                (ItemKey::Unknown("TOTALTRACKS".to_string()), None),
                (
                    ItemKey::Unknown("TRACKTOTAL".to_string()),
                    Some("17".to_string()),
                ),
            ],
        ] {
            let error = normalized_typed_lofty_changes(backend, &changes)
                .expect_err("value/deletion conflicts must fail closed");
            assert!(error.contains("conflicting metadata changes"));
        }
    }

    #[test]
    fn typed_lofty_backends_normalize_synthetic_numbering_keys() {
        for backend in [
            MetadataPersistenceBackend::LoftyId3v2,
            MetadataPersistenceBackend::NativeWavPackApe,
            MetadataPersistenceBackend::LoftyApe,
            MetadataPersistenceBackend::LoftyMp4Ilst,
        ] {
            for (logical, expected) in [
                ("TRACKNUMBER", ItemKey::TrackNumber),
                ("TRACKTOTAL", ItemKey::TrackTotal),
                ("TOTALTRACKS", ItemKey::TrackTotal),
                ("DISCNUMBER", ItemKey::DiscNumber),
                ("DISKNUMBER", ItemKey::DiscNumber),
                ("DISCTOTAL", ItemKey::DiscTotal),
                ("DISKTOTAL", ItemKey::DiscTotal),
                ("TOTALDISCS", ItemKey::DiscTotal),
            ] {
                assert_eq!(
                    normalize_numbering_item_key_for_backend(
                        backend,
                        &ItemKey::Unknown(logical.to_string()),
                    ),
                    expected,
                    "unexpected {logical} normalization for {backend:?}"
                );
            }
        }
    }

    #[test]
    fn backend_native_numbering_aliases_normalize_only_for_their_owner() {
        for (backend, alias, expected) in [
            (
                MetadataPersistenceBackend::LoftyId3v2,
                "TRCK",
                ItemKey::TrackNumber,
            ),
            (
                MetadataPersistenceBackend::LoftyId3v2,
                "TPOS",
                ItemKey::DiscNumber,
            ),
            (
                MetadataPersistenceBackend::LoftyApe,
                "Track",
                ItemKey::TrackNumber,
            ),
            (
                MetadataPersistenceBackend::LoftyApe,
                "Disc",
                ItemKey::DiscNumber,
            ),
            (
                MetadataPersistenceBackend::LoftyMp4Ilst,
                "trkn",
                ItemKey::TrackNumber,
            ),
            (
                MetadataPersistenceBackend::LoftyMp4Ilst,
                "disk",
                ItemKey::DiscNumber,
            ),
        ] {
            assert_eq!(
                normalize_numbering_item_key_for_backend(
                    backend,
                    &ItemKey::Unknown(alias.to_string()),
                ),
                expected,
                "unexpected native-alias normalization for {backend:?} {alias:?}"
            );
        }

        let vorbis_track = ItemKey::Unknown("TRACK".to_string());
        assert_eq!(
            normalize_numbering_item_key_for_backend(
                MetadataPersistenceBackend::LoftyVorbisComments,
                &vorbis_track,
            ),
            vorbis_track,
            "a custom Vorbis TRACK field must not inherit APE semantics"
        );
    }

    #[test]
    fn unclassified_and_vorbis_backends_do_not_invent_typed_key_support() {
        let logical = ItemKey::Unknown("TRACKTOTAL".to_string());
        for backend in [
            MetadataPersistenceBackend::LoftyVorbisComments,
            MetadataPersistenceBackend::UnclassifiedLofty,
        ] {
            assert_eq!(
                normalize_numbering_item_key_for_backend(backend, &logical),
                logical,
                "{backend:?} must retain its existing key path"
            );
        }
    }

    #[test]
    fn numbering_representation_classification_is_lossless_and_explicit() {
        assert_eq!(
            numbering_value_representation("7"),
            MetadataNumberingRepresentation::PlainUnsigned
        );
        assert_eq!(
            numbering_value_representation("7/17"),
            MetadataNumberingRepresentation::NumericFraction
        );
        assert_eq!(
            numbering_value_representation("01/17"),
            MetadataNumberingRepresentation::NumericFraction
        );
        assert_eq!(
            numbering_value_representation("01"),
            MetadataNumberingRepresentation::PaddedUnsigned
        );
        for lexical in ["A01", "0", "+7", " 7 ", "7/not-a-total"] {
            assert_eq!(
                numbering_value_representation(lexical),
                MetadataNumberingRepresentation::Lexical,
                "unexpected representation for {lexical:?}"
            );
        }
    }

    #[test]
    fn persistence_boundary_rejects_unsupported_numbering_without_false_positives() {
        let lexical = [(
            ItemKey::Unknown("TRACKNUMBER".to_string()),
            Some("A01".to_string()),
        )];
        let fraction = [(
            ItemKey::TrackNumber,
            Some("7/17".to_string()),
        )];
        let plain = [(
            ItemKey::Unknown("TRACKNUMBER".to_string()),
            Some("7".to_string()),
        )];
        let raw_id3_alias = [(
            ItemKey::Unknown("TRCK".to_string()),
            Some("A01".to_string()),
        )];
        let custom_vorbis_track = [(
            ItemKey::Unknown("TRACK".to_string()),
            Some("A01".to_string()),
        )];
        let unrelated = [(ItemKey::TrackTitle, Some("A01".to_string()))];

        assert_eq!(
            MetadataNumberingField::from_item_key_for_backend(
                MetadataPersistenceBackend::LoftyId3v2,
                &raw_id3_alias[0].0,
            ),
            Some(MetadataNumberingField::TrackNumber)
        );
        assert_eq!(
            MetadataNumberingField::from_item_key_for_backend(
                MetadataPersistenceBackend::LoftyId3v2,
                &custom_vorbis_track[0].0,
            ),
            None,
            "APE's exact Track alias must remain an unrelated custom key on ID3v2"
        );
        assert!(validate_numbering_changes_for_backend(
            MetadataPersistenceBackend::LoftyId3v2,
            &plain,
        )
        .is_ok());
        for changes in [&lexical[..], &fraction[..], &raw_id3_alias[..]] {
            let error = validate_numbering_changes_for_backend(
                MetadataPersistenceBackend::LoftyId3v2,
                changes,
            )
            .expect_err("numeric-only backend must reject unsupported numbering");
            assert!(error.contains("TRACKNUMBER"));
            assert!(error.contains("unsigned"));
        }
        assert!(validate_numbering_changes_for_backend(
            MetadataPersistenceBackend::LoftyVorbisComments,
            &lexical,
        )
        .is_ok());
        assert!(validate_numbering_changes_for_backend(
            MetadataPersistenceBackend::LoftyVorbisComments,
            &custom_vorbis_track,
        )
        .is_ok());
        assert!(validate_numbering_changes_for_backend(
            MetadataPersistenceBackend::LoftyId3v2,
            &unrelated,
        )
        .is_ok());
    }

    #[test]
    fn ape_persistence_boundary_accepts_exact_numeric_spellings_only() {
        for backend in [
            MetadataPersistenceBackend::NativeWavPackApe,
            MetadataPersistenceBackend::LoftyApe,
        ] {
            for accepted in ["7", "01", "7/17", "01/17"] {
                assert!(
                    validate_numbering_changes_for_backend(
                        backend,
                        &[(ItemKey::TrackNumber, Some(accepted.to_string()))],
                    )
                    .is_ok(),
                    "{backend:?} rejected {accepted:?}"
                );
            }
            let error = validate_numbering_changes_for_backend(
                backend,
                &[(ItemKey::TrackNumber, Some("A01".to_string()))],
            )
            .expect_err("APE numbering must remain non-lexical");
            assert!(error.contains("lexical"), "{error}");
        }
    }

    #[test]
    fn lofty_primary_tag_types_map_to_explicit_backends() {
        assert_eq!(
            metadata_backend_for_lofty_tag_type(TagType::VorbisComments),
            MetadataPersistenceBackend::LoftyVorbisComments
        );
        assert_eq!(
            metadata_backend_for_lofty_tag_type(TagType::Id3v2),
            MetadataPersistenceBackend::LoftyId3v2
        );
        assert_eq!(
            metadata_backend_for_lofty_tag_type(TagType::Ape),
            MetadataPersistenceBackend::LoftyApe
        );
        assert_eq!(
            metadata_backend_for_lofty_tag_type(TagType::Mp4Ilst),
            MetadataPersistenceBackend::LoftyMp4Ilst
        );
        assert_eq!(
            metadata_backend_for_lofty_tag_type(TagType::RiffInfo),
            MetadataPersistenceBackend::UnclassifiedLofty
        );
    }

    #[test]
    fn native_and_unsupported_routes_match_writer_dispatch() {
        assert_eq!(
            metadata_persistence_route_for_path(Path::new("track.flac")),
            MetadataPersistenceRoute::NativeFlacVorbis
        );
        assert_eq!(
            metadata_persistence_route_for_path(Path::new("track.DSF")),
            MetadataPersistenceRoute::NativeDsfId3
        );
        assert_eq!(
            metadata_persistence_route_for_path(Path::new("track.WV")),
            MetadataPersistenceRoute::WavPackApeDispatch
        );
        assert_eq!(
            metadata_persistence_route_for_path(Path::new("track.ape")),
            MetadataPersistenceRoute::ReadOnlyApeFamily
        );
        assert_eq!(
            metadata_persistence_route_for_path(Path::new("track.MPC")),
            MetadataPersistenceRoute::ReadOnlyApeFamily
        );
        assert_eq!(
            metadata_persistence_route_for_path(Path::new("track.dff")),
            MetadataPersistenceRoute::UnsupportedDff
        );
        assert_eq!(
            metadata_persistence_route_for_path(Path::new("track.mp3")),
            MetadataPersistenceRoute::Lofty
        );
    }

    #[test]
    fn flac_magic_uses_the_native_route_without_extension_inference() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("extensionless-audio");
        std::fs::write(&path, b"fLaC").expect("write FLAC magic fixture");
        assert_eq!(
            metadata_persistence_route_for_path(&path),
            MetadataPersistenceRoute::NativeFlacVorbis
        );
    }

    #[test]
    fn generic_probe_failure_fails_closed_instead_of_guessing_from_extension() {
        let error = metadata_numbering_capability_for_path(Path::new("missing-track.mp3"))
            .expect_err("a missing generic carrier must not inherit extension capabilities");
        assert!(error.contains("cannot determine metadata numbering capabilities"));
    }

    #[test]
    fn capability_intersection_is_conservative() {
        assert_eq!(
            MetadataNumberingCapabilities::TEXTUAL.intersection(
                MetadataNumberingCapabilities::PLAIN_UNSIGNED_ONLY,
            ),
            MetadataNumberingCapabilities::PLAIN_UNSIGNED_ONLY
        );
        assert_eq!(
            MetadataNumberingCapabilities::TEXTUAL
                .intersection(MetadataNumberingCapabilities::APE_NUMERIC),
            MetadataNumberingCapabilities::APE_NUMERIC
        );
        assert_eq!(
            MetadataNumberingCapabilities::TEXTUAL
                .intersection(MetadataNumberingCapabilities::NONE),
            MetadataNumberingCapabilities::NONE
        );
    }
}
