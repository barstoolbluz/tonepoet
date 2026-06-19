#![forbid(unsafe_code)]

//! DVD-Audio Phase 1 reader: volume abstraction plus AMG, ATSI, and SAMG
//! navigation parsing plus lightweight AOB readability probing for CPPM/MKB
//! classification. This module deliberately does not decode MLP/LPCM, construct
//! `TrackSourceRef`, or call the conversion pipeline.

pub mod cppm;
pub mod endian;
pub mod error;
pub mod ifo;
pub mod model;
pub mod parser;
pub mod sector;
pub mod volume;

pub use error::{DvdaError, Result};
pub use model::*;
pub use parser::parse_dvda_volume;
pub use cppm::refine_copy_protection_from_aob_probe;
pub use volume::{DirectoryDvdaVolume, DvdaFile, DvdaVolume, Iso9660DvdaVolume, IsoUdfDvdaVolume, UdfAudioTsFileInfo, UdfFileExtent, UdfFileStorageKind};

#[cfg(feature = "iso-isomage")]
pub use volume::IsoDvdaVolume;
