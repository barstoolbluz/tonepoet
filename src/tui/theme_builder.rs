//! Interactive theme builder overlay.
//!
//! The builder owns an editable copy of the 27 public palette inputs, exposes
//! derived colors as separately lockable elements, and resolves the three-layer
//! theme cascade without mutating built-in palettes.

use std::cell::Cell;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use super::button_map::{ButtonRenderMap, TuiButton};
use super::text_input::{handle_text_input_key, TextInputState};
use super::theme::{
    self, BuilderSlot, ColorDepth, NamedSwatch, ThemeApplyOptions, ThemeOverrides,
    ThemePaletteDraft, ThemeDraftSource, ROLE_KEYS, ROLE_LABELS,
};

const MAX_RECENT_COLORS: usize = 12;
const MAX_SAVED_SWATCHES: usize = 12;
const DEFAULT_DERIVED_VISIBLE_ROWS: usize = 14;
const DEFAULT_PRESET_VISIBLE_ROWS: usize = 12;
const DEFAULT_GALLERY_VISIBLE_ROWS: usize = 18;

#[derive(Debug, Clone)]
pub struct ThemeBuilderState {
    pub palette: ThemePaletteDraft,
    pub selected_slot: BuilderSlot,
    pub hex_input: TextInputState,
    pub rgb_values: [u8; 3],
    pub depth_mode: ColorDepth,
    pub recent_colors: Vec<Color>,
    pub saved_swatch_cursor: usize,
    pub recent_swatch_cursor: usize,
    pub swatch_name_input: TextInputState,
    pub user_overrides: ThemeOverrides,
    pub view: ThemeBuilderView,
    pub editor_focus: BuilderEditorFocus,
    pub derived_cursor: usize,
    pub derived_scroll: usize,
    pub derived_visible_rows: Cell<usize>,
    pub derived_hex_input: TextInputState,
    pub lock_target: DerivedLockTarget,
    pub apply_dialog: ApplyDialogState,
    pub preset_cursor: usize,
    pub preset_scroll: usize,
    pub preset_visible_rows: Cell<usize>,
    /// Cached theme-library snapshot used by the preset dropdown/gallery.
    /// The renderer reads this snapshot so visible rows do not trigger
    /// repeated filesystem scans or per-row theme-file parsing.
    pub theme_library: Vec<theme::ThemeChoice>,
    /// True when the preset list is opened as a standalone theme gallery from
    /// Config. Selecting a row applies that library theme directly instead of
    /// importing it as an editable builder draft.
    pub preset_applies_on_select: bool,
    pub dirty: bool,
    pub status: Option<String>,
    pub deleted_theme_slug: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeBuilderView {
    Main,
    Preset,
    Derived,
    Apply,
    DeleteConfirm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuilderEditorFocus {
    Slots,
    Hex,
    Red,
    Green,
    Blue,
    Depth,
    SwatchName,
    SavedSwatches,
    RecentSwatches,
}

impl BuilderEditorFocus {
    fn next(self) -> Self {
        match self {
            Self::Slots => Self::Hex,
            Self::Hex => Self::Red,
            Self::Red => Self::Green,
            Self::Green => Self::Blue,
            Self::Blue => Self::Depth,
            Self::Depth => Self::SwatchName,
            Self::SwatchName => Self::SavedSwatches,
            Self::SavedSwatches => Self::RecentSwatches,
            Self::RecentSwatches => Self::Slots,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DerivedLockTarget {
    ThemeAuthor,
    UserOverride,
}

impl DerivedLockTarget {
    fn toggle(self) -> Self {
        match self {
            Self::ThemeAuthor => Self::UserOverride,
            Self::UserOverride => Self::ThemeAuthor,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::ThemeAuthor => "theme author",
            Self::UserOverride => "you",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ApplyDialogState {
    pub honor_theme_locks: bool,
    pub keep_user_overrides: bool,
    pub focus: ApplyDialogFocus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyDialogFocus {
    ThemeLocks,
    UserOverrides,
    Apply,
}

impl ApplyDialogFocus {
    fn next(self) -> Self {
        match self {
            Self::ThemeLocks => Self::UserOverrides,
            Self::UserOverrides => Self::Apply,
            Self::Apply => Self::ThemeLocks,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThemeBuilderAction {
    None,
    Close,
    Save,
    Apply,
    /// Apply the selected library theme directly. Used by Config's standalone
    /// Browse-all gallery so the builder does not convert built-ins into
    /// unsaved `*-custom` drafts merely to switch themes.
    ApplyPreset(String),
    Status(String),
}

impl ThemeBuilderState {
    pub fn from_active_theme(theme: theme::Theme) -> Self {
        Self::from_active_theme_with_library(theme, theme::theme_choices())
    }

    pub fn from_active_theme_with_library(
        theme: theme::Theme,
        choices: Vec<theme::ThemeChoice>,
    ) -> Self {
        let mut palette = if let Ok(draft) = theme::load_theme_draft(theme.slug) {
            draft
        } else {
            ThemePaletteDraft::from_theme(theme)
        };
        if matches!(palette.source, ThemeDraftSource::BuiltIn) {
            palette.slug = format!("{}-custom", palette.slug);
            palette.name = format!("{} Custom", palette.name);
            palette.source = ThemeDraftSource::NewCustom;
        }
        Self::from_palette_with_library(palette, choices)
    }

    pub fn theme_gallery_from_active_theme(theme: theme::Theme, selected: usize) -> Self {
        let choices = theme::theme_choices();
        Self::theme_gallery_from_active_theme_with_library(theme, selected, choices)
    }

    pub fn theme_gallery_from_active_theme_with_library(
        theme: theme::Theme,
        selected: usize,
        choices: Vec<theme::ThemeChoice>,
    ) -> Self {
        let choices_len = choices.len();
        let mut state = Self::from_active_theme_with_library(theme, choices);
        state.view = ThemeBuilderView::Preset;
        state.preset_applies_on_select = true;
        state.preset_cursor = selected.min(choices_len.saturating_sub(1));
        state.preset_visible_rows.set(DEFAULT_GALLERY_VISIBLE_ROWS);
        sync_preset_scroll(&mut state, DEFAULT_GALLERY_VISIBLE_ROWS);
        state.status = Some("Select a theme to apply it".to_string());
        state
    }

    pub fn from_palette(palette: ThemePaletteDraft) -> Self {
        Self::from_palette_with_library(palette, theme::theme_choices())
    }

    pub fn from_palette_with_library(
        palette: ThemePaletteDraft,
        theme_library: Vec<theme::ThemeChoice>,
    ) -> Self {
        let selected_slot = BuilderSlot::Role(0);
        let color = palette.color_at_slot(selected_slot);
        let (r, g, b) = theme::rgb_tuple(color);
        let user_overrides = ThemeOverrides::load_default().unwrap_or_default();
        let derived_key = selected_derived_key(0);
        let derived_theme = theme::preview_theme_from_draft(&palette);
        let derived_color = theme::theme_color_by_derived_key(derived_theme, derived_key)
            .unwrap_or(color);
        Self {
            palette,
            selected_slot,
            hex_input: TextInputState::new_selected(theme::color_to_hex(color)),
            rgb_values: [r, g, b],
            depth_mode: ColorDepth::TrueColor,
            recent_colors: Vec::new(),
            saved_swatch_cursor: 0,
            recent_swatch_cursor: 0,
            swatch_name_input: TextInputState::new_selected(default_swatch_name_for_slot(selected_slot).to_string()),
            user_overrides,
            view: ThemeBuilderView::Main,
            editor_focus: BuilderEditorFocus::Slots,
            derived_cursor: 0,
            derived_scroll: 0,
            derived_visible_rows: Cell::new(DEFAULT_DERIVED_VISIBLE_ROWS),
            derived_hex_input: TextInputState::new_selected(theme::color_to_hex(derived_color)),
            lock_target: DerivedLockTarget::ThemeAuthor,
            apply_dialog: ApplyDialogState {
                honor_theme_locks: true,
                keep_user_overrides: true,
                focus: ApplyDialogFocus::ThemeLocks,
            },
            preset_cursor: 0,
            preset_scroll: 0,
            preset_visible_rows: Cell::new(0),
            theme_library,
            preset_applies_on_select: false,
            dirty: false,
            status: None,
            deleted_theme_slug: None,
        }
    }

    pub fn refresh_theme_library(&mut self) {
        self.theme_library = theme::theme_choices();
        sync_preset_scroll(self, preset_visible_rows_for_state(self));
    }

    pub fn replace_theme_library(&mut self, choices: Vec<theme::ThemeChoice>) {
        self.theme_library = choices;
        self.preset_cursor = self.preset_cursor.min(self.theme_library.len().saturating_sub(1));
        sync_preset_scroll(self, preset_visible_rows_for_state(self));
    }

    pub fn apply_options(&self) -> ThemeApplyOptions {
        ThemeApplyOptions {
            honor_theme_locks: self.apply_dialog.honor_theme_locks,
            keep_user_overrides: self.apply_dialog.keep_user_overrides,
        }
    }

    pub fn resolved_theme(&self) -> theme::Theme {
        theme::resolve_theme_draft_for_depth(
            &self.palette,
            self.apply_options(),
            &self.user_overrides,
            self.depth_mode,
        )
    }

    pub fn selected_color(&self) -> Color {
        self.palette.color_at_slot(self.selected_slot)
    }

    fn set_selected_color(&mut self, color: Color) {
        let previous = self.selected_color();
        self.palette.set_color_at_slot(self.selected_slot, color);
        self.sync_hex_and_rgb_from_slot();
        if previous != color {
            self.push_recent(previous);
            self.dirty = true;
        }
    }

    fn sync_hex_and_rgb_from_slot(&mut self) {
        let color = self.selected_color();
        self.hex_input = TextInputState::new_selected(theme::color_to_hex(color));
        let (r, g, b) = theme::rgb_tuple(color);
        self.rgb_values = [r, g, b];
    }

    fn set_selected_slot(&mut self, slot: BuilderSlot) {
        self.selected_slot = slot;
        self.sync_hex_and_rgb_from_slot();
        if let Some(name) = self.palette.slot_binding_name(slot) {
            self.swatch_name_input = TextInputState::new_selected(name.to_string());
        } else if self.swatch_name_input.text.trim().is_empty() {
            self.swatch_name_input = TextInputState::new_selected(default_swatch_name_for_slot(slot).to_string());
        }
    }

    fn push_recent(&mut self, color: Color) {
        if self.recent_colors.first().copied() == Some(color) {
            return;
        }
        self.recent_colors.retain(|existing| *existing != color);
        self.recent_colors.insert(0, color);
        self.recent_colors.truncate(MAX_RECENT_COLORS);
        self.recent_swatch_cursor = self.recent_swatch_cursor.min(self.recent_colors.len().saturating_sub(1));
    }

    fn adjust_rgb_channel(&mut self, channel: usize, delta: i16) {
        let idx = channel.min(2);
        let next = (i16::from(self.rgb_values[idx]) + delta).clamp(0, 255) as u8;
        if self.rgb_values[idx] != next {
            self.rgb_values[idx] = next;
            self.set_selected_color(theme::color_from_rgb_tuple((
                self.rgb_values[0],
                self.rgb_values[1],
                self.rgb_values[2],
            )));
        }
    }

    fn selected_derived_key(&self) -> &'static str {
        selected_derived_key(self.derived_cursor)
    }

    fn sync_derived_hex_from_selected(&mut self) {
        let key = self.selected_derived_key();
        let color = self.user_overrides.overrides.get(key)
            .or_else(|| self.palette.derived_locks.get(key))
            .copied()
            .unwrap_or_else(|| {
                let auto = theme::preview_resolve_theme_draft_for_depth(
                    &self.palette,
                    ThemeApplyOptions { honor_theme_locks: false, keep_user_overrides: false },
                    &ThemeOverrides::default(),
                    self.depth_mode,
                );
                theme::theme_color_by_derived_key(auto, key).unwrap_or(self.selected_color())
            });
        self.derived_hex_input = TextInputState::new_selected(theme::color_to_hex(color));
    }

    fn active_derived_map_mut(&mut self) -> &mut std::collections::BTreeMap<String, Color> {
        match self.lock_target {
            DerivedLockTarget::ThemeAuthor => &mut self.palette.derived_locks,
            DerivedLockTarget::UserOverride => &mut self.user_overrides.overrides,
        }
    }

    fn lock_selected_derived(&mut self) {
        let key = self.selected_derived_key().to_string();
        let color = if let Ok(color) = theme::parse_hex_color(&self.derived_hex_input.text) {
            color
        } else {
            let auto = theme::preview_resolve_theme_draft_for_depth(
                &self.palette,
                ThemeApplyOptions { honor_theme_locks: false, keep_user_overrides: false },
                &ThemeOverrides::default(),
                self.depth_mode,
            );
            theme::theme_color_by_derived_key(auto, &key).unwrap_or(self.selected_color())
        };
        self.active_derived_map_mut().insert(key, color);
        self.dirty = true;
        self.sync_derived_hex_from_selected();
    }

    fn release_selected_derived(&mut self) {
        let key = self.selected_derived_key().to_string();
        let removed = match self.lock_target {
            DerivedLockTarget::ThemeAuthor => self.palette.derived_locks.remove(&key).is_some(),
            DerivedLockTarget::UserOverride => self.user_overrides.overrides.remove(&key).is_some(),
        };
        if removed {
            self.dirty = true;
        }
        self.sync_derived_hex_from_selected();
    }

    fn apply_hex_to_selected_slot(&mut self) -> Result<(), String> {
        match theme::parse_hex_color(&self.hex_input.text) {
            Ok(color) => {
                self.set_selected_color(color);
                self.status = None;
                Ok(())
            }
            Err(err) => {
                let message = err.to_string();
                self.status = Some(message.clone());
                Err(message)
            }
        }
    }

    fn apply_hex_to_selected_derived(&mut self) -> Result<(), String> {
        match theme::parse_hex_color(&self.derived_hex_input.text) {
            Ok(color) => {
                let key = self.selected_derived_key().to_string();
                self.active_derived_map_mut().insert(key, color);
                self.dirty = true;
                self.status = None;
                Ok(())
            }
            Err(err) => {
                let message = err.to_string();
                self.status = Some(message.clone());
                Err(message)
            }
        }
    }

    fn save_current_swatch(&mut self) {
        let color = self.selected_color();
        let fallback = default_swatch_name_for_slot(self.selected_slot);
        let requested = sanitize_swatch_name(&self.swatch_name_input.text)
            .unwrap_or_else(|| fallback.to_string());

        if let Some(index) = self.palette.swatches.iter().position(|swatch| swatch.name == requested) {
            self.palette.update_swatch_color(&requested, color);
            let _ = self.palette.bind_slot_to_swatch(self.selected_slot, &requested);
            self.saved_swatch_cursor = index;
            self.sync_hex_and_rgb_from_slot();
            self.status = Some(format!("Updated swatch {requested}; bound selected slot"));
            self.dirty = true;
            return;
        }

        if self.palette.swatches.len() >= MAX_SAVED_SWATCHES {
            self.status = Some("Saved swatch library is full; delete one first".to_string());
            return;
        }

        self.palette.swatches.push(NamedSwatch::new(requested.clone(), color));
        let _ = self.palette.bind_slot_to_swatch(self.selected_slot, &requested);
        self.saved_swatch_cursor = self.palette.swatches.len().saturating_sub(1);
        self.swatch_name_input = TextInputState::new_selected(requested.clone());
        self.sync_hex_and_rgb_from_slot();
        self.status = Some(format!("Saved and bound swatch {requested}"));
        self.dirty = true;
    }

    fn delete_selected_swatch(&mut self) {
        if self.palette.swatches.is_empty() {
            self.status = Some("No saved swatch to delete".to_string());
            return;
        }
        let index = self.saved_swatch_cursor.min(self.palette.swatches.len() - 1);
        let removed = self.palette.remove_swatch_at(index).expect("index checked above");
        self.saved_swatch_cursor = self.saved_swatch_cursor.min(self.palette.swatches.len().saturating_sub(1));
        self.status = Some(format!("Deleted swatch {}; existing bound slots kept their current colors", removed.name));
        self.dirty = true;
    }

    fn apply_saved_swatch(&mut self, index: usize) {
        if let Some(swatch) = self.palette.swatches.get(index).cloned() {
            let previous = self.selected_color();
            self.saved_swatch_cursor = index;
            self.swatch_name_input = TextInputState::new_selected(swatch.name.clone());
            match self.palette.bind_slot_to_swatch(self.selected_slot, &swatch.name) {
                Ok(()) => {
                    if previous != swatch.color {
                        self.push_recent(previous);
                    }
                    self.sync_hex_and_rgb_from_slot();
                    self.status = Some(format!("Bound selected slot to swatch {}", swatch.name));
                    self.dirty = true;
                }
                Err(err) => self.status = Some(format!("Swatch bind failed: {err}")),
            }
        }
    }

    fn apply_recent_swatch(&mut self, index: usize) {
        if let Some(color) = self.recent_colors.get(index).copied() {
            self.recent_swatch_cursor = index;
            self.set_selected_color(color);
            self.status = Some(format!("Applied recent color {}", theme::color_to_hex(color)));
        }
    }

    fn move_saved_swatch_cursor(&mut self, delta: isize) {
        if self.palette.swatches.is_empty() {
            self.saved_swatch_cursor = 0;
            return;
        }
        self.saved_swatch_cursor = move_cursor(self.saved_swatch_cursor, self.palette.swatches.len(), delta);
        if let Some(swatch) = self.palette.swatches.get(self.saved_swatch_cursor) {
            self.swatch_name_input = TextInputState::new_selected(swatch.name.clone());
        }
    }

    fn move_recent_swatch_cursor(&mut self, delta: isize) {
        if self.recent_colors.is_empty() {
            self.recent_swatch_cursor = 0;
            return;
        }
        self.recent_swatch_cursor = move_cursor(self.recent_swatch_cursor, self.recent_colors.len(), delta);
    }

    fn revert_from_disk(&mut self) {
        if !matches!(self.palette.source, ThemeDraftSource::Custom) {
            self.status = Some("Revert is available after this theme has been saved".to_string());
            return;
        }
        match theme::load_custom_theme_by_slug(&self.palette.slug) {
            Ok(draft) => {
                self.palette = draft;
                self.selected_slot = BuilderSlot::Role(0);
                self.saved_swatch_cursor = 0;
                self.swatch_name_input = TextInputState::new_selected(default_swatch_name_for_slot(self.selected_slot).to_string());
                self.sync_hex_and_rgb_from_slot();
                self.sync_derived_hex_from_selected();
                self.dirty = false;
                self.status = Some("Reverted theme from disk".to_string());
            }
            Err(err) => {
                self.status = Some(format!("Revert failed: {err}"));
            }
        }
    }

    fn request_delete_current_custom_theme(&mut self) {
        if !matches!(self.palette.source, ThemeDraftSource::Custom) {
            self.status = Some("Only saved custom themes can be deleted".to_string());
            return;
        }
        self.view = ThemeBuilderView::DeleteConfirm;
        self.status = Some(format!(
            "Confirm deletion of custom theme '{}'",
            self.palette.name
        ));
    }

    fn cancel_delete_current_custom_theme(&mut self) {
        self.view = ThemeBuilderView::Main;
        self.status = Some("Theme deletion canceled".to_string());
    }

    fn confirm_delete_current_custom_theme(&mut self) {
        if !matches!(self.palette.source, ThemeDraftSource::Custom) {
            self.view = ThemeBuilderView::Main;
            self.status = Some("Only saved custom themes can be deleted".to_string());
            return;
        }
        let deleted_slug = self.palette.slug.clone();
        match theme::delete_custom_theme_file(&deleted_slug) {
            Ok(path) => {
                self.palette.source = ThemeDraftSource::NewCustom;
                self.deleted_theme_slug = Some(deleted_slug);
                self.dirty = true;
                self.view = ThemeBuilderView::Main;
                self.status = Some(format!(
                    "Deleted custom theme {}; current edits remain open as unsaved",
                    path.display()
                ));
            }
            Err(err) => {
                self.view = ThemeBuilderView::Main;
                self.status = Some(format!("Delete theme failed: {err}"));
            }
        }
    }

    fn load_preset_at_cursor(&mut self) {
        if self.theme_library.is_empty() {
            return;
        }
        let idx = self.preset_cursor.min(self.theme_library.len() - 1);
        let choice = self.theme_library[idx].clone();
        match theme::load_theme_draft(&choice.slug) {
            Ok(mut draft) => {
                if matches!(draft.source, ThemeDraftSource::BuiltIn) {
                    draft.slug = format!("{}-custom", draft.slug);
                    draft.name = format!("{} Custom", draft.name);
                    draft.source = ThemeDraftSource::NewCustom;
                }
                self.palette = draft;
                self.selected_slot = BuilderSlot::Role(0);
                self.saved_swatch_cursor = 0;
                self.swatch_name_input = TextInputState::new_selected(default_swatch_name_for_slot(self.selected_slot).to_string());
                self.sync_hex_and_rgb_from_slot();
                self.sync_derived_hex_from_selected();
                self.dirty = true;
                self.status = Some(format!("Loaded preset {}", choice.name));
            }
            Err(err) => self.status = Some(format!("Preset load failed: {err}")),
        }
    }
}

pub fn handle_theme_builder_key(state: &mut ThemeBuilderState, key: KeyEvent) -> ThemeBuilderAction {
    match state.view {
        ThemeBuilderView::Main => handle_main_key(state, key),
        ThemeBuilderView::Preset => handle_preset_key(state, key),
        ThemeBuilderView::Derived => handle_derived_key(state, key),
        ThemeBuilderView::Apply => handle_apply_key(state, key),
        ThemeBuilderView::DeleteConfirm => handle_delete_confirm_key(state, key),
    }
}

fn handle_main_key(state: &mut ThemeBuilderState, key: KeyEvent) -> ThemeBuilderAction {
    match (key.code, key.modifiers) {
        (KeyCode::Esc, _) => return ThemeBuilderAction::Close,
        (KeyCode::Char('s'), modifiers) if modifiers.contains(KeyModifiers::CONTROL) => return ThemeBuilderAction::Save,
        (KeyCode::Char('a'), KeyModifiers::NONE) => {
            state.view = ThemeBuilderView::Apply;
            return ThemeBuilderAction::None;
        }
        (KeyCode::Char('d'), KeyModifiers::NONE) => {
            state.view = ThemeBuilderView::Derived;
            state.sync_derived_hex_from_selected();
            return ThemeBuilderAction::None;
        }
        (KeyCode::Char('+'), KeyModifiers::NONE) => {
            state.save_current_swatch();
            return ThemeBuilderAction::None;
        }
        (KeyCode::Char('m'), KeyModifiers::NONE) => {
            state.palette.dark = !state.palette.dark;
            state.dirty = true;
            return ThemeBuilderAction::None;
        }
        (KeyCode::Char('r'), KeyModifiers::NONE) => {
            state.revert_from_disk();
            return ThemeBuilderAction::None;
        }
        (KeyCode::Char('x'), KeyModifiers::NONE) => {
            state.request_delete_current_custom_theme();
            return ThemeBuilderAction::None;
        }
        (KeyCode::Char('p'), KeyModifiers::NONE) => {
            open_preset_dropdown(state);
            return ThemeBuilderAction::None;
        }
        (KeyCode::Tab, _) => {
            state.editor_focus = state.editor_focus.next();
            return ThemeBuilderAction::None;
        }
        _ => {}
    }

    match state.editor_focus {
        BuilderEditorFocus::Slots => match (key.code, key.modifiers) {
            (KeyCode::Up | KeyCode::Char('k'), KeyModifiers::NONE) => state.set_selected_slot(state.selected_slot.previous()),
            (KeyCode::Down | KeyCode::Char('j'), KeyModifiers::NONE) => state.set_selected_slot(state.selected_slot.next()),
            (KeyCode::Left | KeyCode::Char('h'), KeyModifiers::NONE) => state.set_selected_slot(state.selected_slot.previous()),
            (KeyCode::Right | KeyCode::Char('l'), KeyModifiers::NONE) => state.set_selected_slot(state.selected_slot.next()),
            _ => {}
        },
        BuilderEditorFocus::Hex => match key.code {
            KeyCode::Enter => {
                if let Err(message) = state.apply_hex_to_selected_slot() {
                    return ThemeBuilderAction::Status(message);
                }
            }
            _ => {
                if handle_text_input_key(&mut state.hex_input, &key) {
                    if let Ok(color) = theme::parse_hex_color(&state.hex_input.text) {
                        state.set_selected_color(color);
                    }
                }
            }
        },
        BuilderEditorFocus::Red => handle_slider_key(state, key, 0),
        BuilderEditorFocus::Green => handle_slider_key(state, key, 1),
        BuilderEditorFocus::Blue => handle_slider_key(state, key, 2),
        BuilderEditorFocus::Depth => match (key.code, key.modifiers) {
            (KeyCode::Left | KeyCode::Char('h'), KeyModifiers::NONE) => state.depth_mode = state.depth_mode.previous(),
            (KeyCode::Right | KeyCode::Char('l'), KeyModifiers::NONE) => state.depth_mode = state.depth_mode.next(),
            _ => {}
        },
        BuilderEditorFocus::SwatchName => match key.code {
            KeyCode::Enter => state.save_current_swatch(),
            _ => {
                let _ = handle_text_input_key(&mut state.swatch_name_input, &key);
            }
        },
        BuilderEditorFocus::SavedSwatches => match (key.code, key.modifiers) {
            (KeyCode::Left | KeyCode::Char('h'), KeyModifiers::NONE) => state.move_saved_swatch_cursor(-1),
            (KeyCode::Right | KeyCode::Char('l'), KeyModifiers::NONE) => state.move_saved_swatch_cursor(1),
            (KeyCode::Enter, KeyModifiers::NONE) => state.apply_saved_swatch(state.saved_swatch_cursor),
            (KeyCode::Delete | KeyCode::Backspace, KeyModifiers::NONE) => state.delete_selected_swatch(),
            _ => {}
        },
        BuilderEditorFocus::RecentSwatches => match (key.code, key.modifiers) {
            (KeyCode::Left | KeyCode::Char('h'), KeyModifiers::NONE) => state.move_recent_swatch_cursor(-1),
            (KeyCode::Right | KeyCode::Char('l'), KeyModifiers::NONE) => state.move_recent_swatch_cursor(1),
            (KeyCode::Enter, KeyModifiers::NONE) => state.apply_recent_swatch(state.recent_swatch_cursor),
            _ => {}
        },
    }

    ThemeBuilderAction::None
}


fn handle_preset_key(state: &mut ThemeBuilderState, key: KeyEvent) -> ThemeBuilderAction {
    let choices_len = state.theme_library.len();
    match (key.code, key.modifiers) {
        (KeyCode::Esc, _) => {
            if state.preset_applies_on_select {
                return ThemeBuilderAction::Close;
            }
            state.view = ThemeBuilderView::Main;
        }
        (KeyCode::Up | KeyCode::Char('k'), KeyModifiers::NONE) => {
            if choices_len > 0 {
                state.preset_cursor = move_cursor(state.preset_cursor, choices_len, -1);
                let visible_rows = preset_visible_rows_for_state(state);
                sync_preset_scroll(state, visible_rows);
            }
        }
        (KeyCode::Down | KeyCode::Char('j'), KeyModifiers::NONE) => {
            if choices_len > 0 {
                state.preset_cursor = move_cursor(state.preset_cursor, choices_len, 1);
                let visible_rows = preset_visible_rows_for_state(state);
                sync_preset_scroll(state, visible_rows);
            }
        }
        (KeyCode::PageUp, KeyModifiers::NONE) => {
            if choices_len > 0 {
                let page = preset_visible_rows_for_state(state).max(1);
                state.preset_cursor = state.preset_cursor.saturating_sub(page);
                sync_preset_scroll(state, page);
            }
        }
        (KeyCode::PageDown, KeyModifiers::NONE) => {
            if choices_len > 0 {
                let page = preset_visible_rows_for_state(state).max(1);
                state.preset_cursor = (state.preset_cursor + page).min(choices_len - 1);
                sync_preset_scroll(state, page);
            }
        }
        (KeyCode::Enter, KeyModifiers::NONE) => {
            if state.preset_applies_on_select {
                if let Some(slug) = selected_preset_slug(state) {
                    return ThemeBuilderAction::ApplyPreset(slug);
                }
                return ThemeBuilderAction::Close;
            }
            state.load_preset_at_cursor();
            state.view = ThemeBuilderView::Main;
        }
        _ => {}
    }
    ThemeBuilderAction::None
}

fn handle_slider_key(state: &mut ThemeBuilderState, key: KeyEvent, channel: usize) {
    let step = if key.modifiers.contains(KeyModifiers::SHIFT) { 16 } else { 1 };
    match key.code {
        KeyCode::Left | KeyCode::Char('h') => state.adjust_rgb_channel(channel, -step),
        KeyCode::Right | KeyCode::Char('l') => state.adjust_rgb_channel(channel, step),
        _ => {}
    }
}

fn handle_derived_key(state: &mut ThemeBuilderState, key: KeyEvent) -> ThemeBuilderAction {
    let spec_len = theme::derived_element_specs().len();
    match (key.code, key.modifiers) {
        (KeyCode::Esc, _) => {
            state.view = ThemeBuilderView::Main;
        }
        (KeyCode::Up | KeyCode::Char('k'), KeyModifiers::NONE) => {
            move_derived_cursor(state, -1);
        }
        (KeyCode::Down | KeyCode::Char('j'), KeyModifiers::NONE) => {
            if spec_len > 0 {
                move_derived_cursor(state, 1);
            }
        }
        (KeyCode::Char('t'), KeyModifiers::NONE) => {
            state.lock_target = state.lock_target.toggle();
            state.status = Some(format!("Derived locks now target {}", state.lock_target.label()));
        }
        (KeyCode::Char('l') | KeyCode::Enter, KeyModifiers::NONE) => state.lock_selected_derived(),
        (KeyCode::Char('u'), KeyModifiers::NONE) => state.release_selected_derived(),
        (KeyCode::Char('s'), modifiers) if modifiers.contains(KeyModifiers::CONTROL) => return ThemeBuilderAction::Save,
        _ => {
            if handle_text_input_key(&mut state.derived_hex_input, &key) {
                let _ = state.apply_hex_to_selected_derived();
            }
        }
    }
    ThemeBuilderAction::None
}

fn handle_apply_key(state: &mut ThemeBuilderState, key: KeyEvent) -> ThemeBuilderAction {
    match (key.code, key.modifiers) {
        (KeyCode::Esc, _) => {
            state.view = ThemeBuilderView::Main;
            ThemeBuilderAction::None
        }
        (KeyCode::Tab, _) | (KeyCode::Down, KeyModifiers::NONE) => {
            state.apply_dialog.focus = state.apply_dialog.focus.next();
            ThemeBuilderAction::None
        }
        (KeyCode::Up, KeyModifiers::NONE) => {
            state.apply_dialog.focus = match state.apply_dialog.focus {
                ApplyDialogFocus::ThemeLocks => ApplyDialogFocus::Apply,
                ApplyDialogFocus::UserOverrides => ApplyDialogFocus::ThemeLocks,
                ApplyDialogFocus::Apply => ApplyDialogFocus::UserOverrides,
            };
            ThemeBuilderAction::None
        }
        (KeyCode::Enter | KeyCode::Char(' '), KeyModifiers::NONE) => {
            match state.apply_dialog.focus {
                ApplyDialogFocus::ThemeLocks => toggle_theme_lock_resolution(state),
                ApplyDialogFocus::UserOverrides => toggle_user_override_resolution(state),
                ApplyDialogFocus::Apply => return ThemeBuilderAction::Apply,
            }
            ThemeBuilderAction::None
        }
        (KeyCode::Char('t'), KeyModifiers::NONE) => {
            toggle_theme_lock_resolution(state);
            ThemeBuilderAction::None
        }
        (KeyCode::Char('u'), KeyModifiers::NONE) => {
            toggle_user_override_resolution(state);
            ThemeBuilderAction::None
        }
        (KeyCode::Char('a'), KeyModifiers::NONE) => ThemeBuilderAction::Apply,
        _ => ThemeBuilderAction::None,
    }
}

fn handle_delete_confirm_key(state: &mut ThemeBuilderState, key: KeyEvent) -> ThemeBuilderAction {
    match (key.code, key.modifiers) {
        (KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('c'), KeyModifiers::NONE) => {
            state.cancel_delete_current_custom_theme();
        }
        (KeyCode::Enter | KeyCode::Char('y'), KeyModifiers::NONE) => {
            state.confirm_delete_current_custom_theme();
        }
        _ => {}
    }
    ThemeBuilderAction::None
}


fn toggle_theme_lock_resolution(state: &mut ThemeBuilderState) {
    if state.palette.derived_locks.is_empty() {
        state.apply_dialog.honor_theme_locks = false;
        state.status = Some("This theme ships no derived locks".to_string());
    } else {
        state.apply_dialog.honor_theme_locks = !state.apply_dialog.honor_theme_locks;
        state.status = None;
    }
}

fn toggle_user_override_resolution(state: &mut ThemeBuilderState) {
    if state.user_overrides.is_empty() {
        state.apply_dialog.keep_user_overrides = false;
        state.status = Some("You have no personal theme overrides".to_string());
    } else {
        state.apply_dialog.keep_user_overrides = !state.apply_dialog.keep_user_overrides;
        state.status = None;
    }
}

pub fn handle_theme_builder_mouse(
    state: &mut ThemeBuilderState,
    mouse: MouseEvent,
    hit: Option<TuiButton>,
) -> ThemeBuilderAction {
    match mouse.kind {
        MouseEventKind::ScrollUp if state.view == ThemeBuilderView::Preset => {
            move_preset_cursor(state, -3);
            return ThemeBuilderAction::None;
        }
        MouseEventKind::ScrollDown if state.view == ThemeBuilderView::Preset => {
            move_preset_cursor(state, 3);
            return ThemeBuilderAction::None;
        }
        MouseEventKind::ScrollUp if state.view == ThemeBuilderView::Derived => {
            move_derived_cursor(state, -3);
            return ThemeBuilderAction::None;
        }
        MouseEventKind::ScrollDown if state.view == ThemeBuilderView::Derived => {
            move_derived_cursor(state, 3);
            return ThemeBuilderAction::None;
        }
        MouseEventKind::Down(MouseButton::Left) => {}
        _ => return ThemeBuilderAction::None,
    }

    match hit {
        Some(TuiButton::ThemeBuilderPreset) if state.view == ThemeBuilderView::Main => {
            open_preset_dropdown(state);
        }
        Some(TuiButton::ThemeBuilderPresetRow(index)) if state.view == ThemeBuilderView::Preset => {
            state.preset_cursor = index;
            if state.preset_applies_on_select {
                if let Some(slug) = selected_preset_slug(state) {
                    return ThemeBuilderAction::ApplyPreset(slug);
                }
                return ThemeBuilderAction::Close;
            }
            state.load_preset_at_cursor();
            state.view = ThemeBuilderView::Main;
        }
        Some(TuiButton::ThemeBuilderPresetCancel) if state.view == ThemeBuilderView::Preset => {
            if state.preset_applies_on_select {
                return ThemeBuilderAction::Close;
            }
            state.view = ThemeBuilderView::Main;
        }
        Some(TuiButton::ThemeBuilderMode) if state.view == ThemeBuilderView::Main => {
            state.palette.dark = !state.palette.dark;
            state.dirty = true;
        }
        Some(TuiButton::ThemeBuilderSlot(slot)) if state.view == ThemeBuilderView::Main => {
            state.editor_focus = BuilderEditorFocus::Slots;
            state.set_selected_slot(slot);
        }
        Some(TuiButton::ThemeBuilderHexField) if state.view == ThemeBuilderView::Main => {
            state.editor_focus = BuilderEditorFocus::Hex;
        }
        Some(TuiButton::ThemeBuilderRgbSlider(channel)) if state.view == ThemeBuilderView::Main => {
            state.editor_focus = match channel {
                0 => BuilderEditorFocus::Red,
                1 => BuilderEditorFocus::Green,
                _ => BuilderEditorFocus::Blue,
            };
        }
        Some(TuiButton::ThemeBuilderDepth(depth)) if state.view == ThemeBuilderView::Main => {
            state.editor_focus = BuilderEditorFocus::Depth;
            state.depth_mode = depth;
            state.status = Some(format!("Preview depth: {}", depth.label()));
        }
        Some(TuiButton::ThemeBuilderSwatchNameField) if state.view == ThemeBuilderView::Main => {
            state.editor_focus = BuilderEditorFocus::SwatchName;
        }
        Some(TuiButton::ThemeBuilderSavedSwatch(index)) if state.view == ThemeBuilderView::Main => {
            state.editor_focus = BuilderEditorFocus::SavedSwatches;
            state.apply_saved_swatch(index);
        }
        Some(TuiButton::ThemeBuilderRecentSwatch(index)) if state.view == ThemeBuilderView::Main => {
            state.editor_focus = BuilderEditorFocus::RecentSwatches;
            state.apply_recent_swatch(index);
        }
        Some(TuiButton::ThemeBuilderSaveSwatch) if state.view == ThemeBuilderView::Main => {
            state.save_current_swatch();
        }
        Some(TuiButton::ThemeBuilderDeleteSwatch) if state.view == ThemeBuilderView::Main => {
            state.delete_selected_swatch();
        }
        Some(TuiButton::ThemeBuilderDeleteConfirm) if state.view == ThemeBuilderView::DeleteConfirm => {
            state.confirm_delete_current_custom_theme();
        }
        Some(TuiButton::ThemeBuilderDeleteCancel) if state.view == ThemeBuilderView::DeleteConfirm => {
            state.cancel_delete_current_custom_theme();
        }
        Some(_) if state.view == ThemeBuilderView::DeleteConfirm => {}
        Some(TuiButton::ThemeBuilderSave) if state.view == ThemeBuilderView::Main => return ThemeBuilderAction::Save,
        Some(TuiButton::ThemeBuilderApply) if state.view == ThemeBuilderView::Main => {
            state.view = ThemeBuilderView::Apply;
        }
        Some(TuiButton::ThemeBuilderDerived) if state.view == ThemeBuilderView::Main => {
            state.view = ThemeBuilderView::Derived;
            state.sync_derived_hex_from_selected();
        }
        Some(TuiButton::ThemeBuilderRevert) if state.view == ThemeBuilderView::Main => {
            state.revert_from_disk();
        }
        Some(TuiButton::ThemeBuilderDeleteTheme) if state.view == ThemeBuilderView::Main => {
            state.request_delete_current_custom_theme();
        }
        Some(TuiButton::ThemeBuilderCancel) if state.view == ThemeBuilderView::Main => {
            return ThemeBuilderAction::Close;
        }
        Some(TuiButton::ThemeBuilderDerivedRow(index)) if state.view == ThemeBuilderView::Derived => {
            let specs_len = theme::derived_element_specs().len();
            if index < specs_len {
                state.derived_cursor = index;
                state.derived_scroll = state.derived_scroll.min(index);
                state.sync_derived_hex_from_selected();
            }
        }
        Some(TuiButton::ThemeBuilderDerivedLock) if state.view == ThemeBuilderView::Derived => {
            state.lock_selected_derived();
        }
        Some(TuiButton::ThemeBuilderDerivedRelease) if state.view == ThemeBuilderView::Derived => {
            state.release_selected_derived();
        }
        Some(TuiButton::ThemeBuilderDerivedTarget) if state.view == ThemeBuilderView::Derived => {
            state.lock_target = state.lock_target.toggle();
            state.status = Some(format!("Derived locks now target {}", state.lock_target.label()));
        }
        Some(TuiButton::ThemeBuilderDerivedDone) if state.view == ThemeBuilderView::Derived => {
            state.view = ThemeBuilderView::Main;
        }
        Some(TuiButton::ThemeBuilderApplyThemeLocks) if state.view == ThemeBuilderView::Apply => {
            state.apply_dialog.focus = ApplyDialogFocus::ThemeLocks;
            toggle_theme_lock_resolution(state);
        }
        Some(TuiButton::ThemeBuilderApplyUserOverrides) if state.view == ThemeBuilderView::Apply => {
            state.apply_dialog.focus = ApplyDialogFocus::UserOverrides;
            toggle_user_override_resolution(state);
        }
        Some(TuiButton::ThemeBuilderApplyConfirm) if state.view == ThemeBuilderView::Apply => {
            state.apply_dialog.focus = ApplyDialogFocus::Apply;
            return ThemeBuilderAction::Apply;
        }
        Some(TuiButton::ThemeBuilderApplyCancel) if state.view == ThemeBuilderView::Apply => {
            state.view = ThemeBuilderView::Main;
        }
        _ => {}
    }

    ThemeBuilderAction::None
}

fn move_derived_cursor(state: &mut ThemeBuilderState, delta: isize) {
    let specs_len = theme::derived_element_specs().len();
    if specs_len == 0 {
        return;
    }
    let current = state.derived_cursor as isize;
    let max = specs_len.saturating_sub(1) as isize;
    state.derived_cursor = (current + delta).clamp(0, max) as usize;
    let visible = derived_visible_rows_for_state(state);
    let max_scroll = specs_len.saturating_sub(visible);
    state.derived_scroll = state.derived_scroll.min(max_scroll);
    if state.derived_cursor < state.derived_scroll {
        state.derived_scroll = state.derived_cursor;
    } else if state.derived_cursor >= state.derived_scroll + visible {
        state.derived_scroll = state.derived_cursor + 1 - visible;
    }
    state.derived_scroll = state.derived_scroll.min(max_scroll);
    state.sync_derived_hex_from_selected();
}

pub fn draw_theme_builder(
    f: &mut Frame,
    state: &ThemeBuilderState,
    button_map: &mut ButtonRenderMap,
    theme: theme::Theme,
) {
    match state.view {
        ThemeBuilderView::Main => draw_main_view(f, state, button_map, theme),
        ThemeBuilderView::Preset => {
            if !state.preset_applies_on_select {
                draw_main_view(f, state, button_map, theme);
            }
            draw_preset_dropdown(f, state, button_map, theme);
        }
        ThemeBuilderView::Derived => draw_derived_view(f, state, button_map, theme),
        ThemeBuilderView::Apply => draw_apply_dialog(f, state, button_map, theme),
        ThemeBuilderView::DeleteConfirm => {
            draw_main_view(f, state, button_map, theme);
            draw_delete_confirm_dialog(f, state, button_map, theme);
        }
    }
}

fn draw_main_view(f: &mut Frame, state: &ThemeBuilderState, button_map: &mut ButtonRenderMap, theme: theme::Theme) {
    let area = scaled_centered_rect(82, 25, f.size());
    f.render_widget(Clear, area);
    let block = Block::default()
        .title(Span::styled(" Theme Builder ", Style::default().fg(theme.title).add_modifier(Modifier::BOLD)))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(19), Constraint::Length(2)])
        .split(inner);

    let header = Line::from(vec![
        Span::styled(" Preset ▾ ", Style::default().fg(theme.pill_active_fg).bg(theme.dropdown_bg)),
        Span::raw("  "),
        Span::styled(&state.palette.name, Style::default().fg(theme.text_bright).add_modifier(Modifier::BOLD)),
        Span::raw("  "),
        Span::styled(format!("Mode: {} {}", if state.palette.dark { "●" } else { "○" }, state.palette.mode_label()), Style::default().fg(theme.warning)),
        Span::raw("  "),
        Span::styled(format!("Depth: {}", state.depth_mode.label()), Style::default().fg(theme.info)),
        Span::raw("  "),
        Span::styled("p opens preset list", Style::default().fg(theme.text_dim)),
    ]);
    f.render_widget(Paragraph::new(vec![header, Line::raw("")]), chunks[0]);
    record_rect(button_map, TuiButton::ThemeBuilderPreset, chunks[0].x, chunks[0].y, 10, 1);
    let mode_x = chunks[0].x.saturating_add(14).saturating_add(state.palette.name.len().min(28) as u16);
    record_rect(button_map, TuiButton::ThemeBuilderMode, mode_x, chunks[0].y, 18, 1);

    let left_width = proportional_width(chunks[1].width.saturating_sub(1), 34, 27, 44);
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(left_width), Constraint::Length(1), Constraint::Min(52)])
        .split(chunks[1]);
    draw_slot_list(f, body[0], state, button_map, theme);
    draw_editor(f, body[2], state, button_map, theme);

    let footer = Line::from(vec![
        chip("^s Save", theme.chip_go, theme), Span::raw(" "),
        chip("a Apply", theme.tab_active, theme), Span::raw(" "),
        chip("d Derived", theme.warning, theme), Span::raw(" "),
        chip("m Mode", theme.dropdown_bg, theme), Span::raw(" "),
        chip("r Revert", theme.dropdown_bg, theme), Span::raw(" "),
        chip("x Delete", theme.destructive, theme), Span::raw(" "),
        chip("+ Save swatch", theme.dropdown_bg, theme), Span::raw(" "),
        chip("Esc Cancel", theme.chip_dismiss, theme),
        status_span(state, theme),
    ]);
    f.render_widget(Paragraph::new(footer), chunks[2]);
    record_footer_chips(
        button_map,
        chunks[2].x,
        chunks[2].y,
        &[
            (TuiButton::ThemeBuilderSave, "^s Save"),
            (TuiButton::ThemeBuilderApply, "a Apply"),
            (TuiButton::ThemeBuilderDerived, "d Derived"),
            (TuiButton::ThemeBuilderMode, "m Mode"),
            (TuiButton::ThemeBuilderRevert, "r Revert"),
            (TuiButton::ThemeBuilderDeleteTheme, "x Delete"),
            (TuiButton::ThemeBuilderSaveSwatch, "+ Save swatch"),
            (TuiButton::ThemeBuilderCancel, "Esc Cancel"),
        ],
    );
}

fn draw_preset_dropdown(f: &mut Frame, state: &ThemeBuilderState, button_map: &mut ButtonRenderMap, theme: theme::Theme) {
    let choices = &state.theme_library;
    let area = scaled_centered_rect(64, 18, f.size());
    f.render_widget(Clear, area);
    let title = if state.preset_applies_on_select { " Theme Gallery " } else { " Presets " };
    let block = Block::default()
        .title(Span::styled(title, Style::default().fg(theme.title).add_modifier(Modifier::BOLD)))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let visible_rows = inner.height.saturating_sub(3).max(1) as usize;
    state.preset_visible_rows.set(visible_rows);
    let max_scroll = choices.len().saturating_sub(visible_rows);
    let start = state.preset_scroll.min(max_scroll);
    let end = (start + visible_rows).min(choices.len());
    let mut lines = Vec::new();
    let name_width = inner.width.saturating_sub(44).clamp(18, 36) as usize;
    for (absolute, choice) in choices.iter().enumerate().take(end).skip(start) {
        let selected = absolute == state.preset_cursor.min(choices.len().saturating_sub(1));
        let marker = if selected { "›" } else { " " };
        let source = if choice.built_in { "built-in" } else { "custom" };
        let mode = if choice.dark { "dark" } else { "light" };
        let locks = if choice.author_lock_count > 0 {
            format!(" · {} locks", choice.author_lock_count)
        } else {
            String::new()
        };
        let style = if selected {
            Style::default().fg(theme.text_bright).bg(theme.selection_bg).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.text)
        };
        let mut spans = vec![
            Span::styled(format!("{marker} "), style),
            Span::styled(format!("{:<name_width$}", choice.name.chars().take(name_width).collect::<String>()), style),
            Span::styled(format!(" {:<8} {:<5}", source, mode), Style::default().fg(theme.text_dim)),
        ];
        spans.push(Span::raw(" "));
        for color in choice.accents.iter().take(10).copied() {
            spans.push(Span::styled("██", Style::default().fg(color).bg(color)));
            spans.push(Span::raw(" "));
        }
        if !locks.is_empty() {
            spans.push(Span::styled(locks, Style::default().fg(theme.warning)));
        }
        lines.push(Line::from(spans));
        record_rect(
            button_map,
            TuiButton::ThemeBuilderPresetRow(absolute),
            inner.x,
            inner.y.saturating_add((absolute - start) as u16),
            inner.width,
            1,
        );
    }
    while lines.len() < visible_rows {
        lines.push(Line::raw(""));
    }
    let footer = if state.preset_applies_on_select {
        Line::from(vec![
            Span::styled("Enter/click applies theme", Style::default().fg(theme.text_dim)),
            Span::raw("  "),
            Span::styled("Esc Close", Style::default().fg(theme.chip_dismiss)),
        ])
    } else {
        Line::from(vec![
            Span::styled("Enter/click loads as editable draft", Style::default().fg(theme.text_dim)),
            Span::raw("  "),
            Span::styled("Esc Cancel", Style::default().fg(theme.chip_dismiss)),
        ])
    };
    lines.push(footer);
    f.render_widget(Paragraph::new(lines), inner);
    record_rect(button_map, TuiButton::ThemeBuilderPresetCancel, inner.x.saturating_add(inner.width.saturating_sub(12)), inner.y.saturating_add(inner.height.saturating_sub(1)), 12, 1);
}

fn draw_slot_list(f: &mut Frame, area: Rect, state: &ThemeBuilderState, button_map: &mut ButtonRenderMap, theme: theme::Theme) {
    let mut lines = Vec::new();
    lines.push(Line::from(Span::styled(" Roles", Style::default().fg(theme.header).add_modifier(Modifier::BOLD))));
    for idx in 0..ROLE_KEYS.len() {
        let slot = BuilderSlot::Role(idx);
        record_rect(button_map, TuiButton::ThemeBuilderSlot(slot), area.x, area.y.saturating_add(1 + idx as u16), area.width, 1);
        let selected = state.selected_slot == slot;
        let color = state.palette.role_color(idx);
        let style = if selected {
            Style::default().fg(theme.text_bright).bg(theme.selection_bg).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.text)
        };
        let label_width = area.width.saturating_sub(10).clamp(12, 28) as usize;
        lines.push(Line::from(vec![
            Span::styled("██", Style::default().fg(color).bg(color)),
            Span::styled(format!(" {:<label_width$}", ROLE_LABELS[idx]), style),
            Span::styled(theme::color_to_hex(color), style),
        ]));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(" Accents (16)", Style::default().fg(theme.header).add_modifier(Modifier::BOLD))));
    for row in 0..4 {
        let mut spans = vec![Span::raw(" ")];
        for col in 0..4 {
            let idx = row * 4 + col;
            let slot = BuilderSlot::Accent(idx);
            record_rect(button_map, TuiButton::ThemeBuilderSlot(slot), area.x.saturating_add(1 + (col as u16 * 4)), area.y.saturating_add(14 + row as u16), 3, 1);
            let selected = state.selected_slot == slot;
            let color = state.palette.accents[idx];
            let label = if selected { format!("{:02}", idx) } else { "██".to_string() };
            spans.push(Span::styled(label, Style::default().fg(color).bg(if selected { theme.selection_bg } else { color })));
            spans.push(Span::raw("  "));
        }
        lines.push(Line::from(spans));
    }
    lines.push(Line::from(Span::styled(" 0-11 hue · 12-15 special", Style::default().fg(theme.text_dim))));
    f.render_widget(Paragraph::new(lines), area);
}

fn draw_editor(f: &mut Frame, area: Rect, state: &ThemeBuilderState, button_map: &mut ButtonRenderMap, theme: theme::Theme) {
    let color = state.selected_color();
    let (xidx, xcolor) = theme::nearest_xterm_256(color);
    let (depth_index, depth_color) = theme::nearest_color_for_depth(color, state.depth_mode);
    let mut lines = Vec::new();
    lines.push(Line::from(vec![
        Span::styled("Editing ", Style::default().fg(theme.text_dim)),
        Span::styled(state.selected_slot.label(), Style::default().fg(theme.text_bright).add_modifier(Modifier::BOLD)),
        Span::raw("   "),
        chip("+ Save", theme.chip_go, theme),
    ]));
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::styled("████████████████", Style::default().fg(color).bg(color)),
        Span::raw("  "),
        Span::styled(theme::color_to_hex(color), Style::default().fg(theme.text_bright)),
        Span::raw("  "),
        Span::styled(
            state.palette.slot_binding_name(state.selected_slot)
                .map(|name| format!("bound ${name}"))
                .unwrap_or_else(|| "unbound".to_string()),
            Style::default().fg(theme.text_dim),
        ),
    ]));
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        focus_mark(state.editor_focus == BuilderEditorFocus::Hex, theme),
        Span::styled("Hex field ", Style::default().fg(theme.label)),
        Span::styled(format!("[ {:<7} ]", state.hex_input.text), input_style(state.editor_focus == BuilderEditorFocus::Hex, theme)),
    ]));
    lines.push(slider_line("R", state.rgb_values[0], 0, state.editor_focus == BuilderEditorFocus::Red, theme));
    lines.push(slider_line("G", state.rgb_values[1], 1, state.editor_focus == BuilderEditorFocus::Green, theme));
    lines.push(slider_line("B", state.rgb_values[2], 2, state.editor_focus == BuilderEditorFocus::Blue, theme));
    lines.push(Line::raw(""));
    let depth_spans = ColorDepth::ALL.iter().flat_map(|depth| {
        let active = *depth == state.depth_mode;
        let style = if active { Style::default().fg(theme.panel_bg).bg(theme.tab_active).add_modifier(Modifier::BOLD) } else { Style::default().fg(theme.text).bg(theme.dropdown_bg) };
        vec![Span::styled(format!(" {} ", depth.label()), style), Span::raw(" ")]
    }).collect::<Vec<_>>();
    lines.push(Line::from(vec![focus_mark(state.editor_focus == BuilderEditorFocus::Depth, theme), Span::styled("Depth ", Style::default().fg(theme.label))]));
    lines.push(Line::from(depth_spans));
    let mut depth_readout = vec![
        Span::styled("→256 ", Style::default().fg(theme.label)),
        Span::styled("██", Style::default().fg(xcolor).bg(xcolor)),
        Span::raw(" "),
        Span::styled(theme::color_to_hex(xcolor), Style::default().fg(theme.text_bright)),
        Span::raw(" index "),
        Span::styled(xidx.to_string(), Style::default().fg(theme.info)),
    ];
    if state.depth_mode != ColorDepth::TrueColor {
        depth_readout.extend([
            Span::raw("  "),
            Span::styled(format!("→{} ", state.depth_mode.label()), Style::default().fg(theme.label)),
            Span::styled("██", Style::default().fg(depth_color).bg(depth_color)),
            Span::raw(" "),
            Span::styled(theme::color_to_hex(depth_color), Style::default().fg(theme.text_bright)),
            Span::raw(" #"),
            Span::styled(depth_index.to_string(), Style::default().fg(theme.info)),
        ]);
    }
    lines.push(Line::from(depth_readout));
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        focus_mark(state.editor_focus == BuilderEditorFocus::SwatchName, theme),
        Span::styled("Swatch name ", Style::default().fg(theme.label)),
        Span::styled(format!("[ {:<18} ]", state.swatch_name_input.text), input_style(state.editor_focus == BuilderEditorFocus::SwatchName, theme)),
        Span::raw(" "),
        Span::styled("Del removes selected saved", Style::default().fg(theme.text_dim)),
    ]));
    lines.push(saved_swatch_row(state, theme));
    lines.push(recent_swatch_row(state, theme));
    lines.push(Line::raw(""));
    let preview_capacity = (area.height as usize).saturating_sub(lines.len());
    lines.extend(preview_lines(state, theme, preview_capacity));
    f.render_widget(Paragraph::new(lines), area);
    record_editor_buttons(button_map, area, state);
}

