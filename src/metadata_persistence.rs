//! Authoritative metadata-writer routing and numbering-value capabilities.
//!
//! This module is deliberately UI-neutral. The metadata persistence layer uses
//! [`metadata_persistence_route_for_path`] to select its write route. Feature
//! layers query [`metadata_numbering_capability_for_path`] so they evaluate
//! the actual backend selected by that route. Unknown routes and unclassified Lofty tag
//! types fail closed: adding support requires an explicit backend declaration
//! here rather than inheriting an unsafe default.

use std::io::Read;
use std::path::Path;

use lofty::file::TaggedFileExt;
use lofty::tag::{ItemKey, TagType};

const FLAC_MAGIC: &[u8; 4] = b"fLaC";

/// Top-level persistence route used by the metadata writer.
///
/// The native routes are format-owned. `Lofty` means the writer probes the
/// carrier and writes its actual primary tag type through Lofty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataPersistenceRoute {
    NativeFlacVorbis,
    NativeDsfId3,
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
    LoftyVorbisComments,
    LoftyId3v2,
    LoftyApe,
    LoftyMp4Ilst,
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
    /// Fraction representations such as `1/17` and padded variants.
    pub numeric_fraction: bool,
    /// Lexical representations whose spelling matters, including `01` and
    /// side-prefixed values such as `A01`.
    pub lexical: bool,
}

impl MetadataNumberingCapabilities {
    pub const NONE: Self = Self {
        plain_unsigned: false,
        numeric_fraction: false,
        lexical: false,
    };

    pub const TEXTUAL: Self = Self {
        plain_unsigned: true,
        numeric_fraction: true,
        lexical: true,
    };

    pub const PLAIN_UNSIGNED_ONLY: Self = Self {
        plain_unsigned: true,
        numeric_fraction: false,
        lexical: false,
    };

    pub const fn intersection(self, other: Self) -> Self {
        Self {
            plain_unsigned: self.plain_unsigned && other.plain_unsigned,
            numeric_fraction: self.numeric_fraction && other.numeric_fraction,
            lexical: self.lexical && other.lexical,
        }
    }

    pub const fn supports(self, representation: MetadataNumberingRepresentation) -> bool {
        match representation {
            MetadataNumberingRepresentation::PlainUnsigned => self.plain_unsigned,
            MetadataNumberingRepresentation::NumericFraction => self.numeric_fraction,
            MetadataNumberingRepresentation::Lexical => self.lexical,
        }
    }
}

/// Representation requirements understood by persistence and feature layers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataNumberingRepresentation {
    PlainUnsigned,
    NumericFraction,
    Lexical,
}

