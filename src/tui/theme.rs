//! Tokyo Night color theme for tonepoet TUI

use ratatui::style::{Color, Modifier, Style};

// Base
pub const BG: Color = Color::Rgb(26, 27, 38);
pub const SURFACE: Color = Color::Rgb(36, 40, 59);
pub const BORDER_DIM: Color = Color::Rgb(42, 45, 61);

// Text
pub const TEXT: Color = Color::Rgb(169, 177, 214);
pub const TEXT_BRIGHT: Color = Color::Rgb(192, 202, 245);
pub const TEXT_MUTED: Color = Color::Rgb(86, 95, 137);
pub const TEXT_DIM: Color = Color::Rgb(65, 72, 104);

// Accents
pub const BLUE: Color = Color::Rgb(122, 162, 247);
pub const AMBER: Color = Color::Rgb(224, 175, 104);
pub const GREEN: Color = Color::Rgb(158, 206, 106);
pub const PURPLE: Color = Color::Rgb(187, 154, 247);
pub const CYAN: Color = Color::Rgb(115, 218, 202);
pub const RED: Color = Color::Rgb(247, 118, 142);

// Progress dialog — derived from the base theme so custom themes inherit correctly.
pub const PROGRESS_DIALOG_BG: Color = SURFACE;
pub const PROGRESS_DIALOG_TEXT: Color = TEXT_BRIGHT;
pub const PROGRESS_DIALOG_BORDER: Color = CYAN;
pub const PROGRESS_DIALOG_TITLE: Color = TEXT_BRIGHT;
pub const PROGRESS_DIALOG_LABEL: Color = TEXT_MUTED;
pub const PROGRESS_DIALOG_CURRENT_FILE: Color = CYAN;
pub const PROGRESS_DIALOG_DIM: Color = TEXT_DIM;
pub const PROGRESS_DIALOG_BAR_FILLED: Color = CYAN;
pub const PROGRESS_DIALOG_BAR_UNFILLED: Color = BORDER_DIM;
pub const PROGRESS_DIALOG_PERCENT: Color = TEXT_BRIGHT;
pub const PROGRESS_DIALOG_BUTTON_BG: Color = TEXT;
pub const PROGRESS_DIALOG_BUTTON_FG: Color = BG;
pub const PROGRESS_DIALOG_ABORT_BG: Color = RED;
pub const PROGRESS_DIALOG_ABORT_FG: Color = BG;

// Hover
pub const HOVER_BG: Color = SURFACE; // Subtle background lift on hover

// Pill styles
pub const PILL_ACTIVE_BG: Color = BLUE;
pub const PILL_ACTIVE_FG: Color = BG;
pub const PILL_DIM_BG: Color = TEXT_DIM;
pub const PILL_PRESET_BG: Color = Color::Rgb(31, 94, 79);
pub const PILL_PRESET_FG: Color = CYAN;

// Convenience style constructors
pub fn muted() -> Style {
    Style::default().fg(TEXT_MUTED)
}

pub fn bright() -> Style {
    Style::default().fg(TEXT_BRIGHT)
}

pub fn text() -> Style {
    Style::default().fg(TEXT)
}

pub fn accent() -> Style {
    Style::default().fg(CYAN)
}

pub fn border(color: Color) -> Style {
    Style::default().fg(color)
}

pub fn bold(color: Color) -> Style {
    Style::default().fg(color).add_modifier(Modifier::BOLD)
}
