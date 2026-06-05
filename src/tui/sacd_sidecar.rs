// SPDX-License-Identifier: GPL-3.0-or-later
//
// Copyright (C) 2026 the tonepoet authors.
//
// SACD ISO sidecar (`<stem>.xml`) reader for the sacd-extract /
// foobar2000 "metabase" XML format (v1.1). The on-disc shape is:
//
//   <?xml version="1.0" encoding="utf-8"?>
//   <!--SACD metabase file-->
//   <root>
//     <store id="<32-hex>" type="SACD" version="1.1">
//       <track id="N">
//         <meta name="K" value="V"/>
//         <meta name="K" value="V"/>
//         ...
//         <replaygain name="replaygain_track_gain" value="..."/>
//         ...
//       </track>
//       ...
//     </store>
//   </root>
//
// Track IDs are 1-based across the disc; on hybrid discs, IDs 1..N1
// are the stereo area and (N1+1)..(N1+N2) are the multi-channel
// area. Track IDs may appear out of order in the file — the logical
// order is by TRACKNUMBER within an area.
//
// This module is read-only for now; sidecar **writes** (preserving
// foreign DISCOGS_*/DR/replaygain fields via read-modify-write) land
// in C5c. v1 file discovery is by the same-stem rule:
// `<stem>.iso` ↔ `<stem>.xml` in the same directory.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One `<track id="N">` block parsed out of the sidecar.
#[derive(Debug, Clone, Default)]
pub struct SidecarTrack {
    /// Spec-encoded id (1..N1 stereo, (N1+1)..(N1+N2) MCH).
    pub id: u32,
    /// `<meta name="K" value="V"/>` entries, deduplicated by key
    /// (later occurrences overwrite earlier ones).
    pub meta: BTreeMap<String, String>,
    /// `<replaygain name="K" value="V"/>` entries — kept separate
    /// because the sidecar nests them under their own element rather
    /// than as `<meta>`. Common keys: replaygain_track_gain,
    /// replaygain_track_peak, replaygain_album_gain,
    /// replaygain_album_peak.
    pub replaygain: BTreeMap<String, String>,
}

/// Top-level parsed sidecar. `store_id` and `version` come from the
/// `<store>` element; `tracks` holds every `<track>` block in the
/// order they appeared in the file (NOT sorted — caller applies
/// area/TRACKNUMBER ordering).
#[derive(Debug, Clone, Default)]
pub struct SidecarMetadata {
    pub store_id: String,
    pub version: String,
    pub tracks: Vec<SidecarTrack>,
}

/// Errors surfaced by the sidecar reader.
#[derive(Debug, Clone)]
pub enum SidecarError {
    Io(String),
    /// XML did not contain the expected `<store>` element.
    NotMetabase,
    /// Malformed XML at the byte offset.
    Malformed(String),
}

impl std::fmt::Display for SidecarError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SidecarError::Io(m) => write!(f, "sidecar I/O: {}", m),
            SidecarError::NotMetabase => write!(f, "sidecar: not a SACD metabase XML"),
            SidecarError::Malformed(m) => write!(f, "sidecar malformed: {}", m),
        }
    }
}
impl std::error::Error for SidecarError {}

/// Discover the sidecar XML for an SACD ISO using the same-stem rule:
/// `disc.iso` ↔ `disc.xml` (case-insensitive `.iso` extension on
/// input; output stem keeps the original case). Returns the path
/// if the file exists, else None.
pub fn find_sidecar_for_iso(iso: &Path) -> Option<PathBuf> {
    expected_sidecar_path_for_iso(iso).filter(|p| p.exists())
}

/// Compute the expected sidecar path for an SACD ISO via the same-
/// stem rule, **without** requiring the file to exist. Used by the
/// mint-on-save flow when an ISO has no sidecar yet — we still need
/// to know where to write the freshly-minted XML.
pub fn expected_sidecar_path_for_iso(iso: &Path) -> Option<PathBuf> {
    let stem = iso.file_stem()?;
    let dir = iso.parent()?;
    Some(dir.join(stem).with_extension("xml"))
}

/// Parse an SACD metabase sidecar XML file. Tolerant of out-of-order
/// elements, missing optional fields, and trailing/leading whitespace.
/// Rejects only when the XML cannot be tokenised or no `<store>`
/// element appears.
pub fn parse_sidecar(path: &Path) -> Result<SidecarMetadata, SidecarError> {
    let bytes = std::fs::read(path).map_err(|e| SidecarError::Io(format!("read: {}", e)))?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|e| SidecarError::Malformed(format!("not UTF-8: {}", e)))?;
    parse_sidecar_str(text)
}

/// Parse a sidecar from an in-memory string. Exposed so tests don't
/// need to round-trip through the filesystem.
pub fn parse_sidecar_str(text: &str) -> Result<SidecarMetadata, SidecarError> {
    let mut out = SidecarMetadata::default();
    let mut cur: Option<SidecarTrack> = None;
    let mut saw_store = false;

    for tag in iter_tags(text)? {
        match tag {
            Tag::Open { name, attrs } if name == "store" => {
                saw_store = true;
                out.store_id = attrs.get("id").cloned().unwrap_or_default();
                out.version = attrs.get("version").cloned().unwrap_or_default();
            }
            Tag::Close { name } if name == "store" => {
                // close the dangling track if any
                if let Some(t) = cur.take() {
                    out.tracks.push(t);
                }
            }
            Tag::Open { name, attrs } if name == "track" => {
                if let Some(t) = cur.take() {
                    out.tracks.push(t);
                }
                let id = attrs
                    .get("id")
                    .and_then(|s| s.parse::<u32>().ok())
                    .unwrap_or(0);
                cur = Some(SidecarTrack {
                    id,
                    ..Default::default()
                });
            }
            Tag::Close { name } if name == "track" => {
                if let Some(t) = cur.take() {
                    out.tracks.push(t);
                }
            }
            Tag::SelfClose { name, attrs } if name == "meta" => {
                if let Some(t) = cur.as_mut() {
                    if let (Some(k), Some(v)) = (attrs.get("name"), attrs.get("value")) {
                        // Normalise meta keys to uppercase. In-the-wild
                        // metabase XML files inconsistently use upper
                        // (sacd-extract default), lower (some foobar2000
                        // pressings — e.g. SME JSACD SRGS 4504), or
                        // mixed case. Downstream merge code looks up
                        // canonical uppercase keys (ALBUM/TITLE/etc.),
                        // so normalising at parse time keeps everything
                        // consistent. Write path emits uppercase too,
                        // which standardises file shape on save (a
                        // mild but defensible reformat).
                        t.meta.insert(k.to_ascii_uppercase(), v.clone());
                    }
                }
            }
            Tag::SelfClose { name, attrs } if name == "replaygain" => {
                if let Some(t) = cur.as_mut() {
                    if let (Some(k), Some(v)) = (attrs.get("name"), attrs.get("value")) {
                        t.replaygain.insert(k.clone(), v.clone());
                    }
                }
            }
            // Tolerate other elements (e.g. <root>, <?xml?>, <!-- comment -->) silently.
            _ => {}
        }
    }
    if let Some(t) = cur.take() {
        out.tracks.push(t);
    }
    if !saw_store {
        return Err(SidecarError::NotMetabase);
    }
    Ok(out)
}

