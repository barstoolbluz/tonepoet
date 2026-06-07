// SPDX-License-Identifier: GPL-2.0-or-later
//! Metadata types for the DSD file-format module.

use crate::dsd_file::asset::{DsdAssetInfo, DsdAssetMetadata};
use crate::dsd_file::inspect::DsdContainerInfo;

/// File-level metadata view shared by DSF, DSDIFF/DSD, and DSDIFF/DST.
///
/// This is intentionally conservative. It does not interpret arbitrary ID3 or
/// DSDIFF footer payloads as tags; it records validated raw bytes and structural
/// container metadata so higher layers can decide how much parsing they need.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DsdFileMetadata {
    pub container: DsdContainerInfo,
    pub raw_id3v2: Option<Vec<u8>>,
    pub asset_metadata: DsdAssetMetadata,
}

impl DsdFileMetadata {
    pub fn from_container(container: DsdContainerInfo) -> Self {
        Self {
            container,
            raw_id3v2: None,
            asset_metadata: DsdAssetMetadata::default(),
        }
    }

    pub fn from_asset_info(info: &DsdAssetInfo) -> Option<Self> {
        let stream = info.streams.first()?;
        let container = info.provenance.container.clone()?;
        let mut metadata = Self::from_container(container);
        metadata.raw_id3v2 = info.metadata.raw_id3v2.clone();
        metadata.asset_metadata = info.metadata.clone();
        if metadata.container.channel_count != stream.channel_count {
            return None;
        }
        Some(metadata)
    }
}
