#![forbid(unsafe_code)]

//! Compatibility re-export for the DVD-Audio parser.
//!
//! The parser implementation lives in the workspace crate `dvda-phase1`.
//! Keep this module as a stable import path for the main crate only; do not add
//! parser logic here.

pub use dvda_phase1::*;
