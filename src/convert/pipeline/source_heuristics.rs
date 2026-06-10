//! Source-specific naming heuristics.
//!
//! This module enriches `PreparedSource` metadata with naming tokens
//! derived from archive filenames, passwords, and label dictionaries.
//! It hooks into the pipeline between materialization and template
//! rendering — the orchestrator calls `maybe_enrich` which checks
//! trigger conditions and populates `album_metadata.extra` with tokens
//! that the existing `resolve_extra_tokens` system resolves.

use std::path::Path;

use super::types::PreparedSource;

// ── Trigger detection ───────────────────────────────────────────────

const ARCHIVE_PASSWORD: &str = "b0nn13mCmurr@y";
const UPLOADER_NAME: &str = "PBThal";

/// Check whether the source should receive archive-specific naming enrichment.
pub fn is_active(archive_password: Option<&str>, folder_template: Option<&str>) -> bool {
    if let Some(pw) = archive_password {
        if pw == ARCHIVE_PASSWORD {
            return true;
        }
    }
    if let Some(tmpl) = folder_template {
        let lower = tmpl.to_ascii_lowercase();
        if lower.contains("%pbthal%") || lower.contains("%uploader%") || lower.contains("%archive_year%") {
            return true;
        }
    }
    false
}

/// Enrich source metadata if trigger conditions are met.
pub fn maybe_enrich(
    source: &mut PreparedSource,
    container: &Path,
    archive_password: Option<&str>,
    folder_template: Option<&str>,
) {
    if !is_active(archive_password, folder_template) {
        return;
    }
    enrich(source, container);
}

// ── Enrichment ──────────────────────────────────────────────────────

fn enrich(source: &mut PreparedSource, container: &Path) {
    let extra = &mut source.album_metadata.extra;
    let filename = container
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");

    // Static tokens.
    extra
        .entry("pbthal".to_string())
        .or_insert_with(|| UPLOADER_NAME.to_string());
    extra
        .entry("uploader".to_string())
        .or_insert_with(|| UPLOADER_NAME.to_string());

    // Source spec from actual track data (e.g., "24-96").
    let source_spec = source
        .tracks
        .first()
        .map(|t| format_source_spec(t.bit_depth, t.sample_rate))
        .unwrap_or_else(|| "24-96".to_string());
    extra
        .entry("source_spec".to_string())
        .or_insert(source_spec.clone());

    // Archive date from filename (e.g., "-jun-2022.7z").
    if let Some((year, month)) = extract_archive_date(filename) {
        extra
            .entry("archive_year".to_string())
            .or_insert(year);
        extra
            .entry("archive_month".to_string())
            .or_insert(month);
    }

    // Pressing info from parenthetical hint in filename.
    if let Some(hint) = extract_pressing_hint(filename) {
        let year = source
            .album_metadata
            .date
            .as_deref()
            .and_then(extract_four_digit_year);
        let raw_info = resolve_pressing_info(&hint, year.as_deref());
        // Strip the baked-in "  24-96" suffix from the dictionary result.
        // The source spec is a separate token (%SOURCE_SPEC%) so the user
        // controls the separator in their template.
        let info = strip_spec_suffix(&raw_info);
        extra.entry("pressing_info".to_string()).or_insert(info.clone());

        // Strip the pressing hint from the album name so it doesn't
        // appear twice (once in %ALBUM%, once in %PRESSING_INFO%).
        // The hint is baked into the FLAC tags by the original tagger.
        let parens = format!("({})", hint);
        if let Some(ref mut album) = source.album_metadata.album {
            if album.contains(&parens) {
                *album = album.replace(&parens, "").trim().to_string();
            }
        }
        // Also strip from per-track extra["album"] if present.
        for track in &mut source.tracks {
            if let Some(track_album) = track.metadata.extra.get_mut("album") {
                if track_album.contains(&parens) {
                    *track_album = track_album.replace(&parens, "").trim().to_string();
                }
            }
        }

        // Compose the album tag override for the metadata stage.
        // Format: "Album (PressingInfo / SourceSpec) [Uploader]"
        // This goes into the ALBUM tag without corrupting album_metadata.album
        // (which templates and CUE sheets use).
        let uploader = extra
            .get("uploader")
            .cloned()
            .unwrap_or_else(|| UPLOADER_NAME.to_string());
        if let Some(ref album) = source.album_metadata.album {
            let album_tag = format!("{album} ({info} / {source_spec}) [{uploader}]");
            extra
                .entry("album_tag_override".to_string())
                .or_insert(album_tag);
        }
    }

    // Title casing on album metadata and track metadata.
    apply_title_casing(source);
}

// ── Archive filename parsing ────────────────────────────────────────

