//! Runtime-selectable TUI theme system.
//!
//! `AppState::theme` is the runtime source of truth. Rendering snapshots that
//! value once per frame and passes it explicitly into render helpers. `Theme` is
//! `Copy` and contains only scalar colors plus static strings, so passing it by
//! value is intentional and cheap.

use ratatui::style::{Color, Modifier, Style};
use once_cell::sync::Lazy;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;


const DEFAULT_THEME_SLUG: &str = "tokyo-night";

/// Theme palettes reserve slots 0-11 for the theme hue wheel and slots 12-15 for semantic pane accents.
pub const THEME_ACCENT_COUNT: usize = 16;
const WARM_ACCENT: usize = 12;
const COOL_ACCENT: usize = 13;
const INFO_ACCENT: usize = 14;
const SUCCESS_ACCENT: usize = 15;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThemePalette {
    pub slug: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub dark: bool,
    pub panel_bg: Color,
    pub border: Color,
    pub title: Color,
    pub tab_active: Color,
    pub tab_inactive: Color,
    pub header: Color,
    pub label: Color,
    pub value: Color,
    pub selection_bg: Color,
    pub chip_go: Color,
    pub chip_dismiss: Color,
    pub accents: [Color; THEME_ACCENT_COUNT],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    pub slug: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub dark: bool,
    // First-class role tokens from ThemePalette / theming brief.
    pub panel_bg: Color,
    pub border: Color,
    pub title: Color,
    pub tab_active: Color,
    pub tab_inactive: Color,
    pub header: Color,
    pub label: Color,
    pub value: Color,
    pub selection_bg: Color,
    pub chip_go: Color,
    pub chip_dismiss: Color,
    pub accents: [Color; THEME_ACCENT_COUNT],
    // Compatibility aliases retained for existing renderer call sites.
    pub bg: Color,
    pub surface: Color,
    pub border_dim: Color,
    pub text: Color,
    pub text_bright: Color,
    pub text_muted: Color,
    pub text_dim: Color,
    pub blue: Color,
    pub amber: Color,
    pub green: Color,
    pub purple: Color,
    pub cyan: Color,
    pub red: Color,
    // Semantic status aliases. Use these in new code instead of raw hue aliases.
    pub error: Color,
    pub destructive: Color,
    pub error_dim: Color,
    pub warning: Color,
    pub success: Color,
    pub info: Color,
    pub dismiss: Color,
    pub progress_dialog_bg: Color,
    pub progress_dialog_text: Color,
    pub progress_dialog_border: Color,
    pub progress_dialog_title: Color,
    pub progress_dialog_label: Color,
    pub progress_dialog_current_file: Color,
    pub progress_dialog_dim: Color,
    pub progress_dialog_bar_filled: Color,
    pub progress_dialog_bar_unfilled: Color,
    pub progress_dialog_percent: Color,
    pub progress_dialog_button_bg: Color,
    pub progress_dialog_button_fg: Color,
    pub progress_dialog_abort_bg: Color,
    pub progress_dialog_abort_fg: Color,
    pub hover_bg: Color,
    pub pill_active_bg: Color,
    pub pill_active_fg: Color,
    pub pill_dim_bg: Color,
    pub pill_preset_bg: Color,
    pub pill_preset_fg: Color,
    pub input_focused_bg: Color,
    pub input_unfocused_bg: Color,
    pub input_disabled_bg: Color,
    pub dropdown_bg: Color,
}

const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::Rgb(r, g, b)
}

fn mix(a: Color, b: Color, numer_b: u16, denom: u16) -> Color {
    let (ar, ag, ab) = rgb_components(a);
    let (br, bg, bb) = rgb_components(b);
    let numer_a = denom.saturating_sub(numer_b);
    Color::Rgb(
        (((ar as u16 * numer_a) + (br as u16 * numer_b)) / denom) as u8,
        (((ag as u16 * numer_a) + (bg as u16 * numer_b)) / denom) as u8,
        (((ab as u16 * numer_a) + (bb as u16 * numer_b)) / denom) as u8,
    )
}

fn rgb_components(color: Color) -> (u8, u8, u8) {
    match color {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Black => (0, 0, 0),
        Color::Red => (205, 49, 49),
        Color::Green => (13, 188, 121),
        Color::Yellow => (229, 229, 16),
        Color::Blue => (36, 114, 200),
        Color::Magenta => (188, 63, 188),
        Color::Cyan => (17, 168, 205),
        Color::Gray => (229, 229, 229),
        Color::DarkGray => (102, 102, 102),
        Color::LightRed => (241, 76, 76),
        Color::LightGreen => (35, 209, 139),
        Color::LightYellow => (245, 245, 67),
        Color::LightBlue => (59, 142, 234),
        Color::LightMagenta => (214, 112, 214),
        Color::LightCyan => (41, 184, 219),
        Color::White => (255, 255, 255),
        _ => (128, 128, 128),
    }
}

impl Theme {
    pub fn from_palette(palette: &ThemePalette) -> Self {
        let surface = mix(palette.panel_bg, palette.border, 1, 3);
        let border_dim = mix(palette.panel_bg, palette.border, 1, 2);
        let text_bright = if palette.dark {
            mix(palette.value, rgb(255, 255, 255), 1, 6)
        } else {
            mix(palette.value, rgb(0, 0, 0), 1, 8)
        };
        let text_dim = mix(palette.label, palette.panel_bg, 1, 2);
        let input_focused_bg = mix(palette.panel_bg, palette.selection_bg, 3, 4);
        let input_unfocused_bg = mix(palette.panel_bg, palette.selection_bg, 1, 2);
        let input_disabled_bg = mix(palette.panel_bg, palette.border, 1, 5);
        let dropdown_bg = mix(palette.panel_bg, palette.border, 1, 4);
        let hover_bg = mix(palette.panel_bg, palette.selection_bg, 2, 3);
        let error_dim = mix(palette.panel_bg, palette.accents[0], 1, 2);
        Self {
            slug: palette.slug,
            name: palette.name,
            description: palette.description,
            dark: palette.dark,
            panel_bg: palette.panel_bg,
            border: palette.border,
            title: palette.title,
            tab_active: palette.tab_active,
            tab_inactive: palette.tab_inactive,
            header: palette.header,
            label: palette.label,
            value: palette.value,
            selection_bg: palette.selection_bg,
            chip_go: palette.chip_go,
            chip_dismiss: palette.chip_dismiss,
            accents: palette.accents,
            bg: palette.panel_bg,
            surface,
            border_dim,
            text: palette.value,
            text_bright,
            text_muted: palette.label,
            text_dim,
            blue: palette.tab_active,
            amber: palette.accents[WARM_ACCENT],
            green: palette.accents[SUCCESS_ACCENT],
            purple: palette.accents[COOL_ACCENT],
            cyan: palette.accents[INFO_ACCENT],
            red: palette.accents[0],
            error: palette.accents[0],
            destructive: palette.accents[0],
            error_dim,
            warning: palette.accents[WARM_ACCENT],
            success: palette.accents[SUCCESS_ACCENT],
            info: palette.accents[INFO_ACCENT],
            dismiss: palette.chip_dismiss,
            progress_dialog_bg: surface,
            progress_dialog_text: text_bright,
            progress_dialog_border: palette.accents[INFO_ACCENT],
            progress_dialog_title: text_bright,
            progress_dialog_label: palette.label,
            progress_dialog_current_file: palette.accents[INFO_ACCENT],
            progress_dialog_dim: text_dim,
            progress_dialog_bar_filled: palette.accents[INFO_ACCENT],
            progress_dialog_bar_unfilled: border_dim,
            progress_dialog_percent: text_bright,
            progress_dialog_button_bg: palette.chip_go,
            progress_dialog_button_fg: palette.panel_bg,
            progress_dialog_abort_bg: palette.chip_dismiss,
            progress_dialog_abort_fg: palette.panel_bg,
            hover_bg,
            pill_active_bg: palette.tab_active,
            pill_active_fg: palette.panel_bg,
            pill_dim_bg: text_dim,
            pill_preset_bg: mix(palette.panel_bg, palette.chip_go, 1, 2),
            pill_preset_fg: palette.chip_go,
            input_focused_bg,
            input_unfocused_bg,
            input_disabled_bg,
            dropdown_bg,
        }
    }

    pub fn muted(self) -> Style {
        Style::default().fg(self.text_muted)
    }

    pub fn bright(self) -> Style {
        Style::default().fg(self.text_bright)
    }

    pub fn text_style(self) -> Style {
        Style::default().fg(self.text)
    }

    pub fn accent(self) -> Style {
        Style::default().fg(self.cyan)
    }

    pub fn error(self) -> Style {
        Style::default().fg(self.error)
    }

    pub fn destructive(self) -> Style {
        Style::default().fg(self.destructive)
    }

    pub fn dismiss(self) -> Style {
        Style::default().fg(self.dismiss)
    }

    pub fn border(self, color: Color) -> Style {
        Style::default().fg(color)
    }

    pub fn bold(self, color: Color) -> Style {
        Style::default().fg(color).add_modifier(Modifier::BOLD)
    }
}

macro_rules! palette {
    ($slug:literal, $name:literal, $desc:literal, $dark:literal,
     $panel:expr, $border:expr, $title:expr, $active:expr, $inactive:expr,
     $header:expr, $label:expr, $value:expr, $selection:expr, $go:expr, $dismiss:expr,
     [$($accent:expr),+ $(,)?]) => {
        ThemePalette {
            slug: $slug,
            name: $name,
            description: $desc,
            dark: $dark,
            panel_bg: $panel,
            border: $border,
            title: $title,
            tab_active: $active,
            tab_inactive: $inactive,
            header: $header,
            label: $label,
            value: $value,
            selection_bg: $selection,
            chip_go: $go,
            chip_dismiss: $dismiss,
            accents: [$($accent),+],
        }
    };
}

