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
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
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
use crate::tui::message::AppMessage;

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
    Phase,
    Available,
    Pipeline,
    Config,
    Preview,
}

#[derive(Debug, Clone)]
pub struct ConversionActionsWizardState {
    pub draft: ActionPipeline,
    pub phase: ActionPhase,
    pub focus: ActionsWizardFocus,
    pub available_index: usize,
    pub pipeline_index: usize,
    pub config_index: usize,
    pub edit_input: Option<crate::tui::text_input::TextInputState>,
    pub preview_lines: Vec<String>,
    pub preview_notice: String,
}

impl ConversionActionsWizardState {
    pub fn new(draft: ActionPipeline) -> Self {
        let mut state = Self {
            draft,
            phase: ActionPhase::Post,
            focus: ActionsWizardFocus::Available,
            available_index: 0,
            pipeline_index: 0,
            config_index: 0,
            edit_input: None,
            preview_lines: Vec::new(),
            preview_notice: "Preview simulates the selected conversion source and its planned destination; scripts are never executed.".to_string(),
        };
        state.clamp();
        state.refresh_summary_preview();
        state
    }

    pub fn actions(&self) -> &[ConversionAction] {
        self.draft.for_phase(self.phase)
    }

    fn actions_mut(&mut self) -> &mut Vec<ConversionAction> {
        self.draft.for_phase_mut(self.phase)
    }

    fn selected_action(&self) -> Option<&ConversionAction> {
        self.actions().get(self.pipeline_index)
    }

    fn selected_action_mut(&mut self) -> Option<&mut ConversionAction> {
        let index = self.pipeline_index;
        self.actions_mut().get_mut(index)
    }

    fn clamp(&mut self) {
        self.available_index = self.available_index.min(ACTION_KINDS.len().saturating_sub(1));
        self.pipeline_index = self
            .pipeline_index
            .min(self.actions().len().saturating_sub(1));
        let field_count = self
            .selected_action()
            .map(action_fields)
            .map(|fields| fields.len())
            .unwrap_or(0);
        self.config_index = self.config_index.min(field_count.saturating_sub(1));
    }

