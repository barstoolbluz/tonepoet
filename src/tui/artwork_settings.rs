//! Modal artwork preferences and the ordered filename editor.
//!
//! Only Browse's existing configuration is mutated. The renderer owns fixed
//! column geometry; both pointer targets and keyboard focus refer to it.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    widgets::{Block, Clear, Paragraph},
    Frame,
};

use super::{
    app::AppState,
    button_map::{ButtonRenderMap, TuiButton},
    display_width,
    theme::{contrast_ratio, Theme},
};
use crate::config::{is_safe_artwork_basename, BrowseArtworkConfig};

const WIDTH: u16 = 76;
const HEIGHT: u16 = 13;
const POPUP_WIDTH: u16 = 56;
const POPUP_HEIGHT: u16 = 13;
const LIST_ROWS: usize = 6;
const CONTROL_ROWS: [u16; 6] = [2, 3, 6, 7, 8, 9];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PopupFocus {
    List,
    Name,
    MoveUp,
    MoveDown,
    Remove,
    Restore,
    Add,
    Done,
}

impl PopupFocus {
    const ALL: [Self; 8] = [
        Self::List, Self::Name, Self::MoveUp, Self::MoveDown, Self::Remove,
        Self::Restore, Self::Add, Self::Done,
    ];

    fn step(self, backwards: bool) -> Self {
        let current = Self::ALL.iter().position(|item| *item == self).unwrap_or(0);
        Self::ALL[(current + if backwards { Self::ALL.len() - 1 } else { 1 }) % Self::ALL.len()]
    }
}

#[derive(Debug, Clone)]
struct FilenamesPopup {
    focus: PopupFocus,
    cursor: usize,
    scroll: usize,
    name: String,
    restore_armed: bool,
}

impl FilenamesPopup {
    fn new() -> Self {
        Self { focus: PopupFocus::List, cursor: 0, scroll: 0, name: String::new(), restore_armed: false }
    }

    fn keep_visible(&mut self, count: usize) {
        if count == 0 {
            self.cursor = 0;
            self.scroll = 0;
            return;
        }
        self.cursor = self.cursor.min(count - 1);
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        } else if self.cursor >= self.scroll + LIST_ROWS {
            self.scroll = self.cursor + 1 - LIST_ROWS;
        }
        self.scroll = self.scroll.min(count.saturating_sub(LIST_ROWS));
    }
}

#[derive(Debug, Clone)]
struct HeightEdit {
    which: usize,
    text: String,
    selected_all: bool,
}

/// Only this session state is ephemeral. The source of truth remains the
/// existing Browse config, which is updated and persisted on every commit.
#[derive(Debug, Clone)]
pub struct ArtworkSettingsState {
    opened: BrowseArtworkConfig,
    focus: usize,
    height_edit: Option<HeightEdit>,
    filenames: Option<FilenamesPopup>,
    help: bool,
    area: Rect,
    popup_area: Rect,
    screen: Rect,
    anchor: Rect,
    center_over_preview: bool,
    hovered: Option<TuiButton>,
    hide_image: bool,
}

impl ArtworkSettingsState {
    pub fn new(config: &BrowseArtworkConfig, anchor: Rect) -> Self {
        Self {
            opened: config.clone(),
            focus: 0,
            height_edit: None,
            filenames: None,
            help: false,
            area: Rect::default(),
            popup_area: Rect::default(),
            screen: Rect::default(),
            anchor,
            center_over_preview: false,
            hovered: None,
            hide_image: false,
        }
    }

    /// Preview actions use the same screen-centered window position as the
    /// current viewer fallback. Placement beside artwork remains a viewer policy.
    pub fn new_for_preview(config: &BrowseArtworkConfig, anchor: Rect) -> Self {
        let mut state = Self::new(config, anchor);
        state.center_over_preview = true;
        state
    }

    /// A centered fallback cannot leave a usable artwork rectangle alongside it.
    pub fn hide_image(&self) -> bool { self.hide_image }

    pub fn prepare_geometry(&mut self, screen: Rect) { self.geometry(screen); }

    /// Observe the image rectangle calculated by the active Browse renderer.
    /// The rectangle may move when a height changes without a terminal resize.
    pub fn update_anchor(&mut self, anchor: Rect, screen: Rect) {
        if self.anchor != anchor {
            self.anchor = anchor;
            self.reflow_geometry(screen);
        } else {
            self.geometry(screen);
        }
    }

    fn changed(&self, current: &BrowseArtworkConfig) -> bool {
        self.opened.prefer_embedded != current.prefer_embedded
            || self.opened.filenames != current.filenames
            || self.opened.allow_upscale != current.allow_upscale
            || self.opened.open_fullscreen != current.open_fullscreen
            || self.opened.pane_height_percent != current.pane_height_percent
            || self.opened.popup_height_percent != current.popup_height_percent
    }

    fn original_visible_values(&self, current: &BrowseArtworkConfig) -> BrowseArtworkConfig {
        let mut restored = current.clone();
        restored.prefer_embedded = self.opened.prefer_embedded;
        restored.filenames = self.opened.filenames.clone();
        restored.allow_upscale = self.opened.allow_upscale;
        restored.open_fullscreen = self.opened.open_fullscreen;
        restored.pane_height_percent = self.opened.pane_height_percent;
        restored.popup_height_percent = self.opened.popup_height_percent;
        restored
    }

    fn geometry(&mut self, screen: Rect) {
        if self.screen == screen && self.area.width == WIDTH && self.area.height == HEIGHT {
            return;
        }
        self.reflow_geometry(screen);
    }

    fn reflow_geometry(&mut self, screen: Rect) {
        self.screen = screen;
        if screen.width < 80 || screen.height < 17 {
            self.area = Rect::default();
            self.popup_area = Rect::default();
            self.hide_image = true;
            return;
        }
        let center = Rect::new(
            screen.x + (screen.width - WIDTH) / 2,
            screen.y + (screen.height - HEIGHT) / 2,
            WIDTH, HEIGHT,
        );
        let anchor = self.anchor;
        let x_min = screen.x + 2;
        let y_min = screen.y + 1;
        let x_max = screen.right().saturating_sub(WIDTH + 2);
        let y_max = screen.bottom().saturating_sub(HEIGHT + 1);
        let centered_y = anchor.y.saturating_add(anchor.height / 2).saturating_sub(HEIGHT / 2);
        let centered_x = anchor.x.saturating_add(anchor.width / 2).saturating_sub(WIDTH / 2);
        let fits = |x: u16, y: u16| {
            x >= x_min && x <= x_max && y >= y_min && y <= y_max
        };
        let choice = if self.center_over_preview || anchor.width == 0 || anchor.height == 0 {
            None
        } else {
            anchor.x.checked_sub(WIDTH + 2)
                .map(|x| (x, centered_y.clamp(y_min, y_max)))
                .filter(|&(x, y)| fits(x, y))
                .or_else(|| {
                    let x = anchor.right().saturating_add(2);
                    let y = centered_y.clamp(y_min, y_max);
                    fits(x, y).then_some((x, y))
                })
                .or_else(|| {
                    let y = anchor.y.checked_sub(HEIGHT + 1)?;
                    let x = centered_x.clamp(x_min, x_max);
                    fits(x, y).then_some((x, y))
                })
                .or_else(|| {
                    let x = centered_x.clamp(x_min, x_max);
                    let y = anchor.bottom().saturating_add(1);
                    fits(x, y).then_some((x, y))
                })
        };
        // Retain the prior position while it still leaves the image visible.
        // Reposition only after an anchor change invalidates that separation,
        // or when a previously hidden image can now be shown.
        let separate_from_image = |area: Rect| {
            area.right().saturating_add(2) <= anchor.x
                || anchor.right().saturating_add(2) <= area.x
                || area.bottom().saturating_add(1) <= anchor.y
                || anchor.bottom().saturating_add(1) <= area.y
        };
        let previous_valid = !self.hide_image
            && fits(self.area.x, self.area.y)
            && self.area.width == WIDTH && self.area.height == HEIGHT
            && separate_from_image(self.area);
        // Preview settings always open at screen center. Hide the image only
        // if its current rectangle would intrude on that centered modal.
        self.hide_image = if self.center_over_preview {
            anchor.width > 0 && anchor.height > 0 && !separate_from_image(center)
        } else {
            choice.is_none()
        };
        self.area = if self.center_over_preview {
            center
        } else {
            match choice {
                Some(_) if previous_valid => self.area,
                Some((x, y)) => Rect::new(x, y, WIDTH, HEIGHT),
                None => center,
            }
        };
        self.popup_area = Rect::new(
            self.area.x + (WIDTH - POPUP_WIDTH) / 2,
            (self.area.y + 2).min(screen.bottom() - POPUP_HEIGHT),
            POPUP_WIDTH, POPUP_HEIGHT,
        );
    }
}