// ---------------------------------------------------------------
// Minimal hand-rolled XML tokenizer
// ---------------------------------------------------------------
//
// The metabase format is a tiny, fixed subset of XML:
//   - declaration: <?xml ... ?> (ignored)
//   - comments:    <!-- ... --> (ignored)
//   - elements:    <name attr1="v1" attr2="v2" ...>  / </name> / <name ... />
//   - attribute values double-quoted; & entities (&amp; etc.) decoded
//
// We don't aim for general XML conformance — just to read what
// sacd-extract and foobar2000 emit. That's tight enough that a 120-
// line hand-rolled tokenizer is more honest than pulling in a heavy
// XML crate.

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tag {
    Open {
        name: String,
        attrs: BTreeMap<String, String>,
    },
    Close {
        name: String,
    },
    SelfClose {
        name: String,
        attrs: BTreeMap<String, String>,
    },
}

fn iter_tags(text: &str) -> Result<Vec<Tag>, SidecarError> {
    let bytes = text.as_bytes();
    let mut out: Vec<Tag> = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        // Look for special openers we need to skip wholesale.
        if bytes[i..].starts_with(b"<!--") {
            // Comment: skip to next "-->"
            let close = find_after(bytes, i + 4, b"-->").ok_or_else(|| {
                SidecarError::Malformed(format!("unterminated comment at offset {}", i))
            })?;
            i = close + 3;
            continue;
        }
        if bytes[i..].starts_with(b"<?") {
            // Processing instruction: skip to "?>"
            let close = find_after(bytes, i + 2, b"?>").ok_or_else(|| {
                SidecarError::Malformed(format!("unterminated PI at offset {}", i))
            })?;
            i = close + 2;
            continue;
        }
        if bytes[i..].starts_with(b"<!") {
            // DOCTYPE/etc: skip to next ">"
            let close = find_after(bytes, i + 2, b">").ok_or_else(|| {
                SidecarError::Malformed(format!("unterminated declaration at offset {}", i))
            })?;
            i = close + 1;
            continue;
        }

        // Regular element: <name ... > or </name>
        let close = find_after(bytes, i + 1, b">")
            .ok_or_else(|| SidecarError::Malformed(format!("unterminated tag at offset {}", i)))?;
        let inner = &bytes[i + 1..close];
        i = close + 1;

        if inner.is_empty() {
            return Err(SidecarError::Malformed(format!(
                "empty tag at offset {}",
                close - 1
            )));
        }

        if inner[0] == b'/' {
            // Close tag
            let name = std::str::from_utf8(&inner[1..])
                .map_err(|e| SidecarError::Malformed(format!("utf8: {}", e)))?
                .trim()
                .to_string();
            out.push(Tag::Close { name });
            continue;
        }

        let self_close = *inner.last().unwrap() == b'/';
        let body = if self_close {
            &inner[..inner.len() - 1]
        } else {
            inner
        };
        let body = std::str::from_utf8(body)
            .map_err(|e| SidecarError::Malformed(format!("utf8: {}", e)))?
            .trim();
        // Split name and attributes.
        let mut parts = body.splitn(2, char::is_whitespace);
        let name = parts.next().unwrap_or("").to_string();
        let attrs_str = parts.next().unwrap_or("");
        let attrs = parse_attributes(attrs_str)?;
        if self_close {
            out.push(Tag::SelfClose { name, attrs });
        } else {
            out.push(Tag::Open { name, attrs });
        }
    }
    Ok(out)
}

fn find_after(bytes: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || from > bytes.len() {
        return None;
    }
    bytes[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| from + p)
}

/// Parse a sequence of `name="value"` attribute pairs. Values are
/// double-quoted (the metabase format never uses single quotes) and
/// may contain XML entity references for &amp; &lt; &gt; &quot;
/// &apos; — decoded inline.
fn parse_attributes(s: &str) -> Result<BTreeMap<String, String>, SidecarError> {
    let mut out = BTreeMap::new();
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        // Skip leading whitespace.
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        // Read name up to '='.
        let name_start = i;
        while i < bytes.len() && bytes[i] != b'=' && !bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'=' {
            return Err(SidecarError::Malformed(format!(
                "expected '=' after attribute name at offset {}",
                i
            )));
        }
        let name = std::str::from_utf8(&bytes[name_start..i])
            .map_err(|e| SidecarError::Malformed(format!("utf8: {}", e)))?
            .to_string();
        i += 1; // past '='
        if i >= bytes.len() || bytes[i] != b'"' {
            return Err(SidecarError::Malformed(format!(
                "expected '\"' opening value for '{}' at offset {}",
                name, i
            )));
        }
        i += 1; // past opening quote
        let value_start = i;
        while i < bytes.len() && bytes[i] != b'"' {
            i += 1;
        }
        if i >= bytes.len() {
            return Err(SidecarError::Malformed(format!(
                "unterminated value for '{}' starting at offset {}",
                name, value_start
            )));
        }
        let raw_value = std::str::from_utf8(&bytes[value_start..i])
            .map_err(|e| SidecarError::Malformed(format!("utf8: {}", e)))?;
        let value = decode_xml_entities(raw_value);
        i += 1; // past closing quote
        out.insert(name, value);
    }
    Ok(out)
}

/// Minimal XML entity decoder covering the five predefined entities.
/// Numeric character references aren't decoded because the metabase
/// format doesn't use them in practice.
fn decode_xml_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '&' {
            out.push(c);
            continue;
        }
        // Read until ';' or whitespace/EOF
        let mut name = String::new();
        let mut terminated = false;
        while let Some(&next) = chars.peek() {
            if next == ';' {
                terminated = true;
                chars.next();
                break;
            }
            if next.is_whitespace() {
                break;
            }
            name.push(next);
            chars.next();
            if name.len() > 8 {
                break;
            }
        }
        if !terminated {
            out.push('&');
            out.push_str(&name);
            continue;
        }
        let replacement = match name.as_str() {
            "amp" => "&",
            "lt" => "<",
            "gt" => ">",
            "quot" => "\"",
            "apos" => "'",
            _ => {
                // Unknown — preserve verbatim so foreign refs round-trip.
                out.push('&');
                out.push_str(&name);
                out.push(';');
                continue;
            }
        };
        out.push_str(replacement);
    }
    out
}

