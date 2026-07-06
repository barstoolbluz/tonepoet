
//! Interactive theme builder overlay.
//!
//! The builder owns an editable copy of the public palette inputs, exposes
//! derived colors as theme-authored locks, and presents navigation as tabs plus
//! floating overlays so browsing/actions do not destroy editing context.

use std::{cell::Cell, path::{Path, PathBuf}};

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
    self, BuilderSlot, ColorDepth, NamedSwatch, ThemeApplyOptions, ThemeDraftSource,
    ThemeOverrides, ThemePaletteDraft, ROLE_KEYS, ROLE_LABELS,
};

const MAX_RECENT_COLORS: usize = 12;
const MAX_SAVED_SWATCHES: usize = 12;
const DEFAULT_DERIVED_VISIBLE_ROWS: usize = 16;
const DEFAULT_GALLERY_VISIBLE_ROWS: usize = 18;

const BLOCK: &str = "\u{2588}\u{2588}";
const BAR_FILLED: &str = "\u{2588}";
const BAR_EMPTY: &str = "\u{2591}";
const SWATCH_LEFT: &str = "\u{2595}";
const SWATCH_RIGHT: &str = "\u{258F}";
const AUTO_MARK: &str = "\u{25CB}";
const LOCK_MARK: &str = "\u{25CF}";
const ARROW: &str = "\u{2192}";
const ELLIPSIS_MORE: &str = "\u{2026} more";

#[derive(Debug, Clone)]
pub struct ThemeBuilderState {
    pub palette: ThemePaletteDraft,
    pub selected_slot: BuilderSlot,
    pub last_role_slot: BuilderSlot,
    pub hex_input: TextInputState,
    pub rgb_values: [u8; 3],
    pub depth_mode: ColorDepth,
    pub recent_colors: Vec<Color>,
    pub saved_swatch_cursor: usize,
    pub recent_swatch_cursor: usize,
    pub swatch_name_input: TextInputState,
    pub swatch_naming_active: bool,
    pub user_overrides: ThemeOverrides,
    pub tab: BuilderTab,
    pub overlay: BuilderOverlay,
    pub editor_focus: BuilderEditorFocus,
    pub derived_cursor: usize,
    /// Scroll offset into the flattened derived list, including nonselectable
    /// group headers. Keeping the offset in rendered-row space avoids jitter
    /// when headers enter or leave the viewport.
    pub derived_scroll: usize,
    pub derived_visible_rows: Cell<usize>,
    pub derived_hex_input: TextInputState,
    pub apply_dialog: ApplyDialogState,
    pub more_menu: MoreMenuState,
    pub preset_cursor: usize,
    pub preset_scroll: usize,
    pub preset_visible_rows: Cell<usize>,
    /// Gallery column count from the most recent render. Keyboard navigation
    /// uses this geometry so it stays in lockstep with the visible grid.
    pub preset_visible_columns: Cell<usize>,
    pub gallery_filter_input: TextInputState,
    pub gallery_filter_active: bool,
    pub export_path_input: TextInputState,
    pub import_path_input: TextInputState,
    pub gallery_dark: bool,
    /// Cached theme-library snapshot used by the gallery. Renderers must use
    /// this snapshot and never rescan the filesystem during draw.
    pub theme_library: Vec<theme::ThemeChoice>,
    /// True when the gallery is opened from Config. Selecting a family applies
    /// the chosen library slug directly instead of importing it as an editable
    /// builder draft.
    pub preset_applies_on_select: bool,
    pub dirty: bool,
    pub status: Option<String>,
    pub deleted_theme_slug: Option<String>,
}

/// Which tab is active in the persistent two-pane builder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuilderTab {
    Edit,
    Preview,
    Derived,
}

impl BuilderTab {
    fn from_index(index: usize) -> Self {
        match index {
            1 => Self::Preview,
            2 => Self::Derived,
            _ => Self::Edit,
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Edit => 0,
            Self::Preview => 1,
            Self::Derived => 2,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Edit => "Edit",
            Self::Preview => "Preview",
            Self::Derived => "Derived",
        }
    }
}

/// Floating overlay state. Overlays are transient and return to the same tab
/// and focus state when dismissed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuilderOverlay {
    None,
    Gallery,
    MoreMenu,
    Apply,
    DeleteConfirm,
    ExportDialog,
    ImportDialog,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuilderEditorFocus {
    Slots,
    Hex,
    Red,
    Green,
    Blue,
}

impl BuilderEditorFocus {
    fn next(self) -> Self {
        match self {
            Self::Slots => Self::Hex,
            Self::Hex => Self::Red,
            Self::Red => Self::Green,
            Self::Green => Self::Blue,
            Self::Blue => Self::Slots,
        }
    }

