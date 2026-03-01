//! TUI Audio Conversion Wizard
//! 
//! A complete Terminal User Interface wizard for configuring audio format conversions.
//! Built with ratatui, this provides an interactive multi-step interface with full
//! mouse and keyboard support.

pub mod types;
pub mod ui;
pub mod events;
pub mod presets;

// Re-export main types
pub use types::{
    SimpleWizard, AudioFormat, DitherType, NyquistTransition,
    OpusContentType, AacProfile, ReplayGainMode, ConversionSettings,
    PopupState, PopupType, FileBrowser, BrowserAction, DestinationMode
};

pub use ui::{draw_wizard, MouseAreas, ButtonId};

pub use presets::{ConversionPreset, PresetManager};