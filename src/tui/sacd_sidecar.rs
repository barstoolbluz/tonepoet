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
    let stem = iso.file_stem()?;
    let dir = iso.parent()?;
    let candidate = dir.join(stem).with_extension("xml");
    if candidate.exists() {
        Some(candidate)
    } else {
        None
    }
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
                cur = Some(SidecarTrack { id, ..Default::default() });
            }
            Tag::Close { name } if name == "track" => {
                if let Some(t) = cur.take() {
                    out.tracks.push(t);
                }
            }
            Tag::SelfClose { name, attrs } if name == "meta" => {
                if let Some(t) = cur.as_mut() {
                    if let (Some(k), Some(v)) = (attrs.get("name"), attrs.get("value")) {
                        t.meta.insert(k.clone(), v.clone());
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
    Open { name: String, attrs: BTreeMap<String, String> },
    Close { name: String },
    SelfClose { name: String, attrs: BTreeMap<String, String> },
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
            let close = find_after(bytes, i + 4, b"-->")
                .ok_or_else(|| SidecarError::Malformed(format!("unterminated comment at offset {}", i)))?;
            i = close + 3;
            continue;
        }
        if bytes[i..].starts_with(b"<?") {
            // Processing instruction: skip to "?>"
            let close = find_after(bytes, i + 2, b"?>")
                .ok_or_else(|| SidecarError::Malformed(format!("unterminated PI at offset {}", i)))?;
            i = close + 2;
            continue;
        }
        if bytes[i..].starts_with(b"<!") {
            // DOCTYPE/etc: skip to next ">"
            let close = find_after(bytes, i + 2, b">")
                .ok_or_else(|| SidecarError::Malformed(format!("unterminated declaration at offset {}", i)))?;
            i = close + 1;
            continue;
        }

        // Regular element: <name ... > or </name>
        let close = find_after(bytes, i + 1, b">")
            .ok_or_else(|| SidecarError::Malformed(format!("unterminated tag at offset {}", i)))?;
        let inner = &bytes[i + 1..close];
        i = close + 1;

        if inner.is_empty() {
            return Err(SidecarError::Malformed(format!("empty tag at offset {}", close - 1)));
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
        let body = if self_close { &inner[..inner.len() - 1] } else { inner };
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
        assert_eq!(a1[0].meta.get("TITLE").map(String::as_str), Some("Track One"));
        assert_eq!(a1[1].meta.get("TITLE").map(String::as_str), Some("Track Two"));
        assert_eq!(a1[2].meta.get("TITLE").map(String::as_str), Some("Track Three"));
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
            m.tracks[0].replaygain.get("replaygain_track_gain").map(String::as_str),
            Some("+5.00 dB"),
        );
    }

    #[test]
    fn parse_sidecar_rejects_non_metabase_xml() {
        let xml = r#"<root><nothing/></root>"#;
        assert!(matches!(parse_sidecar_str(xml), Err(SidecarError::NotMetabase)));
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
        assert!(m.store_id.len() == 32, "store id should be 32 hex chars: got {:?}", m.store_id);
        assert!(!m.tracks.is_empty(), "should have at least one track");
        assert!(
            m.tracks.iter().any(|t| t.meta.contains_key("TITLE")),
            "at least one track should have a TITLE",
        );
    }
}