fn mix(left: Color, right: Color, weight_right: u16, denom: u16) -> Color {
    let rgb = |color| match color {
        Color::Rgb(r, g, b) => (r, g, b),
        _ => (128, 128, 128),
    };
    let (lr, lg, lb) = rgb(left);
    let (rr, rg, rb) = rgb(right);
    let a = denom - weight_right;
    Color::Rgb(
        ((u16::from(lr) * a + u16::from(rr) * weight_right) / denom) as u8,
        ((u16::from(lg) * a + u16::from(rg) * weight_right) / denom) as u8,
        ((u16::from(lb) * a + u16::from(rb) * weight_right) / denom) as u8,
    )
}

fn readable(color: Color, bg: Color) -> Color {
    if contrast_ratio(color, bg) >= 4.5 {
        return color;
    }
    let mut best = None;
    for pole in [Color::Rgb(255, 255, 255), Color::Rgb(0, 0, 0)] {
        if contrast_ratio(pole, bg) < 4.5 {
            continue;
        }
        let (mut lo, mut hi) = (0u16, 256u16);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if contrast_ratio(mix(color, pole, mid, 256), bg) >= 4.5 {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }
        if best.is_none_or(|(amount, _): (u16, Color)| lo < amount) {
            best = Some((lo, mix(color, pole, lo, 256)));
        }
    }
    best.map_or(color, |(_, adjusted)| adjusted)
}

// Soft text derives from the already legible label on its actual region
// fill. This also follows the mockups in light themes, whose palette label
// must be adjusted before it is blended with the value color.
fn soft(theme: Theme, background: Color) -> Color {
    mix(readable(theme.label, background), theme.value, 1, 3)
}

fn well(theme: Theme) -> Color {
    if theme.dark {
        mix(theme.panel_bg, Color::Rgb(0, 0, 0), 1, 3)
    } else {
        mix(theme.panel_bg, Color::Rgb(255, 255, 255), 2, 3)
    }
}

fn band_color(theme: Theme, focused: bool) -> Color {
    if focused { theme.border_dim } else { theme.surface }
}

fn paint(f: &mut Frame, rect: Rect, x: u16, y: u16, text: &str, color: Color, bg: Color) {
    if x >= rect.width || y >= rect.height { return; }
    let max = (rect.width - x) as usize;
    let fitted = display_width::truncate_right(text, max);
    let width = display_width::width(&fitted) as u16;
    if width == 0 { return; }
    f.render_widget(
        Paragraph::new(fitted).style(Style::default().fg(readable(color, bg)).bg(bg)),
        Rect::new(rect.x + x, rect.y + y, width, 1),
    );
}

fn paint_raw(f: &mut Frame, rect: Rect, x: u16, y: u16, text: &str, color: Color, bg: Color) {
    if x >= rect.width || y >= rect.height { return; }
    let fitted = display_width::truncate_right(text, (rect.width - x) as usize);
    let width = display_width::width(&fitted) as u16;
    if width > 0 {
        f.render_widget(Paragraph::new(fitted).style(Style::default().fg(color).bg(bg)),
            Rect::new(rect.x + x, rect.y + y, width, 1));
    }
}

fn fill(f: &mut Frame, rect: Rect, color: Color) {
    if rect.width > 0 && rect.height > 0 {
        f.render_widget(Block::default().style(Style::default().bg(color)), rect);
    }
}

fn frame(f: &mut Frame, area: Rect, title: &str, rules: &[u16], theme: Theme) {
    // Style-only blocks preserve old symbols. Clear once for all three modal
    // frames; the subsequent region fills are intentionally style-only.
    f.render_widget(Clear, area);
    fill(f, area, theme.panel_bg);
    paint(f, area, 0, 0, "┌", theme.title, theme.panel_bg);
    fill(f, Rect::new(area.x + 1, area.y, area.width - 2, 1), theme.title);
    paint(f, area, 1, 0, &format!("▸ {title}"), theme.panel_bg, theme.title);
    paint(f, area, area.width - 1, 0, "┐", theme.title, theme.panel_bg);
    for y in 1..area.height - 1 {
        if rules.contains(&y) {
            paint(f, area, 0, y, &format!("├{}┤", "─".repeat((area.width - 2) as usize)), theme.title, theme.panel_bg);
        } else {
            paint(f, area, 0, y, "│", theme.title, theme.panel_bg);
            paint(f, area, area.width - 1, y, "│", theme.title, theme.panel_bg);
        }
    }
    paint(f, area, 0, area.height - 1, &format!("└{}┘", "─".repeat((area.width - 2) as usize)), theme.title, theme.panel_bg);
}

fn button(f: &mut Frame, area: Rect, x: u16, y: u16, width: u16, label: &str,
          focused: bool, hovered: bool, bg: Color, theme: Theme) {
    let background = if focused { theme.pill_active_bg }
        else if hovered { theme.pill_neutral_hover_bg } else { bg };
    let foreground = if focused { theme.pill_active_fg } else { theme.value };
    let words = display_width::truncate_right(label, width as usize);
    let words = format!("{words:<width$}", width = width as usize);
    paint(f, area, x, y, &words, foreground, background);
}

fn record(map: &mut ButtonRenderMap, button: TuiButton, area: Rect,
          x: u16, y: u16, width: u16) {
    map.record_button(button, Rect::new(area.x + x, area.y + y, width, 1));
}

fn summary_count(names: &[String], max_width: usize) -> usize {
    let mut width = 0usize;
    let mut shown = 0;
    for name in names {
        let candidate = width + if shown == 0 { 0 } else { 3 } + display_width::width(name);
        let remaining = names.len() - shown - 1;
        let suffix = if remaining == 0 { 0 } else { 1 + format!("+{remaining}").len() };
        if candidate + suffix > max_width { break; }
        width = candidate;
        shown += 1;
    }
    shown
}

#[cfg(test)]
fn summary(names: &[String], max_width: usize) -> String {
    let shown = summary_count(names, max_width);
    let mut result = names[..shown].join(" · ");
    let hidden = names.len() - shown;
    if hidden > 0 {
        if !result.is_empty() { result.push(' '); }
        result.push_str(&format!("+{hidden}"));
    }
    result
}

fn height_value(config: &BrowseArtworkConfig, which: usize) -> u8 {
    if which == 0 { config.pane_height_percent } else { config.popup_height_percent }
}

fn edit_height(state: &mut ArtworkSettingsState, config: &BrowseArtworkConfig,
               which: usize, digit: Option<char>) {
    state.focus = which + 4;
    state.height_edit = Some(HeightEdit {
        which,
        text: digit.map_or_else(|| height_value(config, which).to_string(), |d| d.to_string()),
        selected_all: digit.is_none(),
    });
}

fn changed_config(app: &mut AppState, new_config: BrowseArtworkConfig) {
    let old = &app.browse.artwork_config;
    if *old == new_config { return; }
    let rescan = old.prefer_embedded != new_config.prefer_embedded || old.filenames != new_config.filenames;
    let resize = old.allow_upscale != new_config.allow_upscale
        || old.pane_height_percent != new_config.pane_height_percent
        || old.popup_height_percent != new_config.popup_height_percent;
    app.browse.artwork_config = new_config;
    if rescan {
        app.browse_artwork.invalidate_for_settings();
    } else if resize {
        app.browse_artwork.invalidate_protocol();
    }
    super::context_menu::persist_browse_config(app);
}

fn change_height(app: &mut AppState, which: usize, value: u8) {
    let mut config = app.browse.artwork_config.clone();
    let value = value.clamp(1, 100);
    if which == 0 { config.pane_height_percent = value; }
    else { config.popup_height_percent = value; }
    changed_config(app, config);
}

fn commit_height(app: &mut AppState, state: &mut ArtworkSettingsState) {
    let Some(edit) = state.height_edit.take() else { return; };
    if let Ok(value) = edit.text.parse::<u16>() {
        change_height(app, edit.which, value.clamp(1, 100) as u8);
    }
}

fn set_focus(state: &mut ArtworkSettingsState, focus: usize) {
    state.focus = focus.min(CONTROL_ROWS.len() - 1);
}

fn popup_action(app: &mut AppState, state: &mut ArtworkSettingsState, focus: PopupFocus) -> bool {
    let Some(popup) = &mut state.filenames else { return true; };
    let mut config = app.browse.artwork_config.clone();
    let names = &mut config.filenames;
    popup.restore_armed &= focus == PopupFocus::Restore;
    match focus {
        PopupFocus::MoveUp if popup.cursor > 0 && popup.cursor < names.len() => {
            names.swap(popup.cursor, popup.cursor - 1);
            popup.cursor -= 1;
        }
        PopupFocus::MoveDown if popup.cursor + 1 < names.len() => {
            names.swap(popup.cursor, popup.cursor + 1);
            popup.cursor += 1;
        }
        PopupFocus::Remove if popup.cursor < names.len() => {
            names.remove(popup.cursor);
        }
        PopupFocus::Restore => {
            if !popup.restore_armed {
                popup.restore_armed = true;
                return true;
            }
            popup.restore_armed = false;
            *names = BrowseArtworkConfig::default().filenames;
            popup.cursor = 0;
            popup.scroll = 0;
        }
        PopupFocus::Add => {
            let name = popup.name.trim();
            if name.is_empty() { return true; }
            if let Some(index) = names.iter().position(|item| item.eq_ignore_ascii_case(name)) {
                popup.cursor = index;
            } else if names.len() < 32 && is_safe_artwork_basename(name) {
                names.push(name.to_string());
                popup.cursor = names.len() - 1;
            } else {
                return true;
            }
            popup.name.clear();
            popup.focus = PopupFocus::List;
        }
        PopupFocus::Done => {
            state.filenames = None;
            return true;
        }
        _ => {}
    }
    popup.keep_visible(names.len());
    changed_config(app, config);
    true
}