fn slider_line(label: &str, value: u8, channel: usize, focused: bool, theme: theme::Theme) -> Line<'static> {
    let filled = ((usize::from(value) * 12) / 255).min(12);
    let empty = 12usize.saturating_sub(filled);
    let channel_color = match channel {
        0 => Color::Rgb(value, 0, 0),
        1 => Color::Rgb(0, value, 0),
        _ => Color::Rgb(0, 0, value),
    };
    Line::from(vec![
        focus_mark(focused, theme),
        Span::styled(format!("{label} {:>3} ", value), Style::default().fg(theme.label)),
        Span::styled("█".repeat(filled), Style::default().fg(channel_color)),
        Span::styled("░".repeat(empty), Style::default().fg(theme.border_dim)),
    ])
}

fn saved_swatch_row(state: &ThemeBuilderState, theme: theme::Theme) -> Line<'static> {
    let mut spans = vec![
        focus_mark(state.editor_focus == BuilderEditorFocus::SavedSwatches, theme),
        Span::styled("Saved ", Style::default().fg(theme.label)),
    ];
    if state.palette.swatches.is_empty() {
        spans.push(Span::styled("(none)", Style::default().fg(theme.text_dim)));
    } else {
        for (index, swatch) in state.palette.swatches.iter().take(12).enumerate() {
            let selected = index == state.saved_swatch_cursor.min(state.palette.swatches.len().saturating_sub(1));
            let style = if selected {
                Style::default().fg(swatch.color).bg(theme.selection_bg).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(swatch.color).bg(swatch.color)
            };
            spans.push(Span::styled(if selected { "[]" } else { "██" }, style));
            spans.push(Span::raw(" "));
        }
    }
    spans.push(Span::styled("[+]", Style::default().fg(theme.chip_go)));
    spans.push(Span::raw(" "));
    spans.push(Span::styled("[del]", Style::default().fg(theme.chip_dismiss)));
    Line::from(spans)
}

