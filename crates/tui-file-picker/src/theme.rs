use ratatui::style::{Color, Modifier, Style};

/// Style palette for embedding the picker in different applications.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePickerTheme {
    pub border: Style,
    pub border_dim: Style,
    pub title: Style,
    pub toolbar: Style,
    pub toolbar_active: Style,
    pub button: Style,
    pub button_focused: Style,
    pub button_disabled: Style,
    pub label: Style,
    pub text: Style,
    pub text_dim: Style,
    pub folder: Style,
    pub selected: Style,
    pub header: Style,
    pub status: Style,
    pub menu: Style,
    pub menu_selected: Style,
    pub menu_disabled: Style,
    pub accelerator: Style,
    pub destructive: Style,
    pub error: Style,
}

impl Default for FilePickerTheme {
    fn default() -> Self {
        Self {
            border: Style::default().fg(Color::Cyan),
            border_dim: Style::default().fg(Color::DarkGray),
            title: Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            toolbar: Style::default().fg(Color::White),
            toolbar_active: Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD),
            button: Style::default().fg(Color::Black).bg(Color::Cyan),
            button_focused: Style::default().fg(Color::Black).bg(Color::White).add_modifier(Modifier::BOLD),
            button_disabled: Style::default().fg(Color::DarkGray).bg(Color::Black),
            label: Style::default().fg(Color::DarkGray),
            text: Style::default().fg(Color::White),
            text_dim: Style::default().fg(Color::Gray),
            folder: Style::default().fg(Color::Yellow),
            selected: Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD),
            header: Style::default().fg(Color::Gray).add_modifier(Modifier::BOLD),
            status: Style::default().fg(Color::Black).bg(Color::Gray),
            menu: Style::default().fg(Color::Black).bg(Color::Gray),
            menu_selected: Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD),
            menu_disabled: Style::default().fg(Color::DarkGray).bg(Color::Gray),
            accelerator: Style::default().fg(Color::Yellow).add_modifier(Modifier::UNDERLINED),
            destructive: Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            error: Style::default().fg(Color::Red),
        }
    }
}
