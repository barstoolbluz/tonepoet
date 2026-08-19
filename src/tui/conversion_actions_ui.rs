//! Conversion-actions wizard and explicit `:actions-run` dry-run/apply overlay.
//!
//! The UI owns only editable configuration and presentation. Planning and
//! execution always go through `convert::pipeline::actions`, so preview and
//! apply cannot drift semantically.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame,
};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::convert::pipeline::actions::{
    acquire_explicit_action_run_lock_for_album, ExplicitActionRunLock,
    ExplicitPreviewProgressObserver, ManualInvocationState, NoExplicitPreviewProgress,
};
use crate::convert::pipeline::{
    describe_plan, ActionCancellation,
    ActionContext, ActionEngine, ActionPhase,
    ActionPhaseReport, ActionPipeline, ConversionAction, CopyAction, CreateFolderAction,
    DeleteAction, MoveAction, CapabilityActionFilesystem, ProcessGroupScriptRunner,
    RenameAction, RenameMode, RunScriptAction, TargetSpec,
};
use crate::tui::app::AppState;
use crate::tui::button_map::{ButtonRenderMap, TuiButton};
use crate::tui::message::AppMessage;


fn cell_width(value: &str) -> u16 {
    super::display_width::width(value).min(u16::MAX as usize) as u16
}

fn truncate_to_cells(value: &str, max_width: u16) -> String {
    super::display_width::truncate_right(value, max_width as usize)
}

fn pad_to_cells(value: String, width: u16) -> String {
    super::display_width::pad_or_truncate(&value, width as usize, false)
}

fn fit_to_cells(value: &str, width: u16) -> String {
    pad_to_cells(truncate_to_cells(value, width), width)
}

fn wrap_to_visual_rows(value: &str, width: u16) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let mut rows = Vec::new();
    for raw in value.split('\n') {
        if raw.is_empty() {
            rows.push(String::new());
            continue;
        }
        let mut current = String::new();
        let mut used = 0u16;
        for ch in raw.chars() {
            let mut buf = [0u8; 4];
            let rendered = ch.encode_utf8(&mut buf);
            let width_for_char = cell_width(rendered);
            if used > 0 && used.saturating_add(width_for_char) > width {
                rows.push(fit_to_cells(&current, width));
                current.clear();
                used = 0;
            }
            if width_for_char > width {
                rows.push(truncate_to_cells(rendered, width));
                continue;
            }
            current.push(ch);
            used = used.saturating_add(width_for_char);
        }
        rows.push(fit_to_cells(&current, width));
    }
    if rows.is_empty() {
        rows.push(String::new());
    }
    rows
}

fn preview_visual_rows_for_width(state: &ConversionActionsWizardState, width: u16) -> Vec<String> {
    let mut rows = Vec::new();
    for line in &state.preview_lines {
        rows.extend(wrap_to_visual_rows(&format!("  {line}"), width));
    }
    if rows.is_empty() {
        rows.push(String::new());
    }
    rows
}

fn clamp_preview_scroll(scroll: usize, state: &ConversionActionsWizardState, width: u16, visible: usize) -> usize {
    if visible == 0 {
        return 0;
    }
    let rows = preview_visual_rows_for_width(state, width);
    scroll.min(rows.len().saturating_sub(visible))
}

const DEFAULT_PREVIEW_SCROLL_WIDTH: u16 = 80;
const DEFAULT_PREVIEW_SCROLL_VISIBLE_ROWS: usize = 3;

fn preview_scroll_geometry(preview_rect: Option<Rect>) -> (u16, usize) {
    preview_rect
        .map(|rect| {
            (
                rect.width.max(1),
                rect.height.saturating_sub(2).max(1) as usize,
            )
        })
        .unwrap_or((DEFAULT_PREVIEW_SCROLL_WIDTH, DEFAULT_PREVIEW_SCROLL_VISIBLE_ROWS))
}

fn current_preview_scroll(state: &ConversionActionsWizardState) -> usize {
    match &state.dialog {
        ActionsWizardDialog::Configure(session) => session.preview_scroll,
        ActionsWizardDialog::Pipeline => 0,
    }
}

fn set_preview_scroll(state: &mut ConversionActionsWizardState, scroll: usize) {
    if let ActionsWizardDialog::Configure(session) = &mut state.dialog {
        session.preview_scroll = scroll;
    }
}

fn clamped_preview_scroll_for_rect(
    state: &ConversionActionsWizardState,
    scroll: usize,
    preview_rect: Option<Rect>,
) -> usize {
    let (width, visible) = preview_scroll_geometry(preview_rect);
    clamp_preview_scroll(scroll, state, width, visible)
}

fn clamp_preview_scroll_for_rect(state: &mut ConversionActionsWizardState, preview_rect: Option<Rect>) {
    let current = current_preview_scroll(state);
    let clamped = clamped_preview_scroll_for_rect(state, current, preview_rect);
    set_preview_scroll(state, clamped);
}

fn scroll_preview_by_delta(
    state: &mut ConversionActionsWizardState,
    delta: isize,
    preview_rect: Option<Rect>,
) {
    let current = clamped_preview_scroll_for_rect(state, current_preview_scroll(state), preview_rect);
    let next = if delta < 0 {
        current.saturating_sub(delta_magnitude(delta))
    } else {
        current.saturating_add(delta as usize)
    };
    let clamped = clamped_preview_scroll_for_rect(state, next, preview_rect);
    set_preview_scroll(state, clamped);
}

pub fn clamp_wizard_preview_scroll_for_rect(
    state: &mut ConversionActionsWizardState,
    preview_rect: Option<Rect>,
) {
    clamp_preview_scroll_for_rect(state, preview_rect);
}

fn scroll_offset_for_delta(current: usize, total: usize, visible: usize, delta: isize) -> usize {
    if total <= visible || visible == 0 {
        return 0;
    }
    let max_scroll = total.saturating_sub(visible);
    if delta < 0 {
        current.saturating_sub(delta_magnitude(delta)).min(max_scroll)
    } else {
        current.saturating_add(delta as usize).min(max_scroll)
    }
}

fn clamp_scroll_to_view(current: usize, total: usize, visible: usize) -> usize {
    if total <= visible || visible == 0 {
        0
    } else {
        current.min(total.saturating_sub(visible))
    }
}

fn scroll_to_include(current: usize, selected: usize, total: usize, visible: usize) -> usize {
    if total <= visible || visible == 0 {
        return 0;
    }
    let mut scroll = clamp_scroll_to_view(current, total, visible);
    if selected < scroll {
        scroll = selected;
    } else if selected >= scroll.saturating_add(visible) {
        scroll = selected.saturating_add(1).saturating_sub(visible);
    }
    clamp_scroll_to_view(scroll, total, visible)
}

const ACTION_KINDS: [&str; 6] = [
    "rename",
    "copy",
    "move",
    "delete",
    "create folder",
    "run script",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionsWizardFocus {
    /// Header radio that controls the phase used for newly added actions.
    Phase,
    Available,
    Pipeline,
    /// Dialog-B field list.
    Config,
    /// Dialog-B dry-run preview.
    Preview,
}

#[derive(Debug, Clone)]
pub struct ActionConfigSession {
    pub phase: ActionPhase,
    pub index: usize,
    pub original: Option<ConversionAction>,
    pub fresh: bool,
    pub preview_scroll: usize,
}


#[derive(Debug, Clone)]
pub struct ActionConfigEdit {
    pub field_index: usize,
    pub input: crate::tui::text_input::TextInputState,
}

#[derive(Debug, Clone)]
pub enum ActionsWizardDialog {
    Pipeline,
    Configure(ActionConfigSession),
}

#[derive(Debug, Clone)]
pub struct ConversionActionsWizardState {
    pub draft: ActionPipeline,
    /// Phase used when adding a new action from the Available pane.
    pub phase: ActionPhase,
    /// Phase of the selected pipeline row. Reordering is intentionally scoped
    /// to this phase; `m` explicitly moves an action across the pre/post boundary.
    pub pipeline_phase: ActionPhase,
    pub focus: ActionsWizardFocus,
    pub available_index: usize,
    pub available_scroll: usize,
    pub pipeline_index: usize,
    pub pipeline_scroll: usize,
    pub config_index: usize,
    pub config_scroll: usize,
    pub edit_input: Option<ActionConfigEdit>,
    pub preview_lines: Vec<String>,
    pub preview_notice: String,
    pub preview_operation_count: usize,
    pub preview_match_count: Option<usize>,
    /// True only when Dialog B's per-action preview no longer corresponds to
    /// the current pending configuration. Pure navigation, scrolling, focus
    /// changes, and Dialog A selection changes must leave this false so the
    /// input path does not perform synchronous preview filesystem I/O.
    pub preview_dirty: bool,
    /// True only after the real action planner has accepted the currently
    /// configured Dialog B action. Planner success is sufficient to apply,
    /// but it is not required when the preview context itself is unavailable.
    pub preview_valid: bool,
    /// True when no contextual dry-run can be constructed for the current
    /// action because there is no selected source or the preview sandbox cannot
    /// be built. This is distinct from planner rejection: a locally valid
    /// configuration can still be applied and saved as a default.
    pub preview_unavailable: bool,
    /// True when a real planner preview was attempted and rejected the current
    /// action. This is a semantic failure and blocks Apply.
    pub preview_planner_failed: bool,
    pub dialog: ActionsWizardDialog,
}

impl ConversionActionsWizardState {
    pub fn new(draft: ActionPipeline) -> Self {
        let mut state = Self {
            draft,
            phase: ActionPhase::Post,
            pipeline_phase: ActionPhase::Post,
            focus: ActionsWizardFocus::Available,
            available_index: 0,
            available_scroll: 0,
            pipeline_index: 0,
            pipeline_scroll: 0,
            config_index: 0,
            config_scroll: 0,
            edit_input: None,
            preview_lines: Vec::new(),
            preview_notice: "Preview simulates the selected conversion source and its planned destination; scripts are never executed.".to_string(),
            preview_operation_count: 0,
            preview_match_count: None,
            preview_dirty: false,
            preview_valid: false,
            preview_unavailable: false,
            preview_planner_failed: false,
            dialog: ActionsWizardDialog::Pipeline,
        };
        state.clamp();
        state.refresh_summary_preview();
        state
    }

    pub fn actions(&self) -> &[ConversionAction] {
        self.draft.for_phase(self.pipeline_phase)
    }

    fn actions_mut(&mut self) -> &mut Vec<ConversionAction> {
        self.draft.for_phase_mut(self.pipeline_phase)
    }

    fn actions_for(&self, phase: ActionPhase) -> &[ConversionAction] {
        self.draft.for_phase(phase)
    }

    fn actions_for_mut(&mut self, phase: ActionPhase) -> &mut Vec<ConversionAction> {
        self.draft.for_phase_mut(phase)
    }

    fn selected_action(&self) -> Option<&ConversionAction> {
        self.actions().get(self.pipeline_index)
    }

    fn configured_target(&self) -> Option<(ActionPhase, usize)> {
        match &self.dialog {
            ActionsWizardDialog::Configure(session) => Some((session.phase, session.index)),
            ActionsWizardDialog::Pipeline => None,
        }
    }

    fn configured_action(&self) -> Option<&ConversionAction> {
        let (phase, index) = self.configured_target()?;
        self.actions_for(phase).get(index)
    }

    fn configured_action_mut(&mut self) -> Option<&mut ConversionAction> {
        let (phase, index) = self.configured_target()?;
        self.actions_for_mut(phase).get_mut(index)
    }

    fn preview_phase(&self) -> ActionPhase {
        self.configured_target()
            .map(|(phase, _)| phase)
            .unwrap_or(self.pipeline_phase)
    }

    fn clamp(&mut self) {
        self.available_index = self.available_index.min(ACTION_KINDS.len().saturating_sub(1));
        self.available_scroll = self.available_scroll.min(ACTION_KINDS.len().saturating_sub(1));
        let selected_len = self.actions().len();
        if selected_len == 0 {
            let alternate = match self.pipeline_phase {
                ActionPhase::Pre => ActionPhase::Post,
                ActionPhase::Post => ActionPhase::Pre,
            };
            if !self.actions_for(alternate).is_empty() {
                self.pipeline_phase = alternate;
            }
        }
        self.pipeline_index = self.pipeline_index.min(self.actions().len().saturating_sub(1));
        self.pipeline_scroll = self.pipeline_scroll.min(pipeline_visual_rows(self).len().saturating_sub(1));
        let field_count = self
            .configured_action()
            .or_else(|| self.selected_action())
            .map(action_fields)
            .map(|fields| fields.len())
            .unwrap_or(0);
        self.config_index = self.config_index.min(field_count.saturating_sub(1));
        self.config_scroll = self.config_scroll.min(field_count.saturating_sub(1));
        if let ActionsWizardDialog::Configure(session) = &self.dialog {
            if session.index >= self.actions_for(session.phase).len() {
                self.dialog = ActionsWizardDialog::Pipeline;
                self.focus = ActionsWizardFocus::Pipeline;
            }
        }
    }

    fn refresh_summary_preview(&mut self) {
        let mut lines = Vec::new();
        lines.push("Pre-conversion".to_string());
        if self.draft.pre.is_empty() {
            lines.push("  (none yet)".to_string());
        } else {
            lines.extend(
                self.draft
                    .pre
                    .iter()
                    .enumerate()
                    .map(|(index, action)| format!("  {} {}", index + 1, action_summary(action))),
            );
        }
        lines.push("".to_string());
        lines.push("Post-conversion".to_string());
        if self.draft.post.is_empty() {
            lines.push("  (none yet)".to_string());
        } else {
            lines.extend(
                self.draft
                    .post
                    .iter()
                    .enumerate()
                    .map(|(index, action)| format!("  {} {}", index + 1, action_summary(action))),
            );
        }
        self.preview_lines = lines;
        self.preview_operation_count = 0;
        self.preview_match_count = None;
        self.preview_dirty = false;
        self.preview_valid = false;
        self.preview_unavailable = false;
        self.preview_planner_failed = false;
        if self.preview_lines.is_empty() {
            self.preview_lines.push("No actions configured.".to_string());
        }
    }

    fn mark_config_preview_dirty(&mut self) {
        if matches!(self.dialog, ActionsWizardDialog::Configure(_)) {
            self.preview_dirty = true;
            self.preview_valid = false;
            self.preview_unavailable = false;
            self.preview_planner_failed = false;
            self.preview_operation_count = 0;
            self.preview_match_count = None;
        }
    }
}

/// Rebuild the wizard dry-run pane through the real conversion output planner
/// and action planner. Post-action preview uses an isolated temporary mirror of
/// the selected source's planned audio outputs and companion copies. Pre-action
/// preview uses the selected conversion source itself. Browse navigation state
/// is never consulted for wizard identity or scope.
pub fn refresh_wizard_preview_for_app(
    state: &mut ConversionActionsWizardState,
    app: &AppState,
) {
    if !matches!(state.dialog, ActionsWizardDialog::Configure(_)) {
        state.preview_dirty = false;
        state.preview_valid = false;
        state.preview_unavailable = false;
        state.preview_planner_failed = false;
        state.refresh_summary_preview();
        return;
    }
    state.preview_dirty = false;
    state.preview_valid = false;
    state.preview_unavailable = false;
    state.preview_planner_failed = false;
    state.preview_operation_count = 0;
    state.preview_match_count = None;
    let source_path = match app.convert.source.mode.current_path() {
        Some(path) => path.clone(),
        None => {
            set_preview_unavailable(
                state,
                "Preview unavailable: no conversion source is selected".to_string(),
                "Select a conversion source to simulate actions.".to_string(),
            );
            return;
        }
    };
    let request = match wizard_preview_request(state, app, &source_path) {
        Ok(request) => request,
        Err(error) => {
            set_preview_unavailable(
                state,
                format!("Preview unavailable: {error}"),
                "Conversion destination simulation failed before planning.".to_string(),
            );
            return;
        }
    };

    if state.preview_phase() == ActionPhase::Pre {
        let context = match crate::convert::pipeline::stages::conversion_action_pre_preview_context(&request) {
            Ok(context) => context,
            Err(error) => {
                set_preview_unavailable(
                    state,
                    format!("Preview unavailable: {error}"),
                    "Could not construct a pre-action preview context.".to_string(),
                );
                return;
            }
        };
        render_action_preview(
            state,
            &context,
            None,
            format!(
                "Dry run against selected conversion source {}; scripts are not executed.",
                source_path.display()
            ),
        );
        return;
    }

    let layout = match crate::convert::pipeline::stages::conversion_action_destination_preview(
        &request,
        wizard_preview_track_metadata(app),
    ) {
        Ok(layout) => layout,
        Err(error) => {
            set_preview_unavailable(
                state,
                format!("Preview unavailable: {error}"),
                format!(
                    "Could not simulate the conversion destination for {}.",
                    source_path.display()
                ),
            );
            return;
        }
    };
    let temporary = match tempfile::tempdir() {
        Ok(temporary) => temporary,
        Err(error) => {
            set_preview_unavailable(
                state,
                format!("Preview unavailable: cannot create isolated simulation: {error}"),
                "Could not create the isolated preview directory.".to_string(),
            );
            return;
        }
    };
    let simulated_album = temporary.path().join("planned-album");
    if let Err(error) = materialize_preview_placeholders(&simulated_album, &layout.entries) {
        set_preview_unavailable(
            state,
            format!("Preview unavailable: {error}"),
            "Could not materialize the isolated preview directory.".to_string(),
        );
        return;
    }

    let mut protected_generated_paths = BTreeSet::new();
    protected_generated_paths.insert(simulated_album.join("conversion.log"));
    let context = ActionContext {
        run_identity: format!("wizard-preview-{}", Uuid::new_v4()),
        album_identity: format!("wizard-preview:{}", layout.album_dir.display()),
        phase: ActionPhase::Post,
        subject_dir: simulated_album.clone(),
        source_path: source_path.clone(),
        source_is_directory: source_path.is_dir(),
        output_root: temporary.path().to_path_buf(),
        album_dir: simulated_album.clone(),
        environment_album_dir: None,
        retained_album_capability: None,
        retained_output_capability: None,
        retained_journal_capability: None,
        coordination_io_dir: None,
        protected_sources: BTreeSet::from([source_path.clone()]),
        protected_generated_paths,
        album_tokens: layout.album_tokens,
        disc_count: layout.disc_count,
        journal_dir: temporary.path().join(".tonepoet-action-preview-journals"),
        batch_source_scope_root: None,
        explicit_scope: true,
        semantics: ui_action_semantics(),
    };
    let omitted = if layout.omitted_other_album_roots == 0 {
        String::new()
    } else {
        format!(
            " {} additional rendered album root(s) are outside the primary post-action scope.",
            layout.omitted_other_album_roots
        )
    };
    render_action_preview(
        state,
        &context,
        Some((&simulated_album, &layout.album_dir)),
        format!(
            "Simulated destination for {}: {} planned audio file(s), {} planned companion path(s).{} Scripts are not executed.",
            source_path.display(),
            layout.audio_count,
            layout.companion_count,
            omitted
        ),
    );
}


fn set_preview_unavailable(
    state: &mut ConversionActionsWizardState,
    line: String,
    notice: String,
) {
    state.preview_lines = vec![line];
    state.preview_notice = notice;
    state.preview_operation_count = 0;
    state.preview_match_count = None;
    state.preview_valid = false;
    state.preview_unavailable = true;
    state.preview_planner_failed = false;
}

fn wizard_preview_request(
    state: &ConversionActionsWizardState,
    app: &AppState,
    source_path: &Path,
) -> Result<crate::convert::pipeline::PipelineRequest, String> {
    let format = crate::convert::FormatDetector::detect(source_path)
        .map_err(|error| error.to_string())?;
    let mut options = crate::tui::convert_actions::pills_to_options(
        &app.convert.format,
        &app.convert.output_options,
        &app.config,
    );
    app.convert
        .output_options
        .apply_companion_copying_to_conversion_options(&mut options);
    options.album_artist_override = app
        .convert
        .metadata
        .album_artist_for_conversion
        .as_ref()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    options.actions = state.draft.clone();
    let item = crate::convert::ConversionItem::new(source_path.to_path_buf(), format, options);
    crate::convert::processor::build_pipeline_request(&item).map_err(|error| error.to_string())
}

fn wizard_preview_track_metadata(
    app: &AppState,
) -> Vec<crate::convert::pipeline::TrackMetadata> {
    let mut base = crate::convert::pipeline::TrackMetadata {
        title: app.convert.metadata.title.clone(),
        artist: app.convert.metadata.artist.clone().into(),
        album_artist: app.convert.metadata.album_artist_for_conversion.clone().into(),
        genre: app.convert.metadata.genre.clone().into(),
        date: app.convert.metadata.year.clone(),
        ..crate::convert::pipeline::TrackMetadata::default()
    };
    if let Some(album) = app.convert.metadata.album.clone() {
        base.extra.insert("album".to_string(), album);
    }
    match &app.convert.source.mode {
        crate::tui::app::SourceMode::MultiTrack {
            tracks,
            selected,
            album_title,
            album_artist,
            ..
        } => tracks
            .iter()
            .zip(selected.iter().copied().chain(std::iter::repeat(true)))
            .filter(|(_, selected)| *selected)
            .map(|(track, _)| {
                let mut metadata = base.clone();
                metadata.track_number = Some(track.number);
                metadata.title = track.title.clone().or(metadata.title);
                if let Some(performer) = track.performer.clone() {
                    metadata.artist = Some(performer).into();
                }
                if let Some(album_artist) = album_artist.clone() {
                    metadata.album_artist = Some(album_artist).into();
                }
                if let Some(album) = album_title.clone() {
                    metadata.extra.insert("album".to_string(), album);
                }
                metadata
            })
            .collect(),
        _ => vec![base],
    }
}

fn materialize_preview_placeholders(
    root: &Path,
    entries: &[(PathBuf, bool)],
) -> Result<(), String> {
    std::fs::create_dir_all(root)
        .map_err(|error| format!("cannot create simulated album root {}: {error}", root.display()))?;
    for (relative, is_directory) in entries {
        if relative.is_absolute()
            || relative
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir | std::path::Component::RootDir | std::path::Component::Prefix(_)))
        {
            return Err(format!("planner returned unsafe preview path {}", relative.display()));
        }
        let path = root.join(relative);
        if *is_directory {
            std::fs::create_dir_all(&path)
                .map_err(|error| format!("cannot create preview directory {}: {error}", path.display()))?;
        } else {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|error| format!("cannot create preview parent {}: {error}", parent.display()))?;
            }
            std::fs::File::create(&path)
                .map_err(|error| format!("cannot create preview file {}: {error}", path.display()))?;
        }
    }
    Ok(())
}