fn recent_swatch_row(state: &ThemeBuilderState, theme: theme::Theme) -> Line<'static> {
    let mut spans = vec![
        focus_mark(state.editor_focus == BuilderEditorFocus::RecentSwatches, theme),
        Span::styled("Recent", Style::default().fg(theme.label)),
        Span::raw(" "),
    ];
    if state.recent_colors.is_empty() {
        spans.push(Span::styled("(none)", Style::default().fg(theme.text_dim)));
    } else {
        for (index, color) in state.recent_colors.iter().take(12).copied().enumerate() {
            let selected = index == state.recent_swatch_cursor.min(state.recent_colors.len().saturating_sub(1));
            let style = if selected {
                Style::default().fg(color).bg(theme.selection_bg).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(color).bg(color)
            };
            spans.push(Span::styled(if selected { "[]" } else { "██" }, style));
            spans.push(Span::raw(" "));
        }
    }
    Line::from(spans)
}

fn preview_lines(state: &ThemeBuilderState, theme: theme::Theme, max_rows: usize) -> Vec<Line<'static>> {
    if max_rows == 0 {
        return Vec::new();
    }

    let preview = theme::preview_resolve_theme_draft_for_depth(
        &state.palette,
        ThemeApplyOptions { honor_theme_locks: true, keep_user_overrides: true },
        &state.user_overrides,
        state.depth_mode,
    );
    let color = state.selected_color();
    let (r, g, b) = theme::rgb_tuple(color);

    let mut lines = vec![
        Line::from(Span::styled("Live preview", Style::default().fg(theme.header).add_modifier(Modifier::BOLD))),
        Line::from(vec![
            Span::styled(" Metadata ", Style::default().fg(preview.pill_active_fg).bg(preview.tab_active)),
            Span::styled("  Artwork  ", Style::default().fg(preview.tab_inactive)),
            Span::styled(" Section header", Style::default().fg(preview.header).add_modifier(Modifier::BOLD)),
        ]),
        Line::from(vec![
            Span::styled(" label ", Style::default().fg(preview.label)),
            Span::styled(" value ", Style::default().fg(preview.value)),
            Span::styled(" selected ", Style::default().fg(preview.value).bg(preview.selection_bg)),
            Span::raw(" "),
            Span::styled(" OK ", Style::default().fg(preview.progress_dialog_button_fg).bg(preview.progress_dialog_button_bg)),
            Span::raw(" "),
            Span::styled(" Esc ", Style::default().fg(preview.progress_dialog_abort_fg).bg(preview.progress_dialog_abort_bg)),
        ]),
        Line::from(vec![
            Span::styled("derived ", Style::default().fg(preview.text_dim)),
            Span::styled("██", Style::default().fg(preview.surface).bg(preview.surface)), Span::raw(" surface "),
            Span::styled("██", Style::default().fg(preview.progress_dialog_border).bg(preview.progress_dialog_border)), Span::raw(" border "),
            Span::styled("auto (not edited)", Style::default().fg(preview.text_dim)),
        ]),
        Line::from(vec![
            Span::styled("input ", Style::default().fg(preview.label)),
            Span::styled(state.selected_slot.label(), Style::default().fg(preview.value).add_modifier(Modifier::BOLD)),
            Span::raw("  "),
            Span::styled("██", Style::default().fg(color).bg(color)),
            Span::raw(" "),
            Span::styled(format!("{}  rgb({r},{g},{b})", theme::color_to_hex(color)), Style::default().fg(preview.value)),
        ]),
        Line::from(vec![
            Span::styled("progress ", Style::default().fg(preview.label)),
            Span::styled("████████░░░░", Style::default().fg(preview.progress_dialog_bar_filled)),
            Span::raw(" "),
            Span::styled("resolving derived roles", Style::default().fg(preview.text_dim)),
        ]),
        Line::from(vec![
            Span::styled("notice ", Style::default().fg(preview.label)),
            Span::styled("warning", Style::default().fg(preview.warning)),
            Span::raw(" · "),
            Span::styled("success", Style::default().fg(preview.success)),
            Span::raw(" · "),
            Span::styled("error", Style::default().fg(preview.error)),
        ]),
        Line::from(vec![
            Span::styled("swatches ", Style::default().fg(preview.label)),
            Span::styled("██", Style::default().fg(preview.accents[0]).bg(preview.accents[0])),
            Span::raw(" "),
            Span::styled("██", Style::default().fg(preview.accents[1]).bg(preview.accents[1])),
            Span::raw(" "),
            Span::styled("██", Style::default().fg(preview.accents[2]).bg(preview.accents[2])),
            Span::raw(" "),
            Span::styled("palette identity", Style::default().fg(preview.text_dim)),
        ]),
    ];

    lines.truncate(max_rows);
    lines
}

