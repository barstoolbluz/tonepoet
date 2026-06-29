use ratatui::style::Color;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WizardTheme {
    pub background: Color,
    pub surface: Color,
    pub overlay: Color,
    pub border: Color,
    pub title: Color,
    pub text: Color,
    pub text_muted: Color,
    pub text_dim: Color,
    pub accent: Color,
    pub selected_bg: Color,
    pub selected_fg: Color,
    pub hover_bg: Color,
    pub focus_bg: Color,
    pub success: Color,
    pub warning: Color,
    pub error: Color,
    pub error_dim: Color,
    pub disabled_bg: Color,
    pub disabled_fg: Color,
    pub input_bg: Color,
}

pub const DEFAULT_WIZARD_THEME: WizardTheme = WizardTheme {
    background: Color::Rgb(40, 40, 40),
    surface: Color::Rgb(60, 60, 60),
    overlay: Color::Rgb(30, 30, 30),
    border: Color::Cyan,
    title: Color::White,
    text: Color::White,
    text_muted: Color::Gray,
    text_dim: Color::DarkGray,
    accent: Color::Cyan,
    selected_bg: Color::Cyan,
    selected_fg: Color::Black,
    hover_bg: Color::Rgb(220, 255, 240),
    focus_bg: Color::Rgb(180, 220, 225),
    success: Color::Green,
    warning: Color::Yellow,
    error: Color::Red,
    error_dim: Color::Rgb(200, 80, 80),
    disabled_bg: Color::Rgb(80, 80, 80),
    disabled_fg: Color::DarkGray,
    input_bg: Color::Rgb(50, 50, 50),
};

impl Default for WizardTheme {
    fn default() -> Self {
        DEFAULT_WIZARD_THEME
    }
}