// ---------------------------------------------------------------
// Convenience accessors over SidecarMetadata
// ---------------------------------------------------------------

impl SidecarMetadata {
    /// Tracks belonging to a given 1-based area (1 = first / stereo,
    /// 2 = second / MCH). Returns them sorted by parsed TRACKNUMBER
    /// when present, falling back to declared track id.
    ///
    /// Area boundary: tracks with id ≤ TOTALTRACKS belong to area 1,
    /// tracks with id > TOTALTRACKS belong to area 2. (TOTALTRACKS
    /// is replicated on every track row.)
    pub fn tracks_for_area(&self, area_one_based: u8) -> Vec<&SidecarTrack> {
        let total: u32 = self
            .tracks
            .iter()
            .find_map(|t| t.meta.get("TOTALTRACKS"))
            .and_then(|s| s.trim().parse::<u32>().ok())
            .unwrap_or(0);
        let mut filtered: Vec<&SidecarTrack> = self
            .tracks
            .iter()
            .filter(|t| match area_one_based {
                1 => total == 0 || t.id <= total,
                2 => total > 0 && t.id > total,
                _ => false,
            })
            .collect();
        filtered.sort_by_key(|t| {
            t.meta
                .get("TRACKNUMBER")
                .and_then(|s| s.trim().parse::<u32>().ok())
                .unwrap_or(t.id)
        });
        filtered
    }
}

// ---------------------------------------------------------------
// Serialization (write)
// ---------------------------------------------------------------

/// Serialize a `SidecarMetadata` to XML in a shape compatible with
/// sacd-extract / foobar2000's Super Audio CD Decoder. Track order
/// in the output matches the order in `metadata.tracks` (call-site
/// concern; the parser preserves in-file order). Within each track,
/// `<meta>` entries are emitted in BTreeMap iteration order
/// (alphabetical by key) — this is a tolerable departure from
/// strictly preserving the original key order, since the format
/// doesn't constrain key order. `<replaygain>` entries follow the
/// `<meta>` block.
///
/// Bytes are UTF-8; XML predefined entities (`& < > " '`) are
/// escaped. Numeric character references aren't used (we don't
/// emit them and the parser doesn't decode them — neither side
/// trips on the inverse operation).
pub fn serialize_sidecar(metadata: &SidecarMetadata) -> String {
    let mut out = String::with_capacity(2048);
    out.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>");
    out.push_str("<!--SACD metabase file-->");
    out.push_str("<root>");
    out.push_str("<store id=\"");
    out.push_str(&escape_xml(&metadata.store_id));
    out.push_str("\" type=\"SACD\" version=\"");
    out.push_str(&escape_xml(if metadata.version.is_empty() {
        "1.1"
    } else {
        &metadata.version
    }));
    out.push_str("\">");

    for track in &metadata.tracks {
        out.push('\n');
        out.push_str("<track id=\"");
        out.push_str(&track.id.to_string());
        out.push_str("\">");
        for (k, v) in &track.meta {
            out.push_str("<meta name=\"");
            out.push_str(&escape_xml(k));
            out.push_str("\" value=\"");
            out.push_str(&escape_xml(v));
            out.push_str("\"/>");
        }
        for (k, v) in &track.replaygain {
            out.push_str("<replaygain name=\"");
            out.push_str(&escape_xml(k));
            out.push_str("\" value=\"");
            out.push_str(&escape_xml(v));
            out.push_str("\"/>");
        }
        out.push_str("</track>");
    }

    out.push('\n');
    out.push_str("</store>");
    out.push_str("</root>");
    out.push('\n');
    out
}

/// Atomic write: serialize, write to a sibling `.tmp` file, fsync,
/// then rename over the target. On rename failure the temp file is
/// removed. This avoids torn writes if the process is killed mid-
/// write, and is consistent with how the rest of tonepoet writes
/// CUE files / metadata journal entries.
pub fn write_sidecar(path: &Path, metadata: &SidecarMetadata) -> Result<(), SidecarError> {
    use std::io::Write;
    let bytes = serialize_sidecar(metadata).into_bytes();
    let parent = path
        .parent()
        .ok_or_else(|| SidecarError::Io(format!("no parent dir for '{}'", path.display())))?;
    let tmp = parent.join(format!(
        ".{}.tmp",
        path.file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "sidecar".to_string()),
    ));
    {
        let mut f = std::fs::File::create(&tmp)
            .map_err(|e| SidecarError::Io(format!("create tmp '{}': {}", tmp.display(), e)))?;
        f.write_all(&bytes)
            .map_err(|e| SidecarError::Io(format!("write tmp: {}", e)))?;
        f.sync_all()
            .map_err(|e| SidecarError::Io(format!("fsync tmp: {}", e)))?;
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(SidecarError::Io(format!(
            "rename '{}' → '{}': {}",
            tmp.display(),
            path.display(),
            e,
        )));
    }
    Ok(())
}

