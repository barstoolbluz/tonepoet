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
//! ## Status
//!
//! This crate implements local SACD ISO audio extraction for both
//! uncompressed DSD and DST-encoded frames, with DSF/DFF output,
//! structured frame parsing, strict integrity validation, and explicit
//! damaged-ISO salvage reporting. Scarlet Book metadata parsing lives
//! in tonepoet's `tui::sacd` module, which passes parsed area state into
//! this crate's high-integrity extraction API.

pub mod consts;
pub mod dff_footer;
pub mod dff_writer;
pub mod dsf_writer;
pub mod dst;
pub mod extract;
pub mod frame;
pub mod id3;
pub mod iso_reader;

pub use frame::FrameFormat;

#[cfg(test)]
mod test_util;