fn effective_preview_pipeline(
    state: &ConversionActionsWizardState,
) -> Result<(ActionPipeline, Option<ConversionAction>), String> {
    let Some((phase, index)) = state.configured_target() else {
        return Ok((state.draft.clone(), None));
    };
    let Some(mut action) = state.actions_for(phase).get(index).cloned() else {
        return Ok((ActionPipeline::default(), None));
    };
    if let Some(edit) = state.edit_input.as_ref() {
        set_action_field_text(&mut action, edit.field_index, &edit.input.text)?;
    }

    let mut pipeline = ActionPipeline::default();
    pipeline.for_phase_mut(phase).push(action.clone());
    Ok((pipeline, Some(action)))
}

fn commit_pending_config_edit(state: &mut ConversionActionsWizardState) -> Result<(), String> {
    let Some(edit) = state.edit_input.take() else {
        return Ok(());
    };
    let value = edit.input.text.clone();
    let field_index = edit.field_index;
    if let Some(action) = state.configured_action_mut() {
        if let Err(error) = set_action_field_text(action, field_index, &value) {
            state.edit_input = Some(edit);
            return Err(error);
        }
    }
    state.config_index = field_index;
    state.mark_config_preview_dirty();
    Ok(())
}

fn commit_pending_config_edit_or_notice(state: &mut ConversionActionsWizardState) -> bool {
    match commit_pending_config_edit(state) {
        Ok(()) => true,
        Err(error) => {
            state.preview_dirty = false;
            state.preview_valid = false;
            state.preview_unavailable = false;
            state.preview_planner_failed = true;
            state.preview_lines = vec![format!("Planning failed: {error}")];
            state.preview_notice = error;
            false
        }
    }
}

fn render_action_preview(
    state: &mut ConversionActionsWizardState,
    context: &ActionContext,
    path_translation: Option<(&Path, &Path)>,
    notice: String,
) {
    let filesystem = CapabilityActionFilesystem::new();
    let scripts = ProcessGroupScriptRunner;
    let engine = ActionEngine {
        filesystem: &filesystem,
        scripts: &scripts,
    };
    let (preview_pipeline, configured_action) = match effective_preview_pipeline(state) {
        Ok(inputs) => inputs,
        Err(error) => {
            state.preview_lines = vec![format!("Planning failed: {error}")];
            state.preview_notice = notice;
            state.preview_operation_count = 0;
            state.preview_match_count = None;
            state.preview_valid = false;
            state.preview_unavailable = false;
            state.preview_planner_failed = true;
            return;
        }
    };
    let match_count = configured_action
        .as_ref()
        .and_then(|action| preview_target_match_count(context, action));
    match engine.preview_phase(&preview_pipeline, context) {
        Ok(plans) => {
            state.preview_lines.clear();
            state.preview_valid = true;
            state.preview_unavailable = false;
            state.preview_planner_failed = false;
            state.preview_operation_count = plans
                .iter()
                .map(|plan| plan.operations.len())
                .sum();
            state.preview_match_count = match_count;

            let single_action_preview = configured_action.is_some();
            for (index, plan) in plans.iter().enumerate() {
                if !single_action_preview {
                    state.preview_lines.push(format!("{}. {}", index + 1, plan.action_kind));
                }
                state.preview_lines.extend(describe_plan(plan).into_iter().map(|line| {
                    translate_preview_line(line, path_translation)
                }));
            }
            if state.preview_lines.is_empty() {
                state.preview_lines.push("No operations planned.".to_string());
            }
            state.preview_notice = notice;
        }
        Err(error) => {
            state.preview_lines = vec![format!("Planning failed: {error}")];
            state.preview_notice = notice;
            state.preview_operation_count = 0;
            state.preview_match_count = match_count;
            state.preview_valid = false;
            state.preview_unavailable = false;
            state.preview_planner_failed = true;
        }
    }
}

fn translate_preview_line(line: String, path_translation: Option<(&Path, &Path)>) -> String {
    if let Some((from, to)) = path_translation {
        let from = from.to_string_lossy();
        let to = to.to_string_lossy();
        line.replace(from.as_ref(), to.as_ref())
    } else {
        line
    }
}

fn preview_target_match_count(context: &ActionContext, action: &ConversionAction) -> Option<usize> {
    let targeting = action_targeting(action)?;
    if targeting.target.is_empty() {
        return Some(0);
    }
    let candidates = collect_preview_match_candidates(context).ok()?;
    let mut count = 0usize;
    for candidate in candidates {
        if context.protected_generated_paths.contains(&candidate) {
            continue;
        }
        if !targeting.allow_sources && context.protected_sources.contains(&candidate) {
            continue;
        }
        if target_spec_matches_preview_path(&context.subject_dir, targeting, &candidate)? {
            count += 1;
        }
    }
    Some(count)
}

fn action_targeting(action: &ConversionAction) -> Option<&TargetSpec> {
    match action {
        ConversionAction::Rename(action) => Some(&action.targeting),
        ConversionAction::Copy(action) => Some(&action.targeting),
        ConversionAction::Move(action) => Some(&action.targeting),
        ConversionAction::Delete(action) => Some(&action.targeting),
        ConversionAction::CreateFolder(_) | ConversionAction::Runscript(_) => None,
    }
}

fn collect_preview_match_candidates(context: &ActionContext) -> std::io::Result<Vec<PathBuf>> {
    let root = &context.subject_dir;
    if root.is_file() {
        return Ok(vec![root.clone()]);
    }
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    collect_preview_match_candidates_from_dir(root, &mut out)?;
    Ok(out)
}

fn collect_preview_match_candidates_from_dir(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_preview_match_candidates_from_dir(&path, out)?;
        } else if file_type.is_file() {
            out.push(path);
        }
    }
    Ok(())
}

fn target_spec_matches_preview_path(
    root: &Path,
    targeting: &TargetSpec,
    path: &Path,
) -> Option<bool> {
    let rel = path.strip_prefix(root).unwrap_or(path);
    let rel = normalize_preview_match_path(rel)?;
    let file_name = path.file_name()?.to_str()?;
    let target_match = preview_patterns_match(&targeting.target, &rel, file_name)?;
    let excluded = preview_patterns_match(&targeting.exclude, &rel, file_name)?;
    Some(target_match && !excluded)
}

fn normalize_preview_match_path(path: &Path) -> Option<String> {
    let mut parts = Vec::new();
    for component in path.components() {
        let std::path::Component::Normal(part) = component else {
            return None;
        };
        parts.push(part.to_str()?);
    }
    Some(parts.join("/"))
}

fn preview_patterns_match(patterns: &[String], rel_path: &str, file_name: &str) -> Option<bool> {
    for pattern in patterns {
        let pattern = pattern.trim();
        if pattern.is_empty() {
            continue;
        }
        if !preview_glob_supported(pattern) {
            return None;
        }
        let normalized = pattern.replace('\\', "/");
        let subject = if normalized.contains('/') { rel_path } else { file_name };
        if preview_glob_matches(&normalized, subject) {
            return Some(true);
        }
    }
    Some(false)
}

fn preview_glob_supported(pattern: &str) -> bool {
    !pattern.chars().any(|ch| matches!(ch, '[' | ']' | '{' | '}'))
}

fn preview_glob_matches(pattern: &str, value: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let value: Vec<char> = value.chars().collect();
    let mut pattern_index = 0usize;
    let mut value_index = 0usize;
    let mut star_index: Option<usize> = None;
    let mut star_value_index = 0usize;

    while value_index < value.len() {
        if pattern_index < pattern.len()
            && (pattern[pattern_index] == '?' || pattern[pattern_index] == value[value_index])
        {
            pattern_index += 1;
            value_index += 1;
        } else if pattern_index < pattern.len() && pattern[pattern_index] == '*' {
            star_index = Some(pattern_index);
            pattern_index += 1;
            star_value_index = value_index;
        } else if let Some(star) = star_index {
            pattern_index = star + 1;
            star_value_index += 1;
            value_index = star_value_index;
        } else {
            return false;
        }
    }

    while pattern_index < pattern.len() && pattern[pattern_index] == '*' {
        pattern_index += 1;
    }
    pattern_index == pattern.len()
}



#[derive(Debug, Clone)]
pub enum WizardKeyResult {
    Continue(ConversionActionsWizardState),
    /// Dialog B requested Apply. The caller must refresh the preview if dirty
    /// and then finalize only if the planner accepted the configured action.
    ValidateConfigApply(ConversionActionsWizardState),
    Commit(ActionPipeline),
    CommitDefault(ConversionActionsWizardState),
    Cancel,
}

pub fn handle_wizard_key(
    state: ConversionActionsWizardState,
    key: KeyEvent,
) -> WizardKeyResult {
    handle_wizard_key_with_preview_rect(state, key, None)
}

pub fn handle_wizard_key_with_preview_rect(
    mut state: ConversionActionsWizardState,
    key: KeyEvent,
    preview_rect: Option<Rect>,
) -> WizardKeyResult {
    if let Some(mut edit) = state.edit_input.take() {
        match key.code {
            KeyCode::Esc => {
                state.edit_input = None;
                cancel_action_config(&mut state);
            }
            KeyCode::Enter => {
                state.edit_input = Some(edit);
                if !commit_pending_config_edit_or_notice(&mut state) {
                    return WizardKeyResult::Continue(state);
                }
            }
            _ => {
                crate::tui::text_input::handle_text_input_key(&mut edit.input, &key);
                state.config_index = edit.field_index;
                state.edit_input = Some(edit);
                state.mark_config_preview_dirty();
            }
        }
        return WizardKeyResult::Continue(state);
    }

    let configuring = matches!(state.dialog, ActionsWizardDialog::Configure(_));
    match (key.code, key.modifiers) {
        (KeyCode::Esc, _) | (KeyCode::Char('q'), _) if configuring => {
            cancel_action_config(&mut state);
        }
        (KeyCode::Esc, _) | (KeyCode::Char('q'), _) => return WizardKeyResult::Cancel,
        (KeyCode::Char('s'), KeyModifiers::NONE) if !configuring => {
            return WizardKeyResult::Commit(state.draft)
        }
        (KeyCode::Char('S'), KeyModifiers::SHIFT) if !configuring => {
            return WizardKeyResult::CommitDefault(state)
        }
        (KeyCode::Char('a'), KeyModifiers::NONE) if configuring => {
            return request_action_config_apply(state);
        }
        (KeyCode::Enter, _) if configuring && state.focus == ActionsWizardFocus::Preview => {
            return request_action_config_apply(state);
        }
        (KeyCode::Tab, _) => {
            state.focus = if configuring {
                match state.focus {
                    ActionsWizardFocus::Config => ActionsWizardFocus::Preview,
                    ActionsWizardFocus::Preview => ActionsWizardFocus::Config,
                    _ => ActionsWizardFocus::Config,
                }
            } else {
                match state.focus {
                    ActionsWizardFocus::Phase => ActionsWizardFocus::Available,
                    ActionsWizardFocus::Available => ActionsWizardFocus::Pipeline,
                    ActionsWizardFocus::Pipeline => ActionsWizardFocus::Phase,
                    ActionsWizardFocus::Config | ActionsWizardFocus::Preview => ActionsWizardFocus::Available,
                }
            };
        }
        (KeyCode::BackTab, _) => {
            state.focus = if configuring {
                match state.focus {
                    ActionsWizardFocus::Config => ActionsWizardFocus::Preview,
                    ActionsWizardFocus::Preview => ActionsWizardFocus::Config,
                    _ => ActionsWizardFocus::Config,
                }
            } else {
                match state.focus {
                    ActionsWizardFocus::Phase => ActionsWizardFocus::Pipeline,
                    ActionsWizardFocus::Available => ActionsWizardFocus::Phase,
                    ActionsWizardFocus::Pipeline => ActionsWizardFocus::Available,
                    ActionsWizardFocus::Config | ActionsWizardFocus::Preview => ActionsWizardFocus::Pipeline,
                }
            };
        }
        (KeyCode::Left, _) | (KeyCode::Right, _)
            if !configuring && state.focus == ActionsWizardFocus::Phase =>
        {
            toggle_add_phase(&mut state);
        }
        (KeyCode::Char('t'), _) if !configuring => toggle_add_phase(&mut state),
        (KeyCode::Up, _) | (KeyCode::Char('k'), _) => move_focus_cursor(&mut state, -1, preview_rect),
        (KeyCode::Down, _) | (KeyCode::Char('j'), _) => move_focus_cursor(&mut state, 1, preview_rect),
        (KeyCode::Enter, _) if !configuring && state.focus == ActionsWizardFocus::Available => {
            add_selected_action(&mut state);
        }
        (KeyCode::Enter, _) | (KeyCode::Char(' '), _)
            if !configuring && state.focus == ActionsWizardFocus::Pipeline =>
        {
            open_config_for_selected(&mut state, false);
        }
        (KeyCode::Delete, _) | (KeyCode::Char('d'), _)
            if !configuring && state.focus == ActionsWizardFocus::Pipeline =>
        {
            remove_selected_pipeline_action(&mut state);
        }
        (KeyCode::Char('['), _) if !configuring && state.focus == ActionsWizardFocus::Pipeline => {
            reorder_selected_pipeline_action(&mut state, -1);
        }
        (KeyCode::Char(']'), _) if !configuring && state.focus == ActionsWizardFocus::Pipeline => {
            reorder_selected_pipeline_action(&mut state, 1);
        }
        (KeyCode::Char('m'), _) if !configuring && state.focus == ActionsWizardFocus::Pipeline => {
            move_selected_action_between_phases(&mut state);
        }
        (KeyCode::Enter, _) | (KeyCode::Char(' '), _)
            if configuring && state.focus == ActionsWizardFocus::Config =>
        {
            edit_config_field(&mut state);
        }
        _ => {}
    }
    state.clamp();
    WizardKeyResult::Continue(state)
}

fn toggle_add_phase(state: &mut ConversionActionsWizardState) {
    state.phase = match state.phase {
        ActionPhase::Pre => ActionPhase::Post,
        ActionPhase::Post => ActionPhase::Pre,
    };
    state.refresh_summary_preview();
}

fn add_selected_action(state: &mut ConversionActionsWizardState) {
    if let Some(action) = default_action_for_kind(state.available_index, state.phase) {
        let phase = state.phase;
        state.actions_for_mut(phase).push(action);
        state.pipeline_phase = phase;
        state.pipeline_index = state.actions_for(phase).len().saturating_sub(1);
        state.config_index = 0;
        state.config_scroll = 0;
        open_config_for_selected(state, true);
    } else {
        state.preview_notice = "No available action is selected.".to_string();
    }
}

fn open_config_for_selected(state: &mut ConversionActionsWizardState, fresh: bool) {
    let phase = state.pipeline_phase;
    let index = state.pipeline_index;
    let Some(original) = state.actions_for(phase).get(index).cloned() else {
        return;
    };
    state.edit_input = None;
    state.dialog = ActionsWizardDialog::Configure(ActionConfigSession {
        phase,
        index,
        original: (!fresh).then_some(original),
        fresh,
        preview_scroll: 0,
    });
    state.focus = ActionsWizardFocus::Config;
    state.config_index = 0;
    state.config_scroll = 0;
    state.preview_lines = vec!["Preview pending.".to_string()];
    state.preview_notice = "Dry-run preview will update when context is available; locally valid defaults can still be applied without a selected source.".to_string();
    state.mark_config_preview_dirty();
}