/// Returns false only when the main window should close.
pub fn handle_key(app: &mut AppState, state: &mut ArtworkSettingsState, key: KeyEvent) -> bool {
    if state.help {
        if matches!(key.code, KeyCode::Esc | KeyCode::Char('?') | KeyCode::Enter) {
            state.help = false;
        }
        return true;
    }
    if state.filenames.is_some() { return popup_key(app, state, key); }
    if let Some(edit) = state.height_edit.as_mut() {
        match key.code {
            KeyCode::Esc => { state.height_edit = None; return true; }
            KeyCode::Enter => { commit_height(app, state); return true; }
            KeyCode::Tab | KeyCode::BackTab => {
                commit_height(app, state);
                // Both directions leave DISPLAY, as with region navigation
                // when no height editor owns the keyboard.
                set_focus(state, 0);
                return true;
            }
            KeyCode::Backspace => {
                if edit.selected_all { edit.text.clear(); edit.selected_all = false; }
                else { edit.text.pop(); }
                return true;
            }
            KeyCode::Char(digit) if digit.is_ascii_digit() => {
                if edit.selected_all { edit.text.clear(); edit.selected_all = false; }
                if edit.text.len() < 3 { edit.text.push(digit); }
                return true;
            }
            _ => return true,
        }
    }
    match key.code {
        KeyCode::Esc => return false,
        KeyCode::Char('?') => state.help = true,
        KeyCode::Up => set_focus(state, (state.focus + CONTROL_ROWS.len() - 1) % CONTROL_ROWS.len()),
        KeyCode::Down => set_focus(state, (state.focus + 1) % CONTROL_ROWS.len()),
        KeyCode::Tab | KeyCode::BackTab => {
            // The two regions wrap in either direction; do not change the
            // focused control's row until the region itself changes.
            set_focus(state, if state.focus < 2 { 2 } else { 0 });
        }
        KeyCode::Char('u') if state.changed(&app.browse.artwork_config) => {
            let restored = state.original_visible_values(&app.browse.artwork_config);
            changed_config(app, restored);
        }
        KeyCode::Char(' ') | KeyCode::Enter if matches!(state.focus, 0 | 2 | 3) => {
            let mut config = app.browse.artwork_config.clone();
            match state.focus {
                0 => config.prefer_embedded = !config.prefer_embedded,
                2 => config.allow_upscale = !config.allow_upscale,
                _ => config.open_fullscreen = !config.open_fullscreen,
            }
            changed_config(app, config);
        }
        KeyCode::Enter if state.focus == 1 => state.filenames = Some(FilenamesPopup::new()),
        KeyCode::Enter if state.focus >= 4 => {
            let which = state.focus - 4;
            edit_height(state, &app.browse.artwork_config, which, None);
        }
        KeyCode::Char('[') | KeyCode::Left if state.focus >= 4 => {
            let which = state.focus - 4;
            let next = height_value(&app.browse.artwork_config, which).saturating_sub(5).max(1);
            change_height(app, which, next);
        }
        KeyCode::Char(']') | KeyCode::Right if state.focus >= 4 => {
            let which = state.focus - 4;
            let next = height_value(&app.browse.artwork_config, which).saturating_add(5).min(100);
            change_height(app, which, next);
        }
        KeyCode::Char(digit) if state.focus >= 4 && digit.is_ascii_digit() => {
            let which = state.focus - 4;
            edit_height(state, &app.browse.artwork_config, which, Some(digit));
        }
        _ => {}
    }
    true
}

fn popup_key(app: &mut AppState, state: &mut ArtworkSettingsState, key: KeyEvent) -> bool {
    let popup = state.filenames.as_mut().expect("popup checked");
    if popup.restore_armed && !(popup.focus == PopupFocus::Restore && key.code == KeyCode::Enter) {
        popup.restore_armed = false;
        if key.code == KeyCode::Esc { return true; }
    }
    if popup.focus == PopupFocus::Name
        && !matches!(key.code, KeyCode::Tab | KeyCode::BackTab)
    {
        match key.code {
            KeyCode::Enter => return popup_action(app, state, PopupFocus::Add),
            KeyCode::Esc => { state.filenames = None; return true; }
            KeyCode::Backspace => { popup.name.pop(); return true; }
            KeyCode::Char(ch) if !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
                if popup.name.len() + ch.len_utf8() <= 255 && !ch.is_control() {
                    popup.name.push(ch);
                }
                return true;
            }
            _ => return true,
        }
    }
    match key.code {
        KeyCode::Esc => state.filenames = None,
        KeyCode::Char('?') => state.help = true,
        KeyCode::Tab | KeyCode::BackTab => {
            popup.focus = popup.focus.step(key.code == KeyCode::BackTab
                || key.modifiers.contains(KeyModifiers::SHIFT));
        }
        KeyCode::Up if popup.focus == PopupFocus::List => {
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                return popup_action(app, state, PopupFocus::MoveUp);
            }
            popup.cursor = popup.cursor.saturating_sub(1);
            popup.keep_visible(app.browse.artwork_config.filenames.len());
        }
        KeyCode::Down if popup.focus == PopupFocus::List => {
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                return popup_action(app, state, PopupFocus::MoveDown);
            }
            popup.cursor = popup.cursor.saturating_add(1);
            popup.keep_visible(app.browse.artwork_config.filenames.len());
        }
        KeyCode::Home if popup.focus == PopupFocus::List => { popup.cursor = 0; popup.keep_visible(app.browse.artwork_config.filenames.len()); }
        KeyCode::End if popup.focus == PopupFocus::List => {
            popup.cursor = app.browse.artwork_config.filenames.len().saturating_sub(1);
            popup.keep_visible(app.browse.artwork_config.filenames.len());
        }
        KeyCode::Delete if popup.focus == PopupFocus::List => {
            return popup_action(app, state, PopupFocus::Remove);
        }
        KeyCode::Enter | KeyCode::Char(' ') => {
            let focus = popup.focus;
            if focus == PopupFocus::List { popup.focus = PopupFocus::Name; }
            else { return popup_action(app, state, focus); }
        }
        _ => {}
    }
    true
}

fn at(area: Rect, x: u16, y: u16) -> bool {
    area.width > 0 && x >= area.x && y >= area.y && x < area.right() && y < area.bottom()
}

fn target(map: &ButtonRenderMap, x: u16, y: u16) -> Option<TuiButton> {
    map.find_button_at(x, y)
}

pub fn handle_mouse(app: &mut AppState, state: &mut ArtworkSettingsState, mouse: MouseEvent) -> bool {
    let button = target(&app.button_map, mouse.column, mouse.row);
    if mouse.kind == MouseEventKind::Moved {
        state.hovered = button;
        return true;
    }
    if state.help {
        if matches!(mouse.kind, MouseEventKind::Down(_)) { state.help = false; }
        return true;
    }
    if state.filenames.is_some() {
        return popup_mouse(app, state, mouse, button);
    }
    if !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) { return true; }
    if state.height_edit.is_some() {
        match button {
            Some(TuiButton::ArtworkHeightField(which)) if state.height_edit.as_ref().is_some_and(|e| e.which == which as usize) => {
                // Reselect the pending text without reloading the last saved
                // value. Only Esc is cancellation; other targets commit.
                state.height_edit.as_mut().expect("active height edit").selected_all = true;
                return true;
            }
            _ => commit_height(app, state),
        }
    }
    let control = match button {
        Some(TuiButton::ArtworkToggle(index)) => {
            let index = index as usize;
            set_focus(state, index);
            let mut config = app.browse.artwork_config.clone();
            match index {
                0 => config.prefer_embedded = !config.prefer_embedded,
                2 => config.allow_upscale = !config.allow_upscale,
                3 => config.open_fullscreen = !config.open_fullscreen,
                _ => return true,
            }
            changed_config(app, config);
            true
        }
        Some(TuiButton::ArtworkFilenamesEdit) => {
            state.focus = 1;
            state.filenames = Some(FilenamesPopup::new());
            true
        }
        Some(TuiButton::ArtworkHeightDec(which)) | Some(TuiButton::ArtworkHeightInc(which)) => {
            let step_up = matches!(button, Some(TuiButton::ArtworkHeightInc(_)));
            state.focus = which as usize + 4;
            let old = height_value(&app.browse.artwork_config, which as usize);
            change_height(app, which as usize, if step_up { old.saturating_add(5).min(100) } else { old.saturating_sub(5).max(1) });
            true
        }
        Some(TuiButton::ArtworkHeightField(which)) => {
            edit_height(state, &app.browse.artwork_config, which as usize, None);
            true
        }
        Some(TuiButton::ArtworkHeightBar(which, cell)) => {
            state.focus = which as usize + 4;
            change_height(app, which as usize, cell.saturating_mul(10));
            true
        }
        Some(TuiButton::ArtworkRevert) => {
            let restored = state.original_visible_values(&app.browse.artwork_config);
            changed_config(app, restored);
            true
        }
        Some(TuiButton::ArtworkClose) => return false,
        Some(TuiButton::ArtworkHelp) => { state.help = true; true }
        _ => at(state.area, mouse.column, mouse.row),
    };
    if !control { return false; }
    true
}

