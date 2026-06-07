// SPDX-License-Identifier: GPL-2.0-or-later
//! Compatibility re-export for the DSD file-format inspector.
//!
//! New code should import these types from [`crate::dsd_file::inspect`] or the
//! higher-level [`crate::dsd_file`] facade. This module remains so existing
//! `sacd-rs` callers do not have to update immediately.

pub use crate::dsd_file::inspect::*;