fn request_action_config_apply(mut state: ConversionActionsWizardState) -> WizardKeyResult {
    if !commit_pending_config_edit_or_notice(&mut state) {
        return WizardKeyResult::Continue(state);
    }
    if let Some(action) = state.configured_action() {
        if let Err(error) = validate_action_config(action) {
            state.preview_notice = format!("Cannot apply: {error}");
            state.preview_dirty = false;
            state.preview_valid = false;
            state.preview_unavailable = false;
            state.preview_planner_failed = true;
            state.preview_lines = vec![format!("Planning failed: {error}")];
            return WizardKeyResult::Continue(state);
        }
    }
    if state.preview_dirty {
        return WizardKeyResult::ValidateConfigApply(state);
    }
    finalize_action_config_apply_after_preview(state)
}

pub fn finalize_action_config_apply_after_preview(
    mut state: ConversionActionsWizardState,
) -> WizardKeyResult {
    if state.preview_dirty {
        state.preview_notice = "Cannot apply until the dry-run preview has been refreshed.".to_string();
        return WizardKeyResult::Continue(state);
    }
    if !state.preview_valid && !state.preview_unavailable {
        if !state
            .preview_lines
            .iter()
            .any(|line| line.starts_with("Planning failed:"))
        {
            state.preview_lines = vec!["Planning failed: preview has not accepted this configuration.".to_string()];
        }
        state.preview_notice = if state.preview_planner_failed {
            "Cannot apply: fix the configuration until the planner preview succeeds.".to_string()
        } else {
            "Cannot apply: preview has not accepted or ruled out this configuration.".to_string()
        };
        return WizardKeyResult::Continue(state);
    }
    state.dialog = ActionsWizardDialog::Pipeline;
    state.focus = ActionsWizardFocus::Pipeline;
    state.preview_dirty = false;
    state.preview_valid = false;
    state.preview_unavailable = false;
    state.preview_planner_failed = false;
    state.refresh_summary_preview();
    WizardKeyResult::Continue(state)
}

fn cancel_action_config(state: &mut ConversionActionsWizardState) {
    let ActionsWizardDialog::Configure(session) = state.dialog.clone() else {
        return;
    };
    state.edit_input = None;
    if session.fresh {
        if session.index < state.actions_for(session.phase).len() {
            state.actions_for_mut(session.phase).remove(session.index);
        }
        state.pipeline_phase = session.phase;
        state.pipeline_index = state.pipeline_index.saturating_sub(1);
    } else if let Some(original) = session.original {
        if let Some(slot) = state.actions_for_mut(session.phase).get_mut(session.index) {
            *slot = original;
        }
        state.pipeline_phase = session.phase;
        state.pipeline_index = session.index;
    }
    state.dialog = ActionsWizardDialog::Pipeline;
    state.focus = ActionsWizardFocus::Pipeline;
    state.refresh_summary_preview();
}

fn remove_selected_pipeline_action(state: &mut ConversionActionsWizardState) {
    let index = state.pipeline_index;
    if index < state.actions().len() {
        state.actions_mut().remove(index);
        state.clamp();
        state.refresh_summary_preview();
    }
}

fn reorder_selected_pipeline_action(state: &mut ConversionActionsWizardState, delta: isize) {
    let index = state.pipeline_index;
    if delta < 0 {
        if index > 0 {
            state.actions_mut().swap(index, index - 1);
            state.pipeline_index -= 1;
            state.refresh_summary_preview();
        }
    } else if index + 1 < state.actions().len() {
        state.actions_mut().swap(index, index + 1);
        state.pipeline_index += 1;
        state.refresh_summary_preview();
    }
}

fn move_selected_action_between_phases(state: &mut ConversionActionsWizardState) {
    let from = state.pipeline_phase;
    let index = state.pipeline_index;
    if index >= state.actions_for(from).len() {
        return;
    }
    let to = match from {
        ActionPhase::Pre => ActionPhase::Post,
        ActionPhase::Post => ActionPhase::Pre,
    };
    let action = state.actions_for_mut(from).remove(index);
    state.actions_for_mut(to).push(action);
    state.pipeline_phase = to;
    state.pipeline_index = state.actions_for(to).len().saturating_sub(1);
    state.pipeline_scroll = 0;
    state.refresh_summary_preview();
}

fn edit_config_field(state: &mut ConversionActionsWizardState) {
    let field_index = state.config_index;
    let edit = state
        .configured_action_mut()
        .map(|action| action_field_edit(action, field_index));
    match edit {
        Some(FieldEdit::Text(value)) => {
            state.edit_input = Some(ActionConfigEdit {
                field_index,
                input: crate::tui::text_input::TextInputState::new_selected(value),
            });
        }
        Some(FieldEdit::Changed) => {
            state.refresh_summary_preview();
            state.mark_config_preview_dirty();
        }
        Some(FieldEdit::Unavailable) | None => {}
    }
}

fn move_focus_cursor(state: &mut ConversionActionsWizardState, delta: isize, preview_rect: Option<Rect>) {
    match state.focus {
        ActionsWizardFocus::Available => {
            state.available_index = stepped_index(state.available_index, ACTION_KINDS.len(), delta);
        }
        ActionsWizardFocus::Pipeline => move_pipeline_selection(state, delta),
        ActionsWizardFocus::Config => {
            let count = state
                .configured_action()
                .map(action_fields)
                .map(|fields| fields.len())
                .unwrap_or(0);
            state.config_index = stepped_index(state.config_index, count, delta);
        }
        ActionsWizardFocus::Preview => {
            scroll_preview_by_delta(state, delta, preview_rect);
        }
        ActionsWizardFocus::Phase => {}
    }
}

fn move_pipeline_selection(state: &mut ConversionActionsWizardState, delta: isize) {
    let rows = pipeline_rows(state);
    if rows.is_empty() {
        state.pipeline_index = 0;
        return;
    }
    let current = rows
        .iter()
        .position(|(phase, index)| *phase == state.pipeline_phase && *index == state.pipeline_index)
        .unwrap_or(0);
    let next = stepped_index(current, rows.len(), delta);
    let (phase, index) = rows[next];
    state.pipeline_phase = phase;
    state.pipeline_index = index;
    state.config_index = 0;
    state.config_scroll = 0;
}

fn pipeline_rows(state: &ConversionActionsWizardState) -> Vec<(ActionPhase, usize)> {
    let mut rows = Vec::new();
    rows.extend((0..state.draft.pre.len()).map(|index| (ActionPhase::Pre, index)));
    rows.extend((0..state.draft.post.len()).map(|index| (ActionPhase::Post, index)));
    rows
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PipelineVisualRow {
    Header(ActionPhase),
    Empty(ActionPhase),
    Action(ActionPhase, usize),
    Spacer,
    Hint,
}

fn pipeline_visual_rows(state: &ConversionActionsWizardState) -> Vec<PipelineVisualRow> {
    let mut rows = Vec::new();
    rows.push(PipelineVisualRow::Header(ActionPhase::Pre));
    if state.draft.pre.is_empty() {
        rows.push(PipelineVisualRow::Empty(ActionPhase::Pre));
    } else {
        rows.extend((0..state.draft.pre.len()).map(|index| PipelineVisualRow::Action(ActionPhase::Pre, index)));
    }
    rows.push(PipelineVisualRow::Spacer);
    rows.push(PipelineVisualRow::Header(ActionPhase::Post));
    if state.draft.post.is_empty() {
        rows.push(PipelineVisualRow::Empty(ActionPhase::Post));
    } else {
        rows.extend((0..state.draft.post.len()).map(|index| PipelineVisualRow::Action(ActionPhase::Post, index)));
    }
    rows.push(PipelineVisualRow::Spacer);
    rows.push(PipelineVisualRow::Hint);
    rows
}

fn selected_pipeline_visual_row(
    state: &ConversionActionsWizardState,
    rows: &[PipelineVisualRow],
) -> Option<usize> {
    rows.iter().position(|row| {
        matches!(
            row,
            PipelineVisualRow::Action(phase, index)
                if *phase == state.pipeline_phase && *index == state.pipeline_index
        )
    })
}

fn effective_pipeline_scroll(state: &ConversionActionsWizardState, visible: usize) -> usize {
    let rows = pipeline_visual_rows(state);
    let scroll = clamp_scroll_to_view(state.pipeline_scroll, rows.len(), visible);
    if state.focus == ActionsWizardFocus::Pipeline && matches!(state.dialog, ActionsWizardDialog::Pipeline) {
        if let Some(selected) = selected_pipeline_visual_row(state, &rows) {
            return scroll_to_include(scroll, selected, rows.len(), visible);
        }
    }
    scroll
}

fn effective_available_scroll(state: &ConversionActionsWizardState, visible: usize) -> usize {
    let scroll = clamp_scroll_to_view(state.available_scroll, ACTION_KINDS.len(), visible);
    if state.focus == ActionsWizardFocus::Available && matches!(state.dialog, ActionsWizardDialog::Pipeline) {
        scroll_to_include(scroll, state.available_index, ACTION_KINDS.len(), visible)
    } else {
        scroll
    }
}

fn effective_config_scroll(
    state: &ConversionActionsWizardState,
    field_count: usize,
    visible: usize,
) -> usize {
    let scroll = clamp_scroll_to_view(state.config_scroll, field_count, visible);
    if state.focus == ActionsWizardFocus::Config {
        scroll_to_include(scroll, state.config_index, field_count, visible)
    } else {
        scroll
    }
}

fn select_first_pipeline_action_at_or_after_scroll(state: &mut ConversionActionsWizardState) {
    let rows = pipeline_visual_rows(state);
    for row in rows.iter().skip(state.pipeline_scroll) {
        if let PipelineVisualRow::Action(phase, index) = row {
            state.pipeline_phase = *phase;
            state.pipeline_index = *index;
            return;
        }
    }
    for row in rows.iter().rev() {
        if let PipelineVisualRow::Action(phase, index) = row {
            state.pipeline_phase = *phase;
            state.pipeline_index = *index;
            return;
        }
    }
}

fn delta_magnitude(delta: isize) -> usize {
    delta.checked_abs().unwrap_or(isize::MAX) as usize
}

fn stepped_index(current: usize, count: usize, delta: isize) -> usize {
    if count == 0 {
        return 0;
    }
    if delta < 0 {
        current.saturating_sub(delta_magnitude(delta)).min(count - 1)
    } else {
        current.saturating_add(delta as usize).min(count - 1)
    }
}


fn is_config_dialog_button(button: TuiButton) -> bool {
    matches!(
        button,
        TuiButton::ActionsConfigModal
            | TuiButton::ActionsConfigField(_)
            | TuiButton::ActionsConfigMode(_)
            | TuiButton::ActionsConfigToken(_)
            | TuiButton::ActionsConfigApply
            | TuiButton::ActionsConfigCancel
            | TuiButton::ActionsConfigPreview
    )
}

fn rename_template_field_index(action: &ConversionAction) -> Option<usize> {
    matches!(action, ConversionAction::Rename(_)).then_some(4)
}

fn insert_template_token(state: &mut ConversionActionsWizardState, token: &str) -> bool {
    let Some(template_field) = state.configured_action().and_then(rename_template_field_index) else {
        return true;
    };

    let editing_template = state
        .edit_input
        .as_ref()
        .map(|edit| edit.field_index == template_field)
        .unwrap_or(false);

    if !editing_template && !commit_pending_config_edit_or_notice(state) {
        return false;
    }

    state.focus = ActionsWizardFocus::Config;
    state.config_index = template_field;
    if state.edit_input.is_none() {
        let template = match state.configured_action() {
            Some(ConversionAction::Rename(action)) => action.template.clone(),
            _ => return true,
        };
        state.edit_input = Some(ActionConfigEdit {
            field_index: template_field,
            input: crate::tui::text_input::TextInputState::new(template),
        });
    }

    if let Some(edit) = &mut state.edit_input {
        edit.field_index = template_field;
        let cursor = edit.input.cursor.min(edit.input.text.len());
        edit.input.text.insert_str(cursor, token);
        edit.input.cursor = cursor + token.len();
    }
    state.mark_config_preview_dirty();
    true
}

pub fn handle_wizard_button(
    mut state: ConversionActionsWizardState,
    button: TuiButton,
    double_click: bool,
) -> WizardKeyResult {
    let configuring = matches!(state.dialog, ActionsWizardDialog::Configure(_));
    if configuring && !is_config_dialog_button(button) {
        state.clamp();
        return WizardKeyResult::Continue(state);
    }
    if !configuring && is_config_dialog_button(button) {
        state.clamp();
        return WizardKeyResult::Continue(state);
    }

    match button {
        TuiButton::ActionsAvailable(index) => {
            state.available_index = index.min(ACTION_KINDS.len().saturating_sub(1));
            state.focus = ActionsWizardFocus::Available;
            if double_click {
                add_selected_action(&mut state);
            }
        }
        TuiButton::ActionsAvailablePane => {
            state.focus = ActionsWizardFocus::Available;
        }
        TuiButton::ActionsPipelineRow(pre, index) => {
            state.pipeline_phase = if pre { ActionPhase::Pre } else { ActionPhase::Post };
            state.pipeline_index = index.min(state.actions().len().saturating_sub(1));
            state.focus = ActionsWizardFocus::Pipeline;
            if double_click {
                open_config_for_selected(&mut state, false);
            }
        }
        TuiButton::ActionsPipelinePane => {
            state.focus = ActionsWizardFocus::Pipeline;
        }
        TuiButton::ActionsPipelineNudgeUp(pre, index) => {
            state.pipeline_phase = if pre { ActionPhase::Pre } else { ActionPhase::Post };
            state.pipeline_index = index.min(state.actions().len().saturating_sub(1));
            state.focus = ActionsWizardFocus::Pipeline;
            reorder_selected_pipeline_action(&mut state, -1);
        }
        TuiButton::ActionsPipelineNudgeDown(pre, index) => {
            state.pipeline_phase = if pre { ActionPhase::Pre } else { ActionPhase::Post };
            state.pipeline_index = index.min(state.actions().len().saturating_sub(1));
            state.focus = ActionsWizardFocus::Pipeline;
            reorder_selected_pipeline_action(&mut state, 1);
        }
        TuiButton::ActionsAddingPhase(pre) => {
            state.phase = if pre { ActionPhase::Pre } else { ActionPhase::Post };
            state.focus = ActionsWizardFocus::Phase;
            state.refresh_summary_preview();
        }
        TuiButton::ActionsFooterAdd => {
            state.focus = ActionsWizardFocus::Available;
            add_selected_action(&mut state);
        }
        TuiButton::ActionsFooterConfigure => {
            state.focus = ActionsWizardFocus::Pipeline;
            open_config_for_selected(&mut state, false);
        }
        TuiButton::ActionsFooterSave => return WizardKeyResult::Commit(state.draft),
        TuiButton::ActionsFooterSaveDefault => return WizardKeyResult::CommitDefault(state),
        TuiButton::ActionsFooterDone => return WizardKeyResult::Cancel,
        TuiButton::ActionsConfigField(index) => {
            let same_active_edit = state
                .edit_input
                .as_ref()
                .map(|edit| edit.field_index == index)
                .unwrap_or(false);
            if !same_active_edit && !commit_pending_config_edit_or_notice(&mut state) {
                return WizardKeyResult::Continue(state);
            }
            state.config_index = index;
            state.focus = ActionsWizardFocus::Config;
            if !same_active_edit {
                edit_config_field(&mut state);
            }
        }
        TuiButton::ActionsConfigMode(index) => {
            if !commit_pending_config_edit_or_notice(&mut state) {
                return WizardKeyResult::Continue(state);
            }
            state.focus = ActionsWizardFocus::Config;
            if let Some(ConversionAction::Rename(action)) = state.configured_action_mut() {
                match index {
                    0 => action.mode = RenameMode::Template,
                    1 => action.mode = RenameMode::Uppercase,
                    2 => action.mode = RenameMode::Lowercase,
                    3 => action.mode = RenameMode::Fixcaps,
                    _ => {}
                }
            }
            state.refresh_summary_preview();
            state.mark_config_preview_dirty();
        }
        TuiButton::ActionsConfigToken(index) => {
            const TOKENS: [&str; 5] = ["%ARTIST%", "%ALBUM%", "%DISC%", "%TRACK%", "%YEAR%"];
            if let Some(token) = TOKENS.get(index).copied() {
                if !insert_template_token(&mut state, token) {
                    return WizardKeyResult::Continue(state);
                }
            }
        }
        TuiButton::ActionsConfigApply => return request_action_config_apply(state),
        TuiButton::ActionsConfigCancel => cancel_action_config(&mut state),
        TuiButton::ActionsConfigPreview => {
            if !commit_pending_config_edit_or_notice(&mut state) {
                return WizardKeyResult::Continue(state);
            }
            state.focus = ActionsWizardFocus::Preview;
        }
        TuiButton::ActionsConfigModal => {}
        _ => {}
    }
    state.clamp();
    WizardKeyResult::Continue(state)
}

pub fn handle_wizard_scroll(
    mut state: ConversionActionsWizardState,
    button: Option<TuiButton>,
    delta: isize,
    preview_rect: Option<Rect>,
) -> WizardKeyResult {
    let configuring = matches!(state.dialog, ActionsWizardDialog::Configure(_));
    if configuring && !button.map(is_config_dialog_button).unwrap_or(false) {
        state.clamp();
        return WizardKeyResult::Continue(state);
    }

    match button {
        Some(TuiButton::ActionsAvailable(_)) => {
            state.focus = ActionsWizardFocus::Available;
            state.available_scroll = scroll_offset_for_delta(
                state.available_scroll,
                ACTION_KINDS.len(),
                1,
                delta,
            );
            state.available_index = state.available_scroll.min(ACTION_KINDS.len().saturating_sub(1));
        }
        Some(TuiButton::ActionsAvailablePane) => {
            state.focus = ActionsWizardFocus::Available;
            state.available_scroll = scroll_offset_for_delta(
                state.available_scroll,
                ACTION_KINDS.len(),
                1,
                delta,
            );
            state.available_index = state.available_scroll.min(ACTION_KINDS.len().saturating_sub(1));
        }
        Some(TuiButton::ActionsPipelineRow(pre, _))
        | Some(TuiButton::ActionsPipelineNudgeUp(pre, _))
        | Some(TuiButton::ActionsPipelineNudgeDown(pre, _)) => {
            state.pipeline_phase = if pre { ActionPhase::Pre } else { ActionPhase::Post };
            state.focus = ActionsWizardFocus::Pipeline;
            state.pipeline_scroll = scroll_offset_for_delta(
                state.pipeline_scroll,
                pipeline_visual_rows(&state).len(),
                1,
                delta,
            );
            select_first_pipeline_action_at_or_after_scroll(&mut state);
        }
        Some(TuiButton::ActionsPipelinePane) => {
            state.focus = ActionsWizardFocus::Pipeline;
            state.pipeline_scroll = scroll_offset_for_delta(
                state.pipeline_scroll,
                pipeline_visual_rows(&state).len(),
                1,
                delta,
            );
            select_first_pipeline_action_at_or_after_scroll(&mut state);
        }
        Some(TuiButton::ActionsConfigField(_)) => {
            state.focus = ActionsWizardFocus::Config;
            let field_count = state
                .configured_action()
                .map(action_fields)
                .map(|fields| fields.len())
                .unwrap_or(0);
            state.config_scroll = scroll_offset_for_delta(state.config_scroll, field_count, 1, delta);
            state.config_index = state.config_scroll.min(field_count.saturating_sub(1));
        }
        Some(TuiButton::ActionsConfigPreview) => {
            state.focus = ActionsWizardFocus::Preview;
            scroll_preview_by_delta(&mut state, delta, preview_rect);
        }
        _ => {}
    }
    state.clamp();
    WizardKeyResult::Continue(state)
}

fn default_targeting() -> TargetSpec {
    TargetSpec {
        target: vec!["*".to_string()],
        exclude: Vec::new(),
        allow_sources: false,
        continue_on_error: false,
    }
}

fn default_action_for_kind(index: usize, _phase: ActionPhase) -> Option<ConversionAction> {
    let action = match index {
        0 => ConversionAction::Rename(RenameAction {
            targeting: default_targeting(),
            mode: RenameMode::Template,
            template: "%STEM%".to_string(),
        }),
        1 => ConversionAction::Copy(CopyAction {
            targeting: default_targeting(),
            destination: PathBuf::from("Copied"),
        }),
        2 => ConversionAction::Move(MoveAction {
            targeting: default_targeting(),
            destination: PathBuf::from("Moved"),
        }),
        3 => ConversionAction::Delete(DeleteAction {
            targeting: default_targeting(),
        }),
        4 => ConversionAction::CreateFolder(CreateFolderAction {
            path: PathBuf::from("New folder"),
            continue_on_error: false,
        }),
        5 => ConversionAction::Runscript(RunScriptAction {
            script: PathBuf::from("script.sh"),
            args: Vec::new(),
            timeout_seconds: 600,
            continue_on_error: false,
        }),
        _ => return None,
    };
    Some(action)
}

#[derive(Debug, Clone)]
struct ActionField {
    label: &'static str,
    value: String,
}

fn action_fields(action: &ConversionAction) -> Vec<ActionField> {
    let targeting_fields = |targeting: &TargetSpec| {
        vec![
            ActionField { label: "target", value: targeting.target.join(", ") },
            ActionField { label: "exclude", value: targeting.exclude.join(", ") },
            ActionField { label: "allow sources", value: targeting.allow_sources.to_string() },
            ActionField { label: "continue on error", value: targeting.continue_on_error.to_string() },
        ]
    };
    match action {
        ConversionAction::Rename(action) => {
            let mut fields = targeting_fields(&action.targeting);
            fields.push(ActionField { label: "template", value: action.template.clone() });
            fields
        }
        ConversionAction::Copy(action) => {
            let mut fields = targeting_fields(&action.targeting);
            fields.push(ActionField { label: "destination", value: action.destination.display().to_string() });
            fields
        }
        ConversionAction::Move(action) => {
            let mut fields = targeting_fields(&action.targeting);
            fields.push(ActionField { label: "destination", value: action.destination.display().to_string() });
            fields
        }
        ConversionAction::Delete(action) => targeting_fields(&action.targeting),
        ConversionAction::CreateFolder(action) => vec![
            ActionField { label: "path", value: action.path.display().to_string() },
            ActionField { label: "continue on error", value: action.continue_on_error.to_string() },
        ],
        ConversionAction::Runscript(action) => vec![
            ActionField { label: "script", value: action.script.display().to_string() },
            ActionField {
                label: "argv (JSON array)",
                value: serde_json::to_string(&action.args).unwrap_or_else(|_| "[]".to_string()),
            },
            ActionField { label: "timeout seconds", value: action.timeout_seconds.to_string() },
            ActionField { label: "continue on error", value: action.continue_on_error.to_string() },
        ],
    }
}

enum FieldEdit {
    Text(String),
    Changed,
    Unavailable,
}

fn action_field_edit(action: &mut ConversionAction, field: usize) -> FieldEdit {
    match action {
        ConversionAction::Rename(action) => match field {
            0 => FieldEdit::Text(action.targeting.target.join(", ")),
            1 => FieldEdit::Text(action.targeting.exclude.join(", ")),
            2 => { action.targeting.allow_sources = !action.targeting.allow_sources; FieldEdit::Changed }
            3 => { action.targeting.continue_on_error = !action.targeting.continue_on_error; FieldEdit::Changed }
            4 => FieldEdit::Text(action.template.clone()),
            _ => FieldEdit::Unavailable,
        },
        ConversionAction::Copy(action) => targeting_action_field_edit(&mut action.targeting, field)
            .or_else(|| (field == 4).then(|| FieldEdit::Text(action.destination.display().to_string())))
            .unwrap_or(FieldEdit::Unavailable),
        ConversionAction::Move(action) => targeting_action_field_edit(&mut action.targeting, field)
            .or_else(|| (field == 4).then(|| FieldEdit::Text(action.destination.display().to_string())))
            .unwrap_or(FieldEdit::Unavailable),
        ConversionAction::Delete(action) => targeting_action_field_edit(&mut action.targeting, field)
            .unwrap_or(FieldEdit::Unavailable),
        ConversionAction::CreateFolder(action) => match field {
            0 => FieldEdit::Text(action.path.display().to_string()),
            1 => { action.continue_on_error = !action.continue_on_error; FieldEdit::Changed }
            _ => FieldEdit::Unavailable,
        },
        ConversionAction::Runscript(action) => match field {
            0 => FieldEdit::Text(action.script.display().to_string()),
            1 => FieldEdit::Text(
                serde_json::to_string(&action.args).unwrap_or_else(|_| "[]".to_string()),
            ),
            2 => FieldEdit::Text(action.timeout_seconds.to_string()),
            3 => { action.continue_on_error = !action.continue_on_error; FieldEdit::Changed }
            _ => FieldEdit::Unavailable,
        },
    }
}

fn targeting_action_field_edit(targeting: &mut TargetSpec, field: usize) -> Option<FieldEdit> {
    Some(match field {
        0 => FieldEdit::Text(targeting.target.join(", ")),
        1 => FieldEdit::Text(targeting.exclude.join(", ")),
        2 => { targeting.allow_sources = !targeting.allow_sources; FieldEdit::Changed }
        3 => { targeting.continue_on_error = !targeting.continue_on_error; FieldEdit::Changed }
        _ => return None,
    })
}

fn split_csv(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn require_target_patterns(value: &str) -> Result<Vec<String>, String> {
    let patterns = split_csv(value);
    if patterns.is_empty() {
        Err("target must include at least one pattern".to_string())
    } else {
        Ok(patterns)
    }
}

fn require_non_empty_path(value: &str, label: &str) -> Result<PathBuf, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        Err(format!("{label} must not be empty"))
    } else {
        Ok(PathBuf::from(trimmed))
    }
}