impl MetadataPersistenceBackend {
    /// Capabilities are exhaustive by backend. New backend variants cannot
    /// compile without making an explicit safe declaration here.
    pub const fn numbering_capabilities(self) -> MetadataNumberingCapabilities {
        match self {
            Self::NativeFlacVorbis
            | Self::LoftyVorbisComments
            | Self::LoftyId3v2
            | Self::LoftyApe => MetadataNumberingCapabilities::TEXTUAL,
            Self::NativeDsfId3 | Self::LoftyMp4Ilst => {
                MetadataNumberingCapabilities::PLAIN_UNSIGNED_ONLY
            }
            Self::UnsupportedDff | Self::UnclassifiedLofty => {
                MetadataNumberingCapabilities::NONE
            }
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::NativeFlacVorbis => "native FLAC/Vorbis comments",
            Self::NativeDsfId3 => "native DSF/ID3",
            Self::LoftyVorbisComments => "Lofty Vorbis comments",
            Self::LoftyId3v2 => "Lofty ID3v2",
            Self::LoftyApe => "Lofty APE",
            Self::LoftyMp4Ilst => "Lofty MP4 ilst",
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
    let mut magic = [0u8; 4];
    std::fs::File::open(path)
        .and_then(|mut file| file.read_exact(&mut magic))
        .map(|()| &magic == FLAC_MAGIC)
        .unwrap_or(false)
}

/// Resolve the same top-level route used by the metadata writer.
///
/// Dispatch order is intentional: DSF is extension-owned; FLAC uses either its
/// extension or file magic; DFF is explicitly unsupported; every other path is
/// delegated to Lofty's content probe.
pub fn metadata_persistence_route_for_path(path: &Path) -> MetadataPersistenceRoute {
    if extension_is(path, "dsf") {
        MetadataPersistenceRoute::NativeDsfId3
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

/// Normalize logical editor numbering keys to the typed keys required by the
/// concrete Lofty serializer.
///
/// Core editor rows can be synthesized before a carrier has any corresponding
/// stored item, so their keys are deliberately represented as `Unknown` logical
/// names. ID3v2, APE, and especially MP4 ilst must receive typed numbering keys
/// in order to serialize standard `TRCK`/`TRACK`, `trkn`, and `disk` structures
/// rather than format-specific free-form fields. Unclassified backends remain
/// untouched and fail closed at the capability boundary.
pub fn normalize_numbering_item_key_for_backend(
    backend: MetadataPersistenceBackend,
    key: &ItemKey,
) -> ItemKey {
    if !matches!(
        backend,
        MetadataPersistenceBackend::LoftyId3v2
            | MetadataPersistenceBackend::LoftyApe
            | MetadataPersistenceBackend::LoftyMp4Ilst
    ) {
        return key.clone();
    }

    let ItemKey::Unknown(name) = key else {
        return key.clone();
    };
    let canonical = name
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .map(|character| character.to_ascii_uppercase())
        .collect::<String>();
    match canonical.as_str() {
        "TRACKNUMBER" => ItemKey::TrackNumber,
        "TRACKTOTAL" | "TOTALTRACKS" => ItemKey::TrackTotal,
        "DISCNUMBER" | "DISKNUMBER" => ItemKey::DiscNumber,
        "DISCTOTAL" | "DISKTOTAL" | "TOTALDISCS" => ItemKey::DiscTotal,
        _ => key.clone(),
    }
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

    #[test]
    fn every_declared_backend_reports_explicit_capabilities() {
        for backend in [
            MetadataPersistenceBackend::NativeFlacVorbis,
            MetadataPersistenceBackend::LoftyVorbisComments,
            MetadataPersistenceBackend::LoftyId3v2,
            MetadataPersistenceBackend::LoftyApe,
        ] {
            assert_eq!(
                backend.numbering_capabilities(),
                MetadataNumberingCapabilities::TEXTUAL,
                "unexpected textual capabilities for {backend:?}"
            );
        }
        for backend in [
            MetadataPersistenceBackend::NativeDsfId3,
            MetadataPersistenceBackend::LoftyMp4Ilst,
        ] {
            assert_eq!(
                backend.numbering_capabilities(),
                MetadataNumberingCapabilities::PLAIN_UNSIGNED_ONLY,
                "unexpected numeric capabilities for {backend:?}"
            );
        }
        for backend in [
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
            MetadataPersistenceBackend::LoftyId3v2,
            MetadataPersistenceBackend::LoftyApe,
        ] {
            let capabilities = backend.numbering_capabilities();
            assert!(capabilities.supports(MetadataNumberingRepresentation::PlainUnsigned));
            assert!(capabilities.supports(MetadataNumberingRepresentation::NumericFraction));
            assert!(capabilities.supports(MetadataNumberingRepresentation::Lexical));
        }

        for backend in [
            MetadataPersistenceBackend::NativeDsfId3,
            MetadataPersistenceBackend::LoftyMp4Ilst,
        ] {
            let capabilities = backend.numbering_capabilities();
            assert!(capabilities.supports(MetadataNumberingRepresentation::PlainUnsigned));
            assert!(!capabilities.supports(MetadataNumberingRepresentation::NumericFraction));
            assert!(!capabilities.supports(MetadataNumberingRepresentation::Lexical));
        }
    }

    #[test]
    fn typed_lofty_backends_normalize_synthetic_numbering_keys() {
        for backend in [
            MetadataPersistenceBackend::LoftyId3v2,
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
                .intersection(MetadataNumberingCapabilities::NONE),
            MetadataNumberingCapabilities::NONE
        );
    }
}
