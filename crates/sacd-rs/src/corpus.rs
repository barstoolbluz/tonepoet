// SPDX-License-Identifier: GPL-2.0-or-later
//! Compatibility re-export for DSD corpus validation helpers.
//!
//! New code should import these functions from [`crate::dsd_file::corpus`] or
//! the higher-level [`crate::dsd_file`] facade.

pub use crate::dsd_file::corpus::*;