fn validate_targeting(targeting: &TargetSpec) -> Result<(), String> {
    if targeting.target.iter().all(|value| value.trim().is_empty()) {
        Err("target must include at least one pattern".to_string())
    } else {
        Ok(())
    }
}

fn validate_action_config(action: &ConversionAction) -> Result<(), String> {
    match action {
        ConversionAction::Rename(action) => {
            validate_targeting(&action.targeting)?;
            if matches!(action.mode, RenameMode::Template) && action.template.trim().is_empty() {
                return Err("template must not be empty in Template mode".to_string());
            }
        }
        ConversionAction::Copy(action) => {
            validate_targeting(&action.targeting)?;
            if action.destination.as_os_str().is_empty() {
                return Err("destination must not be empty".to_string());
            }
        }
        ConversionAction::Move(action) => {
            validate_targeting(&action.targeting)?;
            if action.destination.as_os_str().is_empty() {
                return Err("destination must not be empty".to_string());
            }
        }
        ConversionAction::Delete(action) => validate_targeting(&action.targeting)?,
        ConversionAction::CreateFolder(action) => {
            if action.path.as_os_str().is_empty() {
                return Err("folder path must not be empty".to_string());
            }
        }
        ConversionAction::Runscript(action) => {
            if action.script.as_os_str().is_empty() {
                return Err("script path must not be empty".to_string());
            }
            if action.timeout_seconds == 0 {
                return Err("timeout must be greater than zero".to_string());
            }
        }
    }
    Ok(())
}

fn set_action_field_text(
    action: &mut ConversionAction,
    field: usize,
    value: &str,
) -> Result<(), String> {
    match action {
        ConversionAction::Rename(action) => match field {
            0 => action.targeting.target = require_target_patterns(value)?,
            1 => action.targeting.exclude = split_csv(value),
            4 => action.template = value.to_string(),
            _ => {}
        },
        ConversionAction::Copy(action) => match field {
            0 => action.targeting.target = require_target_patterns(value)?,
            1 => action.targeting.exclude = split_csv(value),
            4 => action.destination = require_non_empty_path(value, "destination")?,
            _ => {}
        },
        ConversionAction::Move(action) => match field {
            0 => action.targeting.target = require_target_patterns(value)?,
            1 => action.targeting.exclude = split_csv(value),
            4 => action.destination = require_non_empty_path(value, "destination")?,
            _ => {}
        },
        ConversionAction::Delete(action) => match field {
            0 => action.targeting.target = require_target_patterns(value)?,
            1 => action.targeting.exclude = split_csv(value),
            _ => {}
        },
        ConversionAction::CreateFolder(action) => {
            if field == 0 { action.path = require_non_empty_path(value, "folder path")?; }
        }
        ConversionAction::Runscript(action) => match field {
            0 => action.script = require_non_empty_path(value, "script path")?,
            1 => {
                action.args = serde_json::from_str::<Vec<String>>(value).map_err(|error| {
                    format!(
                        "argv must be a JSON string array (for example [\"--title\", \"A value with spaces\"]): {error}"
                    )
                })?;
            }
            2 => {
                action.timeout_seconds = value
                    .trim()
                    .parse::<u64>()
                    .map_err(|_| "timeout must be a positive integer number of seconds".to_string())?;
                if action.timeout_seconds == 0 {
                    return Err("timeout must be greater than zero".to_string());
                }
            }
            _ => {}
        },
    }
    Ok(())
}