pub const PALETTES: &[ThemePalette] = &[
    palette!("tokyo-night", "Tokyo Night", "Balanced dark blue Tokyo Night palette", true, rgb(0x1a,0x1b,0x26), rgb(0x3b,0x42,0x61), rgb(0x7a,0xa2,0xf7), rgb(0x7a,0xa2,0xf7), rgb(0x56,0x5f,0x89), rgb(0xbb,0x9a,0xf7), rgb(0x7f,0x88,0xb3), rgb(0xc0,0xca,0xf5), rgb(0x33,0x46,0x7c), rgb(0x9e,0xce,0x6a), rgb(0xbb,0x9a,0xf7), [rgb(0xf7,0x76,0x8e), rgb(0xff,0x00,0x7c), rgb(0xff,0x9e,0x64), rgb(0xe0,0xaf,0x68), rgb(0x9e,0xce,0x6a), rgb(0x73,0xda,0xca), rgb(0x41,0xa6,0xb5), rgb(0x7d,0xcf,0xff), rgb(0x7a,0xa2,0xf7), rgb(0x3d,0x59,0xa1), rgb(0x9d,0x7c,0xd8), rgb(0xbb,0x9a,0xf7), rgb(0xe0,0xaf,0x68), rgb(0xbb,0x9a,0xf7), rgb(0x73,0xda,0xca), rgb(0x9e,0xce,0x6a)]),
    palette!("gruvbox", "Gruvbox material", "Warm dark Gruvbox material palette", true, rgb(0x28,0x28,0x28), rgb(0x50,0x49,0x45), rgb(0xd8,0xa6,0x57), rgb(0xe7,0x8a,0x4e), rgb(0x7c,0x6f,0x64), rgb(0xd8,0xa6,0x57), rgb(0xa8,0x99,0x84), rgb(0xeb,0xdb,0xb2), rgb(0x66,0x5c,0x54), rgb(0xa9,0xb6,0x65), rgb(0xd3,0x86,0x9b), [rgb(0xea,0x69,0x62), rgb(0xe7,0x8a,0x4e), rgb(0xd8,0xa6,0x57), rgb(0xa9,0xb6,0x65), rgb(0x89,0xb4,0x82), rgb(0x7d,0xae,0xa3), rgb(0xd3,0x86,0x9b), rgb(0xfb,0x49,0x34), rgb(0xfe,0x80,0x19), rgb(0xfa,0xbd,0x2f), rgb(0xb8,0xbb,0x26), rgb(0x83,0xa5,0x98), rgb(0xd8,0xa6,0x57), rgb(0xd3,0x86,0x9b), rgb(0x7d,0xae,0xa3), rgb(0xa9,0xb6,0x65)]),
    palette!("catppuccin", "Catppuccin Mocha", "Soft dark Catppuccin Mocha palette", true, rgb(0x1e,0x1e,0x2e), rgb(0x45,0x47,0x5a), rgb(0xb4,0xbe,0xfe), rgb(0x89,0xb4,0xfa), rgb(0x6c,0x70,0x86), rgb(0xcb,0xa6,0xf7), rgb(0x93,0x99,0xb2), rgb(0xcd,0xd6,0xf4), rgb(0x58,0x5b,0x70), rgb(0xa6,0xe3,0xa1), rgb(0xcb,0xa6,0xf7), [rgb(0xf5,0xe0,0xdc), rgb(0xf5,0xc2,0xe7), rgb(0xcb,0xa6,0xf7), rgb(0xf3,0x8b,0xa8), rgb(0xfa,0xb3,0x87), rgb(0xf9,0xe2,0xaf), rgb(0xa6,0xe3,0xa1), rgb(0x94,0xe2,0xd5), rgb(0x89,0xdc,0xeb), rgb(0x74,0xc7,0xec), rgb(0x89,0xb4,0xfa), rgb(0xb4,0xbe,0xfe), rgb(0xfa,0xb3,0x87), rgb(0xcb,0xa6,0xf7), rgb(0x89,0xdc,0xeb), rgb(0xa6,0xe3,0xa1)]),
    palette!("rose-pine", "Rosé Pine", "Low-contrast dark Rosé Pine palette", true, rgb(0x19,0x17,0x24), rgb(0x40,0x3d,0x52), rgb(0xc4,0xa7,0xe7), rgb(0xc4,0xa7,0xe7), rgb(0x6e,0x6a,0x86), rgb(0xf6,0xc1,0x77), rgb(0x90,0x8c,0xaa), rgb(0xe0,0xde,0xf4), rgb(0x52,0x4f,0x67), rgb(0x9c,0xcf,0xd8), rgb(0xeb,0x6f,0x92), [rgb(0xeb,0x6f,0x92), rgb(0xf6,0xc1,0x77), rgb(0xeb,0xbc,0xba), rgb(0x31,0x74,0x8f), rgb(0x9c,0xcf,0xd8), rgb(0xc4,0xa7,0xe7), rgb(0xe0,0xde,0xf4), rgb(0x90,0x8c,0xaa), rgb(0x6e,0x6a,0x86), rgb(0x40,0x3d,0x52), rgb(0x52,0x4f,0x67), rgb(0x26,0x23,0x3a), rgb(0xf6,0xc1,0x77), rgb(0xc4,0xa7,0xe7), rgb(0x9c,0xcf,0xd8), rgb(0x8f,0xb5,0x73)]),
    palette!("kanagawa", "Kanagawa", "Dark Kanagawa wave palette", true, rgb(0x1f,0x1f,0x28), rgb(0x54,0x54,0x6d), rgb(0xe6,0xc3,0x84), rgb(0x7e,0x9c,0xd8), rgb(0x72,0x71,0x69), rgb(0xd2,0x7e,0x99), rgb(0x72,0x71,0x69), rgb(0xdc,0xd7,0xba), rgb(0x2d,0x4f,0x67), rgb(0x98,0xbb,0x6c), rgb(0xe4,0x68,0x76), [rgb(0xe4,0x68,0x76), rgb(0xff,0x5d,0x62), rgb(0xff,0xa0,0x66), rgb(0xe6,0xc3,0x84), rgb(0xdc,0xa5,0x61), rgb(0x98,0xbb,0x6c), rgb(0x7a,0xa8,0x9f), rgb(0x65,0x85,0x94), rgb(0x7f,0xb4,0xca), rgb(0x7e,0x9c,0xd8), rgb(0x95,0x7f,0xb8), rgb(0xd2,0x7e,0x99), rgb(0xe6,0xc3,0x84), rgb(0x95,0x7f,0xb8), rgb(0x7f,0xb4,0xca), rgb(0x98,0xbb,0x6c)]),
    palette!("everforest", "Everforest", "Muted dark Everforest palette", true, rgb(0x2d,0x35,0x3b), rgb(0x4f,0x5b,0x58), rgb(0x83,0xc0,0x92), rgb(0x7f,0xbb,0xb3), rgb(0x85,0x92,0x89), rgb(0xdb,0xbc,0x7f), rgb(0x85,0x92,0x89), rgb(0xd3,0xc6,0xaa), rgb(0x54,0x3a,0x48), rgb(0xa7,0xc0,0x80), rgb(0xe6,0x7e,0x80), [rgb(0xe6,0x7e,0x80), rgb(0xe6,0x98,0x75), rgb(0xdb,0xbc,0x7f), rgb(0xa7,0xc0,0x80), rgb(0x83,0xc0,0x92), rgb(0x7f,0xbb,0xb3), rgb(0xd6,0x99,0xb6), rgb(0xd3,0xc6,0xaa), rgb(0x85,0x92,0x89), rgb(0x4f,0x5b,0x58), rgb(0x3d,0x48,0x4d), rgb(0x34,0x3f,0x44), rgb(0xdb,0xbc,0x7f), rgb(0xd6,0x99,0xb6), rgb(0x7f,0xbb,0xb3), rgb(0xa7,0xc0,0x80)]),
    palette!("dracula", "Dracula", "Vivid neon purple, pink, cyan on charcoal", true, rgb(0x28,0x2a,0x36), rgb(0x44,0x47,0x5a), rgb(0xbd,0x93,0xf9), rgb(0xbd,0x93,0xf9), rgb(0x62,0x72,0xa4), rgb(0xff,0x79,0xc6), rgb(0x62,0x72,0xa4), rgb(0xf8,0xf8,0xf2), rgb(0x4d,0x4f,0x68), rgb(0x50,0xfa,0x7b), rgb(0xff,0x79,0xc6), [rgb(0xff,0x55,0x55), rgb(0xff,0xb8,0x6c), rgb(0xf1,0xfa,0x8c), rgb(0x50,0xfa,0x7b), rgb(0x8b,0xe9,0xfd), rgb(0xff,0x79,0xc6), rgb(0xbd,0x93,0xf9), rgb(0x62,0x72,0xa4), rgb(0xf8,0xf8,0xf2), rgb(0x44,0x47,0x5a), rgb(0x34,0x37,0x46), rgb(0x28,0x2a,0x36), rgb(0xff,0xb8,0x6c), rgb(0xbd,0x93,0xf9), rgb(0x8b,0xe9,0xfd), rgb(0x50,0xfa,0x7b)]),
    palette!("nord", "Nord", "Arctic minimal desaturated frost and aurora", true, rgb(0x2e,0x34,0x40), rgb(0x3b,0x42,0x52), rgb(0x88,0xc0,0xd0), rgb(0x81,0xa1,0xc1), rgb(0x4c,0x56,0x6a), rgb(0xb4,0x8e,0xad), rgb(0x7a,0x86,0x9c), rgb(0xd8,0xde,0xe9), rgb(0x43,0x4c,0x5e), rgb(0xa3,0xbe,0x8c), rgb(0xb4,0x8e,0xad), [rgb(0xbf,0x61,0x6a), rgb(0xd0,0x87,0x70), rgb(0xeb,0xcb,0x8b), rgb(0xa3,0xbe,0x8c), rgb(0x8f,0xbc,0xbb), rgb(0x88,0xc0,0xd0), rgb(0x81,0xa1,0xc1), rgb(0x5e,0x81,0xac), rgb(0xb4,0x8e,0xad), rgb(0xd8,0xde,0xe9), rgb(0x4c,0x56,0x6a), rgb(0x3b,0x42,0x52), rgb(0xeb,0xcb,0x8b), rgb(0xb4,0x8e,0xad), rgb(0x88,0xc0,0xd0), rgb(0xa3,0xbe,0x8c)]),
    palette!("solarized-dark", "Solarized Dark", "The classic teal base with precision-tuned accents", true, rgb(0x00,0x2b,0x36), rgb(0x23,0x4d,0x56), rgb(0x2a,0xa1,0x98), rgb(0x26,0x8b,0xd2), rgb(0x58,0x6e,0x75), rgb(0x6c,0x71,0xc4), rgb(0x65,0x7b,0x83), rgb(0x93,0xa1,0xa1), rgb(0x17,0x4b,0x55), rgb(0x85,0x99,0x00), rgb(0xd3,0x36,0x82), [rgb(0xb5,0x89,0x00), rgb(0xcb,0x4b,0x16), rgb(0xdc,0x32,0x2f), rgb(0xd3,0x36,0x82), rgb(0x6c,0x71,0xc4), rgb(0x26,0x8b,0xd2), rgb(0x2a,0xa1,0x98), rgb(0x85,0x99,0x00), rgb(0x83,0x94,0x96), rgb(0x93,0xa1,0xa1), rgb(0x58,0x6e,0x75), rgb(0x07,0x36,0x42), rgb(0xb5,0x89,0x00), rgb(0x6c,0x71,0xc4), rgb(0x2a,0xa1,0x98), rgb(0x85,0x99,0x00)]),
    palette!("one-dark", "One Dark", "Atom\u{2019}s balanced blue, green, red, purple", true, rgb(0x28,0x2c,0x34), rgb(0x3e,0x44,0x51), rgb(0x56,0xb6,0xc2), rgb(0x61,0xaf,0xef), rgb(0x5c,0x63,0x70), rgb(0xc6,0x78,0xdd), rgb(0x7f,0x86,0x93), rgb(0xab,0xb2,0xbf), rgb(0x4b,0x52,0x63), rgb(0x98,0xc3,0x79), rgb(0xe0,0x6c,0x75), [rgb(0xe0,0x6c,0x75), rgb(0xd1,0x9a,0x66), rgb(0xe5,0xc0,0x7b), rgb(0x98,0xc3,0x79), rgb(0x56,0xb6,0xc2), rgb(0x61,0xaf,0xef), rgb(0xc6,0x78,0xdd), rgb(0xbe,0x50,0x46), rgb(0xab,0xb2,0xbf), rgb(0x5c,0x63,0x70), rgb(0x3e,0x44,0x51), rgb(0x21,0x25,0x2b), rgb(0xe5,0xc0,0x7b), rgb(0xc6,0x78,0xdd), rgb(0x56,0xb6,0xc2), rgb(0x98,0xc3,0x79)]),
    palette!("monokai-pro", "Monokai Pro", "Warm and vivid hot pink, lime, gold", true, rgb(0x2d,0x2a,0x2e), rgb(0x5b,0x59,0x5c), rgb(0x78,0xdc,0xe8), rgb(0xff,0x61,0x88), rgb(0x72,0x70,0x72), rgb(0xff,0xd8,0x66), rgb(0x93,0x92,0x93), rgb(0xfc,0xfc,0xfa), rgb(0x49,0x47,0x4a), rgb(0xa9,0xdc,0x76), rgb(0xab,0x9d,0xf2), [rgb(0xff,0x61,0x88), rgb(0xfc,0x98,0x67), rgb(0xff,0xd8,0x66), rgb(0xa9,0xdc,0x76), rgb(0x78,0xdc,0xe8), rgb(0xab,0x9d,0xf2), rgb(0xfc,0xfc,0xfa), rgb(0xc1,0xc0,0xc0), rgb(0x93,0x92,0x93), rgb(0x72,0x70,0x72), rgb(0x40,0x3e,0x41), rgb(0x2d,0x2a,0x2e), rgb(0xff,0xd8,0x66), rgb(0xab,0x9d,0xf2), rgb(0x78,0xdc,0xe8), rgb(0xa9,0xdc,0x76)]),
    palette!("oxocarbon", "Oxocarbon", "IBM Carbon near-black OLED with electric accents", true, rgb(0x16,0x16,0x16), rgb(0x39,0x39,0x39), rgb(0xbe,0x95,0xff), rgb(0x33,0xb1,0xff), rgb(0x52,0x52,0x52), rgb(0xff,0x7e,0xb6), rgb(0x8d,0x8d,0x8d), rgb(0xf2,0xf4,0xf8), rgb(0x52,0x52,0x52), rgb(0x42,0xbe,0x65), rgb(0xee,0x53,0x96), [rgb(0x08,0xbd,0xba), rgb(0x3d,0xdb,0xd9), rgb(0x33,0xb1,0xff), rgb(0x78,0xa9,0xff), rgb(0x42,0xbe,0x65), rgb(0xee,0x53,0x96), rgb(0xff,0x7e,0xb6), rgb(0xbe,0x95,0xff), rgb(0x82,0xcf,0xff), rgb(0xf2,0xf4,0xf8), rgb(0x52,0x52,0x52), rgb(0x26,0x26,0x26), rgb(0xf1,0xc2,0x1b), rgb(0xbe,0x95,0xff), rgb(0x33,0xb1,0xff), rgb(0x42,0xbe,0x65)]),
    palette!("tokyo-night-day", "Tokyo Night Day", "Light Tokyo Night palette", false, rgb(0xe1,0xe2,0xe7), rgb(0xc4,0xc8,0xda), rgb(0x2e,0x7d,0xe9), rgb(0x2e,0x7d,0xe9), rgb(0x84,0x8c,0xb5), rgb(0x78,0x47,0xbd), rgb(0x6a,0x72,0xa0), rgb(0x37,0x60,0xbf), rgb(0xc4,0xca,0xe3), rgb(0x58,0x75,0x39), rgb(0xbb,0x1f,0x70), [rgb(0xf5,0x2a,0x65), rgb(0xbb,0x1f,0x70), rgb(0xb1,0x5c,0x00), rgb(0x8c,0x6c,0x3e), rgb(0x58,0x75,0x39), rgb(0x11,0x8c,0x74), rgb(0x38,0x70,0x68), rgb(0x00,0x71,0x97), rgb(0x2e,0x7d,0xe9), rgb(0x2e,0x58,0x57), rgb(0x78,0x47,0xbd), rgb(0x98,0x54,0xf1), rgb(0x8c,0x6c,0x3e), rgb(0x78,0x47,0xbd), rgb(0x00,0x71,0x97), rgb(0x58,0x75,0x39)]),
    palette!("gruvbox-light", "Gruvbox light", "Warm light Gruvbox palette", false, rgb(0xfb,0xf1,0xc7), rgb(0xd5,0xc4,0xa1), rgb(0xb5,0x76,0x14), rgb(0xaf,0x3a,0x03), rgb(0x92,0x83,0x74), rgb(0x8f,0x3f,0x71), rgb(0x7c,0x6f,0x64), rgb(0x3c,0x38,0x36), rgb(0xeb,0xdb,0xb2), rgb(0x79,0x74,0x0e), rgb(0x9d,0x00,0x06), [rgb(0x9d,0x00,0x06), rgb(0xcc,0x24,0x1d), rgb(0xaf,0x3a,0x03), rgb(0xb5,0x76,0x14), rgb(0x79,0x74,0x0e), rgb(0x98,0x97,0x1a), rgb(0x42,0x7b,0x58), rgb(0x68,0x9d,0x6a), rgb(0x07,0x66,0x78), rgb(0x45,0x85,0x88), rgb(0x8f,0x3f,0x71), rgb(0xb1,0x62,0x86), rgb(0xb5,0x76,0x14), rgb(0x8f,0x3f,0x71), rgb(0x07,0x66,0x78), rgb(0x79,0x74,0x0e)]),
    palette!("catppuccin-latte", "Catppuccin Latte", "Soft light Catppuccin Latte palette", false, rgb(0xef,0xf1,0xf5), rgb(0xbc,0xc0,0xcc), rgb(0x72,0x87,0xfd), rgb(0x1e,0x66,0xf5), rgb(0x8c,0x8f,0xa1), rgb(0x88,0x39,0xef), rgb(0x6c,0x6f,0x85), rgb(0x4c,0x4f,0x69), rgb(0xcc,0xd0,0xda), rgb(0x40,0xa0,0x2b), rgb(0x88,0x39,0xef), [rgb(0xdc,0x8a,0x78), rgb(0xea,0x76,0xcb), rgb(0x88,0x39,0xef), rgb(0xd2,0x0f,0x39), rgb(0xfe,0x64,0x0b), rgb(0xdf,0x8e,0x1d), rgb(0x40,0xa0,0x2b), rgb(0x17,0x92,0x99), rgb(0x04,0xa5,0xe5), rgb(0x20,0x9f,0xb5), rgb(0x1e,0x66,0xf5), rgb(0x72,0x87,0xfd), rgb(0xfe,0x64,0x0b), rgb(0x88,0x39,0xef), rgb(0x04,0xa5,0xe5), rgb(0x40,0xa0,0x2b)]),
    palette!("rose-pine-dawn", "Rosé Pine Dawn", "Light Rosé Pine Dawn palette", false, rgb(0xfa,0xf4,0xed), rgb(0xdf,0xda,0xd9), rgb(0x90,0x7a,0xa9), rgb(0x90,0x7a,0xa9), rgb(0x98,0x93,0xa5), rgb(0xea,0x9d,0x34), rgb(0x79,0x75,0x93), rgb(0x57,0x52,0x79), rgb(0xdf,0xda,0xd9), rgb(0x56,0x94,0x9f), rgb(0xb4,0x63,0x7a), [rgb(0xb4,0x63,0x7a), rgb(0xea,0x9d,0x34), rgb(0xd7,0x82,0x7e), rgb(0x28,0x69,0x83), rgb(0x56,0x94,0x9f), rgb(0x90,0x7a,0xa9), rgb(0x57,0x52,0x79), rgb(0x79,0x75,0x93), rgb(0x98,0x93,0xa5), rgb(0xdf,0xda,0xd9), rgb(0xce,0xca,0xcd), rgb(0xf2,0xe9,0xe1), rgb(0xea,0x9d,0x34), rgb(0x90,0x7a,0xa9), rgb(0x28,0x69,0x83), rgb(0x56,0x9f,0x76)]),
    palette!("kanagawa-lotus", "Kanagawa Lotus", "Light Kanagawa Lotus palette", false, rgb(0xf2,0xec,0xbc), rgb(0xd5,0xce,0xa3), rgb(0x83,0x6f,0x4a), rgb(0x4d,0x69,0x9b), rgb(0x8a,0x89,0x80), rgb(0xb3,0x5b,0x79), rgb(0x71,0x6e,0x61), rgb(0x54,0x54,0x64), rgb(0xdc,0xd5,0xac), rgb(0x6f,0x89,0x4e), rgb(0xc8,0x40,0x53), [rgb(0xc8,0x40,0x53), rgb(0xcc,0x6d,0x00), rgb(0x83,0x6f,0x4a), rgb(0x6f,0x89,0x4e), rgb(0x5e,0x85,0x7a), rgb(0x4e,0x8c,0xa2), rgb(0x4d,0x69,0x9b), rgb(0x5d,0x57,0xa3), rgb(0x62,0x4c,0x83), rgb(0x76,0x6b,0x90), rgb(0xb3,0x5b,0x79), rgb(0xe8,0x24,0x24), rgb(0xcc,0x6d,0x00), rgb(0x62,0x4c,0x83), rgb(0x4e,0x8c,0xa2), rgb(0x6f,0x89,0x4e)]),
    palette!("everforest-light", "Everforest light", "Light Everforest palette", false, rgb(0xfd,0xf6,0xe3), rgb(0xe0,0xdc,0xc7), rgb(0x35,0xa7,0x7c), rgb(0x3a,0x94,0xc5), rgb(0x93,0x9f,0x91), rgb(0xdf,0xa0,0x00), rgb(0x82,0x91,0x81), rgb(0x5c,0x6a,0x72), rgb(0xfb,0xe3,0xda), rgb(0x8d,0xa1,0x01), rgb(0xf8,0x55,0x52), [rgb(0xf8,0x55,0x52), rgb(0xf5,0x7d,0x26), rgb(0xdf,0xa0,0x00), rgb(0x8d,0xa1,0x01), rgb(0x35,0xa7,0x7c), rgb(0x3a,0x94,0xc5), rgb(0xdf,0x69,0xba), rgb(0x5c,0x6a,0x72), rgb(0x93,0x9f,0x91), rgb(0xe0,0xdc,0xc7), rgb(0xef,0xeb,0xd4), rgb(0xfd,0xf6,0xe3), rgb(0xdf,0xa0,0x00), rgb(0xdf,0x69,0xba), rgb(0x3a,0x94,0xc5), rgb(0x8d,0xa1,0x01)]),
    palette!("alucard", "Alucard", "Dracula\u{2019}s daylight twin with jewel-tone ink", false, rgb(0xff,0xfb,0xeb), rgb(0xdd,0xd6,0xb8), rgb(0x64,0x4a,0xc9), rgb(0x64,0x4a,0xc9), rgb(0x8a,0x84,0x5f), rgb(0xa3,0x14,0x4d), rgb(0x6c,0x66,0x4b), rgb(0x1f,0x1f,0x1f), rgb(0xdd,0xd6,0xb8), rgb(0x14,0x71,0x0a), rgb(0xa3,0x14,0x4d), [rgb(0xcb,0x3a,0x2a), rgb(0xa3,0x4d,0x14), rgb(0x84,0x6e,0x15), rgb(0x14,0x71,0x0a), rgb(0x03,0x6a,0x96), rgb(0xa3,0x14,0x4d), rgb(0x64,0x4a,0xc9), rgb(0x6c,0x66,0x4b), rgb(0x1f,0x1f,0x1f), rgb(0xcf,0xcf,0xde), rgb(0xf4,0xee,0xd2), rgb(0xff,0xfb,0xeb), rgb(0x84,0x6e,0x15), rgb(0x64,0x4a,0xc9), rgb(0x03,0x6a,0x96), rgb(0x14,0x71,0x0a)]),
    palette!("nord-light", "Nord Light", "Arctic snow with darkened frost accents", false, rgb(0xec,0xef,0xf4), rgb(0xd8,0xde,0xe9), rgb(0x34,0x70,0x8a), rgb(0x4c,0x6f,0x9c), rgb(0x9a,0xa3,0xb3), rgb(0x8a,0x5d,0x85), rgb(0x60,0x70,0x8a), rgb(0x2e,0x34,0x40), rgb(0xd8,0xde,0xe9), rgb(0x5b,0x7a,0x50), rgb(0x8a,0x5d,0x85), [rgb(0xa5,0x4f,0x58), rgb(0xba,0x6a,0x47), rgb(0x94,0x76,0x2f), rgb(0x5b,0x7a,0x50), rgb(0x35,0x7b,0x78), rgb(0x34,0x70,0x8a), rgb(0x4c,0x6f,0x9c), rgb(0x3b,0x5a,0x82), rgb(0x8a,0x5d,0x85), rgb(0x2e,0x34,0x40), rgb(0x6a,0x75,0x85), rgb(0xd8,0xde,0xe9), rgb(0x94,0x76,0x2f), rgb(0x8a,0x5d,0x85), rgb(0x34,0x70,0x8a), rgb(0x5b,0x7a,0x50)]),
    palette!("solarized-light", "Solarized Light", "The official cream base3 paper with classic accents", false, rgb(0xfd,0xf6,0xe3), rgb(0xde,0xd8,0xc0), rgb(0x2a,0xa1,0x98), rgb(0x26,0x8b,0xd2), rgb(0x93,0xa1,0xa1), rgb(0x6c,0x71,0xc4), rgb(0x65,0x7b,0x83), rgb(0x58,0x6e,0x75), rgb(0xde,0xd8,0xc0), rgb(0x85,0x99,0x00), rgb(0xd3,0x36,0x82), [rgb(0xb5,0x89,0x00), rgb(0xcb,0x4b,0x16), rgb(0xdc,0x32,0x2f), rgb(0xd3,0x36,0x82), rgb(0x6c,0x71,0xc4), rgb(0x26,0x8b,0xd2), rgb(0x2a,0xa1,0x98), rgb(0x85,0x99,0x00), rgb(0x65,0x7b,0x83), rgb(0x58,0x6e,0x75), rgb(0x93,0xa1,0xa1), rgb(0xee,0xe8,0xd5), rgb(0xb5,0x89,0x00), rgb(0x6c,0x71,0xc4), rgb(0x26,0x8b,0xd2), rgb(0x85,0x99,0x00)]),
    palette!("one-light", "One Light", "Atom\u{2019}s clean white, even and professional", false, rgb(0xfa,0xfa,0xfa), rgb(0xd3,0xd3,0xd6), rgb(0x01,0x84,0xbc), rgb(0x40,0x78,0xf2), rgb(0xa0,0xa1,0xa7), rgb(0xa6,0x26,0xa4), rgb(0x69,0x6c,0x77), rgb(0x38,0x3a,0x42), rgb(0xd3,0xd3,0xd6), rgb(0x50,0xa1,0x4f), rgb(0xe4,0x56,0x49), [rgb(0xe4,0x56,0x49), rgb(0xca,0x12,0x43), rgb(0xc1,0x84,0x01), rgb(0x98,0x68,0x01), rgb(0x50,0xa1,0x4f), rgb(0x01,0x84,0xbc), rgb(0x40,0x78,0xf2), rgb(0xa6,0x26,0xa4), rgb(0x38,0x3a,0x42), rgb(0x69,0x6c,0x77), rgb(0xa0,0xa1,0xa7), rgb(0xea,0xea,0xeb), rgb(0xc1,0x84,0x01), rgb(0xa6,0x26,0xa4), rgb(0x01,0x84,0xbc), rgb(0x50,0xa1,0x4f)]),
    palette!("monokai-pro-light", "Monokai Pro Light", "Warm cream with deepened pink, lime, gold", false, rgb(0xfa,0xf4,0xec), rgb(0xe3,0xdc,0xd0), rgb(0x2f,0x8a,0x9c), rgb(0xd4,0x27,0x5a), rgb(0x9a,0x94,0x8c), rgb(0xa0,0x7b,0x16), rgb(0x6f,0x6a,0x66), rgb(0x2d,0x2a,0x2e), rgb(0xe3,0xdc,0xd0), rgb(0x6a,0x9c,0x2f), rgb(0x6d,0x57,0xc9), [rgb(0xd4,0x27,0x5a), rgb(0xc2,0x62,0x2a), rgb(0xa0,0x7b,0x16), rgb(0x6a,0x9c,0x2f), rgb(0x2f,0x8a,0x9c), rgb(0x6d,0x57,0xc9), rgb(0x2d,0x2a,0x2e), rgb(0x6f,0x6a,0x66), rgb(0x9a,0x94,0x8c), rgb(0xe3,0xdc,0xd0), rgb(0xef,0xe7,0xda), rgb(0xfa,0xf4,0xec), rgb(0xa0,0x7b,0x16), rgb(0x6d,0x57,0xc9), rgb(0x2f,0x8a,0x9c), rgb(0x6a,0x9c,0x2f)]),
    palette!("oxocarbon-light", "Oxocarbon Light", "IBM Carbon crisp gray-white with accessible accents", false, rgb(0xf2,0xf4,0xf8), rgb(0xdd,0xe1,0xe6), rgb(0x8a,0x3f,0xfc), rgb(0x0f,0x62,0xfe), rgb(0xa8,0xa8,0xa8), rgb(0xd1,0x27,0x71), rgb(0x52,0x52,0x52), rgb(0x16,0x16,0x16), rgb(0xdd,0xe1,0xe6), rgb(0x19,0x80,0x38), rgb(0xd1,0x27,0x71), [rgb(0xda,0x1e,0x28), rgb(0xba,0x4e,0x00), rgb(0xb2,0x86,0x00), rgb(0x19,0x80,0x38), rgb(0x00,0x7d,0x79), rgb(0x11,0x92,0xe8), rgb(0x0f,0x62,0xfe), rgb(0x8a,0x3f,0xfc), rgb(0xd1,0x27,0x71), rgb(0xee,0x53,0x96), rgb(0x16,0x16,0x16), rgb(0x52,0x52,0x52), rgb(0xb2,0x86,0x00), rgb(0x8a,0x3f,0xfc), rgb(0x0f,0x62,0xfe), rgb(0x19,0x80,0x38)]),
];