/// Extract `(year, month)` from an archive filename's `-month-year` suffix.
///
/// `"38 Special - Special Forces-jun-2022.7z"` → `Some(("2022", "jun"))`
fn extract_archive_date(filename: &str) -> Option<(String, String)> {
    // Strip extension.
    let base = filename.rsplit_once('.').map(|(b, _)| b).unwrap_or(filename);

    // Find the last `-month-year` pattern.
    let months = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    let lower = base.to_ascii_lowercase();
    for month in &months {
        let pattern = format!("-{}-", month);
        if let Some(pos) = lower.rfind(&pattern) {
            let after = &base[pos + pattern.len()..];
            if after.len() == 4 && after.chars().all(|c| c.is_ascii_digit()) {
                return Some((after.to_string(), month.to_string()));
            }
        }
    }
    None
}

/// Extract parenthetical content from the archive filename, after stripping
/// the date suffix.
///
/// `"10,000 Maniacs - Our Time In Eden (AF)-dec-2021.7z"` → `Some("AF")`
/// `"38 Special - Special Forces-jun-2022.7z"` → `None`
fn extract_pressing_hint(filename: &str) -> Option<String> {
    // Strip extension.
    let base = filename.rsplit_once('.').map(|(b, _)| b).unwrap_or(filename);

    // Strip date suffix if present.
    let months = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    let lower = base.to_ascii_lowercase();
    let mut core = base;
    for month in &months {
        let pattern = format!("-{}-", month);
        if let Some(pos) = lower.rfind(&pattern) {
            let after = &base[pos + pattern.len()..];
            if after.len() == 4 && after.chars().all(|c| c.is_ascii_digit()) {
                core = &base[..pos];
                break;
            }
        }
    }

    // Find the last parenthetical group.
    let open = core.rfind('(')?;
    let close = core[open..].find(')')? + open;
    let content = core[open + 1..close].trim();
    if content.is_empty() {
        return None;
    }
    Some(content.to_string())
}

/// Resolve a pressing hint string to a full pressing info description
/// using the label dictionary in `labels.rs`.
///
/// `detect_pressing_info` expects a folder-name-like string with the hint
/// inside parentheses, so we wrap it: `"AF"` → `"(AF)"`.
fn resolve_pressing_info(hint: &str, year: Option<&str>) -> String {
    let wrapped = format!("({hint})");
    let label_info = crate::convert::labels::detect_pressing_info(&wrapped, year);
    let default = "US First-Press LP  24-96";
    if label_info.pressing_info != default {
        label_info.pressing_info
    } else {
        // The dictionary didn't match — use the raw hint.
        format!("{hint} LP")
    }
}

/// Extract a 4-digit year from a date string like "1979" or "1979-03-15".
fn extract_four_digit_year(date: &str) -> Option<String> {
    let trimmed = date.trim();
    if trimmed.len() >= 4 && trimmed[..4].chars().all(|c| c.is_ascii_digit()) {
        Some(trimmed[..4].to_string())
    } else {
        None
    }
}

/// Format source spec from bit depth and sample rate.
/// `(Some(24), 96000)` → `"24-96"`, `(Some(16), 44100)` → `"16-44.1"`
fn format_source_spec(bit_depth: Option<u32>, sample_rate: Option<u32>) -> String {
    let depth = bit_depth.unwrap_or(24);
    let rate_khz = sample_rate.unwrap_or(96000) as f64 / 1000.0;
    if rate_khz == rate_khz.floor() {
        format!("{}-{}", depth, rate_khz as u32)
    } else {
        // Trim trailing zeros: 44.100 → 44.1
        let formatted = format!("{:.1}", rate_khz);
        format!("{}-{}", depth, formatted)
    }
}

/// Strip the baked-in `  dd-rr` spec suffix from a label dictionary result.
/// `"UK First-Press LP  24-96"` → `"UK First-Press LP"`
fn strip_spec_suffix(info: &str) -> String {
    // The dictionary consistently uses double-space before the spec.
    if let Some(pos) = info.rfind("  ") {
        let suffix = info[pos + 2..].trim();
        // Verify the suffix looks like a spec (digits-digits).
        if suffix.contains('-') && suffix.chars().all(|c| c.is_ascii_digit() || c == '-' || c == '.') {
            return info[..pos].to_string();
        }
    }
    info.to_string()
}

// ── Title casing ────────────────────────────────────────────────────