fn action_summary(action: &ConversionAction) -> String {
    match action {
        ConversionAction::Rename(action) => format!(
            "rename {:?} [{}]",
            action.mode,
            action.targeting.target.join(", ")
        ),
        ConversionAction::Copy(action) => format!(
            "copy [{}] -> {}",
            action.targeting.target.join(", "),
            action.destination.display()
        ),
        ConversionAction::Move(action) => format!(
            "move [{}] -> {}",
            action.targeting.target.join(", "),
            action.destination.display()
        ),
        ConversionAction::Delete(action) => {
            format!("delete [{}]", action.targeting.target.join(", "))
        }
        ConversionAction::CreateFolder(action) => {
            format!("create folder {}", action.path.display())
        }
        ConversionAction::Runscript(action) => {
            format!("run {} {:?}", action.script.display(), action.args)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionsRunStatus {
    Preparing,
    Preview,
    RecoveryPreview,
    Running,
    Complete,
    Failed,
    Stale,
    Cancelling,
}

#[derive(Debug, Clone)]
pub struct PreparedActionsRunAuthority {
    pub invocation_id: String,
    pub context: ActionContext,
    pub plans: Vec<crate::convert::pipeline::ActionPlan>,
    pub expected_identity_sha256: String,
    pub preview_authority_sha256: String,
    pub invocation_state: ManualInvocationState,
}

#[derive(Debug, Clone)]
pub struct ActionsRunState {
    pub preparation_id: String,
    pub target: PathBuf,
    pub pipeline: ActionPipeline,
    pub prepared: Option<PreparedActionsRunAuthority>,
    pub preview_lines: Vec<String>,
    pub status: ActionsRunStatus,
    pub report: Option<ActionPhaseReport>,
    pub error: Option<String>,
    pub cancellation: Arc<AtomicBool>,
}

impl ActionsRunState {
    pub fn invocation_id(&self) -> Option<&str> {
        self.prepared
            .as_ref()
            .map(|prepared| prepared.invocation_id.as_str())
    }
}

#[derive(Clone)]
struct SharedCancellation(Arc<AtomicBool>);

impl ActionCancellation for SharedCancellation {
    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

#[derive(Clone)]
struct ChannelExplicitPreviewProgress {
    preparation_id: String,
    tx: mpsc::Sender<AppMessage>,
}

impl ExplicitPreviewProgressObserver for ChannelExplicitPreviewProgress {
    fn update(&self, phase: &'static str, completed: u64, total: Option<u64>) {
        let detail = match total {
            Some(total) if total > 0 => format!("{phase}: {completed}/{total}"),
            Some(_) => format!("{phase}: complete"),
            None => phase.to_string(),
        };
        let _ = self.tx.blocking_send(AppMessage::ActionsRunPreparationProgress {
            preparation_id: self.preparation_id.clone(),
            detail,
        });
    }
}

fn unresolved_explicit_target(raw: Option<&str>, current_dir: &Path) -> PathBuf {
    match raw.map(str::trim).filter(|value| !value.is_empty()) {
        None => current_dir.to_path_buf(),
        Some("~") => dirs::home_dir().unwrap_or_else(|| current_dir.to_path_buf()),
        Some(value) if value.starts_with("~/") => dirs::home_dir()
            .unwrap_or_else(|| current_dir.to_path_buf())
            .join(&value[2..]),
        Some(value) => {
            let path = PathBuf::from(value);
            if path.is_absolute() {
                path
            } else {
                current_dir.join(path)
            }
        }
    }
}

pub fn start_actions_run_preparation(
    current_dir: PathBuf,
    pipeline: ActionPipeline,
    argument: Option<String>,
    tx: &mpsc::Sender<AppMessage>,
) -> ActionsRunState {
    let preparation_id = format!("actions-preview:{}", Uuid::new_v4());
    let cancellation = Arc::new(AtomicBool::new(false));
    let state = ActionsRunState {
        preparation_id: preparation_id.clone(),
        target: unresolved_explicit_target(argument.as_deref(), &current_dir),
        pipeline: pipeline.clone(),
        prepared: None,
        preview_lines: vec![
            "Scanning directory entries and binding only concrete operands; unrelated audio payloads are not hashed."
                .to_string(),
        ],
        status: ActionsRunStatus::Preparing,
        report: None,
        error: None,
        cancellation: cancellation.clone(),
    };
    let tx = tx.clone();
    tokio::task::spawn_blocking(move || {
        let progress = ChannelExplicitPreviewProgress {
            preparation_id: preparation_id.clone(),
            tx: tx.clone(),
        };
        let result = build_actions_run_state_from_inputs(
            &current_dir,
            pipeline,
            argument.as_deref(),
            preparation_id.clone(),
            cancellation,
            &progress,
        );
        let _ = tx.blocking_send(AppMessage::ActionsRunPrepared {
            preparation_id,
            result,
        });
    });
    state
}

pub fn build_actions_run_state(
    app: &AppState,
    argument: Option<&str>,
) -> Result<ActionsRunState, String> {
    build_actions_run_state_from_inputs(
        &app.browse.current_dir,
        app.convert.output_options.actions.clone(),
        argument,
        format!("actions-preview:{}", Uuid::new_v4()),
        Arc::new(AtomicBool::new(false)),
        &NoExplicitPreviewProgress,
    )
}

fn build_actions_run_state_from_inputs(
    current_dir: &Path,
    pipeline: ActionPipeline,
    argument: Option<&str>,
    preparation_id: String,
    cancellation: Arc<AtomicBool>,
    progress: &dyn ExplicitPreviewProgressObserver,
) -> Result<ActionsRunState, String> {
    let cancellation_view = SharedCancellation(cancellation.clone());
    if cancellation_view.is_cancelled() {
        return Err("Action preview preparation cancelled; nothing was executed".to_string());
    }
    let raw = argument.map(str::trim).filter(|value| !value.is_empty());
    let target = resolve_explicit_target(raw, current_dir)?;
    if pipeline.post.is_empty() {
        return Err("No post-conversion actions are configured. Use :actions first.".to_string());
    }
    let lock = acquire_explicit_action_run_lock_for_album(&target)
        .map_err(|error| error.to_string())?;
    let canonical_target = lock.canonical_album_dir().to_path_buf();
    let identity = crate::convert::pipeline::stages::conversion_action_explicit_identity_locked(
        &canonical_target,
        &lock,
    )?;
    let expected_identity_sha256 = identity.payload_sha256.clone();
    let context = explicit_context_from_identity(&canonical_target, identity, &lock)?;
    let filesystem = CapabilityActionFilesystem::new();
    let scripts = ProcessGroupScriptRunner;
    let engine = ActionEngine {
        filesystem: &filesystem,
        scripts: &scripts,
    };
    let prepared = engine
        .prepare_explicit_invocation_with_lock_cancellable_observed(
            &pipeline,
            &context,
            &expected_identity_sha256,
            &cancellation_view,
            progress,
            &lock,
        )
        .map_err(|error| error.to_string())?;
    let mut preview_lines = Vec::new();
    if prepared.is_recovery {
        preview_lines.push(format!(
            "RECOVERY REQUIRED — resuming durable invocation {}. This is not a fresh plan.",
            prepared.invocation_id
        ));
        if prepared.recovery_operations.is_empty() {
            preview_lines.push(
                "The reviewed plan is durable; no operation had reached a journaled mutation state before interruption."
                    .to_string(),
            );
        } else {
            for operation in &prepared.recovery_operations {
                preview_lines.push(format!(
                    "Action {} {} [{}]: {}{}",
                    operation.action_index + 1,
                    operation.action_kind,
                    operation.durable_state,
                    operation.summary,
                    if operation.script_started {
                        " (script started; never replayed automatically)"
                    } else if operation.cleanup_only {
                        " (cleanup only)"
                    } else {
                        ""
                    },
                ));
            }
        }
    } else {
        preview_lines.push(format!(
            "Prepared invocation {}. Apply is bound to this exact serialized plan.",
            prepared.invocation_id
        ));
        for (index, plan) in prepared.plans.iter().enumerate() {
            preview_lines.push(format!("Action {}: {}", index + 1, plan.action_kind));
            preview_lines.extend(
                describe_plan(plan)
                    .into_iter()
                    .map(|line| format!("  {line}")),
            );
        }
    }
    if prepared.plans.iter().all(|plan| plan.operations.is_empty())
        && prepared.recovery_operations.is_empty()
    {
        preview_lines.push("No operations are present in the reviewed plan.".to_string());
    }
    Ok(ActionsRunState {
        preparation_id,
        target: canonical_target,
        pipeline,
        prepared: Some(PreparedActionsRunAuthority {
            invocation_id: prepared.invocation_id,
            context,
            plans: prepared.plans.clone(),
            expected_identity_sha256,
            preview_authority_sha256: prepared.authority_sha256,
            invocation_state: prepared.state,
        }),
        preview_lines,
        status: if prepared.is_recovery {
            ActionsRunStatus::RecoveryPreview
        } else {
            ActionsRunStatus::Preview
        },
        report: None,
        error: None,
        cancellation,
    })
}

pub fn import_actions_identity(
    current_dir: &Path,
    source_argument: Option<&str>,
) -> Result<String, String> {
    let target = resolve_explicit_target(None, current_dir)?;
    let source = match source_argument.map(str::trim).filter(|value| !value.is_empty()) {
        None => target.join(".tonepoet-action-identity.import.json"),
        Some("~") => dirs::home_dir()
            .ok_or_else(|| "home directory is unavailable".to_string())?,
        Some(value) if value.starts_with("~/") => dirs::home_dir()
            .ok_or_else(|| "home directory is unavailable".to_string())?
            .join(&value[2..]),
        Some(value) => {
            let path = PathBuf::from(value);
            if path.is_absolute() { path } else { current_dir.join(path) }
        }
    };
    let identity = crate::convert::pipeline::stages::import_conversion_action_identity(
        &target,
        &source,
    )?;
    Ok(format!(
        "Imported canonical action identity {} for {}",
        identity.payload_sha256,
        target.display()
    ))
}

fn resolve_explicit_target(raw: Option<&str>, current_dir: &Path) -> Result<PathBuf, String> {
    let candidate = match raw {
        None => current_dir.to_path_buf(),
        Some(value) if value == "~" => dirs::home_dir().ok_or_else(|| "home directory is unavailable".to_string())?,
        Some(value) if value.starts_with("~/") => dirs::home_dir()
            .ok_or_else(|| "home directory is unavailable".to_string())?
            .join(&value[2..]),
        Some(value) => {
            let path = PathBuf::from(value);
            if path.is_absolute() { path } else { current_dir.join(path) }
        }
    };
    let canonical = candidate
        .canonicalize()
        .map_err(|error| format!("cannot open action target {}: {error}", candidate.display()))?;
    if !canonical.is_dir() {
        return Err(format!("action target is not a directory: {}", canonical.display()));
    }
    if canonical.parent().is_none() {
        return Err(format!(":actions-run refuses a filesystem root: {}", canonical.display()));
    }
    Ok(canonical)
}

#[allow(dead_code)] // bundle-provided API surface, not yet wired to a caller
fn explicit_context(target: &Path) -> Result<ActionContext, String> {
    let lock = acquire_explicit_action_run_lock_for_album(target)
        .map_err(|error| error.to_string())?;
    let canonical_target = lock.canonical_album_dir().to_path_buf();
    let identity = crate::convert::pipeline::stages::conversion_action_explicit_identity_locked(
        &canonical_target,
        &lock,
    )?;
    explicit_context_from_identity(&canonical_target, identity, &lock)
}

fn explicit_context_from_identity(
    target: &Path,
    identity: crate::convert::pipeline::stages::ConversionActionExplicitIdentity,
    lock: &ExplicitActionRunLock,
) -> Result<ActionContext, String> {
    let parent = target
        .parent()
        .ok_or_else(|| format!("action target has no parent: {}", target.display()))?
        .to_path_buf();
    let (album_capability, output_capability, journal_capability) = lock
        .retained_manual_context_capabilities()
        .map_err(|error| error.to_string())?;
    let mut protected_generated_paths = BTreeSet::new();
    protected_generated_paths.insert(target.join("conversion.log"));
    protected_generated_paths.insert(target.join(".tonepoet-action-identity.json"));
    protected_generated_paths.insert(target.join(".tonepoet-action-identity.import.json"));
    Ok(ActionContext {
        run_identity: identity.run_identity,
        album_identity: identity.album_identity,
        phase: ActionPhase::Post,
        subject_dir: target.to_path_buf(),
        source_path: target.to_path_buf(),
        source_is_directory: true,
        output_root: parent,
        album_dir: target.to_path_buf(),
        environment_album_dir: None,
        retained_album_capability: Some(Arc::new(album_capability)),
        retained_output_capability: Some(Arc::new(output_capability)),
        retained_journal_capability: Some(Arc::new(journal_capability)),
        coordination_io_dir: None,
        protected_sources: BTreeSet::new(),
        protected_generated_paths,
        album_tokens: identity.album_tokens,
        disc_count: identity.disc_count,
        journal_dir: target.join(".tonepoet-actions-manual"),
        batch_source_scope_root: None,
        explicit_scope: true,
        semantics: ui_action_semantics(),
    })
}

fn ui_action_semantics() -> crate::convert::pipeline::ActionSemantics {
    crate::convert::pipeline::ActionSemantics {
        wildcard_matches: crate::convert::pipeline::stages::conversion_action_wildcard_matches,
        render_template: crate::convert::pipeline::stages::conversion_action_render_template,
        sanitize_component: crate::convert::pipeline::stages::conversion_action_sanitize_component,
        fixcaps: crate::convert::renaming::capitalize_title,
        disc_number_for_path: crate::convert::pipeline::stages::conversion_action_disc_number_for_path,
    }
}

fn lock_and_revalidate_explicit_execution_context(
    target: &Path,
    expected_identity_sha256: &str,
    preview_context: &ActionContext,
) -> Result<(ExplicitActionRunLock, ActionContext), String> {
    let lock = acquire_explicit_action_run_lock_for_album(target)
        .map_err(|error| error.to_string())?;
    let canonical_target = lock.canonical_album_dir().to_path_buf();
    let identity = crate::convert::pipeline::stages::conversion_action_explicit_identity_locked(
        &canonical_target,
        &lock,
    )?;
    if identity.payload_sha256 != expected_identity_sha256 {
        return Err(format!(
            "preview is stale; refresh required because canonical album identity changed (expected {}, found {})",
            expected_identity_sha256,
            identity.payload_sha256
        ));
    }
    let context = explicit_context_from_identity(&canonical_target, identity, &lock)?;
    if context.album_identity != preview_context.album_identity
        || context.album_tokens != preview_context.album_tokens
        || context.disc_count != preview_context.disc_count
    {
        return Err(
            "preview is stale; refresh required because canonical album identity changed"
                .to_string(),
        );
    }
    Ok((lock, context))
}

pub fn start_actions_run(state: &mut ActionsRunState, tx: &mpsc::Sender<AppMessage>) {
    if !matches!(
        state.status,
        ActionsRunStatus::Preview | ActionsRunStatus::RecoveryPreview
    ) {
        return;
    }
    let Some(prepared) = state.prepared.clone() else {
        state.status = ActionsRunStatus::Failed;
        state.error = Some("prepared action authority is unavailable".to_string());
        return;
    };
    state.status = ActionsRunStatus::Running;
    let invocation_id = prepared.invocation_id;
    let pipeline = state.pipeline.clone();
    let preview_context = prepared.context;
    let prepared_plans = prepared.plans;
    let target = state.target.clone();
    let expected_identity_sha256 = prepared.expected_identity_sha256;
    let preview_authority_sha256 = prepared.preview_authority_sha256;
    let cancellation = SharedCancellation(state.cancellation.clone());
    let tx = tx.clone();
    tokio::task::spawn_blocking(move || {
        let result = (|| -> Result<ActionPhaseReport, String> {
            let (mut lock, context) = lock_and_revalidate_explicit_execution_context(
                &target,
                &expected_identity_sha256,
                &preview_context,
            )?;
            let action_claims = crate::convert::pipeline::actions::shared_path_claims_for_action_plans(
                &prepared_plans,
                &context,
            ).map_err(|error| error.to_string())?;
            let _mutation_guard = if action_claims.is_empty() {
                None
            } else {
                Some(crate::concurrency::MutationClaimGuard::acquire(
                    crate::concurrency::LeaseFamily::EphemeralMutation { claim_id: uuid::Uuid::new_v4() },
                    action_claims,
                ).map_err(|error| format!("manual action concurrency admission failed: {error}"))?)
            };
            let manual_supervision_files = _mutation_guard
                .as_ref()
                .map(|guard| guard.lease().duplicate_lifetime_file())
                .transpose()
                .map_err(|error| format!("manual action supervisor lease handoff failed: {error}"))?
                .into_iter()
                .collect::<Vec<_>>();
            let filesystem = CapabilityActionFilesystem::new();
            let scripts = ProcessGroupScriptRunner;
            let engine = ActionEngine { filesystem: &filesystem, scripts: &scripts };
            crate::concurrency::with_thread_supervision_lifetime_files(
                manual_supervision_files,
                || engine
                    .execute_prepared_explicit_phase_with_lock(
                        &pipeline,
                        &context,
                        &expected_identity_sha256,
                        &invocation_id,
                        &preview_authority_sha256,
                        &cancellation,
                        &mut lock,
                    )
                    .map_err(|error| error.to_string()),
            )
        })();
        let _ = tx.blocking_send(AppMessage::ActionsRunComplete { invocation_id, result });
    });
}

pub fn discard_actions_run_preview(state: &ActionsRunState) -> Result<(), String> {
    if state.status == ActionsRunStatus::RecoveryPreview {
        // Closing a recovery preview never retires its journal or active-run
        // authority; the next invocation must rediscover the same work.
        return Ok(());
    }
    if !matches!(state.status, ActionsRunStatus::Preview | ActionsRunStatus::Stale) {
        return Ok(());
    }
    let prepared = state.prepared.as_ref().ok_or_else(|| {
        "prepared action authority is unavailable".to_string()
    })?;
    let lock = acquire_explicit_action_run_lock_for_album(&state.target)
        .map_err(|error| error.to_string())?;
    let filesystem = CapabilityActionFilesystem::new();
    let scripts = ProcessGroupScriptRunner;
    let engine = ActionEngine {
        filesystem: &filesystem,
        scripts: &scripts,
    };
    engine
        .discard_prepared_explicit_preview_with_lock(
            &prepared.context,
            &prepared.invocation_id,
            &prepared.preview_authority_sha256,
            &lock,
        )
        .map_err(|error| error.to_string())
}

pub fn cancel_actions_run(state: &mut ActionsRunState) {
    state.cancellation.store(true, Ordering::SeqCst);
    if matches!(state.status, ActionsRunStatus::Preparing | ActionsRunStatus::Running) {
        state.status = ActionsRunStatus::Cancelling;
    }
}

pub fn complete_actions_run(
    state: &mut ActionsRunState,
    result: Result<ActionPhaseReport, String>,
) {
    match result {
        Ok(report) => {
            state.status = if report.has_errors() {
                ActionsRunStatus::Failed
            } else {
                ActionsRunStatus::Complete
            };
            state.report = Some(report);
            state.error = None;
        }
        Err(error) => {
            state.status = if error.contains("preview is stale")
                || error.contains("refresh required")
            {
                ActionsRunStatus::Stale
            } else {
                ActionsRunStatus::Failed
            };
            state.error = Some(error);
        }
    }
}


pub fn draw_wizard(
    frame: &mut Frame,
    state: &ConversionActionsWizardState,
    buttons: &mut ButtonRenderMap,
    theme: crate::tui::theme::Theme,
) {
    let area = centered_rect(94, 90, frame.size());
    frame.render_widget(Clear, area);
    let block = Block::default()
        .title(" Conversion actions ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border))
        .style(Style::default().bg(theme.panel_bg).fg(theme.text));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(8),
            Constraint::Length(2),
        ])
        .split(inner);

    draw_wizard_header(frame, rows[0], state, buttons, theme);

    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(34), Constraint::Percentage(66)])
        .split(rows[1]);
    draw_available_actions(frame, columns[0], state, buttons, theme);
    draw_combined_pipeline(frame, columns[1], state, buttons, theme);
    draw_wizard_footer(frame, rows[2], buttons, theme);

    if matches!(state.dialog, ActionsWizardDialog::Configure(_)) {
        draw_config_dialog(frame, state, area, buttons, theme);
    }
}

fn draw_wizard_header(
    frame: &mut Frame,
    area: Rect,
    state: &ConversionActionsWizardState,
    buttons: &mut ButtonRenderMap,
    theme: crate::tui::theme::Theme,
) {
    let post = if state.phase == ActionPhase::Post { "● Post" } else { "○ Post" };
    let pre = if state.phase == ActionPhase::Pre { "● Pre" } else { "○ Pre" };
    let right = format!("Adding to   {post}   {pre}");
    let text = "Runs before & after each conversion.";
    let right_width = cell_width(&right);
    let available = area.width.saturating_sub(right_width).saturating_sub(1);
    let mut line = fit_to_cells(text, available);
    if area.width > right_width {
        line.push(' ');
    }
    line.push_str(&right);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(line, Style::default().fg(theme.text)),
        ])),
        area,
    );

    if right_width <= area.width {
        let radio_start = area.x.saturating_add(area.width.saturating_sub(right_width));
        let prefix_width = cell_width("Adding to   ");
        let post_width = cell_width(post);
        buttons.record_button(
            TuiButton::ActionsAddingPhase(false),
            Rect::new(radio_start.saturating_add(prefix_width), area.y, post_width, 1),
        );
        buttons.record_button(
            TuiButton::ActionsAddingPhase(true),
            Rect::new(
                radio_start
                    .saturating_add(prefix_width)
                    .saturating_add(post_width)
                    .saturating_add(cell_width("   ")),
                area.y,
                cell_width(pre),
                1,
            ),
        );
    }
}