fn popup_mouse(app: &mut AppState, state: &mut ArtworkSettingsState,
               mouse: MouseEvent, button: Option<TuiButton>) -> bool {
    let popup = state.filenames.as_mut().expect("popup checked");
    if matches!(mouse.kind, MouseEventKind::ScrollUp | MouseEventKind::ScrollDown) {
        let previous = popup.cursor;
        if mouse.kind == MouseEventKind::ScrollUp { popup.cursor = popup.cursor.saturating_sub(1); }
        else { popup.cursor = popup.cursor.saturating_add(1); }
        popup.keep_visible(app.browse.artwork_config.filenames.len());
        if popup.cursor != previous { popup.restore_armed = false; }
        return true;
    }
    if !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) { return true; }
    if !at(state.popup_area, mouse.column, mouse.row) {
        state.filenames = None;
        return true;
    }
    let focus = match button {
        Some(TuiButton::ArtworkNameRow(index)) => {
            popup.cursor = index;
            popup.focus = PopupFocus::List;
            popup.keep_visible(app.browse.artwork_config.filenames.len());
            popup.restore_armed = false;
            return true;
        }
        Some(TuiButton::ArtworkNameField) => PopupFocus::Name,
        Some(TuiButton::ArtworkNameAction(index)) => match index {
            0 => PopupFocus::MoveUp,
            1 => PopupFocus::MoveDown,
            2 => PopupFocus::Remove,
            3 => PopupFocus::Restore,
            4 => PopupFocus::Add,
            _ => return true,
        },
        Some(TuiButton::ArtworkNameDone) => PopupFocus::Done,
        Some(TuiButton::ArtworkNameHelp) => {
            popup.restore_armed = false;
            state.help = true;
            return true;
        }
        _ => return true,
    };
    if popup.focus != focus { popup.restore_armed = false; }
    popup.focus = focus;
    if !matches!(focus, PopupFocus::Name) { popup_action(app, state, focus); }
    true
}

/// Dim underlying text while leaving image cells and graphics commands intact.
/// The modal itself is rendered afterwards with its own colors.
fn dim_backdrop(f: &mut Frame, state: &ArtworkSettingsState, theme: Theme) {
    let preserve_image = !state.hide_image;
    let anchor = state.anchor;
    let screen = f.size();
    let buffer = f.buffer_mut();
    for y in screen.y..screen.bottom() {
        for x in screen.x..screen.right() {
            if preserve_image && at(anchor, x, y) { continue; }
            let cell = buffer.get_mut(x, y);
            // Graphics protocols use escape sequences and image placeholders
            // that must survive an ordinary textual overlay unchanged.
            if cell.skip || cell.symbol().contains('\u{1b}')
                || cell.symbol().contains('\u{10eeee}')
            {
                continue;
            }
            cell.set_fg(theme.text_dim);
        }
    }
}

/// Renderer for the stable 76-column source/display window and its nested popup.
pub fn draw(f: &mut Frame, state: &mut ArtworkSettingsState,
            config: &BrowseArtworkConfig, map: &mut ButtonRenderMap, theme: Theme) {
    state.geometry(f.size());
    dim_backdrop(f, state, theme);
    let area = state.area;
    if area.width == 0 {
        let message = "Artwork settings require an 80 × 17 terminal";
        let screen = f.size();
        let x = screen.x + screen.width.saturating_sub(display_width::width(message) as u16) / 2;
        let y = screen.y + screen.height / 2;
        f.render_widget(Paragraph::new(message).style(Style::default().fg(theme.value)),
            Rect::new(x, y, screen.right().saturating_sub(x), 1));
        return;
    }
    frame(f, area, "artwork settings", &[4, 10], theme);
    let source_bg = band_color(theme, state.focus < 2);
    let display_bg = band_color(theme, state.focus >= 2);
    fill(f, Rect::new(area.x + 1, area.y + 1, WIDTH - 2, 3), source_bg);
    fill(f, Rect::new(area.x + 1, area.y + 5, WIDTH - 2, 5), display_bg);
    paint(f, area, 3, 1, "SOURCE", if state.focus < 2 { theme.header } else { theme.label }, source_bg);
    paint(f, area, 3, 5, "DISPLAY", if state.focus >= 2 { theme.header } else { theme.label }, display_bg);
    for (index, checked, label, consequence) in [
        (0usize, config.prefer_embedded, "Prefer embedded artwork", if config.prefer_embedded { "embedded artwork comes first" } else { "folder images come first" }),
        (2, config.allow_upscale, "Allow image enlargement", if config.allow_upscale { "small images fill the area" } else { "small images keep their size" }),
        (3, config.open_fullscreen, "Open images full screen", if config.open_fullscreen { "image files open maximized" } else { "image files open at normal size" }),
    ] {
        let row = CONTROL_ROWS[index];
        let background = if index == 0 { source_bg } else { display_bg };
        let selected = state.focus == index && state.filenames.is_none();
        let hovered = state.hovered == Some(TuiButton::ArtworkToggle(index as u8));
        let span_bg = if selected { theme.selection_bg } else if hovered { theme.hover_bg } else { background };
        let checkbox = if checked { "☑" } else { "☐" };
        let text = format!("{checkbox} {label}");
        paint(f, area, 3, row, &text, theme.value, span_bg);
        paint(f, area, 40, row, consequence, soft(theme, background), background);
        record(map, TuiButton::ArtworkToggle(index as u8), area, 3, row, display_width::width(&text) as u16);
    }
    paint(f, area, 3, 3, "Filenames", theme.label, source_bg);
    let shown = summary_count(&config.filenames, 47);
    let mut column = 16u16;
    for (index, name) in config.filenames.iter().take(shown).enumerate() {
        if index > 0 {
            paint_raw(f, area, column, 3, " · ", theme.text_dim, source_bg);
            column += 3;
        }
        paint(f, area, column, 3, name, theme.value, source_bg);
        column += display_width::width(name) as u16;
    }
    let hidden = config.filenames.len().saturating_sub(shown);
    if hidden > 0 {
        if shown > 0 { column += 1; }
        paint(f, area, column, 3, &format!("+{hidden}"),
              soft(theme, source_bg), source_bg);
    }
    button(f, area, 66, 3, 7, " Edit… ", state.focus == 1 && state.filenames.is_none(),
        state.hovered == Some(TuiButton::ArtworkFilenamesEdit), theme.pill_neutral_bg, theme);
    record(map, TuiButton::ArtworkFilenamesEdit, area, 66, 3, 7);
    for which in 0..2 {
        let row = CONTROL_ROWS[which + 4];
        let value = height_value(config, which);
        paint(f, area, 3, row, if which == 0 { "Info height" } else { "View height" }, theme.label, display_bg);
        let selected = state.focus == which + 4 && state.filenames.is_none();
        let span_bg = if selected { theme.selection_bg } else { display_bg };
        fill(f, Rect::new(area.x + 16, area.y + row, 10, 1), span_bg);
        paint(f, area, 16, row, "◂", theme.info, span_bg);
        let edit = state.height_edit.as_ref().filter(|edit| edit.which == which);
        let digits = edit.map_or_else(|| value.to_string(), |edit| edit.text.clone());
        let field_bg = well(theme);
        let digits = format!(" {:>3} ", digits);
        paint(f, area, 18, row, &digits, theme.value, field_bg);
        if edit.is_some() {
            // Block caret occupies the trailing padding cell; it does not
            // displace digits, bar, arrows, or any mouse target.
            paint(f, area, 22, row, " ", theme.panel_bg, theme.editing_cursor);
        }
        paint(f, area, 23, row, "%", theme.value, span_bg);
        paint(f, area, 25, row, "▸", theme.info, span_bg);
        let filled = (f64::from(value) / 10.0).round() as u16;
        for cell in 0..10 {
            if cell < filled {
                paint(f, area, 27 + cell, row, "█", theme.info, display_bg);
            } else {
                paint_raw(f, area, 27 + cell, row, "░", theme.text_dim, display_bg);
            }
            record(map, TuiButton::ArtworkHeightBar(which as u8, (cell + 1) as u8), area, 27 + cell, row, 1);
        }
        paint(f, area, 40, row, if which == 0 { "of the Info pane" } else { "of the View pane" },
              soft(theme, display_bg), display_bg);
        record(map, TuiButton::ArtworkHeightDec(which as u8), area, 16, row, 1);
        record(map, TuiButton::ArtworkHeightField(which as u8), area, 18, row, 5);
        record(map, TuiButton::ArtworkHeightInc(which as u8), area, 25, row, 1);
    }
    if state.changed(config) {
        button(f, area, 26, 11, 10, " u Revert ", false,
            false, theme.tab_active, theme);
        record(map, TuiButton::ArtworkRevert, area, 26, 11, 10);
    }
    button(f, area, 38, 11, 11, " Esc Close ", false,
        false, theme.chip_dismiss, theme);
    record(map, TuiButton::ArtworkClose, area, 38, 11, 11);
    paint(f, area, 63, 11, "?", theme.info, theme.panel_bg);
    paint(f, area, 65, 11, "all keys", theme.label, theme.panel_bg);
    record(map, TuiButton::ArtworkHelp, area, 63, 11, 10);
    if let Some(popup) = &state.filenames {
        // Words recede while the popup owns all input. Preserve the
        // underlying region fills so the placement remains apparent.
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                f.buffer_mut().get_mut(x, y).set_fg(theme.text_dim);
            }
        }
        draw_filenames(f, state.popup_area, popup, config, map, theme, state.hovered);
    }
    if state.help { draw_help(f, state, theme); }
}