fn draw_derived_view(f: &mut Frame, state: &ThemeBuilderState, button_map: &mut ButtonRenderMap, theme: theme::Theme) {
    let area = scaled_centered_rect(76, 22, f.size());
    f.render_widget(Clear, area);
    let block = Block::default()
        .title(Span::styled(" Derived Overrides ", Style::default().fg(theme.title).add_modifier(Modifier::BOLD)))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(17), Constraint::Length(2)])
        .split(inner);
    f.render_widget(Paragraph::new(Line::from(vec![
        Span::styled(" ○ auto ", Style::default().fg(theme.text_dim)),
        Span::styled("● theme lock ", Style::default().fg(theme.warning)),
        Span::styled("● user lock ", Style::default().fg(theme.info)),
        Span::styled(format!(" target: {} (t toggles)", state.lock_target.label()), Style::default().fg(theme.text_dim)),
    ])), chunks[0]);

    let list_width = proportional_width(chunks[1].width.saturating_sub(1), 45, 35, 58);
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(list_width), Constraint::Length(1), Constraint::Min(38)])
        .split(chunks[1]);
    draw_derived_list(f, body[0], state, button_map, theme);
    draw_derived_detail(f, body[2], state, theme);
    let footer = Line::from(vec![
        chip("l Lock", theme.chip_go, theme), Span::raw(" "),
        chip("u Release", theme.dropdown_bg, theme), Span::raw(" "),
        chip("t Target", theme.warning, theme), Span::raw(" "),
        chip("^s Save", theme.chip_go, theme), Span::raw(" "),
        chip("Esc Done", theme.chip_dismiss, theme), status_span(state, theme),
    ]);
    f.render_widget(Paragraph::new(footer), chunks[2]);
    record_footer_chips(
        button_map,
        chunks[2].x,
        chunks[2].y,
        &[
            (TuiButton::ThemeBuilderDerivedLock, "l Lock"),
            (TuiButton::ThemeBuilderDerivedRelease, "u Release"),
            (TuiButton::ThemeBuilderDerivedTarget, "t Target"),
            (TuiButton::ThemeBuilderSave, "^s Save"),
            (TuiButton::ThemeBuilderDerivedDone, "Esc Done"),
        ],
    );
}