fn draw_available_actions(
    frame: &mut Frame,
    area: Rect,
    state: &ConversionActionsWizardState,
    buttons: &mut ButtonRenderMap,
    theme: crate::tui::theme::Theme,
) {
    let block = pane_block(
        " Available ",
        state.focus == ActionsWizardFocus::Available && matches!(state.dialog, ActionsWizardDialog::Pipeline),
        theme,
    );
    let inner = block.inner(area);
    frame.render_widget(block, area);
    buttons.record_button(TuiButton::ActionsAvailablePane, inner);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let hint_rows = if inner.height >= 4 { 2 } else { 0 };
    let list_height = inner.height.saturating_sub(hint_rows) as usize;
    let scroll = effective_available_scroll(state, list_height);
    let mut lines: Vec<Line<'static>> = Vec::new();

    for visual_row in 0..list_height {
        let index = scroll.saturating_add(visual_row);
        if let Some(kind) = ACTION_KINDS.get(index) {
            let selected = state.focus == ActionsWizardFocus::Available
                && state.available_index == index
                && matches!(state.dialog, ActionsWizardDialog::Pipeline);
            let marker = if selected { "▸" } else { " " };
            let style = if selected {
                selected_style(theme)
            } else {
                Style::default().fg(theme.text)
            };
            let label = fit_to_cells(&format!(" {marker} {}", title_case(kind)), inner.width);
            lines.push(Line::styled(label, style));
            buttons.record_button(
                TuiButton::ActionsAvailable(index),
                Rect::new(inner.x, inner.y.saturating_add(visual_row as u16), inner.width, 1),
            );
        } else {
            lines.push(Line::from(""));
        }
    }

    if hint_rows > 0 {
        lines.push(Line::from(""));
        let hint = "Enter → add to selected phase";
        lines.push(Line::styled(
            truncate_to_cells(hint, inner.width),
            Style::default().fg(theme.text_dim),
        ));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_combined_pipeline(
    frame: &mut Frame,
    area: Rect,
    state: &ConversionActionsWizardState,
    buttons: &mut ButtonRenderMap,
    theme: crate::tui::theme::Theme,
) {
    let block = pane_block(
        " Pipeline ",
        state.focus == ActionsWizardFocus::Pipeline && matches!(state.dialog, ActionsWizardDialog::Pipeline),
        theme,
    );
    let inner = block.inner(area);
    frame.render_widget(block, area);
    buttons.record_button(TuiButton::ActionsPipelinePane, inner);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let visual_rows = pipeline_visual_rows(state);
    let scroll = effective_pipeline_scroll(state, inner.height as usize);
    let mut lines: Vec<Line<'static>> = Vec::new();

    for visual_y in 0..inner.height as usize {
        let row = visual_rows.get(scroll.saturating_add(visual_y)).copied();
        let screen_y = inner.y.saturating_add(visual_y as u16);
        match row {
            Some(PipelineVisualRow::Header(phase)) => {
                let title = match phase {
                    ActionPhase::Pre => "Pre-conversion",
                    ActionPhase::Post => "Post-conversion",
                };
                lines.push(Line::styled(
                    fit_to_cells(title, inner.width),
                    Style::default().fg(theme.info).add_modifier(Modifier::BOLD),
                ));
            }
            Some(PipelineVisualRow::Empty(_)) => {
                lines.push(Line::styled(
                    fit_to_cells("  (none yet)", inner.width),
                    Style::default().fg(theme.text_dim),
                ));
            }
            Some(PipelineVisualRow::Action(phase, index)) => {
                let Some(action) = state.actions_for(phase).get(index) else {
                    lines.push(Line::from(""));
                    continue;
                };
                let selected = state.pipeline_phase == phase
                    && state.pipeline_index == index
                    && state.focus == ActionsWizardFocus::Pipeline
                    && matches!(state.dialog, ActionsWizardDialog::Pipeline);
                let marker = if selected { "▸" } else { " " };
                let raw_label = format!(" {marker} {} {}", index + 1, compact_action_summary(action));
                let arrow_cells = cell_width("  ▲ ▼");
                let reserve_arrows = selected && inner.width >= arrow_cells.saturating_add(1);
                let label_width_budget = if reserve_arrows {
                    inner.width.saturating_sub(arrow_cells)
                } else {
                    inner.width
                };
                let label = truncate_to_cells(&raw_label, label_width_budget);
                let label_width = cell_width(&label);
                let mut spans = vec![Span::styled(
                    label,
                    if selected { selected_style(theme) } else { Style::default().fg(theme.text) },
                )];
                if reserve_arrows {
                    spans.push(Span::raw("  "));
                    spans.push(Span::styled("▲", Style::default().fg(theme.info).add_modifier(Modifier::BOLD)));
                    spans.push(Span::raw(" "));
                    spans.push(Span::styled("▼", Style::default().fg(theme.info).add_modifier(Modifier::BOLD)));
                }
                lines.push(Line::from(spans));

                let phase_button = phase == ActionPhase::Pre;
                buttons.record_button(
                    TuiButton::ActionsPipelineRow(phase_button, index),
                    Rect::new(inner.x, screen_y, inner.width, 1),
                );
                if reserve_arrows {
                    let arrow_up_x = inner.x.saturating_add(label_width).saturating_add(cell_width("  "));
                    let arrow_down_x = arrow_up_x.saturating_add(cell_width("▲ "));
                    buttons.record_button(
                        TuiButton::ActionsPipelineNudgeUp(phase_button, index),
                        Rect::new(arrow_up_x, screen_y, cell_width("▲"), 1),
                    );
                    buttons.record_button(
                        TuiButton::ActionsPipelineNudgeDown(phase_button, index),
                        Rect::new(arrow_down_x, screen_y, cell_width("▼"), 1),
                    );
                }
            }
            Some(PipelineVisualRow::Hint) => {
                lines.push(Line::styled(
                    truncate_to_cells(
                        "space configure · ↑↓ select · [ ] reorder · m move phase · del remove",
                        inner.width,
                    ),
                    Style::default().fg(theme.text_dim),
                ));
            }
            Some(PipelineVisualRow::Spacer) | None => lines.push(Line::from("")),
        }
    }

    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_wizard_footer(
    frame: &mut Frame,
    area: Rect,
    buttons: &mut ButtonRenderMap,
    theme: crate::tui::theme::Theme,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let button_style = Style::default().bg(theme.pill_dim_bg).fg(theme.pill_active_fg);
    let save_style = Style::default().bg(theme.success).fg(theme.pill_active_fg);
    let default_style = Style::default().bg(theme.info).fg(theme.pill_active_fg);
    let text_style = Style::default().fg(theme.text);
    let segments: &[(Option<TuiButton>, &str, Style)] = &[
        (Some(TuiButton::ActionsFooterAdd), " Enter Add ", button_style),
        (None, "  ", text_style),
        (Some(TuiButton::ActionsFooterConfigure), " space Configure ", button_style),
        (None, "  ", text_style),
        (None, " ↑↓ Reorder ", button_style),
        (None, "  ", text_style),
        (None, " m Move phase ", button_style),
        (None, "  ", text_style),
        (Some(TuiButton::ActionsFooterSave), " s Save ", save_style),
        (None, "  ", text_style),
        (Some(TuiButton::ActionsFooterSaveDefault), " S Default ", default_style),
        (None, "  ", text_style),
        (Some(TuiButton::ActionsFooterDone), " Esc Done", text_style),
    ];

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut x = area.x;
    let mut y = area.y;
    let right = area.x.saturating_add(area.width);
    let bottom = area.y.saturating_add(area.height);

    for (button, label, style) in segments.iter().copied() {
        if y >= bottom {
            break;
        }
        let width = cell_width(label);
        if width == 0 {
            continue;
        }
        if x > area.x && x.saturating_add(width) > right {
            lines.push(Line::from(spans));
            spans = Vec::new();
            y = y.saturating_add(1);
            x = area.x;
            if y >= bottom {
                break;
            }
        }
        let remaining = right.saturating_sub(x);
        if remaining == 0 {
            continue;
        }
        let visible_width = width.min(remaining);
        let rendered = if visible_width < width {
            truncate_to_cells(label, visible_width)
        } else {
            label.to_string()
        };
        if let Some(button) = button {
            buttons.record_button(button, Rect::new(x, y, visible_width, 1));
        }
        spans.push(Span::styled(rendered, style));
        x = x.saturating_add(visible_width);
    }
    lines.push(Line::from(spans));
    while lines.len() < area.height as usize {
        lines.push(Line::from(""));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

fn draw_config_dialog(
    frame: &mut Frame,
    state: &ConversionActionsWizardState,
    parent: Rect,
    buttons: &mut ButtonRenderMap,
    theme: crate::tui::theme::Theme,
) {
    let action = match state.configured_action() {
        Some(action) => action,
        None => return,
    };
    let title = format!(" Configure · {} ", action_title(action));
    let area = config_dialog_rect(parent);
    frame.render_widget(Clear, area);
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border))
        .style(Style::default().bg(theme.panel_bg).fg(theme.text));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    buttons.record_button(TuiButton::ActionsConfigModal, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(5),
            Constraint::Length(6),
            Constraint::Length(2),
        ])
        .split(inner);

    draw_config_mode_row(frame, rows[0], action, buttons, theme);
    draw_config_fields(frame, rows[1], state, action, buttons, theme);
    draw_config_preview(frame, rows[2], state, buttons, theme);
    draw_config_footer(frame, rows[3], buttons, theme);
}

fn draw_config_mode_row(
    frame: &mut Frame,
    area: Rect,
    action: &ConversionAction,
    buttons: &mut ButtonRenderMap,
    theme: crate::tui::theme::Theme,
) {
    let ConversionAction::Rename(rename) = action else {
        frame.render_widget(Paragraph::new(Line::from("")), area);
        return;
    };
    let modes = ["Template", "Uppercase", "Lowercase", "Fixcaps"];
    let selected_index = rename_mode_index(&rename.mode);
    let mut spans = vec![Span::styled(" Mode   ", Style::default().fg(theme.text))];
    let mut x = area.x.saturating_add(8);
    let right = area.x.saturating_add(area.width);
    for (index, label) in modes.iter().enumerate() {
        let selected = selected_index == index;
        let token = format!("{} {}", if selected { "●" } else { "○" }, label);
        let token_width = cell_width(&token);
        if x >= right || x.saturating_add(token_width) > right {
            break;
        }
        buttons.record_button(
            TuiButton::ActionsConfigMode(index),
            Rect::new(x, area.y, token_width, 1),
        );
        x = x.saturating_add(token_width).saturating_add(cell_width("   "));
        spans.push(Span::styled(
            token,
            if selected { Style::default().fg(theme.info).add_modifier(Modifier::BOLD) } else { Style::default().fg(theme.text) },
        ));
        spans.push(Span::raw("   "));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_config_fields(
    frame: &mut Frame,
    area: Rect,
    state: &ConversionActionsWizardState,
    action: &ConversionAction,
    buttons: &mut ButtonRenderMap,
    theme: crate::tui::theme::Theme,
) {
    if area.height == 0 || area.width == 0 {
        return;
    }

    let fields = action_fields(action);
    let has_tokens = matches!(action, ConversionAction::Rename(_));
    let token_rows = if has_tokens && area.height >= 2 { 1 } else { 0 };
    let field_height = area.height.saturating_sub(token_rows) as usize;
    let scroll = effective_config_scroll(state, fields.len(), field_height);
    let mut lines: Vec<Line<'static>> = Vec::new();

    for visual_row in 0..field_height {
        let index = scroll.saturating_add(visual_row);
        if let Some(field) = fields.get(index) {
            let selected = state.focus == ActionsWizardFocus::Config && index == state.config_index;
            let style = if selected { selected_style(theme) } else { Style::default().fg(theme.text) };
            let value = if let Some(edit) = state.edit_input.as_ref().filter(|edit| edit.field_index == index) {
                edit.input.text.clone()
            } else {
                field.value.clone()
            };
            let mut row = format!(" {:<17} [ {} ]", title_case(field.label), value);
            if field.label == "target" {
                if let Some(matches) = state.preview_match_count {
                    row.push_str(&format!(
                        "   matches {matches} file{}",
                        if matches == 1 { "" } else { "s" }
                    ));
                }
            }
            let label = fit_to_cells(&row, area.width);
            lines.push(Line::styled(label, style));
            buttons.record_button(
                TuiButton::ActionsConfigField(index),
                Rect::new(area.x, area.y.saturating_add(visual_row as u16), area.width, 1),
            );
        } else {
            lines.push(Line::from(""));
        }
    }

    if has_tokens && token_rows > 0 {
        let token_y = area.y.saturating_add(field_height as u16);
        let tokens = ["%ARTIST%", "%ALBUM%", "%DISC%", "%TRACK%", "%YEAR%"];
        let mut spans = vec![Span::styled(" Tokens  ", Style::default().fg(theme.text_dim))];
        let mut x = area.x.saturating_add(cell_width(" Tokens  "));
        let right = area.x.saturating_add(area.width);
        for (index, token) in tokens.iter().enumerate() {
            let token_width = cell_width(token);
            if x >= right || x.saturating_add(token_width) > right {
                break;
            }
            buttons.record_button(
                TuiButton::ActionsConfigToken(index),
                Rect::new(x, token_y, token_width, 1),
            );
            x = x.saturating_add(token_width).saturating_add(1);
            spans.push(Span::styled(*token, Style::default().fg(theme.info)));
            spans.push(Span::raw(" "));
        }
        lines.push(Line::from(spans));
    }

    frame.render_widget(Paragraph::new(lines), area);
}

fn draw_config_preview(
    frame: &mut Frame,
    area: Rect,
    state: &ConversionActionsWizardState,
    buttons: &mut ButtonRenderMap,
    theme: crate::tui::theme::Theme,
) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    buttons.record_button(TuiButton::ActionsConfigPreview, area);
    let count = preview_operation_count(state);
    let mut lines = vec![Line::styled(
        fit_to_cells(
            &format!(" Preview  dry-run · {count} operation{}", if count == 1 { "" } else { "s" }),
            area.width,
        ),
        Style::default().fg(theme.info).add_modifier(Modifier::BOLD),
    )];

    if area.height == 1 {
        frame.render_widget(Paragraph::new(lines), area);
        return;
    }

    let note = fit_to_cells(&preview_footer_note(state), area.width);
    let body_height = area.height.saturating_sub(2) as usize;
    let visual_rows = preview_visual_rows_for_width(state, area.width);
    let requested_scroll = match &state.dialog {
        ActionsWizardDialog::Configure(session) => session.preview_scroll,
        ActionsWizardDialog::Pipeline => 0,
    };
    let scroll = requested_scroll.min(visual_rows.len().saturating_sub(body_height));

    for row in visual_rows.iter().skip(scroll).take(body_height) {
        lines.push(Line::from(fit_to_cells(row, area.width)));
    }
    while lines.len() < area.height.saturating_sub(1) as usize {
        lines.push(Line::from(""));
    }
    lines.push(Line::styled(note, Style::default().fg(theme.text_dim)));
    frame.render_widget(Paragraph::new(lines), area);
}


fn preview_footer_note(state: &ConversionActionsWizardState) -> String {
    match state.configured_action() {
        Some(ConversionAction::Rename(_)) => {
            " Re-running plans 0 operations when names already match.".to_string()
        }
        Some(ConversionAction::Runscript(_)) => {
            " Run scripts may execute again; make scripts idempotent.".to_string()
        }
        Some(_) => " Preview reflects this action only.".to_string(),
        None => " Preview reflects the selected action only.".to_string(),
    }
}

fn draw_config_footer(
    frame: &mut Frame,
    area: Rect,
    buttons: &mut ButtonRenderMap,
    theme: crate::tui::theme::Theme,
) {
    let line = Line::from(vec![
        Span::styled(" Apply ", Style::default().bg(theme.success).fg(theme.pill_active_fg)),
        Span::raw(" ".repeat(area.width.saturating_sub(20) as usize)),
        Span::styled(" Esc Cancel ", Style::default().bg(theme.pill_dim_bg).fg(theme.pill_active_fg)),
    ]);
    frame.render_widget(Paragraph::new(line), area);
    buttons.record_button(TuiButton::ActionsConfigApply, Rect::new(area.x, area.y, 8, 1));
    buttons.record_button(
        TuiButton::ActionsConfigCancel,
        Rect::new(area.x.saturating_add(area.width.saturating_sub(12)), area.y, 12, 1),
    );
}

fn config_dialog_rect(parent: Rect) -> Rect {
    let min_width = parent.width.min(60);
    let min_height = parent.height.min(16);
    let width = parent.width.min(78).max(min_width);
    let height = parent.height.min(22).max(min_height);
    Rect::new(
        parent.x + parent.width.saturating_sub(width) / 2,
        parent.y + parent.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

fn title_case(value: &str) -> String {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn action_title(action: &ConversionAction) -> &'static str {
    match action {
        ConversionAction::Rename(_) => "Rename files",
        ConversionAction::Copy(_) => "Copy files",
        ConversionAction::Move(_) => "Move files",
        ConversionAction::Delete(_) => "Delete files",
        ConversionAction::CreateFolder(_) => "Create folder",
        ConversionAction::Runscript(_) => "Run script",
    }
}

fn compact_action_summary(action: &ConversionAction) -> String {
    match action {
        ConversionAction::Rename(action) => format!(
            "Rename   {}        {:?}",
            action.targeting.target.join(" "),
            action.mode
        ),
        ConversionAction::Copy(action) => format!(
            "Copy     {} → {}",
            action.targeting.target.join(" "),
            action.destination.display()
        ),
        ConversionAction::Move(action) => format!(
            "Move     {} → {}",
            action.targeting.target.join(" "),
            action.destination.display()
        ),
        ConversionAction::Delete(action) => format!("Delete   {}", action.targeting.target.join(" ")),
        ConversionAction::CreateFolder(action) => format!("Folder   {}", action.path.display()),
        ConversionAction::Runscript(action) => format!("Run      {}", action.script.display()),
    }
}

fn rename_mode_index(mode: &RenameMode) -> usize {
    match mode {
        RenameMode::Template => 0,
        RenameMode::Uppercase => 1,
        RenameMode::Lowercase => 2,
        RenameMode::Fixcaps => 3,
    }
}

fn preview_operation_count(state: &ConversionActionsWizardState) -> usize {
    state.preview_operation_count
}

pub fn draw_actions_run(
    frame: &mut Frame,
    state: &ActionsRunState,
    theme: crate::tui::theme::Theme,
) {
    let area = centered_rect(88, 82, frame.size());
    frame.render_widget(Clear, area);
    let title = format!(" Actions Dry Run — {} ", state.target.display());
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border))
        .style(Style::default().bg(theme.panel_bg).fg(theme.text));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(8), Constraint::Length(3)])
        .split(inner);
    let status = match state.status {
        ActionsRunStatus::Preparing => "Preparing exact preview in the background — no mutation has occurred.",
        ActionsRunStatus::Preview => "Prepared preview — no mutation has occurred.",
        ActionsRunStatus::RecoveryPreview => "Recovery preview — apply resumes the displayed durable journal.",
        ActionsRunStatus::Running => "Applying the exact reviewed durable action plan…",
        ActionsRunStatus::Cancelling => "Cancellation requested; recoverable built-in work will stop safely.",
        ActionsRunStatus::Complete => "Action pipeline completed.",
        ActionsRunStatus::Failed => "Action pipeline finished with errors.",
        ActionsRunStatus::Stale => "Preview is stale; refresh is required. Nothing was executed.",
    };
    frame.render_widget(Paragraph::new(status), rows[0]);
    let mut lines = state.preview_lines.iter().cloned().map(Line::from).collect::<Vec<_>>();
    if let Some(report) = &state.report {
        lines.push(Line::from(""));
        lines.push(Line::styled(
            format!("Result: {} operations", report.operation_count()),
            Style::default().fg(if report.has_errors() { theme.error } else { theme.success }),
        ));
        for action in &report.actions {
            lines.push(Line::from(format!(
                "  {}: {:?}{}",
                action.kind,
                action.status,
                action.error.as_ref().map(|error| format!(" — {error}")).unwrap_or_default()
            )));
            for operation in &action.operations {
                lines.push(Line::from(format!("    {:?}: {}", operation.status, operation.summary)));
            }
        }
    }
    if let Some(error) = &state.error {
        lines.push(Line::styled(error.clone(), Style::default().fg(theme.error)));
    }
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }),
        rows[1],
    );
    let footer = match state.status {
        ActionsRunStatus::Preparing => "Esc cancel preview preparation",
        ActionsRunStatus::Preview | ActionsRunStatus::RecoveryPreview => "Enter apply  Esc cancel",
        ActionsRunStatus::Running | ActionsRunStatus::Cancelling => "Esc request cancellation",
        ActionsRunStatus::Complete | ActionsRunStatus::Failed | ActionsRunStatus::Stale => "Enter/Esc close",
    };
    frame.render_widget(Paragraph::new(footer), rows[2]);
}

fn pane_block(
    title: &'static str,
    focused: bool,
    theme: crate::tui::theme::Theme,
) -> Block<'static> {
    Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if focused { theme.info } else { theme.border_dim }))
}

