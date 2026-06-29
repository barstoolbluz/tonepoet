//! Runtime-selectable TUI theme system.
//!
//! `AppState::theme` is the runtime source of truth. Rendering snapshots that
//! value once per frame and passes it explicitly into render helpers. `Theme` is
//! `Copy` and contains only scalar colors plus static strings, so passing it by
//! value is intentional and cheap.

use ratatui::style::{Color, Modifier, Style};

const DEFAULT_THEME_SLUG: &str = "tokyo-night";

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
    pub accents: [Color; 12],
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
    pub accents: [Color; 12],
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
    pub fn from_palette(palette: &'static ThemePalette) -> Self {
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
            amber: palette.header,
            green: palette.chip_go,
            purple: palette.accents[10],
            cyan: palette.accents[5],
            red: palette.accents[0],
            error: palette.accents[0],
            destructive: palette.accents[0],
            error_dim,
            warning: palette.header,
            success: palette.chip_go,
            info: palette.tab_active,
            dismiss: palette.chip_dismiss,
            progress_dialog_bg: surface,
            progress_dialog_text: text_bright,
            progress_dialog_border: palette.tab_active,
            progress_dialog_title: text_bright,
            progress_dialog_label: palette.label,
            progress_dialog_current_file: palette.tab_active,
            progress_dialog_dim: text_dim,
            progress_dialog_bar_filled: palette.tab_active,
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
    palette!("tokyo-night", "Tokyo Night", "Balanced dark blue Tokyo Night palette", true, rgb(0x1a,0x1b,0x26), rgb(0x3b,0x42,0x61), rgb(0x7a,0xa2,0xf7), rgb(0x7a,0xa2,0xf7), rgb(0x56,0x5f,0x89), rgb(0xbb,0x9a,0xf7), rgb(0x7f,0x88,0xb3), rgb(0xc0,0xca,0xf5), rgb(0x3b,0x42,0x61), rgb(0x9e,0xce,0x6a), rgb(0xbb,0x9a,0xf7), [rgb(0xf7,0x76,0x8e), rgb(0xff,0x00,0x7c), rgb(0xff,0x9e,0x64), rgb(0xe0,0xaf,0x68), rgb(0x9e,0xce,0x6a), rgb(0x73,0xda,0xca), rgb(0x41,0xa6,0xb5), rgb(0x7d,0xcf,0xff), rgb(0x7a,0xa2,0xf7), rgb(0x3d,0x59,0xa1), rgb(0x9d,0x7c,0xd8), rgb(0xbb,0x9a,0xf7)]),
    palette!("gruvbox", "Gruvbox material", "Warm dark Gruvbox material palette", true, rgb(0x28,0x28,0x28), rgb(0x50,0x49,0x45), rgb(0xd8,0xa6,0x57), rgb(0xe7,0x8a,0x4e), rgb(0x7c,0x6f,0x64), rgb(0xd8,0xa6,0x57), rgb(0xa8,0x99,0x84), rgb(0xeb,0xdb,0xb2), rgb(0x50,0x49,0x45), rgb(0xa9,0xb6,0x65), rgb(0xd3,0x86,0x9b), [rgb(0xea,0x69,0x62), rgb(0xe7,0x8a,0x4e), rgb(0xd8,0xa6,0x57), rgb(0xa9,0xb6,0x65), rgb(0x89,0xb4,0x82), rgb(0x7d,0xae,0xa3), rgb(0xd3,0x86,0x9b), rgb(0xfb,0x49,0x34), rgb(0xfe,0x80,0x19), rgb(0xfa,0xbd,0x2f), rgb(0xb8,0xbb,0x26), rgb(0x83,0xa5,0x98)]),
    palette!("catppuccin", "Catppuccin Mocha", "Soft dark Catppuccin Mocha palette", true, rgb(0x1e,0x1e,0x2e), rgb(0x45,0x47,0x5a), rgb(0xb4,0xbe,0xfe), rgb(0x89,0xb4,0xfa), rgb(0x6c,0x70,0x86), rgb(0xcb,0xa6,0xf7), rgb(0x93,0x99,0xb2), rgb(0xcd,0xd6,0xf4), rgb(0x45,0x47,0x5a), rgb(0xa6,0xe3,0xa1), rgb(0xcb,0xa6,0xf7), [rgb(0xf5,0xe0,0xdc), rgb(0xf5,0xc2,0xe7), rgb(0xcb,0xa6,0xf7), rgb(0xf3,0x8b,0xa8), rgb(0xfa,0xb3,0x87), rgb(0xf9,0xe2,0xaf), rgb(0xa6,0xe3,0xa1), rgb(0x94,0xe2,0xd5), rgb(0x89,0xdc,0xeb), rgb(0x74,0xc7,0xec), rgb(0x89,0xb4,0xfa), rgb(0xb4,0xbe,0xfe)]),
    palette!("rose-pine", "Rosé Pine", "Low-contrast dark Rosé Pine palette", true, rgb(0x19,0x17,0x24), rgb(0x40,0x3d,0x52), rgb(0xc4,0xa7,0xe7), rgb(0xc4,0xa7,0xe7), rgb(0x6e,0x6a,0x86), rgb(0xf6,0xc1,0x77), rgb(0x90,0x8c,0xaa), rgb(0xe0,0xde,0xf4), rgb(0x40,0x3d,0x52), rgb(0x9c,0xcf,0xd8), rgb(0xeb,0x6f,0x92), [rgb(0xeb,0x6f,0x92), rgb(0xf6,0xc1,0x77), rgb(0xeb,0xbc,0xba), rgb(0x31,0x74,0x8f), rgb(0x9c,0xcf,0xd8), rgb(0xc4,0xa7,0xe7), rgb(0xe0,0xde,0xf4), rgb(0x90,0x8c,0xaa), rgb(0x6e,0x6a,0x86), rgb(0x40,0x3d,0x52), rgb(0x52,0x4f,0x67), rgb(0x26,0x23,0x3a)]),
    palette!("kanagawa", "Kanagawa", "Dark Kanagawa wave palette", true, rgb(0x1f,0x1f,0x28), rgb(0x54,0x54,0x6d), rgb(0xe6,0xc3,0x84), rgb(0x7e,0x9c,0xd8), rgb(0x72,0x71,0x69), rgb(0xd2,0x7e,0x99), rgb(0x72,0x71,0x69), rgb(0xdc,0xd7,0xba), rgb(0x2d,0x4f,0x67), rgb(0x98,0xbb,0x6c), rgb(0xe4,0x68,0x76), [rgb(0xe4,0x68,0x76), rgb(0xff,0x5d,0x62), rgb(0xff,0xa0,0x66), rgb(0xe6,0xc3,0x84), rgb(0xdc,0xa5,0x61), rgb(0x98,0xbb,0x6c), rgb(0x7a,0xa8,0x9f), rgb(0x65,0x85,0x94), rgb(0x7f,0xb4,0xca), rgb(0x7e,0x9c,0xd8), rgb(0x95,0x7f,0xb8), rgb(0xd2,0x7e,0x99)]),
    palette!("everforest", "Everforest", "Muted dark Everforest palette", true, rgb(0x2d,0x35,0x3b), rgb(0x4f,0x5b,0x58), rgb(0x83,0xc0,0x92), rgb(0x7f,0xbb,0xb3), rgb(0x85,0x92,0x89), rgb(0xdb,0xbc,0x7f), rgb(0x85,0x92,0x89), rgb(0xd3,0xc6,0xaa), rgb(0x54,0x3a,0x48), rgb(0xa7,0xc0,0x80), rgb(0xe6,0x7e,0x80), [rgb(0xe6,0x7e,0x80), rgb(0xe6,0x98,0x75), rgb(0xdb,0xbc,0x7f), rgb(0xa7,0xc0,0x80), rgb(0x83,0xc0,0x92), rgb(0x7f,0xbb,0xb3), rgb(0xd6,0x99,0xb6), rgb(0xd3,0xc6,0xaa), rgb(0x85,0x92,0x89), rgb(0x4f,0x5b,0x58), rgb(0x3d,0x48,0x4d), rgb(0x34,0x3f,0x44)]),
    palette!("tokyo-night-day", "Tokyo Night Day", "Light Tokyo Night palette", false, rgb(0xe1,0xe2,0xe7), rgb(0xc4,0xc8,0xda), rgb(0x2e,0x7d,0xe9), rgb(0x2e,0x7d,0xe9), rgb(0x84,0x8c,0xb5), rgb(0x78,0x47,0xbd), rgb(0x6a,0x72,0xa0), rgb(0x37,0x60,0xbf), rgb(0xc4,0xca,0xe3), rgb(0x58,0x75,0x39), rgb(0xbb,0x1f,0x70), [rgb(0xf5,0x2a,0x65), rgb(0xbb,0x1f,0x70), rgb(0xb1,0x5c,0x00), rgb(0x8c,0x6c,0x3e), rgb(0x58,0x75,0x39), rgb(0x11,0x8c,0x74), rgb(0x38,0x70,0x68), rgb(0x00,0x71,0x97), rgb(0x2e,0x7d,0xe9), rgb(0x2e,0x58,0x57), rgb(0x78,0x47,0xbd), rgb(0x98,0x54,0xf1)]),
    palette!("gruvbox-light", "Gruvbox light", "Warm light Gruvbox palette", false, rgb(0xfb,0xf1,0xc7), rgb(0xd5,0xc4,0xa1), rgb(0xb5,0x76,0x14), rgb(0xaf,0x3a,0x03), rgb(0x92,0x83,0x74), rgb(0x8f,0x3f,0x71), rgb(0x7c,0x6f,0x64), rgb(0x3c,0x38,0x36), rgb(0xeb,0xdb,0xb2), rgb(0x79,0x74,0x0e), rgb(0x9d,0x00,0x06), [rgb(0x9d,0x00,0x06), rgb(0xcc,0x24,0x1d), rgb(0xaf,0x3a,0x03), rgb(0xb5,0x76,0x14), rgb(0x79,0x74,0x0e), rgb(0x98,0x97,0x1a), rgb(0x42,0x7b,0x58), rgb(0x68,0x9d,0x6a), rgb(0x07,0x66,0x78), rgb(0x45,0x85,0x88), rgb(0x8f,0x3f,0x71), rgb(0xb1,0x62,0x86)]),
    palette!("catppuccin-latte", "Catppuccin Latte", "Soft light Catppuccin Latte palette", false, rgb(0xef,0xf1,0xf5), rgb(0xbc,0xc0,0xcc), rgb(0x72,0x87,0xfd), rgb(0x1e,0x66,0xf5), rgb(0x8c,0x8f,0xa1), rgb(0x88,0x39,0xef), rgb(0x6c,0x6f,0x85), rgb(0x4c,0x4f,0x69), rgb(0xcc,0xd0,0xda), rgb(0x40,0xa0,0x2b), rgb(0x88,0x39,0xef), [rgb(0xdc,0x8a,0x78), rgb(0xea,0x76,0xcb), rgb(0x88,0x39,0xef), rgb(0xd2,0x0f,0x39), rgb(0xfe,0x64,0x0b), rgb(0xdf,0x8e,0x1d), rgb(0x40,0xa0,0x2b), rgb(0x17,0x92,0x99), rgb(0x04,0xa5,0xe5), rgb(0x20,0x9f,0xb5), rgb(0x1e,0x66,0xf5), rgb(0x72,0x87,0xfd)]),
    palette!("rose-pine-dawn", "Rosé Pine Dawn", "Light Rosé Pine Dawn palette", false, rgb(0xfa,0xf4,0xed), rgb(0xdf,0xda,0xd9), rgb(0x90,0x7a,0xa9), rgb(0x90,0x7a,0xa9), rgb(0x98,0x93,0xa5), rgb(0xea,0x9d,0x34), rgb(0x79,0x75,0x93), rgb(0x57,0x52,0x79), rgb(0xdf,0xda,0xd9), rgb(0x56,0x94,0x9f), rgb(0xb4,0x63,0x7a), [rgb(0xb4,0x63,0x7a), rgb(0xea,0x9d,0x34), rgb(0xd7,0x82,0x7e), rgb(0x28,0x69,0x83), rgb(0x56,0x94,0x9f), rgb(0x90,0x7a,0xa9), rgb(0x57,0x52,0x79), rgb(0x79,0x75,0x93), rgb(0x98,0x93,0xa5), rgb(0xdf,0xda,0xd9), rgb(0xce,0xca,0xcd), rgb(0xf2,0xe9,0xe1)]),
    palette!("kanagawa-lotus", "Kanagawa Lotus", "Light Kanagawa Lotus palette", false, rgb(0xf2,0xec,0xbc), rgb(0xd5,0xce,0xa3), rgb(0x83,0x6f,0x4a), rgb(0x4d,0x69,0x9b), rgb(0x8a,0x89,0x80), rgb(0xb3,0x5b,0x79), rgb(0x71,0x6e,0x61), rgb(0x54,0x54,0x64), rgb(0xdc,0xd5,0xac), rgb(0x6f,0x89,0x4e), rgb(0xc8,0x40,0x53), [rgb(0xc8,0x40,0x53), rgb(0xcc,0x6d,0x00), rgb(0x83,0x6f,0x4a), rgb(0x6f,0x89,0x4e), rgb(0x5e,0x85,0x7a), rgb(0x4e,0x8c,0xa2), rgb(0x4d,0x69,0x9b), rgb(0x5d,0x57,0xa3), rgb(0x62,0x4c,0x83), rgb(0x76,0x6b,0x90), rgb(0xb3,0x5b,0x79), rgb(0xe8,0x24,0x24)]),
    palette!("everforest-light", "Everforest light", "Light Everforest palette", false, rgb(0xfd,0xf6,0xe3), rgb(0xe0,0xdc,0xc7), rgb(0x35,0xa7,0x7c), rgb(0x3a,0x94,0xc5), rgb(0x93,0x9f,0x91), rgb(0xdf,0xa0,0x00), rgb(0x82,0x91,0x81), rgb(0x5c,0x6a,0x72), rgb(0xfb,0xe3,0xda), rgb(0x8d,0xa1,0x01), rgb(0xf8,0x55,0x52), [rgb(0xf8,0x55,0x52), rgb(0xf5,0x7d,0x26), rgb(0xdf,0xa0,0x00), rgb(0x8d,0xa1,0x01), rgb(0x35,0xa7,0x7c), rgb(0x3a,0x94,0xc5), rgb(0xdf,0x69,0xba), rgb(0x5c,0x6a,0x72), rgb(0x93,0x9f,0x91), rgb(0xe0,0xdc,0xc7), rgb(0xef,0xeb,0xd4), rgb(0xfd,0xf6,0xe3)]),
];