pub fn palettes() -> &'static [ThemePalette] {
    PALETTES
}

pub fn palette_by_slug(slug: &str) -> Option<&'static ThemePalette> {
    let normalized = validate_theme_slug(slug).ok()?;
    PALETTES.iter().find(|palette| palette.slug == normalized.as_str())
}

pub fn is_builtin_theme_slug(slug: &str) -> bool {
    palette_by_slug(slug).is_some()
}

pub fn theme_by_slug(slug: &str) -> Option<Theme> {
    palette_by_slug(slug).map(Theme::from_palette)
}

pub fn theme_by_slug_or_default(slug: &str) -> Theme {
    theme_by_slug(slug).unwrap_or_else(|| Theme::from_palette(default_palette()))
}


pub fn default_theme_slug() -> &'static str {
    DEFAULT_THEME_SLUG
}

pub fn default_theme_name() -> String {
    DEFAULT_THEME_SLUG.to_string()
}

pub fn default_palette() -> &'static ThemePalette {
    palette_by_slug(DEFAULT_THEME_SLUG).expect("default theme must exist")
}

const THEME_VARIANT_PAIRS: &[(&str, &str)] = &[
    ("tokyo-night", "tokyo-night-day"),
    ("gruvbox", "gruvbox-light"),
    ("catppuccin", "catppuccin-latte"),
    ("rose-pine", "rose-pine-dawn"),
    ("kanagawa", "kanagawa-lotus"),
    ("everforest", "everforest-light"),
    ("dracula", "alucard"),
    ("nord", "nord-light"),
    ("solarized-dark", "solarized-light"),
    ("one-dark", "one-light"),
    ("monokai-pro", "monokai-pro-light"),
    ("oxocarbon", "oxocarbon-light"),
];

pub fn paired_theme_slug(current: &str) -> Option<String> {
    let choices = theme_choices();
    paired_theme_slug_in_choices(current, &choices)
}

pub fn paired_theme_slug_in_choices(current: &str, choices: &[ThemeChoice]) -> Option<String> {
    let normalized = validate_theme_slug(current).ok()?;
    for (dark, light) in THEME_VARIANT_PAIRS {
        if normalized == *dark {
            return Some((*light).to_string());
        }
        if normalized == *light {
            return Some((*dark).to_string());
        }
    }

    // Custom themes may still follow the built-in naming convention. Only
    // return a convention-derived pair when that pair is actually present in
    // the caller's cached library snapshot, so pressing `m` never persists a
    // non-existent slug and render paths do not need to touch the filesystem.
    const LIGHT_SUFFIXES: &[&str] = &["-light", "-day", "-dawn", "-lotus", "-latte"];
    let exists = |slug: &str| choices.iter().any(|choice| choice.slug == slug);
    for suffix in LIGHT_SUFFIXES {
        if let Some(base) = normalized.strip_suffix(suffix) {
            if !base.is_empty() && exists(base) {
                return Some(base.to_string());
            }
        }
    }
    for suffix in LIGHT_SUFFIXES {
        let candidate = format!("{normalized}{suffix}");
        if exists(&candidate) {
            return Some(candidate);
        }
    }
    None
}

pub fn next_palette_slug(current: &str) -> &'static str {
    adjacent_palette_slug(current, 1)
}

pub fn previous_palette_slug(current: &str) -> &'static str {
    adjacent_palette_slug(current, PALETTES.len().saturating_sub(1))
}

fn adjacent_palette_slug(current: &str, offset: usize) -> &'static str {
    let idx = validate_theme_slug(current)
        .ok()
        .and_then(|normalized| {
            PALETTES
                .iter()
                .position(|palette| palette.slug == normalized.as_str())
        })
        .unwrap_or(0);
    PALETTES[(idx + offset) % PALETTES.len()].slug
}

pub fn validate_theme_slug(slug: &str) -> anyhow::Result<String> {
    if slug.is_empty() {
        anyhow::bail!("theme slug cannot be empty");
    }
    if slug != slug.trim() || slug.chars().any(char::is_whitespace) {
        anyhow::bail!("theme slug cannot contain whitespace");
    }
    if slug.contains('/') || slug.contains('\\') {
        anyhow::bail!("theme slug cannot contain path separators");
    }
    if slug.contains("..") {
        anyhow::bail!("theme slug cannot contain '..'");
    }

    let normalized = slug.to_ascii_lowercase().replace('_', "-");
    if normalized.is_empty() {
        anyhow::bail!("theme slug cannot be empty");
    }
    if normalized.len() > 80 {
        anyhow::bail!("theme slug cannot exceed 80 bytes");
    }
    if normalized.starts_with('-') || normalized.ends_with('-') || normalized.contains("--") {
        anyhow::bail!("theme slug cannot start, end, or repeat '-' separators");
    }
    if !normalized.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-') {
        anyhow::bail!("theme slug may contain only ASCII letters, digits, '-' or '_'");
    }
    Ok(normalized)
}

fn normalize_slug(slug: &str) -> String {
    validate_theme_slug(slug).unwrap_or_else(|_| DEFAULT_THEME_SLUG.to_string())
}


// -- Custom theme files, derived-element metadata, and resolution -----------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorDepth {
    Ansi8,
    Ansi16,
    Xterm256,
    TrueColor,
}

impl ColorDepth {
    pub const ALL: [Self; 4] = [Self::Ansi8, Self::Ansi16, Self::Xterm256, Self::TrueColor];