fn draw_derived_list(f: &mut Frame, area: Rect, state: &ThemeBuilderState, button_map: &mut ButtonRenderMap, theme: theme::Theme) {
    let specs = theme::derived_element_specs();
    let auto_theme = theme::preview_resolve_theme_draft_for_depth(
        &state.palette,
        ThemeApplyOptions { honor_theme_locks: false, keep_user_overrides: false },
        &ThemeOverrides::default(),
        state.depth_mode,
    );
    let visible = area.height.saturating_sub(1).max(1) as usize;
    state.derived_visible_rows.set(visible);
    let max_scroll = specs.len().saturating_sub(visible);
    let start = state.derived_scroll.min(max_scroll);
    let mut lines = Vec::new();
    for (idx, spec) in specs.iter().enumerate().skip(start).take(visible) {
        let row_y = area.y.saturating_add((idx - start) as u16);
        record_rect(button_map, TuiButton::ThemeBuilderDerivedRow(idx), area.x, row_y, area.width, 1);
        let selected = idx == state.derived_cursor;
        let source = if state.user_overrides.overrides.contains_key(spec.key) {
            ("●", theme.info)
        } else if state.palette.derived_locks.contains_key(spec.key) {
            ("●", theme.warning)
        } else {
            ("○", theme.text_dim)
        };
        let color = state.user_overrides.overrides.get(spec.key)
            .or_else(|| state.palette.derived_locks.get(spec.key))
            .copied()
            .or_else(|| theme::theme_color_by_derived_key(auto_theme, spec.key))
            .unwrap_or(theme.text_dim);
        let style = if selected { Style::default().fg(theme.text_bright).bg(theme.selection_bg).add_modifier(Modifier::BOLD) } else { Style::default().fg(theme.text) };
        let key_width = area.width.saturating_sub(12).clamp(24, 42) as usize;
        lines.push(Line::from(vec![
            Span::styled(format!("{} ", source.0), Style::default().fg(source.1)),
            Span::styled(format!("{:<key_width$} ", spec.key), style),
            Span::styled("██", Style::default().fg(color).bg(color)),
            Span::raw(" "),
            Span::styled(theme::color_to_hex(color), style),
        ]));
    }
    lines.push(Line::from(Span::styled(format!("{}/{}", state.derived_cursor + 1, specs.len()), Style::default().fg(theme.text_dim))));
    f.render_widget(Paragraph::new(lines), area);
}

