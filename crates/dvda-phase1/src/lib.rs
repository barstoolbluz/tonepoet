#![forbid(unsafe_code)]

//! DVD-Audio parser crate used by tonepoet's DVD-Audio materializer.
//!
//! This crate is the single source of truth for DVD-Audio AMG, ATSI, SAMG,
//! AOB inventory, and AUDIO_TS volume parsing. The main crate re-exports this
//! API from `crate::tui::dvda` for compatibility with existing module paths,
//! but parser code must live here.

pub mod tui {
    pub mod dvda;
}

pub use tui::dvda::*;