fn list_visible(name: &str, width: usize) -> String {
    display_width::truncate_right(name, width)
}

fn name_preview(name: &str) -> String {
    let mut chars = Vec::new();
    let mut width = 0;
    for ch in name.chars().rev() {
        let w = display_width::char_width(ch);
        if width + w > 19 { break; }
        chars.push(ch);
        width += w;
    }
    chars.into_iter().rev().collect()
}

fn draw_filenames(f: &mut Frame, area: Rect, popup: &FilenamesPopup,
                  config: &BrowseArtworkConfig, map: &mut ButtonRenderMap,
                  theme: Theme, hovered: Option<TuiButton>) {
    frame(f, area, "Filenames", &[10], theme);
    fill(f, Rect::new(area.x + 1, area.y + 1, POPUP_WIDTH - 2, 1), theme.surface);
    paint(f, area, 3, 1, "The first name found in a track’s folder wins.",
        soft(theme, theme.surface), theme.surface);
    fill(f, Rect::new(area.x + 1, area.y + 2, POPUP_WIDTH - 2, 8), theme.dropdown_bg);
    for visible in 0..LIST_ROWS {
        let index = popup.scroll + visible;
        let row = 3 + visible as u16;
        let background = if visible % 2 == 0 { theme.border_dim }
            else { mix(theme.panel_bg, theme.border, 2, 3) };
        fill(f, Rect::new(area.x + 1, area.y + row, 30, 1), background);
        if let Some(name) = config.filenames.get(index) {
            let selected = popup.cursor == index && popup.focus == PopupFocus::List;
            if selected { fill(f, Rect::new(area.x + 1, area.y + row, 30, 1), theme.selection_bg); }
            let row_bg = if selected { theme.selection_bg } else { background };
            if selected { paint(f, area, 1, row, "▌", theme.info, row_bg); }
            paint(f, area, 3, row, &format!("{:<3}", index + 1), theme.label, row_bg);
            paint(f, area, 7, row, &list_visible(name, 22), theme.value, row_bg);
            record(map, TuiButton::ArtworkNameRow(index), area, 1, row, 30);
        }
    }
    for (index, focus, row, width, label) in [
        (0, PopupFocus::MoveUp, 3, 11, " Move up "),
        (1, PopupFocus::MoveDown, 4, 11, " Move down "),
        (2, PopupFocus::Remove, 5, 11, " Remove "),
        (3, PopupFocus::Restore, 7, 18, if popup.restore_armed { " Press again " } else { " Restore defaults " }),
        (4, PopupFocus::Add, 9, 11, " Add "),
    ] {
        let armed = focus == PopupFocus::Restore && popup.restore_armed;
        button(f, area, 34, row, width, label, popup.focus == focus && !armed,
            !armed && hovered == Some(TuiButton::ArtworkNameAction(index)),
            if armed { theme.destructive } else { theme.pill_neutral_bg }, theme);
        record(map, TuiButton::ArtworkNameAction(index), area, 34, row, width);
    }
    paint(f, area, 3, 9, "Name", theme.label, theme.dropdown_bg);
    let name_bg = well(theme);
    let visible = name_preview(&popup.name);
    // Pad in terminal cells, not Unicode scalar values: double-width
    // filenames must never shift the Add button or their mouse hitboxes.
    let field = format!(" {}{} ", visible,
                        " ".repeat(19usize.saturating_sub(display_width::width(&visible))));
    paint(f, area, 9, 9, &field, theme.value, name_bg);
    if popup.focus == PopupFocus::Name {
        let caret_col = 10 + display_width::width(&visible) as u16;
        paint(f, area, caret_col.min(29), 9, " ", theme.panel_bg, theme.editing_cursor);
    }
    record(map, TuiButton::ArtworkNameField, area, 9, 9, 21);
    fill(f, Rect::new(area.x + 1, area.y + 11, POPUP_WIDTH - 2, 1), theme.dropdown_bg);
    paint(f, area, 3, 11, "?", theme.info, theme.dropdown_bg);
    paint(f, area, 5, 11, "all keys", theme.label, theme.dropdown_bg);
    record(map, TuiButton::ArtworkNameHelp, area, 3, 11, 10);
    // Done remains affirmative green even when focused. Underline is the
    // focus cue here; repainting it with the generic active blue would
    // incorrectly turn the popup's only affirmative action blue.
    let done_bg = theme.chip_go;
    let done_style = Style::default()
        .fg(readable(theme.panel_bg, done_bg))
        .bg(done_bg)
        .add_modifier(if popup.focus == PopupFocus::Done || hovered == Some(TuiButton::ArtworkNameDone) {
            Modifier::UNDERLINED
        } else { Modifier::empty() });
    f.render_widget(Paragraph::new(" Done ").style(done_style),
                    Rect::new(area.x + 47, area.y + 11, 6, 1));
    record(map, TuiButton::ArtworkNameDone, area, 47, 11, 6);
}