    fn refresh_summary_preview(&mut self) {
        self.preview_lines = self
            .actions()
            .iter()
            .enumerate()
            .map(|(index, action)| format!("{}. {}", index + 1, action_summary(action)))
            .collect();
        if self.preview_lines.is_empty() {
            self.preview_lines.push("No actions configured for this phase.".to_string());
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
    let source_path = match app.convert.source.mode.current_path() {
        Some(path) => path.clone(),
        None => {
            state.preview_lines = vec!["Preview unavailable: no conversion source is selected".to_string()];
            state.preview_notice = "Select a conversion source to simulate actions.".to_string();
            return;
        }
    };
    let request = match wizard_preview_request(state, app, &source_path) {
        Ok(request) => request,
        Err(error) => {
            state.preview_lines = vec![format!("Preview unavailable: {error}")];
            state.preview_notice = "Conversion destination simulation failed before planning.".to_string();
            return;
        }
    };

    if state.phase == ActionPhase::Pre {
        let context = match crate::convert::pipeline::stages::conversion_action_pre_preview_context(&request) {
            Ok(context) => context,
            Err(error) => {
                state.preview_lines = vec![format!("Preview unavailable: {error}")];
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
            state.preview_lines = vec![format!("Preview unavailable: {error}")];
            state.preview_notice = format!(
                "Could not simulate the conversion destination for {}.",
                source_path.display()
            );
            return;
        }
    };
    let temporary = match tempfile::tempdir() {
        Ok(temporary) => temporary,
        Err(error) => {
            state.preview_lines = vec![format!("Preview unavailable: cannot create isolated simulation: {error}")];
            return;
        }
    };
    let simulated_album = temporary.path().join("planned-album");
    if let Err(error) = materialize_preview_placeholders(&simulated_album, &layout.entries) {
        state.preview_lines = vec![format!("Preview unavailable: {error}")];
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
        artist: app.convert.metadata.artist.clone(),
        album_artist: app.convert.metadata.album_artist_for_conversion.clone(),
        genre: app.convert.metadata.genre.clone(),
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
                metadata.artist = track.performer.clone().or(metadata.artist);
                metadata.album_artist = album_artist.clone().or(metadata.album_artist);
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
    match engine.preview_phase(&state.draft, context) {
        Ok(plans) => {
            state.preview_lines.clear();
            for (index, plan) in plans.iter().enumerate() {
                state.preview_lines.push(format!("{}. {}", index + 1, plan.action_kind));
                state.preview_lines.extend(describe_plan(plan).into_iter().map(|line| {
                    let line = if let Some((from, to)) = path_translation {
                        let from = from.to_string_lossy();
                        let to = to.to_string_lossy();
                        line.replace(from.as_ref(), to.as_ref())
                    } else {
                        line
                    };
                    format!("   {line}")
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
        }
    }
}

#[derive(Debug, Clone)]
pub enum WizardKeyResult {
    Continue(ConversionActionsWizardState),
    Commit(ActionPipeline),
    Cancel,
}

pub fn handle_wizard_key(
    mut state: ConversionActionsWizardState,
    key: KeyEvent,
) -> WizardKeyResult {
    if let Some(mut input) = state.edit_input.take() {
        match key.code {
            KeyCode::Esc => {
                state.edit_input = None;
            }
            KeyCode::Enter => {
                let value = input.text.clone();
                let field_index = state.config_index;
                if let Some(action) = state.selected_action_mut() {
                    if let Err(error) = set_action_field_text(action, field_index, &value) {
                        state.preview_notice = error;
                        state.edit_input = Some(input);
                        return WizardKeyResult::Continue(state);
                    }
                }
                state.edit_input = None;
                state.refresh_summary_preview();
            }
            _ => {
                crate::tui::text_input::handle_text_input_key(&mut input, &key);
                state.edit_input = Some(input);
            }
        }
        return WizardKeyResult::Continue(state);
    }

    match (key.code, key.modifiers) {
        (KeyCode::Esc, _) | (KeyCode::Char('q'), _) => return WizardKeyResult::Cancel,
        (KeyCode::Char('s'), KeyModifiers::NONE)
        | (KeyCode::Char('S'), KeyModifiers::SHIFT) => {
            return WizardKeyResult::Commit(state.draft)
        }
        (KeyCode::Tab, _) => {
            state.focus = match state.focus {
                ActionsWizardFocus::Phase => ActionsWizardFocus::Available,
                ActionsWizardFocus::Available => ActionsWizardFocus::Pipeline,
                ActionsWizardFocus::Pipeline => ActionsWizardFocus::Config,
                ActionsWizardFocus::Config => ActionsWizardFocus::Preview,
                ActionsWizardFocus::Preview => ActionsWizardFocus::Phase,
            };
        }
        (KeyCode::BackTab, _) => {
            state.focus = match state.focus {
                ActionsWizardFocus::Phase => ActionsWizardFocus::Preview,
                ActionsWizardFocus::Available => ActionsWizardFocus::Phase,
                ActionsWizardFocus::Pipeline => ActionsWizardFocus::Available,
                ActionsWizardFocus::Config => ActionsWizardFocus::Pipeline,
                ActionsWizardFocus::Preview => ActionsWizardFocus::Config,
            };
        }
        (KeyCode::Left, _) | (KeyCode::Right, _)
            if state.focus == ActionsWizardFocus::Phase =>
        {
            state.phase = match state.phase {
                ActionPhase::Pre => ActionPhase::Post,
                ActionPhase::Post => ActionPhase::Pre,
            };
            state.pipeline_index = 0;
            state.config_index = 0;
            state.refresh_summary_preview();
        }
        (KeyCode::Up, _) => move_focus_cursor(&mut state, -1),
        (KeyCode::Down, _) => move_focus_cursor(&mut state, 1),
        (KeyCode::Char('k'), _) => move_focus_cursor(&mut state, -1),
        (KeyCode::Char('j'), _) => move_focus_cursor(&mut state, 1),
        (KeyCode::Enter, _) if state.focus == ActionsWizardFocus::Available => {
            if let Some(action) = default_action_for_kind(state.available_index, state.phase) {
                state.actions_mut().push(action);
                state.pipeline_index = state.actions().len().saturating_sub(1);
                state.config_index = 0;
                state.focus = ActionsWizardFocus::Config;
                state.refresh_summary_preview();
            } else {
                state.preview_notice =
                    "Pre phase permits only create folder and run script in v1.".to_string();
            }
        }
        (KeyCode::Delete, _) | (KeyCode::Char('d'), _)
            if state.focus == ActionsWizardFocus::Pipeline =>
        {
            let index = state.pipeline_index;
            if index < state.actions().len() {
                state.actions_mut().remove(index);
                state.clamp();
                state.refresh_summary_preview();
            }
        }
        (KeyCode::Char('['), _) if state.focus == ActionsWizardFocus::Pipeline => {
            let index = state.pipeline_index;
            if index > 0 {
                state.actions_mut().swap(index, index - 1);
                state.pipeline_index -= 1;
                state.refresh_summary_preview();
            }
        }
        (KeyCode::Char(']'), _) if state.focus == ActionsWizardFocus::Pipeline => {
            let index = state.pipeline_index;
            if index + 1 < state.actions().len() {
                state.actions_mut().swap(index, index + 1);
                state.pipeline_index += 1;
                state.refresh_summary_preview();
            }
        }
        (KeyCode::Enter, _) | (KeyCode::Char(' '), _)
            if state.focus == ActionsWizardFocus::Config =>
        {
            let field_index = state.config_index;
            let edit = state
                .selected_action_mut()
                .map(|action| action_field_edit(action, field_index));
            match edit {
                Some(FieldEdit::Text(value)) => {
                    state.edit_input = Some(
                        crate::tui::text_input::TextInputState::new_selected(value),
                    );
                }
                Some(FieldEdit::Changed) => state.refresh_summary_preview(),
                Some(FieldEdit::Unavailable) | None => {}
            }
        }
        _ => {}
    }
    state.clamp();
    WizardKeyResult::Continue(state)
}

fn move_focus_cursor(state: &mut ConversionActionsWizardState, delta: isize) {
    match state.focus {
        ActionsWizardFocus::Available => {
            state.available_index = stepped_index(state.available_index, ACTION_KINDS.len(), delta);
        }
        ActionsWizardFocus::Pipeline => {
            state.pipeline_index = stepped_index(state.pipeline_index, state.actions().len(), delta);
            state.config_index = 0;
        }
        ActionsWizardFocus::Config => {
            let count = state
                .selected_action()
                .map(action_fields)
                .map(|fields| fields.len())
                .unwrap_or(0);
            state.config_index = stepped_index(state.config_index, count, delta);
        }
        ActionsWizardFocus::Phase | ActionsWizardFocus::Preview => {}
    }
}

fn stepped_index(current: usize, count: usize, delta: isize) -> usize {
    if count == 0 {
        return 0;
    }
    if delta < 0 {
        current.saturating_sub(delta.unsigned_abs()).min(count - 1)
    } else {
        current.saturating_add(delta as usize).min(count - 1)
    }
}

fn default_targeting() -> TargetSpec {
    TargetSpec {
        target: vec!["*".to_string()],
        exclude: Vec::new(),
        allow_sources: false,
        continue_on_error: false,
    }
}

fn default_action_for_kind(index: usize, phase: ActionPhase) -> Option<ConversionAction> {
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
    if phase == ActionPhase::Pre
        && !matches!(
            &action,
            ConversionAction::CreateFolder(_) | ConversionAction::Runscript(_)
        )
    {
        None
    } else {
        Some(action)
    }
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
            fields.push(ActionField { label: "mode", value: format!("{:?}", action.mode).to_ascii_lowercase() });
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
            4 => {
                action.mode = match action.mode {
                    RenameMode::Template => RenameMode::Uppercase,
                    RenameMode::Uppercase => RenameMode::Lowercase,
                    RenameMode::Lowercase => RenameMode::Fixcaps,
                    RenameMode::Fixcaps => RenameMode::Template,
                };
                FieldEdit::Changed
            }
            5 => FieldEdit::Text(action.template.clone()),
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

fn set_action_field_text(
    action: &mut ConversionAction,
    field: usize,
    value: &str,
) -> Result<(), String> {
    match action {
        ConversionAction::Rename(action) => match field {
            0 => action.targeting.target = split_csv(value),
            1 => action.targeting.exclude = split_csv(value),
            5 => action.template = value.to_string(),
            _ => {}
        },
        ConversionAction::Copy(action) => match field {
            0 => action.targeting.target = split_csv(value),
            1 => action.targeting.exclude = split_csv(value),
            4 => action.destination = PathBuf::from(value.trim()),
            _ => {}
        },
        ConversionAction::Move(action) => match field {
            0 => action.targeting.target = split_csv(value),
            1 => action.targeting.exclude = split_csv(value),
            4 => action.destination = PathBuf::from(value.trim()),
            _ => {}
        },
        ConversionAction::Delete(action) => match field {
            0 => action.targeting.target = split_csv(value),
            1 => action.targeting.exclude = split_csv(value),
            _ => {}
        },
        ConversionAction::CreateFolder(action) => {
            if field == 0 { action.path = PathBuf::from(value.trim()); }
        }
        ConversionAction::Runscript(action) => match field {
            0 => action.script = PathBuf::from(value.trim()),
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
            let filesystem = CapabilityActionFilesystem::new();
            let scripts = ProcessGroupScriptRunner;
            let engine = ActionEngine { filesystem: &filesystem, scripts: &scripts };
            engine
                .execute_prepared_explicit_phase_with_lock(
                    &pipeline,
                    &context,
                    &expected_identity_sha256,
                    &invocation_id,
                    &preview_authority_sha256,
                    &cancellation,
                    &mut lock,
                )
                .map_err(|error| error.to_string())
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
    theme: crate::tui::theme::Theme,
) {
    let area = centered_rect(92, 88, frame.size());
    frame.render_widget(Clear, area);
    let block = Block::default()
        .title(" Conversion Actions ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border))
        .style(Style::default().bg(theme.panel_bg).fg(theme.text));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(12),
            Constraint::Length(3),
        ])
        .split(inner);
    draw_phase_tabs(frame, rows[0], state, theme);
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(24),
            Constraint::Percentage(33),
            Constraint::Percentage(43),
        ])
        .split(rows[1]);
    draw_available(frame, columns[0], state, theme);
    draw_pipeline(frame, columns[1], state, theme);
    draw_config_and_preview(frame, columns[2], state, theme);
    let footer = Line::from(vec![
        Span::styled(" Tab ", Style::default().bg(theme.pill_dim_bg).fg(theme.pill_active_fg)),
        Span::raw(" focus  "),
        Span::styled(" Enter ", Style::default().bg(theme.pill_active_bg).fg(theme.pill_active_fg)),
        Span::raw(" add/edit  "),
        Span::styled(" [ ] ", Style::default().bg(theme.pill_dim_bg).fg(theme.pill_active_fg)),
        Span::raw(" reorder  "),
        Span::styled(" d ", Style::default().bg(theme.destructive).fg(theme.pill_active_fg)),
        Span::raw(" delete  "),
        Span::styled(" s ", Style::default().bg(theme.success).fg(theme.pill_active_fg)),
        Span::raw(" save  Esc cancel"),
    ]);
    frame.render_widget(Paragraph::new(footer).wrap(Wrap { trim: true }), rows[2]);
}

fn draw_phase_tabs(
    frame: &mut Frame,
    area: Rect,
    state: &ConversionActionsWizardState,
    theme: crate::tui::theme::Theme,
) {
    let active = |phase| state.phase == phase;
    let line = Line::from(vec![
        Span::raw(" Phase  "),
        Span::styled(
            " PRE ",
            Style::default()
                .bg(if active(ActionPhase::Pre) { theme.tab_active } else { theme.tab_inactive })
                .fg(theme.pill_active_fg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            " POST ",
            Style::default()
                .bg(if active(ActionPhase::Post) { theme.tab_active } else { theme.tab_inactive })
                .fg(theme.pill_active_fg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("   ←/→ switch"),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

fn draw_available(
    frame: &mut Frame,
    area: Rect,
    state: &ConversionActionsWizardState,
    theme: crate::tui::theme::Theme,
) {
    let items = ACTION_KINDS
        .iter()
        .enumerate()
        .map(|(index, kind)| {
            let unavailable = state.phase == ActionPhase::Pre && index < 4;
            let style = if unavailable {
                Style::default().fg(theme.text_dim)
            } else {
                Style::default().fg(theme.text)
            };
            ListItem::new(format!(" {}{}", if unavailable { "· " } else { "+ " }, kind)).style(style)
        })
        .collect::<Vec<_>>();
    let mut list_state = ListState::default();
    list_state.select(Some(state.available_index));
    let block = pane_block(
        " Available ",
        state.focus == ActionsWizardFocus::Available,
        theme,
    );
    frame.render_stateful_widget(
        List::new(items).block(block).highlight_style(selected_style(theme)),
        area,
        &mut list_state,
    );
}

fn draw_pipeline(
    frame: &mut Frame,
    area: Rect,
    state: &ConversionActionsWizardState,
    theme: crate::tui::theme::Theme,
) {
    let items = if state.actions().is_empty() {
        vec![ListItem::new(" (empty)").style(Style::default().fg(theme.text_dim))]
    } else {
        state
            .actions()
            .iter()
            .enumerate()
            .map(|(index, action)| ListItem::new(format!(" {}. {}", index + 1, action_summary(action))))
            .collect()
    };
    let mut list_state = ListState::default();
    if !state.actions().is_empty() {
        list_state.select(Some(state.pipeline_index));
    }
    let block = pane_block(
        if state.phase == ActionPhase::Pre { " Pre pipeline " } else { " Post pipeline " },
        state.focus == ActionsWizardFocus::Pipeline,
        theme,
    );
    frame.render_stateful_widget(
        List::new(items).block(block).highlight_style(selected_style(theme)),
        area,
        &mut list_state,
    );
}

fn draw_config_and_preview(
    frame: &mut Frame,
    area: Rect,
    state: &ConversionActionsWizardState,
    theme: crate::tui::theme::Theme,
) {
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(56), Constraint::Percentage(44)])
        .split(area);
    let fields = state
        .selected_action()
        .map(action_fields)
        .unwrap_or_default();
    let field_lines = if fields.is_empty() {
        vec![Line::from("Select or add an action.")]
    } else {
        fields
            .iter()
            .enumerate()
            .map(|(index, field)| {
                let style = if state.focus == ActionsWizardFocus::Config && index == state.config_index {
                    selected_style(theme)
                } else {
                    Style::default().fg(theme.text)
                };
                Line::styled(format!(" {}: {}", field.label, field.value), style)
            })
            .collect()
    };
    let mut config_text = field_lines;
    if let Some(input) = &state.edit_input {
        config_text.push(Line::styled(
            format!(" editing: {}", input.text),
            Style::default().fg(theme.info).add_modifier(Modifier::BOLD),
        ));
    }
    frame.render_widget(
        Paragraph::new(config_text)
            .block(pane_block(
                " Action configuration ",
                state.focus == ActionsWizardFocus::Config,
                theme,
            ))
            .wrap(Wrap { trim: false }),
        sections[0],
    );
    let mut preview = vec![Line::styled(
        state.preview_notice.clone(),
        Style::default().fg(theme.text_dim),
    )];
    preview.extend(state.preview_lines.iter().cloned().map(Line::from));
    frame.render_widget(
        Paragraph::new(preview)
            .block(pane_block(
                " Dry-run summary ",
                state.focus == ActionsWizardFocus::Preview,
                theme,
            ))
            .wrap(Wrap { trim: false }),
        sections[1],
    );
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
    fn pre_wizard_rejects_destructive_default_actions() {
        for index in 0..4 {
            assert!(default_action_for_kind(index, ActionPhase::Pre).is_none());
        }
        assert!(default_action_for_kind(4, ActionPhase::Pre).is_some());
        assert!(default_action_for_kind(5, ActionPhase::Pre).is_some());
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