fn draw_derived_detail(f: &mut Frame, area: Rect, state: &ThemeBuilderState, theme: theme::Theme) {
    let spec = &theme::derived_element_specs()[state.derived_cursor.min(theme::derived_element_specs().len() - 1)];
    let auto_theme = theme::preview_resolve_theme_draft_for_depth(
        &state.palette,
        ThemeApplyOptions { honor_theme_locks: false, keep_user_overrides: false },
        &ThemeOverrides::default(),
        state.depth_mode,
    );
    let auto_color = theme::theme_color_by_derived_key(auto_theme, spec.key).unwrap_or(theme.text_dim);
    let user_color = state.user_overrides.overrides.get(spec.key).copied();
    let author_color = state.palette.derived_locks.get(spec.key).copied();
    let locked_color = user_color.or(author_color);
    let provenance = if user_color.is_some() { "locked by you" } else if author_color.is_some() { "locked by theme" } else { "auto" };
    let mut lines = vec![
        Line::from(Span::styled(spec.key, Style::default().fg(theme.info).add_modifier(Modifier::BOLD))),
        Line::raw(""),
        Line::from(vec![Span::styled("from = ", Style::default().fg(theme.label)), Span::styled(spec.formula, Style::default().fg(theme.text))]),
        Line::from(vec![Span::styled("auto  ", Style::default().fg(theme.label)), Span::styled("██", Style::default().fg(auto_color).bg(auto_color)), Span::raw(" "), Span::styled(theme::color_to_hex(auto_color), Style::default().fg(theme.text_bright))]),
        Line::raw(""),
        Line::from(vec![Span::styled("state ", Style::default().fg(theme.label)), Span::styled(provenance, Style::default().fg(if user_color.is_some() { theme.info } else if author_color.is_some() { theme.warning } else { theme.text_dim }))]),
        Line::from(vec![Span::styled("lock  ", Style::default().fg(theme.label)), Span::styled(format!("[ {:<7} ]", state.derived_hex_input.text), input_style(true, theme))]),
    ];
    if let Some(color) = locked_color {
        lines.push(Line::from(vec![Span::styled("value ", Style::default().fg(theme.label)), Span::styled("██", Style::default().fg(color).bg(color)), Span::raw(" "), Span::styled(theme::color_to_hex(color), Style::default().fg(theme.text_bright))]));
        lines.push(Line::from(Span::styled("Stays pinned even if its input colors change later.", Style::default().fg(theme.text_dim))));
    } else {
        lines.push(Line::from(Span::styled("Auto follows the derivation formula.", Style::default().fg(theme.text_dim))));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![Span::styled("used by ", Style::default().fg(theme.label)), Span::styled(spec.used_by, Style::default().fg(theme.text))]));
    f.render_widget(Paragraph::new(lines), area);
}

fn draw_apply_dialog(f: &mut Frame, state: &ThemeBuilderState, button_map: &mut ButtonRenderMap, theme: theme::Theme) {
    let area = scaled_centered_rect(64, 18, f.size());
    f.render_widget(Clear, area);
    let block = Block::default()
        .title(Span::styled(" Apply / Resolve Theme ", Style::default().fg(theme.title).add_modifier(Modifier::BOLD)))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let theme_locks = state.palette.derived_locks.len();
    let user_locks = state.user_overrides.len();
    let tally = theme::theme_resolution_tally(&state.palette, state.apply_options(), &state.user_overrides);
    let by_theme = tally.by_theme;
    let by_user = tally.by_user;
    let auto = tally.auto;
    let mut lines = vec![
        Line::from(vec![Span::styled(&state.palette.name, Style::default().fg(theme.text_bright).add_modifier(Modifier::BOLD)), Span::raw(" · "), Span::styled(state.palette.mode_label(), Style::default().fg(theme.warning)), Span::raw(" · "), Span::styled(state.depth_mode.label(), Style::default().fg(theme.info))]),
        Line::from(Span::styled(format!("Ships {theme_locks} locked colors · You have {user_locks} personal overrides"), Style::default().fg(theme.text_dim))),
        Line::raw(""),
        switch_line("Theme locked colors", "Honor the theme", "Re-derive for my terminal", state.apply_dialog.honor_theme_locks, state.palette.derived_locks.is_empty(), state.apply_dialog.focus == ApplyDialogFocus::ThemeLocks, theme),
        Line::from(Span::styled(format!("  Re-derive recomputes from formulas at {} depth.", state.depth_mode.label()), Style::default().fg(theme.text_dim))),
        switch_line("Your overrides", "Keep mine", "Use theme as authored", state.apply_dialog.keep_user_overrides, state.user_overrides.is_empty(), state.apply_dialog.focus == ApplyDialogFocus::UserOverrides, theme),
        Line::from(Span::styled("  Your layer sits above the theme's locks.", Style::default().fg(theme.text_dim))),
        Line::raw(""),
        Line::from(vec![
            Span::styled(format!("● {by_theme} by theme"), Style::default().fg(theme.warning)),
            Span::raw("  "),
            Span::styled(format!("● {by_user} by you"), Style::default().fg(theme.info)),
            Span::raw("  "),
            Span::styled(format!("○ {auto} auto"), Style::default().fg(theme.text_dim)),
        ]),
        Line::raw(""),
    ];
    let apply_style = if state.apply_dialog.focus == ApplyDialogFocus::Apply {
        Style::default().fg(theme.progress_dialog_button_fg).bg(theme.progress_dialog_button_bg).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.progress_dialog_button_fg).bg(theme.progress_dialog_button_bg)
    };
    lines.push(Line::from(vec![Span::styled(" a Apply ", apply_style), Span::raw(" "), chip("Esc Cancel", theme.chip_dismiss, theme)]));
    f.render_widget(Paragraph::new(lines).alignment(Alignment::Left), inner);
    record_rect(button_map, TuiButton::ThemeBuilderApplyThemeLocks, inner.x, inner.y.saturating_add(3), inner.width, 1);
    record_rect(button_map, TuiButton::ThemeBuilderApplyUserOverrides, inner.x, inner.y.saturating_add(5), inner.width, 1);
    record_footer_chips(
        button_map,
        inner.x,
        inner.y.saturating_add(10),
        &[
            (TuiButton::ThemeBuilderApplyConfirm, "a Apply"),
            (TuiButton::ThemeBuilderApplyCancel, "Esc Cancel"),
        ],
    );
}


fn draw_delete_confirm_dialog(
    f: &mut Frame,
    state: &ThemeBuilderState,
    button_map: &mut ButtonRenderMap,
    theme: theme::Theme,
) {
    let area = scaled_centered_rect(58, 10, f.size());
    f.render_widget(Clear, area);
    let block = Block::default()
        .title(Span::styled(" Confirm Theme Deletion ", Style::default().fg(theme.destructive).add_modifier(Modifier::BOLD)))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.destructive));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let lines = vec![
        Line::from(vec![
            Span::styled("Delete custom theme ", Style::default().fg(theme.text)),
            Span::styled(&state.palette.name, Style::default().fg(theme.text_bright).add_modifier(Modifier::BOLD)),
            Span::styled("?", Style::default().fg(theme.text)),
        ]),
        Line::raw(""),
        Line::from(Span::styled(
            format!("This will remove the saved custom theme file for slug '{}'.", state.palette.slug),
            Style::default().fg(theme.text_dim),
        )),
        Line::from(Span::styled(
            "The current draft remains open as unsaved edits after deletion.",
            Style::default().fg(theme.text_dim),
        )),
        Line::raw(""),
        Line::from(vec![
            chip("y Delete", theme.destructive, theme),
            Span::raw(" "),
            chip("Esc Cancel", theme.chip_dismiss, theme),
        ]),
    ];
    f.render_widget(Paragraph::new(lines).alignment(Alignment::Left), inner);
    record_footer_chips(
        button_map,
        inner.x,
        inner.y.saturating_add(5),
        &[
            (TuiButton::ThemeBuilderDeleteConfirm, "y Delete"),
            (TuiButton::ThemeBuilderDeleteCancel, "Esc Cancel"),
        ],
    );
}

