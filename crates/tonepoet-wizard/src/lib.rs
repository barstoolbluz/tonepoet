//! TUI Audio Conversion Wizard
//!
//! A complete Terminal User Interface wizard for configuring audio format conversions.
//! Built with ratatui, this provides an interactive multi-step interface with full
//! mouse and keyboard support.

pub mod events;
pub mod presets;
pub mod theme;
pub mod types;
pub mod ui;

// Re-export main types
pub use types::{
    AacProfile, AudioFormat, BrowserAction, ConversionSettings, DestinationMode, DitherType,
    FileBrowser, NyquistTransition, OpusContentType, PopupState, PopupType, ReplayGainMode,
    SimpleWizard,
};

pub use theme::WizardTheme;
pub use ui::{draw_wizard, draw_wizard_with_theme, ButtonId, MouseAreas};

pub use presets::{ConversionPreset, PresetManager};
