//! Pure-Rust SACD ISO audio extraction.
//!
//! Extracts DSD audio streams from ScarletBook-format SACD ISO files
//! and writes them as Sony DSF or Philips DSDIFF (DFF) files. Supports
//! both uncompressed DSD and DST-encoded source frames.
//!
//! ## Scope
//!
//! This crate handles **audio extraction**: reading raw DSD frames
//! from per-track sector ranges, optionally DST-decoding them, and
//! serializing into one of the standard DSD file formats. It does
//! not parse ScarletBook metadata (master TOC, area TOCs, per-track
//! text, ISRCs, etc.) — that lives in tonepoet's `tui::sacd` module.
//! Callers pass pre-parsed metadata in.
//!
//! ## License
//!
//! GPL-2.0-or-later. This crate's DST decoder is a Rust port of the
//! DST decoder in [Sound-Linux-More/sacd-extract][upstream], which is
//! GPL-2.0. Derivative-work licensing requires GPL-2.0-or-later on
//! the port. Compatible with tonepoet's GPL-3.0-or-later top-level
//! license.
//!
//! [upstream]: https://github.com/Sound-Linux-More/sacd-extract
//!
//! ## Roadmap
//!
//! - **PR 1 (this crate set)**: uncompressed-DSD extraction —
//!   frame reader, DSF/DFF writers, orchestration, byte-exact test
//!   against the C reference on a known uncompressed SACD.
//! - **PR 2**: DST decoder port (handed off to a reasoning model
//!   for the dense math, integrated here).
//! - **PR 3**: tonepoet pipeline integration (Convert UI for SACD,
//!   Analyze + DR via sox decimation).

pub mod dff_writer;
pub mod dsf_writer;
pub mod extract;
pub mod frame;
pub mod iso_reader;

#[cfg(test)]
mod test_util;