fn switch_line(label: &str, on_label: &str, off_label: &str, on: bool, disabled: bool, focused: bool, theme: theme::Theme) -> Line<'static> {
    let active = if disabled { Style::default().fg(theme.text_dim).bg(theme.input_disabled_bg) } else { Style::default().fg(theme.panel_bg).bg(theme.tab_active).add_modifier(Modifier::BOLD) };
    let inactive = Style::default().fg(if disabled { theme.text_dim } else { theme.text }).bg(theme.dropdown_bg);
    Line::from(vec![
        focus_mark(focused, theme),
        Span::styled(format!("{label:<21}"), Style::default().fg(theme.label)),
        Span::styled(format!(" {on_label} "), if on { active } else { inactive }),
        Span::raw(" vs "),
        Span::styled(format!(" {off_label} "), if on { inactive } else { active }),
    ])
}

fn focus_mark(focused: bool, theme: theme::Theme) -> Span<'static> {
    if focused {
        Span::styled("› ", Style::default().fg(theme.warning).add_modifier(Modifier::BOLD))
    } else {
        Span::raw("  ")
    }
}

fn input_style(focused: bool, theme: theme::Theme) -> Style {
    if focused {
        Style::default().fg(theme.text_bright).bg(theme.input_focused_bg).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.text).bg(theme.input_unfocused_bg)
    }
}

fn chip(label: &str, bg: Color, theme: theme::Theme) -> Span<'static> {
    Span::styled(format!(" {label} "), Style::default().fg(theme.pill_active_fg).bg(bg).add_modifier(Modifier::BOLD))
}

fn status_span(state: &ThemeBuilderState, theme: theme::Theme) -> Span<'static> {
    if let Some(status) = &state.status {
        Span::styled(format!("  {status}"), Style::default().fg(theme.text_dim))
    } else if state.dirty {
        Span::styled("  unsaved", Style::default().fg(theme.warning))
    } else {
        Span::raw("")
    }
}

fn selected_derived_key(index: usize) -> &'static str {
    theme::derived_element_specs()[index.min(theme::derived_element_specs().len() - 1)].key
}

fn default_swatch_name_for_slot(slot: BuilderSlot) -> &'static str {
    match slot {
        BuilderSlot::Role(index) => ROLE_KEYS[index.min(ROLE_KEYS.len() - 1)],
        BuilderSlot::Accent(index) => match index.min(15) {
            0 => "accent_00",
            1 => "accent_01",
            2 => "accent_02",
            3 => "accent_03",
            4 => "accent_04",
            5 => "accent_05",
            6 => "accent_06",
            7 => "accent_07",
            8 => "accent_08",
            9 => "accent_09",
            10 => "accent_10",
            11 => "accent_11",
            12 => "warm_accent",
            13 => "cool_accent",
            14 => "info_accent",
            _ => "success_accent",
        },
    }
}

fn sanitize_swatch_name(input: &str) -> Option<String> {
    let mut out = String::new();
    let mut previous_sep = false;
    for ch in input.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            previous_sep = false;
        } else if (ch == '_' || ch == '-' || ch.is_whitespace()) && !previous_sep && !out.is_empty() {
            out.push('_');
            previous_sep = true;
        }
    }
    while out.ends_with('_') {
        out.pop();
    }
    if out.is_empty() { None } else { Some(out) }
}

fn move_cursor(current: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }
    let max = len.saturating_sub(1) as isize;
    (current as isize + delta).clamp(0, max) as usize
}

fn open_preset_dropdown(state: &mut ThemeBuilderState) {
    state.refresh_theme_library();
    if state.theme_library.is_empty() {
        state.status = Some("No themes available".to_string());
        return;
    }
    let active_slug = state.palette.slug.trim().to_ascii_lowercase().replace('_', "-");
    state.preset_cursor = state.theme_library.iter()
        .position(|choice| choice.slug == active_slug || format!("{}-custom", choice.slug) == active_slug)
        .unwrap_or_else(|| state.preset_cursor.min(state.theme_library.len() - 1));
    let visible_rows = preset_visible_rows_for_state(state);
    sync_preset_scroll(state, visible_rows);
    state.view = ThemeBuilderView::Preset;
}

fn sync_preset_scroll(state: &mut ThemeBuilderState, visible_rows: usize) {
    let visible_rows = visible_rows.max(1);
    let max_scroll = state.theme_library.len().saturating_sub(visible_rows);
    state.preset_scroll = state.preset_scroll.min(max_scroll);
    if state.preset_cursor < state.preset_scroll {
        state.preset_scroll = state.preset_cursor;
    } else if state.preset_cursor >= state.preset_scroll.saturating_add(visible_rows) {
        state.preset_scroll = state.preset_cursor + 1 - visible_rows;
    }
    state.preset_scroll = state.preset_scroll.min(max_scroll);
}

fn move_preset_cursor(state: &mut ThemeBuilderState, delta: isize) {
    let len = state.theme_library.len();
    if len == 0 {
        return;
    }
    state.preset_cursor = move_cursor(state.preset_cursor, len, delta);
    let visible_rows = preset_visible_rows_for_state(state);
    sync_preset_scroll(state, visible_rows);
}

fn scaled_centered_rect(min_width: u16, min_height: u16, area: Rect) -> Rect {
    let width = ((u32::from(area.width) * 75) / 100).max(u32::from(min_width)) as u16;
    let height = ((u32::from(area.height) * 75) / 100).max(u32::from(min_height)) as u16;
    centered_rect(width, height, area)
}

fn proportional_width(total: u16, percent: u16, min_width: u16, max_width: u16) -> u16 {
    if total == 0 {
        return 0;
    }
    let scaled = ((u32::from(total) * u32::from(percent)) / 100) as u16;
    scaled.max(min_width.min(total)).min(max_width.min(total))
}

fn derived_visible_rows_for_state(state: &ThemeBuilderState) -> usize {
    state.derived_visible_rows.get().max(1)
}

fn preset_visible_rows_for_state(state: &ThemeBuilderState) -> usize {
    let fallback = if state.preset_applies_on_select {
        DEFAULT_GALLERY_VISIBLE_ROWS
    } else {
        DEFAULT_PRESET_VISIBLE_ROWS
    };
    let visible = state.preset_visible_rows.get();
    if visible == 0 { fallback } else { visible.max(1) }
}