fn selected_style(theme: crate::tui::theme::Theme) -> Style {
    Style::default()
        .bg(theme.selection_bg)
        .fg(theme.text_bright)
        .add_modifier(Modifier::BOLD)
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::collections::BTreeMap;
    use std::fs;

    #[derive(serde::Serialize)]
    struct TestIdentityPayload {
        schema_version: u8,
        album_tokens: BTreeMap<String, String>,
        disc_count: Option<u32>,
    }

    #[derive(serde::Serialize)]
    struct TestIdentityRecord {
        payload: TestIdentityPayload,
        payload_sha256: String,
    }

    fn write_test_identity(
        target: &Path,
        artist: &str,
        album: &str,
        title_extra: &str,
    ) -> String {
        let mut album_tokens = BTreeMap::new();
        for (name, value) in [
            ("ARTIST", artist),
            ("ALBUM_ARTIST", artist),
            ("ALBUM", album),
            ("TITLE_EXTRA", title_extra),
            ("YEAR", "1988"),
            ("GENRE", "Rock"),
            ("CATALOG", ""),
            ("FORMAT", "FLAC"),
            ("SAMPLERATE", "44.1kHz"),
            ("BITDEPTH", "16"),
            ("EXT", ""),
            ("DISCNUMBER", "1"),
            ("NNDISCNUMBER", "01"),
            ("NNNDISCNUMBER", "001"),
            ("DISCTOTAL", "2"),
            ("DISC_FOLDER", "disc 01"),
        ] {
            album_tokens.insert(name.to_string(), value.to_string());
        }
        let payload = TestIdentityPayload {
            schema_version: 1,
            album_tokens,
            disc_count: Some(2),
        };
        let payload_bytes = serde_json::to_vec(&payload).expect("identity payload");
        let payload_sha256 = hex::encode(Sha256::digest(&payload_bytes));
        let record = TestIdentityRecord {
            payload,
            payload_sha256: payload_sha256.clone(),
        };
        fs::write(
            target.join(".tonepoet-action-identity.json"),
            serde_json::to_vec_pretty(&record).expect("identity record"),
        )
        .expect("write identity record");
        payload_sha256
    }

    #[test]
    fn pre_phase_add_targeting_allows_every_action_kind() {
        for index in 0..ACTION_KINDS.len() {
            assert!(
                default_action_for_kind(index, ActionPhase::Pre).is_some(),
                "Adding to Pre must not filter action kind {index}"
            );
        }
    }

    #[test]
    fn action_field_round_trip_preserves_csv_order() {
        let mut action = default_action_for_kind(0, ActionPhase::Post).unwrap();
        set_action_field_text(&mut action, 0, "*.log, *.cue").unwrap();
        let ConversionAction::Rename(action) = action else { panic!("rename") };
        assert_eq!(action.targeting.target, vec!["*.log", "*.cue"]);
    }

    #[test]
    fn wizard_reorder_is_stable() {
        let mut state = ConversionActionsWizardState::new(ActionPipeline {
            pre: Vec::new(),
            post: vec![
                default_action_for_kind(4, ActionPhase::Post).unwrap(),
                default_action_for_kind(5, ActionPhase::Post).unwrap(),
            ],
        });
        state.focus = ActionsWizardFocus::Pipeline;
        state.pipeline_index = 1;
        let WizardKeyResult::Continue(state) = handle_wizard_key(
            state,
            KeyEvent::new(KeyCode::Char('['), KeyModifiers::NONE),
        ) else { panic!("continue") };
        assert!(matches!(state.draft.post[0], ConversionAction::Runscript(_)));
    }


    #[test]
    fn wizard_radio_phase_targeting_controls_configure_on_add() {
        let state = ConversionActionsWizardState::new(ActionPipeline::default());
        let WizardKeyResult::Continue(state) = handle_wizard_button(
            state,
            TuiButton::ActionsAddingPhase(true),
            false,
        ) else {
            panic!("continue")
        };
        assert_eq!(state.phase, ActionPhase::Pre);

        let WizardKeyResult::Continue(state) = handle_wizard_button(
            state,
            TuiButton::ActionsAvailable(0),
            true,
        ) else {
            panic!("continue")
        };
        assert_eq!(state.draft.pre.len(), 1);
        assert!(state.draft.post.is_empty());
        assert!(matches!(state.draft.pre[0], ConversionAction::Rename(_)));
        assert!(matches!(state.dialog, ActionsWizardDialog::Configure(_)));
    }

    #[test]
    fn wizard_cancel_fresh_config_removes_placeholder() {
        let state = ConversionActionsWizardState::new(ActionPipeline::default());
        let WizardKeyResult::Continue(state) = handle_wizard_button(
            state,
            TuiButton::ActionsAvailable(0),
            true,
        ) else {
            panic!("continue")
        };
        assert_eq!(state.draft.post.len(), 1);
        assert!(matches!(state.dialog, ActionsWizardDialog::Configure(_)));

        let WizardKeyResult::Continue(state) = handle_wizard_key(
            state,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        ) else {
            panic!("continue")
        };
        assert!(state.draft.post.is_empty());
        assert!(matches!(state.dialog, ActionsWizardDialog::Pipeline));
    }

    #[test]
    fn wizard_moves_selected_action_between_phases_explicitly() {
        let mut state = ConversionActionsWizardState::new(ActionPipeline {
            pre: Vec::new(),
            post: vec![default_action_for_kind(5, ActionPhase::Post).unwrap()],
        });
        state.focus = ActionsWizardFocus::Pipeline;
        state.pipeline_phase = ActionPhase::Post;
        state.pipeline_index = 0;

        let WizardKeyResult::Continue(state) = handle_wizard_key(
            state,
            KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE),
        ) else {
            panic!("continue")
        };
        assert!(state.draft.post.is_empty());
        assert_eq!(state.draft.pre.len(), 1);
        assert_eq!(state.pipeline_phase, ActionPhase::Pre);
    }

    #[test]
    fn wizard_shift_s_returns_commit_default() {
        let state = ConversionActionsWizardState::new(ActionPipeline {
            pre: Vec::new(),
            post: vec![default_action_for_kind(4, ActionPhase::Post).unwrap()],
        });
        let WizardKeyResult::CommitDefault(state) = handle_wizard_key(
            state,
            KeyEvent::new(KeyCode::Char('S'), KeyModifiers::SHIFT),
        ) else {
            panic!("expected commit-default")
        };
        assert_eq!(state.draft.post.len(), 1);
    }

    #[test]
    fn unicode_radio_geometry_uses_terminal_cells_not_utf8_bytes() {
        assert_eq!(cell_width("● Template"), 10);
        assert_eq!("● Template".len(), 12);
        assert_eq!(cell_width("Adding to   ● Post   ○ Pre"), 26);
        assert_eq!("Adding to   ● Post   ○ Pre".len(), 30);
    }

    #[test]
    fn rendered_pipeline_nudge_arrows_own_their_click_cells() {
        let theme = crate::tui::theme::theme_by_slug(crate::tui::theme::default_theme_slug())
            .expect("default theme");
        let mut state = ConversionActionsWizardState::new(ActionPipeline {
            pre: Vec::new(),
            post: vec![default_action_for_kind(4, ActionPhase::Post).unwrap()],
        });
        state.focus = ActionsWizardFocus::Pipeline;
        state.pipeline_phase = ActionPhase::Post;
        state.pipeline_index = 0;

        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
        let mut buttons = ButtonRenderMap::new();
        terminal
            .draw(|frame| draw_wizard(frame, &state, &mut buttons, theme))
            .expect("draw wizard");

        let up_rect = buttons
            .find_button_rect(&TuiButton::ActionsPipelineNudgeUp(false, 0))
            .expect("up nudge hitbox");
        let down_rect = buttons
            .find_button_rect(&TuiButton::ActionsPipelineNudgeDown(false, 0))
            .expect("down nudge hitbox");
        assert_eq!(terminal.backend().buffer().get(up_rect.x, up_rect.y).symbol(), "▲");
        assert_eq!(terminal.backend().buffer().get(down_rect.x, down_rect.y).symbol(), "▼");
        assert_eq!(
            buttons.find_button_at(up_rect.x, up_rect.y),
            Some(TuiButton::ActionsPipelineNudgeUp(false, 0))
        );
        assert_eq!(
            buttons.find_button_at(down_rect.x, down_rect.y),
            Some(TuiButton::ActionsPipelineNudgeDown(false, 0))
        );
    }

    #[test]
    fn pipeline_scroll_wheel_updates_viewport_and_visible_targets() {
        let theme = crate::tui::theme::theme_by_slug(crate::tui::theme::default_theme_slug())
            .expect("default theme");
        let mut state = ConversionActionsWizardState::new(ActionPipeline {
            pre: Vec::new(),
            post: (0..18)
                .map(|_| default_action_for_kind(4, ActionPhase::Post).unwrap())
                .collect(),
        });
        state.focus = ActionsWizardFocus::Pipeline;

        let WizardKeyResult::Continue(state) = handle_wizard_scroll(
            state,
            Some(TuiButton::ActionsPipelinePane),
            6,
            None,
        ) else {
            panic!("continue")
        };
        assert!(state.pipeline_scroll > 0, "wheel must move the viewport offset");
        assert!(state.pipeline_index > 0, "selection follows the scrolled visible row");

        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
        let mut buttons = ButtonRenderMap::new();
        terminal
            .draw(|frame| draw_wizard(frame, &state, &mut buttons, theme))
            .expect("draw wizard");
        assert_eq!(
            buttons.find_button_at(
                buttons
                    .find_button_rect(&TuiButton::ActionsPipelineRow(false, state.pipeline_index))
                    .expect("selected visible row")
                    .x,
                buttons
                    .find_button_rect(&TuiButton::ActionsPipelineRow(false, state.pipeline_index))
                    .expect("selected visible row")
                    .y,
            ),
            Some(TuiButton::ActionsPipelineRow(false, state.pipeline_index))
        );
    }

    #[test]
    fn available_scroll_wheel_updates_viewport_offset() {
        let state = ConversionActionsWizardState::new(ActionPipeline::default());
        let WizardKeyResult::Continue(state) = handle_wizard_scroll(
            state,
            Some(TuiButton::ActionsAvailablePane),
            3,
            None,
        ) else {
            panic!("continue")
        };
        assert_eq!(state.available_scroll, 3);
        assert_eq!(state.available_index, 3);
    }

    #[test]
    fn config_fields_truncate_to_one_visual_row_per_hitbox() {
        let theme = crate::tui::theme::theme_by_slug(crate::tui::theme::default_theme_slug())
            .expect("default theme");
        let state = ConversionActionsWizardState::new(ActionPipeline::default());
        let WizardKeyResult::Continue(mut state) = handle_wizard_button(
            state,
            TuiButton::ActionsAvailable(0),
            true,
        ) else {
            panic!("continue")
        };
        if let ConversionAction::Rename(action) = &mut state.draft.post[0] {
            action.targeting.target = vec!["*.log".repeat(30)];
            action.template = "%ARTIST% - %ALBUM% - %DISC% - %TRACK% - %YEAR%".repeat(8);
        }

        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
        let mut buttons = ButtonRenderMap::new();
        terminal
            .draw(|frame| draw_wizard(frame, &state, &mut buttons, theme))
            .expect("draw wizard");

        let field0 = buttons
            .find_button_rect(&TuiButton::ActionsConfigField(0))
            .expect("target field hitbox");
        let field1 = buttons
            .find_button_rect(&TuiButton::ActionsConfigField(1))
            .expect("exclude field hitbox");
        assert_eq!(field1.y, field0.y.saturating_add(1));
        assert_eq!(field0.height, 1);
        assert_eq!(field1.height, 1);
    }


    #[test]
    fn configure_preview_pipeline_contains_only_selected_action() {
        let mut state = ConversionActionsWizardState::new(ActionPipeline {
            pre: Vec::new(),
            post: vec![
                default_action_for_kind(0, ActionPhase::Post).unwrap(),
                default_action_for_kind(1, ActionPhase::Post).unwrap(),
            ],
        });
        state.focus = ActionsWizardFocus::Pipeline;
        state.pipeline_phase = ActionPhase::Post;
        state.pipeline_index = 0;
        open_config_for_selected(&mut state, false);

        let (pipeline, configured) = effective_preview_pipeline(&state).expect("preview pipeline");
        assert!(pipeline.pre.is_empty());
        assert_eq!(pipeline.post.len(), 1);
        assert!(matches!(pipeline.post[0], ConversionAction::Rename(_)));
        assert!(matches!(configured, Some(ConversionAction::Rename(_))));
    }

    #[test]
    fn configure_preview_pipeline_applies_pending_edit_to_preview_clone_only() {
        let mut state = ConversionActionsWizardState::new(ActionPipeline {
            pre: Vec::new(),
            post: vec![default_action_for_kind(0, ActionPhase::Post).unwrap()],
        });
        state.pipeline_phase = ActionPhase::Post;
        state.pipeline_index = 0;
        open_config_for_selected(&mut state, false);
        state.edit_input = Some(ActionConfigEdit {
            field_index: 0,
            input: crate::tui::text_input::TextInputState::new_selected("*.cue".to_string()),
        });

        let (pipeline, configured) = effective_preview_pipeline(&state).expect("preview pipeline");
        let ConversionAction::Rename(preview_action) = &pipeline.post[0] else {
            panic!("rename")
        };
        assert_eq!(preview_action.targeting.target, vec!["*.cue"]);
        let ConversionAction::Rename(draft_action) = &state.draft.post[0] else {
            panic!("rename")
        };
        assert_eq!(draft_action.targeting.target, vec!["*"]);
        assert!(matches!(configured, Some(ConversionAction::Rename(_))));
    }

    #[test]
    fn preview_match_hint_counts_target_files_with_excludes() {
        let temp = tempfile::tempdir().expect("temp dir");
        fs::write(temp.path().join("Disc 1.log"), b"log").expect("log");
        fs::write(temp.path().join("Disc 1.cue"), b"cue").expect("cue");
        fs::write(temp.path().join("skip.log"), b"skip").expect("skip");
        fs::write(temp.path().join("cover.jpg"), b"jpg").expect("cover");
        let targeting = TargetSpec {
            target: vec!["*.log".to_string(), "*.cue".to_string()],
            exclude: vec!["skip*".to_string()],
            ..TargetSpec::default()
        };
        let mut matches = 0usize;
        for name in ["Disc 1.log", "Disc 1.cue", "skip.log", "cover.jpg"] {
            if target_spec_matches_preview_path(temp.path(), &targeting, &temp.path().join(name))
                .expect("resolved preview glob")
            {
                matches += 1;
            }
        }
        assert_eq!(matches, 2);
    }

    #[test]
    fn operation_count_comes_from_preview_state_not_formatted_lines() {
        let mut state = ConversionActionsWizardState::new(ActionPipeline::default());
        state.preview_lines = vec![
            "   formatted description line".to_string(),
            "   another wrapped description line".to_string(),
        ];
        state.preview_operation_count = 1;
        assert_eq!(preview_operation_count(&state), 1);
    }

    #[test]
    fn keyboard_preview_scroll_is_clamped_to_rendered_viewport() {
        let mut state = ConversionActionsWizardState::new(ActionPipeline {
            pre: Vec::new(),
            post: vec![default_action_for_kind(0, ActionPhase::Post).unwrap()],
        });
        state.pipeline_phase = ActionPhase::Post;
        state.pipeline_index = 0;
        open_config_for_selected(&mut state, false);
        state.focus = ActionsWizardFocus::Preview;
        state.preview_dirty = false;
        state.preview_lines = vec![
            "rename /very/long/path/that/must/wrap/across/several/terminal/rows/source.log -> /very/long/path/that/must/wrap/across/several/terminal/rows/destination.log".to_string(),
        ];
        let preview_rect = Some(Rect::new(0, 0, 32, 5));
        let rows = preview_visual_rows_for_width(&state, 32);

        let mut state = state;
        for _ in 0..100 {
            let WizardKeyResult::Continue(next) = handle_wizard_key_with_preview_rect(
                state,
                KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
                preview_rect,
            ) else {
                panic!("continue")
            };
            state = next;
        }
        let ActionsWizardDialog::Configure(session) = &state.dialog else {
            panic!("configure")
        };
        assert_eq!(session.preview_scroll, rows.len().saturating_sub(3));

        let WizardKeyResult::Continue(state) = handle_wizard_key_with_preview_rect(
            state,
            KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
            preview_rect,
        ) else {
            panic!("continue")
        };
        let ActionsWizardDialog::Configure(session) = &state.dialog else {
            panic!("configure")
        };
        assert_eq!(session.preview_scroll, rows.len().saturating_sub(3).saturating_sub(1));
    }

    #[test]
    fn preview_scroll_is_reclamped_when_preview_content_shrinks() {
        let mut state = ConversionActionsWizardState::new(ActionPipeline {
            pre: Vec::new(),
            post: vec![default_action_for_kind(0, ActionPhase::Post).unwrap()],
        });
        state.pipeline_phase = ActionPhase::Post;
        state.pipeline_index = 0;
        open_config_for_selected(&mut state, false);
        state.preview_lines = vec![
            "rename /very/long/path/that/must/wrap/across/several/terminal/rows/source.log -> /very/long/path/that/must/wrap/across/several/terminal/rows/destination.log".to_string(),
        ];
        if let ActionsWizardDialog::Configure(session) = &mut state.dialog {
            session.preview_scroll = usize::MAX;
        }
        state.preview_lines = vec!["No operations planned.".to_string()];
        clamp_wizard_preview_scroll_for_rect(&mut state, Some(Rect::new(0, 0, 32, 5)));
        let ActionsWizardDialog::Configure(session) = &state.dialog else {
            panic!("configure")
        };
        assert_eq!(session.preview_scroll, 0);
    }

    #[test]
    fn preview_scroll_uses_wrapped_visual_rows_and_clamps_at_end() {
        let mut state = ConversionActionsWizardState::new(ActionPipeline {
            pre: Vec::new(),
            post: vec![default_action_for_kind(0, ActionPhase::Post).unwrap()],
        });
        state.pipeline_phase = ActionPhase::Post;
        state.pipeline_index = 0;
        open_config_for_selected(&mut state, false);
        state.preview_dirty = false;
        state.preview_lines = vec![
            "rename /very/long/path/that/must/wrap/across/several/terminal/rows/source.log -> /very/long/path/that/must/wrap/across/several/terminal/rows/destination.log".to_string(),
        ];
        state.preview_operation_count = 1;
        let rows = preview_visual_rows_for_width(&state, 32);
        assert!(rows.len() > 1, "long paths must be measured as visual rows");

        let WizardKeyResult::Continue(state) = handle_wizard_scroll(
            state,
            Some(TuiButton::ActionsConfigPreview),
            999,
            Some(Rect::new(0, 0, 32, 5)),
        ) else {
            panic!("continue")
        };
        let ActionsWizardDialog::Configure(session) = &state.dialog else {
            panic!("configure")
        };
        assert_eq!(session.preview_scroll, rows.len().saturating_sub(3));
    }

    #[test]
    fn preview_draw_keeps_idempotency_note_visible_after_overscroll() {
        let theme = crate::tui::theme::theme_by_slug(crate::tui::theme::default_theme_slug())
            .expect("default theme");
        let mut state = ConversionActionsWizardState::new(ActionPipeline {
            pre: Vec::new(),
            post: vec![default_action_for_kind(0, ActionPhase::Post).unwrap()],
        });
        state.pipeline_phase = ActionPhase::Post;
        state.pipeline_index = 0;
        open_config_for_selected(&mut state, false);
        state.preview_lines = vec![
            "rename /very/long/path/that/wraps/source.log -> /very/long/path/that/wraps/destination.log".to_string(),
        ];
        if let ActionsWizardDialog::Configure(session) = &mut state.dialog {
            session.preview_scroll = usize::MAX;
        }
        let backend = ratatui::backend::TestBackend::new(50, 8);
        let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
        let mut buttons = ButtonRenderMap::new();
        terminal
            .draw(|frame| draw_config_preview(
                frame,
                Rect::new(0, 0, 50, 8),
                &state,
                &mut buttons,
                theme,
            ))
            .expect("draw preview");
        let mut last = String::new();
        for x in 0..50 {
            last.push_str(terminal.backend().buffer().get(x, 7).symbol());
        }
        assert!(last.contains("Re-running plans 0 operations"));
    }

    #[test]
    fn runscript_preview_note_does_not_claim_filename_idempotency() {
        let mut state = ConversionActionsWizardState::new(ActionPipeline {
            pre: Vec::new(),
            post: vec![default_action_for_kind(5, ActionPhase::Post).unwrap()],
        });
        state.pipeline_phase = ActionPhase::Post;
        state.pipeline_index = 0;
        open_config_for_selected(&mut state, false);

        let note = preview_footer_note(&state);

        assert!(!note.contains("names already match"));
        assert!(note.contains("Run scripts"));
    }

    #[test]
    fn pure_navigation_does_not_mark_preview_dirty() {
        let state = ConversionActionsWizardState::new(ActionPipeline::default());
        let WizardKeyResult::Continue(state) = handle_wizard_key(
            state,
            KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
        ) else {
            panic!("continue")
        };
        assert!(!state.preview_dirty, "Tab focus changes must not trigger preview I/O");

        let WizardKeyResult::Continue(state) = handle_wizard_button(
            state,
            TuiButton::ActionsAvailable(2),
            false,
        ) else {
            panic!("continue")
        };
        assert!(!state.preview_dirty, "single-click selection must not trigger preview I/O");
    }

    #[test]
    fn opening_config_marks_only_dialog_b_preview_dirty() {
        let state = ConversionActionsWizardState::new(ActionPipeline::default());
        let WizardKeyResult::Continue(state) = handle_wizard_button(
            state,
            TuiButton::ActionsAvailable(0),
            true,
        ) else {
            panic!("continue")
        };
        assert!(matches!(state.dialog, ActionsWizardDialog::Configure(_)));
        assert!(state.preview_dirty);
        assert!(!state.preview_valid);
    }

    #[test]
    fn apply_requests_validation_when_preview_is_dirty() {
        let state = ConversionActionsWizardState::new(ActionPipeline::default());
        let WizardKeyResult::Continue(state) = handle_wizard_button(
            state,
            TuiButton::ActionsAvailable(0),
            true,
        ) else {
            panic!("continue")
        };
        let WizardKeyResult::ValidateConfigApply(state) = handle_wizard_button(
            state,
            TuiButton::ActionsConfigApply,
            false,
        ) else {
            panic!("apply should require preview validation")
        };
        assert!(state.preview_dirty);
        assert!(matches!(state.dialog, ActionsWizardDialog::Configure(_)));
    }

    #[test]
    fn apply_does_not_close_when_planner_preview_failed() {
        let state = ConversionActionsWizardState::new(ActionPipeline::default());
        let WizardKeyResult::Continue(mut state) = handle_wizard_button(
            state,
            TuiButton::ActionsAvailable(0),
            true,
        ) else {
            panic!("continue")
        };
        state.preview_dirty = false;
        state.preview_valid = false;
        state.preview_unavailable = false;
        state.preview_planner_failed = true;
        state.preview_lines = vec!["Planning failed: destination is empty".to_string()];

        let WizardKeyResult::Continue(state) = handle_wizard_button(
            state,
            TuiButton::ActionsConfigApply,
            false,
        ) else {
            panic!("continue")
        };
        assert!(matches!(state.dialog, ActionsWizardDialog::Configure(_)));
        assert!(state.preview_notice.contains("Cannot apply"));
    }

    #[test]
    fn apply_closes_when_preview_context_is_unavailable_but_config_is_valid() {
        let state = ConversionActionsWizardState::new(ActionPipeline::default());
        let WizardKeyResult::Continue(mut state) = handle_wizard_button(
            state,
            TuiButton::ActionsAvailable(0),
            true,
        ) else {
            panic!("continue")
        };
        state.preview_dirty = false;
        state.preview_valid = false;
        state.preview_unavailable = true;
        state.preview_planner_failed = false;
        state.preview_lines = vec!["Preview unavailable: no conversion source is selected".to_string()];

        let WizardKeyResult::Continue(state) = handle_wizard_button(
            state,
            TuiButton::ActionsConfigApply,
            false,
        ) else {
            panic!("continue")
        };
        assert!(matches!(state.dialog, ActionsWizardDialog::Pipeline));
    }

    #[test]
    fn valid_action_applies_after_real_no_source_preview_unavailable_refresh() {
        let app = AppState::new_for_test(crate::config::TonepoetConfig::default());
        let state = ConversionActionsWizardState::new(ActionPipeline::default());
        let WizardKeyResult::Continue(mut state) = handle_wizard_button(
            state,
            TuiButton::ActionsAvailable(0),
            true,
        ) else {
            panic!("continue")
        };
        refresh_wizard_preview_for_app(&mut state, &app);
        assert!(state.preview_unavailable);
        assert!(!state.preview_valid);
        assert!(!state.preview_dirty);

        let WizardKeyResult::Continue(state) = handle_wizard_button(
            state,
            TuiButton::ActionsConfigApply,
            false,
        ) else {
            panic!("continue")
        };
        assert!(matches!(state.dialog, ActionsWizardDialog::Pipeline));
    }

    #[test]
    fn apply_closes_only_after_successful_current_preview() {
        let state = ConversionActionsWizardState::new(ActionPipeline::default());
        let WizardKeyResult::Continue(mut state) = handle_wizard_button(
            state,
            TuiButton::ActionsAvailable(0),
            true,
        ) else {
            panic!("continue")
        };
        state.preview_dirty = false;
        state.preview_valid = true;

        let WizardKeyResult::Continue(state) = handle_wizard_button(
            state,
            TuiButton::ActionsConfigApply,
            false,
        ) else {
            panic!("continue")
        };
        assert!(matches!(state.dialog, ActionsWizardDialog::Pipeline));
    }

    #[test]
    fn empty_target_edit_cannot_be_applied() {
        let state = ConversionActionsWizardState::new(ActionPipeline::default());
        let WizardKeyResult::Continue(state) = handle_wizard_button(
            state,
            TuiButton::ActionsAvailable(0),
            true,
        ) else {
            panic!("continue")
        };
        let WizardKeyResult::Continue(mut state) = handle_wizard_button(
            state,
            TuiButton::ActionsConfigField(0),
            false,
        ) else {
            panic!("continue")
        };
        state.edit_input.as_mut().expect("target edit").input.text.clear();

        let WizardKeyResult::Continue(state) = handle_wizard_button(
            state,
            TuiButton::ActionsConfigApply,
            false,
        ) else {
            panic!("continue")
        };
        assert!(matches!(state.dialog, ActionsWizardDialog::Configure(_)));
        assert!(state.edit_input.is_some(), "invalid text remains editable");
        assert!(state.preview_notice.contains("target must include"));
    }

    #[test]
    fn target_field_renders_match_count_hint_when_available() {
        let theme = crate::tui::theme::theme_by_slug(crate::tui::theme::default_theme_slug())
            .expect("default theme");
        let state = ConversionActionsWizardState::new(ActionPipeline::default());
        let WizardKeyResult::Continue(mut state) = handle_wizard_button(
            state,
            TuiButton::ActionsAvailable(0),
            true,
        ) else {
            panic!("continue")
        };
        state.preview_match_count = Some(2);

        let backend = ratatui::backend::TestBackend::new(100, 30);
        let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
        let mut buttons = ButtonRenderMap::new();
        terminal
            .draw(|frame| draw_wizard(frame, &state, &mut buttons, theme))
            .expect("draw wizard");
        let field = buttons
            .find_button_rect(&TuiButton::ActionsConfigField(0))
            .expect("target field hitbox");
        let row = (field.x..field.x.saturating_add(field.width))
            .map(|x| terminal.backend().buffer().get(x, field.y).symbol())
            .collect::<String>();
        assert!(row.contains("matches 2 files"), "row was: {row}");
    }

    #[test]
    fn wizard_double_click_available_dispatch_adds_and_configures() {
        let mut clicks = crate::tui::button_map::DoubleClickState::default();
        assert!(!clicks.register_click(
            TuiButton::ActionsAvailable(0),
            10,
            10,
            std::time::Duration::from_millis(400),
        ));
        assert!(clicks.register_click(
            TuiButton::ActionsAvailable(0),
            10,
            10,
            std::time::Duration::from_millis(400),
        ));

        let state = ConversionActionsWizardState::new(ActionPipeline::default());
        let WizardKeyResult::Continue(state) = handle_wizard_button(
            state,
            TuiButton::ActionsAvailable(0),
            true,
        ) else {
            panic!("continue")
        };
        assert_eq!(state.draft.post.len(), 1);
        assert!(matches!(state.dialog, ActionsWizardDialog::Configure(_)));
    }


    #[test]
    fn configure_dialog_ignores_underlying_dialog_a_mouse_targets() {
        let state = ConversionActionsWizardState::new(ActionPipeline::default());
        let WizardKeyResult::Continue(state) = handle_wizard_button(
            state,
            TuiButton::ActionsAvailable(0),
            true,
        ) else {
            panic!("continue")
        };
        assert!(matches!(state.dialog, ActionsWizardDialog::Configure(_)));
        assert_eq!(state.draft.post.len(), 1);

        let WizardKeyResult::Continue(state) = handle_wizard_button(
            state,
            TuiButton::ActionsAvailable(5),
            true,
        ) else {
            panic!("continue")
        };
        assert!(matches!(state.dialog, ActionsWizardDialog::Configure(_)));
        assert_eq!(
            state.draft.post.len(),
            1,
            "Dialog A hits must not add actions behind Dialog B"
        );
    }

    #[test]
    fn configure_modal_surface_consumes_blank_mouse_hits() {
        let state = ConversionActionsWizardState::new(ActionPipeline::default());
        let WizardKeyResult::Continue(state) = handle_wizard_button(
            state,
            TuiButton::ActionsAvailable(0),
            true,
        ) else {
            panic!("continue")
        };
        let WizardKeyResult::Continue(state) = handle_wizard_button(
            state,
            TuiButton::ActionsConfigModal,
            false,
        ) else {
            panic!("continue")
        };
        assert!(matches!(state.dialog, ActionsWizardDialog::Configure(_)));
        assert_eq!(state.draft.post.len(), 1);
    }

    #[test]
    fn rendered_config_modal_catch_all_blocks_dialog_a_hit_targets() {
        let theme = crate::tui::theme::theme_by_slug(crate::tui::theme::default_theme_slug())
            .expect("default theme");
        let mut state = ConversionActionsWizardState::new(ActionPipeline {
            pre: Vec::new(),
            post: vec![default_action_for_kind(0, ActionPhase::Post).unwrap()],
        });
        state.focus = ActionsWizardFocus::Pipeline;
        state.pipeline_phase = ActionPhase::Post;
        state.pipeline_index = 0;
        open_config_for_selected(&mut state, false);

        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
        let mut buttons = ButtonRenderMap::new();
        terminal
            .draw(|frame| draw_wizard(frame, &state, &mut buttons, theme))
            .expect("draw wizard");

        let modal = buttons
            .find_button_rect(&TuiButton::ActionsConfigModal)
            .expect("modal catch-all hitbox");
        let mut blank_modal_cell = None;
        'outer: for y in modal.y..modal.y.saturating_add(modal.height) {
            for x in modal.x..modal.x.saturating_add(modal.width) {
                if buttons.find_button_at(x, y) == Some(TuiButton::ActionsConfigModal) {
                    blank_modal_cell = Some((x, y));
                    break 'outer;
                }
            }
        }
        let (x, y) = blank_modal_cell.expect("blank modal surface cell");
        assert_eq!(buttons.find_button_at(x, y), Some(TuiButton::ActionsConfigModal));
    }

    #[test]
    fn rendered_footer_hitboxes_follow_wrapped_footer_controls() {
        let theme = crate::tui::theme::theme_by_slug(crate::tui::theme::default_theme_slug())
            .expect("default theme");
        let state = ConversionActionsWizardState::new(ActionPipeline::default());
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
        let mut buttons = ButtonRenderMap::new();
        terminal
            .draw(|frame| draw_wizard(frame, &state, &mut buttons, theme))
            .expect("draw wizard");

        for button in [
            TuiButton::ActionsFooterAdd,
            TuiButton::ActionsFooterConfigure,
            TuiButton::ActionsFooterSave,
            TuiButton::ActionsFooterSaveDefault,
            TuiButton::ActionsFooterDone,
        ] {
            let rect = buttons.find_button_rect(&button).expect("visible footer button");
            assert_eq!(
                buttons.find_button_at(rect.x, rect.y),
                Some(button.clone()),
                "footer hitbox must cover the rendered cell for {button:?}"
            );
            // Footer pills render with a leading padded space (part of the
            // pill), so the FIRST cell is legitimately a styled space; the
            // hitbox row must still contain the control's visible text.
            let row_has_text = (rect.x..rect.x.saturating_add(rect.width)).any(|cx| {
                terminal.backend().buffer().get(cx, rect.y).symbol() != " "
            });
            assert!(
                row_has_text,
                "footer hitbox for {button:?} must cover visible text"
            );
        }
    }

    #[test]
    fn configure_dialog_80x24_registers_only_visible_core_controls() {
        let theme = crate::tui::theme::theme_by_slug(crate::tui::theme::default_theme_slug())
            .expect("default theme");
        let mut state = ConversionActionsWizardState::new(ActionPipeline {
            pre: Vec::new(),
            post: vec![default_action_for_kind(0, ActionPhase::Post).unwrap()],
        });
        state.focus = ActionsWizardFocus::Pipeline;
        state.pipeline_phase = ActionPhase::Post;
        state.pipeline_index = 0;
        open_config_for_selected(&mut state, false);

        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
        let mut buttons = ButtonRenderMap::new();
        terminal
            .draw(|frame| draw_wizard(frame, &state, &mut buttons, theme))
            .expect("draw wizard");

        for button in [
            TuiButton::ActionsConfigModal,
            TuiButton::ActionsConfigField(0),
            TuiButton::ActionsConfigMode(0),
            TuiButton::ActionsConfigToken(0),
            TuiButton::ActionsConfigApply,
            TuiButton::ActionsConfigCancel,
        ] {
            let rect = buttons.find_button_rect(&button).expect("visible config control");
            assert!(rect.x.saturating_add(rect.width) <= 80, "{button:?} x bounds: {rect:?}");
            assert!(rect.y.saturating_add(rect.height) <= 24, "{button:?} y bounds: {rect:?}");
            assert_eq!(buttons.find_button_at(rect.x, rect.y), Some(button));
        }
    }

    #[test]
    fn mouse_field_switch_commits_pending_edit_to_owning_field() {
        let state = ConversionActionsWizardState::new(ActionPipeline::default());
        let WizardKeyResult::Continue(state) = handle_wizard_button(
            state,
            TuiButton::ActionsAvailable(0),
            true,
        ) else {
            panic!("continue")
        };
        let WizardKeyResult::Continue(mut state) = handle_wizard_button(
            state,
            TuiButton::ActionsConfigField(0),
            false,
        ) else {
            panic!("continue")
        };
        state.edit_input.as_mut().expect("edit").input.text = "*.log, *.cue".to_string();

        let WizardKeyResult::Continue(state) = handle_wizard_button(
            state,
            TuiButton::ActionsConfigField(4),
            false,
        ) else {
            panic!("continue")
        };
        let ConversionAction::Rename(action) = &state.draft.post[0] else {
            panic!("rename")
        };
        assert_eq!(action.targeting.target, vec!["*.log", "*.cue"]);
        assert_eq!(state.edit_input.as_ref().expect("template edit").field_index, 4);
    }

    #[test]
    fn rename_config_fields_do_not_duplicate_mode_radio() {
        let action = ConversionAction::Rename(RenameAction {
            targeting: TargetSpec {
                target: vec!["*.cue".to_string()],
                exclude: Vec::new(),
                allow_sources: false,
                continue_on_error: false,
            },
            mode: RenameMode::Template,
            template: "%ALBUM%".to_string(),
        });

        let fields = action_fields(&action);

        assert!(fields.iter().any(|field| field.label == "template"));
        assert!(
            fields.iter().all(|field| field.label != "mode"),
            "rename mode is represented by the dedicated radio row, not a duplicate ordinary field"
        );
        assert_eq!(rename_template_field_index(&action), Some(4));
    }

    #[test]
    fn token_click_targets_template_not_active_non_template_field() {
        let state = ConversionActionsWizardState::new(ActionPipeline::default());
        let WizardKeyResult::Continue(state) = handle_wizard_button(
            state,
            TuiButton::ActionsAvailable(0),
            true,
        ) else {
            panic!("continue")
        };
        let WizardKeyResult::Continue(mut state) = handle_wizard_button(
            state,
            TuiButton::ActionsConfigField(0),
            false,
        ) else {
            panic!("continue")
        };
        state.edit_input.as_mut().expect("target edit").input.text = "*.cue".to_string();

        let WizardKeyResult::Continue(state) = handle_wizard_button(
            state,
            TuiButton::ActionsConfigToken(1),
            false,
        ) else {
            panic!("continue")
        };
        let ConversionAction::Rename(action) = &state.draft.post[0] else {
            panic!("rename")
        };
        assert_eq!(action.targeting.target, vec!["*.cue"]);
        let edit = state.edit_input.as_ref().expect("template edit");
        assert_eq!(edit.field_index, 4);
        assert!(edit.input.text.contains("%ALBUM%"));
        assert!(!action.targeting.target.iter().any(|value| value.contains("%ALBUM%")));
    }

    #[test]
    fn actions_run_reuses_published_canonical_identity_and_plans_zero_repeat_rename() {
        let temp = tempfile::tempdir().expect("temp dir");
        let target = temp.path().join("published-album");
        fs::create_dir_all(&target).expect("album dir");
        let already_named = target.join("Deep Purple - Nobody's Perfect (Japan - SHM) [Disc 01].cue");
        fs::write(&already_named, b"cue").expect("published cue");
        write_test_identity(&target, "Deep Purple", "Nobody's Perfect", "Japan - SHM");

        let context = explicit_context(&target).expect("validated explicit identity");
        assert_eq!(context.album_tokens["ALBUM_ARTIST"], "Deep Purple");
        assert!(!context.album_tokens.contains_key("ALBUMARTIST"));
        let pipeline = ActionPipeline {
            pre: Vec::new(),
            post: vec![ConversionAction::Rename(RenameAction {
                targeting: TargetSpec {
                    target: vec![already_named
                        .file_name()
                        .expect("file name")
                        .to_string_lossy()
                        .to_string()],
                    ..TargetSpec::default()
                },
                mode: RenameMode::Template,
                template: "%ARTIST% - %ALBUM% (%TITLE_EXTRA%) [Disc %NNDISCNUMBER%]"
                    .to_string(),
            })],
        };
        let filesystem = CapabilityActionFilesystem::new();
        let scripts = ProcessGroupScriptRunner;
        let engine = ActionEngine {
            filesystem: &filesystem,
            scripts: &scripts,
        };
        let plans = engine
            .preview_phase(&pipeline, &context)
            .expect("manual action preview");
        assert_eq!(plans.len(), 1);
        assert!(
            plans[0].operations.is_empty(),
            "canonical manual identity must make an already-correct conversion rename idempotent"
        );
    }

    #[test]
    fn actions_run_revalidates_identity_checksum_under_lock_before_execution() {
        let temp = tempfile::tempdir().expect("temp dir");
        let target = temp.path().join("published-album");
        fs::create_dir_all(&target).expect("album dir");
        let expected_sha = write_test_identity(
            &target,
            "Deep Purple",
            "Nobody's Perfect",
            "Japan - SHM",
        );
        let preview_context = explicit_context(&target).expect("preview identity");

        let replacement_sha = write_test_identity(
            &target,
            "Rainbow",
            "Long Live Rock 'n' Roll",
            "Deluxe",
        );
        assert_ne!(expected_sha, replacement_sha);

        let error = match lock_and_revalidate_explicit_execution_context(
            &target,
            &expected_sha,
            &preview_context,
        ) {
            Ok((_lock, _context)) => {
                panic!("stale preview identity must be rejected before planning or mutation")
            }
            Err(error) => error,
        };
        assert!(
            error.contains("preview is stale")
                && error.contains("canonical album identity changed"),
            "stale-preview refusal must name the identity change, got: {error}"
        );
    }

    #[test]
    fn explicit_context_mutates_the_album_object_protected_by_its_lock() {
        let temp = tempfile::tempdir().expect("temp dir");
        let output = temp.path().join("output");
        let retained_output = temp.path().join("output-retained");
        let target = output.join("Album");
        fs::create_dir_all(&target).expect("album dir");
        fs::write(target.join("a-only.log"), b"original").expect("original marker");
        write_test_identity(&target, "Artist", "Album", "Edition");

        let mut lock = acquire_explicit_action_run_lock_for_album(&target)
            .expect("explicit authority");
        let canonical_target = lock.canonical_album_dir().to_path_buf();
        let identity = crate::convert::pipeline::stages::conversion_action_explicit_identity_locked(
            &canonical_target,
            &lock,
        )
        .expect("canonical identity");
        let context = explicit_context_from_identity(&canonical_target, identity, &lock)
            .expect("descriptor-bound explicit context");
        assert!(context.retained_album_capability.is_some());
        assert!(context.retained_output_capability.is_some());
        assert!(context.retained_journal_capability.is_some());

        lock.release_publication_authority();
        fs::rename(&output, &retained_output).expect("rename original output");
        fs::create_dir_all(&target).expect("replacement album");
        fs::write(target.join("b-only.log"), b"replacement").expect("replacement marker");

        let pipeline = ActionPipeline {
            pre: Vec::new(),
            post: vec![ConversionAction::Delete(DeleteAction {
                targeting: TargetSpec {
                    target: vec!["*.log".to_string()],
                    ..TargetSpec::default()
                },
            })],
        };
        let filesystem = CapabilityActionFilesystem::new();
        let scripts = ProcessGroupScriptRunner;
        let engine = ActionEngine {
            filesystem: &filesystem,
            scripts: &scripts,
        };
        let cancellation = SharedCancellation(Arc::new(AtomicBool::new(false)));
        engine
            .execute_phase(&pipeline, &context, &cancellation)
            .expect("descriptor-bound manual execution");

        assert!(!retained_output.join("Album/a-only.log").exists());
        assert!(target.join("b-only.log").exists());
        assert!(!target.join("a-only.log").exists());
        assert!(lock.holds_action_execution_authority());
    }

}
