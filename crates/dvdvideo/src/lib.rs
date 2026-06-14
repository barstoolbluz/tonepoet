//! DVD-Video disc reader — ISO 9660 + UDF 1.02 mount + VIDEO_TS directory
//! walk + IFO parser + VOB demuxer.
//!
//! Vendored from oxideav-dvd 0.0.2 (MIT license) and extended with
//! DVD-Video audio stream attribute parsing for LPCM extraction.
//!
//! Original: https://crates.io/crates/oxideav-dvd
//! Clean-room per ECMA-267/268 + OSTA UDF 1.02 + mpucoder + stnsoft references.

#![forbid(unsafe_code)]

pub mod error;
pub mod iso9660;
pub mod udf;
pub mod ifo;
pub mod disc;
pub mod source;
pub mod vob;
pub mod nav;

pub use disc::DvdDisc;
pub use error::{Error, Result};
pub use ifo::{VmgIfo, VtsiMat, VtsIfo, DvdTitle, DvdChapter, Pgc};
pub use vob::DvdSubstream;