fn selected_preset_slug(state: &ThemeBuilderState) -> Option<String> {
    state.theme_library
        .get(state.preset_cursor.min(state.theme_library.len().saturating_sub(1)))
        .map(|choice| choice.slug.clone())
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

fn record_rect(
    button_map: &mut ButtonRenderMap,
    button: TuiButton,
    x: u16,
    y: u16,
    width: u16,
    height: u16,
) {
    if width == 0 || height == 0 {
        return;
    }
    button_map.record_button(button, Rect { x, y, width, height });
}

fn record_footer_chips(button_map: &mut ButtonRenderMap, x: u16, y: u16, chips: &[(TuiButton, &str)]) {
    let mut cursor = x;
    for (button, label) in chips.iter().copied() {
        let width = label.len().saturating_add(2).min(u16::MAX as usize) as u16;
        record_rect(button_map, button, cursor, y, width, 1);
        cursor = cursor.saturating_add(width).saturating_add(1);
    }
}

fn record_editor_buttons(button_map: &mut ButtonRenderMap, area: Rect, state: &ThemeBuilderState) {
    let save_x = area
        .x
        .saturating_add(8)
        .saturating_add(state.selected_slot.label().len().min(24) as u16)
        .saturating_add(3);
    record_rect(button_map, TuiButton::ThemeBuilderSaveSwatch, save_x, area.y, 8, 1);
    record_rect(button_map, TuiButton::ThemeBuilderHexField, area.x, area.y.saturating_add(4), area.width.min(32), 1);
    for channel in 0..3 {
        record_rect(
            button_map,
            TuiButton::ThemeBuilderRgbSlider(channel),
            area.x,
            area.y.saturating_add(5 + channel as u16),
            area.width.min(24),
            1,
        );
    }

    let mut depth_x = area.x;
    for depth in ColorDepth::ALL {
        let width = depth.label().len().saturating_add(2).min(u16::MAX as usize) as u16;
        record_rect(button_map, TuiButton::ThemeBuilderDepth(depth), depth_x, area.y.saturating_add(10), width, 1);
        depth_x = depth_x.saturating_add(width).saturating_add(1);
    }

    let swatch_name_y = area.y.saturating_add(13);
    record_rect(button_map, TuiButton::ThemeBuilderSwatchNameField, area.x, swatch_name_y, area.width.min(36), 1);
    record_swatch_buttons(
        button_map,
        TuiButton::ThemeBuilderSaveSwatch,
        Some(TuiButton::ThemeBuilderDeleteSwatch),
        |index| TuiButton::ThemeBuilderSavedSwatch(index),
        area.x,
        swatch_name_y.saturating_add(1),
        state.palette.swatches.len(),
    );
    record_swatch_buttons(
        button_map,
        TuiButton::ThemeBuilderSaveSwatch,
        None,
        |index| TuiButton::ThemeBuilderRecentSwatch(index),
        area.x,
        swatch_name_y.saturating_add(2),
        state.recent_colors.len(),
    );
}

fn record_swatch_buttons<F>(
    button_map: &mut ButtonRenderMap,
    add_button: TuiButton,
    delete_button: Option<TuiButton>,
    mut swatch_button: F,
    x: u16,
    y: u16,
    count: usize,
) where
    F: FnMut(usize) -> TuiButton,
{
    let start = x.saturating_add(9);
    let visible = count.min(12);
    for index in 0..visible {
        record_rect(
            button_map,
            swatch_button(index),
            start.saturating_add(index as u16 * 3),
            y,
            2,
            1,
        );
    }
    let add_x = if visible == 0 {
        start.saturating_add(6)
    } else {
        start.saturating_add(visible as u16 * 3)
    };
    record_rect(button_map, add_button, add_x, y, 3, 1);
    if let Some(delete_button) = delete_button {
        record_rect(button_map, delete_button, add_x.saturating_add(4), y, 5, 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn edited_hex_updates_selected_palette_slot() {
        let mut state = ThemeBuilderState::from_palette(ThemePaletteDraft::from_palette(theme::default_palette()));
        state.editor_focus = BuilderEditorFocus::Hex;
        state.hex_input = TextInputState::new_selected("#000000".to_string());
        assert_eq!(handle_theme_builder_key(&mut state, key(KeyCode::Enter)), ThemeBuilderAction::None);
        assert_eq!(state.palette.panel_bg, Color::Rgb(0, 0, 0));
        assert!(state.dirty);
    }

    #[test]
    fn derived_lock_can_target_author_or_user_layer() {
        let mut state = ThemeBuilderState::from_palette(ThemePaletteDraft::from_palette(theme::default_palette()));
        state.view = ThemeBuilderView::Derived;
        state.derived_hex_input = TextInputState::new_selected("#010203".to_string());
        assert_eq!(handle_theme_builder_key(&mut state, key(KeyCode::Char('l'))), ThemeBuilderAction::None);
        assert_eq!(state.palette.derived_locks.get("surface"), Some(&Color::Rgb(1, 2, 3)));
        state.lock_target = DerivedLockTarget::UserOverride;
        state.derived_hex_input = TextInputState::new_selected("#040506".to_string());
        assert_eq!(handle_theme_builder_key(&mut state, key(KeyCode::Char('l'))), ThemeBuilderAction::None);
        assert_eq!(state.user_overrides.overrides.get("surface"), Some(&Color::Rgb(4, 5, 6)));
    }

    fn left_click() -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn mouse_click_selects_builder_slot() {
        let mut state = ThemeBuilderState::from_palette(ThemePaletteDraft::from_palette(theme::default_palette()));
        assert_eq!(
            handle_theme_builder_mouse(&mut state, left_click(), Some(TuiButton::ThemeBuilderSlot(BuilderSlot::Accent(12)))),
            ThemeBuilderAction::None,
        );
        assert_eq!(state.selected_slot, BuilderSlot::Accent(12));
        assert_eq!(state.editor_focus, BuilderEditorFocus::Slots);
    }

    #[test]
    fn mouse_click_saved_swatch_assigns_selected_color() {
        let mut state = ThemeBuilderState::from_palette(ThemePaletteDraft::from_palette(theme::default_palette()));
        state.palette.swatches.push(NamedSwatch::new("saved", Color::Rgb(9, 8, 7)));
        assert_eq!(
            handle_theme_builder_mouse(&mut state, left_click(), Some(TuiButton::ThemeBuilderSavedSwatch(0))),
            ThemeBuilderAction::None,
        );
        assert_eq!(state.selected_color(), Color::Rgb(9, 8, 7));
        assert_eq!(state.editor_focus, BuilderEditorFocus::SavedSwatches);
    }


    #[test]
    fn keyboard_can_select_arbitrary_saved_and_recent_swatches() {
        let mut state = ThemeBuilderState::from_palette(ThemePaletteDraft::from_palette(theme::default_palette()));
        state.palette.swatches.push(NamedSwatch::new("first", Color::Rgb(1, 1, 1)));
        state.palette.swatches.push(NamedSwatch::new("second", Color::Rgb(2, 2, 2)));
        state.editor_focus = BuilderEditorFocus::SavedSwatches;

        assert_eq!(handle_theme_builder_key(&mut state, key(KeyCode::Right)), ThemeBuilderAction::None);
        assert_eq!(handle_theme_builder_key(&mut state, key(KeyCode::Enter)), ThemeBuilderAction::None);
        assert_eq!(state.selected_color(), Color::Rgb(2, 2, 2));
        assert_eq!(state.swatch_name_input.text, "second");

        state.recent_colors = vec![Color::Rgb(3, 3, 3), Color::Rgb(4, 4, 4)];
        state.editor_focus = BuilderEditorFocus::RecentSwatches;
        assert_eq!(handle_theme_builder_key(&mut state, key(KeyCode::Right)), ThemeBuilderAction::None);
        assert_eq!(handle_theme_builder_key(&mut state, key(KeyCode::Enter)), ThemeBuilderAction::None);
        assert_eq!(state.selected_color(), Color::Rgb(4, 4, 4));
    }

    #[test]
    fn swatch_name_field_updates_existing_named_swatch() {
        let mut state = ThemeBuilderState::from_palette(ThemePaletteDraft::from_palette(theme::default_palette()));
        state.swatch_name_input = TextInputState::new_selected("brand purple".to_string());
        state.set_selected_color(Color::Rgb(10, 20, 30));
        state.save_current_swatch();
        assert_eq!(state.palette.swatches[0].name, "brand_purple");
        assert_eq!(state.palette.swatches[0].color, Color::Rgb(10, 20, 30));

        state.set_selected_color(Color::Rgb(40, 50, 60));
        state.swatch_name_input = TextInputState::new_selected("brand_purple".to_string());
        state.save_current_swatch();
        assert_eq!(state.palette.swatches.len(), 1);
        assert_eq!(state.palette.swatches[0].color, Color::Rgb(40, 50, 60));
    }

    #[test]
    fn preset_key_opens_real_dropdown_before_loading() {
        let mut state = ThemeBuilderState::from_palette(ThemePaletteDraft::from_palette(theme::default_palette()));
        assert_eq!(handle_theme_builder_key(&mut state, key(KeyCode::Char('p'))), ThemeBuilderAction::None);
        assert_eq!(state.view, ThemeBuilderView::Preset);
    }

    #[test]
    fn selected_depth_quantizes_resolved_preview_theme() {
        let mut state = ThemeBuilderState::from_palette(ThemePaletteDraft::from_palette(theme::default_palette()));
        state.depth_mode = ColorDepth::Xterm256;
        let resolved = state.resolved_theme();
        assert_eq!(resolved.panel_bg, theme::quantize_color_for_depth(state.palette.panel_bg, ColorDepth::Xterm256));
    }
    #[test]
    fn mouse_click_apply_confirm_returns_apply_action() {
        let mut state = ThemeBuilderState::from_palette(ThemePaletteDraft::from_palette(theme::default_palette()));
        state.view = ThemeBuilderView::Apply;
        assert_eq!(
            handle_theme_builder_mouse(&mut state, left_click(), Some(TuiButton::ThemeBuilderApplyConfirm)),
            ThemeBuilderAction::Apply,
        );
    }

    #[test]
    fn delete_theme_requires_confirmation_before_filesystem_action() {
        let mut state = ThemeBuilderState::from_palette(ThemePaletteDraft::from_palette(theme::default_palette()));
        state.palette.slug = "missing-theme".to_string();
        state.palette.source = ThemeDraftSource::Custom;

        state.request_delete_current_custom_theme();
        assert_eq!(state.view, ThemeBuilderView::DeleteConfirm);
        assert!(state.deleted_theme_slug.is_none());

        state.cancel_delete_current_custom_theme();
        assert_eq!(state.view, ThemeBuilderView::Main);
        assert!(state.deleted_theme_slug.is_none());

        state.request_delete_current_custom_theme();
        state.confirm_delete_current_custom_theme();
        assert!(state.deleted_theme_slug.is_none());
        assert!(state.status.unwrap_or_default().starts_with("Delete theme failed:"));
    }




    #[test]
    fn gallery_enter_returns_builtin_slug_without_custom_draft_suffix() {
        let choices = theme::theme_choices();
        let gruvbox_index = choices.iter()
            .position(|choice| choice.slug == "gruvbox")
            .expect("gruvbox choice");
        let mut state = ThemeBuilderState::theme_gallery_from_active_theme_with_library(
            theme::theme_by_slug("tokyo-night").expect("theme"),
            gruvbox_index,
            choices,
        );

        match handle_theme_builder_key(&mut state, key(KeyCode::Enter)) {
            ThemeBuilderAction::ApplyPreset(slug) => {
                assert_eq!(slug, "gruvbox");
                assert!(!slug.ends_with("-custom"));
            }
            other => panic!("expected ApplyPreset action, got {:?}", other),
        }
    }

    #[test]
    fn preset_gallery_uses_cached_theme_library_for_page_math() {
        let mut choices = theme::theme_choices();
        choices.truncate(5);
        let mut state = ThemeBuilderState::theme_gallery_from_active_theme_with_library(
            theme::theme_by_slug("tokyo-night").expect("theme"),
            0,
            choices,
        );
        state.preset_visible_rows.set(25);

        assert_eq!(handle_theme_builder_key(&mut state, key(KeyCode::PageDown)), ThemeBuilderAction::None);
        assert_eq!(state.preset_cursor, 4, "page movement must clamp to cached library length");
        assert_eq!(state.preset_scroll, 0, "oversized rendered capacity should not invent scroll");
    }

    #[test]
    fn derived_keyboard_scroll_uses_rendered_visible_rows() {
        let mut state = ThemeBuilderState::from_palette(ThemePaletteDraft::from_palette(theme::default_palette()));
        state.view = ThemeBuilderView::Derived;
        state.derived_visible_rows.set(30);

        for _ in 0..28 {
            assert_eq!(handle_theme_builder_key(&mut state, key(KeyCode::Down)), ThemeBuilderAction::None);
        }
        assert_eq!(state.derived_cursor, 28);
        assert_eq!(state.derived_scroll, 0);

        // One more Down should not advance past the last element but may adjust scroll.
        assert_eq!(handle_theme_builder_key(&mut state, key(KeyCode::Down)), ThemeBuilderAction::None);
        assert_eq!(state.derived_cursor, 28);
    }

    #[test]
    fn preset_page_movement_uses_rendered_gallery_capacity() {
        let mut state = ThemeBuilderState::from_palette(ThemePaletteDraft::from_palette(theme::default_palette()));
        let choices_len = theme::theme_choices().len();
        assert!(choices_len > 1);
        state.view = ThemeBuilderView::Preset;
        state.preset_applies_on_select = true;
        state.preset_visible_rows.set(25);

        assert_eq!(handle_theme_builder_key(&mut state, key(KeyCode::PageDown)), ThemeBuilderAction::None);
        assert_eq!(state.preset_cursor, 25.min(choices_len - 1));
        assert_eq!(state.preset_scroll, state.preset_cursor.saturating_add(1).saturating_sub(25));
    }

    fn cached_gallery_choice() -> theme::ThemeChoice {
        let mut accents = [Color::Rgb(0, 0, 0); theme::THEME_ACCENT_COUNT];
        for (index, color) in accents.iter_mut().enumerate() {
            *color = Color::Rgb(
                30u8.saturating_add(index as u8),
                90u8.saturating_add(index as u8),
                170u8.saturating_add(index as u8),
            );
        }
        theme::ThemeChoice {
            slug: "cached-gallery-only".to_string(),
            name: "Cached Gallery Only".to_string(),
            description: "Injected gallery preview".to_string(),
            dark: true,
            built_in: false,
            author_lock_count: 0,
            accents,
        }
    }

    fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
        let area = buffer.area;
        let mut out = String::new();
        for y in area.y..area.y.saturating_add(area.height) {
            for x in area.x..area.x.saturating_add(area.width) {
                out.push_str(buffer.get(x, y).symbol());
            }
            out.push('\n');
        }
        out
    }

    fn rendered_color_count(buffer: &ratatui::buffer::Buffer, expected_colors: &[Color]) -> usize {
        expected_colors.iter().copied().fold(Vec::new(), |mut unique, color| {
            if !unique.contains(&color) {
                unique.push(color);
            }
            unique
        }).into_iter().filter(|expected| {
            buffer.content().iter().any(|cell| {
                cell.symbol().chars().any(|ch| ch == '█')
                    && (cell.fg == *expected || cell.bg == *expected)
            })
        }).count()
    }

    #[test]
    fn preview_lines_expand_when_editor_height_has_room() {
        let state = ThemeBuilderState::from_palette(ThemePaletteDraft::from_palette(theme::default_palette()));
        let theme = theme::theme_by_slug("tokyo-night").expect("theme");

        let compact = preview_lines(&state, theme, 4);
        let roomy = preview_lines(&state, theme, 8);

        assert_eq!(compact.len(), 4);
        assert!(roomy.len() > compact.len());
        let has_progress = roomy.iter().any(|line| {
            line.spans.iter().any(|span| span.content.contains("progress"))
        });
        let has_swatches = roomy.iter().any(|line| {
            line.spans.iter().any(|span| span.content.contains("swatches"))
        });
        assert!(has_progress);
        assert!(has_swatches);
    }

    #[test]
    fn gallery_renderer_uses_injected_cached_library_preview_data() {
        let choice = cached_gallery_choice();
        let expected_accents = choice.accents;
        let state = ThemeBuilderState::theme_gallery_from_active_theme_with_library(
            theme::theme_by_slug("tokyo-night").expect("theme"),
            0,
            vec![choice],
        );
        let theme = theme::theme_by_slug("tokyo-night").expect("theme");
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let mut button_map = ButtonRenderMap::new();

        terminal.draw(|frame| draw_theme_builder(frame, &state, &mut button_map, theme)).expect("draw");

        let buffer = terminal.backend().buffer();
        let text = buffer_text(buffer);
        assert!(text.contains("Cached Gallery Only"));
        assert!(!text.contains("Tokyo Night"), "gallery rows should come from the injected snapshot, not discovery or active theme metadata");
        assert_eq!(
            rendered_color_count(buffer, &expected_accents[..10]),
            10,
            "gallery should render injected preview swatches",
        );
    }

    #[test]
    fn delete_confirmation_mouse_cancel_does_not_delete() {
        let mut state = ThemeBuilderState::from_palette(ThemePaletteDraft::from_palette(theme::default_palette()));
        state.palette.slug = "missing-theme".to_string();
        state.palette.source = ThemeDraftSource::Custom;
        state.view = ThemeBuilderView::DeleteConfirm;

        assert_eq!(
            handle_theme_builder_mouse(&mut state, left_click(), Some(TuiButton::ThemeBuilderDeleteCancel)),
            ThemeBuilderAction::None,
        );
        assert_eq!(state.view, ThemeBuilderView::Main);
        assert!(state.deleted_theme_slug.is_none());
    }
}
