//! Pill-style option selector widget

use ratatui::style::Style;
use ratatui::text::Span;


/// A single option in a pill row
#[derive(Debug, Clone)]
pub struct PillOption<T> {
    pub value: T,
    pub label: String,
    pub enabled: bool,
}

/// State for a horizontal row of pill-style selectable options
#[derive(Debug, Clone)]
pub struct PillState<T> {
    pub options: Vec<PillOption<T>>,
    pub selected: usize,
}

impl<T: Clone + PartialEq> PillState<T> {
    /// Create a new PillState from (value, label) pairs, all enabled
    pub fn new(items: Vec<(T, &str)>) -> Self {
        let options = items
            .into_iter()
            .map(|(value, label)| PillOption {
                value,
                label: label.to_string(),
                enabled: true,
            })
            .collect();
        Self {
            options,
            selected: 0,
        }
    }

    /// Get the currently selected value. Panics if options is empty.
    pub fn selected_value(&self) -> &T {
        debug_assert!(!self.options.is_empty(), "PillState has no options");
        &self.options[self.selected].value
    }

    /// Get the currently selected label. Panics if options is empty.
    pub fn selected_label(&self) -> &str {
        debug_assert!(!self.options.is_empty(), "PillState has no options");
        &self.options[self.selected].label
    }

    /// Move selection to the next enabled option
    pub fn select_next(&mut self) {
        let len = self.options.len();
        for i in 1..len {
            let idx = (self.selected + i) % len;
            if self.options[idx].enabled {
                self.selected = idx;
                return;
            }
        }
    }

    /// Move selection to the previous enabled option
    pub fn select_prev(&mut self) {
        let len = self.options.len();
        for i in 1..len {
            let idx = (self.selected + len - i) % len;
            if self.options[idx].enabled {
                self.selected = idx;
                return;
            }
        }
    }

    /// Select by value
    /// Select the option carrying `value`. Returns whether the selection was
    /// applied — disabled or unknown options are refused, and callers that
    /// echo the change to the user must not report success for a refusal.
    pub fn select_value(&mut self, value: &T) -> bool {
        if let Some(idx) = self.options.iter().position(|o| &o.value == value) {
            if self.options[idx].enabled {
                self.selected = idx;
                return true;
            }
        }
        false
    }

    /// Set enabled state for a specific value
    pub fn set_enabled(&mut self, value: &T, enabled: bool) {
        if let Some(opt) = self.options.iter_mut().find(|o| &o.value == value) {
            opt.enabled = enabled;
        }
    }

    /// Set all options enabled/disabled
    pub fn set_all_enabled(&mut self, enabled: bool) {
        for opt in &mut self.options {
            opt.enabled = enabled;
        }
    }

    /// Number of options
    pub fn len(&self) -> usize {
        self.options.len()
    }
}

/// Render a pill row as a Vec of Spans
pub fn render_pill_spans<T: Clone>(state: &PillState<T>, row_focused: bool, theme: super::theme::Theme) -> Vec<Span<'static>> {
    let mut spans = Vec::new();

    for (i, opt) in state.options.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("  "));
        }

        let label = format!(" {} ", opt.label);

        let style = if !opt.enabled {
            // Disabled — always dimmed, even if selected
            Style::default().fg(theme.border_dim)
        } else if i == state.selected {
            // Active/selected pill (and enabled)
            Style::default()
                .fg(theme.pill_active_fg)
                .bg(theme.pill_active_bg)
                .add_modifier(ratatui::style::Modifier::BOLD)
        } else if row_focused {
            // Available pill in focused row — brighter to show navigable
            Style::default().fg(theme.text_muted)
        } else {
            // Available pill in unfocused row
            Style::default().fg(theme.text_dim)
        };

        spans.push(Span::styled(label, style));
    }

    spans
}