pub fn palettes() -> &'static [ThemePalette] {
    PALETTES
}

pub fn palette_by_slug(slug: &str) -> Option<&'static ThemePalette> {
    let normalized = normalize_slug(slug);
    PALETTES.iter().find(|palette| palette.slug == normalized.as_str())
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

pub fn next_palette_slug(current: &str) -> &'static str {
    adjacent_palette_slug(current, 1)
}

pub fn previous_palette_slug(current: &str) -> &'static str {
    adjacent_palette_slug(current, PALETTES.len().saturating_sub(1))
}

fn adjacent_palette_slug(current: &str, offset: usize) -> &'static str {
    let normalized = normalize_slug(current);
    let idx = PALETTES
        .iter()
        .position(|palette| palette.slug == normalized.as_str())
        .unwrap_or(0);
    PALETTES[(idx + offset) % PALETTES.len()].slug
}

fn normalize_slug(slug: &str) -> String {
    slug.trim().to_ascii_lowercase().replace('_', "-")
}

#[cfg(test)]
mod tests {
    use super::*;

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


    #[test]
    fn all_required_theme_slugs_and_display_names_match_the_product_contract() {
        let expected = [
            ("tokyo-night", "Tokyo Night"),
            ("gruvbox", "Gruvbox material"),
            ("catppuccin", "Catppuccin Mocha"),
            ("rose-pine", "Rosé Pine"),
            ("kanagawa", "Kanagawa"),
            ("everforest", "Everforest"),
            ("tokyo-night-day", "Tokyo Night Day"),
            ("gruvbox-light", "Gruvbox light"),
            ("catppuccin-latte", "Catppuccin Latte"),
            ("rose-pine-dawn", "Rosé Pine Dawn"),
            ("kanagawa-lotus", "Kanagawa Lotus"),
            ("everforest-light", "Everforest light"),
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
            ("tokyo-night", true, ["#1a1b26", "#3b4261", "#7aa2f7", "#7aa2f7", "#565f89", "#bb9af7", "#7f88b3", "#c0caf5", "#3b4261", "#9ece6a", "#bb9af7"], ["#f7768e", "#ff007c", "#ff9e64", "#e0af68", "#9ece6a", "#73daca", "#41a6b5", "#7dcfff", "#7aa2f7", "#3d59a1", "#9d7cd8", "#bb9af7"]),
            ("gruvbox", true, ["#282828", "#504945", "#d8a657", "#e78a4e", "#7c6f64", "#d8a657", "#a89984", "#ebdbb2", "#504945", "#a9b665", "#d3869b"], ["#ea6962", "#e78a4e", "#d8a657", "#a9b665", "#89b482", "#7daea3", "#d3869b", "#fb4934", "#fe8019", "#fabd2f", "#b8bb26", "#83a598"]),
            ("catppuccin", true, ["#1e1e2e", "#45475a", "#b4befe", "#89b4fa", "#6c7086", "#cba6f7", "#9399b2", "#cdd6f4", "#45475a", "#a6e3a1", "#cba6f7"], ["#f5e0dc", "#f5c2e7", "#cba6f7", "#f38ba8", "#fab387", "#f9e2af", "#a6e3a1", "#94e2d5", "#89dceb", "#74c7ec", "#89b4fa", "#b4befe"]),
            ("rose-pine", true, ["#191724", "#403d52", "#c4a7e7", "#c4a7e7", "#6e6a86", "#f6c177", "#908caa", "#e0def4", "#403d52", "#9ccfd8", "#eb6f92"], ["#eb6f92", "#f6c177", "#ebbcba", "#31748f", "#9ccfd8", "#c4a7e7", "#e0def4", "#908caa", "#6e6a86", "#403d52", "#524f67", "#26233a"]),
            ("kanagawa", true, ["#1f1f28", "#54546d", "#e6c384", "#7e9cd8", "#727169", "#d27e99", "#727169", "#dcd7ba", "#2d4f67", "#98bb6c", "#e46876"], ["#e46876", "#ff5d62", "#ffa066", "#e6c384", "#dca561", "#98bb6c", "#7aa89f", "#658594", "#7fb4ca", "#7e9cd8", "#957fb8", "#d27e99"]),
            ("everforest", true, ["#2d353b", "#4f5b58", "#83c092", "#7fbbb3", "#859289", "#dbbc7f", "#859289", "#d3c6aa", "#543a48", "#a7c080", "#e67e80"], ["#e67e80", "#e69875", "#dbbc7f", "#a7c080", "#83c092", "#7fbbb3", "#d699b6", "#d3c6aa", "#859289", "#4f5b58", "#3d484d", "#343f44"]),
            ("tokyo-night-day", false, ["#e1e2e7", "#c4c8da", "#2e7de9", "#2e7de9", "#848cb5", "#7847bd", "#6a72a0", "#3760bf", "#c4cae3", "#587539", "#bb1f70"], ["#f52a65", "#bb1f70", "#b15c00", "#8c6c3e", "#587539", "#118c74", "#387068", "#007197", "#2e7de9", "#2e5857", "#7847bd", "#9854f1"]),
            ("gruvbox-light", false, ["#fbf1c7", "#d5c4a1", "#b57614", "#af3a03", "#928374", "#8f3f71", "#7c6f64", "#3c3836", "#ebdbb2", "#79740e", "#9d0006"], ["#9d0006", "#cc241d", "#af3a03", "#b57614", "#79740e", "#98971a", "#427b58", "#689d6a", "#076678", "#458588", "#8f3f71", "#b16286"]),
            ("catppuccin-latte", false, ["#eff1f5", "#bcc0cc", "#7287fd", "#1e66f5", "#8c8fa1", "#8839ef", "#6c6f85", "#4c4f69", "#ccd0da", "#40a02b", "#8839ef"], ["#dc8a78", "#ea76cb", "#8839ef", "#d20f39", "#fe640b", "#df8e1d", "#40a02b", "#179299", "#04a5e5", "#209fb5", "#1e66f5", "#7287fd"]),
            ("rose-pine-dawn", false, ["#faf4ed", "#dfdad9", "#907aa9", "#907aa9", "#9893a5", "#ea9d34", "#797593", "#575279", "#dfdad9", "#56949f", "#b4637a"], ["#b4637a", "#ea9d34", "#d7827e", "#286983", "#56949f", "#907aa9", "#575279", "#797593", "#9893a5", "#dfdad9", "#cecacd", "#f2e9e1"]),
            ("kanagawa-lotus", false, ["#f2ecbc", "#d5cea3", "#836f4a", "#4d699b", "#8a8980", "#b35b79", "#716e61", "#545464", "#dcd5ac", "#6f894e", "#c84053"], ["#c84053", "#cc6d00", "#836f4a", "#6f894e", "#5e857a", "#4e8ca2", "#4d699b", "#5d57a3", "#624c83", "#766b90", "#b35b79", "#e82424"]),
            ("everforest-light", false, ["#fdf6e3", "#e0dcc7", "#35a77c", "#3a94c5", "#939f91", "#dfa000", "#829181", "#5c6a72", "#fbe3da", "#8da101", "#f85552"], ["#f85552", "#f57d26", "#dfa000", "#8da101", "#35a77c", "#3a94c5", "#df69ba", "#5c6a72", "#939f91", "#e0dcc7", "#efebd4", "#fdf6e3"]),
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
    fn theme_cycling_order_is_stable_and_wraps() {
        let slugs: Vec<&'static str> = PALETTES.iter().map(|palette| palette.slug).collect();
        assert_eq!(slugs, vec![
            "tokyo-night", "gruvbox", "catppuccin", "rose-pine", "kanagawa", "everforest",
            "tokyo-night-day", "gruvbox-light", "catppuccin-latte", "rose-pine-dawn",
            "kanagawa-lotus", "everforest-light",
        ]);
        for pair in slugs.windows(2) {
            assert_eq!(next_palette_slug(pair[0]), pair[1]);
            assert_eq!(previous_palette_slug(pair[1]), pair[0]);
        }
        assert_eq!(next_palette_slug("everforest-light"), "tokyo-night");
        assert_eq!(previous_palette_slug("tokyo-night"), "everforest-light");
        assert_eq!(next_palette_slug("missing-theme"), "gruvbox");
        assert_eq!(previous_palette_slug("missing-theme"), "everforest-light");
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
    fn tokyo_night_default_matches_the_intended_design_target() {
        let palette = default_palette();
        assert_eq!(palette.slug, "tokyo-night");
        assert_eq!(palette.panel_bg, hex_color("#1a1b26"));
        assert_eq!(palette.border, hex_color("#3b4261"));
        assert_eq!(palette.tab_active, hex_color("#7aa2f7"));
        assert_eq!(palette.selection_bg, hex_color("#3b4261"));
        let theme = Theme::from_palette(palette);
        assert_eq!(theme.bg, hex_color("#1a1b26"));
        assert_eq!(theme.blue, hex_color("#7aa2f7"));
        assert_eq!(theme.cyan, hex_color("#73daca"));
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
