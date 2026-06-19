#![forbid(unsafe_code)]

//! Compatibility alias for the historical feature-gated ISO backend name.
//!
//! The production ISO backend is `IsoUdfDvdaVolume`. The old `IsoDvdaVolume`
//! name is retained behind the existing `iso-isomage` feature so older call
//! sites can compile without adding a second ISO implementation.

pub type IsoDvdaVolume = super::iso_udf::IsoUdfDvdaVolume;