fn draw_help(f: &mut Frame, state: &ArtworkSettingsState, theme: Theme) {
    let rows: &[&str] = if state.filenames.is_some() { &[
        "Up/Down    Select filename",
        "Ctrl+Up/Down    Move entry",
        "Delete    Remove selected entry",
        "Tab / Shift+Tab    Change focus",
        "Enter    Activate button or Add name",
        "Esc    Close / disarm Restore defaults",
        "?    Close all keys",
    ] } else { &[
        "Up/Down    Move between controls",
        "Tab / Shift+Tab    Change region",
        "Space / Enter    Toggle or open",
        "[ / ]    Decrease / increase height",
        "Digits / Enter    Edit height; Enter commits",
        "Esc    Cancel edit or close window",
        "u    Revert changed settings",
        "?    Close all keys",
    ] };
    let screen = f.size();
    let width = 56u16.min(screen.width);
    let height = (rows.len() as u16 + 4).min(screen.height);
    let rect = Rect::new(screen.x + (screen.width - width) / 2,
                         screen.y + (screen.height - height) / 2, width, height);
    frame(f, rect, "All keys", &[], theme);
    fill(f, Rect::new(rect.x + 1, rect.y + 1, width - 2, height - 2), theme.dropdown_bg);
    for (i, row) in rows.iter().enumerate() {
        if i as u16 + 2 < height - 1 {
            paint(f, rect, 3, i as u16 + 2, row, theme.value, theme.dropdown_bg);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn right_border_and_local_backgrounds_survive_text_in_both_themes() {
        use ratatui::{backend::TestBackend, Terminal};

        for slug in ["tokyo-night", "tokyo-night-day"] {
            let theme = super::super::theme::theme_by_slug(slug).unwrap();
            let config = BrowseArtworkConfig::default();
            let mut state = ArtworkSettingsState::new(&config, Rect::default());
            let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
            let mut map = ButtonRenderMap::default();
            terminal.draw(|frame| draw(frame, &mut state, &config, &mut map, theme)).unwrap();
            let area = state.area;
            let buffer = terminal.backend().buffer();
            for y in 0..area.height {
                let expected = if y == 0 { "\u{2510}" } else if y == area.height - 1 {
                    "\u{2518}"
                } else if y == 4 || y == 10 { "\u{2524}" } else { "\u{2502}" };
                let cell = buffer.get(area.right() - 1, area.y + y);
                assert_eq!(cell.symbol(), expected, "{slug} main row {y}");
                assert_eq!(cell.fg, readable(theme.title, theme.panel_bg), "{slug} main border color at row {y}");
            }
            let selected_width = display_width::width("\u{2611} Prefer embedded artwork") as u16;
            assert_eq!(buffer.get(area.x + 3 + selected_width - 1, area.y + 2).bg,
                       theme.selection_bg, "{slug} selected label ends at its text");
            assert_eq!(buffer.get(area.x + 3 + selected_width, area.y + 2).bg,
                       band_color(theme, true), "{slug} selected label must not flood the row");
            for x in 38..49 {
                assert_eq!(buffer.get(area.x + x, area.y + 11).bg,
                           theme.chip_dismiss, "{slug} close chip cell {x}");
            }
            assert_eq!(buffer.get(area.x + 49, area.y + 11).bg,
                       theme.panel_bg, "{slug} close chip ends at its fixed width");

            state.help = true;
            terminal.draw(|frame| draw(frame, &mut state, &config, &mut map, theme)).unwrap();
            let window_help = Rect::new(12, 6, 56, 12);
            let buffer = terminal.backend().buffer();
            for y in 1..window_help.height - 1 {
                let cell = buffer.get(window_help.right() - 1, window_help.y + y);
                assert_eq!(cell.symbol(), "\u{2502}", "{slug} window help row {y}");
            }
            state.help = false;
            state.filenames = Some(FilenamesPopup::new());
            map.clear();
            terminal.draw(|frame| draw(frame, &mut state, &config, &mut map, theme)).unwrap();
            let popup = state.popup_area;
            let buffer = terminal.backend().buffer();
            for y in 0..popup.height {
                let expected = if y == 0 { "\u{2510}" } else if y == popup.height - 1 {
                    "\u{2518}"
                } else if y == 10 { "\u{2524}" } else { "\u{2502}" };
                let cell = buffer.get(popup.right() - 1, popup.y + y);
                assert_eq!(cell.symbol(), expected, "{slug} popup row {y}");
                assert_eq!(cell.fg, readable(theme.title, theme.panel_bg), "{slug} popup border color at row {y}");
            }
            assert_eq!(buffer.get(popup.x + 35, popup.y + 9).symbol(), "A",
                       "{slug} Add must remain visible");

            state.help = true;
            terminal.draw(|frame| draw(frame, &mut state, &config, &mut map, theme)).unwrap();
            let help_height = 11;
            let help = Rect::new(12, (24 - help_height) / 2, 56, help_height);
            let buffer = terminal.backend().buffer();
            for y in 0..help.height {
                let expected = if y == 0 { "\u{2510}" } else if y == help.height - 1 {
                    "\u{2518}"
                } else { "\u{2502}" };
                let cell = buffer.get(help.right() - 1, help.y + y);
                assert_eq!(cell.symbol(), expected, "{slug} all-keys row {y}");
                assert_eq!(cell.fg, readable(theme.title, theme.panel_bg), "{slug} all-keys border color at row {y}");
            }
        }
    }

    #[test]
    fn filename_actions_and_name_field_match_mockup_columns() {
        use ratatui::{backend::TestBackend, Terminal};

        for slug in ["tokyo-night", "tokyo-night-day"] {
            let config = BrowseArtworkConfig::default();
            let theme = super::super::theme::theme_by_slug(slug).unwrap();
            let mut state = ArtworkSettingsState::new(&config, Rect::default());
            let mut popup = FilenamesPopup::new();
            popup.name = "cover.webp".to_owned();
            state.filenames = Some(popup);
            let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
            let mut map = ButtonRenderMap::default();
            terminal.draw(|frame| draw(frame, &mut state, &config, &mut map, theme)).unwrap();
            let area = state.popup_area;
            let buffer = terminal.backend().buffer();
            for (index, row, first_letter, width) in [
                (0u8, 3, "M", 11), (1, 4, "M", 11), (2, 5, "R", 11),
                (3, 7, "R", 18), (4, 9, "A", 11),
            ] {
                assert_eq!(buffer.get(area.x + 35, area.y + row).symbol(), first_letter,
                           "{slug} popup action {index} text start");
                assert_eq!(map.button_rect(TuiButton::ArtworkNameAction(index)),
                           Some(Rect::new(area.x + 34, area.y + row, width, 1)),
                           "{slug} popup action {index} hitbox");
            }
            assert_eq!(buffer.get(area.x + 10, area.y + 9).symbol(), "c",
                       "{slug} typed Name text starts at column 10");
            assert_eq!(map.button_rect(TuiButton::ArtworkNameField),
                       Some(Rect::new(area.x + 9, area.y + 9, 21, 1)),
                       "{slug} Name field click target");
            assert_eq!(map.button_rect(TuiButton::ArtworkNameDone),
                       Some(Rect::new(area.x + 47, area.y + 11, 6, 1)),
                       "{slug} Done must not move");
        }
    }

    #[test]
    fn preview_settings_center_and_track_image_overlap_without_moving() {
        use ratatui::{backend::TestBackend, Terminal};
        use super::super::{
            app::{ActiveOverlay, AppScreen},
            context_menu::{execute_context_action, ContextAction},
        };

        let screen = Rect::new(0, 0, 160, 60);
        let small_art = Rect::new(116, 41, 42, 12);
        let expected = Rect::new(42, 23, WIDTH, HEIGHT);

        // Exercise the actual Info-preview action. Calling new_for_preview()
        // directly would miss a regression in the context-menu dispatcher.
        let mut app = AppState::new_for_test(crate::config::TonepoetConfig::default());
        app.current_screen = AppScreen::Browse;
        assert!(!app.browse_artwork.popup_open, "test must open from Info preview");
        app.button_map.record_button(TuiButton::BrowseInfoArtworkArea, small_art);
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        execute_context_action(&mut app, ContextAction::BrowseArtworkOpenSettings, &tx, false);
        let mut preview = match std::mem::replace(&mut app.active_overlay, ActiveOverlay::None) {
            ActiveOverlay::ArtworkSettings(settings) => *settings,
            other => panic!("Settings action did not open the artwork overlay: {other:?}"),
        };
        assert_eq!(preview.anchor, small_art, "use the registered Info artwork rectangle");
        let config = app.browse.artwork_config.clone();
        let mut terminal = Terminal::new(TestBackend::new(screen.width, screen.height)).unwrap();
        let mut map = ButtonRenderMap::default();
        terminal.draw(|frame| draw(frame, &mut preview, &config, &mut map, app.theme)).unwrap();
        assert_eq!(preview.area, expected, "Info context action must open settings centered");
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer.get(expected.x, expected.y).symbol(), "┌");
        assert_eq!(buffer.get(expected.right() - 1, expected.y).symbol(), "┐");
        assert!(!preview.hide_image());
        assert_eq!(preview.popup_area.x, expected.x + (WIDTH - POPUP_WIDTH) / 2);

        // A normal viewer still follows its original artwork-relative policy.
        let mut viewer = ArtworkSettingsState::new(&config, small_art);
        viewer.prepare_geometry(screen);
        assert_ne!(viewer.area, expected);
        assert!(!viewer.hide_image());

        preview.update_anchor(Rect::new(116, 28, 42, 25), screen);
        assert_eq!(preview.area, expected);
        assert!(preview.hide_image());
        preview.update_anchor(Rect::new(116, 43, 42, 10), screen);
        assert_eq!(preview.area, expected);
        assert!(!preview.hide_image());

        let resized = Rect::new(0, 0, 132, 52);
        preview.update_anchor(Rect::new(95, 36, 35, 12), resized);
        assert_eq!(preview.area, Rect::new(28, 19, WIDTH, HEIGHT));
        assert!(!preview.hide_image());
    }

    #[test]
    fn summary_keeps_names_whole_and_reserves_remaining_count() {
        let names = vec!["cover.jpg", "folder.jpg", "front.jpg", "cover.png", "album.jpg"]
            .into_iter().map(str::to_owned).collect::<Vec<_>>();
        assert_eq!(summary(&names, 41), "cover.jpg · folder.jpg · front.jpg +2");
        let shipped = BrowseArtworkConfig::default();
        assert_eq!(shipped.pane_height_percent, 28);
        assert_eq!(shipped.popup_height_percent, 60);
        assert_eq!(shipped.filenames.len(), 6);
    }

    #[test]
    fn geometry_is_stable_and_popup_fits_at_minimum_size() {
        let mut state = ArtworkSettingsState::new(&BrowseArtworkConfig::default(), Rect::default());
        state.geometry(Rect::new(0, 0, 80, 17));
        assert_eq!(state.area, Rect::new(2, 2, WIDTH, HEIGHT));
        assert_eq!(state.popup_area, Rect::new(12, 4, POPUP_WIDTH, POPUP_HEIGHT));
        state.geometry(Rect::new(0, 0, 80, 17));
        assert_eq!(state.area.x, 2);
        state.geometry(Rect::new(0, 0, 79, 17));
        assert_eq!(state.area, Rect::default());
    }

    #[test]
    fn filename_popup_scroll_and_focus_do_not_drop_items() {
        let mut popup = FilenamesPopup::new();
        popup.cursor = 31;
        popup.keep_visible(32);
        assert_eq!(popup.scroll, 26);
        popup.cursor = 0;
        popup.keep_visible(32);
        assert_eq!(popup.scroll, 0);
        assert_eq!(PopupFocus::Done.step(false), PopupFocus::List);
    }

    #[test]
    fn rendered_window_has_fixed_controls_in_dark_and_day_themes() {
        use ratatui::{backend::TestBackend, Terminal};

        for slug in ["tokyo-night", "tokyo-night-day"] {
            let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
            let mut config = BrowseArtworkConfig::default();
            let mut state = ArtworkSettingsState::new(&config, Rect::default());
            let mut map = ButtonRenderMap::default();
            let theme = super::super::theme::theme_by_slug(slug).unwrap();
            terminal.draw(|frame| draw(frame, &mut state, &config, &mut map, theme)).unwrap();
            let window = state.area;
            assert_eq!(window, Rect::new(2, 5, WIDTH, HEIGHT), "{slug}");
            let buffer = terminal.backend().buffer();
            let symbol = |col, row| buffer.get(window.x + col, window.y + row).symbol();
            for row in [4, 10] {
                assert_eq!(symbol(0, row), "├", "{slug} divider at row {row}");
                assert_eq!(symbol(WIDTH - 1, row), "┤", "{slug}");
            }
            assert_eq!(symbol(0, HEIGHT - 1), "└", "{slug}");
            assert_eq!(symbol(WIDTH - 1, HEIGHT - 1), "┘", "{slug}");
            assert_eq!(symbol(3, 2), "☑", "{slug}");
            assert_eq!(symbol(3, 6), "☐", "{slug}");
            assert_eq!(symbol(16, 8), "◂", "{slug}");
            assert_eq!(symbol(23, 8), "%", "{slug}");
            assert_eq!(symbol(25, 8), "▸", "{slug}");
            assert_eq!(symbol(27, 8), "█", "{slug}");
            assert_eq!(symbol(30, 8), "░", "{slug}");
            assert_eq!(symbol(27, 9), "█", "{slug}");
            assert_eq!(symbol(33, 9), "░", "{slug}");
            assert_eq!(map.button_rect(TuiButton::ArtworkHeightField(0)),
                       Some(Rect::new(window.x + 18, window.y + 8, 5, 1)));
            assert_eq!(map.button_rect(TuiButton::ArtworkFilenamesEdit),
                       Some(Rect::new(window.x + 66, window.y + 3, 7, 1)));
            assert_eq!(map.button_rect(TuiButton::ArtworkClose),
                       Some(Rect::new(window.x + 38, window.y + 11, 11, 1)));
            assert!(map.button_rect(TuiButton::ArtworkRevert).is_none(), "{slug}");

            config.pane_height_percent = 65;
            map.clear();
            terminal.draw(|frame| draw(frame, &mut state, &config, &mut map, theme)).unwrap();
            assert_eq!(map.button_rect(TuiButton::ArtworkRevert),
                       Some(Rect::new(window.x + 26, window.y + 11, 10, 1)));
            assert_eq!(map.button_rect(TuiButton::ArtworkClose),
                       Some(Rect::new(window.x + 38, window.y + 11, 11, 1)));
            let buffer = terminal.backend().buffer();
            assert_eq!(buffer.get(window.x + 33, window.y + 8).symbol(), "█",
                       "65% must fill seven cells in {slug}");
        }
    }

    #[test]
    fn filenames_popup_hitboxes_are_stable_in_armed_state() {
        use ratatui::{backend::TestBackend, Terminal};

        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let config = BrowseArtworkConfig::default();
        let mut state = ArtworkSettingsState::new(&config, Rect::default());
        state.filenames = Some(FilenamesPopup::new());
        let mut map = ButtonRenderMap::default();
        let theme = super::super::theme::theme_by_slug_or_default("tokyo-night");
        terminal.draw(|frame| draw(frame, &mut state, &config, &mut map, theme)).unwrap();
        assert_eq!(state.popup_area, Rect::new(12, 7, POPUP_WIDTH, POPUP_HEIGHT));
        let restore = Rect::new(46, 14, 18, 1);
        assert_eq!(map.button_rect(TuiButton::ArtworkNameAction(3)), Some(restore));
        assert_eq!(map.button_rect(TuiButton::ArtworkNameField),
                   Some(Rect::new(21, 16, 21, 1)));
        assert_eq!(map.button_rect(TuiButton::ArtworkNameDone),
                   Some(Rect::new(59, 18, 6, 1)));
        state.filenames.as_mut().unwrap().restore_armed = true;
        map.clear();
        terminal.draw(|frame| draw(frame, &mut state, &config, &mut map, theme)).unwrap();
        assert_eq!(map.button_rect(TuiButton::ArtworkNameAction(3)), Some(restore));
    }

    #[test]
    fn filename_name_field_keeps_last_character_visible_in_both_themes() {
        use ratatui::{backend::TestBackend, Terminal};

        let name = "original_cover.jpeg";
        assert_eq!(display_width::width(name), 19);
        for slug in ["tokyo-night", "tokyo-night-day"] {
            let config = BrowseArtworkConfig::default();
            let mut state = ArtworkSettingsState::new(&config, Rect::default());
            let mut popup = FilenamesPopup::new();
            popup.focus = PopupFocus::Name;
            popup.name = name.to_owned();
            state.filenames = Some(popup);
            let mut map = ButtonRenderMap::default();
            let theme = super::super::theme::theme_by_slug(slug).unwrap();
            let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
            terminal.draw(|frame| draw(frame, &mut state, &config, &mut map, theme)).unwrap();

            let area = state.popup_area;
            let buffer = terminal.backend().buffer();
            let displayed = (10..29)
                .map(|col| buffer.get(area.x + col, area.y + 9).symbol())
                .collect::<Vec<_>>().join("");
            assert_eq!(displayed, name, "{slug}");
            let caret = buffer.get(area.x + 29, area.y + 9);
            assert_eq!(caret.symbol(), " ", "{slug}");
            assert_eq!(caret.bg, theme.editing_cursor, "{slug}");
            assert_eq!(map.button_rect(TuiButton::ArtworkNameField),
                       Some(Rect::new(area.x + 9, area.y + 9, 21, 1)), "{slug}");
        }
    }

    #[test]
    fn height_bar_half_up_and_endpoints() {
        for (percent, cells) in [(1u8, 0), (28, 3), (60, 6), (65, 7), (100, 10)] {
            assert_eq!((f64::from(percent) / 10.0).round() as u16, cells);
        }
    }

    #[test]
    fn backtab_traverses_regions_height_edits_and_filename_controls() {
        let mut app = AppState::new_for_test(crate::config::TonepoetConfig::default());
        let mut state = ArtworkSettingsState::new(&app.browse.artwork_config, Rect::default());
        let backtab = KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT);
        let tab = KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);

        handle_key(&mut app, &mut state, tab);
        assert_eq!(state.focus, 2);
        handle_key(&mut app, &mut state, backtab);
        assert_eq!(state.focus, 0);
        handle_key(&mut app, &mut state,
                   KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT));
        assert_eq!(state.focus, 2);
        handle_key(&mut app, &mut state, tab);
        assert_eq!(state.focus, 0);

        for which in 0..2 {
            edit_height(&mut state, &app.browse.artwork_config, which, None);
            assert!(state.height_edit.is_some());
            handle_key(&mut app, &mut state, backtab);
            assert_eq!(state.focus, 0);
            assert!(state.height_edit.is_none());
            edit_height(&mut state, &app.browse.artwork_config, which, None);
            handle_key(&mut app, &mut state, tab);
            assert_eq!(state.focus, 0);
            assert!(state.height_edit.is_none());
            edit_height(&mut state, &app.browse.artwork_config, which, None);
            handle_key(&mut app, &mut state,
                       KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT));
            assert_eq!(state.focus, 0);
            assert!(state.height_edit.is_none());
        }
        assert_eq!(app.browse.artwork_config.pane_height_percent, 28);
        assert_eq!(app.browse.artwork_config.popup_height_percent, 60);

        state.filenames = Some(FilenamesPopup::new());
        state.filenames.as_mut().unwrap().focus = PopupFocus::Done;
        popup_key(&mut app, &mut state, backtab);
        assert_eq!(state.filenames.as_ref().unwrap().focus, PopupFocus::Add);
        state.filenames.as_mut().unwrap().focus = PopupFocus::Name;
        popup_key(&mut app, &mut state, backtab);
        assert_eq!(state.filenames.as_ref().unwrap().focus, PopupFocus::List);
        popup_key(&mut app, &mut state, backtab);
        assert_eq!(state.filenames.as_ref().unwrap().focus, PopupFocus::Done);
        popup_key(&mut app, &mut state, tab);
        assert_eq!(state.filenames.as_ref().unwrap().focus, PopupFocus::List);
    }

    #[test]
    fn height_click_reselects_pending_text_without_cancelling() {
        use ratatui::{backend::TestBackend, Terminal};

        let mut app = AppState::new_for_test(crate::config::TonepoetConfig::default());
        let mut state = ArtworkSettingsState::new(&app.browse.artwork_config, Rect::default());
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| draw(frame, &mut state,
            &app.browse.artwork_config, &mut app.button_map, app.theme)).unwrap();
        let field = app.button_map.button_rect(TuiButton::ArtworkHeightField(0)).unwrap();
        edit_height(&mut state, &app.browse.artwork_config, 0, None);
        let pending = state.height_edit.as_mut().unwrap();
        pending.text = "65".to_owned();
        pending.selected_all = false;
        handle_mouse(&mut app, &mut state, MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: field.x + 2, row: field.y,
            modifiers: KeyModifiers::NONE,
        });
        let edit = state.height_edit.as_ref().unwrap();
        assert_eq!(edit.text, "65");
        assert!(edit.selected_all);
        assert_eq!(edit.text.parse::<u8>().unwrap(), 65);
        assert_eq!(app.browse.artwork_config.pane_height_percent, 28);
        handle_key(&mut app, &mut state, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(state.height_edit.is_none());
        assert_eq!(app.browse.artwork_config.pane_height_percent, 28);
    }

    #[test]
    fn modal_frames_clear_prior_symbols_in_both_themes() {
        use ratatui::{backend::TestBackend, Terminal};
        let background = vec!["Z".repeat(80); 24].join("\n");
        for slug in ["tokyo-night", "tokyo-night-day"] {
            let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
            let config = BrowseArtworkConfig::default();
            let mut state = ArtworkSettingsState::new(&config, Rect::default());
            let theme = super::super::theme::theme_by_slug(slug).unwrap();
            let mut map = ButtonRenderMap::default();
            let backdrop = |frame: &mut Frame| {
                frame.render_widget(Paragraph::new(background.as_str()).style(
                    Style::default().fg(theme.value).bg(theme.bg)), frame.size());
            };
            terminal.draw(|frame| {
                backdrop(frame);
                draw(frame, &mut state, &config, &mut map, theme);
            }).unwrap();
            let window = state.area;
            let buffer = terminal.backend().buffer();
            assert_eq!(buffer.get(window.x + 60, window.y + 1).symbol(), " ", "{slug}");
            assert_eq!(buffer.get(0, 0).symbol(), "Z", "{slug}");
            assert_eq!(buffer.get(0, 0).fg, theme.text_dim, "{slug}");

            state.filenames = Some(FilenamesPopup::new());
            map.clear();
            terminal.draw(|frame| {
                backdrop(frame);
                draw(frame, &mut state, &config, &mut map, theme);
            }).unwrap();
            let popup = state.popup_area;
            let buffer = terminal.backend().buffer();
            assert_eq!(buffer.get(popup.x + 33, popup.y + 2).symbol(), " ", "{slug} popup");
            assert_eq!(buffer.get(popup.x + 30, popup.y + 6).symbol(), " ", "{slug} list");

            state.help = true;
            terminal.draw(|frame| {
                backdrop(frame);
                draw(frame, &mut state, &config, &mut map, theme);
            }).unwrap();
            let help_width = 56;
            let help_height = 11; // Seven filename help rows plus frame and padding.
            let help_x = (80 - help_width) / 2;
            let help_y = (24 - help_height) / 2;
            let buffer = terminal.backend().buffer();
            assert_eq!(buffer.get(help_x + 43, help_y + 1).symbol(), " ", "{slug} help");
            assert_eq!(buffer.get(0, 0).symbol(), "Z", "{slug} outside");
        }
    }

    #[test]
    fn backdrop_dims_text_but_preserves_visible_art_and_native_commands() {
        use ratatui::{backend::TestBackend, Terminal};
        let terminal_area = Rect::new(0, 0, 180, 55);
        let image = Rect::new(140, 18, 25, 12);
        let config = BrowseArtworkConfig::default();
        let mut state = ArtworkSettingsState::new(&config, image);
        let mut terminal = Terminal::new(TestBackend::new(180, 55)).unwrap();
        let theme = super::super::theme::theme_by_slug_or_default("tokyo-night");
        let mut map = ButtonRenderMap::default();
        terminal.draw(|frame| {
            frame.render_widget(Paragraph::new(vec!["Z".repeat(180); 55].join("\n"))
                .style(Style::default().fg(theme.value)), terminal_area);
            frame.buffer_mut().get_mut(170, 2).set_symbol("\x1b_Ga=T;AA\x1b\\");
            draw(frame, &mut state, &config, &mut map, theme);
        }).unwrap();
        assert!(!state.hide_image());
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer.get(2, 2).fg, theme.text_dim);
        assert_eq!(buffer.get(image.x + 1, image.y + 1).fg, theme.value);
        assert_eq!(buffer.get(170, 2).fg, theme.value);
        assert_eq!(buffer.get(170, 2).symbol(), "\x1b_Ga=T;AA\x1b\\");
    }

    #[test]
    fn changing_artwork_rect_reflows_only_when_needed() {
        let config = BrowseArtworkConfig::default();
        let screen = Rect::new(0, 0, 120, 60);
        let mut info = ArtworkSettingsState::new(&config, Rect::default());
        info.update_anchor(Rect::new(30, 38, 60, 10), screen);
        let initial = info.area;
        assert!(!info.hide_image());
        assert_eq!(initial.y, 24);
        info.update_anchor(Rect::new(30, 20, 60, 28), screen);
        assert!(!info.hide_image());
        assert_eq!(info.area.y, 6); // The old modal would overlap the grown image.
        info.update_anchor(Rect::new(30, 20, 60, 29), screen);
        assert_eq!(info.area.y, 6); // Keep a still-valid window position.
        info.update_anchor(Rect::new(20, 1, 80, 56), screen);
        assert!(info.hide_image());
        info.update_anchor(Rect::new(30, 44, 60, 8), screen);
        assert!(!info.hide_image()); // A shrunken image is visible again.
        assert_eq!(info.area.y, 30);
        info.update_anchor(Rect::new(20, 28, 60, 8), Rect::new(0, 0, 100, 44));
        assert!(info.area.right() <= 100 && info.area.bottom() <= 44);
        assert!(!info.hide_image());

        let viewer_screen = Rect::new(0, 0, 120, 50);
        let mut viewer = ArtworkSettingsState::new(&config, Rect::default());
        viewer.update_anchor(Rect::new(20, 7, 80, 36), viewer_screen);
        assert!(viewer.hide_image());
        viewer.update_anchor(Rect::new(20, 21, 80, 10), viewer_screen);
        assert!(!viewer.hide_image());
        assert_eq!(viewer.area.y, 7);
        viewer.update_anchor(Rect::new(20, 10, 80, 28), viewer_screen);
        assert!(viewer.hide_image());
    }

    #[test]
    fn restore_confirmation_disarms_after_wheel_and_help() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut app = AppState::new_for_test(crate::config::TonepoetConfig::default());
        app.browse.artwork_config.filenames.push("custom.png".to_owned());
        let mut state = ArtworkSettingsState::new(&app.browse.artwork_config, Rect::default());
        state.filenames = Some(FilenamesPopup::new());
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| draw(frame, &mut state,
            &app.browse.artwork_config, &mut app.button_map, app.theme)).unwrap();
        let restore = app.button_map.button_rect(TuiButton::ArtworkNameAction(3)).unwrap();
        let help = app.button_map.button_rect(TuiButton::ArtworkNameHelp).unwrap();
        let popup_area = state.popup_area;
        let mouse = |kind, area: Rect| MouseEvent {
            kind, column: area.x, row: area.y, modifiers: KeyModifiers::NONE,
        };
        popup_action(&mut app, &mut state, PopupFocus::Restore);
        assert!(state.filenames.as_ref().unwrap().restore_armed);
        handle_mouse(&mut app, &mut state,
            mouse(MouseEventKind::ScrollDown, popup_area));
        assert_eq!(state.filenames.as_ref().unwrap().cursor, 1);
        assert!(!state.filenames.as_ref().unwrap().restore_armed);
        handle_mouse(&mut app, &mut state,
            mouse(MouseEventKind::Down(MouseButton::Left), restore));
        assert!(state.filenames.as_ref().unwrap().restore_armed);
        assert!(app.browse.artwork_config.filenames.contains(&"custom.png".to_owned()));
        handle_mouse(&mut app, &mut state,
            mouse(MouseEventKind::Down(MouseButton::Left), help));
        assert!(state.help);
        assert!(!state.filenames.as_ref().unwrap().restore_armed);
        handle_key(&mut app, &mut state, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!state.help);
        handle_mouse(&mut app, &mut state,
            mouse(MouseEventKind::Down(MouseButton::Left), restore));
        assert!(state.filenames.as_ref().unwrap().restore_armed);
        assert!(app.browse.artwork_config.filenames.contains(&"custom.png".to_owned()));

        // With no intervening interaction, two consecutive presses confirm.
        // The shipped list needs no disk write, keeping this test isolated.
        let mut defaults = AppState::new_for_test(crate::config::TonepoetConfig::default());
        let mut confirmation = ArtworkSettingsState::new(
            &defaults.browse.artwork_config, Rect::default());
        confirmation.filenames = Some(FilenamesPopup::new());
        popup_action(&mut defaults, &mut confirmation, PopupFocus::Restore);
        assert!(confirmation.filenames.as_ref().unwrap().restore_armed);
        popup_action(&mut defaults, &mut confirmation, PopupFocus::Restore);
        assert!(!confirmation.filenames.as_ref().unwrap().restore_armed);
        assert_eq!(defaults.browse.artwork_config.filenames,
                   BrowseArtworkConfig::default().filenames);
    }
}
