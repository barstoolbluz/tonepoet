// SPDX-License-Identifier: GPL-2.0-or-later
//! File-format validation policies for DSF and DSDIFF readers.
//!
//! The default reader behavior is intentionally interoperable: DSDIFF/DST
//! `DSTC` and `DSTI` are validated when present, but ordinary files that omit
//! them can still be streamed. Application-level validators and acceptance
//! gates often need stricter behavior. These policies make that choice explicit
//! without making the low-level reader guess what a caller wants.

use crate::dsd_file::reader::{DsdFileReader, DsdReadError, DstCrcStatus, DstDsdiffStreamReader, DstFrameReader};
use std::io::{Read, Seek};

/// Policy for DSDIFF/DST container CRC chunks (`DSTC`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DstCrcValidationPolicy {
    /// Do not require CRC chunks. When present, malformed `DSTC` chunks are
    /// still rejected by the reader, and decode paths may still verify them.
    Optional,
    /// Require every `DSTF` frame to have a structurally valid `DSTC` chunk.
    /// This does not force decoding; it proves the container supplied a CRC for
    /// every frame.
    RequirePresent,
    /// Require CRC chunks and require decode-time verification to pass. This
    /// policy is consumed by validation/copy paths that decode DST; plain
    /// encoded-frame readers cannot prove it without decoding.
    RequirePresentAndPassed,
}

impl Default for DstCrcValidationPolicy {
    fn default() -> Self {
        Self::Optional
    }
}

/// Policy for DSDIFF/DST frame indexes (`DSTI`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DsdiffIndexValidationPolicy {
    /// `DSTI` is optional. If present, it must match physical `DSTF` offsets and
    /// sizes exactly.
    ValidateWhenPresent,
    /// Require a valid `DSTI` table for DSDIFF/DST input.
    RequirePresent,
}

impl Default for DsdiffIndexValidationPolicy {
    fn default() -> Self {
        Self::ValidateWhenPresent
    }
}

/// Reader policies that callers can apply consistently across the DSD file
/// module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DsdFileReadPolicies {
    pub dst_crc: DstCrcValidationPolicy,
    pub dsdiff_index: DsdiffIndexValidationPolicy,
}

impl Default for DsdFileReadPolicies {
    fn default() -> Self {
        Self {
            dst_crc: DstCrcValidationPolicy::Optional,
            dsdiff_index: DsdiffIndexValidationPolicy::ValidateWhenPresent,
        }
    }
}

impl DsdFileReadPolicies {
    /// Interoperable reader policy: optional `DSTC`/`DSTI`, strict validation
    /// when either is present.
    pub fn interoperable() -> Self {
        Self::default()
    }

    /// Writer/readback policy for files emitted by `sacd-rs`: every DFF/DST
    /// frame should have both `DSTC` and `DSTI`.
    pub fn require_dsdiff_dst_integrity() -> Self {
        Self {
            dst_crc: DstCrcValidationPolicy::RequirePresent,
            dsdiff_index: DsdiffIndexValidationPolicy::RequirePresent,
        }
    }

    /// Full decode policy used by acceptance/corpus validation. The caller must
    /// decode DST frames and upgrade `PresentUnchecked` statuses to
    /// `PresentPassed`/`PresentFailed`.
    pub fn require_decoded_crc_pass() -> Self {
        Self {
            dst_crc: DstCrcValidationPolicy::RequirePresentAndPassed,
            dsdiff_index: DsdiffIndexValidationPolicy::RequirePresent,
        }
    }

    pub fn with_dst_crc(mut self, policy: DstCrcValidationPolicy) -> Self {
        self.dst_crc = policy;
        self
    }

    pub fn with_dsdiff_index(mut self, policy: DsdiffIndexValidationPolicy) -> Self {
        self.dsdiff_index = policy;
        self
    }

    pub fn validate_opened_reader<R: Read + Seek>(&self, reader: &DsdFileReader<R>) -> Result<(), DsdReadError> {
        if let DsdFileReader::DsdiffDst(dst) = reader {
            self.validate_dsdiff_dst_reader(dst)?;
        }
        Ok(())
    }

    pub fn validate_dsdiff_dst_reader<R: Read + Seek>(&self, reader: &DstDsdiffStreamReader<R>) -> Result<(), DsdReadError> {
        if self.dsdiff_index == DsdiffIndexValidationPolicy::RequirePresent && !reader.has_dsti() {
            return Err(DsdReadError::Malformed {
                offset: reader.info().data_offset,
                reason: "DSDIFF/DST policy requires a valid DSTI index, but none was present".to_string(),
            });
        }
        if matches!(self.dst_crc, DstCrcValidationPolicy::RequirePresent | DstCrcValidationPolicy::RequirePresentAndPassed)
            && reader.frames_without_dstc() != 0
        {
            return Err(DsdReadError::DstCrc {
                offset: reader.info().data_offset,
                frame_index: None,
                status: DstCrcStatus::Malformed {
                    reason: format!(
                        "DSDIFF/DST policy requires DSTC for every frame, but {} frame(s) omit it",
                        reader.frames_without_dstc()
                    ),
                },
            });
        }
        Ok(())
    }

    pub fn validate_decoded_crc_status(&self, status: &DstCrcStatus) -> Result<(), DsdReadError> {
        match (self.dst_crc, status) {
            (DstCrcValidationPolicy::RequirePresentAndPassed, DstCrcStatus::PresentPassed { .. }) => Ok(()),
            (DstCrcValidationPolicy::RequirePresentAndPassed, DstCrcStatus::NoCrc) => Err(DsdReadError::DstCrc {
                offset: 0,
                frame_index: None,
                status: DstCrcStatus::Malformed { reason: "policy requires CRC pass, but no DSTC was present".to_string() },
            }),
            (DstCrcValidationPolicy::RequirePresentAndPassed, DstCrcStatus::PresentUnchecked { .. }) => Err(DsdReadError::DstCrc {
                offset: 0,
                frame_index: None,
                status: DstCrcStatus::Malformed { reason: "policy requires decoded CRC verification, but status is still unchecked".to_string() },
            }),
            (DstCrcValidationPolicy::RequirePresentAndPassed, DstCrcStatus::PresentFailed { .. } | DstCrcStatus::Malformed { .. }) => Err(DsdReadError::DstCrc {
                offset: 0,
                frame_index: None,
                status: status.clone(),
            }),
            (DstCrcValidationPolicy::RequirePresent, DstCrcStatus::NoCrc) => Err(DsdReadError::DstCrc {
                offset: 0,
                frame_index: None,
                status: DstCrcStatus::Malformed { reason: "policy requires DSTC, but no CRC was present".to_string() },
            }),
            (_, DstCrcStatus::Malformed { .. } | DstCrcStatus::PresentFailed { .. }) => Err(DsdReadError::DstCrc {
                offset: 0,
                frame_index: None,
                status: status.clone(),
            }),
            _ => Ok(()),
        }
    }
}