/// Encode XML predefined entities for inclusion in attribute values.
/// Only `& < > " '` are translated — numeric character references
/// aren't emitted because the parser doesn't decode them.
fn escape_xml(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

/// Synthesize an in-memory `SidecarMetadata` from a parsed
/// `SacdMetadata` (Master TOC + SACDText + per-area tracks + per-track
/// text and ISRC). Used by the mint-on-save path when an SACD ISO has
/// no existing sidecar XML.
///
/// Both areas (stereo + multi-channel) are populated when present;
/// track IDs follow the ScarletBook convention `1..=N1` for stereo and
/// `(N1+1)..=(N1+N2)` for multi-channel. Album-level fields
/// (`ALBUM` / `ALBUMARTIST` / `DATE` / `CATALOGNUMBER` / `GENRE`) are
/// replicated per-track to match the metabase XML shape foobar2000 /
/// JRiver expect. Per-track fields (`TITLE` / `ARTIST` / `COMPOSER` /
/// `LYRICIST` / `ARRANGER` / `ISRC`) come from the area's per-track
/// text and ISRC tables when present; absent values are simply omitted
/// rather than emitted as empty strings.
///
/// The returned sidecar has `store_id` **empty** — callers must fill
/// it before writing (typically via [`mint_disc_id`]). `version` is
/// `"1.1"`, matching every metabase sidecar observed in the wild.
pub fn seed_sidecar_from_scarletbook(md: &super::sacd::SacdMetadata) -> SidecarMetadata {
    let album = md.album_title().unwrap_or("").trim().to_string();
    let album_artist = md.album_artist().unwrap_or("").trim().to_string();
    let date = md
        .master_toc
        .disc_date
        .map(|d| d.year.to_string())
        .unwrap_or_default();
    let catalog = md.master_toc.album_catalog_number.trim().to_string();
    let genre = md
        .master_toc
        .album_genres
        .first()
        .map(|g| g.name().to_string())
        .unwrap_or_default();

    let album_proto: Vec<(&'static str, String)> = [
        ("ALBUM", album.clone()),
        ("ALBUMARTIST", album_artist.clone()),
        ("DATE", date),
        ("CATALOGNUMBER", catalog),
        ("GENRE", genre),
    ]
    .into_iter()
    .filter(|(_, v)| !v.is_empty())
    .collect();

    let stereo_n = md.stereo.as_ref().map(|a| a.tracks.len()).unwrap_or(0);

    let build_track =
        |id: u32, idx_in_area: usize, area_n: usize, t: &super::sacd::TrackEntry| -> SidecarTrack {
            let mut meta: BTreeMap<String, String> = album_proto
                .iter()
                .map(|(k, v)| ((*k).to_string(), v.clone()))
                .collect();

            meta.insert("TRACKNUMBER".to_string(), (idx_in_area + 1).to_string());
            meta.insert("TOTALTRACKS".to_string(), area_n.to_string());

            if let Some(title) = t.text.title.as_deref().filter(|s| !s.is_empty()) {
                meta.insert("TITLE".to_string(), title.to_string());
            }
            let track_artist = t
                .text
                .performer
                .as_deref()
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .unwrap_or_else(|| album_artist.clone());
            if !track_artist.is_empty() {
                meta.insert("ARTIST".to_string(), track_artist);
            }
            if let Some(c) = t.text.composer.as_deref().filter(|s| !s.is_empty()) {
                meta.insert("COMPOSER".to_string(), c.to_string());
            }
            if let Some(sw) = t.text.songwriter.as_deref().filter(|s| !s.is_empty()) {
                meta.insert("LYRICIST".to_string(), sw.to_string());
            }
            if let Some(arr) = t.text.arranger.as_deref().filter(|s| !s.is_empty()) {
                meta.insert("ARRANGER".to_string(), arr.to_string());
            }
            if let Some(isrc) = t.isrc.as_deref().filter(|s| !s.is_empty()) {
                meta.insert("ISRC".to_string(), isrc.to_string());
            }

            SidecarTrack {
                id,
                meta,
                replaygain: BTreeMap::new(),
            }
        };

    let mut tracks: Vec<SidecarTrack> = Vec::new();

    if let Some(area) = md.stereo.as_ref() {
        for (i, t) in area.tracks.iter().enumerate() {
            tracks.push(build_track((i + 1) as u32, i, area.tracks.len(), t));
        }
    }
    if let Some(area) = md.multi_channel.as_ref() {
        for (i, t) in area.tracks.iter().enumerate() {
            let id = (stereo_n + i + 1) as u32;
            tracks.push(build_track(id, i, area.tracks.len(), t));
        }
    }

    SidecarMetadata {
        store_id: String::new(),
        version: "1.1".to_string(),
        tracks,
    }
}

/// Length of the master TOC region hashed for the disc id: 10 sectors
/// × 2048 bytes = 20480 bytes, starting at LSN 510. These are physical
/// SACD spec constants (`MASTER_TOC_LEN`, `SACD_LSN_SIZE`,
/// `START_OF_MASTER_TOC` in the upstream `foo_input_sacd` source).
const DISC_ID_REGION_LSN: u64 = 510;
const SACD_LSN_SIZE: u64 = 2048;
const DISC_ID_REGION_LEN: usize = 10 * SACD_LSN_SIZE as usize;

/// Compute the canonical sacd-extract / foobar2000 metabase disc id
/// from the raw 20480-byte master TOC region. The id is the MD5 of
/// the region formatted as 32 uppercase hex chars (no separators).
///
/// Reference: `foo_input_sacd/sacd_metabase.cpp` constructor — MD5 over
/// `MASTER_TOC_LEN * SACD_LSN_SIZE` bytes starting at
/// `START_OF_MASTER_TOC`, emitted with `%02X` per byte. Verified bit-
/// perfect against 412/414 sidecars in empirical testing; the two
/// non-matches both lack the SACDMTOC magic and aren't standard SACDs.
pub fn compute_disc_id(master_toc_region: &[u8]) -> String {
    use md5::{Digest, Md5};
    debug_assert_eq!(
        master_toc_region.len(),
        DISC_ID_REGION_LEN,
        "disc id must be computed over exactly {} bytes of master TOC",
        DISC_ID_REGION_LEN,
    );
    let mut h = Md5::new();
    h.update(master_toc_region);
    let digest = h.finalize();
    let mut hex = String::with_capacity(32);
    for &b in digest.iter() {
        use std::fmt::Write;
        let _ = write!(&mut hex, "{:02X}", b);
    }
    hex
}

/// Read the master TOC region from an SACD ISO and compute its
/// canonical disc id. Rejects files whose region doesn't start with
/// the `SACDMTOC` magic (non-conformant rips, R2R-to-DSD containers,
/// truncated files) with `SidecarError::NotMetabase`.
///
/// This only supports LSN-format (2048-byte sector) ISOs, which is the
/// universal shape for `.iso` files on disk; physical-disc PSN format
/// (2064-byte sectors with 12-byte sync headers) is not handled because
/// tonepoet never sees raw disc reads.
pub fn mint_disc_id(iso_path: &Path) -> Result<String, SidecarError> {
    use std::fs::File;
    use std::io::{Read, Seek, SeekFrom};
    let mut f = File::open(iso_path).map_err(|e| SidecarError::Io(format!("open: {}", e)))?;
    f.seek(SeekFrom::Start(DISC_ID_REGION_LSN * SACD_LSN_SIZE))
        .map_err(|e| SidecarError::Io(format!("seek: {}", e)))?;
    let mut buf = vec![0u8; DISC_ID_REGION_LEN];
    f.read_exact(&mut buf)
        .map_err(|e| SidecarError::Io(format!("read: {}", e)))?;
    if &buf[..8] != b"SACDMTOC" {
        return Err(SidecarError::NotMetabase);
    }
    Ok(compute_disc_id(&buf))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_SINGLE_AREA: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<!--SACD metabase file-->
<root><store id="ABCDEF0123456789ABCDEF0123456789" type="SACD" version="1.1">
<track id="1"><meta name="TITLE" value="Track One"/><meta name="ARTIST" value="An Artist"/><meta name="TRACKNUMBER" value="01"/><meta name="TOTALTRACKS" value="3"/></track>
<track id="3"><meta name="TITLE" value="Track Three"/><meta name="ARTIST" value="An Artist"/><meta name="TRACKNUMBER" value="03"/><meta name="TOTALTRACKS" value="3"/></track>
<track id="2"><meta name="TITLE" value="Track Two"/><meta name="ARTIST" value="An Artist"/><meta name="TRACKNUMBER" value="02"/><meta name="TOTALTRACKS" value="3"/></track>
</store></root>"#;

    const SAMPLE_HYBRID: &str = r#"<?xml version="1.0"?><root><store id="00112233445566778899AABBCCDDEEFF" type="SACD" version="1.1">
<track id="1"><meta name="TITLE" value="St-1"/><meta name="TRACKNUMBER" value="01"/><meta name="TOTALTRACKS" value="2"/></track>
<track id="2"><meta name="TITLE" value="St-2"/><meta name="TRACKNUMBER" value="02"/><meta name="TOTALTRACKS" value="2"/></track>
<track id="3"><meta name="TITLE" value="MC-1"/><meta name="TRACKNUMBER" value="01"/><meta name="TOTALTRACKS" value="2"/></track>
<track id="4"><meta name="TITLE" value="MC-2"/><meta name="TRACKNUMBER" value="02"/><meta name="TOTALTRACKS" value="2"/></track>
</store></root>"#;

    #[test]
    fn parse_sidecar_extracts_store_and_track_count() {
        let m = parse_sidecar_str(SAMPLE_SINGLE_AREA).expect("parse");
        assert_eq!(m.store_id, "ABCDEF0123456789ABCDEF0123456789");
        assert_eq!(m.version, "1.1");
        assert_eq!(m.tracks.len(), 3);
    }

    #[test]
    fn parse_sidecar_preserves_in_file_track_order() {
        let m = parse_sidecar_str(SAMPLE_SINGLE_AREA).expect("parse");
        // In-file order is 1, 3, 2.
        assert_eq!(m.tracks[0].id, 1);
        assert_eq!(m.tracks[1].id, 3);
        assert_eq!(m.tracks[2].id, 2);
    }

    #[test]
    fn tracks_for_area_sorts_by_tracknumber() {
        let m = parse_sidecar_str(SAMPLE_SINGLE_AREA).expect("parse");
        let a1 = m.tracks_for_area(1);
        assert_eq!(a1.len(), 3);
        assert_eq!(
            a1[0].meta.get("TITLE").map(String::as_str),
            Some("Track One")
        );
        assert_eq!(
            a1[1].meta.get("TITLE").map(String::as_str),
            Some("Track Two")
        );
        assert_eq!(
            a1[2].meta.get("TITLE").map(String::as_str),
            Some("Track Three")
        );
    }

    #[test]
    fn tracks_for_area_splits_hybrid_disc() {
        let m = parse_sidecar_str(SAMPLE_HYBRID).expect("parse");
        let a1 = m.tracks_for_area(1);
        let a2 = m.tracks_for_area(2);
        assert_eq!(a1.len(), 2);
        assert_eq!(a2.len(), 2);
        assert_eq!(a1[0].meta.get("TITLE").map(String::as_str), Some("St-1"));
        assert_eq!(a2[0].meta.get("TITLE").map(String::as_str), Some("MC-1"));
    }

    #[test]
    fn parse_sidecar_picks_up_replaygain_separately() {
        let xml = r#"<root><store id="X" type="SACD" version="1.1">
<track id="1"><meta name="TITLE" value="T"/><replaygain name="replaygain_track_gain" value="+5.00 dB"/></track>
</store></root>"#;
        let m = parse_sidecar_str(xml).expect("parse");
        assert_eq!(m.tracks[0].meta.get("TITLE").map(String::as_str), Some("T"));
        assert_eq!(
            m.tracks[0]
                .replaygain
                .get("replaygain_track_gain")
                .map(String::as_str),
            Some("+5.00 dB"),
        );
    }

    #[test]
    fn parse_sidecar_rejects_non_metabase_xml() {
        let xml = r#"<root><nothing/></root>"#;
        assert!(matches!(
            parse_sidecar_str(xml),
            Err(SidecarError::NotMetabase)
        ));
    }

    #[test]
    fn parse_sidecar_normalises_meta_keys_to_uppercase() {
        // Regression: SME JSACD SRGS 4504 (A Tribute to Jack Johnson)
        // and similar foobar2000-emitted sidecars use lowercase meta
        // key names (`album`, `title`, `artist`, `tracknumber`).
        // Downstream merge code looks up canonical uppercase keys,
        // so the parser must normalise.
        let xml = r#"<root><store id="A" type="SACD" version="1.1">
<track id="1"><meta name="album" value="My Album"/><meta name="title" value="t"/><meta name="artist" value="An Artist"/><meta name="tracknumber" value="1"/><meta name="totaltracks" value="1"/></track>
</store></root>"#;
        let m = parse_sidecar_str(xml).expect("parse");
        let t = &m.tracks[0];
        assert_eq!(t.meta.get("ALBUM").map(String::as_str), Some("My Album"));
        assert_eq!(t.meta.get("TITLE").map(String::as_str), Some("t"));
        assert_eq!(t.meta.get("ARTIST").map(String::as_str), Some("An Artist"));
        assert_eq!(t.meta.get("TRACKNUMBER").map(String::as_str), Some("1"));
        // Original lowercase keys must NOT be present (single
        // canonical form).
        assert!(!t.meta.contains_key("album"));
        assert!(!t.meta.contains_key("title"));
    }

    #[test]
    fn parse_sidecar_tolerates_comments_and_processing_instructions() {
        let xml = r#"<?xml version="1.0"?><!--header comment-->
<root><!--inner--><store id="A" type="SACD" version="1.1"><track id="1"><meta name="TITLE" value="X"/></track></store></root>"#;
        let m = parse_sidecar_str(xml).expect("parse");
        assert_eq!(m.store_id, "A");
        assert_eq!(m.tracks.len(), 1);
    }

    #[test]
    fn decode_xml_entities_handles_basic_set() {
        assert_eq!(decode_xml_entities("plain"), "plain");
        assert_eq!(decode_xml_entities("A &amp; B"), "A & B");
        assert_eq!(decode_xml_entities("&lt;tag&gt;"), "<tag>");
        assert_eq!(decode_xml_entities("&quot;q&quot;"), "\"q\"");
        assert_eq!(decode_xml_entities("&apos;a&apos;"), "'a'");
        // Unknown entities round-trip verbatim.
        assert_eq!(decode_xml_entities("&unknown;X"), "&unknown;X");
        // Unterminated ampersand → preserved.
        assert_eq!(decode_xml_entities("A & B"), "A & B");
    }

    #[test]
    fn find_sidecar_returns_same_stem_when_present() {
        let td = tempfile::tempdir().expect("tempdir");
        let iso = td.path().join("disc.iso");
        let xml = td.path().join("disc.xml");
        std::fs::write(&iso, b"\0\0\0\0").unwrap();
        std::fs::write(&xml, b"<root/>").unwrap();
        assert_eq!(find_sidecar_for_iso(&iso), Some(xml));
    }

    #[test]
    fn find_sidecar_returns_none_when_absent() {
        let td = tempfile::tempdir().expect("tempdir");
        let iso = td.path().join("disc.iso");
        std::fs::write(&iso, b"\0\0\0\0").unwrap();
        assert!(find_sidecar_for_iso(&iso).is_none());
    }

    #[test]
    fn serialize_then_parse_roundtrips_a_track() {
        let mut m = SidecarMetadata {
            store_id: "DEADBEEF0123456789ABCDEF01234567".to_string(),
            version: "1.1".to_string(),
            tracks: Vec::new(),
        };
        let mut t = SidecarTrack::default();
        t.id = 3;
        t.meta.insert("TITLE".to_string(), "My Track".to_string());
        t.meta.insert("ARTIST".to_string(), "An Artist".to_string());
        t.meta.insert("TRACKNUMBER".to_string(), "03".to_string());
        t.replaygain
            .insert("replaygain_track_gain".to_string(), "+5.00 dB".to_string());
        m.tracks.push(t);

        let xml = serialize_sidecar(&m);
        let parsed = parse_sidecar_str(&xml).expect("parse roundtrip");
        assert_eq!(parsed.store_id, m.store_id);
        assert_eq!(parsed.tracks.len(), 1);
        assert_eq!(parsed.tracks[0].id, 3);
        assert_eq!(
            parsed.tracks[0].meta.get("TITLE").map(String::as_str),
            Some("My Track")
        );
        assert_eq!(
            parsed.tracks[0]
                .replaygain
                .get("replaygain_track_gain")
                .map(String::as_str),
            Some("+5.00 dB"),
        );
    }

    #[test]
    fn serialize_escapes_xml_entities_in_values() {
        let mut m = SidecarMetadata {
            store_id: "A".into(),
            version: "1.1".into(),
            tracks: Vec::new(),
        };
        let mut t = SidecarTrack::default();
        t.id = 1;
        t.meta.insert("TITLE".into(), "Foo & <Bar> \"Baz\"".into());
        m.tracks.push(t);
        let xml = serialize_sidecar(&m);
        assert!(xml.contains("&amp;"), "{}", xml);
        assert!(xml.contains("&lt;Bar&gt;"), "{}", xml);
        assert!(xml.contains("&quot;Baz&quot;"), "{}", xml);
        // Roundtrip restores original characters.
        let parsed = parse_sidecar_str(&xml).expect("parse");
        assert_eq!(
            parsed.tracks[0].meta.get("TITLE").map(String::as_str),
            Some("Foo & <Bar> \"Baz\""),
        );
    }

    #[test]
    fn serialize_preserves_foreign_meta_keys() {
        // Foreign keys (DISCOGS_*, DYNAMIC RANGE, etc.) must round-trip
        // — that's the entire point of read-modify-write writes
        // preserving fields tonepoet doesn't surface.
        let mut m = SidecarMetadata {
            store_id: "X".into(),
            version: "1.1".into(),
            tracks: Vec::new(),
        };
        let mut t = SidecarTrack::default();
        t.id = 1;
        t.meta.insert("TITLE".into(), "T".into());
        t.meta.insert("DISCOGS_RELEASE_ID".into(), "12345".into());
        t.meta.insert("DYNAMIC RANGE".into(), "15".into());
        t.meta.insert("PUBLISHER".into(), "Pub Co".into());
        m.tracks.push(t);

        let xml = serialize_sidecar(&m);
        let parsed = parse_sidecar_str(&xml).expect("parse");
        let pt = &parsed.tracks[0];
        assert_eq!(pt.meta.get("TITLE").map(String::as_str), Some("T"));
        assert_eq!(
            pt.meta.get("DISCOGS_RELEASE_ID").map(String::as_str),
            Some("12345")
        );
        assert_eq!(pt.meta.get("DYNAMIC RANGE").map(String::as_str), Some("15"));
        assert_eq!(pt.meta.get("PUBLISHER").map(String::as_str), Some("Pub Co"));
    }

    #[test]
    fn write_sidecar_atomic_replaces_existing_file() {
        let td = tempfile::tempdir().expect("tempdir");
        let path = td.path().join("disc.xml");
        std::fs::write(&path, b"<old>data</old>").unwrap();

        let m = SidecarMetadata {
            store_id: "NEW0000000000000000000000000000A".into(),
            version: "1.1".into(),
            tracks: vec![SidecarTrack {
                id: 1,
                meta: std::iter::once(("TITLE".to_string(), "Hi".to_string())).collect(),
                replaygain: Default::default(),
            }],
        };
        write_sidecar(&path, &m).expect("write");

        let parsed = parse_sidecar(&path).expect("parse what we wrote");
        assert_eq!(parsed.store_id, "NEW0000000000000000000000000000A");
        assert_eq!(parsed.tracks.len(), 1);
        assert_eq!(
            parsed.tracks[0].meta.get("TITLE").map(String::as_str),
            Some("Hi")
        );

        // Tmp file must have been cleaned up.
        let leftovers: Vec<_> = std::fs::read_dir(td.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name().to_string_lossy().starts_with('.')
                    && e.file_name().to_string_lossy().ends_with(".tmp")
            })
            .collect();
        assert!(
            leftovers.is_empty(),
            "tmp file left behind: {:?}",
            leftovers
        );
    }

    fn make_track(
        title: Option<&str>,
        performer: Option<&str>,
        isrc: Option<&str>,
    ) -> super::super::sacd::TrackEntry {
        use super::super::sacd::{PlayTime, TrackEntry, TrackText};
        TrackEntry {
            start_lsn: 0,
            length_lsn: 0,
            start_time: PlayTime {
                minutes: 0,
                seconds: 0,
                frames: 0,
            },
            duration: PlayTime {
                minutes: 3,
                seconds: 30,
                frames: 0,
            },
            text: TrackText {
                title: title.map(String::from),
                performer: performer.map(String::from),
                ..Default::default()
            },
            isrc: isrc.map(String::from),
            structured_isrc: None,
            genre: None,
        }
    }

    fn make_area(
        kind: super::super::sacd::AreaKind,
        n_tracks: usize,
    ) -> super::super::sacd::AreaInfo {
        use super::super::sacd::{AreaInfo, AreaTocHeader, FrameFormat, PlayTime};
        let tracks = (0..n_tracks)
            .map(|i| {
                make_track(
                    Some(&format!("Track {}", i + 1)),
                    Some("Track Artist"),
                    None,
                )
            })
            .collect();
        AreaInfo {
            header: AreaTocHeader {
                kind,
                spec_version: (1, 20),
                size_sectors: 0,
                max_byte_rate: 0,
                sample_frequency: 4,
                frame_format: FrameFormat::Dsd3In14,
                channel_count: if matches!(kind, super::super::sacd::AreaKind::Stereo) {
                    2
                } else {
                    6
                },
                loudspeaker_config: 0,
                extra_settings: 0,
                max_available_channels: 2,
                area_mute_flags: 0,
                total_playtime: PlayTime {
                    minutes: 30,
                    seconds: 0,
                    frames: 0,
                },
                track_offset: 0,
                track_count: n_tracks as u8,
                track_start_lsn: 0,
                track_end_lsn: 0,
                text_area_count: 0,
                locales: vec![],
                description: None,
                description_phonetic: None,
                copyright: None,
                copyright_phonetic: None,
            },
            tracks,
            consistency: Default::default(),
        }
    }

    fn make_md(stereo_n: Option<usize>, mch_n: Option<usize>) -> super::super::sacd::SacdMetadata {
        use super::super::sacd::{
            AreaKind, AreaPointer, DiscDate, Genre, MasterToc, SacdMetadata, SacdText,
        };
        SacdMetadata {
            master_toc: MasterToc {
                spec_version: (1, 20),
                album_set_size: 1,
                album_sequence_number: 1,
                album_catalog_number: "TEST-123".to_string(),
                album_genres: vec![Genre {
                    category: 1,
                    genre: 14,
                }], // Jazz
                two_channel: AreaPointer {
                    toc_1_start: 0,
                    toc_2_start: 0,
                    toc_size_sectors: 0,
                },
                multi_channel: AreaPointer {
                    toc_1_start: 0,
                    toc_2_start: 0,
                    toc_size_sectors: 0,
                },
                disc_type_hybrid: stereo_n.is_some() && mch_n.is_some(),
                disc_catalog_number: String::new(),
                disc_genres: vec![],
                disc_date: Some(DiscDate {
                    year: 1965,
                    month: 3,
                    day: 1,
                }),
                text_area_count: 1,
                locales: vec![],
            },
            master_text: Some(SacdText {
                album_title: Some("Test Album".to_string()),
                album_artist: Some("Test Artist".to_string()),
                album_publisher: None,
                album_copyright: None,
                album_title_phonetic: None,
                album_artist_phonetic: None,
                album_publisher_phonetic: None,
                album_copyright_phonetic: None,
                disc_title: None,
                disc_artist: None,
                disc_publisher: None,
                disc_copyright: None,
                disc_title_phonetic: None,
                disc_artist_phonetic: None,
                disc_publisher_phonetic: None,
                disc_copyright_phonetic: None,
                charset: 2,
            }),
            stereo: stereo_n.map(|n| make_area(AreaKind::Stereo, n)),
            multi_channel: mch_n.map(|n| make_area(AreaKind::MultiChannel, n)),
            consistency: Default::default(),
        }
    }

    #[test]
    fn seed_stereo_only_disc_emits_one_track_per_area_entry() {
        let md = make_md(Some(3), None);
        let s = seed_sidecar_from_scarletbook(&md);
        assert_eq!(s.tracks.len(), 3);
        assert_eq!(s.tracks[0].id, 1);
        assert_eq!(s.tracks[2].id, 3);
        assert_eq!(
            s.tracks[0].meta.get("ALBUM").map(String::as_str),
            Some("Test Album")
        );
        assert_eq!(
            s.tracks[0].meta.get("ALBUMARTIST").map(String::as_str),
            Some("Test Artist")
        );
        assert_eq!(
            s.tracks[0].meta.get("DATE").map(String::as_str),
            Some("1965")
        );
        assert_eq!(
            s.tracks[0].meta.get("CATALOGNUMBER").map(String::as_str),
            Some("TEST-123")
        );
        assert_eq!(
            s.tracks[0].meta.get("GENRE").map(String::as_str),
            Some("Jazz")
        );
        assert_eq!(
            s.tracks[0].meta.get("TRACKNUMBER").map(String::as_str),
            Some("1")
        );
        assert_eq!(
            s.tracks[0].meta.get("TOTALTRACKS").map(String::as_str),
            Some("3")
        );
        assert_eq!(
            s.tracks[0].meta.get("TITLE").map(String::as_str),
            Some("Track 1")
        );
        assert_eq!(
            s.tracks[2].meta.get("TITLE").map(String::as_str),
            Some("Track 3")
        );
        assert!(
            s.store_id.is_empty(),
            "store_id is filled by caller, not seed"
        );
        assert_eq!(s.version, "1.1");
    }

    #[test]
    fn seed_hybrid_disc_populates_both_areas_with_continuous_ids() {
        let md = make_md(Some(2), Some(3));
        let s = seed_sidecar_from_scarletbook(&md);
        // stereo: ids 1, 2; mch: ids 3, 4, 5
        assert_eq!(s.tracks.len(), 5);
        assert_eq!(
            s.tracks.iter().map(|t| t.id).collect::<Vec<_>>(),
            vec![1, 2, 3, 4, 5]
        );
        // TOTALTRACKS is per-area
        assert_eq!(
            s.tracks[0].meta.get("TOTALTRACKS").map(String::as_str),
            Some("2")
        );
        assert_eq!(
            s.tracks[2].meta.get("TOTALTRACKS").map(String::as_str),
            Some("3")
        );
        // TRACKNUMBER restarts per area
        assert_eq!(
            s.tracks[0].meta.get("TRACKNUMBER").map(String::as_str),
            Some("1")
        );
        assert_eq!(
            s.tracks[2].meta.get("TRACKNUMBER").map(String::as_str),
            Some("1")
        );
    }

    #[test]
    fn seed_omits_album_keys_when_scarletbook_provides_no_value() {
        use super::super::sacd::{AreaPointer, MasterToc, SacdMetadata};
        // Master TOC with no date / no catalog / no genres; no SACDText.
        let md = SacdMetadata {
            master_toc: MasterToc {
                spec_version: (1, 20),
                album_set_size: 1,
                album_sequence_number: 1,
                album_catalog_number: String::new(),
                album_genres: vec![],
                two_channel: AreaPointer {
                    toc_1_start: 0,
                    toc_2_start: 0,
                    toc_size_sectors: 0,
                },
                multi_channel: AreaPointer {
                    toc_1_start: 0,
                    toc_2_start: 0,
                    toc_size_sectors: 0,
                },
                disc_type_hybrid: false,
                disc_catalog_number: String::new(),
                disc_genres: vec![],
                disc_date: None,
                text_area_count: 0,
                locales: vec![],
            },
            master_text: None,
            stereo: Some(make_area(super::super::sacd::AreaKind::Stereo, 1)),
            multi_channel: None,
            consistency: Default::default(),
        };
        let s = seed_sidecar_from_scarletbook(&md);
        assert_eq!(s.tracks.len(), 1);
        assert!(s.tracks[0].meta.get("ALBUM").is_none());
        assert!(s.tracks[0].meta.get("ALBUMARTIST").is_none());
        assert!(s.tracks[0].meta.get("DATE").is_none());
        assert!(s.tracks[0].meta.get("CATALOGNUMBER").is_none());
        assert!(s.tracks[0].meta.get("GENRE").is_none());
        // Per-track defaults still emitted
        assert_eq!(
            s.tracks[0].meta.get("TRACKNUMBER").map(String::as_str),
            Some("1")
        );
        assert_eq!(
            s.tracks[0].meta.get("TITLE").map(String::as_str),
            Some("Track 1")
        );
    }

    #[test]
    fn seed_then_serialize_then_parse_roundtrips_structure() {
        let mut s = seed_sidecar_from_scarletbook(&make_md(Some(2), None));
        // Caller fills store_id before writing — simulate that here.
        s.store_id = "0123456789ABCDEF0123456789ABCDEF".to_string();
        let xml = serialize_sidecar(&s);
        let parsed = parse_sidecar_str(&xml).expect("seeded sidecar should round-trip");
        assert_eq!(parsed.store_id, s.store_id);
        assert_eq!(parsed.tracks.len(), 2);
        assert_eq!(
            parsed.tracks[0].meta.get("TITLE").map(String::as_str),
            Some("Track 1")
        );
    }

    #[test]
    fn expected_sidecar_path_for_iso_returns_same_stem_without_existence_check() {
        let p = std::path::Path::new("/tmp/no/such/dir/Foo Bar.iso");
        let got = expected_sidecar_path_for_iso(p).expect("non-empty parent + stem");
        assert_eq!(got, std::path::Path::new("/tmp/no/such/dir/Foo Bar.xml"));
    }

    #[test]
    fn compute_disc_id_is_deterministic_and_32_uppercase_hex() {
        // Inputs must be the canonical 20480-byte length; vary a single
        // byte to confirm sensitivity.
        let mut buf_a = vec![0u8; DISC_ID_REGION_LEN];
        buf_a[0] = 0xAB;
        let a = compute_disc_id(&buf_a);
        let b = compute_disc_id(&buf_a);
        assert_eq!(a, b, "must be deterministic");
        assert_eq!(a.len(), 32);
        assert!(
            a.chars()
                .all(|c| c.is_ascii_digit() || ('A'..='F').contains(&c)),
            "must be uppercase hex, got {}",
            a
        );
        let mut buf_c = vec![0u8; DISC_ID_REGION_LEN];
        buf_c[0] = 0xCD;
        assert_ne!(a, compute_disc_id(&buf_c));
    }

    #[test]
    fn compute_disc_id_matches_known_md5_vector() {
        // Cross-implementation check: MD5 of 20480 zero bytes, computed
        // by Python's hashlib, must match. Locks in both the hash
        // function (MD5, not SHA-256) and the hex casing (uppercase).
        let zeros = vec![0u8; DISC_ID_REGION_LEN];
        assert_eq!(compute_disc_id(&zeros), "DAA100DF6E6711906B61C9AB5AA16032");
    }

    /// End-to-end seed + mint + serialize + reparse against a real
    /// SACD ISO. Verifies the full Phase A mint-on-save data flow.
    /// Only runs when `TONEPOET_SACD_FIXTURE_ISO` is set; if a
    /// foobar2000-emitted sidecar already exists alongside the ISO,
    /// also asserts the canonical disc id matches.
    #[test]
    fn seed_mint_serialize_reparse_against_real_iso_when_env_var_set() {
        let Ok(path) = std::env::var("TONEPOET_SACD_FIXTURE_ISO") else {
            return;
        };
        let p = std::path::Path::new(&path);
        if !p.exists() {
            return;
        }
        let md = super::super::sacd::parse_sacd_iso(p).expect("real SACD ISO should parse");
        let mut s = seed_sidecar_from_scarletbook(&md);
        s.store_id = mint_disc_id(p).expect("mint canonical disc id");
        let xml = serialize_sidecar(&s);
        let reparsed = parse_sidecar_str(&xml)
            .expect("seeded+minted XML should round-trip through parse_sidecar_str");
        assert_eq!(reparsed.store_id.len(), 32);
        assert_eq!(reparsed.store_id, s.store_id);
        assert!(
            !reparsed.tracks.is_empty(),
            "seed should produce at least one track"
        );
        if let Some(real_sc_path) = find_sidecar_for_iso(p) {
            let real =
                parse_sidecar(&real_sc_path).expect("real sidecar alongside ISO should parse");
            assert_eq!(
                reparsed.store_id, real.store_id,
                "Phase A minted id must match foobar2000's existing <store id>",
            );
        }
    }

    /// Real-ISO fixture: only runs when `TONEPOET_SACD_FIXTURE_ISO`
    /// points at an SACD ISO. If `TONEPOET_SACD_FIXTURE_DISC_ID` is
    /// also set, asserts bit-perfect match against foobar2000 / sacd-
    /// extract's metabase id. CI leaves these unset.
    #[test]
    fn mint_disc_id_matches_real_iso_when_env_var_set() {
        let Ok(path) = std::env::var("TONEPOET_SACD_FIXTURE_ISO") else {
            return;
        };
        let p = std::path::Path::new(&path);
        if !p.exists() {
            eprintln!("TONEPOET_SACD_FIXTURE_ISO='{}' not found — skipping", path);
            return;
        }
        let id = mint_disc_id(p).expect("mint_disc_id should succeed on a valid SACD ISO");
        assert_eq!(id.len(), 32, "id should be 32 hex chars: {:?}", id);
        if let Ok(expected) = std::env::var("TONEPOET_SACD_FIXTURE_DISC_ID") {
            assert_eq!(
                id,
                expected.to_uppercase(),
                "minted id should match canonical sacd-extract/foobar2000 disc id",
            );
        }
    }

    /// Real-world sidecar fixture: only runs when
    /// `TONEPOET_SACD_FIXTURE_XML` points at an existing sidecar
    /// file. Verifies the parser handles the in-the-wild shape
    /// produced by sacd-extract / foobar2000 (which the synthetic
    /// fixtures above approximate but don't perfectly replicate).
    /// CI can leave this unset; developers point it at their own
    /// library to smoke-test.
    #[test]
    fn parse_real_sidecar_when_env_var_set() {
        let Ok(path) = std::env::var("TONEPOET_SACD_FIXTURE_XML") else {
            return;
        };
        let p = std::path::Path::new(&path);
        if !p.exists() {
            eprintln!("TONEPOET_SACD_FIXTURE_XML='{}' not found — skipping", path);
            return;
        }
        let m = parse_sidecar(p).unwrap_or_else(|e| {
            panic!("parsing real fixture '{}' failed: {}", path, e);
        });
        assert!(!m.store_id.is_empty(), "store id should be non-empty");
        assert!(
            m.store_id.len() == 32,
            "store id should be 32 hex chars: got {:?}",
            m.store_id
        );
        assert!(!m.tracks.is_empty(), "should have at least one track");
        assert!(
            m.tracks.iter().any(|t| t.meta.contains_key("TITLE")),
            "at least one track should have a TITLE",
        );
    }
}
