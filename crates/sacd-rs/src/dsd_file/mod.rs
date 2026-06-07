// SPDX-License-Identifier: GPL-2.0-or-later
//! DSD file-format module.
//!
//! This module is the canonical home for DSF, DSDIFF/DSD, and DSDIFF/DST
//! container handling in `sacd-rs`. It consolidates the formerly separate
//! inspector, streaming reader, source-model, asset-model, validation, and
//! corpus helpers under one namespace while preserving the old root modules as
//! compatibility re-exports.
//!
//! The design intentionally keeps SACD ISO extraction separate: ScarletBook ISO
//! sector parsing still lives in `frame`/`extract`, while ISO tracks can be
//! adapted into the same [`DsdSource`] model as DSF and DSDIFF files.

use std::io::{Read, Seek};

pub mod asset;
pub mod corpus;
pub mod inspect;
pub mod metadata;
pub mod ops;
pub mod policy;
pub mod reader;
pub mod source;

pub use asset::{
    open_dsd_asset, DsdAsset, DsdAssetError, DsdAssetInfo, DsdAssetKind, DsdAssetMetadata,
    DsdAssetProvenance, DsdAudioStreamInfo, DsdFileAsset, SacdIsoTrackAsset,
};
pub use corpus::{
    report_has_decoded_dst_coverage, validate_dsd_corpus_paths, DsdCorpusAcceptanceFailure,
    DsdCorpusEntryReport, DsdCorpusValidationOptions, DsdCorpusValidationReport,
};
pub use inspect::{
    inspect_dsd_container, inspect_dsdiff, inspect_dsf, DsdByteOrder, DsdCompression,
    DsdContainerDiagnostic, DsdContainerDiagnosticSeverity, DsdContainerError, DsdContainerFormat,
    DsdContainerInfo,
};
pub use metadata::DsdFileMetadata;
pub use ops::{
    describe_container, validate_dsd_stream, write_decoded_dsd_to_dff, write_decoded_dsd_to_dsf,
    DsdSourceKind as ValidationDsdSourceKind, DsdStreamCopyStats, DsdValidationFailure,
    DsdValidationFailureKind, DsdValidationMode, DsdValidationOptions, DsdValidationReport,
};
pub use policy::{DsdFileReadPolicies, DsdiffIndexValidationPolicy, DstCrcValidationPolicy};
pub use reader::{
    open_dsd_as_decoded_reader, open_dsd_file, DsdChannelFrame, DsdDecodedFileReader,
    DsdFileReader, DsdFrame, DsdFrameReader, DsdFrameRef, DsdFrameSeek, DsdReadError, DsfStreamReader,
    DsdDsdiffStreamReader, DstCrcStatus, DstDsdiffStreamReader, DstFrame, DstFrameReader,
    DstToDsdAdapter,
};
pub use source::{
    drain_decoded_dsd_source, open_dsd_source, DecodedDsdSource, DsdFileSource, DsdSource,
    DsdSourceDrainStats, DsdSourceError, DsdSourceFrame, DsdSourceInfo,
    DsdSourceKind as CommonDsdSourceKind, DsdSourceSeek, IsoTrackRange, IsoTrackSource,
    IsoTrackSourceOptions, SourceDsdFrame, SourceDstFrame, SourceToDsdAdapter,
};

/// Conventional names for the concrete file readers requested by callers that
/// prefer format names over the original stream-reader names.
pub type DsfReader<R> = DsfStreamReader<R>;
pub type DsdiffDsdReader<R> = DsdDsdiffStreamReader<R>;
pub type DsdiffDstReader<R> = DstDsdiffStreamReader<R>;

/// Stateless DSDIFF inspector facade.
pub struct DsdiffInspector;

impl DsdiffInspector {
    pub fn inspect<R: Read + Seek>(reader: &mut R) -> Result<DsdContainerInfo, DsdContainerError> {
        inspect_dsdiff(reader)
    }
}

/// Stateless DSF inspector facade.
pub struct DsfInspector;

impl DsfInspector {
    pub fn inspect<R: Read + Seek>(reader: &mut R) -> Result<DsdContainerInfo, DsdContainerError> {
        inspect_dsf(reader)
    }
}

/// Open a DSD file and apply caller-selected CRC/index policies.
pub fn open_dsd_file_with_policies<R: Read + Seek>(
    reader: R,
    policies: DsdFileReadPolicies,
) -> Result<DsdFileReader<R>, DsdReadError> {
    let opened = open_dsd_file(reader)?;
    policies.validate_opened_reader(&opened)?;
    Ok(opened)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_reader_aliases_are_usable<R: Read + Seek>() {
        fn _accept_dsf<T: Read + Seek>(_r: Option<DsfReader<T>>) {}
        fn _accept_dsdiff_dsd<T: Read + Seek>(_r: Option<DsdiffDsdReader<T>>) {}
        fn _accept_dsdiff_dst<T: Read + Seek>(_r: Option<DsdiffDstReader<T>>) {}
        _accept_dsf::<R>(None);
        _accept_dsdiff_dsd::<R>(None);
        _accept_dsdiff_dst::<R>(None);
    }

    #[test]
    fn facade_exports_expected_reader_aliases() {
        assert_reader_aliases_are_usable::<std::io::Cursor<Vec<u8>>>();
    }

    #[test]
    fn default_policies_are_interoperable() {
        let policies = DsdFileReadPolicies::default();
        assert_eq!(policies.dst_crc, DstCrcValidationPolicy::Optional);
        assert_eq!(policies.dsdiff_index, DsdiffIndexValidationPolicy::ValidateWhenPresent);
    }
}