    fn previous(self) -> Self {
        match self {
            Self::Slots => Self::Blue,
            Self::Hex => Self::Slots,
            Self::Red => Self::Hex,
            Self::Green => Self::Red,
            Self::Blue => Self::Green,
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
pub struct MoreMenuState {
    pub cursor: usize,
    pub items: Vec<MoreMenuItem>,
}

impl Default for MoreMenuState {
    fn default() -> Self {
        Self { cursor: 0, items: Vec::new() }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoreMenuItem {
    Revert,
    Duplicate,
    Delete,
    Separator,
    Export,
    Import,
}

impl MoreMenuItem {
    fn label(self) -> &'static str {
        match self {
            Self::Revert => "r  Revert",
            Self::Duplicate => "d  Duplicate",
            Self::Delete => "x  Delete",
            Self::Separator => "----------------",
            Self::Export => "e  Export .theme",
            Self::Import => "i  Import .theme",
        }
    }

    fn is_selectable(self) -> bool {
        !matches!(self, Self::Separator)
    }
}

#[must_use = "theme-builder actions must be handled by the caller; ApplyPreset is required for standalone Config gallery"]
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

#[derive(Debug, Clone)]
struct GalleryFamily {
    key: String,
    name: String,
    dark: Option<theme::ThemeChoice>,
    light: Option<theme::ThemeChoice>,
    fallback: theme::ThemeChoice,
}

#[derive(Debug, Clone)]
struct DerivedListRow {
    spec_index: Option<usize>,
    group: &'static str,
}

impl ThemeBuilderState {
    pub fn from_active_theme(theme: theme::Theme) -> Self {
        Self::from_active_theme_with_library(theme, theme::theme_choices())
    }

    pub fn from_active_theme_with_library(theme: theme::Theme, choices: Vec<theme::ThemeChoice>) -> Self {
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
        let selected_slug = choices.get(selected).map(|choice| choice.slug.clone());
        let mut state = Self::from_active_theme_with_library(theme, choices);
        state.overlay = BuilderOverlay::Gallery;
        state.preset_applies_on_select = true;
        if let Some(selected_slug) = selected_slug {
            let families = visible_gallery_families(&state);
            if let Some((index, dark_variant)) = families.iter().enumerate().find_map(|(index, family)| {
                if family.dark.as_ref().map(|choice| choice.slug.as_str()) == Some(selected_slug.as_str()) {
                    Some((index, true))
                } else if family.light.as_ref().map(|choice| choice.slug.as_str()) == Some(selected_slug.as_str()) {
                    Some((index, false))
                } else if family.fallback.slug == selected_slug {
                    Some((index, family.fallback.dark))
                } else {
                    None
                }
            }) {
                state.preset_cursor = index;
                state.gallery_dark = dark_variant;
            } else {
                state.preset_cursor = 0;
            }
        } else {
            state.preset_cursor = 0;
        }
        state.preset_visible_rows.set(DEFAULT_GALLERY_VISIBLE_ROWS);
        state.preset_visible_columns.set(1);
        sync_gallery_scroll(&mut state, DEFAULT_GALLERY_VISIBLE_ROWS);
        state.status = Some("Select a theme to apply it".to_string());
        state
    }

    pub fn set_gallery_cursor_to_slug(&mut self, slug: &str) -> bool {
        let families = visible_gallery_families(self);
        if let Some((index, dark_variant)) = families.iter().enumerate().find_map(|(index, family)| {
            if family.dark.as_ref().map(|choice| choice.slug.as_str()) == Some(slug) {
                Some((index, true))
            } else if family.light.as_ref().map(|choice| choice.slug.as_str()) == Some(slug) {
                Some((index, false))
            } else if family.fallback.slug == slug {
                Some((index, family.fallback.dark))
            } else {
                None
            }
        }) {
            self.preset_cursor = index;
            self.gallery_dark = dark_variant;
            sync_gallery_scroll(self, gallery_visible_rows_for_state(self));
            true
        } else {
            false
        }
    }

    pub fn selected_gallery_family_contains_slug(&self, slug: &str) -> bool {
        let families = visible_gallery_families(self);
        families.get(self.preset_cursor.min(families.len().saturating_sub(1)))
            .map(|family| {
                family.dark.as_ref().map(|choice| choice.slug.as_str()) == Some(slug)
                    || family.light.as_ref().map(|choice| choice.slug.as_str()) == Some(slug)
                    || family.fallback.slug == slug
            })
            .unwrap_or(false)
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
        let gallery_dark = palette.dark;
        Self {
            palette,
            selected_slot,
            last_role_slot: selected_slot,
            hex_input: TextInputState::new_selected(theme::color_to_hex(color)),
            rgb_values: [r, g, b],
            depth_mode: ColorDepth::TrueColor,
            recent_colors: Vec::new(),
            saved_swatch_cursor: 0,
            recent_swatch_cursor: 0,
            swatch_name_input: TextInputState::new_selected(default_swatch_name_for_slot(selected_slot).to_string()),
            swatch_naming_active: false,
            user_overrides,
            tab: BuilderTab::Edit,
            overlay: BuilderOverlay::None,
            editor_focus: BuilderEditorFocus::Slots,
            derived_cursor: 0,
            derived_scroll: 0,
            derived_visible_rows: Cell::new(DEFAULT_DERIVED_VISIBLE_ROWS),
            derived_hex_input: TextInputState::new_selected(theme::color_to_hex(derived_color)),
            apply_dialog: ApplyDialogState {
                honor_theme_locks: true,
                keep_user_overrides: true,
                focus: ApplyDialogFocus::ThemeLocks,
            },
            more_menu: MoreMenuState::default(),
            preset_cursor: 0,
            preset_scroll: 0,
            preset_visible_rows: Cell::new(0),
            preset_visible_columns: Cell::new(1),
            gallery_filter_input: TextInputState::empty(),
            gallery_filter_active: false,
            export_path_input: TextInputState::empty(),
            import_path_input: TextInputState::empty(),
            gallery_dark,
            theme_library,
            preset_applies_on_select: false,
            dirty: false,
            status: None,
            deleted_theme_slug: None,
        }
    }

    pub fn refresh_theme_library(&mut self) {
        self.theme_library = theme::theme_choices();
        clamp_gallery_cursor(self);
    }

    pub fn replace_theme_library(&mut self, choices: Vec<theme::ThemeChoice>) {
        self.theme_library = choices;
        clamp_gallery_cursor(self);
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

    fn activate_slot(&mut self, slot: BuilderSlot) {
        self.editor_focus = BuilderEditorFocus::Slots;
        match slot {
            // A plain accent click is an assignment action.  Use the last
            // role the user selected, not the current slot, so the operation
            // is stable even if a previous navigation step or stale hit path
            // left an accent highlighted before the click reached us.  This is
            // the mouse analogue of pressing Enter on a highlighted accent.
            BuilderSlot::Accent(index) => self.assign_accent_to_role(self.last_role_slot, index),
            BuilderSlot::Role(_) => self.set_selected_slot(slot),
        }
    }

    fn select_slot_for_editing(&mut self, slot: BuilderSlot) {
        self.editor_focus = BuilderEditorFocus::Slots;
        self.set_selected_slot(slot);
    }

    fn assign_accent_to_role(&mut self, role_slot: BuilderSlot, accent_index: usize) {
        if !matches!(role_slot, BuilderSlot::Role(_)) {
            return;
        }
        let Some(color) = self.palette.accents.get(accent_index).copied() else {
            self.status = Some(format!("Accent {accent_index} is out of range"));
            return;
        };
        self.editor_focus = BuilderEditorFocus::Slots;
        self.set_selected_slot(role_slot);
        self.set_selected_color(color);
        self.status = Some(format!("Assigned accent {accent_index:02} to {}", role_slot.label()));
    }

    fn apply_selected_accent_to_last_role(&mut self) -> bool {
        let BuilderSlot::Accent(index) = self.selected_slot else {
            return false;
        };
        self.assign_accent_to_role(self.last_role_slot, index);
        true
    }

    fn sync_hex_and_rgb_from_slot(&mut self) {
        let color = self.selected_color();
        self.hex_input = TextInputState::new_selected(theme::color_to_hex(color));
        let (r, g, b) = theme::rgb_tuple(color);
        self.rgb_values = [r, g, b];
    }

    fn set_selected_slot(&mut self, slot: BuilderSlot) {
        self.selected_slot = slot;
        if matches!(slot, BuilderSlot::Role(_)) {
            self.last_role_slot = slot;
        }
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

    fn selected_derived_auto_color(&self) -> Color {
        let key = self.selected_derived_key();
        let auto = theme::preview_resolve_theme_draft_for_depth(
            &self.palette,
            ThemeApplyOptions { honor_theme_locks: false, keep_user_overrides: false },
            &ThemeOverrides::default(),
            self.depth_mode,
        );
        theme::theme_color_by_derived_key(auto, key).unwrap_or(self.selected_color())
    }

    fn selected_derived_display_color(&self) -> Color {
        let key = self.selected_derived_key();
        self.palette.derived_locks.get(key).copied().unwrap_or_else(|| self.selected_derived_auto_color())
    }

    fn selected_derived_locked(&self) -> bool {
        self.palette.derived_locks.contains_key(self.selected_derived_key())
    }

    fn sync_derived_hex_from_selected(&mut self) {
        let color = self.selected_derived_display_color();
        self.derived_hex_input = TextInputState::new_selected(theme::color_to_hex(color));
    }

    fn release_selected_derived(&mut self) {
        let key = self.selected_derived_key().to_string();
        if self.palette.derived_locks.remove(&key).is_some() {
            self.dirty = true;
        }
        self.editor_focus = BuilderEditorFocus::Slots;
        self.sync_derived_hex_from_selected();
    }

    fn toggle_selected_derived_lock(&mut self) {
        if self.selected_derived_locked() {
            self.release_selected_derived();
            self.status = Some("Derived color returned to auto".to_string());
        } else {
            let key = self.selected_derived_key().to_string();
            let seed = self.selected_derived_display_color();
            let previous = self.palette.derived_locks.insert(key, seed);
            if previous != Some(seed) {
                self.dirty = true;
            }
            self.editor_focus = BuilderEditorFocus::Hex;
            self.sync_derived_hex_from_selected();
            self.status = Some("Derived color locked from current value".to_string());
        }
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
        if !self.selected_derived_locked() {
            self.status = Some("Press space to lock this derived color before editing".to_string());
            return Err("derived color is auto".to_string());
        }
        match theme::parse_hex_color(&self.derived_hex_input.text) {
            Ok(color) => {
                let key = self.selected_derived_key().to_string();
                let previous = self.palette.derived_locks.insert(key, color);
                if previous != Some(color) {
                    self.dirty = true;
                }
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

    pub fn flush_active_editor_input(&mut self) -> Result<(), String> {
        match (self.tab, self.editor_focus) {
            (BuilderTab::Edit, BuilderEditorFocus::Hex) => self.apply_hex_to_selected_slot(),
            (BuilderTab::Derived, BuilderEditorFocus::Hex) if self.selected_derived_locked() => {
                self.apply_hex_to_selected_derived()
            }
            _ => Ok(()),
        }
    }

    fn adjust_derived_rgb_channel(&mut self, channel: usize, delta: i16) {
        if !self.selected_derived_locked() {
            return;
        }
        let color = theme::parse_hex_color(&self.derived_hex_input.text)
            .unwrap_or_else(|_| self.selected_derived_display_color());
        let (mut r, mut g, mut b) = theme::rgb_tuple(color);
        let channel_value = match channel.min(2) {
            0 => &mut r,
            1 => &mut g,
            _ => &mut b,
        };
        *channel_value = (i16::from(*channel_value) + delta).clamp(0, 255) as u8;
        let next = theme::color_from_rgb_tuple((r, g, b));
        if next == color {
            return;
        }
        let key = self.selected_derived_key().to_string();
        let previous = self.palette.derived_locks.insert(key, next);
        self.derived_hex_input = TextInputState::new_selected(theme::color_to_hex(next));
        if previous != Some(next) {
            self.dirty = true;
        }
    }

    fn begin_swatch_naming(&mut self) {
        let suggested = self.palette
            .slot_binding_name(self.selected_slot)
            .unwrap_or_else(|| default_swatch_name_for_slot(self.selected_slot));
        self.swatch_name_input = TextInputState::new_selected(suggested.to_string());
        self.swatch_naming_active = true;
        self.status = Some("Name this swatch, then press Enter to save or Esc to cancel".to_string());
    }

    fn cancel_swatch_naming(&mut self) {
        self.swatch_naming_active = false;
        self.status = Some("Swatch save canceled".to_string());
    }

    fn save_current_swatch(&mut self) {
        let color = self.selected_color();
        let fallback = default_swatch_name_for_slot(self.selected_slot);
        let requested = sanitize_swatch_name(&self.swatch_name_input.text)
            .unwrap_or_else(|| fallback.to_string());

        if let Some(index) = self.palette.swatches.iter().position(|swatch| swatch.name == requested) {
            let previous_binding = self.palette.slot_binding_name(self.selected_slot).map(str::to_owned);
            let previous_swatch_color = self.palette.swatches[index].color;
            self.palette.update_swatch_color(&requested, color);
            let _ = self.palette.bind_slot_to_swatch(self.selected_slot, &requested);
            self.saved_swatch_cursor = index;
            self.swatch_name_input = TextInputState::new_selected(requested.clone());
            self.swatch_naming_active = false;
            self.sync_hex_and_rgb_from_slot();
            self.status = Some(format!("Updated swatch {requested}; bound selected slot"));
            if previous_swatch_color != color || previous_binding.as_deref() != Some(requested.as_str()) {
                self.dirty = true;
            }
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
        self.swatch_naming_active = false;
        self.sync_hex_and_rgb_from_slot();
        self.status = Some(format!("Saved and bound swatch {requested}"));
        self.dirty = true;
    }

    fn apply_saved_swatch(&mut self, index: usize) {
        if let Some(swatch) = self.palette.swatches.get(index).cloned() {
            let previous = self.selected_color();
            let previous_binding = self.palette.slot_binding_name(self.selected_slot).map(str::to_owned);
            self.saved_swatch_cursor = index;
            self.swatch_name_input = TextInputState::new_selected(swatch.name.clone());
            self.swatch_naming_active = false;
            match self.palette.bind_slot_to_swatch(self.selected_slot, &swatch.name) {
                Ok(()) => {
                    if previous != swatch.color {
                        self.push_recent(previous);
                    }
                    self.sync_hex_and_rgb_from_slot();
                    self.status = Some(format!("Bound selected slot to swatch {}", swatch.name));
                    if previous != swatch.color || previous_binding.as_deref() != Some(swatch.name.as_str()) {
                        self.dirty = true;
                    }
                }
                Err(err) => self.status = Some(format!("Swatch bind failed: {err}")),
            }
        }
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
                self.last_role_slot = self.selected_slot;
                self.saved_swatch_cursor = 0;
                self.swatch_naming_active = false;
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

    fn duplicate_current_theme(&mut self) {
        let base_slug = format!("{}-copy", self.palette.slug);
        match theme::unique_custom_theme_slug(&base_slug) {
            Ok(duplicate_slug) => {
                self.palette.slug = duplicate_slug;
                if !self.palette.name.to_ascii_lowercase().ends_with(" copy") {
                    self.palette.name = format!("{} Copy", self.palette.name);
                }
                self.palette.source = ThemeDraftSource::NewCustom;
                self.dirty = true;
                self.status = Some(format!("Duplicated as unsaved draft '{}'", self.palette.slug));
            }
            Err(err) => {
                self.status = Some(format!("Duplicate failed: {err}"));
            }
        }
    }

    fn default_export_path(&self) -> PathBuf {
        let slug = self.palette
            .save_slug()
            .unwrap_or_else(|_| self.palette.slug.clone());
        theme::custom_theme_path_for_slug(&slug)
            .unwrap_or_else(|_| theme::custom_theme_dir().join(format!("{slug}.toml")))
    }

    fn begin_export_dialog(&mut self) {
        self.export_path_input = TextInputState::new_selected(self.default_export_path().display().to_string());
        self.overlay = BuilderOverlay::ExportDialog;
        self.status = Some("Choose an export path, then press Enter".to_string());
    }

    fn begin_import_dialog(&mut self) {
        let default_path = theme::custom_theme_dir().join("import.toml");
        self.import_path_input = TextInputState::new_selected(default_path.display().to_string());
        self.overlay = BuilderOverlay::ImportDialog;
        self.status = Some("Enter a .toml theme file path to import".to_string());
    }

    fn export_current_theme(&mut self) {
        let path = expand_user_path(self.export_path_input.text.trim());
        if path.as_os_str().is_empty() {
            self.status = Some("Export failed: path is empty".to_string());
            return;
        }
        if let Err(err) = self.flush_active_editor_input() {
            self.status = Some(format!("Export failed: {err}"));
            return;
        }
        let mut draft = self.palette.clone();
        if matches!(draft.source, ThemeDraftSource::BuiltIn | ThemeDraftSource::NewCustom) || theme::is_builtin_theme_slug(&draft.slug) {
            let base = if theme::is_builtin_theme_slug(&draft.slug) {
                format!("{}-custom", draft.slug)
            } else {
                draft.slug.clone()
            };
            match theme::unique_custom_theme_slug_excluding_path(&base, Some(&path)) {
                Ok(slug) => draft.slug = slug,
                Err(err) => {
                    self.status = Some(format!("Export failed: {err}"));
                    return;
                }
            }
        }
        draft.source = ThemeDraftSource::Custom;
        match theme::export_theme_file_to_path(&draft, &path) {
            Ok(written) => {
                let saved_canonical = theme::is_canonical_custom_theme_path_for_slug(&written, &draft.slug)
                    .unwrap_or(false);
                if saved_canonical {
                    self.palette = draft;
                    self.palette.source = ThemeDraftSource::Custom;
                    self.dirty = false;
                }
                self.refresh_theme_library();
                self.overlay = BuilderOverlay::None;
                self.status = Some(format!("Exported theme to {}", written.display()));
            }
            Err(err) => self.status = Some(format!("Export failed: {err}")),
        }
    }

    fn import_theme_from_dialog(&mut self) {
        let path = expand_user_path(self.import_path_input.text.trim());
        if path.as_os_str().is_empty() {
            self.status = Some("Import failed: path is empty".to_string());
            return;
        }
        match theme::import_theme_file_to_custom_dir(&path) {
            Ok((draft, written)) => {
                self.palette = draft;
                self.selected_slot = BuilderSlot::Role(0);
                self.last_role_slot = self.selected_slot;
                self.saved_swatch_cursor = 0;
                self.swatch_naming_active = false;
                self.swatch_name_input = TextInputState::new_selected(default_swatch_name_for_slot(self.selected_slot).to_string());
                self.gallery_dark = self.palette.dark;
                self.sync_hex_and_rgb_from_slot();
                self.sync_derived_hex_from_selected();
                self.dirty = false;
                self.refresh_theme_library();
                self.overlay = BuilderOverlay::None;
                self.status = Some(format!("Imported theme to {}", written.display()));
            }
            Err(err) => self.status = Some(format!("Import failed: {err}")),
        }
    }

    fn request_delete_current_custom_theme(&mut self) {
        if !matches!(self.palette.source, ThemeDraftSource::Custom) {
            self.status = Some("Only saved custom themes can be deleted".to_string());
            return;
        }
        self.overlay = BuilderOverlay::DeleteConfirm;
        self.status = Some(format!("Confirm deletion of custom theme '{}'", self.palette.name));
    }

    fn cancel_delete_current_custom_theme(&mut self) {
        self.overlay = BuilderOverlay::None;
        self.status = Some("Theme deletion canceled".to_string());
    }

    fn confirm_delete_current_custom_theme(&mut self) {
        if !matches!(self.palette.source, ThemeDraftSource::Custom) {
            self.overlay = BuilderOverlay::None;
            self.status = Some("Only saved custom themes can be deleted".to_string());
            return;
        }
        let deleted_slug = self.palette.slug.clone();
        match theme::delete_custom_theme_file(&deleted_slug) {
            Ok(path) => {
                self.palette.source = ThemeDraftSource::NewCustom;
                self.deleted_theme_slug = Some(deleted_slug);
                self.dirty = true;
                self.overlay = BuilderOverlay::None;
                self.status = Some(format!("Deleted custom theme {}; current edits remain open as unsaved", path.display()));
            }
            Err(err) => {
                self.overlay = BuilderOverlay::None;
                self.status = Some(format!("Delete theme failed: {err}"));
            }
        }
    }

    fn load_gallery_choice(&mut self, choice: theme::ThemeChoice) {
        match theme::load_theme_draft(&choice.slug) {
            Ok(mut draft) => {
                if matches!(draft.source, ThemeDraftSource::BuiltIn) {
                    draft.slug = format!("{}-custom", draft.slug);
                    draft.name = format!("{} Custom", draft.name);
                    draft.source = ThemeDraftSource::NewCustom;
                }
                self.palette = draft;
                self.selected_slot = BuilderSlot::Role(0);
                self.last_role_slot = self.selected_slot;
                self.saved_swatch_cursor = 0;
                self.swatch_naming_active = false;
                self.swatch_name_input = TextInputState::new_selected(default_swatch_name_for_slot(self.selected_slot).to_string());
                self.gallery_dark = self.palette.dark;
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
    match state.overlay {
        BuilderOverlay::Gallery => return handle_gallery_key(state, key),
        BuilderOverlay::MoreMenu => return handle_more_menu_key(state, key),
        BuilderOverlay::Apply => return handle_apply_key(state, key),
        BuilderOverlay::DeleteConfirm => return handle_delete_confirm_key(state, key),
        BuilderOverlay::ExportDialog => return handle_export_dialog_key(state, key),
        BuilderOverlay::ImportDialog => return handle_import_dialog_key(state, key),
        BuilderOverlay::None => {}
    }

    if state.swatch_naming_active && state.tab == BuilderTab::Edit {
        return handle_swatch_naming_key(state, key);
    }

    let editing_hex = matches!(state.tab, BuilderTab::Edit | BuilderTab::Derived)
        && state.editor_focus == BuilderEditorFocus::Hex;

    match (key.code, key.modifiers) {
        (KeyCode::Esc, _) => return ThemeBuilderAction::Close,
        (KeyCode::Char('s'), modifiers) if modifiers.contains(KeyModifiers::CONTROL) => return ThemeBuilderAction::Save,
        (KeyCode::Char('a'), KeyModifiers::NONE) if !editing_hex => {
            state.overlay = BuilderOverlay::Apply;
            return ThemeBuilderAction::None;
        }
        (KeyCode::Char('p'), KeyModifiers::NONE) if !editing_hex => {
            open_gallery_overlay(state);
            return ThemeBuilderAction::None;
        }
        (KeyCode::Char('.'), KeyModifiers::NONE) if !editing_hex => {
            open_more_menu(state);
            return ThemeBuilderAction::None;
        }
        (KeyCode::Char('m'), KeyModifiers::NONE) if !editing_hex => {
            state.palette.dark = !state.palette.dark;
            state.gallery_dark = state.palette.dark;
            state.dirty = true;
            return ThemeBuilderAction::None;
        }
        (KeyCode::Char('D'), KeyModifiers::SHIFT) | (KeyCode::Char('D'), KeyModifiers::NONE) if !editing_hex => {
            state.depth_mode = next_builder_depth(state.depth_mode);
            state.sync_derived_hex_from_selected();
            state.status = Some(format!("Preview depth: {}", state.depth_mode.label()));
            return ThemeBuilderAction::None;
        }
        _ => {}
    }

    match state.tab {
        BuilderTab::Edit => handle_edit_tab_key(state, key),
        BuilderTab::Preview => handle_preview_tab_key(state, key),
        BuilderTab::Derived => handle_derived_tab_key(state, key),
    }
}

fn handle_edit_tab_key(state: &mut ThemeBuilderState, key: KeyEvent) -> ThemeBuilderAction {
    match (key.code, key.modifiers) {
        (KeyCode::Tab, _) => {
            state.editor_focus = state.editor_focus.next();
            return ThemeBuilderAction::None;
        }
        (KeyCode::BackTab, _) => {
            state.editor_focus = state.editor_focus.previous();
            return ThemeBuilderAction::None;
        }
        (KeyCode::Char('+'), KeyModifiers::NONE) if state.editor_focus != BuilderEditorFocus::Hex => {
            state.begin_swatch_naming();
            return ThemeBuilderAction::None;
        }
        _ => {}
    }

    match state.editor_focus {
        BuilderEditorFocus::Slots => match (key.code, key.modifiers) {
            (KeyCode::Enter | KeyCode::Char('y'), KeyModifiers::NONE) => {
                state.apply_selected_accent_to_last_role();
            }
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
    }

    ThemeBuilderAction::None
}

fn handle_preview_tab_key(state: &mut ThemeBuilderState, key: KeyEvent) -> ThemeBuilderAction {
    match (key.code, key.modifiers) {
        (KeyCode::Enter | KeyCode::Char('y'), KeyModifiers::NONE) => {
            state.apply_selected_accent_to_last_role();
        }
        (KeyCode::Up | KeyCode::Char('k'), KeyModifiers::NONE) => state.set_selected_slot(state.selected_slot.previous()),
        (KeyCode::Down | KeyCode::Char('j'), KeyModifiers::NONE) => state.set_selected_slot(state.selected_slot.next()),
        (KeyCode::Left | KeyCode::Char('h'), KeyModifiers::NONE) => state.set_selected_slot(state.selected_slot.previous()),
        (KeyCode::Right | KeyCode::Char('l'), KeyModifiers::NONE) => state.set_selected_slot(state.selected_slot.next()),
        _ => {}
    }
    ThemeBuilderAction::None
}

fn handle_derived_tab_key(state: &mut ThemeBuilderState, key: KeyEvent) -> ThemeBuilderAction {
    let locked = state.selected_derived_locked();
    match (key.code, key.modifiers) {
        (KeyCode::Tab, _) => {
            if locked {
                state.editor_focus = state.editor_focus.next();
            } else {
                state.editor_focus = BuilderEditorFocus::Slots;
            }
            return ThemeBuilderAction::None;
        }
        (KeyCode::BackTab, _) => {
            if locked {
                state.editor_focus = state.editor_focus.previous();
            } else {
                state.editor_focus = BuilderEditorFocus::Slots;
            }
            return ThemeBuilderAction::None;
        }
        (KeyCode::Char(' '), KeyModifiers::NONE) => {
            state.toggle_selected_derived_lock();
            return ThemeBuilderAction::None;
        }
        _ => {}
    }

    match state.editor_focus {
        BuilderEditorFocus::Slots => match (key.code, key.modifiers) {
            (KeyCode::Up | KeyCode::Char('k'), KeyModifiers::NONE) => move_derived_cursor(state, -1),
            (KeyCode::Down | KeyCode::Char('j'), KeyModifiers::NONE) => move_derived_cursor(state, 1),
            (KeyCode::PageUp, KeyModifiers::NONE) => page_derived_cursor(state, -1),
            (KeyCode::PageDown, KeyModifiers::NONE) => page_derived_cursor(state, 1),
            _ => {}
        },
        BuilderEditorFocus::Hex if locked => match key.code {
            KeyCode::Enter => {
                let _ = state.apply_hex_to_selected_derived();
            }
            _ => {
                if handle_text_input_key(&mut state.derived_hex_input, &key) {
                    let _ = state.apply_hex_to_selected_derived();
                }
            }
        },
        BuilderEditorFocus::Red if locked => handle_derived_slider_key(state, key, 0),
        BuilderEditorFocus::Green if locked => handle_derived_slider_key(state, key, 1),
        BuilderEditorFocus::Blue if locked => handle_derived_slider_key(state, key, 2),
        _ => state.editor_focus = BuilderEditorFocus::Slots,
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

fn handle_derived_slider_key(state: &mut ThemeBuilderState, key: KeyEvent, channel: usize) {
    let step = if key.modifiers.contains(KeyModifiers::SHIFT) { 16 } else { 1 };
    match key.code {
        KeyCode::Left | KeyCode::Char('h') => state.adjust_derived_rgb_channel(channel, -step),
        KeyCode::Right | KeyCode::Char('l') => state.adjust_derived_rgb_channel(channel, step),
        _ => {}
    }
}

fn handle_gallery_key(state: &mut ThemeBuilderState, key: KeyEvent) -> ThemeBuilderAction {
    if state.gallery_filter_active {
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => {
                state.gallery_filter_active = false;
                return ThemeBuilderAction::None;
            }
            (KeyCode::Enter, _) => {
                state.gallery_filter_active = false;
                return ThemeBuilderAction::None;
            }
            _ => {
                if handle_text_input_key(&mut state.gallery_filter_input, &key) {
                    clamp_gallery_cursor(state);
                }
                return ThemeBuilderAction::None;
            }
        }
    }

    let family_count = visible_gallery_families(state).len();
    match (key.code, key.modifiers) {
        (KeyCode::Esc, _) => {
            if state.preset_applies_on_select {
                return ThemeBuilderAction::Close;
            }
            state.overlay = BuilderOverlay::None;
        }
        (KeyCode::Char('/'), KeyModifiers::NONE) => {
            state.gallery_filter_active = true;
            state.gallery_filter_input.select_all_text();
        }
        (KeyCode::Char('m'), KeyModifiers::NONE) => {
            state.gallery_dark = !state.gallery_dark;
            clamp_gallery_cursor(state);
        }
        (KeyCode::Up | KeyCode::Char('k'), KeyModifiers::NONE) => move_gallery_cursor_vertical(state, -1),
        (KeyCode::Down | KeyCode::Char('j'), KeyModifiers::NONE) => move_gallery_cursor_vertical(state, 1),
        (KeyCode::Left | KeyCode::Char('h'), KeyModifiers::NONE) => move_gallery_cursor_horizontal(state, -1),
        (KeyCode::Right | KeyCode::Char('l'), KeyModifiers::NONE) => move_gallery_cursor_horizontal(state, 1),
        (KeyCode::PageUp, KeyModifiers::NONE) => {
            let page = gallery_visible_rows_for_state(state).max(1);
            state.preset_cursor = state.preset_cursor.saturating_sub(page);
            sync_gallery_scroll(state, page);
        }
        (KeyCode::PageDown, KeyModifiers::NONE) => {
            if family_count > 0 {
                let page = gallery_visible_rows_for_state(state).max(1);
                state.preset_cursor = (state.preset_cursor + page).min(family_count - 1);
                sync_gallery_scroll(state, page);
            }
        }
        (KeyCode::Enter, KeyModifiers::NONE) => {
            if let Some(choice) = selected_gallery_choice(state) {
                if state.preset_applies_on_select {
                    return ThemeBuilderAction::ApplyPreset(choice.slug);
                }
                state.load_gallery_choice(choice);
                state.overlay = BuilderOverlay::None;
            }
        }
        _ => {}
    }
    ThemeBuilderAction::None
}

fn handle_more_menu_key(state: &mut ThemeBuilderState, key: KeyEvent) -> ThemeBuilderAction {
    match (key.code, key.modifiers) {
        (KeyCode::Esc, _) => state.overlay = BuilderOverlay::None,
        (KeyCode::Up | KeyCode::Char('k'), KeyModifiers::NONE) => move_more_menu_cursor(state, -1),
        (KeyCode::Down | KeyCode::Char('j'), KeyModifiers::NONE) => move_more_menu_cursor(state, 1),
        (KeyCode::Enter, KeyModifiers::NONE) => execute_more_menu_cursor(state),
        (KeyCode::Char('r'), KeyModifiers::NONE) => execute_more_menu_direct(state, MoreMenuItem::Revert),
        (KeyCode::Char('d'), KeyModifiers::NONE) => execute_more_menu_direct(state, MoreMenuItem::Duplicate),
        (KeyCode::Char('x'), KeyModifiers::NONE) => execute_more_menu_direct(state, MoreMenuItem::Delete),
        (KeyCode::Char('e'), KeyModifiers::NONE) => execute_more_menu_direct(state, MoreMenuItem::Export),
        (KeyCode::Char('i'), KeyModifiers::NONE) => execute_more_menu_direct(state, MoreMenuItem::Import),
        _ => {}
    }
    ThemeBuilderAction::None
}

fn handle_apply_key(state: &mut ThemeBuilderState, key: KeyEvent) -> ThemeBuilderAction {
    match (key.code, key.modifiers) {
        (KeyCode::Esc, _) => {
            state.overlay = BuilderOverlay::None;
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

fn handle_swatch_naming_key(state: &mut ThemeBuilderState, key: KeyEvent) -> ThemeBuilderAction {
    match (key.code, key.modifiers) {
        (KeyCode::Esc, _) => state.cancel_swatch_naming(),
        (KeyCode::Enter, KeyModifiers::NONE) => state.save_current_swatch(),
        _ => {
            if handle_text_input_key(&mut state.swatch_name_input, &key) {
                state.status = None;
            }
        }
    }
    ThemeBuilderAction::None
}

fn handle_export_dialog_key(state: &mut ThemeBuilderState, key: KeyEvent) -> ThemeBuilderAction {
    match (key.code, key.modifiers) {
        (KeyCode::Esc, _) => {
            state.overlay = BuilderOverlay::None;
            state.status = Some("Export canceled".to_string());
        }
        (KeyCode::Enter, KeyModifiers::NONE) => state.export_current_theme(),
        _ => {
            if handle_text_input_key(&mut state.export_path_input, &key) {
                state.status = None;
            }
        }
    }
    ThemeBuilderAction::None
}

fn handle_import_dialog_key(state: &mut ThemeBuilderState, key: KeyEvent) -> ThemeBuilderAction {
    match (key.code, key.modifiers) {
        (KeyCode::Esc, _) => {
            state.overlay = BuilderOverlay::None;
            state.status = Some("Import canceled".to_string());
        }
        (KeyCode::Enter, KeyModifiers::NONE) => state.import_theme_from_dialog(),
        _ => {
            if handle_text_input_key(&mut state.import_path_input, &key) {
                state.status = None;
            }
        }
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
        MouseEventKind::ScrollUp if state.overlay == BuilderOverlay::Gallery => {
            move_gallery_cursor(state, -3);
            return ThemeBuilderAction::None;
        }
        MouseEventKind::ScrollDown if state.overlay == BuilderOverlay::Gallery => {
            move_gallery_cursor(state, 3);
            return ThemeBuilderAction::None;
        }
        MouseEventKind::ScrollUp if state.tab == BuilderTab::Derived && state.overlay == BuilderOverlay::None => {
            move_derived_cursor(state, -3);
            return ThemeBuilderAction::None;
        }
        MouseEventKind::ScrollDown if state.tab == BuilderTab::Derived && state.overlay == BuilderOverlay::None => {
            move_derived_cursor(state, 3);
            return ThemeBuilderAction::None;
        }
        MouseEventKind::Down(MouseButton::Left) => {}
        _ => return ThemeBuilderAction::None,
    }

    match state.overlay {
        BuilderOverlay::Gallery => return handle_gallery_mouse(state, hit),
        BuilderOverlay::MoreMenu => return handle_more_menu_mouse(state, hit),
        BuilderOverlay::Apply => return handle_apply_mouse(state, hit),
        BuilderOverlay::DeleteConfirm => return handle_delete_confirm_mouse(state, hit),
        BuilderOverlay::ExportDialog => return handle_export_dialog_mouse(state, hit),
        BuilderOverlay::ImportDialog => return handle_import_dialog_mouse(state, hit),
        BuilderOverlay::None => {}
    }

    match hit {
        Some(TuiButton::ThemeBuilderTab(index)) => {
            state.tab = BuilderTab::from_index(index);
            state.editor_focus = BuilderEditorFocus::Slots;
            if state.tab == BuilderTab::Derived {
                state.sync_derived_hex_from_selected();
            }
        }
        Some(TuiButton::ThemeBuilderPreset) => open_gallery_overlay(state),
        Some(TuiButton::ThemeBuilderMode) => {
            state.palette.dark = !state.palette.dark;
            state.gallery_dark = state.palette.dark;
            state.dirty = true;
        }
        Some(TuiButton::ThemeBuilderDepth(_)) => {
            state.depth_mode = next_builder_depth(state.depth_mode);
            state.sync_derived_hex_from_selected();
            state.status = Some(format!("Preview depth: {}", state.depth_mode.label()));
        }
        Some(TuiButton::ThemeBuilderSlot(slot)) if matches!(state.tab, BuilderTab::Edit | BuilderTab::Preview) => {
            if mouse.modifiers.is_empty() {
                state.activate_slot(slot);
            } else {
                // Modified clicks keep the lower palette grid available for
                // direct accent-slot editing without compromising the primary
                // swatch-click assignment workflow.
                state.select_slot_for_editing(slot);
            }
        }
        Some(TuiButton::ThemeBuilderHexField) if state.tab == BuilderTab::Edit => {
            state.editor_focus = BuilderEditorFocus::Hex;
        }
        Some(TuiButton::ThemeBuilderHexField) if state.tab == BuilderTab::Derived && state.selected_derived_locked() => {
            state.editor_focus = BuilderEditorFocus::Hex;
        }
        Some(TuiButton::ThemeBuilderRgbSlider(channel)) if state.tab == BuilderTab::Edit => {
            state.editor_focus = match channel { 0 => BuilderEditorFocus::Red, 1 => BuilderEditorFocus::Green, _ => BuilderEditorFocus::Blue };
        }
        Some(TuiButton::ThemeBuilderRgbSlider(channel)) if state.tab == BuilderTab::Derived && state.selected_derived_locked() => {
            state.editor_focus = match channel { 0 => BuilderEditorFocus::Red, 1 => BuilderEditorFocus::Green, _ => BuilderEditorFocus::Blue };
        }
        Some(TuiButton::ThemeBuilderInlineSwatchName) if state.tab == BuilderTab::Edit => {
            state.swatch_naming_active = true;
            state.swatch_name_input.select_all_text();
        }
        Some(TuiButton::ThemeBuilderSavedSwatch(index)) if state.tab == BuilderTab::Edit => {
            state.apply_saved_swatch(index);
        }
        Some(TuiButton::ThemeBuilderSaveSwatch) if state.tab == BuilderTab::Edit => {
            if state.swatch_naming_active {
                state.save_current_swatch();
            } else {
                state.begin_swatch_naming();
            }
        }
        Some(TuiButton::ThemeBuilderSave) => return ThemeBuilderAction::Save,
        Some(TuiButton::ThemeBuilderApply) => state.overlay = BuilderOverlay::Apply,
        Some(TuiButton::ThemeBuilderMoreMenu) => open_more_menu(state),
        Some(TuiButton::ThemeBuilderCancel) => return ThemeBuilderAction::Close,
        Some(TuiButton::ThemeBuilderDerivedRow(index)) if state.tab == BuilderTab::Derived => {
            if index < theme::derived_element_specs().len() {
                state.derived_cursor = index;
                sync_derived_scroll(state, derived_visible_rows_for_state(state));
                state.editor_focus = BuilderEditorFocus::Slots;
                state.sync_derived_hex_from_selected();
            }
        }
        Some(TuiButton::ThemeBuilderDerivedLock) if state.tab == BuilderTab::Derived => state.toggle_selected_derived_lock(),
        _ => {}
    }

    ThemeBuilderAction::None
}

fn handle_gallery_mouse(state: &mut ThemeBuilderState, hit: Option<TuiButton>) -> ThemeBuilderAction {
    match hit {
        Some(TuiButton::ThemeBuilderPresetRow(index)) => {
            state.preset_cursor = index.min(visible_gallery_families(state).len().saturating_sub(1));
            if let Some(choice) = selected_gallery_choice(state) {
                if state.preset_applies_on_select {
                    return ThemeBuilderAction::ApplyPreset(choice.slug);
                }
                state.load_gallery_choice(choice);
                state.overlay = BuilderOverlay::None;
            }
        }
        Some(TuiButton::ThemeBuilderGalleryMode) => {
            state.gallery_dark = !state.gallery_dark;
            clamp_gallery_cursor(state);
        }
        Some(TuiButton::ThemeBuilderGalleryFilter) => {
            state.gallery_filter_active = true;
            state.gallery_filter_input.select_all_text();
        }
        Some(TuiButton::ThemeBuilderPresetCancel) => {
            if state.preset_applies_on_select {
                return ThemeBuilderAction::Close;
            }
            state.overlay = BuilderOverlay::None;
        }
        _ => {}
    }
    ThemeBuilderAction::None
}

fn handle_more_menu_mouse(state: &mut ThemeBuilderState, hit: Option<TuiButton>) -> ThemeBuilderAction {
    if let Some(TuiButton::ThemeBuilderMoreMenuItem(index)) = hit {
        if index < state.more_menu.items.len() && state.more_menu.items[index].is_selectable() {
            state.more_menu.cursor = index;
            execute_more_menu_cursor(state);
        }
    }
    ThemeBuilderAction::None
}

fn handle_apply_mouse(state: &mut ThemeBuilderState, hit: Option<TuiButton>) -> ThemeBuilderAction {
    match hit {
        Some(TuiButton::ThemeBuilderApplyThemeLocks) => {
            state.apply_dialog.focus = ApplyDialogFocus::ThemeLocks;
            toggle_theme_lock_resolution(state);
        }
        Some(TuiButton::ThemeBuilderApplyUserOverrides) => {
            state.apply_dialog.focus = ApplyDialogFocus::UserOverrides;
            toggle_user_override_resolution(state);
        }
        Some(TuiButton::ThemeBuilderApplyConfirm) => {
            state.apply_dialog.focus = ApplyDialogFocus::Apply;
            return ThemeBuilderAction::Apply;
        }
        Some(TuiButton::ThemeBuilderApplyCancel) => state.overlay = BuilderOverlay::None,
        _ => {}
    }
    ThemeBuilderAction::None
}

fn handle_delete_confirm_mouse(state: &mut ThemeBuilderState, hit: Option<TuiButton>) -> ThemeBuilderAction {
    match hit {
        Some(TuiButton::ThemeBuilderDeleteConfirm) => state.confirm_delete_current_custom_theme(),
        Some(TuiButton::ThemeBuilderDeleteCancel) => state.cancel_delete_current_custom_theme(),
        _ => {}
    }
    ThemeBuilderAction::None
}

fn handle_export_dialog_mouse(state: &mut ThemeBuilderState, hit: Option<TuiButton>) -> ThemeBuilderAction {
    match hit {
        Some(TuiButton::ThemeBuilderFilePath) => state.export_path_input.select_all_text(),
        Some(TuiButton::ThemeBuilderFileConfirm) => state.export_current_theme(),
        Some(TuiButton::ThemeBuilderFileCancel) => {
            state.overlay = BuilderOverlay::None;
            state.status = Some("Export canceled".to_string());
        }
        _ => {}
    }
    ThemeBuilderAction::None
}

fn handle_import_dialog_mouse(state: &mut ThemeBuilderState, hit: Option<TuiButton>) -> ThemeBuilderAction {
    match hit {
        Some(TuiButton::ThemeBuilderFilePath) => state.import_path_input.select_all_text(),
        Some(TuiButton::ThemeBuilderFileConfirm) => state.import_theme_from_dialog(),
        Some(TuiButton::ThemeBuilderFileCancel) => {
            state.overlay = BuilderOverlay::None;
            state.status = Some("Import canceled".to_string());
        }
        _ => {}
    }
    ThemeBuilderAction::None
}

fn open_gallery_overlay(state: &mut ThemeBuilderState) {
    state.refresh_theme_library();
    if visible_gallery_families(state).is_empty() {
        state.status = Some("No themes available".to_string());
        return;
    }
    state.overlay = BuilderOverlay::Gallery;
    state.gallery_dark = state.palette.dark;
    clamp_gallery_cursor(state);
}

fn open_more_menu(state: &mut ThemeBuilderState) {
    let mut items = Vec::new();
    if matches!(state.palette.source, ThemeDraftSource::Custom) {
        items.push(MoreMenuItem::Revert);
    }
    items.push(MoreMenuItem::Duplicate);
    if matches!(state.palette.source, ThemeDraftSource::Custom) {
        items.push(MoreMenuItem::Delete);
    }
    items.push(MoreMenuItem::Separator);
    items.push(MoreMenuItem::Export);
    items.push(MoreMenuItem::Import);
    state.more_menu.items = items;
    state.more_menu.cursor = state.more_menu.items.iter().position(|item| item.is_selectable()).unwrap_or(0);
    state.overlay = BuilderOverlay::MoreMenu;
}

fn move_more_menu_cursor(state: &mut ThemeBuilderState, delta: isize) {
    if state.more_menu.items.is_empty() {
        state.more_menu.cursor = 0;
        return;
    }
    let len = state.more_menu.items.len();
    let mut cursor = state.more_menu.cursor;
    for _ in 0..len {
        cursor = move_cursor(cursor, len, delta);
        if state.more_menu.items[cursor].is_selectable() {
            state.more_menu.cursor = cursor;
            return;
        }
        if cursor == 0 || cursor == len - 1 {
            break;
        }
    }
}

fn execute_more_menu_direct(state: &mut ThemeBuilderState, item: MoreMenuItem) {
    if let Some(index) = state.more_menu.items.iter().position(|candidate| *candidate == item) {
        state.more_menu.cursor = index;
        execute_more_menu_cursor(state);
    }
}

fn execute_more_menu_cursor(state: &mut ThemeBuilderState) {
    let Some(item) = state.more_menu.items.get(state.more_menu.cursor).copied() else { return; };
    match item {
        MoreMenuItem::Revert => {
            state.overlay = BuilderOverlay::None;
            state.revert_from_disk();
        }
        MoreMenuItem::Duplicate => {
            state.overlay = BuilderOverlay::None;
            state.duplicate_current_theme();
        }
        MoreMenuItem::Delete => {
            state.overlay = BuilderOverlay::None;
            state.request_delete_current_custom_theme();
        }
        MoreMenuItem::Export => state.begin_export_dialog(),
        MoreMenuItem::Import => state.begin_import_dialog(),
        MoreMenuItem::Separator => {}
    }
}

fn move_derived_cursor(state: &mut ThemeBuilderState, delta: isize) {
    let specs_len = theme::derived_element_specs().len();
    if specs_len == 0 {
        return;
    }
    let current = state.derived_cursor as isize;
    let max = specs_len.saturating_sub(1) as isize;
    state.derived_cursor = (current + delta).clamp(0, max) as usize;
    sync_derived_scroll(state, derived_visible_rows_for_state(state));
    state.sync_derived_hex_from_selected();
}

fn page_derived_cursor(state: &mut ThemeBuilderState, direction: isize) {
    let step = derived_visible_rows_for_state(state).max(1) as isize;
    move_derived_cursor(state, direction.saturating_mul(step));
}

fn sync_derived_scroll(state: &mut ThemeBuilderState, visible_rows: usize) {
    let rows = derived_list_rows();
    let visible_rows = visible_rows.max(1);
    let selected_row = rows.iter()
        .position(|row| row.spec_index == Some(state.derived_cursor))
        .unwrap_or(0);
    let max_scroll = rows.len().saturating_sub(visible_rows);
    state.derived_scroll = state.derived_scroll.min(max_scroll);
    if selected_row < state.derived_scroll {
        state.derived_scroll = selected_row;
    } else if selected_row >= state.derived_scroll + visible_rows {
        state.derived_scroll = selected_row + 1 - visible_rows;
    }
    state.derived_scroll = state.derived_scroll.min(max_scroll);
}

pub fn draw_theme_builder(
    f: &mut Frame,
    state: &ThemeBuilderState,
    button_map: &mut ButtonRenderMap,
    theme: theme::Theme,
) {
    if state.overlay == BuilderOverlay::Gallery && state.preset_applies_on_select {
        draw_gallery_overlay(f, state, button_map, theme);
        return;
    }

    draw_two_pane_builder(f, state, button_map, theme);
    match state.overlay {
        BuilderOverlay::None => {}
        BuilderOverlay::Gallery => draw_gallery_overlay(f, state, button_map, theme),
        BuilderOverlay::MoreMenu => draw_more_menu(f, state, button_map, theme),
        BuilderOverlay::Apply => draw_apply_dialog(f, state, button_map, theme),
        BuilderOverlay::DeleteConfirm => draw_delete_confirm_dialog(f, state, button_map, theme),
        BuilderOverlay::ExportDialog => draw_theme_file_dialog(f, state, button_map, theme, true),
        BuilderOverlay::ImportDialog => draw_theme_file_dialog(f, state, button_map, theme, false),
    }
}

fn draw_two_pane_builder(f: &mut Frame, state: &ThemeBuilderState, button_map: &mut ButtonRenderMap, theme: theme::Theme) {
    let area = scaled_centered_rect(92, 28, f.size());
    f.render_widget(Clear, area);
    let block = Block::default()
        .title(Span::styled(" Theme Builder ", Style::default().fg(theme.title).add_modifier(Modifier::BOLD)))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(21), Constraint::Length(2)])
        .split(inner);

    draw_builder_header(f, chunks[0], state, button_map, theme);

    let left_width = proportional_width(chunks[1].width.saturating_sub(1), 34, 29, 44);
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(left_width), Constraint::Length(1), Constraint::Min(48)])
        .split(chunks[1]);

    match state.tab {
        BuilderTab::Edit | BuilderTab::Preview => draw_slot_list(f, body[0], state, button_map, theme),
        BuilderTab::Derived => draw_derived_list(f, body[0], state, button_map, theme),
    }
    draw_right_pane(f, body[2], state, button_map, theme);
    draw_builder_footer(f, chunks[2], state, button_map, theme);
}

fn draw_builder_header(f: &mut Frame, area: Rect, state: &ThemeBuilderState, button_map: &mut ButtonRenderMap, theme: theme::Theme) {
    let title_width = area.width.saturating_sub(48).clamp(12, 32) as usize;
    let name = truncate_chars(&state.palette.name, title_width);
    let mode_label = format!("Mode {} {}", if state.palette.dark { LOCK_MARK } else { AUTO_MARK }, state.palette.mode_label());
    let depth_label = format!("Depth {}", state.depth_mode.label());
    let preset_label = "p presets";
    let line = Line::from(vec![
        Span::styled(format!("{name:<title_width$}"), Style::default().fg(theme.text_bright).add_modifier(Modifier::BOLD)),
        Span::raw("  "),
        Span::styled(mode_label.clone(), Style::default().fg(theme.warning)),
        Span::raw("  "),
        Span::styled(depth_label.clone(), Style::default().fg(theme.info)),
        Span::raw("  "),
        Span::styled(preset_label, Style::default().fg(theme.text_dim)),
    ]);
    f.render_widget(Paragraph::new(vec![line, Line::raw("")]), area);

    let row_end = area.x.saturating_add(area.width);
    let mode_x = area.x.saturating_add(title_width as u16).saturating_add(2);
    let mode_width = mode_label.chars().count().min(u16::MAX as usize) as u16;
    let depth_x = mode_x.saturating_add(mode_width).saturating_add(2);
    let depth_width = depth_label.chars().count().min(u16::MAX as usize) as u16;
    let preset_x = depth_x.saturating_add(depth_width).saturating_add(2);
    let preset_width = preset_label.chars().count().min(u16::MAX as usize) as u16;

    record_clipped_rect(button_map, TuiButton::ThemeBuilderMode, mode_x, area.y, mode_width, 1, row_end);
    record_clipped_rect(button_map, TuiButton::ThemeBuilderDepth(state.depth_mode), depth_x, area.y, depth_width, 1, row_end);
    record_clipped_rect(button_map, TuiButton::ThemeBuilderPreset, preset_x, area.y, preset_width, 1, row_end);
}

fn draw_right_pane(f: &mut Frame, area: Rect, state: &ThemeBuilderState, button_map: &mut ButtonRenderMap, theme: theme::Theme) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(area);
    draw_tab_strip(f, chunks[0], state, button_map, theme);
    match state.tab {
        BuilderTab::Edit => draw_edit_card(f, chunks[1], state, button_map, theme),
        BuilderTab::Preview => draw_preview_card(f, chunks[1], state, theme),
        BuilderTab::Derived => draw_derived_card(f, chunks[1], state, button_map, theme),
    }
}

fn draw_tab_strip(f: &mut Frame, area: Rect, state: &ThemeBuilderState, button_map: &mut ButtonRenderMap, theme: theme::Theme) {
    let tabs = [BuilderTab::Edit, BuilderTab::Preview, BuilderTab::Derived];
    let labels: Vec<String> = tabs.iter().map(|tab| format!(" {} ", tab.label())).collect();
    let total_width: u16 = labels.iter()
        .map(|label| label.chars().count() as u16)
        .sum::<u16>()
        .saturating_add(tabs.len().saturating_sub(1) as u16);
    let mut x = area.x.saturating_add(area.width.saturating_sub(total_width));
    let mut spans = Vec::new();

    for (index, (tab, label)) in tabs.iter().copied().zip(labels.into_iter()).enumerate() {
        if index > 0 {
            spans.push(Span::raw(" "));
            x = x.saturating_add(1);
        }
        let active = state.tab == tab;
        let style = if active {
            Style::default().fg(theme.pill_active_fg).bg(theme.tab_active).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.text).bg(theme.dropdown_bg)
        };
        let width = label.chars().count() as u16;
        let visible_width = area.x.saturating_add(area.width).saturating_sub(x).min(width);
        record_rect(button_map, TuiButton::ThemeBuilderTab(tab.index()), x, area.y, visible_width, 1);
        spans.push(Span::styled(label, style));
        x = x.saturating_add(width);
    }
    f.render_widget(Paragraph::new(Line::from(spans)).alignment(Alignment::Right), area);
}

fn draw_builder_footer(f: &mut Frame, area: Rect, state: &ThemeBuilderState, button_map: &mut ButtonRenderMap, theme: theme::Theme) {
    let mut spans = vec![
        chip("^s Save", theme.chip_go, theme), Span::raw(" "),
        chip("a Apply", theme.tab_active, theme), Span::raw(" "),
    ];
    let mut chips = vec![
        (TuiButton::ThemeBuilderSave, "^s Save"),
        (TuiButton::ThemeBuilderApply, "a Apply"),
    ];
    if state.tab == BuilderTab::Derived {
        spans.push(chip("space Lock/Auto", theme.warning, theme));
        spans.push(Span::raw(" "));
        chips.push((TuiButton::ThemeBuilderDerivedLock, "space Lock/Auto"));
    } else {
        spans.push(chip(ELLIPSIS_MORE, theme.dropdown_bg, theme));
        spans.push(Span::raw(" "));
        chips.push((TuiButton::ThemeBuilderMoreMenu, ELLIPSIS_MORE));
    }
    spans.push(chip("Esc Cancel", theme.chip_dismiss, theme));
    spans.push(status_span(state, theme));
    chips.push((TuiButton::ThemeBuilderCancel, "Esc Cancel"));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
    record_footer_chips(button_map, area.x, area.y, &chips);
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
            Span::styled(BLOCK, Style::default().fg(color).bg(color)),
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
            let label = if selected { format!("{:02}", idx) } else { BLOCK.to_string() };
            spans.push(Span::styled(label, Style::default().fg(color).bg(if selected { theme.selection_bg } else { color })));
            spans.push(Span::raw("  "));
        }
        lines.push(Line::from(spans));
    }
    lines.push(Line::from(Span::styled(" 0-11 hue · 12-15 special", Style::default().fg(theme.text_dim))));
    f.render_widget(Paragraph::new(lines), area);
}

fn draw_edit_card(f: &mut Frame, area: Rect, state: &ThemeBuilderState, button_map: &mut ButtonRenderMap, theme: theme::Theme) {
    let block = Block::default()
        .title(Span::styled(format!(" {} ", state.selected_slot.label()), Style::default().fg(theme.header).add_modifier(Modifier::BOLD)))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let color = state.selected_color();
    let (r, g, b) = theme::rgb_tuple(color);
    let (xidx, xcolor) = theme::nearest_xterm_256(color);
    let lines = vec![
        Line::raw(""),
        swatch_summary_line(color, Some((r, g, b)), theme),
        Line::raw(""),
        hex_line("Hex", &state.hex_input.text, state.editor_focus == BuilderEditorFocus::Hex, true, theme),
        slider_line("R", state.rgb_values[0], 0, state.editor_focus == BuilderEditorFocus::Red, true, theme),
        slider_line("G", state.rgb_values[1], 1, state.editor_focus == BuilderEditorFocus::Green, true, theme),
        slider_line("B", state.rgb_values[2], 2, state.editor_focus == BuilderEditorFocus::Blue, true, theme),
        Line::raw(""),
        Line::from(vec![
            Span::styled("  Depth ", Style::default().fg(theme.label)),
            Span::styled(state.depth_mode.label(), Style::default().fg(theme.info)),
            Span::styled(format!(" · 256{} ", ARROW), Style::default().fg(theme.text_dim)),
            Span::styled(BLOCK, Style::default().fg(xcolor).bg(xcolor)),
            Span::raw(" "),
            Span::styled(theme::color_to_hex(xcolor), Style::default().fg(theme.text_bright)),
            Span::raw(" "),
            Span::styled(xidx.to_string(), Style::default().fg(theme.info)),
        ]),
        Line::raw(""),
        swatches_inline_row(state, theme),
    ];
    f.render_widget(Paragraph::new(lines), inner);
    record_editor_buttons(button_map, inner, state, false);
}

fn draw_preview_card(f: &mut Frame, area: Rect, state: &ThemeBuilderState, theme: theme::Theme) {
    let block = Block::default()
        .title(Span::styled(" Live preview ", Style::default().fg(theme.header).add_modifier(Modifier::BOLD)))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let lines = preview_lines(state, theme, inner.height as usize);
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_derived_card(f: &mut Frame, area: Rect, state: &ThemeBuilderState, button_map: &mut ButtonRenderMap, theme: theme::Theme) {
    let specs = theme::derived_element_specs();
    if specs.is_empty() {
        return;
    }
    let spec = &specs[state.derived_cursor.min(specs.len() - 1)];
    let locked = state.selected_derived_locked();
    let auto_color = state.selected_derived_auto_color();
    let color = state.selected_derived_display_color();
    let (r, g, b) = theme::rgb_tuple(color);
    let block = Block::default()
        .title(Span::styled(format!(" {} ", spec.key), Style::default().fg(theme.header).add_modifier(Modifier::BOLD)))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let toggle = if locked {
        Line::from(vec![
            Span::styled(format!("  {AUTO_MARK} Auto  "), Style::default().fg(theme.text_dim)),
            Span::styled(format!("{LOCK_MARK} Locked"), Style::default().fg(theme.warning).add_modifier(Modifier::BOLD)),
            Span::styled("   space releases to auto", Style::default().fg(theme.text_dim)),
        ])
    } else {
        Line::from(vec![
            Span::styled(format!("  {LOCK_MARK} Auto  "), Style::default().fg(theme.text_dim).add_modifier(Modifier::BOLD)),
            Span::styled(format!("{AUTO_MARK} Lock"), Style::default().fg(theme.warning)),
            Span::styled("   space to lock & edit", Style::default().fg(theme.text_dim)),
        ])
    };
    let lines = vec![
        Line::from(vec![
            Span::styled("  from = ", Style::default().fg(theme.label)),
            Span::styled(spec.formula, Style::default().fg(theme.text)),
            Span::raw("  "),
            Span::styled(BLOCK, Style::default().fg(auto_color).bg(auto_color)),
            Span::raw(" "),
            Span::styled(theme::color_to_hex(auto_color), Style::default().fg(theme.text_bright)),
        ]),
        toggle,
        Line::raw(""),
        swatch_summary_line(color, None, theme),
        Line::raw(""),
        hex_line("Hex", &state.derived_hex_input.text, state.editor_focus == BuilderEditorFocus::Hex, locked, theme),
        slider_line("R", r, 0, state.editor_focus == BuilderEditorFocus::Red, locked, theme),
        slider_line("G", g, 1, state.editor_focus == BuilderEditorFocus::Green, locked, theme),
        slider_line("B", b, 2, state.editor_focus == BuilderEditorFocus::Blue, locked, theme),
        Line::raw(""),
        if locked {
            Line::from(Span::styled("  Pinned - ignores source accent edits.", Style::default().fg(theme.text_dim)))
        } else {
            Line::from(Span::styled("  Computed - tracks its source colors.", Style::default().fg(theme.text_dim)))
        },
        Line::from(vec![Span::styled("  used by ", Style::default().fg(theme.label)), Span::styled(spec.used_by, Style::default().fg(theme.text))]),
    ];
    f.render_widget(Paragraph::new(lines), inner);
    record_editor_buttons(button_map, inner, state, true);
    record_rect(button_map, TuiButton::ThemeBuilderDerivedLock, inner.x, inner.y.saturating_add(1), inner.width.min(36), 1);
}

fn swatch_summary_line(color: Color, rgb: Option<(u8, u8, u8)>, theme: theme::Theme) -> Line<'static> {
    let mut spans = vec![Span::raw("  ")];
    spans.extend(framed_swatch(color, theme));
    spans.push(Span::raw("  "));
    spans.push(Span::styled(theme::color_to_hex(color), Style::default().fg(theme.text_bright).add_modifier(Modifier::BOLD)));
    if let Some((r, g, b)) = rgb {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(format!("rgb({r},{g},{b})"), Style::default().fg(theme.text_dim)));
    }
    Line::from(spans)
}

fn framed_swatch(color: Color, theme: theme::Theme) -> Vec<Span<'static>> {
    vec![
        Span::styled(SWATCH_LEFT, Style::default().fg(theme.border)),
        Span::styled("\u{2588}".repeat(14), Style::default().fg(color).bg(color)),
        Span::styled(SWATCH_RIGHT, Style::default().fg(theme.border)),
    ]
}

fn hex_line(label: &str, text: &str, focused: bool, editable: bool, theme: theme::Theme) -> Line<'static> {
    let style = if editable { input_style(focused, theme) } else { Style::default().fg(theme.text_dim).bg(theme.input_disabled_bg) };
    Line::from(vec![
        focus_mark(focused && editable, theme),
        Span::styled(format!("{label:<4}"), Style::default().fg(theme.label)),
        Span::styled(format!("[ {:<7} ]", text), style),
    ])
}

fn slider_line(label: &str, value: u8, channel: usize, focused: bool, editable: bool, theme: theme::Theme) -> Line<'static> {
    let filled = ((usize::from(value) * 18) / 255).min(18);
    let empty = 18usize.saturating_sub(filled);
    let channel_color = if editable {
        match channel {
            0 => Color::Rgb(value, 0, 0),
            1 => Color::Rgb(0, value, 0),
            _ => Color::Rgb(0, 0, value),
        }
    } else {
        theme.text_dim
    };
    Line::from(vec![
        focus_mark(focused && editable, theme),
        Span::styled(format!("{label} ["), Style::default().fg(theme.label)),
        Span::styled(BAR_FILLED.repeat(filled), Style::default().fg(channel_color)),
        Span::styled(BAR_EMPTY.repeat(empty), Style::default().fg(theme.border_dim)),
        Span::styled(format!("] {value:>3}"), Style::default().fg(if editable { theme.text } else { theme.text_dim })),
    ])
}

fn swatches_inline_row(state: &ThemeBuilderState, theme: theme::Theme) -> Line<'static> {
    let mut spans = vec![Span::styled("  Swatches ", Style::default().fg(theme.label))];
    if state.swatch_naming_active {
        spans.push(Span::styled("Name ", Style::default().fg(theme.text_dim)));
        spans.push(Span::styled(
            format!("[ {:<18} ]", truncate_chars(&state.swatch_name_input.text, 18)),
            input_style(true, theme),
        ));
        spans.push(Span::styled(" Enter save · Esc cancel", Style::default().fg(theme.text_dim)));
        return Line::from(spans);
    }

    if state.palette.swatches.is_empty() {
        spans.push(Span::styled("(none · + to save)", Style::default().fg(theme.text_dim)));
    } else {
        for (index, swatch) in state.palette.swatches.iter().take(12).enumerate() {
            let selected = index == state.saved_swatch_cursor.min(state.palette.swatches.len().saturating_sub(1));
            let style = if selected {
                Style::default().fg(swatch.color).bg(theme.selection_bg).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(swatch.color).bg(swatch.color)
            };
            spans.push(Span::styled(if selected { "[]" } else { BLOCK }, style));
            spans.push(Span::raw(" "));
        }
        spans.push(Span::styled(" + to save", Style::default().fg(theme.text_dim)));
    }
    Line::from(spans)
}

fn preview_lines(state: &ThemeBuilderState, _theme: theme::Theme, max_rows: usize) -> Vec<Line<'static>> {
    if max_rows == 0 {
        return Vec::new();
    }
    let preview = state.resolved_theme();
    let color = state.selected_color();
    let (r, g, b) = theme::rgb_tuple(color);
    let mut lines = vec![
        Line::raw(""),
        Line::from(vec![
            Span::styled("  Metadata ", Style::default().fg(preview.pill_active_fg).bg(preview.tab_active).add_modifier(Modifier::BOLD)),
            Span::styled("  Artwork  ", Style::default().fg(preview.tab_inactive)),
            Span::styled("  ReplayGain", Style::default().fg(preview.tab_inactive)),
        ]),
        Line::raw(""),
        Line::from(Span::styled("  General", Style::default().fg(preview.header).add_modifier(Modifier::BOLD))),
        Line::from(vec![Span::styled("   Sample rate   ", Style::default().fg(preview.label)), Span::styled("44100 Hz", Style::default().fg(preview.value))]),
        Line::from(vec![
            Span::styled("  \u{25B8} resampler.rs", Style::default().fg(preview.value).bg(preview.selection_bg)),
            Span::styled("   61 KB   Rust source", Style::default().fg(preview.text_dim).bg(preview.selection_bg)),
        ]),
        Line::raw(""),
        Line::from(vec![
            Span::styled("  progress ", Style::default().fg(preview.label)),
            Span::styled("\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}", Style::default().fg(preview.progress_dialog_bar_filled)),
            Span::styled("\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}", Style::default().fg(preview.progress_dialog_bar_unfilled)),
            Span::styled("  62%  ", Style::default().fg(preview.progress_dialog_percent)),
            Span::styled(" OK ", Style::default().fg(preview.progress_dialog_button_fg).bg(preview.progress_dialog_button_bg)),
            Span::raw(" "),
            Span::styled(" Esc ", Style::default().fg(preview.progress_dialog_abort_fg).bg(preview.progress_dialog_abort_bg)),
        ]),
        Line::from(vec![
            Span::styled("  selected ", Style::default().fg(preview.label)),
            Span::styled(state.selected_slot.label(), Style::default().fg(preview.value).add_modifier(Modifier::BOLD)),
            Span::raw("  "),
            Span::styled(BLOCK, Style::default().fg(color).bg(color)),
            Span::raw(" "),
            Span::styled(format!("{} rgb({r},{g},{b})", theme::color_to_hex(color)), Style::default().fg(preview.value)),
        ]),
        Line::from(vec![
            Span::styled("  derived ", Style::default().fg(preview.text_dim)),
            Span::styled(BLOCK, Style::default().fg(preview.surface).bg(preview.surface)), Span::raw(" "),
            Span::styled(BLOCK, Style::default().fg(preview.progress_dialog_border).bg(preview.progress_dialog_border)), Span::raw(" "),
            Span::styled("auto (computed)", Style::default().fg(preview.text_dim)),
        ]),
        Line::from(vec![
            Span::styled("  states  ", Style::default().fg(preview.label)),
            Span::styled("warning", Style::default().fg(preview.warning)),
            Span::raw(" · "),
            Span::styled("success", Style::default().fg(preview.success)),
            Span::raw(" · "),
            Span::styled("error", Style::default().fg(preview.error)),
        ]),
    ];
    lines.truncate(max_rows);
    lines
}

fn draw_derived_list(f: &mut Frame, area: Rect, state: &ThemeBuilderState, button_map: &mut ButtonRenderMap, theme: theme::Theme) {
    let rows = derived_list_rows();
    let visible = area.height.saturating_sub(1).max(1) as usize;
    state.derived_visible_rows.set(visible);
    let max_scroll = rows.len().saturating_sub(visible);
    let start = state.derived_scroll.min(max_scroll);
    let auto_theme = theme::preview_resolve_theme_draft_for_depth(
        &state.palette,
        ThemeApplyOptions { honor_theme_locks: false, keep_user_overrides: false },
        &ThemeOverrides::default(),
        state.depth_mode,
    );
    let mut lines = Vec::new();
    for (row_offset, row) in rows.iter().enumerate().skip(start).take(visible) {
        let y = area.y.saturating_add((row_offset - start) as u16);
        if let Some(index) = row.spec_index {
            let spec = &theme::derived_element_specs()[index];
            record_rect(button_map, TuiButton::ThemeBuilderDerivedRow(index), area.x, y, area.width, 1);
            let selected = index == state.derived_cursor;
            let locked = state.palette.derived_locks.contains_key(spec.key);
            let source = if locked { (LOCK_MARK, theme.warning) } else { (AUTO_MARK, theme.text_dim) };
            let color = state.palette.derived_locks.get(spec.key)
                .copied()
                .or_else(|| theme::theme_color_by_derived_key(auto_theme, spec.key))
                .unwrap_or(theme.text_dim);
            let style = if selected { Style::default().fg(theme.text_bright).bg(theme.selection_bg).add_modifier(Modifier::BOLD) } else { Style::default().fg(theme.text) };
            let key_width = area.width.saturating_sub(8).clamp(18, 34) as usize;
            lines.push(Line::from(vec![
                Span::styled(format!("{} ", source.0), Style::default().fg(source.1)),
                Span::styled(format!("{:<key_width$} ", spec.key), style),
                Span::styled(BLOCK, Style::default().fg(color).bg(color)),
            ]));
        } else {
            lines.push(Line::from(Span::styled(format!("  -- {} --", row.group), Style::default().fg(theme.text_dim))));
        }
    }
    let specs_len = theme::derived_element_specs().len();
    lines.push(Line::from(vec![
        Span::styled(format!("{}/{}  ", state.derived_cursor + 1, specs_len), Style::default().fg(theme.text_dim)),
        Span::styled(format!("{AUTO_MARK} auto  "), Style::default().fg(theme.text_dim)),
        Span::styled(format!("{LOCK_MARK} locked"), Style::default().fg(theme.warning)),
    ]));
    f.render_widget(Paragraph::new(lines), area);
}

fn draw_gallery_overlay(f: &mut Frame, state: &ThemeBuilderState, button_map: &mut ButtonRenderMap, theme: theme::Theme) {
    let families = visible_gallery_families(state);
    let area = scaled_centered_rect(78, 22, f.size());
    f.render_widget(Clear, area);
    let block = Block::default()
        .title(Span::styled(" Themes ", Style::default().fg(theme.title).add_modifier(Modifier::BOLD)))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(1), Constraint::Length(2)])
        .split(inner);
    let built_in_count = state.theme_library.iter().filter(|choice| choice.built_in).count();
    let custom_count = state.theme_library.len().saturating_sub(built_in_count);
    let header = Line::from(vec![
        Span::styled(format!("Mode {} Dark  {} Light", if state.gallery_dark { LOCK_MARK } else { AUTO_MARK }, if state.gallery_dark { AUTO_MARK } else { LOCK_MARK }), Style::default().fg(theme.warning)),
        Span::raw("      "),
        Span::styled(format!("{built_in_count} built-in · {custom_count} custom"), Style::default().fg(theme.text_dim)),
        Span::raw("      "),
        Span::styled(format!("/ {}", state.gallery_filter_input.text), if state.gallery_filter_active { input_style(true, theme) } else { Style::default().fg(theme.text_dim) }),
    ]);
    f.render_widget(Paragraph::new(vec![header, Line::raw("")]), chunks[0]);
    record_rect(button_map, TuiButton::ThemeBuilderGalleryMode, chunks[0].x, chunks[0].y, 22, 1);
    record_rect(button_map, TuiButton::ThemeBuilderGalleryFilter, chunks[0].x.saturating_add(chunks[0].width.saturating_sub(20)), chunks[0].y, 20, 1);

    let columns = gallery_columns_for_width(chunks[1].width);
    state.preset_visible_columns.set(columns);
    let rows_visible = (chunks[1].height as usize).max(1);
    let cells_visible = rows_visible.saturating_mul(columns).max(1);
    state.preset_visible_rows.set(cells_visible);
    let max_scroll = families.len().saturating_sub(cells_visible);
    let start = state.preset_scroll.min(max_scroll);
    let end = (start + cells_visible).min(families.len());
    let row_width = chunks[1].width / columns as u16;
    for (absolute, family) in families.iter().enumerate().take(end).skip(start) {
        let local = absolute - start;
        let row = local / columns;
        let col = local % columns;
        let x = chunks[1].x.saturating_add(col as u16 * row_width);
        let y = chunks[1].y.saturating_add(row as u16);
        let width = if col + 1 == columns { chunks[1].x + chunks[1].width - x } else { row_width };
        let choice = display_choice_for_family(family, state.gallery_dark);
        let selected = absolute == state.preset_cursor.min(families.len().saturating_sub(1));
        let active = active_slug_matches(&state.palette.slug, &choice.slug);
        let style = if selected { Style::default().fg(theme.text_bright).bg(theme.selection_bg).add_modifier(Modifier::BOLD) } else { Style::default().fg(theme.text) };
        let name_width = width.saturating_sub(30).clamp(12, 28) as usize;
        let mut spans = vec![
            Span::styled(if active { "\u{25B8} " } else if selected { "> " } else { "  " }, style),
            Span::styled(format!("{:<name_width$}", truncate_chars(&family.name, name_width)), style),
        ];
        if !choice.built_in {
            spans.push(Span::styled(" custom ", Style::default().fg(theme.panel_bg).bg(theme.warning)));
        } else {
            spans.push(Span::raw(" "));
        }
        for color in choice.accents.iter().take(10).copied() {
            spans.push(Span::styled(BLOCK, Style::default().fg(color).bg(color)));
        }
        f.render_widget(Paragraph::new(Line::from(spans)), Rect::new(x, y, width, 1));
        record_rect(button_map, TuiButton::ThemeBuilderPresetRow(absolute), x, y, width, 1);
    }
    let footer = Line::from(vec![
        Span::styled("\u{2191}\u{2193}\u{2190}\u{2192} move", Style::default().fg(theme.text_dim)),
        Span::raw("   "),
        Span::styled("Enter apply", Style::default().fg(theme.text_dim)),
        Span::raw("   "),
        Span::styled("/ filter", Style::default().fg(theme.text_dim)),
        Span::raw("   "),
        Span::styled("Esc close", Style::default().fg(theme.chip_dismiss)),
    ]);
    f.render_widget(Paragraph::new(footer), chunks[2]);
    record_rect(button_map, TuiButton::ThemeBuilderPresetCancel, chunks[2].x.saturating_add(chunks[2].width.saturating_sub(12)), chunks[2].y, 12, 1);
}

fn draw_more_menu(f: &mut Frame, state: &ThemeBuilderState, button_map: &mut ButtonRenderMap, theme: theme::Theme) {
    let width = 26;
    let height = state.more_menu.items.len().saturating_add(2).max(4) as u16;
    let screen = f.size();
    let anchor = button_map.find_button_rect(&TuiButton::ThemeBuilderMoreMenu);
    let max_x = screen.x.saturating_add(screen.width.saturating_sub(width));
    let x = anchor.map(|rect| rect.x).unwrap_or_else(|| screen.x.saturating_add(4)).min(max_x);
    let y = anchor
        .map(|rect| rect.y.saturating_sub(height))
        .unwrap_or_else(|| screen.y.saturating_add(screen.height.saturating_sub(height + 3)));
    let area = Rect::new(x, y, width, height);
    f.render_widget(Clear, area);
    let block = Block::default()
        .title(Span::styled(" More ", Style::default().fg(theme.title).add_modifier(Modifier::BOLD)))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let mut lines = Vec::new();
    for (index, item) in state.more_menu.items.iter().copied().enumerate() {
        let selected = index == state.more_menu.cursor && item.is_selectable();
        if item == MoreMenuItem::Separator {
            lines.push(Line::from(Span::styled("----------------------", Style::default().fg(theme.border_dim))));
            continue;
        }
        let style = if selected { Style::default().fg(theme.text_bright).bg(theme.selection_bg).add_modifier(Modifier::BOLD) } else { Style::default().fg(theme.text) };
        lines.push(Line::from(Span::styled(item.label(), style)));
        record_rect(button_map, TuiButton::ThemeBuilderMoreMenuItem(index), inner.x, inner.y.saturating_add(index as u16), inner.width, 1);
    }
    f.render_widget(Paragraph::new(lines), inner);
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
    let lines = vec![
        Line::from(vec![Span::styled(&state.palette.name, Style::default().fg(theme.text_bright).add_modifier(Modifier::BOLD)), Span::raw(" · "), Span::styled(state.palette.mode_label(), Style::default().fg(theme.warning)), Span::raw(" · "), Span::styled(state.depth_mode.label(), Style::default().fg(theme.info))]),
        Line::from(Span::styled(format!("Ships {theme_locks} locked colors · You have {user_locks} personal overrides"), Style::default().fg(theme.text_dim))),
        Line::raw(""),
        switch_line("Theme locked colors", "Honor the theme", "Re-derive for my terminal", state.apply_dialog.honor_theme_locks, state.palette.derived_locks.is_empty(), state.apply_dialog.focus == ApplyDialogFocus::ThemeLocks, theme),
        Line::from(Span::styled(format!("  Re-derive recomputes from formulas at {} depth.", state.depth_mode.label()), Style::default().fg(theme.text_dim))),
        switch_line("Your overrides", "Keep mine", "Use theme as authored", state.apply_dialog.keep_user_overrides, state.user_overrides.is_empty(), state.apply_dialog.focus == ApplyDialogFocus::UserOverrides, theme),
        Line::from(Span::styled("  Your layer sits above the theme's locks.", Style::default().fg(theme.text_dim))),
        Line::raw(""),
        Line::from(vec![
            Span::styled(format!("{LOCK_MARK} {} by theme", tally.by_theme), Style::default().fg(theme.warning)),
            Span::raw("  "),
            Span::styled(format!("{LOCK_MARK} {} by you", tally.by_user), Style::default().fg(theme.info)),
            Span::raw("  "),
            Span::styled(format!("{AUTO_MARK} {} auto", tally.auto), Style::default().fg(theme.text_dim)),
        ]),
        Line::raw(""),
        Line::from(vec![chip("a Apply", theme.progress_dialog_button_bg, theme), Span::raw(" "), chip("Esc Cancel", theme.chip_dismiss, theme)]),
    ];
    f.render_widget(Paragraph::new(lines).alignment(Alignment::Left), inner);
    record_rect(button_map, TuiButton::ThemeBuilderApplyThemeLocks, inner.x, inner.y.saturating_add(3), inner.width, 1);
    record_rect(button_map, TuiButton::ThemeBuilderApplyUserOverrides, inner.x, inner.y.saturating_add(5), inner.width, 1);
    record_footer_chips(button_map, inner.x, inner.y.saturating_add(10), &[
        (TuiButton::ThemeBuilderApplyConfirm, "a Apply"),
        (TuiButton::ThemeBuilderApplyCancel, "Esc Cancel"),
    ]);
}

fn draw_delete_confirm_dialog(f: &mut Frame, state: &ThemeBuilderState, button_map: &mut ButtonRenderMap, theme: theme::Theme) {
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
        Line::from(Span::styled(format!("This will remove the saved custom theme file for slug '{}'.", state.palette.slug), Style::default().fg(theme.text_dim))),
        Line::from(Span::styled("The current draft remains open as unsaved edits after deletion.", Style::default().fg(theme.text_dim))),
        Line::raw(""),
        Line::from(vec![chip("y Delete", theme.destructive, theme), Span::raw(" "), chip("Esc Cancel", theme.chip_dismiss, theme)]),
    ];
    f.render_widget(Paragraph::new(lines).alignment(Alignment::Left), inner);
    record_footer_chips(button_map, inner.x, inner.y.saturating_add(5), &[
        (TuiButton::ThemeBuilderDeleteConfirm, "y Delete"),
        (TuiButton::ThemeBuilderDeleteCancel, "Esc Cancel"),
    ]);
}

fn draw_theme_file_dialog(
    f: &mut Frame,
    state: &ThemeBuilderState,
    button_map: &mut ButtonRenderMap,
    theme: theme::Theme,
    export: bool,
) {
    let area = scaled_centered_rect(74, 11, f.size());
    f.render_widget(Clear, area);
    let title = if export { " Export Theme File " } else { " Import Theme File " };
    let block = Block::default()
        .title(Span::styled(title, Style::default().fg(theme.title).add_modifier(Modifier::BOLD)))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let input = if export { &state.export_path_input } else { &state.import_path_input };
    let help = if export {
        "Writes the current draft as the normal .toml theme file."
    } else {
        "Loads a .toml theme file and imports it with a safe custom slug."
    };
    let label = if export { "Export path" } else { "Import path" };
    let action = if export { "Enter Export" } else { "Enter Import" };
    let max_path = inner.width.saturating_sub(18).max(8) as usize;
    let lines = vec![
        Line::from(Span::styled(help, Style::default().fg(theme.text_dim))),
        Line::raw(""),
        Line::from(vec![
            Span::styled(format!("{label:<12} "), Style::default().fg(theme.label)),
            Span::styled(format!("[ {} ]", truncate_chars(&input.text, max_path)), input_style(true, theme)),
        ]),
        Line::raw(""),
        Line::from(vec![chip(action, theme.progress_dialog_button_bg, theme), Span::raw(" "), chip("Esc Cancel", theme.chip_dismiss, theme)]),
    ];
    f.render_widget(Paragraph::new(lines).alignment(Alignment::Left), inner);
    record_rect(button_map, TuiButton::ThemeBuilderFilePath, inner.x.saturating_add(13), inner.y.saturating_add(2), inner.width.saturating_sub(13), 1);
    record_footer_chips(button_map, inner.x, inner.y.saturating_add(4), &[
        (TuiButton::ThemeBuilderFileConfirm, action),
        (TuiButton::ThemeBuilderFileCancel, "Esc Cancel"),
    ]);
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

fn derived_list_rows() -> Vec<DerivedListRow> {
    let mut rows = Vec::new();
    let mut current_group: Option<&'static str> = None;
    for (index, spec) in theme::derived_element_specs().iter().enumerate() {
        if current_group != Some(spec.group) {
            current_group = Some(spec.group);
            rows.push(DerivedListRow { spec_index: None, group: spec.group });
        }
        rows.push(DerivedListRow { spec_index: Some(index), group: spec.group });
    }
    rows
}

fn visible_gallery_families(state: &ThemeBuilderState) -> Vec<GalleryFamily> {
    let filter = state.gallery_filter_input.text.trim().to_ascii_lowercase();
    let mut families: Vec<GalleryFamily> = Vec::new();
    for choice in state.theme_library.iter().cloned() {
        let key = gallery_family_key(&choice);
        let name = gallery_family_name(&choice);
        if !filter.is_empty()
            && !name.to_ascii_lowercase().contains(&filter)
            && !choice.slug.to_ascii_lowercase().contains(&filter)
        {
            continue;
        }
        if let Some(existing) = families.iter_mut().find(|family| family.key == key) {
            if choice.dark {
                existing.dark = Some(choice.clone());
            } else {
                existing.light = Some(choice.clone());
            }
            if existing.fallback.dark != state.gallery_dark && choice.dark == state.gallery_dark {
                existing.fallback = choice;
            }
        } else {
            families.push(GalleryFamily {
                key,
                name,
                dark: if choice.dark { Some(choice.clone()) } else { None },
                light: if choice.dark { None } else { Some(choice.clone()) },
                fallback: choice,
            });
        }
    }
    families.sort_by(|a, b| a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase()));
    families
}

fn gallery_family_key(choice: &theme::ThemeChoice) -> String {
    let mut slug = choice.slug.to_ascii_lowercase().replace('_', "-");
    for suffix in ["-light", "-dark", "-day", "-dawn", "-lotus", "-latte"] {
        if let Some(stripped) = slug.strip_suffix(suffix) {
            slug = stripped.to_string();
            break;
        }
    }
    slug
}

fn gallery_family_name(choice: &theme::ThemeChoice) -> String {
    let mut name = choice.name.clone();
    for suffix in [" Light", " Dark", " Day", " Dawn", " Lotus", " Latte", " light", " dark", " day", " dawn", " lotus", " latte"] {
        if let Some(stripped) = name.strip_suffix(suffix) {
            name = stripped.to_string();
            break;
        }
    }
    name
}

fn display_choice_for_family(family: &GalleryFamily, dark: bool) -> theme::ThemeChoice {
    if dark {
        family.dark.clone().or_else(|| family.light.clone()).unwrap_or_else(|| family.fallback.clone())
    } else {
        family.light.clone().or_else(|| family.dark.clone()).unwrap_or_else(|| family.fallback.clone())
    }
}

fn selected_gallery_choice(state: &ThemeBuilderState) -> Option<theme::ThemeChoice> {
    let families = visible_gallery_families(state);
    families.get(state.preset_cursor.min(families.len().saturating_sub(1)))
        .map(|family| display_choice_for_family(family, state.gallery_dark))
}

fn clamp_gallery_cursor(state: &mut ThemeBuilderState) {
    let len = visible_gallery_families(state).len();
    state.preset_cursor = state.preset_cursor.min(len.saturating_sub(1));
    sync_gallery_scroll(state, gallery_visible_rows_for_state(state));
}

fn sync_gallery_scroll(state: &mut ThemeBuilderState, visible_rows: usize) {
    let len = visible_gallery_families(state).len();
    let visible_rows = visible_rows.max(1);
    let max_scroll = len.saturating_sub(visible_rows);
    state.preset_scroll = state.preset_scroll.min(max_scroll);
    if state.preset_cursor < state.preset_scroll {
        state.preset_scroll = state.preset_cursor;
    } else if state.preset_cursor >= state.preset_scroll.saturating_add(visible_rows) {
        state.preset_scroll = state.preset_cursor + 1 - visible_rows;
    }
    state.preset_scroll = state.preset_scroll.min(max_scroll);
}

fn move_gallery_cursor(state: &mut ThemeBuilderState, delta: isize) {
    let len = visible_gallery_families(state).len();
    if len == 0 {
        state.preset_cursor = 0;
        state.preset_scroll = 0;
        return;
    }
    state.preset_cursor = move_cursor(state.preset_cursor, len, delta);
    sync_gallery_scroll(state, gallery_visible_rows_for_state(state));
}

fn move_gallery_cursor_vertical(state: &mut ThemeBuilderState, rows: isize) {
    let len = visible_gallery_families(state).len();
    if len == 0 {
        state.preset_cursor = 0;
        state.preset_scroll = 0;
        return;
    }
    let columns = gallery_columns_for_state(state);
    if columns <= 1 {
        move_gallery_cursor(state, rows.signum());
        return;
    }

    let current_row = state.preset_cursor / columns;
    let current_col = state.preset_cursor % columns;
    let target_row = current_row as isize + rows;
    if target_row < 0 {
        return;
    }
    let target = target_row as usize * columns + current_col;
    if target < len {
        state.preset_cursor = target;
        sync_gallery_scroll(state, gallery_visible_rows_for_state(state));
    }
}

fn move_gallery_cursor_horizontal(state: &mut ThemeBuilderState, columns_delta: isize) {
    let len = visible_gallery_families(state).len();
    if len == 0 {
        state.preset_cursor = 0;
        state.preset_scroll = 0;
        return;
    }
    let columns = gallery_columns_for_state(state);
    if columns <= 1 {
        move_gallery_cursor(state, columns_delta.signum());
        return;
    }

    let col = state.preset_cursor % columns;
    let next = if columns_delta < 0 {
        if col == 0 { state.preset_cursor } else { state.preset_cursor.saturating_sub(1) }
    } else if col + 1 >= columns || state.preset_cursor + 1 >= len {
        state.preset_cursor
    } else {
        state.preset_cursor + 1
    };
    state.preset_cursor = next;
    sync_gallery_scroll(state, gallery_visible_rows_for_state(state));
}

fn gallery_columns_for_width(width: u16) -> usize {
    if width >= 96 { 2 } else { 1 }
}

fn gallery_columns_for_state(state: &ThemeBuilderState) -> usize {
    state.preset_visible_columns.get().max(1)
}

fn gallery_visible_rows_for_state(state: &ThemeBuilderState) -> usize {
    let visible = state.preset_visible_rows.get();
    if visible == 0 { DEFAULT_GALLERY_VISIBLE_ROWS } else { visible.max(1) }
}

fn active_slug_matches(current: &str, choice: &str) -> bool {
    let current = current.trim().to_ascii_lowercase().replace('_', "-");
    let choice = choice.trim().to_ascii_lowercase().replace('_', "-");
    current == choice || current == format!("{choice}-custom") || format!("{current}-custom") == choice
}

fn next_builder_depth(depth: ColorDepth) -> ColorDepth {
    match depth {
        ColorDepth::TrueColor => ColorDepth::Xterm256,
        ColorDepth::Xterm256 => ColorDepth::Ansi16,
        ColorDepth::Ansi16 | ColorDepth::Ansi8 => ColorDepth::TrueColor,
    }
}

fn focus_mark(focused: bool, theme: theme::Theme) -> Span<'static> {
    if focused {
        Span::styled("> ", Style::default().fg(theme.warning).add_modifier(Modifier::BOLD))
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

fn record_rect(button_map: &mut ButtonRenderMap, button: TuiButton, x: u16, y: u16, width: u16, height: u16) {
    if width == 0 || height == 0 {
        return;
    }
    button_map.record_button(button, Rect { x, y, width, height });
}

fn record_clipped_rect(button_map: &mut ButtonRenderMap, button: TuiButton, x: u16, y: u16, width: u16, height: u16, row_end: u16) {
    if x >= row_end {
        return;
    }
    record_rect(button_map, button, x, y, width.min(row_end.saturating_sub(x)), height);
}

fn record_footer_chips(button_map: &mut ButtonRenderMap, x: u16, y: u16, chips: &[(TuiButton, &str)]) {
    let mut cursor = x;
    for (button, label) in chips.iter().copied() {
        let width = label.chars().count().saturating_add(2).min(u16::MAX as usize) as u16;
        record_rect(button_map, button, cursor, y, width, 1);
        cursor = cursor.saturating_add(width).saturating_add(1);
    }
}

fn record_editor_buttons(button_map: &mut ButtonRenderMap, area: Rect, state: &ThemeBuilderState, derived: bool) {
    let hex_y = area.y.saturating_add(if derived { 5 } else { 3 });
    record_rect(button_map, TuiButton::ThemeBuilderHexField, area.x, hex_y, area.width.min(32), 1);
    for channel in 0..3 {
        record_rect(button_map, TuiButton::ThemeBuilderRgbSlider(channel), area.x, hex_y.saturating_add(1 + channel as u16), area.width.min(30), 1);
    }
    if !derived {
        let swatch_y = area.y.saturating_add(10);
        if state.swatch_naming_active {
            record_rect(button_map, TuiButton::ThemeBuilderInlineSwatchName, area.x.saturating_add(16), swatch_y, area.width.saturating_sub(16).min(24), 1);
            record_rect(button_map, TuiButton::ThemeBuilderSaveSwatch, area.x.saturating_add(area.width.saturating_sub(18)), swatch_y, 11, 1);
        } else {
            let visible = state.palette.swatches.len().min(12);
            for index in 0..visible {
                record_rect(button_map, TuiButton::ThemeBuilderSavedSwatch(index), area.x.saturating_add(12 + index as u16 * 3), swatch_y, 2, 1);
            }
            let save_x = if visible == 0 {
                area.x.saturating_add(20)
            } else {
                area.x.saturating_add(12 + visible as u16 * 3)
            };
            record_rect(button_map, TuiButton::ThemeBuilderSaveSwatch, save_x, swatch_y, 10, 1);
        }
    }
}

fn expand_user_path(input: &str) -> PathBuf {
    if input.is_empty() {
        return PathBuf::new();
    }
    if input == "~" {
        return std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(input));
    }
    if let Some(rest) = input.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    Path::new(input).to_path_buf()
}

fn truncate_chars(input: &str, max: usize) -> String {
    let mut out = String::new();
    for ch in input.chars().take(max) {
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::test_support::XdgConfigHomeGuard;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
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
    fn edited_hex_updates_selected_palette_slot() {
        let mut state = ThemeBuilderState::from_palette(ThemePaletteDraft::from_palette(theme::default_palette()));
        state.editor_focus = BuilderEditorFocus::Hex;
        state.hex_input = TextInputState::new_selected("#000000".to_string());
        assert_eq!(handle_theme_builder_key(&mut state, key(KeyCode::Enter)), ThemeBuilderAction::None);
        assert_eq!(state.palette.panel_bg, Color::Rgb(0, 0, 0));
        assert!(state.dirty);
    }

    #[test]
    fn editor_focus_cycle_is_five_stops() {
        let mut focus = BuilderEditorFocus::Slots;
        let mut seen = Vec::new();
        for _ in 0..5 {
            seen.push(focus);
            focus = focus.next();
        }
        assert_eq!(seen, vec![
            BuilderEditorFocus::Slots,
            BuilderEditorFocus::Hex,
            BuilderEditorFocus::Red,
            BuilderEditorFocus::Green,
            BuilderEditorFocus::Blue,
        ]);
        assert_eq!(focus, BuilderEditorFocus::Slots);
    }

    #[test]
    fn tab_click_switches_persistent_tabs() {
        let mut state = ThemeBuilderState::from_palette(ThemePaletteDraft::from_palette(theme::default_palette()));
        assert_eq!(state.tab, BuilderTab::Edit);
        assert_eq!(handle_theme_builder_mouse(&mut state, left_click(), Some(TuiButton::ThemeBuilderTab(1))), ThemeBuilderAction::None);
        assert_eq!(state.tab, BuilderTab::Preview);
        assert_eq!(handle_theme_builder_mouse(&mut state, left_click(), Some(TuiButton::ThemeBuilderTab(2))), ThemeBuilderAction::None);
        assert_eq!(state.tab, BuilderTab::Derived);
        assert_eq!(handle_theme_builder_mouse(&mut state, left_click(), Some(TuiButton::ThemeBuilderTab(0))), ThemeBuilderAction::None);
        assert_eq!(state.tab, BuilderTab::Edit);
    }

    #[test]
    fn derived_space_toggle_writes_theme_lock_only() {
        let mut state = ThemeBuilderState::from_palette(ThemePaletteDraft::from_palette(theme::default_palette()));
        state.tab = BuilderTab::Derived;
        assert_eq!(handle_theme_builder_key(&mut state, key(KeyCode::Char(' '))), ThemeBuilderAction::None);
        assert!(state.palette.derived_locks.contains_key("surface"));
        assert!(!state.user_overrides.overrides.contains_key("surface"));
        assert_eq!(state.editor_focus, BuilderEditorFocus::Hex);
    }

    #[test]
    fn derived_release_clears_theme_lock_to_formula() {
        let mut state = ThemeBuilderState::from_palette(ThemePaletteDraft::from_palette(theme::default_palette()));
        state.tab = BuilderTab::Derived;
        state.palette.derived_locks.insert("surface".to_string(), Color::Rgb(1, 2, 3));
        state.sync_derived_hex_from_selected();
        assert_eq!(state.derived_hex_input.text, "#010203");
        assert_eq!(handle_theme_builder_key(&mut state, key(KeyCode::Char(' '))), ThemeBuilderAction::None);
        assert!(!state.palette.derived_locks.contains_key("surface"));
        assert_ne!(state.derived_hex_input.text, "#010203");
    }

    #[test]
    fn derived_lock_seeds_from_displayed_value() {
        let mut state = ThemeBuilderState::from_palette(ThemePaletteDraft::from_palette(theme::default_palette()));
        state.tab = BuilderTab::Derived;
        let displayed = state.selected_derived_display_color();
        assert_eq!(handle_theme_builder_key(&mut state, key(KeyCode::Char(' '))), ThemeBuilderAction::None);
        assert_eq!(state.palette.derived_locks.get("surface"), Some(&displayed));
    }

    #[test]
    fn mouse_click_accent_assigns_to_selected_role_and_keeps_role_selected() {
        let mut state = ThemeBuilderState::from_palette(ThemePaletteDraft::from_palette(theme::default_palette()));
        let original = Color::Rgb(1, 2, 3);
        let accent = Color::Rgb(9, 8, 7);
        state.palette.panel_bg = original;
        state.palette.accents[12] = accent;
        state.sync_hex_and_rgb_from_slot();
        state.dirty = false;

        assert_eq!(
            handle_theme_builder_mouse(&mut state, left_click(), Some(TuiButton::ThemeBuilderSlot(BuilderSlot::Accent(12)))),
            ThemeBuilderAction::None,
        );

        assert_eq!(state.selected_slot, BuilderSlot::Role(0));
        assert_eq!(state.last_role_slot, BuilderSlot::Role(0));
        assert_eq!(state.palette.panel_bg, accent);
        assert_eq!(state.selected_color(), accent);
        assert_eq!(state.hex_input.text, "#090807");
        assert_eq!(state.recent_colors.first().copied(), Some(original));
        assert!(state.dirty);
        assert_eq!(state.resolved_theme().panel_bg, accent);
    }

    #[test]
    fn mouse_click_accent_while_accent_selected_still_assigns_to_last_role() {
        let mut state = ThemeBuilderState::from_palette(ThemePaletteDraft::from_palette(theme::default_palette()));
        let target_slot = BuilderSlot::Role(2);
        let original_role = Color::Rgb(1, 2, 3);
        let accent = Color::Rgb(9, 8, 7);
        state.palette.set_color_at_slot(target_slot, original_role);
        state.palette.accents[1] = accent;
        state.set_selected_slot(target_slot);
        state.set_selected_slot(BuilderSlot::Accent(0));
        state.dirty = false;

        assert_eq!(
            handle_theme_builder_mouse(&mut state, left_click(), Some(TuiButton::ThemeBuilderSlot(BuilderSlot::Accent(1)))),
            ThemeBuilderAction::None,
        );

        assert_eq!(state.selected_slot, target_slot);
        assert_eq!(state.last_role_slot, target_slot);
        assert_eq!(state.palette.role_color(2), accent);
        assert_eq!(state.selected_color(), accent);
        assert!(state.dirty);
    }

    #[test]
    fn modified_mouse_click_accent_selects_accent_for_direct_editing() {
        let mut state = ThemeBuilderState::from_palette(ThemePaletteDraft::from_palette(theme::default_palette()));
        let original_role = Color::Rgb(1, 2, 3);
        let accent = Color::Rgb(9, 8, 7);
        state.palette.panel_bg = original_role;
        state.palette.accents[1] = accent;
        state.set_selected_slot(BuilderSlot::Accent(0));
        state.dirty = false;

        let mut click = left_click();
        click.modifiers = KeyModifiers::CONTROL;
        assert_eq!(
            handle_theme_builder_mouse(&mut state, click, Some(TuiButton::ThemeBuilderSlot(BuilderSlot::Accent(1)))),
            ThemeBuilderAction::None,
        );

        assert_eq!(state.selected_slot, BuilderSlot::Accent(1));
        assert_eq!(state.palette.panel_bg, original_role);
        assert_eq!(state.selected_color(), accent);
        assert!(!state.dirty);
    }

    #[test]
    fn mouse_click_role_while_role_selected_navigates_without_mutation() {
        let mut state = ThemeBuilderState::from_palette(ThemePaletteDraft::from_palette(theme::default_palette()));
        let role_one_color = Color::Rgb(3, 4, 5);
        state.palette.set_color_at_slot(BuilderSlot::Role(1), role_one_color);
        state.set_selected_slot(BuilderSlot::Role(0));
        state.dirty = false;

        assert_eq!(
            handle_theme_builder_mouse(&mut state, left_click(), Some(TuiButton::ThemeBuilderSlot(BuilderSlot::Role(1)))),
            ThemeBuilderAction::None,
        );

        assert_eq!(state.selected_slot, BuilderSlot::Role(1));
        assert_eq!(state.last_role_slot, BuilderSlot::Role(1));
        assert_eq!(state.selected_color(), role_one_color);
        assert!(!state.dirty);
    }

    #[test]
    fn keyboard_enter_on_highlighted_accent_assigns_to_last_role() {
        let mut state = ThemeBuilderState::from_palette(ThemePaletteDraft::from_palette(theme::default_palette()));
        let target_slot = BuilderSlot::Role(ROLE_KEYS.len().saturating_sub(1));
        let original = Color::Rgb(21, 22, 23);
        let accent = Color::Rgb(31, 32, 33);
        state.palette.set_color_at_slot(target_slot, original);
        state.palette.accents[0] = accent;
        state.set_selected_slot(target_slot);
        state.dirty = false;

        assert_eq!(handle_theme_builder_key(&mut state, key(KeyCode::Down)), ThemeBuilderAction::None);
        assert_eq!(state.selected_slot, BuilderSlot::Accent(0));
        assert_eq!(state.last_role_slot, target_slot);

        assert_eq!(handle_theme_builder_key(&mut state, key(KeyCode::Enter)), ThemeBuilderAction::None);
        assert_eq!(state.selected_slot, target_slot);
        assert_eq!(state.selected_color(), accent);
        assert_eq!(state.recent_colors.first().copied(), Some(original));
        assert!(state.dirty);
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
    }

    #[test]
    fn reapplying_already_bound_swatch_does_not_mark_dirty() {
        let mut state = ThemeBuilderState::from_palette(ThemePaletteDraft::from_palette(theme::default_palette()));
        state.palette.swatches.push(NamedSwatch::new("saved", state.selected_color()));
        state.apply_saved_swatch(0);
        state.dirty = false;

        state.apply_saved_swatch(0);

        assert!(!state.dirty);
    }

    #[test]
    fn derived_slider_clamped_noop_does_not_mark_dirty() {
        let mut state = ThemeBuilderState::from_palette(ThemePaletteDraft::from_palette(theme::default_palette()));
        state.tab = BuilderTab::Derived;
        state.palette.derived_locks.insert("surface".to_string(), Color::Rgb(255, 2, 3));
        state.sync_derived_hex_from_selected();
        state.dirty = false;

        state.adjust_derived_rgb_channel(0, 1);

        assert_eq!(state.palette.derived_locks.get("surface"), Some(&Color::Rgb(255, 2, 3)));
        assert!(!state.dirty);
    }

    #[test]
    fn swatch_plus_opens_inline_name_before_saving() {
        let mut state = ThemeBuilderState::from_palette(ThemePaletteDraft::from_palette(theme::default_palette()));
        state.set_selected_color(Color::Rgb(10, 20, 30));

        assert_eq!(handle_theme_builder_key(&mut state, key(KeyCode::Char('+'))), ThemeBuilderAction::None);
        assert!(state.swatch_naming_active);
        assert!(state.palette.swatches.is_empty(), "+ opens naming; it must not save immediately");

        state.swatch_name_input = TextInputState::new_selected("brand purple".to_string());
        assert_eq!(handle_theme_builder_key(&mut state, key(KeyCode::Enter)), ThemeBuilderAction::None);
        assert!(!state.swatch_naming_active);
        assert_eq!(state.palette.swatches[0].name, "brand_purple");
        assert_eq!(state.palette.swatches[0].color, Color::Rgb(10, 20, 30));

        state.set_selected_color(Color::Rgb(40, 50, 60));
        assert_eq!(handle_theme_builder_key(&mut state, key(KeyCode::Char('+'))), ThemeBuilderAction::None);
        state.swatch_name_input = TextInputState::new_selected("brand_purple".to_string());
        assert_eq!(handle_theme_builder_key(&mut state, key(KeyCode::Enter)), ThemeBuilderAction::None);
        assert_eq!(state.palette.swatches.len(), 1);
        assert_eq!(state.palette.swatches[0].color, Color::Rgb(40, 50, 60));
    }

    #[test]
    fn swatch_inline_name_escape_cancels_without_saving() {
        let mut state = ThemeBuilderState::from_palette(ThemePaletteDraft::from_palette(theme::default_palette()));
        assert_eq!(handle_theme_builder_key(&mut state, key(KeyCode::Char('+'))), ThemeBuilderAction::None);
        assert!(state.swatch_naming_active);
        assert_eq!(handle_theme_builder_key(&mut state, key(KeyCode::Esc)), ThemeBuilderAction::None);
        assert!(!state.swatch_naming_active);
        assert!(state.palette.swatches.is_empty());
    }

    #[test]
    fn collapsed_swatch_row_records_no_recent_or_delete_clutter() {
        let mut state = ThemeBuilderState::from_palette(ThemePaletteDraft::from_palette(theme::default_palette()));
        state.palette.swatches.push(NamedSwatch::new("saved", Color::Rgb(9, 8, 7)));
        state.recent_colors.push(Color::Rgb(1, 2, 3));
        let theme = theme::theme_by_slug("tokyo-night").expect("theme");
        let backend = TestBackend::new(90, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let mut button_map = ButtonRenderMap::new();

        terminal.draw(|frame| draw_edit_card(frame, Rect::new(2, 2, 70, 16), &state, &mut button_map, theme)).expect("draw");

        assert!(button_map.find_button_rect(&TuiButton::ThemeBuilderSavedSwatch(0)).is_some());
        assert!(button_map.find_button_rect(&TuiButton::ThemeBuilderSaveSwatch).is_some());
        assert!(button_map.find_button_rect(&TuiButton::ThemeBuilderInlineSwatchName).is_none());
        let text = buffer_text(terminal.backend().buffer());
        assert!(!text.contains("Recent"));
        assert!(!text.contains("del"));
    }

    #[test]
    fn inline_swatch_naming_records_actual_name_hitbox_only_while_active() {
        let mut state = ThemeBuilderState::from_palette(ThemePaletteDraft::from_palette(theme::default_palette()));
        let theme = theme::theme_by_slug("tokyo-night").expect("theme");
        let backend = TestBackend::new(90, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let area = Rect::new(2, 2, 70, 16);

        let mut closed_map = ButtonRenderMap::new();
        terminal.draw(|frame| draw_edit_card(frame, area, &state, &mut closed_map, theme)).expect("draw closed");
        assert!(closed_map.find_button_rect(&TuiButton::ThemeBuilderInlineSwatchName).is_none());

        let save = closed_map.find_button_rect(&TuiButton::ThemeBuilderSaveSwatch).expect("save swatch rect");
        assert_eq!(closed_map.find_button_at(save.x, save.y), Some(TuiButton::ThemeBuilderSaveSwatch));
        assert_eq!(handle_theme_builder_mouse(&mut state, left_click(), Some(TuiButton::ThemeBuilderSaveSwatch)), ThemeBuilderAction::None);
        assert!(state.swatch_naming_active);

        let mut active_map = ButtonRenderMap::new();
        terminal.draw(|frame| draw_edit_card(frame, area, &state, &mut active_map, theme)).expect("draw active");
        let name = active_map.find_button_rect(&TuiButton::ThemeBuilderInlineSwatchName).expect("inline name rect");
        assert_eq!(terminal.backend().buffer().get(name.x, name.y).symbol(), "[");
        assert_eq!(active_map.find_button_at(name.x + 2, name.y), Some(TuiButton::ThemeBuilderInlineSwatchName));
    }

    #[test]
    fn preset_key_opens_gallery_overlay_without_losing_tab() {
        let mut state = ThemeBuilderState::from_palette(ThemePaletteDraft::from_palette(theme::default_palette()));
        state.tab = BuilderTab::Preview;
        assert_eq!(handle_theme_builder_key(&mut state, key(KeyCode::Char('p'))), ThemeBuilderAction::None);
        assert_eq!(state.overlay, BuilderOverlay::Gallery);
        assert_eq!(state.tab, BuilderTab::Preview);
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
        state.overlay = BuilderOverlay::Apply;
        assert_eq!(
            handle_theme_builder_mouse(&mut state, left_click(), Some(TuiButton::ThemeBuilderApplyConfirm)),
            ThemeBuilderAction::Apply,
        );
    }

    #[test]
    fn more_menu_delete_present_for_custom_absent_for_builtin_or_new() {
        let mut state = ThemeBuilderState::from_palette(ThemePaletteDraft::from_palette(theme::default_palette()));
        open_more_menu(&mut state);
        assert!(!state.more_menu.items.contains(&MoreMenuItem::Delete));
        assert!(!state.more_menu.items.contains(&MoreMenuItem::Revert));

        state.palette.source = ThemeDraftSource::Custom;
        open_more_menu(&mut state);
        assert!(state.more_menu.items.contains(&MoreMenuItem::Delete));
        assert!(state.more_menu.items.contains(&MoreMenuItem::Revert));
    }

    #[test]
    fn more_menu_duplicate_creates_visible_collision_free_copy() {
        let _xdg = XdgConfigHomeGuard::new("tonepoet-theme-builder-duplicate");
        let mut existing = ThemePaletteDraft::from_palette(theme::default_palette());
        existing.slug = "tokyo-night-custom-copy".to_string();
        existing.source = ThemeDraftSource::NewCustom;
        theme::save_theme_file(&existing).expect("seed duplicate collision");

        let mut state = ThemeBuilderState::from_palette(ThemePaletteDraft::from_palette(theme::default_palette()));
        state.palette.slug = "tokyo-night-custom".to_string();
        state.palette.name = "Tokyo Night Custom".to_string();
        state.palette.source = ThemeDraftSource::NewCustom;
        open_more_menu(&mut state);
        execute_more_menu_direct(&mut state, MoreMenuItem::Duplicate);
        assert_eq!(state.palette.slug, "tokyo-night-custom-copy-2");
        assert_eq!(state.palette.source, ThemeDraftSource::NewCustom);
        assert!(state.dirty);
    }

    #[test]
    fn more_menu_export_opens_dialog_and_writes_theme_file() {
        let _xdg = XdgConfigHomeGuard::new("tonepoet-theme-builder-export");
        let mut state = ThemeBuilderState::from_palette(ThemePaletteDraft::from_palette(theme::default_palette()));
        state.palette.slug = "export-me".to_string();
        state.palette.name = "Export Me".to_string();
        state.palette.source = ThemeDraftSource::NewCustom;
        open_more_menu(&mut state);
        execute_more_menu_direct(&mut state, MoreMenuItem::Export);
        assert_eq!(state.overlay, BuilderOverlay::ExportDialog);

        let path = theme::custom_theme_dir().join("explicit-export.toml");
        state.export_path_input = TextInputState::new_selected(path.display().to_string());
        assert_eq!(handle_theme_builder_key(&mut state, key(KeyCode::Enter)), ThemeBuilderAction::None);
        assert_eq!(state.overlay, BuilderOverlay::None);
        assert!(path.exists());
        assert!(std::fs::read_to_string(path).expect("read export").contains("Export Me"));
    }

    #[test]
    fn export_to_noncanonical_custom_dir_file_does_not_mark_builder_clean_or_saved() {
        let _xdg = XdgConfigHomeGuard::new("tonepoet-theme-builder-export-noncanonical-clean");
        let mut state = ThemeBuilderState::from_palette(ThemePaletteDraft::from_palette(theme::default_palette()));
        state.palette.slug = "export-me".to_string();
        state.palette.name = "Export Me".to_string();
        state.palette.source = ThemeDraftSource::NewCustom;
        state.dirty = true;

        let path = theme::custom_theme_dir().join("explicit-export.toml");
        state.export_path_input = TextInputState::new_selected(path.display().to_string());
        state.export_current_theme();

        assert_eq!(state.overlay, BuilderOverlay::None);
        assert!(path.exists());
        assert!(!theme::custom_theme_path_for_slug("export-me").expect("canonical path").exists());
        assert_eq!(state.palette.slug, "export-me");
        assert_eq!(state.palette.source, ThemeDraftSource::NewCustom);
        assert!(state.dirty, "noncanonical export is a copy, not a saved canonical theme");
        assert_eq!(theme::unique_custom_theme_slug("export-me").expect("unique slug"), "export-me-2");
    }

    #[test]
    fn repeated_export_to_same_noncanonical_path_is_slug_idempotent() {
        let _xdg = XdgConfigHomeGuard::new("tonepoet-theme-builder-export-noncanonical-idempotent");
        let mut state = ThemeBuilderState::from_palette(ThemePaletteDraft::from_palette(theme::default_palette()));
        state.palette.slug = "export-me".to_string();
        state.palette.name = "Export Me".to_string();
        state.palette.source = ThemeDraftSource::NewCustom;
        state.dirty = true;

        let path = theme::custom_theme_dir().join("explicit-export.toml");
        state.export_path_input = TextInputState::new_selected(path.display().to_string());

        state.export_current_theme();
        let first = theme::load_theme_file(&path).expect("read first export");
        assert_eq!(first.slug, "export-me");

        state.overlay = BuilderOverlay::ExportDialog;
        state.export_current_theme();
        let second = theme::load_theme_file(&path).expect("read second export");
        assert_eq!(second.slug, "export-me");

        assert_eq!(state.palette.slug, "export-me");
        assert_eq!(state.palette.source, ThemeDraftSource::NewCustom);
        assert!(state.dirty, "noncanonical export remains a copy, not a saved canonical theme");
    }

    #[test]
    fn export_flushes_pending_hex_edit_before_serializing() {
        let _xdg = XdgConfigHomeGuard::new("tonepoet-theme-builder-export-flush-hex");
        let mut state = ThemeBuilderState::from_palette(ThemePaletteDraft::from_palette(theme::default_palette()));
        state.palette.slug = "export-flush".to_string();
        state.palette.name = "Export Flush".to_string();
        state.palette.source = ThemeDraftSource::NewCustom;
        state.editor_focus = BuilderEditorFocus::Hex;
        state.hex_input = TextInputState::new_selected("#010203".to_string());

        let path = theme::custom_theme_path_for_slug("export-flush").expect("canonical path");
        state.export_path_input = TextInputState::new_selected(path.display().to_string());
        state.export_current_theme();

        let exported = theme::load_theme_file(&path).expect("read exported draft");
        assert_eq!(exported.panel_bg, Color::Rgb(1, 2, 3));
        assert_eq!(state.palette.panel_bg, Color::Rgb(1, 2, 3));
        assert_eq!(state.overlay, BuilderOverlay::None);
    }

    #[test]
    fn export_to_canonical_custom_path_marks_builder_clean_and_saved() {
        let _xdg = XdgConfigHomeGuard::new("tonepoet-theme-builder-export-canonical-clean");
        let mut state = ThemeBuilderState::from_palette(ThemePaletteDraft::from_palette(theme::default_palette()));
        state.palette.slug = "canonical-export".to_string();
        state.palette.name = "Canonical Export".to_string();
        state.palette.source = ThemeDraftSource::NewCustom;
        state.dirty = true;

        let path = theme::custom_theme_path_for_slug("canonical-export").expect("canonical path");
        state.export_path_input = TextInputState::new_selected(path.display().to_string());
        state.export_current_theme();

        assert_eq!(state.overlay, BuilderOverlay::None);
        assert!(path.exists());
        assert_eq!(state.palette.slug, "canonical-export");
        assert_eq!(state.palette.source, ThemeDraftSource::Custom);
        assert!(!state.dirty, "canonical export is equivalent to saving this custom theme");
    }

    #[test]
    fn more_menu_import_opens_dialog_and_imports_collision_free_theme() {
        let _xdg = XdgConfigHomeGuard::new("tonepoet-theme-builder-import");
        let mut source = ThemePaletteDraft::from_palette(theme::default_palette());
        source.slug = "import-me".to_string();
        source.name = "Import Me".to_string();
        source.source = ThemeDraftSource::Custom;
        let source_path = theme::custom_theme_dir()
            .parent()
            .expect("config parent")
            .join("incoming.toml");
        theme::export_theme_file_to_path(&source, &source_path).expect("write import source");

        let mut state = ThemeBuilderState::from_palette(ThemePaletteDraft::from_palette(theme::default_palette()));
        open_more_menu(&mut state);
        execute_more_menu_direct(&mut state, MoreMenuItem::Import);
        assert_eq!(state.overlay, BuilderOverlay::ImportDialog);
        state.import_path_input = TextInputState::new_selected(source_path.display().to_string());
        assert_eq!(handle_theme_builder_key(&mut state, key(KeyCode::Enter)), ThemeBuilderAction::None);

        assert_eq!(state.overlay, BuilderOverlay::None);
        assert_eq!(state.palette.name, "Import Me");
        assert_eq!(state.palette.slug, "import-me");
        assert_eq!(state.palette.source, ThemeDraftSource::Custom);
        assert!(theme::custom_theme_path_for_slug("import-me").expect("theme path").exists());
    }

    #[test]
    fn export_dialog_rendered_confirm_hitbox_writes_theme_file() {
        let _xdg = XdgConfigHomeGuard::new("tonepoet-theme-builder-export-hitbox");
        let mut state = ThemeBuilderState::from_palette(ThemePaletteDraft::from_palette(theme::default_palette()));
        state.palette.slug = "export-hitbox".to_string();
        state.palette.name = "Export Hitbox".to_string();
        state.palette.source = ThemeDraftSource::NewCustom;
        state.begin_export_dialog();
        let path = theme::custom_theme_dir().join("export-hitbox-explicit.toml");
        state.export_path_input = TextInputState::new_selected(path.display().to_string());
        let theme = theme::theme_by_slug("tokyo-night").expect("theme");
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let mut button_map = ButtonRenderMap::new();

        terminal.draw(|frame| draw_theme_builder(frame, &state, &mut button_map, theme)).expect("draw export dialog");
        let confirm = button_map.find_button_rect(&TuiButton::ThemeBuilderFileConfirm).expect("export confirm rect");
        assert!(buffer_text(terminal.backend().buffer()).contains("Export Theme File"));
        assert_eq!(button_map.find_button_at(confirm.x, confirm.y), Some(TuiButton::ThemeBuilderFileConfirm));
        assert_eq!(handle_theme_builder_mouse(&mut state, left_click(), button_map.find_button_at(confirm.x, confirm.y)), ThemeBuilderAction::None);

        assert_eq!(state.overlay, BuilderOverlay::None);
        assert!(path.exists());
        assert!(std::fs::read_to_string(path).expect("read export").contains("Export Hitbox"));
    }

    #[test]
    fn import_dialog_rendered_confirm_hitbox_imports_theme_file() {
        let _xdg = XdgConfigHomeGuard::new("tonepoet-theme-builder-import-hitbox");
        let mut source = ThemePaletteDraft::from_palette(theme::default_palette());
        source.slug = "import-hitbox".to_string();
        source.name = "Import Hitbox".to_string();
        source.source = ThemeDraftSource::Custom;
        let source_path = theme::custom_theme_dir()
            .parent()
            .expect("config parent")
            .join("incoming-hitbox.toml");
        theme::export_theme_file_to_path(&source, &source_path).expect("write import source");

        let mut state = ThemeBuilderState::from_palette(ThemePaletteDraft::from_palette(theme::default_palette()));
        state.begin_import_dialog();
        state.import_path_input = TextInputState::new_selected(source_path.display().to_string());
        let theme = theme::theme_by_slug("tokyo-night").expect("theme");
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let mut button_map = ButtonRenderMap::new();

        terminal.draw(|frame| draw_theme_builder(frame, &state, &mut button_map, theme)).expect("draw import dialog");
        let confirm = button_map.find_button_rect(&TuiButton::ThemeBuilderFileConfirm).expect("import confirm rect");
        assert!(buffer_text(terminal.backend().buffer()).contains("Import Theme File"));
        assert_eq!(button_map.find_button_at(confirm.x, confirm.y), Some(TuiButton::ThemeBuilderFileConfirm));
        assert_eq!(handle_theme_builder_mouse(&mut state, left_click(), button_map.find_button_at(confirm.x, confirm.y)), ThemeBuilderAction::None);

        assert_eq!(state.overlay, BuilderOverlay::None);
        assert_eq!(state.palette.slug, "import-hitbox");
        assert_eq!(state.palette.name, "Import Hitbox");
    }

    #[test]
    fn delete_theme_requires_confirmation_before_filesystem_action() {
        let mut state = ThemeBuilderState::from_palette(ThemePaletteDraft::from_palette(theme::default_palette()));
        state.palette.slug = "missing-theme".to_string();
        state.palette.source = ThemeDraftSource::Custom;

        state.request_delete_current_custom_theme();
        assert_eq!(state.overlay, BuilderOverlay::DeleteConfirm);
        assert!(state.deleted_theme_slug.is_none());

        state.cancel_delete_current_custom_theme();
        assert_eq!(state.overlay, BuilderOverlay::None);
        assert!(state.deleted_theme_slug.is_none());

        state.request_delete_current_custom_theme();
        state.confirm_delete_current_custom_theme();
        assert!(state.deleted_theme_slug.is_none());
        assert!(state.status.unwrap_or_default().starts_with("Delete theme failed:"));
    }

    fn cached_gallery_choice(slug: &str, name: &str, dark: bool) -> theme::ThemeChoice {
        let mut accents = [Color::Rgb(0, 0, 0); theme::THEME_ACCENT_COUNT];
        for (index, color) in accents.iter_mut().enumerate() {
            *color = Color::Rgb(
                30u8.saturating_add(index as u8),
                90u8.saturating_add(index as u8),
                170u8.saturating_add(index as u8),
            );
        }
        theme::ThemeChoice {
            slug: slug.to_string(),
            name: name.to_string(),
            description: "Injected gallery preview".to_string(),
            dark,
            built_in: false,
            author_lock_count: 0,
            accents,
        }
    }

    #[test]
    fn gallery_enter_returns_builtin_slug_without_custom_draft_suffix() {
        let choices = theme::theme_choices();
        let gruvbox_index = choices.iter()
            .position(|choice| choice.slug == "gruvbox")
            .unwrap_or(0);
        let mut state = ThemeBuilderState::theme_gallery_from_active_theme_with_library(
            theme::theme_by_slug("tokyo-night").expect("theme"),
            gruvbox_index,
            choices,
        );

        match handle_theme_builder_key(&mut state, key(KeyCode::Enter)) {
            ThemeBuilderAction::ApplyPreset(slug) => assert!(!slug.ends_with("-custom")),
            other => panic!("expected ApplyPreset action, got {:?}", other),
        }
    }

    #[test]
    fn gallery_mode_toggle_preserves_cursor() {
        let mut state = ThemeBuilderState::theme_gallery_from_active_theme_with_library(
            theme::theme_by_slug("tokyo-night").expect("theme"),
            0,
            vec![
                cached_gallery_choice("duo-dark", "Duo Dark", true),
                cached_gallery_choice("duo-light", "Duo Light", false),
                cached_gallery_choice("solo", "Solo", true),
            ],
        );
        state.preset_cursor = 1;
        let before = state.preset_cursor;
        assert_eq!(handle_theme_builder_key(&mut state, key(KeyCode::Char('m'))), ThemeBuilderAction::None);
        assert_eq!(state.preset_cursor, before);
    }

    #[test]
    fn gallery_filter_narrows_visible_entries() {
        let mut state = ThemeBuilderState::theme_gallery_from_active_theme_with_library(
            theme::theme_by_slug("tokyo-night").expect("theme"),
            0,
            vec![
                cached_gallery_choice("alpha", "Alpha", true),
                cached_gallery_choice("beta", "Beta", true),
            ],
        );
        assert_eq!(visible_gallery_families(&state).len(), 2);
        state.gallery_filter_input = TextInputState::new("alp".to_string());
        assert_eq!(visible_gallery_families(&state).len(), 1);
        assert_eq!(visible_gallery_families(&state)[0].name, "Alpha");
    }

    #[test]
    fn derived_keyboard_scroll_uses_rendered_visible_rows() {
        let mut state = ThemeBuilderState::from_palette(ThemePaletteDraft::from_palette(theme::default_palette()));
        state.tab = BuilderTab::Derived;
        state.derived_visible_rows.set(derived_list_rows().len());

        for _ in 0..28 {
            assert_eq!(handle_theme_builder_key(&mut state, key(KeyCode::Down)), ThemeBuilderAction::None);
        }
        assert_eq!(state.derived_cursor, 28);
        assert_eq!(state.derived_scroll, 0);

        assert_eq!(handle_theme_builder_key(&mut state, key(KeyCode::Down)), ThemeBuilderAction::None);
        assert_eq!(state.derived_cursor, 28);
    }

    #[test]
    fn header_preset_hitbox_tracks_visible_header_text() {
        let state = ThemeBuilderState::from_palette(ThemePaletteDraft::from_palette(theme::default_palette()));
        let theme = theme::theme_by_slug("tokyo-night").expect("theme");
        let backend = TestBackend::new(100, 8);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let mut button_map = ButtonRenderMap::new();
        let area = Rect::new(5, 2, 90, 2);

        terminal.draw(|frame| draw_builder_header(frame, area, &state, &mut button_map, theme)).expect("draw header");

        let preset = button_map.find_button_rect(&TuiButton::ThemeBuilderPreset).expect("preset rect");
        let mode = button_map.find_button_rect(&TuiButton::ThemeBuilderMode).expect("mode rect");
        let depth = button_map.find_button_rect(&TuiButton::ThemeBuilderDepth(state.depth_mode)).expect("depth rect");
        assert!(preset.x > depth.x + depth.width, "preset target should follow the rendered depth label");
        assert!(preset.x < area.x + area.width.saturating_sub(10), "preset target should not be pinned to a stale far-right coordinate");

        let buffer = terminal.backend().buffer();
        assert_eq!(buffer.get(mode.x, mode.y).symbol(), "M");
        assert_eq!(buffer.get(depth.x, depth.y).symbol(), "D");
        assert_eq!(buffer.get(preset.x, preset.y).symbol(), "p");
        assert_eq!(buffer.get(preset.x + 2, preset.y).symbol(), "p");
        assert_eq!(button_map.find_button_at(preset.x, preset.y), Some(TuiButton::ThemeBuilderPreset));
        assert_eq!(button_map.find_button_at(area.x + area.width - 2, area.y), None);
    }

    #[test]
    fn right_aligned_tab_strip_records_visible_hitboxes() {
        let state = ThemeBuilderState::from_palette(ThemePaletteDraft::from_palette(theme::default_palette()));
        let theme = theme::theme_by_slug("tokyo-night").expect("theme");
        let backend = TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let mut button_map = ButtonRenderMap::new();
        let tab_area = Rect::new(10, 4, 50, 1);

        terminal.draw(|frame| draw_tab_strip(frame, tab_area, &state, &mut button_map, theme)).expect("draw");

        let edit = button_map.find_button_rect(&TuiButton::ThemeBuilderTab(0)).expect("edit tab rect");
        let preview = button_map.find_button_rect(&TuiButton::ThemeBuilderTab(1)).expect("preview tab rect");
        let derived = button_map.find_button_rect(&TuiButton::ThemeBuilderTab(2)).expect("derived tab rect");

        // " Edit " + gap + " Preview " + gap + " Derived " = 26 columns.
        assert_eq!(edit, Rect::new(tab_area.x + tab_area.width - 26, tab_area.y, 6, 1));
        assert_eq!(preview.x, edit.x + edit.width + 1);
        assert_eq!(derived.x, preview.x + preview.width + 1);
        assert!(edit.x > tab_area.x, "hitbox must follow the right-aligned rendered tabs");

        let buffer = terminal.backend().buffer();
        assert_eq!(buffer.get(edit.x + 1, edit.y).symbol(), "E");
        assert_eq!(buffer.get(preview.x + 1, preview.y).symbol(), "P");
        assert_eq!(buffer.get(derived.x + 1, derived.y).symbol(), "D");
        assert_eq!(button_map.find_button_at(edit.x + 1, edit.y), Some(TuiButton::ThemeBuilderTab(0)));
        assert_eq!(button_map.find_button_at(preview.x + 1, preview.y), Some(TuiButton::ThemeBuilderTab(1)));
        assert_eq!(button_map.find_button_at(derived.x + 1, derived.y), Some(TuiButton::ThemeBuilderTab(2)));
        assert_eq!(button_map.find_button_at(tab_area.x, tab_area.y), None);
    }

    #[test]
    fn derived_editor_hitboxes_follow_rendered_derived_card_offsets() {
        let mut state = ThemeBuilderState::from_palette(ThemePaletteDraft::from_palette(theme::default_palette()));
        state.tab = BuilderTab::Derived;
        state.toggle_selected_derived_lock();
        let theme = theme::theme_by_slug("tokyo-night").expect("theme");
        let backend = TestBackend::new(90, 30);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let mut button_map = ButtonRenderMap::new();
        let area = Rect::new(3, 7, 50, 16);

        terminal.draw(|frame| draw_derived_card(frame, area, &state, &mut button_map, theme)).expect("draw");

        let inner = Rect::new(area.x + 1, area.y + 1, area.width - 2, area.height - 2);
        let hex = button_map.find_button_rect(&TuiButton::ThemeBuilderHexField).expect("hex rect");
        let red = button_map.find_button_rect(&TuiButton::ThemeBuilderRgbSlider(0)).expect("red rect");
        let blue = button_map.find_button_rect(&TuiButton::ThemeBuilderRgbSlider(2)).expect("blue rect");
        assert_eq!(hex.y, inner.y + 5);
        assert_eq!(red.y, inner.y + 6);
        assert_eq!(blue.y, inner.y + 8);
        assert_eq!(terminal.backend().buffer().get(hex.x + 2, hex.y).symbol(), "H");
        assert_eq!(terminal.backend().buffer().get(red.x + 2, red.y).symbol(), "R");
        assert_eq!(terminal.backend().buffer().get(blue.x + 2, blue.y).symbol(), "B");
        assert_eq!(button_map.find_button_at(hex.x + 2, hex.y), Some(TuiButton::ThemeBuilderHexField));
        assert_eq!(button_map.find_button_at(red.x + 2, red.y), Some(TuiButton::ThemeBuilderRgbSlider(0)));
        assert_eq!(button_map.find_button_at(blue.x + 2, blue.y), Some(TuiButton::ThemeBuilderRgbSlider(2)));
    }

    #[test]
    fn gallery_renderer_records_column_count_for_keyboard_navigation() {
        let state = ThemeBuilderState::theme_gallery_from_active_theme_with_library(
            theme::theme_by_slug("tokyo-night").expect("theme"),
            0,
            vec![
                cached_gallery_choice("alpha", "Alpha", true),
                cached_gallery_choice("beta", "Beta", true),
                cached_gallery_choice("gamma", "Gamma", true),
            ],
        );
        let theme = theme::theme_by_slug("tokyo-night").expect("theme");
        let backend = TestBackend::new(160, 40);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let mut button_map = ButtonRenderMap::new();

        terminal.draw(|frame| draw_theme_builder(frame, &state, &mut button_map, theme)).expect("draw");

        assert_eq!(state.preset_visible_columns.get(), 2);
    }

    #[test]
    fn gallery_keyboard_navigation_uses_rendered_two_column_geometry() {
        let mut state = ThemeBuilderState::theme_gallery_from_active_theme_with_library(
            theme::theme_by_slug("tokyo-night").expect("theme"),
            0,
            vec![
                cached_gallery_choice("alpha", "Alpha", true),
                cached_gallery_choice("beta", "Beta", true),
                cached_gallery_choice("gamma", "Gamma", true),
                cached_gallery_choice("delta", "Delta", true),
            ],
        );
        let theme = theme::theme_by_slug("tokyo-night").expect("theme");
        let backend = TestBackend::new(160, 40);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let mut button_map = ButtonRenderMap::new();

        terminal.draw(|frame| draw_theme_builder(frame, &state, &mut button_map, theme)).expect("draw");
        assert_eq!(state.preset_visible_columns.get(), 2);

        assert_eq!(handle_theme_builder_key(&mut state, key(KeyCode::Right)), ThemeBuilderAction::None);
        assert_eq!(state.preset_cursor, 1);
        assert_eq!(handle_theme_builder_key(&mut state, key(KeyCode::Down)), ThemeBuilderAction::None);
        assert_eq!(state.preset_cursor, 3);
        assert_eq!(handle_theme_builder_key(&mut state, key(KeyCode::Left)), ThemeBuilderAction::None);
        assert_eq!(state.preset_cursor, 2);
        assert_eq!(handle_theme_builder_key(&mut state, key(KeyCode::Up)), ThemeBuilderAction::None);
        assert_eq!(state.preset_cursor, 0);
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
                cell.symbol().chars().any(|ch| ch == '\u{2588}')
                    && (cell.fg == *expected || cell.bg == *expected)
            })
        }).count()
    }

    #[test]
    fn preview_lines_expand_when_card_height_has_room() {
        let state = ThemeBuilderState::from_palette(ThemePaletteDraft::from_palette(theme::default_palette()));
        let theme = theme::theme_by_slug("tokyo-night").expect("theme");

        let compact = preview_lines(&state, theme, 4);
        let roomy = preview_lines(&state, theme, 10);

        assert_eq!(compact.len(), 4);
        assert!(roomy.len() > compact.len());
        assert!(roomy.iter().any(|line| line.spans.iter().any(|span| span.content.contains("progress"))));
        assert!(roomy.iter().any(|line| line.spans.iter().any(|span| span.content.contains("derived"))));
    }

    #[test]
    fn gallery_renderer_uses_injected_cached_library_preview_data() {
        let choice = cached_gallery_choice("cached-gallery-only", "Cached Gallery Only", true);
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
        assert_eq!(rendered_color_count(buffer, &expected_accents[..10]), 10);
    }

    #[test]
    fn delete_confirmation_mouse_cancel_does_not_delete() {
        let mut state = ThemeBuilderState::from_palette(ThemePaletteDraft::from_palette(theme::default_palette()));
        state.palette.slug = "missing-theme".to_string();
        state.palette.source = ThemeDraftSource::Custom;
        state.overlay = BuilderOverlay::DeleteConfirm;

        assert_eq!(
            handle_theme_builder_mouse(&mut state, left_click(), Some(TuiButton::ThemeBuilderDeleteCancel)),
            ThemeBuilderAction::None,
        );
        assert_eq!(state.overlay, BuilderOverlay::None);
        assert!(state.deleted_theme_slug.is_none());
    }
}
