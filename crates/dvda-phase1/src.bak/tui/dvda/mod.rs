#![forbid(unsafe_code)]

//! DVD-Audio Phase 1 reader: volume abstraction plus AMG, ATSI, and SAMG
//! navigation parsing. This module deliberately does not demux AOB data,
//! decode MLP/LPCM, construct `TrackSourceRef`, or call the conversion pipeline.

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
pub use volume::{DirectoryDvdaVolume, DvdaFile, DvdaVolume};

#[cfg(feature = "iso-isomage")]
pub use volume::IsoDvdaVolume;