    pub fn label(self) -> &'static str {
        match self {
            Self::Ansi8 => "8",
            Self::Ansi16 => "16",
            Self::Xterm256 => "256",
            Self::TrueColor => "True Color",
        }
    }

    pub fn next(self) -> Self {
        let idx = Self::ALL.iter().position(|depth| *depth == self).unwrap_or(0);
        Self::ALL[(idx + 1) % Self::ALL.len()]
    }

    pub fn previous(self) -> Self {
        let idx = Self::ALL.iter().position(|depth| *depth == self).unwrap_or(0);
        Self::ALL[(idx + Self::ALL.len() - 1) % Self::ALL.len()]
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedSwatch {
    pub name: String,
    pub color: Color,
}

impl NamedSwatch {
    pub fn new(name: impl Into<String>, color: Color) -> Self {
        Self { name: name.into(), color }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemePaletteDraft {
    pub slug: String,
    pub name: String,
    pub description: String,
    pub dark: bool,
    pub panel_bg: Color,
    pub border: Color,
    pub title: Color,
    pub tab_active: Color,
    pub tab_inactive: Color,
    pub header: Color,
    pub label: Color,
    pub value: Color,
    pub selection_bg: Color,
    pub chip_go: Color,
    pub chip_dismiss: Color,
    pub accents: [Color; THEME_ACCENT_COUNT],
    pub swatches: Vec<NamedSwatch>,
    /// Symbolic swatch bindings for first-class palette slots. Keys use
    /// stable TOML-path names such as `roles.title` and `accents.warm`.
    /// A bound slot follows the named swatch until the slot is edited
    /// directly, which clears the binding.
    pub slot_bindings: BTreeMap<String, String>,
    pub derived_locks: BTreeMap<String, Color>,
    pub source: ThemeDraftSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeDraftSource {
    BuiltIn,
    Custom,
    NewCustom,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeChoice {
    pub slug: String,
    pub name: String,
    pub description: String,
    pub dark: bool,
    pub built_in: bool,
    pub author_lock_count: usize,
    /// First-class preview colors for renderers. Carrying these in the library
    /// entry keeps draw paths from re-reading and reparsing theme files just to
    /// paint a palette ribbon.
    pub accents: [Color; THEME_ACCENT_COUNT],
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ThemeLibrarySnapshot {
    pub choices: Vec<ThemeChoice>,
}

impl ThemeLibrarySnapshot {
    pub fn load() -> Self {
        Self { choices: theme_choices() }
    }

    pub fn choices(&self) -> &[ThemeChoice] {
        &self.choices
    }

    pub fn is_empty(&self) -> bool {
        self.choices.is_empty()
    }

    pub fn len(&self) -> usize {
        self.choices.len()
    }

    pub fn built_in_count(&self) -> usize {
        self.choices.iter().filter(|choice| choice.built_in).count()
    }

    pub fn custom_count(&self) -> usize {
        self.choices.len().saturating_sub(self.built_in_count())
    }

    pub fn active_index(&self, configured_slug: &str, runtime_slug: &str) -> usize {
        let configured = normalize_slug(configured_slug);
        let runtime = normalize_slug(runtime_slug);
        self.choices
            .iter()
            .position(|choice| choice.slug == configured || choice.slug == runtime)
            .unwrap_or(0)
    }

    pub fn next_slug(&self, current: &str, forward: bool) -> String {
        next_theme_slug_in_choices(current, forward, &self.choices)
    }

    pub fn paired_slug(&self, current: &str) -> Option<String> {
        paired_theme_slug_in_choices(current, &self.choices)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThemeApplyOptions {
    pub honor_theme_locks: bool,
    pub keep_user_overrides: bool,
}

impl Default for ThemeApplyOptions {
    fn default() -> Self {
        Self { honor_theme_locks: true, keep_user_overrides: true }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ThemeResolutionTally {
    pub by_theme: usize,
    pub by_user: usize,
    pub auto: usize,
}

pub fn theme_resolution_tally(
    draft: &ThemePaletteDraft,
    options: ThemeApplyOptions,
    user_overrides: &ThemeOverrides,
) -> ThemeResolutionTally {
    let valid_keys = derived_element_specs()
        .iter()
        .map(|spec| spec.key)
        .collect::<BTreeSet<_>>();
    let user_keys = if options.keep_user_overrides {
        user_overrides.overrides
            .keys()
            .filter(|key| valid_keys.contains(key.as_str()))
            .cloned()
            .collect::<BTreeSet<_>>()
    } else {
        BTreeSet::new()
    };
    let theme_keys = if options.honor_theme_locks {
        draft.derived_locks
            .keys()
            .filter(|key| valid_keys.contains(key.as_str()) && !user_keys.contains(*key))
            .cloned()
            .collect::<BTreeSet<_>>()
    } else {
        BTreeSet::new()
    };
    let by_user = user_keys.len();
    let by_theme = theme_keys.len();
    ThemeResolutionTally {
        by_theme,
        by_user,
        auto: valid_keys.len().saturating_sub(by_theme + by_user),
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ThemeOverrides {
    pub overrides: BTreeMap<String, Color>,
}

impl ThemeOverrides {
    pub fn is_empty(&self) -> bool { self.overrides.is_empty() }
    pub fn len(&self) -> usize { self.overrides.len() }

    pub fn load_default() -> anyhow::Result<Self> {
        Self::load_from_path(&theme_overrides_path())
    }

    pub fn save_default(&self) -> anyhow::Result<()> {
        self.save_to_path(&theme_overrides_path())
    }

    pub fn load_from_path(path: &Path) -> anyhow::Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(path)?;
        let file: ThemeOverridesFile = toml::from_str(&content)?;
        let mut overrides = BTreeMap::new();
        for (key, value) in file.overrides {
            if derived_element_spec(&key).is_some() {
                overrides.insert(key, parse_color_token(&value, &BTreeMap::new())?);
            }
        }
        Ok(Self { overrides })
    }

    pub fn save_to_path(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = ThemeOverridesFile {
            overrides: self.overrides.iter()
                .map(|(key, color)| (key.clone(), color_to_hex(*color)))
                .collect(),
        };
        let encoded = toml::to_string_pretty(&file)?;
        atomic_write(path, encoded.as_bytes())?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct DerivedElementSpec {
    pub key: &'static str,
    pub label: &'static str,
    pub formula: &'static str,
    pub used_by: &'static str,
    pub group: &'static str,
}

pub const ROLE_KEYS: [&str; 11] = [
    "panel_bg", "border", "title", "tab_active", "tab_inactive", "header", "label", "value",
    "selection_bg", "chip_go", "chip_dismiss",
];

pub const ROLE_LABELS: [&str; 11] = [
    "panel bg", "border", "title", "tab active", "tab inactive", "header", "label", "value",
    "selection bg", "chip go", "chip dismiss",
];

pub const ACCENT_LABELS: [&str; THEME_ACCENT_COUNT] = [
    "hue 0", "hue 1", "hue 2", "hue 3", "hue 4", "hue 5", "hue 6", "hue 7",
    "hue 8", "hue 9", "hue 10", "hue 11", "warm", "cool", "info", "success",
];

const DERIVED_SPECS: &[DerivedElementSpec] = &[
    DerivedElementSpec { key: "surface", label: "surface", formula: "mix(panel_bg, border, 1:3)", used_by: "secondary panes and elevated surfaces", group: "surfaces" },
    DerivedElementSpec { key: "border_dim", label: "border dim", formula: "mix(panel_bg, border, 1:2)", used_by: "subtle borders and inactive dividers", group: "surfaces" },
    DerivedElementSpec { key: "text_bright", label: "text bright", formula: "value mixed toward terminal white/black by mode", used_by: "titles, emphasized text, progress percent", group: "text" },
    DerivedElementSpec { key: "text_dim", label: "text dim", formula: "mix(label, panel_bg, 1:2)", used_by: "help text, disabled copy, low-emphasis annotations", group: "text" },
    DerivedElementSpec { key: "hover_bg", label: "hover bg", formula: "mix(panel_bg, selection_bg, 2:3)", used_by: "hovered rows and mouse targets", group: "interaction" },
    DerivedElementSpec { key: "input_focused_bg", label: "input focused bg", formula: "mix(panel_bg, selection_bg, 3:4)", used_by: "focused text inputs", group: "interaction" },
    DerivedElementSpec { key: "input_unfocused_bg", label: "input unfocused bg", formula: "mix(panel_bg, selection_bg, 1:2)", used_by: "inactive text inputs", group: "interaction" },
    DerivedElementSpec { key: "input_disabled_bg", label: "input disabled bg", formula: "mix(panel_bg, border, 1:5)", used_by: "disabled text inputs", group: "interaction" },
    DerivedElementSpec { key: "dropdown_bg", label: "dropdown bg", formula: "mix(panel_bg, border, 1:4)", used_by: "dropdowns, menus, overlays", group: "interaction" },
    DerivedElementSpec { key: "pill_active_bg", label: "pill active bg", formula: "tab_active", used_by: "active footer/navigation pills", group: "pills" },
    DerivedElementSpec { key: "pill_active_fg", label: "pill active fg", formula: "panel_bg", used_by: "text inside active pills", group: "pills" },
    DerivedElementSpec { key: "pill_dim_bg", label: "pill dim bg", formula: "text_dim", used_by: "dimmed footer pills", group: "pills" },
    DerivedElementSpec { key: "pill_preset_bg", label: "pill preset bg", formula: "mix(panel_bg, chip_go, 1:2)", used_by: "preset-selection pills", group: "pills" },
    DerivedElementSpec { key: "pill_preset_fg", label: "pill preset fg", formula: "chip_go", used_by: "preset-selection pill text", group: "pills" },
    DerivedElementSpec { key: "progress_dialog_bg", label: "progress dialog bg", formula: "surface", used_by: "file task progress overlay", group: "progress dialog" },
    DerivedElementSpec { key: "progress_dialog_border", label: "progress dialog border", formula: "info accent", used_by: "file task progress overlay border", group: "progress dialog" },
    DerivedElementSpec { key: "progress_dialog_text", label: "progress dialog text", formula: "text_bright", used_by: "progress overlay body text", group: "progress dialog" },
    DerivedElementSpec { key: "progress_dialog_title", label: "progress dialog title", formula: "text_bright", used_by: "progress overlay title", group: "progress dialog" },
    DerivedElementSpec { key: "progress_dialog_label", label: "progress dialog label", formula: "label", used_by: "progress overlay labels", group: "progress dialog" },
    DerivedElementSpec { key: "progress_dialog_current_file", label: "progress current file", formula: "info accent", used_by: "current-file line in progress overlay", group: "progress dialog" },
    DerivedElementSpec { key: "progress_dialog_dim", label: "progress dialog dim", formula: "text_dim", used_by: "secondary progress text", group: "progress dialog" },
    DerivedElementSpec { key: "progress_dialog_bar_filled", label: "progress bar filled", formula: "info accent", used_by: "filled progress bar", group: "progress dialog" },
    DerivedElementSpec { key: "progress_dialog_bar_unfilled", label: "progress bar unfilled", formula: "border_dim", used_by: "unfilled progress bar", group: "progress dialog" },
    DerivedElementSpec { key: "progress_dialog_percent", label: "progress percent", formula: "text_bright", used_by: "progress percentage", group: "progress dialog" },
    DerivedElementSpec { key: "progress_dialog_button_bg", label: "progress button bg", formula: "chip_go", used_by: "confirm/continue buttons", group: "progress dialog" },
    DerivedElementSpec { key: "progress_dialog_button_fg", label: "progress button fg", formula: "panel_bg", used_by: "confirm/continue button text", group: "progress dialog" },
    DerivedElementSpec { key: "progress_dialog_abort_bg", label: "progress abort bg", formula: "chip_dismiss", used_by: "abort/cancel buttons", group: "progress dialog" },
    DerivedElementSpec { key: "progress_dialog_abort_fg", label: "progress abort fg", formula: "panel_bg", used_by: "abort/cancel button text", group: "progress dialog" },
    DerivedElementSpec { key: "error_dim", label: "error dim", formula: "mix(panel_bg, error, 1:2)", used_by: "low-emphasis destructive text", group: "states" },
];

pub fn derived_element_specs() -> &'static [DerivedElementSpec] { DERIVED_SPECS }

pub fn derived_element_spec(key: &str) -> Option<&'static DerivedElementSpec> {
    DERIVED_SPECS.iter().find(|spec| spec.key == key)
}

fn config_base_dir() -> PathBuf {
    #[cfg(test)]
    if let Some(path) = crate::tui::test_support::test_config_home_override() {
        return path;
    }

    dirs::config_dir().unwrap_or_else(|| PathBuf::from("."))
}

fn tonepoet_config_dir() -> PathBuf {
    config_base_dir().join("tonepoet")
}

pub fn theme_dir() -> PathBuf {
    tonepoet_config_dir().join("themes")
}

pub fn swatches_path() -> PathBuf {
    tonepoet_config_dir().join("swatches.toml")
}

pub fn theme_overrides_path() -> PathBuf {
    tonepoet_config_dir().join("theme_overrides.toml")
}

pub fn color_to_hex(color: Color) -> String {
    let (r, g, b) = rgb_components(color);
    format!("#{r:02X}{g:02X}{b:02X}")
}

pub fn parse_hex_color(input: &str) -> anyhow::Result<Color> {
    let trimmed = input.trim();
    let hex = trimmed.strip_prefix('#').unwrap_or(trimmed);
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        anyhow::bail!("expected #RRGGBB color, got '{input}'");
    }
    let r = u8::from_str_radix(&hex[0..2], 16)?;
    let g = u8::from_str_radix(&hex[2..4], 16)?;
    let b = u8::from_str_radix(&hex[4..6], 16)?;
    Ok(Color::Rgb(r, g, b))
}

pub fn rgb_tuple(color: Color) -> (u8, u8, u8) { rgb_components(color) }

pub fn color_from_rgb_tuple(rgb: (u8, u8, u8)) -> Color { Color::Rgb(rgb.0, rgb.1, rgb.2) }

pub fn mix_colors(a: Color, b: Color, numer_b: u16, denom: u16) -> Color { mix(a, b, numer_b, denom) }

pub fn nearest_xterm_256(color: Color) -> (u8, Color) {
    let target = rgb_components(color);
    let mut best_index = 0u8;
    let mut best_distance = u32::MAX;
    let mut consider = |index: u8, rgb: (u8, u8, u8)| {
        let distance = squared_rgb_distance(target, rgb);
        if distance < best_distance {
            best_index = index;
            best_distance = distance;
        }
    };

    for (index, rgb) in ANSI_16_RGB.iter().copied().enumerate() {
        consider(index as u8, rgb);
    }

    const CUBE: [u8; 6] = [0, 95, 135, 175, 215, 255];
    for (r_index, r) in CUBE.iter().copied().enumerate() {
        for (g_index, g) in CUBE.iter().copied().enumerate() {
            for (b_index, b) in CUBE.iter().copied().enumerate() {
                let index = 16 + (36 * r_index) + (6 * g_index) + b_index;
                consider(index as u8, (r, g, b));
            }
        }
    }

    for gray_index in 0..24 {
        let value = 8 + (gray_index * 10);
        consider((232 + gray_index) as u8, (value, value, value));
    }

    (best_index, xterm_256_rgb(best_index))
}

pub fn nearest_color_for_depth(color: Color, depth: ColorDepth) -> (u16, Color) {
    match depth {
        ColorDepth::TrueColor => (0, color),
        ColorDepth::Xterm256 => {
            let (index, color) = nearest_xterm_256(color);
            (u16::from(index), color)
        }
        ColorDepth::Ansi16 => nearest_ansi_color(color, 16),
        ColorDepth::Ansi8 => nearest_ansi_color(color, 8),
    }
}

pub fn quantize_color_for_depth(color: Color, depth: ColorDepth) -> Color {
    nearest_color_for_depth(color, depth).1
}

fn nearest_ansi_color(color: Color, count: usize) -> (u16, Color) {
    let target = rgb_components(color);
    let mut best_index = 0usize;
    let mut best_distance = u32::MAX;
    for (index, rgb) in ANSI_16_RGB.iter().copied().take(count.min(ANSI_16_RGB.len())).enumerate() {
        let distance = squared_rgb_distance(target, rgb);
        if distance < best_distance {
            best_index = index;
            best_distance = distance;
        }
    }
    let (r, g, b) = ANSI_16_RGB[best_index];
    (best_index as u16, Color::Rgb(r, g, b))
}

fn quantize_theme_for_depth(theme: &mut Theme, depth: ColorDepth) {
    if depth == ColorDepth::TrueColor {
        return;
    }
    theme.panel_bg = quantize_color_for_depth(theme.panel_bg, depth);
    theme.border = quantize_color_for_depth(theme.border, depth);
    theme.title = quantize_color_for_depth(theme.title, depth);
    theme.tab_active = quantize_color_for_depth(theme.tab_active, depth);
    theme.tab_inactive = quantize_color_for_depth(theme.tab_inactive, depth);
    theme.header = quantize_color_for_depth(theme.header, depth);
    theme.label = quantize_color_for_depth(theme.label, depth);
    theme.value = quantize_color_for_depth(theme.value, depth);
    theme.selection_bg = quantize_color_for_depth(theme.selection_bg, depth);
    theme.chip_go = quantize_color_for_depth(theme.chip_go, depth);
    theme.chip_dismiss = quantize_color_for_depth(theme.chip_dismiss, depth);
    for color in &mut theme.accents {
        *color = quantize_color_for_depth(*color, depth);
    }
    theme.bg = quantize_color_for_depth(theme.bg, depth);
    theme.surface = quantize_color_for_depth(theme.surface, depth);
    theme.border_dim = quantize_color_for_depth(theme.border_dim, depth);
    theme.text = quantize_color_for_depth(theme.text, depth);
    theme.text_bright = quantize_color_for_depth(theme.text_bright, depth);
    theme.text_muted = quantize_color_for_depth(theme.text_muted, depth);
    theme.text_dim = quantize_color_for_depth(theme.text_dim, depth);
    theme.blue = quantize_color_for_depth(theme.blue, depth);
    theme.amber = quantize_color_for_depth(theme.amber, depth);
    theme.green = quantize_color_for_depth(theme.green, depth);
    theme.purple = quantize_color_for_depth(theme.purple, depth);
    theme.cyan = quantize_color_for_depth(theme.cyan, depth);
    theme.red = quantize_color_for_depth(theme.red, depth);
    theme.error = quantize_color_for_depth(theme.error, depth);
    theme.destructive = quantize_color_for_depth(theme.destructive, depth);
    theme.error_dim = quantize_color_for_depth(theme.error_dim, depth);
    theme.warning = quantize_color_for_depth(theme.warning, depth);
    theme.success = quantize_color_for_depth(theme.success, depth);
    theme.info = quantize_color_for_depth(theme.info, depth);
    theme.dismiss = quantize_color_for_depth(theme.dismiss, depth);
    theme.progress_dialog_bg = quantize_color_for_depth(theme.progress_dialog_bg, depth);
    theme.progress_dialog_text = quantize_color_for_depth(theme.progress_dialog_text, depth);
    theme.progress_dialog_border = quantize_color_for_depth(theme.progress_dialog_border, depth);
    theme.progress_dialog_title = quantize_color_for_depth(theme.progress_dialog_title, depth);
    theme.progress_dialog_label = quantize_color_for_depth(theme.progress_dialog_label, depth);
    theme.progress_dialog_current_file = quantize_color_for_depth(theme.progress_dialog_current_file, depth);
    theme.progress_dialog_dim = quantize_color_for_depth(theme.progress_dialog_dim, depth);
    theme.progress_dialog_bar_filled = quantize_color_for_depth(theme.progress_dialog_bar_filled, depth);
    theme.progress_dialog_bar_unfilled = quantize_color_for_depth(theme.progress_dialog_bar_unfilled, depth);
    theme.progress_dialog_percent = quantize_color_for_depth(theme.progress_dialog_percent, depth);
    theme.progress_dialog_button_bg = quantize_color_for_depth(theme.progress_dialog_button_bg, depth);
    theme.progress_dialog_button_fg = quantize_color_for_depth(theme.progress_dialog_button_fg, depth);
    theme.progress_dialog_abort_bg = quantize_color_for_depth(theme.progress_dialog_abort_bg, depth);
    theme.progress_dialog_abort_fg = quantize_color_for_depth(theme.progress_dialog_abort_fg, depth);
    theme.hover_bg = quantize_color_for_depth(theme.hover_bg, depth);
    theme.pill_active_bg = quantize_color_for_depth(theme.pill_active_bg, depth);
    theme.pill_active_fg = quantize_color_for_depth(theme.pill_active_fg, depth);
    theme.pill_dim_bg = quantize_color_for_depth(theme.pill_dim_bg, depth);
    theme.pill_preset_bg = quantize_color_for_depth(theme.pill_preset_bg, depth);
    theme.pill_preset_fg = quantize_color_for_depth(theme.pill_preset_fg, depth);
    theme.input_focused_bg = quantize_color_for_depth(theme.input_focused_bg, depth);
    theme.input_unfocused_bg = quantize_color_for_depth(theme.input_unfocused_bg, depth);
    theme.input_disabled_bg = quantize_color_for_depth(theme.input_disabled_bg, depth);
    theme.dropdown_bg = quantize_color_for_depth(theme.dropdown_bg, depth);
}

fn quantize_draft_for_depth(draft: &ThemePaletteDraft, depth: ColorDepth) -> ThemePaletteDraft {
    let mut quantized = draft.with_resolved_bindings();
    if depth == ColorDepth::TrueColor {
        return quantized;
    }
    quantized.panel_bg = quantize_color_for_depth(quantized.panel_bg, depth);
    quantized.border = quantize_color_for_depth(quantized.border, depth);
    quantized.title = quantize_color_for_depth(quantized.title, depth);
    quantized.tab_active = quantize_color_for_depth(quantized.tab_active, depth);
    quantized.tab_inactive = quantize_color_for_depth(quantized.tab_inactive, depth);
    quantized.header = quantize_color_for_depth(quantized.header, depth);
    quantized.label = quantize_color_for_depth(quantized.label, depth);
    quantized.value = quantize_color_for_depth(quantized.value, depth);
    quantized.selection_bg = quantize_color_for_depth(quantized.selection_bg, depth);
    quantized.chip_go = quantize_color_for_depth(quantized.chip_go, depth);
    quantized.chip_dismiss = quantize_color_for_depth(quantized.chip_dismiss, depth);
    for color in &mut quantized.accents {
        *color = quantize_color_for_depth(*color, depth);
    }
    for swatch in &mut quantized.swatches {
        swatch.color = quantize_color_for_depth(swatch.color, depth);
    }
    for color in quantized.derived_locks.values_mut() {
        *color = quantize_color_for_depth(*color, depth);
    }
    quantized
}

fn xterm_256_rgb(index: u8) -> Color {
    if index < 16 {
        let (r, g, b) = ANSI_16_RGB[index as usize];
        return Color::Rgb(r, g, b);
    }
    if index < 232 {
        const CUBE: [u8; 6] = [0, 95, 135, 175, 215, 255];
        let cube = index - 16;
        let r = CUBE[(cube / 36) as usize];
        let g = CUBE[((cube % 36) / 6) as usize];
        let b = CUBE[(cube % 6) as usize];
        return Color::Rgb(r, g, b);
    }
    let value = 8 + ((index - 232) * 10);
    Color::Rgb(value, value, value)
}

fn squared_rgb_distance(a: (u8, u8, u8), b: (u8, u8, u8)) -> u32 {
    let dr = i32::from(a.0) - i32::from(b.0);
    let dg = i32::from(a.1) - i32::from(b.1);
    let db = i32::from(a.2) - i32::from(b.2);
    (dr * dr + dg * dg + db * db) as u32
}

const ANSI_16_RGB: [(u8, u8, u8); 16] = [
    (0, 0, 0), (128, 0, 0), (0, 128, 0), (128, 128, 0),
    (0, 0, 128), (128, 0, 128), (0, 128, 128), (192, 192, 192),
    (128, 128, 128), (255, 0, 0), (0, 255, 0), (255, 255, 0),
    (0, 0, 255), (255, 0, 255), (0, 255, 255), (255, 255, 255),
];

impl ThemePaletteDraft {
    pub fn from_palette(palette: &ThemePalette) -> Self {
        Self {
            slug: palette.slug.to_string(),
            name: palette.name.to_string(),
            description: palette.description.to_string(),
            dark: palette.dark,
            panel_bg: palette.panel_bg,
            border: palette.border,
            title: palette.title,
            tab_active: palette.tab_active,
            tab_inactive: palette.tab_inactive,
            header: palette.header,
            label: palette.label,
            value: palette.value,
            selection_bg: palette.selection_bg,
            chip_go: palette.chip_go,
            chip_dismiss: palette.chip_dismiss,
            accents: palette.accents,
            swatches: Vec::new(),
            slot_bindings: BTreeMap::new(),
            derived_locks: BTreeMap::new(),
            source: ThemeDraftSource::BuiltIn,
        }
    }

    pub fn from_theme(theme: Theme) -> Self {
        Self {
            slug: theme.slug.to_string(),
            name: theme.name.to_string(),
            description: theme.description.to_string(),
            dark: theme.dark,
            panel_bg: theme.panel_bg,
            border: theme.border,
            title: theme.title,
            tab_active: theme.tab_active,
            tab_inactive: theme.tab_inactive,
            header: theme.header,
            label: theme.label,
            value: theme.value,
            selection_bg: theme.selection_bg,
            chip_go: theme.chip_go,
            chip_dismiss: theme.chip_dismiss,
            accents: theme.accents,
            swatches: Vec::new(),
            slot_bindings: BTreeMap::new(),
            derived_locks: BTreeMap::new(),
            source: if palette_by_slug(theme.slug).is_some() { ThemeDraftSource::BuiltIn } else { ThemeDraftSource::Custom },
        }
    }

    pub fn save_slug(&self) -> anyhow::Result<String> {
        if matches!(self.source, ThemeDraftSource::BuiltIn | ThemeDraftSource::NewCustom) {
            make_unique_custom_slug(&self.slug, None)
        } else {
            validate_theme_slug(&self.slug)
        }
    }

    pub fn save_path(&self) -> anyhow::Result<PathBuf> {
        theme_path_for_slug(&self.save_slug()?)
    }

    pub fn color_at_slot(&self, slot: BuilderSlot) -> Color {
        match slot {
            BuilderSlot::Role(index) => self.role_color(index),
            BuilderSlot::Accent(index) => self.accents[index.min(THEME_ACCENT_COUNT - 1)],
        }
    }

    pub fn set_color_at_slot(&mut self, slot: BuilderSlot, color: Color) {
        match slot {
            BuilderSlot::Role(index) => self.set_role_color(index, color),
            BuilderSlot::Accent(index) => {
                self.set_accent_color_direct(index, color);
                self.slot_bindings.remove(&accent_binding_key(index));
            }
        }
    }

    pub fn role_color(&self, index: usize) -> Color {
        match index.min(ROLE_KEYS.len() - 1) {
            0 => self.panel_bg,
            1 => self.border,
            2 => self.title,
            3 => self.tab_active,
            4 => self.tab_inactive,
            5 => self.header,
            6 => self.label,
            7 => self.value,
            8 => self.selection_bg,
            9 => self.chip_go,
            _ => self.chip_dismiss,
        }
    }

    pub fn set_role_color(&mut self, index: usize, color: Color) {
        self.set_role_color_direct(index, color);
        self.slot_bindings.remove(&role_binding_key(index));
    }

    fn set_role_color_direct(&mut self, index: usize, color: Color) {
        match index.min(ROLE_KEYS.len() - 1) {
            0 => self.panel_bg = color,
            1 => self.border = color,
            2 => self.title = color,
            3 => self.tab_active = color,
            4 => self.tab_inactive = color,
            5 => self.header = color,
            6 => self.label = color,
            7 => self.value = color,
            8 => self.selection_bg = color,
            9 => self.chip_go = color,
            _ => self.chip_dismiss = color,
        }
    }

    fn set_accent_color_direct(&mut self, index: usize, color: Color) {
        self.accents[index.min(THEME_ACCENT_COUNT - 1)] = color;
    }

    pub fn slot_binding_name(&self, slot: BuilderSlot) -> Option<&str> {
        self.slot_bindings.get(&binding_key_for_slot(slot)).map(String::as_str)
    }

    pub fn bind_slot_to_swatch(&mut self, slot: BuilderSlot, swatch_name: &str) -> anyhow::Result<()> {
        let (color, name) = self.swatches
            .iter()
            .find(|swatch| swatch.name == swatch_name)
            .map(|swatch| (swatch.color, swatch.name.clone()))
            .ok_or_else(|| anyhow::anyhow!("unknown swatch '{swatch_name}'"))?;
        self.set_color_at_slot_direct(slot, color);
        self.slot_bindings.insert(binding_key_for_slot(slot), name);
        Ok(())
    }

    pub fn update_swatch_color(&mut self, swatch_name: &str, color: Color) -> bool {
        let mut found = false;
        for swatch in &mut self.swatches {
            if swatch.name == swatch_name {
                swatch.color = color;
                found = true;
                break;
            }
        }
        if found {
            self.resolve_bindings_for_swatch(swatch_name);
        }
        found
    }

    pub fn remove_swatch_at(&mut self, index: usize) -> Option<NamedSwatch> {
        if index >= self.swatches.len() {
            return None;
        }
        let removed = self.swatches.remove(index);
        self.slot_bindings.retain(|_, name| name != &removed.name);
        Some(removed)
    }

    pub fn with_resolved_bindings(&self) -> Self {
        let mut resolved = self.clone();
        resolved.resolve_all_bindings();
        resolved
    }

    pub fn resolve_all_bindings(&mut self) {
        let bindings = self.slot_bindings.clone();
        for (key, swatch_name) in bindings {
            if let Some(color) = self.swatches.iter().find(|swatch| swatch.name == swatch_name).map(|swatch| swatch.color) {
                self.set_color_by_binding_key(&key, color);
            } else {
                self.slot_bindings.remove(&key);
            }
        }
    }

    fn resolve_bindings_for_swatch(&mut self, swatch_name: &str) {
        let Some(color) = self.swatches.iter().find(|swatch| swatch.name == swatch_name).map(|swatch| swatch.color) else {
            return;
        };
        let keys = self.slot_bindings
            .iter()
            .filter_map(|(key, name)| if name == swatch_name { Some(key.clone()) } else { None })
            .collect::<Vec<_>>();
        for key in keys {
            self.set_color_by_binding_key(&key, color);
        }
    }

    fn set_color_by_binding_key(&mut self, key: &str, color: Color) {
        if let Some(index) = key.strip_prefix("roles.").and_then(role_index_by_key) {
            self.set_role_color_direct(index, color);
        } else if let Some(index) = accent_index_by_binding_key(key) {
            self.set_accent_color_direct(index, color);
        }
    }

    fn set_color_at_slot_direct(&mut self, slot: BuilderSlot, color: Color) {
        match slot {
            BuilderSlot::Role(index) => self.set_role_color_direct(index, color),
            BuilderSlot::Accent(index) => self.set_accent_color_direct(index, color),
        }
    }

    pub fn mode_label(&self) -> &'static str {
        if self.dark { "Dark" } else { "Light" }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuilderSlot {
    Role(usize),
    Accent(usize),
}

impl BuilderSlot {
    pub fn index(self) -> usize {
        match self {
            Self::Role(index) => index.min(ROLE_KEYS.len() - 1),
            Self::Accent(index) => ROLE_KEYS.len() + index.min(THEME_ACCENT_COUNT - 1),
        }
    }

    pub fn from_index(index: usize) -> Self {
        if index < ROLE_KEYS.len() {
            Self::Role(index)
        } else {
            Self::Accent((index - ROLE_KEYS.len()).min(THEME_ACCENT_COUNT - 1))
        }
    }

    pub fn previous(self) -> Self {
        let total = ROLE_KEYS.len() + THEME_ACCENT_COUNT;
        Self::from_index((self.index() + total - 1) % total)
    }

    pub fn next(self) -> Self {
        let total = ROLE_KEYS.len() + THEME_ACCENT_COUNT;
        Self::from_index((self.index() + 1) % total)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Role(index) => ROLE_LABELS[index.min(ROLE_LABELS.len() - 1)],
            Self::Accent(index) => ACCENT_LABELS[index.min(THEME_ACCENT_COUNT - 1)],
        }
    }
}


fn role_binding_key(index: usize) -> String {
    format!("roles.{}", ROLE_KEYS[index.min(ROLE_KEYS.len() - 1)])
}

fn accent_binding_key(index: usize) -> String {
    match index.min(THEME_ACCENT_COUNT - 1) {
        0..=11 => format!("accents.hue.{index}"),
        WARM_ACCENT => "accents.warm".to_string(),
        COOL_ACCENT => "accents.cool".to_string(),
        INFO_ACCENT => "accents.info".to_string(),
        SUCCESS_ACCENT => "accents.success".to_string(),
        _ => unreachable!("accent index is clamped"),
    }
}

fn binding_key_for_slot(slot: BuilderSlot) -> String {
    match slot {
        BuilderSlot::Role(index) => role_binding_key(index),
        BuilderSlot::Accent(index) => accent_binding_key(index),
    }
}

fn role_index_by_key(key: &str) -> Option<usize> {
    ROLE_KEYS.iter().position(|candidate| *candidate == key)
}

fn accent_index_by_binding_key(key: &str) -> Option<usize> {
    match key {
        "accents.warm" => Some(WARM_ACCENT),
        "accents.cool" => Some(COOL_ACCENT),
        "accents.info" => Some(INFO_ACCENT),
        "accents.success" => Some(SUCCESS_ACCENT),
        _ => key.strip_prefix("accents.hue.")
            .and_then(|suffix| suffix.parse::<usize>().ok())
            .filter(|index| *index < 12),
    }
}

fn swatch_ref_name(token: &str, swatches: &BTreeMap<String, Color>) -> anyhow::Result<Option<String>> {
    let trimmed = token.trim();
    let Some(name) = trimmed.strip_prefix('$') else {
        return Ok(None);
    };
    if name.is_empty() || name.chars().any(char::is_whitespace) || name.contains('/') || name.contains('\\') {
        anyhow::bail!("invalid swatch reference '${name}'");
    }
    if !swatches.contains_key(name) {
        anyhow::bail!("unknown swatch reference '${name}'");
    }
    Ok(Some(name.to_string()))
}

fn parse_palette_slot_token(
    token: &str,
    swatches: &BTreeMap<String, Color>,
    binding_key: String,
    slot_bindings: &mut BTreeMap<String, String>,
) -> anyhow::Result<Color> {
    if let Some(name) = swatch_ref_name(token, swatches)? {
        slot_bindings.insert(binding_key, name);
    }
    parse_color_token(token, swatches)
}

fn color_token_for_bound_slot(draft: &ThemePaletteDraft, binding_key: String, color: Color) -> String {
    if let Some(name) = draft.slot_bindings.get(&binding_key) {
        if draft.swatches.iter().any(|swatch| swatch.name == *name) {
            return format!("${name}");
        }
    }
    color_to_hex(color)
}


/// Build a derived theme for transient preview calculations without allocating
/// leaked runtime metadata. Use `theme_from_draft` when the result must live in
/// `AppState`; use this for throwaway render-time derivation.
pub fn preview_theme_from_draft(draft: &ThemePaletteDraft) -> Theme {
    let draft = draft.with_resolved_bindings();
    let palette = ThemePalette {
        slug: "theme-builder-preview",
        name: "Theme Builder Preview",
        description: "Transient preview theme",
        dark: draft.dark,
        panel_bg: draft.panel_bg,
        border: draft.border,
        title: draft.title,
        tab_active: draft.tab_active,
        tab_inactive: draft.tab_inactive,
        header: draft.header,
        label: draft.label,
        value: draft.value,
        selection_bg: draft.selection_bg,
        chip_go: draft.chip_go,
        chip_dismiss: draft.chip_dismiss,
        accents: draft.accents,
    };
    Theme::from_palette(&palette)
}

pub fn theme_from_draft(draft: &ThemePaletteDraft) -> Theme {
    let draft = draft.with_resolved_bindings();
    let palette = ThemePalette {
        slug: intern_runtime_string(normalize_slug(&draft.slug)),
        name: intern_runtime_string(draft.name.clone()),
        description: intern_runtime_string(draft.description.clone()),
        dark: draft.dark,
        panel_bg: draft.panel_bg,
        border: draft.border,
        title: draft.title,
        tab_active: draft.tab_active,
        tab_inactive: draft.tab_inactive,
        header: draft.header,
        label: draft.label,
        value: draft.value,
        selection_bg: draft.selection_bg,
        chip_go: draft.chip_go,
        chip_dismiss: draft.chip_dismiss,
        accents: draft.accents,
    };
    Theme::from_palette(&palette)
}

pub fn resolve_theme_draft(
    draft: &ThemePaletteDraft,
    options: ThemeApplyOptions,
    user_overrides: &ThemeOverrides,
) -> Theme {
    resolve_theme_draft_for_depth(draft, options, user_overrides, ColorDepth::TrueColor)
}

pub fn preview_resolve_theme_draft_for_depth(
    draft: &ThemePaletteDraft,
    options: ThemeApplyOptions,
    user_overrides: &ThemeOverrides,
    depth: ColorDepth,
) -> Theme {
    let quantized_draft = quantize_draft_for_depth(draft, depth);
    let mut theme = preview_theme_from_draft(&quantized_draft);
    quantize_theme_for_depth(&mut theme, depth);
    if options.honor_theme_locks {
        apply_derived_locks(&mut theme, &quantized_draft.derived_locks);
    }
    if options.keep_user_overrides {
        apply_derived_locks_for_depth(&mut theme, &user_overrides.overrides, depth);
    }
    theme
}

pub fn resolve_theme_draft_for_depth(
    draft: &ThemePaletteDraft,
    options: ThemeApplyOptions,
    user_overrides: &ThemeOverrides,
    depth: ColorDepth,
) -> Theme {
    let quantized_draft = quantize_draft_for_depth(draft, depth);
    let mut theme = theme_from_draft(&quantized_draft);
    quantize_theme_for_depth(&mut theme, depth);
    if options.honor_theme_locks {
        apply_derived_locks(&mut theme, &quantized_draft.derived_locks);
    }
    if options.keep_user_overrides {
        apply_derived_locks_for_depth(&mut theme, &user_overrides.overrides, depth);
    }
    theme
}

pub fn load_runtime_theme(slug: &str, options: ThemeApplyOptions) -> anyhow::Result<Theme> {
    let overrides = ThemeOverrides::load_default().unwrap_or_default();
    let draft = load_theme_draft(slug)?;
    Ok(resolve_theme_draft(&draft, options, &overrides))
}

pub fn load_theme_draft(slug: &str) -> anyhow::Result<ThemePaletteDraft> {
    if let Some(palette) = palette_by_slug(slug) {
        return Ok(ThemePaletteDraft::from_palette(palette));
    }
    load_custom_theme_by_slug(slug)
}

pub fn load_custom_theme_by_slug(slug: &str) -> anyhow::Result<ThemePaletteDraft> {
    let normalized = validate_theme_slug(slug)?;
    if palette_by_slug(&normalized).is_some() {
        anyhow::bail!("custom theme slug '{normalized}' collides with a built-in theme");
    }
    let direct_path = theme_path_for_slug(&normalized)?;
    if direct_path.exists() {
        let draft = load_theme_file(&direct_path)?;
        if draft.slug == normalized {
            return Ok(draft);
        }
    }

    for draft in load_custom_theme_files()? {
        if draft.slug == normalized {
            return Ok(draft);
        }
    }
    anyhow::bail!("unknown theme slug '{slug}'")
}

pub fn load_custom_theme_files() -> anyhow::Result<Vec<ThemePaletteDraft>> {
    let dir = theme_dir();
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut drafts = Vec::new();
    let mut entries = std::fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.path());
    for entry in entries {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("toml") {
            continue;
        }
        match load_theme_file(&path) {
            Ok(mut draft) => {
                if palette_by_slug(&draft.slug).is_some() {
                    log::warn!(
                        "Skipping custom theme {}: slug '{}' collides with a built-in theme",
                        path.display(),
                        draft.slug
                    );
                    continue;
                }
                draft.source = ThemeDraftSource::Custom;
                drafts.push(draft);
            }
            Err(err) => log::warn!("Skipping invalid custom theme {}: {err}", path.display()),
        }
    }
    Ok(drafts)
}

pub fn save_theme_file(draft: &ThemePaletteDraft) -> anyhow::Result<PathBuf> {
    let save_slug = draft.save_slug()?;
    if palette_by_slug(&save_slug).is_some() {
        anyhow::bail!("custom theme slug '{save_slug}' collides with a built-in theme");
    }
    let path = theme_path_for_slug(&save_slug)?;
    let mut file_draft = draft.clone();
    file_draft.slug = save_slug;
    file_draft.source = ThemeDraftSource::Custom;
    let file = ThemeFile::from_draft(&file_draft);
    let encoded = toml::to_string_pretty(&file)?;
    atomic_write(&path, encoded.as_bytes())?;
    Ok(path)
}

pub fn delete_custom_theme_file(slug: &str) -> anyhow::Result<PathBuf> {
    let normalized = validate_theme_slug(slug)?;
    if palette_by_slug(&normalized).is_some() {
        anyhow::bail!("built-in theme '{normalized}' cannot be deleted");
    }
    let path = theme_path_for_slug(&normalized)?;
    if !path.exists() {
        anyhow::bail!("custom theme '{normalized}' does not exist");
    }
    std::fs::remove_file(&path)?;
    sync_parent_directory(path.parent().unwrap_or_else(|| Path::new(".")));
    Ok(path)
}

/// Directory where user-authored theme files are stored.
pub fn custom_theme_dir() -> PathBuf {
    theme_dir()
}

/// Return the canonical custom theme path for a validated slug.
pub fn custom_theme_path_for_slug(slug: &str) -> anyhow::Result<PathBuf> {
    theme_path_for_slug(slug)
}

/// Pick a collision-free custom slug immediately, before a UI action exposes it.
pub fn unique_custom_theme_slug(base: &str) -> anyhow::Result<String> {
    unique_custom_theme_slug_excluding_path(base, None)
}

/// Pick a collision-free custom slug while ignoring the file that is about to
/// be overwritten. This keeps repeated exports to the same explicit path
/// idempotent: the destination file's current internal slug must not be
/// treated as a collision with itself.
pub fn unique_custom_theme_slug_excluding_path(base: &str, exclude_path: Option<&Path>) -> anyhow::Result<String> {
    make_unique_custom_slug(base, exclude_path)
}

/// Write a theme draft to an explicit export path using the normal theme-file format.
pub fn export_theme_file_to_path(draft: &ThemePaletteDraft, path: &Path) -> anyhow::Result<PathBuf> {
    if palette_by_slug(&draft.slug).is_some() {
        anyhow::bail!("theme slug '{}' collides with a built-in theme", draft.slug);
    }
    let file = ThemeFile::from_draft(draft);
    let encoded = toml::to_string_pretty(&file)?;
    atomic_write(path, encoded.as_bytes())?;
    Ok(path.to_path_buf())
}

/// Import a theme file into the user's custom theme directory with a collision-free slug.
pub fn import_theme_file_to_custom_dir(path: &Path) -> anyhow::Result<(ThemePaletteDraft, PathBuf)> {
    let mut draft = load_theme_file(path)?;
    let mut base = draft.slug.clone();
    if palette_by_slug(&base).is_some() {
        base = format!("{base}-custom");
    }
    draft.slug = unique_custom_theme_slug(&base)?;
    draft.source = ThemeDraftSource::Custom;
    let saved_path = save_theme_file(&draft)?;
    let loaded = load_theme_file(&saved_path)?;
    Ok((loaded, saved_path))
}

pub fn load_theme_file(path: &Path) -> anyhow::Result<ThemePaletteDraft> {
    let content = std::fs::read_to_string(path)?;
    let file: ThemeFile = toml::from_str(&content)?;
    file.into_draft()
}

pub fn theme_choices() -> Vec<ThemeChoice> {
    let mut choices: Vec<ThemeChoice> = PALETTES.iter().map(|palette| ThemeChoice {
        slug: palette.slug.to_string(),
        name: palette.name.to_string(),
        description: palette.description.to_string(),
        dark: palette.dark,
        built_in: true,
        author_lock_count: 0,
        accents: palette.accents,
    }).collect();

    if let Ok(custom) = load_custom_theme_files() {
        let mut custom: Vec<ThemeChoice> = custom.into_iter().map(|draft| ThemeChoice {
            slug: draft.slug,
            name: draft.name,
            description: draft.description,
            dark: draft.dark,
            built_in: false,
            author_lock_count: draft.derived_locks.len(),
            accents: draft.accents,
        }).collect();
        custom.sort_by(|a, b| a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase()));
        choices.extend(custom);
    }
    choices
}

pub fn next_theme_slug_in_library(current: &str, forward: bool) -> String {
    let choices = theme_choices();
    next_theme_slug_in_choices(current, forward, &choices)
}

pub fn next_theme_slug_in_choices(current: &str, forward: bool, choices: &[ThemeChoice]) -> String {
    if choices.is_empty() {
        return DEFAULT_THEME_SLUG.to_string();
    }
    let normalized = normalize_slug(current);
    let idx = choices.iter().position(|choice| choice.slug == normalized).unwrap_or(0);
    let next = if forward {
        (idx + 1) % choices.len()
    } else {
        (idx + choices.len() - 1) % choices.len()
    };
    choices[next].slug.clone()
}

pub fn theme_color_by_derived_key(theme: Theme, key: &str) -> Option<Color> {
    Some(match key {
        "surface" => theme.surface,
        "border_dim" => theme.border_dim,
        "text_bright" => theme.text_bright,
        "text_dim" => theme.text_dim,
        "hover_bg" => theme.hover_bg,
        "input_focused_bg" => theme.input_focused_bg,
        "input_unfocused_bg" => theme.input_unfocused_bg,
        "input_disabled_bg" => theme.input_disabled_bg,
        "dropdown_bg" => theme.dropdown_bg,
        "pill_active_bg" => theme.pill_active_bg,
        "pill_active_fg" => theme.pill_active_fg,
        "pill_dim_bg" => theme.pill_dim_bg,
        "pill_preset_bg" => theme.pill_preset_bg,
        "pill_preset_fg" => theme.pill_preset_fg,
        "progress_dialog_bg" => theme.progress_dialog_bg,
        "progress_dialog_border" => theme.progress_dialog_border,
        "progress_dialog_text" => theme.progress_dialog_text,
        "progress_dialog_title" => theme.progress_dialog_title,
        "progress_dialog_label" => theme.progress_dialog_label,
        "progress_dialog_current_file" => theme.progress_dialog_current_file,
        "progress_dialog_dim" => theme.progress_dialog_dim,
        "progress_dialog_bar_filled" => theme.progress_dialog_bar_filled,
        "progress_dialog_bar_unfilled" => theme.progress_dialog_bar_unfilled,
        "progress_dialog_percent" => theme.progress_dialog_percent,
        "progress_dialog_button_bg" => theme.progress_dialog_button_bg,
        "progress_dialog_button_fg" => theme.progress_dialog_button_fg,
        "progress_dialog_abort_bg" => theme.progress_dialog_abort_bg,
        "progress_dialog_abort_fg" => theme.progress_dialog_abort_fg,
        "error_dim" => theme.error_dim,
        _ => return None,
    })
}

pub fn set_theme_derived_color(theme: &mut Theme, key: &str, color: Color) -> bool {
    match key {
        "surface" => theme.surface = color,
        "border_dim" => theme.border_dim = color,
        "text_bright" => theme.text_bright = color,
        "text_dim" => theme.text_dim = color,
        "hover_bg" => theme.hover_bg = color,
        "input_focused_bg" => theme.input_focused_bg = color,
        "input_unfocused_bg" => theme.input_unfocused_bg = color,
        "input_disabled_bg" => theme.input_disabled_bg = color,
        "dropdown_bg" => theme.dropdown_bg = color,
        "pill_active_bg" => theme.pill_active_bg = color,
        "pill_active_fg" => theme.pill_active_fg = color,
        "pill_dim_bg" => theme.pill_dim_bg = color,
        "pill_preset_bg" => theme.pill_preset_bg = color,
        "pill_preset_fg" => theme.pill_preset_fg = color,
        "progress_dialog_bg" => theme.progress_dialog_bg = color,
        "progress_dialog_border" => theme.progress_dialog_border = color,
        "progress_dialog_text" => theme.progress_dialog_text = color,
        "progress_dialog_title" => theme.progress_dialog_title = color,
        "progress_dialog_label" => theme.progress_dialog_label = color,
        "progress_dialog_current_file" => theme.progress_dialog_current_file = color,
        "progress_dialog_dim" => theme.progress_dialog_dim = color,
        "progress_dialog_bar_filled" => theme.progress_dialog_bar_filled = color,
        "progress_dialog_bar_unfilled" => theme.progress_dialog_bar_unfilled = color,
        "progress_dialog_percent" => theme.progress_dialog_percent = color,
        "progress_dialog_button_bg" => theme.progress_dialog_button_bg = color,
        "progress_dialog_button_fg" => theme.progress_dialog_button_fg = color,
        "progress_dialog_abort_bg" => theme.progress_dialog_abort_bg = color,
        "progress_dialog_abort_fg" => theme.progress_dialog_abort_fg = color,
        "error_dim" => theme.error_dim = color,
        _ => return false,
    }
    true
}

pub fn apply_derived_locks(theme: &mut Theme, locks: &BTreeMap<String, Color>) {
    for (key, color) in locks {
        let _ = set_theme_derived_color(theme, key, *color);
    }
}

fn apply_derived_locks_for_depth(theme: &mut Theme, locks: &BTreeMap<String, Color>, depth: ColorDepth) {
    for (key, color) in locks {
        let _ = set_theme_derived_color(theme, key, quantize_color_for_depth(*color, depth));
    }
}

static RUNTIME_THEME_STRING_INTERNER: Lazy<Mutex<BTreeMap<String, &'static str>>> = Lazy::new(|| Mutex::new(BTreeMap::new()));

fn intern_runtime_string(value: String) -> &'static str {
    let mut interner = RUNTIME_THEME_STRING_INTERNER
        .lock()
        .expect("runtime theme string interner poisoned");
    if let Some(existing) = interner.get(&value) {
        return *existing;
    }
    let interned = Box::leak(value.clone().into_boxed_str());
    interner.insert(value, interned);
    interned
}

fn make_unique_custom_slug(base: &str, exclude_path: Option<&Path>) -> anyhow::Result<String> {
    let normalized = validate_theme_slug(base)?;
    let existing_internal_slugs = custom_theme_internal_slugs_excluding_path(exclude_path)?;
    if !custom_theme_candidate_exists(&normalized, &existing_internal_slugs, exclude_path)? {
        return Ok(normalized);
    }
    for idx in 2..10_000usize {
        let candidate = format!("{normalized}-{idx}");
        if !custom_theme_candidate_exists(&candidate, &existing_internal_slugs, exclude_path)? {
            return Ok(candidate);
        }
    }
    Ok(format!("{normalized}-{}", chrono::Utc::now().timestamp()))
}

fn custom_theme_internal_slugs_excluding_path(exclude_path: Option<&Path>) -> anyhow::Result<BTreeSet<String>> {
    let dir = theme_dir();
    if !dir.exists() {
        return Ok(BTreeSet::new());
    }

    let mut slugs = BTreeSet::new();
    let mut entries = std::fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.path());
    for entry in entries {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("toml") {
            continue;
        }
        if exclude_path.map_or(false, |exclude| same_theme_file_path(&path, exclude)) {
            continue;
        }
        match load_theme_file(&path) {
            Ok(draft) => {
                if palette_by_slug(&draft.slug).is_none() {
                    slugs.insert(draft.slug);
                }
            }
            Err(err) => log::warn!("Skipping invalid custom theme {}: {err}", path.display()),
        }
    }
    Ok(slugs)
}

fn custom_theme_candidate_exists(
    slug: &str,
    existing_internal_slugs: &BTreeSet<String>,
    exclude_path: Option<&Path>,
) -> anyhow::Result<bool> {
    let normalized = validate_theme_slug(slug)?;
    let canonical_path = theme_path_for_slug(&normalized)?;
    let canonical_collides = canonical_path.exists()
        && !exclude_path.map_or(false, |exclude| same_theme_file_path(&canonical_path, exclude));
    Ok(
        palette_by_slug(&normalized).is_some()
            || existing_internal_slugs.contains(&normalized)
            || canonical_collides
    )
}

fn same_theme_file_path(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

pub fn is_canonical_custom_theme_path_for_slug(path: &Path, slug: &str) -> anyhow::Result<bool> {
    let canonical_path = theme_path_for_slug(slug)?;
    if path == canonical_path {
        return Ok(true);
    }
    match (path.canonicalize(), canonical_path.canonicalize()) {
        (Ok(left), Ok(right)) => Ok(left == right),
        _ => Ok(false),
    }
}

fn theme_path_for_slug(slug: &str) -> anyhow::Result<PathBuf> {
    let normalized = validate_theme_slug(slug)?;
    Ok(theme_dir().join(format!("{normalized}.toml")))
}

fn atomic_write(path: &Path, contents: &[u8]) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("cannot write {} without a parent directory", path.display()))?;
    std::fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow::anyhow!("invalid output filename {}", path.display()))?;
    let tmp_path = parent.join(format!(".{file_name}.{}.tmp", uuid::Uuid::new_v4()));

    let result = (|| -> anyhow::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)?;
        file.write_all(contents)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp_path, path)?;
        sync_parent_directory(parent);
        Ok(())
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(&tmp_path);
    }
    result
}

#[cfg(unix)]
fn sync_parent_directory(parent: &Path) {
    if let Ok(dir) = std::fs::File::open(parent) {
        let _ = dir.sync_all();
    }
}

#[cfg(not(unix))]
fn sync_parent_directory(_parent: &Path) {}

fn parse_color_token(token: &str, swatches: &BTreeMap<String, Color>) -> anyhow::Result<Color> {
    let trimmed = token.trim();
    if let Some(name) = trimmed.strip_prefix('$') {
        if let Some(color) = swatches.get(name) {
            return Ok(*color);
        }
        anyhow::bail!("unknown swatch reference '${name}'");
    }
    parse_hex_color(trimmed)
}

fn color_token_for(color: Color, _swatches: &[NamedSwatch]) -> String {
    // Do not infer symbolic references from color equality. Bindings are
    // semantic edges in the draft model, not a property of equal RGB values.
    color_to_hex(color)
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct ThemeFile {
    meta: ThemeFileMeta,
    roles: ThemeFileRoles,
    accents: ThemeFileAccents,
    #[serde(default)]
    swatches: BTreeMap<String, String>,
    #[serde(default)]
    derived_locks: BTreeMap<String, String>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct ThemeFileMeta {
    name: String,
    slug: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    dark: bool,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct ThemeFileRoles {
    panel_bg: String,
    border: String,
    title: String,
    tab_active: String,
    tab_inactive: String,
    header: String,
    label: String,
    value: String,
    selection_bg: String,
    chip_go: String,
    chip_dismiss: String,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct ThemeFileAccents {
    hue: Vec<String>,
    warm: String,
    cool: String,
    info: String,
    success: String,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct ThemeOverridesFile {
    #[serde(default)]
    overrides: BTreeMap<String, String>,
}

impl ThemeFile {
    fn from_draft(draft: &ThemePaletteDraft) -> Self {
        let swatches: BTreeMap<String, String> = draft.swatches.iter()
            .map(|swatch| (swatch.name.clone(), color_to_hex(swatch.color)))
            .collect();
        Self {
            meta: ThemeFileMeta {
                name: draft.name.clone(),
                slug: draft.slug.clone(),
                description: draft.description.clone(),
                dark: draft.dark,
            },
            roles: ThemeFileRoles {
                panel_bg: color_token_for_bound_slot(draft, role_binding_key(0), draft.panel_bg),
                border: color_token_for_bound_slot(draft, role_binding_key(1), draft.border),
                title: color_token_for_bound_slot(draft, role_binding_key(2), draft.title),
                tab_active: color_token_for_bound_slot(draft, role_binding_key(3), draft.tab_active),
                tab_inactive: color_token_for_bound_slot(draft, role_binding_key(4), draft.tab_inactive),
                header: color_token_for_bound_slot(draft, role_binding_key(5), draft.header),
                label: color_token_for_bound_slot(draft, role_binding_key(6), draft.label),
                value: color_token_for_bound_slot(draft, role_binding_key(7), draft.value),
                selection_bg: color_token_for_bound_slot(draft, role_binding_key(8), draft.selection_bg),
                chip_go: color_token_for_bound_slot(draft, role_binding_key(9), draft.chip_go),
                chip_dismiss: color_token_for_bound_slot(draft, role_binding_key(10), draft.chip_dismiss),
            },
            accents: ThemeFileAccents {
                hue: draft.accents[..12].iter().enumerate().map(|(index, color)| color_token_for_bound_slot(draft, accent_binding_key(index), *color)).collect(),
                warm: color_token_for_bound_slot(draft, accent_binding_key(WARM_ACCENT), draft.accents[WARM_ACCENT]),
                cool: color_token_for_bound_slot(draft, accent_binding_key(COOL_ACCENT), draft.accents[COOL_ACCENT]),
                info: color_token_for_bound_slot(draft, accent_binding_key(INFO_ACCENT), draft.accents[INFO_ACCENT]),
                success: color_token_for_bound_slot(draft, accent_binding_key(SUCCESS_ACCENT), draft.accents[SUCCESS_ACCENT]),
            },
            swatches,
            derived_locks: draft.derived_locks.iter()
                .map(|(key, color)| (key.clone(), color_token_for(*color, &draft.swatches)))
                .collect(),
        }
    }

    fn into_draft(self) -> anyhow::Result<ThemePaletteDraft> {
        if self.accents.hue.len() != 12 {
            anyhow::bail!("[accents].hue must contain exactly 12 colors");
        }
        let mut swatches = BTreeMap::new();
        for (name, value) in &self.swatches {
            swatches.insert(name.clone(), parse_hex_color(value)?);
        }
        let named_swatches = swatches.iter()
            .map(|(name, color)| NamedSwatch::new(name.clone(), *color))
            .collect::<Vec<_>>();
        let mut slot_bindings = BTreeMap::new();
        let mut accents = [Color::Reset; THEME_ACCENT_COUNT];
        for (idx, value) in self.accents.hue.iter().enumerate() {
            accents[idx] = parse_palette_slot_token(value, &swatches, accent_binding_key(idx), &mut slot_bindings)?;
        }
        accents[WARM_ACCENT] = parse_palette_slot_token(&self.accents.warm, &swatches, accent_binding_key(WARM_ACCENT), &mut slot_bindings)?;
        accents[COOL_ACCENT] = parse_palette_slot_token(&self.accents.cool, &swatches, accent_binding_key(COOL_ACCENT), &mut slot_bindings)?;
        accents[INFO_ACCENT] = parse_palette_slot_token(&self.accents.info, &swatches, accent_binding_key(INFO_ACCENT), &mut slot_bindings)?;
        accents[SUCCESS_ACCENT] = parse_palette_slot_token(&self.accents.success, &swatches, accent_binding_key(SUCCESS_ACCENT), &mut slot_bindings)?;

        let mut derived_locks = BTreeMap::new();
        for (key, value) in self.derived_locks {
            if derived_element_spec(&key).is_none() {
                anyhow::bail!("unknown derived lock '{key}'");
            }
            derived_locks.insert(key, parse_color_token(&value, &swatches)?);
        }

        Ok(ThemePaletteDraft {
            slug: validate_theme_slug(&self.meta.slug)?,
            name: self.meta.name,
            description: self.meta.description,
            dark: self.meta.dark,
            panel_bg: parse_palette_slot_token(&self.roles.panel_bg, &swatches, role_binding_key(0), &mut slot_bindings)?,
            border: parse_palette_slot_token(&self.roles.border, &swatches, role_binding_key(1), &mut slot_bindings)?,
            title: parse_palette_slot_token(&self.roles.title, &swatches, role_binding_key(2), &mut slot_bindings)?,
            tab_active: parse_palette_slot_token(&self.roles.tab_active, &swatches, role_binding_key(3), &mut slot_bindings)?,
            tab_inactive: parse_palette_slot_token(&self.roles.tab_inactive, &swatches, role_binding_key(4), &mut slot_bindings)?,
            header: parse_palette_slot_token(&self.roles.header, &swatches, role_binding_key(5), &mut slot_bindings)?,
            label: parse_palette_slot_token(&self.roles.label, &swatches, role_binding_key(6), &mut slot_bindings)?,
            value: parse_palette_slot_token(&self.roles.value, &swatches, role_binding_key(7), &mut slot_bindings)?,
            selection_bg: parse_palette_slot_token(&self.roles.selection_bg, &swatches, role_binding_key(8), &mut slot_bindings)?,
            chip_go: parse_palette_slot_token(&self.roles.chip_go, &swatches, role_binding_key(9), &mut slot_bindings)?,
            chip_dismiss: parse_palette_slot_token(&self.roles.chip_dismiss, &swatches, role_binding_key(10), &mut slot_bindings)?,
            accents,
            swatches: named_swatches,
            slot_bindings,
            derived_locks,
            source: ThemeDraftSource::Custom,
        })
    }
}

#[cfg(test)]
mod theme_builder_file_tests {
    use super::*;
    use crate::tui::test_support::XdgConfigHomeGuard;


    #[test]
    fn symbolic_swatch_bindings_survive_roundtrip_and_drive_bound_slots() {
        let _xdg = XdgConfigHomeGuard::new("tonepoet-theme-symbolic-swatch");
        let mut draft = ThemePaletteDraft::from_palette(default_palette());
        draft.source = ThemeDraftSource::NewCustom;
        draft.slug = "symbolic-theme".to_string();
        draft.swatches.push(NamedSwatch::new("brand_purple", Color::Rgb(1, 2, 3)));
        draft.bind_slot_to_swatch(BuilderSlot::Role(2), "brand_purple").expect("bind title");
        draft.bind_slot_to_swatch(BuilderSlot::Accent(WARM_ACCENT), "brand_purple").expect("bind warm accent");

        let path = save_theme_file(&draft).expect("save symbolic theme");
        let encoded = std::fs::read_to_string(&path).expect("read saved theme");
        assert!(encoded.contains("title = \"$brand_purple\""));
        assert!(encoded.contains("warm = \"$brand_purple\""));

        let mut loaded = load_theme_file(&path).expect("load symbolic theme");
        assert_eq!(loaded.slot_binding_name(BuilderSlot::Role(2)), Some("brand_purple"));
        assert_eq!(loaded.slot_binding_name(BuilderSlot::Accent(WARM_ACCENT)), Some("brand_purple"));
        loaded.update_swatch_color("brand_purple", Color::Rgb(9, 8, 7));
        assert_eq!(loaded.title, Color::Rgb(9, 8, 7));
        assert_eq!(loaded.accents[WARM_ACCENT], Color::Rgb(9, 8, 7));
    }

    #[test]
    fn symbolic_save_does_not_infer_duplicate_color_bindings() {
        let _xdg = XdgConfigHomeGuard::new("tonepoet-theme-no-equality-binding");
        let mut draft = ThemePaletteDraft::from_palette(default_palette());
        draft.source = ThemeDraftSource::NewCustom;
        draft.slug = "no-equality-binding".to_string();
        draft.swatches.push(NamedSwatch::new("brand_purple", Color::Rgb(10, 20, 30)));
        draft.title = Color::Rgb(10, 20, 30);
        draft.header = Color::Rgb(10, 20, 30);
        draft.bind_slot_to_swatch(BuilderSlot::Role(2), "brand_purple").expect("bind title only");

        let path = save_theme_file(&draft).expect("save theme");
        let encoded = std::fs::read_to_string(&path).expect("read saved theme");
        assert!(encoded.contains("title = \"$brand_purple\""));
        assert!(encoded.contains("header = \"#0A141E\""));
    }

    #[test]
    fn resolution_tally_counts_final_provenance_not_raw_maps() {
        let mut draft = ThemePaletteDraft::from_palette(default_palette());
        draft.derived_locks.insert("progress_dialog_border".to_string(), Color::Rgb(1, 2, 3));
        draft.derived_locks.insert("surface".to_string(), Color::Rgb(4, 5, 6));
        let mut overrides = ThemeOverrides::default();
        overrides.overrides.insert("progress_dialog_border".to_string(), Color::Rgb(7, 8, 9));

        let tally = theme_resolution_tally(&draft, ThemeApplyOptions::default(), &overrides);
        assert_eq!(tally.by_user, 1);
        assert_eq!(tally.by_theme, 1);
        assert_eq!(tally.auto, derived_element_specs().len() - 2);
    }

    #[test]
    fn delete_custom_theme_file_removes_customs_but_rejects_builtins() {
        let _xdg = XdgConfigHomeGuard::new("tonepoet-theme-delete-custom");
        let mut draft = ThemePaletteDraft::from_palette(default_palette());
        draft.source = ThemeDraftSource::NewCustom;
        draft.slug = "delete-me".to_string();
        let path = save_theme_file(&draft).expect("save theme");
        assert!(path.exists());
        let deleted = delete_custom_theme_file("delete-me").expect("delete custom theme");
        assert_eq!(deleted, path);
        assert!(!path.exists());
        assert!(delete_custom_theme_file(default_theme_slug()).is_err());
    }

    #[test]
    fn custom_theme_files_with_builtin_slug_are_skipped_and_not_saved() {
        let _xdg = XdgConfigHomeGuard::new("tonepoet-theme-builtin-collision");

        let mut colliding = ThemePaletteDraft::from_palette(default_palette());
        colliding.source = ThemeDraftSource::Custom;
        colliding.slug = default_theme_slug().to_string();
        assert!(save_theme_file(&colliding).is_err());

        let mut authored = ThemePaletteDraft::from_palette(default_palette());
        authored.source = ThemeDraftSource::Custom;
        authored.slug = default_theme_slug().to_string();
        authored.name = "Manual collision".to_string();
        std::fs::create_dir_all(theme_dir()).expect("create theme dir");
        let path = theme_dir().join("manual-collision.toml");
        let encoded = toml::to_string_pretty(&ThemeFile::from_draft(&authored)).expect("encode collision");
        std::fs::write(&path, encoded).expect("write collision file");

        let loaded = load_custom_theme_files().expect("load custom themes");
        assert!(loaded.iter().all(|draft| draft.slug != default_theme_slug()));
        assert!(load_custom_theme_by_slug(default_theme_slug()).is_err());
    }

    #[test]
    fn unique_custom_slug_checks_internal_slugs_from_noncanonical_files() {
        let _xdg = XdgConfigHomeGuard::new("tonepoet-theme-noncanonical-slug-unique");

        let mut draft = ThemePaletteDraft::from_palette(default_palette());
        draft.source = ThemeDraftSource::Custom;
        draft.slug = "export-me".to_string();
        draft.name = "Export Me".to_string();

        let noncanonical_path = theme_dir().join("explicit-export.toml");
        export_theme_file_to_path(&draft, &noncanonical_path).expect("write noncanonical export");
        assert!(noncanonical_path.exists());
        assert!(!theme_path_for_slug("export-me").expect("canonical path").exists());

        assert_eq!(unique_custom_theme_slug("export-me").expect("unique slug"), "export-me-2");
    }

    #[test]
    fn unique_custom_slug_can_exclude_destination_file_being_overwritten() {
        let _xdg = XdgConfigHomeGuard::new("tonepoet-theme-noncanonical-slug-exclude-destination");

        let mut draft = ThemePaletteDraft::from_palette(default_palette());
        draft.source = ThemeDraftSource::Custom;
        draft.slug = "export-me".to_string();
        draft.name = "Export Me".to_string();

        let noncanonical_path = theme_dir().join("explicit-export.toml");
        export_theme_file_to_path(&draft, &noncanonical_path).expect("write noncanonical export");
        assert_eq!(unique_custom_theme_slug("export-me").expect("without exclusion"), "export-me-2");
        assert_eq!(
            unique_custom_theme_slug_excluding_path("export-me", Some(&noncanonical_path))
                .expect("with destination exclusion"),
            "export-me"
        );
    }

    #[test]
    fn canonical_custom_theme_path_requires_slug_filename_match() {
        let _xdg = XdgConfigHomeGuard::new("tonepoet-theme-canonical-path-check");
        let canonical = theme_path_for_slug("canonical-check").expect("canonical path");
        let explicit = theme_dir().join("explicit-export.toml");
        std::fs::create_dir_all(theme_dir()).expect("create theme dir");
        std::fs::write(&canonical, "").expect("touch canonical");
        std::fs::write(&explicit, "").expect("touch explicit");

        assert!(is_canonical_custom_theme_path_for_slug(&canonical, "canonical-check").expect("canonical check"));
        assert!(!is_canonical_custom_theme_path_for_slug(&explicit, "canonical-check").expect("explicit check"));
    }

    #[test]
    fn custom_theme_file_round_trips_roles_accents_swatches_and_locks() {
        let _xdg = XdgConfigHomeGuard::new("tonepoet-theme-file-roundtrip");
        let mut draft = ThemePaletteDraft::from_palette(default_palette());
        draft.source = ThemeDraftSource::NewCustom;
        draft.slug = "my-custom-theme".to_string();
        draft.name = "My Custom Theme".to_string();
        draft.swatches.push(NamedSwatch::new("brand_purple", draft.header));
        draft.derived_locks.insert("progress_dialog_border".to_string(), draft.border);

        let path = save_theme_file(&draft).expect("save custom theme");
        let loaded = load_theme_file(&path).expect("load custom theme");

        assert_eq!(loaded.slug, "my-custom-theme");
        assert_eq!(loaded.name, "My Custom Theme");
        assert_eq!(loaded.accents[WARM_ACCENT], draft.accents[WARM_ACCENT]);
        assert_eq!(loaded.derived_locks.get("progress_dialog_border"), Some(&draft.border));
    }


    #[test]
    fn theme_slug_validation_rejects_path_material_and_bad_filenames() {
        for slug in [
            "",
            "../evil",
            "evil/slug",
            "evil\\slug",
            "evil slug",
            " evil",
            "evil ",
            "evil.slug",
            "evil:slug",
            "-evil",
            "evil-",
            "evil--slug",
        ] {
            assert!(validate_theme_slug(slug).is_err(), "slug should be rejected: {slug:?}");
        }
        assert_eq!(validate_theme_slug("My_Custom-Theme9").expect("valid slug"), "my-custom-theme9");
    }

    #[test]
    fn save_theme_file_rejects_unsafe_slug_before_building_a_path() {
        let _xdg = XdgConfigHomeGuard::new("tonepoet-theme-invalid-slug");
        let mut draft = ThemePaletteDraft::from_palette(default_palette());
        draft.source = ThemeDraftSource::NewCustom;
        draft.slug = "../escape".to_string();
        let err = save_theme_file(&draft).expect_err("unsafe slug must not be saved");
        assert!(err.to_string().contains("path separators") || err.to_string().contains(".."));
        assert!(!theme_dir().join("escape.toml").exists());
    }

    #[test]
    fn preview_resolution_does_not_use_runtime_theme_metadata() {
        let mut draft = ThemePaletteDraft::from_palette(default_palette());
        draft.slug = "preview-should-not-intern".to_string();
        draft.name = "Preview Should Not Intern".to_string();
        let theme = preview_resolve_theme_draft_for_depth(
            &draft,
            ThemeApplyOptions::default(),
            &ThemeOverrides::default(),
            ColorDepth::TrueColor,
        );
        assert_eq!(theme.slug, "theme-builder-preview");
        assert_eq!(theme.name, "Theme Builder Preview");
    }

    #[test]
    fn theme_and_override_persistence_use_final_files_without_leftover_temps() {
        let _xdg = XdgConfigHomeGuard::new("tonepoet-theme-atomic-write");
        let mut draft = ThemePaletteDraft::from_palette(default_palette());
        draft.source = ThemeDraftSource::NewCustom;
        draft.slug = "atomic-theme".to_string();
        let path = save_theme_file(&draft).expect("save theme atomically");
        assert!(path.exists());

        let mut overrides = ThemeOverrides::default();
        overrides.overrides.insert("surface".to_string(), Color::Rgb(1, 2, 3));
        overrides.save_default().expect("save overrides atomically");
        assert!(theme_overrides_path().exists());

        for dir in [theme_dir(), theme_overrides_path().parent().expect("override parent").to_path_buf()] {
            for entry in std::fs::read_dir(dir).expect("read persistence dir") {
                let path = entry.expect("dir entry").path();
                let name = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
                assert!(!name.ends_with(".tmp"), "leftover temp file: {}", path.display());
            }
        }
    }

    #[test]
    fn depth_aware_resolution_quantizes_inputs_and_locks() {
        let mut draft = ThemePaletteDraft::from_palette(default_palette());
        draft.panel_bg = Color::Rgb(12, 34, 56);
        draft.derived_locks.insert("progress_dialog_border".to_string(), Color::Rgb(17, 99, 201));
        let mut overrides = ThemeOverrides::default();
        overrides.overrides.insert("progress_dialog_border".to_string(), Color::Rgb(201, 99, 17));

        let theme = resolve_theme_draft_for_depth(
            &draft,
            ThemeApplyOptions { honor_theme_locks: false, keep_user_overrides: false },
            &overrides,
            ColorDepth::Xterm256,
        );
        assert_eq!(theme.panel_bg, quantize_color_for_depth(draft.panel_bg, ColorDepth::Xterm256));

        let theme = resolve_theme_draft_for_depth(
            &draft,
            ThemeApplyOptions { honor_theme_locks: true, keep_user_overrides: true },
            &overrides,
            ColorDepth::Xterm256,
        );
        assert_eq!(theme.progress_dialog_border, quantize_color_for_depth(Color::Rgb(201, 99, 17), ColorDepth::Xterm256));

        let theme = resolve_theme_draft_for_depth(
            &draft,
            ThemeApplyOptions { honor_theme_locks: true, keep_user_overrides: false },
            &overrides,
            ColorDepth::Xterm256,
        );
        assert_eq!(theme.progress_dialog_border, quantize_color_for_depth(Color::Rgb(17, 99, 201), ColorDepth::Xterm256));
    }

    #[test]
    fn resolve_theme_honors_author_locks_below_user_overrides() {
        let mut draft = ThemePaletteDraft::from_palette(default_palette());
        draft.derived_locks.insert("progress_dialog_border".to_string(), draft.border);
        let mut overrides = ThemeOverrides::default();
        overrides.overrides.insert("progress_dialog_border".to_string(), draft.accents[WARM_ACCENT]);

        let theme = resolve_theme_draft(&draft, ThemeApplyOptions::default(), &overrides);
        assert_eq!(theme.progress_dialog_border, draft.accents[WARM_ACCENT]);

        let theme = resolve_theme_draft(&draft, ThemeApplyOptions { honor_theme_locks: true, keep_user_overrides: false }, &overrides);
        assert_eq!(theme.progress_dialog_border, draft.border);

        let theme = resolve_theme_draft(&draft, ThemeApplyOptions { honor_theme_locks: false, keep_user_overrides: false }, &overrides);
        assert_eq!(theme.progress_dialog_border, draft.accents[INFO_ACCENT]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paired_theme_slug_maps_builtin_dark_light_pairs() {
        assert_eq!(paired_theme_slug("gruvbox").as_deref(), Some("gruvbox-light"));
        assert_eq!(paired_theme_slug("gruvbox-light").as_deref(), Some("gruvbox"));
        assert_eq!(paired_theme_slug("tokyo-night").as_deref(), Some("tokyo-night-day"));
        assert_eq!(paired_theme_slug("alucard").as_deref(), Some("dracula"));
    }


    fn hex_color(hex: &str) -> Color {
        let hex = hex.strip_prefix('#').expect("expected #rrggbb");
        assert_eq!(hex.len(), 6);
        let r = u8::from_str_radix(&hex[0..2], 16).expect("red");
        let g = u8::from_str_radix(&hex[2..4], 16).expect("green");
        let b = u8::from_str_radix(&hex[4..6], 16).expect("blue");
        Color::Rgb(r, g, b)
    }

    fn palette_roles(palette: &ThemePalette) -> [Color; 11] {
        [
            palette.panel_bg,
            palette.border,
            palette.title,
            palette.tab_active,
            palette.tab_inactive,
            palette.header,
            palette.label,
            palette.value,
            palette.selection_bg,
            palette.chip_go,
            palette.chip_dismiss,
        ]
    }

    fn squared_distance(a: (u8, u8, u8), b: (u8, u8, u8)) -> u32 {
        let dr = i32::from(a.0) - i32::from(b.0);
        let dg = i32::from(a.1) - i32::from(b.1);
        let db = i32::from(a.2) - i32::from(b.2);
        (dr * dr + dg * dg + db * db) as u32
    }

    fn nearest_xterm_256_index(color: Color) -> u8 {
        let target = rgb_components(color);
        let mut best_index = 0u8;
        let mut best_distance = u32::MAX;
        let mut consider = |index: u8, rgb: (u8, u8, u8)| {
            let distance = squared_distance(target, rgb);
            if distance < best_distance {
                best_index = index;
                best_distance = distance;
            }
        };

        for (index, rgb) in [
            (0, 0, 0),
            (128, 0, 0),
            (0, 128, 0),
            (128, 128, 0),
            (0, 0, 128),
            (128, 0, 128),
            (0, 128, 128),
            (192, 192, 192),
            (128, 128, 128),
            (255, 0, 0),
            (0, 255, 0),
            (255, 255, 0),
            (0, 0, 255),
            (255, 0, 255),
            (0, 255, 255),
            (255, 255, 255),
        ]
        .iter()
        .copied()
        .enumerate()
        {
            consider(index as u8, rgb);
        }

        const CUBE: [u8; 6] = [0, 95, 135, 175, 215, 255];
        for (r_index, r) in CUBE.iter().copied().enumerate() {
            for (g_index, g) in CUBE.iter().copied().enumerate() {
                for (b_index, b) in CUBE.iter().copied().enumerate() {
                    let index = 16 + (36 * r_index) + (6 * g_index) + b_index;
                    consider(index as u8, (r, g, b));
                }
            }
        }

        for gray_index in 0..24 {
            let value = 8 + (gray_index * 10);
            consider((232 + gray_index) as u8, (value, value, value));
        }

        best_index
    }


    fn hue_degrees(color: Color) -> u16 {
        let (r, g, b) = rgb_components(color);
        let r = f32::from(r) / 255.0;
        let g = f32::from(g) / 255.0;
        let b = f32::from(b) / 255.0;
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let delta = max - min;
        if delta == 0.0 {
            return 0;
        }

        let hue = if max == r {
            60.0 * ((g - b) / delta).rem_euclid(6.0)
        } else if max == g {
            60.0 * (((b - r) / delta) + 2.0)
        } else {
            60.0 * (((r - g) / delta) + 4.0)
        };
        hue.round() as u16
    }

    fn assert_hue_family(slug: &str, role: &str, color: Color, ranges: &[(u16, u16)]) {
        let hue = hue_degrees(color);
        assert!(
            ranges.iter().any(|(start, end)| (*start..=*end).contains(&hue)),
            "{slug} {role} semantic accent has hue {hue}, outside expected ranges {ranges:?}"
        );
    }

    #[test]
    fn all_required_theme_slugs_and_display_names_match_the_product_contract() {
        let expected = [
            ("tokyo-night", "Tokyo Night"),
            ("gruvbox", "Gruvbox material"),
            ("catppuccin", "Catppuccin Mocha"),
            ("rose-pine", "Rosé Pine"),
            ("kanagawa", "Kanagawa"),
            ("everforest", "Everforest"),
            ("dracula", "Dracula"),
            ("nord", "Nord"),
            ("solarized-dark", "Solarized Dark"),
            ("one-dark", "One Dark"),
            ("monokai-pro", "Monokai Pro"),
            ("oxocarbon", "Oxocarbon"),
            ("tokyo-night-day", "Tokyo Night Day"),
            ("gruvbox-light", "Gruvbox light"),
            ("catppuccin-latte", "Catppuccin Latte"),
            ("rose-pine-dawn", "Rosé Pine Dawn"),
            ("kanagawa-lotus", "Kanagawa Lotus"),
            ("everforest-light", "Everforest light"),
            ("alucard", "Alucard"),
            ("nord-light", "Nord Light"),
            ("solarized-light", "Solarized Light"),
            ("one-light", "One Light"),
            ("monokai-pro-light", "Monokai Pro Light"),
            ("oxocarbon-light", "Oxocarbon Light"),
        ];

        let actual: Vec<(&'static str, &'static str)> = PALETTES
            .iter()
            .map(|palette| (palette.slug, palette.name))
            .collect();
        assert_eq!(actual, expected);

        for (slug, name) in expected {
            let palette = palette_by_slug(slug).expect("required theme slug must exist");
            assert_eq!(palette.name, name, "display name drift for {slug}");
            let theme = theme_by_slug(slug).expect("required theme must resolve");
            assert_eq!(theme.slug, slug);
            assert_eq!(theme.name, name);
        }
    }

    #[test]
    fn built_in_palette_values_match_the_theming_brief() {
        let expected = [
            ("tokyo-night", true, ["#1a1b26", "#3b4261", "#7aa2f7", "#7aa2f7", "#565f89", "#bb9af7", "#7f88b3", "#c0caf5", "#33467c", "#9ece6a", "#bb9af7"], ["#f7768e", "#ff007c", "#ff9e64", "#e0af68", "#9ece6a", "#73daca", "#41a6b5", "#7dcfff", "#7aa2f7", "#3d59a1", "#9d7cd8", "#bb9af7", "#e0af68", "#bb9af7", "#73daca", "#9ece6a"]),
            ("gruvbox", true, ["#282828", "#504945", "#d8a657", "#e78a4e", "#7c6f64", "#d8a657", "#a89984", "#ebdbb2", "#665c54", "#a9b665", "#d3869b"], ["#ea6962", "#e78a4e", "#d8a657", "#a9b665", "#89b482", "#7daea3", "#d3869b", "#fb4934", "#fe8019", "#fabd2f", "#b8bb26", "#83a598", "#d8a657", "#d3869b", "#7daea3", "#a9b665"]),
            ("catppuccin", true, ["#1e1e2e", "#45475a", "#b4befe", "#89b4fa", "#6c7086", "#cba6f7", "#9399b2", "#cdd6f4", "#585b70", "#a6e3a1", "#cba6f7"], ["#f5e0dc", "#f5c2e7", "#cba6f7", "#f38ba8", "#fab387", "#f9e2af", "#a6e3a1", "#94e2d5", "#89dceb", "#74c7ec", "#89b4fa", "#b4befe", "#fab387", "#cba6f7", "#89dceb", "#a6e3a1"]),
            ("rose-pine", true, ["#191724", "#403d52", "#c4a7e7", "#c4a7e7", "#6e6a86", "#f6c177", "#908caa", "#e0def4", "#524f67", "#9ccfd8", "#eb6f92"], ["#eb6f92", "#f6c177", "#ebbcba", "#31748f", "#9ccfd8", "#c4a7e7", "#e0def4", "#908caa", "#6e6a86", "#403d52", "#524f67", "#26233a", "#f6c177", "#c4a7e7", "#9ccfd8", "#8fb573"]),
            ("kanagawa", true, ["#1f1f28", "#54546d", "#e6c384", "#7e9cd8", "#727169", "#d27e99", "#727169", "#dcd7ba", "#2d4f67", "#98bb6c", "#e46876"], ["#e46876", "#ff5d62", "#ffa066", "#e6c384", "#dca561", "#98bb6c", "#7aa89f", "#658594", "#7fb4ca", "#7e9cd8", "#957fb8", "#d27e99", "#e6c384", "#957fb8", "#7fb4ca", "#98bb6c"]),
            ("everforest", true, ["#2d353b", "#4f5b58", "#83c092", "#7fbbb3", "#859289", "#dbbc7f", "#859289", "#d3c6aa", "#543a48", "#a7c080", "#e67e80"], ["#e67e80", "#e69875", "#dbbc7f", "#a7c080", "#83c092", "#7fbbb3", "#d699b6", "#d3c6aa", "#859289", "#4f5b58", "#3d484d", "#343f44", "#dbbc7f", "#d699b6", "#7fbbb3", "#a7c080"]),
            ("dracula", true, ["#282a36", "#44475a", "#bd93f9", "#bd93f9", "#6272a4", "#ff79c6", "#6272a4", "#f8f8f2", "#4d4f68", "#50fa7b", "#ff79c6"], ["#ff5555", "#ffb86c", "#f1fa8c", "#50fa7b", "#8be9fd", "#ff79c6", "#bd93f9", "#6272a4", "#f8f8f2", "#44475a", "#343746", "#282a36", "#ffb86c", "#bd93f9", "#8be9fd", "#50fa7b"]),
            ("nord", true, ["#2e3440", "#3b4252", "#88c0d0", "#81a1c1", "#4c566a", "#b48ead", "#7a869c", "#d8dee9", "#434c5e", "#a3be8c", "#b48ead"], ["#bf616a", "#d08770", "#ebcb8b", "#a3be8c", "#8fbcbb", "#88c0d0", "#81a1c1", "#5e81ac", "#b48ead", "#d8dee9", "#4c566a", "#3b4252", "#ebcb8b", "#b48ead", "#88c0d0", "#a3be8c"]),
            ("solarized-dark", true, ["#002b36", "#234d56", "#2aa198", "#268bd2", "#586e75", "#6c71c4", "#657b83", "#93a1a1", "#174b55", "#859900", "#d33682"], ["#b58900", "#cb4b16", "#dc322f", "#d33682", "#6c71c4", "#268bd2", "#2aa198", "#859900", "#839496", "#93a1a1", "#586e75", "#073642", "#b58900", "#6c71c4", "#2aa198", "#859900"]),
            ("one-dark", true, ["#282c34", "#3e4451", "#56b6c2", "#61afef", "#5c6370", "#c678dd", "#7f8693", "#abb2bf", "#4b5263", "#98c379", "#e06c75"], ["#e06c75", "#d19a66", "#e5c07b", "#98c379", "#56b6c2", "#61afef", "#c678dd", "#be5046", "#abb2bf", "#5c6370", "#3e4451", "#21252b", "#e5c07b", "#c678dd", "#56b6c2", "#98c379"]),
            ("monokai-pro", true, ["#2d2a2e", "#5b595c", "#78dce8", "#ff6188", "#727072", "#ffd866", "#939293", "#fcfcfa", "#49474a", "#a9dc76", "#ab9df2"], ["#ff6188", "#fc9867", "#ffd866", "#a9dc76", "#78dce8", "#ab9df2", "#fcfcfa", "#c1c0c0", "#939293", "#727072", "#403e41", "#2d2a2e", "#ffd866", "#ab9df2", "#78dce8", "#a9dc76"]),
            ("oxocarbon", true, ["#161616", "#393939", "#be95ff", "#33b1ff", "#525252", "#ff7eb6", "#8d8d8d", "#f2f4f8", "#525252", "#42be65", "#ee5396"], ["#08bdba", "#3ddbd9", "#33b1ff", "#78a9ff", "#42be65", "#ee5396", "#ff7eb6", "#be95ff", "#82cfff", "#f2f4f8", "#525252", "#262626", "#f1c21b", "#be95ff", "#33b1ff", "#42be65"]),
            ("tokyo-night-day", false, ["#e1e2e7", "#c4c8da", "#2e7de9", "#2e7de9", "#848cb5", "#7847bd", "#6a72a0", "#3760bf", "#c4cae3", "#587539", "#bb1f70"], ["#f52a65", "#bb1f70", "#b15c00", "#8c6c3e", "#587539", "#118c74", "#387068", "#007197", "#2e7de9", "#2e5857", "#7847bd", "#9854f1", "#8c6c3e", "#7847bd", "#007197", "#587539"]),
            ("gruvbox-light", false, ["#fbf1c7", "#d5c4a1", "#b57614", "#af3a03", "#928374", "#8f3f71", "#7c6f64", "#3c3836", "#ebdbb2", "#79740e", "#9d0006"], ["#9d0006", "#cc241d", "#af3a03", "#b57614", "#79740e", "#98971a", "#427b58", "#689d6a", "#076678", "#458588", "#8f3f71", "#b16286", "#b57614", "#8f3f71", "#076678", "#79740e"]),
            ("catppuccin-latte", false, ["#eff1f5", "#bcc0cc", "#7287fd", "#1e66f5", "#8c8fa1", "#8839ef", "#6c6f85", "#4c4f69", "#ccd0da", "#40a02b", "#8839ef"], ["#dc8a78", "#ea76cb", "#8839ef", "#d20f39", "#fe640b", "#df8e1d", "#40a02b", "#179299", "#04a5e5", "#209fb5", "#1e66f5", "#7287fd", "#fe640b", "#8839ef", "#04a5e5", "#40a02b"]),
            ("rose-pine-dawn", false, ["#faf4ed", "#dfdad9", "#907aa9", "#907aa9", "#9893a5", "#ea9d34", "#797593", "#575279", "#dfdad9", "#56949f", "#b4637a"], ["#b4637a", "#ea9d34", "#d7827e", "#286983", "#56949f", "#907aa9", "#575279", "#797593", "#9893a5", "#dfdad9", "#cecacd", "#f2e9e1", "#ea9d34", "#907aa9", "#286983", "#569f76"]),
            ("kanagawa-lotus", false, ["#f2ecbc", "#d5cea3", "#836f4a", "#4d699b", "#8a8980", "#b35b79", "#716e61", "#545464", "#dcd5ac", "#6f894e", "#c84053"], ["#c84053", "#cc6d00", "#836f4a", "#6f894e", "#5e857a", "#4e8ca2", "#4d699b", "#5d57a3", "#624c83", "#766b90", "#b35b79", "#e82424", "#cc6d00", "#624c83", "#4e8ca2", "#6f894e"]),
            ("everforest-light", false, ["#fdf6e3", "#e0dcc7", "#35a77c", "#3a94c5", "#939f91", "#dfa000", "#829181", "#5c6a72", "#fbe3da", "#8da101", "#f85552"], ["#f85552", "#f57d26", "#dfa000", "#8da101", "#35a77c", "#3a94c5", "#df69ba", "#5c6a72", "#939f91", "#e0dcc7", "#efebd4", "#fdf6e3", "#dfa000", "#df69ba", "#3a94c5", "#8da101"]),
            ("alucard", false, ["#fffbeb", "#ddd6b8", "#644ac9", "#644ac9", "#8a845f", "#a3144d", "#6c664b", "#1f1f1f", "#ddd6b8", "#14710a", "#a3144d"], ["#cb3a2a", "#a34d14", "#846e15", "#14710a", "#036a96", "#a3144d", "#644ac9", "#6c664b", "#1f1f1f", "#cfcfde", "#f4eed2", "#fffbeb", "#846e15", "#644ac9", "#036a96", "#14710a"]),
            ("nord-light", false, ["#eceff4", "#d8dee9", "#34708a", "#4c6f9c", "#9aa3b3", "#8a5d85", "#60708a", "#2e3440", "#d8dee9", "#5b7a50", "#8a5d85"], ["#a54f58", "#ba6a47", "#94762f", "#5b7a50", "#357b78", "#34708a", "#4c6f9c", "#3b5a82", "#8a5d85", "#2e3440", "#6a7585", "#d8dee9", "#94762f", "#8a5d85", "#34708a", "#5b7a50"]),
            ("solarized-light", false, ["#fdf6e3", "#ded8c0", "#2aa198", "#268bd2", "#93a1a1", "#6c71c4", "#657b83", "#586e75", "#ded8c0", "#859900", "#d33682"], ["#b58900", "#cb4b16", "#dc322f", "#d33682", "#6c71c4", "#268bd2", "#2aa198", "#859900", "#657b83", "#586e75", "#93a1a1", "#eee8d5", "#b58900", "#6c71c4", "#268bd2", "#859900"]),
            ("one-light", false, ["#fafafa", "#d3d3d6", "#0184bc", "#4078f2", "#a0a1a7", "#a626a4", "#696c77", "#383a42", "#d3d3d6", "#50a14f", "#e45649"], ["#e45649", "#ca1243", "#c18401", "#986801", "#50a14f", "#0184bc", "#4078f2", "#a626a4", "#383a42", "#696c77", "#a0a1a7", "#eaeaeb", "#c18401", "#a626a4", "#0184bc", "#50a14f"]),
            ("monokai-pro-light", false, ["#faf4ec", "#e3dcd0", "#2f8a9c", "#d4275a", "#9a948c", "#a07b16", "#6f6a66", "#2d2a2e", "#e3dcd0", "#6a9c2f", "#6d57c9"], ["#d4275a", "#c2622a", "#a07b16", "#6a9c2f", "#2f8a9c", "#6d57c9", "#2d2a2e", "#6f6a66", "#9a948c", "#e3dcd0", "#efe7da", "#faf4ec", "#a07b16", "#6d57c9", "#2f8a9c", "#6a9c2f"]),
            ("oxocarbon-light", false, ["#f2f4f8", "#dde1e6", "#8a3ffc", "#0f62fe", "#a8a8a8", "#d12771", "#525252", "#161616", "#dde1e6", "#198038", "#d12771"], ["#da1e28", "#ba4e00", "#b28600", "#198038", "#007d79", "#1192e8", "#0f62fe", "#8a3ffc", "#d12771", "#ee5396", "#161616", "#525252", "#b28600", "#8a3ffc", "#0f62fe", "#198038"]),
        ];

        assert_eq!(PALETTES.len(), expected.len());
        for (index, (slug, dark, roles, accents)) in expected.iter().enumerate() {
            let palette = &PALETTES[index];
            assert_eq!(palette.slug, *slug, "palette order drifted at index {index}");
            assert_eq!(palette.dark, *dark, "dark flag mismatch for {slug}");
            assert_eq!(palette_roles(palette), (*roles).map(hex_color), "role mismatch for {slug}");
            assert_eq!(palette.accents, (*accents).map(hex_color), "accent mismatch for {slug}");
        }
    }


    #[test]
    fn semantic_accent_slots_drive_convert_pane_border_roles() {
        for palette in PALETTES {
            assert_eq!(palette.accents.len(), THEME_ACCENT_COUNT, "accent count for {}", palette.slug);
            let theme = Theme::from_palette(palette);
            assert_eq!(theme.accents, palette.accents, "accent passthrough for {}", palette.slug);
            assert_eq!(theme.amber, palette.accents[WARM_ACCENT], "source pane accent for {}", palette.slug);
            assert_eq!(theme.purple, palette.accents[COOL_ACCENT], "metadata pane accent for {}", palette.slug);
            assert_eq!(theme.cyan, palette.accents[INFO_ACCENT], "output-options pane accent for {}", palette.slug);
            assert_eq!(theme.green, palette.accents[SUCCESS_ACCENT], "format pane accent for {}", palette.slug);
            assert_eq!(theme.warning, theme.amber, "warning alias for {}", palette.slug);
            assert_eq!(theme.info, theme.cyan, "info alias for {}", palette.slug);
            assert_eq!(theme.success, theme.green, "success alias for {}", palette.slug);
        }
    }

    #[test]
    fn semantic_accent_slots_have_contractual_hue_families() {
        for palette in PALETTES {
            assert_hue_family(
                palette.slug,
                "warm/source",
                palette.accents[WARM_ACCENT],
                &[(15, 60)],
            );
            assert_hue_family(
                palette.slug,
                "cool/metadata",
                palette.accents[COOL_ACCENT],
                &[(230, 359)],
            );
            assert_hue_family(
                palette.slug,
                "info/output-options",
                palette.accents[INFO_ACCENT],
                &[(165, 230)],
            );
            assert_hue_family(
                palette.slug,
                "success/format",
                palette.accents[SUCCESS_ACCENT],
                &[(55, 160)],
            );
        }
    }

    #[test]
    fn convert_pane_border_accents_are_pairwise_distinct() {
        for palette in PALETTES {
            let theme = Theme::from_palette(palette);
            let pane_accents = [theme.amber, theme.purple, theme.cyan, theme.green];
            for i in 0..pane_accents.len() {
                for j in (i + 1)..pane_accents.len() {
                    assert_ne!(
                        pane_accents[i], pane_accents[j],
                        "convert pane accent slots {i} and {j} must differ for {}",
                        palette.slug
                    );
                }
            }
        }
    }

    #[test]
    fn convert_pane_border_accents_remain_distinct_in_xterm_256() {
        for palette in PALETTES {
            let theme = Theme::from_palette(palette);
            let pane_accents = [theme.amber, theme.purple, theme.cyan, theme.green];
            let xterm_indices = pane_accents.map(nearest_xterm_256_index);
            for i in 0..xterm_indices.len() {
                for j in (i + 1)..xterm_indices.len() {
                    assert_ne!(
                        xterm_indices[i], xterm_indices[j],
                        "convert pane accent slots {i} and {j} collapse to xterm-256 color {} for {}",
                        xterm_indices[i],
                        palette.slug
                    );
                }
            }
        }
    }

    #[test]
    fn tokyo_night_restores_warm_source_border_instead_of_header_purple() {
        let palette = palette_by_slug("tokyo-night").expect("tokyo-night palette");
        let theme = Theme::from_palette(palette);
        assert_eq!(theme.amber, hex_color("#e0af68"));
        assert_eq!(theme.purple, hex_color("#bb9af7"));
        assert_eq!(theme.cyan, hex_color("#73daca"));
        assert_eq!(theme.green, hex_color("#9ece6a"));
        assert_ne!(theme.amber, palette.header);
        assert_ne!(theme.amber, theme.purple);
    }

    #[test]
    fn oxocarbon_uses_carbon_yellow_as_warm_source_border() {
        let palette = palette_by_slug("oxocarbon").expect("oxocarbon palette");
        let theme = Theme::from_palette(palette);
        assert_eq!(theme.amber, hex_color("#f1c21b"));
        assert_eq!(theme.purple, hex_color("#be95ff"));
        assert_eq!(theme.cyan, hex_color("#33b1ff"));
        assert_eq!(theme.green, hex_color("#42be65"));
        assert_ne!(theme.amber, theme.purple);
        assert_hue_family(palette.slug, "warm/source", theme.amber, &[(15, 60)]);
    }

    #[test]
    fn theme_cycling_order_is_stable_and_wraps() {
        let slugs: Vec<&'static str> = PALETTES.iter().map(|palette| palette.slug).collect();
        assert_eq!(slugs, vec![
            "tokyo-night", "gruvbox", "catppuccin", "rose-pine", "kanagawa", "everforest",
            "dracula", "nord", "solarized-dark", "one-dark", "monokai-pro", "oxocarbon",
            "tokyo-night-day", "gruvbox-light", "catppuccin-latte", "rose-pine-dawn",
            "kanagawa-lotus", "everforest-light",
            "alucard", "nord-light", "solarized-light", "one-light", "monokai-pro-light", "oxocarbon-light",
        ]);
        for pair in slugs.windows(2) {
            assert_eq!(next_palette_slug(pair[0]), pair[1]);
            assert_eq!(previous_palette_slug(pair[1]), pair[0]);
        }
        let last = slugs.last().unwrap();
        let first = slugs.first().unwrap();
        assert_eq!(next_palette_slug(last), *first);
        assert_eq!(previous_palette_slug(first), *last);
        assert_eq!(next_palette_slug("missing-theme"), "gruvbox");
        assert_eq!(previous_palette_slug("missing-theme"), *last);
    }


    #[test]
    fn light_theme_chip_text_uses_panel_background_for_contrast() {
        for palette in PALETTES.iter().filter(|palette| !palette.dark) {
            let theme = Theme::from_palette(palette);
            assert_eq!(theme.progress_dialog_button_fg, palette.panel_bg, "go chip fg for {}", palette.slug);
            assert_eq!(theme.progress_dialog_abort_fg, palette.panel_bg, "dismiss chip fg for {}", palette.slug);
            assert_eq!(theme.pill_active_fg, palette.panel_bg, "active pill fg for {}", palette.slug);
        }
    }

    #[test]
    fn every_dark_palette_distinguishes_selection_from_border() {
        for palette in PALETTES.iter().filter(|palette| palette.dark) {
            assert_ne!(
                palette.selection_bg, palette.border,
                "dark palette {} must not reuse its border color for row selection",
                palette.slug
            );
        }
    }

    #[test]
    fn tokyo_night_default_matches_the_intended_design_target() {
        let palette = default_palette();
        assert_eq!(palette.slug, "tokyo-night");
        assert_eq!(palette.panel_bg, hex_color("#1a1b26"));
        assert_eq!(palette.border, hex_color("#3b4261"));
        assert_eq!(palette.tab_active, hex_color("#7aa2f7"));
        assert_eq!(palette.selection_bg, hex_color("#33467c"));
        let theme = Theme::from_palette(palette);
        assert_eq!(theme.bg, hex_color("#1a1b26"));
        assert_eq!(theme.blue, hex_color("#7aa2f7"));
        assert_eq!(theme.amber, hex_color("#e0af68"));
        assert_eq!(theme.purple, hex_color("#bb9af7"));
        assert_eq!(theme.cyan, hex_color("#73daca"));
        assert_eq!(theme.green, hex_color("#9ece6a"));
        assert_eq!(theme.warning, theme.amber);
        assert_eq!(theme.info, theme.cyan);
        assert_eq!(theme.success, theme.green);
        assert_eq!(theme.red, hex_color("#f7768e"));
        assert_eq!(theme.error, hex_color("#f7768e"));
        assert_eq!(theme.destructive, hex_color("#f7768e"));
        assert_ne!(theme.error_dim, theme.error);
        assert_eq!(theme.error_dim, mix(theme.bg, theme.error, 1, 2));
        assert_eq!(theme.dismiss, hex_color("#bb9af7"));
        assert_eq!(theme.chip_dismiss, hex_color("#bb9af7"));
        assert_eq!(theme.title, hex_color("#7aa2f7"));
        assert_eq!(theme.tab_inactive, hex_color("#565f89"));
    }

    #[test]
    fn runtime_theme_preserves_role_tokens_and_separates_error_from_dismiss() {
        let palette = palette_by_slug("tokyo-night").expect("tokyo-night palette");
        let theme = Theme::from_palette(palette);
        assert_eq!(theme.panel_bg, palette.panel_bg);
        assert_eq!(theme.border, palette.border);
        assert_eq!(theme.title, palette.title);
        assert_eq!(theme.tab_active, palette.tab_active);
        assert_eq!(theme.tab_inactive, palette.tab_inactive);
        assert_eq!(theme.header, palette.header);
        assert_eq!(theme.label, palette.label);
        assert_eq!(theme.value, palette.value);
        assert_eq!(theme.selection_bg, palette.selection_bg);
        assert_eq!(theme.chip_go, palette.chip_go);
        assert_eq!(theme.chip_dismiss, palette.chip_dismiss);
        assert_eq!(theme.accents, palette.accents);
        assert_eq!(theme.error, hex_color("#f7768e"));
        assert_eq!(theme.destructive, hex_color("#f7768e"));
        assert_ne!(theme.error_dim, theme.error);
        assert_eq!(theme.error_dim, mix(theme.bg, theme.error, 1, 2));
        assert_eq!(theme.red, theme.error);
        assert_eq!(theme.dismiss, hex_color("#bb9af7"));
        assert_eq!(theme.dismiss, palette.chip_dismiss);
        assert_ne!(theme.error, theme.dismiss);
    }


    #[test]
    fn title_and_tab_inactive_roles_are_reachable_and_used_by_renderers() {
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let theme = theme_by_slug("tokyo-night").expect("theme");
        assert_eq!(theme.title, hex_color("#7aa2f7"));
        assert_eq!(theme.tab_inactive, hex_color("#565f89"));
        let header = std::fs::read_to_string(manifest.join("src/tui/draw_header.rs")).expect("header source");
        let footer = std::fs::read_to_string(manifest.join("src/tui/draw_footer.rs")).expect("footer source");
        assert!(header.contains("theme.title"), "header renderer should use the title role");
        assert!(footer.contains("theme.tab_inactive"), "footer tab renderer should use the inactive-tab role");
    }

    #[test]
    fn renderer_paths_do_not_consult_ambient_theme_state() {
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let render_paths = [
            "src/tui/draw.rs",
            "src/tui/draw_browse.rs",
            "src/tui/draw_footer.rs",
            "src/tui/draw_header.rs",
            "src/tui/draw_metadata.rs",
            "src/tui/draw_output.rs",
            "src/tui/draw_output_options.rs",
            "src/tui/draw_overlays.rs",
            "src/tui/draw_preset_bar.rs",
            "src/tui/draw_queue.rs",
            "src/tui/draw_source.rs",
            "src/tui/draw_status.rs",
            "src/tui/convert_screen.rs",
            "src/tui/disc_browser.rs",
            "src/tui/help.rs",
            "src/tui/pill.rs",
            "src/tui/bookmarks_overlay.rs",
            "src/tui/recent_overlay.rs",
            "src/tui/presets_overlay.rs",
            "src/tui/template_builder.rs",
        ];
        let forbidden = [
            concat!("theme", "::", "active"),
            concat!("theme", "::", "frame"),
            concat!("super", "::", "theme", "::", "active"),
            concat!("super", "::", "theme", "::", "frame"),
            concat!("bind", "_", "theme"),
            concat!("bind", "_", "frame", "_", "theme"),
            concat!("Frame", "Theme", "Guard"),
            concat!("FRAME", "_", "THEME"),
        ];
        for rel in render_paths {
            let text = std::fs::read_to_string(manifest.join(rel)).expect(rel);
            for token in forbidden {
                assert!(
                    !text.contains(token),
                    "forbidden ambient theme token `{token}` found in renderer path {rel}; pass Theme explicitly"
                );
            }
        }
    }

    #[test]
    fn theme_module_does_not_define_ambient_theme_shims() {
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let rel = "src/tui/theme.rs";
        let text = std::fs::read_to_string(manifest.join(rel)).expect(rel);
        let forbidden = [
            concat!("thread", "_local!"),
            concat!("FRAME", "_THEME"),
            concat!("Frame", "ThemeGuard"),
            concat!("bind", "_theme"),
            concat!("bind", "_frame", "_theme"),
            concat!("pub fn ", "frame", "("),
            concat!("fn ", "frame", "("),
            concat!("frame", "().muted"),
            concat!("frame", "().bright"),
            concat!("frame", "().text_style"),
            concat!("frame", "().accent"),
            concat!("frame", "().border"),
            concat!("frame", "().bold"),
            concat!("pub fn ", "active", "("),
            concat!("fn ", "active", "("),
        ];
        for token in forbidden {
            assert!(
                !text.contains(token),
                "forbidden ambient theme token `{token}` found in {rel}; use explicit Theme methods"
            );
        }
    }

}