fn apply_title_casing(source: &mut PreparedSource) {
    if let Some(ref mut album) = source.album_metadata.album {
        *album = crate::convert::renaming::capitalize_title(album);
    }
    if let Some(ref mut artist) = source.album_metadata.album_artist {
        *artist = crate::convert::renaming::capitalize_title(artist);
    }
    for track in &mut source.tracks {
        if let Some(ref mut title) = track.metadata.title {
            *title = crate::convert::renaming::capitalize_title(title);
        }
        if let Some(ref mut artist) = track.metadata.artist {
            *artist = crate::convert::renaming::capitalize_title(artist);
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_password_match() {
        assert!(is_active(Some("b0nn13mCmurr@y"), None));
    }

    #[test]
    fn trigger_template_token_pbthal() {
        assert!(is_active(None, Some("%ARTIST% - %ALBUM% [%PBTHAL%]")));
    }

    #[test]
    fn trigger_template_token_uploader() {
        assert!(is_active(None, Some("%ARTIST% [%UPLOADER%]")));
    }

    #[test]
    fn trigger_template_token_archive_year() {
        assert!(is_active(None, Some("[%ARCHIVE_YEAR%]")));
    }

    #[test]
    fn trigger_template_case_insensitive() {
        assert!(is_active(None, Some("[%pbthal%]")));
    }

    #[test]
    fn no_trigger_without_match() {
        assert!(!is_active(None, Some("%ARTIST%/%ALBUM%")));
        assert!(!is_active(Some("wrong_password"), None));
        assert!(!is_active(None, None));
    }

    #[test]
    fn date_extraction_standard() {
        assert_eq!(
            extract_archive_date("38 Special - Special Forces-jun-2022.7z"),
            Some(("2022".into(), "jun".into()))
        );
    }

    #[test]
    fn date_extraction_case_insensitive() {
        assert_eq!(
            extract_archive_date("Album-JAN-2019.7z"),
            Some(("2019".into(), "jan".into()))
        );
    }

    #[test]
    fn date_extraction_no_date() {
        assert_eq!(extract_archive_date("Album.7z"), None);
    }

    #[test]
    fn date_extraction_incomplete_year() {
        assert_eq!(extract_archive_date("Album-jun-22.7z"), None);
    }

    #[test]
    fn pressing_hint_with_parens() {
        assert_eq!(
            extract_pressing_hint("10,000 Maniacs - Our Time In Eden (AF)-dec-2021.7z"),
            Some("AF".into())
        );
    }

    #[test]
    fn pressing_hint_country() {
        assert_eq!(
            extract_pressing_hint("UFO - 1 (German)-jun-2016.7z"),
            Some("German".into())
        );
    }

    #[test]
    fn pressing_hint_no_parens() {
        assert_eq!(
            extract_pressing_hint("38 Special - Special Forces-jun-2022.7z"),
            None
        );
    }

    #[test]
    fn pressing_hint_empty_parens() {
        assert_eq!(
            extract_pressing_hint("Artist - Album ()-jun-2022.7z"),
            None
        );
    }

    #[test]
    fn pressing_resolution_known_label() {
        let info = resolve_pressing_info("AF", None);
        assert!(
            info.contains("Audio Fidelity") || info.contains("AF"),
            "expected Audio Fidelity match, got: {info}"
        );
    }

    #[test]
    fn pressing_resolution_country() {
        let info = resolve_pressing_info("UK", None);
        assert!(info.contains("UK"), "expected UK in pressing info, got: {info}");
    }

    #[test]
    fn pressing_resolution_unknown_falls_back() {
        let info = resolve_pressing_info("XYZ_UNKNOWN", None);
        assert_eq!(info, "XYZ_UNKNOWN LP");
    }

    #[test]
    fn four_digit_year_extraction() {
        assert_eq!(extract_four_digit_year("1979"), Some("1979".into()));
        assert_eq!(extract_four_digit_year("1979-03-15"), Some("1979".into()));
        assert_eq!(extract_four_digit_year("abc"), None);
        assert_eq!(extract_four_digit_year(""), None);
    }

    #[test]
    fn pressing_hint_stripped_from_album_name() {
        use super::super::types::*;
        use std::collections::BTreeMap;
        use std::path::PathBuf;

        let mut source = PreparedSource {
            container: PathBuf::from("archive.7z"),
            kind: SourceKind::SevenZip,
            tracks: vec![PreparedTrack {
                id: TrackId { source_ordinal: 1, disc_number: None, track_number: 1 },
                source_ref: TrackSourceRef::StagedFile(PathBuf::from("01.flac")),
                metadata: TrackMetadata {
                    title: Some("Dreadlock Holiday".into()),
                    extra: {
                        let mut m = BTreeMap::new();
                        m.insert("album".into(), "Bloody Tourists (UK)".into());
                        m
                    },
                    ..TrackMetadata::default()
                },
                expected_samples: None,
                sample_rate: Some(96000),
                bit_depth: Some(24),
            source_audio: SourceAudioDescriptor::default(),
            }],
            album_metadata: AlbumMetadata {
                album: Some("Bloody Tourists (UK)".into()),
                album_artist: Some("10cc".into()),
                date: Some("1978".into()),
                ..AlbumMetadata::default()
            },
            provenance: ExtractionProvenance {
                source_kind: SourceKind::SevenZip,
                source_sha256: None,
                tool_versions: BTreeMap::new(),
                extracted_at: chrono::Utc::now(),
            },
        };

        enrich(
            &mut source,
            std::path::Path::new("10cc - Bloody Tourists (UK)-jun-2023.7z"),
        );

        assert_eq!(source.album_metadata.album.as_deref(), Some("Bloody Tourists"));
        assert_eq!(
            source.tracks[0].metadata.extra.get("album").map(String::as_str),
            Some("Bloody Tourists")
        );
        let pi = source.album_metadata.extra.get("pressing_info").expect("pressing_info");
        assert!(!pi.contains("24-96"), "pressing_info should not contain spec suffix, got: {pi}");
        assert!(pi.contains("UK"), "pressing_info should contain country, got: {pi}");

        let spec = source.album_metadata.extra.get("source_spec").expect("source_spec");
        assert_eq!(spec, "24-96");

        let tag = source.album_metadata.extra.get("album_tag_override").expect("album_tag_override");
        assert!(tag.starts_with("Bloody Tourists ("), "album tag should start with album name, got: {tag}");
        assert!(tag.contains(" / 24-96)"), "album tag should contain / source_spec, got: {tag}");
        assert!(tag.ends_with("[PBThal]"), "album tag should end with uploader, got: {tag}");
    }

    #[test]
    fn source_spec_standard() {
        assert_eq!(format_source_spec(Some(24), Some(96000)), "24-96");
    }

    #[test]
    fn source_spec_cd_quality() {
        assert_eq!(format_source_spec(Some(16), Some(44100)), "16-44.1");
    }

    #[test]
    fn source_spec_hi_res() {
        assert_eq!(format_source_spec(Some(24), Some(192000)), "24-192");
    }

    #[test]
    fn source_spec_48k() {
        assert_eq!(format_source_spec(Some(24), Some(48000)), "24-48");
    }

    #[test]
    fn strip_spec_standard() {
        assert_eq!(strip_spec_suffix("UK First-Press LP  24-96"), "UK First-Press LP");
    }

    #[test]
    fn strip_spec_no_suffix() {
        assert_eq!(strip_spec_suffix("UK First-Press LP"), "UK First-Press LP");
    }

    #[test]
    fn strip_spec_single_space_preserved() {
        assert_eq!(strip_spec_suffix("MFSL LP 24-96"), "MFSL LP 24-96");
    }

    #[test]
    fn album_tag_override_idempotent() {
        use super::super::types::*;
        use std::collections::BTreeMap;
        use std::path::PathBuf;

        let mut source = PreparedSource {
            container: PathBuf::from("archive.7z"),
            kind: SourceKind::SevenZip,
            tracks: vec![PreparedTrack {
                id: TrackId { source_ordinal: 1, disc_number: None, track_number: 1 },
                source_ref: TrackSourceRef::StagedFile(PathBuf::from("01.flac")),
                metadata: TrackMetadata {
                    title: Some("Track".into()),
                    extra: {
                        let mut m = BTreeMap::new();
                        m.insert("album".into(), "Album (UK)".into());
                        m
                    },
                    ..TrackMetadata::default()
                },
                expected_samples: None,
                sample_rate: Some(96000),
                bit_depth: Some(24),
            source_audio: SourceAudioDescriptor::default(),
            }],
            album_metadata: AlbumMetadata {
                album: Some("Album (UK)".into()),
                album_artist: Some("Artist".into()),
                date: Some("2000".into()),
                ..AlbumMetadata::default()
            },
            provenance: ExtractionProvenance {
                source_kind: SourceKind::SevenZip,
                source_sha256: None,
                tool_versions: BTreeMap::new(),
                extracted_at: chrono::Utc::now(),
            },
        };

        let path = std::path::Path::new("Artist - Album (UK)-jan-2020.7z");
        enrich(&mut source, path);
        let tag_after_first = source.album_metadata.extra.get("album_tag_override").cloned();

        enrich(&mut source, path);
        let tag_after_second = source.album_metadata.extra.get("album_tag_override").cloned();

        assert_eq!(tag_after_first, tag_after_second, "album_tag_override should be idempotent");
        // album_metadata.album should still be clean (not double-wrapped)
        let album = source.album_metadata.album.as_deref().unwrap();
        assert!(!album.contains("[PBThal]"), "album should not contain tag wrapper, got: {album}");
    }
}
